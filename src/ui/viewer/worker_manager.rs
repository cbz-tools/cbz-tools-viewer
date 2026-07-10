use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{mpsc, Arc, Mutex, OnceLock, RwLock},
    thread,
    time::Duration,
};

use eframe::egui;

use crate::{
    domain::{app_settings::ViewerQuality, archive::BookId, archive_settings::SpreadMode},
    infra::worker::viewer_loader::{
        ViewerLoadRequest, ViewerLoader, ViewerResult, ViewerResultKind,
    },
};

use super::{
    auto_spread_plan::AutoSpreadPlan,
    decode_layout::request_display_width_for_pair,
    state::{RgbaCacheKey, RgbaPageCache},
    streaming_cache::{
        desired_auto_streaming_sequence, SimpleStreamingCachePolicy, StreamingCachePlanner,
        StreamingCacheStopReason, StreamingCompletionAdmissionInput, StreamingCompletionDropReason,
    },
    working_set::{
        BgAdmissionState, BgInflightEntry, BgRenderContext, PageRenderSignatureKey,
        RenderSignature, WorkingSetAnchorPage,
    },
};

#[cfg(any(debug_assertions, test))]
const DEBUG_STATE_RESPONSE_TIMEOUT: Duration = Duration::from_millis(250);
const COMMAND_LOOP_POLL_INTERVAL: Duration = Duration::from_millis(8);

type StreamingCacheContext = (
    Vec<u32>,
    std::collections::HashSet<u32>,
    std::collections::HashSet<u32>,
    std::collections::HashSet<u32>,
    Vec<u32>,
    super::streaming_cache::StreamingCachePlan,
);

fn bg_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("CBZ_VIEWER_BG_TRACE")
            .map(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && value != "false"
            })
            .unwrap_or(false)
    })
}

macro_rules! bg_trace_debug {
    ($($arg:tt)*) => {
        if bg_trace_enabled() {
            tracing::debug!($($arg)*);
        }
    };
}

/// Viewer worker manager に渡す現在状態のスナップショット。
/// `requested_page` / `displayed_page` / `target_page` / visible pages は物理ページ index。
/// 優先順位は持たず、Policy / Planner が参照する入力だけを集める。
#[derive(Clone, Debug)]
pub(super) struct ViewerWorkerManagerSnapshot {
    pub(super) generation: u64,
    pub(super) book_id: BookId,
    pub(super) book_path: Arc<Path>,
    pub(super) page_count: u32,
    pub(super) spread_setting: SpreadMode,
    pub(super) cover_blank: bool,
    pub(super) quality: ViewerQuality,
    /// ViewerState から共有される不変の AUTO display plan。
    pub(super) auto_spread_plan: Option<Arc<AutoSpreadPlan>>,
    /// Viewer が扱う物理ページ位置。
    pub(super) requested_page: u32,
    pub(super) displayed_page: u32,
    pub(super) target_page: u32,
    /// Viewer 上で現在表示中の先頭物理ページ。
    pub(super) visible_page_first: Option<u32>,
    /// Viewer 上で現在表示中の2枚目の物理ページ。
    pub(super) visible_page_second: Option<u32>,
    pub(super) loading: bool,
    pub(super) nav_mode_follow_latest: bool,
    pub(super) prefetch_dir: i32,
    pub(super) max_tex_side: u32,
    pub(super) full_equivalent_area_w: u32,
    pub(super) full_equivalent_area_h: u32,
    pub(super) background_worker_count: usize,
    pub(super) rgba_cache_max_mb: u16,
    pub(super) active_animation_stream_view: Option<u32>,
    pub(super) animation_stream_request_id: Option<u64>,
}

impl ViewerWorkerManagerSnapshot {
    pub(super) fn current_generation(&self) -> u64 {
        self.generation
    }
}

fn bg_cache_saturation_reset_required(
    previous: Option<&ViewerWorkerManagerSnapshot>,
    next: &ViewerWorkerManagerSnapshot,
) -> bool {
    previous.is_none_or(|prev| {
        prev.book_id != next.book_id
            || prev.rgba_cache_max_mb != next.rgba_cache_max_mb
            || prev.quality != next.quality
            || prev.max_tex_side != next.max_tex_side
            || prev.full_equivalent_area_w != next.full_equivalent_area_w
            || prev.full_equivalent_area_h != next.full_equivalent_area_h
            || prev.spread_setting != next.spread_setting
            || prev.cover_blank != next.cover_blank
    })
}

fn streaming_candidate_limit(worker_capacity: usize) -> usize {
    worker_capacity.max(1).saturating_mul(4)
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) enum ViewerWorkerManagerNotification {
    BgDispatched {
        generation: u64,
        request_id: u64,
        page: u32,
        render_signature: RenderSignature,
    },
    DroppedStale {
        request_id: u64,
        reason: &'static str,
    },
    Error {
        message: String,
    },
}

#[cfg(any(debug_assertions, test))]
#[derive(Debug, Clone, Default)]
pub(super) struct ViewerWorkerManagerDebugState {
    pub(super) inflight_by_request_id: usize,
    pub(super) fifo_len: usize,
    pub(super) dispatch_limit_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct L2StreamingStatus {
    pub(super) generation: u64,
    pub(super) book_id: Option<BookId>,
    pub(super) settled: bool,
}

#[derive(Debug)]
enum ViewerWorkerManagerCommand {
    Update(ViewerWorkerManagerSnapshot),
    #[cfg(any(debug_assertions, test))]
    DebugState(mpsc::Sender<ViewerWorkerManagerDebugState>),
    Shutdown,
}

enum WorkerManagerCommandOutcome {
    None,
    UpdateApplied,
    Shutdown,
}

/// Policy / Planner / BG worker / BG RGBA cache を接続して実行する窓口。
/// 優先順位は持たず、snapshot を受けて計画を回すだけに留める。
pub(super) struct ViewerWorkerManagerHandle {
    command_tx: mpsc::Sender<ViewerWorkerManagerCommand>,
    notification_rx: Mutex<mpsc::Receiver<ViewerWorkerManagerNotification>>,
    bg_rgba_cache: Arc<RwLock<RgbaPageCache>>,
    l2_status: Arc<RwLock<L2StreamingStatus>>,
}

impl ViewerWorkerManagerHandle {
    pub(super) fn spawn(loader: Arc<ViewerLoader>, repaint_ctx: egui::Context) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (notification_tx, notification_rx) = mpsc::channel();
        let bg_rgba_cache = Arc::new(RwLock::new(RgbaPageCache::new()));
        let bg_rgba_cache_thread = Arc::clone(&bg_rgba_cache);
        let l2_status = Arc::new(RwLock::new(L2StreamingStatus::default()));
        let l2_status_thread = Arc::clone(&l2_status);
        let repaint_ctx_thread = repaint_ctx.clone();

