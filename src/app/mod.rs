//! アプリルート。
//!
//! `eframe::App` 実装と、Library / Viewer / 設定 UI の接続を持つ。

use std::collections::HashMap;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use eframe::egui::{self, Key, PointerButton};
use parking_lot::RwLock;

use self::external_tool::{ExternalToolRunning, ExternalToolUiState};
use self::library_ops::PendingAfterLoad;
use self::platform::{normalize_dir_path, sanitize_favorite_dirs};
use self::ui_helpers::{calc_cache_size_mb, dialog_button_row, setup_style, DialogButtonSpec};
use self::viewer_ops::{
    FavoriteToggleResult, LibraryNavSnapshot, RebuildSelectedImagesAsCbzAndNextResult,
    ViewerSyncEvent,
};
use crate::domain::app_settings::AppSettings;
use crate::domain::archive::{BookId, BookMeta, CbzRebuildPlanOptions, LibraryEntry};
use crate::domain::performance::PerformanceResources;
use crate::infra::cache::{disk::DiskCache, page_map::PageMapDiskCache};
use crate::infra::worker::external_tool_worker::ExternalToolWorker;
use crate::session::{sort_key_to_str, sort_order_to_str, LeftPaneTab, SessionState};
use crate::ui::{
    i18n::{tr, TextKey},
    icons,
    library::{self, LibraryAction, LibraryState},
    settings::{self, SettingsEvent},
    sidebar, theme, topbar,
    viewer::ExternalToolTrigger,
};
use crate::util::path_eq::normalize_path_for_selection;
use crate::LaunchOptions;

mod external_tool;
mod file_ops;
mod library_ops;
mod platform;
mod ui_helpers;
pub mod viewer_app;
mod viewer_ops;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeleteDialogChoice {
    Ok,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BookSettingsClearDialogChoice {
    Reset,
    Cancel,
}

#[derive(Clone, Debug)]
pub(super) struct RebuiltCbzLibrarySyncResult {
    pub rebuilt_path: PathBuf,
}

#[derive(Clone, Debug)]
struct EntryProperties {
    name: String,
    path: String,
    kind: String,
    size_bytes: Option<u64>,
    modified: Option<SystemTime>,
    page_count: Option<u32>,
}

const ENTRY_PROPERTIES_DIALOG_W: f32 = 500.0;
const ENTRY_PROPERTY_LABEL_W: f32 = 72.0;
const ENTRY_PROPERTY_VALUE_W: f32 = 280.0;
// Keep rendered Label lines safely narrower than the fixed value cell.
// A natural-width egui Label can otherwise request a little more width than
// the measured text width and push the following action cell.
const ENTRY_PROPERTY_TEXT_SAFE_MARGIN: f32 = 36.0;
const ENTRY_PROPERTY_ACTION_W: f32 = 74.0;
const ENTRY_PROPERTY_CELL_GAP: f32 = 8.0;
const ENTRY_PROPERTY_ROW_GAP: f32 = 2.0;
const ENTRY_PROPERTY_MULTILINE_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryPropertyRowHeight {
    Single,
    ThreeLines,
}

#[derive(Clone, Debug)]
struct EntryPropertyRow {
    label: String,
    value: String,
    copy_label: Option<String>,
    height: EntryPropertyRowHeight,
}

pub struct App {
    library: LibraryState,
    favorites: Vec<PathBuf>,
    sidebar_open: bool,
    initial_dir: Option<PathBuf>,

    /// アプリ全体設定（サムネサイズ等）
    app_settings: AppSettings,
    /// この起動時に検出した PC 資源情報
    performance_resources: PerformanceResources,
    /// 設定ウィンドウ表示フラグ
    settings_open: bool,
    /// キャッシュ使用量（MB）。-1.0 = 未計算
    cache_size_mb: f32,

    /// 起動時に選択を復元するファイルパス
    pending_select: Option<PathBuf>,
    /// 外部ファイルドロップ後に、ロード完了へ持ち越す選択対象
    pending_drop_select: Option<PathBuf>,
    /// 非同期ライブラリロード完了後に適用する選択・スクロール復元
    pending_after_load: Option<PendingAfterLoad>,

    /// セッション保存用ウィンドウ情報（最大化中は更新しない）
    saved_win_pos: Option<[f32; 2]>,
    saved_win_size: Option<[f32; 2]>,

    // ── ファイル操作モーダル状態 ───────────────────────────────────────────
    renaming: Option<(usize, String)>,
    properties_dialog: Option<EntryProperties>,
    deleting: Option<Vec<usize>>,
    delete_dialog_choice: DeleteDialogChoice,
    book_settings_clearing: Option<Vec<usize>>,
    book_settings_clear_dialog_choice: BookSettingsClearDialogChoice,
    // グループ設定ダイアログ
    setting_group_targets: Vec<usize>,
    setting_group_buf: String,
    setting_group_open: bool,
    /// 外部ドラッグ終了後、マウス解放イベントを通常 UI に流さないためのガード
    suppress_pointer_until_release: bool,
    /// 外部ドラッグ直後に自ウィンドウへ返ってくる dropped_files を 1 回だけ無視する
    suppress_next_dropped_files: bool,
    pending_toast: Option<(String, std::time::Instant)>,
    pending_error_dialog: Option<String>,

