//! ライブラリ用 filesystem scanner。
//!
//! `current_dir` 直下を列挙し、Folder / FolderBook / ImageFile / VideoFile /
//! Archive を分類する。

use std::{collections::HashMap, path::Path, sync::Arc, time::SystemTime};

use anyhow::Result;

use crate::domain::archive::{
    BookId, BookMeta, FolderMeta, ImageFileMeta, LibraryEntry, VideoFileMeta,
};
use crate::domain::page_map::SourceRevision;
use crate::util::archive_path::{is_supported_archive_path, is_supported_image_path};

const VIDEO_EXTENSIONS: [&str; 7] = ["mp4", "mkv", "webm", "avi", "mov", "wmv", "m4v"];

#[derive(Clone, Debug)]
pub struct SourceSnapshot {
    pub path: Arc<Path>,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug)]
pub struct ScannedDir {
    pub entries: Vec<LibraryEntry>,
    pub source_snapshots: HashMap<BookId, SourceSnapshot>,
    pub static_page_counts: HashMap<BookId, (SourceRevision, usize)>,
}

/// Scans a library directory and prepares hot-path indexes while still on the
/// background scan thread. Static candidates without a same-revision memo are
/// recorded with a zero page count; Page Map I/O is deferred to thumbnail work.
pub fn scan_dir_with_hot_path_indexes(
    root: &Path,
    previous_static_page_counts: &HashMap<BookId, (SourceRevision, usize)>,
) -> Result<ScannedDir> {
    let mut entries = Vec::new();
    let mut source_snapshots = HashMap::new();
    let mut static_page_candidates = Vec::new();
    for dirent in std::fs::read_dir(root)? {
        let Ok(dirent) = dirent else {
            continue;
        };
        let path = dirent.path();
        let Ok(meta) = dirent.metadata() else {
            continue;
        };

        if meta.is_dir() {
            let id = BookId::from_path(&path);
            let title: Arc<str> = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(Arc::from)
                .unwrap_or_else(|| Arc::from(path.to_string_lossy().as_ref()));
            let revision_modified = meta.modified().ok();
            let folder_meta = FolderMeta {
                id,
                path: Arc::from(path.as_path()),
                title,
                modified: revision_modified.unwrap_or(SystemTime::UNIX_EPOCH),
                revision_modified,
            };
            // 直下画像を持つディレクトリだけを FolderBook に昇格する。
            // ここで Folder と分けておくと、Library の本移動・Viewer 入口・削除の
            // いずれでも圧縮書庫と同じ本扱いにできる。
            if has_direct_image(&path) {
                source_snapshots.insert(
                    folder_meta.id.clone(),
                    SourceSnapshot {
                        path: Arc::clone(&folder_meta.path),
                        size: 0,
                        modified: folder_meta.revision_modified,
                    },
                );
                static_page_candidates.push(folder_meta.id.clone());
                entries.push(LibraryEntry::FolderBook(folder_meta));
            } else {
                entries.push(LibraryEntry::Folder(folder_meta));
            }
            continue;
        }

        if meta.is_file() {
            let title: Arc<str> = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
                .map(Arc::from)
                .unwrap_or_else(|| Arc::from(path.to_string_lossy().as_ref()));
            let revision_modified = meta.modified().ok();
            let modified = revision_modified.unwrap_or(SystemTime::UNIX_EPOCH);
            if is_supported_archive_path(&path) {
                let id = BookId::from_path(&path);
                entries.push(LibraryEntry::Archive(BookMeta {
                    id: id.clone(),
                    path: Arc::from(path.as_path()),
                    title,
                    size: meta.len(),
                    modified,
                    page_count: None,
                }));
                source_snapshots.insert(
                    id.clone(),
                    SourceSnapshot {
                        path: Arc::from(path.as_path()),
                        size: meta.len(),
                        modified: Some(modified),
                    },
                );
                static_page_candidates.push(id);
            } else if is_supported_image_path(&path) {
                let id = BookId::from_path(&path);
                entries.push(LibraryEntry::ImageFile(ImageFileMeta {
                    id: id.clone(),
                    path: Arc::from(path.as_path()),
                    title,
                    size: meta.len(),
                    modified,
                }));
                source_snapshots.insert(
                    id,
                    SourceSnapshot {
                        path: Arc::from(path.as_path()),
                        size: meta.len(),
                        modified: Some(modified),
                    },
                );
            } else if is_supported_video_path(&path) {
                let id = BookId::from_path(&path);
                log::trace!(
                    "[video] entry detected path={} size={}",
                    path.display(),
                    meta.len()
                );
                entries.push(LibraryEntry::VideoFile(VideoFileMeta {
                    id: id.clone(),
                    path: Arc::from(path.as_path()),
                    title,
                    size: meta.len(),
                    modified,
                }));
                source_snapshots.insert(
                    id,
                    SourceSnapshot {
                        path: Arc::from(path.as_path()),
                        size: meta.len(),
                        modified: Some(modified),
                    },
                );
            }
            continue;
        }
    }

    let static_page_counts = static_page_candidates
        .into_iter()
        .filter_map(|id| {
            let snapshot = source_snapshots.get(&id)?;
            let revision = SourceRevision::from_file_state(snapshot.size, snapshot.modified);
            let page_count = previous_static_page_counts
                .get(&id)
                .filter(|(previous_revision, _)| previous_revision == &revision)
                .map_or(0, |(_, page_count)| *page_count);
            Some((id, (revision, page_count)))
        })
        .collect();
    Ok(ScannedDir {
        entries,
        source_snapshots,
        static_page_counts,
    })
}

