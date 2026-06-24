use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use std::time::{Instant, SystemTime};

use eframe::egui::{self, Key};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::domain::app_settings::{
    normalize_external_tool_executable, AppSettings, ReadingDirection, ViewerQuality,
};
use crate::domain::archive::{BookId, BookMeta};
use crate::infra::archive::folder::FolderImageReader;
use crate::domain::archive_settings::{SettingsStore, SpreadMode};
use crate::domain::performance::{PerformanceResources, PerformanceSettingsResolved};
use crate::infra::cache::disk::DiskCache;
use crate::infra::ipc::{
    AdjacentBooksKind, ImageOrderSnapshot, IpcClient, LibraryToViewer, ViewerBookState,
    ViewerFavoriteState, ViewerToLibrary,
};
use crate::infra::page_map::viewer_bootstrap::bootstrap_viewer_page_map;
use crate::infra::worker::external_tool_worker::{
    ExternalToolRunRequest, ExternalToolRunResult, ExternalToolWorker,
};
use crate::ui::i18n::{tr, TextKey};
use crate::ui::thumb_cache::load_disk_thumb_texture;
use crate::ui::viewer::{
    self, BoundaryPreviewDirection, ExternalToolButtonModel, ExternalToolToolbarState,
    ExternalToolTrigger, ViewerAction, ViewerState, ViewerUiCapabilities,
};
use crate::util::archive_path::is_supported_archive_path;
use crate::{LaunchOptions, StartupMode};

use super::ui_helpers::{dialog_button_row, setup_style, DialogButtonSpec};
use crate::ui::{icons, theme};

#[cfg(windows)]
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    UI::WindowsAndMessaging::{
        AdjustWindowRectEx, GetWindowLongPtrW, GetWindowPlacement, IsZoomed, SetWindowPlacement,
        GWL_EXSTYLE, GWL_STYLE, SW_SHOWMAXIMIZED, WINDOWPLACEMENT, WINDOW_EX_STYLE, WINDOW_STYLE,
    },
};

const READING_SESSION_ACK_TIMEOUT: Duration = Duration::from_millis(800);
const LIBRARY_IPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const LIBRARY_ACTION_RETRY_BUDGET: u8 = 1;
const EXTERNAL_TOOL_SUCCESS_FEEDBACK_DURATION: Duration = Duration::from_secs(3);
const EXTERNAL_TOOL_UI_REPAINT_INTERVAL: Duration = Duration::from_millis(50);
const STARTUP_RESTORE_RECT_MAX_ATTEMPTS: u8 = 10;
const STARTUP_MAXIMIZE_TRIGGER_FRAME: u32 = 1;

pub struct ViewerApp {
    state: ViewerState,
    settings: SettingsStore,
    app_settings: AppSettings,
    mode: ViewerMode,
    is_fullscreen: bool,
    opened_as_fullscreen: bool,
    delete_dialog_open: bool,
    delete_dialog_choice: ViewerDeleteDialogChoice,
    delete_dialog_online: DeleteDialogOnlineState,
    book_state: ViewerBookState,
    favorite_state: ViewerFavoriteState,
    pending_favorite_toggle_previous_state: Option<ViewerFavoriteState>,
    map_make_skip: bool,
    external_tool_worker: ExternalToolWorker,
    external_tool_running: Option<ExternalToolRunning>,
    external_tool_ui_state: ExternalToolUiState,
    external_tool_next_request_id: u64,
    boundary_preview_disk_cache: Option<DiskCache>,
    performance_settings: PerformanceSettingsResolved,
    ipc_retry_budget: u8,
    pending_startup_maximize: bool,
    startup_maximize_sent: bool,
    startup_maximize_frame_count: u32,
    startup_restore_rect_adjustment_pending: bool,
    startup_restore_rect_adjustment_attempts: u8,
    saved_viewer_win_pos: Option<[f32; 2]>,
    saved_viewer_win_size: Option<[f32; 2]>,
    saved_viewer_window_maximized: Option<bool>,
    image_order_snapshot_applied: bool,
}

enum ViewerMode {
    Standalone,
    Library {
        request_tx: mpsc::Sender<ViewerToLibrary>,
        event_rx: mpsc::Receiver<IpcEvent>,
        last_request_id: u64,
        pending_action: Option<PendingLibraryAction>,
        pending_action_request_id: Option<u64>,
        pending_viewer_state_request_id: Option<u64>,
        pending_favorite_toggle_request_id: Option<u64>,
    },
    SnapshotOnly {
        request_tx: mpsc::Sender<ViewerToLibrary>,
        event_rx: mpsc::Receiver<IpcEvent>,
        last_request_id: u64,
        pending_viewer_state_request_id: Option<u64>,
    },
    Detached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewerDeleteDialogChoice {
    DeleteAndClose,
    DeleteAndNext,
    Cancel,
}

#[derive(Clone, Copy, Debug)]
enum PendingLibraryAction {
    Prev,
    Next,
    DeleteAndClose,
    DeleteAndNext,
    DeleteDialogProbe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeleteDialogOnlineState {
    Unknown,
    Checking,
    Online,
    Offline,
}

enum IpcEvent {
    Message(LibraryToViewer),
    Disconnected,
}

struct ExternalToolRunning {
    request_id: u64,
    tool_index: usize,
    path: std::path::PathBuf,
}

enum ExternalToolUiState {
    Idle,
    Running {
        tool_index: usize,
        path: std::path::PathBuf,
    },
    Success {
        tool_index: usize,
        path: std::path::PathBuf,
        until: Instant,
    },
    Failed {
        tool_index: usize,
        path: std::path::PathBuf,
    },
}

impl ViewerMode {
    fn can_navigate_book(&self) -> bool {
        matches!(self, Self::Library { .. })
    }

    fn can_delete_and_next(&self) -> bool {
        matches!(self, Self::Library { .. })
    }

    fn ui_capabilities(&self) -> ViewerUiCapabilities {
        ViewerUiCapabilities {
            allow_delete: self.can_delete_and_next(),
            allow_book_navigation: self.can_navigate_book(),
            allow_favorite_toggle: self.can_navigate_book(),
        }
    }
}

impl ViewerApp {
    fn viewer_text(&self, key: TextKey) -> &'static str {
        tr(self.app_settings.ui_language, key)
    }

    fn mark_viewer_feedback(&mut self, key: TextKey, now: Instant) {
        let text = self.viewer_text(key);
        self.state.mark_key_feedback(text, now);
    }

    fn request_viewer_state_sync(
        mode: &mut ViewerMode,
        book_state: &mut ViewerBookState,
        path: &Path,
    ) {
        let (request_tx, last_request_id, pending_viewer_state_request_id) = match mode {
            ViewerMode::Library {
                request_tx,
                last_request_id,
                pending_viewer_state_request_id,
                ..
            }
            | ViewerMode::SnapshotOnly {
                request_tx,
                last_request_id,
                pending_viewer_state_request_id,
                ..
            } => (request_tx, last_request_id, pending_viewer_state_request_id),
            _ => return,
        };

        *last_request_id = last_request_id.saturating_add(1);
        let request_id = *last_request_id;
        book_state.favorite_state = ViewerFavoriteState::Unknown;
        *pending_viewer_state_request_id = Some(request_id);
        let request = ViewerToLibrary::RequestViewerState {
            request_id,
            current_path: path.to_path_buf(),
        };
        if request_tx.send(request).is_err() {
            tracing::warn!(
                request_id,
                path = %path.display(),
                "viewer.ipc.viewer_state.request.send.failed"
            );
            *pending_viewer_state_request_id = None;
        }
    }

