use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use parking_lot::RwLock;

use crate::domain::app_settings::ViewerOpenMode;
use crate::domain::archive::BookId;
use crate::domain::archive_settings::{book_settings_path, ReadingState, SettingsStore};
use crate::infra::archive::folder::FolderImageReader;
use crate::infra::favorite_store::FavoriteStore;
use crate::infra::ipc::{
    ImageOrderSnapshot, IpcErrorCode, IpcServer, LibraryToViewer, ViewerBookState,
    ViewerFavoriteState, ViewerToLibrary,
};
use crate::util::archive_path::{is_supported_archive_path, is_supported_image_path};
use crate::util::book_nav::{self, NavDirection};
use crate::util::path_eq::normalize_path_for_selection;

use super::platform::{monitor_rect_from_point, paths_equivalent_for_selection};
use super::App;

/// Library で作った本の移動順を Viewer/IPC に渡すための共有スナップショット。
///
/// `books` が現在順、`previous_books` は current_path が新順に未反映の
/// 一時的な応答を救うための直前順で、Viewer 側の移動の安定性を保つ。
#[derive(Clone, Debug, Default)]
pub(super) struct LibraryNavSnapshot {
    /// 現在の Library 表示順。Viewer の前後本判定はこれを基準にする。
    pub(super) books: Vec<PathBuf>,
    /// 直前の表示順。current_path が新順にまだ載っていない IPC 応答の救済用。
    pub(super) previous_books: Vec<PathBuf>,
    /// 順序が実際に変わったときだけ進める世代番号。
    pub(super) epoch: u64,
}

#[derive(Clone, Debug)]
struct ResolvedNavigationBooks {
    books: Vec<PathBuf>,
    anchor_index: usize,
    current_present: bool,
}

#[derive(Clone, Debug)]
struct ViewerLaunchSpec {
    path: PathBuf,
    with_pipe: bool,
    start_page: Option<u32>,
    ipc_current_path: Option<PathBuf>,
    image_order_snapshot: Option<ImageOrderSnapshot>,
    snapshot_only_ipc: bool,
}

#[derive(Clone, Debug)]
pub(super) enum ViewerSyncEvent {
    Deleted {
        deleted_path: PathBuf,
        next_path: Option<PathBuf>,
    },
    ReadingSessionFinished {
        book_path: PathBuf,
    },
    Navigated {
        path: PathBuf,
    },
    FavoriteToggle {
        request_id: u64,
        current_path: PathBuf,
        response_tx: mpsc::Sender<FavoriteToggleResult>,
    },
    RebuildSelectedImagesAsCbzAndNext {
        request_id: u64,
        book_id: BookId,
        current_path: PathBuf,
        delete_entries: Vec<String>,
        next_path: Option<PathBuf>,
        response_tx: mpsc::Sender<RebuildSelectedImagesAsCbzAndNextResult>,
    },
}

#[derive(Clone, Debug)]
pub(super) enum FavoriteToggleResult {
    Success(ViewerFavoriteState),
    Error(IpcErrorCode),
}

#[derive(Clone, Debug)]
pub(super) enum RebuildSelectedImagesAsCbzAndNextResult {
    NavigateTo(PathBuf),
    NoMoreBooks,
    Error(IpcErrorCode),
}

