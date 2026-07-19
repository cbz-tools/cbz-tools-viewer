use std::path::PathBuf;

use crate::util::archive_path::is_supported_archive_path as is_supported_archive_path_shared;
use crate::util::path_eq;

pub(super) fn normalize_dir_path(path: PathBuf) -> PathBuf {
    let canonical = std::fs::canonicalize(&path).unwrap_or(path);
    strip_windows_verbatim_prefix(canonical)
}

pub(super) fn normalize_drop_select_path(path: PathBuf) -> PathBuf {
    let canonical = std::fs::canonicalize(&path).unwrap_or(path);
    strip_windows_verbatim_prefix(canonical)
}

pub(super) fn sanitize_favorite_dirs<I>(paths: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut out = Vec::new();
    for path in paths {
        if !path.is_dir() {
            continue;
        }
        let normalized = normalize_dir_path(path);
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Path;

        let raw = path.to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return Path::new(&format!(r"\\{rest}")).to_path_buf();
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return Path::new(rest).to_path_buf();
        }
        path
    }
    #[cfg(not(windows))]
    {
        path
    }
}

pub(super) fn is_supported_archive_path(path: &std::path::Path) -> bool {
    is_supported_archive_path_shared(path)
}

pub(super) fn paths_equivalent_for_selection(a: &std::path::Path, b: &std::path::Path) -> bool {
    path_eq::paths_equivalent_for_selection(a, b)
}

#[cfg(target_os = "windows")]
pub(super) fn monitor_rect_from_point(x: f32, y: f32) -> Option<[f32; 4]> {
    crate::platform::windows_monitor::monitor_rect_from_point(x, y)
}

#[cfg(not(target_os = "windows"))]
pub(super) fn monitor_rect_from_point(_x: f32, _y: f32) -> Option<[f32; 4]> {
    None
}
