//! ライブラリ画面（サムネイルグリッド）。
//!
//! `LibraryState` がアプリの主要 UI 状態を保持し、
//! `show()` が egui パネル内での描画を担う。

use parking_lot::RwLock;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant, SystemTime},
};

// UI が idle 中でも poll_worker が動くようにする repaint 間隔
const POLL_INTERVAL_MS: u64 = 80;
// 1フレームで UI スレッドが反映するサムネイル結果の上限
const MAX_THUMB_RESULTS_PER_FRAME: usize = 48;
// ライブラリフォルダのリアルタイム追従用ポーリング間隔
const LIBRARY_DIR_POLL_INTERVAL: Duration = Duration::from_secs(3);

use eframe::egui;

use crate::{
    domain::{
        app_settings::{LibraryCardSelectionStyle, LibraryHudMode, LibraryHudStyle, UiLanguage},
        archive::{BookId, BookMeta, LibraryEntry},
        archive_settings::{book_settings_path, FileSettings, ReadingState, SettingsStore},
        kind_group::KindGroupConfig,
        sort::{SortKey, SortOrder},
    },
    infra::favorite_store::{FavoriteState, FavoriteStore},
    infra::worker::thumb_worker::{ThumbTask, ThumbWorker, WorkerMsg},
    util::{
        natural_sort,
        path_eq::{normalize_path_for_selection, paths_equivalent_for_selection},
    },
};

use super::{
    i18n::{tr, TextKey},
    theme,
    virtual_grid::{self, ContextAction, ExternalToolMenuItem, KeyboardSelection},
};

// ── LibraryAction ─────────────────────────────────────────────────────────────

/// ライブラリ画面から上位（app.rs）へ通知するアクション
#[derive(Debug)]
pub enum LibraryAction {
    None,
    /// Archive を Viewer で開く
    OpenArchive(usize),
    /// Folder を開く
    OpenFolder(usize),
    /// Explorer で開く
    OpenInExplorer(usize),
    /// 名前変更（対象 idx）
    Rename(usize),
    /// 削除確認（対象 idx のリスト）
    Delete(Vec<usize>),
    /// ファイルをクリップボードへコピー（対象 idx のリスト）
    Copy(Vec<usize>),
    /// グループ設定（対象 idx のリスト）
    SetGroup(Vec<usize>),
    /// 本固有設定を初期化（対象 idx のリスト）
    ClearBookSettings(Vec<usize>),
    /// お気に入り切り替え（対象 idx）
    ToggleFavorite(usize),
    /// 選択中ファイルをウィンドウ外へドラッグコピー
    ExternalDrag(Vec<usize>),
    /// 外部ツール実行（対象 idx リスト）
    RunExternalTool {
        tool_index: usize,
        targets: Vec<usize>,
    },
}

pub fn poll_shortcuts(
    ctx: &egui::Context,
    state: &mut LibraryState,
    interaction_blocked: bool,
) -> Option<LibraryAction> {
    let has_text_focus = state.has_text_input_focus();
    if interaction_blocked || has_text_focus {
        state.ctrl_c_was_held = false;
        state.ctrl_a_was_held = false;
        return None;
    }

    let (f2, del, ctrl_c_pressed, ctrl_a_pressed) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::F2),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
            i.consume_key(egui::Modifiers::CTRL, egui::Key::C),
            i.consume_key(egui::Modifiers::CTRL, egui::Key::A),
        )
    });
    let app_focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));

    let ctrl_c_win32 = app_focused && detect_ctrl_key_edge(0x43, &mut state.ctrl_c_was_held);
    let ctrl_a_win32 = app_focused && detect_ctrl_key_edge(0x41, &mut state.ctrl_a_was_held);

    let ctrl_c = ctrl_c_pressed || ctrl_c_win32;
    let ctrl_a = ctrl_a_pressed || ctrl_a_win32;

    if f2 {
        if let Some(idx) = state
            .selected_idx
            .filter(|idx| matches!(state.entries.get(*idx), Some(LibraryEntry::Archive(_))))
        {
            return Some(LibraryAction::Rename(idx));
        }
    }
    if del && state.selected_idx.is_some() {
        return Some(LibraryAction::Delete(state.effective_selection()));
    }
    if ctrl_c
        && state
            .selected_idx
            .is_some_and(|idx| matches!(state.entries.get(idx), Some(LibraryEntry::Archive(_))))
    {
        return Some(LibraryAction::Copy(state.effective_selection()));
    }
    if ctrl_a {
        state.select_all_visible();
    }

    None
}

// ── LibraryState ──────────────────────────────────────────────────────────────

pub struct LibraryState {
    /// raw スキャン結果（sort/filter のベース）
    raw_entries: Vec<LibraryEntry>,
    /// 表示用エントリ（sort + filter 適用済み）
    pub entries: Vec<LibraryEntry>,

    /// BookId ごとのサムネイル表示状態
    pub book_states: HashMap<BookId, BookViewState>,

    pub worker: ThumbWorker,

    pub current_dir: Option<PathBuf>,
    /// topbar のパス入力欄に表示する文字列（current_dir と独立して編集可能）
    pub path_input: String,
    pub is_path_editing: bool,
    pub path_edit_buffer: String,
    pub path_edit_select_all_pending: bool,
    pub history_back: Vec<HistoryEntry>,
    pub history_forward: Vec<HistoryEntry>,
    pub sort_key: SortKey,
    pub sort_order: SortOrder,
    /// フィルタ条件（topbar が書き込む）
    pub filter: LibraryFilter,
    filter_dirty: bool,

    /// グリッドで現在の主選択インデックス（シングルクリック・矢印キーで更新）
    pub selected_idx: Option<usize>,
    /// Ctrl/Shift クリックによる複数選択セット
    pub selected_set: HashSet<usize>,
    /// Shift 選択のアンカー（Shift+クリック起点）
    pub anchor_idx: Option<usize>,
    /// Ctrl+A による全選択状態フラグ
    select_all_active: bool,
    /// トップバーのパス入力がフォーカス中
    pub path_input_focused: bool,
    /// トップバーのフィルタ入力がフォーカス中
    pub filter_input_focused: bool,
    /// トップバーのフィルタ入力へフォーカスを移す要求
    pub filter_focus_request: bool,
    /// Ctrl+C 押下のエッジ検出用
    ctrl_c_was_held: bool,
    /// Ctrl+A 押下のエッジ検出用
    ctrl_a_was_held: bool,

    /// グリッドの垂直スクロール量（セッション保存用）
    pub scroll_y: f32,
    /// 起動時に復元するスクロール量
    pub initial_scroll_y: f32,
    /// true の間だけ initial_scroll_y を ScrollArea に適用する
    pub scroll_restore_pending: bool,
    /// キーナビ後のスクロール追従要求（次フレームに適用して消費）
    pub scroll_to_pending: Option<f32>,
    /// 選択済み要素を次フレームで可視範囲へ寄せる 1 回限りの要求
    pub scroll_selected_into_view_pending: bool,
    /// サイドバー操作後にグリッドのコンテキストメニューキャッシュをリセットする
    pub reset_context_menu_cache: bool,
    reading_hud_states: HashMap<PathBuf, ReadingHudState>,

    // ── サムネイルサイズ（AppSettings から更新） ─────────────────────────────
    /// サムネイル幅（px）
    pub thumb_w: f32,
    /// サムネイル高さ（px）
    pub thumb_h: f32,
    /// ライブラリ画面のホイールスクロール倍率
    pub wheel_scroll_multiplier: f32,
    /// ライブラリグリッドの HUD 表示モード
    pub hud_mode: LibraryHudMode,
    /// ライブラリカード HUD の配色プリセット
    pub hud_style: LibraryHudStyle,
    /// ライブラリカード選択状態の配色プリセット
    pub selection_style: LibraryCardSelectionStyle,
    /// ライブラリ HUD のフォントサイズ
    pub hud_font_size: f32,
    favorite_store: Arc<RwLock<FavoriteStore>>,
    pub(crate) artifact_gate: Arc<RwLock<()>>,
    /// current_dir の差分ポーリングを最後に実行した時刻
    last_dir_poll_at: Instant,
    /// 起動時 initial_dir スキャンの世代管理（古い結果破棄用）
    async_load_generation: u64,
    /// 起動時 initial_dir 非同期スキャン結果の受信口
    async_load_rx: Option<mpsc::Receiver<AsyncLoadResult>>,
    /// 起動時 initial_dir の非同期スキャン中フラグ
    async_loading: bool,
    /// 定期差分スキャンの世代管理（古い結果破棄用）
    diff_scan_generation: u64,
    /// 定期差分スキャン結果の受信口
    diff_scan_rx: Option<mpsc::Receiver<AsyncDiffScanResult>>,
    /// 定期差分スキャンの実行中フラグ
    diff_scan_running: bool,
    /// 手動リロード結果反映時に復元する選択・スクロール情報
    manual_reload_restore: Option<ManualReloadRestore>,
    // グループ全体管理
    kind_config: KindGroupConfig,
    kind_config_last_poll_at: Instant,
    kind_config_poll_generation: u64,
    kind_config_last_modified: Option<SystemTime>,
    kind_config_error: Option<String>,
    group_counts: GroupCountSnapshot,
}

