use std::path::PathBuf;

use eframe::egui;

use crate::domain::archive::{BookId, BookMeta, LibraryEntry};
use crate::infra::favorite_store::FavoriteState;
use crate::ui::library::HistoryEntry;

use super::App;
use super::platform::{
    is_supported_archive_path, normalize_dir_path, paths_equivalent_for_selection,
};

#[derive(Clone, Debug)]
pub(super) struct PendingAfterLoad {
    pub(super) selected_path: Option<PathBuf>,
    pub(super) scroll_y: Option<f32>,
}

impl App {
    pub(super) fn book_entry_ref(entry: &LibraryEntry) -> Option<&BookMeta> {
        match entry {
            LibraryEntry::Archive(entry) => Some(entry),
            LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => {
                None
            }
        }
    }

    pub(super) fn library_navigation_book_path(entry: &LibraryEntry) -> Option<PathBuf> {
        match entry {
            // Viewer の本移動列に入るのは Archive と FolderBook だけ。
            LibraryEntry::Archive(entry) => Some(entry.path.as_ref().to_path_buf()),
            LibraryEntry::FolderBook(entry) => Some(entry.path.as_ref().to_path_buf()),
            LibraryEntry::Folder(_) | LibraryEntry::ImageFile(_) => None,
        }
    }

    pub(super) fn library_navigation_book_id(entry: &LibraryEntry) -> Option<BookId> {
        match entry {
            // FolderBook は Directory でも `BookId` 相当の安定キーが必要なので、
            // path から同じハッシュを作って navigation / thumb で共通利用する。
            LibraryEntry::Archive(entry) => Some(entry.id.clone()),
            LibraryEntry::FolderBook(entry) => Some(BookId::from_path(entry.path.as_ref())),
            LibraryEntry::Folder(_) | LibraryEntry::ImageFile(_) => None,
        }
    }

    pub(super) fn book_entry_at(&self, idx: usize) -> Option<&BookMeta> {
        self.library.entries.get(idx).and_then(Self::book_entry_ref)
    }

    pub(super) fn entry_path_at(&self, idx: usize) -> Option<PathBuf> {
        self.library.entries.get(idx).map(|entry| match entry {
            LibraryEntry::Archive(entry) => entry.path.as_ref().to_path_buf(),
            LibraryEntry::Folder(entry) | LibraryEntry::FolderBook(entry) => {
                entry.path.as_ref().to_path_buf()
            }
            LibraryEntry::ImageFile(entry) => entry.path.as_ref().to_path_buf(),
        })
    }

    pub(super) fn selected_entry_path(&self) -> Option<PathBuf> {
        self.library
            .selected_idx
            .and_then(|idx| self.entry_path_at(idx))
    }

    pub(super) fn history_snapshot(&self) -> Option<HistoryEntry> {
        Some(HistoryEntry {
            dir: self.library.current_dir.clone()?,
            selected_path: self.selected_entry_path(),
            scroll_offset: self.library.scroll_y,
        })
    }

    pub(super) fn set_pending_after_load(
        &mut self,
        selected_path: Option<PathBuf>,
        scroll_y: Option<f32>,
    ) {
        self.pending_after_load = Some(PendingAfterLoad {
            selected_path,
            scroll_y,
        });
    }

    pub(super) fn restore_selection_by_path(&mut self, path: Option<PathBuf>) {
        let Some(path) = path else {
            self.library.selected_idx = None;
            self.library.selected_set.clear();
            self.library.anchor_idx = None;
            return;
        };
        self.library.selected_idx = self.library.entries.iter().position(|entry| match entry {
            LibraryEntry::Archive(entry) => {
                paths_equivalent_for_selection(entry.path.as_ref(), path.as_path())
            }
            LibraryEntry::Folder(entry) | LibraryEntry::FolderBook(entry) => {
                paths_equivalent_for_selection(entry.path.as_ref(), path.as_path())
            }
            LibraryEntry::ImageFile(entry) => {
                paths_equivalent_for_selection(entry.path.as_ref(), path.as_path())
            }
        });
        self.library.selected_set.clear();
        self.library.anchor_idx = self.library.selected_idx;
    }

    pub(super) fn push_back_history(&mut self, entry: HistoryEntry) {
        self.library.history_back.push(entry);
        if self.library.history_back.len() > 128 {
            let overflow = self.library.history_back.len() - 128;
            self.library.history_back.drain(0..overflow);
        }
    }

    pub(super) fn navigate_to_dir_with_history(&mut self, path: PathBuf) {
        if self
            .library
            .current_dir
            .as_ref()
            .is_some_and(|cur| *cur == path)
        {
            return;
        }
        if let Some(snapshot) = self.history_snapshot() {
            self.push_back_history(snapshot);
        }
        self.library.history_forward.clear();
        self.pending_after_load = Some(PendingAfterLoad {
            selected_path: None,
            scroll_y: Some(0.0),
        });
        self.load_library_dir(path);
    }

    pub(super) fn navigate_back(&mut self) {
        let Some(target) = self.library.history_back.pop() else {
            return;
        };
        if let Some(snapshot) = self.history_snapshot() {
            self.library.history_forward.push(snapshot);
        }
        self.pending_after_load = Some(PendingAfterLoad {
            selected_path: target.selected_path,
            scroll_y: Some(target.scroll_offset.max(0.0)),
        });
        self.load_library_dir(target.dir.clone());
    }