    viewer_processes: Vec<Child>,
    /// 起動初期フレームのウィンドウ状態観測ログ用カウンタ。
    ui_frame_counter: u64,
    external_tool_worker: ExternalToolWorker,
    external_tool_running: Option<ExternalToolRunning>,
    external_tool_ui_state: ExternalToolUiState,
    external_tool_next_request_id: u64,
    pending_external_tool_runs:
        std::sync::Arc<parking_lot::Mutex<Vec<(usize, PathBuf, ExternalToolTrigger)>>>,
    pending_history_paths: std::sync::Arc<parking_lot::Mutex<Vec<PathBuf>>>,
    pending_viewer_sync_events: std::sync::Arc<parking_lot::Mutex<Vec<ViewerSyncEvent>>>,
    pending_rebuilt_viewer_paths: std::sync::Arc<parking_lot::Mutex<Vec<PathBuf>>>,
    library_book_order: Arc<RwLock<LibraryNavSnapshot>>,
    left_pane_tab: LeftPaneTab,
    open_history: std::collections::VecDeque<crate::session::HistoryEntry>,
    history_thumb_textures: HashMap<String, egui::TextureHandle>,
    sidebar_disk_cache: Option<DiskCache>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        session: SessionState,
        launch: LaunchOptions,
    ) -> Self {
        setup_style(&cc.egui_ctx);

        let performance_resources = crate::infra::system_resources::detect_pc_resources();
        let app_settings = AppSettings::load_with_resources(&performance_resources);
        let performance_settings =
            app_settings.normalized_performance_settings(&performance_resources);
        tracing::debug!(
            "[app.settings.applied] source=app_settings viewer_quality={:?} viewer_l1_vram_cache_max_mb={} viewer_l2_ram_cache_max_mib={} viewer_background_worker_count={}",
            app_settings.viewer_quality,
            performance_settings.l1_vram_cache_max_mib,
            performance_settings.l2_ram_cache_max_mib,
            performance_settings.background_worker_count
        );
        let mut library = LibraryState::new(cc.egui_ctx.clone());
        library.sort_key = session.parse_sort_key();
        library.sort_order = session.parse_sort_order();
        library.initial_scroll_y = session.grid_scroll_y;
        library.scroll_restore_pending = true;
        library.filter.keyword = session.filter_text.clone();
        library.thumb_w = app_settings.thumb_w();
        library.thumb_h = app_settings.thumb_h();
        library.wheel_scroll_multiplier = app_settings.library_wheel_multiplier();
        library.hud_mode = app_settings.library_hud_mode;
        library.hud_style = app_settings.library_hud_style;
        library.selection_style = app_settings.library_card_selection_style;
        library.hud_font_size = app_settings.library_hud_font_size();

        let initial_dir = launch
            .initial_library_dir
            .or_else(|| {
                session
                    .last_dir
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
            })
            .map(normalize_dir_path);
        let pending_select = launch
            .startup_select_path
            .map(normalize_dir_path)
            .or_else(|| {
                session
                    .selected_path
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .map(normalize_dir_path)
            });

        let favorites = sanitize_favorite_dirs(session.favorite_dirs.iter().map(PathBuf::from));
        tracing::debug!(
            favorites = ?favorites.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            initial_dir = ?initial_dir.as_ref().map(|p| p.display().to_string()),
            "app: restored session state"
        );
        let left_pane_tab = session.left_pane_tab;
        let open_history = session.history;
        let sidebar_disk_cache = DiskCache::open(DiskCache::default_root())
            .or_else(|_| {
                DiskCache::open(
                    std::env::temp_dir()
                        .join(crate::app_identity::app_data_dir())
                        .join("thumbs"),
                )
            })
            .ok();
        Self {
            library,
            favorites,
            sidebar_open: false,
            initial_dir,
            app_settings,
            performance_resources,
            settings_open: false,
            cache_size_mb: -1.0,
            pending_select,
            pending_drop_select: None,
            pending_after_load: None,
            saved_win_pos: None,
            saved_win_size: None,
            renaming: None,
            properties_dialog: None,
            deleting: None,
            delete_dialog_choice: DeleteDialogChoice::Ok,
            book_settings_clearing: None,
            book_settings_clear_dialog_choice: BookSettingsClearDialogChoice::Reset,
            setting_group_targets: Vec::new(),
            setting_group_buf: String::new(),
            setting_group_open: false,
            suppress_pointer_until_release: false,
            suppress_next_dropped_files: false,
            pending_toast: None,
            pending_error_dialog: None,
            viewer_processes: Vec::new(),
            ui_frame_counter: 0,
            external_tool_worker: ExternalToolWorker::spawn(),
            external_tool_running: None,
            external_tool_ui_state: ExternalToolUiState::Idle,
            external_tool_next_request_id: 1,
            pending_external_tool_runs: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            pending_history_paths: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            pending_viewer_sync_events: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            pending_rebuilt_viewer_paths: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
            library_book_order: Arc::new(RwLock::new(LibraryNavSnapshot::default())),
            left_pane_tab,
            open_history,
            history_thumb_textures: HashMap::new(),
            sidebar_disk_cache,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.ui_frame_counter = self.ui_frame_counter.saturating_add(1);
        if self.suppress_pointer_until_release {
            if ctx.input(|i| i.pointer.any_down()) {
                ctx.request_repaint();
            } else {
                self.suppress_pointer_until_release = false;
            }
        }
        if self.suppress_next_dropped_files && ctx.input(|i| i.raw.dropped_files.is_empty()) {
            self.suppress_next_dropped_files = false;
        }
        self.reap_viewer_processes();
        if !self.viewer_processes.is_empty() {
            // IPC 分離後は子プロセス死活監視のためだけに再描画を起こす。
            // 高頻度ポーリングは不要なので 1 秒間隔で回収する。
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        // 外部ツールトリガーは最優先で排出し、遅延を避ける
        self.drain_pending_external_tool_runs(ctx);
        self.drain_pending_history_paths();
        self.drain_pending_viewer_sync_events(ctx);
        self.drain_pending_rebuilt_viewer_paths(ctx);
        if !self.pending_external_tool_runs.lock().is_empty() {
            ctx.request_repaint();
        }

        // ── ウィンドウ情報を毎フレーム保存（最大化中は除く）──────────────────
        let is_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !is_maximized {
            if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
                self.saved_win_pos = Some([outer.min.x, outer.min.y]);
                self.saved_win_size = Some([outer.width(), outer.height()]);
            }
        }

        // ── 起動時の初期スキャン ──────────────────────────────────────────────
        if let Some(dir) = self.initial_dir.take() {
            self.library.start_load_dir_async(normalize_dir_path(dir));
        }
        if self.library.poll_async_load(ctx) {
            self.resolve_pending_select();
            self.apply_pending_after_load();
            self.resolve_pending_drop_select();
        }
        self.poll_external_tool_results(ctx);
        self.schedule_external_tool_state_repaint(ctx);
        self.tick_external_tool_ui_state();
        self.drain_pending_external_tool_runs(ctx);
        self.drain_pending_history_paths();
        self.drain_pending_viewer_sync_events(ctx);
        self.drain_pending_rebuilt_viewer_paths(ctx);
        self.library.wheel_scroll_multiplier = self.app_settings.library_wheel_multiplier();

        // ── 設定ウィンドウ ────────────────────────────────────────────────────
        if self.settings_open {
            if self.cache_size_mb < 0.0 {
                self.cache_size_mb = calc_cache_size_mb();
            }

            let ui_language = self.app_settings.ui_language;
            let prev_settings = self.app_settings.clone();
            let mut app_settings = self.app_settings.clone();
            let event = settings::show(
                ctx,
                &mut self.settings_open,
                ui_language,
                &mut app_settings,
                &self.performance_resources,
                self.cache_size_mb,
            );

            if app_settings.clamped_display_w() != prev_settings.clamped_display_w() {
                self.app_settings = app_settings.clone();
                self.library
                    .apply_thumb_size(self.app_settings.thumb_w(), self.app_settings.thumb_h());
            }

            if app_settings.clamped_library_hud_font_level()
                != prev_settings.clamped_library_hud_font_level()
            {
                self.library.hud_font_size = app_settings.library_hud_font_size();
            }
            if app_settings.library_hud_style != prev_settings.library_hud_style {
                self.library.hud_style = app_settings.library_hud_style;
            }
            if app_settings.library_card_selection_style
                != prev_settings.library_card_selection_style
            {
                self.library.selection_style = app_settings.library_card_selection_style;
            }

            match event {
                SettingsEvent::ThumbSizeChanged => {
                    self.app_settings = app_settings.clone();
                    self.app_settings
                        .save_with_resources(&self.performance_resources);
                    self.library
                        .apply_thumb_size(self.app_settings.thumb_w(), self.app_settings.thumb_h());
                }
                SettingsEvent::ClearCache => {
                    self.library.worker.clear_cache_state();
                    let mut clear_cache_failed = false;
                    {
                        let _artifact_guard = self.library.artifact_gate.write();
                        let thumb_cache =
                            DiskCache::open(DiskCache::default_root()).or_else(|_| {
                                DiskCache::open(
                                    std::env::temp_dir()
                                        .join(crate::app_identity::app_data_dir())
                                        .join("thumbs"),
                                )
                            });
                        let page_map_cache = PageMapDiskCache::open(
                            PageMapDiskCache::default_root(),
                        )
                        .or_else(|_| {
                            PageMapDiskCache::open(
                                std::env::temp_dir()
                                    .join(crate::app_identity::app_data_dir())
                                    .join("page_maps"),
                            )
                        });

                        match (thumb_cache, page_map_cache) {
                            (Ok(thumb_cache), Ok(page_map_cache)) => {
                                let thumb_clear = thumb_cache.clear_all();
                                let page_map_clear = page_map_cache.clear_all();
                                match (thumb_clear, page_map_clear) {
                                    (Ok(_), Ok(_)) => {
                                        self.cache_size_mb = 0.0;
                                        tracing::info!("cache cleared");
                                    }
                                    (thumb_res, page_map_res) => {
                                        if let Err(e) = thumb_res {
                                            tracing::error!("thumb cache clear: {e}");
                                        }
                                        if let Err(e) = page_map_res {
                                            tracing::error!("page map cache clear: {e}");
                                        }
                                    }
                                }
                            }
                            (thumb_res, page_map_res) => {
                                if let Err(e) = thumb_res {
                                    tracing::error!("thumb cache open failed: {e}");
                                }
                                if let Err(e) = page_map_res {
                                    tracing::error!("page map cache open failed: {e}");
                                }
                                clear_cache_failed = true;
                            }
                        }
                    }
                    if clear_cache_failed {
                        self.show_error_dialog(tr(ui_language, TextKey::CacheClearFailed));
                    }
                    self.library.reload_thumbs();
                }
                SettingsEvent::None => {
                    self.app_settings = app_settings;
                }
            }

            if !self.settings_open {
                self.cache_size_mb = -1.0;
                self.app_settings
                    .save_with_resources(&self.performance_resources);
            }
        }

        // ── ライブラリ（常時表示） ────────────────────────────────────────────
        self.show_library(ctx, frame);
        self.refresh_library_book_order();

        self.drain_pending_external_tool_runs(ctx);
        self.drain_pending_history_paths();
        // ThumbWorker ポーリング（入力処理後に実行して応答性を優先）
        self.library.poll_worker(ctx);
        self.library.apply_pending_updates(ctx);
        // ライブラリ画面のリアルタイム追従（3秒ポーリング）。
        // 既存サムネイルは保持し、追加/削除/同一パス入れ替えだけを差分反映する。
        self.library.poll_current_dir_changes(ctx);
    }

    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_session();
        self.app_settings
            .save_with_resources(&self.performance_resources);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.save_session();
        self.app_settings
            .save_with_resources(&self.performance_resources);
    }
}

// ── 内部メソッド ─────────────────────────────────────────────────────────────

impl App {
    fn refresh_library_book_order(&self) {
        let ordered_books: Vec<PathBuf> = self
            .library
            .entries
            .iter()
            .filter_map(|entry| {
                let path = Self::library_navigation_book_path(entry)?;
                let book_id = Self::library_navigation_book_id(entry)?;
                if self
                    .library
                    .book_states
                    .get(&book_id)
                    .is_some_and(|state| state.thumb_failed)
                {
                    return None;
                }
                Some(path)
            })
            .collect();
        let mut guard = self.library_book_order.write();
        // 内容も順序も変わっていないフレームでは、epoch を進めず旧順も残す。
        // ここで更新すると毎フレーム stale 救済用の previous_books が上書きされる。
        if guard.books == ordered_books {
            return;
        }
        let folder_book_count = ordered_books.iter().filter(|path| path.is_dir()).count();
        let book_count = ordered_books.len().saturating_sub(folder_book_count);
        tracing::trace!(
            total = ordered_books.len(),
            book_count,
            folder_book_count,
            "library.navigation.snapshot.refresh"
        );
        guard.epoch = guard.epoch.saturating_add(1);
        guard.previous_books = guard.books.clone();
        guard.books = ordered_books;
    }