impl App {
    // 責務境界:
    // - Library 側: viewer subprocess の起動と navigation/delete IPC を担う。
    // - Viewer 側: UI 状態、描画、fullscreen を保持する。
    fn spawn_viewer(
        &mut self,
        spec: ViewerLaunchSpec,
        root_ctx: &egui::Context,
    ) -> anyhow::Result<()> {
        let current_exe = std::env::current_exe()?;
        let mut cmd = Command::new(current_exe);
        cmd.arg(&spec.path);
        let initial_path = spec
            .ipc_current_path
            .clone()
            .unwrap_or_else(|| spec.path.clone());
        if let Some(page) = spec.start_page {
            cmd.arg("--viewer-start-page").arg(page.to_string());
        }

        if spec.with_pipe {
            let server = IpcServer::with_generated_name()?;
            let pipe_name = server.pipe_name().to_owned();
            cmd.arg("--pipe").arg(&pipe_name);
            if spec.snapshot_only_ipc {
                cmd.arg("--viewer-snapshot-only-ipc");
            }
            if self.app_settings.viewer_open_mode == ViewerOpenMode::Fullscreen {
                // Library から直接 fullscreen で起動する場合だけ、起動直後の矩形を渡す。
                cmd.arg("--fullscreen");
                if let Some(outer) = root_ctx.input(|i| i.viewport().outer_rect) {
                    let target_pos = outer.min;
                    let target_size = root_ctx
                        .input(|i| i.viewport().monitor_size)
                        .filter(|s| s.x > 1.0 && s.y > 1.0)
                        .unwrap_or_else(|| outer.size());
                    cmd.arg("--viewer-full-x")
                        .arg(format!("{}", target_pos.x))
                        .arg("--viewer-full-y")
                        .arg(format!("{}", target_pos.y))
                        .arg("--viewer-full-w")
                        .arg(format!("{}", target_size.x))
                        .arg("--viewer-full-h")
                        .arg(format!("{}", target_size.y));
                }
            } else {
                // 通常起動では、初回位置とモニターの fallback だけを渡す。
                let library_center =
                    root_ctx.input(|i| i.viewport().outer_rect.map(|outer| outer.center()));
                let viewer_monitor_rect =
                    library_center.and_then(|center| monitor_rect_from_point(center.x, center.y));
                if let Some(outer) = root_ctx.input(|i| i.viewport().outer_rect) {
                    cmd.arg("--viewer-window-x")
                        .arg(format!("{}", outer.min.x))
                        .arg("--viewer-window-y")
                        .arg(format!("{}", outer.min.y));
                }
                if let Some([mx, my, mw, mh]) = viewer_monitor_rect {
                    cmd.arg("--viewer-monitor-x")
                        .arg(format!("{}", mx))
                        .arg("--viewer-monitor-y")
                        .arg(format!("{}", my))
                        .arg("--viewer-monitor-w")
                        .arg(format!("{}", mw))
                        .arg("--viewer-monitor-h")
                        .arg(format!("{}", mh));
                }
            }
            let child = cmd.spawn()?;
            self.viewer_processes.push(child);

            let library_book_order = Arc::clone(&self.library_book_order);
            let pending_viewer_sync_events = Arc::clone(&self.pending_viewer_sync_events);
            let favorite_store = self.library.favorite_store_handle();
            let resume_from_last_reading_position =
                self.app_settings.resume_from_last_reading_position;
            let repaint_ctx = root_ctx.clone();
            let image_order_snapshot = spec.image_order_snapshot.clone();
            std::thread::Builder::new()
                .name("viewer-ipc-accept".to_owned())
                .spawn(move || {
                    let Ok(mut conn) = server.accept() else {
                        return;
                    };
                    let mut current_path = initial_path;
                    loop {
                        let msg = match conn.recv_from_viewer() {
                            Ok(msg) => msg,
                            Err(_) => break,
                        };
                        let processed = match msg {
                            ViewerToLibrary::FavoriteToggle {
                                request_id,
                                current_path: request_path,
                            } => {
                                let (response_tx, response_rx) =
                                    mpsc::channel::<FavoriteToggleResult>();
                                pending_viewer_sync_events.lock().push(
                                    ViewerSyncEvent::FavoriteToggle {
                                        request_id,
                                        current_path: request_path,
                                        response_tx,
                                    },
                                );
                                repaint_ctx.request_repaint();
                                let response = match response_rx.recv() {
                                    Ok(FavoriteToggleResult::Success(favorite_state)) => {
                                        LibraryToViewer::FavoriteToggleResponse {
                                            request_id,
                                            favorite_state,
                                        }
                                    }
                                    Ok(FavoriteToggleResult::Error(code)) => {
                                        LibraryToViewer::Error {
                                            request_id,
                                            code,
                                            retryable: false,
                                        }
                                    }
                                    Err(_) => LibraryToViewer::Error {
                                        request_id,
                                        code: IpcErrorCode::Unknown,
                                        retryable: false,
                                    },
                                };
                                Some((response, None))
                            }
                            ViewerToLibrary::RebuildSelectedImagesAsCbzAndNext {
                                request_id,
                                book_id,
                                delete_entries,
                            } => {
                                if !is_requested_book_current(current_path.as_path(), &book_id) {
                                    Some(ipc_error_response(request_id, IpcErrorCode::FileNotFound))
                                } else {
                                    let books = resolved_navigation_books_for_delete(
                                        current_path.as_path(),
                                        &library_book_order,
                                    );
                                    let next_path = book_nav::move_target_path(
                                        &books,
                                        current_path.as_path(),
                                        NavDirection::Next,
                                        false,
                                    )
                                    .or_else(|| {
                                        book_nav::move_target_path(
                                            &books,
                                            current_path.as_path(),
                                            NavDirection::Previous,
                                            false,
                                        )
                                    });
                                    let (response_tx, response_rx) =
                                        mpsc::channel::<RebuildSelectedImagesAsCbzAndNextResult>();
                                    pending_viewer_sync_events.lock().push(
                                        ViewerSyncEvent::RebuildSelectedImagesAsCbzAndNext {
                                            request_id,
                                            book_id,
                                            current_path: current_path.clone(),
                                            delete_entries,
                                            next_path: next_path.clone(),
                                            response_tx,
                                        },
                                    );
                                    repaint_ctx.request_repaint();
                                    let response = match response_rx.recv() {
                                        Ok(
                                            RebuildSelectedImagesAsCbzAndNextResult::NavigateTo(
                                                path,
                                            ),
                                        ) => {
                                            current_path.clone_from(&path);
                                            let book_state = viewer_book_state_for_path(
                                                favorite_store.as_ref(),
                                                path.as_path(),
                                                resume_from_last_reading_position,
                                                None,
                                            );
                                            (
                                                LibraryToViewer::NavigateTo {
                                                    request_id,
                                                    path: path.clone(),
                                                    book_state,
                                                },
                                                Some(ViewerSyncEvent::Navigated { path }),
                                            )
                                        }
                                        Ok(RebuildSelectedImagesAsCbzAndNextResult::NoMoreBooks) => {
                                            (LibraryToViewer::NoMoreBooks { request_id }, None)
                                        }
                                        Ok(RebuildSelectedImagesAsCbzAndNextResult::Error(code)) => {
                                            (
                                                LibraryToViewer::Error {
                                                    request_id,
                                                    code,
                                                    retryable: false,
                                                },
                                                None,
                                            )
                                        }
                                        Err(_) => (
                                            LibraryToViewer::Error {
                                                request_id,
                                                code: IpcErrorCode::Unknown,
                                                retryable: false,
                                            },
                                            None,
                                        ),
                                    };
                                    Some(response)
                                }
                            }
                            other => process_viewer_ipc_request(
                                other,
                                &mut current_path,
                                image_order_snapshot.as_ref(),
                                &library_book_order,
                                favorite_store.as_ref(),
                                resume_from_last_reading_position,
                            ),
                        };
                        let Some((response, sync_event)) = processed else {
                            break;
                        };
                        if let Some(event) = sync_event {
                            pending_viewer_sync_events.lock().push(event);
                        }
                        if conn.send_to_viewer(&response).is_err() {
                            break;
                        }
                    }
                })?;
            return Ok(());
        }

        cmd.arg("--viewer-offline");
        if let Some(page) = spec.start_page {
            cmd.arg("--viewer-start-page").arg(page.to_string());
        }
        if self.app_settings.viewer_open_mode == ViewerOpenMode::Fullscreen {
            // Library から直接 fullscreen で起動する場合だけ、起動直後の矩形を渡す。
            cmd.arg("--fullscreen");
            if let Some(outer) = root_ctx.input(|i| i.viewport().outer_rect) {
                let target_pos = outer.min;
                let target_size = root_ctx
                    .input(|i| i.viewport().monitor_size)
                    .filter(|s| s.x > 1.0 && s.y > 1.0)
                    .unwrap_or_else(|| outer.size());
                cmd.arg("--viewer-full-x")
                    .arg(format!("{}", target_pos.x))
                    .arg("--viewer-full-y")
                    .arg(format!("{}", target_pos.y))
                    .arg("--viewer-full-w")
                    .arg(format!("{}", target_size.x))
                    .arg("--viewer-full-h")
                    .arg(format!("{}", target_size.y));
            }
        } else {
            // 通常起動では、初回位置とモニターの fallback だけを渡す。
            let library_center =
                root_ctx.input(|i| i.viewport().outer_rect.map(|outer| outer.center()));
            let viewer_monitor_rect =
                library_center.and_then(|center| monitor_rect_from_point(center.x, center.y));
            if let Some(outer) = root_ctx.input(|i| i.viewport().outer_rect) {
                cmd.arg("--viewer-window-x")
                    .arg(format!("{}", outer.min.x))
                    .arg("--viewer-window-y")
                    .arg(format!("{}", outer.min.y));
            }
            if let Some([mx, my, mw, mh]) = viewer_monitor_rect {
                cmd.arg("--viewer-monitor-x")
                    .arg(format!("{}", mx))
                    .arg("--viewer-monitor-y")
                    .arg(format!("{}", my))
                    .arg("--viewer-monitor-w")
                    .arg(format!("{}", mw))
                    .arg("--viewer-monitor-h")
                    .arg(format!("{}", mh));
            }
        }
        let child = cmd.spawn()?;
        self.viewer_processes.push(child);
        Ok(())
    }

