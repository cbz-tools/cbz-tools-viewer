//! mmap ベースの ZipReader。
//!
//! - open 時に中央ディレクトリだけを読み、エントリ一覧を保持する
//! - `read` は `&self` なので並列呼出しできる
//! - ZIP64 に対応する
//! - Stored / Deflate を扱う
use std::{
    io::{Seek, Write},
    path::Path,
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use flate2::read::DeflateDecoder;
use memmap2::Mmap;
use std::io::Read as _;
// encoding_rs: Shift-JIS (CP932) ZIP ファイル名のデコードに使用

use crate::domain::page_map::PageImageFormat;
use crate::infra::image::page_map::{JpegMetadataProbe, MetadataProbeResult, probe_png_metadata};
use crate::util::{archive_path::is_supported_image_name, natural_sort};

use super::{
    BookReader, CbzRebuildArchiveEntry, CbzRebuildArchiveEntryKind,
    page_map::{
        ZipPageMapIssueReason, ZipPageMapSlowFailureReason, ZipPageMapSlowReason,
        page_map_format_for_name,
    },
    write_cbz_rebuild_directory_entry, write_cbz_rebuild_file_entry,
};
use zip_writer::ZipWriter;

// ── 定数 ─────────────────────────────────────────────────────────────────────

const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_LOC64: u32 = 0x0706_4b50;
const SIG_CDIR: u32 = 0x0201_4b50;
const SIG_LOCAL: u32 = 0x0403_4b50;

const EOCD_MIN_LEN: usize = 22;
const CDIR_FIXED: usize = 46;
const LOCAL_FIXED: usize = 30;
const ZIP64_EXTRA_ID: u16 = 0x0001;

const U32_MAX_AS_64: u64 = 0xFFFF_FFFF;

// ── 内部型 ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum Compression {
    Stored,
    Deflate,
    Unsupported(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZipCompressionMethod {
    Stored,
    Deflate,
}

impl From<Compression> for ZipCompressionMethod {
    fn from(value: Compression) -> Self {
        match value {
            Compression::Stored => Self::Stored,
            Compression::Deflate => Self::Deflate,
            Compression::Unsupported(_) => Self::Stored,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ZipEntry {
    name: Arc<str>,
    compression: Compression,
    /// 中央ディレクトリが記録するローカルヘッダのオフセット
    /// 圧縮データの実オフセットは read_raw 時に local_data_offset() で遅延計算する
    lh_offset: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    is_dir: bool,
}

pub(crate) struct ZipArchiveCore {
    mmap: Arc<Mmap>,
    entries: Vec<ZipEntry>,
}

// ── ZipReader ─────────────────────────────────────────────────────────────────

pub struct ZipReader {
    core: ZipArchiveCore,
    image_entries: Vec<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct ZipImageEntryInfo<'a> {
    pub page_index: u32,
    pub entry_index: u32,
    pub name: &'a str,
    pub compression: ZipCompressionMethod,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ZipPageMapMetadataRead {
    pub format: PageImageFormat,
    pub width: u32,
    pub height: u32,
    pub used_lightweight: bool,
    pub compressed_bytes_touched: usize,
    pub uncompressed_bytes_produced: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum ZipPageMapReadOutcome {
    Ready(ZipPageMapMetadataRead),
    SlowRequired(ZipPageMapSlowReason),
    Failed(ZipPageMapIssueReason),
}

impl ZipArchiveCore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file =
            std::fs::File::open(path).with_context(|| format!("open: {}", path.display()))?;

        // Safety: ファイルは読み取り専用で開く。プロセス中に外部からの書き換えが
        // あった場合は UB になりうるが、memmap2 の通常用途として許容する。
        let mmap =
            unsafe { Mmap::map(&file) }.with_context(|| format!("mmap: {}", path.display()))?;

        let entries =
            parse_entries(&mmap).with_context(|| format!("parse ZIP: {}", path.display()))?;

        Ok(Self {
            mmap: Arc::new(mmap),
            entries,
        })
    }

    pub(crate) fn total_entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn entry(&self, index: usize) -> Option<&ZipEntry> {
        self.entries.get(index)
    }

    pub(crate) fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    pub(crate) fn find_entry_index_by_name(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.name.as_ref() == name)
    }

    pub(crate) fn read_entry_by_index(&self, index: usize) -> Result<Bytes> {
        let started = Instant::now();
        let e = self.entries.get(index).context("index out of range")?;
        // ローカルヘッダを読んで圧縮データの実オフセットを遅延計算
        // （parse 時でなくここで解決することで、壊れた1エントリが ZIP 全体を破壊しない）
        let data_off = local_data_offset(&self.mmap, e.lh_offset)
            .with_context(|| format!("local header for '{}'", e.name))?;
        let off = data_off as usize;
        let len = e.compressed_size as usize;
        let raw = self
            .mmap
            .get(off..off + len)
            .with_context(|| format!("mmap slice OOB: off={off} len={len}"))?;
        let decoded = decompress(raw, e)?;
        tracing::trace!(
            name = %e.name,
            compressed_bytes = e.compressed_size,
            uncompressed_bytes = decoded.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "zip_reader: read_raw complete"
        );
        Ok(decoded)
    }

    pub(crate) fn read_entry_by_name(&self, name: &str) -> Result<Bytes> {
        let index = self
            .find_entry_index_by_name(name)
            .with_context(|| format!("entry not found: {name}"))?;
        self.read_entry_by_index(index)
    }

    fn read_entry_by_index_for_page_map(
        &self,
        index: usize,
    ) -> std::result::Result<Bytes, ZipPageMapSlowFailureReason> {
        let e = self
            .entries
            .get(index)
            .ok_or(ZipPageMapSlowFailureReason::EntryReadError)?;
        let data_off = local_data_offset(&self.mmap, e.lh_offset)
            .map_err(|_| ZipPageMapSlowFailureReason::EntryReadError)?;
        let off = data_off as usize;
        let len = e.compressed_size as usize;
        let raw = self
            .mmap
            .get(off..off + len)
            .ok_or(ZipPageMapSlowFailureReason::EntryReadError)?;
        decompress(raw, e).map_err(|_| ZipPageMapSlowFailureReason::InflateError)
    }

    fn read_page_map_metadata_for_entry_index(
        &self,
        entry_idx: usize,
    ) -> Result<ZipPageMapReadOutcome> {
        let entry = self.entries.get(entry_idx).context("index out of range")?;
        let data_off = local_data_offset(&self.mmap, entry.lh_offset)
            .with_context(|| format!("local header for '{}'", entry.name))?;
        let raw = self
            .mmap
            .get(data_off as usize..data_off as usize + entry.compressed_size as usize)
            .with_context(|| {
                format!(
                    "mmap slice OOB: off={data_off} len={}",
                    entry.compressed_size
                )
            })?;
        match entry.compression {
            Compression::Stored => read_page_map_metadata_stored(entry, raw),
            Compression::Deflate => read_page_map_metadata_deflate(entry, raw),
            Compression::Unsupported(_) => Ok(ZipPageMapReadOutcome::SlowRequired(
                ZipPageMapSlowReason::UnsupportedLightweightFormat,
            )),
        }
    }
}

impl ZipReader {
    pub fn open(path: &Path) -> Result<Self> {
        let started = Instant::now();
        let core = ZipArchiveCore::open(path)?;
        let image_entries = build_image_entries(core.entries());

        let reader = Self {
            core,
            image_entries,
        };

        tracing::debug!(
            path = %path.display(),
            entry_count = reader.core.total_entry_count(),
            image_count = reader.page_count(),
            elapsed_ms = started.elapsed().as_millis(),
            "zip_reader: open complete"
        );

        Ok(reader)
    }

    /// 画像ページ数を返す（natural sort 順）
    pub fn page_count(&self) -> u32 {
        self.image_entries.len() as u32
    }

    pub(crate) fn page_display_labels(&self) -> Vec<String> {
        self.page_map_image_entry_infos()
            .map(|info| display_name_from_archive_entry(info.name))
            .collect()
    }

    pub(crate) fn page_entry_names(&self) -> Vec<String> {
        self.page_map_image_entry_infos()
            .map(|info| info.name.to_owned())
            .collect()
    }

    pub(crate) fn page_map_image_entry_infos(
        &self,
    ) -> impl Iterator<Item = ZipImageEntryInfo<'_>> + '_ {
        self.image_entries
            .iter()
            .enumerate()
            .map(|(page_index, &entry_index)| {
                let entry = &self.core.entries()[entry_index];
                ZipImageEntryInfo {
                    page_index: page_index as u32,
                    entry_index: entry_index as u32,
                    name: &entry.name,
                    compression: entry.compression.into(),
                    compressed_size: entry.compressed_size,
                    uncompressed_size: entry.uncompressed_size,
                }
            })
    }

    pub(crate) fn read_page_map_metadata_for_entry_index(
        &self,
        entry_idx: usize,
    ) -> Result<ZipPageMapReadOutcome> {
        self.core.read_page_map_metadata_for_entry_index(entry_idx)
    }

    /// n 番目の画像ページ（0-indexed, natural sort 順）を返す
    pub fn read_page_n(&self, page_n: u32) -> Result<Bytes> {
        let entry_idx = *self.image_entries.get(page_n as usize).with_context(|| {
            format!(
                "page {page_n} out of range (total {})",
                self.image_entries.len()
            )
        })?;
        self.core.read_entry_by_index(entry_idx)
    }

    pub(crate) fn read_page_n_for_page_map(
        &self,
        page_n: u32,
    ) -> std::result::Result<Bytes, ZipPageMapSlowFailureReason> {
        let entry_idx = self
            .image_entries
            .get(page_n as usize)
            .copied()
            .ok_or(ZipPageMapSlowFailureReason::EntryReadError)?;
        self.core.read_entry_by_index_for_page_map(entry_idx)
    }

    pub(crate) fn read_entry_by_index_for_page_map(
        &self,
        entry_idx: usize,
    ) -> std::result::Result<Bytes, ZipPageMapSlowFailureReason> {
        self.core.read_entry_by_index_for_page_map(entry_idx)
    }
}

pub(crate) fn list_cbz_rebuild_entries(path: &Path) -> Result<Vec<CbzRebuildArchiveEntry>> {
    let core = ZipArchiveCore::open(path)?;
    Ok(core
        .entries()
        .iter()
        .map(|entry| CbzRebuildArchiveEntry {
            name: entry.name.to_string(),
            kind: if entry.is_dir {
                CbzRebuildArchiveEntryKind::Directory
            } else if is_image_name(&entry.name) {
                CbzRebuildArchiveEntryKind::Image
            } else {
                CbzRebuildArchiveEntryKind::NonImage
            },
        })
        .collect())
}

pub(crate) fn write_cbz_rebuild_keep_entries<W: Write + Seek>(
    path: &Path,
    keep_entries: &[CbzRebuildArchiveEntry],
    writer: &mut ZipWriter<W>,
) -> Result<()> {
    let core = ZipArchiveCore::open(path)?;
    for entry in keep_entries {
        match entry.kind {
            CbzRebuildArchiveEntryKind::Directory => {
                write_cbz_rebuild_directory_entry(writer, &entry.name)?;
            }
            CbzRebuildArchiveEntryKind::Image | CbzRebuildArchiveEntryKind::NonImage => {
                let bytes = core.read_entry_by_name(&entry.name)?;
                write_cbz_rebuild_file_entry(writer, &entry.name, &bytes)?;
            }
        }
    }
    Ok(())
}

fn display_name_from_archive_entry(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(name)
        .to_owned()
}

fn read_page_map_metadata_stored(entry: &ZipEntry, raw: &[u8]) -> Result<ZipPageMapReadOutcome> {
    const LIMIT: usize = 1024 * 1024;
    const CHUNK_SIZE: usize = 4096;

    match page_map_format_for_name(&entry.name) {
        Some(PageImageFormat::Jpeg) => {
            let mut probe = JpegMetadataProbe::new();
            let mut consumed = 0usize;
            while consumed < raw.len() && consumed < LIMIT {
                let end = (consumed + CHUNK_SIZE).min(raw.len()).min(LIMIT);
                match probe.feed(&raw[..end])? {
                    MetadataProbeResult::Done {
                        format,
                        width,
                        height,
                        bytes_touched,
                    } => {
                        return Ok(ZipPageMapReadOutcome::Ready(ZipPageMapMetadataRead {
                            format,
                            width,
                            height,
                            used_lightweight: true,
                            compressed_bytes_touched: bytes_touched,
                            uncompressed_bytes_produced: bytes_touched,
                        }));
                    }
                    MetadataProbeResult::Invalid => {
                        return Ok(ZipPageMapReadOutcome::Failed(
                            ZipPageMapIssueReason::InvalidHeader,
                        ));
                    }
                    MetadataProbeResult::NeedMore => {
                        consumed = end;
                    }
                }
            }
            if consumed >= LIMIT {
                Ok(ZipPageMapReadOutcome::SlowRequired(
                    ZipPageMapSlowReason::HeaderLimit,
                ))
            } else {
                Ok(ZipPageMapReadOutcome::Failed(
                    ZipPageMapIssueReason::InvalidHeader,
                ))
            }
        }
        Some(PageImageFormat::Png) => match probe_png_metadata(raw)? {
            MetadataProbeResult::Done {
                format,
                width,
                height,
                bytes_touched,
            } => Ok(ZipPageMapReadOutcome::Ready(ZipPageMapMetadataRead {
                format,
                width,
                height,
                used_lightweight: true,
                compressed_bytes_touched: bytes_touched,
                uncompressed_bytes_produced: bytes_touched,
            })),
            MetadataProbeResult::NeedMore => {
                if raw.len() > LIMIT {
                    Ok(ZipPageMapReadOutcome::SlowRequired(
                        ZipPageMapSlowReason::HeaderLimit,
                    ))
                } else {
                    Ok(ZipPageMapReadOutcome::Failed(
                        ZipPageMapIssueReason::InvalidHeader,
                    ))
                }
            }
            MetadataProbeResult::Invalid => Ok(ZipPageMapReadOutcome::Failed(
                ZipPageMapIssueReason::InvalidHeader,
            )),
        },
        Some(_) => Ok(ZipPageMapReadOutcome::SlowRequired(
            ZipPageMapSlowReason::UnsupportedLightweightFormat,
        )),
        None => Ok(ZipPageMapReadOutcome::Failed(
            ZipPageMapIssueReason::UnsupportedFormat,
        )),
    }
}

fn read_page_map_metadata_deflate(entry: &ZipEntry, raw: &[u8]) -> Result<ZipPageMapReadOutcome> {
    const CHUNK_SIZE: usize = 4096;
    const MAX_HEADER_BYTES: usize = 1024 * 1024;

    let mut source = CountingRead::new(std::io::Cursor::new(raw));
    let mut decoder = DeflateDecoder::new(&mut source);
    let mut decoded = Vec::new();
    let mut buf = [0u8; CHUNK_SIZE];

    let mut jpeg_probe = JpegMetadataProbe::new();
    let is_jpeg = matches!(
        page_map_format_for_name(&entry.name),
        Some(PageImageFormat::Jpeg)
    );
    let is_png = matches!(
        page_map_format_for_name(&entry.name),
        Some(PageImageFormat::Png)
    );
    if !is_jpeg && !is_png {
        return Ok(ZipPageMapReadOutcome::SlowRequired(
            ZipPageMapSlowReason::UnsupportedLightweightFormat,
        ));
    }

    loop {
        let read_len = decoder
            .read(&mut buf)
            .with_context(|| format!("deflate '{}'", entry.name))?;
        if read_len == 0 {
            return Ok(ZipPageMapReadOutcome::Failed(
                ZipPageMapIssueReason::DeflateError,
            ));
        }
        decoded.extend_from_slice(&buf[..read_len]);
        if decoded.len() >= MAX_HEADER_BYTES {
            return Ok(ZipPageMapReadOutcome::SlowRequired(
                ZipPageMapSlowReason::HeaderLimit,
            ));
        }

        let probe_result = if is_jpeg {
            jpeg_probe.feed(&decoded)?
        } else {
            probe_png_metadata(&decoded)?
        };

        match probe_result {
            MetadataProbeResult::NeedMore => {
                if decoded.len() >= MAX_HEADER_BYTES {
                    return Ok(ZipPageMapReadOutcome::SlowRequired(
                        ZipPageMapSlowReason::HeaderLimit,
                    ));
                }
                continue;
            }
            MetadataProbeResult::Invalid => {
                return Ok(ZipPageMapReadOutcome::Failed(
                    ZipPageMapIssueReason::InvalidHeader,
                ));
            }
            MetadataProbeResult::Done {
                format,
                width,
                height,
                bytes_touched,
            } => {
                drop(decoder);
                return Ok(ZipPageMapReadOutcome::Ready(ZipPageMapMetadataRead {
                    format,
                    width,
                    height,
                    used_lightweight: true,
                    compressed_bytes_touched: source.bytes_read,
                    uncompressed_bytes_produced: bytes_touched,
                }));
            }
        }
    }
}

// ── BookReader impl ────────────────────────────────────────────────────────────

impl BookReader for ZipReader {
    fn page_count(&self) -> u32 {
        self.page_count()
    }

    fn read_page_n(&self, n: u32) -> Result<Bytes> {
        self.read_page_n(n)
    }

    fn read_first_image(&self) -> Result<Bytes> {
        // §10 表紙高速取得: open 時に構築した natural sort 済みインデックスを再利用
        let idx = *self.image_entries.first().context("no image in archive")?;
        self.core.read_entry_by_index(idx)
    }
}

fn build_image_entries(entries: &[ZipEntry]) -> Vec<usize> {
    let mut image_entries: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            !e.is_dir
                && !matches!(e.compression, Compression::Unsupported(_))
                && is_image_name(&e.name)
        })
        .map(|(i, _)| i)
        .collect();
    image_entries.sort_by(|&a, &b| natural_sort::compare(&entries[a].name, &entries[b].name));
    image_entries
}

// ── ZIP パーサ ────────────────────────────────────────────────────────────────

fn parse_entries(data: &[u8]) -> Result<Vec<ZipEntry>> {
    let (cd_offset, cd_size) = find_central_dir(data)?;
    let cd = data
        .get(cd_offset..cd_offset + cd_size)
        .context("central directory out of bounds")?;
    parse_central_dir(data, cd)
}

/// EOCD を末尾から検索し、中央ディレクトリの (offset, size) を返す
fn find_central_dir(data: &[u8]) -> Result<(usize, usize)> {
    if data.len() < EOCD_MIN_LEN {
        bail!("file too small to be a ZIP");
    }

    // コメント最大 65535 バイトを考慮して末尾から探索
    let search_start = data.len().saturating_sub(EOCD_MIN_LEN + 65535);
    let eocd_pos = find_sig_from_end(data, SIG_EOCD, search_start)
        .context("EOCD signature not found — not a ZIP file")?;

    let eocd = &data[eocd_pos..];

    // ZIP64 ロケータが直前にあれば ZIP64 モードで読む
    if eocd_pos >= 20 {
        let maybe_loc = &data[eocd_pos - 20..];
        if r32(maybe_loc, 0) == SIG_LOC64 {
            return find_central_dir_zip64(data, maybe_loc);
        }
    }

    // 通常 EOCD (22 バイト固定部)
    let cd_size = r32(eocd, 12) as usize;
    let cd_offset = r32(eocd, 16) as usize;

    if cd_offset == U32_MAX_AS_64 as usize {
        bail!("ZIP64 required but no ZIP64 locator found");
    }
    Ok((cd_offset, cd_size))
}

/// ZIP64 EOCD ロケータから中央ディレクトリの (offset, size) を取得
fn find_central_dir_zip64(data: &[u8], locator: &[u8]) -> Result<(usize, usize)> {
    let eocd64_off = r64(locator, 8) as usize;
    let eocd64 = data
        .get(eocd64_off..eocd64_off + 56)
        .context("ZIP64 EOCD out of bounds")?;

    if r32(eocd64, 0) != SIG_EOCD64 {
        bail!("invalid ZIP64 EOCD signature");
    }

    let cd_size = r64(eocd64, 40) as usize;
    let cd_offset = r64(eocd64, 48) as usize;
    Ok((cd_offset, cd_size))
}

/// 中央ディレクトリを走査してエントリ一覧を返す
fn parse_central_dir(_file_data: &[u8], cd: &[u8]) -> Result<Vec<ZipEntry>> {
    let mut entries = Vec::new();
    let mut pos = 0usize;

    while pos + CDIR_FIXED <= cd.len() {
        if r32(cd, pos) != SIG_CDIR {
            break; // 末尾パディングに達した
        }

        let flags = r16(cd, pos + 8);
        let method = r16(cd, pos + 10);
        let comp_size32 = r32(cd, pos + 20) as u64;
        let unc_size32 = r32(cd, pos + 24) as u64;
        let name_len = r16(cd, pos + 28) as usize;
        let extra_len = r16(cd, pos + 30) as usize;
        let comment_len = r16(cd, pos + 32) as usize;
        let lh_off32 = r32(cd, pos + 42) as u64;

        let name_start = pos + CDIR_FIXED;
        let name_end = name_start + name_len;
        if name_end > cd.len() {
            bail!("central dir entry name out of bounds at pos={pos}");
        }

        // ファイル名デコード:
        //   bit11 (UTF-8 flag) が立っている → UTF-8
        //   そうでなければ → Shift-JIS (CP932) を試みる（日本の ZIP 標準）
        //   CP932 でも無効なら UTF-8 として lossy デコード
        let name_bytes = &cd[name_start..name_end];
        let name: Arc<str> = if flags & (1 << 11) != 0 {
            // UTF-8 フラグあり
            std::str::from_utf8(name_bytes)
                .map(|s| s.into())
                .unwrap_or_else(|_| String::from_utf8_lossy(name_bytes).as_ref().into())
        } else {
            // UTF-8 として有効なら UTF-8 として扱う（英数字ファイル名など）
            // 無効なら Shift-JIS (Windows-31J / CP932) として解釈
            if std::str::from_utf8(name_bytes).is_ok() {
                // ASCII / 純 UTF-8 ファイル名
                String::from_utf8_lossy(name_bytes).as_ref().into()
            } else {
                decode_shift_jis(name_bytes)
            }
        };

        // ZIP64 拡張フィールド解析
        let extra_start = name_end;
        let extra_end = extra_start + extra_len;
        let extra = cd.get(extra_start..extra_end).unwrap_or(&[]);
        let (comp_size, unc_size, lh_off) =
            resolve_zip64_sizes(comp_size32, unc_size32, lh_off32, extra);

        let compression = match method {
            0 => Compression::Stored,
            8 => Compression::Deflate,
            m => {
                tracing::debug!("unsupported compression method {m} for '{name}'");
                Compression::Unsupported(m)
            }
        };

        let is_dir = name.ends_with('/') || name.ends_with('\\');

        entries.push(ZipEntry {
            name,
            compression,
            lh_offset: lh_off,
            compressed_size: comp_size,
            uncompressed_size: unc_size,
            is_dir,
        });

        pos += CDIR_FIXED + name_len + extra_len + comment_len;
    }

    Ok(entries)
}

/// ローカルファイルヘッダを読み、圧縮データ先頭オフセットを返す
fn local_data_offset(data: &[u8], lh_offset: u64) -> Result<u64> {
    let off = lh_offset as usize;
    let lh = data
        .get(off..off + LOCAL_FIXED)
        .context("local header out of bounds")?;

    if r32(lh, 0) != SIG_LOCAL {
        bail!("invalid local file header signature at offset {off:#x}");
    }

    let fname_len = r16(lh, 26) as u64;
    let extra_len = r16(lh, 28) as u64;
    Ok(lh_offset + LOCAL_FIXED as u64 + fname_len + extra_len)
}

/// ZIP64 拡張フィールドから実際のサイズ・オフセットを解決
fn resolve_zip64_sizes(comp32: u64, unc32: u64, lh_off32: u64, extra: &[u8]) -> (u64, u64, u64) {
    // いずれも 0xFFFF_FFFF でない場合は ZIP64 拡張不要
    if comp32 != U32_MAX_AS_64 && unc32 != U32_MAX_AS_64 && lh_off32 != U32_MAX_AS_64 {
        return (comp32, unc32, lh_off32);
    }

    // ZIP64 extra field (id=0x0001) を探す
    let mut ep = 0usize;
    while ep + 4 <= extra.len() {
        let id = r16(extra, ep);
        let size = r16(extra, ep + 2) as usize;
        if id == ZIP64_EXTRA_ID {
            let mut vp = ep + 4;
            let mut unc = unc32;
            let mut comp = comp32;
            let mut lh = lh_off32;
            // フィールドは必要なものだけ順に並ぶ
            if unc == U32_MAX_AS_64 && vp + 8 <= ep + 4 + size {
                unc = r64(extra, vp);
                vp += 8;
            }
            if comp == U32_MAX_AS_64 && vp + 8 <= ep + 4 + size {
                comp = r64(extra, vp);
                vp += 8;
            }
            if lh == U32_MAX_AS_64 && vp + 8 <= ep + 4 + size {
                lh = r64(extra, vp);
            }
            return (comp, unc, lh);
        }
        ep += 4 + size;
    }

    (comp32, unc32, lh_off32) // ZIP64 フィールドが見つからなかった
}

// ── 展開 ─────────────────────────────────────────────────────────────────────

fn decompress(raw: &[u8], entry: &ZipEntry) -> Result<Bytes> {
    match entry.compression {
        Compression::Stored => Ok(Bytes::copy_from_slice(raw)),
        Compression::Deflate => {
            // ページ表示・Page Map 作成では展開済みエントリ全体が必要なため、ここでは
            // `uncompressed_size` に固定上限を設けない。壊れた ZIP の過大な申告値では
            // 予約・蓄積メモリが大きくなり得る。上限を導入する場合は、通常表示・サムネイル・
            // Page Map の各経路で共有するメモリ予算と、正常な大判画像の扱いを合わせて設計すること。
            let cap = entry.uncompressed_size as usize;
            let mut out = Vec::with_capacity(cap);
            DeflateDecoder::new(raw)
                .read_to_end(&mut out)
                .with_context(|| format!("deflate '{}': expected {} bytes", entry.name, cap))?;
            Ok(out.into())
        }
        Compression::Unsupported(method) => {
            bail!(
                "unsupported compression method {} for '{}'",
                method,
                entry.name
            )
        }
    }
}

#[derive(Clone, Debug)]
struct CountingRead<R> {
    inner: R,
    bytes_read: usize,
}

impl<R> CountingRead<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }
}