    fn reap_viewer_processes(&mut self) {
        let mut alive = Vec::with_capacity(self.viewer_processes.len());
        for mut child in self.viewer_processes.drain(..) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::debug!(
                        pid = child.id(),
                        status = ?status.code(),
                        "viewer subprocess exited"
                    );
                }
                Ok(None) => alive.push(child),
                Err(e) => {
                    tracing::warn!(pid = child.id(), error = %e, "viewer subprocess wait failed");
                }
            }
        }
        self.viewer_processes = alive;
    }

    pub(super) fn push_open_history(&mut self, path: PathBuf) {
        let opened_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut temp = SessionState {
            history: mem::take(&mut self.open_history),
            ..SessionState::default()
        };
        temp.push_history(path, opened_at_ms);
        self.open_history = temp.history;
    }

    pub(super) fn remove_disk_thumb_by_key(&self, key: &(BookId, u64, Option<SystemTime>)) {
        tracing::debug!(
            id = %key.0 .0.to_hex(),
            file_size = key.1,
            modified = ?key.2,
            "app: request disk thumb removal"
        );
        let cache = DiskCache::open(DiskCache::default_root()).or_else(|_| {
            DiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("thumbs"),
            )
        });
        let Ok(cache) = cache else {
            tracing::debug!("app: disk thumb removal skipped because cache open failed");
            return;
        };
        if let Err(e) = cache.remove_thumb(&key.0, key.1, key.2) {
            tracing::debug!(error = %e, "app: disk thumb removal failed");
        } else {
            tracing::debug!("app: disk thumb removal done");
        }
    }

    pub(super) fn remove_disk_thumbs_by_id(&self, id: &BookId) {
        tracing::debug!(id = %id.0.to_hex(), "app: request disk thumb removal by id");
        let cache = DiskCache::open(DiskCache::default_root()).or_else(|_| {
            DiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("thumbs"),
            )
        });
        let Ok(cache) = cache else {
            tracing::debug!("app: disk thumb removal skipped because cache open failed");
            return;
        };
        if let Err(e) = cache.remove_thumbs_by_id(id) {
            tracing::debug!(error = %e, "app: disk thumb removal by id failed");
        } else {
            tracing::debug!("app: disk thumb removal by id done");
        }
    }

    pub(super) fn remove_disk_page_map_by_key(&self, key: &(BookId, u64, Option<SystemTime>)) {
        tracing::debug!(
            id = %key.0 .0.to_hex(),
            file_size = key.1,
            modified = ?key.2,
            "app: request page map removal"
        );
        let cache = PageMapDiskCache::open(PageMapDiskCache::default_root()).or_else(|_| {
            PageMapDiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("page_maps"),
            )
        });
        let Ok(cache) = cache else {
            tracing::debug!("app: page map removal skipped because cache open failed");
            return;
        };
        if let Err(e) = cache.remove_page_map(&key.0, key.1, key.2) {
            tracing::debug!(error = %e, "app: page map removal failed");
        } else {
            tracing::debug!("app: page map removal done");
        }
    }

    pub(super) fn remove_disk_page_maps_by_id(&self, id: &BookId) {
        tracing::debug!(id = %id.0.to_hex(), "app: request page map removal by id");
        let cache = PageMapDiskCache::open(PageMapDiskCache::default_root()).or_else(|_| {
            PageMapDiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("page_maps"),
            )
        });
        let Ok(cache) = cache else {
            tracing::debug!("app: page map removal skipped because cache open failed");
            return;
        };
        if let Err(e) = cache.remove_page_maps_by_id(id) {
            tracing::debug!(error = %e, "app: page map removal by id failed");
        } else {
            tracing::debug!("app: page map removal by id done");
        }
    }

    pub(super) fn remove_worker_book_cache(&self, id: &BookId) {
        self.library.worker.remove_book_cache(id.clone());
    }

    pub(super) fn update_path_dependent_state(&mut self, old_path: &Path, new_path: Option<&Path>) {
        let old_norm = normalize_path_for_selection(old_path);
        let new_norm = new_path.map(normalize_path_for_selection);

        let current_dir_matches = self
            .library
            .current_dir
            .as_ref()
            .is_some_and(|cur_dir| platform::paths_equivalent_for_selection(cur_dir, old_path));
        if current_dir_matches {
            match new_path {
                Some(new_path) => {
                    let normalized = normalize_dir_path(new_path.to_path_buf());
                    self.library.path_input = normalized.to_string_lossy().into_owned();
                    self.library.current_dir = Some(normalized);
                }
                None => {
                    self.library.current_dir = None;
                    self.library.path_input.clear();
                }
            }
        }

        if self
            .pending_select
            .as_ref()
            .is_some_and(|path| normalize_path_for_selection(path.as_path()) == old_norm)
        {
            self.pending_select = new_path.map(|p| p.to_path_buf());
        }
        if self
            .pending_drop_select
            .as_ref()
            .is_some_and(|path| normalize_path_for_selection(path.as_path()) == old_norm)
        {
            self.pending_drop_select = new_path.map(|p| p.to_path_buf());
        }
        if let Some(pending) = self.pending_after_load.as_mut() {
            if let Some(selected) = pending.selected_path.as_mut() {
                if normalize_path_for_selection(selected.as_path()) == old_norm {
                    pending.selected_path = new_path.map(|p| p.to_path_buf());
                }
            }
        }

        {
            let mut pending = self.pending_history_paths.lock();
            for path in pending.iter_mut() {
                if normalize_path_for_selection(path.as_path()) == old_norm {
                    if let Some(new_path) = new_path {
                        *path = new_path.to_path_buf();
                    } else {
                        *path = PathBuf::new();
                    }
                }
            }
            pending.retain(|path| !path.as_os_str().is_empty());
        }

        for entry in &mut self.open_history {
            if entry.normalized_path == old_norm {
                if let Some(new_path) = new_path {
                    entry.path = new_path.to_path_buf();
                    entry.normalized_path = new_norm.clone().unwrap_or_else(|| old_norm.clone());
                } else {
                    entry.normalized_path.clear();
                }
            }
        }
        self.open_history
            .retain(|entry| !entry.normalized_path.is_empty());

        if let Some(new_path) = new_path {
            let new_path_buf = new_path.to_path_buf();
            for path in &mut self.favorites {
                if normalize_path_for_selection(path.as_path()) == old_norm {
                    *path = new_path_buf.clone();
                }
            }
        } else {
            self.favorites
                .retain(|path| normalize_path_for_selection(path.as_path()) != old_norm);
        }

        let mut snapshot = self.library_book_order.write();
        for path in &mut snapshot.previous_books {
            if platform::paths_equivalent_for_selection(path.as_path(), old_path) {
                if let Some(new_path) = new_path {
                    *path = new_path.to_path_buf();
                } else {
                    *path = PathBuf::new();
                }
            }
        }
        snapshot
            .previous_books
            .retain(|path| !path.as_os_str().is_empty());
        for path in &mut snapshot.books {
            if platform::paths_equivalent_for_selection(path.as_path(), old_path) {
                if let Some(new_path) = new_path {
                    *path = new_path.to_path_buf();
                } else {
                    *path = PathBuf::new();
                }
            }
        }
        snapshot.books.retain(|path| !path.as_os_str().is_empty());
        snapshot.epoch = snapshot.epoch.saturating_add(1);

        let old_key_prefix = format!("{}:", old_norm);
        if let Some(new_path) = new_path {
            let new_norm = normalize_path_for_selection(new_path);
            let new_key_prefix = format!("{}:", new_norm);
            let mut updated = HashMap::new();
            for (key, tex) in self.history_thumb_textures.drain() {
                if let Some(rest) = key.strip_prefix(&old_key_prefix) {
                    updated.insert(format!("{}{}", new_key_prefix, rest), tex);
                } else {
                    updated.insert(key, tex);
                }
            }
            self.history_thumb_textures = updated;
        } else {
            self.history_thumb_textures
                .retain(|key, _| !key.starts_with(&old_key_prefix));
        }
    }

    pub(super) fn rename_disk_thumb_artifact(
        &self,
        old_id: &BookId,
        new_id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) {
        tracing::debug!(
            old_id = %old_id.0.to_hex(),
            new_id = %new_id.0.to_hex(),
            file_size,
            modified = ?modified,
            "app: request disk thumb rename"
        );
        let cache = DiskCache::open(DiskCache::default_root()).or_else(|_| {
            DiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("thumbs"),
            )
        });
        let Ok(cache) = cache else {
            tracing::debug!("app: disk thumb rename skipped because cache open failed");
            return;
        };
        match cache.rename_thumb_artifact(old_id, new_id, file_size, modified) {
            Ok(true) => tracing::debug!("app: disk thumb rename done"),
            Ok(false) => tracing::debug!("app: disk thumb rename miss"),
            Err(e) => tracing::debug!(error = %e, "app: disk thumb rename failed"),
        }
    }

    pub(super) fn rename_disk_page_map_artifact(
        &self,
        old_id: &BookId,
        new_id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) {
        tracing::debug!(
            old_id = %old_id.0.to_hex(),
            new_id = %new_id.0.to_hex(),
            file_size,
            modified = ?modified,
            "app: request page map rename"
        );
        let cache = PageMapDiskCache::open(PageMapDiskCache::default_root()).or_else(|_| {
            PageMapDiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("page_maps"),
            )
        });
        let Ok(cache) = cache else {
            tracing::debug!("app: page map rename skipped because cache open failed");
            return;
        };
        match cache.rename_page_map_artifact(old_id, new_id, file_size, modified) {
            Ok(true) => tracing::debug!("app: page map rename done"),
            Ok(false) => tracing::debug!("app: page map rename miss"),
            Err(e) => tracing::debug!(error = %e, "app: page map rename failed"),
        }
    }

    pub(super) fn apply_renamed_path_diff(
        &mut self,
        old_path: &Path,
        new_path: &Path,
        book_meta: &BookMeta,
    ) {
        let new_book_id = BookId::from_path(new_path);
        let cache_key = (
            book_meta.id.clone(),
            book_meta.size,
            Some(book_meta.modified),
        );
        self.remove_worker_book_cache(&book_meta.id);
        {
            let _artifact_guard = self.library.artifact_gate.write();
            self.rename_disk_thumb_artifact(
                &book_meta.id,
                &new_book_id,
                book_meta.size,
                Some(book_meta.modified),
            );
            self.rename_disk_page_map_artifact(
                &book_meta.id,
                &new_book_id,
                book_meta.size,
                Some(book_meta.modified),
            );
        }

        self.update_path_dependent_state(old_path, Some(new_path));
        crate::domain::archive_settings::SettingsStore::rename_path_on_disk(old_path, new_path);
        self.library
            .rename_reading_hud_state_for_path(old_path, new_path);
        let favorite_store = self.library.favorite_store_handle();
        let mut favorite_store = favorite_store.write();
        if favorite_store.rename_path(old_path, new_path) && !favorite_store.save() {
            tracing::warn!(
                old_path = %old_path.display(),
                new_path = %new_path.display(),
                "favorite rename save failed"
            );
            *favorite_store = crate::infra::favorite_store::FavoriteStore::load();
        }
        if let Err(e) = crate::infra::kind_group_store::rename_override(
            &old_path.to_string_lossy(),
            &new_path.to_string_lossy(),
        ) {
            tracing::debug!(
                error = %e,
                old_path = %old_path.display(),
                new_path = %new_path.display(),
                "kind-group override rename failed"
            );
        }
        tracing::debug!(
            old_id = %cache_key.0 .0.to_hex(),
            new_id = %new_book_id.0.to_hex(),
            old_path = %old_path.display(),
            new_path = %new_path.display(),
            "app: renamed path dependencies updated"
        );
    }

    fn drain_pending_history_paths(&mut self) {
        let mut pending = self.pending_history_paths.lock();
        if pending.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = pending.drain(..).collect();
        drop(pending);
        for path in paths {
            // Viewer 内の本移動に追従して、ライブラリ側の主選択も同期する。
            self.restore_selection_by_path(Some(path.clone()));
            self.library.scroll_selected_into_view_pending = true;
            self.push_open_history(path);
        }
    }

    fn drain_pending_rebuilt_viewer_paths(&mut self, ctx: &egui::Context) {
        let mut pending = self.pending_rebuilt_viewer_paths.lock();
        if pending.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = pending.drain(..).collect();
        drop(pending);
        for path in paths {
            if let Err(()) = self.open_viewer_by_path(path.clone(), ctx) {
                tracing::warn!(
                    path = %path.display(),
                    "rebuilt cbz auto-open failed"
                );
            }
        }
    }

    fn drain_pending_viewer_sync_events(&mut self, ctx: &egui::Context) {
        let mut pending = self.pending_viewer_sync_events.lock();
        if pending.is_empty() {
            return;
        }
        let events: Vec<ViewerSyncEvent> = pending.drain(..).collect();
        drop(pending);
        for event in events {
            match event {
                ViewerSyncEvent::Deleted {
                    deleted_path,
                    next_path,
                } => {
                    self.apply_deleted_path_diff(deleted_path.as_path(), next_path.as_deref());
                }
                ViewerSyncEvent::ReadingSessionFinished { book_path } => {
                    self.library
                        .refresh_reading_hud_state_for_path(book_path.as_path());
                    self.library.mark_filter_dirty();
                    ctx.request_repaint();
                }
                ViewerSyncEvent::Navigated { path } => {
                    self.restore_selection_by_path(Some(path.clone()));
                    self.library.scroll_selected_into_view_pending = true;
                    self.push_open_history(path);
                }
                ViewerSyncEvent::FavoriteToggle {
                    request_id,
                    current_path,
                    response_tx,
                } => {
                    let result = if !current_path.exists() {
                        tracing::warn!(
                            request_id,
                            path = %current_path.display(),
                            "viewer.ipc.favorite_toggle.file_not_found"
                        );
                        FavoriteToggleResult::Error(crate::infra::ipc::IpcErrorCode::FileNotFound)
                    } else if let Some(state) = self.toggle_favorite(current_path.as_path()) {
                        let favorite_state = match state {
                            crate::infra::favorite_store::FavoriteState::Favorite => {
                                crate::infra::ipc::ViewerFavoriteState::On
                            }
                            crate::infra::favorite_store::FavoriteState::NotFavorite => {
                                crate::infra::ipc::ViewerFavoriteState::Off
                            }
                        };
                        FavoriteToggleResult::Success(favorite_state)
                    } else {
                        tracing::warn!(
                            request_id,
                            path = %current_path.display(),
                            "viewer.ipc.favorite_toggle.failed"
                        );
                        FavoriteToggleResult::Error(crate::infra::ipc::IpcErrorCode::Unknown)
                    };
                    if response_tx.send(result).is_err() {
                        tracing::warn!(
                            request_id,
                            path = %current_path.display(),
                            "viewer.ipc.favorite_toggle.response_channel_closed"
                        );
                    }
                }
                ViewerSyncEvent::RebuildSelectedImagesAsCbzAndNext {
                    request_id,
                    book_id,
                    current_path,
                    delete_entries,
                    next_path,
                    response_tx,
                } => {
                    let result = match self.library.archive_entry_by_book_id(&book_id) {
                        Some(entry) => {
                            let rebuild_result = self.rebuild_cbz_and_sync_library_entry(
                                &entry,
                                crate::domain::archive::CbzRebuildPlanOptions {
                                    delete_entries,
                                    remaining_image_entries_after_delete: None,
                                },
                            );
                            match rebuild_result {
                                Ok(sync_result) => {
                                    if self.app_settings.open_rebuilt_cbz_in_new_viewer {
                                        self.pending_rebuilt_viewer_paths
                                            .lock()
                                            .push(sync_result.rebuilt_path);
                                    }
                                    match next_path {
                                        Some(path) => {
                                            RebuildSelectedImagesAsCbzAndNextResult::NavigateTo(
                                                path,
                                            )
                                        }
                                        None => {
                                            RebuildSelectedImagesAsCbzAndNextResult::NoMoreBooks
                                        }
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        request_id,
                                        path = %current_path.display(),
                                        error = %error,
                                        "viewer.ipc.rebuild_selected_images_as_cbz_and_next.failed"
                                    );
                                    RebuildSelectedImagesAsCbzAndNextResult::Error(
                                        crate::infra::ipc::IpcErrorCode::Unknown,
                                    )
                                }
                            }
                        }
                        None => {
                            tracing::warn!(
                                request_id,
                                path = %current_path.display(),
                                "viewer.ipc.rebuild_selected_images_as_cbz_and_next.file_not_found"
                            );
                            RebuildSelectedImagesAsCbzAndNextResult::Error(
                                crate::infra::ipc::IpcErrorCode::FileNotFound,
                            )
                        }
                    };
                    if response_tx.send(result).is_err() {
                        tracing::warn!(
                            request_id,
                            path = %current_path.display(),
                            "viewer.ipc.rebuild_selected_images_as_cbz_and_next.response_channel_closed"
                        );
                    }
                }
            }
        }
    }

    pub(super) fn apply_deleted_path_diff(
        &mut self,
        deleted_path: &Path,
        next_path: Option<&Path>,
    ) {
        let deleted_cleanup = self.library.deleted_path_cleanup(deleted_path);
        if let Some(book_meta) = deleted_cleanup.as_ref().and_then(|c| c.book_meta.as_ref()) {
            self.remove_worker_book_cache(&book_meta.id);
        } else if let Some(thumb_id) = deleted_cleanup.as_ref().and_then(|c| c.thumb_id.as_ref()) {
            self.remove_worker_book_cache(thumb_id);
        }
        {
            let _artifact_guard = self.library.artifact_gate.write();
            if let Some(book_meta) = deleted_cleanup.as_ref().and_then(|c| c.book_meta.as_ref()) {
                let key = (
                    book_meta.id.clone(),
                    book_meta.size,
                    Some(book_meta.modified),
                );
                self.remove_disk_thumb_by_key(&key);
                self.remove_disk_page_map_by_key(&key);
            } else if let Some(thumb_id) =
                deleted_cleanup.as_ref().and_then(|c| c.thumb_id.as_ref())
            {
                self.remove_disk_thumbs_by_id(thumb_id);
                self.remove_disk_page_maps_by_id(thumb_id);
            }
        }
        let _ = self.library.remove_deleted_path(deleted_path);
        if !matches!(
            deleted_cleanup.as_ref().map(|c| c.kind),
            Some(crate::ui::library::DeletedEntryKind::ImageFile)
        ) {
            crate::domain::archive_settings::SettingsStore::remove_path_from_disk(deleted_path);
        }
        if let Err(e) = crate::infra::kind_group_store::remove_overrides_bulk(&[deleted_path
            .to_string_lossy()
            .into_owned()])
        {
            tracing::debug!(error = %e, path = %deleted_path.display(), "kind-group override remove skipped");
        }
        self.update_path_dependent_state(deleted_path, None);
        if let Some(target) = next_path {
            self.restore_selection_by_path(Some(target.to_path_buf()));
            self.push_open_history(target.to_path_buf());
        } else if self.library.entries.is_empty() {
            self.library.selected_idx = None;
            self.library.anchor_idx = None;
            self.library.selected_set.clear();
        }
    }

    pub(super) fn rebuild_cbz_and_sync_library_entry(
        &mut self,
        entry: &LibraryEntry,
        options: CbzRebuildPlanOptions,
    ) -> anyhow::Result<RebuiltCbzLibrarySyncResult> {
        let completed = crate::domain::archive::rebuild_cbz_for_library_entry(entry, options)?;
        let rebuilt_path = completed.plan.output_path.clone();
        let rebuilt_entry = crate::infra::fs::scanner::scan_path(rebuilt_path.as_path())?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cbz rebuild output is not a supported library entry: {}",
                    rebuilt_path.display()
                )
            })?;
        if !matches!(rebuilt_entry, LibraryEntry::Archive(_)) {
            anyhow::bail!(
                "cbz rebuild output is not an archive library entry: {}",
                rebuilt_path.display()
            );
        }
        self.apply_deleted_path_diff(completed.plan.input_path.as_path(), None);
        self.library
            .register_rebuilt_cbz_entry(completed.plan.input_path.as_path(), rebuilt_entry);
        Ok(RebuiltCbzLibrarySyncResult { rebuilt_path })
    }

    fn save_session(&mut self) {
        let selected_path = self
            .library
            .selected_idx
            .and_then(|i| self.library.entries.get(i))
            .and_then(Self::book_entry_ref)
            .map(|e| e.path.to_string_lossy().into_owned());

        let mut state = SessionState::load();
        state.window_x = self.saved_win_pos.map(|p| p[0]);
        state.window_y = self.saved_win_pos.map(|p| p[1]);
        state.window_w = self.saved_win_size.map(|s| s[0]).unwrap_or(1200.0);
        state.window_h = self.saved_win_size.map(|s| s[1]).unwrap_or(800.0);
        state.last_dir = self
            .library
            .current_dir
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned());
        state.sort_key = sort_key_to_str(&self.library.sort_key).to_owned();
        state.sort_order = sort_order_to_str(&self.library.sort_order).to_owned();
        state.grid_scroll_y = self.library.scroll_y;
        state.selected_path = selected_path;
        state.favorite_dirs = self
            .favorites
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        state.filter_text = self.library.filter.keyword.clone();
        state.viewer_quality = self.app_settings.viewer_quality;
        state.viewer_rgba_cache_max_mb = self.app_settings.viewer_rgba_cache_max_mb;
        state.viewer_background_worker_count = self.app_settings.viewer_background_worker_count;
        state.left_pane_tab = self.left_pane_tab;
        state.history = self.open_history.clone();
        tracing::debug!(
            favorites = ?state.favorite_dirs,
            last_dir = ?state.last_dir,
            selected_path = ?state.selected_path,
            "app: saving session state"
        );
        state.save();
    }

    // ── Library 画面 ──────────────────────────────────────────────────────────

    // egui の deprecated API をまだ使うため、show_library だけ allow を残す。
    // 置換後はこの許可を外せる。
    #[allow(deprecated)]
    fn show_library(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        const TOPBAR_H: f32 = 40.0;
        let ui_language = self.app_settings.ui_language;

        if self.sidebar_open && ctx.input(|i| i.key_pressed(Key::Escape)) {
            log::debug!("[window] close requested source=esc-sidebar");
            self.sidebar_open = false;
        }

        let sidebar_toggled_this_frame =
            self.render_topbar_and_apply_result(ctx, ui_language, TOPBAR_H);

        let modal_open = self.renaming.is_some()
            || self.properties_dialog.is_some()
            || self.deleting.is_some()
            || self.book_settings_clearing.is_some()
            || self.setting_group_open;
        let interaction_blocked = modal_open || self.suppress_pointer_until_release;
        let keyboard_blocked = self.library.is_path_editing
            || self.library.path_input_focused
            || self.library.filter_input_focused;

        self.process_library_shortcuts(ctx, interaction_blocked, keyboard_blocked);
        if !self.settings_open && !keyboard_blocked {
            if let Some(action) =
                library::poll_shortcuts(ctx, &mut self.library, interaction_blocked)
            {
                self.dispatch_library_shortcut_action(action, frame);
            }
        }

        let library_external_tools = self.external_tool_menu_items_for_library();
        let library_external_busy = self.is_external_tool_busy();

        self.render_library_panel_and_dispatch_action(
            ctx,
            frame,
            ui_language,
            &library_external_tools,
            library_external_busy,
        );
        self.render_sidebar_overlay_and_dispatch_action(
            ctx,
            ui_language,
            TOPBAR_H,
            sidebar_toggled_this_frame,
        );
        self.render_library_feedback_overlays(ctx, ui_language, TOPBAR_H);
        self.render_library_modals(ctx, ui_language);
    }

    #[allow(deprecated)]
    fn render_topbar_and_apply_result(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
        topbar_height: f32,
    ) -> bool {
        let topbar_result = egui::Panel::top("topbar")
            .exact_size(topbar_height)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                topbar::show(
                    ui,
                    &mut self.library,
                    ui_language,
                    &mut self.app_settings.viewer_open_mode,
                    self.suppress_next_dropped_files,
                )
            })
            .inner;
        let _ = topbar_result.scan_dir.as_ref();

        if !self.settings_open {
            self.handle_external_drop_in_app(ctx);
        }
        self.apply_topbar_result(ctx, topbar_result)
    }

    fn apply_topbar_result(
        &mut self,
        ctx: &egui::Context,
        topbar_result: topbar::TopbarResult,
    ) -> bool {
        if let Some(path) = topbar_result.breadcrumb_nav {
            self.navigate_to_dir_with_history(path);
        }
        if topbar_result.path_blank_clicked && !self.settings_open {
            self.begin_path_edit();
        }
        if topbar_result.path_cancelled {
            self.library.is_path_editing = false;
            self.library.path_input_focused = false;
            self.library.path_edit_select_all_pending = false;
        }
        if let Some(path) = topbar_result.path_commit {
            self.commit_path_edit(path);
        }
        if self.library.is_path_editing && !self.library.path_edit_select_all_pending {
            let outside_click = ctx.input(|i| i.pointer.press_origin()).is_some_and(|pos| {
                topbar_result
                    .path_edit_rect
                    .is_none_or(|rect| !rect.contains(pos))
            });
            if outside_click {
                self.library.is_path_editing = false;
                self.library.path_input_focused = false;
                self.library.path_edit_select_all_pending = false;
            }
        }
        if topbar_result.nav_back {
            self.navigate_back();
        }
        if topbar_result.nav_forward {
            self.navigate_forward();
        }
        if topbar_result.nav_up {
            self.navigate_parent();
        }
        if topbar_result.nav_reload {
            self.reload_current_dir(ctx);
        }
        let sidebar_toggled_this_frame = topbar_result.toggle_sidebar;
        if topbar_result.toggle_sidebar {
            self.sidebar_open = !self.sidebar_open;
        }
        if topbar_result.settings_requested {
            self.settings_open = true;
        }
        if topbar_result.hud_mode_changed {
            self.app_settings.library_hud_mode = self.library.hud_mode;
            self.library.hud_font_size = self.app_settings.library_hud_font_size();
            self.app_settings
                .save_with_resources(&self.performance_resources);
        }
        if topbar_result.viewer_open_mode_changed {
            self.app_settings
                .save_with_resources(&self.performance_resources);
        }
        sidebar_toggled_this_frame
    }

    fn process_library_shortcuts(
        &mut self,
        ctx: &egui::Context,
        interaction_blocked: bool,
        keyboard_blocked: bool,
    ) {
        if !self.settings_open && !interaction_blocked {
            let (ctrl_l, ctrl_f, alt_d) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::CTRL, Key::L),
                    i.consume_key(egui::Modifiers::CTRL, Key::F),
                    i.consume_key(egui::Modifiers::ALT, Key::D),
                )
            });
            if ctrl_l || alt_d {
                self.begin_path_edit();
            }
            if ctrl_f {
                self.library.is_path_editing = false;
                self.library.path_input_focused = false;
                self.library.path_edit_select_all_pending = false;
                self.library.filter_focus_request = true;
            }
        }
        if !self.settings_open && !interaction_blocked && !keyboard_blocked {
            let (alt_left, alt_right, alt_up, f5, mouse_back, mouse_forward) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::ALT, Key::ArrowLeft),
                    i.consume_key(egui::Modifiers::ALT, Key::ArrowRight),
                    i.consume_key(egui::Modifiers::ALT, Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, Key::F5),
                    i.pointer.button_pressed(PointerButton::Extra1),
                    i.pointer.button_pressed(PointerButton::Extra2),
                )
            });
            if alt_left || mouse_back {
                self.navigate_back();
            }
            if alt_right || mouse_forward {
                self.navigate_forward();
            }
            if alt_up {
                self.navigate_parent();
            }
            if f5 {
                self.reload_current_dir(ctx);
            }
        }
    }

    fn dispatch_library_shortcut_action(&mut self, action: LibraryAction, frame: &eframe::Frame) {
        if self.dispatch_library_common_action(&action, frame) {
            return;
        }
        match action {
            LibraryAction::OpenArchive(_) | LibraryAction::None => {}
            LibraryAction::RunExternalTool { .. } => {}
            _ => {}
        }
    }

    fn dispatch_library_common_action(
        &mut self,
        action: &LibraryAction,
        frame: &eframe::Frame,
    ) -> bool {
        match action {
            LibraryAction::Rename(idx) => self.begin_rename(*idx),
            LibraryAction::Properties(idx) => self.show_entry_properties(*idx),
            LibraryAction::Delete(idxs) => self.begin_delete(idxs.clone()),
            LibraryAction::Copy(idxs) => self.do_copy(idxs.clone()),
            LibraryAction::ExternalDrag(idxs) => self.start_external_drag(idxs, frame),
            LibraryAction::ToggleFavorite(idx) => {
                let _ = self.toggle_favorite_entry(*idx);
            }
            LibraryAction::OpenFolder(idx) => {
                if let Some(path) = self.entry_path_at(*idx) {
                    self.navigate_to_dir_with_history(path);
                }
            }
            LibraryAction::OpenInExplorer(idx) => {
                if let Some(path) = self.entry_path_at(*idx) {
                    self.open_in_explorer(path.as_path());
                }
            }
            LibraryAction::ClearBookSettings(targets) => {
                self.begin_clear_book_settings(targets.clone())
            }
            LibraryAction::SetGroup(targets) => {
                self.setting_group_targets = targets.clone();
                self.setting_group_buf = String::new();
                self.setting_group_open = true;
            }
            LibraryAction::OpenArchive(_)
            | LibraryAction::RunExternalTool { .. }
            | LibraryAction::None => return false,
        }
        true
    }

    #[allow(deprecated)]
    fn render_library_panel_and_dispatch_action(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        ui_language: crate::domain::app_settings::UiLanguage,
        library_external_tools: &[crate::ui::virtual_grid::ExternalToolMenuItem],
        library_external_busy: bool,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let modal_open = self.renaming.is_some()
                || self.properties_dialog.is_some()
                || self.deleting.is_some()
                || self.book_settings_clearing.is_some()
                || self.setting_group_open;
            let interaction_blocked = modal_open || self.suppress_pointer_until_release;
            let action = library::show(
                ui,
                &mut self.library,
                ui_language,
                interaction_blocked,
                library_external_tools,
                library_external_busy,
            );
            self.dispatch_library_ui_action(action, ctx, frame);
        });
    }

    fn dispatch_library_ui_action(
        &mut self,
        action: LibraryAction,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        if !self.settings_open && self.dispatch_library_common_action(&action, frame) {
            return;
        }
        match action {
            LibraryAction::OpenArchive(idx) if !self.settings_open => {
                let open_as_viewer = self.library.entries.get(idx).is_none_or(|entry| {
                    !matches!(entry, LibraryEntry::FolderBook(_))
                        || self.app_settings.folder_book_open_as_viewer
                });
                if open_as_viewer {
                    self.open_viewer(idx, ctx)
                } else if let Some(path) = self.entry_path_at(idx) {
                    self.navigate_to_dir_with_history(path);
                }
            }
            LibraryAction::RunExternalTool {
                tool_index,
                targets,
            } if !self.settings_open => {
                self.trigger_external_tool_from_library(tool_index, &targets);
            }
            _ => {}
        }
    }

    fn render_sidebar_overlay_and_dispatch_action(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
        topbar_height: f32,
        sidebar_toggled_this_frame: bool,
    ) {
        if !self.sidebar_open {
            return;
        }

        let overlay = egui::Area::new("sidebar_overlay".into())
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(0.0, topbar_height))
            .show(ctx, |ui| {
                let frame = egui::Frame::new()
                    .fill(theme::SURFACE_BG)
                    .stroke(egui::Stroke::new(1.0, theme::SEPARATOR_WEAK))
                    .shadow(egui::epaint::Shadow {
                        offset: [2, 0],
                        blur: 10,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(24),
                    })
                    .inner_margin(egui::Margin::same(theme::SIDEBAR_INNER_MARGIN as i8));

                frame.show(ui, |ui| {
                    ui.set_min_width(theme::SIDEBAR_W);
                    ui.set_max_width(theme::SIDEBAR_W);
                    let panel_height = (ctx.content_rect().height()
                        - topbar_height
                        - theme::SIDEBAR_INNER_MARGIN * 2.0)
                        .max(120.0);
                    ui.set_min_height(panel_height);
                    ui.set_max_height(panel_height);
                    sidebar::show(
                        ui,
                        sidebar::SidebarViewContext {
                            state: &mut self.library,
                            favorites: &mut self.favorites,
                            left_pane_tab: &mut self.left_pane_tab,
                            language: ui_language,
                            history: self.open_history.make_contiguous(),
                            history_textures: &mut self.history_thumb_textures,
                            disk_cache: self.sidebar_disk_cache.as_ref(),
                        },
                    )
                })
            });

        if !self.settings_open {
            if let Some(sidebar_action) = overlay.inner.inner {
                match sidebar_action {
                    sidebar::SidebarAction::OpenFavorite(path) => {
                        self.library.history_back.clear();
                        self.library.history_forward.clear();
                        self.set_pending_after_load(None, Some(0.0));
                        self.load_library_dir(path);
                        self.sidebar_open = false;
                    }
                    sidebar::SidebarAction::OpenInExplorer(path) => {
                        self.open_in_explorer(path.as_path());
                        self.sidebar_open = false;
                    }
                    sidebar::SidebarAction::OpenHistory(path) => {
                        let _ = self.open_viewer_by_path(path, ctx);
                        self.sidebar_open = false;
                    }
                }
            }
        }

        let should_close = !sidebar_toggled_this_frame
            && ctx.input(|i| {
                i.pointer.any_click()
                    && i.pointer.interact_pos().is_some_and(|pos| {
                        pos.y < topbar_height || !overlay.response.rect.contains(pos)
                    })
            });
        if should_close {
            self.sidebar_open = false;
        }
    }

    fn render_library_feedback_overlays(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
        topbar_height: f32,
    ) {
        if let Some((message, at)) = self.pending_toast.as_ref() {
            let elapsed = at.elapsed();
            if elapsed.as_secs_f32() <= 2.0 {
                egui::Area::new("path_toast".into())
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::RIGHT_TOP, [-16.0, topbar_height + 10.0])
                    .show(ctx, |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 220))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(120)))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(message).color(egui::Color32::WHITE));
                            });
                    });
                ctx.request_repaint();
            } else {
                self.pending_toast = None;
            }
        }

        if let Some(message) = self.pending_error_dialog.clone() {
            let mut open = true;
            egui::Window::new(tr(ui_language, TextKey::ErrorTitle))
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .min_width(460.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(tr(ui_language, TextKey::ViewerInitFailed));
                    ui.add_space(6.0);
                    ui.label(message);
                    ui.add_space(10.0);
                    if ui.button(tr(ui_language, TextKey::Ok)).clicked() {
                        self.pending_error_dialog = None;
                    }
                });
            if !open {
                self.pending_error_dialog = None;
            }
        }
    }

    fn render_library_modals(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
    ) {
        self.render_rename_dialog(ctx, ui_language);
        self.render_properties_dialog(ctx, ui_language);
        self.render_delete_dialog(ctx, ui_language);
        self.render_clear_book_settings_dialog(ctx, ui_language);
        self.render_group_settings_dialog(ctx, ui_language);
    }

    fn render_properties_dialog(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
    ) {
        if let Some(props) = self.properties_dialog.clone() {
            let mut open = true;
            let mut close_requested = false;

            egui::Window::new(tr(ui_language, TextKey::PropertiesTitle))
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .min_width(ENTRY_PROPERTIES_DIALOG_W)
                .max_width(ENTRY_PROPERTIES_DIALOG_W)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(ENTRY_PROPERTIES_DIALOG_W);
                    ui.set_max_width(ENTRY_PROPERTIES_DIALOG_W);
                    egui::Frame::new()
                        .fill(theme::SURFACE_BG)
                        .inner_margin(egui::Margin::symmetric(22, 20))
                        .corner_radius(egui::CornerRadius::same(7))
                        .show(ui, |ui| {
                            ui.set_min_width(entry_property_grid_width());
                            ui.set_max_width(entry_property_grid_width());

                            let mut rows = vec![
                                EntryPropertyRow {
                                    label: tr(ui_language, TextKey::NameLabel).to_owned(),
                                    value: props.name.clone(),
                                    copy_label: Some(tr(ui_language, TextKey::Copy).to_owned()),
                                    height: EntryPropertyRowHeight::ThreeLines,
                                },
                                EntryPropertyRow {
                                    label: tr(ui_language, TextKey::PathLabel).to_owned(),
                                    value: props.path.clone(),
                                    copy_label: Some(tr(ui_language, TextKey::Copy).to_owned()),
                                    height: EntryPropertyRowHeight::ThreeLines,
                                },
                                EntryPropertyRow {
                                    label: tr(ui_language, TextKey::TypeLabel).to_owned(),
                                    value: props.kind.clone(),
                                    copy_label: None,
                                    height: EntryPropertyRowHeight::Single,
                                },
                            ];

                            if let Some(size_bytes) = props.size_bytes {
                                rows.push(EntryPropertyRow {
                                    label: tr(ui_language, TextKey::SizeLabel).to_owned(),
                                    value: format_entry_info_file_size(size_bytes),
                                    copy_label: None,
                                    height: EntryPropertyRowHeight::Single,
                                });
                            }
                            if let Some(modified) = props.modified {
                                rows.push(EntryPropertyRow {
                                    label: tr(ui_language, TextKey::ModifiedAt).to_owned(),
                                    value: format_entry_info_modified(modified),
                                    copy_label: None,
                                    height: EntryPropertyRowHeight::Single,
                                });
                            }
                            if let Some(page_count) = props.page_count {
                                rows.push(EntryPropertyRow {
                                    label: tr(ui_language, TextKey::PageCountLabel).to_owned(),
                                    value: page_count.to_string(),
                                    copy_label: None,
                                    height: EntryPropertyRowHeight::Single,
                                });
                            }

                            render_entry_property_grid(ui, &rows);

                            ui.add_space(12.0);
                            let close_button_w = 132.0;
                            let mut close_clicked = false;
                            ui.horizontal(|ui| {
                                ui.add_space(
                                    ((entry_property_grid_width() - close_button_w) / 2.0).max(0.0),
                                );
                                let buttons = dialog_button_row(
                                    ui,
                                    31.0,
                                    &[DialogButtonSpec {
                                        id: ui.id().with(("properties_dialog", "close")),
                                        label: tr(ui_language, TextKey::Close),
                                        width: close_button_w,
                                        is_default: true,
                                    }],
                                );
                                close_clicked = buttons[0].clicked;
                            });
                            if close_clicked
                                || ui.input(|i| {
                                    i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter)
                                })
                            {
                                close_requested = true;
                            }
                        });
                });

            if close_requested || !open {
                self.properties_dialog = None;
            }
        }
    }

    fn render_rename_dialog(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
    ) {
        if let Some((idx, mut buf)) = self.renaming.take() {
            let mut open = true;
            let mut confirmed = false;
            let mut cancelled = false;

            egui::Window::new(tr(ui_language, TextKey::RenameTitle))
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .min_width(520.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(500.0);
                    egui::Frame::new()
                        .fill(theme::SURFACE_BG)
                        .inner_margin(egui::Margin::symmetric(22, 20))
                        .corner_radius(egui::CornerRadius::same(7))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(tr(ui_language, TextKey::NewFileNamePrompt))
                                    .size(theme::FONT_SIZE_SMALL)
                                    .color(theme::TEXT_SUBTLE),
                            );
                            ui.add_space(8.0);
                            let resp = ui
                                .scope(|ui| {
                                    ui.visuals_mut().selection.bg_fill =
                                        theme::ACCENT.linear_multiply(0.3);
                                    ui.visuals_mut().selection.stroke =
                                        egui::Stroke::new(1.0, theme::ACCENT_ACTIVE);
                                    ui.visuals_mut().widgets.active.bg_stroke =
                                        egui::Stroke::new(1.0, theme::ACCENT_ACTIVE);
                                    ui.visuals_mut().widgets.hovered.bg_stroke =
                                        egui::Stroke::new(1.0, theme::ACCENT_HOVER);
                                    ui.add_sized(
                                        [ui.available_width(), 32.0],
                                        egui::TextEdit::singleline(&mut buf)
                                            .hint_text(tr(ui_language, TextKey::NewFileNameHint)),
                                    )
                                })
                                .inner;
                            resp.request_focus();
                            ui.add_space(14.0);
                            let buttons = dialog_button_row(
                                ui,
                                31.0,
                                &[
                                    DialogButtonSpec {
                                        id: ui.id().with(("rename_dialog", "confirm")),
                                        label: tr(ui_language, TextKey::Confirm),
                                        width: 132.0,
                                        is_default: true,
                                    },
                                    DialogButtonSpec {
                                        id: ui.id().with(("rename_dialog", "cancel")),
                                        label: tr(ui_language, TextKey::Cancel),
                                        width: 132.0,
                                        is_default: false,
                                    },
                                ],
                            );
                            if buttons[0].clicked || ui.input(|i| i.key_pressed(Key::Enter)) {
                                confirmed = true;
                            }
                            if buttons[1].clicked || ui.input(|i| i.key_pressed(Key::Escape)) {
                                cancelled = true;
                            }
                        });
                });

            if confirmed {
                self.commit_rename(idx, buf);
            } else if !cancelled && open {
                self.renaming = Some((idx, buf));
            }
        }
    }

    fn render_delete_dialog(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
    ) {
        if let Some(ref idxs) = self.deleting.clone() {
            let mut open = true;
            let mut confirmed = false;
            let mut cancelled = false;

            let count = idxs.len();
            let label: String = if count == 1 {
                self.library
                    .entries
                    .get(idxs[0])
                    .map(|entry| match entry {
                        LibraryEntry::Archive(entry) => entry.title.to_string(),
                        LibraryEntry::Folder(entry) | LibraryEntry::FolderBook(entry) => {
                            entry.title.to_string()
                        }
                        LibraryEntry::ImageFile(entry) => entry.title.to_string(),
                    })
                    .unwrap_or_else(|| "1".to_owned())
            } else {
                let mut file_count = 0usize;
                let mut folder_count = 0usize;
                for idx in idxs {
                    match self.library.entries.get(*idx) {
                        Some(LibraryEntry::Archive(_)) => file_count += 1,
                        Some(LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_)) => {
                            folder_count += 1
                        }
                        Some(LibraryEntry::ImageFile(_)) => file_count += 1,
                        None => {}
                    }
                }
                match (file_count, folder_count) {
                    (0, folders) => tr(ui_language, TextKey::FolderCount).replacen(
                        "{}",
                        &folders.to_string(),
                        1,
                    ),
                    (files, 0) => {
                        tr(ui_language, TextKey::FileCount).replacen("{}", &files.to_string(), 1)
                    }
                    (files, folders) => tr(ui_language, TextKey::FilesAndFoldersCount)
                        .replacen("{}", &files.to_string(), 1)
                        .replacen("{}", &folders.to_string(), 1),
                }
            };

            egui::Window::new(tr(ui_language, TextKey::DeleteConfirmTitle))
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .min_width(480.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(460.0);
                    let (left, right, enter, escape) = ui.input(|i| {
                        (
                            i.key_pressed(Key::ArrowLeft),
                            i.key_pressed(Key::ArrowRight),
                            i.key_pressed(Key::Enter),
                            i.key_pressed(Key::Escape),
                        )
                    });
                    if left {
                        self.delete_dialog_choice = DeleteDialogChoice::Ok;
                    }
                    if right {
                        self.delete_dialog_choice = DeleteDialogChoice::Cancel;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(icons::icon(icons::ICON_DELETE, 22.0).color(theme::DELETE_RED));
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(
                                tr(ui_language, TextKey::DeleteQuestion).replacen("{}", &label, 1),
                            )
                            .color(theme::TEXT_MAIN),
                        );
                    });
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(tr(ui_language, TextKey::IrreversibleActionNote))
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_SUBTLE),
                    );
                    ui.add_space(20.0);
                    let buttons = dialog_button_row(
                        ui,
                        31.0,
                        &[
                            DialogButtonSpec {
                                id: ui.id().with(("library_delete_dialog", "ok")),
                                label: tr(ui_language, TextKey::Delete),
                                width: 132.0,
                                is_default: self.delete_dialog_choice == DeleteDialogChoice::Ok,
                            },
                            DialogButtonSpec {
                                id: ui.id().with(("library_delete_dialog", "cancel")),
                                label: tr(ui_language, TextKey::Cancel),
                                width: 132.0,
                                is_default: self.delete_dialog_choice == DeleteDialogChoice::Cancel,
                            },
                        ],
                    );
                    if buttons[0].clicked {
                        self.delete_dialog_choice = DeleteDialogChoice::Ok;
                        confirmed = true;
                    }
                    if buttons[1].clicked {
                        self.delete_dialog_choice = DeleteDialogChoice::Cancel;
                        cancelled = true;
                    }
                    ui.add_space(2.0);
                    if enter {
                        match self.delete_dialog_choice {
                            DeleteDialogChoice::Ok => confirmed = true,
                            DeleteDialogChoice::Cancel => cancelled = true,
                        }
                    }
                    if escape {
                        cancelled = true;
                    }
                });

            if confirmed {
                self.commit_delete(idxs.clone(), ctx);
                self.deleting = None;
            } else if cancelled || !open {
                self.deleting = None;
                self.delete_dialog_choice = DeleteDialogChoice::Ok;
            }
        }
    }

    fn render_clear_book_settings_dialog(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
    ) {
        if let Some(ref idxs) = self.book_settings_clearing.clone() {
            let mut open = true;
            let mut confirmed = false;
            let mut cancelled = false;
            let label = if idxs.len() == 1 {
                tr(ui_language, TextKey::ClearBookSettingsQuestion).to_owned()
            } else {
                tr(ui_language, TextKey::ClearBookSettingsQuestionMultiple).replacen(
                    "{}",
                    &idxs.len().to_string(),
                    1,
                )
            };

            egui::Window::new(tr(ui_language, TextKey::ClearBookSettings))
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .min_width(520.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(500.0);
                    let (left, right, enter, escape) = ui.input(|i| {
                        (
                            i.key_pressed(Key::ArrowLeft),
                            i.key_pressed(Key::ArrowRight),
                            i.key_pressed(Key::Enter),
                            i.key_pressed(Key::Escape),
                        )
                    });
                    if left {
                        self.book_settings_clear_dialog_choice =
                            BookSettingsClearDialogChoice::Reset;
                    }
                    if right {
                        self.book_settings_clear_dialog_choice =
                            BookSettingsClearDialogChoice::Cancel;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(icons::icon(icons::ICON_REFRESH, 22.0));
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(label).color(theme::TEXT_MAIN));
                    });
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(tr(ui_language, TextKey::ClearBookSettingsNote))
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_SUBTLE),
                    );
                    ui.add_space(20.0);
                    let buttons = dialog_button_row(
                        ui,
                        31.0,
                        &[
                            DialogButtonSpec {
                                id: ui
                                    .id()
                                    .with(("library_clear_book_settings_dialog", "reset")),
                                label: tr(ui_language, TextKey::Reset),
                                width: 132.0,
                                is_default: self.book_settings_clear_dialog_choice
                                    == BookSettingsClearDialogChoice::Reset,
                            },
                            DialogButtonSpec {
                                id: ui
                                    .id()
                                    .with(("library_clear_book_settings_dialog", "cancel")),
                                label: tr(ui_language, TextKey::Cancel),
                                width: 132.0,
                                is_default: self.book_settings_clear_dialog_choice
                                    == BookSettingsClearDialogChoice::Cancel,
                            },
                        ],
                    );
                    if buttons[0].clicked {
                        self.book_settings_clear_dialog_choice =
                            BookSettingsClearDialogChoice::Reset;
                        confirmed = true;
                    }
                    if buttons[1].clicked {
                        self.book_settings_clear_dialog_choice =
                            BookSettingsClearDialogChoice::Cancel;
                        cancelled = true;
                    }
                    ui.add_space(2.0);
                    if enter {
                        match self.book_settings_clear_dialog_choice {
                            BookSettingsClearDialogChoice::Reset => confirmed = true,
                            BookSettingsClearDialogChoice::Cancel => cancelled = true,
                        }
                    }
                    if escape {
                        cancelled = true;
                    }
                });

            if confirmed {
                self.commit_clear_book_settings(idxs.clone(), ctx);
                self.book_settings_clearing = None;
            } else if cancelled || !open {
                self.book_settings_clearing = None;
                self.book_settings_clear_dialog_choice = BookSettingsClearDialogChoice::Reset;
            }
        }
    }

    fn render_group_settings_dialog(
        &mut self,
        ctx: &egui::Context,
        ui_language: crate::domain::app_settings::UiLanguage,
    ) {
        if self.setting_group_open {
            let mut open = true;
            let mut confirmed = false;
            let mut cleared = false;
            let mut request_close = false;

            egui::Window::new(tr(ui_language, TextKey::GroupSettingsTitle))
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.set_min_width(450.0);

                    let target_label = if self.setting_group_targets.len() == 1 {
                        self.setting_group_targets
                            .first()
                            .and_then(|&i| self.library.entries.get(i))
                            .and_then(|e| {
                                if let LibraryEntry::Archive(meta) = e {
                                    Some(meta.title.to_string())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default()
                    } else {
                        tr(ui_language, TextKey::GroupTargetsSentence).replacen(
                            "{}",
                            &self.setting_group_targets.len().to_string(),
                            1,
                        )
                    };
                    ui.label(
                        egui::RichText::new(&target_label)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_SUBTLE),
                    );

                    ui.separator();

                    let mut all_groups: Vec<String> = {
                        let leaf = self.library.leaf_group_counts().keys().cloned();
                        let parent = self.library.kind_groups().keys().cloned();
                        leaf.chain(parent)
                            .collect::<std::collections::HashSet<_>>()
                            .into_iter()
                            .collect()
                    };
                    all_groups.sort();

                    if !all_groups.is_empty() {
                        ui.label(
                            egui::RichText::new(tr(ui_language, TextKey::ExistingGroups))
                                .size(theme::FONT_SIZE_SMALL)
                                .color(theme::TEXT_SUBTLE),
                        );
                        let group_row_height = ui.spacing().interact_size.y.max(1.0);
                        egui::ScrollArea::vertical()
                            .max_height(group_row_height * 20.0)
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for group in &all_groups {
                                        let is_selected = self.setting_group_buf == *group;
                                        if ui.selectable_label(is_selected, group).clicked() {
                                            self.setting_group_buf = group.clone();
                                        }
                                    }
                                });
                            });
                        ui.separator();
                    }

                    ui.label(
                        egui::RichText::new(tr(ui_language, TextKey::OrManualInput))
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_SUBTLE),
                    );

                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.setting_group_buf)
                            .hint_text(tr(ui_language, TextKey::GroupNameHint))
                            .desired_width(f32::INFINITY),
                    );
                    if response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && !self.setting_group_buf.is_empty()
                    {
                        confirmed = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        request_close = true;
                    }

                    ui.separator();

                    let buttons = dialog_button_row(
                        ui,
                        31.0,
                        &[
                            DialogButtonSpec {
                                id: ui.id().with(("group_settings_dialog", "confirm")),
                                label: tr(ui_language, TextKey::Confirm),
                                width: 100.0,
                                is_default: true,
                            },
                            DialogButtonSpec {
                                id: ui.id().with(("group_settings_dialog", "clear")),
                                label: tr(ui_language, TextKey::ReturnToUncategorized),
                                width: 120.0,
                                is_default: false,
                            },
                            DialogButtonSpec {
                                id: ui.id().with(("group_settings_dialog", "cancel")),
                                label: tr(ui_language, TextKey::Cancel),
                                width: 100.0,
                                is_default: false,
                            },
                        ],
                    );
                    let can_confirm = !self.setting_group_buf.trim().is_empty();
                    if buttons[0].clicked && can_confirm {
                        confirmed = true;
                    }
                    if buttons[1].clicked {
                        cleared = true;
                    }
                    if buttons[2].clicked {
                        request_close = true;
                    }
                });

            if request_close {
                open = false;
            }

            if confirmed {
                let group = self.setting_group_buf.trim().to_string();
                if !group.is_empty() {
                    self.apply_group_settings_override(group);
                }
                open = false;
            }

            if cleared {
                self.clear_group_settings_override();
                open = false;
            }

            if !open {
                self.setting_group_open = false;
                self.setting_group_targets.clear();
                self.setting_group_buf.clear();
            }
        }
    }

    fn setting_group_override_paths(&self) -> Vec<String> {
        self.setting_group_targets
            .iter()
            .filter_map(|&i| self.library.entries.get(i))
            .filter_map(|e| {
                if let LibraryEntry::Archive(meta) = e {
                    Some(crate::util::path_eq::normalize_path_for_override(
                        &meta.path,
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    fn apply_group_settings_override(&mut self, group: String) {
        let paths = self.setting_group_override_paths();
        let bulk: Vec<(String, String)> =
            paths.iter().map(|p| (p.clone(), group.clone())).collect();
        if let Err(e) = crate::infra::kind_group_store::set_overrides_bulk(&bulk) {
            log::warn!("[kind-group] override set error: {e}");
        }
        log::debug!(
            "[kind-group] override set files={} group={:?}",
            bulk.len(),
            group
        );
        self.library.reload_kind_config();
    }

    fn clear_group_settings_override(&mut self) {
        let paths = self.setting_group_override_paths();
        if let Err(e) = crate::infra::kind_group_store::remove_overrides_bulk(&paths) {
            log::warn!("[kind-group] override remove error: {e}");
        }
        log::debug!("[kind-group] override cleared files={}", paths.len());
        self.library.reload_kind_config();
    }

    // ── ビューアウィンドウ管理 ────────────────────────────────────────────────
}

fn entry_property_grid_width() -> f32 {
    ENTRY_PROPERTY_LABEL_W
        + ENTRY_PROPERTY_CELL_GAP
        + ENTRY_PROPERTY_VALUE_W
        + ENTRY_PROPERTY_CELL_GAP
        + ENTRY_PROPERTY_ACTION_W
}

fn entry_property_text_max_width() -> f32 {
    (ENTRY_PROPERTY_VALUE_W - ENTRY_PROPERTY_TEXT_SAFE_MARGIN).max(1.0)
}

fn render_entry_property_grid(ui: &mut egui::Ui, rows: &[EntryPropertyRow]) {
    ui.set_min_width(entry_property_grid_width());
    ui.set_max_width(entry_property_grid_width());

    let line_h = ui.spacing().interact_size.y;
    let font = egui::FontId::proportional(theme::FONT_SIZE_BODY);

    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = ENTRY_PROPERTY_ROW_GAP;
        for row in rows {
            render_entry_property_row(ui, row, line_h, &font);
        }
    });
}

fn render_entry_property_row(
    ui: &mut egui::Ui,
    row: &EntryPropertyRow,
    line_h: f32,
    font: &egui::FontId,
) {
    let row_h = entry_property_row_height(row.height, line_h);
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = ENTRY_PROPERTY_CELL_GAP;
        ui.spacing_mut().item_spacing.y = 0.0;
        render_entry_property_label_cell(ui, &row.label, row.height, line_h);
        render_entry_property_value_cell(ui, row, line_h, font);
        render_entry_property_action_cell(ui, row, line_h);
        ui.allocate_space(egui::vec2(0.0, row_h));
    });
}

fn entry_property_row_height(row_height: EntryPropertyRowHeight, line_h: f32) -> f32 {
    match row_height {
        EntryPropertyRowHeight::Single => line_h,
        EntryPropertyRowHeight::ThreeLines => line_h * ENTRY_PROPERTY_MULTILINE_ROWS as f32,
    }
}

fn render_entry_property_label_cell(
    ui: &mut egui::Ui,
    label: &str,
    row_height: EntryPropertyRowHeight,
    line_h: f32,
) {
    let row_h = entry_property_row_height(row_height, line_h);
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_LABEL_W, row_h),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_width(ENTRY_PROPERTY_LABEL_W);
            ui.set_max_width(ENTRY_PROPERTY_LABEL_W);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(egui::RichText::new(label).color(theme::TEXT_MAIN));
            let remaining_h = (line_h - ui.min_rect().height()).max(0.0);
            if remaining_h > 0.0 {
                ui.add_space(remaining_h);
            }
        },
    );
}

