//! 画像デコード、リサイズ、WebP エンコード。
//!
//! - magic number で形式を確定する
//! - format ごとに decode 経路を分ける
//! - viewer 向け経路では `display_w` と `max_tex_side` を反映する

use std::time::Instant;

use anyhow::{Context, Result};
use libwebp_sys::{
    WebPAnimDecoder, WebPAnimDecoderDelete, WebPAnimDecoderGetInfo, WebPAnimDecoderGetNext,
    WebPAnimDecoderHasMoreFrames, WebPAnimDecoderNewInternal, WebPAnimDecoderOptions,
    WebPAnimDecoderOptionsInitInternal, WebPAnimInfo, WebPData, WebPGetDemuxABIVersion,
    WEBP_CSP_MODE,
};

use crate::domain::app_settings::ViewerQuality;
use crate::domain::page::ImageFormatHint;

// ── 公開型 ────────────────────────────────────────────────────────────────────
/// アニメーションフレームの最小遅延（ms）。16ms ≒ 62.5fps 上限。
pub const MIN_FRAME_DELAY_MS: u32 = 16;
/// `egui` / `wgpu` から texture 上限を取得できない場合の保守的な fallback。
pub(crate) const DEFAULT_MAX_TEXTURE_SIDE: u32 = 8192;
const RIFF_HEADER_LEN: usize = 12;
const RIFF_CHUNK_HEADER_LEN: usize = 8;
const WEBP_ANIMATION_FLAG: u8 = 0x02;
const STATIC_FRAME_DELAY_MS: u32 = 0;
const FALLBACK_FRAME_DELAY_MS: u32 = 100;
const MIN_ANIMATION_DELAY_MS: u32 = 20;

#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8 固定
}

/// アニメーションの 1 フレーム
#[derive(Clone, Debug)]
pub struct FrameData {
    pub image: DecodedImage,
    /// フレーム表示時間（ミリ秒）。静止画は 0。
    pub delay_ms: u32,
}

/// `decode_for_viewer_frames` の戻り値。
/// デコード済みフレーム列 + リサイズ前の元サイズ（見開き AUTO 判定に使用）。
pub struct ViewerFrames {
    pub frames: Vec<FrameData>,
    /// リサイズ前の元の幅（px）。見開き AUTO 判定（表示候補の縦長判定）に使う。
    pub orig_w: u32,
    /// リサイズ前の元の高さ（px）。
    pub orig_h: u32,
}

/// animated WebP の逐次フレーム供給結果。
pub struct AnimationFrameChunk {
    pub frames: Vec<FrameData>,
    pub exhausted: bool,
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
}

/// `libwebp` の WebPAnimDecoder をラップし、順次フレームを安全寄りに扱う。
pub struct WebpAnimFrameSource {
    // Keep WebPData.bytes alive for libwebp decoder lifetime.
    _data: Vec<u8>,
    decoder: *mut WebPAnimDecoder,
    info: WebPAnimInfo,
    prev_timestamp: i32,
    emitted_frames: usize,
}

impl Drop for WebpAnimFrameSource {
    fn drop(&mut self) {
        // SAFETY: `decoder` は `WebPAnimDecoderNewInternal` 成功時だけ保持し、Drop で 1 回だけ解放する。
        unsafe {
            if !self.decoder.is_null() {
                WebPAnimDecoderDelete(self.decoder);
            }
        }
    }
}

impl WebpAnimFrameSource {
    pub fn new(data: Vec<u8>) -> Result<Self> {
        if !is_animated_webp_fast(&data) {
            anyhow::bail!("not an animated webp");
        }

        // SAFETY: C struct は直後に init API へ渡し、未初期化のまま読み出さない。
        let mut options: WebPAnimDecoderOptions = unsafe { std::mem::zeroed() };
        let demux_abi = WebPGetDemuxABIVersion();
        // SAFETY: `options` は有効な書込み先で、ABI version は libwebp から取得したものを使う。
        let ok = unsafe { WebPAnimDecoderOptionsInitInternal(&mut options, demux_abi) };
        if ok == 0 {
            anyhow::bail!("WebPAnimDecoder options init failed");
        }
        options.color_mode = WEBP_CSP_MODE::MODE_RGBA;
        options.use_threads = 1;

        let webp_data = WebPData {
            bytes: data.as_ptr(),
            size: data.len(),
        };
        // SAFETY:
        // `webp_data.bytes` は `data` の所有期間中ずっと生存し、`Self` が `_data` で保持する。
        // `options` は初期化済みで、この呼び出し中生存する。
        let decoder = unsafe { WebPAnimDecoderNewInternal(&webp_data, &options, demux_abi) };
        if decoder.is_null() {
            anyhow::bail!("WebPAnimDecoder creation failed");
        }

        // SAFETY: C struct は libwebp が全フィールドを書き込む出力バッファとしてだけ使う。
        let mut info: WebPAnimInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `decoder` は null でない生成成功値、`info` は有効な出力先。
        let ok = unsafe { WebPAnimDecoderGetInfo(decoder, &mut info) };
        if ok == 0 {
            // SAFETY: 生成成功した `decoder` を early return 前にここで解放する。
            unsafe { WebPAnimDecoderDelete(decoder) };
            anyhow::bail!("WebPAnimDecoder info failed");
        }

        Ok(Self {
            _data: data,
            decoder,
            info,
            prev_timestamp: 0,
            emitted_frames: 0,
        })
    }

