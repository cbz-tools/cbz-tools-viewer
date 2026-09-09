use anyhow::{Context, Result};

use crate::domain::{page::ImageFormatHint, page_map::PageImageFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageOrientation {
    Normal,
    FlipHorizontal,
    Rotate180,
    FlipVertical,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
}

impl ImageOrientation {
    pub fn from_exif_value(value: u16) -> Self {
        match value {
            2 => Self::FlipHorizontal,
            3 => Self::Rotate180,
            4 => Self::FlipVertical,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            _ => Self::Normal,
        }
    }

    fn swaps_dimensions(self) -> bool {
        matches!(
            self,
            Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270
        )
    }
}

pub fn logical_dimensions(width: u32, height: u32, orientation: ImageOrientation) -> (u32, u32) {
    if orientation.swaps_dimensions() {
        (height, width)
    } else {
        (width, height)
    }
}

pub fn normalize_rgba(
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    orientation: ImageOrientation,
) -> Result<(u32, u32, Vec<u8>)> {
    let expected_len = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()?
                .checked_mul(width)?
                .checked_mul(4)
        })
        .context("RGBA image dimensions overflow")?;
    if pixels.len() != expected_len {
        anyhow::bail!(
            "RGBA pixel buffer length mismatch: dimensions={width}x{height} pixels={}",
            pixels.len()
        );
    }
    if orientation == ImageOrientation::Normal || width == 0 || height == 0 {
        return Ok((width, height, pixels));
    }

    let (dst_width, dst_height) = logical_dimensions(width, height, orientation);
    let dst_len = usize::try_from(dst_width)
        .ok()
        .and_then(|width| {
            usize::try_from(dst_height)
                .ok()?
                .checked_mul(width)?
                .checked_mul(4)
        })
        .context("oriented RGBA dimensions overflow")?;
    let mut normalized = vec![0u8; dst_len];
    let width = usize::try_from(width).context("RGBA width exceeds usize")?;
    let height = usize::try_from(height).context("RGBA height exceeds usize")?;
    let dst_width = usize::try_from(dst_width).context("oriented RGBA width exceeds usize")?;

    for y in 0..usize::try_from(dst_height).context("oriented RGBA height exceeds usize")? {
        for x in 0..dst_width {
            let (src_x, src_y) = match orientation {
                ImageOrientation::Normal => (x, y),
                ImageOrientation::FlipHorizontal => (width - 1 - x, y),
                ImageOrientation::Rotate180 => (width - 1 - x, height - 1 - y),
                ImageOrientation::FlipVertical => (x, height - 1 - y),
                ImageOrientation::Transpose => (y, x),
                ImageOrientation::Rotate90 => (y, height - 1 - x),
                ImageOrientation::Transverse => (width - 1 - y, height - 1 - x),
                ImageOrientation::Rotate270 => (width - 1 - y, x),
            };
            let src_offset = (src_y * width + src_x) * 4;
            let dst_offset = (y * dst_width + x) * 4;
            normalized[dst_offset..dst_offset + 4]
                .copy_from_slice(&pixels[src_offset..src_offset + 4]);
        }
    }

    Ok((
        u32::try_from(dst_width).context("oriented RGBA width exceeds u32")?,
        u32::try_from(usize::try_from(dst_height).context("oriented RGBA height exceeds usize")?)
            .context("oriented RGBA height exceeds u32")?,
        normalized,
    ))
}

pub fn for_page_format(data: &[u8], format: PageImageFormat) -> ImageOrientation {
    match format {
        PageImageFormat::Jpeg => jpeg(data),
        PageImageFormat::Png => png(data),
        PageImageFormat::WebP => webp(data),
        PageImageFormat::Tiff => tiff(data),
        PageImageFormat::Avif | PageImageFormat::Bmp | PageImageFormat::Gif => {
            ImageOrientation::Normal
        }
    }
}

pub fn for_image_hint(data: &[u8], format: ImageFormatHint) -> ImageOrientation {
    match format {
        ImageFormatHint::Jpeg => jpeg(data),
        ImageFormatHint::Png => png(data),
        ImageFormatHint::WebP => webp(data),
        ImageFormatHint::Avif | ImageFormatHint::Gif => ImageOrientation::Normal,
        ImageFormatHint::Unknown => tiff(data),
    }
}