        thread::Builder::new()
            .name("viewer-worker-manager".into())
            .spawn(move || {
                worker_manager_loop(
                    loader,
                    bg_rgba_cache_thread,
                    l2_status_thread,
                    repaint_ctx_thread,
                    command_rx,
                    notification_tx,
                )
            })
            .expect("viewer worker manager thread spawn");

        Self {
            command_tx,
            notification_rx: Mutex::new(notification_rx),
            bg_rgba_cache,
            l2_status,
        }
    }

    pub(super) fn update_state(&self, snapshot: ViewerWorkerManagerSnapshot) {
        let _ = self
            .command_tx
            .send(ViewerWorkerManagerCommand::Update(snapshot));
    }

    pub(super) fn try_recv_notification(&self) -> Option<ViewerWorkerManagerNotification> {
        self.notification_rx
            .lock()
            .ok()
            .and_then(|rx| rx.try_recv().ok())
    }

    pub(super) fn flush_notifications(&self) {
        while self.try_recv_notification().is_some() {}
    }

    #[cfg(any(debug_assertions, test))]
    pub(super) fn debug_state(&self) -> Option<ViewerWorkerManagerDebugState> {
        let (tx, rx) = mpsc::channel();
        if self
            .command_tx
            .send(ViewerWorkerManagerCommand::DebugState(tx))
            .is_err()
        {
            return None;
        }
        rx.recv_timeout(DEBUG_STATE_RESPONSE_TIMEOUT).ok()
    }

    pub(super) fn bg_rgba_cache(&self) -> Arc<RwLock<RgbaPageCache>> {
        Arc::clone(&self.bg_rgba_cache)
    }

    pub(super) fn l2_status(&self) -> L2StreamingStatus {
        self.l2_status
            .read()
            .map(|status| status.clone())
            .unwrap_or_default()
    }
}

impl Drop for ViewerWorkerManagerHandle {
    fn drop(&mut self) {
        let _ = self.command_tx.send(ViewerWorkerManagerCommand::Shutdown);
    }
}

struct ViewerWorkerManagerState {
    snapshot: Option<ViewerWorkerManagerSnapshot>,
    active_generation: Option<u64>,
    bg_cache_saturated: bool,
    bg_admission_state: HashMap<PageRenderSignatureKey, BgAdmissionState>,
    bg_inflight_by_request_id: HashMap<u64, BgInflightEntry>,
    bg_inflight_by_key: HashMap<PageRenderSignatureKey, u64>,
    prefetch_inflight_pages_by_shard: Vec<Option<u32>>,
    background_worker_count: usize,
    bg_summary_dispatch_count: usize,
}

