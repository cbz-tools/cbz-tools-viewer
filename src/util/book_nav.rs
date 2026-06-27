use std::path::{Path, PathBuf};

use crate::util::archive_path::{is_supported_archive_path, is_supported_image_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDirection {
    Previous,
    Next,
}

pub fn list_supported_books_in_dir(
    current: &Path,
    is_supported: fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let Some(dir) = current.parent() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut books: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_supported(p.as_path()))
        .collect();
    books.sort_by(|a, b| {
        let an = a.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let bn = b.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        crate::util::natural_sort::compare(an, bn)
    });
    books
}

fn dir_contains_supported_images(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.filter_map(|e| e.ok()).any(|entry| {
        let path = entry.path();
        path.is_file() && is_supported_image_path(path.as_path())
    })
}

pub fn is_supported_navigation_book_path(path: &Path) -> bool {
    (path.is_file() && is_supported_archive_path(path))
        || (path.is_dir() && dir_contains_supported_images(path))
}

pub fn list_supported_navigation_books_in_dir(current: &Path) -> Vec<PathBuf> {
    let Some(dir) = current.parent() else {
        return Vec::new();
    };
    let mut books = list_supported_books_in_dir(current, is_supported_archive_path);
    let Ok(rd) = std::fs::read_dir(dir) else {
        return books;
    };
    let dir_books: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && is_supported_navigation_book_path(p.as_path())
                && !books.iter().any(|existing| existing == p)
        })
        .collect();
    books.extend(dir_books);
    books.sort_by(|a, b| {
        let an = a.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let bn = b.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        crate::util::natural_sort::compare(an, bn)
    });
    books
}

pub fn adjacent_paths(paths: &[PathBuf], current: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(idx) = paths.iter().position(|p| p.as_path() == current) else {
        return (None, None);
    };
    let prev = idx.checked_sub(1).and_then(|i| paths.get(i)).cloned();
    let next = paths.get(idx + 1).cloned();
    (prev, next)
}

pub fn move_target_index(
    len: usize,
    current_index: usize,
    direction: NavDirection,
    wrap_next: bool,
) -> Option<usize> {
    if len <= 1 || current_index >= len {
        return None;
    }
    match direction {
        NavDirection::Previous => current_index.checked_sub(1),
        NavDirection::Next => {
            if current_index + 1 < len {
                Some(current_index + 1)
            } else if wrap_next {
                Some(0)
            } else {
                None
            }
        }
    }
}

pub fn move_target_path(
    paths: &[PathBuf],
    current: &Path,
    direction: NavDirection,
    wrap_next: bool,
) -> Option<PathBuf> {
    let idx = paths.iter().position(|p| p.as_path() == current)?;
    let target_idx = move_target_index(paths.len(), idx, direction, wrap_next)?;
    paths.get(target_idx).cloned()
}

pub fn move_target_path_from_insertion_index(
    paths: &[PathBuf],
    insertion_index: usize,
    direction: NavDirection,
    wrap_next: bool,
) -> Option<PathBuf> {
    match direction {
        NavDirection::Previous => insertion_index
            .checked_sub(1)
            .and_then(|idx| paths.get(idx))
            .cloned(),
        NavDirection::Next => {
            if insertion_index < paths.len() {
                paths.get(insertion_index).cloned()
            } else if wrap_next && !paths.is_empty() {
                paths.first().cloned()
            } else {
                None
            }
        }
    }
}