    pub fn frame_count(&self) -> u32 {
        self.info.frame_count
    }

    pub fn has_more_frames(&self) -> bool {
        // SAFETY: `decoder` は `Self` の生存中だけ保持され、Drop まで解放しない。
        unsafe { WebPAnimDecoderHasMoreFrames(self.decoder) > 0 }
    }

    pub fn next_frame(&mut self) -> Result<Option<FrameData>> {
        if !self.has_more_frames() {
            return Ok(None);
        }

        let bytes_per_frame = (self.info.canvas_width as usize)
            .checked_mul(self.info.canvas_height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("WebP animation frame size overflow")?;
        let mut buf: *mut u8 = std::ptr::null_mut();
        let mut timestamp: i32 = 0;
        // SAFETY: `decoder` は有効で、`buf` / `timestamp` は libwebp への出力先。
        let ok = unsafe { WebPAnimDecoderGetNext(self.decoder, &mut buf, &mut timestamp) };
        if ok == 0 || buf.is_null() {
            anyhow::bail!("WebPAnimDecoder get next frame failed");
        }

        // SAFETY:
        // `buf` は libwebp が返した RGBA バッファ先頭で、サイズは checked 計算した
        // canvas 幅高 × 4 bytes/pixel。
        // ここでは即 `Vec` へコピーし、借用を持ち出さない。
        let pixels = unsafe { std::slice::from_raw_parts(buf, bytes_per_frame) }.to_vec();
        let raw_delay = if self.emitted_frames == 0 {
            timestamp
        } else {
            timestamp.saturating_sub(self.prev_timestamp)
        };
        self.prev_timestamp = timestamp;
        self.emitted_frames += 1;

        Ok(Some(FrameData {
            image: DecodedImage {
                width: self.info.canvas_width,
                height: self.info.canvas_height,
                pixels,
            },
            delay_ms: raw_delay.max(MIN_FRAME_DELAY_MS as i32) as u32,
        }))
    }

    pub fn decode_chunk(&mut self, frame_limit: usize) -> Result<AnimationFrameChunk> {
        let limit = frame_limit.max(1);
        let mut frames = Vec::with_capacity(limit.min(self.info.frame_count as usize));
        while frames.len() < limit {
            let Some(frame) = self.next_frame()? else {
                break;
            };
            frames.push(frame);
        }

        Ok(AnimationFrameChunk {
            exhausted: !self.has_more_frames(),
            width: self.info.canvas_width,
            height: self.info.canvas_height,
            frame_count: self.info.frame_count,
            frames,
        })
    }
}

// ── 単フレームデコード ────────────────────────────────────────────────────────

/// 生バイト列を RGBA8 にデコード（最初のフレームのみ）。
/// magic number で実際のフォーマットを確定し、hint は補助的に使う。
pub fn decode(data: &[u8], hint: ImageFormatHint) -> Result<DecodedImage> {
    let fmt = resolve_fmt(data, hint);
    match fmt {
        ImageFormatHint::Jpeg => decode_jpeg(data),
        ImageFormatHint::Png => decode_png_static(data),
        ImageFormatHint::WebP => {
            if let Ok(decoded) = decode_webp(data) {
                // 静止画 WebP: webp crate（高速）
                Ok(decoded)
            } else {
                // アニメーション WebP または静止画フォールバック:
                // decode_webp_frames は全フレームをデコードするためサムネイル・単フレーム用途には不適。
                // image::load_from_memory は先頭フレームのみデコードするため高速・省メモリ。
                decode_generic(data)
            }
        }
        ImageFormatHint::Avif => decode_avif_static(data),
        _ => decode_generic(data),
    }
}

/// サムネイル専用デコード。`target_width` を渡すことで縮小デコードを活用する。
///
/// - JPEG + `mozjpeg` feature: DCT 縮小デコード（1/8・1/4・1/2 から最適スケール選択）
/// - それ以外: `decode()` と同じ挙動
///
/// 出力はまだ `target_width` と一致するとは限らない（`resize_to_width` で仕上げる）。
pub fn decode_for_thumb(
    data: &[u8],
    hint: ImageFormatHint,
    target_width: u32,
) -> Result<DecodedImage> {
    let fmt = resolve_fmt(data, hint);

    // mozjpeg feature が有効な場合は JPEG を DCT 縮小デコード
    #[cfg(feature = "mozjpeg")]
    if matches!(fmt, ImageFormatHint::Jpeg) {
        return decode_jpeg_mozjpeg_with_fallback(data, target_width, &ViewerQuality::Balanced);
    }

    // mozjpeg なし、または非 JPEG → 通常デコード（target_width は resize_to_width に委ねる）
    let _ = target_width;
    match fmt {
        ImageFormatHint::Jpeg => decode_jpeg(data),
        ImageFormatHint::Avif => decode_avif_static(data),
        ImageFormatHint::WebP => {
            if let Ok(decoded) = decode_webp(data) {
                Ok(decoded)
            } else {
                decode_generic(data)
            }
        }
        _ => decode_generic(data),
    }
}

// ── 全フレームデコード（アニメーション対応）──────────────────────────────────

/// 全フレームをデコードして返す。
/// - 静止画: 1 要素のベクタ（delay_ms = 0）
/// - GIF / アニメ WebP / APNG: 全フレームを delay 付きで返す
pub fn decode_frames(data: &[u8], hint: ImageFormatHint) -> Result<Vec<FrameData>> {
    let fmt = resolve_fmt(data, hint);
    match fmt {
        ImageFormatHint::Gif => decode_gif_frames(data),
        ImageFormatHint::WebP => decode_webp_frames(data),
        ImageFormatHint::Png => decode_png_frames(data),
        _ => {
            // JPEG / AVIF / Unknown: 静止画として 1 フレーム返す
            let image = decode(data, fmt)?;
            Ok(vec![FrameData {
                image,
                delay_ms: STATIC_FRAME_DELAY_MS,
            }])
        }
    }
}

// ── フォーマット解決 ──────────────────────────────────────────────────────────

fn resolve_fmt(data: &[u8], hint: ImageFormatHint) -> ImageFormatHint {
    if data.len() >= 12 {
        let m = ImageFormatHint::from_magic(data);
        if m != ImageFormatHint::Unknown {
            m
        } else {
            hint
        }
    } else {
        hint
    }
}

// ── JPEG ──────────────────────────────────────────────────────────────────────

/// JPEG → RGBA8（zune-jpeg: image crate より高速）
fn decode_jpeg(data: &[u8]) -> Result<DecodedImage> {
    use std::io::Cursor;

    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut dec = JpegDecoder::new_with_options(Cursor::new(data), opts);
    dec.decode_headers()
        .map_err(|e| anyhow::anyhow!("JPEG header: {e:?}"))?;
    let info = dec.info().context("JPEG info missing")?;
    let rgb = dec
        .decode()
        .map_err(|e| anyhow::anyhow!("JPEG decode: {e:?}"))?;

    let w = info.width as usize;
    let h = info.height as usize;

    if rgb.len() != w * h * 3 {
        tracing::warn!(
            "zune-jpeg buffer mismatch: got {} bytes, expected {}×{}×3={} — fallback to image crate",
            rgb.len(), w, h, w * h * 3
        );
        return decode_generic(data);
    }

    let pixels: Vec<u8> = rgb
        .chunks_exact(3)
        .flat_map(|c| [c[0], c[1], c[2], 255u8])
        .collect();

    Ok(DecodedImage {
        width: w as u32,
        height: h as u32,
        pixels,
    })
}

// ── WebP（静止画）────────────────────────────────────────────────────────────

/// WebP static → RGBA8（webp crate: 純 Rust、高速）
pub fn decode_webp(data: &[u8]) -> Result<DecodedImage> {
    let decoder = webp::Decoder::new(data);
    let img = decoder.decode().context("WebP static decode failed")?;
    let (w, h) = (img.width(), img.height());

    let pixels: Vec<u8> = if img.is_alpha() {
        img.to_vec()
    } else {
        img.chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255u8])
            .collect()
    };

    Ok(DecodedImage {
        width: w,
        height: h,
        pixels,
    })
}