fn render_entry_property_value_cell(
    ui: &mut egui::Ui,
    row: &EntryPropertyRow,
    line_h: f32,
    font: &egui::FontId,
) {
    let row_h = entry_property_row_height(row.height, line_h);
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_VALUE_W, row_h),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            match row.height {
                EntryPropertyRowHeight::Single => {
                    let text = truncate_entry_property_line(
                        ui.painter(),
                        &row.value,
                        entry_property_text_max_width(),
                        font,
                    );
                    render_entry_property_text_line(ui, &text, line_h);
                }
                EntryPropertyRowHeight::ThreeLines => {
                    let lines = split_entry_property_lines(
                        ui.painter(),
                        &row.value,
                        entry_property_text_max_width(),
                        ENTRY_PROPERTY_MULTILINE_ROWS,
                        font,
                    );
                    for line_idx in 0..ENTRY_PROPERTY_MULTILINE_ROWS {
                        let line = lines.get(line_idx).map(String::as_str).unwrap_or("");
                        render_entry_property_text_line(ui, line, line_h);
                    }
                }
            }
        },
    );
}

fn render_entry_property_text_line(ui: &mut egui::Ui, text: &str, line_h: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_VALUE_W, line_h),
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            ui.set_min_width(ENTRY_PROPERTY_VALUE_W);
            ui.set_max_width(ENTRY_PROPERTY_VALUE_W);
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add(egui::Label::new(
                egui::RichText::new(text).color(theme::TEXT_MAIN),
            ));
        },
    );
}