fn worker_manager_loop(
    loader: Arc<ViewerLoader>,
    bg_rgba_cache: Arc<RwLock<RgbaPageCache>>,
    l2_status: Arc<RwLock<L2StreamingStatus>>,
    repaint_ctx: egui::Context,
    command_rx: mpsc::Receiver<ViewerWorkerManagerCommand>,
    notification_tx: mpsc::Sender<ViewerWorkerManagerNotification>,
) {
    let mut state = ViewerWorkerManagerState {
        snapshot: None,
        active_generation: None,
        bg_cache_saturated: false,
        bg_admission_state: HashMap::new(),
        bg_inflight_by_request_id: HashMap::new(),
        bg_inflight_by_key: HashMap::new(),
        prefetch_inflight_pages_by_shard: Vec::new(),
        background_worker_count: 0,
        bg_summary_dispatch_count: 0,
    };

    loop {
        if matches!(
            drain_worker_manager_commands(
                &loader,
                &bg_rgba_cache,
                &l2_status,
                &mut state,
                &notification_tx,
                &command_rx,
                usize::MAX,
            ),
            WorkerManagerCommandOutcome::Shutdown
        ) {
            return;
        }

        let mut handled_any = false;
        while let Some(result) = loader.try_recv_background() {
            handled_any = true;
            let should_pump = handle_background_result_streaming(
                &bg_rgba_cache,
                &l2_status,
                &mut state,
                &repaint_ctx,
                &notification_tx,
                result,
            );
            let command_outcome = drain_worker_manager_commands(
                &loader,
                &bg_rgba_cache,
                &l2_status,
                &mut state,
                &notification_tx,
                &command_rx,
                1,
            );
            if matches!(command_outcome, WorkerManagerCommandOutcome::Shutdown) {
                return;
            }
            if should_pump && matches!(command_outcome, WorkerManagerCommandOutcome::None) {
                pump_background_work(
                    &loader,
                    &bg_rgba_cache,
                    &l2_status,
                    &mut state,
                    &notification_tx,
                    "bg_completion",
                );
            }
        }

        if handled_any {
            continue;
        }

        match command_rx.recv_timeout(COMMAND_LOOP_POLL_INTERVAL) {
            Ok(command) => {
                if matches!(
                    apply_worker_manager_command(
                        command,
                        &loader,
                        &bg_rgba_cache,
                        &l2_status,
                        &mut state,
                        &notification_tx,
                    ),
                    WorkerManagerCommandOutcome::Shutdown
                ) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn apply_worker_manager_command(
    command: ViewerWorkerManagerCommand,
    loader: &Arc<ViewerLoader>,
    bg_rgba_cache: &Arc<RwLock<RgbaPageCache>>,
    l2_status: &Arc<RwLock<L2StreamingStatus>>,
    state: &mut ViewerWorkerManagerState,
    notification_tx: &mpsc::Sender<ViewerWorkerManagerNotification>,
) -> WorkerManagerCommandOutcome {
    match command {
        ViewerWorkerManagerCommand::Update(snapshot) => {
            let cache_reset_required =
                bg_cache_saturation_reset_required(state.snapshot.as_ref(), &snapshot);
            if cache_reset_required {
                state.bg_cache_saturated = false;
                state.bg_inflight_by_request_id.clear();
                state.bg_inflight_by_key.clear();
                state.prefetch_inflight_pages_by_shard =
                    vec![None; snapshot.background_worker_count.max(1)];
                state.bg_admission_state.clear();
            }
            state.active_generation = Some(snapshot.generation);
            if state.background_worker_count != snapshot.background_worker_count {
                state.background_worker_count = snapshot.background_worker_count;
                state.prefetch_inflight_pages_by_shard =
                    vec![None; snapshot.background_worker_count.max(1)];
            }
            state.snapshot = Some(snapshot);
            pump_background_work(
                loader,
                bg_rgba_cache,
                l2_status,
                state,
                notification_tx,
                "initial_pump",
            );
            WorkerManagerCommandOutcome::UpdateApplied
        }
        #[cfg(any(debug_assertions, test))]
        ViewerWorkerManagerCommand::DebugState(reply_tx) => {
            let _ = reply_tx.send(capture_debug_state_streaming(state, bg_rgba_cache));
            WorkerManagerCommandOutcome::None
        }
        ViewerWorkerManagerCommand::Shutdown => WorkerManagerCommandOutcome::Shutdown,
    }
}

fn drain_worker_manager_commands(
    loader: &Arc<ViewerLoader>,
    bg_rgba_cache: &Arc<RwLock<RgbaPageCache>>,
    l2_status: &Arc<RwLock<L2StreamingStatus>>,
    state: &mut ViewerWorkerManagerState,
    notification_tx: &mpsc::Sender<ViewerWorkerManagerNotification>,
    command_rx: &mpsc::Receiver<ViewerWorkerManagerCommand>,
    max_commands: usize,
) -> WorkerManagerCommandOutcome {
    let mut processed = 0usize;
    loop {
        if processed >= max_commands {
            return WorkerManagerCommandOutcome::None;
        }
        match command_rx.try_recv() {
            Ok(command) => {
                processed = processed.saturating_add(1);
                match apply_worker_manager_command(
                    command,
                    loader,
                    bg_rgba_cache,
                    l2_status,
                    state,
                    notification_tx,
                ) {
                    WorkerManagerCommandOutcome::None => {}
                    WorkerManagerCommandOutcome::UpdateApplied => {}
                    WorkerManagerCommandOutcome::Shutdown => {
                        return WorkerManagerCommandOutcome::Shutdown;
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => return WorkerManagerCommandOutcome::None,
            Err(mpsc::TryRecvError::Disconnected) => return WorkerManagerCommandOutcome::Shutdown,
        }
    }
}

fn bg_cache_revision(bg_rgba_cache: &Arc<RwLock<RgbaPageCache>>) -> u64 {
    bg_rgba_cache
        .read()
        .map(|cache| cache.mutation_revision())
        .unwrap_or(0)
}

fn streaming_anchor_page(snapshot: &ViewerWorkerManagerSnapshot) -> u32 {
    navigation_base_page(snapshot).min(snapshot.page_count.saturating_sub(1))
}

fn streaming_anchor_source(snapshot: &ViewerWorkerManagerSnapshot) -> &'static str {
    if snapshot.nav_mode_follow_latest {
        "target_page"
    } else if snapshot.loading {
        "requested_page"
    } else {
        "displayed_page"
    }
}

/// 選択中の物理ページから working-set の起点を作る。
fn working_set_anchor_from_navigation_state(
    snapshot: &ViewerWorkerManagerSnapshot,
) -> WorkingSetAnchorPage {
    let physical_page = navigation_base_page(snapshot).min(snapshot.page_count.saturating_sub(1));
    match snapshot.spread_setting {
        SpreadMode::Single => WorkingSetAnchorPage::Single {
            requested_page: physical_page,
        },
        SpreadMode::Spread => WorkingSetAnchorPage::Spread {
            navigation_page: physical_page,
        },
        SpreadMode::Auto => WorkingSetAnchorPage::Auto {
            navigation_page: physical_page,
        },
    }
}

fn bg_cache_limit_bytes(snapshot: &ViewerWorkerManagerSnapshot) -> usize {
    (snapshot.rgba_cache_max_mb as usize)
        .saturating_mul(1024)
        .saturating_mul(1024)
}

fn bg_cache_page_eviction_bytes(bg_rgba_cache: &RgbaPageCache) -> HashMap<u32, usize> {
    let mut page_eviction_bytes: HashMap<u32, usize> = HashMap::new();
    for entry in bg_rgba_cache.ready_entry_snapshots() {
        let bytes = page_eviction_bytes.entry(entry.page).or_insert(0usize);
        *bytes = (*bytes).saturating_add(entry.bytes);
    }
    page_eviction_bytes
}

fn bg_cache_protected_pages(snapshot: &ViewerWorkerManagerSnapshot) -> HashSet<u32> {
    let (visible_left, visible_right) = snapshot_visible_pages(snapshot);
    let mut protected_pages = HashSet::new();
    for page in [visible_left, visible_right].into_iter().flatten() {
        protected_pages.insert(page);
    }
    protected_pages
}

fn build_streaming_cache_context(
    snapshot: &ViewerWorkerManagerSnapshot,
    state: &ViewerWorkerManagerState,
    bg_cache_pages: &[u32],
    cache_current_bytes: usize,
    cache_max_bytes: usize,
    cache_saturated: bool,
    worker_capacity: usize,
) -> StreamingCacheContext {
    // Policy で順序を作り、Planner で cache / inflight / budget と照合する。
    // Manager は結果をそのまま実行し、ここで優先順位を再計算しない。
    let current_physical_page =
        streaming_anchor_page(snapshot).min(snapshot.page_count.saturating_sub(1));
    let (visible_left, visible_right) = snapshot_visible_pages(snapshot);
    let mut visible_pages = Vec::new();
    let mut visible_page_set = HashSet::new();
    for page in [visible_left, visible_right].into_iter().flatten() {
        if visible_page_set.insert(page) {
            visible_pages.push(page);
        }
    }
    let protected_pages = visible_page_set.clone();
    let too_large_pages = state
        .bg_admission_state
        .iter()
        .filter_map(|(key, admission)| {
            matches!(
                *admission,
                BgAdmissionState::TooLargeForBgRgba | BgAdmissionState::InsertDidNotSurvive
            )
            .then_some(key.page)
        })
        .collect::<HashSet<_>>();
    let inflight_pages = state
        .bg_inflight_by_request_id
        .values()
        .map(|entry| entry.page)
        .collect::<HashSet<_>>();
    let desired_sequence = if snapshot.spread_setting == SpreadMode::Auto {
        if let Some(plan) = snapshot.auto_spread_plan.as_deref() {
            desired_auto_streaming_sequence(
                plan,
                current_physical_page,
                snapshot.page_count,
                visible_pages.iter().copied(),
            )
        } else {
            SimpleStreamingCachePolicy::new(
                current_physical_page,
                snapshot.page_count,
                visible_pages.clone(),
            )
            .desired_sequence()
        }
    } else {
        SimpleStreamingCachePolicy::new(
            current_physical_page,
            snapshot.page_count,
            visible_pages.clone(),
        )
        .desired_sequence()
    };
    let plan = StreamingCachePlanner::plan(
        desired_sequence.clone(),
        bg_cache_pages,
        cache_current_bytes,
        cache_max_bytes,
        cache_saturated,
        streaming_candidate_limit(worker_capacity),
        &inflight_pages,
        &too_large_pages,
        &visible_page_set,
        &protected_pages,
        worker_capacity,
    );
    (
        visible_pages,
        visible_page_set,
        protected_pages,
        too_large_pages,
        desired_sequence,
        plan,
    )
}

fn refresh_bg_rgba_cache_limit(
    bg_rgba_cache: &mut RgbaPageCache,
    snapshot: &ViewerWorkerManagerSnapshot,
) -> (Vec<u32>, usize, usize) {
    let protected_pages = bg_cache_protected_pages(snapshot);
    let current_physical_page =
        streaming_anchor_page(snapshot).min(snapshot.page_count.saturating_sub(1));
    let max_bytes = bg_cache_limit_bytes(snapshot);
    let _ = bg_rgba_cache.set_max_bytes_with_context(
        max_bytes,
        current_physical_page,
        &protected_pages,
    );
    (
        bg_rgba_cache.page_order(),
        bg_rgba_cache.current_bytes(),
        bg_rgba_cache.max_bytes(),
    )
}

fn bg_result_is_stale(
    snapshot: &ViewerWorkerManagerSnapshot,
    entry: &BgInflightEntry,
) -> Option<&'static str> {
    let ctx = &entry.render_context;
    if snapshot.book_id != ctx.book_id {
        return Some("book_changed");
    }
    if snapshot.quality != ctx.quality {
        return Some("render_condition_changed");
    }
    if snapshot.max_tex_side != ctx.max_tex_side {
        return Some("render_condition_changed");
    }
    if snapshot.full_equivalent_area_w != ctx.full_equivalent_area_w
        || snapshot.full_equivalent_area_h != ctx.full_equivalent_area_h
    {
        return Some("render_condition_changed");
    }
    if snapshot.spread_setting != ctx.spread_setting || snapshot.cover_blank != ctx.cover_blank {
        return Some("render_condition_changed");
    }
    let current_layout = resolve_candidate_layout(snapshot, entry.page);
    let current_signature = RenderSignature::from_decode_request(
        snapshot.quality,
        current_layout.page_decode_w,
        current_layout.page_decode_h,
        snapshot.max_tex_side,
    );
    if current_layout.effective_spread != ctx.spread_mode
        || current_signature != entry.render_signature
    {
        return Some("render_condition_changed");
    }
    None
}

fn dispatch_streaming_pages(
    loader: &Arc<ViewerLoader>,
    state: &mut ViewerWorkerManagerState,
    notification_tx: &mpsc::Sender<ViewerWorkerManagerNotification>,
    snapshot: &ViewerWorkerManagerSnapshot,
    dispatch_pages: Vec<u32>,
    refill_reason: &'static str,
) -> usize {
    let requested = dispatch_pages.len();
    let mut dispatched = 0usize;
    let mut skipped_shard_busy = 0usize;
    let mut skipped_selected_shard = 0usize;
    let mut selected_shards = vec![false; snapshot.background_worker_count.max(1)];
    let available_slots = snapshot
        .background_worker_count
        .saturating_sub(state.bg_inflight_by_request_id.len());
    for page in dispatch_pages {
        if dispatched >= available_slots {
            break;
        }
        let shard = page as usize % snapshot.background_worker_count.max(1);
        if selected_shards.get(shard).copied().unwrap_or(false) {
            skipped_selected_shard = skipped_selected_shard.saturating_add(1);
            continue;
        }
        if state
            .prefetch_inflight_pages_by_shard
            .get(shard)
            .and_then(|slot| *slot)
            .is_some()
        {
            skipped_shard_busy = skipped_shard_busy.saturating_add(1);
            continue;
        }
        let layout = resolve_candidate_layout(snapshot, page);
        let render_signature = RenderSignature::from_decode_request(
            snapshot.quality,
            layout.page_decode_w,
            layout.page_decode_h,
            snapshot.max_tex_side,
        );
        let request_id = loader.send_request(ViewerLoadRequest {
            path: Arc::clone(&snapshot.book_path),
            view_idx: page,
            page_left: Some(page),
            page_right: None,
            display_w: layout.page_decode_w,
            display_h: layout.page_decode_h,
            quality: snapshot.quality,
            max_tex_side: snapshot.max_tex_side,
            frame_cache_cap: frame_cache_cap_from_worker_count(snapshot.background_worker_count),
            nav_id: snapshot.generation,
            interactive: false,
        });
        state.bg_inflight_by_key.insert(
            PageRenderSignatureKey {
                page,
                render_signature,
            },
            request_id,
        );
        state.bg_inflight_by_request_id.insert(
            request_id,
            BgInflightEntry {
                request_id,
                page,
                render_signature,
                render_context: BgRenderContext {
                    book_id: snapshot.book_id.clone(),
                    quality: snapshot.quality,
                    spread_setting: snapshot.spread_setting.clone(),
                    spread_mode: layout.effective_spread,
                    cover_blank: snapshot.cover_blank,
                    full_equivalent_area_w: snapshot.full_equivalent_area_w,
                    full_equivalent_area_h: snapshot.full_equivalent_area_h,
                    max_tex_side: snapshot.max_tex_side,
                },
                working_set_anchor_page: working_set_anchor_from_navigation_state(snapshot),
                source_view: Some(page),
            },
        );
        state.bg_summary_dispatch_count = state.bg_summary_dispatch_count.saturating_add(1);
        if let Some(slot) = state.prefetch_inflight_pages_by_shard.get_mut(shard) {
            if slot.is_none() {
                *slot = Some(page);
            }
        }
        selected_shards[shard] = true;
        dispatched += 1;
        let _ = notification_tx.send(ViewerWorkerManagerNotification::BgDispatched {
            generation: snapshot.current_generation(),
            request_id,
            page,
            render_signature,
        });
    }
    bg_trace_debug!(
        "[viewer-worker-manager-dispatch] generation={} book_id={:?} refill_reason={} dispatch_requested={} dispatched={} skipped_shard_busy={} skipped_selected_shard={} available_slots={}",
        snapshot.current_generation(),
        snapshot.book_id,
        refill_reason,
        requested,
        dispatched,
        skipped_shard_busy,
        skipped_selected_shard,
        available_slots
    );
    dispatched
}

#[cfg(any(debug_assertions, test))]
fn capture_debug_state_streaming(
    state: &ViewerWorkerManagerState,
    bg_rgba_cache: &Arc<RwLock<RgbaPageCache>>,
) -> ViewerWorkerManagerDebugState {
    let Some(snapshot) = state.snapshot.as_ref() else {
        return ViewerWorkerManagerDebugState {
            inflight_by_request_id: state.bg_inflight_by_request_id.len(),
            fifo_len: 0,
            dispatch_limit_reason: None,
        };
    };

    let (bg_cache_pages, bg_cache_current_bytes, bg_cache_max_bytes) = match bg_rgba_cache.write() {
        Ok(mut cache) => refresh_bg_rgba_cache_limit(&mut cache, snapshot),
        Err(_) => (Vec::new(), 0, 0),
    };
    let available_slots = snapshot
        .background_worker_count
        .saturating_sub(state.bg_inflight_by_request_id.len());
    let (
        _visible_pages,
        _visible_page_set,
        _protected_pages,
        _too_large_pages,
        _desired_sequence,
        plan,
    ) = build_streaming_cache_context(
        snapshot,
        state,
        &bg_cache_pages,
        bg_cache_current_bytes,
        bg_cache_max_bytes,
        state.bg_cache_saturated,
        available_slots,
    );
    ViewerWorkerManagerDebugState {
        inflight_by_request_id: state.bg_inflight_by_request_id.len(),
        fifo_len: plan.dispatch_pages.len(),
        dispatch_limit_reason: plan.stop_reason.map(|reason| match reason {
            StreamingCacheStopReason::NoWorkerCapacity => "no_worker_capacity",
            StreamingCacheStopReason::CacheLimitUnavailable => "cache_limit_unavailable",
            StreamingCacheStopReason::CacheNotFullDispatch => "cache_not_full_dispatch",
            StreamingCacheStopReason::PriorityImprovementDispatch => {
                "priority_improvement_dispatch"
            }
            StreamingCacheStopReason::CacheFullNoPriorityImprovement => {
                "cache_full_no_priority_improvement"
            }
            StreamingCacheStopReason::NoDispatchablePages => "no_dispatchable_pages",
        }),
    }
}

/// 追跡済みの BG 完了を処理できたとき true。通常の pump を続ける合図にする。
fn handle_background_result_streaming(
    bg_rgba_cache: &Arc<RwLock<RgbaPageCache>>,
    l2_status: &Arc<RwLock<L2StreamingStatus>>,
    state: &mut ViewerWorkerManagerState,
    repaint_ctx: &egui::Context,
    notification_tx: &mpsc::Sender<ViewerWorkerManagerNotification>,
    result: ViewerResult,
) -> bool {
    let Some(snapshot) = state.snapshot.clone() else {
        update_l2_settled_status(l2_status, None, false);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::DroppedStale {
            request_id: result.request_id,
            reason: "missing_snapshot",
        });
        return false;
    };
    if result.kind == ViewerResultKind::AnimationFramesChunk {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::DroppedStale {
            request_id: result.request_id,
            reason: "animation_stream_not_managed_here",
        });
        return false;
    }

    let Some(entry) = state.bg_inflight_by_request_id.remove(&result.request_id) else {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::DroppedStale {
            request_id: result.request_id,
            reason: "untracked_bg_result",
        });
        return false;
    };
    let entry_key = entry.key();
    state.bg_inflight_by_key.remove(&entry_key);
    release_prefetch_inflight_slot(state, entry.page);

    if let Some(reason) = bg_result_is_stale(&snapshot, &entry) {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::DroppedStale {
            request_id: result.request_id,
            reason,
        });
        return false;
    }

    let frames = result
        .left
        .as_ref()
        .filter(|_| !result.left_is_animation_stream)
        .or_else(|| {
            result
                .right
                .as_ref()
                .filter(|_| !result.right_is_animation_stream)
        });
    let Some(frames) = frames else {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::DroppedStale {
            request_id: result.request_id,
            reason: "background_empty_frames",
        });
        return false;
    };
    let Some(bytes) = RgbaPageCache::static_rgba_bytes(frames.as_ref()) else {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::DroppedStale {
            request_id: result.request_id,
            reason: "background_non_static_frames",
        });
        return false;
    };
    let Some(mut cache) = bg_rgba_cache.write().ok() else {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::Error {
            message: "bg cache lock poisoned".to_owned(),
        });
        return false;
    };
    let (bg_cache_pages, bg_cache_current_bytes, bg_cache_max_bytes) =
        refresh_bg_rgba_cache_limit(&mut cache, &snapshot);
    let page_eviction_bytes = bg_cache_page_eviction_bytes(&cache);
    let available_slots = snapshot
        .background_worker_count
        .saturating_sub(state.bg_inflight_by_request_id.len());
    let (_, visible_page_set, protected_pages, _, desired_sequence, _plan) =
        build_streaming_cache_context(
            &snapshot,
            state,
            &bg_cache_pages,
            bg_cache_current_bytes,
            bg_cache_max_bytes,
            state.bg_cache_saturated,
            available_slots,
        );
    let existing_exact_bytes = cache
        .ready_entry_snapshots()
        .into_iter()
        .find(|snapshot| {
            snapshot.key.page == entry.page
                && snapshot.key.render_signature == entry.render_signature
        })
        .map(|snapshot| snapshot.bytes)
        .unwrap_or(0);
    let admission =
        StreamingCachePlanner::plan_completion_admission(StreamingCompletionAdmissionInput {
            desired_sequence: &desired_sequence,
            cache_pages: &bg_cache_pages,
            page_eviction_bytes: &page_eviction_bytes,
            cache_current_bytes: bg_cache_current_bytes.saturating_sub(existing_exact_bytes),
            cache_max_bytes: bg_cache_max_bytes,
            completed_page: entry.page,
            completed_entry_bytes: bytes,
            visible_pages: &visible_page_set,
            protected_pages: &protected_pages,
        });

    if !admission.admit {
        if matches!(
            admission.drop_reason,
            Some(StreamingCompletionDropReason::TooLargeForBgRgba)
        ) {
            state
                .bg_admission_state
                .insert(entry.key(), BgAdmissionState::TooLargeForBgRgba);
        }
        bg_trace_debug!(
            "[bg_rgba.drop] page={} completion_admission=drop reason={:?} current_rank={:?} worst_rank={:?} bg_rgba.current_bytes={} bg_rgba.max_bytes={} bg_rgba.entry_count={} render_signature.quality={:?} render_signature.target_w={} render_signature.target_h={} render_signature.max_tex_side={}",
            entry.page,
            admission.drop_reason,
            admission.completed_rank,
            admission.worst_evictable_rank,
            cache.current_bytes(),
            cache.max_bytes(),
            cache.entry_count(),
            entry.render_signature.quality,
            entry.render_signature.target_w,
            entry.render_signature.target_h,
            entry.render_signature.max_tex_side
        );
        update_l2_settled_status(
            l2_status,
            Some(&snapshot),
            state.bg_inflight_by_request_id.is_empty(),
        );
        return true;
    }

    let inserted = cache.insert_with_eviction_candidates(
        RgbaCacheKey {
            page: entry.page,
            render_signature: entry.render_signature,
        },
        Arc::clone(frames),
        "background",
        &admission.evict_candidates,
        &protected_pages,
    );
    if inserted.eviction.evicted_count > 0 || !inserted.inserted_survived {
        state.bg_cache_saturated = true;
    }
    if !inserted.inserted || !inserted.inserted_survived {
        state
            .bg_admission_state
            .insert(entry.key(), BgAdmissionState::InsertDidNotSurvive);
        let _ = notification_tx.send(ViewerWorkerManagerNotification::DroppedStale {
            request_id: result.request_id,
            reason: "background_insert_evicted",
        });
        bg_trace_debug!(
            "[bg_rgba.insert-drop] page={} inserted={} inserted_survived={} bg_rgba.current_bytes={} bg_rgba.max_bytes={} bg_rgba.entry_count={} evict_candidates={} render_signature.quality={:?} render_signature.target_w={} render_signature.target_h={} render_signature.max_tex_side={}",
            entry.page,
            inserted.inserted,
            inserted.inserted_survived,
            cache.current_bytes(),
            cache.max_bytes(),
            cache.entry_count(),
            admission.evict_candidates.len(),
            entry.render_signature.quality,
            entry.render_signature.target_w,
            entry.render_signature.target_h,
            entry.render_signature.max_tex_side
        );
        update_l2_settled_status(
            l2_status,
            Some(&snapshot),
            state.bg_inflight_by_request_id.is_empty(),
        );
        return true;
    }

    state
        .bg_admission_state
        .insert(entry.key(), BgAdmissionState::Admissible);

    bg_trace_debug!(
        "[bg_rgba.insert] page={} inserted_survived={} bg_rgba.current_bytes={} bg_rgba.max_bytes={} bg_rgba.entry_count={} evict_candidates={} render_signature.quality={:?} render_signature.target_w={} render_signature.target_h={} render_signature.max_tex_side={}",
        entry.page,
        inserted.inserted_survived,
        cache.current_bytes(),
        cache.max_bytes(),
        cache.entry_count(),
        admission.evict_candidates.len(),
        entry.render_signature.quality,
        entry.render_signature.target_w,
        entry.render_signature.target_h,
        entry.render_signature.max_tex_side
    );

    repaint_ctx.request_repaint();
    update_l2_settled_status(
        l2_status,
        Some(&snapshot),
        state.bg_inflight_by_request_id.is_empty(),
    );
    true
}