pub(crate) struct BookViewState {
    pub texture: Option<egui::TextureHandle>,
    pub thumb_requested: bool,
    pub thumb_failed: bool,
    pub force_reload: bool,
    pub kind_group: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReadingHudState {
    #[default]
    Unread,
    Reading,
    ReadingPercent(u32),
    Read,
}

impl ReadingHudState {
    fn from_file_settings(settings: &FileSettings) -> Self {
        match settings.reading_state {
            ReadingState::Unread => Self::Unread,
            ReadingState::Reading => {
                let Some(reading_page_count) =
                    settings.reading_page_count.filter(|count| *count > 0)
                else {
                    return Self::Reading;
                };
                let Some(resume_page) = settings
                    .resume_page
                    .filter(|page| *page < reading_page_count)
                else {
                    return Self::Reading;
                };
                let percent = ((resume_page + 1).saturating_mul(100) / reading_page_count) as u32;
                Self::ReadingPercent(percent)
            }
            ReadingState::Read => Self::Read,
        }
    }
}

#[derive(Default, Clone, PartialEq, Eq)]
pub enum LibraryScope {
    #[default]
    Any,
    Favorites,
    Uncategorized,
    Unread,
    Reading,
    Read,
    NamedGroup(String),
}

#[derive(Default, Clone)]
pub struct LibraryFilter {
    pub keyword: String,
    pub scope: LibraryScope,
}

impl LibraryFilter {
    pub fn clear_keyword(&mut self) {
        self.keyword.clear();
    }

    pub fn matches(
        &self,
        entry: &LibraryEntry,
        book_states: &HashMap<BookId, BookViewState>,
        reading_hud_states: &HashMap<PathBuf, ReadingHudState>,
        kind_config: &KindGroupConfig,
        favorite_store: &FavoriteStore,
    ) -> bool {
        self.keyword_matches(entry)
            && self.scope_matches(
                entry,
                book_states,
                reading_hud_states,
                kind_config,
                favorite_store,
            )
    }

    fn keyword_matches(&self, entry: &LibraryEntry) -> bool {
        if self.keyword.is_empty() {
            return true;
        }
        let lower = self.keyword.to_ascii_lowercase();
        entry.title().to_ascii_lowercase().contains(&lower)
    }

    fn scope_matches(
        &self,
        entry: &LibraryEntry,
        book_states: &HashMap<BookId, BookViewState>,
        reading_hud_states: &HashMap<PathBuf, ReadingHudState>,
        kind_config: &KindGroupConfig,
        favorite_store: &FavoriteStore,
    ) -> bool {
        match &self.scope {
            LibraryScope::Any => true,
            LibraryScope::Favorites => Self::is_favorite_match(entry, favorite_store),
            LibraryScope::Uncategorized => Self::is_uncategorized_match(entry, book_states),
            LibraryScope::Unread => {
                Self::is_reading_state_match(entry, reading_hud_states, ReadingHudState::Unread)
            }
            LibraryScope::Reading => {
                Self::is_reading_state_match(entry, reading_hud_states, ReadingHudState::Reading)
            }
            LibraryScope::Read => {
                Self::is_reading_state_match(entry, reading_hud_states, ReadingHudState::Read)
            }
            LibraryScope::NamedGroup(name) => {
                Self::is_named_group_match(entry, name, book_states, kind_config)
            }
        }
    }

    fn is_favorite_match(entry: &LibraryEntry, favorite_store: &FavoriteStore) -> bool {
        if !entry.is_favorite_target() {
            return false;
        }
        let normalized = normalize_path_for_selection(Self::entry_path_ref(entry));
        favorite_store.contains(&normalized)
    }

    fn entry_path_ref(entry: &LibraryEntry) -> &Path {
        entry.path()
    }

    fn is_uncategorized_match(
        entry: &LibraryEntry,
        book_states: &HashMap<BookId, BookViewState>,
    ) -> bool {
        // 未分類 / グループ集計は book_states を正とするので、ここで扱うのは
        // Archive だけに限定する。FolderBook/ImageFile/Folder は対象外。
        let LibraryEntry::Archive(meta) = entry else {
            return false;
        };
        book_states
            .get(&meta.id)
            .map(|s| s.kind_group.is_none())
            .unwrap_or(false)
    }

    fn is_reading_state_match(
        entry: &LibraryEntry,
        reading_hud_states: &HashMap<PathBuf, ReadingHudState>,
        expected: ReadingHudState,
    ) -> bool {
        let LibraryEntry::Folder(_) = entry else {
            let key = book_settings_path(entry.path());
            return match (
                reading_hud_states
                    .get(&key)
                    .copied()
                    .unwrap_or(ReadingHudState::Unread),
                expected,
            ) {
                (ReadingHudState::ReadingPercent(_), ReadingHudState::Reading) => true,
                (actual, expected) => actual == expected,
            };
        };
        false
    }

    fn is_named_group_match(
        entry: &LibraryEntry,
        name: &str,
        book_states: &HashMap<BookId, BookViewState>,
        kind_config: &KindGroupConfig,
    ) -> bool {
        // Named group も Archive の kind_group を起点に判定する。
        // FolderBook は本移動対象だが、グループ状態は持たせない。
        let LibraryEntry::Archive(meta) = entry else {
            return false;
        };
        let Some(kind_group) = book_states
            .get(&meta.id)
            .and_then(|s| s.kind_group.as_deref())
        else {
            return false;
        };
        if kind_group == name {
            return true;
        }
        kind_config
            .groups
            .get(name)
            .map(|def| def.children.iter().any(|c| c == kind_group))
            .unwrap_or(false)
    }
}

struct AsyncLoadResult {
    generation: u64,
    path: PathBuf,
    result: Result<Vec<LibraryEntry>, anyhow::Error>,
}

struct AsyncDiffScanResult {
    generation: u64,
    path: PathBuf,
    reason: DiffScanReason,
    result: Result<Vec<LibraryEntry>, anyhow::Error>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffScanReason {
    Periodic,
    ManualReload,
}

struct ManualReloadRestore {
    generation: u64,
    selected_path_before: Option<PathBuf>,
    scroll_before: f32,
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub dir: PathBuf,
    pub selected_path: Option<PathBuf>,
    pub scroll_offset: f32,
}

#[derive(Default)]
struct GroupCountSnapshot {
    leaf_counts: HashMap<String, usize>,
    parent_counts: HashMap<String, usize>,
    uncategorized_count: usize,
    favorite_count: usize,
    reading_unread_count: usize,
    reading_reading_count: usize,
    reading_read_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeletedEntryKind {
    Archive,
    FolderBook,
    ImageFile,
    Folder,
}

#[derive(Clone, Debug)]
pub(crate) struct DeletedEntryCleanup {
    pub kind: DeletedEntryKind,
    pub book_meta: Option<BookMeta>,
    pub thumb_id: Option<BookId>,
}

impl LibraryState {
    fn show_empty_library_message(&self, ui: &mut egui::Ui, language: UiLanguage) -> bool {
        if !self.entries.is_empty() {
            return false;
        }
        ui.centered_and_justified(|ui| {
            let empty_label = if self.is_async_loading() {
                tr(language, TextKey::Loading)
            } else if self.current_dir.is_none() {
                tr(language, TextKey::LibraryEmpty)
            } else {
                tr(language, TextKey::NoMatchingBooks)
            };
            ui.label(
                egui::RichText::new(empty_label)
                    .size(theme::FONT_SIZE_EMPTY)
                    .color(theme::TEXT_SUBTLE),
            );
        });
        true
    }

    fn resolve_open_action(&self, idx: usize) -> LibraryAction {
        if matches!(self.entries.get(idx), Some(LibraryEntry::Folder(_))) {
            return LibraryAction::OpenFolder(idx);
        }
        if matches!(
            self.entries.get(idx),
            Some(LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_))
        ) {
            return LibraryAction::OpenArchive(idx);
        }
        if let Some(LibraryEntry::Archive(entry)) = self.entries.get(idx) {
            if self.has_ready_texture(&entry.id) {
                return LibraryAction::OpenArchive(idx);
            }
        }
        LibraryAction::None
    }

    fn context_target_indices(&self, idx: usize) -> Vec<usize> {
        if self.selected_set.contains(&idx) || self.selected_idx == Some(idx) {
            self.effective_selection()
        } else {
            vec![idx]
        }
    }