// ── PNG / GIF / AVIF / その他 ─────────────────────────────────────────────────

/// image crate を使って汎用デコード（PNG / GIF / AVIF など）
fn decode_generic(data: &[u8]) -> Result<DecodedImage> {
    let img = image::load_from_memory(data).context("image decode")?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(DecodedImage {
        width: w,
        height: h,
        pixels: rgba.into_raw(),
    })
}

fn decode_png_static(data: &[u8]) -> Result<DecodedImage> {
    decode_generic(data)
}

fn decode_avif_static(data: &[u8]) -> Result<DecodedImage> {
    decode_generic(data).with_context(|| {
        "AVIF decode failed: AVIF decoder is not available or image data is unsupported by image crate"
    })
}

// ── フレームデコード実装 ──────────────────────────────────────────────────────

fn decode_gif_frames(data: &[u8]) -> Result<Vec<FrameData>> {
    use image::codecs::gif::GifDecoder;
    use image::AnimationDecoder as _;

    // 現在の Viewer はフレーム列全体を保持して再生するため、GIF は全フレームを
    // フルサイズで収集してから表示サイズへ縮小する。大判・長尺 GIF では一時メモリが
    // 大きくなり得る。上限を設ける場合は、途中フレームを欠かさない逐次デコードと
    // 再生キャッシュの設計を GIF/APNG 共通で導入すること。
    let decoder = GifDecoder::new(std::io::Cursor::new(data)).context("GIF decoder")?;
    let frames: Vec<_> = decoder
        .into_frames()
        .collect::<image::ImageResult<Vec<_>>>()
        .map_err(|e| anyhow::anyhow!("GIF frames: {e}"))?;

    if frames.is_empty() {
        let image = decode_generic(data)?;
        return Ok(vec![FrameData {
            image,
            delay_ms: STATIC_FRAME_DELAY_MS,
        }]);
    }
    Ok(frames.into_iter().map(image_frame_to_frame_data).collect())
}

fn decode_webp_frames(data: &[u8]) -> Result<Vec<FrameData>> {
    decode_webp_frames_with_libwebp(data, None)
}

fn decode_webp_frames_with_libwebp(
    data: &[u8],
    frame_limit: Option<usize>,
) -> Result<Vec<FrameData>> {
    if !is_animated_webp_fast(data) {
        let image = decode_webp(data)?;
        return Ok(vec![FrameData {
            image,
            delay_ms: STATIC_FRAME_DELAY_MS,
        }]);
    }

    let mut source = WebpAnimFrameSource::new(data.to_vec())?;
    let frame_limit = frame_limit.unwrap_or(source.frame_count() as usize);
    let chunk = source.decode_chunk(frame_limit)?;

    if chunk.frames.is_empty() {
        let image = decode_webp(data)?;
        return Ok(vec![FrameData {
            image,
            delay_ms: STATIC_FRAME_DELAY_MS,
        }]);
    }

    Ok(chunk.frames)
}