    pub(super) fn open_viewer(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(entry) = self.library.entries.get(idx).cloned() else {
            return;
        };
        let Some((spec, history_path)) = self.viewer_launch_spec_for_entry(&entry) else {
            return;
        };
        self.push_open_history(history_path);
        if let Err(e) = self.spawn_viewer(spec, ctx) {
            tracing::error!(error = %e, "failed to spawn viewer subprocess");
            self.show_toast("Failed to open viewer");
        }
    }

    pub(super) fn open_viewer_by_path(
        &mut self,
        path: PathBuf,
        ctx: &egui::Context,
    ) -> Result<(), ()> {
        if !path.exists() {
            self.show_toast("Path not found");
            return Err(());
        }
        let Some((spec, history_path)) = self.viewer_launch_spec_for_path(path.as_path()) else {
            self.show_toast("Unsupported file type");
            return Err(());
        };
        self.push_open_history(history_path);
        if let Err(e) = self.spawn_viewer(spec, ctx) {
            tracing::error!(error = %e, "failed to spawn viewer subprocess by path");
            self.show_toast("Failed to open viewer");
            return Err(());
        }
        Ok(())
    }

    fn viewer_launch_spec_for_entry(
        &self,
        entry: &crate::domain::archive::LibraryEntry,
    ) -> Option<(ViewerLaunchSpec, PathBuf)> {
        match entry {
            crate::domain::archive::LibraryEntry::Archive(meta) => Some((
                ViewerLaunchSpec {
                    path: meta.path.as_ref().to_path_buf(),
                    with_pipe: true,
                    start_page: self
                        .viewer_start_page_for_path(meta.path.as_ref(), None)
                        .map(|page| page as u32),
                    ipc_current_path: None,
                    image_order_snapshot: None,
                    snapshot_only_ipc: false,
                },
                meta.path.as_ref().to_path_buf(),
            )),
            crate::domain::archive::LibraryEntry::FolderBook(meta) => Some((
                ViewerLaunchSpec {
                    path: meta.path.as_ref().to_path_buf(),
                    with_pipe: true,
                    start_page: self
                        .viewer_start_page_for_path(meta.path.as_ref(), None)
                        .map(|page| page as u32),
                    ipc_current_path: None,
                    image_order_snapshot: None,
                    snapshot_only_ipc: false,
                },
                meta.path.as_ref().to_path_buf(),
            )),
            crate::domain::archive::LibraryEntry::ImageFile(meta) => {
                let reader = FolderImageReader::open(meta.path.parent()?).ok()?;
                let start_page = reader.page_index_for_path(meta.path.as_ref())?;
                Some((
                    ViewerLaunchSpec {
                        path: meta.path.parent()?.to_path_buf(),
                        with_pipe: true,
                        start_page: self
                            .viewer_start_page_for_path(
                                meta.path.as_ref(),
                                Some(start_page as usize),
                            )
                            .map(|page| page as u32),
                        ipc_current_path: None,
                        image_order_snapshot: self
                            .image_order_snapshot_from_entries(meta.path.as_ref()),
                        snapshot_only_ipc: true,
                    },
                    meta.path.as_ref().to_path_buf(),
                ))
            }
            crate::domain::archive::LibraryEntry::Folder(_) => None,
        }
    }

