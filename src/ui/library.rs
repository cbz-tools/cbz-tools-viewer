//! ライブラリ画面（サムネイルグリッド）。
//!
//! `LibraryState` がアプリの主要 UI 状態を保持し、
//! `show()` が egui パネル内での描画を担う。

use parking_lot::RwLock;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

// UI が idle 中でも poll_worker が動くようにする repaint 間隔
const POLL_INTERVAL_MS: u64 = 80;
// 1フレームで UI スレッドが反映するサムネイル結果の上限
const MAX_THUMB_RESULTS_PER_FRAME: usize = 48;
const BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE: usize = 3_000;
// visible 範囲を優先して保持する Library thumbnail TextureHandle 上限
const LIBRARY_THUMB_TEXTURE_KEEP_MAX_BYTES: usize = 256 * 1024 * 1024;
const RGBA_BYTES_PER_PIXEL: usize = 4;
// ライブラリフォルダのリアルタイム追従用ポーリング間隔
const LIBRARY_DIR_POLL_INTERVAL: Duration = Duration::from_secs(3);
const PREVIEW_HOVER_DELAY: Duration = Duration::from_millis(300);
const VIDEO_PREVIEW_CYCLE: Duration = Duration::from_secs(10);
const VIDEO_PREVIEW_STEP_PERCENT: u8 = 10;
const VIDEO_PREVIEW_FIRST_SCENE_PERCENT: u8 = 10;
const VIDEO_PREVIEW_LAST_SCENE_PERCENT: u8 = 90;
pub(crate) const ANIMATED_PREVIEW_SCRUB_BUCKET_COUNT: u16 = 16;
// 起動前に記録済みの Page Map 失敗を、カードが初めて表示される際に読み出す。

#[derive(Clone, Copy)]
enum WorkerPollResult {
    Received,
    Failed,
    Stale,
    StaleAndContinue,
    Ignored,
    IgnoredAndContinue,
}

fn open_artifact_failure_cache() -> Option<ArtifactFailureDiskCache> {
    ArtifactFailureDiskCache::open(ArtifactFailureDiskCache::default_root())
        .or_else(|_| {
            ArtifactFailureDiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("artifact_failures"),
            )
        })
        .map_err(|error| {
            tracing::warn!("library page-map failure cache unavailable: {error}");
            error
        })
        .ok()
}

fn open_thumb_cache() -> Option<DiskCache> {
    DiskCache::open(DiskCache::default_root())
        .or_else(|_| DiskCache::open(std::env::temp_dir().join("cbz-thumbs")))
        .map_err(|error| {
            tracing::warn!("library thumbnail cache unavailable: {error}");
            error
        })
        .ok()
}

use eframe::egui;

use crate::{
    domain::{
        app_settings::{LibraryCardSelectionStyle, LibraryHudMode, LibraryHudStyle, UiLanguage},
        archive::{BookId, BookMeta, LibraryEntry},
        archive_settings::{
            FileSettings, ReadingState, SettingsStore, book_settings_path, book_settings_path_ref,
        },
        kind_group::KindGroupConfig,
        page_map::SourceRevision,
        sort::{SortKey, SortOrder},
    },
    infra::cache::{
        artifact_failure::{ArtifactFailureDiskCache, ArtifactKind},
        disk::DiskCache,
    },
    infra::favorite_store::{FavoriteState, FavoriteStore},
    infra::page_map::coordinator::PageMapStatus,
    infra::worker::thumb_worker::{
        AnimatedPreviewTask, StaticPreviewTask, ThumbTask, ThumbWorker, VideoPreviewTask,
        VideoThumbTask, WorkerMsg,
    },
    repaint::RepaintNotifier,
    util::{
        natural_sort,
        path_eq::{normalize_path_for_selection, paths_equivalent_for_selection},
    },
};

use super::{
    i18n::{TextKey, tr},
    theme,
    virtual_grid::{
        self, AnimatedPreviewMode, ContextAction, ExternalToolMenuItem, HoveredPreviewCell,
        HoveredPreviewKind, KeyboardSelection, VideoPreviewMode,
    },
};

// ── LibraryAction ─────────────────────────────────────────────────────────────

/// ライブラリ画面から上位（app.rs）へ通知するアクション
#[derive(Debug)]
pub enum LibraryAction {
    None,
    /// Ctrl+ホイールによるサムネイル表示サイズ変更（+1: 拡大、-1: 縮小）
    ThumbDisplaySizeChanged(i8),
    /// Archive を Viewer で開く
    OpenArchive(usize),
    /// VideoFile を OS 標準関連付けアプリで開く
    OpenVideo(usize),
    /// Folder を開く
    OpenFolder(usize),
    /// Explorer で開く
    OpenInExplorer(usize),
    /// 名前変更（対象 idx）
    Rename(usize),
    /// プロパティ表示（対象 idx）
    Properties(usize),
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

    let (f2, del, alt_enter, ctrl_c_pressed, ctrl_a_pressed) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::F2),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
            i.consume_key(egui::Modifiers::ALT, egui::Key::Enter),
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
    if alt_enter {
        if let Some(idx) = state.selected_idx {
            return Some(LibraryAction::Properties(idx));
        }
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
    /// current raw_entries に限った、お気に入り対象の既存 BookId 索引。
    favorite_book_ids: HashSet<BookId>,
    /// 表示用エントリ（sort + filter 適用済み）
    pub entries: Vec<LibraryEntry>,
    /// Authoritative source snapshots produced by the background scanner for all
    /// thumbnail/preview sources. Worker result admission additionally checks the
    /// filesystem, but drawing only consults this index.
    source_snapshots: HashMap<BookId, crate::infra::fs::scanner::SourceSnapshot>,
    /// Session-local cache-only Page Map availability for the current revision.
    /// A zero count is an exact-revision checked-negative and never enables Scrub.
    static_page_map_counts: HashMap<BookId, StaticPageMapMemo>,
    /// Invalidates in-flight scan count payloads after a newer memo mutation.
    static_page_map_memo_epoch: u64,

    /// BookId ごとのサムネイル表示状態
    pub book_states: HashMap<BookId, BookViewState>,
    /// VideoFile ごとのサムネイル状態。本の状態・scope とは独立させる。
    pub video_states: HashMap<BookId, VideoViewState>,
    preview: LibraryPreviewState,

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
    page_map_failure_cache: Option<ArtifactFailureDiskCache>,
    page_map_failure_revisions: HashMap<BookId, SourceRevision>,
    page_map_failure_checked_revisions: HashMap<BookId, SourceRevision>,
    /// Non-visible thumbnail/Page Map preparation targets supplied by the
    /// Library-side fixed-rate pump.
    background_artifact_targets: VecDeque<BackgroundArtifactTarget>,
    background_artifact_total: usize,
    background_artifact_checked: usize,
    background_artifact_supplied: usize,
    background_artifact_credit: f64,
    background_artifact_last_refill_at: Instant,
    background_artifact_worker_generation: u64,
    background_artifact_completion_logged: bool,
    thumb_cache: Option<DiskCache>,
    /// Last visible display-thumbnail task identity sequence sent to the worker.
    last_display_set: Option<Vec<DisplayThumbKey>>,

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
    pub texture_size: Option<[usize; 2]>,
    pub thumb_ready: bool,
    pub thumb_requested: bool,
    pub thumb_failed: bool,
    pub force_reload: bool,
    pub kind_group: Option<String>,
}