    fn send_reading_session_finished(&mut self, wait_for_ack: bool) {
        if !matches!(self.mode, ViewerMode::Library { .. }) {
            return;
        }
        let book_path = self.state.entry().path.as_ref().to_path_buf();
        let Some(snapshot) = self.state.take_reading_session_snapshot() else {
            return;
        };
        let ViewerMode::Library {
            request_tx,
            last_request_id,
            event_rx,
            ..
        } = &mut self.mode
        else {
            return;
        };
        *last_request_id = last_request_id.saturating_add(1);
        let request_id = *last_request_id;
        let request = ViewerToLibrary::ReadingSessionFinished {
            request_id,
            book_path,
            displayed_any_page: snapshot.displayed_any_page,
            reached_end: snapshot.reached_end,
            resume_page: snapshot.resume_page,
            page_count: snapshot.page_count,
        };
        if request_tx.send(request).is_err() {
            tracing::warn!(
                request_id,
                path = %self.state.entry().path.display(),
                "viewer.ipc.reading_session_finished.request.send.failed"
            );
            self.state.complete_reading_session_notification();
            return;
        }
        if !wait_for_ack {
            return;
        }

        let timeout = READING_SESSION_ACK_TIMEOUT;
        let started_at = Instant::now();
        loop {
            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                tracing::warn!(
                    request_id,
                    timeout_ms = timeout.as_millis() as u64,
                    "viewer.ipc.reading_session_finished.ack.timeout"
                );
                return;
            }
            let remaining = timeout - elapsed;
            match event_rx.recv_timeout(remaining) {
                Ok(IpcEvent::Message(LibraryToViewer::ReadingSessionFinishedAck {
                    request_id: ack,
                })) if ack == request_id => {
                    self.state.complete_reading_session_notification();
                    return;
                }
                Ok(IpcEvent::Message(_)) => {
                    continue;
                }
                Ok(IpcEvent::Disconnected) => {
                    tracing::warn!(
                        request_id,
                        timeout_ms = timeout.as_millis() as u64,
                        "viewer.ipc.reading_session_finished.ack.wait.disconnected"
                    );
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        request_id,
                        timeout_ms = timeout.as_millis() as u64,
                        "viewer.ipc.reading_session_finished.ack.timeout"
                    );
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::warn!(
                        request_id,
                        timeout_ms = timeout.as_millis() as u64,
                        "viewer.ipc.reading_session_finished.ack.wait.disconnected"
                    );
                    return;
                }
            }
        }
    }

    fn request_favorite_toggle(&mut self) -> anyhow::Result<()> {
        let current_path = self.state.entry().path.as_ref().to_path_buf();
        let current_path_display = current_path.display().to_string();
        let current_state = self.favorite_state;
        let (request_tx, request_id) = {
            let ViewerMode::Library {
                request_tx,
                last_request_id,
                pending_favorite_toggle_request_id,
                ..
            } = &mut self.mode
            else {
                anyhow::bail!("viewer mode is not library");
            };

            if pending_favorite_toggle_request_id.is_some() {
                anyhow::bail!("favorite toggle already in flight");
            }

            *last_request_id = last_request_id.saturating_add(1);
            let request_id = *last_request_id;
            *pending_favorite_toggle_request_id = Some(request_id);
            (request_tx.clone(), request_id)
        };
        self.pending_favorite_toggle_previous_state = Some(current_state);
        self.favorite_state = ViewerFavoriteState::Unknown;
        self.book_state.favorite_state = ViewerFavoriteState::Unknown;
        let request = ViewerToLibrary::FavoriteToggle {
            request_id,
            current_path,
        };
        if request_tx.send(request).is_err() {
            tracing::warn!(
                request_id,
                path = %current_path_display,
                "viewer.ipc.favorite_toggle.request.send.failed"
            );
            self.restore_pending_favorite_toggle_state();
            anyhow::bail!("ipc request queue disconnected");
        }
        Ok(())
    }

    fn open_boundary_preview_disk_cache() -> Option<DiskCache> {
        DiskCache::open(DiskCache::default_root())
            .or_else(|_| {
                DiskCache::open(
                    std::env::temp_dir()
                        .join(crate::app_identity::app_data_dir())
                        .join("thumbs"),
                )
            })
            .ok()
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        session: crate::session::SessionState,
        launch: LaunchOptions,
    ) -> anyhow::Result<Self> {
        let path = launch
            .startup_select_path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("viewer mode requires archive path"))?;
        if !(path.is_dir() || is_supported_archive_path(path.as_path())) {
            anyhow::bail!("unsupported archive path: {}", path.display());
        }
        setup_style(&cc.egui_ctx);

        let performance_resources: PerformanceResources =
            crate::infra::system_resources::detect_pc_resources();
        let app_settings = AppSettings::load_with_resources(&performance_resources);
        let performance_settings =
            app_settings.normalized_performance_settings(&performance_resources);
        tracing::debug!(
            "[viewer.settings.applied] source=app_settings viewer_quality={:?} viewer_l1_vram_cache_max_mb={} viewer_l2_ram_cache_max_mib={} viewer_background_worker_count={}",
            app_settings.viewer_quality,
            performance_settings.l1_vram_cache_max_mib,
            performance_settings.l2_ram_cache_max_mib,
            performance_settings.background_worker_count
        );

        let mut mode = match launch.mode {
            StartupMode::ViewerStandalone => ViewerMode::Standalone,
            StartupMode::ViewerLibrary => match launch.pipe_name.as_deref() {
                Some(pipe_name) => match IpcClient::connect(pipe_name, LIBRARY_IPC_CONNECT_TIMEOUT)
                {
                    Ok(client) => {
                        let (request_tx, request_rx) = mpsc::channel::<ViewerToLibrary>();
                        let (event_tx, event_rx) = mpsc::channel::<IpcEvent>();
                        let repaint_ctx = cc.egui_ctx.clone();
                        let io_spawned = std::thread::Builder::new()
                            .name("viewer-ipc-io".to_owned())
                            .spawn(move || {
                                let mut conn = client;
                                while let Ok(request) = request_rx.recv() {
                                    tracing::debug!(request = ?request, "viewer.ipc.writer.send.begin");
                                    if conn.send_to_library(&request).is_err() {
                                        tracing::warn!("viewer.ipc.writer.send.failed");
                                        let _ = event_tx.send(IpcEvent::Disconnected);
                                        repaint_ctx.request_repaint();
                                        break;
                                    }
                                    tracing::debug!(request = ?request, "viewer.ipc.writer.send.done");
                                    match conn.recv_from_library() {
                                        Ok(msg) => {
                                            if event_tx.send(IpcEvent::Message(msg)).is_err() {
                                                break;
                                            }
                                            repaint_ctx.request_repaint();
                                        }
                                        Err(_) => {
                                            let _ = event_tx.send(IpcEvent::Disconnected);
                                            repaint_ctx.request_repaint();
                                            break;
                                        }
                                    }
                                }
                            })
                            .is_ok();
                        if !io_spawned {
                            tracing::warn!(
                                io_spawned,
                                "ipc worker spawn failed; downgrade to detached"
                            );
                            ViewerMode::Detached
                        } else {
                            let common = (
                                request_tx,
                                event_rx,
                            );
                            if launch.viewer_snapshot_only_ipc {
                                ViewerMode::SnapshotOnly {
                                    request_tx: common.0,
                                    event_rx: common.1,
                                    last_request_id: 0,
                                    pending_viewer_state_request_id: None,
                                }
                            } else {
                                ViewerMode::Library {
                                    request_tx: common.0,
                                    event_rx: common.1,
                                    last_request_id: 0,
                                    pending_action: None,
                                    pending_action_request_id: None,
                                    pending_viewer_state_request_id: None,
                                    pending_favorite_toggle_request_id: None,
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ipc connect failed; downgrade to detached");
                        ViewerMode::Detached
                    }
                },
                None => ViewerMode::Detached,
            },
            StartupMode::Library => ViewerMode::Standalone,
        };

        let entry = book_meta_from_path(path.as_path());
        let settings = SettingsStore::load();
        let file_settings = settings.get(&entry.path);
        let effective_quality = file_settings
            .quality_override
            .unwrap_or(app_settings.viewer_quality);
        let page_map_mode =
            Self::viewer_page_map_mode_for_launch(&entry, launch.map_make_skip, &mode);
        let full_equivalent_size_hint = cc
            .egui_ctx
            .input(|i| i.viewport().monitor_size)
            .filter(|s| s.x > 1.0 && s.y > 1.0)
            .map(|monitor_size_points| viewer::FullEquivalentSizeHint {
                monitor_size_points,
                source: viewer::FullEquivalentSizeHintSource::ViewerViewport,
            });
        let mut book_state = ViewerBookState {
            favorite_state: ViewerFavoriteState::Unknown,
            reading_state: file_settings.reading_state,
            start_page: launch.viewer_start_page.map(|page| page as usize),
        };
        let state = ViewerState::new(
            cc.egui_ctx.clone(),
            viewer::ViewerStateInit {
                entry,
                start_page: book_state.start_page.map(|page| page as u32).unwrap_or(0),
                cover_blank: file_settings.cover_blank,
                quality_override: file_settings.quality_override,
                global_reading_direction: app_settings.reading_direction,
                reading_direction_override: file_settings.reading_direction_override,
                spread_setting: file_settings.spread_mode,
                performance_settings,
                quality: effective_quality,
                slideshow_interval_secs: file_settings.slideshow_interval_secs,
                full_equivalent_size_hint,
                page_map_mode,
            },
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        let favorite_state = book_state.favorite_state;
        Self::request_viewer_state_sync(&mut mode, &mut book_state, path.as_path());
        let saved_viewer_win_pos = session
            .viewer_window_x
            .zip(session.viewer_window_y)
            .map(|(x, y)| [x, y]);
        let saved_viewer_win_size = session
            .viewer_window_w
            .zip(session.viewer_window_h)
            .and_then(|(w, h)| {
                if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
                    Some([w, h])
                } else {
                    None
                }
            });
        let saved_viewer_window_maximized = session.viewer_window_maximized;

        let pending_startup_maximize =
            !launch.start_fullscreen && session.viewer_window_maximized.unwrap_or(false);
        let startup_restore_rect_adjustment_pending = pending_startup_maximize
            && saved_viewer_win_pos.is_some()
            && saved_viewer_win_size.is_some();
        tracing::debug!(
            pending_startup_maximize,
            start_fullscreen = launch.start_fullscreen,
            "viewer startup maximize policy initialized"
        );

        if launch.start_fullscreen {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        set_viewer_title(&cc.egui_ctx, path.as_path());

        Ok(Self {
            state,
            settings,
            app_settings,
            mode,
            is_fullscreen: launch.start_fullscreen,
            opened_as_fullscreen: launch.mode == StartupMode::ViewerLibrary
                && launch.start_fullscreen,
            delete_dialog_open: false,
            delete_dialog_choice: ViewerDeleteDialogChoice::DeleteAndNext,
            delete_dialog_online: DeleteDialogOnlineState::Unknown,
            book_state,
            favorite_state,
            pending_favorite_toggle_previous_state: None,
            map_make_skip: launch.map_make_skip,
            external_tool_worker: ExternalToolWorker::spawn(),
            external_tool_running: None,
            external_tool_ui_state: ExternalToolUiState::Idle,
            external_tool_next_request_id: 1,
            boundary_preview_disk_cache: Self::open_boundary_preview_disk_cache(),
            performance_settings,
            ipc_retry_budget: LIBRARY_ACTION_RETRY_BUDGET,
            pending_startup_maximize,
            startup_maximize_sent: false,
            startup_maximize_frame_count: 0,
            startup_restore_rect_adjustment_pending,
            startup_restore_rect_adjustment_attempts: 0,
            saved_viewer_win_pos,
            saved_viewer_win_size,
            saved_viewer_window_maximized,
            image_order_snapshot_applied: false,
        })
    }

    fn reopen_to_path(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
        book_state: ViewerBookState,
        force_page_map_unavailable: bool,
    ) -> anyhow::Result<()> {
        let started_at = Instant::now();
        self.send_reading_session_finished(false);
        tracing::info!(
            target_path = %path.display(),
            "viewer.ipc.apply_navigate.start"
        );
        self.state.close_boundary_preview();
        let entry = book_meta_from_path(path);
        let file_settings = self.settings.get(&entry.path);
        let effective_quality = file_settings
            .quality_override
            .unwrap_or(self.app_settings.viewer_quality);
        let page_map_mode = if force_page_map_unavailable {
            crate::infra::page_map::viewer_bootstrap::ViewerPageMapMode::Unavailable
        } else {
            Self::viewer_page_map_mode_for_launch(&entry, self.map_make_skip, &self.mode)
        };
        let full_equivalent_size_hint = ctx
            .input(|i| i.viewport().monitor_size)
            .filter(|s| s.x > 1.0 && s.y > 1.0)
            .map(|monitor_size_points| viewer::FullEquivalentSizeHint {
                monitor_size_points,
                source: viewer::FullEquivalentSizeHintSource::ViewerViewport,
            });
        self.state.clear_gpu_texture_history("book_changed");
        self.state.flush_worker();
        self.state = ViewerState::new(
            ctx.clone(),
            viewer::ViewerStateInit {
                entry,
                start_page: book_state.start_page.map(|page| page as u32).unwrap_or(0),
                cover_blank: file_settings.cover_blank,
                quality_override: file_settings.quality_override,
                global_reading_direction: self.app_settings.reading_direction,
                reading_direction_override: file_settings.reading_direction_override,
                spread_setting: file_settings.spread_mode,
                performance_settings: self.performance_settings,
                quality: effective_quality,
                slideshow_interval_secs: file_settings.slideshow_interval_secs,
                full_equivalent_size_hint,
                page_map_mode,
            },
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        self.book_state = book_state;
        self.favorite_state = book_state.favorite_state;
        set_viewer_title(ctx, path);
        tracing::info!(
            target_path = %path.display(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "viewer.ipc.apply_navigate.done"
        );
        Ok(())
    }

    fn handle_ipc_navigation(
        &mut self,
        request: ViewerToLibrary,
        action: PendingLibraryAction,
        request_id: u64,
    ) -> anyhow::Result<()> {
        let ViewerMode::Library {
            request_tx,
            pending_action,
            pending_action_request_id,
            ..
        } = &mut self.mode
        else {
            anyhow::bail!("viewer mode is not library");
        };
        tracing::info!(
            request_id,
            action = ?action,
            request = ?request,
            "viewer.ipc.request.send.begin"
        );
        request_tx
            .send(request)
            .map_err(|_| anyhow::anyhow!("ipc request queue disconnected"))?;
        tracing::info!(
            request_id,
            action = ?action,
            "viewer.ipc.request.send.done"
        );
        *pending_action = Some(action);
        *pending_action_request_id = Some(request_id);
        self.ipc_retry_budget = LIBRARY_ACTION_RETRY_BUDGET;
        Ok(())
    }

    fn send_boundary_preview_request(&mut self) {
        if !matches!(self.mode, ViewerMode::Library { .. }) {
            self.state.close_boundary_preview();
            return;
        }
        if !self.state.boundary_preview_needs_request() {
            return;
        }
        let ViewerMode::Library {
            request_tx,
            last_request_id,
            ..
        } = &mut self.mode
        else {
            return;
        };
        *last_request_id = last_request_id.saturating_add(1);
        let request_id = *last_request_id;
        let request = ViewerToLibrary::RequestAdjacentBooks {
            request_id,
            kind: AdjacentBooksKind::BoundaryPreview,
        };
        if request_tx.send(request).is_err() {
            self.state.close_boundary_preview();
            return;
        }
        self.state.boundary_preview_mark_request_sent(request_id);
    }

    fn book_meta_for_preview_path(path: &Path) -> Option<BookMeta> {
        let metadata = std::fs::metadata(path).ok()?;
        let title = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Some(BookMeta {
            id: BookId::from_path(path),
            path: Arc::from(path),
            title: Arc::from(title),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            page_count: None,
        })
    }

    fn warn_boundary_preview_thumb_miss(request_id: u64, book: &BookMeta) {
        tracing::warn!(
            request_id,
            path = %book.path.display(),
            "boundary preview thumbnail cache miss"
        );
    }

    fn load_boundary_preview_thumbnail(
        &mut self,
        ctx: &egui::Context,
        request_id: u64,
        book: &BookMeta,
    ) -> bool {
        let Some(cache) = self.boundary_preview_disk_cache.as_ref() else {
            Self::warn_boundary_preview_thumb_miss(request_id, book);
            return false;
        };
        let texture_name = format!(
            "boundary_preview_thumb_{}_{}",
            book.id.0.to_hex(),
            request_id
        );
        let Some(thumb) = load_disk_thumb_texture(
            ctx,
            cache,
            book.path.as_ref(),
            book.size,
            Some(book.modified),
            texture_name,
        ) else {
            Self::warn_boundary_preview_thumb_miss(request_id, book);
            return false;
        };
        self.state.boundary_preview_set_thumbnail(request_id, thumb)
    }

    fn poll_library_ipc(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        {
            match &mut self.mode {
                ViewerMode::Library { event_rx, .. } | ViewerMode::SnapshotOnly { event_rx, .. } => {
                    while let Ok(event) = event_rx.try_recv() {
                        events.push(event);
                    }
                }
                _ => return,
            }
        }
        let snapshot_only_mode = matches!(self.mode, ViewerMode::SnapshotOnly { .. });
        for event in events {
            match event {
                IpcEvent::Disconnected => {
                    tracing::warn!("viewer.ipc.disconnected");
                    let had_pending_favorite_toggle =
                        self.pending_favorite_toggle_previous_state.is_some();
                    self.restore_pending_favorite_toggle_state();
                    if had_pending_favorite_toggle {
                        self.mark_viewer_feedback(TextKey::FavoriteUpdateFailed, Instant::now());
                    }
                    self.state.close_boundary_preview();
                    self.mode = ViewerMode::Detached;
                    return;
                }
                IpcEvent::Message(msg) => match msg {
                    LibraryToViewer::ResponseViewerState {
                        request_id,
                        book_state,
                        image_order_snapshot,
                    } => {
                        if self.is_current_viewer_state_request(request_id) {
                            let book_state = self.apply_image_order_snapshot_if_needed(
                                ctx,
                                book_state,
                                image_order_snapshot,
                            );
                            self.state
                                .apply_start_page_before_initial_load(book_state.start_page);
                            self.book_state = book_state;
                            self.favorite_state = book_state.favorite_state;
                            self.clear_pending_viewer_state_request();
                            if snapshot_only_mode {
                                self.mode = ViewerMode::Detached;
                            }
                        }
                    }
                    LibraryToViewer::Deleted {
                        request_id,
                        next_path,
                        next_book_state,
                        ..
                    } => {
                        if self.is_current_library_request(request_id) {
                            match self.current_pending_library_action() {
                                Some(PendingLibraryAction::DeleteAndClose) => {
                                    self.send_reading_session_finished(true);
                                    self.state.close_boundary_preview();
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                                Some(PendingLibraryAction::DeleteAndNext) => {
                                    self.state.close_boundary_preview();
                                    if let (Some(path), Some(book_state)) =
                                        (next_path, next_book_state)
                                    {
                                        if let Err(e) =
                                            self.reopen_to_path(
                                                ctx,
                                                path.as_path(),
                                                book_state,
                                                false,
                                            )
                                        {
                                            tracing::warn!(error = %e, "ipc delete-next apply failed");
                                        }
                                    } else {
                                        self.send_reading_session_finished(true);
                                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                    }
                                }
                                _ => {}
                            }
                            self.clear_pending_library_action();
                        }
                    }
                    LibraryToViewer::NavigateTo {
                        request_id,
                        path,
                        book_state,
                    } => {
                        if self.is_current_library_request(request_id) {
                            if matches!(
                                self.current_pending_library_action(),
                                Some(PendingLibraryAction::DeleteDialogProbe)
                            ) {
                                self.delete_dialog_online = DeleteDialogOnlineState::Online;
                            }
                            self.state.close_boundary_preview();
                            if let Err(e) =
                                self.reopen_to_path(ctx, path.as_path(), book_state, false)
                            {
                                tracing::warn!(error = %e, "ipc navigate apply failed");
                            }
                            self.clear_pending_library_action();
                        }
                    }
                    LibraryToViewer::NoMoreBooks { request_id } => {
                        if self.is_current_library_request(request_id) {
                            let pending_action = self.current_pending_library_action();
                            // 「移動先なし」のときだけ文言を出す。成功時は遷移後の状態に任せて上書きしない。
                            if matches!(pending_action, Some(PendingLibraryAction::Prev)) {
                                self.mark_viewer_feedback(TextKey::NoPreviousBook, Instant::now());
                            } else if matches!(pending_action, Some(PendingLibraryAction::Next)) {
                                self.mark_viewer_feedback(TextKey::NoNextBook, Instant::now());
                            }
                            if matches!(
                                pending_action,
                                Some(PendingLibraryAction::DeleteDialogProbe)
                            ) {
                                self.delete_dialog_online = DeleteDialogOnlineState::Online;
                            }
                            if matches!(pending_action, Some(PendingLibraryAction::DeleteAndNext)) {
                                self.send_reading_session_finished(true);
                                self.state.close_boundary_preview();
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            self.clear_pending_library_action();
                        }
                    }
                    LibraryToViewer::Error {
                        request_id,
                        code,
                        retryable,
                    } => {
                        if self.state.boundary_preview_clear_if_matches(request_id) {
                            continue;
                        }
                        tracing::warn!(
                            request_id,
                            code = ?code,
                            retryable,
                            pending_action = ?self.current_pending_library_action(),
                            "viewer.ipc.response.error"
                        );
                        if self.is_current_library_request(request_id) {
                            let retried = retryable && self.retry_pending_library_action();
                            if !retried {
                                if matches!(
                                    self.current_pending_library_action(),
                                    Some(PendingLibraryAction::DeleteDialogProbe)
                                        | Some(PendingLibraryAction::DeleteAndNext)
                                        | Some(PendingLibraryAction::DeleteAndClose)
                                ) {
                                    self.delete_dialog_online = DeleteDialogOnlineState::Offline;
                                    self.mark_viewer_feedback(
                                        TextKey::DeleteFailed,
                                        Instant::now(),
                                    );
                                }
                                self.clear_pending_library_action();
                            }
                        } else if self.is_current_favorite_toggle_request(request_id) {
                            tracing::warn!(
                                request_id,
                                code = ?code,
                                retryable,
                                "viewer.ipc.favorite_toggle.error"
                            );
                            self.restore_pending_favorite_toggle_state();
                            self.mark_viewer_feedback(
                                TextKey::FavoriteUpdateFailed,
                                Instant::now(),
                            );
                        }
                    }
                    LibraryToViewer::FavoriteToggleResponse {
                        request_id,
                        favorite_state,
                    } => {
                        if self.is_current_favorite_toggle_request(request_id) {
                            self.favorite_state = favorite_state;
                            self.book_state.favorite_state = favorite_state;
                            self.pending_favorite_toggle_previous_state = None;
                            self.clear_pending_favorite_toggle_request();
                        }
                    }
                    LibraryToViewer::ReadingSessionFinishedAck { request_id: _ } => {}
                    LibraryToViewer::AdjacentBooks {
                        request_id,
                        kind,
                        prev,
                        next,
                    } => match kind {
                        AdjacentBooksKind::DeleteDialog => {
                            if self.is_current_library_request(request_id)
                                && matches!(
                                    self.current_pending_library_action(),
                                    Some(PendingLibraryAction::DeleteDialogProbe)
                                )
                            {
                                self.delete_dialog_online = DeleteDialogOnlineState::Online;
                                self.clear_pending_library_action();
                            }
                        }
                        AdjacentBooksKind::BoundaryPreview => {
                            let Some(probe) = self
                                .state
                                .boundary_preview_direction_for_request(request_id)
                            else {
                                continue;
                            };
                            let candidate_path = match probe {
                                BoundaryPreviewDirection::Previous => prev.as_deref(),
                                BoundaryPreviewDirection::Next => next.as_deref(),
                            };
                            let Some(path) = candidate_path else {
                                let _ = self.state.boundary_preview_clear_if_matches(request_id);
                                continue;
                            };
                            let Some(meta) = Self::book_meta_for_preview_path(path) else {
                                let _ = self.state.boundary_preview_clear_if_matches(request_id);
                                continue;
                            };
                            if !self.state.boundary_preview_mark_ready(request_id, meta) {
                                let _ = self.state.boundary_preview_clear_if_matches(request_id);
                                continue;
                            }
                            let Some(book) = self.state.boundary_preview_ready_book().cloned()
                            else {
                                let _ = self.state.boundary_preview_clear_if_matches(request_id);
                                continue;
                            };
                            if !self.load_boundary_preview_thumbnail(ctx, request_id, &book) {
                                let _ = self.state.boundary_preview_clear_if_matches(request_id);
                            }
                        }
                    },
                },
            }
        }
    }

    fn is_current_library_request(&self, request_id: u64) -> bool {
        match &self.mode {
            ViewerMode::Library {
                pending_action_request_id,
                ..
            } => pending_action_request_id == &Some(request_id),
            _ => false,
        }
    }

    fn is_current_viewer_state_request(&self, request_id: u64) -> bool {
        match &self.mode {
            ViewerMode::Library {
                pending_viewer_state_request_id,
                ..
            }
            | ViewerMode::SnapshotOnly {
                pending_viewer_state_request_id,
                ..
            } => pending_viewer_state_request_id == &Some(request_id),
            _ => false,
        }
    }

    fn is_current_favorite_toggle_request(&self, request_id: u64) -> bool {
        match &self.mode {
            ViewerMode::Library {
                pending_favorite_toggle_request_id,
                ..
            } => pending_favorite_toggle_request_id == &Some(request_id),
            _ => false,
        }
    }

    fn clear_pending_library_action(&mut self) {
        if let ViewerMode::Library {
            pending_action,
            pending_action_request_id,
            ..
        } = &mut self.mode
        {
            *pending_action = None;
            *pending_action_request_id = None;
        }
    }

    fn clear_pending_viewer_state_request(&mut self) {
        match &mut self.mode {
            ViewerMode::Library {
                pending_viewer_state_request_id,
                ..
            }
            | ViewerMode::SnapshotOnly {
                pending_viewer_state_request_id,
                ..
            } => {
                *pending_viewer_state_request_id = None;
            }
            _ => {}
        }
    }

    fn clear_pending_favorite_toggle_request(&mut self) {
        if let ViewerMode::Library {
            pending_favorite_toggle_request_id,
            ..
        } = &mut self.mode
        {
            *pending_favorite_toggle_request_id = None;
        }
    }

    fn restore_pending_favorite_toggle_state(&mut self) {
        if let Some(previous) = self.pending_favorite_toggle_previous_state.take() {
            self.favorite_state = previous;
            self.book_state.favorite_state = previous;
        }
        self.clear_pending_favorite_toggle_request();
    }

    fn current_pending_library_action(&self) -> Option<PendingLibraryAction> {
        match &self.mode {
            ViewerMode::Library { pending_action, .. } => *pending_action,
            _ => None,
        }
    }

    fn retry_pending_library_action(&mut self) -> bool {
        if self.ipc_retry_budget == 0 {
            return false;
        }
        let Some(action) = self.current_pending_library_action() else {
            return false;
        };
        let ViewerMode::Library {
            ref mut last_request_id,
            ..
        } = self.mode
        else {
            return false;
        };
        self.ipc_retry_budget = self.ipc_retry_budget.saturating_sub(1);
        *last_request_id = last_request_id.saturating_add(1);
        let request_id = *last_request_id;
        let request = match action {
            PendingLibraryAction::Prev => ViewerToLibrary::RequestPrevBook { request_id },
            PendingLibraryAction::Next => ViewerToLibrary::RequestNextBook { request_id },
            PendingLibraryAction::DeleteAndClose => ViewerToLibrary::Delete {
                request_id,
                book_id: BookId::from_path(self.state.entry().path.as_ref()),
            },
            PendingLibraryAction::DeleteDialogProbe => ViewerToLibrary::RequestAdjacentBooks {
                request_id,
                kind: AdjacentBooksKind::DeleteDialog,
            },
            PendingLibraryAction::DeleteAndNext => ViewerToLibrary::DeleteAndNext {
                request_id,
                book_id: BookId::from_path(self.state.entry().path.as_ref()),
            },
        };
        self.handle_ipc_navigation(request, action, request_id)
            .is_ok()
    }

    fn apply_image_order_snapshot_if_needed(
        &mut self,
        ctx: &egui::Context,
        mut book_state: ViewerBookState,
        image_order_snapshot: Option<ImageOrderSnapshot>,
    ) -> ViewerBookState {
        let original_book_state = book_state;
        if self.image_order_snapshot_applied {
            return book_state;
        }
        let Some(snapshot) = image_order_snapshot else {
            return book_state;
        };
        let Some(start_page) = validate_image_order_snapshot(&snapshot) else {
            return book_state;
        };
        if FolderImageReader::install_viewer_order_override(
            snapshot.folder.as_path(),
            snapshot.ordered_images.clone(),
        )
        .is_err()
        {
            return book_state;
        }
        book_state.start_page = Some(start_page);
        self.image_order_snapshot_applied = true;
        if let Err(error) =
            self.reopen_to_path(ctx, snapshot.folder.as_path(), book_state, true)
        {
            tracing::warn!(
                error = %error,
                path = %snapshot.folder.display(),
                "viewer image order snapshot reopen failed"
            );
            FolderImageReader::clear_viewer_order_override(snapshot.folder.as_path());
            self.image_order_snapshot_applied = false;
            return original_book_state;
        }
        book_state
    }

    fn viewer_page_map_mode_for_launch(
        entry: &BookMeta,
        map_make_skip: bool,
        mode: &ViewerMode,
    ) -> crate::infra::page_map::viewer_bootstrap::ViewerPageMapMode {
        if matches!(mode, ViewerMode::SnapshotOnly { .. }) {
            return crate::infra::page_map::viewer_bootstrap::ViewerPageMapMode::Unavailable;
        }
        bootstrap_viewer_page_map(entry, map_make_skip)
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.state.close_boundary_preview();
        self.is_fullscreen = !self.is_fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
    }

    fn apply_delete_choice(&mut self, _ctx: &egui::Context) {
        let choice = self.delete_dialog_choice;
        self.delete_dialog_open = false;
        self.delete_dialog_choice = ViewerDeleteDialogChoice::DeleteAndNext;
        match choice {
            ViewerDeleteDialogChoice::Cancel => {}
            ViewerDeleteDialogChoice::DeleteAndClose => {
                if let ViewerMode::Library {
                    ref mut last_request_id,
                    ..
                } = self.mode
                {
                    *last_request_id = last_request_id.saturating_add(1);
                    let request_id = *last_request_id;
                    let book_id = BookId::from_path(self.state.entry().path.as_ref());
                    let result = self.handle_ipc_navigation(
                        ViewerToLibrary::Delete {
                            request_id,
                            book_id,
                        },
                        PendingLibraryAction::DeleteAndClose,
                        request_id,
                    );
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "delete-and-close failed; detached");
                        self.delete_dialog_online = DeleteDialogOnlineState::Offline;
                        self.mark_viewer_feedback(TextKey::DeleteFailed, Instant::now());
                        self.mode = ViewerMode::Detached;
                    }
                } else {
                    self.mark_viewer_feedback(TextKey::DeleteFailed, Instant::now());
                }
            }
            ViewerDeleteDialogChoice::DeleteAndNext => {
                if self.delete_dialog_online == DeleteDialogOnlineState::Offline {
                    self.mark_viewer_feedback(TextKey::DeleteFailed, Instant::now());
                    return;
                }
                if let ViewerMode::Library {
                    ref mut last_request_id,
                    ..
                } = self.mode
                {
                    *last_request_id = last_request_id.saturating_add(1);
                    let request_id = *last_request_id;
                    let book_id = BookId::from_path(self.state.entry().path.as_ref());
                    let result = self.handle_ipc_navigation(
                        ViewerToLibrary::DeleteAndNext {
                            request_id,
                            book_id,
                        },
                        PendingLibraryAction::DeleteAndNext,
                        request_id,
                    );
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "delete-and-next failed; detached");
                        self.delete_dialog_online = DeleteDialogOnlineState::Offline;
                        self.mark_viewer_feedback(TextKey::DeleteFailed, Instant::now());
                        self.mode = ViewerMode::Detached;
                    }
                } else {
                    self.mark_viewer_feedback(TextKey::DeleteFailed, Instant::now());
                }
            }
        }
    }

    fn begin_delete_dialog(&mut self) {
        self.delete_dialog_open = true;
        self.delete_dialog_choice = ViewerDeleteDialogChoice::DeleteAndNext;
        self.delete_dialog_online = DeleteDialogOnlineState::Unknown;
        if let ViewerMode::Library {
            ref mut last_request_id,
            ..
        } = self.mode
        {
            *last_request_id = last_request_id.saturating_add(1);
            let request_id = *last_request_id;
            self.delete_dialog_online = DeleteDialogOnlineState::Checking;
            let result = self.handle_ipc_navigation(
                ViewerToLibrary::RequestAdjacentBooks {
                    request_id,
                    kind: AdjacentBooksKind::DeleteDialog,
                },
                PendingLibraryAction::DeleteDialogProbe,
                request_id,
            );
            if result.is_err() {
                self.delete_dialog_online = DeleteDialogOnlineState::Offline;
                self.mode = ViewerMode::Detached;
            }
        }
    }

    fn show_delete_dialog(&mut self, ctx: &egui::Context) {
        if !self.delete_dialog_open {
            return;
        }
        let language = self.app_settings.ui_language;
        let title = tr(language, TextKey::DeleteConfirmTitle);
        let question = tr(language, TextKey::DeleteQuestion).replacen(
            "{}",
            self.state.entry().title.as_ref(),
            1,
        );
        let irreversible_note = tr(language, TextKey::IrreversibleActionNote);
        let delete_label = tr(language, TextKey::Delete);
        let delete_and_next_label = tr(language, TextKey::DeleteAndNextBook);
        let cancel_label = tr(language, TextKey::Cancel);
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(title)
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(500.0);
                let (left, right, enter, escape) = ui.input_mut(|i| {
                    (
                        i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft),
                        i.consume_key(egui::Modifiers::NONE, Key::ArrowRight),
                        i.consume_key(egui::Modifiers::NONE, Key::Enter),
                        i.consume_key(egui::Modifiers::NONE, Key::Escape),
                    )
                });
                if left {
                    self.delete_dialog_choice = match self.delete_dialog_choice {
                        ViewerDeleteDialogChoice::DeleteAndClose => {
                            ViewerDeleteDialogChoice::DeleteAndClose
                        }
                        ViewerDeleteDialogChoice::DeleteAndNext => {
                            ViewerDeleteDialogChoice::DeleteAndClose
                        }
                        ViewerDeleteDialogChoice::Cancel => ViewerDeleteDialogChoice::DeleteAndNext,
                    };
                }
                if right {
                    self.delete_dialog_choice = match self.delete_dialog_choice {
                        ViewerDeleteDialogChoice::DeleteAndClose => {
                            ViewerDeleteDialogChoice::DeleteAndNext
                        }
                        ViewerDeleteDialogChoice::DeleteAndNext => ViewerDeleteDialogChoice::Cancel,
                        ViewerDeleteDialogChoice::Cancel => ViewerDeleteDialogChoice::Cancel,
                    };
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(icons::icon(icons::ICON_DELETE, 22.0).color(theme::DELETE_RED));
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(question).color(theme::TEXT_MAIN));
                });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(irreversible_note)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_SUBTLE),
                );
                ui.add_space(20.0);
                let buttons = dialog_button_row(
                    ui,
                    31.0,
                    &[
                        DialogButtonSpec {
                            id: ui.id().with(("viewer_delete_dialog", "delete_and_close")),
                            label: delete_label,
                            width: 116.0,
                            is_default: self.delete_dialog_choice
                                == ViewerDeleteDialogChoice::DeleteAndClose,
                        },
                        DialogButtonSpec {
                            id: ui.id().with(("viewer_delete_dialog", "delete_and_next")),
                            label: delete_and_next_label,
                            width: 180.0,
                            is_default: self.delete_dialog_choice
                                == ViewerDeleteDialogChoice::DeleteAndNext,
                        },
                        DialogButtonSpec {
                            id: ui.id().with(("viewer_delete_dialog", "cancel")),
                            label: cancel_label,
                            width: 116.0,
                            is_default: self.delete_dialog_choice
                                == ViewerDeleteDialogChoice::Cancel,
                        },
                    ],
                );
                if buttons[0].clicked {
                    self.delete_dialog_choice = ViewerDeleteDialogChoice::DeleteAndClose;
                    confirmed = true;
                }
                if buttons[1].clicked {
                    self.delete_dialog_choice = ViewerDeleteDialogChoice::DeleteAndNext;
                    confirmed = true;
                }
                if buttons[2].clicked {
                    self.delete_dialog_choice = ViewerDeleteDialogChoice::Cancel;
                    cancelled = true;
                }
                ui.add_space(2.0);
                if enter {
                    match self.delete_dialog_choice {
                        ViewerDeleteDialogChoice::DeleteAndClose
                        | ViewerDeleteDialogChoice::DeleteAndNext => confirmed = true,
                        ViewerDeleteDialogChoice::Cancel => cancelled = true,
                    }
                }
                if escape {
                    cancelled = true;
                }
            });
        if confirmed {
            self.apply_delete_choice(ctx);
        } else if cancelled || !open {
            self.delete_dialog_open = false;
            self.delete_dialog_choice = ViewerDeleteDialogChoice::DeleteAndNext;
            self.delete_dialog_online = DeleteDialogOnlineState::Unknown;
        }
    }

    fn external_tool_button_models(&self) -> Vec<ExternalToolButtonModel> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (tool_index, tool) in self.app_settings.external_tools.iter().enumerate() {
            if tool.name.trim().is_empty()
                || tool.executable.trim().is_empty()
                || !AppSettings::external_tool_shortcut_candidates().contains(&tool.shortcut)
            {
                continue;
            }
            let key = super::external_tool::external_tool_shortcut_to_egui_key(tool.shortcut);
            if !seen.insert(key) {
                tracing::warn!(
                    "[external-tool] duplicate shortcut ignored key={} tool={} tool_index={}",
                    tool.shortcut.as_char(),
                    tool.name,
                    tool_index
                );
                continue;
            }
            out.push(ExternalToolButtonModel {
                tool_index,
                name: tool.name.clone(),
                shortcut: tool.shortcut.as_char(),
                key,
            });
        }
        out
    }

    fn external_tool_toolbar_state_for_ui(&self) -> ExternalToolToolbarState {
        match &self.external_tool_ui_state {
            ExternalToolUiState::Idle => ExternalToolToolbarState::Idle,
            ExternalToolUiState::Running { tool_index, path } => {
                ExternalToolToolbarState::Running {
                    tool_index: *tool_index,
                    path: path.clone(),
                }
            }
            ExternalToolUiState::Success {
                tool_index, path, ..
            } => ExternalToolToolbarState::Success {
                tool_index: *tool_index,
                path: path.clone(),
            },
            ExternalToolUiState::Failed { tool_index, path } => ExternalToolToolbarState::Failed {
                tool_index: *tool_index,
                path: path.clone(),
            },
        }
    }

    fn request_external_tool_run_from_trigger(
        &mut self,
        tool_index: usize,
        target_path: std::path::PathBuf,
        _trigger: ExternalToolTrigger,
    ) {
        if self.external_tool_running.is_some() {
            tracing::warn!(
                "[external-tool] request rejected busy tool_index={} path={}",
                tool_index,
                target_path.display()
            );
            return;
        }
        let Some(tool) = self.app_settings.external_tools.get(tool_index).cloned() else {
            return;
        };
        let request_id = self.external_tool_next_request_id;
        self.external_tool_next_request_id = self.external_tool_next_request_id.saturating_add(1);
        let req = ExternalToolRunRequest {
            request_id,
            tool_index,
            tool_name: tool.name.clone(),
            executable: normalize_external_tool_executable(&tool.executable),
            args: tool.args,
            background: tool.background,
            target_path: target_path.clone(),
            target_paths: vec![target_path.clone()],
            accepted_at: Instant::now(),
        };
        if !self.external_tool_worker.request(req) {
            return;
        }
        self.external_tool_running = Some(ExternalToolRunning {
            request_id,
            tool_index,
            path: target_path.clone(),
        });
        self.external_tool_ui_state = ExternalToolUiState::Running {
            tool_index,
            path: target_path,
        };
    }

    fn poll_external_tool_results(&mut self, ctx: &egui::Context) {
        let mut got_any = false;
        while let Some(result) = self.external_tool_worker.try_recv() {
            got_any = true;
            self.handle_external_tool_result(result);
        }
        if got_any {
            ctx.request_repaint();
        }
    }

    fn handle_external_tool_result(&mut self, result: ExternalToolRunResult) {
        let Some(running) = &self.external_tool_running else {
            return;
        };
        if running.request_id != result.request_id
            || running.tool_index != result.tool_index
            || running.path != result.target_path
        {
            return;
        }
        self.external_tool_running = None;
        if result.success {
            self.external_tool_ui_state = ExternalToolUiState::Success {
                tool_index: result.tool_index,
                path: result.target_path,
                until: Instant::now() + EXTERNAL_TOOL_SUCCESS_FEEDBACK_DURATION,
            };
        } else {
            self.external_tool_ui_state = ExternalToolUiState::Failed {
                tool_index: result.tool_index,
                path: result.target_path,
            };
        }
    }

    fn tick_external_tool_ui_state(&mut self) {
        if let ExternalToolUiState::Success { until, .. } = self.external_tool_ui_state {
            if Instant::now() >= until {
                self.external_tool_ui_state = ExternalToolUiState::Idle;
            }
        }
    }

    fn schedule_external_tool_state_repaint(&self, ctx: &egui::Context) {
        if self.external_tool_running.is_some() {
            ctx.request_repaint_after(EXTERNAL_TOOL_UI_REPAINT_INTERVAL);
        }
        if let ExternalToolUiState::Success { until, .. } = self.external_tool_ui_state {
            let now = Instant::now();
            if now < until {
                ctx.request_repaint_after(until.saturating_duration_since(now));
            }
        }
    }

    fn capture_viewer_window_state(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !self.is_fullscreen {
            self.saved_viewer_window_maximized = Some(maximized);
        }
        if self.is_fullscreen || maximized {
            return;
        }
        let Some(outer) = ctx.input(|i| i.viewport().outer_rect) else {
            return;
        };
        let Some(inner) = ctx.input(|i| i.viewport().inner_rect) else {
            return;
        };
        if outer.width() <= 0.0 || outer.height() <= 0.0 {
            return;
        }
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return;
        }
        self.saved_viewer_win_pos = Some([outer.min.x, outer.min.y]);
        self.saved_viewer_win_size = Some([inner.width(), inner.height()]);
    }

    fn save_viewer_window_geometry(&self) {
        let has_geometry =
            self.saved_viewer_win_pos.is_some() && self.saved_viewer_win_size.is_some();
        let has_maximized = self.saved_viewer_window_maximized.is_some();
        if !has_geometry && !has_maximized {
            return;
        };
        let mut session = crate::session::SessionState::load();
        if let (Some(pos), Some(size)) = (self.saved_viewer_win_pos, self.saved_viewer_win_size) {
            session.viewer_window_x = Some(pos[0]);
            session.viewer_window_y = Some(pos[1]);
            session.viewer_window_w = Some(size[0]);
            session.viewer_window_h = Some(size[1]);
        }
        if let Some(maximized) = self.saved_viewer_window_maximized {
            session.viewer_window_maximized = Some(maximized);
        }
        session.save();
    }

    #[cfg(windows)]
    fn main_window_hwnd(frame: &eframe::Frame) -> Option<HWND> {
        let handle = frame.window_handle().ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
            _ => None,
        }
    }

    #[cfg(not(windows))]
    fn main_window_hwnd(_frame: &eframe::Frame) -> Option<()> {
        None
    }

    fn maybe_apply_startup_restore_rect_adjustment(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        if !self.startup_restore_rect_adjustment_pending {
            return;
        }
        if self.is_fullscreen || !self.startup_maximize_sent {
            ctx.request_repaint();
            return;
        }
        if self.startup_restore_rect_adjustment_attempts >= STARTUP_RESTORE_RECT_MAX_ATTEMPTS {
            tracing::warn!(
                attempts = self.startup_restore_rect_adjustment_attempts,
                "viewer.startup_restore_rect.adjustment.give_up"
            );
            self.startup_restore_rect_adjustment_pending = false;
            return;
        }
        self.startup_restore_rect_adjustment_attempts = self
            .startup_restore_rect_adjustment_attempts
            .saturating_add(1);

        #[cfg(windows)]
        {
            let Some(hwnd) = Self::main_window_hwnd(frame) else {
                ctx.request_repaint();
                return;
            };
            let egui_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            let mut placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            let placement_ok = unsafe { GetWindowPlacement(hwnd, &mut placement).is_ok() };
            let win32_zoomed = unsafe { IsZoomed(hwnd).as_bool() };
            let show_maximized = placement.showCmd == SW_SHOWMAXIMIZED.0 as u32;
            if !(egui_maximized && placement_ok && win32_zoomed && show_maximized) {
                ctx.request_repaint();
                return;
            }
            tracing::debug!(
                egui_maximized,
                placement_ok,
                win32_zoomed,
                show_cmd = placement.showCmd,
                "viewer.startup_restore_rect.adjustment.ready"
            );
            if self.apply_startup_restore_rect_adjustment_windows(hwnd, placement) {
                self.startup_restore_rect_adjustment_pending = false;
                return;
            }
        }
        #[cfg(not(windows))]
        {
            let _ = frame;
            self.startup_restore_rect_adjustment_pending = false;
            return;
        }

        if self.startup_restore_rect_adjustment_attempts >= STARTUP_RESTORE_RECT_MAX_ATTEMPTS {
            tracing::warn!(
                attempts = self.startup_restore_rect_adjustment_attempts,
                "viewer.startup_restore_rect.adjustment.failed"
            );
            self.startup_restore_rect_adjustment_pending = false;
        } else {
            ctx.request_repaint();
        }
    }

    #[cfg(windows)]
    fn apply_startup_restore_rect_adjustment_windows(
        &self,
        hwnd: HWND,
        mut placement: WINDOWPLACEMENT,
    ) -> bool {
        let (Some(saved_pos), Some(saved_size)) =
            (self.saved_viewer_win_pos, self.saved_viewer_win_size)
        else {
            tracing::warn!("viewer.startup_restore_rect.adjustment.saved_rect.unavailable");
            return false;
        };

        // SAFETY: `hwnd` は現在の viewer window handle で、style 読み取りは副作用を持たない。
        let style_bits = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
        // SAFETY: `hwnd` は現在の viewer window handle で、extended style 読み取りは副作用を持たない。
        let exstyle_bits = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let style = WINDOW_STYLE(style_bits as u32);
        let exstyle = WINDOW_EX_STYLE(exstyle_bits as u32);
        let inner_w = saved_size[0].round().max(1.0) as i32;
        let inner_h = saved_size[1].round().max(1.0) as i32;
        let mut rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: inner_w,
            bottom: inner_h,
        };
        // SAFETY:
        // `rect` は有効な入出力バッファで、style / exstyle は直前に同一 hwnd から取得した値。
        unsafe {
            if AdjustWindowRectEx(&mut rect, style, false, exstyle).is_err() {
                tracing::warn!("viewer.startup_restore_rect.adjustment.adjust_window_rect.failed");
                return false;
            }
        }
        let outer_w = rect.right - rect.left;
        let outer_h = rect.bottom - rect.top;
        if outer_w <= 0 || outer_h <= 0 {
            tracing::warn!(
                outer_w,
                outer_h,
                "viewer.startup_restore_rect.adjustment.invalid_outer_size"
            );
            return false;
        }
        let mut work_offset_x = 0i32;
        let mut work_offset_y = 0i32;
        // SAFETY:
        // `hwnd` は有効 window handle で、`monitor_info.cbSize` は Win32 要件どおり設定済み。
        unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if !monitor.0.is_null() {
                let mut monitor_info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
                    work_offset_x = monitor_info.rcWork.left - monitor_info.rcMonitor.left;
                    work_offset_y = monitor_info.rcWork.top - monitor_info.rcMonitor.top;
                } else {
                    tracing::warn!(
                        "viewer.startup_restore_rect.adjustment.monitor_info.unavailable"
                    );
                }
            } else {
                tracing::warn!("viewer.startup_restore_rect.adjustment.monitor.unavailable");
            }
        }
        let left = saved_pos[0].round() as i32 - work_offset_x;
        let top = saved_pos[1].round() as i32 - work_offset_y;
        let before = placement.rcNormalPosition;
        placement.rcNormalPosition.left = left;
        placement.rcNormalPosition.top = top;
        placement.rcNormalPosition.right = left + outer_w;
        placement.rcNormalPosition.bottom = top + outer_h;

        tracing::debug!(
            saved_pos = ?saved_pos,
            saved_size = ?saved_size,
            show_cmd = placement.showCmd,
            before_left = before.left,
            before_top = before.top,
            before_right = before.right,
            before_bottom = before.bottom,
            work_offset_x,
            work_offset_y,
            after_left = placement.rcNormalPosition.left,
            after_top = placement.rcNormalPosition.top,
            after_right = placement.rcNormalPosition.right,
            after_bottom = placement.rcNormalPosition.bottom,
            "viewer.startup_restore_rect.adjustment.apply"
        );
        // SAFETY: `placement` は `GetWindowPlacement` 由来の構造体を更新したもので、同じ hwnd へ戻す。
        unsafe {
            if SetWindowPlacement(hwnd, &placement).is_err() {
                tracing::warn!(
                    "viewer.startup_restore_rect.adjustment.set_window_placement.failed"
                );
                return false;
            }
        }
        tracing::debug!("viewer.startup_restore_rect.adjustment.applied");
        true
    }

    #[cfg(not(windows))]
    fn apply_startup_restore_rect_adjustment_windows(&self, _frame: &eframe::Frame) -> bool {
        false
    }
}