fn release_prefetch_inflight_slot(state: &mut ViewerWorkerManagerState, page: u32) {
    let shard_count = state.background_worker_count.max(1);
    let shard = page as usize % shard_count;
    let slot_len = state.prefetch_inflight_pages_by_shard.len();
    match state.prefetch_inflight_pages_by_shard.get_mut(shard) {
        Some(slot) if slot.is_none() || *slot == Some(page) => {
            *slot = None;
            bg_trace_debug!(
                "[bg_slot.release] page={} shard={} inflight_pages_by_shard_len={}",
                page,
                shard,
                slot_len
            );
        }
        Some(slot) => {
            bg_trace_debug!(
                "[bg_slot.release-skip] page={} shard={} current_slot={:?} inflight_pages_by_shard_len={}",
                page,
                shard,
                *slot,
                slot_len
            );
        }
        None => {
            bg_trace_debug!(
                "[bg_slot.release-skip] page={} shard={} current_slot=none inflight_pages_by_shard_len={}",
                page,
                shard,
                slot_len
            );
        }
    }
}

fn pump_background_work(
    loader: &Arc<ViewerLoader>,
    bg_rgba_cache: &Arc<RwLock<RgbaPageCache>>,
    l2_status: &Arc<RwLock<L2StreamingStatus>>,
    state: &mut ViewerWorkerManagerState,
    notification_tx: &mpsc::Sender<ViewerWorkerManagerNotification>,
    refill_reason: &'static str,
) {
    let Some(snapshot) = state.snapshot.clone() else {
        update_l2_settled_status(l2_status, None, false);
        bg_trace_debug!("[viewer-worker-manager-pump-skip] reason=missing_snapshot");
        return;
    };
    if snapshot.page_count == 0 {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        bg_trace_debug!(
            "[viewer-worker-manager-pump-skip] reason=empty_page_count generation={} book_id={:?}",
            snapshot.generation,
            snapshot.book_id
        );
        return;
    }
    if snapshot.active_animation_stream_view.is_some()
        || snapshot.animation_stream_request_id.is_some()
    {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        bg_trace_debug!(
            "[viewer-worker-manager-pump-skip] reason=animation_stream_active generation={} book_id={:?} active_view={:?} request_id={:?}",
            snapshot.generation,
            snapshot.book_id,
            snapshot.active_animation_stream_view,
            snapshot.animation_stream_request_id
        );
        return;
    }
    if snapshot.prefetch_dir == 0 {
        update_l2_settled_status(l2_status, Some(&snapshot), false);
        bg_trace_debug!(
            "[viewer-worker-manager-pump-skip] reason=zero_prefetch_dir generation={} book_id={:?} requested_page={} displayed_page={} target_page={}",
            snapshot.generation,
            snapshot.book_id,
            snapshot.requested_page,
            snapshot.displayed_page,
            snapshot.target_page
        );
        return;
    }

    let (bg_cache_pages, bg_cache_current_bytes, bg_cache_max_bytes) = {
        let Some(mut cache) = bg_rgba_cache.write().ok() else {
            update_l2_settled_status(l2_status, Some(&snapshot), false);
            bg_trace_debug!(
                "[viewer-worker-manager-pump-skip] reason=bg_cache_lock_poisoned generation={} book_id={:?}",
                snapshot.generation,
                snapshot.book_id
            );
            return;
        };
        refresh_bg_rgba_cache_limit(&mut cache, &snapshot)
    };
    let available_slots = snapshot
        .background_worker_count
        .saturating_sub(state.bg_inflight_by_request_id.len());
    let (_, _, _, _, desired_sequence, plan) = build_streaming_cache_context(
        &snapshot,
        state,
        &bg_cache_pages,
        bg_cache_current_bytes,
        bg_cache_max_bytes,
        state.bg_cache_saturated,
        available_slots,
    );
    bg_trace_debug!(
        "[viewer-worker-manager-pump-context] generation={} book_id={:?} anchor_source={} anchor_page={} requested_page={} displayed_page={} target_page={} current_page={} total_pages={} desired_head={:?}",
        snapshot.generation,
        snapshot.book_id,
        streaming_anchor_source(&snapshot),
        streaming_anchor_page(&snapshot),
        snapshot.requested_page,
        snapshot.displayed_page,
        snapshot.target_page,
        streaming_anchor_page(&snapshot),
        snapshot.page_count,
        desired_sequence.iter().take(20).copied().collect::<Vec<_>>()
    );

    match plan.stop_reason {
        Some(StreamingCacheStopReason::NoWorkerCapacity) => {
            update_l2_settled_status(l2_status, Some(&snapshot), false);
            bg_trace_debug!(
                "[viewer-worker-manager-pump-skip] reason=no_available_slots generation={} book_id={:?} refill_reason={}",
                snapshot.generation,
                snapshot.book_id,
                refill_reason
            );
            return;
        }
        Some(StreamingCacheStopReason::CacheLimitUnavailable) => {
            update_l2_settled_status(l2_status, Some(&snapshot), false);
            bg_trace_debug!(
                "[viewer-worker-manager-pump-skip] reason=cache_limit_unavailable generation={} book_id={:?} refill_reason={}",
                snapshot.generation,
                snapshot.book_id,
                refill_reason
            );
            return;
        }
        Some(StreamingCacheStopReason::CacheFullNoPriorityImprovement) => {
            update_l2_settled_status(
                l2_status,
                Some(&snapshot),
                state.bg_inflight_by_request_id.is_empty(),
            );
            bg_trace_debug!(
                "[viewer-worker-manager-pump-skip] reason=cache_full_no_priority_improvement generation={} book_id={:?} refill_reason={} desired_sequence_len={} cache_pages={} cache_bytes={}/{}",
                snapshot.generation,
                snapshot.book_id,
                refill_reason,
                desired_sequence.len(),
                bg_cache_pages.len(),
                bg_cache_current_bytes,
                bg_cache_max_bytes
            );
            return;
        }
        Some(StreamingCacheStopReason::NoDispatchablePages) => {
            update_l2_settled_status(
                l2_status,
                Some(&snapshot),
                state.bg_inflight_by_request_id.is_empty(),
            );
            bg_trace_debug!(
                "[viewer-worker-manager-pump-skip] reason=no_dispatchable_pages generation={} book_id={:?} refill_reason={} desired_sequence_len={} cache_pages={}",
                snapshot.generation,
                snapshot.book_id,
                refill_reason,
                desired_sequence.len(),
                bg_cache_pages.len()
            );
            return;
        }
        Some(StreamingCacheStopReason::CacheNotFullDispatch)
        | Some(StreamingCacheStopReason::PriorityImprovementDispatch)
        | None => {}
    }

    let dispatched = dispatch_streaming_pages(
        loader,
        state,
        notification_tx,
        &snapshot,
        plan.dispatch_pages.clone(),
        refill_reason,
    );

    bg_trace_debug!(
        "[viewer-worker-manager-pump] generation={} book_id={:?} refill_reason={} worker_count={} available_worker_slots={} desired_sequence_len={} dispatchable_pages={} evict_candidates={} dispatched={} cache_pages={} cache_bytes={}/{} inflight={} bg_cache_revision={}",
        snapshot.generation,
        snapshot.book_id,
        refill_reason,
        snapshot.background_worker_count,
        available_slots,
        desired_sequence.len(),
        plan.dispatch_pages.len(),
        plan.evict_candidates.len(),
        dispatched,
        bg_cache_pages.len(),
        bg_cache_current_bytes,
        bg_cache_max_bytes,
        state.bg_inflight_by_request_id.len(),
        bg_cache_revision(bg_rgba_cache)
    );
    update_l2_settled_status(l2_status, Some(&snapshot), false);
}