pub fn is_animated_webp(data: &[u8]) -> Result<bool> {
    use image::codecs::webp::WebPDecoder;

    let decoder = WebPDecoder::new(std::io::Cursor::new(data)).context("WebP decoder")?;
    Ok(decoder.has_animation())
}

pub fn is_animated_webp_fast(data: &[u8]) -> bool {
    if data.len() < RIFF_HEADER_LEN + RIFF_CHUNK_HEADER_LEN + 1 {
        return false;
    }
    if &data[0..4] != b"RIFF" || &data[8..RIFF_HEADER_LEN] != b"WEBP" {
        return false;
    }

    let mut offset = RIFF_HEADER_LEN;
    while offset + RIFF_CHUNK_HEADER_LEN <= data.len() {
        let chunk = &data[offset..offset + 4];
        let chunk_len = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;
        let payload = offset + RIFF_CHUNK_HEADER_LEN;
        if payload > data.len() {
            return false;
        }
        if chunk == b"VP8X" {
            if payload >= data.len() {
                return false;
            }
            return data[payload] & WEBP_ANIMATION_FLAG != 0;
        }

        let padded = (chunk_len + 1) & !1;
        offset = payload.saturating_add(padded);
    }

    false
}

fn decode_png_frames(data: &[u8]) -> Result<Vec<FrameData>> {
    use image::codecs::png::PngDecoder;
    use image::AnimationDecoder as _;

    let decoder = PngDecoder::new(std::io::Cursor::new(data)).context("PNG decoder")?;

    // is_apng() は ImageResult<bool> を返す
    if decoder.is_apng().unwrap_or(false) {
        // GIF と同様に、現在は APNG の全フレームをフルサイズで収集してから縮小する。
        // メモリ上限を導入する場合は、正常なアニメーションを途中で切らないよう、
        // GIF と共通の逐次デコード・再生キャッシュへ移行すること。
        let apng = decoder.apng().context("APNG")?;
        let frames: Vec<_> = apng
            .into_frames()
            .collect::<image::ImageResult<Vec<_>>>()
            .map_err(|e| anyhow::anyhow!("APNG frames: {e}"))?;
        if frames.len() > 1 {
            return Ok(frames.into_iter().map(image_frame_to_frame_data).collect());
        }
    }

    // 静止画にフォールバック
    let image = decode_png_static(data)?;
    Ok(vec![FrameData {
        image,
        delay_ms: STATIC_FRAME_DELAY_MS,
    }])
}

fn is_apng(data: &[u8]) -> Result<bool> {
    use image::codecs::png::PngDecoder;

    let decoder = PngDecoder::new(std::io::Cursor::new(data)).context("PNG decoder")?;
    Ok(decoder.is_apng().unwrap_or(false))
}

/// image::Frame → FrameData 変換（共通）
fn image_frame_to_frame_data(f: image::Frame) -> FrameData {
    let (numer, denom) = f.delay().numer_denom_ms();
    let delay_ms = if denom == 0 {
        FALLBACK_FRAME_DELAY_MS
    } else {
        (numer / denom).max(MIN_ANIMATION_DELAY_MS)
    };
    let rgba = f.into_buffer();
    let (w, h) = rgba.dimensions();
    FrameData {
        image: DecodedImage {
            width: w,
            height: h,
            pixels: rgba.into_raw(),
        },
        delay_ms,
    }
}

// ── mozjpeg 縮小デコード ──────────────────────────────────────────────────────

/// JPEG → RGBA8（mozjpeg DCT 縮小デコード）
///
/// `target_width` から最適なスケール分子（分母=8）を選択してデコードする。
/// 出力サイズはスケール後の近似値であり、`target_width` と一致しない場合がある。
/// 呼び出し側で `resize_to_width` により最終サイズに揃える。
///
/// スケール選択（分母 8 固定）:
/// - orig ≥ 8×target → 1/8（最大縮小、最速）
/// - orig ≥ 4×target → 1/4
/// - orig ≥ 2×target → 1/2
/// - それ以外        → 1/1（フルサイズ）
#[cfg(feature = "mozjpeg")]
fn decode_jpeg_mozjpeg_with_fallback(
    data: &[u8],
    target_width: u32,
    quality: &ViewerQuality,
) -> Result<DecodedImage> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode_jpeg_mozjpeg(data, target_width, quality)
    })) {
        Ok(Ok(img)) => Ok(img),
        Ok(Err(e)) => {
            tracing::debug!(
                error = %e,
                "mozjpeg decode failed; fallback to image crate"
            );
            decode_generic(data)
        }
        Err(_) => {
            tracing::debug!("mozjpeg decode panicked; fallback to image crate");
            decode_generic(data)
        }
    }
}

