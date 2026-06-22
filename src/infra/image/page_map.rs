use std::io::Cursor;

use anyhow::{Context, Result};
use image::{ImageFormat, ImageReader};

use crate::domain::page_map::PageImageFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataProbeResult {
    NeedMore,
    Invalid,
    Done {
        format: PageImageFormat,
        width: u32,
        height: u32,
        bytes_touched: usize,
    },
}

pub struct JpegMetadataProbe {
    state: JpegProbeState,
    pos: usize,
}

impl JpegMetadataProbe {
    pub fn new() -> Self {
        Self {
            state: JpegProbeState::Start,
            pos: 0,
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<MetadataProbeResult> {
        loop {
            match self.state {
                JpegProbeState::Start => {
                    if data.len() < 2 {
                        return Ok(MetadataProbeResult::NeedMore);
                    }
                    if data[0] != 0xFF || data[1] != 0xD8 {
                        return Ok(MetadataProbeResult::Invalid);
                    }
                    self.pos = 2;
                    self.state = JpegProbeState::Scan;
                }
                JpegProbeState::Scan => {
                    if self.pos >= data.len() {
                        return Ok(MetadataProbeResult::NeedMore);
                    }
                    if data[self.pos] != 0xFF {
                        return Ok(MetadataProbeResult::Invalid);
                    }
                    while self.pos < data.len() && data[self.pos] == 0xFF {
                        self.pos += 1;
                    }
                    if self.pos >= data.len() {
                        return Ok(MetadataProbeResult::NeedMore);
                    }
                    let marker = data[self.pos];
                    self.pos += 1;
                    if marker == 0xD8 {
                        continue;
                    }
                    if marker == 0xD9 || marker == 0xDA {
                        return Ok(MetadataProbeResult::Invalid);
                    }
                    if is_jpeg_standalone_marker(marker) {
                        continue;
                    }
                    self.state = JpegProbeState::NeedSegmentLength {
                        marker,
                        len_bytes: [0; 2],
                        len_read: 0,
                    };
                }
                JpegProbeState::NeedSegmentLength {
                    marker,
                    mut len_bytes,
                    mut len_read,
                } => {
                    while len_read < 2 && self.pos < data.len() {
                        len_bytes[len_read] = data[self.pos];
                        self.pos += 1;
                        len_read += 1;
                    }
                    if len_read < 2 {
                        self.state = JpegProbeState::NeedSegmentLength {
                            marker,
                            len_bytes,
                            len_read,
                        };
                        return Ok(MetadataProbeResult::NeedMore);
                    }
                    let seg_len = u16::from_be_bytes(len_bytes) as usize;
                    if seg_len < 2 {
                        return Ok(MetadataProbeResult::Invalid);
                    }
                    let payload = seg_len - 2;
                    if is_jpeg_sof_marker(marker) {
                        if payload < 6 {
                            return Ok(MetadataProbeResult::Invalid);
                        }
                        self.state = JpegProbeState::SofData { remaining: payload };
                    } else {
                        self.state = JpegProbeState::SkipSegment { remaining: payload };
                    }
                }
                JpegProbeState::SkipSegment { mut remaining } => {
                    if remaining == 0 {
                        self.state = JpegProbeState::Scan;
                        continue;
                    }
                    let available = data.len().saturating_sub(self.pos);
                    if available == 0 {
                        self.state = JpegProbeState::SkipSegment { remaining };
                        return Ok(MetadataProbeResult::NeedMore);
                    }
                    let take = remaining.min(available);
                    self.pos += take;
                    remaining -= take;
                    if remaining == 0 {
                        self.state = JpegProbeState::Scan;
                    } else {
                        self.state = JpegProbeState::SkipSegment { remaining };
                        return Ok(MetadataProbeResult::NeedMore);
                    }
                }
                JpegProbeState::SofData { remaining } => {
                    if remaining < 6 {
                        return Ok(MetadataProbeResult::Invalid);
                    }
                    if data.len().saturating_sub(self.pos) < 6 {
                        self.state = JpegProbeState::SofData { remaining };
                        return Ok(MetadataProbeResult::NeedMore);
                    }
                    let _precision = data[self.pos];
                    let height =
                        u16::from_be_bytes([data[self.pos + 1], data[self.pos + 2]]) as u32;
                    let width = u16::from_be_bytes([data[self.pos + 3], data[self.pos + 4]]) as u32;
                    if width == 0 || height == 0 {
                        return Ok(MetadataProbeResult::Invalid);
                    }
                    return Ok(MetadataProbeResult::Done {
                        format: PageImageFormat::Jpeg,
                        width,
                        height,
                        bytes_touched: self.pos + 6,
                    });
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum JpegProbeState {
    Start,
    Scan,
    NeedSegmentLength {
        marker: u8,
        len_bytes: [u8; 2],
        len_read: usize,
    },
    SkipSegment {
        remaining: usize,
    },
    SofData {
        remaining: usize,
    },
}

pub fn probe_png_metadata(data: &[u8]) -> Result<MetadataProbeResult> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 24 {
        return Ok(MetadataProbeResult::NeedMore);
    }
    if &data[0..8] != PNG_SIGNATURE {
        return Ok(MetadataProbeResult::Invalid);
    }
    let ihdr_len = u32::from_be_bytes(
        data[8..12]
            .try_into()
            .expect("PNG metadata path validates minimum length first"),
    ) as usize;
    if ihdr_len != 13 {
        return Ok(MetadataProbeResult::Invalid);
    }
    if &data[12..16] != b"IHDR" {
        return Ok(MetadataProbeResult::Invalid);
    }
    let width = u32::from_be_bytes(
        data[16..20]
            .try_into()
            .expect("PNG metadata path validates minimum length first"),
    );
    let height = u32::from_be_bytes(
        data[20..24]
            .try_into()
            .expect("PNG metadata path validates minimum length first"),
    );
    if width == 0 || height == 0 {
        return Ok(MetadataProbeResult::Invalid);
    }
    Ok(MetadataProbeResult::Done {
        format: PageImageFormat::Png,
        width,
        height,
        bytes_touched: 24,
    })
}

pub fn read_jpeg_metadata(data: &[u8]) -> Result<(PageImageFormat, u32, u32)> {
    let mut probe = JpegMetadataProbe::new();
    match probe.feed(data)? {
        MetadataProbeResult::Done {
            format,
            width,
            height,
            ..
        } => Ok((format, width, height)),
        MetadataProbeResult::NeedMore => anyhow::bail!("JPEG header too short"),
        MetadataProbeResult::Invalid => anyhow::bail!("invalid JPEG header"),
    }
}

pub fn read_image_metadata(data: &[u8]) -> Result<Option<(PageImageFormat, u32, u32)>> {
    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .context("image guess format")?;
    let Some(format) = reader.format() else {
        return Ok(None);
    };
    let format = match image_format_to_page_image_format(format) {
        Some(format) => format,
        None => return Ok(None),
    };
    let (width, height) = reader.into_dimensions().context("image dimensions")?;
    Ok(Some((format, width, height)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightweightImageMetadataOutcome {
    Ready {
        format: PageImageFormat,
        width: u32,
        height: u32,
    },
    FallbackRequired,
    Unsupported,
}

pub fn read_image_metadata_lightweight_first(
    data: &[u8],
    hint: Option<PageImageFormat>,
) -> LightweightImageMetadataOutcome {
    match hint {
        Some(PageImageFormat::Jpeg) => match read_jpeg_metadata(data) {
            Ok((format, width, height)) => LightweightImageMetadataOutcome::Ready {
                format,
                width,
                height,
            },
            Err(_) => LightweightImageMetadataOutcome::FallbackRequired,
        },
        Some(PageImageFormat::Png) => match probe_png_metadata(data) {
            Ok(MetadataProbeResult::Done {
                format,
                width,
                height,
                ..
            }) => LightweightImageMetadataOutcome::Ready {
                format,
                width,
                height,
            },
            Ok(MetadataProbeResult::NeedMore) | Ok(MetadataProbeResult::Invalid) => {
                LightweightImageMetadataOutcome::FallbackRequired
            }
            Err(_) => LightweightImageMetadataOutcome::FallbackRequired,
        },
        Some(_) | None => LightweightImageMetadataOutcome::Unsupported,
    }
}

fn image_format_to_page_image_format(format: ImageFormat) -> Option<PageImageFormat> {
    match format {
        ImageFormat::Jpeg => Some(PageImageFormat::Jpeg),
        ImageFormat::Png => Some(PageImageFormat::Png),
        ImageFormat::WebP => Some(PageImageFormat::WebP),
        ImageFormat::Avif => Some(PageImageFormat::Avif),
        ImageFormat::Bmp => Some(PageImageFormat::Bmp),
        ImageFormat::Tiff => Some(PageImageFormat::Tiff),
        ImageFormat::Gif => Some(PageImageFormat::Gif),
        _ => None,
    }
}

fn is_jpeg_standalone_marker(marker: u8) -> bool {
    marker == 0x01 || (0xD0..=0xD7).contains(&marker)
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat as CrateImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    fn make_jpeg(progressive: bool) -> Vec<u8> {
        let img = RgbaImage::from_pixel(8, 6, Rgba([32, 96, 160, 255]));
        if progressive {
            let mut cinfo = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
            cinfo.set_size(8, 6);
            cinfo.set_progressive_mode();
            cinfo.set_quality(80.0);
            let mut cinfo = cinfo.start_compress(Vec::new()).unwrap();
            let scanlines: Vec<u8> = DynamicImage::ImageRgba8(img.clone()).to_rgb8().into_raw();
            cinfo.write_scanlines(&scanlines).unwrap();
            cinfo.finish().unwrap()
        } else {
            let mut cursor = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(img)
                .write_to(&mut cursor, CrateImageFormat::Jpeg)
                .unwrap();
            cursor.into_inner()
        }
    }

    #[test]
    fn jpeg_baseline_metadata() {
        let jpeg = make_jpeg(false);
        let mut probe = JpegMetadataProbe::new();
        match probe.feed(&jpeg).unwrap() {
            MetadataProbeResult::Done {
                format,
                width,
                height,
                ..
            } => {
                assert_eq!(format, PageImageFormat::Jpeg);
                assert_eq!((width, height), (8, 6));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn jpeg_progressive_metadata() {
        let jpeg = make_jpeg(true);
        let mut probe = JpegMetadataProbe::new();
        match probe.feed(&jpeg).unwrap() {
            MetadataProbeResult::Done {
                format,
                width,
                height,
                ..
            } => {
                assert_eq!(format, PageImageFormat::Jpeg);
                assert_eq!((width, height), (8, 6));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn jpeg_rejects_invalid_header() {
        let mut probe = JpegMetadataProbe::new();
        assert!(matches!(
            probe.feed(b"not jpeg").unwrap(),
            MetadataProbeResult::Invalid
        ));
    }

    #[test]
    fn png_metadata() {
        let img = RgbaImage::from_pixel(7, 5, Rgba([1, 2, 3, 255]));
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img)
            .write_to(&mut cursor, CrateImageFormat::Png)
            .unwrap();
        let png = cursor.into_inner();
        match probe_png_metadata(&png).unwrap() {
            MetadataProbeResult::Done {
                format,
                width,
                height,
                ..
            } => {
                assert_eq!(format, PageImageFormat::Png);
                assert_eq!((width, height), (7, 5));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn png_rejects_invalid_signature() {
        assert!(matches!(
            probe_png_metadata(b"not png").unwrap(),
            MetadataProbeResult::NeedMore
        ));
        let mut data = vec![0u8; 24];
        data[0] = 0x89;
        assert!(matches!(
            probe_png_metadata(&data).unwrap(),
            MetadataProbeResult::Invalid
        ));
    }

    #[test]
    fn lightweight_first_uses_jpeg_probe() {
        let jpeg = make_jpeg(false);
        match read_image_metadata_lightweight_first(&jpeg, Some(PageImageFormat::Jpeg)) {
            LightweightImageMetadataOutcome::Ready {
                format,
                width,
                height,
            } => {
                assert_eq!(format, PageImageFormat::Jpeg);
                assert_eq!((width, height), (8, 6));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn lightweight_first_uses_png_probe() {
        let mut cursor = Cursor::new(Vec::new());
        let img = image::RgbaImage::from_pixel(10, 12, image::Rgba([20, 40, 60, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cursor, CrateImageFormat::Png)
            .unwrap();
        let png = cursor.into_inner();
        match read_image_metadata_lightweight_first(&png, Some(PageImageFormat::Png)) {
            LightweightImageMetadataOutcome::Ready {
                format,
                width,
                height,
            } => {
                assert_eq!(format, PageImageFormat::Png);
                assert_eq!((width, height), (10, 12));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
}