    fn resolve_context_action(
        &mut self,
        idx: usize,
        action: ContextAction,
    ) -> Option<LibraryAction> {
        match action {
            ContextAction::Open => {
                let action = self.resolve_open_action(idx);
                if matches!(action, LibraryAction::None) {
                    None
                } else {
                    Some(action)
                }
            }
            ContextAction::Rename => Some(LibraryAction::Rename(idx)),
            ContextAction::Delete => {
                let targets = self.context_target_indices(idx);
                Some(LibraryAction::Delete(targets))
            }
            ContextAction::Copy => {
                let targets = self.context_target_indices(idx);
                Some(LibraryAction::Copy(targets))
            }
            ContextAction::OpenInExplorer => Some(LibraryAction::OpenInExplorer(idx)),
            ContextAction::MoveToFolder => match self.entries.get(idx) {
                Some(LibraryEntry::FolderBook(_)) => Some(LibraryAction::OpenFolder(idx)),
                _ => None,
            },
            ContextAction::SetGroup => {
                // UI のグループ設定は Archive の個別 override だけを書き換える。
                // FolderBook / ImageFile / Folder を混ぜると保存先と整合しない。
                let targets: Vec<usize> = self
                    .effective_selection()
                    .into_iter()
                    .filter(|&i| matches!(self.entries.get(i), Some(LibraryEntry::Archive(_))))
                    .collect();
                if targets.is_empty() {
                    None
                } else {
                    Some(LibraryAction::SetGroup(targets))
                }
            }
            ContextAction::ClearBookSettings => {
                let targets: Vec<usize> = self
                    .context_target_indices(idx)
                    .into_iter()
                    .filter(|&i| {
                        matches!(
                            self.entries.get(i),
                            Some(LibraryEntry::Archive(_) | LibraryEntry::FolderBook(_))
                        )
                    })
                    .collect();
                if targets.is_empty() {
                    None
                } else {
                    Some(LibraryAction::ClearBookSettings(targets))
                }
            }
            ContextAction::ToggleFavorite => self
                .entries
                .get(idx)
                .filter(|entry| entry.is_favorite_target())
                .map(|_| LibraryAction::ToggleFavorite(idx)),
            ContextAction::ApplyFilterToken(token) => {
                self.filter.keyword = token;
                self.mark_filter_dirty();
                None
            }
            ContextAction::RunExternalTool(tool_index) => {
                let targets = self.context_target_indices(idx);
                Some(LibraryAction::RunExternalTool {
                    tool_index,
                    targets,
                })
            }
        }
    }

    pub fn kind_config_error(&self) -> Option<&str> {
        self.kind_config_error.as_deref()
    }

    pub fn kind_groups(&self) -> &HashMap<String, crate::domain::kind_group::GroupDef> {
        &self.kind_config.groups
    }

    pub fn leaf_group_counts(&self) -> &HashMap<String, usize> {
        &self.group_counts.leaf_counts
    }

    pub fn parent_group_counts(&self) -> &HashMap<String, usize> {
        &self.group_counts.parent_counts
    }

    pub fn uncategorized_count(&self) -> usize {
        self.group_counts.uncategorized_count
    }

    pub fn favorite_count(&self) -> usize {
        self.group_counts.favorite_count
    }

    pub fn reading_unread_count(&self) -> usize {
        self.group_counts.reading_unread_count
    }

    pub fn reading_reading_count(&self) -> usize {
        self.group_counts.reading_reading_count
    }

    pub fn reading_read_count(&self) -> usize {
        self.group_counts.reading_read_count
    }

    pub fn is_favorite_entry(&self, entry: &LibraryEntry) -> bool {
        if !entry.is_favorite_target() {
            return false;
        }
        let normalized = normalize_path_for_selection(Self::entry_path_ref(entry));
        self.favorite_store.read().contains(&normalized)
    }

    pub fn toggle_favorite(&mut self, path: &Path) -> Option<FavoriteState> {
        if !path.exists() {
            log::warn!(
                "[favorite] toggle skipped because path does not exist: {}",
                path.display()
            );
            return None;
        }

        let state = {
            let mut favorite_store = self.favorite_store.write();
            let state = favorite_store.toggle(path);
            if !favorite_store.save() {
                log::warn!(
                    "[favorite] save failed after toggle; reloading favorites store path={}",
                    path.display()
                );
                *favorite_store = FavoriteStore::load();
                None
            } else {
                Some(state)
            }
        };

        self.recompute_group_counts();
        self.rebuild_entries();
        state
    }

    pub fn toggle_favorite_entry(&mut self, entry: &LibraryEntry) -> Option<FavoriteState> {
        if !entry.is_favorite_target() {
            return None;
        }
        if !entry.path().exists() {
            log::warn!(
                "[favorite] toggle skipped because path does not exist: {}",
                entry.path().display()
            );
            return None;
        }

        let state = {
            let mut favorite_store = self.favorite_store.write();
            let state = match entry {
                LibraryEntry::Archive(meta) => favorite_store.toggle(meta.path.as_ref()),
                LibraryEntry::FolderBook(meta) => favorite_store.toggle_with_metadata(
                    meta.path.as_ref(),
                    0,
                    system_time_to_unix_secs(meta.modified),
                ),
                LibraryEntry::Folder(_) | LibraryEntry::ImageFile(_) => return None,
            };
            if !favorite_store.save() {
                log::warn!(
                    "[favorite] save failed after toggle; reloading favorites store path={}",
                    entry.path().display()
                );
                *favorite_store = FavoriteStore::load();
                None
            } else {
                Some(state)
            }
        };

        self.recompute_group_counts();
        self.rebuild_entries();
        state
    }

    pub(crate) fn favorite_store_handle(&self) -> Arc<RwLock<FavoriteStore>> {
        Arc::clone(&self.favorite_store)
    }

    pub(crate) fn reading_hud_state_for_entry(&self, entry: &LibraryEntry) -> ReadingHudState {
        let LibraryEntry::Folder(_) = entry else {
            let key = book_settings_path(entry.path());
            return self
                .reading_hud_states
                .get(&key)
                .copied()
                .unwrap_or_default();
        };
        ReadingHudState::Unread
    }

    pub(crate) fn refresh_reading_hud_state_for_path(&mut self, path: &Path) {
        let key = book_settings_path(path);
        let settings = SettingsStore::load().get(key.as_path());
        self.reading_hud_states
            .insert(key, ReadingHudState::from_file_settings(&settings));
        self.recompute_group_counts();
    }

    pub(crate) fn remove_reading_hud_state_for_path(&mut self, path: &Path) {
        let key = book_settings_path(path);
        self.reading_hud_states.remove(&key);
        self.recompute_group_counts();
    }

    pub(crate) fn rename_reading_hud_state_for_path(&mut self, old_path: &Path, new_path: &Path) {
        let old_key = book_settings_path(old_path);
        let new_key = book_settings_path(new_path);
        if old_key == new_key {
            self.refresh_reading_hud_state_for_path(new_path);
            return;
        }
        self.reading_hud_states.remove(&old_key);
        self.refresh_reading_hud_state_for_path(new_path);
        self.recompute_group_counts();
    }

    pub(crate) fn remove_deleted_path(
        &mut self,
        deleted_path: &Path,
    ) -> Option<DeletedEntryCleanup> {
        let cleanup = self.deleted_path_cleanup(deleted_path)?;
        {
            let mut favorite_store = self.favorite_store.write();
            let removed = favorite_store.remove(deleted_path);
            if removed && !favorite_store.save() {
                log::warn!(
                    "[favorite] save failed after delete; reloading favorites store path={}",
                    deleted_path.display()
                );
                *favorite_store = FavoriteStore::load();
            }
        }

        let deleted_raw_idx = self.raw_entries.iter().position(|entry| {
            paths_equivalent_for_selection(Self::entry_path_ref(entry), deleted_path)
        });
        if let Some(idx) = deleted_raw_idx {
            self.raw_entries.remove(idx);
        }

        let deleted_idx = self.entries.iter().position(|entry| {
            paths_equivalent_for_selection(Self::entry_path_ref(entry), deleted_path)
        });
        if let Some(idx) = deleted_idx {
            self.entries.remove(idx);
            self.selected_set.retain(|&i| i != idx);
            self.selected_set = self
                .selected_set
                .iter()
                .map(|&i| if i > idx { i - 1 } else { i })
                .collect();
            self.anchor_idx = self.anchor_idx.map(|i| if i > idx { i - 1 } else { i });
            self.selected_idx = self.selected_idx.map(|i| {
                if i > idx {
                    i - 1
                } else if i == idx {
                    i.min(self.entries.len().saturating_sub(1))
                } else {
                    i
                }
            });
        }

        if let Some(id) = cleanup.thumb_id.as_ref() {
            self.book_states.remove(id);
        }
        if !matches!(cleanup.kind, DeletedEntryKind::ImageFile) {
            self.remove_reading_hud_state_for_path(deleted_path);
        }
        self.recompute_group_counts();
        Some(cleanup)
    }

