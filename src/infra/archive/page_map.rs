use std::path::Path;
use std::time::{Duration, Instant};

use crate::{
    domain::page_map::{BookPageMap, PageDescriptor, PageFormat, SourceRevision},
    infra::{
        archive::folder::FolderImageReader,
        archive::zip::{ZipPageMapReadOutcome, ZipReader},
        archive::BookReader,
        image::page_map::{
            read_image_metadata, read_image_metadata_lightweight_first,
            LightweightImageMetadataOutcome,
        },
        page_map::build::{assemble_zip_fast_page_map, FastBuildOutcome, PageMapBuildStatus},
    },
};

#[cfg(feature = "rar")]
#[path = "page_map_rar.rs"]
mod rar_adaptive;

#[cfg(feature = "rar")]
pub(crate) use rar_adaptive::build_book_page_map_slow_from_rar_path;
#[cfg(feature = "rar")]
pub(crate) use rar_adaptive::RarPageMapSlowOutcome;

#[cfg_attr(not(test), allow(dead_code))]
/// Folder の FAST 判定。Ready だけが cache 保存へ進める。
/// RequiresComplete は complete 経路を持つ呼び出し側だけが扱う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderPageMapFastStatus {
    Ready,
    RequiresComplete,
    Failed,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderPageMapFastOutcome {
    pub status: FolderPageMapFastStatus,
    pub page_map: Option<BookPageMap>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderPageMapFastLaneOutput {
    pub status: FolderPageMapFastStatus,
    pub pages: Vec<PageDescriptor>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderPageMapSlowFailureReason {
    ReadError,
    MetadataError,
    UnsupportedFormat,
    NoImageEntries,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FolderPageMapSlowFailure {
    pub page_index: Option<u32>,
    pub entry_index: Option<u32>,
    pub reason: FolderPageMapSlowFailureReason,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FolderPageMapSlowOutcome {
    Success(BookPageMap),
    Failure(FolderPageMapSlowFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZipPageMapIssueReason {
    HeaderLimit,
    UnsupportedLightweightFormat,
    ZipStructure,
    DeflateError,
    InvalidHeader,
    UnsupportedFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZipPageMapSlowFailureReason {
    ZipOpenError,
    EntryReadError,
    InflateError,
    InvalidHeader,
    MetadataError,
    UnsupportedFormat,
    NoImageEntries,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ZipPageMapSlowFailure {
    pub page_index: Option<u32>,
    pub entry_index: Option<u32>,
    pub reason: ZipPageMapSlowFailureReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ZipPageMapSlowOutcome {
    Success(BookPageMap),
    Failure(ZipPageMapSlowFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZipPageMapSlowReason {
    HeaderLimit,
    UnsupportedLightweightFormat,
}

/// ZIP/CBZ の FAST 判定。SlowRequired は slow 経路を持つ呼び出し側だけが扱う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZipPageMapFastStatus {
    Ready,
    SlowRequired(ZipPageMapSlowReason),
    Failed(ZipPageMapIssueReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ZipPageMapIssue {
    pub page_index: u32,
    pub entry_index: u32,
    pub reason: ZipPageMapIssueReason,
}

#[derive(Clone, Debug)]
pub(crate) struct ZipPageMapFastOutput {
    pub status: ZipPageMapFastStatus,
    pub pages: Vec<PageDescriptor>,
    pub issue: Option<ZipPageMapIssue>,
    pub compressed_bytes_seen: u64,
    pub uncompressed_bytes_seen: u64,
    pub lightweight_pages: usize,
    pub compressed_bytes_touched: usize,
    pub uncompressed_bytes_produced: usize,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ZipPageMapFastEntrySuccess {
    format: PageFormat,
    width: u32,
    height: u32,
    used_lightweight: bool,
    compressed_bytes_touched: usize,
    uncompressed_bytes_produced: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZipPageMapFastEntryResult {
    Ready(ZipPageMapFastEntrySuccess),
    SlowRequired(ZipPageMapSlowReason),
    Failed(ZipPageMapIssueReason),
}

pub(crate) fn build_book_page_map_slow_from_zip_reader(
    reader: &ZipReader,
    revision: SourceRevision,
) -> ZipPageMapSlowOutcome {
    let mut pages = Vec::with_capacity(reader.page_count() as usize);
    let mut saw_image_entry = false;

    for info in reader.page_map_image_entry_infos() {
        saw_image_entry = true;
        let raw = match reader.read_page_n_for_page_map(info.page_index) {
            Ok(raw) => raw,
            Err(reason) => {
                return ZipPageMapSlowOutcome::Failure(ZipPageMapSlowFailure {
                    page_index: Some(info.page_index),
                    entry_index: Some(info.entry_index),
                    reason,
                });
            }
        };
        let (format, width, height) = match read_image_metadata(&raw) {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                return ZipPageMapSlowOutcome::Failure(ZipPageMapSlowFailure {
                    page_index: Some(info.page_index),
                    entry_index: Some(info.entry_index),
                    reason: ZipPageMapSlowFailureReason::UnsupportedFormat,
                });
            }
            Err(_) => {
                return ZipPageMapSlowOutcome::Failure(ZipPageMapSlowFailure {
                    page_index: Some(info.page_index),
                    entry_index: Some(info.entry_index),
                    reason: match page_map_format_for_name(info.name) {
                        Some(PageFormat::Jpeg) | Some(PageFormat::Png) => {
                            ZipPageMapSlowFailureReason::InvalidHeader
                        }
                        Some(_) => ZipPageMapSlowFailureReason::MetadataError,
                        None => ZipPageMapSlowFailureReason::UnsupportedFormat,
                    },
                });
            }
        };
        debug_assert!(width > 0 && height > 0);
        pages.push(PageDescriptor {
            format,
            width,
            height,
        });
    }

    if !saw_image_entry {
        return ZipPageMapSlowOutcome::Failure(ZipPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: ZipPageMapSlowFailureReason::NoImageEntries,
        });
    }

    ZipPageMapSlowOutcome::Success(BookPageMap::new(revision, pages))
}

#[cfg_attr(not(test), allow(dead_code))]
/// DIR 本の FAST Page Map を組み立てる。軽量メタデータで確定できない場合は RequiresComplete を返す。
pub(crate) fn build_book_page_map_fast_from_folder_reader(
    reader: &FolderImageReader,
    revision: SourceRevision,
) -> FolderPageMapFastOutcome {
    let lane = build_folder_page_map_fast_lanes(reader);
    let expected_page_count = reader.page_count() as usize;
    let is_ready = matches!(lane.status, FolderPageMapFastStatus::Ready)
        && expected_page_count > 0
        && lane.pages.len() == expected_page_count;

    FolderPageMapFastOutcome {
        status: if is_ready {
            FolderPageMapFastStatus::Ready
        } else if matches!(lane.status, FolderPageMapFastStatus::Ready) {
            FolderPageMapFastStatus::Failed
        } else {
            lane.status
        },
        page_map: is_ready.then(|| BookPageMap::new(revision, lane.pages)),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// DIR 本の FAST 経路の出力。軽量メタデータで Page Map を作る途中結果を返す。
pub(crate) fn build_folder_page_map_fast_lanes(
    reader: &FolderImageReader,
) -> FolderPageMapFastLaneOutput {
    let mut pages = Vec::with_capacity(reader.page_count() as usize);

    for info in reader.page_map_image_entry_infos() {
        let Some(name) = info.path.file_name().and_then(|s| s.to_str()) else {
            return FolderPageMapFastLaneOutput {
                status: FolderPageMapFastStatus::Failed,
                pages: Vec::new(),
            };
        };
        let Some(format_hint) = page_map_format_for_name(name) else {
            return FolderPageMapFastLaneOutput {
                status: FolderPageMapFastStatus::RequiresComplete,
                pages: Vec::new(),
            };
        };
        let format_hint = match format_hint {
            PageFormat::Jpeg | PageFormat::Png => format_hint,
            _ => {
                return FolderPageMapFastLaneOutput {
                    status: FolderPageMapFastStatus::RequiresComplete,
                    pages: Vec::new(),
                };
            }
        };
        let raw = match reader.read_page_n(info.page_index) {
            Ok(raw) => raw,
            Err(_) => {
                return FolderPageMapFastLaneOutput {
                    status: FolderPageMapFastStatus::Failed,
                    pages: Vec::new(),
                };
            }
        };
        let (format, width, height) =
            match read_image_metadata_lightweight_first(&raw, Some(format_hint)) {
                LightweightImageMetadataOutcome::Ready {
                    format,
                    width,
                    height,
                } => (format, width, height),
                LightweightImageMetadataOutcome::FallbackRequired
                | LightweightImageMetadataOutcome::Unsupported => {
                    return FolderPageMapFastLaneOutput {
                        status: FolderPageMapFastStatus::RequiresComplete,
                        pages: Vec::new(),
                    };
                }
            };
        pages.push(PageDescriptor {
            format,
            width,
            height,
        });
    }

    FolderPageMapFastLaneOutput {
        status: FolderPageMapFastStatus::Ready,
        pages,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// DIR 本の slow Page Map を組み立てる。FAST が使えない呼び出し側のフォールバック経路。
pub(crate) fn build_book_page_map_slow_from_folder_reader(
    reader: &FolderImageReader,
    revision: SourceRevision,
) -> FolderPageMapSlowOutcome {
    let mut pages = Vec::with_capacity(reader.page_count() as usize);
    let mut saw_image_entry = false;

    for info in reader.page_map_image_entry_infos() {
        saw_image_entry = true;
        let raw = match reader.read_page_n(info.page_index) {
            Ok(raw) => raw,
            Err(_) => {
                return FolderPageMapSlowOutcome::Failure(FolderPageMapSlowFailure {
                    page_index: Some(info.page_index),
                    entry_index: Some(info.page_index),
                    reason: FolderPageMapSlowFailureReason::ReadError,
                });
            }
        };
        let (format, width, height) = match read_image_metadata(&raw) {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                return FolderPageMapSlowOutcome::Failure(FolderPageMapSlowFailure {
                    page_index: Some(info.page_index),
                    entry_index: Some(info.page_index),
                    reason: FolderPageMapSlowFailureReason::UnsupportedFormat,
                });
            }
            Err(_) => {
                return FolderPageMapSlowOutcome::Failure(FolderPageMapSlowFailure {
                    page_index: Some(info.page_index),
                    entry_index: Some(info.page_index),
                    reason: FolderPageMapSlowFailureReason::MetadataError,
                });
            }
        };
        pages.push(PageDescriptor {
            format,
            width,
            height,
        });
    }

    if !saw_image_entry {
        return FolderPageMapSlowOutcome::Failure(FolderPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: FolderPageMapSlowFailureReason::NoImageEntries,
        });
    }

    FolderPageMapSlowOutcome::Success(BookPageMap::new(revision, pages))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_book_page_map_fast_from_folder_path(
    path: &Path,
    revision: SourceRevision,
) -> FolderPageMapFastOutcome {
    match FolderImageReader::open(path) {
        Ok(reader) => build_book_page_map_fast_from_folder_reader(&reader, revision),
        Err(_) => FolderPageMapFastOutcome {
            status: FolderPageMapFastStatus::Failed,
            page_map: None,
        },
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_book_page_map_slow_from_folder_path(
    path: &Path,
    revision: SourceRevision,
) -> FolderPageMapSlowOutcome {
    match FolderImageReader::open(path) {
        Ok(reader) => build_book_page_map_slow_from_folder_reader(&reader, revision),
        Err(_) => FolderPageMapSlowOutcome::Failure(FolderPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: FolderPageMapSlowFailureReason::ReadError,
        }),
    }
}

pub(crate) fn build_zip_page_map_fast_lanes(reader: &ZipReader) -> ZipPageMapFastOutput {
    let started = Instant::now();
    let mut pages = Vec::new();
    let mut issue = None;
    let mut status = ZipPageMapFastStatus::Ready;
    let mut compressed_bytes_seen = 0u64;
    let mut uncompressed_bytes_seen = 0u64;
    let mut lightweight_pages = 0usize;
    let mut compressed_bytes_touched = 0usize;
    let mut uncompressed_bytes_produced = 0usize;

    for info in reader.page_map_image_entry_infos() {
        compressed_bytes_seen += info.compressed_size;
        uncompressed_bytes_seen += info.uncompressed_size;

        let meta = match read_zip_page_map_fast_entry(reader, info.entry_index as usize) {
            ZipPageMapFastEntryResult::Ready(meta) => meta,
            ZipPageMapFastEntryResult::SlowRequired(reason) => {
                status = ZipPageMapFastStatus::SlowRequired(reason);
                issue = Some(ZipPageMapIssue {
                    page_index: info.page_index,
                    entry_index: info.entry_index,
                    reason: match reason {
                        ZipPageMapSlowReason::HeaderLimit => ZipPageMapIssueReason::HeaderLimit,
                        ZipPageMapSlowReason::UnsupportedLightweightFormat => {
                            ZipPageMapIssueReason::UnsupportedLightweightFormat
                        }
                    },
                });
                pages.clear();
                break;
            }
            ZipPageMapFastEntryResult::Failed(reason) => {
                status = ZipPageMapFastStatus::Failed(reason);
                issue = Some(ZipPageMapIssue {
                    page_index: info.page_index,
                    entry_index: info.entry_index,
                    reason,
                });
                pages.clear();
                break;
            }
        };
        if meta.used_lightweight {
            lightweight_pages += 1;
        }
        compressed_bytes_touched += meta.compressed_bytes_touched;
        uncompressed_bytes_produced += meta.uncompressed_bytes_produced;
        pages.push(PageDescriptor {
            format: meta.format,
            width: meta.width,
            height: meta.height,
        });
    }

    if matches!(status, ZipPageMapFastStatus::Ready) && pages.len() != reader.page_count() as usize
    {
        status = ZipPageMapFastStatus::Failed(ZipPageMapIssueReason::ZipStructure);
        issue = None;
        pages.clear();
    }

    ZipPageMapFastOutput {
        status,
        pages,
        issue,
        compressed_bytes_seen,
        uncompressed_bytes_seen,
        lightweight_pages,
        compressed_bytes_touched,
        uncompressed_bytes_produced,
        elapsed: started.elapsed(),
    }
}

/// ZIP/CBZ の FAST Page Map を組み立てる。slow が必要ならその旨を結果で返す。
pub fn build_zip_page_map_fast(path: &Path, revision: SourceRevision) -> FastBuildOutcome {
    let started = Instant::now();
    let reader = match ZipReader::open(path) {
        Ok(reader) => reader,
        Err(e) => {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "page-map fast open failed"
            );
            return FastBuildOutcome {
                status: PageMapBuildStatus::Failed(ZipPageMapIssueReason::ZipStructure),
                page_map: None,
            };
        }
    };

    let Some(_page0_info) = reader.page_map_image_entry_infos().next() else {
        tracing::debug!(
            path = %path.display(),
            "page-map fast failed because no image entries were found"
        );
        return FastBuildOutcome {
            status: PageMapBuildStatus::Failed(ZipPageMapIssueReason::UnsupportedFormat),
            page_map: None,
        };
    };

    let fast_lane_output = build_zip_page_map_fast_lanes(&reader);
    let outcome = assemble_zip_fast_page_map(
        revision,
        reader.page_count(),
        fast_lane_output.status,
        fast_lane_output.pages,
    );
    tracing::debug!(
        path = %path.display(),
        elapsed_ms = started.elapsed().as_millis(),
        status = ?outcome.status,
        "page-map fast outcome"
    );
    outcome
}

fn read_zip_page_map_fast_entry(
    reader: &ZipReader,
    entry_index: usize,
) -> ZipPageMapFastEntryResult {
    match reader.read_page_map_metadata_for_entry_index(entry_index) {
        Ok(ZipPageMapReadOutcome::Ready(meta)) => {
            ZipPageMapFastEntryResult::Ready(ZipPageMapFastEntrySuccess {
                format: meta.format,
                width: meta.width,
                height: meta.height,
                used_lightweight: meta.used_lightweight,
                compressed_bytes_touched: meta.compressed_bytes_touched,
                uncompressed_bytes_produced: meta.uncompressed_bytes_produced,
            })
        }
        Ok(ZipPageMapReadOutcome::SlowRequired(reason)) => {
            ZipPageMapFastEntryResult::SlowRequired(reason)
        }
        Ok(ZipPageMapReadOutcome::Failed(reason)) => ZipPageMapFastEntryResult::Failed(reason),
        Err(_) => ZipPageMapFastEntryResult::Failed(ZipPageMapIssueReason::ZipStructure),
    }
}

/// ファイル名からの形式ヒント。実データ判定の代わりにはならない。
pub(crate) fn page_map_format_for_name(name: &str) -> Option<PageFormat> {
    match name.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some(PageFormat::Jpeg),
        "png" => Some(PageFormat::Png),
        "webp" => Some(PageFormat::WebP),
        "avif" | "avifs" => Some(PageFormat::Avif),
        "bmp" => Some(PageFormat::Bmp),
        "tif" | "tiff" => Some(PageFormat::Tiff),
        "gif" => Some(PageFormat::Gif),
        _ => None,
    }
}
