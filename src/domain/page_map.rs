use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

/// 正式な Page Map REV。現行バイナリレイアウトは REV 1 として固定する。
pub const PAGE_MAP_SCHEMA_VERSION: u16 = 1;

const PAGE_MAP_MAGIC: &[u8; 8] = b"CBZPMAP\0";
const PAGE_MAP_HEADER_LEN: usize = 52;
const PAGE_MAP_RECORD_LEN: usize = 16;
const PAGE_MAP_MAX_PAGE_COUNT: usize = 1_048_576;
const PAGE_MAP_HEADER_RESERVED_LEN: usize = 8;
const PAGE_MAP_RECORD_FIXED_LEN: usize = 10;
const PAGE_MAP_RECORD_RESERVE: usize = PAGE_MAP_RECORD_LEN - PAGE_MAP_RECORD_FIXED_LEN;
const PAGE_MAP_SOURCE_REVISION_OFFSET: usize =
    PAGE_MAP_HEADER_LEN - SourceRevision::ENCODED_LEN - PAGE_MAP_HEADER_RESERVED_LEN;
const PAGE_MAP_SOURCE_REVISION_END: usize =
    PAGE_MAP_SOURCE_REVISION_OFFSET + SourceRevision::ENCODED_LEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageFormat {
    Jpeg,
    Png,
    WebP,
    Avif,
    Bmp,
    Tiff,
    Gif,
}