fn render_entry_property_action_cell(ui: &mut egui::Ui, row: &EntryPropertyRow, line_h: f32) {
    let row_h = entry_property_row_height(row.height, line_h);
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_ACTION_W, row_h),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if let Some(copy_label) = &row.copy_label {
                if ui
                    .add_sized(
                        [ENTRY_PROPERTY_ACTION_W, line_h],
                        egui::Button::new(copy_label),
                    )
                    .clicked()
                {
                    ui.ctx().copy_text(row.value.clone());
                }
            } else {
                ui.add_space(line_h);
            }
        },
    );
}

fn split_entry_property_lines(
    painter: &egui::Painter,
    value: &str,
    max_width: f32,
    max_lines: usize,
    font: &egui::FontId,
) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    if value.is_empty() {
        return vec![String::new()];
    }

    let chars: Vec<char> = value.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;

    while start < chars.len() && lines.len() < max_lines {
        let is_last_line = lines.len() + 1 == max_lines;
        let mut end =
            best_fit_entry_property_end(painter, &chars, start, chars.len(), max_width, font);
        if end <= start {
            end = (start + 1).min(chars.len());
        }

        if is_last_line && end < chars.len() {
            lines.push(fit_entry_property_line_with_ellipsis(
                painter,
                &chars[start..],
                max_width,
                font,
            ));
            break;
        }

        lines.push(chars[start..end].iter().collect());
        start = end;
    }

    lines
}