#[cfg(feature = "mozjpeg")]
fn decode_jpeg_mozjpeg(
    data: &[u8],
    target_width: u32,
    quality: &ViewerQuality,
) -> Result<DecodedImage> {
    let mut decomp = mozjpeg::Decompress::new_mem(data).context("mozjpeg open")?;

    let orig_w = decomp.width() as u32;
    let numerator = jpeg_scale_numerator(orig_w, target_width, quality);

    // numerator < 8 のときのみ縮小設定（== 8 はデフォルトと同じなので省略可だが明示）
    decomp.scale(numerator);

    let mut started = decomp.rgb().context("mozjpeg start rgb")?;

    let w = started.width() as u32;
    let h = started.height() as u32;

    let rgb: Vec<[u8; 3]> = started.read_scanlines().context("mozjpeg read scanlines")?;
    started.finish().context("mozjpeg finish")?;

    let pixels: Vec<u8> = rgb.iter().flat_map(|p| [p[0], p[1], p[2], 255u8]).collect();

    Ok(DecodedImage {
        width: w,
        height: h,
        pixels,
    })
}

/// target_width に対して最適な mozjpeg スケール分子を返す（分母は 8 固定）。
/// Speed / Balanced は DCT 縮小を使い、Quality / Original はフルデコードする。
/// 戻り値: 1=1/8, 2=1/4, 4=1/2, 8=1/1
#[cfg(feature = "mozjpeg")]
fn jpeg_scale_numerator(orig_w: u32, target_w: u32, quality: &ViewerQuality) -> u8 {
    if matches!(quality, ViewerQuality::Quality | ViewerQuality::Original) {
        return 8;
    }
    if target_w == 0 || orig_w <= target_w {
        return 8;
    }
    let guard_percent = match quality {
        ViewerQuality::Speed => 0,
        ViewerQuality::Balanced => 20,
        _ => 0,
    };
    let guarded_target_w = target_w
        .saturating_add(
            ((target_w.saturating_mul(guard_percent)) / 100).max((guard_percent > 0) as u32),
        )
        .min(orig_w);
    let ratio = orig_w / guarded_target_w;
    if ratio >= 8 {
        1
    } else if ratio >= 4 {
        2
    } else if ratio >= 2 {
        4
    } else {
        8
    }
}

// ── リサイズ ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum ResizeFilter {
    Bilinear,
    Lanczos3,
}

/// RGBA8 ピクセルを target_width に縮小（アスペクト比維持）。
pub fn resize_to_width(decoded: DecodedImage, target_width: u32) -> Result<DecodedImage> {
    resize_to_width_with_filter(decoded, target_width, ResizeFilter::Bilinear)
}

fn resize_to_width_with_filter(
    decoded: DecodedImage,
    target_width: u32,
    filter: ResizeFilter,
) -> Result<DecodedImage> {
    use fast_image_resize::images::Image as FirImage;
    use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

    if decoded.width == 0 || decoded.height == 0 || target_width == 0 {
        return Ok(decoded);
    }
    if decoded.width <= target_width {
        return Ok(decoded);
    }

    let target_h =
        ((decoded.height as u64 * target_width as u64) / decoded.width as u64).max(1) as u32;

    let src = FirImage::from_vec_u8(
        decoded.width,
        decoded.height,
        decoded.pixels,
        PixelType::U8x4,
    )
    .map_err(|e| anyhow::anyhow!("fir src: {e}"))?;
    let mut dst = FirImage::new(target_width, target_h, PixelType::U8x4);

    let filter_type = match filter {
        ResizeFilter::Bilinear => FilterType::Bilinear,
        ResizeFilter::Lanczos3 => FilterType::Lanczos3,
    };
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter_type));
    Resizer::new()
        .resize(&src, &mut dst, &opts)
        .map_err(|e| anyhow::anyhow!("fir resize: {e}"))?;

    Ok(DecodedImage {
        width: target_width,
        height: target_h,
        pixels: dst.into_vec(),
    })
}

// ── ビューア専用デコード ──────────────────────────────────────────────────────

/// ビューア表示用に画像をデコードし、全フレームと元サイズを返す。
///
/// - **JPEG**: `mozjpeg` feature 有効時は画質ごとの target / DCT 縮小方針に従う
///   → フルデコードより高速化できるモードを優先
/// - **PNG / WebP / GIF / APNG など**: `decode_frames()` でデコード後に `max_tex_side` でキャップ
/// - すべてのフォーマットで `max_tex_side` を超えるフレームはリサイズ（GPU panic 防止）
/// - `ViewerFrames.orig_w/orig_h` にリサイズ前の元サイズを格納
///
/// `display_w = 0` の場合はスケーリングをスキップしてフルデコード。
pub fn decode_for_viewer_frames(
    data: &[u8],
    hint: ImageFormatHint,
    display_w: u32,
    _display_h: u32,
    quality: ViewerQuality,
    max_tex_side: u32,
) -> Result<ViewerFrames> {
    let fmt = resolve_fmt(data, hint);
    let cap = if max_tex_side > 0 {
        max_tex_side
    } else {
        DEFAULT_MAX_TEXTURE_SIDE
    };
    let target = effective_viewer_target_side(display_w, cap);

    match fmt {
        ImageFormatHint::Jpeg => decode_jpeg_for_viewer(data, display_w, quality, cap),
        ImageFormatHint::WebP => {
            if is_animated_webp(data).unwrap_or(false) {
                decode_animated_for_viewer_frames(data, fmt, target)
            } else {
                decode_webp_for_viewer(data, target, cap, quality)
            }
        }
        ImageFormatHint::Avif => decode_avif_for_viewer(data, target, cap, quality),
        ImageFormatHint::Png => {
            if is_apng(data).unwrap_or(false) {
                decode_animated_for_viewer_frames(data, fmt, target)
            } else {
                decode_png_for_viewer(data, target, cap, quality)
            }
        }
        ImageFormatHint::Gif => decode_animated_for_viewer_frames(data, fmt, target),
        _ => decode_generic_for_viewer(data, fmt, target, cap, quality),
    }
}