pub type PageImageFormat = PageFormat;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageDescriptor {
    pub format: PageFormat,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum SourceRevision {
    #[default]
    Unknown,
    FileState {
        file_size: u64,
        modified_nanos: i64,
    },
}

impl SourceRevision {
    pub fn from_file_state(file_size: u64, modified: Option<SystemTime>) -> Self {
        Self::FileState {
            file_size,
            modified_nanos: system_time_to_i64_nanos(modified.as_ref()),
        }
    }

    pub(crate) const ENCODED_LEN: usize = 24;

    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Unknown => {
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(&0u64.to_le_bytes());
                out.extend_from_slice(&0i64.to_le_bytes());
                out.extend_from_slice(&0u32.to_le_bytes());
            }
            Self::FileState {
                file_size,
                modified_nanos,
            } => {
                out.extend_from_slice(&1u16.to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(&file_size.to_le_bytes());
                out.extend_from_slice(&modified_nanos.to_le_bytes());
                out.extend_from_slice(&0u32.to_le_bytes());
            }
        }
    }

    pub(crate) fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != Self::ENCODED_LEN {
            return None;
        }

        let kind = u16::from_le_bytes(data[0..2].try_into().ok()?);
        let flags = u16::from_le_bytes(data[2..4].try_into().ok()?);
        let file_size = u64::from_le_bytes(data[4..12].try_into().ok()?);
        let modified_nanos = i64::from_le_bytes(data[12..20].try_into().ok()?);
        let reserved = &data[20..24];
        if flags != 0 || reserved.iter().any(|&b| b != 0) {
            return None;
        }

        match kind {
            0 => {
                if file_size == 0 && modified_nanos == 0 {
                    Some(Self::Unknown)
                } else {
                    None
                }
            }
            1 => Some(Self::FileState {
                file_size,
                modified_nanos,
            }),
            _ => None,
        }
    }

    pub(crate) fn is_persistable(&self) -> bool {
        match self {
            Self::Unknown => false,
            Self::FileState { .. } => true,
        }
    }

    pub(crate) fn persistable_key(&self) -> Option<(u64, i64)> {
        match self {
            Self::Unknown => None,
            Self::FileState {
                file_size,
                modified_nanos,
            } => Some((*file_size, *modified_nanos)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookPageMap {
    pub revision: SourceRevision,
    pub pages: Vec<PageDescriptor>,
}

impl Default for BookPageMap {
    fn default() -> Self {
        Self {
            revision: SourceRevision::Unknown,
            pages: Vec::new(),
        }
    }
}

impl BookPageMap {
    pub fn new(revision: SourceRevision, pages: Vec<PageDescriptor>) -> Self {
        Self { revision, pages }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn get(&self, page_index: usize) -> Option<&PageDescriptor> {
        self.pages.get(page_index)
    }

    pub(crate) fn encode_cache_bytes(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(PAGE_MAP_HEADER_LEN + self.pages.len() * PAGE_MAP_RECORD_LEN);
        out.extend_from_slice(PAGE_MAP_MAGIC);
        out.extend_from_slice(&PAGE_MAP_SCHEMA_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(PAGE_MAP_RECORD_LEN as u32).to_le_bytes());
        out.extend_from_slice(&(self.pages.len() as u32).to_le_bytes());
        self.revision.encode_into(&mut out);
        out.extend_from_slice(&[0u8; PAGE_MAP_HEADER_RESERVED_LEN]);

        for page in &self.pages {
            out.extend_from_slice(&page.width.to_le_bytes());
            out.extend_from_slice(&page.height.to_le_bytes());
            out.push(page_format_to_u8(page.format));
            out.push(0);
            out.extend_from_slice(&[0u8; PAGE_MAP_RECORD_RESERVE]);
        }

        out
    }

    pub(crate) fn decode_cache_bytes(
        data: &[u8],
        expected_revision: &SourceRevision,
    ) -> Result<Option<Self>> {
        if data.len() < PAGE_MAP_HEADER_LEN || &data[0..8] != PAGE_MAP_MAGIC {
            return Ok(None);
        }
        if !expected_revision.is_persistable() {
            return Ok(None);
        }

        let schema_version = u16::from_le_bytes(
            data[8..10]
                .try_into()
                .expect("page map header length was validated"),
        );
        let flags = u16::from_le_bytes(
            data[10..12]
                .try_into()
                .expect("page map header length was validated"),
        );
        let record_size = u32::from_le_bytes(
            data[12..16]
                .try_into()
                .expect("page map header length was validated"),
        ) as usize;
        let page_count = u32::from_le_bytes(
            data[16..20]
                .try_into()
                .expect("page map header length was validated"),
        ) as usize;
        let Some(source_revision) = SourceRevision::decode(
            &data[PAGE_MAP_SOURCE_REVISION_OFFSET..PAGE_MAP_SOURCE_REVISION_END],
        ) else {
            return Ok(None);
        };
        let reserved = &data[PAGE_MAP_SOURCE_REVISION_END..PAGE_MAP_HEADER_LEN];

        if schema_version != PAGE_MAP_SCHEMA_VERSION
            || flags != 0
            || record_size != PAGE_MAP_RECORD_LEN
        {
            return Ok(None);
        }
        if page_count > PAGE_MAP_MAX_PAGE_COUNT {
            return Ok(None);
        }
        if reserved.iter().any(|&b| b != 0) {
            return Ok(None);
        }
        if !source_revision.is_persistable() {
            return Ok(None);
        }
        if &source_revision != expected_revision {
            return Ok(None);
        }

        let Some(page_bytes) = page_count.checked_mul(PAGE_MAP_RECORD_LEN) else {
            return Ok(None);
        };
        let Some(total_len) = PAGE_MAP_HEADER_LEN.checked_add(page_bytes) else {
            return Ok(None);
        };
        if data.len() != total_len {
            return Ok(None);
        }

        let mut pages = Vec::with_capacity(page_count);
        let mut pos = PAGE_MAP_HEADER_LEN;
        for _ in 0..page_count {
            let width = u32::from_le_bytes(
                data[pos..pos + 4]
                    .try_into()
                    .expect("record length was validated against page count"),
            );
            let height = u32::from_le_bytes(
                data[pos + 4..pos + 8]
                    .try_into()
                    .expect("record length was validated against page count"),
            );
            let Some(format) = page_format_from_u8(data[pos + 8]) else {
                return Ok(None);
            };
            let record_flags = data[pos + 9];
            let record_reserved = &data[pos + 10..pos + PAGE_MAP_RECORD_LEN];
            if record_flags != 0 || record_reserved.iter().any(|&b| b != 0) {
                return Ok(None);
            }
            if width == 0 || height == 0 {
                return Ok(None);
            }
            pages.push(PageDescriptor {
                format,
                width,
                height,
            });
            pos += PAGE_MAP_RECORD_LEN;
        }

        Ok(Some(Self {
            revision: source_revision,
            pages,
        }))
    }
}

fn page_format_to_u8(format: PageFormat) -> u8 {
    match format {
        PageFormat::Jpeg => 1,
        PageFormat::Png => 2,
        PageFormat::WebP => 3,
        PageFormat::Avif => 4,
        PageFormat::Bmp => 5,
        PageFormat::Tiff => 6,
        PageFormat::Gif => 7,
    }
}

fn page_format_from_u8(value: u8) -> Option<PageFormat> {
    match value {
        1 => Some(PageFormat::Jpeg),
        2 => Some(PageFormat::Png),
        3 => Some(PageFormat::WebP),
        4 => Some(PageFormat::Avif),
        5 => Some(PageFormat::Bmp),
        6 => Some(PageFormat::Tiff),
        7 => Some(PageFormat::Gif),
        _ => None,
    }
}

fn system_time_to_i64_nanos(time: Option<&SystemTime>) -> i64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_map_schema_version_is_rev1() {
        assert_eq!(PAGE_MAP_SCHEMA_VERSION, 1);
    }

    #[test]
    fn page_map_rev1_roundtrip() {
        let map = BookPageMap::new(
            SourceRevision::FileState {
                file_size: 123,
                modified_nanos: 456,
            },
            vec![PageDescriptor {
                format: PageFormat::Jpeg,
                width: 800,
                height: 1200,
            }],
        );

        let bytes = map.encode_cache_bytes();
        assert_eq!(u16::from_le_bytes(bytes[8..10].try_into().unwrap()), 1);

        let decoded = BookPageMap::decode_cache_bytes(
            &bytes,
            &SourceRevision::FileState {
                file_size: 123,
                modified_nanos: 456,
            },
        )
        .expect("decode result")
        .expect("rev1 cache should decode");
        assert_eq!(decoded, map);
    }

    #[test]
    fn page_map_rejects_non_rev1() {
        let map = BookPageMap::new(
            SourceRevision::FileState {
                file_size: 123,
                modified_nanos: 456,
            },
            vec![PageDescriptor {
                format: PageFormat::Png,
                width: 400,
                height: 300,
            }],
        );
        let mut bytes = map.encode_cache_bytes();
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());

        let decoded = BookPageMap::decode_cache_bytes(
            &bytes,
            &SourceRevision::FileState {
                file_size: 123,
                modified_nanos: 456,
            },
        )
        .expect("decode result");
        assert!(decoded.is_none());
    }
}