    pub(crate) fn deleted_path_cleanup(&self, deleted_path: &Path) -> Option<DeletedEntryCleanup> {
        let deleted_entry = self
            .raw_entries
            .iter()
            .find(|entry| paths_equivalent_for_selection(Self::entry_path_ref(entry), deleted_path))
            .cloned()?;
        let kind = match &deleted_entry {
            LibraryEntry::Archive(_) => DeletedEntryKind::Archive,
            LibraryEntry::FolderBook(_) => DeletedEntryKind::FolderBook,
            LibraryEntry::ImageFile(_) => DeletedEntryKind::ImageFile,
            LibraryEntry::Folder(_) => DeletedEntryKind::Folder,
        };
        let thumb_id = deleted_entry.thumb_id();
        let book_meta = Self::book_entry_ref(&deleted_entry).cloned();
        Some(DeletedEntryCleanup {
            kind,
            book_meta,
            thumb_id,
        })
    }

    fn book_state_mut(&mut self, id: &BookId) -> &mut BookViewState {
        self.book_states
            .entry(id.clone())
            .or_insert_with(|| BookViewState {
                texture: None,
                thumb_requested: false,
                thumb_failed: false,
                force_reload: false,
                kind_group: None,
            })
    }

    fn book_state(&self, id: &BookId) -> Option<&BookViewState> {
        self.book_states.get(id)
    }

    fn has_ready_texture(&self, id: &BookId) -> bool {
        self.book_state(id)
            .is_some_and(|s| s.texture.is_some() && !s.thumb_failed)
    }

    fn ready_texture_count(&self) -> usize {
        self.book_states
            .values()
            .filter(|state| state.texture.is_some())
            .count()
    }

    fn requested_count(&self) -> usize {
        self.book_states
            .values()
            .filter(|state| state.thumb_requested)
            .count()
    }

    fn failed_count(&self) -> usize {
        self.book_states
            .values()
            .filter(|state| state.thumb_failed)
            .count()
    }

    pub fn remove_book(&mut self, id: &BookId) {
        self.book_states.remove(id);
        self.recompute_group_counts();
    }

    pub fn clear_books(&mut self) {
        self.book_states.clear();
        self.recompute_group_counts();
    }

    fn book_entry_ref(entry: &LibraryEntry) -> Option<&BookMeta> {
        match entry {
            LibraryEntry::Archive(entry) => Some(entry),
            LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => {
                None
            }
        }
    }

    fn entry_path_ref(entry: &LibraryEntry) -> &Path {
        entry.path()
    }

    fn entry_title_ref(entry: &LibraryEntry) -> &str {
        entry.title()
    }

    fn entry_modified(entry: &LibraryEntry) -> SystemTime {
        entry.modified()
    }

    pub fn new(ctx: eframe::egui::Context) -> Self {
        let kind_config = crate::infra::kind_group_store::load().unwrap_or_else(|e| {
            log::warn!("[kind-group] parse error: {e}");
            KindGroupConfig::default()
        });
        let artifact_gate = Arc::new(RwLock::new(()));
        Self {
            raw_entries: Vec::new(),
            entries: Vec::new(),
            book_states: HashMap::new(),
            artifact_gate: Arc::clone(&artifact_gate),
            worker: ThumbWorker::spawn(ctx, artifact_gate),
            current_dir: None,
            path_input: String::new(),
            is_path_editing: false,
            path_edit_buffer: String::new(),
            path_edit_select_all_pending: false,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            sort_key: SortKey::default(),
            sort_order: SortOrder::default(),
            filter: LibraryFilter::default(),
            filter_dirty: false,
            selected_idx: None,
            selected_set: HashSet::new(),
            anchor_idx: None,
            select_all_active: false,
            path_input_focused: false,
            filter_input_focused: false,
            filter_focus_request: false,
            ctrl_c_was_held: false,
            ctrl_a_was_held: false,
            scroll_y: 0.0,
            initial_scroll_y: 0.0,
            scroll_restore_pending: false,
            scroll_to_pending: None,
            scroll_selected_into_view_pending: false,
            reset_context_menu_cache: false,
            reading_hud_states: HashMap::new(),
            thumb_w: theme::THUMB_W,
            thumb_h: theme::THUMB_H,
            wheel_scroll_multiplier: 2.0,
            hud_mode: LibraryHudMode::On,
            hud_style: LibraryHudStyle::Default,
            selection_style: LibraryCardSelectionStyle::Default,
            hud_font_size: theme::FONT_SIZE_BODY,
            favorite_store: Arc::new(RwLock::new(FavoriteStore::load())),
            last_dir_poll_at: Instant::now(),
            async_load_generation: 0,
            async_load_rx: None,
            async_loading: false,
            diff_scan_generation: 0,
            diff_scan_rx: None,
            diff_scan_running: false,
            manual_reload_restore: None,
            kind_config,
            kind_config_last_poll_at: Instant::now(),
            kind_config_poll_generation: 0,
            kind_config_last_modified: crate::infra::kind_group_store::last_modified(),
            kind_config_error: None,
            group_counts: GroupCountSnapshot::default(),
        }
    }

    // ── フォルダスキャン ──────────────────────────────────────────────────────

    pub fn start_load_dir_async(&mut self, path: PathBuf) {
        use crate::infra::fs::scanner;
        let generation = self.invalidate_async_load();
        self.invalidate_diff_scan();
        self.filter.scope = LibraryScope::Any;
        log::debug!(
            "[async-load] start generation={} path={}",
            generation,
            path.display()
        );
        let (tx, rx) = mpsc::channel();
        let path_for_worker = path.clone();
        self.async_load_rx = Some(rx);
        self.async_loading = true;
        self.worker.clear_pending_tasks();
        self.path_input = path.to_string_lossy().into_owned();
        self.current_dir = Some(path);
        self.raw_entries.clear();
        self.entries.clear();
        self.clear_books();
        self.selected_idx = None;
        self.selected_set.clear();
        self.anchor_idx = None;
        self.select_all_active = false;

        thread::spawn(move || {
            log::debug!(
                "[async-load] worker begin generation={} path={}",
                generation,
                path_for_worker.display()
            );
            let result = scanner::scan_dir(&path_for_worker);
            log::debug!(
                "[async-load] worker finished generation={} ok={}",
                generation,
                result.is_ok()
            );
            let _ = tx.send(AsyncLoadResult {
                generation,
                path: path_for_worker,
                result,
            });
        });
    }

    pub fn poll_async_load(&mut self, ctx: &egui::Context) -> bool {
        let Some(rx) = self.async_load_rx.as_ref() else {
            return false;
        };
        let Ok(done) = rx.try_recv() else {
            if self.async_loading {
                ctx.request_repaint_after(Duration::from_millis(POLL_INTERVAL_MS));
            }
            return false;
        };

        self.async_load_rx = None;
        self.async_loading = false;
        log::debug!(
            "[async-load] received generation={} current={}",
            done.generation,
            self.async_load_generation
        );
        if done.generation != self.async_load_generation {
            log::debug!(
                "[async-load] stale result dropped generation={} current={}",
                done.generation,
                self.async_load_generation
            );
            return false;
        }

        match done.result {
            Ok(entries) => {
                log::debug!(
                    "[async-load] apply generation={} entries={}",
                    done.generation,
                    entries.len()
                );
                self.apply_loaded_dir(done.path, entries);
                ctx.request_repaint();
                true
            }
            Err(e) => {
                tracing::error!("scan_dir(async): {e}");
                false
            }
        }
    }

    pub fn is_async_loading(&self) -> bool {
        self.async_loading
    }

    /// 現在開いているフォルダを差分スキャンし、追加/削除/置き換えだけを反映する。
    ///
    /// 非同期フルロードとは違い、既存サムネイル状態は全クリアしない。
    /// - 追加: Loading 状態で一覧に追加し、サムネイル要求へ進める
    /// - 削除: raw/textures/requested/failed から掃除する
    /// - 同一 path/id の置き換え: size/modified 変化を検出し、failed/retry 復旧用に再要求する
    pub fn apply_pending_updates(&mut self, ctx: &egui::Context) {
        self.apply_dir_scan_result(ctx);
        self.apply_kind_config_result();
    }

    pub fn poll_current_dir_changes(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if now.duration_since(self.last_dir_poll_at) < LIBRARY_DIR_POLL_INTERVAL {
            return;
        }
        self.last_dir_poll_at = now;

        let Some(dir) = self.current_dir.clone() else {
            return;
        };
        if self.diff_scan_running {
            log::debug!("[diff-scan] skip already running path={}", dir.display());
            return;
        }
        self.start_diff_scan_async(dir, DiffScanReason::Periodic);
        ctx.request_repaint_after(Duration::from_millis(POLL_INTERVAL_MS));
    }