pub fn scan_path(path: &Path) -> Result<Option<LibraryEntry>> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(None);
    };

    if meta.is_dir() {
        let id = BookId::from_path(path);
        let title: Arc<str> = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(Arc::from)
            .unwrap_or_else(|| Arc::from(path.to_string_lossy().as_ref()));
        let revision_modified = meta.modified().ok();
        let folder_meta = FolderMeta {
            id,
            path: Arc::from(path),
            title,
            modified: revision_modified.unwrap_or(SystemTime::UNIX_EPOCH),
            revision_modified,
        };
        if has_direct_image(path) {
            return Ok(Some(LibraryEntry::FolderBook(folder_meta)));
        }
        return Ok(Some(LibraryEntry::Folder(folder_meta)));
    }

    if meta.is_file() {
        let title: Arc<str> = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .map(Arc::from)
            .unwrap_or_else(|| Arc::from(path.to_string_lossy().as_ref()));
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if is_supported_archive_path(path) {
            return Ok(Some(LibraryEntry::Archive(BookMeta {
                id: BookId::from_path(path),
                path: Arc::from(path),
                title,
                size: meta.len(),
                modified,
                page_count: None,
            })));
        }
        if is_supported_image_path(path) {
            let id = BookId::from_path(path);
            return Ok(Some(LibraryEntry::ImageFile(ImageFileMeta {
                id,
                path: Arc::from(path),
                title,
                size: meta.len(),
                modified,
            })));
        }
        if is_supported_video_path(path) {
            let id = BookId::from_path(path);
            log::trace!(
                "[video] entry detected path={} size={}",
                path.display(),
                meta.len()
            );
            return Ok(Some(LibraryEntry::VideoFile(VideoFileMeta {
                id,
                path: Arc::from(path),
                title,
                size: meta.len(),
                modified,
            })));
        }
    }

    Ok(None)
}

fn has_direct_image(path: &Path) -> bool {
    let Ok(read_dir) = std::fs::read_dir(path) else {
        return false;
    };

    read_dir.filter_map(|entry| entry.ok()).any(|entry| {
        let Ok(meta) = entry.metadata() else {
            return false;
        };
        meta.is_file() && is_supported_image_path(&entry.path())
    })
}

fn is_supported_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .is_some_and(|extension| VIDEO_EXTENSIONS.contains(&extension.as_str()))
}