fn update_l2_settled_status(
    l2_status: &Arc<RwLock<L2StreamingStatus>>,
    snapshot: Option<&ViewerWorkerManagerSnapshot>,
    settled: bool,
) {
    let Ok(mut status) = l2_status.write() else {
        return;
    };
    status.generation = snapshot.map(|snapshot| snapshot.generation).unwrap_or(0);
    status.book_id = snapshot.map(|snapshot| snapshot.book_id.clone());
    status.settled = settled;
}

/// streaming / working-set の基準に使う物理ページ。
/// forced-spread index でも Auto plan 単位でもない。
fn navigation_base_page(snapshot: &ViewerWorkerManagerSnapshot) -> u32 {
    if snapshot.nav_mode_follow_latest {
        snapshot.target_page
    } else if snapshot.loading {
        snapshot.requested_page
    } else {
        snapshot.displayed_page
    }
}

/// Viewer 上で現在表示中の物理ページ。
fn snapshot_visible_pages(snapshot: &ViewerWorkerManagerSnapshot) -> (Option<u32>, Option<u32>) {
    (snapshot.visible_page_first, snapshot.visible_page_second)
}

fn is_leading_cover_blank_spread(
    snapshot: &ViewerWorkerManagerSnapshot,
    physical_page: u32,
) -> bool {
    snapshot.cover_blank
        && physical_page == 0
        && !matches!(snapshot.spread_setting, SpreadMode::Single)
        && snapshot.page_count > 0
}