fn truncate_entry_property_line(
    painter: &egui::Painter,
    value: &str,
    max_width: f32,
    font: &egui::FontId,
) -> String {
    if measured_entry_property_text_width(painter, value, font) <= max_width {
        return value.to_owned();
    }
    let chars: Vec<char> = value.chars().collect();
    fit_entry_property_line_with_ellipsis(painter, &chars, max_width, font)
}

fn best_fit_entry_property_end(
    painter: &egui::Painter,
    chars: &[char],
    start: usize,
    limit: usize,
    max_width: f32,
    font: &egui::FontId,
) -> usize {
    let mut lo = start + 1;
    let mut hi = limit;
    let mut best = start;

    while lo <= hi {
        let mid = (lo + hi) / 2;
        let text: String = chars[start..mid].iter().collect();
        if measured_entry_property_text_width(painter, &text, font) <= max_width {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid.saturating_sub(1);
        }
    }

    best
}

fn fit_entry_property_line_with_ellipsis(
    painter: &egui::Painter,
    chars: &[char],
    max_width: f32,
    font: &egui::FontId,
) -> String {
    let ellipsis = "…";
    if measured_entry_property_text_width(painter, ellipsis, font) > max_width {
        return String::new();
    }

    let mut lo = 0;
    let mut hi = chars.len();
    let mut best = 0;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let mut text: String = chars[..mid].iter().collect();
        text.push_str(ellipsis);
        if measured_entry_property_text_width(painter, &text, font) <= max_width {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid.saturating_sub(1);
        }
    }

    let mut text: String = chars[..best].iter().collect();
    text.push_str(ellipsis);
    text
}

