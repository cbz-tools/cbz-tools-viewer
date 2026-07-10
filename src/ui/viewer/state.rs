use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;

use crate::domain::app_settings::{ReadingDirection, ViewerQuality};
use crate::domain::archive::{BookId, BookMeta};
use crate::domain::archive_settings::{clamp_slideshow_interval_secs, SpreadMode};
use crate::domain::performance::{mib_to_bytes, split_mib_evenly, PerformanceSettingsResolved};
use crate::infra::archive::{viewer_page_display_labels, viewer_page_entry_names};
use crate::infra::image::decode as img;
use crate::infra::ipc::AdjacentBook;
use crate::infra::page_map::viewer_bootstrap::ViewerPageMapMode;
use crate::infra::page_map::viewer_bootstrap::try_load_existing_viewer_page_map_for_spad;
use crate::infra::worker::viewer_loader::{
    ViewerLoadRequest, ViewerLoader, ViewerResult, ViewerResultKind,
};
use crate::ui::thumb_cache::LoadedDiskThumb;
use crate::util::path_eq::paths_equivalent_for_selection;

use super::auto_spread_plan::{build_auto_spread_plan, AutoSpreadPlan};
use super::decode_layout::{request_display_width_for_pair, static_rgba_bytes_for_decode};
#[cfg(debug_assertions)]
use super::gpu_texture_history::GpuTextureHistorySnapshot;
use super::gpu_texture_history::{GpuTextureHistory, GpuTextureHit, GpuTextureHitSource};
use super::gpu_warmup_cache::GpuWarmupCache;
#[cfg(debug_assertions)]
use super::gpu_warmup_cache::GpuWarmupCacheSnapshot;
use super::gpu_warmup_planner::{
    plan_gpu_warmup, resolve_future_candidate, GpuWarmupCandidateSnapshot, GpuWarmupPlanSnapshot,
    RgbaReadyEntrySnapshot,
};
use super::streaming_cache::SimpleStreamingCachePolicy;
use super::worker_manager::{
    L2StreamingStatus, ViewerWorkerManagerHandle, ViewerWorkerManagerNotification,
    ViewerWorkerManagerSnapshot,
};
use super::working_set::{
    page_render_signature_rank, BgAdmissionState, Direction, DisplayRequirement,
    GpuTextureEntrySnapshot, PageRenderSignatureKey, RenderSignature, WorkingSetAnchorPage,
    WorkingSetPage, WorkingSetPlan,
};
use super::PageContent;

const PREFETCH_DEEP_IDLE_DELAY: Duration = Duration::from_millis(250);
const FOLLOW_LATEST_THRESHOLD: Duration = Duration::from_millis(180);
const NAV_CONSECUTIVE_WINDOW: Duration = Duration::from_millis(50);
const DISPLAY_TARGET_STABILITY_NUMERATOR: u32 = 105;
const DISPLAY_TARGET_STABILITY_DENOMINATOR: u32 = 100;
const RGBA_CACHE_HEADROOM_NUM: usize = 5;
const RGBA_CACHE_HEADROOM_DEN: usize = 4;
const PENDING_PLACEHOLDER_DELAY: Duration = Duration::from_millis(150);
const KEY_FEEDBACK_DURATION: Duration = Duration::from_secs(2);
const INTERACTIVE_DISPLAY_CANDIDATE_LIMIT: usize = 8;
const LOG_VIEWPORT_TRANSITION: bool = cfg!(debug_assertions);
const TRANSITION_LOG_FRAMES: u8 = 30;
fn spad_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("CBZ_VIEWER_SPAD_TRACE")
            .map(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && value != "false"
            })
            .unwrap_or(false)
    })
}

macro_rules! spad_trace_debug {
    ($($arg:tt)*) => {
        if spad_trace_enabled() {
            tracing::debug!($($arg)*);
        }
    };
}
pub(super) fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn spread_mode_tag(mode: SpreadMode) -> u8 {
    match mode {
        SpreadMode::Auto => 0,
        SpreadMode::Single => 1,
        SpreadMode::Spread => 2,
    }
}

#[derive(Clone, Copy)]
pub(super) enum RequestHitState {
    None,
    Hit(bool),
}

impl RequestHitState {
    fn as_log_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Hit(true) => "true",
            Self::Hit(false) => "false",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct InteractiveRequestCacheKey {
    entry_id: BookId,
    page_count: u32,
    spread_mode: u8,
    quality: ViewerQuality,
    display_w: u32,
    display_h: u32,
    max_tex_side: u32,
    working_set_anchor_page: u32,
    direction: Direction,
}

#[derive(Clone, Debug)]
pub(super) struct InteractiveRequestPlanCache {
    key: InteractiveRequestCacheKey,
    plan: WorkingSetPlan,
}

pub(super) struct NavTrace {
    started_at: Instant,
    request_left_hit: RequestHitState,
    request_right_hit: RequestHitState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisplayPageState {
    Ready,
    Missing,
}

pub(super) struct InteractivePendingGroup {
    group_id: u64,
    generation: u64,
    page_left: Option<u32>,
    page_right: Option<u32>,
    left_request_id: Option<u64>,
    right_request_id: Option<u64>,
    left_result: Option<crate::infra::worker::viewer_loader::ViewerResult>,
    right_result: Option<crate::infra::worker::viewer_loader::ViewerResult>,
}

pub(super) struct OverlayRenderResult {
    pub(super) new_view: Option<u32>,
    pub(super) interacting: bool,
}

pub struct ViewerStateInit {
    pub entry: BookMeta,
    pub start_page: u32,
    pub spad_session_id: u64,
    pub cover_blank: bool,
    pub quality_override: Option<ViewerQuality>,
    pub global_reading_direction: ReadingDirection,
    pub reading_direction_override: Option<ReadingDirection>,
    pub spread_setting: SpreadMode,
    pub performance_settings: PerformanceSettingsResolved,
    pub quality: ViewerQuality,
    pub slideshow_interval_secs: f32,
    pub full_equivalent_size_hint: Option<FullEquivalentSizeHint>,
    pub page_map_mode: ViewerPageMapMode,
}

pub(super) struct InteractiveGroupRequest {
    nav_id: u64,
    physical_page: u32,
    page_left: Option<u32>,
    page_right: Option<u32>,
    request_display_w: u32,
    request_display_h: u32,
    max_tex_side: u32,
}

pub(super) struct ViewRequestContext<'a> {
    nav_id: u64,
    physical_page: u32,
    display_w: u32,
    display_h: u32,
    max_tex_side: u32,
    ctx: &'a egui::Context,
    reason: &'static str,
}

type GpuWarmupInputs = (
    Vec<GpuWarmupCandidateSnapshot>,
    Vec<RgbaReadyEntrySnapshot>,
    Vec<GpuTextureEntrySnapshot>,
    Vec<GpuTextureEntrySnapshot>,
    HashMap<u32, DisplayRequirement>,
);

struct DisplayCommitPages {
    left_page: Option<u32>,
    right_page: Option<u32>,
}

struct DisplayCommitSlot<'a> {
    page: Option<u32>,
    content: Option<PageContent>,
    hit: Option<&'a GpuTextureHit>,
    register_gpu_history: bool,
}

struct DisplayCommitContext<'a> {
    result: &'a crate::infra::worker::viewer_loader::ViewerResult,
    upload_started: Instant,
    ctx: &'a egui::Context,
    poll_started: Instant,
    display_w: u32,
    display_h: u32,
    max_tex_side: u32,
    gpu_history_hit: bool,
    left: DisplayCommitSlot<'a>,
    right: DisplayCommitSlot<'a>,
}

struct FinalizeDisplayCommitContext<'a> {
    result: &'a ViewerResult,
    upload_elapsed: u128,
    poll_started: Instant,
    display_w: u32,
    display_h: u32,
    max_tex_side: u32,
    gpu_history_hit: bool,
    pages: DisplayCommitPages,
    ctx: &'a egui::Context,
}

struct SyntheticDisplayRequest {
    nav_id: u64,
    physical_page: u32,
    page_left: Option<u32>,
    page_right: Option<u32>,
    left: Option<Arc<Vec<img::FrameData>>>,
    right: Option<Arc<Vec<img::FrameData>>>,
    request_display_w: u32,
    request_display_h: u32,
    request_quality: ViewerQuality,
    request_max_tex_side: u32,
}

struct PartialDisplayReuseContext<'a> {
    nav_id: u64,
    physical_page: u32,
    page_left: Option<u32>,
    page_right: Option<u32>,
    request_display_w: u32,
    request_display_h: u32,
    max_tex_side: u32,
    display_w: u32,
    display_h: u32,
    ctx: &'a egui::Context,
    left_hit: Option<&'a GpuTextureHit>,
    right_hit: Option<&'a GpuTextureHit>,
}

struct PartialDisplayRequest {
    nav_id: u64,
    physical_page: u32,
    page_left: Option<u32>,
    page_right: Option<u32>,
    request_display_w: u32,
    request_display_h: u32,
    max_tex_side: u32,
}

struct InteractiveRgbaCacheInsertContext<'a> {
    request_kind: &'static str,
    pages: DisplayCommitPages,
    left: Option<&'a Arc<Vec<img::FrameData>>>,
    right: Option<&'a Arc<Vec<img::FrameData>>>,
    target_w: u32,
    target_h: u32,
    quality: &'a ViewerQuality,
    max_tex_side: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpadSide {
    Prev,
    Next,
}

impl SpadSide {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prev => "prev",
            Self::Next => "next",
        }
    }
}

#[derive(Clone)]
struct SpadReadyPage {
    frames: Arc<Vec<img::FrameData>>,
    render_signature: RenderSignature,
}

#[derive(Clone)]
pub struct SpadTargetLayoutSettings {
    pub spread_setting: SpreadMode,
    pub cover_blank: bool,
}

#[derive(Clone, Copy)]
enum SpadDecodeLayoutSource {
    CurrentLayout,
    TargetPageMap,
}

impl SpadDecodeLayoutSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::CurrentLayout => "current_layout",
            Self::TargetPageMap => "target_page_map",
        }
    }
}

#[derive(Clone, Copy)]
struct SpadTargetLayoutHint {
    source: SpadDecodeLayoutSource,
    page_count: u32,
    resolved_entry_page: u32,
    effective_spread: bool,
}

#[derive(Clone, Copy)]
struct SpadResolvedDecodeTarget {
    source: SpadDecodeLayoutSource,
    effective_spread: bool,
    decode_w: u32,
    decode_h: u32,
    two_page_rgba_bytes: usize,
}

struct SpadTargetState {
    path: PathBuf,
    _book_state: crate::infra::ipc::ViewerBookState,
    page_count: Option<u32>,
    entry_page: u32,
    layout_hint: Option<SpadTargetLayoutHint>,
    scheduled_pages: Vec<u32>,
    next_dispatch_index: usize,
    ready_pages: BTreeMap<u32, SpadReadyPage>,
    failed_pages: HashSet<u32>,
    current_bytes: usize,
    max_bytes: usize,
    guaranteed_bytes: usize,
    extra_budget_bytes: usize,
    exhausted: bool,
}

struct SpadInflightRequest {
    request_id: u64,
    session: u64,
    generation: u64,
    side: SpadSide,
    path: PathBuf,
    page: u32,
    render_signature: RenderSignature,
}

#[derive(Default)]
struct ViewerSpadState {
    session: u64,
    generation: u64,
    next_inflight: Option<SpadInflightRequest>,
    prev_inflight: Option<SpadInflightRequest>,
    prev: Option<SpadTargetState>,
    next: Option<SpadTargetState>,
    no_dispatch_logged: bool,
}

pub(crate) struct SpadPromotionPage {
    page: u32,
    frames: Arc<Vec<img::FrameData>>,
    render_signature: RenderSignature,
    target_path: PathBuf,
    session: u64,
    generation: u64,
    target_page_count: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default)]
struct SpadBudgetPlan {
    prev_guaranteed_bytes: usize,
    next_guaranteed_bytes: usize,
    prev_extra_budget_bytes: usize,
    next_extra_budget_bytes: usize,
    prev_total_budget_bytes: usize,
    next_total_budget_bytes: usize,
    l2_effective_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoundaryPreviewDirection {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BoundaryPreviewProbe {
    pub(super) direction: BoundaryPreviewDirection,
    pub(super) in_flight: bool,
    pub(super) request_id: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) enum BoundaryPreviewState {
    Hidden,
    Loading(BoundaryPreviewProbe),
    Ready {
        probe: BoundaryPreviewProbe,
        book: BookMeta,
        thumbnail: Option<LoadedDiskThumb>,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BoundaryPreviewReadyView<'a> {
    pub(super) direction: BoundaryPreviewDirection,
    pub(super) book: &'a BookMeta,
    pub(super) thumbnail: &'a LoadedDiskThumb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NavMode {
    Sequential,
    FollowLatest,
}

// ── RGBA ページキャッシュ ────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct RgbaCacheKey {
    pub(super) page: u32,
    pub(super) render_signature: RenderSignature,
}

struct CachedRgbaPage {
    frames: Arc<Vec<img::FrameData>>,
    bytes: usize,
    source: &'static str,
}

type TolerantCacheHit = (Arc<Vec<img::FrameData>>, &'static str, (u32, u32));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RgbaEvictionOutcome {
    pub evicted_count: usize,
    pub evicted_bytes: usize,
    pub nearest_evicted_distance: Option<u32>,
    pub protected_evicted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RgbaInsertOutcome {
    pub inserted: bool,
    pub inserted_survived: bool,
    pub eviction: RgbaEvictionOutcome,
}

/// BG と interactive の責務境界を越えない RGBA 専用 LRU。
/// 保持・参照・挿入・退避だけに閉じ、優先順位や dispatch 判断は持たない。
pub(super) struct RgbaPageCache {
    entries: HashMap<RgbaCacheKey, CachedRgbaPage>,
    order: VecDeque<RgbaCacheKey>,
    current_bytes: usize,
    max_bytes: usize,
    mutation_revision: u64,
}

impl RgbaPageCache {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            current_bytes: 0,
            max_bytes: 0,
            mutation_revision: 0,
        }
    }

    pub(super) fn set_max_bytes_with_context(
        &mut self,
        max_bytes: usize,
        current_page: u32,
        protected_pages: &HashSet<u32>,
    ) -> RgbaEvictionOutcome {
        self.max_bytes = max_bytes;
        self.evict_to_budget("budget_update", current_page, protected_pages)
    }

    pub(super) fn get(&mut self, key: &RgbaCacheKey) -> Option<Arc<Vec<img::FrameData>>> {
        let frames = self
            .entries
            .get(key)
            .map(|entry| Arc::clone(&entry.frames))?;
        self.order.retain(|k| k != key);
        self.order.push_back(key.clone());
        Some(frames)
    }

    /// Planner が選んだ exact L2 entry を非破壊参照する。
    /// LRU を touch せず、remove せず、統計も変えない。
    /// Executor が候補を再選択しないための参照 API。
    pub(super) fn peek_exact(
        &self,
        key: &RgbaCacheKey,
    ) -> Option<(Arc<Vec<img::FrameData>>, RenderSignature, &'static str)> {
        let entry = self.entries.get(key)?;
        Some((
            Arc::clone(&entry.frames),
            key.render_signature,
            entry.source,
        ))
    }

    fn best_suitable_candidate(
        &self,
        page: u32,
        requirement: DisplayRequirement,
    ) -> Option<RgbaCacheKey> {
        let mut best: Option<((u64, u32, u32, usize), RgbaCacheKey)> = None;
        for (candidate, entry) in &self.entries {
            let Some(rank) = page_render_signature_rank(
                candidate.page,
                candidate.render_signature,
                page,
                requirement,
                entry.bytes,
            ) else {
                continue;
            };
            if best.as_ref().is_none_or(|(prev, _)| rank < *prev) {
                best = Some((rank, candidate.clone()));
            }
        }
        best.map(|(_, key)| key)
    }

    fn get_suitable(
        &mut self,
        page: u32,
        requirement: DisplayRequirement,
    ) -> Option<(Arc<Vec<img::FrameData>>, RenderSignature)> {
        let key = self.best_suitable_candidate(page, requirement)?;
        let frames = self.get(&key)?;
        Some((frames, key.render_signature))
    }

    #[allow(dead_code)]
    fn peek_suitable(
        &self,
        page: u32,
        requirement: DisplayRequirement,
    ) -> Option<(Arc<Vec<img::FrameData>>, RenderSignature)> {
        let key = self.best_suitable_candidate(page, requirement)?;
        let entry = self.entries.get(&key)?;
        Some((Arc::clone(&entry.frames), key.render_signature))
    }

    #[allow(dead_code)]
    fn get_with_tolerance(&mut self, key: &RgbaCacheKey) -> Option<TolerantCacheHit> {
        if let Some(frames) = self.get(key) {
            return Some((
                frames,
                "exact",
                (key.render_signature.target_w, key.render_signature.target_h),
            ));
        }

        None
    }

    #[allow(dead_code)]
    pub(super) fn contains(&self, key: &RgbaCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    pub(super) fn contains_page(&self, page: u32) -> bool {
        self.entries.keys().any(|key| key.page == page)
    }

    pub(super) fn ready_entry_snapshots(&self) -> Vec<RgbaReadyEntrySnapshot> {
        self.entries
            .iter()
            .map(|(key, entry)| RgbaReadyEntrySnapshot {
                key: key.clone(),
                page: key.page,
                bytes: entry.bytes,
                signature: key.render_signature,
                source: entry.source,
            })
            .collect()
    }

    pub(super) fn page_order(&self) -> Vec<u32> {
        let mut pages = Vec::new();
        let mut seen = HashSet::new();
        for key in &self.order {
            if seen.insert(key.page) {
                pages.push(key.page);
            }
        }
        pages
    }

    pub(super) fn remove_page(&mut self, page: u32) -> RgbaEvictionOutcome {
        let mut evicted_count: usize = 0;
        let mut evicted_bytes: usize = 0;
        let mut nearest_evicted_distance: Option<u32> = None;
        let mut removed_any = false;
        let keys: Vec<RgbaCacheKey> = self
            .entries
            .keys()
            .filter(|key| key.page == page)
            .cloned()
            .collect();
        for key in keys {
            if let Some(old) = self.entries.remove(&key) {
                self.current_bytes = self.current_bytes.saturating_sub(old.bytes);
                self.order.retain(|k| k != &key);
                evicted_count = evicted_count.saturating_add(1);
                evicted_bytes = evicted_bytes.saturating_add(old.bytes);
                nearest_evicted_distance = Some(0);
                removed_any = true;
            }
        }
        if removed_any {
            self.bump_mutation_revision();
        }
        RgbaEvictionOutcome {
            evicted_count,
            evicted_bytes,
            nearest_evicted_distance,
            protected_evicted: false,
        }
    }

    #[allow(dead_code)]
    pub(super) fn has_tolerance_match(&self, key: &RgbaCacheKey) -> Option<(u32, u32)> {
        self.entries
            .get(key)
            .map(|_| (key.render_signature.target_w, key.render_signature.target_h))
    }

    #[allow(dead_code)]
    fn has_page_variant(&self, key: &RgbaCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    pub(super) fn insert_with_context(
        &mut self,
        key: RgbaCacheKey,
        frames: Arc<Vec<img::FrameData>>,
        source: &'static str,
        current_page: u32,
        protected_pages: &HashSet<u32>,
    ) -> RgbaInsertOutcome {
        let Some(bytes) = Self::static_rgba_bytes(frames.as_ref()) else {
            return RgbaInsertOutcome::default();
        };
        if self.max_bytes == 0 || bytes > self.max_bytes {
            return RgbaInsertOutcome::default();
        }
        let inserted_key = key.clone();
        if let Some(old) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(old.bytes);
            self.order.retain(|k| k != &key);
        }
        self.current_bytes = self.current_bytes.saturating_add(bytes);
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CachedRgbaPage {
                frames,
                bytes,
                source,
            },
        );
        self.bump_mutation_revision();
        let eviction = self.evict_to_budget("insert", current_page, protected_pages);
        let inserted_survived = self.entries.contains_key(&inserted_key);
        RgbaInsertOutcome {
            inserted: true,
            inserted_survived,
            eviction,
        }
    }

    pub(super) fn insert(
        &mut self,
        key: RgbaCacheKey,
        frames: Arc<Vec<img::FrameData>>,
        source: &'static str,
        current_page: u32,
        protected_pages: &HashSet<u32>,
    ) -> bool {
        self.insert_with_context(key, frames, source, current_page, protected_pages)
            .inserted
    }

    pub(super) fn insert_with_eviction_candidates(
        &mut self,
        key: RgbaCacheKey,
        frames: Arc<Vec<img::FrameData>>,
        source: &'static str,
        eviction_candidates: &[u32],
        protected_pages: &HashSet<u32>,
    ) -> RgbaInsertOutcome {
        // 退避候補の順位付けは planner 側の責務なので、この層では候補順をそのまま使う。
        // 候補だけでは budget を満たせない場合、挿入済みページでも残せないことがある。
        let Some(bytes) = Self::static_rgba_bytes(frames.as_ref()) else {
            return RgbaInsertOutcome::default();
        };
        if self.max_bytes == 0 || bytes > self.max_bytes {
            return RgbaInsertOutcome::default();
        }

        let inserted_key = key.clone();
        if let Some(old) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(old.bytes);
            self.order.retain(|k| k != &key);
        }
        self.current_bytes = self.current_bytes.saturating_add(bytes);
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CachedRgbaPage {
                frames,
                bytes,
                source,
            },
        );
        self.bump_mutation_revision();

        let mut eviction = RgbaEvictionOutcome::default();
        let mut inserted_survived = true;
        while self.current_bytes > self.max_bytes {
            let next_page = eviction_candidates.iter().copied().find(|page| {
                *page != inserted_key.page
                    && !protected_pages.contains(page)
                    && self.contains_page(*page)
            });
            let Some(page) = next_page else {
                break;
            };
            let outcome = self.remove_page(page);
            eviction.evicted_count = eviction.evicted_count.saturating_add(outcome.evicted_count);
            eviction.evicted_bytes = eviction.evicted_bytes.saturating_add(outcome.evicted_bytes);
            eviction.nearest_evicted_distance = Some(
                eviction
                    .nearest_evicted_distance
                    .map(|prev| prev.min(outcome.nearest_evicted_distance.unwrap_or(0)))
                    .unwrap_or_else(|| outcome.nearest_evicted_distance.unwrap_or(0)),
            );
            eviction.protected_evicted |= outcome.protected_evicted;
        }

        if self.current_bytes > self.max_bytes && self.contains_page(inserted_key.page) {
            let outcome = self.remove_page(inserted_key.page);
            eviction.evicted_count = eviction.evicted_count.saturating_add(outcome.evicted_count);
            eviction.evicted_bytes = eviction.evicted_bytes.saturating_add(outcome.evicted_bytes);
            eviction.nearest_evicted_distance = Some(
                eviction
                    .nearest_evicted_distance
                    .map(|prev| prev.min(outcome.nearest_evicted_distance.unwrap_or(0)))
                    .unwrap_or_else(|| outcome.nearest_evicted_distance.unwrap_or(0)),
            );
            inserted_survived = false;
        }

        RgbaInsertOutcome {
            inserted: true,
            inserted_survived: inserted_survived && self.entries.contains_key(&inserted_key),
            eviction,
        }
    }

    pub(super) fn static_rgba_bytes(frames: &[img::FrameData]) -> Option<usize> {
        if frames.len() != 1 {
            return None;
        }
        Some(frames[0].image.pixels.len())
    }

    fn evict_to_budget(
        &mut self,
        trigger: &'static str,
        current_page: u32,
        protected_pages: &HashSet<u32>,
    ) -> RgbaEvictionOutcome {
        let mut evicted_count: usize = 0;
        let mut evicted_bytes: usize = 0;
        let mut nearest_evicted_distance: Option<u32> = None;
        let mut protected_evicted = false;
        while self.current_bytes > self.max_bytes {
            let next_index = self
                .order
                .iter()
                .position(|key| !protected_pages.contains(&key.page));
            let key = match next_index {
                Some(index) => self
                    .order
                    .remove(index)
                    .expect("position came from the same deque"),
                None => match self.order.pop_front() {
                    Some(key) => key,
                    None => {
                        self.current_bytes = 0;
                        break;
                    }
                },
            };
            let outside_protected_set = !protected_pages.contains(&key.page);
            if !outside_protected_set {
                protected_evicted = true;
            }
            if let Some(old) = self.entries.remove(&key) {
                let cache_bytes_before = self.current_bytes;
                let distance = key.page.abs_diff(current_page);
                tracing::debug!(
                    "[bg_rgba.evict] page={} target={}x{} quality={:?} bytes={} reason=budget source={} trigger={} bg_rgba.current_bytes_before={} bg_rgba.current_bytes_after={} bg_rgba.max_bytes={} current_page={} distance={} outside_protected_set={}",
                    key.page,
                    key.render_signature.target_w,
                    key.render_signature.target_h,
                    key.render_signature.quality,
                    old.bytes,
                    old.source,
                    trigger,
                    cache_bytes_before,
                    cache_bytes_before.saturating_sub(old.bytes),
                    self.max_bytes,
                    current_page,
                    distance,
                    outside_protected_set
                );
                self.current_bytes = self.current_bytes.saturating_sub(old.bytes);
                evicted_count = evicted_count.saturating_add(1);
                evicted_bytes = evicted_bytes.saturating_add(old.bytes);
                nearest_evicted_distance = Some(
                    nearest_evicted_distance
                        .map(|prev| prev.min(distance))
                        .unwrap_or(distance),
                );
            }
        }
        if evicted_count > 0 {
            self.bump_mutation_revision();
        }
        RgbaEvictionOutcome {
            evicted_count,
            evicted_bytes,
            nearest_evicted_distance,
            protected_evicted,
        }
    }

    fn bump_mutation_revision(&mut self) {
        self.mutation_revision = self.mutation_revision.saturating_add(1);
    }

    pub(super) fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    pub(super) fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub(super) fn mutation_revision(&self) -> u64 {
        self.mutation_revision
    }

    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

// ── 状態 ─────────────────────────────────────────────────────────────────────