    fn viewer_launch_spec_for_path(&self, path: &Path) -> Option<(ViewerLaunchSpec, PathBuf)> {
        if path.is_dir() {
            return Some((
                ViewerLaunchSpec {
                    path: path.to_path_buf(),
                    with_pipe: false,
                    start_page: self
                        .viewer_start_page_for_path(path, None)
                        .map(|page| page as u32),
                    ipc_current_path: None,
                    image_order_snapshot: None,
                    snapshot_only_ipc: false,
                },
                path.to_path_buf(),
            ));
        }
        if is_supported_archive_path(path) {
            return Some((
                ViewerLaunchSpec {
                    path: path.to_path_buf(),
                    with_pipe: true,
                    start_page: self
                        .viewer_start_page_for_path(path, None)
                        .map(|page| page as u32),
                    ipc_current_path: None,
                    image_order_snapshot: None,
                    snapshot_only_ipc: false,
                },
                path.to_path_buf(),
            ));
        }
        if is_supported_image_path(path) {
            let parent = path.parent()?;
            let reader = FolderImageReader::open(parent).ok()?;
            let start_page = reader.page_index_for_path(path)?;
            return Some((
                ViewerLaunchSpec {
                    path: parent.to_path_buf(),
                    with_pipe: false,
                    start_page: self
                        .viewer_start_page_for_path(path, Some(start_page as usize))
                        .map(|page| page as u32),
                    ipc_current_path: None,
                    image_order_snapshot: None,
                    snapshot_only_ipc: false,
                },
                path.to_path_buf(),
            ));
        }
        None
    }

