use std::path::{Path, PathBuf};

use eframe::egui;

use crate::domain::archive::LibraryEntry;
use crate::platform::windows_drag;

#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(windows)]
// raw-window-handle で eframe から Win32 HWND を取り出す。
// Windows の COM/OLE 外部ドラッグで SHDoDragDrop を使うために必要。
// 代替 HWND API が eframe に入るまでは、外部ドラッグを再確認せずには外せない。
use std::{ffi::OsStr, iter::once, os::windows::ffi::OsStrExt};
#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::HWND,
    Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
};

use super::platform::{is_supported_archive_path, normalize_drop_select_path};
use super::{App, BookSettingsClearDialogChoice, DeleteDialogChoice};

#[cfg(windows)]
fn main_window_hwnd(frame: &eframe::Frame) -> Option<isize> {
    let handle = frame.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
fn main_window_hwnd(_frame: &eframe::Frame) -> Option<isize> {
    None
}

enum DroppedItemKind {
    Folder,
    Book,
    UnsupportedFile,
    NoPath,
}

impl App {
    pub(super) fn open_in_explorer(&self, path: &Path) {
        let _ = self;
        #[cfg(windows)]
        {
            open_with_shell_execute(path);
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            tracing::warn!("open-in-explorer is only supported on Windows");
        }
    }

    pub(super) fn begin_rename(&mut self, idx: usize) {
        if let Some(entry) = self.book_entry_at(idx) {
            let stem = entry
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.renaming = Some((idx, stem));
        }
    }

    pub(super) fn commit_rename(&mut self, idx: usize, new_stem: String) {
        let book = match self.book_entry_at(idx).cloned() {
            Some(e) => e,
            None => return,
        };
        let entry = book.path.as_ref().to_owned();
        let ext = entry
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let new_name = format!("{}{}", new_stem.trim(), ext);
        if new_name.is_empty() || new_name == ext {
            return;
        }
        if let Some(parent) = entry.parent() {
            let new_path = parent.join(&new_name);
            if new_path == entry {
                return;
            }
            if let Err(e) = std::fs::rename(&entry, &new_path) {
                tracing::error!("rename failed: {e}");
            } else {
                tracing::info!("renamed: {} → {}", entry.display(), new_path.display());
                self.apply_renamed_path_diff(entry.as_path(), new_path.as_path(), &book);

                if let Some(snapshot) = self.history_snapshot() {
                    self.set_pending_after_load(
                        Some(new_path.clone()),
                        Some(snapshot.scroll_offset),
                    );
                    self.load_library_dir(snapshot.dir);
                } else if let Some(parent) = entry.parent().map(PathBuf::from) {
                    self.set_pending_after_load(Some(new_path.clone()), None);
                    self.load_library_dir(parent);
                }
            }
        }
    }

    pub(super) fn begin_delete(&mut self, idxs: Vec<usize>) {
        if !idxs.is_empty() {
            self.deleting = Some(idxs);
            self.delete_dialog_choice = DeleteDialogChoice::Ok;
        }
    }

    pub(super) fn begin_clear_book_settings(&mut self, idxs: Vec<usize>) {
        let targets: Vec<usize> = idxs
            .into_iter()
            .filter(|&idx| {
                matches!(
                    self.library.entries.get(idx),
                    Some(LibraryEntry::Archive(_) | LibraryEntry::FolderBook(_))
                )
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        self.book_settings_clearing = Some(targets);
        self.book_settings_clear_dialog_choice = BookSettingsClearDialogChoice::Reset;
    }

    pub(super) fn commit_delete(&mut self, idxs: Vec<usize>, _ctx: &egui::Context) {
        let mut delete_idxs = idxs;
        delete_idxs.sort_unstable();
        delete_idxs.dedup();
        let targets: Vec<PathBuf> = delete_idxs
            .iter()
            .filter_map(|&i| self.entry_path_at(i))
            .collect();
        let mut had_failure = false;
        for path in &targets {
            let result = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            if let Err(e) = result {
                tracing::error!(
                    "delete failed: {} kind={} — {e}",
                    path.display(),
                    if path.is_dir() { "folder" } else { "file" }
                );
                had_failure = true;
                break;
            } else {
                tracing::info!(
                    "deleted: {} kind={}",
                    path.display(),
                    if path.is_dir() { "folder" } else { "file" }
                );
                self.apply_deleted_path_diff(path.as_path(), None);
            }
        }

        if had_failure {
            tracing::error!(
                "delete aborted: treated as whole-operation failure (already deleted entries remain)"
            );
        }
    }

    pub(super) fn do_copy(&self, idxs: Vec<usize>) {
        let paths: Vec<PathBuf> = idxs
            .iter()
            .filter_map(|&i| self.book_entry_at(i))
            .map(|e| e.path.as_ref().to_owned())
            .collect();
        if paths.is_empty() {
            return;
        }
        copy_files_to_clipboard(&paths);
    }

    pub(super) fn start_external_drag(&mut self, idxs: &[usize], frame: &eframe::Frame) {
        let paths: Vec<PathBuf> = idxs
            .iter()
            .filter_map(|&i| self.book_entry_at(i))
            .map(|e| e.path.as_ref().to_owned())
            .collect();
        if paths.is_empty() {
            return;
        }
        let Some(hwnd) = main_window_hwnd(frame) else {
            tracing::error!("external drag unavailable: failed to get HWND");
            return;
        };

        if let Err(e) = windows_drag::start_file_drag(hwnd, &paths) {
            tracing::error!(
                "external drag failed: count={} first={} — {e}",
                paths.len(),
                paths[0].display()
            );
        }
        self.suppress_pointer_until_release = true;
        self.suppress_next_dropped_files = true;
    }

    fn classify_dropped_item(path: Option<&std::path::Path>) -> DroppedItemKind {
        let Some(path) = path else {
            return DroppedItemKind::NoPath;
        };
        if path.is_dir() {
            DroppedItemKind::Folder
        } else if path.is_file() && is_supported_archive_path(path) {
            DroppedItemKind::Book
        } else {
            DroppedItemKind::UnsupportedFile
        }
    }

    pub(super) fn handle_external_drop_in_app(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        if files.is_empty() {
            return;
        }

        if self.suppress_next_dropped_files {
            return;
        }

        let first = &files[0];
        match Self::classify_dropped_item(first.path.as_deref()) {
            DroppedItemKind::NoPath => {
                self.pending_drop_select = None;
            }
            DroppedItemKind::Folder => {
                self.pending_drop_select = None;
                let Some(path) = first.path.clone() else {
                    return;
                };
                log::debug!("[drop] kind=folder path={}", path.display());
                log::debug!("[drop] navigate dir={}", path.display());
                self.library.history_back.clear();
                self.library.history_forward.clear();
                self.load_library_dir(path);
            }
            DroppedItemKind::Book => {
                let Some(path) = first.path.clone() else {
                    return;
                };
                let target = normalize_drop_select_path(path.clone());
                let parent = path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| path.clone());
                log::debug!(
                    "[drop] kind=book path={} parent={}",
                    path.display(),
                    parent.display()
                );
                log::debug!("[drop] pending_select target={}", target.display());
                log::debug!("[drop] navigate dir={}", parent.display());
                self.pending_drop_select = Some(target);
                self.library.history_back.clear();
                self.library.history_forward.clear();
                self.load_library_dir(parent);
            }
            DroppedItemKind::UnsupportedFile => {
                self.pending_drop_select = None;
                let Some(path) = first.path.clone() else {
                    return;
                };
                let Some(parent) = path.parent().map(PathBuf::from) else {
                    log::debug!("[drop] ignored reason=no_parent path={}", path.display());
                    return;
                };
                log::debug!(
                    "[drop] kind=unsupported_file path={} parent={}",
                    path.display(),
                    parent.display()
                );
                log::debug!("[drop] navigate dir={}", parent.display());
                self.library.history_back.clear();
                self.library.history_forward.clear();
                self.load_library_dir(parent);
            }
        }
    }

    pub(super) fn commit_clear_book_settings(&mut self, idxs: Vec<usize>, ctx: &egui::Context) {
        let mut clear_idxs = idxs;
        clear_idxs.sort_unstable();
        clear_idxs.dedup();
        let mut targets: Vec<PathBuf> = Vec::new();
        for idx in clear_idxs {
            if let Some(LibraryEntry::Archive(_) | LibraryEntry::FolderBook(_)) =
                self.library.entries.get(idx)
            {
                if let Some(path) = self.entry_path_at(idx) {
                    targets.push(path);
                }
            }
        }
        targets.sort();
        targets.dedup();
        if targets.is_empty() {
            return;
        }

        for path in &targets {
            crate::domain::archive_settings::SettingsStore::remove_path_from_disk(path.as_path());
            self.library
                .remove_reading_hud_state_for_path(path.as_path());
        }
        self.library.mark_filter_dirty();
        self.library.reset_context_menu_cache = true;
        ctx.request_repaint();
    }
}

#[cfg(windows)]
fn open_with_shell_execute(path: &Path) {
    let file = utf16z(OsStr::new("explorer.exe"));
    let params = if path.is_dir() {
        utf16z_quote_path(path)
    } else {
        utf16z_select_path(path)
    };

    // SAFETY:
    // 文字列バッファはこの呼び出し中ずっと生存し、すべて NUL 終端済み。
    // `ShellExecuteW` の失敗は戻り値で判定し、ここでは所有権移動も発生しない。
    let result = unsafe {
        ShellExecuteW(
            Some(HWND(std::ptr::null_mut())),
            PCWSTR::null(),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    let result_code = result.0 as usize;
    if result_code <= 32 {
        tracing::error!(
            path = %path.display(),
            result = result_code,
            "open-in-explorer failed"
        );
    }
}

#[cfg(windows)]
fn utf16z(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(once(0)).collect()
}

#[cfg(windows)]
fn utf16z_quote_path(path: &Path) -> Vec<u16> {
    let mut wide = Vec::with_capacity(path.as_os_str().encode_wide().count() + 3);
    wide.push('"' as u16);
    wide.extend(path.as_os_str().encode_wide());
    wide.push('"' as u16);
    wide.push(0);
    wide
}

#[cfg(windows)]
fn utf16z_select_path(path: &Path) -> Vec<u16> {
    let mut wide = Vec::with_capacity(path.as_os_str().encode_wide().count() + 11);
    wide.extend("/select,\"".encode_utf16());
    wide.extend(path.as_os_str().encode_wide());
    wide.push('"' as u16);
    wide.push(0);
    wide
}

fn copy_files_to_clipboard(paths: &[PathBuf]) {
    // Windows の CF_HDROP ファイル一覧コピーには clipboard-win を使う。
    // egui の clipboard はこの用途では文字列専用なので、ここは置き換えない。
    use clipboard_win::{formats, Clipboard, Setter};
    let strings: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    match Clipboard::new_attempts(10) {
        Ok(_clip) => match formats::FileList.write_clipboard(strings.as_slice()) {
            Ok(_) => tracing::info!("copied {} file(s) to clipboard", paths.len()),
            Err(e) => tracing::error!("clipboard write failed: {:?}", e),
        },
        Err(e) => tracing::error!("clipboard open failed: {:?}", e),
    }
}