/// ビュー再生成後も持ち越す読み取り状態。
pub(super) struct ViewerPersistentState {
    pub entry: BookMeta,
    pub(super) page_display_labels: Vec<String>,
    pub(super) page_entry_names: Vec<String>,
    pub(super) page_map_mode: ViewerPageMapMode,
    pub(super) auto_spread_plan: Option<Arc<AutoSpreadPlan>>,
    pub page_count: u32,
    /// commit 済みの基準ページ。
    pub displayed_page: u32,
    /// UI と補充判断が追う、未反映を含む要求ページ。
    pub requested_page: u32,
    /// 最新入力を優先して追従するためのページ。
    pub target_page: u32,
    /// ユーザー設定の表示単位。
    pub spread_setting: SpreadMode,
    /// 現在のページ構成に対する実効見開き可否。
    pub spread_mode: bool,
    /// グローバル既定のページ開き設定。
    #[allow(dead_code)]
    pub(super) global_reading_direction: ReadingDirection,
    /// 本ごとのページ開き override。
    #[allow(dead_code)]
    pub(super) reading_direction_override: Option<ReadingDirection>,
    /// 見開き時に表紙前へ仮想ブランクを入れるか。
    pub cover_blank: bool,
    /// 本ごとの画質 override。None はグローバル設定に従う。
    pub quality_override: Option<ViewerQuality>,
    /// pending 中の判定揺れを抑えるための見開き構成スナップショット。
    pub(super) spread_snapshot: SpreadSnapshot,
}

#[derive(Clone, Debug)]
pub(super) struct SpreadSnapshot {
    pub(super) key: SpreadSnapshotKey,
    pub(super) effective_spread: bool,
    pub(super) page_right: Option<u32>,
    pub(super) valid: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SpreadSnapshotKey {
    pub(super) entry_id: BookId,
    pub(super) page_count: u32,
    pub(super) spread_setting: SpreadMode,
    pub(super) cover_blank: bool,
    /// 見開き再計算の基準になる物理ページ。
    pub(super) physical_page: u32,
}

#[derive(Clone, Debug)]
struct ReadingSessionState {
    displayed_any_page: bool,
    reached_end: bool,
    resume_page: Option<usize>,
    page_count: usize,
    notification_in_flight: bool,
    notification_sent: bool,
}

impl ReadingSessionState {
    fn new() -> Self {
        Self {
            displayed_any_page: false,
            reached_end: false,
            resume_page: None,
            page_count: 0,
            notification_in_flight: false,
            notification_sent: false,
        }
    }

    fn record_display_commit(
        &mut self,
        left_page: Option<u32>,
        right_page: Option<u32>,
        page_count: usize,
    ) {
        if left_page.is_none() && right_page.is_none() {
            return;
        }
        self.displayed_any_page = true;
        self.resume_page = left_page.or(right_page).map(|page| page as usize);
        if page_count > 0 {
            self.page_count = page_count;
            if let Some(last_page) = page_count.checked_sub(1).map(|page| page as u32) {
                self.reached_end = left_page == Some(last_page) || right_page == Some(last_page);
            }
        }
    }

    fn begin_notification(&mut self) -> Option<ReadingSessionSnapshot> {
        if self.notification_sent || self.notification_in_flight {
            return None;
        }
        self.notification_in_flight = true;
        Some(ReadingSessionSnapshot {
            displayed_any_page: self.displayed_any_page,
            reached_end: self.reached_end,
            resume_page: self.resume_page,
            page_count: self.page_count,
        })
    }

    fn complete_notification(&mut self) {
        self.notification_in_flight = false;
        self.notification_sent = true;
    }