fn measured_entry_property_text_width(
    painter: &egui::Painter,
    text: &str,
    font: &egui::FontId,
) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_MAIN)
        .size()
        .x
}

fn entry_properties_for(entry: &LibraryEntry) -> EntryProperties {
    let path = entry.path();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let kind = match entry {
        LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) => String::from("DIR"),
        LibraryEntry::Archive(_) | LibraryEntry::ImageFile(_) => format_entry_kind(path),
    };
    let size_bytes = match entry {
        LibraryEntry::Archive(entry) => Some(entry.size),
        LibraryEntry::ImageFile(entry) => Some(entry.size),
        LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) => None,
    };
    let page_count = match entry {
        LibraryEntry::Archive(entry) => entry.page_count,
        LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => None,
    };

    EntryProperties {
        name,
        path: path.display().to_string(),
        kind,
        size_bytes,
        modified: Some(entry.modified()),
        page_count,
    }
}

fn format_entry_kind(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_uppercase());
    match ext.as_deref() {
        Some("JPG") | Some("JPEG") => String::from("JPEG"),
        Some("PNG") => String::from("PNG"),
        Some("WEBP") => String::from("WebP"),
        Some("AVIF") => String::from("AVIF"),
        Some("BMP") => String::from("BMP"),
        Some("TIF") | Some("TIFF") => String::from("TIFF"),
        Some("GIF") => String::from("GIF"),
        Some(other) => other.to_string(),
        None => String::from("FILE"),
    }
}

fn format_entry_info_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    if bytes == 0 {
        return String::from("0 B");
    }
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_entry_info_modified(modified: SystemTime) -> String {
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => Local
            .timestamp_opt(duration.as_secs() as i64, duration.subsec_nanos())
            .single()
            .map(|dt| dt.format("%Y/%m/%d %H:%M").to_string())
            .unwrap_or_else(|| String::from("-")),
        Err(_) => String::from("-"),
    }
}
