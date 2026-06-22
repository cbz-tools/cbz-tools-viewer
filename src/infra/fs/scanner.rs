//! ライブラリ用 filesystem scanner。
//!
//! `current_dir` 直下を列挙し、Folder / FolderBook / ImageFile / Archive を分類する。

use std::{path::Path, sync::Arc, time::SystemTime};

use anyhow::Result;

use crate::domain::archive::{BookId, BookMeta, FolderMeta, ImageFileMeta, LibraryEntry};
use crate::util::archive_path::is_supported_archive_path;

pub fn scan_dir(root: &Path) -> Result<Vec<LibraryEntry>> {
    let mut entries = Vec::new();
    for dirent in std::fs::read_dir(root)? {
        let Ok(dirent) = dirent else {
            continue;
        };
        let path = dirent.path();
        let Ok(meta) = dirent.metadata() else {
            continue;
        };

        if meta.is_dir() {
            let title: Arc<str> = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(Arc::from)
                .unwrap_or_else(|| Arc::from(path.to_string_lossy().as_ref()));
            let folder_meta = FolderMeta {
                path: Arc::from(path.as_path()),
                title,
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            };
            // 直下画像を持つディレクトリだけを FolderBook に昇格する。
            // ここで Folder と分けておくと、Library の本移動・Viewer 入口・削除の
            // いずれでも圧縮書庫と同じ本扱いにできる。
            if has_direct_image(&path) {
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
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if is_supported_archive_path(&path) {
                entries.push(LibraryEntry::Archive(BookMeta {
                    id: BookId::from_path(&path),
                    path: Arc::from(path.as_path()),
                    title,
                    size: meta.len(),
                    modified,
                    page_count: None,
                }));
            } else if is_supported_image(&path) {
                entries.push(LibraryEntry::ImageFile(ImageFileMeta {
                    path: Arc::from(path.as_path()),
                    title,
                    size: meta.len(),
                    modified,
                }));
            }
            continue;
        }
    }

    Ok(entries)
}

fn has_direct_image(path: &Path) -> bool {
    let Ok(read_dir) = std::fs::read_dir(path) else {
        return false;
    };

    read_dir.filter_map(|entry| entry.ok()).any(|entry| {
        let Ok(meta) = entry.metadata() else {
            return false;
        };
        meta.is_file() && is_supported_image(&entry.path())
    })
}

fn is_supported_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|x| x.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif" | "avifs" | "bmp" | "tif" | "tiff"
    )
}