/// JPEG ビューア専用デコード（JPEG のみ呼ぶ）。
fn decode_jpeg_for_viewer(
    data: &[u8],
    display_w: u32,
    quality: ViewerQuality,
    cap: u32,
) -> Result<ViewerFrames> {
    // 元サイズをヘッダーのみ読んで取得（デコードなし: 高速）
    let (orig_w, orig_h) = read_jpeg_size(data).unwrap_or((0, 0));
    let target = match quality {
        ViewerQuality::Original if orig_w > 0 && orig_h > 0 => {
            safe_original_target_side(orig_w, orig_h, cap)
        }
        _ => effective_viewer_target_side(display_w, cap),
    };
    let resize_filter = jpeg_final_resize_filter(&quality);

    // mozjpeg feature 有効時: 画質ごとの DCT 縮小方針に従ってデコード
    #[cfg(feature = "mozjpeg")]
    let img = decode_jpeg_mozjpeg_with_fallback(data, target, &quality)?;

    // mozjpeg なし: 通常デコード
    #[cfg(not(feature = "mozjpeg"))]
    let img = decode_jpeg(data)?;

    let img = resize_to_max_side_with_filter(img, target, resize_filter)?;
    Ok(ViewerFrames {
        orig_w,
        orig_h,
        frames: vec![FrameData {
            image: img,
            delay_ms: STATIC_FRAME_DELAY_MS,
        }],
    })
}

fn decode_webp_for_viewer(
    data: &[u8],
    target: u32,
    cap: u32,
    quality: ViewerQuality,
) -> Result<ViewerFrames> {
    let img = decode_webp(data)?;
    decode_static_image_for_viewer(img, target, cap, &quality)
}

fn decode_avif_for_viewer(
    data: &[u8],
    target: u32,
    cap: u32,
    quality: ViewerQuality,
) -> Result<ViewerFrames> {
    let img = decode_avif_static(data).with_context(|| "AVIF decode failed in viewer path")?;
    decode_static_image_for_viewer(img, target, cap, &quality)
}

fn decode_png_for_viewer(
    data: &[u8],
    target: u32,
    cap: u32,
    quality: ViewerQuality,
) -> Result<ViewerFrames> {
    let _total_started = Instant::now();

    // PNG-1: static PNG のボトルネック切り分け用ログ。
    // image::load_from_memory は PNG inflate + filter 復元 + DynamicImage 生成を含む。
    let decode_started = Instant::now();
    let dyn_img = image::load_from_memory(data).context("PNG image decode")?;
    let _decode_ms = decode_started.elapsed().as_millis();

    // DynamicImage -> RGBA8 変換コストを分離して計測する。
    let rgba_started = Instant::now();
    let rgba = dyn_img.to_rgba8();
    let _rgba_ms = rgba_started.elapsed().as_millis();
    let (orig_w, orig_h) = rgba.dimensions();

    let mut img = DecodedImage {
        width: orig_w,
        height: orig_h,
        pixels: rgba.into_raw(),
    };

    // PNG-2: static raster 向け viewer quality pipeline。
    // Speed / Balanced は安全な場合のみ 2x2 平均の事前 1/2 縮小を使い、
    // Quality / Original は事前縮小なしでそのまま最終リサイズへ進む。
    let prescale_started = Instant::now();
    let before_w = img.width;
    let before_h = img.height;
    let quality_result = apply_static_raster_quality_for_viewer(img, target, cap, &quality)?;
    img = quality_result.image;
    if quality_result.prescale_applied {
        tracing::trace!(
            before_w,
            before_h,
            after_w = img.width,
            after_h = img.height,
            target,
            quality = ?quality,
            prescale_mode = quality_result.prescale_mode,
            prescale_ms = prescale_started.elapsed().as_millis(),
            "viewer_loader: png prescale applied"
        );
    }
    let _resize_filter = quality_result.resize_filter;

    Ok(ViewerFrames {
        orig_w,
        orig_h,
        frames: vec![FrameData {
            image: img,
            delay_ms: STATIC_FRAME_DELAY_MS,
        }],
    })
}

#[derive(Clone, Copy, Debug)]
enum StaticRasterPrescaleHalfMode {
    Average2x2,
}

impl StaticRasterPrescaleHalfMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Average2x2 => "average2x2",
        }
    }
}

struct StaticRasterViewerQualityResult {
    image: DecodedImage,
    prescale_applied: bool,
    prescale_mode: &'static str,
    resize_filter: ResizeFilter,
}

fn static_raster_resize_filter_for_viewer(quality: &ViewerQuality) -> ResizeFilter {
    match quality {
        ViewerQuality::Speed => ResizeFilter::Bilinear,
        ViewerQuality::Balanced | ViewerQuality::Quality | ViewerQuality::Original => {
            ResizeFilter::Lanczos3
        }
    }
}