impl<R: std::io::Read> std::io::Read for CountingRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes_read += read;
        Ok(read)
    }
}

// ── ユーティリティ ────────────────────────────────────────────────────────────

/// 末尾から `signature` を検索し、見つかった位置を返す
fn find_sig_from_end(data: &[u8], sig: u32, search_start: usize) -> Option<usize> {
    let sig_bytes = sig.to_le_bytes();
    // data[search_start..] の全範囲で windows(4) を逆順に検索
    data[search_start..]
        .windows(4)
        .rposition(|w| w == sig_bytes)
        .map(|p| p + search_start)
}

#[inline]
fn r16(b: &[u8], off: usize) -> u16 {
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(&b[off..off + 2]);
    u16::from_le_bytes(bytes)
}
#[inline]
fn r32(b: &[u8], off: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&b[off..off + 4]);
    u32::from_le_bytes(bytes)
}
#[inline]
fn r64(b: &[u8], off: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(bytes)
}

/// Shift-JIS (Windows-31J / CP932) バイト列を UTF-8 文字列にデコードする。
/// encoding_rs は BOM なし CP932 として解釈し、UTF-8 に変換する。
fn decode_shift_jis(bytes: &[u8]) -> Arc<str> {
    let (cow, _encoding, had_errors) = encoding_rs::SHIFT_JIS.decode(bytes);
    if had_errors {
        // デコードエラーがあっても最善のフォールバックを返す
        tracing::debug!(
            "Shift-JIS decode had errors for {} bytes, using lossy result",
            bytes.len()
        );
    }
    cow.as_ref().into()
}

fn is_image_name(name: &str) -> bool {
    is_supported_image_name(name.rsplit('.').next().unwrap_or(""))
}

// ── テスト ────────────────────────────────────────────────────────────────────