    pub(super) fn navigate_forward(&mut self) {
        let Some(target) = self.library.history_forward.pop() else {
            return;
        };
        if let Some(snapshot) = self.history_snapshot() {
            self.push_back_history(snapshot);
        }
        self.pending_after_load = Some(PendingAfterLoad {
            selected_path: target.selected_path,
            scroll_y: Some(target.scroll_offset.max(0.0)),
        });
        self.load_library_dir(target.dir.clone());
    }

    pub(super) fn navigate_parent(&mut self) {
        let Some(cur) = self.library.current_dir.clone() else {
            return;
        };
        let Some(parent) = cur.parent().map(PathBuf::from) else {
            return;
        };
        self.navigate_to_dir_with_history(parent);
    }

    pub(super) fn reload_current_dir(&mut self, ctx: &egui::Context) {
        self.library.reload_current_dir_diff(ctx);
    }

    pub(super) fn toggle_favorite(&mut self, path: &std::path::Path) -> Option<FavoriteState> {
        self.library.toggle_favorite(path)
    }

    pub(super) fn toggle_favorite_entry(&mut self, idx: usize) -> Option<FavoriteState> {
        let entry = self.library.entries.get(idx)?.clone();
        self.library.toggle_favorite_entry(&entry)
    }

    pub(super) fn load_library_dir(&mut self, path: PathBuf) {
        let normalized = normalize_dir_path(path);
        self.library.filter.scope = crate::ui::library::LibraryScope::Any;
        self.library.mark_filter_dirty();
        self.library.reset_context_menu_cache = true;
        log::debug!(
            "[library-load] request path={} source=app.load_library_dir",
            normalized.display()
        );
        tracing::debug!(
            requested = %normalized.display(),
            current_before = ?self.library.current_dir.as_ref().map(|p| p.display().to_string()),
            favorites = ?self.favorites.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "app: loading library dir"
        );
        self.library.start_load_dir_async(normalized.clone());
        tracing::debug!(
            current_after = ?self.library.current_dir.as_ref().map(|p| p.display().to_string()),
            requested = %normalized.display(),
            favorites = ?self.favorites.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "app: started async library dir load"
        );
    }

    pub(super) fn resolve_pending_select(&mut self) {
        if let Some(target) = self.pending_select.take() {
            if self.library.entries.is_empty() {
                return;
            }
            let idx = self
                .library
                .entries
                .iter()
                .position(|e| matches!(e, LibraryEntry::Archive(a) if *a.path == target))
                .unwrap_or(0);
            self.library.selected_idx = Some(idx);
        }
    }

    pub(super) fn resolve_pending_drop_select(&mut self) {
        let Some(target) = self.pending_drop_select.take() else {
            return;
        };
        let Some(idx) = self
            .library
            .entries
            .iter()
            .position(|entry| matches!(entry, LibraryEntry::Archive(a) if *a.path == target))
        else {
            log::debug!(
                "[drop] select_failed reason=not_found target={}",
                target.display()
            );
            return;
        };
        self.library.selected_idx = Some(idx);
        self.library.selected_set.clear();
        self.library.anchor_idx = Some(idx);
        self.library.scroll_selected_into_view_pending = true;
    }

    pub(super) fn apply_pending_after_load(&mut self) {
        let Some(pending) = self.pending_after_load.take() else {
            return;
        };
        if pending.selected_path.is_some() {
            self.restore_selection_by_path(pending.selected_path);
        } else {
            self.library.selected_idx = None;
            self.library.selected_set.clear();
            self.library.anchor_idx = None;
        }
        if let Some(scroll) = pending.scroll_y {
            self.library.scroll_to_pending = Some(scroll.max(0.0));
            self.library.scroll_y = scroll.max(0.0);
        }
    }

    pub(super) fn show_toast(&mut self, message: impl Into<String>) {
        self.pending_toast = Some((message.into(), std::time::Instant::now()));
    }

    pub(super) fn show_error_dialog(&mut self, message: impl Into<String>) {
        self.pending_error_dialog = Some(message.into());
    }

    pub(super) fn begin_path_edit(&mut self) {
        self.library.is_path_editing = true;
        self.library.path_input_focused = false;
        self.library.path_edit_select_all_pending = true;
        self.library.path_edit_buffer = self
            .library
            .current_dir
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
    }

    pub(super) fn commit_path_edit(&mut self, raw: String) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            self.show_toast("Path not found");
            return;
        }
        let input = PathBuf::from(trimmed);
        if input.is_dir() {
            self.navigate_to_dir_with_history(normalize_dir_path(input));
            self.library.is_path_editing = false;
            return;
        }
        if input.is_file() {
            if !is_supported_archive_path(&input) {
                self.show_toast("Unsupported file type");
                return;
            }
            let Some(parent) = input.parent().map(PathBuf::from) else {
                self.show_toast("Path not found");
                return;
            };
            self.navigate_to_dir_with_history(normalize_dir_path(parent));
            let target = normalize_dir_path(input);
            self.pending_after_load = Some(PendingAfterLoad {
                selected_path: Some(target),
                scroll_y: None,
            });
            self.library.is_path_editing = false;
            return;
        }
        self.show_toast("Path not found");
    }
}