    fn viewer_start_page_for_path(
        &self,
        path: &Path,
        explicit_start_page: Option<usize>,
    ) -> Option<usize> {
        if let Some(explicit_start_page) = explicit_start_page {
            return Some(explicit_start_page);
        }
        let settings_path = book_settings_path(path);
        let file_settings = SettingsStore::load().get(settings_path.as_path());
        if !self.app_settings.resume_from_last_reading_position
            || !matches!(
                file_settings.reading_state,
                ReadingState::Reading | ReadingState::Read
            )
        {
            return None;
        }
        match file_settings.resume_page {
            Some(page)
                if file_settings
                    .reading_page_count
                    .is_some_and(|count| page >= count) =>
            {
                Some(0)
            }
            resume_page => resume_page,
        }
    }

    fn image_order_snapshot_from_entries(&self, start_image: &Path) -> Option<ImageOrderSnapshot> {
        let folder = start_image.parent()?.to_path_buf();
        let normalized_folder = normalize_path_for_selection(folder.as_path());
        let ordered_images: Vec<PathBuf> = self
            .library
            .entries
            .iter()
            .filter_map(|entry| match entry {
                crate::domain::archive::LibraryEntry::ImageFile(meta)
                    if meta.path.parent().is_some_and(|parent| {
                        normalize_path_for_selection(parent) == normalized_folder
                    }) =>
                {
                    Some(meta.path.as_ref().to_path_buf())
                }
                _ => None,
            })
            .collect();
        let normalized_start_image = normalize_path_for_selection(start_image);
        if ordered_images.is_empty()
            || !ordered_images
                .iter()
                .any(|path| normalize_path_for_selection(path) == normalized_start_image)
        {
            return None;
        }
        Some(ImageOrderSnapshot {
            folder,
            start_image: start_image.to_path_buf(),
            ordered_images,
        })
    }
}