fn validate_image_order_snapshot(snapshot: &ImageOrderSnapshot) -> Option<usize> {
    if snapshot.ordered_images.is_empty() || !snapshot.folder.is_dir() || !snapshot.start_image.is_file()
    {
        return None;
    }
    let normalized_folder = crate::util::path_eq::normalize_path_for_override(&snapshot.folder);
    if crate::util::path_eq::normalize_path_for_override(snapshot.start_image.parent()?)
        != normalized_folder
    {
        return None;
    }
    let normalized_start_image =
        crate::util::path_eq::normalize_path_for_override(&snapshot.start_image);
    let start_page = snapshot
        .ordered_images
        .iter()
        .position(|path| {
            crate::util::path_eq::normalize_path_for_override(path) == normalized_start_image
        })?;
    let mut seen = std::collections::HashSet::with_capacity(snapshot.ordered_images.len());
    for path in &snapshot.ordered_images {
        if !path.is_file() {
            return None;
        }
        if crate::util::path_eq::normalize_path_for_override(path.parent()?) != normalized_folder {
            return None;
        }
        if !crate::util::archive_path::is_supported_image_path(path) {
            return None;
        }
        if !seen.insert(crate::util::path_eq::normalize_path_for_override(path)) {
            return None;
        }
    }
    Some(start_page)
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.pending_startup_maximize && !self.startup_maximize_sent {
            self.startup_maximize_frame_count = self.startup_maximize_frame_count.saturating_add(1);
            if self.startup_maximize_frame_count >= STARTUP_MAXIMIZE_TRIGGER_FRAME {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                self.startup_maximize_sent = true;
                self.pending_startup_maximize = false;
                tracing::debug!(
                    frames = self.startup_maximize_frame_count,
                    "startup windowed maximize command sent"
                );
            } else {
                ctx.request_repaint();
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            self.send_reading_session_finished(true);
            self.state.close_boundary_preview();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.state.close_boundary_preview();
            if self.delete_dialog_open {
                self.delete_dialog_open = false;
                self.delete_dialog_choice = ViewerDeleteDialogChoice::DeleteAndNext;
            } else if self.opened_as_fullscreen {
                self.send_reading_session_finished(true);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            } else if self.is_fullscreen {
                self.is_fullscreen = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            } else {
                self.send_reading_session_finished(true);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        self.poll_library_ipc(ctx);
        self.poll_external_tool_results(ctx);
        self.tick_external_tool_ui_state();
        self.schedule_external_tool_state_repaint(ctx);

        let mut cover_blank_changed: Option<bool> = None;
        let mut spread_changed: Option<SpreadMode> = None;
        let mut interval_changed: Option<f32> = None;
        let mut reading_direction_override_changed: Option<Option<ReadingDirection>> = None;
        let mut quality_changed: Option<Option<ViewerQuality>> = None;
        let capabilities = self.mode.ui_capabilities();
        let external_tools = self.external_tool_button_models();
        let external_tool_state = self.external_tool_toolbar_state_for_ui();

        #[allow(deprecated)]
        let panel = if self.is_fullscreen {
            egui::CentralPanel::default()
                .frame(egui::Frame::default().inner_margin(egui::Margin::same(0)))
        } else {
            egui::CentralPanel::default()
        };
        #[allow(deprecated)]
        {
            panel.show(ctx, |ui| {
                let action = viewer::show(
                    ui,
                    viewer::ViewerShowContext {
                        state: &mut self.state,
                        language: self.app_settings.ui_language,
                        favorite_state: self.favorite_state,
                        favorite_toggle_pending: self
                            .pending_favorite_toggle_previous_state
                            .is_some(),
                        interaction_blocked: self.delete_dialog_open,
                        is_fullscreen: self.is_fullscreen,
                        external_tools: &external_tools,
                        external_tool_state: &external_tool_state,
                        global_quality: self.app_settings.viewer_quality,
                        capabilities,
                        boundary_preview_thumb_size: egui::vec2(
                            self.app_settings.thumb_w(),
                            self.app_settings.thumb_h(),
                        ),
                        boundary_preview_hud_font_size: self.app_settings.library_hud_font_size(),
                    },
                    &mut viewer::ViewerSettingsChangeSink {
                        cover_blank: &mut cover_blank_changed,
                        spread: &mut spread_changed,
                        slideshow_interval: &mut interval_changed,
                        reading_direction_override: &mut reading_direction_override_changed,
                        quality_override: &mut quality_changed,
                    },
                );
                match action {
                    ViewerAction::None => {}
                    ViewerAction::ToggleFullscreen => {
                        self.toggle_fullscreen(ctx);
                    }
                    ViewerAction::RequestDelete => {
                        self.begin_delete_dialog();
                    }
                    ViewerAction::ToggleFavorite => {
                        if let Err(e) = self.request_favorite_toggle() {
                            tracing::warn!(error = %e, "request-favorite-toggle failed");
                            self.mark_viewer_feedback(
                                TextKey::FavoriteUpdateFailed,
                                Instant::now(),
                            );
                        }
                    }
                    ViewerAction::PreviousBook => {
                        if let ViewerMode::Library {
                            ref mut last_request_id,
                            ..
                        } = self.mode
                        {
                            *last_request_id = last_request_id.saturating_add(1);
                            let request_id = *last_request_id;
                            let result = self.handle_ipc_navigation(
                                ViewerToLibrary::RequestPrevBook { request_id },
                                PendingLibraryAction::Prev,
                                request_id,
                            );
                            if let Err(e) = result {
                                tracing::warn!(error = %e, "request-prev-book failed; detached");
                                self.state.close_boundary_preview();
                                self.mode = ViewerMode::Detached;
                            }
                        }
                    }
                    ViewerAction::NextBook => {
                        if let ViewerMode::Library {
                            ref mut last_request_id,
                            ..
                        } = self.mode
                        {
                            *last_request_id = last_request_id.saturating_add(1);
                            let request_id = *last_request_id;
                            let result = self.handle_ipc_navigation(
                                ViewerToLibrary::RequestNextBook { request_id },
                                PendingLibraryAction::Next,
                                request_id,
                            );
                            if let Err(e) = result {
                                tracing::warn!(error = %e, "request-next-book failed; detached");
                                self.state.close_boundary_preview();
                                self.mode = ViewerMode::Detached;
                            }
                        }
                    }
                    ViewerAction::RunExternalTool {
                        tool_index,
                        target_path,
                        trigger,
                    } => {
                        self.request_external_tool_run_from_trigger(
                            tool_index,
                            target_path,
                            trigger,
                        );
                    }
                }
            });
        }

        self.send_boundary_preview_request();

        let path = self.state.entry().path.as_ref().to_path_buf();
        if let Some(v) = cover_blank_changed {
            self.settings.set_cover_blank(path.clone(), v);
        }
        if let Some(v) = spread_changed {
            self.settings.set_spread_mode(path.clone(), v);
        }
        if let Some(v) = interval_changed {
            self.settings.set_slideshow_interval_secs(path.clone(), v);
        }
        if let Some(v) = reading_direction_override_changed {
            self.settings
                .set_reading_direction_override(path.clone(), v);
        }
        if let Some(v) = quality_changed {
            self.settings.set_quality_override(path, v);
        }
        self.show_delete_dialog(ctx);
        self.capture_viewer_window_state(ctx);
        self.maybe_apply_startup_restore_rect_adjustment(ctx, frame);
    }

    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.send_reading_session_finished(true);
        self.save_viewer_window_geometry();
    }
}

fn book_meta_from_path(path: &Path) -> BookMeta {
    let title = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    }
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let (size, modified) = std::fs::metadata(path)
        .map(|m| (m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        .unwrap_or((0, SystemTime::UNIX_EPOCH));
    BookMeta {
        id: crate::domain::archive::BookId::from_path(path),
        path: Arc::from(path),
        title: Arc::from(title),
        size,
        modified,
        page_count: None,
    }
}

fn viewer_title_for(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| crate::app_identity::PRODUCT_NAME.to_owned())
}

fn set_viewer_title(ctx: &egui::Context, path: &Path) {
    let title = viewer_title_for(path);
    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
}