/// PNG / 静止ラスターの事前 1/2 縮小の方式を返す。
///
/// Speed / Balanced のみ対象。半分にしても最終 target を下回らない場合だけ許可する。
/// Quality / Original は事前縮小しない。
fn static_raster_prescale_half_mode_for_viewer(
    width: u32,
    height: u32,
    max_side: u32,
    quality: &ViewerQuality,
) -> Option<StaticRasterPrescaleHalfMode> {
    if width < 2 || height < 2 || max_side == 0 {
        return None;
    }
    if width <= max_side && height <= max_side {
        return None;
    }

    let target_w = if width >= height {
        max_side
    } else {
        ((width as u64 * max_side as u64) / height as u64).max(1) as u32
    };
    let target_h = if height >= width {
        max_side
    } else {
        ((height as u64 * max_side as u64) / width as u64).max(1) as u32
    };

    match quality {
        ViewerQuality::Original | ViewerQuality::Quality => None,
        ViewerQuality::Speed | ViewerQuality::Balanced => {
            if (width / 2) >= target_w && (height / 2) >= target_h {
                Some(StaticRasterPrescaleHalfMode::Average2x2)
            } else {
                None
            }
        }
    }
}

fn apply_static_raster_quality_for_viewer(
    mut decoded: DecodedImage,
    display_target_side: u32,
    max_tex_side: u32,
    quality: &ViewerQuality,
) -> Result<StaticRasterViewerQualityResult> {
    let target_max_side = static_raster_target_max_side_for_viewer(
        display_target_side,
        decoded.width,
        decoded.height,
        max_tex_side,
        quality,
    );
    let prescale_mode = static_raster_prescale_half_mode_for_viewer(
        decoded.width,
        decoded.height,
        target_max_side,
        quality,
    );
    if let Some(StaticRasterPrescaleHalfMode::Average2x2) = prescale_mode {
        decoded = prescale_rgba_half_average(decoded)?;
    }

    let resize_filter = static_raster_resize_filter_for_viewer(quality);
    let _resize_started = Instant::now();
    let image = resize_to_max_side_with_filter(decoded, target_max_side, resize_filter)?;

    Ok(StaticRasterViewerQualityResult {
        image,
        prescale_applied: prescale_mode.is_some(),
        prescale_mode: prescale_mode
            .map(StaticRasterPrescaleHalfMode::as_str)
            .unwrap_or("none"),
        resize_filter,
    })
}

fn static_raster_target_max_side_for_viewer(
    display_target_side: u32,
    src_w: u32,
    src_h: u32,
    max_tex_side: u32,
    quality: &ViewerQuality,
) -> u32 {
    match quality {
        ViewerQuality::Original => safe_original_target_side(src_w, src_h, max_tex_side),
        _ => display_target_side.max(1),
    }
}

/// RGBA8 を 2x2 平均で 1/2 縮小する。
///
/// PNG decode 後の巨大 RGBA に対する後段 resize 負荷を下げるための軽量な事前縮小。
/// odd サイズの場合は最後の 1px 行/列を切り捨てるが、呼び出し側で最終サイズを
/// 下回らない条件を確認してから使う。
fn prescale_rgba_half_average(decoded: DecodedImage) -> Result<DecodedImage> {
    if decoded.width < 2 || decoded.height < 2 {
        return Ok(decoded);
    }

    let src_w = decoded.width as usize;
    let src_h = decoded.height as usize;
    let dst_w = src_w / 2;
    let dst_h = src_h / 2;
    let mut pixels = vec![0u8; dst_w * dst_h * 4];

    for y in 0..dst_h {
        let sy = y * 2;
        for x in 0..dst_w {
            let sx = x * 2;
            let dst = (y * dst_w + x) * 4;
            for c in 0..4 {
                let p00 = decoded.pixels[((sy * src_w + sx) * 4) + c] as u16;
                let p01 = decoded.pixels[((sy * src_w + sx + 1) * 4) + c] as u16;
                let p10 = decoded.pixels[(((sy + 1) * src_w + sx) * 4) + c] as u16;
                let p11 = decoded.pixels[(((sy + 1) * src_w + sx + 1) * 4) + c] as u16;
                pixels[dst + c] = ((p00 + p01 + p10 + p11 + 2) / 4) as u8;
            }
        }
    }

    Ok(DecodedImage {
        width: dst_w as u32,
        height: dst_h as u32,
        pixels,
    })
}

fn decode_generic_for_viewer(
    data: &[u8],
    fmt: ImageFormatHint,
    target: u32,
    cap: u32,
    quality: ViewerQuality,
) -> Result<ViewerFrames> {
    let img = decode(data, fmt)?;
    decode_static_image_for_viewer(img, target, cap, &quality)
}