fn process_viewer_ipc_request(
    msg: ViewerToLibrary,
    current_path: &mut PathBuf,
    image_order_snapshot: Option<&ImageOrderSnapshot>,
    library_book_order: &Arc<RwLock<LibraryNavSnapshot>>,
    favorite_store: &RwLock<FavoriteStore>,
    resume_from_last_reading_position: bool,
) -> Option<(LibraryToViewer, Option<ViewerSyncEvent>)> {
    let started_at = Instant::now();
    let (response, sync_event) = match msg {
        ViewerToLibrary::RequestViewerState {
            request_id,
            current_path: _,
        } => {
            let state_path = image_order_snapshot
                .map(|snapshot| snapshot.start_image.as_path())
                .unwrap_or_else(|| current_path.as_path());
            let book_state = viewer_book_state_for_path(
                favorite_store,
                state_path,
                resume_from_last_reading_position,
                None,
            );
            (
                LibraryToViewer::ResponseViewerState {
                    request_id,
                    book_state,
                    image_order_snapshot: image_order_snapshot.cloned(),
                },
                None,
            )
        }
        ViewerToLibrary::ReadingSessionFinished {
            request_id,
            book_path,
            displayed_any_page,
            reached_end,
            resume_page,
            page_count,
        } => {
            SettingsStore::update_reading_session_on_disk(
                book_path.as_path(),
                displayed_any_page,
                reached_end,
                resume_page,
                page_count,
            );
            (
                LibraryToViewer::ReadingSessionFinishedAck { request_id },
                Some(ViewerSyncEvent::ReadingSessionFinished { book_path }),
            )
        }
        ViewerToLibrary::FavoriteToggle { request_id, .. } => {
            return Some(ipc_error_response(request_id, IpcErrorCode::InvalidRequest));
        }
        ViewerToLibrary::Delete {
            request_id,
            book_id,
        } => {
            if !is_requested_book_current(current_path.as_path(), &book_id) {
                return Some(ipc_error_response(request_id, IpcErrorCode::FileNotFound));
            }
            let deleted_path = current_path.clone();
            tracing::trace!(
                request_id,
                path = %deleted_path.display(),
                delete_kind = %delete_kind_label(deleted_path.as_path()),
                "viewer.ipc.delete.start"
            );
            if delete_path(current_path.as_path()).is_err() {
                return Some(ipc_error_response(request_id, IpcErrorCode::DeleteFailed));
            }
            deleted_response(request_id, deleted_path, None, None)
        }
        ViewerToLibrary::DeleteAndNext {
            request_id,
            book_id,
        } => {
            if !is_requested_book_current(current_path.as_path(), &book_id) {
                return Some(ipc_error_response(request_id, IpcErrorCode::FileNotFound));
            }
            let deleted_path = current_path.clone();
            tracing::trace!(
                request_id,
                path = %deleted_path.display(),
                delete_kind = %delete_kind_label(deleted_path.as_path()),
                "viewer.ipc.delete_and_next.start"
            );
            // 削除前に次本の候補を決める。先に消すと current_path 基準の前後判定が壊れる。
            let books =
                resolved_navigation_books_for_delete(current_path.as_path(), library_book_order);
            let select_after = book_nav::move_target_path(
                &books,
                current_path.as_path(),
                NavDirection::Next,
                false,
            )
            .or_else(|| {
                book_nav::move_target_path(
                    &books,
                    current_path.as_path(),
                    NavDirection::Previous,
                    false,
                )
            });
            if delete_path(current_path.as_path()).is_err() {
                return Some(ipc_error_response(request_id, IpcErrorCode::DeleteFailed));
            }
            match select_after {
                Some(path) => {
                    *current_path = path.clone();
                    let next_book_state = Some(viewer_book_state_for_path(
                        favorite_store,
                        path.as_path(),
                        resume_from_last_reading_position,
                        None,
                    ));
                    deleted_response(request_id, deleted_path, Some(path), next_book_state)
                }
                None => deleted_response(request_id, deleted_path, None, None),
            }
        }
        ViewerToLibrary::RebuildSelectedImagesAsCbzAndNext {
            request_id,
            book_id,
            delete_entries,
        } => {
            let _ = (book_id, delete_entries);
            return Some(ipc_error_response(request_id, IpcErrorCode::InvalidRequest));
        }
        ViewerToLibrary::RequestAdjacentBooks { request_id, kind } => {
            let resolved =
                match resolved_navigation_books(current_path.as_path(), library_book_order) {
                    Ok(books) => books,
                    Err(code) => {
                        let retryable = code.retryable();
                        return Some((
                            LibraryToViewer::Error {
                                request_id,
                                code,
                                retryable,
                            },
                            None,
                        ));
                    }
                };
            let (prev, next) = if resolved.current_present {
                book_nav::adjacent_paths(&resolved.books, current_path.as_path())
            } else {
                (
                    resolved
                        .anchor_index
                        .checked_sub(1)
                        .and_then(|idx| resolved.books.get(idx))
                        .cloned(),
                    resolved.books.get(resolved.anchor_index).cloned(),
                )
            };
            tracing::trace!(
                request_id,
                kind = ?kind,
                current = %current_path.display(),
                current_kind = %navigation_book_kind_label(current_path.as_path()),
                current_present = resolved.current_present,
                prev = prev.as_deref().map(|p| p.display().to_string()).as_deref().unwrap_or("-"),
                prev_kind = prev
                    .as_deref()
                    .map(navigation_book_kind_label)
                    .unwrap_or("-"),
                next = next.as_deref().map(|p| p.display().to_string()).as_deref().unwrap_or("-"),
                next_kind = next
                    .as_deref()
                    .map(navigation_book_kind_label)
                    .unwrap_or("-"),
                "viewer.ipc.request_adjacent_books.result"
            );
            (
                LibraryToViewer::AdjacentBooks {
                    request_id,
                    kind,
                    prev,
                    next,
                },
                None,
            )
        }
        ViewerToLibrary::RequestNextBook { request_id } => {
            let resolved =
                match resolved_navigation_books(current_path.as_path(), library_book_order) {
                    Ok(books) => books,
                    Err(code) => {
                        let retryable = code.retryable();
                        return Some((
                            LibraryToViewer::Error {
                                request_id,
                                code,
                                retryable,
                            },
                            None,
                        ));
                    }
                };
            let target = if resolved.current_present {
                book_nav::move_target_path(
                    &resolved.books,
                    current_path.as_path(),
                    NavDirection::Next,
                    false,
                )
            } else {
                book_nav::move_target_path_from_insertion_index(
                    &resolved.books,
                    resolved.anchor_index,
                    NavDirection::Next,
                    false,
                )
            };
            match target {
                Some(path) => {
                    *current_path = path.clone();
                    let book_state = viewer_book_state_for_path(
                        favorite_store,
                        path.as_path(),
                        resume_from_last_reading_position,
                        None,
                    );
                    (
                        LibraryToViewer::NavigateTo {
                            request_id,
                            path: path.clone(),
                            book_state,
                        },
                        Some(ViewerSyncEvent::Navigated { path }),
                    )
                }
                None => (LibraryToViewer::NoMoreBooks { request_id }, None),
            }
        }
        ViewerToLibrary::RequestPrevBook { request_id } => {
            let resolved =
                match resolved_navigation_books(current_path.as_path(), library_book_order) {
                    Ok(books) => books,
                    Err(code) => {
                        let retryable = code.retryable();
                        return Some((
                            LibraryToViewer::Error {
                                request_id,
                                code,
                                retryable,
                            },
                            None,
                        ));
                    }
                };
            let target = if resolved.current_present {
                book_nav::move_target_path(
                    &resolved.books,
                    current_path.as_path(),
                    NavDirection::Previous,
                    false,
                )
            } else {
                book_nav::move_target_path_from_insertion_index(
                    &resolved.books,
                    resolved.anchor_index,
                    NavDirection::Previous,
                    false,
                )
            };
            match target {
                Some(path) => {
                    *current_path = path.clone();
                    let book_state = viewer_book_state_for_path(
                        favorite_store,
                        path.as_path(),
                        resume_from_last_reading_position,
                        None,
                    );
                    (
                        LibraryToViewer::NavigateTo {
                            request_id,
                            path: path.clone(),
                            book_state,
                        },
                        Some(ViewerSyncEvent::Navigated { path }),
                    )
                }
                None => (LibraryToViewer::NoMoreBooks { request_id }, None),
            }
        }
        ViewerToLibrary::Closed => return None,
    };
    tracing::info!(
        current_path = %current_path.display(),
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "library.ipc.request.processed"
    );
    Some((response, sync_event))
}