pub fn jpeg(data: &[u8]) -> ImageOrientation {
    let mut pos = 2usize;
    if data.len() < pos || data.get(..2) != Some(&[0xFF, 0xD8]) {
        return ImageOrientation::Normal;
    }
    while pos < data.len() {
        if data[pos] != 0xFF {
            return ImageOrientation::Normal;
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        let Some(&marker) = data.get(pos) else {
            return ImageOrientation::Normal;
        };
        pos += 1;
        if marker == 0xDA || marker == 0xD9 {
            return ImageOrientation::Normal;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        let Some(length_bytes) = data.get(pos..pos + 2) else {
            return ImageOrientation::Normal;
        };
        let segment_len = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        if segment_len < 2 {
            return ImageOrientation::Normal;
        }
        let payload_start = pos + 2;
        let payload_len = segment_len - 2;
        let Some(payload) = data.get(payload_start..payload_start + payload_len) else {
            return ImageOrientation::Normal;
        };
        if marker == 0xE1 {
            if let Some(orientation) = exif_orientation(payload) {
                return orientation;
            }
        }
        pos = payload_start + payload_len;
    }
    ImageOrientation::Normal
}

pub fn png(data: &[u8]) -> ImageOrientation {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.get(..8) != Some(SIGNATURE) {
        return ImageOrientation::Normal;
    }
    let mut pos = 8usize;
    while let Some(header) = data.get(pos..pos + 8) {
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let payload_start = pos + 8;
        let Some(payload) = data.get(payload_start..payload_start + length) else {
            return ImageOrientation::Normal;
        };
        let chunk_type = &header[4..8];
        if chunk_type == b"eXIf" {
            if let Some(orientation) = tiff_orientation(payload) {
                return orientation;
            }
        }
        if chunk_type == b"IDAT" || chunk_type == b"IEND" {
            break;
        }
        let Some(next) = payload_start
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
        else {
            return ImageOrientation::Normal;
        };
        pos = next;
    }
    ImageOrientation::Normal
}

pub fn webp(data: &[u8]) -> ImageOrientation {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return ImageOrientation::Normal;
    }
    let mut pos = 12usize;
    while let Some(header) = data.get(pos..pos + 8) {
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let payload_start = pos + 8;
        let Some(payload) = data.get(payload_start..payload_start + length) else {
            return ImageOrientation::Normal;
        };
        if &header[..4] == b"EXIF" {
            if let Some(orientation) = exif_orientation(payload) {
                return orientation;
            }
        }
        let Some(next) = payload_start
            .checked_add(length)
            .and_then(|end| end.checked_add(length & 1))
        else {
            return ImageOrientation::Normal;
        };
        pos = next;
    }
    ImageOrientation::Normal
}

pub fn tiff(data: &[u8]) -> ImageOrientation {
    exif_orientation(data).unwrap_or(ImageOrientation::Normal)
}

fn exif_orientation(payload: &[u8]) -> Option<ImageOrientation> {
    let tiff = payload.strip_prefix(b"Exif\0\0").unwrap_or(payload);
    tiff_orientation(tiff)
}

pub fn jpeg_app1(data: &[u8]) -> ImageOrientation {
    exif_orientation(data).unwrap_or(ImageOrientation::Normal)
}

fn tiff_orientation(data: &[u8]) -> Option<ImageOrientation> {
    let little_endian = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read_u16 = |offset: usize| -> Option<u16> {
        let bytes = data.get(offset..offset + 2)?;
        Some(if little_endian {
            u16::from_le_bytes([bytes[0], bytes[1]])
        } else {
            u16::from_be_bytes([bytes[0], bytes[1]])
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let bytes = data.get(offset..offset + 4)?;
        Some(if little_endian {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        })
    };

    if read_u16(2)? != 42 {
        return None;
    }
    let ifd_offset = usize::try_from(read_u32(4)?).ok()?;
    let entry_count = usize::from(read_u16(ifd_offset)?);
    let entries_end = ifd_offset
        .checked_add(2)?
        .checked_add(entry_count.checked_mul(12)?)?;
    if entries_end > data.len() {
        return None;
    }
    for index in 0..entry_count {
        let entry = ifd_offset
            .checked_add(2)?
            .checked_add(index.checked_mul(12)?)?;
        if read_u16(entry)? != 0x0112 {
            continue;
        }
        let value_type = read_u16(entry + 2)?;
        let count = read_u32(entry + 4)?;
        if value_type != 3 || count != 1 {
            return None;
        }
        return Some(ImageOrientation::from_exif_value(read_u16(entry + 8)?));
    }
    None
}