    fn apply_kind_config_result(&mut self) {
        if self.kind_config_last_poll_at.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.kind_config_last_poll_at = Instant::now();
        self.kind_config_poll_generation = self.kind_config_poll_generation.saturating_add(1);

        let current_modified = crate::infra::kind_group_store::last_modified();
        if current_modified != self.kind_config_last_modified {
            self.kind_config_last_modified = current_modified;
            log::debug!(
                "[kind-group] reload detected last_modified={:?}",
                current_modified
            );
            self.reload_kind_config();
        }
    }

    /// current_dir を再スキャンし、差分だけを反映する明示的な reload。
    /// 非同期フルロードと違って thumbnail state を全消去しない。
    pub fn reload_current_dir_diff(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.current_dir.clone() else {
            return;
        };
        log::debug!("[diff-scan] manual reload requested path={}", dir.display());
        self.invalidate_diff_scan();
        let selected_path_before = self
            .selected_idx
            .and_then(|idx| self.entries.get(idx))
            .map(|entry| Self::entry_path_ref(entry).to_path_buf());
        let scroll_before = self.scroll_y.max(0.0);
        let generation = self.start_diff_scan_async(dir, DiffScanReason::ManualReload);
        self.manual_reload_restore = Some(ManualReloadRestore {
            generation,
            selected_path_before,
            scroll_before,
        });
        ctx.request_repaint_after(Duration::from_millis(POLL_INTERVAL_MS));
    }

    fn apply_scanned_entries_preserving_state(&mut self, scanned: Vec<LibraryEntry>) -> bool {
        let selected_paths = self.selected_paths_snapshot();
        let selected_path = self
            .selected_idx
            .and_then(|idx| self.entries.get(idx))
            .map(|entry| Self::entry_path_ref(entry).to_path_buf());
        let anchor_path = self
            .anchor_idx
            .and_then(|idx| self.entries.get(idx))
            .map(|entry| Self::entry_path_ref(entry).to_path_buf());

        let old_by_id = Self::book_meta_by_id(&self.raw_entries);
        let new_by_id = Self::book_meta_by_id(&scanned);
        let old_folderbook_by_id = Self::folderbook_modified_by_id(&self.raw_entries);
        let old_imagefile_by_id = Self::imagefile_snapshot_by_id(&self.raw_entries);
        let new_folderbook_by_id = Self::folderbook_modified_by_id(&scanned);
        let new_imagefile_by_id = Self::imagefile_snapshot_by_id(&scanned);

        let mut changed = false;
        let mut content_changed_ids = HashSet::new();
        let old_entry_keys: HashSet<(PathBuf, bool)> = self
            .raw_entries
            .iter()
            .map(|entry| {
                (
                    Self::entry_path_ref(entry).to_path_buf(),
                    matches!(entry, LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_)),
                )
            })
            .collect();
        let new_entry_keys: HashSet<(PathBuf, bool)> = scanned
            .iter()
            .map(|entry| {
                (
                    Self::entry_path_ref(entry).to_path_buf(),
                    matches!(entry, LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_)),
                )
            })
            .collect();
        if old_entry_keys != new_entry_keys {
            changed = true;
        }

        // 削除された本の状態を掃除する。
        for id in old_by_id.keys() {
            if !new_by_id.contains_key(id) {
                self.remove_book(id);
                changed = true;
            }
        }
        for id in old_folderbook_by_id.keys() {
            if !new_folderbook_by_id.contains_key(id) {
                self.remove_book(id);
                changed = true;
            }
        }
        for id in old_imagefile_by_id.keys() {
            if !new_imagefile_by_id.contains_key(id) {
                self.remove_book(id);
                changed = true;
            }
        }

        // 追加・同一パス入れ替えを検出する。
        for (id, new_entry) in &new_by_id {
            match old_by_id.get(id) {
                None => {
                    // 新規追加。Loading として扱うため、失敗状態だけ念のため解除する。
                    self.remove_book(id);
                    changed = true;
                }
                Some(old_entry) if entry_file_snapshot_changed(old_entry, new_entry) => {
                    // 同じ path/id でも内容が変わったケース。
                    // NG→OK だけでなく OK→NG もあるため、旧サムネイル/要求済み/失敗状態を解除し、
                    // worker 側にも古い cache を使わず再生成させる。
                    let state = self.book_state_mut(id);
                    state.texture = None;
                    state.thumb_requested = false;
                    state.thumb_failed = false;
                    content_changed_ids.insert(id.clone());
                    changed = true;
                }
                _ => {}
            }
        }
        for (id, new_modified) in &new_folderbook_by_id {
            match old_folderbook_by_id.get(id) {
                None => {
                    self.remove_book(id);
                    changed = true;
                }
                Some(old_modified) if old_modified != new_modified => {
                    self.remove_book(id);
                    changed = true;
                }
                _ => {}
            }
        }
        for (id, new_snapshot) in &new_imagefile_by_id {
            match old_imagefile_by_id.get(id) {
                None => {
                    self.remove_book(id);
                    changed = true;
                }
                Some(old_snapshot) if old_snapshot != new_snapshot => {
                    let state = self.book_state_mut(id);
                    state.texture = None;
                    state.thumb_requested = false;
                    state.thumb_failed = false;
                    changed = true;
                }
                _ => {}
            }
        }

        if !changed {
            return false;
        }

        for id in content_changed_ids {
            self.book_state_mut(&id).force_reload = true;
        }
        self.raw_entries = scanned;
        self.prefill_kind_groups();
        self.rebuild_entries();
        self.restore_selection_by_paths(
            &selected_paths,
            selected_path.as_deref(),
            anchor_path.as_deref(),
        );
        true
    }

    fn book_meta_by_id(entries: &[LibraryEntry]) -> HashMap<BookId, BookMeta> {
        entries
            .iter()
            .filter_map(Self::book_entry_ref)
            .cloned()
            .map(|entry| (entry.id.clone(), entry))
            .collect()
    }