fn is_requested_book_current(current_path: &Path, book_id: &BookId) -> bool {
    BookId::from_path(current_path) == *book_id
}

fn delete_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn delete_kind_label(path: &Path) -> &'static str {
    if path.is_dir() {
        "directory delete"
    } else {
        "file delete"
    }
}

fn navigation_book_kind_label(path: &Path) -> &'static str {
    if path.is_dir() {
        "folder_book"
    } else if is_supported_archive_path(path) {
        "archive"
    } else {
        "other"
    }
}

fn ipc_error_response(
    request_id: u64,
    code: IpcErrorCode,
) -> (LibraryToViewer, Option<ViewerSyncEvent>) {
    (
        LibraryToViewer::Error {
            request_id,
            code,
            retryable: false,
        },
        None,
    )
}

fn deleted_response(
    request_id: u64,
    deleted_path: PathBuf,
    next_path: Option<PathBuf>,
    next_book_state: Option<ViewerBookState>,
) -> (LibraryToViewer, Option<ViewerSyncEvent>) {
    (
        LibraryToViewer::Deleted {
            request_id,
            deleted_path: deleted_path.clone(),
            next_path: next_path.clone(),
            next_book_state,
        },
        Some(ViewerSyncEvent::Deleted {
            deleted_path,
            next_path,
        }),
    )
}