fn decode_animated_for_viewer_frames(
    data: &[u8],
    fmt: ImageFormatHint,
    target: u32,
) -> Result<ViewerFrames> {
    let frames = decode_frames(data, fmt)?;
    let (orig_w, orig_h) = frames
        .first()
        .map(|f| (f.image.width, f.image.height))
        .unwrap_or((0, 0));
    let frames = frames
        .into_iter()
        .map(|f| -> Result<FrameData> {
            let image = resize_to_max_side(f.image, target)?;
            Ok(FrameData {
                image,
                delay_ms: f.delay_ms,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ViewerFrames {
        frames,
        orig_w,
        orig_h,
    })
}

fn decode_static_image_for_viewer(
    img: DecodedImage,
    target: u32,
    cap: u32,
    quality: &ViewerQuality,
) -> Result<ViewerFrames> {
    let orig_w = img.width;
    let orig_h = img.height;
    let quality_result = apply_static_raster_quality_for_viewer(img, target, cap, quality)?;
    let img = quality_result.image;
    Ok(ViewerFrames {
        orig_w,
        orig_h,
        frames: vec![FrameData {
            image: img,
            delay_ms: STATIC_FRAME_DELAY_MS,
        }],
    })
}

/// JPEG ヘッダーのみを読んで元画像サイズを返す（デコードなし・高速）。
fn read_jpeg_size(data: &[u8]) -> Result<(u32, u32)> {
    crate::infra::image::page_map::read_jpeg_metadata(data).map(|(_, w, h)| (w, h))
}

/// `display_w` と GPU 上限 `cap` の両方を考慮した有効ターゲット幅を返す。
/// display_w = 0 のときはキャップのみ適用。
fn effective_target(display_w: u32, cap: u32) -> u32 {
    if display_w > 0 {
        display_w.min(cap)
    } else {
        cap
    }
}

/// 表示幅と GPU 上限から、ビューア向けの target 辺を返す。
fn effective_viewer_target_side(display_w: u32, cap: u32) -> u32 {
    effective_target(display_w, cap).max(1)
}

fn jpeg_final_resize_filter(quality: &ViewerQuality) -> ResizeFilter {
    match quality {
        ViewerQuality::Speed => ResizeFilter::Bilinear,
        ViewerQuality::Balanced | ViewerQuality::Quality | ViewerQuality::Original => {
            ResizeFilter::Lanczos3
        }
    }
}

const ORIGINAL_BG_RGBA_MAX_BYTES: u64 = 128 * 1024 * 1024;
const ORIGINAL_BG_RGBA_MAX_PIXELS: u64 = ORIGINAL_BG_RGBA_MAX_BYTES / 4;

fn safe_original_target_side(orig_w: u32, orig_h: u32, cap: u32) -> u32 {
    if orig_w == 0 || orig_h == 0 {
        return cap.max(1);
    }

    let upper = cap.max(1).min(orig_w.max(orig_h).max(1));
    let mut lo = 1u32;
    let mut hi = upper;
    let mut best = 1u32;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        if safe_original_target_fits(orig_w, orig_h, mid, cap) {
            best = mid;
            lo = mid.saturating_add(1);
        } else {
            hi = mid.saturating_sub(1);
        }
    }
    best.max(1)
}

fn safe_original_target_fits(orig_w: u32, orig_h: u32, target_side: u32, cap: u32) -> bool {
    let (w, h) = resized_dims_for_max_side(orig_w, orig_h, target_side);
    let cap = cap.max(1);
    let pixels = (w as u128).saturating_mul(h as u128);
    let rgba_bytes = pixels.saturating_mul(4);
    w <= cap
        && h <= cap
        && pixels <= ORIGINAL_BG_RGBA_MAX_PIXELS as u128
        && rgba_bytes <= ORIGINAL_BG_RGBA_MAX_BYTES as u128
}

fn resized_dims_for_max_side(orig_w: u32, orig_h: u32, target_side: u32) -> (u32, u32) {
    if orig_w == 0 || orig_h == 0 || target_side == 0 {
        return (0, 0);
    }
    if orig_w >= orig_h {
        let w = target_side.min(orig_w);
        let h = ((orig_h as u64 * w as u64) / orig_w as u64).max(1) as u32;
        (w, h)
    } else {
        let h = target_side.min(orig_h);
        let w = ((orig_w as u64 * h as u64) / orig_h as u64).max(1) as u32;
        (w, h)
    }
}

/// 最大辺が `max_side` を超える場合にアスペクト比を維持しながら縮小する。
///
/// GPU テクスチャサイズ上限（`wgpu::Limits::max_texture_dimension_2d`、通常 8192）を
/// 超えると `ctx.load_texture()` が panic するため、アップロード前に呼ぶ。
/// 上限以下の画像はそのまま返す（コピーなし）。
pub fn resize_to_max_side(decoded: DecodedImage, max_side: u32) -> Result<DecodedImage> {
    resize_to_max_side_with_filter(decoded, max_side, ResizeFilter::Bilinear)
}

fn resize_to_max_side_with_filter(
    decoded: DecodedImage,
    max_side: u32,
    filter: ResizeFilter,
) -> Result<DecodedImage> {
    if decoded.width == 0 || decoded.height == 0 || max_side == 0 {
        return Ok(decoded);
    }
    if decoded.width <= max_side && decoded.height <= max_side {
        return Ok(decoded);
    }
    // 長辺を max_side にするための target_width を計算
    let target_w = if decoded.width >= decoded.height {
        max_side
    } else {
        // 高さが長辺: 高さ → max_side になるときの幅を求める
        ((decoded.width as u64 * max_side as u64) / decoded.height as u64).max(1) as u32
    };
    resize_to_width_with_filter(decoded, target_w, filter)
}

// ── WebP エンコード ───────────────────────────────────────────────────────────

/// RGBA8 → WebP lossy (quality=80)。サムネイルキャッシュ用。
pub fn encode_webp(decoded: &DecodedImage) -> Result<Vec<u8>> {
    let encoder = webp::Encoder::from_rgba(&decoded.pixels, decoded.width, decoded.height);
    let webp = encoder.encode(80.0);
    Ok(webp.to_vec())
}