    fn discard_notification(&mut self) {
        self.notification_in_flight = false;
        self.notification_sent = true;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ReadingSessionSnapshot {
    pub(crate) displayed_any_page: bool,
    pub(crate) reached_end: bool,
    pub(crate) resume_page: Option<usize>,
    pub(crate) page_count: usize,
}

/// 表示中ページを守る interactive RGBA と短期キャッシュ群。
pub(super) struct ViewerDisplayAssets {
    /// 左（または単ページ）コンテンツ。マンガでは右側に出る。
    pub content_left: Option<PageContent>,
    /// 右（見開きのみ）コンテンツ。マンガでは左側に出る。
    pub content_right: Option<PageContent>,
    /// 画面に出す保証キャッシュ。BG eviction から隔離する。
    pub(super) interactive_rgba_cache: RgbaPageCache,
    /// 表示済み texture の短期履歴。
    pub(super) gpu_texture_history: GpuTextureHistory,
    /// 未表示の次候補を一時保持する warmup cache。
    pub(super) gpu_warmup_cache: GpuWarmupCache,
    /// 差分ログ用の直近設定。
    pub(super) last_interactive_rgba_cache_config_log:
        Option<(usize, usize, usize, u32, usize, u32, u32)>,
}

/// スライドショーなど、表示の継続性に関わる状態。
pub(super) struct ViewerPlaybackState {
    /// スライドショーが有効か。
    pub(super) slideshow_active: bool,
    /// スライドショー間隔（秒）。
    pub(super) slideshow_interval_secs: f32,
    /// 次の自動送り予定時刻。None は未アーム。
    pub(super) slideshow_next_slide_at: Option<Instant>,
    /// 表示確定後に次回送りを再アームする一回限りのフラグ。
    pub(super) slideshow_arm_on_display: bool,
}

/// 再生成境界をまたぐ前提がない decode / request 状態。
pub(super) struct ViewerRequestState {
    // ── 非同期ローダー ───────────────────────────────────────────────────────
    pub(super) loader: Arc<ViewerLoader>,
    pub(super) worker_manager: ViewerWorkerManagerHandle,
    /// これ以外の結果は破棄する待機中 request_id。
    pub(super) pending_id: u64,
    /// 左右分割など複数要求を同一世代で扱う補助 request_id。
    pub(super) pending_id_aux: Option<u64>,
    /// 古い partial を混ぜないための世代番号。
    pub(super) interactive_generation: u64,
    pub(super) pending_interactive_group: Option<InteractivePendingGroup>,
    /// `show()` で display_w が確定するまで初回リクエストを遅延する。
    pub(super) initial_load_pending: bool,
    // ── キャッシュ補充 ───────────────────────────────────────────────────────
    /// 最後の連続ナビゲーション方向: +1=前進, -1=後退, 0=未定/ジャンプ。
    /// `trigger_prefetch` はこの方向の次ページを LRU キャッシュに補充する。
    pub(super) prefetch_dir: i32,
    pub(super) quality: ViewerQuality,
    pub(super) last_bg_admission_requirement: Option<DisplayRequirement>,
    /// RGBA cache 上限（MB）。interactive / BG worker manager の共通設定値。
    pub(super) rgba_cache_max_mb: u16,
    /// BG RGBA admission の判定結果。page + render_signature 単位で保持する。
    /// 容量は decode 後の実RGBAサイズで判定し、予測 byte では切らない。
    /// 本の置換では全消去が必要。現状は ViewerState 再生成でリセットされる前提。
    pub(super) bg_admission_state: HashMap<PageRenderSignatureKey, BgAdmissionState>,
    /// 現在条件に対応する interactive request 生成結果のキャッシュ。
    pub(super) interactive_request_plan_cache: Option<InteractiveRequestPlanCache>,
    /// バックグラウンド読込 worker 数。interactive request pages はここで制限しない。
    pub(super) background_worker_count: usize,
    /// interactive shard の in-flight 状態。debug cache overlay 用。
    pub(super) interactive_inflight_even_page: Option<u32>,
    pub(super) interactive_inflight_odd_page: Option<u32>,
    /// キャッシュ補充シーケンスの基準 view。
    pub(super) prefetch_anchor_view: u32,
    /// 深いキャッシュ補充を開始してよい時刻。距離 1 の近傍補充は待たない。
    pub(super) prefetch_idle_deadline: Option<Instant>,
    /// 現在フレームの補充判定に使う display target。
    pub(super) prefetch_target_display_w: u32,
    pub(super) prefetch_target_display_h: u32,
    /// 直前フレームの display target。安定判定用。
    pub(super) last_prefetch_target_display_w: Option<u32>,
    pub(super) last_prefetch_target_display_h: Option<u32>,
    /// 同一 target が連続したフレーム数。
    pub(super) display_target_stable_frames: u32,
    /// 直近の安定判定理由。
    pub(super) display_target_stability_reason: &'static str,
    /// loading 中に受けた次の表示要求。常に最新 1 件だけを保持する。
    pub(super) queued_view: Option<u32>,
    /// 現在表示中の animation stream 対象 view。
    pub(super) active_animation_stream_view: Option<u32>,
    /// 将来の逐次アニメ供給 request。
    pub(super) animation_stream_request_id: Option<u64>,
    pub(super) nav_traces: HashMap<u64, NavTrace>,
    pub(super) next_nav_id: u64,
    pub(super) active_nav_id: Option<u64>,
    pub(super) queued_nav: Option<(u32, u64, &'static str)>,
}

/// ウィンドウ再生成で主に初期化される viewport / UI runtime 状態。
pub(super) struct ViewerUiRuntimeState {
    pub loading: bool,
    pub error: Option<String>,
    pub(super) delete_range_context_menu_target_page: Option<u32>,
    /// Sequential 中の pending プレースホルダ表示遅延。チラつき抑制用。
    pub(super) pending_placeholder_after: Option<Instant>,
    /// マウスホイールの積算量。高分解能入力の端数を吸収する。
    pub(super) scroll_accum: f32,
    /// ホイール操作でページ送りした直後の短いクールダウン。
    /// 慣性入力の即再発火を防ぐ。
    pub(super) wheel_cooldown_until: Option<Instant>,
    /// 直近のナビゲーション入力モード。
    pub(super) nav_mode: NavMode,
    /// 直近のナビゲーション入力時刻。
    pub(super) last_nav_input_at: Option<Instant>,
    /// 直近入力間隔（`NAV_CONSECUTIVE_WINDOW`）ベースの連続入力カウント。
    /// 2以上なら長押し相当として扱う。
    pub(super) nav_consecutive_count: u32,
    // ── UI 計測 ─────────────────────────────────────────────────────────────
    pub(super) last_show_at: Option<Instant>,
    pub(super) show_seq: u64,
    pub(super) fullscreen_transition_frames: u8,
    pub(super) fullscreen_overlay_visible_until: Option<Instant>,
    pub(super) viewport_transition_active: bool,
    pub(super) last_stable_display_w: u32,
    pub(super) last_stable_display_h: u32,
    pub(super) transition_log_frames_left: u8,
    pub(super) first_paint_logged: bool,
    pub(super) pending_started_at: Option<Instant>,
    pub(super) pending_placeholder_latched: bool,
    pub(super) pending_visible_last: Option<bool>,
    pub(super) last_display_commit_show_seq: Option<u64>,
    pub(super) gpu_warmup_plan: GpuWarmupPlanSnapshot,
    pub(super) key_feedback_until: Option<Instant>,
    pub(super) key_feedback_text: Option<&'static str>,
    pub(super) last_pending_display_state: Option<PendingDisplayState>,
    pub(super) last_pending_visual_state: PendingVisualState,
    #[cfg(debug_assertions)]
    pub(super) debug_last_display_size_log: Option<(u32, u32, u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingDisplayState {
    pub(super) target_page: u32,
    pub(super) displayed_page: u32,
    pub(super) requested_page: u32,
    pub(super) show_pending: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PendingVisualState {
    pub(super) target_page: u32,
    pub(super) displayed_page: u32,
    pub(super) requested_page: u32,
    pub(super) show_pending: bool,
    pub(super) progress_hover: bool,
    pub(super) progress_drag: bool,
    pub(super) drag_fraction_milli: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewerDeleteRangeSelection {
    pub start: Option<u32>,
    pub end: Option<u32>,
}

impl ViewerDeleteRangeSelection {
    pub fn normalized(self) -> Self {
        match (self.start, self.end) {
            (Some(a), Some(b)) => Self {
                start: Some(a.min(b)),
                end: Some(a.max(b)),
            },
            _ => self,
        }
    }

    pub fn has_any(self) -> bool {
        self.start.is_some() || self.end.is_some()
    }

    pub fn is_complete(self) -> bool {
        self.start.is_some() && self.end.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullEquivalentSizeHintSource {
    ViewerViewport,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FullEquivalentSizeHint {
    pub monitor_size_points: egui::Vec2,
    pub source: FullEquivalentSizeHintSource,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ViewerViewLayout {
    /// レイアウト解決後の基準物理ページ。
    pub(super) physical_page: u32,
    pub(super) page_left: Option<u32>,
    pub(super) page_right: Option<u32>,
    pub(super) effective_spread: bool,
    pub(super) page_display_w: u32,
    pub(super) page_display_h: u32,
    pub(super) page_decode_w: u32,
    pub(super) page_decode_h: u32,
    pub(super) image_area_w: u32,
    pub(super) image_area_h: u32,
    pub(super) full_equivalent_area_w: u32,
    pub(super) full_equivalent_area_h: u32,
    pub(super) hint_source: &'static str,
}

fn resolved_full_equivalent_area_from_hint(
    full_equivalent_size_hint: Option<FullEquivalentSizeHint>,
    display_w: u32,
    display_h: u32,
) -> (u32, u32, &'static str) {
    full_equivalent_size_hint
        .filter(|h| h.monitor_size_points.x > 1.0 && h.monitor_size_points.y > 1.0)
        .map(|hint| {
            (
                hint.monitor_size_points.x as u32,
                hint.monitor_size_points.y as u32,
                match hint.source {
                    FullEquivalentSizeHintSource::ViewerViewport => "viewer-viewport",
                },
            )
        })
        .unwrap_or((display_w, display_h, "fallback-current-layout"))
}

fn build_auto_spread_plan_for_mode(
    page_map_mode: &ViewerPageMapMode,
    cover_blank: bool,
) -> Option<Arc<AutoSpreadPlan>> {
    match page_map_mode {
        ViewerPageMapMode::Mapped(page_map) => {
            build_auto_spread_plan(page_map.as_ref(), cover_blank).map(Arc::new)
        }
        ViewerPageMapMode::Unavailable => None,
    }
}

fn normalize_spread_setting(spread_setting: SpreadMode, auto_mode_available: bool) -> SpreadMode {
    match (spread_setting, auto_mode_available) {
        (SpreadMode::Auto, true) => SpreadMode::Auto,
        (SpreadMode::Auto, false) => SpreadMode::Single,
        (other, _) => other,
    }
}

pub struct ViewerState {
    pub(super) persistent: ViewerPersistentState,
    pub(super) display_assets: ViewerDisplayAssets,
    pub(super) playback: ViewerPlaybackState,
    pub(super) request: ViewerRequestState,
    pub(super) ui_runtime: ViewerUiRuntimeState,
    pub(super) boundary_preview: BoundaryPreviewState,
    pub(super) full_equivalent_size_hint: Option<FullEquivalentSizeHint>,
    spad: ViewerSpadState,
    delete_range_selection: ViewerDeleteRangeSelection,
    reading_session: ReadingSessionState,
}

enum GpuTextureDisplayLookup {
    Full {
        left: Option<GpuTextureHit>,
        right: Option<GpuTextureHit>,
    },
    Partial {
        left: Option<GpuTextureHit>,
        right: Option<GpuTextureHit>,
    },
    Miss,
}

impl ViewerState {
    fn compute_transition_flag(&self) -> bool {
        self.ui_runtime.viewport_transition_active
            || self.ui_runtime.fullscreen_transition_frames > 0
    }

    fn boundary_preview_loading(direction: BoundaryPreviewDirection) -> BoundaryPreviewState {
        BoundaryPreviewState::Loading(BoundaryPreviewProbe {
            direction,
            in_flight: false,
            request_id: None,
        })
    }

    pub fn entry(&self) -> &BookMeta {
        &self.persistent.entry
    }

    pub fn current_requested_page(&self) -> u32 {
        self.persistent.requested_page
    }

    pub fn current_displayed_page_min_max(&self) -> Option<(u32, u32)> {
        let (left, right) = self.current_view_pages(self.persistent.displayed_page);
        match (left, right) {
            (Some(left), Some(right)) => Some((left.min(right), left.max(right))),
            (Some(page), None) | (None, Some(page)) => Some((page, page)),
            (None, None) => None,
        }
    }

    pub fn delete_range_selection(&self) -> ViewerDeleteRangeSelection {
        self.delete_range_selection.normalized()
    }

    pub fn set_delete_range_context_menu_target_page(&mut self, page: Option<u32>) {
        self.ui_runtime.delete_range_context_menu_target_page = page;
    }

    pub fn delete_range_context_menu_target_page(&self) -> Option<u32> {
        self.ui_runtime.delete_range_context_menu_target_page
    }

    pub fn delete_range_clear(&mut self) {
        self.delete_range_selection = ViewerDeleteRangeSelection::default();
    }

    pub fn delete_range_set_start(&mut self, page: u32) {
        self.delete_range_selection = ViewerDeleteRangeSelection {
            start: Some(page),
            end: None,
        };
    }

    pub fn delete_range_set_end(&mut self, page: u32) {
        let start = self.delete_range_selection.start.unwrap_or(page);
        self.delete_range_selection = ViewerDeleteRangeSelection {
            start: Some(start.min(page)),
            end: Some(start.max(page)),
        };
    }

    pub fn delete_range_restart_from_current(&mut self, page: u32) {
        self.delete_range_set_start(page);
    }

    pub fn delete_range_mark_current(&mut self, page: u32) {
        match self.delete_range_selection() {
            ViewerDeleteRangeSelection {
                start: None,
                end: None,
            } => self.delete_range_set_start(page),
            ViewerDeleteRangeSelection {
                start: Some(_),
                end: None,
            } => self.delete_range_set_end(page),
            ViewerDeleteRangeSelection {
                start: Some(_),
                end: Some(_),
            } => self.delete_range_restart_from_current(page),
            ViewerDeleteRangeSelection {
                start: None,
                end: Some(_),
            } => self.delete_range_set_start(page),
        }
    }

    pub fn delete_range_entry_names(&self) -> Option<Vec<String>> {
        let selection = self.delete_range_selection();
        let (Some(start), Some(end)) = (selection.start, selection.end) else {
            return None;
        };
        let mut out = Vec::new();
        for page in start..=end {
            let entry_name = self.persistent.page_entry_names.get(page as usize)?;
            out.push(entry_name.clone());
        }
        Some(out)
    }

    pub fn delete_range_would_remove_all_pages(&self) -> bool {
        let selection = self.delete_range_selection();
        let (Some(start), Some(end)) = (selection.start, selection.end) else {
            return false;
        };
        let selected_page_count = end.saturating_sub(start).saturating_add(1) as usize;
        let total_image_pages = if self.persistent.page_entry_names.is_empty() {
            self.persistent.page_count as usize
        } else {
            self.persistent.page_entry_names.len()
        };
        total_image_pages > 0 && selected_page_count >= total_image_pages
    }

    pub(super) fn current_toolbar_title(&self, blank_label: &str) -> Option<String> {
        let current_page = self.persistent.displayed_page;
        let (page_left, page_right) = self.current_view_pages(current_page);
        if page_left.is_none() && page_right.is_none() {
            return None;
        }

        let has_two_visible_pages = page_left.is_some() && page_right.is_some();
        let use_spread_title = self.persistent.spread_mode
            || has_two_visible_pages
            || self.is_leading_cover_blank_spread(current_page);
        if !use_spread_title {
            return self.page_display_label(page_left.or(page_right));
        }

        let reading_direction = self.effective_reading_direction();
        let leading_cover_blank_spread = self.is_leading_cover_blank_spread(current_page);
        let (screen_left, screen_right) = match reading_direction {
            ReadingDirection::RightToLeft => (page_right, page_left),
            ReadingDirection::LeftToRight => (page_left, page_right),
        };
        let left_is_blank_slot = leading_cover_blank_spread
            && matches!(reading_direction, ReadingDirection::RightToLeft);
        let right_is_blank_slot = leading_cover_blank_spread
            && matches!(reading_direction, ReadingDirection::LeftToRight);
        let left_label =
            self.toolbar_spread_slot_label(screen_left, left_is_blank_slot, blank_label)?;
        let right_label =
            self.toolbar_spread_slot_label(screen_right, right_is_blank_slot, blank_label)?;
        Some(format!("{left_label} / {right_label}"))
    }

    pub(super) fn is_leading_cover_blank_spread(&self, physical_page: u32) -> bool {
        self.persistent.cover_blank
            && physical_page == 0
            && !matches!(self.persistent.spread_setting, SpreadMode::Single)
            && self.persistent.page_count > 0
    }

    pub(crate) fn take_reading_session_snapshot(&mut self) -> Option<ReadingSessionSnapshot> {
        self.reading_session.begin_notification()
    }

    pub(crate) fn complete_reading_session_notification(&mut self) {
        self.reading_session.complete_notification();
    }

    pub(crate) fn discard_reading_session_notification(&mut self) {
        self.reading_session.discard_notification();
    }

    #[cfg(debug_assertions)]
    pub(super) fn gpu_texture_history_snapshot(&self) -> GpuTextureHistorySnapshot {
        self.display_assets.gpu_texture_history.snapshot()
    }

    #[cfg(debug_assertions)]
    pub(super) fn gpu_warmup_cache_snapshot(&self) -> GpuWarmupCacheSnapshot {
        self.display_assets.gpu_warmup_cache.snapshot()
    }

    #[cfg(debug_assertions)]
    pub(super) fn gpu_warmup_plan_snapshot(&self) -> GpuWarmupPlanSnapshot {
        self.ui_runtime.gpu_warmup_plan
    }

    pub fn clear_gpu_texture_history(&mut self, reason: &'static str) {
        self.display_assets.gpu_texture_history.clear(reason);
        self.display_assets.gpu_warmup_cache.clear(reason);
    }

pub fn configure_spad_targets(
        &mut self,
        prev: Option<AdjacentBook>,
        next: Option<AdjacentBook>,
        prev_layout_settings: Option<SpadTargetLayoutSettings>,
        next_layout_settings: Option<SpadTargetLayoutSettings>,
    ) {
        self.cancel_spad("target_reset");
        self.spad.generation = self.spad.generation.saturating_add(1);
        self.spad.next_inflight = None;
        self.spad.prev_inflight = None;
        self.spad.no_dispatch_logged = false;
        self.spad.prev = prev.map(|book| self.build_spad_target(book, prev_layout_settings));
        self.spad.next = next.map(|book| self.build_spad_target(book, next_layout_settings));
        spad_trace_debug!(
            "[spad-target] session={} generation={} prev={} next={}",
            self.spad.session,
            self.spad.generation,
            self.spad
                .prev
                .as_ref()
                .map(|target| format!("{}@{}", target.path.display(), target.entry_page))
                .unwrap_or_else(|| "-".to_owned()),
            self.spad
                .next
                .as_ref()
                .map(|target| format!("{}@{}", target.path.display(), target.entry_page))
                .unwrap_or_else(|| "-".to_owned())
        );
    }

    pub fn cancel_spad(&mut self, reason: &'static str) {
        self.spad.no_dispatch_logged = false;
        let mut cancelled_any = false;
        for inflight in [
            self.spad.next_inflight.take(),
            self.spad.prev_inflight.take(),
        ]
        .into_iter()
        .flatten()
        {
            cancelled_any = true;
            spad_trace_debug!(
                "[spad-cancel] session={} generation={} side={} target={} request_id={} reason={}",
                inflight.session,
                inflight.generation,
                inflight.side.as_str(),
                inflight.path.display(),
                inflight.request_id,
                reason
            );
        }
        if !cancelled_any && (self.spad.prev.is_some() || self.spad.next.is_some())
        {
            spad_trace_debug!(
                "[spad-cancel] session={} generation={} side=none target=- request_id=0 reason={}",
                self.spad.session,
                self.spad.generation,
                reason
            );
        }
    }

    fn record_reading_session_display_commit(
        &mut self,
        left_page: Option<u32>,
        right_page: Option<u32>,
    ) {
        self.reading_session.record_display_commit(
            left_page,
            right_page,
            self.persistent.page_count as usize,
        );
    }

    /// 本の切り替え前に、未受信結果の Arc 参照を残さない。
    pub fn flush_worker(&mut self) {
        self.cancel_spad("flush");
        self.request.loader.flush();
        self.request.worker_manager.flush_notifications();
    }

    pub fn set_full_equivalent_size_hint(&mut self, hint: Option<FullEquivalentSizeHint>) {
        if self.full_equivalent_size_hint == hint {
            return;
        }
        self.full_equivalent_size_hint = hint;
        match hint {
            Some(h) => {
                tracing::debug!(
                    "[viewer-full-size-hint] source={} monitor_w={} monitor_h={}",
                    match h.source {
                        FullEquivalentSizeHintSource::ViewerViewport => "viewer-viewport",
                    },
                    h.monitor_size_points.x,
                    h.monitor_size_points.y
                );
            }
            None => {
                tracing::debug!("[viewer-full-size-hint] source=none monitor_w=0 monitor_h=0");
            }
        }
    }

    pub(super) fn update_full_equivalent_size_hint_from_viewer(
        &mut self,
        monitor_size: Option<egui::Vec2>,
    ) {
        let Some(size) = monitor_size.filter(|s| s.x > 1.0 && s.y > 1.0) else {
            return;
        };
        self.set_full_equivalent_size_hint(Some(FullEquivalentSizeHint {
            monitor_size_points: size,
            source: FullEquivalentSizeHintSource::ViewerViewport,
        }));
    }

    /// ローダー生成と初回 request の分離を保ち、`show()` 以前の未確定幅で走らせない。
    pub fn new(ctx: egui::Context, init: ViewerStateInit) -> Result<Self, String> {
        let ViewerStateInit {
            entry,
            start_page,
            spad_session_id,
            cover_blank,
            quality_override,
            global_reading_direction,
            reading_direction_override,
            spread_setting,
            performance_settings,
            quality,
            slideshow_interval_secs,
            full_equivalent_size_hint,
            page_map_mode,
        } = init;
        let background_worker_count = performance_settings.background_worker_count.max(1);
        let (l1_future_mib, l1_history_mib) =
            split_mib_evenly(performance_settings.l1_vram_cache_max_mib);
        let auto_spread_plan = build_auto_spread_plan_for_mode(&page_map_mode, cover_blank);
        let auto_mode_available = auto_spread_plan.is_some();
        let spread_setting = normalize_spread_setting(spread_setting, auto_mode_available);
        let initial_page = start_page;
        let initial_spread_mode = match spread_setting {
            SpreadMode::Auto => auto_spread_plan
                .as_ref()
                .and_then(|plan| {
                    plan.pages_for_logical_page(initial_page)
                        .map(|(_, second)| second.is_some())
                })
                .unwrap_or(false),
            SpreadMode::Single => false,
            SpreadMode::Spread => true,
        };
        let repaint_ctx = ctx.clone();
        let loader = Arc::new(
            ViewerLoader::spawn(ctx, background_worker_count)
                .map_err(|e| format!("viewer loader init failed: {e}"))?,
        );
        let worker_manager = ViewerWorkerManagerHandle::spawn(Arc::clone(&loader), repaint_ctx);
        let entry_id_for_snapshot = entry.id.clone();
        let page_display_labels =
            viewer_page_display_labels(entry.path.as_ref()).unwrap_or_default();
        let page_entry_names = viewer_page_entry_names(entry.path.as_ref()).unwrap_or_default();
        let spread_setting_for_snapshot = spread_setting.clone();

        Ok(Self {
            persistent: ViewerPersistentState {
                entry,
                page_display_labels,
                page_entry_names,
                page_map_mode,
                auto_spread_plan,
                page_count: 0,
                displayed_page: initial_page,
                requested_page: initial_page,
                target_page: initial_page,
                spread_setting,
                spread_mode: initial_spread_mode,
                global_reading_direction,
                reading_direction_override,
                cover_blank,
                quality_override,
                spread_snapshot: SpreadSnapshot {
                    key: SpreadSnapshotKey {
                        entry_id: entry_id_for_snapshot,
                        page_count: 0,
                        spread_setting: spread_setting_for_snapshot,
                        cover_blank,
                        physical_page: initial_page,
                    },
                    effective_spread: initial_spread_mode,
                    page_right: None,
                    valid: false,
                },
            },
            display_assets: ViewerDisplayAssets {
                content_left: None,
                content_right: None,
                interactive_rgba_cache: RgbaPageCache::new(),
                gpu_texture_history: GpuTextureHistory::new(mib_to_bytes(l1_history_mib)),
                gpu_warmup_cache: GpuWarmupCache::new(mib_to_bytes(l1_future_mib)),
                last_interactive_rgba_cache_config_log: None,
            },
            playback: ViewerPlaybackState {
                slideshow_active: false,
                slideshow_interval_secs: clamp_slideshow_interval_secs(slideshow_interval_secs),
                slideshow_next_slide_at: None,
                slideshow_arm_on_display: false,
            },
            request: ViewerRequestState {
                loader,
                worker_manager,
                pending_id: 0,
                pending_id_aux: None,
                interactive_generation: 0,
                pending_interactive_group: None,
                initial_load_pending: true,
                prefetch_dir: 0,
                quality,
                last_bg_admission_requirement: None,
                rgba_cache_max_mb: performance_settings.l2_ram_cache_max_mib,
                bg_admission_state: HashMap::new(),
                interactive_request_plan_cache: None,
                background_worker_count,
                interactive_inflight_even_page: None,
                interactive_inflight_odd_page: None,
                prefetch_anchor_view: 0,
                prefetch_idle_deadline: None,
                prefetch_target_display_w: 0,
                prefetch_target_display_h: 0,
                last_prefetch_target_display_w: None,
                last_prefetch_target_display_h: None,
                display_target_stable_frames: 0,
                display_target_stability_reason: "no-previous-target",
                queued_view: None,
                active_animation_stream_view: None,
                animation_stream_request_id: None,
                nav_traces: HashMap::new(),
                next_nav_id: 1,
                active_nav_id: None,
                queued_nav: None,
            },
            ui_runtime: ViewerUiRuntimeState {
                loading: true,
                error: None,
                delete_range_context_menu_target_page: None,
                pending_placeholder_after: None,
                scroll_accum: 0.0,
                wheel_cooldown_until: None,
                nav_mode: NavMode::Sequential,
                last_nav_input_at: None,
                nav_consecutive_count: 0,
                last_show_at: None,
                show_seq: 0,
                fullscreen_transition_frames: 0,
                fullscreen_overlay_visible_until: None,
                viewport_transition_active: false,
                last_stable_display_w: 0,
                last_stable_display_h: 0,
                transition_log_frames_left: TRANSITION_LOG_FRAMES,
                first_paint_logged: false,
                pending_started_at: None,
                pending_placeholder_latched: false,
                pending_visible_last: None,
                last_display_commit_show_seq: None,
                gpu_warmup_plan: GpuWarmupPlanSnapshot::default(),
                key_feedback_until: None,
                key_feedback_text: None,
                last_pending_display_state: None,
                last_pending_visual_state: PendingVisualState::default(),
                #[cfg(debug_assertions)]
                debug_last_display_size_log: None,
            },
            boundary_preview: BoundaryPreviewState::Hidden,
            full_equivalent_size_hint,
            spad: ViewerSpadState {
                session: spad_session_id,
                generation: 0,
                next_inflight: None,
                prev_inflight: None,
                prev: None,
                next: None,
                no_dispatch_logged: false,
            },
            delete_range_selection: ViewerDeleteRangeSelection::default(),
            reading_session: ReadingSessionState::new(),
        })
    }

    pub(super) fn update_display_target_stability(&mut self, target_w: u32, target_h: u32) {
        self.request.prefetch_target_display_w = target_w;
        self.request.prefetch_target_display_h = target_h;
        let Some(prev_w) = self.request.last_prefetch_target_display_w else {
            self.request.last_prefetch_target_display_w = Some(target_w);
            self.request.last_prefetch_target_display_h = Some(target_h);
            self.request.display_target_stable_frames = 0;
            self.request.display_target_stability_reason = "no-previous-target";
            return;
        };
        let prev_h = self
            .request
            .last_prefetch_target_display_h
            .unwrap_or(target_h);
        let diff_w = target_w.abs_diff(prev_w);
        let diff_h = target_h.abs_diff(prev_h);
        let changed_px = diff_w > 16 || diff_h > 16;
        let changed_ratio = if prev_w == 0 || prev_h == 0 {
            true
        } else {
            target_w.max(prev_w) * DISPLAY_TARGET_STABILITY_DENOMINATOR
                > target_w.min(prev_w) * DISPLAY_TARGET_STABILITY_NUMERATOR
                || target_h.max(prev_h) * DISPLAY_TARGET_STABILITY_DENOMINATOR
                    > target_h.min(prev_h) * DISPLAY_TARGET_STABILITY_NUMERATOR
        };
        if changed_px || changed_ratio {
            self.request.display_target_stable_frames = 0;
            self.request.display_target_stability_reason = "target-size-changed";
        } else {
            self.request.display_target_stable_frames =
                self.request.display_target_stable_frames.saturating_add(1);
            self.request.display_target_stability_reason = "waiting-stable-frames";
        }
        self.request.last_prefetch_target_display_w = Some(target_w);
        self.request.last_prefetch_target_display_h = Some(target_h);
    }

    pub(super) fn begin_nav(&mut self, from_view: u32, to_view: u32, reason: &'static str) -> u64 {
        let nav_id = self.request.next_nav_id;
        self.request.next_nav_id = self.request.next_nav_id.saturating_add(1);
        self.request.active_nav_id = Some(nav_id);
        self.request.nav_traces.insert(
            nav_id,
            NavTrace {
                started_at: Instant::now(),
                request_left_hit: RequestHitState::None,
                request_right_hit: RequestHitState::None,
            },
        );
        let req = self.request.loader.peek_next_request_id();
        tracing::trace!(
            "[viewer-nav-start] nav_id={} req={} from_view={} to_view={} reason={} bg_rgba_entries={}",
            nav_id,
            req,
            from_view,
            to_view,
            reason,
            self.request
                .worker_manager
                .bg_rgba_cache()
                .read()
                .ok()
                .map(|cache| cache.entry_count())
                .unwrap_or(0)
        );
        nav_id
    }

    pub(super) fn set_request_hit(
        &mut self,
        nav_id: u64,
        request_left_hit: RequestHitState,
        request_right_hit: RequestHitState,
    ) {
        if let Some(trace) = self.request.nav_traces.get_mut(&nav_id) {
            trace.request_left_hit = request_left_hit;
            trace.request_right_hit = request_right_hit;
        }
    }

    pub(super) fn transition_logs_active(&self) -> bool {
        LOG_VIEWPORT_TRANSITION && self.ui_runtime.transition_log_frames_left > 0
    }

    pub fn mark_key_feedback(&mut self, text: &'static str, now: Instant) {
        self.ui_runtime.key_feedback_text = Some(text);
        self.ui_runtime.key_feedback_until = Some(now + KEY_FEEDBACK_DURATION);
    }

    pub(super) fn key_feedback_text(&mut self, now: Instant) -> Option<&'static str> {
        let until = self.ui_runtime.key_feedback_until?;
        if now <= until {
            self.ui_runtime.key_feedback_text
        } else {
            self.ui_runtime.key_feedback_until = None;
            self.ui_runtime.key_feedback_text = None;
            None
        }
    }

    pub fn slideshow_interval_secs(&self) -> f32 {
        self.playback.slideshow_interval_secs
    }

    pub fn slideshow_active(&self) -> bool {
        self.playback.slideshow_active
    }

    pub fn stop_slideshow(&mut self) {
        self.playback.slideshow_active = false;
        self.playback.slideshow_next_slide_at = None;
        self.playback.slideshow_arm_on_display = false;
    }

    pub(super) fn arm_slideshow_from_now(&mut self, now: Instant) {
        self.playback.slideshow_next_slide_at =
            Some(now + Duration::from_secs_f32(self.playback.slideshow_interval_secs));
        self.playback.slideshow_arm_on_display = false;
    }

    pub(super) fn start_slideshow(&mut self, now: Instant) {
        self.boundary_preview_clear();
        self.playback.slideshow_active = true;
        if self.ui_runtime.loading
            || (self.display_assets.content_left.is_none()
                && self.display_assets.content_right.is_none())
        {
            self.playback.slideshow_next_slide_at = None;
            self.playback.slideshow_arm_on_display = true;
        } else {
            self.arm_slideshow_from_now(now);
        }
    }

    pub(super) fn toggle_slideshow(&mut self, now: Instant) {
        if self.playback.slideshow_active {
            self.stop_slideshow();
        } else {
            self.start_slideshow(now);
        }
    }

    pub(super) fn mark_slideshow_wait_display(&mut self) {
        if self.playback.slideshow_active {
            self.playback.slideshow_next_slide_at = None;
            self.playback.slideshow_arm_on_display = true;
        }
    }

    pub(super) fn set_slideshow_interval_secs(&mut self, secs: f32, now: Instant) {
        self.playback.slideshow_interval_secs = clamp_slideshow_interval_secs(secs);
        if self.playback.slideshow_active {
            self.arm_slideshow_from_now(now);
        }
    }

    pub fn set_quality(&mut self, quality: ViewerQuality) {
        if self.request.quality == quality {
            return;
        }
        self.request.quality = quality;
        self.clear_gpu_texture_history("quality_changed");
        self.display_assets.interactive_rgba_cache = RgbaPageCache::new();
        self.clear_interactive_in_flight();
        self.clear_bg_admission_state("quality-change");
        self.request.last_bg_admission_requirement = None;
        self.request.prefetch_idle_deadline = None;
    }

    pub fn current_quality(&self) -> ViewerQuality {
        self.request.quality
    }

    pub fn effective_reading_direction(&self) -> ReadingDirection {
        self.persistent
            .reading_direction_override
            .unwrap_or(self.persistent.global_reading_direction)
    }

    fn static_texture_bytes(texture: &egui::TextureHandle) -> usize {
        let [width, height] = texture.size();
        width.saturating_mul(height).saturating_mul(4)
    }

    fn format_bytes_mb(bytes: usize) -> String {
        const MB: u128 = 1024 * 1024;
        let mb_x10 = ((bytes as u128) * 10 + MB / 2) / MB;
        let whole = mb_x10 / 10;
        let frac = mb_x10 % 10;
        if frac == 0 {
            whole.to_string()
        } else {
            format!("{whole}.{frac}")
        }
    }

    fn static_texture_payload(
        content: Option<&PageContent>,
    ) -> Option<(egui::TextureHandle, usize)> {
        let PageContent::Static(texture) = content? else {
            return None;
        };
        Some((texture.clone(), Self::static_texture_bytes(texture)))
    }

    fn record_committed_gpu_texture_hit(
        &mut self,
        physical_page: u32,
        left_page: Option<u32>,
        right_page: Option<u32>,
        side: &'static str,
        hit: &GpuTextureHit,
    ) {
        let view_idx = physical_page;
        let render_signature = hit.key.render_signature;
        match hit.source {
            GpuTextureHitSource::History => {
                self.display_assets.gpu_texture_history.record_hit();
                tracing::trace!(
                    view_page = view_idx,
                    left_page = ?left_page,
                    right_page = ?right_page,
                    side,
                    page = hit.key.page,
                    source = hit.source.as_str(),
                    hit_kind = hit.hit_kind.as_str(),
                    target_w = render_signature.target_w,
                    target_h = render_signature.target_h,
                    max_tex_side = render_signature.max_tex_side,
                    texture_width = hit.texture_width,
                    texture_height = hit.texture_height,
                    estimated_mb = %Self::format_bytes_mb(hit.estimated_bytes),
                    current_mb = %Self::format_bytes_mb(
                        self.display_assets.gpu_texture_history.current_bytes()
                    ),
                    max_mb = %Self::format_bytes_mb(self.display_assets.gpu_texture_history.max_bytes()),
                    entries = self.display_assets.gpu_texture_history.entry_count(),
                    reason = "display_commit",
                    "gpu-history-hit"
                );
            }
            GpuTextureHitSource::Warmup => {
                self.display_assets.gpu_warmup_cache.record_hit();
                tracing::trace!(
                    view_page = view_idx,
                    left_page = ?left_page,
                    right_page = ?right_page,
                    side,
                    page = hit.key.page,
                    source = hit.source.as_str(),
                    hit_kind = hit.hit_kind.as_str(),
                    target_w = render_signature.target_w,
                    target_h = render_signature.target_h,
                    max_tex_side = render_signature.max_tex_side,
                    texture_width = hit.texture_width,
                    texture_height = hit.texture_height,
                    estimated_mb = %Self::format_bytes_mb(hit.estimated_bytes),
                    current_mb = %Self::format_bytes_mb(
                        self.display_assets.gpu_warmup_cache.current_bytes()
                    ),
                    max_mb = %Self::format_bytes_mb(self.display_assets.gpu_warmup_cache.max_bytes()),
                    entries = self.display_assets.gpu_warmup_cache.entry_count(),
                    reason = "display_commit",
                    "gpu-warmup-hit"
                );
            }
        }
    }

    fn register_gpu_texture_history(
        &mut self,
        page: Option<u32>,
        texture: Option<(egui::TextureHandle, usize)>,
        render_signature: RenderSignature,
    ) {
        let Some(page) = page else {
            return;
        };
        let Some((texture, estimated_bytes)) = texture else {
            return;
        };
        let key = PageRenderSignatureKey {
            page,
            render_signature,
        };
        let _ = self
            .display_assets
            .gpu_warmup_cache
            .remove_without_promotion(&key);
        let _ = self
            .display_assets
            .gpu_texture_history
            .insert(key, texture, estimated_bytes);
    }

    fn gpu_texture_hit_score(hit: &GpuTextureHit, requirement: DisplayRequirement) -> (u64, u8) {
        let score = hit
            .key
            .render_signature
            .target_w
            .abs_diff(requirement.required_w) as u64
            + hit
                .key
                .render_signature
                .target_h
                .abs_diff(requirement.required_h) as u64;
        let source_rank = match hit.source {
            GpuTextureHitSource::History => 0,
            GpuTextureHitSource::Warmup => 1,
        };
        (score, source_rank)
    }

    fn best_gpu_texture_hit_for_page(
        &self,
        page: u32,
        requirement: DisplayRequirement,
    ) -> Option<GpuTextureHit> {
        let history_peek = self
            .display_assets
            .gpu_texture_history
            .peek_suitable(page, requirement);
        let warmup_peek = self
            .display_assets
            .gpu_warmup_cache
            .peek_suitable(page, requirement);
        let chosen_source = match (&history_peek, &warmup_peek) {
            (Some(history), Some(warmup)) => {
                let history_rank = Self::gpu_texture_hit_score(history, requirement);
                let warmup_rank = Self::gpu_texture_hit_score(warmup, requirement);
                if history_rank <= warmup_rank {
                    GpuTextureHitSource::History
                } else {
                    GpuTextureHitSource::Warmup
                }
            }
            (Some(_), None) => GpuTextureHitSource::History,
            (None, Some(_)) => GpuTextureHitSource::Warmup,
            (None, None) => return None,
        };
        match chosen_source {
            GpuTextureHitSource::History => history_peek,
            GpuTextureHitSource::Warmup => warmup_peek,
        }
    }

    /// 表示経路と同じ解決を使い、warmup と描画の要求幅をずらさない。
    fn warmup_requirement_for_page(
        &mut self,
        page: u32,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> DisplayRequirement {
        let layout = self.view_layout_for_with_caller(page, display_w, display_h, false);
        self.display_requirement_for_request(
            layout.page_decode_w,
            layout.page_decode_h,
            max_tex_side,
        )
    }

    /// Desired Future から L1 保持要件を作り、L2 Ready から upload 候補を構築する。
    fn collect_gpu_warmup_inputs(
        &mut self,
        visible_end: u32,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> GpuWarmupInputs {
        let bg_rgba_cache = self.request.worker_manager.bg_rgba_cache();
        let ready_entries = bg_rgba_cache
            .read()
            .ok()
            .map(|cache| cache.ready_entry_snapshots())
            .unwrap_or_default();
        let warm_entries = self.display_assets.gpu_warmup_cache.entry_snapshots();
        let history_entries = self
            .display_assets
            .gpu_texture_history
            .entry_snapshots()
            .into_iter()
            .collect::<Vec<_>>();
        let mut desired_future_requirements = HashMap::new();

        let desired_sequence = {
            let current_page = self
                .persistent
                .displayed_page
                .min(self.persistent.page_count.saturating_sub(1));
            let (visible_left, visible_right) = self.current_view_pages(current_page);
            let mut visible_pages = Vec::new();
            for page in [visible_left, visible_right].into_iter().flatten() {
                if !visible_pages.contains(&page) {
                    visible_pages.push(page);
                }
            }
            SimpleStreamingCachePolicy::new(current_page, self.persistent.page_count, visible_pages)
                .desired_sequence()
        };

        let mut upload_candidates = Vec::new();
        for page in desired_sequence
            .into_iter()
            .filter(|page| *page > visible_end)
        {
            let requirement =
                self.warmup_requirement_for_page(page, display_w, display_h, max_tex_side);
            desired_future_requirements.insert(page, requirement);
            let Some(candidate) = resolve_future_candidate(
                page,
                page.saturating_sub(visible_end),
                requirement,
                &ready_entries,
            ) else {
                let page_exists = ready_entries.iter().any(|entry| entry.page == page);
                tracing::trace!(
                    displayed_page = self.persistent.displayed_page,
                    page,
                    page_exists,
                    target_w = requirement.required_w,
                    target_h = requirement.required_h,
                    quality = ?requirement.quality,
                    max_tex_side = requirement.max_tex_side,
                    reason = "bg-rgba-not-ready",
                    "gpu-warmup-bg-rgba-miss"
                );
                continue;
            };
            upload_candidates.push(candidate);
        }

        (
            upload_candidates,
            ready_entries,
            warm_entries,
            history_entries,
            desired_future_requirements,
        )
    }

    fn apply_gpu_warmup_evictions(
        &mut self,
        visible_end: u32,
        evictions: &[super::gpu_warmup_planner::GpuWarmupEvictCandidate],
        event_name: &'static str,
        phase: &'static str,
    ) {
        for evict in evictions {
            let _ = self
                .display_assets
                .gpu_warmup_cache
                .remove_without_promotion(&evict.key);
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                visible_end,
                page = evict.page,
                bytes = evict.bytes,
                phase,
                reason = evict.reason,
                current_mb = %Self::format_bytes_mb(
                    self.display_assets.gpu_warmup_cache.current_bytes()
                ),
                max_mb = %Self::format_bytes_mb(self.display_assets.gpu_warmup_cache.max_bytes()),
                entries = self.display_assets.gpu_warmup_cache.entry_count(),
                "{event_name}"
            );
        }
    }

    /// L1 の upload は UI スレッドで進め、1 回の呼出しで 1 page だけ処理する。
    pub(super) fn maybe_run_gpu_warmup(
        &mut self,
        ctx: &egui::Context,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> bool {
        tracing::trace!(
            displayed_page = self.persistent.displayed_page,
            show_seq = self.ui_runtime.show_seq,
            last_display_commit_show_seq = ?self.ui_runtime.last_display_commit_show_seq,
            loading = self.ui_runtime.loading,
            pending_interactive_group = self.request.pending_interactive_group.is_some(),
            animation_stream_request_id = ?self.request.animation_stream_request_id,
            active_animation_stream_view = ?self.request.active_animation_stream_view,
            viewport_transition = self.ui_runtime.viewport_transition_active,
            fullscreen_transition = self.ui_runtime.fullscreen_transition_frames > 0,
            "gpu-warmup-check"
        );
        if self.ui_runtime.loading
            || self.request.pending_interactive_group.is_some()
            || self.request.animation_stream_request_id.is_some()
            || self.request.active_animation_stream_view.is_some()
            || self.ui_runtime.viewport_transition_active
            || self.ui_runtime.fullscreen_transition_frames > 0
        {
            let reason = if self.ui_runtime.loading {
                "loading"
            } else if self.request.pending_interactive_group.is_some() {
                "pending-interactive-group"
            } else if self.request.animation_stream_request_id.is_some() {
                "animation-request"
            } else if self.request.active_animation_stream_view.is_some() {
                "animation-active"
            } else if self.ui_runtime.viewport_transition_active {
                "viewport-transition"
            } else {
                "fullscreen-transition"
            };
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                show_seq = self.ui_runtime.show_seq,
                reason,
                "gpu-warmup-skip"
            );
            return false;
        }
        if self.persistent.page_count == 0 {
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                show_seq = self.ui_runtime.show_seq,
                reason = "no-next-view",
                "gpu-warmup-skip"
            );
            return false;
        }
        let (visible_left, visible_right) = self.current_view_pages(self.persistent.displayed_page);
        let Some(visible_end) = [visible_left, visible_right].into_iter().flatten().max() else {
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                show_seq = self.ui_runtime.show_seq,
                reason = "no-next-view",
                "gpu-warmup-skip"
            );
            self.ui_runtime.gpu_warmup_plan = GpuWarmupPlanSnapshot::default();
            return false;
        };

        let (
            future_candidates,
            ready_entries,
            warm_entries,
            history_entries,
            desired_future_requirements,
        ) = self.collect_gpu_warmup_inputs(visible_end, display_w, display_h, max_tex_side);
        let plan = plan_gpu_warmup(
            visible_end,
            ready_entries.len(),
            self.display_assets.gpu_warmup_cache.max_bytes(),
            &future_candidates,
            &warm_entries,
            &history_entries,
            &desired_future_requirements,
        );
        let plan_summary = plan.summary();
        self.ui_runtime.gpu_warmup_plan = plan_summary;
        tracing::trace!(
            displayed_page = self.persistent.displayed_page,
            visible_end,
            l2_ready_count = plan_summary.l2_ready_count,
            future_candidate_count = plan_summary.future_candidate_count,
            warm_count = plan_summary.warm_count,
            warm_mb = %Self::format_bytes_mb(plan_summary.warm_bytes),
            free_mb = %Self::format_bytes_mb(plan_summary.free_bytes),
            best_missing_page = ?plan_summary.best_missing_page,
            worst_warm_page = ?plan_summary.worst_warm_page,
            replacement_needed = plan_summary.replacement_needed,
            replacement_count = plan_summary.replacement_count,
            stale_evict_count = plan_summary.stale_evict_count,
            upload_page = ?plan_summary.upload_page,
            upload_mode = ?plan_summary.upload_mode,
            upload_count = plan_summary.upload_count,
            evict_count = plan_summary.evict_count,
            reason = plan_summary.idle_reason.unwrap_or("active"),
            "gpu-warmup-plan"
        );
        if let Some(reason) = plan_summary.idle_reason {
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                visible_end,
                reason,
                "gpu-warmup-plan-idle"
            );
            return false;
        }

        self.apply_gpu_warmup_evictions(
            visible_end,
            &plan.stale_evict_candidates,
            "gpu-warmup-plan-evict",
            "stale",
        );

        if self.ui_runtime.last_display_commit_show_seq == Some(self.ui_runtime.show_seq) {
            if plan_summary.pending_uploads > 0 {
                ctx.request_repaint();
            }
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                show_seq = self.ui_runtime.show_seq,
                visible_end,
                reason = "same-commit-frame",
                "gpu-warmup-skip"
            );
            return false;
        }

        let Some(upload) = plan.upload_candidate.as_ref() else {
            return false;
        };
        tracing::trace!(
            displayed_page = self.persistent.displayed_page,
            visible_end,
            page = upload.page,
            rgba_page = upload.rgba_key.page,
            target_w = upload.rgba_key.render_signature.target_w,
            target_h = upload.rgba_key.render_signature.target_h,
            quality = ?upload.rgba_key.render_signature.quality,
            max_tex_side = upload.rgba_key.render_signature.max_tex_side,
            mode = plan_summary.upload_mode.unwrap_or("free-space"),
            reason = "planned_upload",
            "gpu-warmup-plan-upload"
        );
        let bg_rgba_cache = self.request.worker_manager.bg_rgba_cache();
        let Some(bg_cache) = bg_rgba_cache.read().ok() else {
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                visible_end,
                page = upload.page,
                reason = "busy",
                "gpu-warmup-plan-idle"
            );
            return false;
        };
        let page_exists = bg_cache.contains_page(upload.page);
        let Some((frames, cached_signature, source)) = bg_cache.peek_exact(&upload.rgba_key) else {
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                visible_end,
                page = upload.page,
                page_exists,
                rgba_page = upload.rgba_key.page,
                target_w = upload.rgba_key.render_signature.target_w,
                target_h = upload.rgba_key.render_signature.target_h,
                quality = ?upload.rgba_key.render_signature.quality,
                max_tex_side = upload.rgba_key.render_signature.max_tex_side,
                reason = "planner-exact-missing",
                "gpu-warmup-bg-rgba-miss"
            );
            return false;
        };
        debug_assert_eq!(cached_signature, upload.key.render_signature);
        if frames.len() != 1 {
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                visible_end,
                page = upload.page,
                frame_count = frames.len(),
                reason = "frame-count",
                "gpu-warmup-candidate-skip"
            );
            return false;
        }

        let content = PageContent::from_frames(frames, "viewer_warmup", ctx);
        let PageContent::Static(texture) = content else {
            tracing::trace!(
                displayed_page = self.persistent.displayed_page,
                visible_end,
                page = upload.page,
                reason = "not-static",
                "gpu-warmup-candidate-skip"
            );
            return false;
        };

        self.apply_gpu_warmup_evictions(
            visible_end,
            &plan.replacement_evict_candidates,
            "gpu-warmup-evict",
            "rank_replacement",
        );

        let estimated_bytes = Self::static_texture_bytes(&texture);
        let key = upload.key;
        if self
            .display_assets
            .gpu_warmup_cache
            .insert(key, texture, estimated_bytes)
        {
            tracing::trace!(
                view_page = self.persistent.displayed_page,
                visible_end,
                page = upload.page,
                rgba_page = upload.rgba_key.page,
                source,
                mode = plan_summary.upload_mode.unwrap_or("free-space"),
                reason = "future_l1",
                distance = upload.distance,
                current_mb = %Self::format_bytes_mb(
                    self.display_assets.gpu_warmup_cache.current_bytes()
                ),
                max_mb = %Self::format_bytes_mb(self.display_assets.gpu_warmup_cache.max_bytes()),
                entries = self.display_assets.gpu_warmup_cache.entry_count(),
                "gpu-warmup-ready"
            );
            if plan_summary.pending_uploads > 0 {
                ctx.request_repaint();
            }
            return true;
        }

        tracing::trace!(
            displayed_page = self.persistent.displayed_page,
            visible_end,
            reason = "no-suitable-entry",
            "gpu-warmup-plan-idle"
        );
        false
    }

    fn commit_display_contents(&mut self, commit: DisplayCommitContext<'_>) -> bool {
        let DisplayCommitContext {
            result,
            upload_started,
            ctx,
            poll_started,
            display_w,
            display_h,
            max_tex_side,
            gpu_history_hit,
            left,
            right,
        } = commit;
        let render_signature = RenderSignature::from_decode_request(
            result.request_quality,
            result.request_display_w,
            result.request_display_h,
            result.request_max_tex_side,
        );
        self.display_assets.content_left = left.content;
        self.display_assets.content_right = right.content;
        let upload_elapsed = upload_started.elapsed().as_millis();
        let committed = self.finalize_display_commit(FinalizeDisplayCommitContext {
            result,
            upload_elapsed,
            poll_started,
            display_w,
            display_h,
            max_tex_side,
            gpu_history_hit,
            pages: DisplayCommitPages {
                left_page: left.page,
                right_page: right.page,
            },
            ctx,
        });
        if !committed {
            return false;
        }

        let mut left_committed = false;
        if let Some(hit) = left.hit {
            match hit.source {
                GpuTextureHitSource::History => {
                    if self.display_assets.gpu_texture_history.touch(&hit.key) {
                        self.record_committed_gpu_texture_hit(
                            result.view_idx,
                            left.page,
                            right.page,
                            "left",
                            hit,
                        );
                        left_committed = true;
                    } else {
                        tracing::warn!(
                            page = hit.key.page,
                            source = hit.source.as_str(),
                            reason = "history_missing_at_commit",
                            "gpu-history-hit"
                        );
                    }
                }
                GpuTextureHitSource::Warmup => {
                    if let Some((texture, estimated_bytes)) = self
                        .display_assets
                        .gpu_warmup_cache
                        .promote_to_history(&hit.key)
                    {
                        let _ = self.display_assets.gpu_texture_history.insert(
                            hit.key,
                            texture,
                            estimated_bytes,
                        );
                        self.record_committed_gpu_texture_hit(
                            result.view_idx,
                            left.page,
                            right.page,
                            "left",
                            hit,
                        );
                        left_committed = true;
                    } else {
                        tracing::warn!(
                            page = hit.key.page,
                            source = hit.source.as_str(),
                            reason = "warmup_missing_at_commit",
                            "gpu-warmup-promote"
                        );
                    }
                }
            }
        }
        if !left_committed && left.register_gpu_history {
            self.register_gpu_texture_history(
                left.page,
                Self::static_texture_payload(self.display_assets.content_left.as_ref()),
                render_signature,
            );
        }

        let mut right_committed = false;
        if let Some(hit) = right.hit {
            match hit.source {
                GpuTextureHitSource::History => {
                    if self.display_assets.gpu_texture_history.touch(&hit.key) {
                        self.record_committed_gpu_texture_hit(
                            result.view_idx,
                            left.page,
                            right.page,
                            "right",
                            hit,
                        );
                        right_committed = true;
                    } else {
                        tracing::warn!(
                            page = hit.key.page,
                            source = hit.source.as_str(),
                            reason = "history_missing_at_commit",
                            "gpu-history-hit"
                        );
                    }
                }
                GpuTextureHitSource::Warmup => {
                    if let Some((texture, estimated_bytes)) = self
                        .display_assets
                        .gpu_warmup_cache
                        .promote_to_history(&hit.key)
                    {
                        let _ = self.display_assets.gpu_texture_history.insert(
                            hit.key,
                            texture,
                            estimated_bytes,
                        );
                        self.record_committed_gpu_texture_hit(
                            result.view_idx,
                            left.page,
                            right.page,
                            "right",
                            hit,
                        );
                        right_committed = true;
                    } else {
                        tracing::warn!(
                            page = hit.key.page,
                            source = hit.source.as_str(),
                            reason = "warmup_missing_at_commit",
                            "gpu-warmup-promote"
                        );
                    }
                }
            }
        }
        if !right_committed && right.register_gpu_history {
            self.register_gpu_texture_history(
                right.page,
                Self::static_texture_payload(self.display_assets.content_right.as_ref()),
                render_signature,
            );
        }

        true
    }

    fn gpu_texture_history_lookup(
        &mut self,
        page_left: Option<u32>,
        page_right: Option<u32>,
        request_display_w: u32,
        request_display_h: u32,
        max_tex_side: u32,
    ) -> GpuTextureDisplayLookup {
        let requirement = self.display_requirement_for_request(
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        let left_hit =
            page_left.and_then(|page| self.best_gpu_texture_hit_for_page(page, requirement));
        let right_hit =
            page_right.and_then(|page| self.best_gpu_texture_hit_for_page(page, requirement));
        match (left_hit, right_hit) {
            (Some(left), Some(right)) => GpuTextureDisplayLookup::Full {
                left: Some(left),
                right: Some(right),
            },
            (Some(left), None) if page_left.is_some() && page_right.is_some() => {
                GpuTextureDisplayLookup::Partial {
                    left: Some(left),
                    right: None,
                }
            }
            (None, Some(right)) if page_left.is_some() && page_right.is_some() => {
                GpuTextureDisplayLookup::Partial {
                    left: None,
                    right: Some(right),
                }
            }
            (Some(left), None) => GpuTextureDisplayLookup::Full {
                left: Some(left),
                right: None,
            },
            (None, Some(right)) => GpuTextureDisplayLookup::Full {
                left: None,
                right: Some(right),
            },
            (None, None) => GpuTextureDisplayLookup::Miss,
        }
    }

    fn finalize_display_commit(&mut self, commit: FinalizeDisplayCommitContext<'_>) -> bool {
        let FinalizeDisplayCommitContext {
            result,
            upload_elapsed,
            poll_started,
            display_w,
            display_h,
            max_tex_side,
            gpu_history_hit,
            pages,
            ctx,
        } = commit;
        let left_page = pages.left_page;
        let right_page = pages.right_page;
        let left_present = self.display_assets.content_left.is_some();
        let right_present = self.display_assets.content_right.is_some();
        if self.transition_logs_active() {
            let left_tex = self
                .display_assets
                .content_left
                .as_ref()
                .map(|c| c.texture().size_vec2())
                .unwrap_or_default();
            let right_tex = self
                .display_assets
                .content_right
                .as_ref()
                .map(|c| c.texture().size_vec2())
                .unwrap_or_default();
            tracing::trace!(
                frame = self.ui_runtime.show_seq,
                request_id = result.request_id,
                left_tex = ?left_tex,
                right_tex = ?right_tex,
                request_display_w = result.request_display_w,
                request_display_h = result.request_display_h,
                in_transition = self.compute_transition_flag(),
                "viewer_ui: texture updated"
            );
        }

        self.ui_runtime.loading = false;
        self.ui_runtime.pending_placeholder_after = None;
        self.persistent.displayed_page = self.persistent.requested_page;
        self.record_reading_session_display_commit(left_page, right_page);
        if self.playback.slideshow_active && self.playback.slideshow_arm_on_display {
            self.arm_slideshow_from_now(Instant::now());
        }
        if self.persistent.displayed_page == self.nav_target() {
            self.ui_runtime.nav_mode = NavMode::Sequential;
        }
        self.ui_runtime.last_display_commit_show_seq = Some(self.ui_runtime.show_seq);
        if result.left_is_animation_stream || result.right_is_animation_stream {
            self.request.active_animation_stream_view = Some(self.persistent.requested_page);
        } else {
            self.request.active_animation_stream_view = None;
        }
        tracing::debug!(
            request_id = result.request_id,
            spread = self.persistent.spread_mode,
            result_kind = ?result.kind,
            gpu_history_hit,
            upload_ms = upload_elapsed,
            poll_total_ms = poll_started.elapsed().as_millis(),
            at_ms = now_ms(),
            "viewer_ui: result applied"
        );
        tracing::debug!(
            frame = self.ui_runtime.show_seq,
            request_id = result.request_id,
            page = self.persistent.requested_page,
            upload_ms = upload_elapsed,
            gpu_history_hit,
            at_ms = now_ms(),
            "viewer-texture: upload done"
        );
        tracing::debug!(
            "[viewer-upload] nav_id={} req={} view={} upload_ms={} gpu_history_hit={}",
            result.nav_id,
            result.request_id,
            self.persistent.requested_page,
            upload_elapsed,
            gpu_history_hit
        );
        let (total_nav_ms, request_left_hit, request_right_hit) = self
            .request
            .nav_traces
            .remove(&result.nav_id)
            .map(|trace| {
                (
                    trace.started_at.elapsed().as_millis().to_string(),
                    trace.request_left_hit.as_log_str(),
                    trace.request_right_hit.as_log_str(),
                )
            })
            .unwrap_or(("none".to_owned(), "none", "none"));
        tracing::debug!(
            "[viewer-nav-commit] nav_id={} req={} view={} total_nav_ms={} request_left_hit={} request_right_hit={} final_left_ready={} final_right_ready={} decode_ms={} upload_ms={} queue_wait_ms={} gpu_history_hit={}",
            result.nav_id,
            result.request_id,
            self.persistent.requested_page,
            total_nav_ms,
            request_left_hit,
            request_right_hit,
            left_present,
            right_present,
            result.decode_ms,
            upload_elapsed,
            result.queue_wait_ms,
            gpu_history_hit
        );

        if self.consume_queued_view(display_w, display_h, max_tex_side, ctx) {
            return true;
        }

        self.trigger_prefetch(display_w, display_h, max_tex_side);
        true
    }

    fn clear_bg_admission_state(&mut self, reason: &'static str) {
        if self.request.bg_admission_state.is_empty() {
            return;
        }
        tracing::debug!(
            "[viewer-bg-admission-clear] reason={} entries={}",
            reason,
            self.request.bg_admission_state.len()
        );
        self.request.bg_admission_state.clear();
    }

    fn ensure_bg_admission_requirement(
        &mut self,
        requirement: DisplayRequirement,
        reason: &'static str,
    ) {
        if self.request.last_bg_admission_requirement == Some(requirement) {
            return;
        }
        self.clear_bg_admission_state(reason);
        self.request.last_bg_admission_requirement = Some(requirement);
    }

    pub fn reload_current_view(&mut self, ctx: &egui::Context) {
        let display_w = self.ui_runtime.last_stable_display_w.max(1);
        let display_h = self.ui_runtime.last_stable_display_h.max(1);
        let max_tex_side = super::max_texture_side_from_context(ctx);
        let current = self.persistent.requested_page;
        let nav_id = self.begin_nav(current, current, "QualityChange");
        self.start_view_request(ViewRequestContext {
            nav_id,
            physical_page: current,
            display_w,
            display_h,
            max_tex_side,
            ctx,
            reason: "QualityChange",
        });
    }

    pub(super) fn frame_cache_cap(&self) -> usize {
        crate::infra::worker::viewer_loader::frame_cache_cap_from_worker_count(
            self.request.background_worker_count,
        )
    }

    fn resolved_full_equivalent_area(
        &self,
        display_w: u32,
        display_h: u32,
    ) -> (u32, u32, &'static str) {
        resolved_full_equivalent_area_from_hint(
            self.full_equivalent_size_hint,
            display_w,
            display_h,
        )
    }

    fn worker_manager_generation(&self, display_w: u32, display_h: u32, max_tex_side: u32) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.persistent.entry.id.hash(&mut hasher);
        self.persistent.page_count.hash(&mut hasher);
        match self.persistent.spread_setting {
            SpreadMode::Auto => 0u8,
            SpreadMode::Single => 1u8,
            SpreadMode::Spread => 2u8,
        }
        .hash(&mut hasher);
        self.persistent.cover_blank.hash(&mut hasher);
        self.request.quality.hash(&mut hasher);
        self.persistent.requested_page.hash(&mut hasher);
        self.persistent.displayed_page.hash(&mut hasher);
        self.persistent.target_page.hash(&mut hasher);
        self.ui_runtime.loading.hash(&mut hasher);
        match self.ui_runtime.nav_mode {
            NavMode::Sequential => 0u8,
            NavMode::FollowLatest => 1u8,
        }
        .hash(&mut hasher);
        self.request.prefetch_dir.hash(&mut hasher);
        self.request.prefetch_anchor_view.hash(&mut hasher);
        let (full_equivalent_area_w, full_equivalent_area_h, _) =
            self.resolved_full_equivalent_area(display_w, display_h);
        full_equivalent_area_w.hash(&mut hasher);
        full_equivalent_area_h.hash(&mut hasher);
        max_tex_side.hash(&mut hasher);
        self.request.background_worker_count.hash(&mut hasher);
        self.request.rgba_cache_max_mb.hash(&mut hasher);
        self.request.active_animation_stream_view.hash(&mut hasher);
        self.request.animation_stream_request_id.hash(&mut hasher);
        hasher.finish()
    }

    pub(super) fn publish_worker_manager_state(
        &self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) {
        let (_, page_right) = self.current_view_pages(self.persistent.displayed_page);
        let effective_spread =
            page_right.is_some() || self.is_leading_cover_blank_spread(self.persistent.displayed_page);
        let page_decode_w = request_display_width_for_pair(
            self.resolved_full_equivalent_area(display_w, display_h).0,
            effective_spread,
        );
        let page_decode_h = self.resolved_full_equivalent_area(display_w, display_h).1;
        let full_equivalent = self.resolved_full_equivalent_area(display_w, display_h);
        let (visible_page_first, visible_page_second) =
            self.current_view_pages(self.persistent.displayed_page);
        let snapshot = ViewerWorkerManagerSnapshot {
            generation: self.worker_manager_generation(display_w, display_h, max_tex_side),
            book_id: self.persistent.entry.id.clone(),
            book_path: Arc::clone(&self.persistent.entry.path),
            page_count: self.persistent.page_count,
            spread_setting: self.persistent.spread_setting.clone(),
            cover_blank: self.persistent.cover_blank,
            quality: self.request.quality,
            auto_spread_plan: self.persistent.auto_spread_plan.clone(),
            requested_page: self.persistent.requested_page,
            displayed_page: self.persistent.displayed_page,
            target_page: self.persistent.target_page,
            visible_page_first,
            visible_page_second,
            loading: self.ui_runtime.loading,
            nav_mode_follow_latest: matches!(self.ui_runtime.nav_mode, NavMode::FollowLatest),
            prefetch_dir: self.request.prefetch_dir,
            max_tex_side,
            full_equivalent_area_w: full_equivalent.0,
            full_equivalent_area_h: full_equivalent.1,
            background_worker_count: self.request.background_worker_count,
            rgba_cache_max_mb: self.effective_l2_rgba_cache_max_mb(
                full_equivalent.0,
                full_equivalent.1,
                page_decode_w,
                page_decode_h,
            ),
            active_animation_stream_view: self.request.active_animation_stream_view,
            animation_stream_request_id: self.request.animation_stream_request_id,
        };
        self.request.worker_manager.update_state(snapshot);
    }

    pub(super) fn poll_worker_manager_notifications(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> bool {
        while let Some(notification) = self.request.worker_manager.try_recv_notification() {
            match notification {
                ViewerWorkerManagerNotification::BgDispatched { .. } => {}
                ViewerWorkerManagerNotification::DroppedStale { .. } => {}
                ViewerWorkerManagerNotification::Error { .. } => {}
            }
        }
        let _ = (display_w, display_h, max_tex_side);
        false
    }

    pub(super) fn display_requirement_for_request(
        &self,
        required_w: u32,
        required_h: u32,
        max_tex_side: u32,
    ) -> DisplayRequirement {
        DisplayRequirement::from_display_request(
            self.request.quality,
            required_w,
            required_h,
            max_tex_side,
        )
    }

    pub(super) fn render_signature_for_decode(
        &self,
        decode_w: u32,
        decode_h: u32,
        max_tex_side: u32,
    ) -> RenderSignature {
        RenderSignature::from_decode_request(self.request.quality, decode_w, decode_h, max_tex_side)
    }

    pub(super) fn rgba_cache_key_with_signature(
        &self,
        page: u32,
        render_signature: RenderSignature,
    ) -> RgbaCacheKey {
        RgbaCacheKey {
            page,
            render_signature,
        }
    }

    fn build_spad_target(
        &self,
        book: AdjacentBook,
        layout_settings: Option<SpadTargetLayoutSettings>,
    ) -> SpadTargetState {
        let start_page = book.book_state.start_page.unwrap_or(0) as u32;
        let mut entry_page = start_page;
        let layout_hint = layout_settings.and_then(|settings| {
            let page_map = try_load_existing_viewer_page_map_for_spad(&book.path)?;
            let page_count = u32::try_from(page_map.page_count()).ok()?;
            if entry_page >= page_count {
                return None;
            }
            let effective_spread = match settings.spread_setting {
                SpreadMode::Single => false,
                SpreadMode::Spread => {
                    !(settings.cover_blank && entry_page == 0)
                        && entry_page.saturating_add(1) < page_count
                }
                SpreadMode::Auto => {
                    let plan = build_auto_spread_plan(&page_map, settings.cover_blank)?;
                    let (anchor_page, page_right) = plan.pages_for_logical_page(entry_page)?;
                    if anchor_page != entry_page {
                        tracing::debug!(
                            path = %book.path.display(),
                            start_page = entry_page,
                            anchor_page,
                            "spad.target_layout.auto_anchor"
                        );
                        entry_page = anchor_page;
                    }
                    page_right.is_some()
                }
            };
            Some(SpadTargetLayoutHint {
                source: SpadDecodeLayoutSource::TargetPageMap,
                page_count,
                resolved_entry_page: entry_page,
                effective_spread,
            })
        });
        match layout_hint {
            Some(hint) => tracing::debug!(
                path = %book.path.display(),
                page_count = hint.page_count,
                entry_page = hint.resolved_entry_page,
                effective_spread = hint.effective_spread,
                "spad.target_layout.cache_hit"
            ),
            None => tracing::debug!(path = %book.path.display(), "spad.target_layout.cache_miss"),
        }
        let page_count = book
            .page_count
            .or_else(|| layout_hint.map(|hint| hint.page_count));
        SpadTargetState {
            path: book.path,
            _book_state: book.book_state,
            page_count,
            entry_page,
            layout_hint,
            scheduled_pages: Vec::new(),
            next_dispatch_index: 0,
            ready_pages: BTreeMap::new(),
            failed_pages: HashSet::new(),
            current_bytes: 0,
            max_bytes: 0,
            guaranteed_bytes: 0,
            extra_budget_bytes: 0,
            exhausted: false,
        }
    }

    fn total_l2_rgba_budget_bytes(&self) -> usize {
        (self.request.rgba_cache_max_mb as usize)
            .saturating_mul(1024)
            .saturating_mul(1024)
    }

    fn resolve_spad_decode_target(
        &self,
        layout_hint: Option<SpadTargetLayoutHint>,
        full_equivalent_w: u32,
        full_equivalent_h: u32,
        current_decode_w: u32,
        current_decode_h: u32,
        current_effective_spread: bool,
    ) -> SpadResolvedDecodeTarget {
        let source = layout_hint
            .map(|hint| hint.source)
            .unwrap_or(SpadDecodeLayoutSource::CurrentLayout);
        let effective_spread = layout_hint
            .map(|hint| hint.effective_spread)
            .unwrap_or(current_effective_spread);
        let decode_w = layout_hint
            .map(|hint| request_display_width_for_pair(full_equivalent_w, hint.effective_spread))
            .unwrap_or(current_decode_w);
        let decode_h = layout_hint.map(|_| full_equivalent_h).unwrap_or(current_decode_h);
        SpadResolvedDecodeTarget {
            source,
            effective_spread,
            decode_w,
            decode_h,
            two_page_rgba_bytes: static_rgba_bytes_for_decode(decode_w, decode_h, 2),
        }
    }

    fn build_spad_budget_plan(
        &self,
        full_equivalent_w: u32,
        full_equivalent_h: u32,
        current_decode_w: u32,
        current_decode_h: u32,
    ) -> SpadBudgetPlan {
        let total_bytes = self.total_l2_rgba_budget_bytes();
        let current_effective_spread = current_decode_w != full_equivalent_w;
        let prev_decode_target = self.resolve_spad_decode_target(
            self.spad.prev.as_ref().and_then(|target| target.layout_hint),
            full_equivalent_w,
            full_equivalent_h,
            current_decode_w,
            current_decode_h,
            current_effective_spread,
        );
        let next_decode_target = self.resolve_spad_decode_target(
            self.spad.next.as_ref().and_then(|target| target.layout_hint),
            full_equivalent_w,
            full_equivalent_h,
            current_decode_w,
            current_decode_h,
            current_effective_spread,
        );
        let prev_guaranteed_bytes = self
            .spad
            .prev
            .as_ref()
            .map_or(0, |_| prev_decode_target.two_page_rgba_bytes);
        let next_guaranteed_bytes = self
            .spad
            .next
            .as_ref()
            .map_or(0, |_| next_decode_target.two_page_rgba_bytes);
        let extra_5_percent_bytes = total_bytes / 20;
        let prev_extra_budget_bytes = if self.spad.prev.is_some() { extra_5_percent_bytes } else { 0 };
        let next_extra_budget_bytes = if self.spad.next.is_some() { extra_5_percent_bytes } else { 0 };
        let prev_total_budget_bytes =
            prev_guaranteed_bytes.saturating_add(prev_extra_budget_bytes);
        let next_total_budget_bytes =
            next_guaranteed_bytes.saturating_add(next_extra_budget_bytes);
        let reserved = prev_total_budget_bytes.saturating_add(next_total_budget_bytes);
        let minimum_l2_bytes = 1024 * 1024;
        let l2_effective_bytes = total_bytes
            .saturating_sub(reserved)
            .max(minimum_l2_bytes)
            .min(total_bytes.max(minimum_l2_bytes));
        SpadBudgetPlan {
            prev_guaranteed_bytes,
            next_guaranteed_bytes,
            prev_extra_budget_bytes,
            next_extra_budget_bytes,
            prev_total_budget_bytes,
            next_total_budget_bytes,
            l2_effective_bytes,
        }
    }

    fn refresh_spad_budget_plan(
        &mut self,
        full_equivalent_w: u32,
        full_equivalent_h: u32,
        current_decode_w: u32,
        current_decode_h: u32,
    ) {
        let plan = self.build_spad_budget_plan(
            full_equivalent_w,
            full_equivalent_h,
            current_decode_w,
            current_decode_h,
        );
        if let Some(prev) = self.spad.prev.as_mut() {
            prev.guaranteed_bytes = plan.prev_guaranteed_bytes;
            prev.extra_budget_bytes = plan.prev_extra_budget_bytes;
            prev.max_bytes = plan.prev_total_budget_bytes;
        }
        if let Some(next) = self.spad.next.as_mut() {
            next.guaranteed_bytes = plan.next_guaranteed_bytes;
            next.extra_budget_bytes = plan.next_extra_budget_bytes;
            next.max_bytes = plan.next_total_budget_bytes;
        }
    }

    fn effective_l2_rgba_cache_max_mb(
        &self,
        full_equivalent_w: u32,
        full_equivalent_h: u32,
        current_decode_w: u32,
        current_decode_h: u32,
    ) -> u16 {
        let bytes = self
            .build_spad_budget_plan(
                full_equivalent_w,
                full_equivalent_h,
                current_decode_w,
                current_decode_h,
            )
            .l2_effective_bytes;
        ((bytes.saturating_add((1024 * 1024) - 1)) / (1024 * 1024)) as u16
    }

    fn l2_settled_status(&self) -> L2StreamingStatus {
        self.request.worker_manager.l2_status()
    }

    fn spad_target_mut(&mut self, side: SpadSide) -> Option<&mut SpadTargetState> {
        match side {
            SpadSide::Prev => self.spad.prev.as_mut(),
            SpadSide::Next => self.spad.next.as_mut(),
        }
    }

    fn spad_inflight(&self, side: SpadSide) -> Option<&SpadInflightRequest> {
        match side {
            SpadSide::Next => self.spad.next_inflight.as_ref(),
            SpadSide::Prev => self.spad.prev_inflight.as_ref(),
        }
    }

    fn take_spad_inflight(&mut self, side: SpadSide) -> Option<SpadInflightRequest> {
        match side {
            SpadSide::Next => self.spad.next_inflight.take(),
            SpadSide::Prev => self.spad.prev_inflight.take(),
        }
    }

    fn set_spad_inflight(&mut self, side: SpadSide, inflight: SpadInflightRequest) {
        match side {
            SpadSide::Next => self.spad.next_inflight = Some(inflight),
            SpadSide::Prev => self.spad.prev_inflight = Some(inflight),
        }
    }

    fn ensure_spad_scheduled_pages(target: &mut SpadTargetState) {
        if !target.scheduled_pages.is_empty() {
            return;
        }
        let Some(page_count) = target.page_count.filter(|page_count| *page_count > 0) else {
            target.scheduled_pages.push(target.entry_page);
            target.scheduled_pages.push(target.entry_page.saturating_add(1));
            return;
        };
        let entry = if target.entry_page < page_count {
            target.entry_page
        } else {
            0
        };
        target.entry_page = entry;
        target.scheduled_pages.push(entry);
        for page in entry.saturating_add(1)..page_count {
            target.scheduled_pages.push(page);
        }
        if target.scheduled_pages.len() < 2 && entry > 0 {
            target.scheduled_pages.push(entry - 1);
        }
    }

    fn mark_spad_page_failed(target: &mut SpadTargetState, page: u32) {
        target.failed_pages.insert(page);
        let should_advance = target
            .scheduled_pages
            .get(target.next_dispatch_index)
            .copied()
            == Some(page);
        if should_advance {
            target.next_dispatch_index = target.next_dispatch_index.saturating_add(1);
        }
    }

    fn trace_spad_exhausted(
        session: u64,
        generation: u64,
        side: SpadSide,
        target: &SpadTargetState,
        reason: &'static str,
    ) {
        spad_trace_debug!(
            "[spad-exhausted] session={} generation={} side={} target={} current_bytes={} max_bytes={} reason={}",
            session,
            generation,
            side.as_str(),
            target.path.display(),
            target.current_bytes,
            target.max_bytes,
            reason
        );
    }

    fn mark_spad_target_exhausted(
        session: u64,
        generation: u64,
        side: SpadSide,
        target: &mut SpadTargetState,
        reason: &'static str,
    ) {
        if target.exhausted {
            return;
        }
        target.exhausted = true;
        Self::trace_spad_exhausted(session, generation, side, target, reason);
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_spad_request(
        &mut self,
        side: SpadSide,
        full_equivalent_w: u32,
        full_equivalent_h: u32,
        current_decode_w: u32,
        current_decode_h: u32,
        current_effective_spread: bool,
        max_tex_side: u32,
    ) -> bool {
        if self.spad_inflight(side).is_some() {
            return false;
        }
        let session = self.spad.session;
        let generation = self.spad.generation;
        let quality = self.request.quality;
        let frame_cache_cap = self.frame_cache_cap();
        let nav_id = self.request.active_nav_id.unwrap_or(0);
        let (path, page, layout_hint, target_budget_bytes) = {
            let Some(target) = self.spad_target_mut(side) else {
                return false;
            };
            if target.exhausted {
                return false;
            }
            if target.current_bytes >= target.max_bytes {
                Self::mark_spad_target_exhausted(
                    session,
                    generation,
                    side,
                    target,
                    "budget_full_before_dispatch",
                );
                return false;
            }
            Self::ensure_spad_scheduled_pages(target);
            loop {
                let Some(page) = target.scheduled_pages.get(target.next_dispatch_index).copied() else {
                    return false;
                };
                if target.ready_pages.contains_key(&page) || target.failed_pages.contains(&page) {
                    target.next_dispatch_index = target.next_dispatch_index.saturating_add(1);
                    continue;
                }
                break (
                    target.path.clone(),
                    page,
                    target.layout_hint,
                    target.max_bytes,
                );
            }
        };
        let decode_target = self.resolve_spad_decode_target(
            layout_hint,
            full_equivalent_w,
            full_equivalent_h,
            current_decode_w,
            current_decode_h,
            current_effective_spread,
        );
        tracing::trace!(
            source = decode_target.source.as_str(),
            effective_spread = decode_target.effective_spread,
            decode_w = decode_target.decode_w,
            decode_h = decode_target.decode_h,
            expected_two_page_rgba_bytes = decode_target.two_page_rgba_bytes,
            target_budget_bytes,
            "spad.dispatch.decode_size"
        );
        let render_signature =
            RenderSignature::from_decode_request(
                quality,
                decode_target.decode_w,
                decode_target.decode_h,
                max_tex_side,
            );
        let request = ViewerLoadRequest {
            path: Arc::from(path.as_path()),
            view_idx: page,
            page_left: Some(page),
            page_right: None,
            display_w: decode_target.decode_w,
            display_h: decode_target.decode_h,
            quality,
            max_tex_side,
            frame_cache_cap,
            nav_id,
            interactive: false,
        };
        let request_id = match side {
            SpadSide::Next => self.request.loader.send_spad_next_request(request),
            SpadSide::Prev => self.request.loader.send_spad_prev_request(request),
        };
        self.set_spad_inflight(side, SpadInflightRequest {
            request_id,
            session,
            generation,
            side,
            path: path.clone(),
            page,
            render_signature,
        });
        spad_trace_debug!(
            "[spad-dispatch] session={} generation={} side={} page={} path={} request_id={}",
            session,
            generation,
            side.as_str(),
            page,
            path.display(),
            request_id
        );
        true
    }

    pub(super) fn poll_spad(
        &mut self,
        ctx: &egui::Context,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> bool {
        let mut handled = false;
        let current_generation = self.worker_manager_generation(display_w, display_h, max_tex_side);
        let current_book_id = self.persistent.entry.id.clone();
        let layout = self.view_layout_for_with_caller(
            self.persistent.displayed_page,
            display_w,
            display_h,
            false,
        );
        self.refresh_spad_budget_plan(
            layout.full_equivalent_area_w,
            layout.full_equivalent_area_h,
            layout.page_decode_w,
            layout.page_decode_h,
        );
        while let Some(result) = self.request.loader.try_recv_spad_next() {
            handled = true;
            self.handle_spad_result(SpadSide::Next, result);
        }
        while let Some(result) = self.request.loader.try_recv_spad_prev() {
            handled = true;
            self.handle_spad_result(SpadSide::Prev, result);
        }
        {
            let l2_status = self.l2_settled_status();
            let skip_reason = if !l2_status.settled {
                Some("not_settled")
            } else if l2_status.book_id.as_ref() != Some(&current_book_id) {
                Some("book_mismatch")
            } else if l2_status.generation != current_generation {
                Some("generation_mismatch")
            } else {
                None
            };
            if let Some(reason) = skip_reason {
                spad_trace_debug!(
                    "[spad-skip-not-settled] reason={} generation={} settled={} inflight={}",
                    reason,
                    l2_status.generation,
                    l2_status.settled,
                    self.spad.next_inflight.is_some() || self.spad.prev_inflight.is_some()
                );
                return handled;
            }
            for side in [SpadSide::Next, SpadSide::Prev] {
                let session = self.spad.session;
                let generation = self.spad.generation;
                if let Some(target) = self.spad_target_mut(side) {
                    Self::ensure_spad_scheduled_pages(target);
                    if !target.exhausted && target.current_bytes >= target.max_bytes {
                        Self::mark_spad_target_exhausted(
                            session,
                            generation,
                            side,
                            target,
                            "budget_full_before_dispatch",
                        );
                    }
                }
            }
            let next_dispatchable = Self::spad_target_dispatchable(self.spad.next.as_ref());
            let prev_dispatchable = Self::spad_target_dispatchable(self.spad.prev.as_ref());
            if !next_dispatchable && !prev_dispatchable {
                let has_any_target = Self::spad_target_exists(self.spad.next.as_ref())
                    || Self::spad_target_exists(self.spad.prev.as_ref());
                let all_existing_targets_exhausted =
                    (!Self::spad_target_exists(self.spad.next.as_ref())
                        || Self::spad_target_exhausted(self.spad.next.as_ref()))
                        && (!Self::spad_target_exists(self.spad.prev.as_ref())
                            || Self::spad_target_exhausted(self.spad.prev.as_ref()));
                let both_targets_exist = Self::spad_target_exists(self.spad.next.as_ref())
                    && Self::spad_target_exists(self.spad.prev.as_ref());
                if !self.spad.no_dispatch_logged {
                    if both_targets_exist && all_existing_targets_exhausted {
                        spad_trace_debug!(
                            "[spad-exhausted] session={} generation={} side=both target=- current_bytes=0 max_bytes=0 reason=both_sides_exhausted",
                            self.spad.session,
                            self.spad.generation
                        );
                    } else if has_any_target && all_existing_targets_exhausted {
                        spad_trace_debug!(
                            "[spad-exhausted] session={} generation={} side=both target=- current_bytes=0 max_bytes=0 reason=all_existing_targets_exhausted",
                            self.spad.session,
                            self.spad.generation
                        );
                    } else {
                        spad_trace_debug!(
                            "[spad-skip] session={} generation={} side=both target=- current_bytes=0 max_bytes=0 reason=no_dispatchable_targets",
                            self.spad.session,
                            self.spad.generation
                        );
                    }
                    self.spad.no_dispatch_logged = true;
                }
                return handled;
            }
            self.spad.no_dispatch_logged = false;
            if self.dispatch_spad_requests(
                layout.full_equivalent_area_w,
                layout.full_equivalent_area_h,
                layout.page_decode_w,
                layout.page_decode_h,
                layout.effective_spread,
                max_tex_side,
            ) {
                ctx.request_repaint();
            }
        }
        handled
    }

    fn spad_target_dispatchable(target: Option<&SpadTargetState>) -> bool {
        let Some(target) = target else {
            return false;
        };
        !target.exhausted
            && target.current_bytes < target.max_bytes
            && Self::spad_target_has_pending_page(target)
    }

    fn spad_target_has_pending_page(target: &SpadTargetState) -> bool {
        if target.scheduled_pages.is_empty() {
            return true;
        }
        target.scheduled_pages[target.next_dispatch_index.min(target.scheduled_pages.len())..]
            .iter()
            .copied()
            .any(|page| {
                !target.ready_pages.contains_key(&page) && !target.failed_pages.contains(&page)
            })
    }

    fn spad_target_exists(target: Option<&SpadTargetState>) -> bool {
        target.is_some()
    }

    fn spad_target_exhausted(target: Option<&SpadTargetState>) -> bool {
        target.is_some_and(|target| target.exhausted)
    }

    fn dispatch_spad_requests(
        &mut self,
        full_equivalent_w: u32,
        full_equivalent_h: u32,
        current_decode_w: u32,
        current_decode_h: u32,
        current_effective_spread: bool,
        max_tex_side: u32,
    ) -> bool {
        let next_dispatched = self.dispatch_spad_request(
            SpadSide::Next,
            full_equivalent_w,
            full_equivalent_h,
            current_decode_w,
            current_decode_h,
            current_effective_spread,
            max_tex_side,
        );
        let prev_dispatched = self.dispatch_spad_request(
            SpadSide::Prev,
            full_equivalent_w,
            full_equivalent_h,
            current_decode_w,
            current_decode_h,
            current_effective_spread,
            max_tex_side,
        );
        next_dispatched || prev_dispatched
    }

    fn handle_spad_result(&mut self, expected_side: SpadSide, result: ViewerResult) {
        let Some(inflight) = self.spad_inflight(expected_side) else {
            spad_trace_debug!("[spad-drop] reason=no_inflight side={} request_id={}", expected_side.as_str(), result.request_id);
            return;
        };
        if inflight.side != expected_side {
            spad_trace_debug!("[spad-drop] reason=side expected_side={} inflight_side={} request_id={}", expected_side.as_str(), inflight.side.as_str(), result.request_id);
            return;
        }
        if inflight.request_id != result.request_id {
            spad_trace_debug!(
                "[spad-drop] reason=request_id side={} request_id={} expected={}",
                expected_side.as_str(),
                result.request_id,
                inflight.request_id
            );
            return;
        }
        if inflight.session != self.spad.session {
            spad_trace_debug!(
                "[spad-drop] reason=session side={} request_id={} result_session={} current_session={}",
                expected_side.as_str(),
                result.request_id,
                inflight.session,
                self.spad.session
            );
            return;
        }
        if inflight.generation != self.spad.generation {
            spad_trace_debug!(
                "[spad-drop] reason=generation side={} request_id={} result_generation={} current_generation={}",
                expected_side.as_str(),
                result.request_id,
                inflight.generation,
                self.spad.generation
            );
            return;
        }
        let Some(inflight) = self.take_spad_inflight(expected_side) else {
            spad_trace_debug!("[spad-drop] reason=no_inflight_after_match side={} request_id={}", expected_side.as_str(), result.request_id);
            return;
        };
        let Some(target) = self.spad_target_mut(inflight.side) else {
            spad_trace_debug!("[spad-drop] reason=target_missing side={} request_id={}", expected_side.as_str(), result.request_id);
            return;
        };
        if target.path != inflight.path {
            spad_trace_debug!("[spad-drop] reason=target_mismatch side={} request_id={}", expected_side.as_str(), result.request_id);
            return;
        }
        if result.page_count > 0 && target.page_count != Some(result.page_count) {
            target.page_count = Some(result.page_count);
            target.scheduled_pages.clear();
            target.next_dispatch_index = 0;
            Self::ensure_spad_scheduled_pages(target);
        }
        if let Some(page_count) = target.page_count {
            if inflight.page >= page_count {
                Self::mark_spad_page_failed(target, inflight.page);
                spad_trace_debug!(
                    "[spad-drop] reason=out_of_range request_id={} page={} page_count={}",
                    result.request_id,
                    inflight.page,
                    page_count
                );
                return;
            }
        }
        if target.failed_pages.contains(&inflight.page) {
            spad_trace_debug!(
                "[spad-drop] reason=already_failed request_id={} page={}",
                result.request_id,
                inflight.page
            );
            return;
        }
        if target.ready_pages.contains_key(&inflight.page) {
            spad_trace_debug!(
                "[spad-drop] reason=already_ready request_id={} page={}",
                result.request_id,
                inflight.page
            );
            return;
        }
        let Some(frames) = result.left.filter(|_| !result.left_is_animation_stream) else {
            Self::mark_spad_page_failed(target, inflight.page);
            spad_trace_debug!("[spad-drop] reason=decode_missing request_id={}", result.request_id);
            return;
        };
        let Some(bytes) = RgbaPageCache::static_rgba_bytes(frames.as_ref()) else {
            Self::mark_spad_page_failed(target, inflight.page);
            spad_trace_debug!("[spad-drop] reason=non_static request_id={}", result.request_id);
            return;
        };
        if target.current_bytes.saturating_add(bytes) > target.max_bytes {
            Self::mark_spad_page_failed(target, inflight.page);
            Self::mark_spad_target_exhausted(
                inflight.session,
                inflight.generation,
                inflight.side,
                target,
                "budget_drop_after_decode",
            );
            spad_trace_debug!(
                "[spad-drop] reason=budget side={} page={} bytes={} current={} max={}",
                inflight.side.as_str(),
                inflight.page,
                bytes,
                target.current_bytes,
                target.max_bytes
            );
            return;
        }
        target.current_bytes = target.current_bytes.saturating_add(bytes);
        target.ready_pages.insert(
            inflight.page,
            SpadReadyPage {
                frames,
                render_signature: inflight.render_signature,
            },
        );
        target.failed_pages.remove(&inflight.page);
        target.next_dispatch_index = target.next_dispatch_index.saturating_add(1);
        spad_trace_debug!(
            "[spad-complete] session={} generation={} side={} page={} ready={} bytes={}",
            inflight.session,
            inflight.generation,
            inflight.side.as_str(),
            inflight.page,
            target.ready_pages.len(),
            target.current_bytes
        );
    }

    pub fn take_spad_ready_pages_for_target(
        &mut self,
        target_path: &Path,
    ) -> Vec<SpadPromotionPage> {
        let mut out = Vec::new();
        let session = self.spad.session;
        let generation = self.spad.generation;
        for side in [SpadSide::Prev, SpadSide::Next] {
            let Some(target) = self.spad_target_mut(side) else {
                continue;
            };
            if !paths_equivalent_for_selection(&target.path, target_path) {
                continue;
            }
            for (page, ready) in &target.ready_pages {
                out.push(SpadPromotionPage {
                    page: *page,
                    frames: Arc::clone(&ready.frames),
                    render_signature: ready.render_signature,
                    target_path: target.path.clone(),
                    session,
                    generation,
                    target_page_count: target.page_count,
                });
            }
        }
        if out.is_empty() {
            let prev_summary = self
                .spad
                .prev
                .as_ref()
                .map(|target| {
                    format!(
                        "path={} ready={} range={} page_count={:?}",
                        target.path.display(),
                        target.ready_pages.len(),
                        Self::spad_ready_range_label(target),
                        target.page_count
                    )
                })
                .unwrap_or_else(|| "none".to_owned());
            let next_summary = self
                .spad
                .next
                .as_ref()
                .map(|target| {
                    format!(
                        "path={} ready={} range={} page_count={:?}",
                        target.path.display(),
                        target.ready_pages.len(),
                        Self::spad_ready_range_label(target),
                        target.page_count
                    )
                })
                .unwrap_or_else(|| "none".to_owned());
            tracing::info!(
                target_path = %target_path.display(),
                prev = %prev_summary,
                next = %next_summary,
                session = self.spad.session,
                generation = self.spad.generation,
                "spad.take_ready.empty"
            );
        } else {
            tracing::info!(
                target_path = %target_path.display(),
                ready_count = out.len(),
                session = self.spad.session,
                generation = self.spad.generation,
                "spad.take_ready.hit"
            );
        }
        out
    }

    pub fn promote_spad_ready_pages_to_l1_future(
        &mut self,
        ctx: &egui::Context,
        spad_ready_pages: Vec<SpadPromotionPage>,
        current_display_w: u32,
        current_display_h: u32,
        current_max_tex_side: u32,
    ) {
        tracing::info!(
            ready_count = spad_ready_pages.len(),
            target_path = %self.persistent.entry.path.display(),
            persistent_page_count = self.persistent.page_count,
            current_display_w,
            current_display_h,
            current_max_tex_side,
            "spad.promote.begin"
        );
        // These values validate consistency within the old SPAD-ready batch.
        // They intentionally do not compare against the newly created ViewerState.
        let ready_batch_session = spad_ready_pages.first().map(|ready| ready.session);
        let ready_batch_generation = spad_ready_pages.first().map(|ready| ready.generation);
        if let Some(page_count) = spad_ready_pages
            .iter()
            .filter(|ready| paths_equivalent_for_selection(&ready.target_path, &self.persistent.entry.path))
            .filter(|ready| Some(ready.session) == ready_batch_session)
            .filter(|ready| Some(ready.generation) == ready_batch_generation)
            .filter_map(|ready| ready.target_page_count)
            .find(|page_count| *page_count > 0)
        {
            if self.adopt_page_count_if_empty(page_count, "spad-promote") {
                tracing::info!(page_count, source = "spad-promote", "spad.promote.page_count_adopt");
            }
        }
        let layout = self.view_layout_for_with_caller(
            self.persistent.displayed_page,
            current_display_w,
            current_display_h,
            false,
        );
        let requirement = self.display_requirement_for_request(
            layout.page_decode_w,
            layout.page_decode_h,
            current_max_tex_side,
        );
        for ready in spad_ready_pages {
            if !paths_equivalent_for_selection(&ready.target_path, &self.persistent.entry.path) {
                tracing::info!(page = ready.page, reason = "target_mismatch", "spad.promote.drop");
                spad_trace_debug!("[spad-promote-l1-failed] page={} reason=target_mismatch", ready.page);
                continue;
            }
            if Some(ready.session) != ready_batch_session {
                tracing::info!(page = ready.page, reason = "session_mismatch", "spad.promote.drop");
                spad_trace_debug!("[spad-promote-l1-failed] page={} reason=session_mismatch", ready.page);
                continue;
            }
            if Some(ready.generation) != ready_batch_generation {
                tracing::info!(page = ready.page, reason = "generation_mismatch", "spad.promote.drop");
                spad_trace_debug!("[spad-promote-l1-failed] page={} reason=generation_mismatch", ready.page);
                continue;
            }
            if let Some(page_count) = ready.target_page_count {
                if ready.page >= page_count {
                    tracing::info!(page = ready.page, reason = "page_out_of_range", page_count, "spad.promote.drop");
                    spad_trace_debug!("[spad-promote-l1-failed] page={} reason=page_out_of_range", ready.page);
                    continue;
                }
            } else if self.persistent.page_count > 0 && ready.page >= self.persistent.page_count {
                tracing::info!(
                    page = ready.page,
                    reason = "page_out_of_range",
                    page_count = self.persistent.page_count,
                    "spad.promote.drop"
                );
                spad_trace_debug!("[spad-promote-l1-failed] page={} reason=page_out_of_range", ready.page);
                continue;
            }
            if !ready.render_signature.is_suitable_for(requirement) {
                tracing::info!(
                    page = ready.page,
                    reason = "render_signature_mismatch",
                    mismatch_reason = ready.render_signature.mismatch_reason(requirement),
                    "spad.promote.drop"
                );
                spad_trace_debug!(
                    "[spad-promote-l1-failed] page={} reason=render_signature_mismatch",
                    ready.page
                );
                continue;
            }
            let content =
                PageContent::from_frames(ready.frames, "viewer_spad_promote", ctx);
            let PageContent::Static(texture) = content else {
                tracing::info!(page = ready.page, reason = "non_static", "spad.promote.drop");
                spad_trace_debug!(
                    "[spad-promote-l1-failed] page={} reason=non_static",
                    ready.page
                );
                continue;
            };
            let key = PageRenderSignatureKey {
                page: ready.page,
                render_signature: ready.render_signature,
            };
            let estimated_bytes = Self::static_texture_bytes(&texture);
            if self
                .display_assets
                .gpu_warmup_cache
                .insert(key, texture, estimated_bytes)
            {
                tracing::info!(page = ready.page, estimated_bytes, "spad.promote.success");
                spad_trace_debug!("[spad-promote-l1] page={}", ready.page);
            } else {
                tracing::info!(page = ready.page, reason = "warmup_rejected", "spad.promote.drop");
                spad_trace_debug!(
                    "[spad-promote-l1-failed] page={} reason=warmup_rejected",
                    ready.page
                );
            }
        }
    }

    pub(super) fn spad_overlay_lines(&self) -> [String; 3] {
        [
            "SPAD".to_owned(),
            format!(
                "  P: {}",
                self.spad
                    .prev
                    .as_ref()
                    .map(Self::spad_ready_range_label)
                    .unwrap_or_else(|| "none".to_owned())
            ),
            format!(
                "  N: {}",
                self.spad
                    .next
                    .as_ref()
                    .map(Self::spad_ready_range_label)
                    .unwrap_or_else(|| "none".to_owned())
            ),
        ]
    }

    fn spad_ready_range_label(target: &SpadTargetState) -> String {
        let Some(first) = target.ready_pages.keys().next().copied() else {
            return "-".to_owned();
        };
        let last = target.ready_pages.keys().next_back().copied().unwrap_or(first);
        format!("{}p {}..{}", target.ready_pages.len(), first, last)
    }

    fn resolve_display_page_state(
        &mut self,
        page: Option<u32>,
        target_w: u32,
        target_h: u32,
        max_tex_side: u32,
    ) -> (DisplayPageState, Option<Arc<Vec<img::FrameData>>>) {
        let Some(page) = page else {
            return (DisplayPageState::Ready, None);
        };
        let display_requirement =
            self.display_requirement_for_request(target_w, target_h, max_tex_side);
        if let Some((frames, cached_signature)) = self
            .display_assets
            .interactive_rgba_cache
            .get_suitable(page, display_requirement)
        {
            tracing::trace!(
                "[interactive_rgba.hit] page={} required_w={} required_h={} cached_w={} cached_h={} render_signature.quality={:?} render_signature.max_tex_side={}",
                page,
                display_requirement.required_w,
                display_requirement.required_h,
                cached_signature.target_w,
                cached_signature.target_h,
                cached_signature.quality,
                cached_signature.max_tex_side
            );
            return (DisplayPageState::Ready, Some(frames));
        }
        tracing::trace!(
            "[interactive_rgba.miss] page={} required_w={} required_h={} render_signature.quality={:?} render_signature.max_tex_side={}",
            page,
            display_requirement.required_w,
            display_requirement.required_h,
            display_requirement.quality,
            display_requirement.max_tex_side
        );
        let bg_rgba_cache = self.request.worker_manager.bg_rgba_cache();
        if let Some((frames, cached_signature)) = bg_rgba_cache
            .write()
            .ok()
            .and_then(|mut cache| cache.get_suitable(page, display_requirement))
        {
            tracing::trace!(
                "[bg_rgba.hit] page={} required_w={} required_h={} cached_w={} cached_h={} render_signature.quality={:?} render_signature.max_tex_side={}",
                page,
                display_requirement.required_w,
                display_requirement.required_h,
                cached_signature.target_w,
                cached_signature.target_h,
                cached_signature.quality,
                cached_signature.max_tex_side
            );
            let interactive_request_pages =
                self.interactive_request_pages(target_w, target_h, max_tex_side);
            let key = self.rgba_cache_key_with_signature(page, cached_signature);
            let inserted = self.display_assets.interactive_rgba_cache.insert(
                key,
                Arc::clone(&frames),
                "bg-promote",
                self.persistent.requested_page,
                &interactive_request_pages,
            );
            if inserted {
                tracing::trace!(
                    "[bg_rgba.promote_to_interactive] page={} required_w={} required_h={} cached_w={} cached_h={} render_signature.quality={:?} render_signature.max_tex_side={}",
                    page,
                    display_requirement.required_w,
                    display_requirement.required_h,
                    cached_signature.target_w,
                    cached_signature.target_h,
                    cached_signature.quality,
                    cached_signature.max_tex_side
                );
            }
            return (DisplayPageState::Ready, Some(frames));
        }
        tracing::trace!(
            "[bg_rgba.miss] page={} required_w={} required_h={} render_signature.quality={:?} render_signature.max_tex_side={}",
            page,
            display_requirement.required_w,
            display_requirement.required_h,
            display_requirement.quality,
            display_requirement.max_tex_side
        );
        tracing::trace!(
            "[bg_inflight.adopt_miss] page={} required_w={} required_h={} render_signature.quality={:?} render_signature.max_tex_side={}",
            page,
            display_requirement.required_w,
            display_requirement.required_h,
            display_requirement.quality,
            display_requirement.max_tex_side
        );
        (DisplayPageState::Missing, None)
    }

    fn make_synthetic_display_result(
        &self,
        request: SyntheticDisplayRequest,
    ) -> crate::infra::worker::viewer_loader::ViewerResult {
        let SyntheticDisplayRequest {
            nav_id,
            physical_page,
            page_left,
            page_right,
            left,
            right,
            request_display_w,
            request_display_h,
            request_quality,
            request_max_tex_side,
        } = request;
        let view_idx = physical_page;
        crate::infra::worker::viewer_loader::ViewerResult {
            request_id: nav_id,
            left,
            right,
            page_count: self.persistent.page_count,
            left_orig_w: 0,
            left_orig_h: 0,
            right_orig_w: 0,
            right_orig_h: 0,
            error: None,
            kind: ViewerResultKind::Display,
            left_is_animation_stream: false,
            right_is_animation_stream: false,
            left_stream_exhausted: false,
            right_stream_exhausted: false,
            queue_wait_ms: 0,
            decode_ms: 0,
            view_idx,
            nav_id,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            request_quality,
            request_max_tex_side,
            worker: "viewer-ui-cache".to_owned(),
        }
    }

    pub(super) fn clear_interactive_in_flight(&mut self) {
        self.request.pending_id = 0;
        self.request.pending_id_aux = None;
        self.request.pending_interactive_group = None;
        self.request.interactive_inflight_even_page = None;
        self.request.interactive_inflight_odd_page = None;
    }

    pub(super) fn start_interactive_group_request(
        &mut self,
        request: InteractiveGroupRequest,
    ) -> u64 {
        let InteractiveGroupRequest {
            nav_id,
            physical_page,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
        } = request;
        let view_idx = physical_page;
        self.request.interactive_generation = self.request.interactive_generation.saturating_add(1);
        let generation = self.request.interactive_generation;
        let group_id = nav_id;
        let mut left_request_id = None;
        let mut right_request_id = None;

        if let Some(page) = page_left {
            let req = self.request.loader.send_request(ViewerLoadRequest {
                path: Arc::clone(&self.persistent.entry.path),
                view_idx,
                page_left: Some(page),
                page_right: None,
                display_w: request_display_w,
                display_h: request_display_h,
                quality: self.request.quality,
                max_tex_side,
                frame_cache_cap: self.frame_cache_cap(),
                nav_id,
                interactive: true,
            });
            left_request_id = Some(req);
            tracing::trace!(
                "[interactive-page-enqueue] group_id={} generation={} request_id={} page={} side=left worker={} nav_id={}",
                group_id,
                generation,
                req,
                page,
                if page % 2 == 0 {
                    "interactive-even"
                } else {
                    "interactive-odd"
                },
                nav_id
            );
        }
        if let Some(page) = page_right {
            let req = self.request.loader.send_request(ViewerLoadRequest {
                path: Arc::clone(&self.persistent.entry.path),
                view_idx,
                page_left: Some(page),
                page_right: None,
                display_w: request_display_w,
                display_h: request_display_h,
                quality: self.request.quality,
                max_tex_side,
                frame_cache_cap: self.frame_cache_cap(),
                nav_id,
                interactive: true,
            });
            right_request_id = Some(req);
            tracing::trace!(
                "[interactive-page-enqueue] group_id={} generation={} request_id={} page={} side=right worker={} nav_id={}",
                group_id,
                generation,
                req,
                page,
                if page % 2 == 0 {
                    "interactive-even"
                } else {
                    "interactive-odd"
                },
                nav_id
            );
        }

        self.request.pending_id = left_request_id.or(right_request_id).unwrap_or(0);
        self.request.pending_id_aux = match (left_request_id, right_request_id) {
            (Some(l), Some(r)) if l != r => Some(r),
            _ => None,
        };
        self.request.interactive_inflight_even_page = [page_left, page_right]
            .into_iter()
            .flatten()
            .find(|page| page % 2 == 0);
        self.request.interactive_inflight_odd_page = [page_left, page_right]
            .into_iter()
            .flatten()
            .find(|page| page % 2 == 1);
        self.request.pending_interactive_group = Some(InteractivePendingGroup {
            group_id,
            generation,
            page_left,
            page_right,
            left_request_id,
            right_request_id,
            left_result: None,
            right_result: None,
        });
        self.request.pending_id
    }

    pub(super) fn absorb_interactive_partial(
        &mut self,
        result: crate::infra::worker::viewer_loader::ViewerResult,
    ) -> Option<crate::infra::worker::viewer_loader::ViewerResult> {
        let Some(group) = self.request.pending_interactive_group.as_mut() else {
            tracing::trace!(
                "[interactive-group-drop] reason=missing_group req={} nav_id={}",
                result.request_id,
                result.nav_id
            );
            return None;
        };

        if group.generation != self.request.interactive_generation {
            tracing::trace!(
                "[interactive-group-drop] reason=generation req={} nav_id={} group_id={} result_generation={} current_generation={}",
                result.request_id,
                result.nav_id,
                group.group_id,
                group.generation,
                self.request.interactive_generation
            );
            return None;
        }

        let page = result.page_left.or(result.page_right);
        if group.left_request_id == Some(result.request_id) {
            self.request.interactive_inflight_even_page = self
                .request
                .interactive_inflight_even_page
                .filter(|p| Some(*p) != page);
            self.request.interactive_inflight_odd_page = self
                .request
                .interactive_inflight_odd_page
                .filter(|p| Some(*p) != page);
            tracing::trace!(
                "[interactive-page-done] group_id={} generation={} request_id={} page={:?} side=left decode_ms={} worker={}",
                group.group_id,
                group.generation,
                result.request_id,
                page,
                result.decode_ms,
                result.worker
            );
            group.left_result = Some(result);
        } else if group.right_request_id == Some(result.request_id) {
            self.request.interactive_inflight_even_page = self
                .request
                .interactive_inflight_even_page
                .filter(|p| Some(*p) != page);
            self.request.interactive_inflight_odd_page = self
                .request
                .interactive_inflight_odd_page
                .filter(|p| Some(*p) != page);
            tracing::trace!(
                "[interactive-page-done] group_id={} generation={} request_id={} page={:?} side=right decode_ms={} worker={}",
                group.group_id,
                group.generation,
                result.request_id,
                page,
                result.decode_ms,
                result.worker
            );
            group.right_result = Some(result);
        } else {
            tracing::trace!(
                "[interactive-group-drop] reason=group_id req={} nav_id={} group_id={}",
                result.request_id,
                result.nav_id,
                group.group_id
            );
            return None;
        }

        let left_done = group.left_request_id.is_none() || group.left_result.is_some();
        let right_done = group.right_request_id.is_none() || group.right_result.is_some();
        if !left_done || !right_done {
            return None;
        }

        let left_decode_ms = group.left_result.as_ref().map(|v| v.decode_ms).unwrap_or(0);
        let right_decode_ms = group
            .right_result
            .as_ref()
            .map(|v| v.decode_ms)
            .unwrap_or(0);
        let left = group.left_result.take();
        let right = group.right_result.take();
        let merged = match (left, right) {
            (Some(mut l), Some(r)) => {
                if l.right.is_none() {
                    l.right = r.left;
                    l.right_orig_w = r.left_orig_w;
                    l.right_orig_h = r.left_orig_h;
                    l.right_is_animation_stream = r.left_is_animation_stream;
                    l.right_stream_exhausted = r.left_stream_exhausted;
                }
                l.decode_ms = l.decode_ms.saturating_add(r.decode_ms);
                l.queue_wait_ms = l.queue_wait_ms.max(r.queue_wait_ms);
                l.page_left = group.page_left;
                l.page_right = group.page_right;
                l
            }
            (Some(mut l), None) => {
                l.page_left = group.page_left;
                l.page_right = group.page_right;
                l
            }
            (None, Some(mut r)) => {
                r.page_left = group.page_left;
                r.page_right = group.page_right;
                r
            }
            (None, None) => return None,
        };
        tracing::trace!(
            "[interactive-group-commit] group_id={} generation={} nav_id={} left_decode_ms={} right_decode_ms={} total_decode_ms={}",
            group.group_id,
            group.generation,
            merged.nav_id,
            left_decode_ms,
            right_decode_ms,
            merged.decode_ms
        );
        self.request.pending_interactive_group = None;
        self.request.pending_id_aux = None;
        Some(merged)
    }

    /// 初回 request は `show()` 後にだけ発火させ、未確定の表示幅を避ける。
    pub(super) fn start_initial_load(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
    ) {
        self.request.initial_load_pending = false;
        self.ui_runtime.loading = true;
        self.load_view(
            self.persistent.requested_page,
            display_w,
            display_h,
            max_tex_side,
            ctx,
        );
    }

    /// 初回 request 前だけ、Library 応答を開始ページの上書きに使う。
    pub(crate) fn apply_start_page_before_initial_load(&mut self, start_page: Option<usize>) {
        let Some(start_page) = start_page else {
            return;
        };
        if !self.request.initial_load_pending {
            return;
        }

        let start_page = start_page as u32;
        self.persistent.requested_page = start_page;
        self.persistent.displayed_page = start_page;
        self.persistent.target_page = start_page;
        self.request.prefetch_anchor_view = start_page;
        self.invalidate_spread_snapshot();

        if matches!(self.persistent.spread_setting, SpreadMode::Auto) {
            self.persistent.spread_mode = self
                .persistent
                .auto_spread_plan
                .as_ref()
                .and_then(|plan| {
                    plan.pages_for_logical_page(start_page)
                        .map(|(_, second)| second.is_some())
                })
                .unwrap_or(false);
        }
    }

    /// 現在の物理ナビゲーションページから working-set の起点を決める。
    pub(super) fn interactive_request_anchor_page(&self) -> WorkingSetAnchorPage {
        match self.persistent.spread_setting {
            SpreadMode::Single => WorkingSetAnchorPage::Single {
                requested_page: self.persistent.requested_page,
            },
            SpreadMode::Spread => {
                let navigation_page = self
                    .navigation_base_page()
                    .min(self.persistent.page_count.saturating_sub(1));
                WorkingSetAnchorPage::Spread { navigation_page }
            }
            SpreadMode::Auto => WorkingSetAnchorPage::Auto {
                navigation_page: self
                    .navigation_base_page()
                    .min(self.persistent.page_count.saturating_sub(1)),
            },
        }
    }

    pub(super) fn interactive_request_direction(&self) -> Direction {
        Direction::from_nav_delta(self.request.prefetch_dir).unwrap_or(Direction::Forward)
    }

    fn interactive_request_cache_key(
        &self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> InteractiveRequestCacheKey {
        InteractiveRequestCacheKey {
            entry_id: self.persistent.entry.id.clone(),
            page_count: self.persistent.page_count,
            spread_mode: spread_mode_tag(self.persistent.spread_setting.clone()),
            quality: self.request.quality,
            display_w,
            display_h,
            max_tex_side,
            working_set_anchor_page: self.interactive_request_anchor_page().navigation_page(),
            direction: self.interactive_request_direction(),
        }
    }

    fn cached_interactive_request_pages(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> WorkingSetPlan {
        let key = self.interactive_request_cache_key(display_w, display_h, max_tex_side);
        if let Some(cache) = &self.request.interactive_request_plan_cache {
            if cache.key == key {
                return cache.plan.clone();
            }
        }
        let plan = self.compute_interactive_request_pages(display_w, display_h, max_tex_side);
        self.request.interactive_request_plan_cache = Some(InteractiveRequestPlanCache {
            key,
            plan: plan.clone(),
        });
        plan
    }

    fn interactive_request_pages(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> HashSet<u32> {
        self.cached_interactive_request_pages(display_w, display_h, max_tex_side)
            .pages()
            .iter()
            .map(|page| page.page)
            .collect()
    }

    fn compute_interactive_request_pages(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> WorkingSetPlan {
        // 容量判定は decode 後の実RGBAサイズで行う。interactive request 生成時に予測byteで切らない。
        self.ensure_bg_admission_requirement(
            self.display_requirement_for_request(display_w, display_h, max_tex_side),
            "bg-admission-render-change",
        );
        let anchor_page = self.interactive_request_anchor_page();
        let direction = self.interactive_request_direction();
        let page_count = self.persistent.page_count;
        let mut plan = WorkingSetPlan::new(anchor_page, direction);
        let mut candidate_scan_count = 0usize;
        let mut seen_pages: HashSet<u32> = HashSet::new();
        let mut push_candidate = |candidate_page: u32| {
            candidate_scan_count = candidate_scan_count.saturating_add(1);
            if !seen_pages.insert(candidate_page) {
                return;
            }
            plan.push(WorkingSetPage {
                page: candidate_page,
            });
        };

        push_candidate(anchor_page.navigation_page());

        for step_index in 0..INTERACTIVE_DISPLAY_CANDIDATE_LIMIT {
            let offset = direction.signed_offset(step_index);
            let candidate_page_i64 = i64::from(anchor_page.navigation_page()) + offset;
            if candidate_page_i64 < 0 || candidate_page_i64 >= i64::from(page_count) {
                continue;
            }
            push_candidate(candidate_page_i64 as u32);
        }

        tracing::trace!(
            "[interactive_request_plan] anchor_page={} direction={} page_count={} candidate_scan_count={} planned_pages={} limit={}",
            anchor_page.navigation_page(),
            direction.as_str(),
            self.persistent.page_count,
            candidate_scan_count,
            plan.page_count(),
            INTERACTIVE_DISPLAY_CANDIDATE_LIMIT
        );

        plan
    }

    pub(super) fn update_interactive_rgba_cache_budget(
        &mut self,
        target_w: u32,
        target_h: u32,
        max_tex_side: u32,
    ) {
        let prev_applied_max_bytes = self.display_assets.interactive_rgba_cache.max_bytes;
        let interactive_request_pages =
            self.interactive_request_pages(target_w, target_h, max_tex_side);
        let page_bytes = target_w as usize * target_h as usize * 4;
        let cache_pages = interactive_request_pages.len().max(1);
        let calculated = page_bytes
            .saturating_mul(cache_pages)
            .saturating_mul(RGBA_CACHE_HEADROOM_NUM)
            / RGBA_CACHE_HEADROOM_DEN;
        let configured_max_bytes = (self.request.rgba_cache_max_mb as usize)
            .saturating_mul(1024)
            .saturating_mul(1024);
        let max_bytes = configured_max_bytes;
        let log_signature = (
            configured_max_bytes,
            calculated,
            max_bytes,
            cache_pages as u32,
            page_bytes,
            target_w,
            target_h,
        );
        let should_log_config =
            self.display_assets.last_interactive_rgba_cache_config_log != Some(log_signature);
        if should_log_config {
            tracing::trace!(
                "[viewer-interactive-rgba-cache-config] target_w={} target_h={} page_bytes={} interactive_request_pages={} calculated_max_bytes={} configured_max_bytes={} prev_applied_max_bytes={} applied_max_bytes={} changed={}",
                target_w,
                target_h,
                page_bytes,
                cache_pages,
                calculated,
                configured_max_bytes,
                prev_applied_max_bytes,
                max_bytes,
                prev_applied_max_bytes != max_bytes
            );
            self.display_assets.last_interactive_rgba_cache_config_log = Some(log_signature);
        }
        let shrunk = max_bytes < prev_applied_max_bytes;
        if should_log_config || prev_applied_max_bytes != max_bytes {
            tracing::trace!(
                "[viewer-interactive-cache-budget-update] previous_max_bytes={} new_max_bytes={} configured_max_bytes={} calculated_max_bytes={} target={}x{} interactive_request_pages={} shrunk={}",
                prev_applied_max_bytes,
                max_bytes,
                configured_max_bytes,
                calculated,
                target_w,
                target_h,
                cache_pages,
                shrunk
            );
        }
        if prev_applied_max_bytes != max_bytes {
            self.clear_bg_admission_state("rgba-cache-capacity-change");
        }
        let _ = self
            .display_assets
            .interactive_rgba_cache
            .set_max_bytes_with_context(
                max_bytes,
                self.persistent.requested_page,
                &interactive_request_pages,
            );
    }

    pub(super) fn rgba_cache_key(
        &self,
        page: u32,
        target_w: u32,
        target_h: u32,
        max_tex_side: u32,
    ) -> RgbaCacheKey {
        self.rgba_cache_key_with_signature(
            page,
            self.render_signature_for_decode(target_w, target_h, max_tex_side),
        )
    }

    pub(super) fn rgba_cache_key_with_quality(
        &self,
        page: u32,
        target_w: u32,
        target_h: u32,
        quality: &ViewerQuality,
        max_tex_side: u32,
    ) -> RgbaCacheKey {
        self.rgba_cache_key_with_signature(
            page,
            RenderSignature::from_decode_request(*quality, target_w, target_h, max_tex_side),
        )
    }

    fn cache_interactive_rgba_pages(&mut self, insert: InteractiveRgbaCacheInsertContext<'_>) {
        let InteractiveRgbaCacheInsertContext {
            request_kind,
            pages,
            left,
            right,
            target_w,
            target_h,
            quality,
            max_tex_side,
        } = insert;
        let current_requested_page = self.persistent.requested_page;
        let interactive_request_pages =
            self.interactive_request_pages(target_w, target_h, max_tex_side);
        let cache_event_label = "interactive_rgba.insert";
        let cache_field_prefix = "interactive_rgba";
        if let (Some(page), Some(frames)) = (pages.left_page, left) {
            let key =
                self.rgba_cache_key_with_quality(page, target_w, target_h, quality, max_tex_side);
            let render_signature = key.render_signature;
            let inserted = self.display_assets.interactive_rgba_cache.insert(
                key,
                Arc::clone(frames),
                request_kind,
                current_requested_page,
                &interactive_request_pages,
            );
            if inserted {
                tracing::trace!(
                    "[{}] page={} {}.current_bytes={} {}.max_bytes={} {}.entry_count={} render_signature.quality={:?} render_signature.target_w={} render_signature.target_h={} render_signature.max_tex_side={}",
                    cache_event_label,
                    page,
                    cache_field_prefix,
                    self.display_assets.interactive_rgba_cache.current_bytes(),
                    cache_field_prefix,
                    self.display_assets.interactive_rgba_cache.max_bytes(),
                    cache_field_prefix,
                    self.display_assets.interactive_rgba_cache.entry_count(),
                    render_signature.quality,
                    render_signature.target_w,
                    render_signature.target_h,
                    render_signature.max_tex_side
                );
            }
        }
        if let (Some(page), Some(frames)) = (pages.right_page, right) {
            let key =
                self.rgba_cache_key_with_quality(page, target_w, target_h, quality, max_tex_side);
            let render_signature = key.render_signature;
            let inserted = self.display_assets.interactive_rgba_cache.insert(
                key,
                Arc::clone(frames),
                request_kind,
                current_requested_page,
                &interactive_request_pages,
            );
            if inserted {
                tracing::trace!(
                    "[{}] page={} {}.current_bytes={} {}.max_bytes={} {}.entry_count={} render_signature.quality={:?} render_signature.target_w={} render_signature.target_h={} render_signature.max_tex_side={}",
                    cache_event_label,
                    page,
                    cache_field_prefix,
                    self.display_assets.interactive_rgba_cache.current_bytes(),
                    cache_field_prefix,
                    self.display_assets.interactive_rgba_cache.max_bytes(),
                    cache_field_prefix,
                    self.display_assets.interactive_rgba_cache.entry_count(),
                    render_signature.quality,
                    render_signature.target_w,
                    render_signature.target_h,
                    render_signature.max_tex_side
                );
            }
        }
    }

    fn try_apply_display_commit(&mut self, request: PartialDisplayReuseContext<'_>) -> bool {
        let PartialDisplayReuseContext {
            nav_id,
            physical_page,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
            display_w,
            display_h,
            ctx,
            left_hit: _,
            right_hit: _,
        } = request;
        let view_idx = physical_page;
        let (left_state, left_frames) = self.resolve_display_page_state(
            page_left,
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        let (right_state, right_frames) = self.resolve_display_page_state(
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        let left_ready = page_left.is_none() || matches!(left_state, DisplayPageState::Ready);
        let right_ready = page_right.is_none() || matches!(right_state, DisplayPageState::Ready);
        if !left_ready || !right_ready {
            tracing::trace!(
                "[viewer-cache-apply-skip] reason=cache_miss nav_id={} view={} page_left={:?} page_right={:?} target_w={} target_h={} max_tex_side={} left_ready={} right_ready={}",
                nav_id,
                view_idx,
                page_left,
                page_right,
                request_display_w,
                request_display_h,
                max_tex_side,
                left_ready,
                right_ready
            );
            return false;
        }

        tracing::trace!(
            "[interactive_commit.done] nav_id={} view={} page_left={:?} page_right={:?} render_signature.quality={:?} render_signature.target_w={} render_signature.target_h={} render_signature.max_tex_side={}",
            nav_id,
            view_idx,
            page_left,
            page_right,
            self.request.quality,
            request_display_w,
            request_display_h,
            max_tex_side
        );
        let result = self.make_synthetic_display_result(SyntheticDisplayRequest {
            nav_id,
            physical_page: view_idx,
            page_left,
            page_right,
            left: left_frames,
            right: right_frames,
            request_display_w,
            request_display_h,
            request_quality: self.request.quality,
            request_max_tex_side: max_tex_side,
        });
        let _ = self.apply_display_result(
            result,
            ctx,
            Instant::now(),
            display_w,
            display_h,
            max_tex_side,
        );
        true
    }

    fn try_apply_partial_display_reuse_commit(
        &mut self,
        request: PartialDisplayReuseContext<'_>,
    ) -> bool {
        let PartialDisplayReuseContext {
            nav_id,
            physical_page,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
            display_w,
            display_h,
            ctx,
            left_hit,
            right_hit,
        } = request;
        let view_idx = physical_page;
        let Some(reused_hit) = left_hit.or(right_hit) else {
            return false;
        };
        let reused_is_left = left_hit.is_some();
        let uploaded_page = if reused_is_left {
            page_right
        } else {
            page_left
        };
        let Some(uploaded_page) = uploaded_page else {
            return false;
        };
        let (uploaded_state, uploaded_frames) = self.resolve_display_page_state(
            Some(uploaded_page),
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        if !matches!(uploaded_state, DisplayPageState::Ready) {
            return false;
        }
        let Some(uploaded_frames) = uploaded_frames else {
            return false;
        };

        let upload_started = Instant::now();
        tracing::trace!(
            frame = self.ui_runtime.show_seq,
            request_id = nav_id,
            page = self.persistent.requested_page,
            at_ms = now_ms(),
            "viewer-texture: upload start"
        );
        let result = self.make_synthetic_display_result(SyntheticDisplayRequest {
            nav_id,
            physical_page: view_idx,
            page_left,
            page_right,
            left: None,
            right: None,
            request_display_w,
            request_display_h,
            request_quality: self.request.quality,
            request_max_tex_side: max_tex_side,
        });
        let (left_content, right_content) = if reused_is_left {
            (
                Some(PageContent::Static(reused_hit.texture.clone())),
                Some(PageContent::from_frames(
                    uploaded_frames,
                    "viewer_right",
                    ctx,
                )),
            )
        } else {
            (
                Some(PageContent::from_frames(
                    uploaded_frames,
                    "viewer_left",
                    ctx,
                )),
                Some(PageContent::Static(reused_hit.texture.clone())),
            )
        };
        let committed = self.commit_display_contents(DisplayCommitContext {
            result: &result,
            upload_started,
            ctx,
            poll_started: Instant::now(),
            display_w,
            display_h,
            max_tex_side,
            gpu_history_hit: false,
            left: DisplayCommitSlot {
                page: page_left,
                content: left_content,
                hit: if reused_is_left {
                    Some(reused_hit)
                } else {
                    None
                },
                register_gpu_history: !reused_is_left,
            },
            right: DisplayCommitSlot {
                page: page_right,
                content: right_content,
                hit: if reused_is_left {
                    None
                } else {
                    Some(reused_hit)
                },
                register_gpu_history: reused_is_left,
            },
        });
        if !committed {
            return false;
        }

        self.display_assets
            .gpu_texture_history
            .record_partial_reuse();
        tracing::trace!(
            view_page = view_idx,
            left_page = ?page_left,
            right_page = ?page_right,
            reused_page = reused_hit.key.page,
            uploaded_page,
            reason = "cpu_rgba_ready",
            current_mb = %Self::format_bytes_mb(
                self.display_assets.gpu_texture_history.current_bytes()
            ),
            max_mb = %Self::format_bytes_mb(self.display_assets.gpu_texture_history.max_bytes()),
            entries = self.display_assets.gpu_texture_history.entry_count(),
            "gpu-history-partial-reuse"
        );
        true
    }

    fn start_partial_display_request(&mut self, request: PartialDisplayRequest) -> bool {
        let PartialDisplayRequest {
            nav_id,
            physical_page,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
        } = request;
        let view_idx = physical_page;
        let (left_state, left_frames) = self.resolve_display_page_state(
            page_left,
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        let (right_state, right_frames) = self.resolve_display_page_state(
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        if matches!(left_state, DisplayPageState::Missing)
            && matches!(right_state, DisplayPageState::Missing)
        {
            return false;
        }

        self.request.interactive_generation = self.request.interactive_generation.saturating_add(1);
        let generation = self.request.interactive_generation;
        let group_id = nav_id;
        let mut left_request_id = None;
        let mut right_request_id = None;
        let mut left_result = None;
        let mut right_result = None;

        match (page_left, left_state) {
            (Some(_page), DisplayPageState::Ready) => {
                left_result = Some(self.make_synthetic_display_result(SyntheticDisplayRequest {
                    nav_id,
                    physical_page: view_idx,
                    page_left,
                    page_right,
                    left: left_frames,
                    right: None,
                    request_display_w,
                    request_display_h,
                    request_quality: self.request.quality,
                    request_max_tex_side: max_tex_side,
                }));
            }
            (Some(page), DisplayPageState::Missing) => {
                let req = self.request.loader.send_request(ViewerLoadRequest {
                    path: Arc::clone(&self.persistent.entry.path),
                    view_idx,
                    page_left: Some(page),
                    page_right: None,
                    display_w: request_display_w,
                    display_h: request_display_h,
                    quality: self.request.quality,
                    max_tex_side,
                    frame_cache_cap: self.frame_cache_cap(),
                    nav_id,
                    interactive: true,
                });
                left_request_id = Some(req);
                tracing::trace!(
                    "[interactive_decode.start] group_id={} generation={} request_id={} page={} side=left worker={} nav_id={}",
                    group_id,
                    generation,
                    req,
                    page,
                    if page % 2 == 0 {
                        "interactive-even"
                    } else {
                        "interactive-odd"
                    },
                    nav_id
                );
            }
            _ => {}
        }
        match (page_right, right_state) {
            (Some(_page), DisplayPageState::Ready) => {
                right_result = Some(self.make_synthetic_display_result(SyntheticDisplayRequest {
                    nav_id,
                    physical_page: view_idx,
                    page_left,
                    page_right,
                    left: None,
                    right: right_frames,
                    request_display_w,
                    request_display_h,
                    request_quality: self.request.quality,
                    request_max_tex_side: max_tex_side,
                }));
            }
            (Some(page), DisplayPageState::Missing) => {
                let req = self.request.loader.send_request(ViewerLoadRequest {
                    path: Arc::clone(&self.persistent.entry.path),
                    view_idx,
                    page_left: Some(page),
                    page_right: None,
                    display_w: request_display_w,
                    display_h: request_display_h,
                    quality: self.request.quality,
                    max_tex_side,
                    frame_cache_cap: self.frame_cache_cap(),
                    nav_id,
                    interactive: true,
                });
                right_request_id = Some(req);
                tracing::trace!(
                    "[interactive_decode.start] group_id={} generation={} request_id={} page={} side=right worker={} nav_id={}",
                    group_id,
                    generation,
                    req,
                    page,
                    if page % 2 == 0 {
                        "interactive-even"
                    } else {
                        "interactive-odd"
                    },
                    nav_id
                );
            }
            _ => {}
        }

        if left_request_id.is_none()
            && right_request_id.is_none()
            && left_result.is_none()
            && right_result.is_none()
        {
            return false;
        }

        self.request.pending_id = left_request_id.or(right_request_id).unwrap_or(nav_id);
        self.request.pending_id_aux = match (left_request_id, right_request_id) {
            (Some(l), Some(r)) if l != r => Some(r),
            _ => None,
        };
        self.request.interactive_inflight_even_page = [page_left, page_right]
            .into_iter()
            .flatten()
            .find(|page| page % 2 == 0);
        self.request.interactive_inflight_odd_page = [page_left, page_right]
            .into_iter()
            .flatten()
            .find(|page| page % 2 == 1);
        self.request.pending_interactive_group = Some(InteractivePendingGroup {
            group_id,
            generation,
            page_left,
            page_right,
            left_request_id,
            right_request_id,
            left_result,
            right_result,
        });
        true
    }

    pub fn load_view(
        &mut self,
        physical_page: u32,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
    ) {
        let nav_id = self.begin_nav(self.nav_target(), physical_page, "load_view");
        self.start_view_request(ViewRequestContext {
            nav_id,
            physical_page,
            display_w,
            display_h,
            max_tex_side,
            ctx,
            reason: "load_view",
        });
    }

    pub(super) fn nav_target(&self) -> u32 {
        self.persistent.target_page
    }

    pub(super) fn register_nav_input(&mut self, now: Instant) {
        // 短時間連打を 1 系列として扱い、入力の性質を切り替える。
        self.ui_runtime.nav_consecutive_count = match self.ui_runtime.last_nav_input_at {
            Some(last) if now.saturating_duration_since(last) <= NAV_CONSECUTIVE_WINDOW => {
                self.ui_runtime.nav_consecutive_count.saturating_add(1)
            }
            _ => 1,
        };
        self.ui_runtime.nav_mode = match self.ui_runtime.last_nav_input_at {
            Some(last) if now.saturating_duration_since(last) <= FOLLOW_LATEST_THRESHOLD => {
                NavMode::FollowLatest
            }
            _ => NavMode::Sequential,
        };
        self.ui_runtime.last_nav_input_at = Some(now);
    }

    pub(super) fn navigation_base_page(&self) -> u32 {
        match self.ui_runtime.nav_mode {
            NavMode::FollowLatest => self.nav_target(),
            NavMode::Sequential => {
                if self.ui_runtime.loading {
                    self.persistent.requested_page
                } else {
                    self.persistent.displayed_page
                }
            }
        }
    }

    pub(super) fn boundary_preview_enabled(
        &self,
        allow_book_navigation: bool,
        is_fullscreen: bool,
    ) -> bool {
        allow_book_navigation && !is_fullscreen && !self.slideshow_active()
    }

    pub(super) fn boundary_preview_visible(
        &self,
        allow_book_navigation: bool,
        is_fullscreen: bool,
    ) -> bool {
        self.boundary_preview_enabled(allow_book_navigation, is_fullscreen)
            && self.boundary_preview_ready_view().is_some()
    }

    pub(super) fn boundary_preview_can_trigger(&self, direction: BoundaryPreviewDirection) -> bool {
        if self.persistent.page_count == 0 {
            return false;
        }
        let base = self.navigation_base_page();
        let (page_left, page_right) = self.current_view_pages(base);
        let first_visible = page_left.or(page_right);
        let last_visible = page_right.or(page_left);
        match direction {
            BoundaryPreviewDirection::Previous => first_visible == Some(0),
            BoundaryPreviewDirection::Next => last_visible == Some(self.persistent.page_count - 1),
        }
    }

    pub(super) fn begin_boundary_preview(&mut self, direction: BoundaryPreviewDirection) {
        match &mut self.boundary_preview {
            BoundaryPreviewState::Hidden => {
                self.boundary_preview = Self::boundary_preview_loading(direction);
            }
            BoundaryPreviewState::Loading(probe) => {
                if probe.direction != direction {
                    self.boundary_preview = Self::boundary_preview_loading(direction);
                }
            }
            BoundaryPreviewState::Ready { probe, .. } => {
                if probe.direction != direction {
                    self.boundary_preview = Self::boundary_preview_loading(direction);
                }
            }
        }
    }

    pub fn boundary_preview_needs_request(&self) -> bool {
        matches!(
            &self.boundary_preview,
            BoundaryPreviewState::Loading(BoundaryPreviewProbe {
                in_flight: false,
                request_id: None,
                ..
            })
        )
    }

    pub fn boundary_preview_mark_request_sent(&mut self, request_id: u64) -> bool {
        let BoundaryPreviewState::Loading(probe) = &mut self.boundary_preview else {
            return false;
        };
        if probe.request_id.is_some() {
            return false;
        }
        probe.request_id = Some(request_id);
        probe.in_flight = true;
        true
    }

    pub fn boundary_preview_direction_for_request(
        &self,
        request_id: u64,
    ) -> Option<BoundaryPreviewDirection> {
        match &self.boundary_preview {
            BoundaryPreviewState::Loading(probe) if probe.request_id == Some(request_id) => {
                Some(probe.direction)
            }
            _ => None,
        }
    }

    pub fn boundary_preview_mark_ready(&mut self, request_id: u64, book: BookMeta) -> bool {
        let BoundaryPreviewState::Loading(probe) = &mut self.boundary_preview else {
            return false;
        };
        if probe.request_id != Some(request_id) {
            return false;
        }
        let direction = probe.direction;
        self.boundary_preview = BoundaryPreviewState::Ready {
            probe: BoundaryPreviewProbe {
                direction,
                in_flight: false,
                request_id: Some(request_id),
            },
            book,
            thumbnail: None,
        };
        true
    }

    pub fn boundary_preview_set_thumbnail(
        &mut self,
        request_id: u64,
        thumbnail: LoadedDiskThumb,
    ) -> bool {
        let BoundaryPreviewState::Ready {
            probe,
            thumbnail: slot,
            ..
        } = &mut self.boundary_preview
        else {
            return false;
        };
        if probe.request_id != Some(request_id) {
            return false;
        }
        *slot = Some(thumbnail);
        true
    }

    pub fn boundary_preview_clear_if_matches(&mut self, request_id: u64) -> bool {
        let matches_request = match &self.boundary_preview {
            BoundaryPreviewState::Loading(probe) => probe.request_id == Some(request_id),
            BoundaryPreviewState::Ready { probe, .. } => probe.request_id == Some(request_id),
            BoundaryPreviewState::Hidden => false,
        };
        if matches_request {
            self.boundary_preview = BoundaryPreviewState::Hidden;
        }
        matches_request
    }

    pub fn boundary_preview_ready_book(&self) -> Option<&BookMeta> {
        match &self.boundary_preview {
            BoundaryPreviewState::Ready { book, .. } => Some(book),
            _ => None,
        }
    }

    pub(super) fn boundary_preview_ready_view(&self) -> Option<BoundaryPreviewReadyView<'_>> {
        match &self.boundary_preview {
            BoundaryPreviewState::Ready {
                probe,
                book,
                thumbnail: Some(thumbnail),
            } => Some(BoundaryPreviewReadyView {
                direction: probe.direction,
                book,
                thumbnail,
            }),
            _ => None,
        }
    }

    pub fn boundary_preview_clear(&mut self) {
        self.boundary_preview = BoundaryPreviewState::Hidden;
    }

    pub fn close_boundary_preview(&mut self) {
        self.boundary_preview_clear();
    }

    pub fn close_boundary_preview_on_successful_page_move(&mut self, moved: bool) {
        if moved {
            self.boundary_preview_clear();
        }
    }

    pub(super) fn show_follow_placeholder(&self) -> bool {
        self.ui_runtime.pending_placeholder_latched
    }

    pub(super) fn pending_placeholder_candidate(&self, now: Instant) -> bool {
        if !(self.ui_runtime.loading
            && self.has_pending_target()
            && !self.suppress_pending_for_animation())
        {
            return false;
        }
        // 長押し相当（N>=2）はゲートを迂回して即プレースホルダ表示。
        if self.ui_runtime.nav_consecutive_count >= 2 {
            return true;
        }
        // 単発/疎な連続入力は gate 到達後にプレースホルダ表示。
        self.ui_runtime
            .pending_placeholder_after
            .is_none_or(|at| now >= at)
    }

    pub(super) fn has_pending_target(&self) -> bool {
        self.persistent.target_page != self.persistent.displayed_page
    }

    pub(super) fn most_advanced_physical_page(&self) -> u32 {
        self.persistent
            .target_page
            .max(self.persistent.requested_page)
            .max(self.persistent.displayed_page)
    }

    pub(super) fn target_physical_page_for_snapshot(&self) -> u32 {
        self.persistent.target_page
    }

    pub(super) fn make_spread_snapshot_key(&self, physical_page: u32) -> SpreadSnapshotKey {
        SpreadSnapshotKey {
            entry_id: self.persistent.entry.id.clone(),
            page_count: self.persistent.page_count,
            spread_setting: self.persistent.spread_setting.clone(),
            cover_blank: self.persistent.cover_blank,
            physical_page,
        }
    }

    pub(super) fn is_last_page_for_physical_page(
        &self,
        physical_page: u32,
        _current_spread: bool,
        current_page_right: Option<u32>,
    ) -> bool {
        if self.persistent.page_count == 0 || physical_page != self.most_advanced_physical_page() {
            return false;
        }
        if let Some(page) = current_page_right {
            return page + 1 >= self.persistent.page_count;
        }
        Some(physical_page).is_some_and(|page| page + 1 >= self.persistent.page_count)
    }

    pub(super) fn resolved_spread_for_physical_page(
        &mut self,
        physical_page: u32,
        current_spread: bool,
        current_page_right: Option<u32>,
        allow_snapshot_update: bool,
    ) -> (bool, Option<u32>) {
        let pending_target = self.has_pending_target();
        let target_physical_page = self.target_physical_page_for_snapshot();
        let current_key = self.make_spread_snapshot_key(target_physical_page);
        if pending_target
            && self.persistent.spread_snapshot.valid
            && self.persistent.spread_snapshot.key == current_key
            && physical_page == self.persistent.spread_snapshot.key.physical_page
        {
            tracing::trace!(
                "[viewer-spread-snapshot] action=use frame={} physical_page={} target_physical_page={} pending_target={} snapshot_spread={} snapshot_page_right={:?}",
                self.ui_runtime.show_seq,
                physical_page,
                target_physical_page,
                pending_target,
                self.persistent.spread_snapshot.effective_spread,
                self.persistent.spread_snapshot.page_right
            );
            return (
                self.persistent.spread_snapshot.effective_spread,
                self.persistent.spread_snapshot.page_right,
            );
        }
        if physical_page != target_physical_page {
            return (current_spread, current_page_right);
        }

        let composition_is_resolved = current_page_right.is_some()
            || self.is_last_page_for_physical_page(
                physical_page,
                current_spread,
                current_page_right,
            )
            || self.is_leading_cover_blank_spread(physical_page);
        let is_snapshot_key_mismatch = !self.persistent.spread_snapshot.valid
            || self.persistent.spread_snapshot.key != current_key;
        if is_snapshot_key_mismatch {
            if composition_is_resolved {
                self.persistent.spread_snapshot = SpreadSnapshot {
                    key: current_key,
                    effective_spread: current_spread,
                    page_right: current_page_right,
                    valid: true,
                };
                tracing::trace!(
                    "[viewer-spread-snapshot] action=re-init frame={} physical_page={} target_physical_page={} pending_target={} displayed={} requested={} target={} composition_is_resolved={} is_last_page={} snapshot_spread={} snapshot_page_right={:?} snapshot_valid=true",
                    self.ui_runtime.show_seq,
                    physical_page,
                    target_physical_page,
                    pending_target,
                    self.persistent.displayed_page,
                    self.persistent.requested_page,
                    self.persistent.target_page,
                    composition_is_resolved,
                    self.is_last_page_for_physical_page(
                        physical_page,
                        current_spread,
                        current_page_right,
                    ),
                    current_spread,
                    current_page_right
                );
            }
            return (current_spread, current_page_right);
        }

        let should_update = allow_snapshot_update
            && !pending_target
            && self.persistent.displayed_page == self.persistent.requested_page
            && self.persistent.displayed_page == self.persistent.target_page
            && composition_is_resolved;
        if should_update {
            if self.persistent.spread_snapshot.valid
                && self.persistent.spread_snapshot.key == current_key
                && self.persistent.spread_snapshot.effective_spread == current_spread
                && self.persistent.spread_snapshot.page_right == current_page_right
            {
                return (current_spread, current_page_right);
            }
            self.persistent.spread_snapshot = SpreadSnapshot {
                key: current_key,
                effective_spread: current_spread,
                page_right: current_page_right,
                valid: true,
            };
            tracing::trace!(
                "[viewer-spread-snapshot] action=update frame={} physical_page={} target_physical_page={} pending_target={} displayed={} requested={} target={} composition_is_resolved={} is_last_page={} snapshot_spread={} snapshot_page_right={:?} snapshot_valid=true",
                self.ui_runtime.show_seq,
                physical_page,
                target_physical_page,
                pending_target,
                self.persistent.displayed_page,
                self.persistent.requested_page,
                self.persistent.target_page,
                composition_is_resolved,
                self.is_last_page_for_physical_page(
                    physical_page,
                    current_spread,
                    current_page_right,
                ),
                current_spread,
                current_page_right
            );
        }

        (current_spread, current_page_right)
    }

    pub(super) fn update_pending_progress_state(&mut self, now: Instant) {
        let pending_now = self.has_pending_target();
        match (pending_now, self.ui_runtime.pending_started_at) {
            (true, None) => {
                self.ui_runtime.pending_started_at = Some(now);
                tracing::trace!(
                    "[viewer-pending] action=start target_page={} displayed_page={} requested_page={}",
                    self.persistent.target_page,
                    self.persistent.displayed_page,
                    self.persistent.requested_page
                );
            }
            (false, Some(started_at)) => {
                tracing::trace!(
                    "[viewer-pending] action=end duration_ms={} target_page={} displayed_page={} requested_page={}",
                    now.saturating_duration_since(started_at).as_millis(),
                    self.persistent.target_page,
                    self.persistent.displayed_page,
                    self.persistent.requested_page
                );
                self.ui_runtime.pending_started_at = None;
            }
            _ => {}
        }

        let show_pending_candidate = self.pending_placeholder_candidate(now);
        if !pending_now {
            self.ui_runtime.pending_placeholder_latched = false;
        } else if show_pending_candidate {
            self.ui_runtime.pending_placeholder_latched = true;
        }
        let pending_visible_now = self.ui_runtime.pending_placeholder_latched;
        if self.ui_runtime.pending_visible_last != Some(pending_visible_now) {
            tracing::trace!(
                "[viewer-pending] action=visible-change pending_visible={} pending_target={} target_page={} displayed_page={} requested_page={}",
                pending_visible_now,
                pending_now,
                self.persistent.target_page,
                self.persistent.displayed_page,
                self.persistent.requested_page
            );
            self.ui_runtime.pending_visible_last = Some(pending_visible_now);
        }
    }

    pub(super) fn pending_visual_state_for_progress(
        &self,
        show_pending: bool,
        progress_hover: bool,
        progress_drag: bool,
        drag_fraction_milli: Option<u16>,
    ) -> PendingVisualState {
        PendingVisualState {
            target_page: self.persistent.target_page,
            displayed_page: self.persistent.displayed_page,
            requested_page: self.persistent.requested_page,
            show_pending,
            progress_hover,
            progress_drag,
            drag_fraction_milli,
        }
    }

    pub(super) fn update_pending_visual_state(&mut self, current: PendingVisualState) -> bool {
        if self.ui_runtime.last_pending_visual_state == current {
            return false;
        }
        self.ui_runtime.last_pending_visual_state = current;
        true
    }

    pub(super) fn pending_display_state(&self, show_pending: bool) -> PendingDisplayState {
        PendingDisplayState {
            target_page: self.persistent.target_page,
            displayed_page: self.persistent.displayed_page,
            requested_page: self.persistent.requested_page,
            show_pending,
        }
    }

    pub(super) fn update_pending_display_state(&mut self, current: PendingDisplayState) -> bool {
        if self.ui_runtime.last_pending_display_state == Some(current) {
            return false;
        }
        self.ui_runtime.last_pending_display_state = Some(current);
        true
    }

    pub(super) fn suppress_pending_for_animation(&self) -> bool {
        if self.request.active_animation_stream_view.is_some() {
            return true;
        }
        let left_anim = self.display_assets.content_left.as_ref().is_some_and(|c| {
            matches!(
                c,
                PageContent::AnimatedReady { .. } | PageContent::AnimatedStream { .. }
            )
        });
        let right_anim = self.display_assets.content_right.as_ref().is_some_and(|c| {
            matches!(
                c,
                PageContent::AnimatedReady { .. } | PageContent::AnimatedStream { .. }
            )
        });
        left_anim || right_anim
    }

    pub(super) fn active_animation_view(&self) -> u32 {
        self.persistent.displayed_page
    }

    pub(super) fn clear_animation_stream_state(&mut self) {
        self.request.active_animation_stream_view = None;
        self.request.animation_stream_request_id = None;
    }

    pub(super) fn invalidate_spread_snapshot(&mut self) {
        if self.persistent.spread_snapshot.valid {
            tracing::trace!(
                "[viewer-spread-snapshot] action=invalidate physical_page={} snapshot_physical_page={} snapshot_spread={} snapshot_page_right={:?}",
                self.persistent.requested_page,
                self.persistent.spread_snapshot.key.physical_page,
                self.persistent.spread_snapshot.effective_spread,
                self.persistent.spread_snapshot.page_right
            );
        }
        self.persistent.spread_snapshot.valid = false;
    }

    fn adopt_page_count_if_empty(&mut self, page_count: u32, source: &'static str) -> bool {
        if self.persistent.page_count != 0 || page_count == 0 {
            return false;
        }

        self.persistent.page_count = page_count;
        let last_page = page_count.saturating_sub(1);
        let requested_page = self.persistent.requested_page.min(last_page);
        let displayed_page = self.persistent.displayed_page.min(last_page);
        let target_page = self.persistent.target_page.min(last_page);
        let prefetch_anchor_view = self.request.prefetch_anchor_view.min(last_page);
        let clamped = requested_page != self.persistent.requested_page
            || displayed_page != self.persistent.displayed_page
            || target_page != self.persistent.target_page
            || prefetch_anchor_view != self.request.prefetch_anchor_view;
        self.persistent.requested_page = requested_page;
        self.persistent.displayed_page = displayed_page;
        self.persistent.target_page = target_page;
        self.request.prefetch_anchor_view = prefetch_anchor_view;
        self.invalidate_spread_snapshot();
        tracing::info!(
            page_count,
            source,
            clamped,
            requested_page,
            displayed_page,
            target_page,
            "viewer.page_count_adopt"
        );
        true
    }

    fn auto_plan(&self) -> Option<&AutoSpreadPlan> {
        self.persistent.auto_spread_plan.as_deref()
    }

    fn auto_mode_available(&self) -> bool {
        self.auto_plan().is_some()
    }

    fn auto_next_anchor(&self, physical_page_or_anchor: u32) -> Option<u32> {
        self.auto_plan()
            .and_then(|plan| plan.next_anchor(physical_page_or_anchor))
    }

    fn auto_previous_anchor(&self, physical_page_or_anchor: u32) -> Option<u32> {
        self.auto_plan()
            .and_then(|plan| plan.previous_anchor(physical_page_or_anchor))
    }

    fn display_pages_for_physical_page(&self, physical_page: u32) -> (Option<u32>, Option<u32>) {
        if physical_page >= self.persistent.page_count {
            return (None, None);
        }
        if self.is_leading_cover_blank_spread(physical_page) {
            return (Some(0), None);
        }
        match self.persistent.spread_setting {
            SpreadMode::Auto => self
                .auto_plan()
                .and_then(|plan| plan.pages_for_logical_page(physical_page))
                .map(|(first, second)| (Some(first), second))
                .unwrap_or((Some(physical_page), None)),
            SpreadMode::Spread => {
                if self.persistent.cover_blank && physical_page == 0 {
                    (Some(0), None)
                } else if physical_page + 1 < self.persistent.page_count {
                    (Some(physical_page), Some(physical_page + 1))
                } else {
                    (Some(physical_page), None)
                }
            }
            SpreadMode::Single => (Some(physical_page), None),
        }
    }

    pub(super) fn current_view_pages(&self, physical_page: u32) -> (Option<u32>, Option<u32>) {
        self.display_pages_for_physical_page(physical_page)
    }

    pub(super) fn request_view_pages(&self, physical_page: u32) -> (Option<u32>, Option<u32>) {
        if self.is_leading_cover_blank_spread(physical_page) {
            return (Some(0), None);
        }
        if self.persistent.page_count == 0 {
            return match self.persistent.spread_setting {
                SpreadMode::Auto => self
                    .auto_plan()
                    .and_then(|plan| plan.pages_for_logical_page(physical_page))
                    .map(|(first, second)| (Some(first), second))
                    .unwrap_or((Some(physical_page), None)),
                SpreadMode::Spread => {
                    if self.persistent.cover_blank && physical_page == 0 {
                        (Some(0), None)
                    } else {
                        (Some(physical_page), physical_page.checked_add(1))
                    }
                }
                SpreadMode::Single => (Some(physical_page), None),
            };
        }
        self.display_pages_for_physical_page(physical_page)
    }

    pub(super) fn view_layout_for_with_caller(
        &mut self,
        physical_page: u32,
        image_area_w: u32,
        image_area_h: u32,
        allow_snapshot_update: bool,
    ) -> ViewerViewLayout {
        let (page_left, current_page_right) = self.request_view_pages(physical_page);
        let current_spread =
            current_page_right.is_some() || self.is_leading_cover_blank_spread(physical_page);
        let (effective_spread, page_right) = self.resolved_spread_for_physical_page(
            physical_page,
            current_spread,
            current_page_right,
            allow_snapshot_update,
        );
        let page_display_w = request_display_width_for_pair(image_area_w, effective_spread);
        let (full_equivalent_area_w, full_equivalent_area_h, hint_source) =
            self.resolved_full_equivalent_area(image_area_w, image_area_h);
        let page_decode_w =
            request_display_width_for_pair(full_equivalent_area_w, effective_spread);
        ViewerViewLayout {
            physical_page,
            page_left,
            page_right,
            effective_spread,
            page_display_w,
            page_display_h: image_area_h,
            page_decode_w,
            page_decode_h: full_equivalent_area_h,
            image_area_w,
            image_area_h,
            full_equivalent_area_w,
            full_equivalent_area_h,
            hint_source,
        }
    }

    pub(super) fn log_view_layout(&self, reason: &'static str, layout: &ViewerViewLayout) {
        if reason == "current-draw" {
            return;
        }
        tracing::trace!(
            "[viewer-layout] reason={} current_page={} spread_mode={} effective_spread={} page_left={:?} page_right={:?} page_display_w={} page_display_h={} image_area_w={} image_area_h={}",
            reason,
            layout.physical_page,
            self.persistent.spread_mode,
            layout.effective_spread,
            layout.page_left,
            layout.page_right,
            layout.page_display_w,
            layout.page_display_h,
            layout.image_area_w,
            layout.image_area_h
        );
        tracing::trace!(
            "[viewer-layout-state] reason={} frame={} physical_page={} displayed_page={} requested_page={} target_page={} pending_target={} spread_setting={:?} spread_mode={} cover_blank={} page_count={}",
            reason,
            self.ui_runtime.show_seq,
            layout.physical_page,
            self.persistent.displayed_page,
            self.persistent.requested_page,
            self.persistent.target_page,
            self.has_pending_target(),
            self.persistent.spread_setting,
            self.persistent.spread_mode,
            self.persistent.cover_blank,
            self.persistent.page_count
        );
        tracing::trace!(
            "[viewer-decode-target] reason={} hint_source={} page_left={:?} page_right={:?} page_display_w={} page_display_h={} page_decode_w={} page_decode_h={} full_equivalent_area_w={} full_equivalent_area_h={}",
            reason,
            layout.hint_source,
            layout.page_left,
            layout.page_right,
            layout.page_display_w,
            layout.page_display_h,
            layout.page_decode_w,
            layout.page_decode_h,
            layout.full_equivalent_area_w,
            layout.full_equivalent_area_h
        );
    }

    pub(super) fn apply_animation_stream_chunk_result(
        &mut self,
        result: crate::infra::worker::viewer_loader::ViewerResult,
        ctx: &egui::Context,
    ) -> bool {
        self.request.animation_stream_request_id = None;
        if self.request.active_animation_stream_view != Some(self.active_animation_view())
            || self.ui_runtime.nav_mode == NavMode::FollowLatest
        {
            tracing::trace!(
                "[viewer-result-drop] nav_id={} req={} current_req={} view={} reason=stale_request worker={}",
                result.nav_id,
                result.request_id,
                self.request.pending_id,
                self.persistent.requested_page,
                result.worker
            );
            return false;
        }
        if let Some(frames) = result.left {
            match self.display_assets.content_left.as_mut() {
                Some(PageContent::AnimatedStream { .. }) => {
                    if let Some(content) = self.display_assets.content_left.as_mut() {
                        content.append_stream_chunk(frames, result.left_stream_exhausted);
                    }
                }
                _ => {
                    self.display_assets.content_left = Some(PageContent::from_stream_chunk(
                        frames,
                        result.left_stream_exhausted,
                        "viewer_left_stream",
                        ctx,
                    ));
                }
            }
        }
        if let Some(frames) = result.right {
            match self.display_assets.content_right.as_mut() {
                Some(PageContent::AnimatedStream { .. }) => {
                    if let Some(content) = self.display_assets.content_right.as_mut() {
                        content.append_stream_chunk(frames, result.right_stream_exhausted);
                    }
                }
                _ => {
                    self.display_assets.content_right = Some(PageContent::from_stream_chunk(
                        frames,
                        result.right_stream_exhausted,
                        "viewer_right_stream",
                        ctx,
                    ));
                }
            }
        }
        tracing::trace!(
            request_id = result.request_id,
            left_exhausted = result.left_stream_exhausted,
            right_exhausted = result.right_stream_exhausted,
            "viewer_ui: animation stream chunk applied"
        );
        true
    }

    pub(super) fn apply_display_result(
        &mut self,
        result: crate::infra::worker::viewer_loader::ViewerResult,
        ctx: &egui::Context,
        poll_started: Instant,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> bool {
        let in_transition = self.ui_runtime.viewport_transition_active
            || self.ui_runtime.fullscreen_transition_frames > 0;
        if self.transition_logs_active() {
            tracing::trace!(
                frame = self.ui_runtime.show_seq,
                request_id = result.request_id,
                display_w,
                display_h,
                left_frames = result.left.as_ref().map(|v| v.len()).unwrap_or(0),
                right_frames = result.right.as_ref().map(|v| v.len()).unwrap_or(0),
                left_stream = result.left_is_animation_stream,
                right_stream = result.right_is_animation_stream,
                in_transition,
                "viewer_ui: decode result apply begin"
            );
        }
        self.adopt_page_count_if_empty(result.page_count, "display-result");

        if let Some(msg) = result.error {
            self.ui_runtime.error = Some(msg);
            self.ui_runtime.loading = false;
            self.ui_runtime.pending_placeholder_after = None;
            self.clear_animation_stream_state();
            self.stop_slideshow();
            tracing::debug!(
                request_id = result.request_id,
                elapsed_ms = poll_started.elapsed().as_millis(),
                "viewer_ui: result applied with error"
            );
            let _ = self.consume_queued_view(display_w, display_h, max_tex_side, ctx);
            return true;
        }

        if self.ui_runtime.nav_mode == NavMode::FollowLatest
            && self.request.queued_view.is_some()
            && self.nav_target() != self.persistent.requested_page
        {
            tracing::trace!(
                request_id = result.request_id,
                requested_page = self.persistent.requested_page,
                nav_target = self.nav_target(),
                queued_view = ?self.request.queued_view,
                "viewer_ui: dropped intermediate result for follow-latest"
            );
            return self.consume_queued_view(display_w, display_h, max_tex_side, ctx);
        }

        let (result_left_page, result_right_page) =
            self.current_view_pages(self.persistent.requested_page);

        let request_display_w = result.request_display_w;
        let request_display_h = result.request_display_h;
        self.update_interactive_rgba_cache_budget(
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        self.cache_interactive_rgba_pages(InteractiveRgbaCacheInsertContext {
            request_kind: "interactive",
            pages: DisplayCommitPages {
                left_page: result_left_page,
                right_page: result_right_page,
            },
            left: result
                .left
                .as_ref()
                .filter(|_| !result.left_is_animation_stream),
            right: result
                .right
                .as_ref()
                .filter(|_| !result.right_is_animation_stream),
            target_w: request_display_w,
            target_h: request_display_h,
            quality: &result.request_quality,
            max_tex_side: result.request_max_tex_side,
        });

        let upload_started = Instant::now();
        tracing::trace!(
            frame = self.ui_runtime.show_seq,
            request_id = result.request_id,
            page = self.persistent.requested_page,
            at_ms = now_ms(),
            "viewer-texture: upload start"
        );
        let result_for_commit = result.clone();
        let left_content = result.left.map(|frames| {
            if result.left_is_animation_stream {
                PageContent::from_stream_chunk(
                    frames,
                    result.left_stream_exhausted,
                    "viewer_left",
                    ctx,
                )
            } else {
                PageContent::from_frames(frames, "viewer_left", ctx)
            }
        });
        let right_content = result.right.map(|frames| {
            if result.right_is_animation_stream {
                PageContent::from_stream_chunk(
                    frames,
                    result.right_stream_exhausted,
                    "viewer_right",
                    ctx,
                )
            } else {
                PageContent::from_frames(frames, "viewer_right", ctx)
            }
        });
        self.commit_display_contents(DisplayCommitContext {
            result: &result_for_commit,
            upload_started,
            ctx,
            poll_started,
            display_w,
            display_h,
            max_tex_side,
            gpu_history_hit: false,
            left: DisplayCommitSlot {
                page: result_left_page,
                content: left_content,
                hit: None,
                register_gpu_history: true,
            },
            right: DisplayCommitSlot {
                page: result_right_page,
                content: right_content,
                hit: None,
                register_gpu_history: true,
            },
        })
    }

    pub(super) fn start_view_request(&mut self, request: ViewRequestContext<'_>) {
        let ViewRequestContext {
            nav_id,
            physical_page,
            display_w,
            display_h,
            max_tex_side,
            ctx,
            reason: _reason,
        } = request;
        let view_idx = physical_page;
        let in_transition = self.compute_transition_flag();
        if self.transition_logs_active() {
            tracing::trace!(
                frame = self.ui_runtime.show_seq,
                path = %self.persistent.entry.path.display(),
                view_idx,
                display_w,
                display_h,
                max_tex_side,
                in_transition,
                reason = "start_view_request",
                "viewer_ui: decode request enqueue"
            );
        }
        tracing::trace!(
            frame = self.ui_runtime.show_seq,
            path = %self.persistent.entry.path.display(),
            view_idx,
            display_w,
            display_h,
            at_ms = now_ms(),
            "viewer-decode: request enqueue"
        );
        self.ui_runtime.loading = true;
        self.ui_runtime.error = None;
        self.ui_runtime.pending_placeholder_after =
            Some(Instant::now() + PENDING_PLACEHOLDER_DELAY);
        self.mark_slideshow_wait_display();
        self.persistent.requested_page = view_idx;
        self.persistent.target_page = view_idx;
        self.request.prefetch_anchor_view = view_idx;
        self.request.prefetch_idle_deadline = None;
        self.clear_animation_stream_state();
        self.clear_interactive_in_flight();
        if matches!(self.persistent.spread_setting, SpreadMode::Auto) {
            let (_visible_left, page_right) = self.display_pages_for_physical_page(view_idx);
            let spread = page_right.is_some() || self.is_leading_cover_blank_spread(view_idx);
            self.persistent.spread_mode = spread;
        } else {
            self.persistent.spread_mode =
                matches!(self.persistent.spread_setting, SpreadMode::Spread);
        }

        self.start_display_request(nav_id, view_idx, display_w, display_h, max_tex_side, ctx);
    }

    fn start_display_request(
        &mut self,
        nav_id: u64,
        physical_page: u32,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
    ) {
        let view_idx = physical_page;
        let req = self.request.loader.peek_next_request_id();
        let layout = self.view_layout_for_with_caller(view_idx, display_w, display_h, false);
        self.log_view_layout("request", &layout);
        let page_left = layout.page_left;
        let page_right = layout.page_right;
        let request_display_w = layout.page_decode_w;
        let request_display_h = layout.page_decode_h;
        tracing::trace!(
            "[interactive_request] page={} effective_spread={} request_display_w={} request_display_h={} render_signature.quality={:?} render_signature.target_w={} render_signature.target_h={} render_signature.max_tex_side={}",
            view_idx,
            layout.effective_spread,
            request_display_w,
            request_display_h,
            self.request.quality,
            request_display_w,
            request_display_h,
            max_tex_side
        );
        self.update_interactive_rgba_cache_budget(
            request_display_w,
            request_display_h,
            max_tex_side,
        );
        match self.gpu_texture_history_lookup(
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
        ) {
            GpuTextureDisplayLookup::Full { left, right } => {
                let result = self.make_synthetic_display_result(SyntheticDisplayRequest {
                    nav_id,
                    physical_page: view_idx,
                    page_left,
                    page_right,
                    left: None,
                    right: None,
                    request_display_w,
                    request_display_h,
                    request_quality: self.request.quality,
                    request_max_tex_side: max_tex_side,
                });
                let left_content = left
                    .as_ref()
                    .map(|hit| PageContent::Static(hit.texture.clone()));
                let right_content = right
                    .as_ref()
                    .map(|hit| PageContent::Static(hit.texture.clone()));
                let _ = self.commit_display_contents(DisplayCommitContext {
                    result: &result,
                    upload_started: Instant::now(),
                    ctx,
                    poll_started: Instant::now(),
                    display_w,
                    display_h,
                    max_tex_side,
                    gpu_history_hit: true,
                    left: DisplayCommitSlot {
                        page: page_left,
                        content: left_content,
                        hit: left.as_ref(),
                        register_gpu_history: false,
                    },
                    right: DisplayCommitSlot {
                        page: page_right,
                        content: right_content,
                        hit: right.as_ref(),
                        register_gpu_history: false,
                    },
                });
                return;
            }
            GpuTextureDisplayLookup::Partial { left, right } => {
                self.display_assets
                    .gpu_texture_history
                    .record_partial_spread_hit();
                if self.try_apply_partial_display_reuse_commit(PartialDisplayReuseContext {
                    nav_id,
                    physical_page: view_idx,
                    page_left,
                    page_right,
                    request_display_w,
                    request_display_h,
                    max_tex_side,
                    display_w,
                    display_h,
                    ctx,
                    left_hit: left.as_ref(),
                    right_hit: right.as_ref(),
                }) {
                    return;
                }
                let reused_hit = left
                    .as_ref()
                    .or(right.as_ref())
                    .expect("partial hit requires either left or right reuse");
                let uploaded_page = if left.is_some() {
                    page_right
                } else {
                    page_left
                };
                let missing_page = uploaded_page.unwrap_or(reused_hit.key.page);
                tracing::trace!(
                    view_page = view_idx,
                    left_page = ?page_left,
                    right_page = ?page_right,
                    gpu_hit_page = reused_hit.key.page,
                    missing_page,
                    entries = self.display_assets.gpu_texture_history.entry_count(),
                    current_mb = %Self::format_bytes_mb(
                        self.display_assets.gpu_texture_history.current_bytes()
                    ),
                    max_mb = %Self::format_bytes_mb(self.display_assets.gpu_texture_history.max_bytes()),
                    reason = "cpu_rgba_not_ready",
                    "gpu-history-partial-fallback"
                );
            }
            GpuTextureDisplayLookup::Miss => {
                self.display_assets.gpu_texture_history.record_miss();
                tracing::trace!(
                    view_page = view_idx,
                    left_page = ?page_left,
                    right_page = ?page_right,
                    entries = self.display_assets.gpu_texture_history.entry_count(),
                    current_mb = %Self::format_bytes_mb(
                        self.display_assets.gpu_texture_history.current_bytes()
                    ),
                    max_mb = %Self::format_bytes_mb(self.display_assets.gpu_texture_history.max_bytes()),
                    reason = "not_found",
                    "gpu-history-miss"
                );
            }
        }
        if self.try_apply_display_commit(PartialDisplayReuseContext {
            nav_id,
            physical_page: view_idx,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
            display_w,
            display_h,
            ctx,
            left_hit: None,
            right_hit: None,
        }) {
            self.trigger_prefetch(display_w, display_h, max_tex_side);
            return;
        }
        if self.start_partial_display_request(PartialDisplayRequest {
            nav_id,
            physical_page: view_idx,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
        }) {
            return;
        }
        let bg_rgba_cache = self.request.worker_manager.bg_rgba_cache();
        let left_hit = page_left.is_some_and(|page| {
            let key = self.rgba_cache_key(page, request_display_w, request_display_h, max_tex_side);
            self.display_assets
                .interactive_rgba_cache
                .get(&key)
                .is_some()
                || bg_rgba_cache
                    .read()
                    .ok()
                    .is_some_and(|cache| cache.contains(&key))
        });
        let right_hit = page_right.is_some_and(|page| {
            let key = self.rgba_cache_key(page, request_display_w, request_display_h, max_tex_side);
            self.display_assets
                .interactive_rgba_cache
                .get(&key)
                .is_some()
                || bg_rgba_cache
                    .read()
                    .ok()
                    .is_some_and(|cache| cache.contains(&key))
        });
        tracing::trace!(
            "[viewer-display-request] nav_id={} req={} view={} left_hit={} right_hit={}",
            nav_id,
            req,
            view_idx,
            left_hit,
            right_hit
        );
        self.set_request_hit(
            nav_id,
            RequestHitState::Hit(left_hit),
            RequestHitState::Hit(right_hit),
        );
        self.request.pending_id = self.start_interactive_group_request(InteractiveGroupRequest {
            nav_id,
            physical_page: view_idx,
            page_left,
            page_right,
            request_display_w,
            request_display_h,
            max_tex_side,
        });
    }

    pub(super) fn request_view(
        &mut self,
        physical_page: u32,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
        reason: &'static str,
    ) {
        let view_idx = physical_page;
        let queued_before = self.request.queued_view;
        let old_target = self.persistent.target_page;
        let nav_id = self.begin_nav(self.nav_target(), view_idx, reason);
        self.display_assets.interactive_rgba_cache = RgbaPageCache::new();
        self.clear_bg_admission_state("movement");
        self.request.last_bg_admission_requirement = None;
        self.request.prefetch_idle_deadline = None;
        if self.ui_runtime.loading {
            self.persistent.target_page = view_idx;
            tracing::trace!(
                "[viewer-nav-input] reason={} displayed_page={} requested_page={} old_target={} new_target={} loading={} queued_before={:?} nav_consecutive_count={} nav_mode={:?} spread_setting={:?} spread_mode={:?}",
                reason,
                self.persistent.displayed_page,
                self.persistent.requested_page,
                old_target,
                self.persistent.target_page,
                self.ui_runtime.loading,
                queued_before,
                self.ui_runtime.nav_consecutive_count,
                self.ui_runtime.nav_mode,
                self.persistent.spread_setting,
                self.persistent.spread_mode
            );
            tracing::trace!(
                "[viewer-target-update] nav_id={} old_displayed={} old_target={} new_target={} loading=true reason={}",
                nav_id,
                self.persistent.displayed_page,
                old_target,
                self.persistent.target_page,
                reason
            );
            tracing::trace!(
                requested_page = self.persistent.requested_page,
                queued_before = ?self.request.queued_view,
                queued_after = view_idx,
                "viewer_ui: queued_view set"
            );
            self.request.queued_view = Some(view_idx);
            self.request.queued_nav = Some((view_idx, nav_id, reason));
            return;
        }
        self.request.queued_view = None;
        self.request.queued_nav = None;
        tracing::trace!(
            "[viewer-nav-input] reason={} displayed_page={} requested_page={} old_target={} new_target={} loading={} queued_before={:?} nav_consecutive_count={} nav_mode={:?} spread_setting={:?} spread_mode={:?}",
            reason,
            self.persistent.displayed_page,
            self.persistent.requested_page,
            old_target,
            view_idx,
            self.ui_runtime.loading,
            queued_before,
            self.ui_runtime.nav_consecutive_count,
            self.ui_runtime.nav_mode,
            self.persistent.spread_setting,
            self.persistent.spread_mode
        );
        if self.persistent.target_page != view_idx {
            tracing::trace!(
                "[viewer-target-update] nav_id={} old_displayed={} old_target={} new_target={} loading=false reason={}",
                nav_id,
                self.persistent.displayed_page,
                self.persistent.target_page,
                view_idx,
                reason
            );
        }
        self.persistent.target_page = view_idx;
        self.start_view_request(ViewRequestContext {
            nav_id,
            physical_page: view_idx,
            display_w,
            display_h,
            max_tex_side,
            ctx,
            reason,
        });
    }

    pub(super) fn consume_queued_view(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
    ) -> bool {
        let Some((next_view, nav_id, reason)) = self.request.queued_nav.take() else {
            return false;
        };
        self.request.queued_view = None;
        if next_view == self.persistent.requested_page {
            tracing::trace!(
                requested_page = self.persistent.requested_page,
                queued_view = next_view,
                "viewer_ui: queued_view dropped duplicate"
            );
            return false;
        }
        tracing::trace!(
            requested_page = self.persistent.requested_page,
            queued_view = next_view,
            "viewer_ui: queued_view consumed"
        );
        self.start_view_request(ViewRequestContext {
            nav_id,
            physical_page: next_view,
            display_w,
            display_h,
            max_tex_side,
            ctx,
            reason,
        });
        true
    }

    fn handle_polled_loader_result(
        &mut self,
        result: ViewerResult,
        ctx: &egui::Context,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        poll_started: Instant,
    ) -> bool {
        let is_active_interactive_id = result.request_id == self.request.pending_id
            || self.request.pending_id_aux == Some(result.request_id);

        // 世代外の結果は到着順ではなく新しさを優先して捨てる。
        if !is_active_interactive_id {
            if self.request.animation_stream_request_id == Some(result.request_id) {
                return self.apply_animation_stream_chunk_result(result, ctx);
            }
            tracing::trace!(
                "[viewer-result-drop] nav_id={} req={} current_req={} view={} reason=stale_noninteractive_result worker={} page_left={:?} page_right={:?}",
                result.nav_id,
                result.request_id,
                self.request.pending_id,
                result.view_idx,
                result.worker,
                result.page_left,
                result.page_right
            );
            return false;
        }

        if self.request.pending_interactive_group.is_some() {
            if let Some(merged) = self.absorb_interactive_partial(result) {
                return self.apply_display_result(
                    merged,
                    ctx,
                    poll_started,
                    display_w,
                    display_h,
                    max_tex_side,
                );
            }
            return true;
        }

        self.apply_display_result(
            result,
            ctx,
            poll_started,
            display_w,
            display_h,
            max_tex_side,
        )
    }

    pub(super) fn poll_loader(
        &mut self,
        ctx: &egui::Context,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> bool {
        let mut handled_any = false;
        let drain_limit = self.request.background_worker_count.max(1);
        let pass_limit = self.request.background_worker_count.max(1);
        for _ in 0..pass_limit {
            let poll_started = Instant::now();
            let mut drained = 0usize;
            while drained < drain_limit {
                let Some(result) = self.request.loader.try_recv_interactive() else {
                    break;
                };
                handled_any |= self.handle_polled_loader_result(
                    result,
                    ctx,
                    display_w,
                    display_h,
                    max_tex_side,
                    poll_started,
                );
                drained = drained.saturating_add(1);
            }
            if drained == 0 {
                break;
            }
        }
        handled_any
    }

    pub(super) fn maybe_request_animation_stream_fill(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
    ) -> bool {
        if self.ui_runtime.loading
            || self.request.queued_view.is_some()
            || self.request.animation_stream_request_id.is_some()
            || self.ui_runtime.nav_mode == NavMode::FollowLatest
        {
            return false;
        }
        let Some(view_idx) = self.request.active_animation_stream_view else {
            return false;
        };
        if view_idx != self.active_animation_view() {
            return false;
        }

        let left_needs_restart = self
            .display_assets
            .content_left
            .as_ref()
            .is_some_and(PageContent::stream_should_restart);
        let right_needs_restart = self
            .display_assets
            .content_right
            .as_ref()
            .is_some_and(PageContent::stream_should_restart);
        let left_needs_fill = !left_needs_restart
            && self
                .display_assets
                .content_left
                .as_ref()
                .is_some_and(PageContent::stream_should_fill);
        let right_needs_fill = !right_needs_restart
            && self
                .display_assets
                .content_right
                .as_ref()
                .is_some_and(PageContent::stream_should_fill);
        if !left_needs_restart && !right_needs_restart && !left_needs_fill && !right_needs_fill {
            return false;
        }

        let layout = self.view_layout_for_with_caller(view_idx, display_w, display_h, false);
        self.log_view_layout("request", &layout);
        let full_left = layout.page_left;
        let full_right = layout.page_right;
        let page_left = if left_needs_restart || left_needs_fill {
            full_left
        } else {
            None
        };
        let page_right = if right_needs_restart || right_needs_fill {
            full_right
        } else {
            None
        };
        let request_display_w = layout.page_decode_w;
        let request_display_h = layout.page_decode_h;
        let request_id = if left_needs_restart || right_needs_restart {
            self.request
                .loader
                .send_animation_stream_start(ViewerLoadRequest {
                    path: Arc::clone(&self.persistent.entry.path),
                    view_idx,
                    page_left,
                    page_right,
                    display_w: request_display_w,
                    display_h: request_display_h,
                    quality: self.request.quality,
                    max_tex_side,
                    frame_cache_cap: self.frame_cache_cap(),
                    nav_id: self.request.active_nav_id.unwrap_or(0),
                    interactive: true,
                })
        } else {
            self.request
                .loader
                .send_animation_stream_fill(ViewerLoadRequest {
                    path: Arc::clone(&self.persistent.entry.path),
                    view_idx,
                    page_left,
                    page_right,
                    display_w: request_display_w,
                    display_h: request_display_h,
                    quality: self.request.quality,
                    max_tex_side,
                    frame_cache_cap: self.frame_cache_cap(),
                    nav_id: self.request.active_nav_id.unwrap_or(0),
                    interactive: true,
                })
        };
        self.request.animation_stream_request_id = Some(request_id);
        if left_needs_restart || left_needs_fill {
            if let Some(content) = self.display_assets.content_left.as_mut() {
                content.mark_stream_fill_in_flight();
            }
        }
        if right_needs_restart || right_needs_fill {
            if let Some(content) = self.display_assets.content_right.as_mut() {
                content.mark_stream_fill_in_flight();
            }
        }
        tracing::trace!(
            request_id,
            view_idx,
            left_needs_restart,
            right_needs_restart,
            left_needs_fill,
            right_needs_fill,
            "viewer_ui: animation stream fill requested"
        );
        true
    }

    pub(super) fn trigger_prefetch(&mut self, display_w: u32, display_h: u32, max_tex_side: u32) {
        if self.request.active_animation_stream_view.is_some() {
            tracing::trace!(
                requested_page = self.persistent.requested_page,
                active_animation_stream_view = ?self.request.active_animation_stream_view,
                "viewer: prefetch suppressed by active animation stream"
            );
            self.request.prefetch_idle_deadline = None;
            return;
        }
        if self.request.animation_stream_request_id.is_some() {
            tracing::trace!(
                requested_page = self.persistent.requested_page,
                animation_stream_request_id = ?self.request.animation_stream_request_id,
                "viewer: prefetch suppressed by animation stream"
            );
            self.request.prefetch_idle_deadline = None;
            return;
        }
        if self.request.prefetch_dir == 0 {
            // 方向未確定時は前進を既定にして、補充の起点を固定する。
            self.request.prefetch_dir = 1;
        }
        let now = Instant::now();
        self.request.prefetch_anchor_view =
            self.interactive_request_anchor_page().navigation_page();
        self.request.prefetch_idle_deadline = Some(now + PREFETCH_DEEP_IDLE_DELAY);
        tracing::trace!(
            requested_page = self.persistent.requested_page,
            spread = self.persistent.spread_mode,
            frame_cache_cap = self.frame_cache_cap(),
            "viewer: prefetch window reset"
        );
        self.publish_worker_manager_state(display_w, display_h, max_tex_side);
    }

    pub fn toggle_spread(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
    ) {
        let selected_page = self.navigation_base_page();

        let next_setting = match (
            self.persistent.spread_setting.clone(),
            self.auto_mode_available(),
        ) {
            (SpreadMode::Auto, true) => SpreadMode::Single,
            (SpreadMode::Auto, false) => SpreadMode::Single,
            (SpreadMode::Single, true) => SpreadMode::Spread,
            (SpreadMode::Single, false) => SpreadMode::Spread,
            (SpreadMode::Spread, true) => SpreadMode::Auto,
            (SpreadMode::Spread, false) => SpreadMode::Single,
        };
        self.persistent.spread_setting = next_setting.clone();
        self.persistent.spread_mode = match next_setting {
            SpreadMode::Auto => {
                let (_visible_left, page_right) =
                    self.display_pages_for_physical_page(selected_page);
                page_right.is_some()
            }
            SpreadMode::Single => false,
            SpreadMode::Spread => true,
        };
        self.invalidate_spread_snapshot();

        self.load_view(selected_page, display_w, display_h, max_tex_side, ctx);
    }

    pub fn toggle_cover_blank(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
    ) {
        let selected_page = self.navigation_base_page();
        self.persistent.cover_blank = !self.persistent.cover_blank;
        self.persistent.auto_spread_plan = build_auto_spread_plan_for_mode(
            &self.persistent.page_map_mode,
            self.persistent.cover_blank,
        );
        self.invalidate_spread_snapshot();
        self.load_view(selected_page, display_w, display_h, max_tex_side, ctx);
    }

    pub fn go_next(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
        reason: &'static str,
    ) {
        if matches!(self.persistent.spread_setting, SpreadMode::Auto) && self.auto_mode_available()
        {
            let current = self.navigation_base_page();
            if let Some(next) = self.auto_next_anchor(current) {
                self.request.prefetch_dir = 1;
                self.request_view(next, display_w, display_h, max_tex_side, ctx, reason);
            }
            return;
        }
        let current = self.navigation_base_page();
        let (_visible_left, visible_right) = self.current_view_pages(current);
        let step = if visible_right.is_some() { 2 } else { 1 };
        let next_page = current.saturating_add(step);
        if next_page < self.persistent.page_count {
            self.request.prefetch_dir = 1;
            self.request_view(next_page, display_w, display_h, max_tex_side, ctx, reason);
        }
    }
    pub fn go_prev(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
        reason: &'static str,
    ) {
        if matches!(self.persistent.spread_setting, SpreadMode::Auto) && self.auto_mode_available()
        {
            let current = self.navigation_base_page();
            if let Some(prev) = self.auto_previous_anchor(current) {
                self.request.prefetch_dir = -1;
                self.request_view(prev, display_w, display_h, max_tex_side, ctx, reason);
            }
            return;
        }
        let current = self.navigation_base_page();
        let (_visible_left, visible_right) = self.current_view_pages(current);
        let step = if visible_right.is_some() { 2 } else { 1 };
        let prev_page = current.saturating_sub(step);
        if prev_page != current {
            self.request.prefetch_dir = -1;
            self.request_view(prev_page, display_w, display_h, max_tex_side, ctx, reason);
        }
    }
    pub fn go_first(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
        reason: &'static str,
    ) {
        if self.nav_target() != 0 {
            self.request.prefetch_dir = 1;
            self.request_view(0, display_w, display_h, max_tex_side, ctx, reason);
        }
    }
    pub fn go_last(
        &mut self,
        display_w: u32,
        display_h: u32,
        max_tex_side: u32,
        ctx: &egui::Context,
        reason: &'static str,
    ) {
        let last = self.persistent.page_count.saturating_sub(1);
        if self.nav_target() != last {
            self.request.prefetch_dir = -1;
            self.request_view(last, display_w, display_h, max_tex_side, ctx, reason);
        }
    }

    fn page_display_label(&self, page: Option<u32>) -> Option<String> {
        let page = page?;
        self.persistent
            .page_display_labels
            .get(page as usize)
            .cloned()
    }

    fn toolbar_spread_slot_label(
        &self,
        page: Option<u32>,
        is_blank_slot: bool,
        blank_label: &str,
    ) -> Option<String> {
        if let Some(label) = self.page_display_label(page) {
            return Some(label);
        }
        if is_blank_slot {
            return Some(blank_label.to_owned());
        }
        None
    }
}

// ── 描画 ─────────────────────────────────────────────────────────────────────