fn favorite_state_for_path(
    favorite_store: &RwLock<FavoriteStore>,
    path: &Path,
) -> ViewerFavoriteState {
    let normalized = normalize_path_for_selection(path);
    if favorite_store.read().contains(&normalized) {
        ViewerFavoriteState::On
    } else {
        ViewerFavoriteState::Off
    }
}

fn viewer_book_state_for_path(
    favorite_store: &RwLock<FavoriteStore>,
    path: &Path,
    resume_from_last_reading_position: bool,
    explicit_start_page: Option<usize>,
) -> ViewerBookState {
    let favorite_state = favorite_state_for_path(favorite_store, path);
    let settings_path = book_settings_path(path);
    let file_settings = SettingsStore::load().get(settings_path.as_path());
    let start_page = if let Some(explicit_start_page) = explicit_start_page {
        Some(explicit_start_page)
    } else if !resume_from_last_reading_position
        || !matches!(
            file_settings.reading_state,
            ReadingState::Reading | ReadingState::Read
        )
    {
        None
    } else {
        match file_settings.resume_page {
            Some(page)
                if file_settings
                    .reading_page_count
                    .is_some_and(|count| page >= count) =>
            {
                Some(0)
            }
            resume_page => resume_page,
        }
    };
    ViewerBookState {
        favorite_state,
        reading_state: file_settings.reading_state,
        start_page,
    }
}

fn resolved_navigation_books(
    current_path: &Path,
    library_book_order: &Arc<RwLock<LibraryNavSnapshot>>,
) -> Result<ResolvedNavigationBooks, IpcErrorCode> {
    let snapshot = library_book_order.read().clone();
    // snapshot が空・未同期・current_path 不在でも、Library 外の fallback で
    // 同じ本順序を組み直せるようにしておく。ここで落とすと delete-next や
    // 旧 IPC 応答の救済が壊れる。
    if snapshot.books.is_empty() {
        return Ok(fallback_navigation_books(current_path));
    }
    if let Some(anchor_index) = snapshot
        .books
        .iter()
        .position(|p| paths_equivalent_for_selection(p.as_path(), current_path))
    {
        return Ok(ResolvedNavigationBooks {
            books: snapshot.books,
            anchor_index,
            current_present: true,
        });
    }
    if let Some(anchor_index) = snapshot
        .previous_books
        .iter()
        .position(|p| paths_equivalent_for_selection(p.as_path(), current_path))
    {
        return Ok(ResolvedNavigationBooks {
            books: snapshot.books,
            anchor_index,
            current_present: false,
        });
    }
    Ok(fallback_navigation_books(current_path))
}

fn resolved_navigation_books_for_delete(
    current_path: &Path,
    library_book_order: &Arc<RwLock<LibraryNavSnapshot>>,
) -> Vec<PathBuf> {
    let snapshot = library_book_order.read().clone();
    if snapshot
        .books
        .iter()
        .any(|p| paths_equivalent_for_selection(p.as_path(), current_path))
    {
        return snapshot.books;
    }
    book_nav::list_supported_navigation_books_in_dir(current_path)
}

fn fallback_navigation_books(current_path: &Path) -> ResolvedNavigationBooks {
    // snapshot が使えない時だけ、親ディレクトリ直下を再列挙して本順序を復元する。
    // Archive と FolderBook を同列に並べるため、ここは Library 側の表示順と同じ
    // 判定規則を再利用する。
    let books = book_nav::list_supported_navigation_books_in_dir(current_path);
    let anchor_index = books
        .iter()
        .position(|p| paths_equivalent_for_selection(p.as_path(), current_path))
        .unwrap_or(0);
    let current_present = books
        .get(anchor_index)
        .is_some_and(|p| paths_equivalent_for_selection(p.as_path(), current_path));
    ResolvedNavigationBooks {
        books,
        anchor_index,
        current_present,
    }
}