pub(crate) struct VideoViewState {
    pub texture: Option<egui::TextureHandle>,
    pub texture_size: Option<[usize; 2]>,
    pub thumb_ready: bool,
    pub thumb_requested: bool,
    pub thumb_failed: bool,
    pub requested_size: Option<u64>,
    pub requested_modified: Option<SystemTime>,
    pub requested_generation: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DisplayThumbKey {
    book_id: BookId,
    expected_size: u64,
    expected_modified: Option<SystemTime>,
    target_width: u16,
    bypass_cache: bool,
}

impl From<&ThumbTask> for DisplayThumbKey {
    fn from(task: &ThumbTask) -> Self {
        Self {
            book_id: task.book_id.clone(),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            target_width: task.target_width,
            bypass_cache: task.bypass_cache,
        }
    }
}

enum BackgroundArtifactTarget {
    Book(BookId),
    Video(BookId),
}

#[derive(Default)]
struct LibraryPreviewState {
    target: Option<HoveredPreviewCell>,
    session_id: u64,
    mode: VideoPreviewMode,
    latest_scrub_scene_index: Option<u64>,
    hover_deadline: Option<Instant>,
    timeline_start: Option<Instant>,
    preview_scroll_y: Option<f32>,
    preview_texture: Option<egui::TextureHandle>,
    next_scene_sequence: u64,
    next_scene_due: Option<Instant>,
    decode_in_flight: bool,
    in_flight_scene_index: Option<u64>,
    display_scene_index: Option<u64>,
    preview_failed: bool,
    animated_started: bool,
    animated_failed: bool,
    animated_unavailable: bool,
    animated_mode: AnimatedPreviewMode,
    animated_latest_target_bucket: Option<u16>,
    animated_last_submitted_target_bucket: Option<u16>,
    animated_in_flight_target_bucket: Option<u16>,
    /// Buckets whose active decode may already have completed after the UI
    /// superseded it. The bounded mask is carried by the next replaceable
    /// worker command so no superseded bucket remains permanently guarded.
    animated_abandon_bucket_mask: u64,
    animated_display_target_bucket: Option<u16>,
    animated_scrub_failed: bool,
    animated_last_frame_index: Option<u64>,
    latest_static_page_index: Option<u32>,
    static_in_flight_page_index: Option<u32>,
    static_display_page_index: Option<u32>,
    static_failed: bool,
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
    Extension(String),
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
        favorite_book_ids: &HashSet<BookId>,
    ) -> bool {
        self.keyword_matches(entry)
            && self.scope_matches(
                entry,
                book_states,
                reading_hud_states,
                kind_config,
                favorite_book_ids,
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
        favorite_book_ids: &HashSet<BookId>,
    ) -> bool {
        match &self.scope {
            LibraryScope::Any => true,
            LibraryScope::Favorites => Self::is_favorite_match(entry, favorite_book_ids),
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
            LibraryScope::Extension(extension) => Self::is_extension_match(entry, extension),
        }
    }

    fn is_extension_match(entry: &LibraryEntry, extension: &str) -> bool {
        normalized_entry_extension(entry)
            .is_some_and(|entry_extension| entry_extension == extension.to_ascii_uppercase())
    }

    fn is_favorite_match(entry: &LibraryEntry, favorite_book_ids: &HashSet<BookId>) -> bool {
        entry
            .favorite_id_ref()
            .is_some_and(|id| favorite_book_ids.contains(id))
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
        if matches!(entry, LibraryEntry::Folder(_) | LibraryEntry::VideoFile(_)) {
            return false;
        }
        match (
            reading_hud_states
                .get(book_settings_path_ref(entry.path()))
                .copied()
                .unwrap_or(ReadingHudState::Unread),
            expected,
        ) {
            (ReadingHudState::ReadingPercent(_), ReadingHudState::Reading) => true,
            (actual, expected) => actual == expected,
        }
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

/// Returns a display-ready, ASCII-case-normalized extension for file entries.
/// Directories are deliberately excluded even when their names contain dots.
fn normalized_entry_extension(entry: &LibraryEntry) -> Option<String> {
    if !matches!(
        entry,
        LibraryEntry::Archive(_) | LibraryEntry::ImageFile(_) | LibraryEntry::VideoFile(_)
    ) {
        return None;
    }
    let extension = entry.path().extension()?.to_str()?;
    (!extension.is_empty()).then(|| extension.to_ascii_uppercase())
}

struct AsyncLoadResult {
    generation: u64,
    static_page_map_memo_epoch: u64,
    path: PathBuf,
    result: Result<crate::infra::fs::scanner::ScannedDir, anyhow::Error>,
}

struct AsyncDiffScanResult {
    generation: u64,
    static_page_map_memo_epoch: u64,
    path: PathBuf,
    reason: DiffScanReason,
    result: Result<crate::infra::fs::scanner::ScannedDir, anyhow::Error>,
}

#[derive(Clone, Debug)]
struct StaticPageMapMemo {
    source_revision: SourceRevision,
    page_count: usize,
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
    extension_counts: HashMap<String, usize>,
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
    VideoFile,
    Folder,
}

#[derive(Clone, Debug)]
pub(crate) struct DeletedEntryCleanup {
    pub kind: DeletedEntryKind,
    pub book_meta: Option<BookMeta>,
    pub thumb_id: Option<BookId>,
    pub video_id: Option<BookId>,
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

    fn resolve_open_action(&self, idx: usize, folder_book_open_as_viewer: bool) -> LibraryAction {
        match self.entries.get(idx) {
            Some(LibraryEntry::Folder(_)) => LibraryAction::OpenFolder(idx),
            Some(LibraryEntry::FolderBook(entry)) => {
                if !folder_book_open_as_viewer {
                    LibraryAction::OpenFolder(idx)
                } else if self.has_ready_thumbnail(&entry.id) {
                    LibraryAction::OpenArchive(idx)
                } else {
                    LibraryAction::None
                }
            }
            Some(LibraryEntry::VideoFile(_)) => LibraryAction::OpenVideo(idx),
            Some(LibraryEntry::ImageFile(_)) => LibraryAction::OpenArchive(idx),
            Some(LibraryEntry::Archive(entry)) if self.has_ready_thumbnail(&entry.id) => {
                LibraryAction::OpenArchive(idx)
            }
            _ => LibraryAction::None,
        }
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
        folder_book_open_as_viewer: bool,
    ) -> Option<LibraryAction> {
        match action {
            ContextAction::Open => {
                let action = self.resolve_open_action(idx, folder_book_open_as_viewer);
                if matches!(action, LibraryAction::None) {
                    None
                } else {
                    Some(action)
                }
            }
            ContextAction::Rename => Some(LibraryAction::Rename(idx)),
            ContextAction::Properties => Some(LibraryAction::Properties(idx)),
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

    pub fn extension_counts(&self) -> &HashMap<String, usize> {
        &self.group_counts.extension_counts
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
        entry
            .favorite_id_ref()
            .is_some_and(|id| self.favorite_book_ids.contains(id))
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

        self.rebuild_favorite_book_ids();
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
                LibraryEntry::Folder(_)
                | LibraryEntry::ImageFile(_)
                | LibraryEntry::VideoFile(_) => return None,
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

        self.rebuild_favorite_book_ids();
        self.recompute_group_counts();
        self.rebuild_entries();
        state
    }

    /// Store 更新後に current raw_entries のお気に入り索引を同期する。
    /// エントリの path が置き換わる rename 中は worker を起動しない。
    pub(crate) fn refresh_favorite_index(&mut self) {
        self.rebuild_favorite_book_ids();
        self.recompute_group_counts();
        self.filter_dirty = true;
    }

    pub(crate) fn favorite_store_handle(&self) -> Arc<RwLock<FavoriteStore>> {
        Arc::clone(&self.favorite_store)
    }

    pub(crate) fn reading_hud_state_for_entry(&self, entry: &LibraryEntry) -> ReadingHudState {
        if matches!(entry, LibraryEntry::Folder(_) | LibraryEntry::VideoFile(_)) {
            return ReadingHudState::Unread;
        }
        self.reading_hud_states
            .get(book_settings_path_ref(entry.path()))
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn has_page_map_failure_for_entry(&self, entry: &LibraryEntry) -> bool {
        let LibraryEntry::Archive(book) = entry else {
            return false;
        };
        let Some(snapshot) = self.source_snapshots.get(&book.id) else {
            return false;
        };
        let revision = SourceRevision::from_file_state(snapshot.size, snapshot.modified);
        self.page_map_failure_revisions.get(&book.id) == Some(&revision)
    }

    fn refresh_visible_page_map_failures(
        &mut self,
        visible_range: std::ops::RangeInclusive<usize>,
        ctx: &egui::Context,
    ) {
        let checks: Vec<_> = visible_range
            .filter_map(|idx| match self.entries.get(idx) {
                Some(LibraryEntry::Archive(book)) => {
                    let snapshot = self.source_snapshots.get(&book.id)?;
                    let revision =
                        SourceRevision::from_file_state(snapshot.size, snapshot.modified);
                    if self.page_map_failure_checked_revisions.get(&book.id) == Some(&revision) {
                        return None;
                    }
                    Some((
                        book.id.clone(),
                        revision.clone(),
                        self.page_map_failure_cache.as_ref().is_some_and(|cache| {
                            cache.has_failure_for_revision(
                                &book.id,
                                &revision,
                                ArtifactKind::PageMap,
                            )
                        }),
                    ))
                }
                _ => None,
            })
            .collect();

        let mut changed = false;
        for (id, revision, failed) in checks {
            self.page_map_failure_checked_revisions
                .insert(id.clone(), revision.clone());
            if failed {
                if self.page_map_failure_revisions.get(&id) != Some(&revision) {
                    self.page_map_failure_revisions.insert(id, revision);
                    changed = true;
                }
            } else if self.page_map_failure_revisions.remove(&id).is_some() {
                changed = true;
            }
        }
        if changed {
            ctx.request_repaint();
        }
    }

    fn apply_page_map_status(&mut self, status: PageMapStatus) {
        if status.task_generation != self.worker.current_generation()
            || status.task_artifact_generation != self.worker.current_artifact_generation()
        {
            return;
        }
        let current_revision = self
            .source_snapshots
            .get(&status.book_id)
            .map(|snapshot| SourceRevision::from_file_state(snapshot.size, snapshot.modified));
        if current_revision.as_ref() != Some(&status.source_revision) {
            return;
        }
        self.page_map_failure_checked_revisions
            .insert(status.book_id.clone(), status.source_revision.clone());
        if status.failed {
            self.static_page_map_counts.remove(&status.book_id);
            self.page_map_failure_revisions
                .insert(status.book_id.clone(), status.source_revision.clone());
        } else if self.page_map_failure_revisions.get(&status.book_id)
            == Some(&status.source_revision)
        {
            self.page_map_failure_revisions.remove(&status.book_id);
        }
        if let Some(page_count) = status.page_count.filter(|page_count| *page_count > 0) {
            self.static_page_map_counts.insert(
                status.book_id,
                StaticPageMapMemo {
                    source_revision: status.source_revision,
                    page_count,
                },
            );
        }
        self.static_page_map_memo_epoch = self.static_page_map_memo_epoch.saturating_add(1);
    }

    fn invalidate_static_page_map_memo(&mut self) {
        self.static_page_map_counts.clear();
        self.static_page_map_memo_epoch = self.static_page_map_memo_epoch.saturating_add(1);
    }

    fn static_page_map_memo_snapshot(&self) -> HashMap<BookId, (SourceRevision, usize)> {
        self.static_page_map_counts
            .iter()
            .filter(|(id, memo)| {
                self.source_snapshots.get(*id).is_some_and(|snapshot| {
                    memo.source_revision
                        == SourceRevision::from_file_state(snapshot.size, snapshot.modified)
                })
            })
            .map(|(id, memo)| (id.clone(), (memo.source_revision.clone(), memo.page_count)))
            .collect()
    }

    fn apply_scanned_static_page_map_counts(
        &mut self,
        static_page_counts: HashMap<BookId, (SourceRevision, usize)>,
        scan_memo_epoch: u64,
    ) {
        if scan_memo_epoch == self.static_page_map_memo_epoch {
            self.static_page_map_counts = static_page_counts
                .into_iter()
                .map(|(id, (source_revision, page_count))| {
                    (
                        id,
                        StaticPageMapMemo {
                            source_revision,
                            page_count,
                        },
                    )
                })
                .collect();
            return;
        }

        // A newer status/cache/delete event won the race with this scan. Keep
        // only its current-revision memo entries and never reintroduce the scan's
        // older cache-only observations.
        self.static_page_map_counts.retain(|id, memo| {
            self.source_snapshots.get(id).is_some_and(|snapshot| {
                memo.source_revision
                    == SourceRevision::from_file_state(snapshot.size, snapshot.modified)
            })
        });
    }

    fn prune_page_map_failure_states(&mut self) {
        let revisions: HashMap<_, _> = self
            .source_snapshots
            .iter()
            .map(|(id, snapshot)| {
                (
                    id.clone(),
                    SourceRevision::from_file_state(snapshot.size, snapshot.modified),
                )
            })
            .collect();
        self.page_map_failure_revisions
            .retain(|id, revision| revisions.get(id) == Some(revision));
        self.page_map_failure_checked_revisions
            .retain(|id, revision| revisions.get(id) == Some(revision));
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
        self.static_page_map_memo_epoch = self.static_page_map_memo_epoch.saturating_add(1);
        self.invalidate_preview();
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
        self.rebuild_favorite_book_ids();

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
            self.source_snapshots.remove(id);
            self.static_page_map_counts.remove(id);
            self.page_map_failure_revisions.remove(id);
            self.page_map_failure_checked_revisions.remove(id);
        }
        if cleanup.kind == DeletedEntryKind::VideoFile {
            if let Some(id) = cleanup.video_id.as_ref() {
                self.video_states.remove(id);
                self.source_snapshots.remove(id);
                self.static_page_map_counts.remove(id);
                self.page_map_failure_revisions.remove(id);
                self.page_map_failure_checked_revisions.remove(id);
            }
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
            LibraryEntry::VideoFile(_) => DeletedEntryKind::VideoFile,
            LibraryEntry::Folder(_) => DeletedEntryKind::Folder,
        };
        let thumb_id = deleted_entry.thumb_id();
        let book_meta = Self::book_entry_ref(&deleted_entry).cloned();
        let video_id = match &deleted_entry {
            LibraryEntry::VideoFile(entry) => Some(entry.id.clone()),
            _ => None,
        };
        Some(DeletedEntryCleanup {
            kind,
            book_meta,
            thumb_id,
            video_id,
        })
    }

    fn book_state_mut(&mut self, id: &BookId) -> &mut BookViewState {
        self.book_states
            .entry(id.clone())
            .or_insert_with(|| BookViewState {
                texture: None,
                texture_size: None,
                thumb_ready: false,
                thumb_requested: false,
                thumb_failed: false,
                force_reload: false,
                kind_group: None,
            })
    }

    fn book_state(&self, id: &BookId) -> Option<&BookViewState> {
        self.book_states.get(id)
    }

    fn video_state_mut(&mut self, id: &BookId) -> &mut VideoViewState {
        self.video_states
            .entry(id.clone())
            .or_insert_with(|| VideoViewState {
                texture: None,
                texture_size: None,
                thumb_ready: false,
                thumb_requested: false,
                thumb_failed: false,
                requested_size: None,
                requested_modified: None,
                requested_generation: None,
            })
    }

    fn video_state(&self, id: &BookId) -> Option<&VideoViewState> {
        self.video_states.get(id)
    }

    fn invalidate_preview(&mut self) {
        self.preview.session_id = self.preview.session_id.wrapping_add(1);
        self.worker.stop_preview();
        self.preview = LibraryPreviewState {
            session_id: self.preview.session_id,
            ..LibraryPreviewState::default()
        };
    }

    fn preview_for_grid(&self) -> Option<(&BookId, &egui::TextureHandle)> {
        self.preview
            .target
            .as_ref()
            .zip(self.preview.preview_texture.as_ref())
            .map(|(target, texture)| (&target.book_id, texture))
    }

    fn update_preview(
        &mut self,
        ctx: &egui::Context,
        hovered: Option<HoveredPreviewCell>,
        target_width: u16,
    ) {
        let target_changed = match (&self.preview.target, &hovered) {
            (Some(current), Some(next)) => {
                current.book_id != next.book_id
                    || current.path.as_ref() != next.path.as_ref()
                    || current.size != next.size
                    || current.modified != next.modified
                    || !matches!(
                        (&current.kind, &next.kind),
                        (
                            HoveredPreviewKind::Video { .. },
                            HoveredPreviewKind::Video { .. }
                        ) | (
                            HoveredPreviewKind::Static { .. },
                            HoveredPreviewKind::Static { .. }
                        ) | (
                            HoveredPreviewKind::Animated { .. },
                            HoveredPreviewKind::Animated { .. }
                        )
                    )
            }
            (None, None) => false,
            _ => true,
        };
        if target_changed {
            self.invalidate_preview();
            if let Some(target) = hovered {
                let now = Instant::now();
                if let HoveredPreviewKind::Video {
                    mode,
                    scrub_scene_index,
                } = target.kind
                {
                    self.preview.mode = mode;
                    self.preview.latest_scrub_scene_index = scrub_scene_index;
                } else if let HoveredPreviewKind::Static {
                    page_index,
                    page_count: _,
                } = target.kind
                {
                    self.preview.latest_static_page_index = Some(page_index);
                    self.preview.static_in_flight_page_index = None;
                    self.preview.static_display_page_index = None;
                } else if let HoveredPreviewKind::Animated {
                    mode,
                    target_bucket,
                } = target.kind
                {
                    self.preview.animated_mode = mode;
                    self.preview.animated_latest_target_bucket = target_bucket;
                    self.preview.animated_last_submitted_target_bucket = None;
                    self.preview.animated_in_flight_target_bucket = None;
                    self.preview.animated_abandon_bucket_mask = 0;
                    self.preview.animated_display_target_bucket = None;
                    self.preview.animated_scrub_failed = false;
                }
                let hover_deadline = match target.kind {
                    HoveredPreviewKind::Static { .. }
                    | HoveredPreviewKind::Animated {
                        mode: AnimatedPreviewMode::TimeScrub,
                        ..
                    } => None,
                    HoveredPreviewKind::Video { .. }
                    | HoveredPreviewKind::Animated {
                        mode: AnimatedPreviewMode::Auto,
                        ..
                    } => Some(now + PREVIEW_HOVER_DELAY),
                };
                self.preview.target = Some(target);
                self.preview.hover_deadline = hover_deadline;
                self.preview.preview_scroll_y = Some(self.scroll_y);
            }
        } else if let Some(hovered) = hovered {
            if let HoveredPreviewKind::Video {
                mode,
                scrub_scene_index,
            } = hovered.kind
            {
                let previous_mode = self.preview.mode;
                self.preview.mode = mode;
                self.preview.latest_scrub_scene_index = scrub_scene_index;
                if previous_mode == VideoPreviewMode::Scrub && mode == VideoPreviewMode::Auto {
                    self.resume_video_preview_auto(Instant::now());
                }
            } else if matches!(hovered.kind, HoveredPreviewKind::Static { .. }) {
                if let HoveredPreviewKind::Static { page_index, .. } = hovered.kind {
                    let page_changed = self.preview.latest_static_page_index != Some(page_index);
                    self.preview.latest_static_page_index = Some(page_index);
                    if page_changed {
                        self.preview.static_failed = false;
                    }
                }
                self.preview.target = Some(hovered.clone());
            } else if let HoveredPreviewKind::Animated {
                mode,
                target_bucket,
            } = hovered.kind
            {
                let previous_mode = self.preview.animated_mode;
                let previous_bucket = self.preview.animated_latest_target_bucket;
                self.preview.target = Some(hovered.clone());
                self.preview.animated_mode = mode;
                self.preview.animated_latest_target_bucket = target_bucket;
                if previous_bucket != target_bucket {
                    self.preview.animated_scrub_failed = false;
                }
                if previous_mode != mode {
                    self.preview.animated_scrub_failed = false;
                    self.preview.animated_display_target_bucket = None;
                    self.preview.animated_last_submitted_target_bucket = None;
                    self.preview.animated_abandon_bucket_mask = 0;
                    self.preview.hover_deadline = None;
                    match mode {
                        AnimatedPreviewMode::TimeScrub => {}
                        AnimatedPreviewMode::Auto => {
                            self.preview.animated_last_frame_index = None;
                            self.preview.animated_last_submitted_target_bucket = None;
                            self.preview.animated_in_flight_target_bucket = None;
                            self.preview.animated_abandon_bucket_mask = 0;
                            self.preview.decode_in_flight = false;
                            if self.preview.animated_started {
                                self.worker
                                    .resume_animated_preview_auto(AnimatedPreviewTask {
                                        session_id: self.preview.session_id,
                                        book_id: hovered.book_id.clone(),
                                        path: hovered.path.clone(),
                                        target_width,
                                        expected_size: hovered.size,
                                        expected_modified: hovered.modified,
                                        scrub_bucket: None,
                                        scrub_bucket_count: ANIMATED_PREVIEW_SCRUB_BUCKET_COUNT,
                                        abandon_bucket_mask: 0,
                                    });
                            }
                        }
                    }
                }
            }
        }

        let Some(target) = self.preview.target.clone() else {
            return;
        };
        if matches!(target.kind, HoveredPreviewKind::Static { .. }) {
            self.update_static_preview(ctx, target, target_width);
            return;
        }
        if matches!(target.kind, HoveredPreviewKind::Animated { .. }) {
            self.update_animated_preview(ctx, target, target_width);
            return;
        }
        if self.preview.preview_failed {
            return;
        }
        let now = Instant::now();
        if let Some(deadline) = self.preview.hover_deadline {
            if now < deadline {
                ctx.request_repaint_after(deadline - now);
                return;
            }
            let interval = video_preview_scene_interval();
            self.preview.hover_deadline = None;
            self.preview.timeline_start = Some(deadline);
            self.preview.next_scene_sequence = 1;
            self.preview.next_scene_due = Some(deadline + interval);
            self.preview.decode_in_flight = true;
            let scene_index = if self.preview.mode == VideoPreviewMode::Scrub {
                self.preview.latest_scrub_scene_index.unwrap_or_default()
            } else {
                0
            };
            self.preview.in_flight_scene_index = Some(scene_index);
            self.worker.start_video_preview(VideoPreviewTask {
                session_id: self.preview.session_id,
                book_id: target.book_id,
                path: target.path,
                target_width,
                expected_size: target.size,
                expected_modified: target.modified,
                scene_percent: video_preview_scene_percent(scene_index),
            });
            return;
        }

        if self.preview.decode_in_flight {
            return;
        }
        if self.preview.mode == VideoPreviewMode::Scrub {
            let Some(scene_index) = self.preview.latest_scrub_scene_index else {
                return;
            };
            if self.preview.display_scene_index == Some(scene_index) {
                return;
            }
            self.preview.decode_in_flight = true;
            self.preview.in_flight_scene_index = Some(scene_index);
            self.worker.request_video_preview_scene(
                self.preview.session_id,
                video_preview_scene_percent(scene_index),
            );
            return;
        }
        let Some(timeline_start) = self.preview.timeline_start else {
            return;
        };
        let Some(next_due) = self.preview.next_scene_due else {
            return;
        };
        if now < next_due {
            ctx.request_repaint_after(next_due - now);
            return;
        }

        let interval = video_preview_scene_interval();
        let timeline_sequence = video_preview_scene_sequence_at(timeline_start, now, interval);
        let sequence = self.preview.next_scene_sequence.max(timeline_sequence);
        let scene_percent = video_preview_scene_percent(sequence);
        self.preview.decode_in_flight = true;
        self.preview.in_flight_scene_index = Some(sequence % video_preview_scene_count());
        self.preview.next_scene_sequence = sequence.saturating_add(1);
        self.preview.next_scene_due =
            Some(timeline_start + interval.mul_f64(self.preview.next_scene_sequence as f64));
        self.worker
            .request_video_preview_scene(self.preview.session_id, scene_percent);
    }

    fn update_static_preview(
        &mut self,
        _ctx: &egui::Context,
        target: HoveredPreviewCell,
        target_width: u16,
    ) {
        if self.preview.static_failed {
            return;
        }
        let Some(page_index) = self.preview.latest_static_page_index else {
            return;
        };
        let HoveredPreviewKind::Static { page_count, .. } = target.kind else {
            return;
        };
        let task = StaticPreviewTask {
            session_id: self.preview.session_id,
            book_id: target.book_id,
            path: target.path,
            target_width,
            expected_size: target.size,
            expected_modified: target.modified,
            page_index,
            page_count,
        };
        if self.preview.decode_in_flight {
            if self.preview.static_in_flight_page_index != Some(page_index) {
                self.preview.static_in_flight_page_index = Some(page_index);
                self.worker.request_static_preview_page(task);
            }
            return;
        }
        if self.preview.static_display_page_index == Some(page_index) {
            return;
        }
        let has_static_session = self.preview.static_display_page_index.is_some()
            || self.preview.static_in_flight_page_index.is_some();
        self.preview.decode_in_flight = true;
        self.preview.static_in_flight_page_index = Some(page_index);
        if has_static_session {
            self.worker.request_static_preview_page(task);
        } else {
            self.worker.start_static_preview(task);
        }
    }

    fn update_animated_preview(
        &mut self,
        ctx: &egui::Context,
        target: HoveredPreviewCell,
        target_width: u16,
    ) {
        let HoveredPreviewKind::Animated {
            mode,
            target_bucket,
        } = target.kind
        else {
            return;
        };
        if self.preview.animated_unavailable {
            return;
        }
        if mode == AnimatedPreviewMode::Auto {
            if self.preview.animated_failed || self.preview.animated_started {
                return;
            }
            let now = Instant::now();
            let Some(deadline) = self.preview.hover_deadline else {
                return;
            };
            if now < deadline {
                ctx.request_repaint_after(deadline - now);
                return;
            }
            self.preview.hover_deadline = None;
            self.preview.animated_started = true;
            self.preview.animated_last_submitted_target_bucket = None;
            self.preview.animated_in_flight_target_bucket = None;
            self.worker.start_animated_preview(AnimatedPreviewTask {
                session_id: self.preview.session_id,
                book_id: target.book_id,
                path: target.path,
                target_width,
                expected_size: target.size,
                expected_modified: target.modified,
                scrub_bucket: None,
                scrub_bucket_count: ANIMATED_PREVIEW_SCRUB_BUCKET_COUNT,
                abandon_bucket_mask: 0,
            });
            return;
        }

        let Some(target_bucket) = target_bucket else {
            return;
        };
        if self.preview.animated_scrub_failed {
            return;
        }
        self.preview.hover_deadline = None;
        if self.preview.animated_started {
            match animated_scrub_request_action(
                self.preview.animated_display_target_bucket,
                self.preview.animated_in_flight_target_bucket,
                self.preview.animated_last_submitted_target_bucket,
                target_bucket,
            ) {
                AnimatedScrubRequestAction::CancelPending => {
                    let request_bucket = self
                        .preview
                        .animated_in_flight_target_bucket
                        .or(self.preview.animated_last_submitted_target_bucket)
                        .unwrap_or(target_bucket);
                    let had_decode_in_flight = self.preview.decode_in_flight;
                    let abandon_bucket_mask = animated_scrub_abandon_mask_for_cancel(
                        self.preview.animated_abandon_bucket_mask,
                        had_decode_in_flight,
                        request_bucket,
                    );
                    self.preview.animated_abandon_bucket_mask = abandon_bucket_mask;
                    let cancelled = self
                        .worker
                        .cancel_animated_preview_scrub(self.preview.session_id, request_bucket);
                    match animated_scrub_cancel_action(cancelled, had_decode_in_flight) {
                        AnimatedScrubCancelAction::Clear => {
                            self.preview.decode_in_flight = false;
                            self.preview.animated_in_flight_target_bucket = None;
                            self.preview.animated_last_submitted_target_bucket = None;
                            if abandon_bucket_mask != 0 {
                                self.worker.abandon_animated_preview_scrub(
                                    self.preview.session_id,
                                    abandon_bucket_mask,
                                );
                            }
                        }
                        AnimatedScrubCancelAction::RetainActive => {
                            if abandon_bucket_mask != 0 {
                                self.worker.abandon_animated_preview_scrub(
                                    self.preview.session_id,
                                    abandon_bucket_mask,
                                );
                            }
                        }
                        AnimatedScrubCancelAction::Retain => {
                            if abandon_bucket_mask != 0 {
                                self.worker.abandon_animated_preview_scrub(
                                    self.preview.session_id,
                                    abandon_bucket_mask,
                                );
                            }
                        }
                    }
                    return;
                }
                AnimatedScrubRequestAction::Noop => return,
                AnimatedScrubRequestAction::Submit => {}
            }
            let abandon_bucket_mask = animated_scrub_abandon_mask_for_submit(
                self.preview.animated_abandon_bucket_mask,
                self.preview.decode_in_flight,
                self.preview.animated_in_flight_target_bucket,
                target_bucket,
            );
            self.preview.animated_abandon_bucket_mask = abandon_bucket_mask;
            self.preview.decode_in_flight = true;
            self.preview.animated_last_submitted_target_bucket = Some(target_bucket);
            self.preview.animated_in_flight_target_bucket = Some(target_bucket);
            self.worker.request_animated_preview_scrub_with_abandon(
                AnimatedPreviewTask {
                    session_id: self.preview.session_id,
                    book_id: target.book_id,
                    path: target.path,
                    target_width,
                    expected_size: target.size,
                    expected_modified: target.modified,
                    scrub_bucket: Some(target_bucket),
                    scrub_bucket_count: ANIMATED_PREVIEW_SCRUB_BUCKET_COUNT,
                    abandon_bucket_mask,
                },
                abandon_bucket_mask,
            );
        } else {
            self.preview.animated_started = true;
            self.preview.decode_in_flight = true;
            self.preview.animated_last_submitted_target_bucket = Some(target_bucket);
            self.preview.animated_in_flight_target_bucket = Some(target_bucket);
            self.worker.start_animated_preview(AnimatedPreviewTask {
                session_id: self.preview.session_id,
                book_id: target.book_id,
                path: target.path,
                target_width,
                expected_size: target.size,
                expected_modified: target.modified,
                scrub_bucket: Some(target_bucket),
                scrub_bucket_count: ANIMATED_PREVIEW_SCRUB_BUCKET_COUNT,
                abandon_bucket_mask: self.preview.animated_abandon_bucket_mask,
            });
        }
    }

    fn preview_response_matches(
        &self,
        session_id: u64,
        book_id: &BookId,
        path: &Path,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
        scene_percent: u8,
    ) -> bool {
        self.preview.decode_in_flight
            && self.preview.in_flight_scene_index == Some(video_preview_scene_index(scene_percent))
            && self.preview.session_id == session_id
            && self.preview.target.as_ref().is_some_and(|target| {
                target.book_id == *book_id
                    && target.path.as_ref() == path
                    && target.size == expected_size
                    && target.modified == expected_modified
            })
    }

    fn static_preview_response_matches(
        &self,
        session_id: u64,
        book_id: &BookId,
        path: &Path,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
        page_index: u32,
    ) -> bool {
        self.preview.decode_in_flight
            && self.preview.session_id == session_id
            && self.preview.latest_static_page_index == Some(page_index)
            && self.preview.static_in_flight_page_index == Some(page_index)
            && self.preview.target.as_ref().is_some_and(|target| {
                target.book_id == *book_id
                    && target.path.as_ref() == path
                    && target.size == expected_size
                    && target.modified == expected_modified
                    && matches!(target.kind, HoveredPreviewKind::Static { .. })
            })
    }

    fn animated_preview_response_matches(
        &self,
        session_id: u64,
        book_id: &BookId,
        path: &Path,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
        scrub_bucket: Option<u16>,
    ) -> bool {
        let expected_mode = if scrub_bucket.is_some() {
            AnimatedPreviewMode::TimeScrub
        } else {
            AnimatedPreviewMode::Auto
        };
        self.preview.session_id == session_id
            && self.preview.animated_started
            && self.preview.animated_mode == expected_mode
            && self.preview.animated_latest_target_bucket == scrub_bucket
            && (scrub_bucket.is_none()
                || self.preview.animated_in_flight_target_bucket == scrub_bucket)
            && self.preview.target.as_ref().is_some_and(|target| {
                target.book_id == *book_id
                    && target.path.as_ref() == path
                    && target.size == expected_size
                    && target.modified == expected_modified
                    && matches!(target.kind, HoveredPreviewKind::Animated { .. })
            })
    }

    fn clear_animated_request_if_matches(&mut self, scrub_bucket: Option<u16>) {
        if self.preview.animated_in_flight_target_bucket == scrub_bucket {
            self.preview.decode_in_flight = false;
            self.preview.animated_in_flight_target_bucket = None;
        }
        if self.preview.animated_last_submitted_target_bucket == scrub_bucket {
            self.preview.animated_last_submitted_target_bucket = None;
        }
    }

    fn resume_video_preview_auto(&mut self, now: Instant) {
        if self.preview.timeline_start.is_some() {
            self.preview.next_scene_due = Some(now);
        }
    }

    fn preview_response_is_current_session(
        &self,
        session_id: u64,
        book_id: &BookId,
        path: &Path,
    ) -> bool {
        self.preview.session_id == session_id
            && self
                .preview
                .target
                .as_ref()
                .is_some_and(|target| target.book_id == *book_id && target.path.as_ref() == path)
    }

    fn preview_response_source_matches(
        &self,
        book_id: &BookId,
        path: &Path,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    ) -> bool {
        self.preview.target.as_ref().is_some_and(|target| {
            target.book_id == *book_id
                && target.path.as_ref() == path
                && target.size == expected_size
                && target.modified == expected_modified
        })
    }

    fn has_ready_thumbnail(&self, id: &BookId) -> bool {
        self.book_state(id)
            .is_some_and(|s| s.thumb_ready && !s.thumb_failed)
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

    pub(crate) fn register_rebuilt_cbz_entry(
        &mut self,
        old_path: &Path,
        rebuilt_entry: LibraryEntry,
    ) {
        self.static_page_map_memo_epoch = self.static_page_map_memo_epoch.saturating_add(1);
        let rebuilt_path = Self::entry_path_ref(&rebuilt_entry).to_path_buf();
        self.raw_entries.retain(|entry| {
            !paths_equivalent_for_selection(Self::entry_path_ref(entry), old_path)
                && !paths_equivalent_for_selection(
                    Self::entry_path_ref(entry),
                    rebuilt_path.as_path(),
                )
        });
        self.raw_entries.push(rebuilt_entry.clone());
        self.rebuild_favorite_book_ids();
        self.remove_hot_path_snapshot_for_path(old_path);
        self.insert_hot_path_snapshot_for_entry(&rebuilt_entry);
        self.static_page_map_counts
            .retain(|id, _| self.source_snapshots.contains_key(id));
        self.prefill_kind_groups();
        self.rebuild_entries();
        self.request_all_thumbs();
        self.request_thumb_for_entry(&rebuilt_entry, true);
    }

    pub fn clear_books(&mut self) {
        self.invalidate_preview();
        self.last_display_set = None;
        self.book_states.clear();
        self.video_states.clear();
        self.recompute_group_counts();
    }

    fn remove_hot_path_snapshot_for_path(&mut self, path: &Path) {
        self.source_snapshots
            .retain(|_, snapshot| snapshot.path.as_ref() != path);
        self.static_page_map_counts
            .retain(|id, _| self.source_snapshots.contains_key(id));
    }

    fn insert_hot_path_snapshot_for_entry(&mut self, entry: &LibraryEntry) {
        let (id, path, size, modified) = match entry {
            LibraryEntry::Archive(meta) => (
                meta.id.clone(),
                Arc::clone(&meta.path),
                meta.size,
                Some(meta.modified),
            ),
            LibraryEntry::FolderBook(meta) => (
                meta.id.clone(),
                Arc::clone(&meta.path),
                0,
                meta.revision_modified,
            ),
            LibraryEntry::ImageFile(meta) => (
                meta.id.clone(),
                Arc::clone(&meta.path),
                meta.size,
                Some(meta.modified),
            ),
            LibraryEntry::VideoFile(meta) => (
                meta.id.clone(),
                Arc::clone(&meta.path),
                meta.size,
                Some(meta.modified),
            ),
            LibraryEntry::Folder(_) => return,
        };
        self.static_page_map_counts.remove(&id);
        self.page_map_failure_revisions.remove(&id);
        self.page_map_failure_checked_revisions.remove(&id);
        self.source_snapshots.insert(
            id,
            crate::infra::fs::scanner::SourceSnapshot {
                path,
                size,
                modified,
            },
        );
    }

    fn book_entry_ref(entry: &LibraryEntry) -> Option<&BookMeta> {
        match entry {
            LibraryEntry::Archive(entry) => Some(entry),
            LibraryEntry::Folder(_)
            | LibraryEntry::FolderBook(_)
            | LibraryEntry::ImageFile(_)
            | LibraryEntry::VideoFile(_) => None,
        }
    }

    pub(crate) fn archive_entry_by_book_id(&self, book_id: &BookId) -> Option<LibraryEntry> {
        self.raw_entries.iter().find_map(|entry| match entry {
            LibraryEntry::Archive(meta) if meta.id == *book_id => Some(entry.clone()),
            _ => None,
        })
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
        let repaint = {
            let ctx = ctx.clone();
            RepaintNotifier::new(move || ctx.request_repaint())
        };
        Self {
            raw_entries: Vec::new(),
            favorite_book_ids: HashSet::new(),
            entries: Vec::new(),
            source_snapshots: HashMap::new(),
            static_page_map_counts: HashMap::new(),
            static_page_map_memo_epoch: 0,
            book_states: HashMap::new(),
            video_states: HashMap::new(),
            preview: LibraryPreviewState::default(),
            artifact_gate: Arc::clone(&artifact_gate),
            worker: ThumbWorker::spawn(repaint, artifact_gate),
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
            page_map_failure_cache: open_artifact_failure_cache(),
            page_map_failure_revisions: HashMap::new(),
            page_map_failure_checked_revisions: HashMap::new(),
            background_artifact_targets: VecDeque::new(),
            background_artifact_total: 0,
            background_artifact_checked: 0,
            background_artifact_supplied: 0,
            background_artifact_credit: 0.0,
            background_artifact_last_refill_at: Instant::now(),
            background_artifact_worker_generation: 0,
            background_artifact_completion_logged: true,
            thumb_cache: open_thumb_cache(),
            last_display_set: None,
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
        self.reset_background_artifact_pump();
        self.worker.update_global_goal_for_library(&path);
        self.path_input = path.to_string_lossy().into_owned();
        self.current_dir = Some(path);
        self.raw_entries.clear();
        self.favorite_book_ids.clear();
        self.entries.clear();
        self.source_snapshots.clear();
        self.invalidate_static_page_map_memo();
        self.clear_books();
        self.selected_idx = None;
        self.selected_set.clear();
        self.anchor_idx = None;
        self.select_all_active = false;
        let static_page_map_memo_epoch = self.static_page_map_memo_epoch;
        let previous_static_page_counts = HashMap::new();

        thread::spawn(move || {
            log::debug!(
                "[async-load] worker begin generation={} path={}",
                generation,
                path_for_worker.display()
            );
            let result = scanner::scan_dir_with_hot_path_indexes(
                &path_for_worker,
                &previous_static_page_counts,
            );
            log::debug!(
                "[async-load] worker finished generation={} ok={}",
                generation,
                result.is_ok()
            );
            let _ = tx.send(AsyncLoadResult {
                generation,
                static_page_map_memo_epoch,
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
            Ok(scanned) => {
                log::debug!(
                    "[async-load] apply generation={} entries={}",
                    done.generation,
                    scanned.entries.len()
                );
                self.apply_loaded_dir(done.path, scanned, done.static_page_map_memo_epoch);
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
    /// - 同一 path/id の置き換え: size/modified 変化を検出し、source change 後の failed state recovery 用に再要求する
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

    fn apply_scanned_entries_preserving_state(
        &mut self,
        scanned: crate::infra::fs::scanner::ScannedDir,
        scan_memo_epoch: u64,
    ) -> bool {
        let crate::infra::fs::scanner::ScannedDir {
            entries: scanned,
            source_snapshots,
            static_page_counts,
        } = scanned;
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
        let old_videofile_by_id = Self::videofile_snapshot_by_id(&self.raw_entries);
        let new_folderbook_by_id = Self::folderbook_modified_by_id(&scanned);
        let new_imagefile_by_id = Self::imagefile_snapshot_by_id(&scanned);
        let new_videofile_by_id = Self::videofile_snapshot_by_id(&scanned);

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
        for id in old_videofile_by_id.keys() {
            if !new_videofile_by_id.contains_key(id) {
                self.video_states.remove(id);
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
                    state.texture_size = None;
                    state.thumb_ready = false;
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
                    content_changed_ids.insert(id.clone());
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
                    state.texture_size = None;
                    state.thumb_ready = false;
                    state.thumb_requested = false;
                    state.thumb_failed = false;
                    content_changed_ids.insert(id.clone());
                    changed = true;
                }
                _ => {}
            }
        }
        for (id, new_snapshot) in &new_videofile_by_id {
            match old_videofile_by_id.get(id) {
                None => {
                    self.video_states.remove(id);
                    changed = true;
                }
                Some(old_snapshot) if old_snapshot != new_snapshot => {
                    self.video_states.remove(id);
                    changed = true;
                }
                _ => {}
            }
        }

        // A scan is authoritative for the hot-path indexes even when it does not
        // alter the visual entry list.
        self.source_snapshots = source_snapshots;
        self.apply_scanned_static_page_map_counts(static_page_counts, scan_memo_epoch);
        self.prune_page_map_failure_states();
        if !changed {
            return false;
        }

        for id in content_changed_ids {
            self.book_state_mut(&id).force_reload = true;
        }
        self.raw_entries = scanned;
        self.rebuild_favorite_book_ids();
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

    fn folderbook_modified_by_id(entries: &[LibraryEntry]) -> HashMap<BookId, Option<SystemTime>> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                LibraryEntry::FolderBook(folder) => {
                    Some((folder.id.clone(), folder.revision_modified))
                }
                _ => None,
            })
            .collect()
    }

    fn imagefile_snapshot_by_id(entries: &[LibraryEntry]) -> HashMap<BookId, (u64, SystemTime)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                LibraryEntry::ImageFile(file) => {
                    Some((file.id.clone(), (file.size, file.modified)))
                }
                _ => None,
            })
            .collect()
    }

    fn videofile_snapshot_by_id(entries: &[LibraryEntry]) -> HashMap<BookId, (u64, SystemTime)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                LibraryEntry::VideoFile(file) => {
                    Some((file.id.clone(), (file.size, file.modified)))
                }
                _ => None,
            })
            .collect()
    }

    fn thumbnail_source_matches_revision(
        &self,
        id: &BookId,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    ) -> bool {
        self.source_matches_revision(id, None, expected_size, expected_modified)
    }

    fn video_source_matches_revision(
        &self,
        id: &BookId,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    ) -> bool {
        self.source_matches_revision(id, None, expected_size, expected_modified)
    }

    fn preview_source_matches_revision(
        &self,
        id: &BookId,
        path: &Path,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    ) -> bool {
        self.source_matches_revision(id, Some(path), expected_size, expected_modified)
    }

    fn preview_snapshot_matches_revision(
        &self,
        id: &BookId,
        path: &Path,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    ) -> bool {
        let is_folder_book = self
            .raw_entries
            .iter()
            .any(|entry| matches!(entry, LibraryEntry::FolderBook(folder) if &folder.id == id));
        self.source_snapshots.get(id).is_some_and(|snapshot| {
            snapshot.path.as_ref() == path
                && (is_folder_book || snapshot.size == expected_size)
                && snapshot.modified == expected_modified
        })
    }

    /// O(1) index admission plus filesystem truth at worker-result receipt.
    fn source_matches_revision(
        &self,
        id: &BookId,
        expected_path: Option<&Path>,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    ) -> bool {
        let is_folder_book = self
            .raw_entries
            .iter()
            .any(|entry| matches!(entry, LibraryEntry::FolderBook(folder) if &folder.id == id));
        let Some(snapshot) = self.source_snapshots.get(id) else {
            return false;
        };
        if expected_path.is_some_and(|path| snapshot.path.as_ref() != path)
            || (!is_folder_book && snapshot.size != expected_size)
            || snapshot.modified != expected_modified
        {
            return false;
        }
        let Ok(current) = std::fs::metadata(snapshot.path.as_ref()) else {
            return false;
        };
        let current_modified = current.modified().ok();
        if is_folder_book {
            current_modified == expected_modified
        } else {
            current.len() == expected_size && current_modified == expected_modified
        }
    }

    fn static_page_count_for_entry(&self, entry: &LibraryEntry) -> Option<usize> {
        let id = match entry {
            LibraryEntry::Archive(book) => &book.id,
            LibraryEntry::FolderBook(folder) => &folder.id,
            _ => return None,
        };
        let revision = self
            .source_snapshots
            .get(id)
            .map(|snapshot| SourceRevision::from_file_state(snapshot.size, snapshot.modified))?;
        self.static_page_map_counts
            .get(id)
            .filter(|memo| memo.source_revision == revision)
            .map(|memo| memo.page_count)
            .filter(|page_count| *page_count > 0)
    }

    fn apply_loaded_dir(
        &mut self,
        path: PathBuf,
        scanned: crate::infra::fs::scanner::ScannedDir,
        scan_memo_epoch: u64,
    ) {
        let crate::infra::fs::scanner::ScannedDir {
            entries,
            source_snapshots,
            static_page_counts,
        } = scanned;
        self.path_input = path.to_string_lossy().into_owned();
        self.current_dir = Some(path);
        self.raw_entries = entries;
        self.rebuild_favorite_book_ids();
        self.source_snapshots = source_snapshots;
        self.apply_scanned_static_page_map_counts(static_page_counts, scan_memo_epoch);
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
        self.request_all_thumbs();
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
        let static_page_map_memo_epoch = self.static_page_map_memo_epoch;
        let previous_static_page_counts = self.static_page_map_memo_snapshot();
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
            let result = scanner::scan_dir_with_hot_path_indexes(
                &path_for_worker,
                &previous_static_page_counts,
            );
            log::debug!(
                "[diff-scan] finished reason={:?} path={} generation={} ok={}",
                reason,
                path_for_worker.display(),
                generation,
                result.is_ok()
            );
            let _ = tx.send(AsyncDiffScanResult {
                generation,
                static_page_map_memo_epoch,
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
                    scanned.entries.len()
                );
                if self.apply_scanned_entries_preserving_state(
                    scanned,
                    done.static_page_map_memo_epoch,
                ) {
                    self.request_all_thumbs();
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

    fn is_video_registered(&self, id: &BookId) -> bool {
        self.video_states.contains_key(id)
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
        let mut extension_counts: HashMap<String, usize> = HashMap::new();
        for entry in &self.raw_entries {
            if let Some(extension) = normalized_entry_extension(entry) {
                *extension_counts.entry(extension).or_insert(0) += 1;
            }
        }
        let favorite_count = self
            .raw_entries
            .iter()
            .filter(|entry| self.is_favorite_entry(entry))
            .count();
        let mut reading_unread_count = 0usize;
        let mut reading_reading_count = 0usize;
        let mut reading_read_count = 0usize;
        for entry in &self.raw_entries {
            if matches!(entry, LibraryEntry::Folder(_) | LibraryEntry::VideoFile(_)) {
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
            extension_counts,
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
        use crate::domain::filename_parser::{FilenamePartRole, parse_filename};
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
                    texture_size: None,
                    thumb_ready: false,
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
        self.invalidate_preview();
        self.last_display_set = None;
        self.prune_page_map_failure_states();
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
        self.recompute_group_counts();
    }

    fn prefill_reading_hud_states(&mut self) {
        let settings = SettingsStore::load();
        self.reading_hud_states.clear();
        for entry in &self.raw_entries {
            if matches!(entry, LibraryEntry::Folder(_) | LibraryEntry::VideoFile(_)) {
                continue;
            }
            let key = book_settings_path(entry.path());
            let file_settings = settings.get(key.as_path());
            self.reading_hud_states
                .insert(key, ReadingHudState::from_file_settings(&file_settings));
        }
    }

    fn filtered_entries(&self) -> Vec<LibraryEntry> {
        self.raw_entries
            .iter()
            .filter(|e| {
                self.filter.matches(
                    e,
                    &self.book_states,
                    &self.reading_hud_states,
                    &self.kind_config,
                    &self.favorite_book_ids,
                )
            })
            .cloned()
            .collect()
    }

    /// Store の正規化済み path スナップショットから、現在の raw_entries にだけ
    /// 対応する既存 BookId を作る。Store lock はスキャン前に必ず解放する。
    fn rebuild_favorite_book_ids(&mut self) {
        let favorite_paths = { self.favorite_store.read().normalized_paths_snapshot() };
        self.favorite_book_ids = self
            .raw_entries
            .iter()
            .filter_map(|entry| {
                let id = entry.favorite_id_ref()?;
                let normalized_path = normalize_path_for_selection(entry.path());
                favorite_paths
                    .contains(&normalized_path)
                    .then(|| id.clone())
            })
            .collect();
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
    /// ストレージは常に 500px 固定なので再生成ゼロ。
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
        self.invalidate_static_page_map_memo();
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
            let result = match msg {
                msg @ (WorkerMsg::VideoPreviewReady(_)
                | WorkerMsg::VideoPreviewFailed(_)
                | WorkerMsg::VideoPreviewStale(_)) => self.handle_video_preview_message(msg, ctx),
                msg @ (WorkerMsg::AnimatedPreviewReady(_)
                | WorkerMsg::AnimatedPreviewFailed(_)
                | WorkerMsg::AnimatedPreviewStale(_)
                | WorkerMsg::AnimatedPreviewUnavailable(_)) => {
                    self.handle_animated_preview_message(msg, ctx)
                }
                msg @ (WorkerMsg::StaticPreviewReady(_)
                | WorkerMsg::StaticPreviewFailed(_)
                | WorkerMsg::StaticPreviewStale(_)) => self.handle_static_preview_message(msg, ctx),
                msg @ (WorkerMsg::VideoReady(_) | WorkerMsg::VideoStale { .. }) => {
                    self.handle_video_thumbnail_message(msg, ctx)
                }
                WorkerMsg::Ready(resp) => self.handle_thumbnail_ready(resp, ctx),
                msg @ (WorkerMsg::Failed(_)
                | WorkerMsg::FailedPermanent(_)
                | WorkerMsg::FailedWithRevision { .. }
                | WorkerMsg::FailedPermanentWithRevision { .. }
                | WorkerMsg::Stale(_)) => self.handle_thumbnail_failure_or_stale(msg, ctx),
                WorkerMsg::PageMapStatus(status) => {
                    self.apply_page_map_status(status);
                    ctx.request_repaint();
                    WorkerPollResult::Ignored
                }
            };
            match result {
                WorkerPollResult::Received => received += 1,
                WorkerPollResult::Failed => failed += 1,
                WorkerPollResult::Stale | WorkerPollResult::StaleAndContinue => stale += 1,
                WorkerPollResult::Ignored | WorkerPollResult::IgnoredAndContinue => {}
            }
            if matches!(
                result,
                WorkerPollResult::StaleAndContinue | WorkerPollResult::IgnoredAndContinue
            ) {
                continue;
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

    fn handle_video_preview_message(
        &mut self,
        msg: WorkerMsg,
        ctx: &egui::Context,
    ) -> WorkerPollResult {
        match msg {
            WorkerMsg::VideoPreviewReady(preview) => {
                let current_session = self.preview_response_is_current_session(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                );
                let source_matches = self.preview_response_source_matches(
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                );
                let current_source_matches = self.video_source_matches_revision(
                    &preview.book_id,
                    preview.expected_size,
                    preview.expected_modified,
                );
                if !current_session
                    || !source_matches
                    || !current_source_matches
                    || !self.preview_response_matches(
                        preview.session_id,
                        &preview.book_id,
                        preview.path.as_ref(),
                        preview.expected_size,
                        preview.expected_modified,
                        preview.scene_percent,
                    )
                {
                    if current_session && (!source_matches || !current_source_matches) {
                        self.invalidate_preview();
                    }
                    return WorkerPollResult::StaleAndContinue;
                }
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [preview.width as usize, preview.height as usize],
                    &preview.pixels,
                );
                let texture = ctx.load_texture(
                    format!(
                        "video-preview-{}-{}",
                        preview.session_id, preview.scene_percent
                    ),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.preview.preview_texture = Some(texture);
                self.preview.decode_in_flight = false;
                self.preview.in_flight_scene_index = None;
                self.preview.display_scene_index =
                    Some(video_preview_scene_index(preview.scene_percent));
                ctx.request_repaint();
                WorkerPollResult::Received
            }
            WorkerMsg::VideoPreviewFailed(preview) | WorkerMsg::VideoPreviewStale(preview) => {
                let _scene_percent = preview.scene_percent;
                let current_session = self.preview_response_is_current_session(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                );
                let source_matches = self.preview_response_source_matches(
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                );
                let current_source_matches = self.video_source_matches_revision(
                    &preview.book_id,
                    preview.expected_size,
                    preview.expected_modified,
                );
                let request_matches = self.preview_response_matches(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                    preview.scene_percent,
                );
                if current_session && (!source_matches || !current_source_matches) {
                    self.invalidate_preview();
                } else if current_session && source_matches && request_matches {
                    self.preview.decode_in_flight = false;
                    self.preview.in_flight_scene_index = None;
                    if self.preview.preview_texture.is_none() {
                        self.preview.preview_failed = true;
                        self.worker.stop_preview();
                    }
                }
                WorkerPollResult::Stale
            }
            _ => unreachable!("video preview dispatcher only forwards video preview messages"),
        }
    }

    fn handle_animated_preview_message(
        &mut self,
        msg: WorkerMsg,
        ctx: &egui::Context,
    ) -> WorkerPollResult {
        match msg {
            WorkerMsg::AnimatedPreviewReady(preview) => {
                let _delay_ms = preview.delay_ms;
                let current_session = self.preview_response_is_current_session(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                );
                let source_matches = self.preview_source_matches_revision(
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                );
                let request_matches = self.animated_preview_response_matches(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                    preview.scrub_bucket,
                );
                if current_session && source_matches {
                    self.preview.animated_abandon_bucket_mask &= !preview.abandon_ack_bucket_mask;
                }
                let newer_frame = self
                    .preview
                    .animated_last_frame_index
                    .is_none_or(|last| preview.frame_index > last);
                if !current_session
                    || !source_matches
                    || !request_matches
                    || (preview.scrub_bucket.is_none() && !newer_frame)
                {
                    if current_session && !source_matches {
                        self.invalidate_preview();
                    } else if current_session
                        && source_matches
                        && (self.preview.animated_in_flight_target_bucket == preview.scrub_bucket
                            || (preview.scrub_bucket.is_some()
                                && self.preview.animated_last_submitted_target_bucket
                                    == preview.scrub_bucket))
                    {
                        self.clear_animated_request_if_matches(preview.scrub_bucket);
                    }
                    return WorkerPollResult::StaleAndContinue;
                }
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [preview.width as usize, preview.height as usize],
                    &preview.pixels,
                );
                let texture = ctx.load_texture(
                    format!(
                        "animated-preview-{}-{}-{}",
                        preview.session_id,
                        preview.scrub_bucket.unwrap_or(u16::MAX),
                        preview.frame_index
                    ),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.preview.preview_texture = Some(texture);
                self.clear_animated_request_if_matches(preview.scrub_bucket);
                if preview.scrub_bucket.is_some() {
                    self.preview.animated_display_target_bucket = preview.scrub_bucket;
                    self.preview.animated_scrub_failed = false;
                } else {
                    self.preview.animated_display_target_bucket = None;
                    self.preview.animated_last_frame_index = Some(preview.frame_index);
                }
                ctx.request_repaint();
                WorkerPollResult::Received
            }
            WorkerMsg::AnimatedPreviewFailed(preview)
            | WorkerMsg::AnimatedPreviewStale(preview) => {
                let _frame_index = preview.frame_index;
                let current_session = self.preview_response_is_current_session(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                );
                let source_matches = self.preview_source_matches_revision(
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                );
                let request_matches = self.animated_preview_response_matches(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                    preview.scrub_bucket,
                );
                if current_session && source_matches {
                    self.preview.animated_abandon_bucket_mask &= !preview.abandon_ack_bucket_mask;
                }
                if current_session && !source_matches {
                    self.invalidate_preview();
                } else if current_session && source_matches && request_matches {
                    self.clear_animated_request_if_matches(preview.scrub_bucket);
                    if preview.abandon_ack {
                        // The abandon acknowledgement only releases the request
                        // state; it is not a scrub failure.
                    } else if preview.scrub_bucket.is_some() {
                        self.preview.animated_scrub_failed = true;
                    } else if self.preview.animated_started {
                        self.preview.animated_started = false;
                        self.preview.animated_failed = true;
                        self.worker.stop_preview();
                    }
                } else if current_session
                    && source_matches
                    && (self.preview.animated_in_flight_target_bucket == preview.scrub_bucket
                        || (preview.scrub_bucket.is_some()
                            && self.preview.animated_last_submitted_target_bucket
                                == preview.scrub_bucket))
                {
                    self.clear_animated_request_if_matches(preview.scrub_bucket);
                }
                WorkerPollResult::Stale
            }
            WorkerMsg::AnimatedPreviewUnavailable(preview) => {
                let current_session = self.preview_response_is_current_session(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                );
                let source_matches = self.preview_source_matches_revision(
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                );
                let request_matches = self.animated_preview_response_matches(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                    preview.scrub_bucket,
                );
                if current_session && source_matches {
                    self.preview.animated_abandon_bucket_mask &= !preview.abandon_ack_bucket_mask;
                }
                if current_session && !source_matches {
                    self.invalidate_preview();
                } else if current_session && source_matches && request_matches {
                    self.clear_animated_request_if_matches(preview.scrub_bucket);
                    self.preview.animated_started = false;
                    self.preview.animated_unavailable = true;
                }
                WorkerPollResult::Stale
            }
            _ => {
                unreachable!("animated preview dispatcher only forwards animated preview messages")
            }
        }
    }

    fn handle_static_preview_message(
        &mut self,
        msg: WorkerMsg,
        ctx: &egui::Context,
    ) -> WorkerPollResult {
        match msg {
            WorkerMsg::StaticPreviewReady(preview) => {
                let current_session = self.preview_response_is_current_session(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                );
                let request_matches = self.static_preview_response_matches(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                    preview.page_index,
                );
                let source_matches = self.preview_source_matches_revision(
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                );
                if !request_matches || !source_matches {
                    if current_session && !source_matches {
                        self.invalidate_preview();
                    }
                    return WorkerPollResult::StaleAndContinue;
                }
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [preview.width as usize, preview.height as usize],
                    &preview.pixels,
                );
                let texture = ctx.load_texture(
                    format!(
                        "static-preview-{}-{}",
                        preview.session_id, preview.page_index
                    ),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.preview.preview_texture = Some(texture);
                self.preview.decode_in_flight = false;
                self.preview.static_in_flight_page_index = None;
                self.preview.static_display_page_index = Some(preview.page_index);
                self.preview.static_failed = false;
                ctx.request_repaint();
                WorkerPollResult::Received
            }
            WorkerMsg::StaticPreviewFailed(preview) | WorkerMsg::StaticPreviewStale(preview) => {
                let current_session = self.preview_response_is_current_session(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                );
                let request_matches = self.static_preview_response_matches(
                    preview.session_id,
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                    preview.page_index,
                );
                let source_matches = self.preview_source_matches_revision(
                    &preview.book_id,
                    preview.path.as_ref(),
                    preview.expected_size,
                    preview.expected_modified,
                );
                if current_session && !source_matches {
                    self.invalidate_preview();
                } else if request_matches && source_matches {
                    let has_displayed_static_preview = self.preview.preview_texture.is_some()
                        && self.preview.static_display_page_index.is_some();
                    self.preview.decode_in_flight = false;
                    self.preview.static_in_flight_page_index = None;
                    self.preview.static_failed = true;
                    if !has_displayed_static_preview {
                        self.preview.preview_texture = None;
                        self.preview.static_display_page_index = None;
                    }
                    self.worker.stop_preview();
                }
                WorkerPollResult::Stale
            }
            _ => unreachable!("static preview dispatcher only forwards static preview messages"),
        }
    }

    fn handle_video_thumbnail_message(
        &mut self,
        msg: WorkerMsg,
        ctx: &egui::Context,
    ) -> WorkerPollResult {
        match msg {
            WorkerMsg::VideoReady(video) => {
                let id = video.ready.book_id.clone();
                let current_state = self.video_states.get(&id);
                let revision_matches = current_state.is_some_and(|state| {
                    state.requested_size == Some(video.expected_size)
                        && state.requested_modified == video.expected_modified
                        && state.requested_generation == Some(video.generation)
                });
                let source_matches = self.video_source_matches_revision(
                    &id,
                    video.expected_size,
                    video.expected_modified,
                );
                if !revision_matches || !source_matches {
                    if revision_matches {
                        if let Some(state) = self.video_states.get_mut(&id) {
                            state.thumb_requested = false;
                            state.requested_size = None;
                            state.requested_modified = None;
                            state.requested_generation = None;
                        }
                    }
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        expected_size = video.expected_size,
                        "poll_worker: stale video thumbnail result dropped"
                    );
                    return WorkerPollResult::StaleAndContinue;
                }

                let resp = video.ready;
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [resp.width as usize, resp.height as usize],
                    &resp.pixels,
                );
                let handle = ctx.load_texture(
                    format!("video-{}", id.0.to_hex()),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                let state = self.video_state_mut(&id);
                state.thumb_ready = true;
                state.thumb_failed = false;
                state.thumb_requested = false;
                state.requested_size = None;
                state.requested_modified = None;
                state.requested_generation = None;
                state.texture_size = Some([resp.width as usize, resp.height as usize]);
                state.texture = Some(handle);
                ctx.request_repaint();
                WorkerPollResult::Received
            }
            WorkerMsg::VideoStale {
                book_id,
                expected_size,
                expected_modified,
                generation,
            } => {
                if let Some(state) = self.video_states.get_mut(&book_id) {
                    if state.requested_size == Some(expected_size)
                        && state.requested_modified == expected_modified
                        && state.requested_generation == Some(generation)
                    {
                        state.thumb_requested = false;
                        state.requested_size = None;
                        state.requested_modified = None;
                        state.requested_generation = None;
                    }
                }
                WorkerPollResult::Stale
            }
            _ => unreachable!("video thumbnail dispatcher only forwards video thumbnail messages"),
        }
    }

    fn handle_thumbnail_ready(
        &mut self,
        resp: crate::infra::worker::thumb_worker::ReadyThumb,
        ctx: &egui::Context,
    ) -> WorkerPollResult {
        if !self.thumbnail_source_matches_revision(
            &resp.book_id,
            resp.expected_size,
            resp.expected_modified,
        ) {
            return WorkerPollResult::StaleAndContinue;
        }
        let is_video = self.is_video_registered(&resp.book_id);
        if !is_video && !self.is_registered(&resp.book_id) {
            return WorkerPollResult::IgnoredAndContinue;
        }
        let img = egui::ColorImage::from_rgba_unmultiplied(
            [resp.width as usize, resp.height as usize],
            &resp.pixels,
        );
        let texture_id = if is_video {
            format!("video-{}", resp.book_id.0.to_hex())
        } else {
            resp.book_id.0.to_hex().to_string()
        };
        let handle = ctx.load_texture(texture_id, img, egui::TextureOptions::LINEAR);
        if is_video {
            let state = self.video_state_mut(&resp.book_id);
            state.thumb_ready = true;
            state.thumb_failed = false;
            state.thumb_requested = false;
            state.requested_size = None;
            state.requested_modified = None;
            state.requested_generation = None;
            state.texture_size = Some([resp.width as usize, resp.height as usize]);
            state.texture = Some(handle);
        } else {
            let state = self.book_state_mut(&resp.book_id);
            state.thumb_ready = true;
            state.thumb_failed = false;
            state.thumb_requested = false;
            state.texture_size = Some([resp.width as usize, resp.height as usize]);
            state.texture = Some(handle);
        }
        ctx.request_repaint();
        WorkerPollResult::Received
    }

    fn handle_thumbnail_failure_or_stale(
        &mut self,
        msg: WorkerMsg,
        ctx: &egui::Context,
    ) -> WorkerPollResult {
        let (id, expected_size, expected_modified) = match msg {
            WorkerMsg::Failed(_) | WorkerMsg::FailedPermanent(_) => {
                // Revisionなしの内部結果はUIへ適用しない。revision付き通知だけを反映する。
                return WorkerPollResult::Stale;
            }
            WorkerMsg::FailedWithRevision {
                book_id,
                expected_size,
                expected_modified,
            }
            | WorkerMsg::FailedPermanentWithRevision {
                book_id,
                expected_size,
                expected_modified,
            } => (book_id, expected_size, expected_modified),
            WorkerMsg::Stale(_id) => {
                // 同じ path/id のファイル差し替え前に開始された古いタスク。
                // 新しいタスク側で再生成されるため、状態を変更しない。
                return WorkerPollResult::Stale;
            }
            _ => {
                unreachable!("failure dispatcher only forwards thumbnail failure or stale messages")
            }
        };
        let source_matches = if self.is_video_registered(&id) {
            self.video_source_matches_revision(&id, expected_size, expected_modified)
        } else {
            self.thumbnail_source_matches_revision(&id, expected_size, expected_modified)
        };
        if !source_matches {
            return WorkerPollResult::StaleAndContinue;
        }
        if let Some(state) = self.book_states.get_mut(&id) {
            state.thumb_requested = false;
        }
        if let Some(state) = self.video_states.get_mut(&id) {
            state.thumb_requested = false;
            state.requested_size = None;
            state.requested_modified = None;
            state.requested_generation = None;
        }
        if !self.is_registered(&id) && !self.is_video_registered(&id) {
            return WorkerPollResult::IgnoredAndContinue;
        }
        let id_hex = id.0.to_hex();
        tracing::debug!(
            id = &id_hex[..8],
            "poll_worker: thumbnail failed permanently"
        );
        // OK→NG の入れ替えでは古い成功サムネイルが残りうる。
        // Failed 状態を必ず反映するため、ここで落とす。
        if self.is_video_registered(&id) {
            let state = self.video_state_mut(&id);
            state.texture = None;
            state.texture_size = None;
            state.thumb_ready = false;
            state.thumb_failed = true;
        } else {
            let state = self.book_state_mut(&id);
            state.texture = None;
            state.texture_size = None;
            state.thumb_ready = false;
            state.thumb_failed = true;
        }
        ctx.request_repaint();
        WorkerPollResult::Failed
    }

    // ── サムネイル一括要求 ────────────────────────────────────────────────────

    // raw_entries 全件を背景対象にし、非可視の cache/request 供給を固定レートで進める。
    // 可視範囲はこの上限外の即時経路で処理する。

    fn request_all_thumbs(&mut self) {
        self.reset_background_artifact_pump();
        for entry in &self.raw_entries {
            match entry {
                LibraryEntry::VideoFile(video) => self
                    .background_artifact_targets
                    .push_back(BackgroundArtifactTarget::Video(video.id.clone())),
                _ => {
                    if let Some(book_id) = entry.thumb_id_ref() {
                        self.background_artifact_targets
                            .push_back(BackgroundArtifactTarget::Book(book_id.clone()));
                    }
                }
            }
        }
        self.background_artifact_total = self.background_artifact_targets.len();
        self.background_artifact_completion_logged = self.background_artifact_total == 0;
        log::debug!(
            "[thumb-prefetch] scan start targets={} rate={}/min (~{}/sec)",
            self.background_artifact_total,
            BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE,
            BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE / 60
        );
    }

    fn reset_background_artifact_pump(&mut self) {
        self.background_artifact_targets.clear();
        self.background_artifact_total = 0;
        self.background_artifact_checked = 0;
        self.background_artifact_supplied = 0;
        self.background_artifact_credit = 0.0;
        self.background_artifact_last_refill_at = Instant::now();
        self.background_artifact_worker_generation = self.worker.current_generation();
        self.background_artifact_completion_logged = true;
    }

    fn visible_background_artifact_ids(
        &self,
        visible_range: Option<&std::ops::RangeInclusive<usize>>,
    ) -> HashSet<BookId> {
        visible_range
            .into_iter()
            .flat_map(|range| range.clone())
            .filter_map(|idx| self.entries.get(idx))
            .filter_map(|entry| match entry {
                LibraryEntry::VideoFile(video) => Some(video.id.clone()),
                _ => entry.thumb_id_ref().cloned(),
            })
            .collect()
    }

    fn pump_background_artifacts(
        &mut self,
        visible_range: Option<&std::ops::RangeInclusive<usize>>,
    ) {
        let current_generation = self.worker.current_generation();
        if current_generation != self.background_artifact_worker_generation {
            // clear_pending_tasks() invalidates the worker generation. Any UI-side
            // targets planned for the old directory must be discarded as well.
            self.reset_background_artifact_pump();
            return;
        }

        if self.background_artifact_targets.is_empty() {
            return;
        }

        let now = Instant::now();
        let elapsed = now
            .saturating_duration_since(self.background_artifact_last_refill_at)
            .as_secs_f64();
        let rate_per_second = BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE as f64 / 60.0;
        // Keep a small bounded credit so an idle UI frame cannot turn into an
        // unbounded burst. At normal 80 ms polling this supplies about four
        // entries per frame, i.e. 50 entries/sec.
        self.background_artifact_credit =
            (self.background_artifact_credit + elapsed * rate_per_second).min(8.0);
        self.background_artifact_last_refill_at = now;
        let budget = self.background_artifact_credit.floor() as usize;
        if budget == 0 {
            return;
        }

        let visible_ids = self.visible_background_artifact_ids(visible_range);
        let target_width = crate::domain::app_settings::AppSettings::storage_width();
        let mut processed = 0usize;
        let mut supplied = 0usize;

        for _ in 0..budget {
            let Some(target) = self.background_artifact_targets.pop_front() else {
                break;
            };
            processed += 1;
            self.background_artifact_checked += 1;

            let id = match &target {
                BackgroundArtifactTarget::Book(id) | BackgroundArtifactTarget::Video(id) => id,
            };
            if visible_ids.contains(id) {
                // The visible mailbox/direct request path owns this entry and is
                // deliberately outside the background rate limit.
                continue;
            }

            match target {
                BackgroundArtifactTarget::Book(book_id) => {
                    let Some(snapshot) = self.source_snapshots.get(&book_id).cloned() else {
                        continue;
                    };
                    let Some(state) = self.book_states.get(&book_id) else {
                        continue;
                    };
                    if state.thumb_failed || state.thumb_requested {
                        continue;
                    }
                    let bypass_cache = state.force_reload;
                    if state.texture.is_some() && !bypass_cache {
                        continue;
                    }
                    let task = ThumbTask {
                        book_id: book_id.clone(),
                        path: Arc::clone(&snapshot.path),
                        target_width,
                        expected_size: snapshot.size,
                        expected_modified: snapshot.modified,
                        bypass_cache,
                    };
                    let cache_hit = !bypass_cache
                        && self.thumb_cache.as_ref().is_some_and(|cache| {
                            cache.has_thumb(&book_id, snapshot.size, snapshot.modified)
                        });
                    if cache_hit {
                        // A thumbnail cache hit still supplies the existing
                        // Page Map request path at this same entry rate.
                        self.worker.request_page_map(task);
                    } else {
                        let state = self.book_state_mut(&book_id);
                        state.thumb_requested = true;
                        if bypass_cache {
                            state.thumb_ready = false;
                        }
                        state.force_reload = false;
                        self.worker.request(task);
                    }
                    supplied += 1;
                }
                BackgroundArtifactTarget::Video(video_id) => {
                    let Some(snapshot) = self.source_snapshots.get(&video_id).cloned() else {
                        continue;
                    };
                    let Some(state) = self.video_states.get(&video_id) else {
                        continue;
                    };
                    if state.thumb_failed || state.thumb_requested || state.texture.is_some() {
                        continue;
                    }
                    let cache_hit = self.thumb_cache.as_ref().is_some_and(|cache| {
                        cache.has_thumb(&video_id, snapshot.size, snapshot.modified)
                    });
                    if cache_hit {
                        continue;
                    }
                    let request_generation = self.worker.current_generation();
                    let state = self.video_state_mut(&video_id);
                    state.thumb_requested = true;
                    state.requested_size = Some(snapshot.size);
                    state.requested_modified = snapshot.modified;
                    state.requested_generation = Some(request_generation);
                    self.worker.request_video(VideoThumbTask {
                        book_id: video_id,
                        path: Arc::clone(&snapshot.path),
                        target_width,
                        expected_size: snapshot.size,
                        expected_modified: snapshot.modified,
                    });
                    supplied += 1;
                }
            }
        }

        self.background_artifact_credit -= processed as f64;
        self.background_artifact_supplied += supplied;
        if self.background_artifact_targets.is_empty()
            && !self.background_artifact_completion_logged
        {
            self.background_artifact_completion_logged = true;
            log::debug!(
                "[thumb-prefetch] scan complete checked={} supplied={} rate={}/min",
                self.background_artifact_checked,
                self.background_artifact_supplied,
                BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE
            );
        }
    }

    fn request_thumb_for_entry(&mut self, entry: &LibraryEntry, bypass_cache: bool) {
        if let LibraryEntry::VideoFile(video) = entry {
            self.request_video_thumb_for_entry(video);
            return;
        }
        let Some(book_id) = entry.thumb_id_ref() else {
            return;
        };
        let Some(snapshot) = self.source_snapshots.get(book_id) else {
            return;
        };
        let path = Arc::clone(&snapshot.path);
        let expected_size = snapshot.size;
        let expected_modified = snapshot.modified;
        let target_width = crate::domain::app_settings::AppSettings::storage_width();
        let state = self.book_state_mut(book_id);
        let should_bypass_cache = bypass_cache || state.force_reload;
        if state.texture.is_some() && !should_bypass_cache {
            return;
        }
        if state.thumb_requested {
            return;
        }
        if should_bypass_cache {
            state.thumb_ready = false;
        }
        state.thumb_failed = false;
        state.thumb_requested = true;
        state.force_reload = false;
        self.worker.request(ThumbTask {
            book_id: book_id.clone(),
            path,
            target_width,
            expected_size,
            expected_modified,
            bypass_cache: should_bypass_cache,
        });
    }

    fn request_video_thumb_for_entry(&mut self, entry: &crate::domain::archive::VideoFileMeta) {
        let Some(snapshot) = self.source_snapshots.get(&entry.id) else {
            return;
        };
        let path = Arc::clone(&snapshot.path);
        let expected_size = snapshot.size;
        let expected_modified = snapshot.modified;
        let request_generation = self.worker.current_generation();
        let state = self.video_state_mut(&entry.id);
        if state.texture.is_some() || state.thumb_requested {
            return;
        }
        state.thumb_requested = true;
        state.requested_size = Some(expected_size);
        state.requested_modified = expected_modified;
        state.requested_generation = Some(request_generation);
        self.worker.request_video(VideoThumbTask {
            book_id: entry.id.clone(),
            path,
            target_width: crate::domain::app_settings::AppSettings::storage_width(),
            expected_size,
            expected_modified,
        });
    }

    fn estimate_texture_bytes_for_entry(&self, entry: &LibraryEntry) -> usize {
        let Some(book_id) = entry.thumb_id_ref() else {
            return 0;
        };
        let Some(state) = self.book_state(book_id) else {
            return 0;
        };
        if state.texture.is_none() {
            return 0;
        }
        let Some([width, height]) = state.texture_size else {
            return 0;
        };
        width
            .saturating_mul(height)
            .saturating_mul(RGBA_BYTES_PER_PIXEL)
    }

    fn compute_texture_keep_indices_by_budget(
        &self,
        visible_range: std::ops::RangeInclusive<usize>,
    ) -> std::collections::HashSet<usize> {
        let mut keep_indices = std::collections::HashSet::new();
        let visible_start = *visible_range.start();
        let visible_end = *visible_range.end();
        let mut visible_bytes = 0usize;

        for idx in visible_range.clone() {
            keep_indices.insert(idx);
            if let Some(entry) = self.entries.get(idx) {
                visible_bytes =
                    visible_bytes.saturating_add(self.estimate_texture_bytes_for_entry(entry));
            }
        }

        let mut remaining_budget =
            LIBRARY_THUMB_TEXTURE_KEEP_MAX_BYTES.saturating_sub(visible_bytes);
        let mut before = visible_start.checked_sub(1);
        let mut after = visible_end
            .checked_add(1)
            .filter(|idx| *idx < self.entries.len());
        let mut prefer_before = true;

        while before.is_some() || after.is_some() {
            let candidate = if prefer_before {
                before
                    .take()
                    .map(|idx| (idx, true))
                    .or_else(|| after.take().map(|idx| (idx, false)))
            } else {
                after
                    .take()
                    .map(|idx| (idx, false))
                    .or_else(|| before.take().map(|idx| (idx, true)))
            };
            let Some((idx, was_before)) = candidate else {
                break;
            };
            let estimated_bytes = self
                .entries
                .get(idx)
                .map(|entry| self.estimate_texture_bytes_for_entry(entry))
                .unwrap_or(0);
            if estimated_bytes <= remaining_budget {
                keep_indices.insert(idx);
                remaining_budget = remaining_budget.saturating_sub(estimated_bytes);
            }
            if was_before {
                before = idx.checked_sub(1);
            } else {
                after = idx.checked_add(1).filter(|next| *next < self.entries.len());
            }
            prefer_before = !prefer_before;
        }

        keep_indices
    }

    fn evict_thumb_textures_outside_keep_indices(
        &mut self,
        keep_indices: &std::collections::HashSet<usize>,
    ) {
        for (idx, entry) in self.entries.iter().enumerate() {
            if keep_indices.contains(&idx) {
                continue;
            }
            let Some(book_id) = entry.thumb_id_ref() else {
                continue;
            };
            if let Some(state) = self.book_states.get_mut(book_id) {
                state.texture = None;
                state.texture_size = None;
            }
        }
    }

    fn clear_display_tasks_if_needed(&mut self) {
        if self
            .last_display_set
            .as_ref()
            .is_some_and(|display_keys| display_keys.is_empty())
        {
            return;
        }
        self.last_display_set = Some(Vec::new());
        self.worker.replace_display_tasks(Vec::new());
    }

    fn ensure_visible_thumb_textures(&mut self, visible_range: &std::ops::RangeInclusive<usize>) {
        let visible_entries: Vec<LibraryEntry> = visible_range
            .clone()
            .filter_map(|idx| self.entries.get(idx).cloned())
            .collect();
        let mut display_tasks = Vec::new();
        let mut display_keys = Vec::new();
        for entry in &visible_entries {
            match entry {
                LibraryEntry::VideoFile(video) => {
                    let should_request = match self.video_state(&video.id) {
                        None => true,
                        Some(state) => {
                            state.texture.is_none() && !state.thumb_failed && !state.thumb_requested
                        }
                    };
                    if should_request {
                        self.request_thumb_for_entry(entry, false);
                    }
                }
                _ => {
                    if let Some(task) = self.display_thumb_task_for_entry(entry) {
                        display_keys.push(DisplayThumbKey::from(&task));
                        display_tasks.push(task);
                    }
                }
            }
        }
        let should_replace_display_tasks = self.last_display_set.as_ref() != Some(&display_keys);
        self.last_display_set = Some(display_keys);
        if should_replace_display_tasks {
            self.worker.replace_display_tasks(display_tasks);
        }
    }

    fn display_thumb_task_for_entry(&self, entry: &LibraryEntry) -> Option<ThumbTask> {
        let book_id = entry.thumb_id_ref()?;
        let snapshot = self.source_snapshots.get(book_id)?;
        let path = Arc::clone(&snapshot.path);
        let expected_size = snapshot.size;
        let expected_modified = snapshot.modified;
        let target_width = crate::domain::app_settings::AppSettings::storage_width();
        let state = self.book_state(book_id)?;
        if state.texture.is_some() || state.thumb_failed {
            return None;
        }
        let bypass_cache = state.force_reload;
        Some(ThumbTask {
            book_id: book_id.clone(),
            path,
            target_width,
            expected_size,
            expected_modified,
            bypass_cache,
        })
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

fn video_preview_scene_count() -> u64 {
    (VIDEO_PREVIEW_FIRST_SCENE_PERCENT..=VIDEO_PREVIEW_LAST_SCENE_PERCENT)
        .step_by(VIDEO_PREVIEW_STEP_PERCENT as usize)
        .count() as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnimatedScrubRequestAction {
    CancelPending,
    Noop,
    Submit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnimatedScrubCancelAction {
    Clear,
    RetainActive,
    Retain,
}

fn animated_scrub_cancel_action(
    cancelled: bool,
    decode_in_flight: bool,
) -> AnimatedScrubCancelAction {
    if cancelled {
        AnimatedScrubCancelAction::Clear
    } else if decode_in_flight {
        AnimatedScrubCancelAction::RetainActive
    } else {
        AnimatedScrubCancelAction::Retain
    }
}

fn animated_scrub_bucket_bit(bucket: u16) -> u64 {
    1u64.checked_shl(u32::from(bucket)).unwrap_or(0)
}

fn animated_scrub_abandon_mask_for_cancel(
    current_mask: u64,
    decode_in_flight: bool,
    request_bucket: u16,
) -> u64 {
    if decode_in_flight {
        current_mask | animated_scrub_bucket_bit(request_bucket)
    } else {
        current_mask
    }
}

fn animated_scrub_abandon_mask_for_submit(
    current_mask: u64,
    decode_in_flight: bool,
    in_flight_bucket: Option<u16>,
    requested_bucket: u16,
) -> u64 {
    match in_flight_bucket {
        Some(bucket) if decode_in_flight && bucket != requested_bucket => {
            current_mask | animated_scrub_bucket_bit(bucket)
        }
        _ => current_mask,
    }
}

fn animated_scrub_request_action(
    displayed_bucket: Option<u16>,
    in_flight_bucket: Option<u16>,
    last_submitted_bucket: Option<u16>,
    target_bucket: u16,
) -> AnimatedScrubRequestAction {
    if displayed_bucket == Some(target_bucket) {
        AnimatedScrubRequestAction::CancelPending
    } else if in_flight_bucket == Some(target_bucket)
        || last_submitted_bucket == Some(target_bucket)
    {
        AnimatedScrubRequestAction::Noop
    } else {
        AnimatedScrubRequestAction::Submit
    }
}

pub(crate) fn animated_preview_target_bucket_from_normalized_x(normalized_x: f32) -> u16 {
    let bucket_count = ANIMATED_PREVIEW_SCRUB_BUCKET_COUNT.max(1);
    let normalized_x = if normalized_x.is_finite() {
        normalized_x.clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((normalized_x * bucket_count as f32).floor() as u16).min(bucket_count - 1)
}

pub(crate) fn video_preview_scene_index_from_normalized_x(normalized_x: f32) -> u64 {
    let count = video_preview_scene_count();
    if count == 0 {
        return 0;
    }
    let normalized_x = normalized_x.clamp(0.0, 1.0);
    ((normalized_x * count as f32).floor() as u64).min(count - 1)
}

pub(crate) fn static_preview_page_index_from_normalized_x(
    normalized_x: f32,
    page_count: usize,
) -> Option<u32> {
    if page_count == 0 {
        return None;
    }
    let max_page_index = page_count.saturating_sub(1).min(u32::MAX as usize) as u32;
    let normalized_x = if normalized_x.is_finite() {
        normalized_x.clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((normalized_x * max_page_index as f32).round() as u32)
}

fn video_preview_scene_index(scene_percent: u8) -> u64 {
    let count = video_preview_scene_count();
    if count == 0 {
        return 0;
    }
    (scene_percent.saturating_sub(VIDEO_PREVIEW_FIRST_SCENE_PERCENT) / VIDEO_PREVIEW_STEP_PERCENT)
        .min((count - 1) as u8) as u64
}

fn video_preview_scene_interval() -> Duration {
    Duration::from_secs_f64(VIDEO_PREVIEW_CYCLE.as_secs_f64() / video_preview_scene_count() as f64)
}

fn video_preview_scene_percent(sequence: u64) -> u8 {
    let index = sequence % video_preview_scene_count();
    VIDEO_PREVIEW_FIRST_SCENE_PERCENT
        .saturating_add((index as u8).saturating_mul(VIDEO_PREVIEW_STEP_PERCENT))
}

fn video_preview_scene_sequence_at(
    timeline_start: Instant,
    now: Instant,
    interval: Duration,
) -> u64 {
    if now <= timeline_start || interval.is_zero() {
        return 0;
    }
    (now.duration_since(timeline_start).as_secs_f64() / interval.as_secs_f64()).floor() as u64
}

fn system_time_to_unix_secs(time: SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

// ── ライブラリパネル描画 ──────────────────────────────────────────────────────

/// ライブラリパネルを描画する。アクションを返す（Open/Rename/Delete/Copy）。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut LibraryState,
    language: UiLanguage,
    interaction_blocked: bool,
    preview_target_width: u16,
    folder_book_open_as_viewer: bool,
    external_tools: &[ExternalToolMenuItem],
    external_tool_busy: bool,
) -> LibraryAction {
    // フィルタ / ソートが変更されていれば再構築
    if state.filter_dirty {
        state.rebuild_entries();
    }

    if state.show_empty_library_message(ui, language) {
        state.clear_display_tasks_if_needed();
        if state.preview.target.is_some() {
            state.invalidate_preview();
        }
        state.pump_background_artifacts(None);
        if !state.background_artifact_targets.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(20));
        }
        return LibraryAction::None;
    }

    // スクロール復元・追従
    let scroll_selected_into_view = std::mem::take(&mut state.scroll_selected_into_view_pending);

    let restore_scroll = if state.scroll_restore_pending {
        state.scroll_restore_pending = false;
        Some(state.initial_scroll_y)
    } else {
        state.scroll_to_pending.take()
    };

    let preview_scroll_changed = state
        .preview
        .preview_scroll_y
        .is_some_and(|preview_scroll_y| (preview_scroll_y - state.scroll_y).abs() > 0.5);
    let preview_source_changed = state.preview.target.as_ref().is_some_and(|target| {
        !state.preview_snapshot_matches_revision(
            &target.book_id,
            target.path.as_ref(),
            target.size,
            target.modified,
        )
    });
    let scroll_input_active = ui.input(|input| input.smooth_scroll_delta.y.abs() > f32::EPSILON);
    if state.preview.target.is_some()
        && (restore_scroll.is_some()
            || scroll_input_active
            || preview_scroll_changed
            || preview_source_changed)
    {
        state.invalidate_preview();
    }

    let thumb_size = egui::vec2(state.thumb_w, state.thumb_h);

    // グリッド描画
    let reset_cache = state.reset_context_menu_cache;
    state.reset_context_menu_cache = false;
    let result = virtual_grid::show_grid(
        ui,
        virtual_grid::GridViewContext {
            entries: &state.entries,
            book_states: &state.book_states,
            video_states: &state.video_states,
            resolve_open_action: &|idx| state.resolve_open_action(idx, folder_book_open_as_viewer),
            preview_texture: state.preview_for_grid(),
            selected_idx: state.selected_idx,
            selected_set: &state.selected_set,
            is_favorite: &|entry| state.is_favorite_entry(entry),
            reading_hud_state: &|entry| state.reading_hud_state_for_entry(entry),
            has_page_map_failure: &|entry| state.has_page_map_failure_for_entry(entry),
            static_page_count: &|entry| state.static_page_count_for_entry(entry),
            interaction_enabled: !interaction_blocked,
            external_tools,
            external_tool_busy,
            language,
        },
        virtual_grid::GridViewConfig {
            restore_scroll,
            scroll_selected_into_view,
            thumb_size,
            wheel_scroll_multiplier: state.wheel_scroll_multiplier,
            hud_mode: state.hud_mode,
            hud_style: state.hud_style,
            selection_style: state.selection_style,
            hud_font_size: state.hud_font_size,
            reset_context_menu_cache: reset_cache,
        },
    );

    state.update_preview(
        ui.ctx(),
        result.hovered_preview.clone(),
        preview_target_width,
    );

    if let Some(visible_range) = result.visible_range.clone() {
        state.refresh_visible_page_map_failures(visible_range.clone(), ui.ctx());
        let keep_indices = state.compute_texture_keep_indices_by_budget(visible_range.clone());
        state.evict_thumb_textures_outside_keep_indices(&keep_indices);
        state.ensure_visible_thumb_textures(&visible_range);
        state.pump_background_artifacts(Some(&visible_range));
        if !state.background_artifact_targets.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(20));
        }
    } else {
        state.clear_display_tasks_if_needed();
        state.pump_background_artifacts(None);
        if !state.background_artifact_targets.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(20));
        }
    }

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
        let resolved_action = state.resolve_context_action(idx, action, folder_book_open_as_viewer);
        if let Some(action) = resolved_action {
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
        return state.resolve_open_action(idx, folder_book_open_as_viewer);
    }

    if let Some(delta) = result.thumb_size_delta {
        return LibraryAction::ThumbDisplaySizeChanged(delta);
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