#[derive(Clone, Copy, Debug)]
struct BgCandidateLayout {
    effective_spread: bool,
    page_decode_w: u32,
    page_decode_h: u32,
}

/// 現在の設定から、候補物理ページに対応するページを解決する。
fn resolve_candidate_pages(
    snapshot: &ViewerWorkerManagerSnapshot,
    physical_page: u32,
) -> (Option<u32>, Option<u32>) {
    if snapshot.page_count == 0 || physical_page >= snapshot.page_count {
        return (None, None);
    }
    if is_leading_cover_blank_spread(snapshot, physical_page) {
        return (Some(0), None);
    }
    match snapshot.spread_setting {
        SpreadMode::Auto => snapshot
            .auto_spread_plan
            .as_deref()
            .and_then(|plan| plan.pages_for_logical_page(physical_page))
            .map(|(first, second)| (Some(first), second))
            .unwrap_or((Some(physical_page), None)),
        SpreadMode::Spread => {
            if snapshot.cover_blank && physical_page == 0 {
                (Some(0), None)
            } else if physical_page + 1 < snapshot.page_count {
                (Some(physical_page), Some(physical_page + 1))
            } else {
                (Some(physical_page), None)
            }
        }
        SpreadMode::Single => (Some(physical_page), None),
    }
}

/// 候補物理ページの decode レイアウトを作る。
fn resolve_candidate_layout(
    snapshot: &ViewerWorkerManagerSnapshot,
    physical_page: u32,
) -> BgCandidateLayout {
    let (_page_left, page_right) = resolve_candidate_pages(snapshot, physical_page);
    let effective_spread =
        page_right.is_some() || is_leading_cover_blank_spread(snapshot, physical_page);
    let page_decode_w =
        request_display_width_for_pair(snapshot.full_equivalent_area_w, effective_spread);
    BgCandidateLayout {
        effective_spread,
        page_decode_w,
        page_decode_h: snapshot.full_equivalent_area_h,
    }
}

fn frame_cache_cap_from_worker_count(worker_count: usize) -> usize {
    crate::infra::worker::viewer_loader::frame_cache_cap_from_worker_count(worker_count)
}