    fn folderbook_modified_by_id(entries: &[LibraryEntry]) -> HashMap<BookId, SystemTime> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                LibraryEntry::FolderBook(folder) => {
                    Some((BookId::from_path(folder.path.as_ref()), folder.modified))
                }
                _ => None,
            })
            .collect()
    }

    fn imagefile_snapshot_by_id(entries: &[LibraryEntry]) -> HashMap<BookId, (u64, SystemTime)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                LibraryEntry::ImageFile(file) => Some((
                    BookId::from_path(file.path.as_ref()),
                    (file.size, file.modified),
                )),
                _ => None,
            })
            .collect()
    }

    fn apply_loaded_dir(&mut self, path: PathBuf, entries: Vec<LibraryEntry>) {
        self.path_input = path.to_string_lossy().into_owned();
        self.current_dir = Some(path);
        self.raw_entries = entries;
        self.clear_books();
        self.filter.scope = LibraryScope::Any;
        self.filter_dirty = true;
        self.selected_idx = None;
        self.selected_set.clear();
        self.anchor_idx = None;
        self.select_all_active = false;
        self.last_dir_poll_at = Instant::now();
        self.prefill_kind_groups();
        self.rebuild_entries();
    }

    fn invalidate_async_load(&mut self) -> u64 {
        self.async_load_generation = self.async_load_generation.saturating_add(1);
        self.async_load_rx = None;
        self.async_loading = false;
        self.async_load_generation
    }

    fn start_diff_scan_async(&mut self, path: PathBuf, reason: DiffScanReason) -> u64 {
        use crate::infra::fs::scanner;

        self.diff_scan_generation = self.diff_scan_generation.saturating_add(1);
        let generation = self.diff_scan_generation;
        let (tx, rx) = mpsc::channel();
        let path_for_worker = path.clone();
        self.diff_scan_rx = Some(rx);
        self.diff_scan_running = true;
        log::debug!(
            "[diff-scan] start reason={:?} path={} generation={}",
            reason,
            path.display(),
            generation
        );

        thread::spawn(move || {
            let result = scanner::scan_dir(&path_for_worker);
            log::debug!(
                "[diff-scan] finished reason={:?} path={} generation={} ok={}",
                reason,
                path_for_worker.display(),
                generation,
                result.is_ok()
            );
            let _ = tx.send(AsyncDiffScanResult {
                generation,
                path: path_for_worker,
                reason,
                result,
            });
        });
        generation
    }

    fn apply_dir_scan_result(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.diff_scan_rx.as_ref() else {
            return;
        };
        let Ok(done) = rx.try_recv() else {
            if self.diff_scan_running {
                ctx.request_repaint_after(Duration::from_millis(POLL_INTERVAL_MS));
            }
            return;
        };

        self.diff_scan_rx = None;
        self.diff_scan_running = false;
        if done.generation != self.diff_scan_generation {
            log::debug!(
                "[diff-scan] drop stale result reason={:?} path={} generation={} current={}",
                done.reason,
                done.path.display(),
                done.generation,
                self.diff_scan_generation
            );
            return;
        }
        let Some(current_dir) = self.current_dir.clone() else {
            log::debug!(
                "[diff-scan] drop stale result reason={:?} path={} generation={} current_dir=None",
                done.reason,
                done.path.display(),
                done.generation
            );
            return;
        };
        if done.path != current_dir {
            log::debug!(
                "[diff-scan] drop stale result reason={:?} path={} generation={} current_dir={}",
                done.reason,
                done.path.display(),
                done.generation,
                current_dir.display()
            );
            return;
        }

        match done.result {
            Ok(scanned) => {
                log::debug!(
                    "[diff-scan] apply reason={:?} path={} entries={}",
                    done.reason,
                    done.path.display(),
                    scanned.len()
                );
                if self.apply_scanned_entries_preserving_state(scanned) {
                    ctx.request_repaint();
                }
                if done.reason == DiffScanReason::ManualReload {
                    if let Some(restore) = self.manual_reload_restore.take() {
                        if restore.generation == done.generation {
                            if let Some(target) = restore.selected_path_before {
                                self.selected_idx = self.entries.iter().position(|entry| {
                                    paths_equivalent_for_selection(
                                        Self::entry_path_ref(entry),
                                        target.as_path(),
                                    )
                                });
                                self.selected_set.clear();
                                self.anchor_idx = self.selected_idx;
                            }
                            self.scroll_to_pending = Some(restore.scroll_before);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("scan_dir(diff async): {e}");
            }
        }
    }

    fn invalidate_diff_scan(&mut self) {
        self.diff_scan_generation = self.diff_scan_generation.saturating_add(1);
        self.diff_scan_rx = None;
        self.diff_scan_running = false;
        self.manual_reload_restore = None;
    }

    /// ライブラリに登録済みか（book_statesへの登録を確認）
    fn is_registered(&self, id: &BookId) -> bool {
        self.book_states.contains_key(id)
    }

    // ── sort / filter 適用 ────────────────────────────────────────────────────

    pub fn mark_filter_dirty(&mut self) {
        self.filter_dirty = true;
    }

    /// book_states を唯一の正として全件再集計
    fn recompute_group_counts(&mut self) {
        let mut leaf_counts: HashMap<String, usize> = HashMap::new();
        let mut uncategorized_count = 0usize;
        for state in self.book_states.values() {
            match &state.kind_group {
                Some(group) => *leaf_counts.entry(group.clone()).or_insert(0) += 1,
                None => uncategorized_count += 1,
            }
        }
        let parent_counts = compute_parent_counts(&leaf_counts, &self.kind_config.groups);
        let favorite_store = self.favorite_store.read();
        let favorite_count = self
            .raw_entries
            .iter()
            .filter(|entry| {
                if !entry.is_favorite_target() {
                    return false;
                }
                let normalized = normalize_path_for_selection(Self::entry_path_ref(entry));
                favorite_store.contains(&normalized)
            })
            .count();
        let mut reading_unread_count = 0usize;
        let mut reading_reading_count = 0usize;
        let mut reading_read_count = 0usize;
        for entry in &self.raw_entries {
            if matches!(entry, LibraryEntry::Folder(_)) {
                continue;
            }
            match self.reading_hud_state_for_entry(entry) {
                ReadingHudState::Unread => reading_unread_count += 1,
                ReadingHudState::Reading | ReadingHudState::ReadingPercent(_) => {
                    reading_reading_count += 1
                }
                ReadingHudState::Read => reading_read_count += 1,
            }
        }
        self.group_counts = GroupCountSnapshot {
            leaf_counts,
            parent_counts,
            uncategorized_count,
            favorite_count,
            reading_unread_count,
            reading_reading_count,
            reading_read_count,
        };
    }

    /// raw_entries 全件の kind_group を即時確定
    /// フォルダ読み込み時・TOMLリロード時に呼ぶ
    fn prefill_kind_groups(&mut self) {
        use crate::domain::filename_parser::{parse_filename, FilenamePartRole};
        use crate::util::path_eq::normalize_path_for_override;

        for entry in &self.raw_entries {
            let LibraryEntry::Archive(meta) = entry else {
                continue;
            };
            let normalized = normalize_path_for_override(&meta.path);
            let parsed = parse_filename(&meta.title);
            let kind = parsed
                .parts
                .iter()
                .find(|p| p.role == FilenamePartRole::Kind)
                .map(|p| p.text.as_str());
            let group = self.kind_config.resolve(&normalized, kind);
            let state = self
                .book_states
                .entry(meta.id.clone())
                .or_insert_with(|| BookViewState {
                    texture: None,
                    thumb_requested: false,
                    thumb_failed: false,
                    force_reload: false,
                    kind_group: None,
                });
            state.kind_group = group;
        }
        self.recompute_group_counts();
        self.filter_dirty = true;
    }

    /// TOMLリロードを試みる・成功時は再マッチング・再集計
    pub fn reload_kind_config(&mut self) {
        match crate::infra::kind_group_store::load() {
            Ok(config) => {
                self.kind_config = config;
                self.kind_config_error = None;
                self.prefill_kind_groups();
                self.filter_dirty = true;
                log::debug!("[kind-group] reloaded");
            }
            Err(e) => {
                log::warn!("[kind-group] parse error: {e}");
                self.kind_config_error = Some(e);
            }
        }
    }

    fn rebuild_entries(&mut self) {
        let selected_paths_before = self.selected_paths_snapshot();
        let selected_path_before = self
            .selected_idx
            .and_then(|idx| self.entries.get(idx))
            .map(|entry| Self::entry_path_ref(entry).to_path_buf());
        let anchor_path_before = self
            .anchor_idx
            .and_then(|idx| self.entries.get(idx))
            .map(|entry| Self::entry_path_ref(entry).to_path_buf());
        let was_all_selected = self.select_all_active;

        self.filter_dirty = false;
        self.prefill_reading_hud_states();

        let mut out = self.filtered_entries();
        self.sort_entries(&mut out);
        self.entries = out;
        if was_all_selected {
            self.selected_idx = None;
            self.selected_set.clear();
            self.anchor_idx = None;
            self.select_all_active = false;
        } else {
            self.restore_selection_by_paths(
                &selected_paths_before,
                selected_path_before.as_deref(),
                anchor_path_before.as_deref(),
            );
        }
        self.request_all_thumbs();
        self.recompute_group_counts();
    }

    fn prefill_reading_hud_states(&mut self) {
        let settings = SettingsStore::load();
        self.reading_hud_states.clear();
        for entry in &self.raw_entries {
            if matches!(entry, LibraryEntry::Folder(_)) {
                continue;
            }
            let key = book_settings_path(entry.path());
            let file_settings = settings.get(key.as_path());
            self.reading_hud_states
                .insert(key, ReadingHudState::from_file_settings(&file_settings));
        }
    }

    fn filtered_entries(&self) -> Vec<LibraryEntry> {
        let favorite_store = self.favorite_store.read();
        self.raw_entries
            .iter()
            .filter(|e| {
                self.filter.matches(
                    e,
                    &self.book_states,
                    &self.reading_hud_states,
                    &self.kind_config,
                    &favorite_store,
                )
            })
            .cloned()
            .collect()
    }

    fn sort_entries(&self, out: &mut [LibraryEntry]) {
        out.sort_by(|a, b| {
            let ord = match self.sort_key {
                SortKey::NameNatural => {
                    natural_sort::compare(Self::entry_title_ref(a), Self::entry_title_ref(b))
                }
                SortKey::Modified => Self::entry_modified(a).cmp(&Self::entry_modified(b)),
                SortKey::Size => {
                    let asize = a.size();
                    let bsize = b.size();
                    asize.cmp(&bsize)
                }
                SortKey::PageCount => {
                    let ap = a.page_count();
                    let bp = b.page_count();
                    ap.cmp(&bp)
                }
            };
            if self.sort_order == SortOrder::Desc {
                ord.reverse()
            } else {
                ord
            }
        });
    }

    // ── サムネイルサイズ変更 ──────────────────────────────────────────────────

    /// サムネイル表示サイズを変更する（テクスチャはそのまま流用）。
    /// ストレージは常に 320px 固定なので再生成ゼロ。
    pub fn apply_thumb_size(&mut self, w: f32, h: f32) {
        if (self.thumb_w - w).abs() < 0.5 && (self.thumb_h - h).abs() < 0.5 {
            return;
        }
        self.thumb_w = w;
        self.thumb_h = h;
    }

    /// サムネキャッシュクリア後の再生成（キャッシュ削除は呼び出し元で行う）
    pub fn reload_thumbs(&mut self) {
        self.clear_books();
        self.prefill_kind_groups(); // book_states を再構築・kind_group確定
        self.request_all_thumbs();
    }

    // ── Worker ポーリング ─────────────────────────────────────────────────────

    pub fn poll_worker(&mut self, ctx: &egui::Context) {
        let mut received = 0usize;
        let mut failed = 0usize;
        let mut stale = 0usize;
        let mut reached_limit = false;
        while let Some(msg) = self.worker.try_recv() {
            match msg {
                WorkerMsg::Ready(resp) => {
                    if !self.is_registered(&resp.book_id) {
                        continue;
                    }
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [resp.width as usize, resp.height as usize],
                        &resp.pixels,
                    );
                    let handle = ctx.load_texture(
                        &*resp.book_id.0.to_hex(),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    {
                        let state = self.book_state_mut(&resp.book_id);
                        state.thumb_failed = false;
                        state.force_reload = false;
                        state.thumb_requested = true;
                        state.texture = Some(handle);
                    }
                    ctx.request_repaint();
                    received += 1;
                }
                WorkerMsg::Failed(id) | WorkerMsg::FailedPermanent(id) => {
                    if let Some(state) = self.book_states.get_mut(&id) {
                        state.thumb_requested = false;
                    }
                    if !self.is_registered(&id) {
                        continue;
                    }
                    tracing::debug!(
                        id = &id.0.to_hex()[..8],
                        "poll_worker: thumbnail failed permanently"
                    );
                    // OK→NG の入れ替えでは古い成功サムネイルが残りうる。
                    // Failed 状態を必ず反映するため、ここで落とす。
                    {
                        let state = self.book_state_mut(&id);
                        state.texture = None;
                        state.force_reload = false;
                        state.thumb_failed = true;
                    }
                    ctx.request_repaint();
                    failed += 1;
                }
                WorkerMsg::Stale(id) => {
                    // 同じ path/id のファイル差し替え前に開始された古いタスク。
                    // 新しいタスク側で再生成されるため、Failed にはしない。
                    if let Some(state) = self.book_states.get_mut(&id) {
                        state.thumb_requested = false;
                    }
                    stale += 1;
                }
            }
            let processed_count = received + failed + stale;
            if processed_count >= MAX_THUMB_RESULTS_PER_FRAME {
                reached_limit = true;
                break;
            }
        }
        if received > 0 || failed > 0 {
            tracing::trace!(
                received,
                failed,
                textures = self.ready_texture_count(),
                requested = self.requested_count(),
                failed_total = self.failed_count(),
                "poll_worker: batch done"
            );
        }
        let processed_count = received + failed + stale;
        if processed_count > 0 {
            log::trace!(
                "[worker] thumb processed={} received={} failed={} stale={} limited={}",
                processed_count,
                received,
                failed,
                stale,
                reached_limit
            );
        }
        if reached_limit {
            // 取りこぼしを防ぐため、残りキュー処理を次フレームで継続する。
            ctx.request_repaint();
        }

        let done = self.ready_texture_count() + self.failed_count();
        let unreceived = self.requested_count().saturating_sub(done);
        if unreceived > 0 {
            ctx.request_repaint_after(Duration::from_millis(POLL_INTERVAL_MS));
        }
    }

    // ── サムネイル一括要求 ────────────────────────────────────────────────────

    fn request_all_thumbs(&mut self) {
        let target_width = crate::domain::app_settings::AppSettings::storage_width();
        let mut tasks = Vec::new();
        let mut request_specs = Vec::new();

        for entry in &self.entries {
            let Some(book_id) = entry.thumb_id() else {
                continue;
            };
            let (path, expected_size, expected_modified) = match entry {
                LibraryEntry::Archive(meta) => {
                    (Arc::clone(&meta.path), meta.size, Some(meta.modified))
                }
                LibraryEntry::FolderBook(meta) => {
                    let Ok(fs_meta) = std::fs::metadata(meta.path.as_ref()) else {
                        continue;
                    };
                    (
                        Arc::clone(&meta.path),
                        fs_meta.len(),
                        fs_meta.modified().ok(),
                    )
                }
                LibraryEntry::ImageFile(meta) => {
                    (Arc::clone(&meta.path), meta.size, Some(meta.modified))
                }
                LibraryEntry::Folder(_) => continue,
            };
            request_specs.push((book_id, path, expected_size, expected_modified));
        }

        for (book_id, path, expected_size, expected_modified) in request_specs {
            if self.book_state(&book_id).is_some_and(|s| s.thumb_failed) {
                continue;
            }
            if self.book_state(&book_id).is_some_and(|s| s.thumb_requested) {
                continue;
            }
            let state = self.book_state_mut(&book_id);
            state.thumb_requested = true;
            let bypass_cache = state.force_reload;
            state.force_reload = false;
            tasks.push(ThumbTask {
                book_id,
                path,
                target_width,
                expected_size,
                expected_modified,
                bypass_cache,
            });
        }

        for task in tasks {
            self.worker.request(task);
        }
    }

    // ── 選択ユーティリティ ────────────────────────────────────────────────────

    /// 現在の実効選択インデックス一覧（複数選択 > 主選択の順で決定）
    pub fn effective_selection(&self) -> Vec<usize> {
        if !self.selected_set.is_empty() {
            let mut v: Vec<usize> = self.selected_set.iter().copied().collect();
            v.sort_unstable();
            // 主選択も含める（selected_set に入っていない場合）
            if let Some(idx) = self.selected_idx {
                if !self.selected_set.contains(&idx) {
                    v.insert(0, idx);
                }
            }
            v
        } else {
            self.selected_idx.map(|i| vec![i]).unwrap_or_default()
        }
    }

    fn selected_paths_snapshot(&self) -> HashSet<PathBuf> {
        self.effective_selection()
            .iter()
            .filter_map(|&idx| self.entries.get(idx))
            .map(|entry| Self::entry_path_ref(entry).to_path_buf())
            .collect()
    }

    fn select_all_visible(&mut self) {
        if self.entries.is_empty() {
            self.selected_idx = None;
            self.selected_set.clear();
            self.anchor_idx = None;
            self.select_all_active = false;
            return;
        }

        let primary = self
            .selected_idx
            .filter(|&idx| idx < self.entries.len())
            .unwrap_or(0);
        self.selected_idx = Some(primary);
        self.selected_set = (0..self.entries.len())
            .filter(|&idx| idx != primary)
            .collect();
        self.anchor_idx = Some(primary);
        self.select_all_active = true;
    }

    fn restore_selection_by_paths(
        &mut self,
        selected_paths: &HashSet<PathBuf>,
        selected_path: Option<&Path>,
        anchor_path: Option<&Path>,
    ) {
        let selected_path_key = selected_path.map(normalize_path_for_selection);
        let anchor_path_key = anchor_path.map(normalize_path_for_selection);
        let selected_paths_keys: HashSet<String> = selected_paths
            .iter()
            .map(|path| normalize_path_for_selection(path.as_path()))
            .collect();
        let entry_path_keys: Vec<String> = self
            .entries
            .iter()
            .map(|entry| normalize_path_for_selection(Self::entry_path_ref(entry)))
            .collect();

        self.selected_set.clear();
        self.selected_idx = selected_path_key.as_ref().and_then(|target_key| {
            entry_path_keys
                .iter()
                .position(|entry_key| entry_key == target_key)
        });
        for (idx, entry_key) in entry_path_keys.iter().enumerate() {
            if selected_paths_keys.contains(entry_key) && Some(idx) != self.selected_idx {
                self.selected_set.insert(idx);
            }
        }
        self.anchor_idx = anchor_path_key
            .as_ref()
            .and_then(|target_key| {
                entry_path_keys
                    .iter()
                    .position(|entry_key| entry_key == target_key)
            })
            .or(self.selected_idx);
        if self.selected_idx.is_none() {
            self.anchor_idx = None;
        }
    }

    /// Shift クリック時の範囲選択: anchor から idx まで selected_set に追加
    fn extend_selection_to(&mut self, idx: usize) {
        let anchor = self.anchor_idx.or(self.selected_idx).unwrap_or(idx);
        let (lo, hi) = if anchor <= idx {
            (anchor, idx)
        } else {
            (idx, anchor)
        };
        for i in lo..=hi {
            self.selected_set.insert(i);
        }
        self.select_all_active = false;
    }

    fn has_text_input_focus(&self) -> bool {
        self.path_input_focused || self.filter_input_focused
    }

    fn is_selected(&self, idx: usize) -> bool {
        self.selected_idx == Some(idx) || self.selected_set.contains(&idx)
    }

    fn ctrl_toggle_selection(&mut self, idx: usize) {
        if self.is_selected(idx) {
            self.remove_from_selection(idx);
        } else {
            if let Some(primary) = self.selected_idx {
                self.selected_set.insert(primary);
            }
            self.selected_set.insert(idx);
            self.selected_idx = Some(idx);
        }
        self.anchor_idx = Some(idx);
        self.select_all_active = false;
    }

    fn remove_from_selection(&mut self, idx: usize) {
        if self.selected_idx == Some(idx) {
            self.selected_set.remove(&idx);
            let mut remaining: Vec<usize> = self.selected_set.iter().copied().collect();
            remaining.sort_unstable();
            if let Some(&next_primary) = remaining.first() {
                self.selected_set.remove(&next_primary);
                self.selected_idx = Some(next_primary);
            } else {
                self.selected_idx = None;
            }
        } else {
            self.selected_set.remove(&idx);
        }
        self.select_all_active = false;
    }
}

fn system_time_to_unix_secs(time: SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

// ── ライブラリパネル描画 ──────────────────────────────────────────────────────

/// ライブラリパネルを描画する。アクションを返す（Open/Rename/Delete/Copy）。
pub fn show(
    ui: &mut egui::Ui,
    state: &mut LibraryState,
    language: UiLanguage,
    interaction_blocked: bool,
    external_tools: &[ExternalToolMenuItem],
    external_tool_busy: bool,
) -> LibraryAction {
    // フィルタ / ソートが変更されていれば再構築
    if state.filter_dirty {
        state.rebuild_entries();
    }

    if state.show_empty_library_message(ui, language) {
        return LibraryAction::None;
    }

    // スクロール復元・追従
    if state.scroll_selected_into_view_pending {
        state.scroll_selected_into_view_pending = false;
        if let Some(selected_idx) = state.selected_idx {
            let gap = theme::GRID_GAP;
            let cell_size = egui::vec2(state.thumb_w, state.thumb_h);
            let avail_w = ui.available_width();
            let cols = ((avail_w + gap) / (cell_size.x + gap)).floor().max(1.0) as usize;
            let row_h = cell_size.y + gap;
            let selected_row = selected_idx / cols;
            let selected_y_top = selected_row as f32 * row_h;
            let selected_y_bottom = selected_y_top + row_h;
            let current_offset = state.scroll_y.max(0.0);
            let visible_h = ui.available_height().max(row_h);
            if selected_y_top < current_offset {
                state.scroll_to_pending = Some(selected_y_top);
            } else if selected_y_bottom > current_offset + visible_h {
                state.scroll_to_pending = Some((selected_y_bottom - visible_h).max(0.0));
            }
        }
    }

    let restore_scroll = if state.scroll_restore_pending {
        state.scroll_restore_pending = false;
        Some(state.initial_scroll_y)
    } else {
        state.scroll_to_pending.take()
    };

    let thumb_size = egui::vec2(state.thumb_w, state.thumb_h);

    // グリッド描画
    let reset_cache = state.reset_context_menu_cache;
    state.reset_context_menu_cache = false;
    let result = virtual_grid::show_grid(
        ui,
        virtual_grid::GridViewContext {
            entries: &state.entries,
            book_states: &state.book_states,
            selected_idx: state.selected_idx,
            selected_set: &state.selected_set,
            is_favorite: &|entry| state.is_favorite_entry(entry),
            reading_hud_state: &|entry| state.reading_hud_state_for_entry(entry),
            interaction_enabled: !interaction_blocked,
            external_tools,
            external_tool_busy,
            language,
        },
        virtual_grid::GridViewConfig {
            restore_scroll,
            thumb_size,
            wheel_scroll_multiplier: state.wheel_scroll_multiplier,
            hud_mode: state.hud_mode,
            hud_style: state.hud_style,
            selection_style: state.selection_style,
            hud_font_size: state.hud_font_size,
            reset_context_menu_cache: reset_cache,
        },
    );

    // ── 選択状態を更新 ────────────────────────────────────────────────────────
    if let Some(sel) = result.selected {
        match sel {
            KeyboardSelection::Plain(sel) => {
                // 通常クリック or キーナビ: 複数選択をクリア
                state.selected_idx = Some(sel);
                state.selected_set.clear();
                state.anchor_idx = Some(sel);
                state.select_all_active = false;
            }
            KeyboardSelection::Shift(sel) => {
                state.extend_selection_to(sel);
                state.selected_idx = Some(sel);
                state.select_all_active = false;
            }
        }
    }

    if let Some(multi) = result.multi_select {
        use virtual_grid::MultiClick;
        match multi {
            MultiClick::Ctrl(idx) => {
                state.ctrl_toggle_selection(idx);
            }
            MultiClick::Shift(idx) => {
                // Shift+クリック: 範囲選択
                state.extend_selection_to(idx);
                state.selected_idx = Some(idx);
                state.select_all_active = false;
            }
        }
    }

    state.scroll_y = result.scroll_y;
    if let Some(y) = result.request_scroll_y {
        state.scroll_to_pending = Some(y);
    }

    // ── コンテキストメニューアクション → LibraryAction へ変換 ───────────────
    if let Some((idx, action)) = result.context_action {
        if let Some(action) = state.resolve_context_action(idx, action) {
            return action;
        }
    }

    if let Some(idx) = result.drag_started {
        return LibraryAction::ExternalDrag(
            if state.selected_set.contains(&idx) || state.selected_idx == Some(idx) {
                state.effective_selection()
            } else {
                vec![idx]
            },
        );
    }

    if let Some(idx) = result.opened {
        return state.resolve_open_action(idx);
    }

    LibraryAction::None
}

fn entry_file_snapshot_changed(old: &BookMeta, new: &BookMeta) -> bool {
    old.size != new.size || old.modified != new.modified || old.path != new.path
}

fn detect_ctrl_key_edge(v_key: i32, was_held: &mut bool) -> bool {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetAsyncKeyState(v_key: i32) -> i16;
        }

        let held = unsafe {
            (GetAsyncKeyState(0x11) as u16 & 0x8000 != 0)
                && (GetAsyncKeyState(v_key) as u16 & 0x8000 != 0)
        };
        let fired = held && !*was_held;
        *was_held = held;
        fired
    }
    #[cfg(not(windows))]
    {
        let _ = was_held;
        false
    }
}

fn compute_parent_counts(
    leaf_counts: &HashMap<String, usize>,
    groups: &HashMap<String, crate::domain::kind_group::GroupDef>,
) -> HashMap<String, usize> {
    let mut memo: HashMap<String, usize> = HashMap::new();

    fn dfs(
        node: &str,
        leaf_counts: &HashMap<String, usize>,
        groups: &HashMap<String, crate::domain::kind_group::GroupDef>,
        memo: &mut HashMap<String, usize>,
    ) -> usize {
        if let Some(&cached) = memo.get(node) {
            return cached;
        }
        let mut total = *leaf_counts.get(node).unwrap_or(&0);
        if let Some(def) = groups.get(node) {
            for child in &def.children {
                total += dfs(child, leaf_counts, groups, memo);
            }
        }
        memo.insert(node.to_string(), total);
        total
    }

    for key in groups.keys() {
        dfs(key, leaf_counts, groups, &mut memo);
    }

    groups
        .keys()
        .map(|k| (k.clone(), *memo.get(k).unwrap_or(&0)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::archive::FolderMeta;
    use crate::infra::favorite_store::{FavoriteEntry, FavoriteStore};
    use std::{collections::HashMap, sync::Arc, time::SystemTime};

    fn folder_entry(path: PathBuf, title: &str) -> LibraryEntry {
        LibraryEntry::Folder(FolderMeta {
            path: Arc::<Path>::from(path.into_boxed_path()),
            title: Arc::<str>::from(title),
            modified: SystemTime::UNIX_EPOCH,
        })
    }

    #[test]
    fn favorites_scope_matches_keyword_and_favorite_store() {
        let path = PathBuf::from(r"C:\books\作品A");
        let normalized = normalize_path_for_selection(&path);
        let entry = folder_entry(path, "作品A");
        let favorite_store = FavoriteStore::from_entries(vec![FavoriteEntry {
            normalized_path: normalized,
            file_size: 123,
            modified: 456,
        }]);

        let mut filter = LibraryFilter {
            keyword: "作品A".to_owned(),
            scope: LibraryScope::Favorites,
        };

        assert!(filter.matches(
            &entry,
            &HashMap::new(),
            &HashMap::new(),
            &KindGroupConfig::default(),
            &favorite_store
        ));

        filter.keyword = "別".to_owned();
        assert!(!filter.matches(
            &entry,
            &HashMap::new(),
            &HashMap::new(),
            &KindGroupConfig::default(),
            &favorite_store
        ));
    }
}
