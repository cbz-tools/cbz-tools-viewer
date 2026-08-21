//! サムネイル生成と Page Map 反映をまとめる worker。
//!
//! UI には先にサムネイルを返し、永続化と Page Map 反映は後段で処理する。
//! complete / slow Page Map の実処理は `PageMapCoordinator` に委譲する。

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ff_decode::{SeekMode, VideoDecoder};
use ff_format::PixelFormat;
use parking_lot::RwLock;
use tokio::sync::{Notify, Semaphore, oneshot};

use super::storage_medium::{StorageMedium, detect_storage_medium_cached};
use crate::domain::archive::BookId;
use crate::domain::page::ImageFormatHint;
use crate::domain::page_map::{BookPageMap, SourceRevision};
use crate::domain::thumbnail::Thumbnail;
use crate::infra::archive::{
    BookReader, BookSourceKind, book_source_kind,
    epub::{EpubImageReader, EpubPageMapFastOutcome, build_book_page_map_fast_from_epub_reader},
    folder::FolderImageReader,
    open_book_reader,
    page_map::{
        FolderPageMapFastLaneOutput, FolderPageMapFastStatus, ZipPageMapFastOutput,
        ZipPageMapFastStatus, ZipPageMapIssueReason, build_folder_page_map_fast_lanes,
        build_zip_page_map_fast_lanes,
    },
};
use crate::infra::cache::artifact_failure::{ArtifactFailureDiskCache, ArtifactKind};
use crate::infra::cache::disk::DiskCache;
use crate::infra::cache::memory::ThumbMemCache;
use crate::infra::cache::page_map::PageMapDiskCache;
use crate::infra::image::decode as img;
use crate::infra::page_map::coordinator::{
    PageMapCompleteRequest, PageMapCoordinator, PageMapFastPersistRequest,
    PageMapReadyPersistRequest, PageMapStatus,
};
use crate::infra::page_map::viewer_bootstrap::try_load_existing_viewer_page_map_for_spad;
use crate::repaint::RepaintNotifier;
use crate::util::archive_path::is_supported_image_path;

/// 通常スロットのタイムアウト。PNG デコード等の長時間処理を許容するため 15s に延ばす。
const NORMAL_TIMEOUT: Duration = Duration::from_secs(15);
/// OOM と長時間ブロックを避けるための thumb 用 raw データ上限。
const MAX_THUMB_RAW_BYTES: usize = 256 * 1024 * 1024;
/// Library thumbnail cache budget を CPU 側 ThumbMemCache にも適用する。
const THUMB_MEM_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;
/// 動画代表フレームを取得する位置の割合。
const VIDEO_THUMB_POSITION_RATIO: f64 = 0.05;
const ANIMATED_PREVIEW_SCRUB_BUCKET_SLOTS: usize = 16;
pub(crate) const BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE: usize = 3_000;
const BACKGROUND_ARTIFACT_MAX_CREDIT: f64 = 8.0;

// ── 公開型 ────────────────────────────────────────────────────────────────────

/// UI → Worker へのリクエスト
#[derive(Clone)]
pub struct ThumbTask {
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub target_width: u16,
    /// 要求時点のファイルサイズ。処理完了までに変化した古い結果を UI に返さないために使う。
    pub expected_size: u64,
    /// 要求時点の更新日時。処理完了までに変化した古い結果を UI に返さないために使う。
    pub expected_modified: Option<SystemTime>,
    /// 同一 path/id のファイル内容が変わった場合、古い memory/disk thumb cache を使わず再生成する。
    pub bypass_cache: bool,
}

/// UI → Worker への動画サムネイル要求。normal task/cache とは別経路で扱う。
#[derive(Clone)]
pub struct VideoThumbTask {
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub target_width: u16,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
}

/// Runtime-only video preview request. It never enters the thumbnail lanes or caches.
#[derive(Clone)]
pub struct VideoPreviewTask {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub target_width: u16,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub scene_percent: u8,
}

/// Runtime-only animated WebP preview request. It never enters the thumbnail
/// lanes or caches and is served by the existing preview worker thread.
#[derive(Clone)]
pub struct AnimatedPreviewTask {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub target_width: u16,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    /// `None` keeps the existing native-timing AUTO loop. `Some` selects a
    /// quantized time-scrub target.
    pub scrub_bucket: Option<u16>,
    pub scrub_bucket_count: u16,
    /// Bounded metadata carried by the replaceable command. These buckets were
    /// superseded while a decode could already be active and must not remain
    /// protected by the worker's last-success guard.
    pub abandon_bucket_mask: u64,
}

/// Runtime-only static page preview request. It never enters thumbnail lanes,
/// Page Map generation, or any cache.
#[derive(Clone)]
pub struct StaticPreviewTask {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub target_width: u16,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub page_index: u32,
    pub page_count: usize,
}

/// Worker → UI への成功レスポンス
pub struct ReadyThumb {
    pub book_id: BookId,
    pub pixels: Arc<[u8]>,
    pub width: u16,
    pub height: u16,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
}

/// Video success response carries the source revision validated by the worker.
pub struct VideoReady {
    pub ready: ReadyThumb,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub generation: u64,
}

pub struct VideoPreviewReady {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub scene_percent: u8,
    pub pixels: Arc<[u8]>,
    pub width: u16,
    pub height: u16,
}

pub struct VideoPreviewFailed {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub scene_percent: u8,
}

pub struct AnimatedPreviewReady {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub frame_index: u64,
    pub delay_ms: u32,
    pub scrub_bucket: Option<u16>,
    pub abandon_ack_bucket_mask: u64,
    pub pixels: Arc<[u8]>,
    pub width: u16,
    pub height: u16,
}

pub struct AnimatedPreviewFailed {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub frame_index: u64,
    pub scrub_bucket: Option<u16>,
    pub abandon_ack: bool,
    pub abandon_ack_bucket_mask: u64,
}

pub struct AnimatedPreviewUnavailable {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub scrub_bucket: Option<u16>,
    pub abandon_ack_bucket_mask: u64,
}

pub struct StaticPreviewReady {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub page_index: u32,
    pub pixels: Arc<[u8]>,
    pub width: u16,
    pub height: u16,
}

pub struct StaticPreviewFailed {
    pub session_id: u64,
    pub book_id: BookId,
    pub path: Arc<Path>,
    pub expected_size: u64,
    pub expected_modified: Option<SystemTime>,
    pub page_index: u32,
}

/// Worker → UI へのメッセージ
pub enum WorkerMsg {
    Ready(ReadyThumb),
    VideoReady(VideoReady),
    VideoPreviewReady(VideoPreviewReady),
    VideoPreviewFailed(VideoPreviewFailed),
    VideoPreviewStale(VideoPreviewFailed),
    AnimatedPreviewReady(AnimatedPreviewReady),
    AnimatedPreviewFailed(AnimatedPreviewFailed),
    AnimatedPreviewStale(AnimatedPreviewFailed),
    AnimatedPreviewUnavailable(AnimatedPreviewUnavailable),
    StaticPreviewReady(StaticPreviewReady),
    StaticPreviewFailed(StaticPreviewFailed),
    StaticPreviewStale(StaticPreviewFailed),
    Failed(BookId),
    FailedWithRevision {
        book_id: BookId,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    },
    /// サムネイル生成の恒久失敗。UI へ FailedPermanent として返す。
    /// rar / avif feature 無効時や、内容として確定的に失敗しているケースを含む。
    FailedPermanent(BookId),
    FailedPermanentWithRevision {
        book_id: BookId,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
    },
    /// 要求後に同じ path/id のファイル内容が変わった古いタスク。UI へ失敗状態としては反映しない。
    Stale(BookId),
    /// 表示用サムネイルの古いタスク。UI 側で該当する表示要求だけを再試行可能にする。
    DisplayStale {
        book_id: BookId,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
        target_width: u16,
        bypass_cache: bool,
    },
    /// VideoFile の古いタスク。要求時点の source snapshot を UI 側で照合して
    /// 新しい video request の Loading 状態を誤って解除しないために使う。
    VideoStale {
        book_id: BookId,
        expected_size: u64,
        expected_modified: Option<SystemTime>,
        generation: u64,
    },
    PageMapStatus(PageMapStatus),
}

#[derive(Clone)]
pub(crate) enum BackgroundArtifactJob {
    Image {
        task: ThumbTask,
        page_map_only: bool,
    },
    Video(VideoThumbTask),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestOrigin {
    Visible,
    Background,
}

impl RequestOrigin {
    fn is_visible(self) -> bool {
        matches!(self, Self::Visible)
    }
}

// ── ThumbWorker ───────────────────────────────────────────────────────────────

pub struct ThumbWorker {
    req_tx: tokio::sync::mpsc::UnboundedSender<WorkerReq>,
    video_req_tx: tokio::sync::mpsc::UnboundedSender<VideoReq>,
    video_background_req_tx: tokio::sync::mpsc::UnboundedSender<VideoReq>,
    resp_rx: std::sync::Mutex<std::sync::mpsc::Receiver<WorkerMsg>>,
    generation: Arc<AtomicU64>,
    artifact_generation: Arc<AtomicU64>,
    lanes: Arc<ThumbnailLaneState>,
    display_mailbox: Arc<DisplayThumbMailbox>,
    preview_control: Arc<PreviewControl>,
    animated_result: Arc<Mutex<Option<WorkerMsg>>>,
    visible_artifact_ids: Arc<Mutex<HashSet<BookId>>>,
    background_scheduler_ready: Arc<std::sync::atomic::AtomicBool>,
    background_scheduler_tx: std::sync::mpsc::Sender<BackgroundSchedulerCommand>,
    background_scheduler_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

enum BackgroundSchedulerCommand {
    Replace {
        jobs: Vec<BackgroundArtifactJob>,
        generation: u64,
    },
    SetVisible(HashSet<BookId>),
    Clear,
    Shutdown,
}

enum WorkerReq {
    Task(ThumbTask, u64, RequestOrigin),
    PageMapOnly(ThumbTask, u64),
    PruneObsoleteArtifacts {
        id: BookId,
        source_path: Arc<Path>,
        source_revision: SourceRevision,
    },
    PruneVideoObsoleteArtifacts {
        id: BookId,
        source_path: Arc<Path>,
        source_revision: SourceRevision,
    },
    ClearPending,
    ClearCaches,
    RemoveArchiveCache(BookId),
    RemoveVideoCache(BookId),
    Shutdown,
}

enum VideoReq {
    Task(VideoThumbTask, u64, RequestOrigin),
    ClearCache,
    RemoveCache(BookId),
    Shutdown,
}

struct DisplayThumbMailbox {
    state: Mutex<DisplayThumbMailboxState>,
    wake: Notify,
    lanes: Arc<ThumbnailLaneState>,
}

struct DisplayThumbMailboxState {
    pending: Vec<(ThumbTask, u64)>,
    closed: bool,
}

impl DisplayThumbMailbox {
    fn new(lanes: Arc<ThumbnailLaneState>) -> Self {
        Self {
            state: Mutex::new(DisplayThumbMailboxState {
                pending: Vec::new(),
                closed: false,
            }),
            wake: Notify::new(),
            lanes,
        }
    }

    fn replace(&self, tasks: Vec<(ThumbTask, u64)>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return;
        }
        for _ in 0..state.pending.len() {
            self.lanes.retire_request_pending(ThumbnailLane::Image);
        }
        for _ in 0..tasks.len() {
            self.lanes.mark_request_pending(ThumbnailLane::Image);
        }
        state.pending = tasks;
        drop(state);
        self.wake.notify_waiters();
    }

    fn clear(&self) {
        self.replace(Vec::new());
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return;
        }
        for _ in 0..state.pending.len() {
            self.lanes.retire_request_pending(ThumbnailLane::Image);
        }
        state.pending.clear();
        state.closed = true;
        drop(state);
        self.wake.notify_waiters();
    }

    fn take_next(&self) -> Option<(ThumbTask, u64)> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let task = state.pending.first().cloned();
        if task.is_some() {
            state.pending.remove(0);
            self.lanes.retire_request_pending(ThumbnailLane::Image);
        }
        drop(state);
        if task.is_some() {
            self.wake.notify_waiters();
        }
        task
    }

    fn has_pending(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        !state.pending.is_empty()
    }

    fn is_closed(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed
    }
}

enum PreviewCommand {
    StartVideo(VideoPreviewTask),
    StartAnimated(AnimatedPreviewTask),
    AnimatedScrub(AnimatedPreviewTask),
    AnimatedScrubAbandon { session_id: u64, buckets: u64 },
    AnimatedAuto(AnimatedPreviewTask),
    StartStatic(StaticPreviewTask),
    VideoScene { session_id: u64, scene_percent: u8 },
    StaticPage(StaticPreviewTask),
    Stop,
    Shutdown,
}

struct PreviewControl {
    state: Mutex<PreviewControlState>,
    wake: Condvar,
}

struct PreviewControlState {
    pending: Option<PreviewCommand>,
    closed: bool,
}

impl PreviewControl {
    fn new() -> Self {
        Self {
            state: Mutex::new(PreviewControlState {
                pending: None,
                closed: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn submit(&self, command: PreviewCommand) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        // A preview command is replaceable state, not a queue. This keeps old scene
        // requests from accumulating while a blocking decode is finishing.
        state.pending = Some(command);
        self.wake.notify_one();
    }

    fn submit_if_idle(&self, command: PreviewCommand) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.closed || state.pending.is_some() {
            return false;
        }
        state.pending = Some(command);
        self.wake.notify_one();
        true
    }

    fn cancel_pending_animated_scrub(&self, session_id: u64, bucket: u16) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let should_cancel = matches!(
            state.pending.as_ref(),
            Some(PreviewCommand::AnimatedScrub(task))
                if task.session_id == session_id && task.scrub_bucket == Some(bucket)
        );
        if should_cancel {
            state.pending = None;
            self.wake.notify_one();
        }
        should_cancel
    }

    fn shutdown(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.closed = true;
        state.pending = Some(PreviewCommand::Shutdown);
        self.wake.notify_one();
    }

    fn take(&self) -> PreviewCommand {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(command) = state.pending.take() {
                return command;
            }
            if state.closed {
                return PreviewCommand::Shutdown;
            }
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn take_pending(&self) -> Option<PreviewCommand> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .pending
            .take()
            .or_else(|| state.closed.then_some(PreviewCommand::Shutdown))
    }

    fn wait_until(&self, deadline: Instant) -> PreviewWait {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(command) = state.pending.take() {
                return PreviewWait::Command(command);
            }
            if state.closed {
                return PreviewWait::Command(PreviewCommand::Shutdown);
            }
            let now = Instant::now();
            if now >= deadline {
                return PreviewWait::Deadline;
            }
            let timeout = deadline.saturating_duration_since(now);
            let (next_state, result) = self
                .wake
                .wait_timeout(state, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
            if result.timed_out() {
                return PreviewWait::Deadline;
            }
        }
    }
}

enum PreviewWait {
    Command(PreviewCommand),
    Deadline,
}

fn submit_animated_preview_scrub(
    preview_control: &PreviewControl,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    mut task: AnimatedPreviewTask,
    abandon_bucket_mask: u64,
) {
    clear_animated_preview_result(animated_result);
    task.abandon_bucket_mask = abandon_bucket_mask;
    preview_control.submit(PreviewCommand::AnimatedScrub(task));
}

enum VideoLoopEvent {
    TaskFinished(Option<Result<(), tokio::task::JoinError>>),
    VisibleRequest(Option<VideoReq>),
    BackgroundRequest(Option<VideoReq>),
}

fn background_artifact_is_visible(
    visible_artifact_ids: &Arc<Mutex<HashSet<BookId>>>,
    id: &BookId,
) -> bool {
    visible_artifact_ids
        .lock()
        .map(|ids| ids.contains(id))
        .unwrap_or(false)
}

fn background_scheduler_loop(
    rx: std::sync::mpsc::Receiver<BackgroundSchedulerCommand>,
    req_tx: tokio::sync::mpsc::UnboundedSender<WorkerReq>,
    video_background_req_tx: tokio::sync::mpsc::UnboundedSender<VideoReq>,
    generation: Arc<AtomicU64>,
    lanes: Arc<ThumbnailLaneState>,
    visible_artifact_ids: Arc<Mutex<HashSet<BookId>>>,
    background_scheduler_ready: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut jobs = VecDeque::new();
    let mut job_generation = generation.load(Ordering::Relaxed);
    let mut credit = 0.0;
    let mut last_refill_at = Instant::now();

    loop {
        if !background_scheduler_ready.load(Ordering::Acquire) {
            match rx.recv() {
                Ok(BackgroundSchedulerCommand::Replace {
                    jobs: replacement,
                    generation: replacement_generation,
                }) => {
                    jobs = replacement.into();
                    job_generation = replacement_generation;
                    credit = 0.0;
                    last_refill_at = Instant::now();
                }
                Ok(BackgroundSchedulerCommand::SetVisible(ids)) => {
                    if let Ok(mut visible) = visible_artifact_ids.lock() {
                        *visible = ids;
                    }
                    background_scheduler_ready.store(true, Ordering::Release);
                }
                Ok(BackgroundSchedulerCommand::Clear) => {
                    jobs.clear();
                    credit = 0.0;
                    last_refill_at = Instant::now();
                    job_generation = generation.load(Ordering::Relaxed);
                }
                Ok(BackgroundSchedulerCommand::Shutdown) | Err(_) => return,
            }
            continue;
        }

        if jobs.is_empty() {
            match rx.recv() {
                Ok(BackgroundSchedulerCommand::Replace {
                    jobs: replacement,
                    generation: replacement_generation,
                }) => {
                    jobs = replacement.into();
                    job_generation = replacement_generation;
                    credit = 0.0;
                    last_refill_at = Instant::now();
                }
                Ok(BackgroundSchedulerCommand::SetVisible(ids)) => {
                    if let Ok(mut visible) = visible_artifact_ids.lock() {
                        *visible = ids;
                    }
                }
                Ok(BackgroundSchedulerCommand::Clear) => {
                    jobs.clear();
                    credit = 0.0;
                    last_refill_at = Instant::now();
                    job_generation = generation.load(Ordering::Relaxed);
                    background_scheduler_ready.store(false, Ordering::Release);
                }
                Ok(BackgroundSchedulerCommand::Shutdown) | Err(_) => return,
            }
            continue;
        }

        if generation.load(Ordering::Relaxed) != job_generation {
            jobs.clear();
            credit = 0.0;
            last_refill_at = Instant::now();
            job_generation = generation.load(Ordering::Relaxed);
            continue;
        }

        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last_refill_at).as_secs_f64();
        credit = (credit + elapsed * (BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE as f64 / 60.0))
            .min(BACKGROUND_ARTIFACT_MAX_CREDIT);
        last_refill_at = now;
        let budget = credit.floor() as usize;
        if budget == 0 {
            let rate_per_second = BACKGROUND_ARTIFACT_CHECKS_PER_MINUTE as f64 / 60.0;
            let until_credit = ((1.0 - credit).max(0.0) / rate_per_second).max(0.001);
            match rx.recv_timeout(Duration::from_secs_f64(until_credit)) {
                Ok(command) => match command {
                    BackgroundSchedulerCommand::Replace {
                        jobs: replacement,
                        generation: replacement_generation,
                    } => {
                        jobs = replacement.into();
                        job_generation = replacement_generation;
                        credit = 0.0;
                        last_refill_at = Instant::now();
                    }
                    BackgroundSchedulerCommand::SetVisible(ids) => {
                        if let Ok(mut visible) = visible_artifact_ids.lock() {
                            *visible = ids;
                        }
                    }
                    BackgroundSchedulerCommand::Clear => {
                        jobs.clear();
                        credit = 0.0;
                        last_refill_at = Instant::now();
                        job_generation = generation.load(Ordering::Relaxed);
                        background_scheduler_ready.store(false, Ordering::Release);
                    }
                    BackgroundSchedulerCommand::Shutdown => return,
                },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }

        for _ in 0..budget {
            let Some(job) = jobs.pop_front() else {
                break;
            };
            credit -= 1.0;
            let id = match &job {
                BackgroundArtifactJob::Image { task, .. } => &task.book_id,
                BackgroundArtifactJob::Video(task) => &task.book_id,
            };
            if background_artifact_is_visible(&visible_artifact_ids, id) {
                continue;
            }
            match job {
                BackgroundArtifactJob::Image {
                    task,
                    page_map_only,
                } => {
                    if page_map_only {
                        let _ = req_tx.send(WorkerReq::PageMapOnly(task, job_generation));
                    } else {
                        lanes.mark_request_pending(ThumbnailLane::Image);
                        if req_tx
                            .send(WorkerReq::Task(
                                task,
                                job_generation,
                                RequestOrigin::Background,
                            ))
                            .is_err()
                        {
                            lanes.retire_request_pending(ThumbnailLane::Image);
                        }
                    }
                }
                BackgroundArtifactJob::Video(task) => {
                    lanes.mark_request_pending(ThumbnailLane::Video);
                    if video_background_req_tx
                        .send(VideoReq::Task(
                            task,
                            job_generation,
                            RequestOrigin::Background,
                        ))
                        .is_err()
                    {
                        lanes.retire_request_pending(ThumbnailLane::Video);
                    }
                }
            }
        }
    }
}

impl ThumbWorker {
    pub fn spawn(repaint: RepaintNotifier, artifact_gate: Arc<RwLock<()>>) -> Self {
        let base_goal = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 8);
        let (req_tx, req_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerReq>();
        let (video_req_tx, video_req_rx) = tokio::sync::mpsc::unbounded_channel::<VideoReq>();
        let (video_background_req_tx, video_background_req_rx) =
            tokio::sync::mpsc::unbounded_channel::<VideoReq>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<WorkerMsg>();
        let (background_scheduler_tx, background_scheduler_rx) =
            std::sync::mpsc::channel::<BackgroundSchedulerCommand>();
        let generation = Arc::new(AtomicU64::new(0));
        let artifact_generation = Arc::new(AtomicU64::new(0));
        let lanes = Arc::new(ThumbnailLaneState::new(base_goal));
        let display_mailbox = Arc::new(DisplayThumbMailbox::new(Arc::clone(&lanes)));
        let preview_control = Arc::new(PreviewControl::new());
        let animated_result = Arc::new(Mutex::new(None));
        let visible_artifact_ids = Arc::new(Mutex::new(HashSet::new()));
        let background_scheduler_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let background_scheduler_handle = thread::Builder::new()
            .name("thumb-background-scheduler".into())
            .spawn({
                let req_tx = req_tx.clone();
                let video_background_req_tx = video_background_req_tx.clone();
                let generation = Arc::clone(&generation);
                let lanes = Arc::clone(&lanes);
                let visible_artifact_ids = Arc::clone(&visible_artifact_ids);
                let background_scheduler_ready = Arc::clone(&background_scheduler_ready);
                move || {
                    background_scheduler_loop(
                        background_scheduler_rx,
                        req_tx,
                        video_background_req_tx,
                        generation,
                        lanes,
                        visible_artifact_ids,
                        background_scheduler_ready,
                    )
                }
            })
            .ok();
        let normal_resp_tx = resp_tx.clone();
        let preview_resp_tx = resp_tx.clone();
        let normal_repaint = repaint.clone();

        std::thread::Builder::new()
            .name("thumb-worker".into())
            .spawn({
                let generation = Arc::clone(&generation);
                let artifact_generation = Arc::clone(&artifact_generation);
                let artifact_gate = Arc::clone(&artifact_gate);
                let lanes = Arc::clone(&lanes);
                let display_mailbox = Arc::clone(&display_mailbox);
                let visible_artifact_ids = Arc::clone(&visible_artifact_ids);
                let req_tx = req_tx.clone();
                move || {
                    worker_main(WorkerMainContext {
                        req_rx,
                        video_req_rx,
                        video_background_req_rx,
                        req_tx,
                        resp_tx: normal_resp_tx,
                        repaint: normal_repaint,
                        generation,
                        artifact_generation,
                        artifact_gate,
                        lanes,
                        display_mailbox,
                        visible_artifact_ids,
                        base_goal,
                    })
                }
            })
            .map_err(|e| {
                tracing::error!("failed to spawn thumb-worker thread: {e}");
                e
            })
            .ok();

        std::thread::Builder::new()
            .name("preview-worker".into())
            .spawn({
                let preview_control = Arc::clone(&preview_control);
                let animated_result = Arc::clone(&animated_result);
                move || preview_worker(preview_control, preview_resp_tx, animated_result, repaint)
            })
            .map_err(|e| {
                tracing::error!("failed to spawn preview-worker thread: {e}");
                e
            })
            .ok();

        Self {
            req_tx,
            video_req_tx,
            video_background_req_tx,
            resp_rx: std::sync::Mutex::new(resp_rx),
            generation,
            artifact_generation,
            lanes,
            display_mailbox,
            preview_control,
            animated_result,
            visible_artifact_ids,
            background_scheduler_ready,
            background_scheduler_tx,
            background_scheduler_handle: Mutex::new(background_scheduler_handle),
        }
    }

    pub fn request(&self, task: ThumbTask) {
        let generation = self.generation.load(Ordering::Relaxed);
        self.lanes.mark_request_pending(ThumbnailLane::Image);
        if self
            .req_tx
            .send(WorkerReq::Task(task, generation, RequestOrigin::Visible))
            .is_err()
        {
            self.lanes.retire_request_pending(ThumbnailLane::Image);
        }
    }

    pub fn replace_display_tasks(&self, tasks: Vec<ThumbTask>) {
        let generation = self.generation.load(Ordering::Relaxed);
        self.display_mailbox
            .replace(tasks.into_iter().map(|task| (task, generation)).collect());
    }

    pub(crate) fn replace_background_artifact_jobs(&self, jobs: Vec<BackgroundArtifactJob>) {
        let generation = self.generation.load(Ordering::Relaxed);
        self.background_scheduler_ready
            .store(false, Ordering::Release);
        let _ = self
            .background_scheduler_tx
            .send(BackgroundSchedulerCommand::Replace { jobs, generation });
    }

    pub(crate) fn set_visible_artifact_ids(&self, ids: HashSet<BookId>) {
        if let Ok(mut visible) = self.visible_artifact_ids.lock() {
            *visible = ids.clone();
        }
        self.background_scheduler_ready
            .store(true, Ordering::Release);
        let _ = self
            .background_scheduler_tx
            .send(BackgroundSchedulerCommand::SetVisible(ids));
    }

    pub(crate) fn is_artifact_visible(&self, id: &BookId) -> bool {
        background_artifact_is_visible(&self.visible_artifact_ids, id)
    }

    pub fn request_video(&self, task: VideoThumbTask) {
        let generation = self.generation.load(Ordering::Relaxed);
        tracing::debug!(
            id = %task.book_id.0.to_hex(),
            path = %task.path.display(),
            width = task.target_width,
            "video thumb request"
        );
        self.lanes.mark_request_pending(ThumbnailLane::Video);
        if self
            .video_req_tx
            .send(VideoReq::Task(task, generation, RequestOrigin::Visible))
            .is_err()
        {
            self.lanes.retire_request_pending(ThumbnailLane::Video);
        }
    }

    pub fn start_video_preview(&self, task: VideoPreviewTask) {
        clear_animated_preview_result(&self.animated_result);
        self.preview_control
            .submit(PreviewCommand::StartVideo(task));
    }

    pub fn start_animated_preview(&self, task: AnimatedPreviewTask) {
        clear_animated_preview_result(&self.animated_result);
        self.preview_control
            .submit(PreviewCommand::StartAnimated(task));
    }

    pub fn request_animated_preview_scrub_with_abandon(
        &self,
        task: AnimatedPreviewTask,
        abandon_bucket_mask: u64,
    ) {
        submit_animated_preview_scrub(
            &self.preview_control,
            &self.animated_result,
            task,
            abandon_bucket_mask,
        );
    }

    pub fn cancel_animated_preview_scrub(&self, session_id: u64, bucket: u16) -> bool {
        self.preview_control
            .cancel_pending_animated_scrub(session_id, bucket)
    }

    pub fn abandon_animated_preview_scrub(&self, session_id: u64, buckets: u64) -> bool {
        self.preview_control
            .submit_if_idle(PreviewCommand::AnimatedScrubAbandon {
                session_id,
                buckets,
            })
    }

    pub fn resume_animated_preview_auto(&self, task: AnimatedPreviewTask) {
        clear_animated_preview_result(&self.animated_result);
        self.preview_control
            .submit(PreviewCommand::AnimatedAuto(task));
    }

    pub fn start_static_preview(&self, task: StaticPreviewTask) {
        clear_animated_preview_result(&self.animated_result);
        self.preview_control
            .submit(PreviewCommand::StartStatic(task));
    }

    pub fn request_video_preview_scene(&self, session_id: u64, scene_percent: u8) {
        self.preview_control.submit(PreviewCommand::VideoScene {
            session_id,
            scene_percent,
        });
    }

    pub fn request_static_preview_page(&self, task: StaticPreviewTask) {
        self.preview_control
            .submit(PreviewCommand::StaticPage(task));
    }

    pub fn stop_preview(&self) {
        clear_animated_preview_result(&self.animated_result);
        self.preview_control.submit(PreviewCommand::Stop);
    }

    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn current_artifact_generation(&self) -> u64 {
        self.artifact_generation.load(Ordering::Relaxed)
    }

    pub fn update_global_goal_for_library(&self, path: &Path) {
        self.lanes.update_global_goal(path);
    }

    pub fn clear_pending_tasks(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.background_scheduler_ready
            .store(false, Ordering::Release);
        self.stop_preview();
        self.display_mailbox.clear();
        if let Ok(mut visible) = self.visible_artifact_ids.lock() {
            visible.clear();
        }
        let _ = self
            .background_scheduler_tx
            .send(BackgroundSchedulerCommand::Clear);
        let _ = self.req_tx.send(WorkerReq::ClearPending);
    }

    pub fn clear_cache_state(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.artifact_generation.fetch_add(1, Ordering::SeqCst);
        self.background_scheduler_ready
            .store(false, Ordering::Release);
        self.stop_preview();
        self.display_mailbox.clear();
        if let Ok(mut visible) = self.visible_artifact_ids.lock() {
            visible.clear();
        }
        let _ = self
            .background_scheduler_tx
            .send(BackgroundSchedulerCommand::Clear);
        let _ = self.req_tx.send(WorkerReq::ClearCaches);
        let _ = self.video_req_tx.send(VideoReq::ClearCache);
    }

    pub fn remove_book_cache(&self, id: BookId) {
        self.artifact_generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.req_tx.send(WorkerReq::RemoveArchiveCache(id));
    }

    pub fn remove_video_cache(&self, id: BookId) {
        self.artifact_generation.fetch_add(1, Ordering::SeqCst);
        self.stop_preview();
        let _ = self.req_tx.send(WorkerReq::RemoveVideoCache(id.clone()));
        let _ = self.video_req_tx.send(VideoReq::RemoveCache(id.clone()));
    }

    pub fn shutdown(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        clear_animated_preview_result(&self.animated_result);
        self.display_mailbox.close();
        let _ = self
            .background_scheduler_tx
            .send(BackgroundSchedulerCommand::Shutdown);
        if let Ok(mut handle) = self.background_scheduler_handle.lock() {
            if let Some(handle) = handle.take() {
                let _ = handle.join();
            }
        }
        let _ = self.req_tx.send(WorkerReq::Shutdown);
        let _ = self.video_req_tx.send(VideoReq::Shutdown);
        let _ = self.video_background_req_tx.send(VideoReq::Shutdown);
        self.preview_control.shutdown();
    }

    pub fn try_recv(&self) -> Option<WorkerMsg> {
        let normal = match self.resp_rx.lock() {
            Ok(rx) => rx.try_recv().ok(),
            Err(_) => {
                tracing::error!("thumb worker resp_rx mutex poisoned");
                None
            }
        };
        if normal.is_some() {
            return normal;
        }
        match self.animated_result.lock() {
            Ok(mut result) => result.take(),
            Err(_) => {
                tracing::error!("thumb worker animated result mutex poisoned");
                None
            }
        }
    }
}

impl Drop for ThumbWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ── ワーカースレッド本体 ──────────────────────────────────────────────────────

struct WorkerMainContext {
    req_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerReq>,
    video_req_rx: tokio::sync::mpsc::UnboundedReceiver<VideoReq>,
    video_background_req_rx: tokio::sync::mpsc::UnboundedReceiver<VideoReq>,
    req_tx: tokio::sync::mpsc::UnboundedSender<WorkerReq>,
    resp_tx: std::sync::mpsc::Sender<WorkerMsg>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
    artifact_generation: Arc<AtomicU64>,
    artifact_gate: Arc<RwLock<()>>,
    lanes: Arc<ThumbnailLaneState>,
    display_mailbox: Arc<DisplayThumbMailbox>,
    visible_artifact_ids: Arc<Mutex<HashSet<BookId>>>,
    base_goal: usize,
}

fn worker_main(context: WorkerMainContext) {
    let WorkerMainContext {
        mut req_rx,
        video_req_rx,
        video_background_req_rx,
        req_tx,
        resp_tx,
        repaint,
        generation,
        artifact_generation,
        artifact_gate,
        lanes,
        display_mailbox,
        visible_artifact_ids,
        base_goal,
    } = context;

    let disk_cache = match DiskCache::open(DiskCache::default_root())
        .or_else(|_| DiskCache::open(std::env::temp_dir().join("cbz-thumbs")))
    {
        Ok(cache) => cache,
        Err(e) => {
            tracing::error!("disk cache open failed; thumb worker disabled: {e}");
            return;
        }
    };

    let disk_cache = Arc::new(disk_cache);
    let page_map_cache = match PageMapDiskCache::open(PageMapDiskCache::default_root()).or_else(
        |_| {
            PageMapDiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("page_maps"),
            )
        },
    ) {
        Ok(cache) => Some(Arc::new(cache)),
        Err(e) => {
            tracing::warn!(
                "page map cache open failed; continuing thumb worker in thumbnail-only mode: {e}"
            );
            None
        }
    };
    let artifact_failure_cache = match ArtifactFailureDiskCache::open(
        ArtifactFailureDiskCache::default_root(),
    )
    .or_else(|_| {
        ArtifactFailureDiskCache::open(
            std::env::temp_dir()
                .join(crate::app_identity::app_data_dir())
                .join("artifact_failures"),
        )
    }) {
        Ok(cache) => Some(Arc::new(cache)),
        Err(e) => {
            tracing::warn!(
                "artifact failure cache open failed; continuing without failure suppression: {e}"
            );
            None
        }
    };
    let page_map_status_notifier = {
        let resp_tx = resp_tx.clone();
        let repaint = repaint.clone();
        let visible_artifact_ids = Arc::clone(&visible_artifact_ids);
        Arc::new(move |status: PageMapStatus| {
            let visible = background_artifact_is_visible(&visible_artifact_ids, &status.book_id);
            let _ = resp_tx.send(WorkerMsg::PageMapStatus(status));
            if visible {
                repaint.request_repaint();
            }
        })
    };
    let page_map_coordinator = Arc::new(PageMapCoordinator::new(
        Arc::clone(&generation),
        Arc::clone(&artifact_generation),
        Arc::clone(&artifact_gate),
        artifact_failure_cache.as_ref().map(Arc::clone),
        Some(page_map_status_notifier),
    ));
    let shared = Arc::new(WorkerShared {
        mem_cache: ThumbMemCache::new(THUMB_MEM_CACHE_MAX_BYTES),
        disk_cache: Arc::clone(&disk_cache),
        page_map_cache,
        artifact_failure_cache,
        page_map_coordinator,
        artifact_generation: Arc::clone(&artifact_generation),
        artifact_gate: Arc::clone(&artifact_gate),
        lanes,
        next_flight_id: AtomicU64::new(0),
        in_flight: Arc::new(Mutex::new(HashSet::new())),
        video_in_flight: Arc::new(Mutex::new(HashSet::new())),
        pruned_revisions: Arc::new(Mutex::new(HashSet::new())),
        req_tx,
    });

    let max_blocking = (base_goal * 4).max(32);
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(max_blocking)
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("thumb worker runtime init failed: {e}");
            return;
        }
    };

    rt.block_on(async move {
        // Image/Video が共有する Global thumbnail permit budget。
        let thumbnail_sem = Arc::new(Semaphore::new(base_goal));
        // 表示要求に追従する旧世代掃除は 1 本に絞り、並列 I/O を増やさない。
        let prune_sem = Arc::new(Semaphore::new(1));

        tracing::info!(
            base_goal,
            max_blocking,
            normal_timeout_s = NORMAL_TIMEOUT.as_secs(),
            "thumb-worker started"
        );

        // ── 通常キュー処理ループ ─────────────────────────────────────────────
        // timeout では task を再起動せず、進行中の処理は背景継続に任せる。
        let normal_loop = tokio::spawn({
            let shared = Arc::clone(&shared);
            let resp_tx = resp_tx.clone();
            let repaint = repaint.clone();
            let thumbnail_sem = Arc::clone(&thumbnail_sem);
            let prune_sem = Arc::clone(&prune_sem);
            let generation = Arc::clone(&generation);
            let display_mailbox = Arc::clone(&display_mailbox);
            let visible_artifact_ids = Arc::clone(&visible_artifact_ids);
            async move {
                while let Some(req) = req_rx.recv().await {
                    match req {
                        WorkerReq::ClearPending => {
                            display_mailbox.clear();
                            shared.lanes.reset_transient();
                            shared.clear_in_flight();
                            shared.page_map_coordinator.clear_all();
                            continue;
                        }
                        WorkerReq::ClearCaches => {
                            display_mailbox.clear();
                            shared.lanes.reset_transient();
                            shared.mem_cache.clear();
                            shared.clear_in_flight();
                            shared.clear_pruned_revisions();
                            shared.page_map_coordinator.clear_all();
                            continue;
                        }
                        WorkerReq::RemoveArchiveCache(id) => {
                            shared.lanes.reset_transient();
                            let removed = shared.mem_cache.remove_by_book_id(&id);
                            shared.remove_in_flight_by_book_id(&id);
                            shared.remove_pruned_revisions_by_book_id(&id);
                            shared.page_map_coordinator.remove_by_book_id(&id);
                            tracing::debug!(
                                id = %id.0.to_hex(),
                                removed,
                                "thumb worker: remove archive cache"
                            );
                            continue;
                        }
                        WorkerReq::RemoveVideoCache(id) => {
                            shared.lanes.reset_transient();
                            let removed = shared.mem_cache.remove_by_book_id(&id);
                            shared.remove_in_flight_by_book_id(&id);
                            shared.remove_pruned_revisions_by_book_id(&id);
                            tracing::debug!(
                                id = %id.0.to_hex(),
                                removed,
                                "thumb worker: remove video cache"
                            );
                            continue;
                        }
                        WorkerReq::PruneObsoleteArtifacts {
                            id,
                            source_path,
                            source_revision,
                        } => {
                            let shared = Arc::clone(&shared);
                            let prune_sem = Arc::clone(&prune_sem);
                            tokio::spawn(async move {
                                let Ok(_permit) = prune_sem.acquire_owned().await else {
                                    return;
                                };
                                let _ = tokio::task::spawn_blocking(move || {
                                    shared.prune_obsolete_artifacts(
                                        &id,
                                        source_path.as_ref(),
                                        &source_revision,
                                    );
                                })
                                .await;
                            });
                            continue;
                        }
                        WorkerReq::PruneVideoObsoleteArtifacts {
                            id,
                            source_path,
                            source_revision,
                        } => {
                            let shared = Arc::clone(&shared);
                            let prune_sem = Arc::clone(&prune_sem);
                            tokio::spawn(async move {
                                let Ok(_permit) = prune_sem.acquire_owned().await else {
                                    return;
                                };
                                let _ = tokio::task::spawn_blocking(move || {
                                    shared.prune_video_obsolete_artifacts(
                                        &id,
                                        source_path.as_ref(),
                                        &source_revision,
                                    );
                                })
                                .await;
                            });
                            continue;
                        }
                        WorkerReq::Shutdown => {
                            display_mailbox.close();
                            shared.lanes.reset_transient();
                            break;
                        }
                        WorkerReq::PageMapOnly(task, task_gen) => {
                            if task_gen != generation.load(Ordering::Relaxed) {
                                continue;
                            }
                            let shared = Arc::clone(&shared);
                            let generation = Arc::clone(&generation);
                            tokio::spawn(async move {
                                if let Some(deferred) = page_map_cache_miss_deferred_for_task(
                                    &task,
                                    &shared,
                                    &generation,
                                    task_gen,
                                ) {
                                    deferred.execute().await;
                                }
                            });
                        }
                        WorkerReq::Task(task, task_gen, origin) => {
                            let _pending = PendingRequestGuard::new(
                                Arc::clone(&shared.lanes),
                                ThumbnailLane::Image,
                            );
                            if task_gen != generation.load(Ordering::Relaxed) {
                                continue;
                            }
                            let Some((permit, image_running)) = acquire_thumbnail_permit(
                                Arc::clone(&thumbnail_sem),
                                Arc::clone(&shared.lanes),
                                ThumbnailLane::Image,
                                Some(Arc::clone(&display_mailbox)),
                                false,
                            )
                            .await
                            else {
                                tracing::error!("thumbnail semaphore closed");
                                break;
                            };
                            let Some(flight) = shared.begin_task(&task) else {
                                drop(permit);
                                drop(image_running);
                                tracing::debug!(
                                    id = %task.book_id.0.to_hex(),
                                    width = task.target_width,
                                    "duplicate thumb task skipped"
                                );
                                continue;
                            };
                            let tx = resp_tx.clone();
                            let repaint = repaint.clone();
                            let generation = Arc::clone(&generation);
                            let visible_artifact_ids_for_task = Arc::clone(&visible_artifact_ids);
                            tokio::spawn({
                                let shared = Arc::clone(&shared);
                                async move {
                                    run_thumb_task(
                                        task,
                                        ThumbTaskRuntime {
                                            shared,
                                            tx,
                                            repaint,
                                            generation,
                                            origin,
                                            visible_artifact_ids: visible_artifact_ids_for_task,
                                        },
                                        permit,
                                        Some(NORMAL_TIMEOUT),
                                        "normal",
                                        task_gen,
                                        flight,
                                    )
                                    .await;
                                    drop(image_running);
                                }
                            });
                        }
                    }
                }
            }
        });

        let video_loop = tokio::spawn({
            let shared = Arc::clone(&shared);
            let tx = resp_tx.clone();
            let repaint = repaint.clone();
            let generation = Arc::clone(&generation);
            let thumbnail_sem = Arc::clone(&thumbnail_sem);
            let visible_artifact_ids = Arc::clone(&visible_artifact_ids);
            async move {
                video_worker_loop(
                    video_req_rx,
                    video_background_req_rx,
                    shared,
                    tx,
                    repaint,
                    generation,
                    thumbnail_sem,
                    Arc::clone(&visible_artifact_ids),
                )
                .await;
            }
        });

        let display_loop = tokio::spawn({
            let shared = Arc::clone(&shared);
            let tx = resp_tx.clone();
            let repaint = repaint.clone();
            let generation = Arc::clone(&generation);
            let thumbnail_sem = Arc::clone(&thumbnail_sem);
            let display_mailbox = Arc::clone(&display_mailbox);
            let visible_artifact_ids = Arc::clone(&visible_artifact_ids);
            async move {
                display_worker_loop(
                    display_mailbox,
                    shared,
                    tx,
                    repaint,
                    generation,
                    thumbnail_sem,
                    visible_artifact_ids,
                )
                .await;
            }
        });

        let _ = tokio::join!(normal_loop, video_loop, display_loop);
    });
}

async fn display_worker_loop(
    display_mailbox: Arc<DisplayThumbMailbox>,
    shared: Arc<WorkerShared>,
    resp_tx: std::sync::mpsc::Sender<WorkerMsg>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
    thumbnail_sem: Arc<Semaphore>,
    visible_artifact_ids: Arc<Mutex<HashSet<BookId>>>,
) {
    loop {
        let notified = display_mailbox.wake.notified();
        if !display_mailbox.has_pending() {
            if display_mailbox.is_closed() {
                return;
            }
            notified.await;
            continue;
        }

        let Some((permit, image_running)) = acquire_thumbnail_permit(
            Arc::clone(&thumbnail_sem),
            Arc::clone(&shared.lanes),
            ThumbnailLane::Image,
            Some(Arc::clone(&display_mailbox)),
            true,
        )
        .await
        else {
            if display_mailbox.is_closed() {
                return;
            }
            continue;
        };

        let Some((task, task_gen)) = display_mailbox.take_next() else {
            drop(permit);
            drop(image_running);
            continue;
        };
        if display_mailbox.is_closed() || task_gen != generation.load(Ordering::Relaxed) {
            drop(permit);
            drop(image_running);
            continue;
        }
        let Some(flight) = shared.begin_task(&task) else {
            drop(permit);
            drop(image_running);
            tracing::debug!(
                id = %task.book_id.0.to_hex(),
                width = task.target_width,
                "display thumb duplicate task skipped"
            );
            continue;
        };
        let tx = resp_tx.clone();
        let repaint = repaint.clone();
        let generation = Arc::clone(&generation);
        let visible_artifact_ids_for_task = Arc::clone(&visible_artifact_ids);
        tokio::spawn({
            let shared = Arc::clone(&shared);
            async move {
                run_thumb_task(
                    task,
                    ThumbTaskRuntime {
                        shared,
                        tx,
                        repaint,
                        generation,
                        origin: RequestOrigin::Visible,
                        visible_artifact_ids: visible_artifact_ids_for_task,
                    },
                    permit,
                    Some(NORMAL_TIMEOUT),
                    "display",
                    task_gen,
                    flight,
                )
                .await;
                drop(image_running);
            }
        });
    }
}

// ── タスク実行（normal / slow 共通）──────────────────────────────────────────

/// permit を保持したまま decode/resize を進め、結果は UI へ先に送る。
/// normal は timeout で離脱しても背景継続し、slow は完了または実エラーまで待つ。
async fn run_thumb_task(
    task: ThumbTask,
    runtime: ThumbTaskRuntime,
    permit: tokio::sync::OwnedSemaphorePermit,
    timeout: Option<Duration>,
    label: &'static str,
    task_gen: u64,
    flight: TaskFlightGuard,
) {
    let file_size_mb = std::fs::metadata(&task.path)
        .map(|m| m.len() / 1_048_576)
        .unwrap_or(0);
    let path_disp = task.path.display().to_string();

    let task_for_blocking = task.clone();
    let generation_for_blocking = Arc::clone(&runtime.generation);
    let shared_for_blocking = Arc::clone(&runtime.shared);
    let handle = tokio::task::spawn_blocking(move || {
        process_thumb(
            task_for_blocking,
            &shared_for_blocking,
            &generation_for_blocking,
            task_gen,
            runtime.origin,
        )
    });

    let (done_tx, done_rx) = oneshot::channel::<()>();
    let tx_for_watch = runtime.tx.clone();
    let repaint_for_watch = runtime.repaint.clone();
    let generation_for_watch = Arc::clone(&runtime.generation);
    let path_disp_for_watch = path_disp.clone();
    tokio::spawn(async move {
        let join_result = handle.await;
        match join_result {
            Ok((msg, deferred)) => {
                handle_thumb_result(
                    task,
                    msg,
                    deferred,
                    ThumbTaskResultContext {
                        shared: Arc::clone(&runtime.shared),
                        task_gen,
                        tx: tx_for_watch,
                        repaint: repaint_for_watch,
                        generation: generation_for_watch,
                        origin: runtime.origin,
                        display: label == "display",
                        visible_artifact_ids: Arc::clone(&runtime.visible_artifact_ids),
                    },
                )
                .await;
            }
            Err(join_err) => {
                tracing::error!(path = %path_disp_for_watch, "spawn_blocking panic: {join_err}");
                if task_gen == runtime.generation.load(Ordering::Relaxed)
                    && thumb_task_file_snapshot_matches(&task)
                {
                    let currently_visible = background_artifact_is_visible(
                        &runtime.visible_artifact_ids,
                        &task.book_id,
                    );
                    let should_notify = runtime.origin.is_visible() || currently_visible;
                    let should_repaint = label == "display" || currently_visible;
                    if should_notify {
                        let _ = runtime.tx.send(WorkerMsg::FailedWithRevision {
                            book_id: task.book_id.clone(),
                            expected_size: task.expected_size,
                            expected_modified: task.expected_modified,
                        });
                    }
                    if should_repaint {
                        runtime.repaint.request_repaint();
                    }
                }
            }
        }
        drop(flight);
        let _ = done_tx.send(());
    });

    if let Some(timeout) = timeout {
        match tokio::time::timeout(timeout, done_rx).await {
            Ok(_) => {}
            Err(_) => {
                tracing::warn!(
                    path = %path_disp,
                    size_mb = file_size_mb,
                    "normal-slot timeout; processing continues in background"
                );
            }
        }
    } else {
        let _ = done_rx.await;
    }
    drop(permit);
}

async fn handle_thumb_result(
    task: ThumbTask,
    msg: WorkerMsg,
    deferred: Option<DeferredCache>,
    runtime: ThumbTaskResultContext,
) {
    if runtime.task_gen != runtime.generation.load(Ordering::Relaxed) {
        return;
    }
    let currently_visible =
        background_artifact_is_visible(&runtime.visible_artifact_ids, &task.book_id);
    let should_notify = runtime.origin.is_visible() || currently_visible;
    match msg {
        WorkerMsg::Ready(ready) => {
            clear_thumbnail_failure(&runtime.shared, &task);
            if currently_visible && !runtime.origin.is_visible() {
                runtime.shared.mem_cache.put(
                    task.book_id.clone(),
                    task.target_width,
                    Thumbnail {
                        width: ready.width,
                        height: ready.height,
                        pixels: Arc::clone(&ready.pixels),
                    },
                );
            }
            if should_notify {
                let _ = runtime.tx.send(WorkerMsg::Ready(ready));
            }
            if runtime.display || currently_visible {
                runtime.repaint.request_repaint();
            }
            // UI を先に返し、WebP 保存は後段で実行する。
            if let Some(dc) = deferred {
                tokio::spawn(async move {
                    dc.execute().await;
                });
            }
        }
        WorkerMsg::Stale(_) => {
            if runtime.display || (runtime.origin == RequestOrigin::Background && currently_visible)
            {
                let _ = runtime.tx.send(WorkerMsg::DisplayStale {
                    book_id: task.book_id.clone(),
                    expected_size: task.expected_size,
                    expected_modified: task.expected_modified,
                    target_width: task.target_width,
                    bypass_cache: task.bypass_cache,
                });
                if runtime.display || currently_visible {
                    runtime.repaint.request_repaint();
                }
            }
            // 通常・背景の古い結果は UI に流さない。差分 scan 側の再要求に任せる。
        }
        WorkerMsg::VideoReady(_) => unreachable!(),
        WorkerMsg::DisplayStale { .. } => unreachable!(),
        WorkerMsg::VideoPreviewReady(_)
        | WorkerMsg::VideoPreviewFailed(_)
        | WorkerMsg::VideoPreviewStale(_)
        | WorkerMsg::AnimatedPreviewReady(_)
        | WorkerMsg::AnimatedPreviewFailed(_)
        | WorkerMsg::AnimatedPreviewStale(_)
        | WorkerMsg::AnimatedPreviewUnavailable(_)
        | WorkerMsg::StaticPreviewReady(_)
        | WorkerMsg::StaticPreviewFailed(_)
        | WorkerMsg::StaticPreviewStale(_) => unreachable!(),
        WorkerMsg::VideoStale { .. } => unreachable!(),
        WorkerMsg::PageMapStatus(_) => {
            // Page Map 状態は後段の coordinator から直接 UI へ通知される。
        }
        WorkerMsg::Failed(id) => {
            debug_assert_eq!(id, task.book_id);
            if thumb_task_file_snapshot_matches(&task) {
                tracing::warn!(path = %task.path.display(), "thumb task failed");
                if should_notify {
                    let _ = runtime.tx.send(WorkerMsg::FailedWithRevision {
                        book_id: task.book_id.clone(),
                        expected_size: task.expected_size,
                        expected_modified: task.expected_modified,
                    });
                }
                if runtime.display || currently_visible {
                    runtime.repaint.request_repaint();
                }
            }
        }
        WorkerMsg::FailedPermanent(id) => {
            debug_assert_eq!(id, task.book_id);
            if thumb_task_file_snapshot_matches(&task) {
                tracing::info!(path = %task.path.display(), "thumb task permanent failed");
                mark_thumbnail_failure(&runtime.shared, &task);
                if should_notify {
                    let _ = runtime.tx.send(WorkerMsg::FailedPermanentWithRevision {
                        book_id: task.book_id.clone(),
                        expected_size: task.expected_size,
                        expected_modified: task.expected_modified,
                    });
                }
                if runtime.display || currently_visible {
                    runtime.repaint.request_repaint();
                }
            }
        }
        WorkerMsg::FailedWithRevision { .. } | WorkerMsg::FailedPermanentWithRevision { .. } => {
            unreachable!()
        }
    }
}

struct ThumbTaskRuntime {
    shared: Arc<WorkerShared>,
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
    origin: RequestOrigin,
    visible_artifact_ids: Arc<Mutex<HashSet<BookId>>>,
}

struct ThumbTaskResultContext {
    shared: Arc<WorkerShared>,
    task_gen: u64,
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
    origin: RequestOrigin,
    display: bool,
    visible_artifact_ids: Arc<Mutex<HashSet<BookId>>>,
}

fn mark_thumbnail_failure(shared: &WorkerShared, task: &ThumbTask) {
    if !thumb_task_file_snapshot_matches(task) {
        return;
    }
    let revision = SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    if let Some(cache) = shared.artifact_failure_cache.as_ref() {
        match cache.mark_failure_for_revision(&task.book_id, &revision, ArtifactKind::Thumbnail) {
            Ok(true) => {
                tracing::debug!(
                    id = %task.book_id.0.to_hex(),
                    source_revision = ?revision,
                    "thumbnail terminal failure cached"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    id = %task.book_id.0.to_hex(),
                    source_revision = ?revision,
                    error = %error,
                    "thumbnail failure cache save failed"
                );
            }
        }
    }
}

fn clear_thumbnail_failure(shared: &WorkerShared, task: &ThumbTask) {
    let revision = SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    if let Some(cache) = shared.artifact_failure_cache.as_ref() {
        match cache.clear_failure_for_revision(&task.book_id, &revision, ArtifactKind::Thumbnail) {
            Ok(true) => {
                tracing::debug!(
                    id = %task.book_id.0.to_hex(),
                    source_revision = ?revision,
                    "thumbnail failure cache cleared after success"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    id = %task.book_id.0.to_hex(),
                    source_revision = ?revision,
                    error = %error,
                    "thumbnail failure cache clear failed"
                );
            }
        }
    }
}

fn mark_thumbnail_failure_for_video(shared: &WorkerShared, task: &VideoThumbTask) {
    if !thumb_task_file_snapshot_matches_video(task) {
        return;
    }
    let revision = SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    if let Some(cache) = shared.artifact_failure_cache.as_ref() {
        match cache.mark_failure_for_revision(&task.book_id, &revision, ArtifactKind::Thumbnail) {
            Ok(true) => tracing::debug!(
                id = %task.book_id.0.to_hex(),
                source_revision = ?revision,
                "video thumbnail failure cached"
            ),
            Ok(false) => {}
            Err(error) => tracing::debug!(
                id = %task.book_id.0.to_hex(),
                source_revision = ?revision,
                error = %error,
                "video thumbnail failure cache save failed"
            ),
        }
    }
}

fn clear_thumbnail_failure_for_video(shared: &WorkerShared, task: &VideoThumbTask) {
    let revision = SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    if let Some(cache) = shared.artifact_failure_cache.as_ref() {
        match cache.clear_failure_for_revision(&task.book_id, &revision, ArtifactKind::Thumbnail) {
            Ok(true) => tracing::debug!(
                id = %task.book_id.0.to_hex(),
                source_revision = ?revision,
                "video thumbnail failure cache cleared after success"
            ),
            Ok(false) => {}
            Err(error) => tracing::debug!(
                id = %task.book_id.0.to_hex(),
                source_revision = ?revision,
                error = %error,
                "video thumbnail failure cache clear failed"
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    size: u64,
    modified: Option<SystemTime>,
}

impl FileSnapshot {
    fn read(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        Ok(Self {
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

struct VideoPreviewSession {
    session_id: u64,
    book_id: BookId,
    path: Arc<Path>,
    target_width: u16,
    expected_size: u64,
    expected_modified: Option<SystemTime>,
    duration: Duration,
    decoder: VideoDecoder,
}

#[derive(Clone)]
struct AnimatedPreviewSnapshot {
    pixels: Arc<[u8]>,
    frame_index: u64,
    delay_ms: u32,
    width: u16,
    height: u16,
}

struct AnimatedPreviewSession {
    task: AnimatedPreviewTask,
    raw: bytes::Bytes,
    source: img::WebpAnimFrameSource,
    scrub_source: Option<img::WebpAnimFrameSource>,
    scrub_timeline: Option<AnimatedPreviewTimeline>,
    scrub_next_frame_index: u64,
    scrub_snapshots: [Option<AnimatedPreviewSnapshot>; ANIMATED_PREVIEW_SCRUB_BUCKET_SLOTS],
    last_successful_scrub_bucket: Option<u16>,
    next_due: Option<Instant>,
    next_frame_index: u64,
}

/// Time-scrub metadata retains only frame start times and total duration; it is
/// collected from the WebP container without decoding frame pixels.
#[derive(Debug, PartialEq, Eq)]
struct AnimatedPreviewTimeline {
    frame_starts_ms: Vec<u64>,
    total_duration_ms: u64,
}

impl AnimatedPreviewTimeline {
    fn frame_index_for_bucket(&self, bucket: u16, bucket_count: u16) -> Option<u64> {
        if self.frame_starts_ms.is_empty() || bucket_count == 0 || bucket >= bucket_count {
            return None;
        }
        let target_ms = if bucket_count == 1 {
            0
        } else {
            self.total_duration_ms
                .saturating_sub(1)
                .saturating_mul(bucket as u64)
                / u64::from(bucket_count - 1)
        };
        let index = self
            .frame_starts_ms
            .partition_point(|start_ms| *start_ms <= target_ms)
            .saturating_sub(1);
        Some(index as u64)
    }
}

struct StaticPreviewSession {
    task: StaticPreviewTask,
    reader: Box<dyn BookReader>,
}

fn preview_worker(
    control: Arc<PreviewControl>,
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    animated_result: Arc<Mutex<Option<WorkerMsg>>>,
    repaint: RepaintNotifier,
) {
    let mut video_session: Option<VideoPreviewSession> = None;
    let mut animated_session: Option<AnimatedPreviewSession> = None;
    let mut static_session: Option<StaticPreviewSession> = None;
    let mut deferred_command: Option<PreviewCommand> = None;
    loop {
        let command = if let Some(deferred) = deferred_command.take() {
            // A command arriving after a frame boundary is newer than the
            // deferred command; keep the replaceable latest-command-wins
            // semantics even while a scrub decode is being resumed.
            control.take_pending().unwrap_or(deferred)
        } else {
            let wait = animated_session
                .as_ref()
                .and_then(|session| session.next_due)
                .map(|deadline| control.wait_until(deadline));
            match wait {
                Some(PreviewWait::Deadline) => {
                    if let Some(session) = animated_session.as_mut() {
                        decode_animated_preview_frame(session, &animated_result, &repaint)
                    }
                    continue;
                }
                Some(PreviewWait::Command(command)) => command,
                None => control.take(),
            }
        };

        match command {
            PreviewCommand::StartVideo(task) => {
                clear_animated_preview_result(&animated_result);
                animated_session = None;
                static_session = None;
                video_session = None;
                if !preview_task_file_snapshot_matches(&task) {
                    send_video_preview_stale(&task, &tx, &repaint);
                    continue;
                }
                let decoder = match VideoDecoder::open(task.path.as_ref())
                    .output_format(PixelFormat::Rgba)
                    .build()
                {
                    Ok(decoder) => decoder,
                    Err(error) => {
                        tracing::debug!(
                            id = %task.book_id.0.to_hex(),
                            path = %task.path.display(),
                            error = %error,
                            "video preview open failed"
                        );
                        send_video_preview_failed(&task, &tx, &repaint);
                        continue;
                    }
                };
                let Some(duration) = decoder
                    .duration_opt()
                    .filter(|duration| !duration.is_zero())
                else {
                    send_video_preview_failed(&task, &tx, &repaint);
                    continue;
                };
                video_session = Some(VideoPreviewSession {
                    session_id: task.session_id,
                    book_id: task.book_id.clone(),
                    path: Arc::clone(&task.path),
                    target_width: task.target_width,
                    expected_size: task.expected_size,
                    expected_modified: task.expected_modified,
                    duration,
                    decoder,
                });
                if let Some(current) = video_session.as_mut() {
                    decode_video_preview_scene(current, task.scene_percent, &tx, &repaint);
                }
            }
            PreviewCommand::StartAnimated(task) => {
                clear_animated_preview_result(&animated_result);
                video_session = None;
                static_session = None;
                animated_session = None;
                if let Some(mut current) =
                    open_animated_preview_session(task, &animated_result, &repaint)
                {
                    if current.task.scrub_bucket.is_some() {
                        decode_animated_preview_scrub(
                            &mut current,
                            &animated_result,
                            &repaint,
                            &control,
                            &mut deferred_command,
                        );
                    } else {
                        decode_animated_preview_frame(&mut current, &animated_result, &repaint);
                    }
                    animated_session = Some(current);
                }
            }
            PreviewCommand::AnimatedScrub(task) => {
                if animated_session
                    .as_ref()
                    .is_some_and(|current| animated_preview_session_matches(current, &task))
                {
                    let current = animated_session.as_mut().expect("animated session exists");
                    current.last_successful_scrub_bucket = clear_abandoned_scrub_guard(
                        current.task.scrub_bucket,
                        current.last_successful_scrub_bucket,
                        task.abandon_bucket_mask,
                    );
                    if should_skip_animated_scrub_command(
                        current.task.scrub_bucket,
                        current.last_successful_scrub_bucket,
                        task.scrub_bucket,
                    ) {
                        current.next_due = None;
                        continue;
                    }
                    current.task = task;
                    current.next_due = None;
                    decode_animated_preview_scrub(
                        current,
                        &animated_result,
                        &repaint,
                        &control,
                        &mut deferred_command,
                    );
                } else {
                    video_session = None;
                    static_session = None;
                    animated_session = None;
                    if let Some(mut current) =
                        open_animated_preview_session(task, &animated_result, &repaint)
                    {
                        current.next_due = None;
                        decode_animated_preview_scrub(
                            &mut current,
                            &animated_result,
                            &repaint,
                            &control,
                            &mut deferred_command,
                        );
                        animated_session = Some(current);
                    }
                }
            }
            PreviewCommand::AnimatedScrubAbandon {
                session_id,
                buckets,
            } => {
                let Some(current) = animated_session.as_mut() else {
                    continue;
                };
                if current.task.session_id != session_id {
                    continue;
                }
                let task = current.task.clone();
                let frame_index = current.next_frame_index;
                current.last_successful_scrub_bucket = clear_abandoned_scrub_guard(
                    current.task.scrub_bucket,
                    current.last_successful_scrub_bucket,
                    buckets,
                );
                send_animated_preview_abandoned(
                    &task,
                    buckets,
                    frame_index,
                    &animated_result,
                    &repaint,
                );
            }
            PreviewCommand::AnimatedAuto(task) => {
                if animated_session
                    .as_ref()
                    .is_some_and(|current| animated_preview_session_matches(current, &task))
                {
                    let current = animated_session.as_mut().expect("animated session exists");
                    current.task.scrub_bucket = None;
                    current.task.abandon_bucket_mask = 0;
                    current.last_successful_scrub_bucket = None;
                    current.next_due = Some(Instant::now());
                } else {
                    video_session = None;
                    static_session = None;
                    animated_session = None;
                    if let Some(mut current) =
                        open_animated_preview_session(task, &animated_result, &repaint)
                    {
                        decode_animated_preview_frame(&mut current, &animated_result, &repaint);
                        animated_session = Some(current);
                    }
                }
            }
            PreviewCommand::StartStatic(task) => {
                clear_animated_preview_result(&animated_result);
                video_session = None;
                animated_session = None;
                static_session = match open_static_preview_session(&task) {
                    Ok(session) => Some(session),
                    Err(stale) => {
                        send_static_preview_result(&task, stale, &animated_result, &repaint);
                        None
                    }
                };
                if let Some(current) = static_session.as_mut() {
                    decode_static_preview_page(current, &task, &animated_result, &repaint);
                }
            }
            PreviewCommand::VideoScene {
                session_id,
                scene_percent,
            } => {
                let Some(current) = video_session.as_mut() else {
                    continue;
                };
                if current.session_id != session_id {
                    continue;
                }
                let task = VideoPreviewTask {
                    session_id: current.session_id,
                    book_id: current.book_id.clone(),
                    path: Arc::clone(&current.path),
                    target_width: current.target_width,
                    expected_size: current.expected_size,
                    expected_modified: current.expected_modified,
                    scene_percent,
                };
                if !preview_task_file_snapshot_matches(&task) {
                    send_video_preview_stale(&task, &tx, &repaint);
                    video_session = None;
                    continue;
                }
                decode_video_preview_scene(current, scene_percent, &tx, &repaint);
            }
            PreviewCommand::StaticPage(task) => {
                if !static_session
                    .as_ref()
                    .is_some_and(|session| static_preview_session_matches(session, &task))
                {
                    video_session = None;
                    animated_session = None;
                    static_session = match open_static_preview_session(&task) {
                        Ok(session) => Some(session),
                        Err(stale) => {
                            send_static_preview_result(&task, stale, &animated_result, &repaint);
                            None
                        }
                    };
                }
                if let Some(current) = static_session.as_mut() {
                    decode_static_preview_page(current, &task, &animated_result, &repaint);
                }
            }
            PreviewCommand::Stop => {
                clear_animated_preview_result(&animated_result);
                video_session = None;
                animated_session = None;
                static_session = None;
            }
            PreviewCommand::Shutdown => {
                clear_animated_preview_result(&animated_result);
                break;
            }
        }
    }
}

fn open_animated_preview_session(
    task: AnimatedPreviewTask,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) -> Option<AnimatedPreviewSession> {
    if !animated_preview_file_snapshot_matches(&task) {
        send_animated_preview_stale(&task, 0, animated_result, repaint);
        return None;
    }
    let raw = match read_thumb_source_bytes(&task.path) {
        Ok(raw) => raw,
        Err(error) => {
            tracing::debug!(
                id = %task.book_id.0.to_hex(),
                path = %task.path.display(),
                error = %error,
                "animated preview source read failed"
            );
            send_animated_preview_failed(&task, 0, animated_result, repaint);
            return None;
        }
    };
    if !animated_preview_file_snapshot_matches(&task) {
        send_animated_preview_stale(&task, 0, animated_result, repaint);
        return None;
    }
    if !img::is_animated_webp_fast(raw.as_ref()) {
        send_animated_preview_unavailable(&task, animated_result, repaint);
        return None;
    }
    let source = match img::WebpAnimFrameSource::new(raw.as_ref()) {
        Ok(source) => source,
        Err(error) => {
            tracing::debug!(
                id = %task.book_id.0.to_hex(),
                path = %task.path.display(),
                error = %error,
                "animated preview decoder open failed"
            );
            send_animated_preview_failed(&task, 0, animated_result, repaint);
            return None;
        }
    };
    Some(AnimatedPreviewSession {
        task,
        raw,
        source,
        scrub_source: None,
        scrub_timeline: None,
        scrub_next_frame_index: 0,
        scrub_snapshots: std::array::from_fn(|_| None),
        last_successful_scrub_bucket: None,
        next_due: None,
        next_frame_index: 0,
    })
}

fn animated_preview_session_matches(
    session: &AnimatedPreviewSession,
    task: &AnimatedPreviewTask,
) -> bool {
    session.task.session_id == task.session_id
        && session.task.book_id == task.book_id
        && session.task.path.as_ref() == task.path.as_ref()
        && session.task.target_width == task.target_width
        && session.task.expected_size == task.expected_size
        && session.task.expected_modified == task.expected_modified
        && session.task.scrub_bucket_count == task.scrub_bucket_count
}

fn should_skip_animated_scrub_command(
    current_task_bucket: Option<u16>,
    last_successful_bucket: Option<u16>,
    requested_bucket: Option<u16>,
) -> bool {
    requested_bucket.is_some()
        && current_task_bucket == requested_bucket
        && last_successful_bucket == requested_bucket
}

fn clear_abandoned_scrub_guard(
    current_task_bucket: Option<u16>,
    last_successful_bucket: Option<u16>,
    abandoned_bucket_mask: u64,
) -> Option<u16> {
    if current_task_bucket.is_some()
        && current_task_bucket == last_successful_bucket
        && current_task_bucket
            .is_some_and(|bucket| scrub_bucket_bit(bucket) & abandoned_bucket_mask != 0)
    {
        None
    } else {
        last_successful_bucket
    }
}

fn scrub_bucket_bit(bucket: u16) -> u64 {
    1u64.checked_shl(u32::from(bucket)).unwrap_or(0)
}

fn scrub_snapshot_slot(bucket: u16, bucket_count: u16) -> Option<usize> {
    if bucket_count == 0 || bucket >= bucket_count {
        return None;
    }
    let slot = usize::from(bucket);
    (slot < ANIMATED_PREVIEW_SCRUB_BUCKET_SLOTS).then_some(slot)
}

fn ensure_animated_preview_scrub_timeline(
    session: &mut AnimatedPreviewSession,
    control: &PreviewControl,
) -> anyhow::Result<Option<PreviewCommand>> {
    if session.scrub_source.is_none() {
        session.scrub_source = Some(img::WebpAnimFrameSource::new(session.raw.as_ref())?);
    }
    if session.scrub_timeline.is_some() {
        return Ok(None);
    }

    let durations = session
        .scrub_source
        .as_ref()
        .expect("scrub source exists before timeline metadata lookup")
        .frame_durations()?;
    let mut frame_starts_ms = Vec::with_capacity(durations.len());
    let mut total_duration_ms = 0_u64;
    for delay_ms in durations {
        frame_starts_ms.push(total_duration_ms);
        total_duration_ms = total_duration_ms
            .checked_add(u64::from(delay_ms))
            .ok_or_else(|| anyhow::anyhow!("animated WebP timeline duration overflowed"))?;
    }
    if frame_starts_ms.is_empty() || total_duration_ms == 0 {
        return Err(anyhow::anyhow!("animated WebP has no usable timeline"));
    }
    session.scrub_timeline = Some(AnimatedPreviewTimeline {
        frame_starts_ms,
        total_duration_ms,
    });
    session.scrub_next_frame_index = 0;
    Ok(control.take_pending())
}

fn publish_animated_preview_snapshot(
    task: &AnimatedPreviewTask,
    snapshot: &AnimatedPreviewSnapshot,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) -> bool {
    if !animated_preview_file_snapshot_matches(task) {
        send_animated_preview_stale(task, snapshot.frame_index, animated_result, repaint);
        return false;
    }
    replace_animated_preview_result(
        animated_result,
        WorkerMsg::AnimatedPreviewReady(AnimatedPreviewReady {
            session_id: task.session_id,
            book_id: task.book_id.clone(),
            path: Arc::clone(&task.path),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            frame_index: snapshot.frame_index,
            delay_ms: snapshot.delay_ms,
            scrub_bucket: task.scrub_bucket,
            abandon_ack_bucket_mask: task.abandon_bucket_mask,
            pixels: Arc::clone(&snapshot.pixels),
            width: snapshot.width,
            height: snapshot.height,
        }),
    );
    repaint.request_repaint();
    true
}

fn decode_animated_preview_scrub(
    session: &mut AnimatedPreviewSession,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
    control: &PreviewControl,
    deferred_command: &mut Option<PreviewCommand>,
) {
    let task = session.task.clone();
    let Some(bucket) = task.scrub_bucket else {
        return;
    };
    if !animated_preview_file_snapshot_matches(&task) {
        send_animated_preview_stale(&task, 0, animated_result, repaint);
        return;
    }
    if let Some(command) = control.take_pending() {
        *deferred_command = Some(command);
        return;
    }
    match ensure_animated_preview_scrub_timeline(session, control) {
        Ok(Some(command)) => {
            *deferred_command = Some(command);
            return;
        }
        Ok(None) => {}
        Err(_) => {
            send_animated_preview_failed(&task, 0, animated_result, repaint);
            return;
        }
    }
    let Some(bucket_slot) = scrub_snapshot_slot(bucket, task.scrub_bucket_count) else {
        send_animated_preview_failed(&task, 0, animated_result, repaint);
        return;
    };
    if let Some(snapshot) = session.scrub_snapshots[bucket_slot].clone() {
        if publish_animated_preview_snapshot(&task, &snapshot, animated_result, repaint) {
            session.last_successful_scrub_bucket = Some(bucket);
        }
        return;
    }
    let Some(frame_index) = session
        .scrub_timeline
        .as_ref()
        .and_then(|timeline| timeline.frame_index_for_bucket(bucket, task.scrub_bucket_count))
    else {
        send_animated_preview_failed(&task, 0, animated_result, repaint);
        return;
    };

    if frame_index < session.scrub_next_frame_index {
        session
            .scrub_source
            .as_mut()
            .expect("scrub source exists after timeline initialization")
            .reset();
        session.scrub_next_frame_index = 0;
    }

    while session.scrub_next_frame_index <= frame_index {
        if !animated_preview_file_snapshot_matches(&task) {
            send_animated_preview_stale(
                &task,
                session.scrub_next_frame_index,
                animated_result,
                repaint,
            );
            return;
        }
        let current_frame_index = session.scrub_next_frame_index;
        let frame = session
            .scrub_source
            .as_mut()
            .expect("scrub source exists after timeline initialization")
            .next_frame();
        let Some(frame) = (match frame {
            Ok(frame) => frame,
            Err(_) => {
                send_animated_preview_failed(&task, current_frame_index, animated_result, repaint);
                return;
            }
        }) else {
            send_animated_preview_failed(&task, current_frame_index, animated_result, repaint);
            return;
        };

        let mut bucket_matches = [false; ANIMATED_PREVIEW_SCRUB_BUCKET_SLOTS];
        if let Some(timeline) = session.scrub_timeline.as_ref() {
            for candidate in 0..task
                .scrub_bucket_count
                .min(ANIMATED_PREVIEW_SCRUB_BUCKET_SLOTS as u16)
            {
                if timeline.frame_index_for_bucket(candidate, task.scrub_bucket_count)
                    == Some(current_frame_index)
                {
                    bucket_matches[usize::from(candidate)] = true;
                }
            }
        }
        if bucket_matches.iter().any(|matched| *matched) {
            let delay_ms = frame.delay_ms;
            let resized = match img::resize_to_width(frame.image, task.target_width as u32) {
                Ok(resized) => resized,
                Err(_) => {
                    send_animated_preview_failed(
                        &task,
                        current_frame_index,
                        animated_result,
                        repaint,
                    );
                    return;
                }
            };
            let snapshot = AnimatedPreviewSnapshot {
                pixels: Arc::from(resized.pixels),
                frame_index: current_frame_index,
                delay_ms,
                width: resized.width as u16,
                height: resized.height as u16,
            };
            for (slot, matched) in bucket_matches.into_iter().enumerate() {
                if matched {
                    session.scrub_snapshots[slot] = Some(snapshot.clone());
                }
            }
        } else {
            drop(frame.image);
        }
        session.scrub_next_frame_index = current_frame_index.saturating_add(1);

        if let Some(command) = control.take_pending() {
            *deferred_command = Some(command);
            return;
        }
    }

    let Some(snapshot) = session.scrub_snapshots[bucket_slot].clone() else {
        send_animated_preview_failed(&task, frame_index, animated_result, repaint);
        return;
    };
    if publish_animated_preview_snapshot(&task, &snapshot, animated_result, repaint) {
        session.last_successful_scrub_bucket = Some(bucket);
    }
}

fn decode_animated_preview_frame(
    session: &mut AnimatedPreviewSession,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) {
    let frame_index = session.next_frame_index;
    let task = &session.task;
    if !animated_preview_file_snapshot_matches(task) {
        send_animated_preview_stale(task, frame_index, animated_result, repaint);
        session.next_due = None;
        return;
    }

    if !session.source.has_more_frames() {
        match img::WebpAnimFrameSource::new(session.raw.as_ref()) {
            Ok(source) => {
                session.source = source;
            }
            Err(_) => {
                send_animated_preview_failed(task, frame_index, animated_result, repaint);
                session.next_due = None;
                return;
            }
        }
    }

    let Some(frame) = (match session.source.next_frame() {
        Ok(frame) => frame,
        Err(_) => {
            send_animated_preview_failed(task, frame_index, animated_result, repaint);
            session.next_due = None;
            return;
        }
    }) else {
        send_animated_preview_failed(task, frame_index, animated_result, repaint);
        session.next_due = None;
        return;
    };
    let delay_ms = frame.delay_ms;
    let resized = match img::resize_to_width(frame.image, task.target_width as u32) {
        Ok(resized) => resized,
        Err(_) => {
            send_animated_preview_failed(task, frame_index, animated_result, repaint);
            session.next_due = None;
            return;
        }
    };
    if !animated_preview_file_snapshot_matches(task) {
        send_animated_preview_stale(task, frame_index, animated_result, repaint);
        session.next_due = None;
        return;
    }

    replace_animated_preview_result(
        animated_result,
        WorkerMsg::AnimatedPreviewReady(AnimatedPreviewReady {
            session_id: task.session_id,
            book_id: task.book_id.clone(),
            path: Arc::clone(&task.path),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            frame_index,
            delay_ms,
            scrub_bucket: None,
            abandon_ack_bucket_mask: task.abandon_bucket_mask,
            pixels: Arc::from(resized.pixels),
            width: resized.width as u16,
            height: resized.height as u16,
        }),
    );
    session.next_frame_index = frame_index.saturating_add(1);
    session.next_due = Some(Instant::now() + Duration::from_millis(delay_ms as u64));
    repaint.request_repaint();
}

fn open_static_preview_session(task: &StaticPreviewTask) -> Result<StaticPreviewSession, bool> {
    if !static_preview_file_snapshot_matches(task) {
        return Err(true);
    }
    let Some(page_map) = try_load_existing_viewer_page_map_for_spad(task.path.as_ref()) else {
        return Err(false);
    };
    if page_map.page_count() == 0 || page_map.page_count() != task.page_count {
        return Err(false);
    }
    let reader = open_book_reader(task.path.as_ref()).map_err(|_| false)?;
    Ok(StaticPreviewSession {
        task: task.clone(),
        reader,
    })
}

fn static_preview_session_matches(
    session: &StaticPreviewSession,
    task: &StaticPreviewTask,
) -> bool {
    session.task.session_id == task.session_id
        && session.task.book_id == task.book_id
        && session.task.path.as_ref() == task.path.as_ref()
        && session.task.target_width == task.target_width
        && session.task.expected_size == task.expected_size
        && session.task.expected_modified == task.expected_modified
        && session.task.page_count == task.page_count
}

fn decode_static_preview_page(
    session: &mut StaticPreviewSession,
    task: &StaticPreviewTask,
    preview_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) {
    if !static_preview_session_matches(session, task) {
        return;
    }
    if task.page_count == 0 || task.page_index as usize >= task.page_count {
        send_static_preview_result(task, false, preview_result, repaint);
        return;
    }
    if !static_preview_file_snapshot_matches(task) {
        send_static_preview_result(task, true, preview_result, repaint);
        return;
    }
    let raw = match session.reader.read_page_n(task.page_index) {
        Ok(raw) if raw.len() <= MAX_THUMB_RAW_BYTES => raw,
        Ok(_) | Err(_) => {
            send_static_preview_result(task, false, preview_result, repaint);
            return;
        }
    };
    let decoded =
        match img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32)
            .and_then(|decoded| img::resize_to_width(decoded, task.target_width as u32))
        {
            Ok(decoded) => decoded,
            Err(_) => {
                send_static_preview_result(task, false, preview_result, repaint);
                return;
            }
        };
    if !static_preview_file_snapshot_matches(task) {
        send_static_preview_result(task, true, preview_result, repaint);
        return;
    }
    replace_animated_preview_result(
        preview_result,
        WorkerMsg::StaticPreviewReady(StaticPreviewReady {
            session_id: task.session_id,
            book_id: task.book_id.clone(),
            path: Arc::clone(&task.path),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            page_index: task.page_index,
            pixels: Arc::from(decoded.pixels),
            width: decoded.width as u16,
            height: decoded.height as u16,
        }),
    );
    repaint.request_repaint();
}

fn send_static_preview_result(
    task: &StaticPreviewTask,
    stale: bool,
    preview_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) {
    replace_animated_preview_result(
        preview_result,
        if stale {
            WorkerMsg::StaticPreviewStale(StaticPreviewFailed {
                session_id: task.session_id,
                book_id: task.book_id.clone(),
                path: Arc::clone(&task.path),
                expected_size: task.expected_size,
                expected_modified: task.expected_modified,
                page_index: task.page_index,
            })
        } else {
            WorkerMsg::StaticPreviewFailed(StaticPreviewFailed {
                session_id: task.session_id,
                book_id: task.book_id.clone(),
                path: Arc::clone(&task.path),
                expected_size: task.expected_size,
                expected_modified: task.expected_modified,
                page_index: task.page_index,
            })
        },
    );
    repaint.request_repaint();
}

fn decode_video_preview_scene(
    session: &mut VideoPreviewSession,
    scene_percent: u8,
    tx: &std::sync::mpsc::Sender<WorkerMsg>,
    repaint: &RepaintNotifier,
) {
    let task = VideoPreviewTask {
        session_id: session.session_id,
        book_id: session.book_id.clone(),
        path: Arc::clone(&session.path),
        target_width: session.target_width,
        expected_size: session.expected_size,
        expected_modified: session.expected_modified,
        scene_percent,
    };
    let result = process_video_preview_scene(&mut session.decoder, session.duration, &task);
    match result {
        Ok(ready) => {
            if !preview_task_file_snapshot_matches(&task) {
                send_video_preview_stale(&task, tx, repaint);
                return;
            }
            let _ = tx.send(WorkerMsg::VideoPreviewReady(VideoPreviewReady {
                session_id: task.session_id,
                book_id: task.book_id,
                path: task.path,
                expected_size: task.expected_size,
                expected_modified: task.expected_modified,
                scene_percent: task.scene_percent,
                pixels: Arc::from(ready.pixels),
                width: ready.width as u16,
                height: ready.height as u16,
            }));
            repaint.request_repaint();
        }
        Err(_) => send_video_preview_failed(&task, tx, repaint),
    }
}

fn send_video_preview_failed(
    task: &VideoPreviewTask,
    tx: &std::sync::mpsc::Sender<WorkerMsg>,
    repaint: &RepaintNotifier,
) {
    let _ = tx.send(WorkerMsg::VideoPreviewFailed(VideoPreviewFailed {
        session_id: task.session_id,
        book_id: task.book_id.clone(),
        path: Arc::clone(&task.path),
        expected_size: task.expected_size,
        expected_modified: task.expected_modified,
        scene_percent: task.scene_percent,
    }));
    repaint.request_repaint();
}

fn send_video_preview_stale(
    task: &VideoPreviewTask,
    tx: &std::sync::mpsc::Sender<WorkerMsg>,
    repaint: &RepaintNotifier,
) {
    let _ = tx.send(WorkerMsg::VideoPreviewStale(VideoPreviewFailed {
        session_id: task.session_id,
        book_id: task.book_id.clone(),
        path: Arc::clone(&task.path),
        expected_size: task.expected_size,
        expected_modified: task.expected_modified,
        scene_percent: task.scene_percent,
    }));
    repaint.request_repaint();
}

fn send_animated_preview_failed(
    task: &AnimatedPreviewTask,
    frame_index: u64,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) {
    replace_animated_preview_result(
        animated_result,
        WorkerMsg::AnimatedPreviewFailed(AnimatedPreviewFailed {
            session_id: task.session_id,
            book_id: task.book_id.clone(),
            path: Arc::clone(&task.path),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            frame_index,
            scrub_bucket: task.scrub_bucket,
            abandon_ack: false,
            abandon_ack_bucket_mask: task.abandon_bucket_mask,
        }),
    );
    repaint.request_repaint();
}

fn send_animated_preview_stale(
    task: &AnimatedPreviewTask,
    frame_index: u64,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) {
    replace_animated_preview_result(
        animated_result,
        WorkerMsg::AnimatedPreviewStale(AnimatedPreviewFailed {
            session_id: task.session_id,
            book_id: task.book_id.clone(),
            path: Arc::clone(&task.path),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            frame_index,
            scrub_bucket: task.scrub_bucket,
            abandon_ack: false,
            abandon_ack_bucket_mask: task.abandon_bucket_mask,
        }),
    );
    repaint.request_repaint();
}

fn send_animated_preview_abandoned(
    task: &AnimatedPreviewTask,
    buckets: u64,
    frame_index: u64,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) {
    let mut task = task.clone();
    task.abandon_bucket_mask = buckets;
    replace_animated_preview_result(
        animated_result,
        WorkerMsg::AnimatedPreviewStale(AnimatedPreviewFailed {
            session_id: task.session_id,
            book_id: task.book_id.clone(),
            path: Arc::clone(&task.path),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            frame_index,
            scrub_bucket: task.scrub_bucket,
            abandon_ack: true,
            abandon_ack_bucket_mask: buckets,
        }),
    );
    repaint.request_repaint();
}

fn send_animated_preview_unavailable(
    task: &AnimatedPreviewTask,
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    repaint: &RepaintNotifier,
) {
    replace_animated_preview_result(
        animated_result,
        WorkerMsg::AnimatedPreviewUnavailable(AnimatedPreviewUnavailable {
            session_id: task.session_id,
            book_id: task.book_id.clone(),
            path: Arc::clone(&task.path),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            scrub_bucket: task.scrub_bucket,
            abandon_ack_bucket_mask: task.abandon_bucket_mask,
        }),
    );
    repaint.request_repaint();
}

fn replace_animated_preview_result(
    animated_result: &Arc<Mutex<Option<WorkerMsg>>>,
    result: WorkerMsg,
) {
    match animated_result.lock() {
        Ok(mut current) => *current = Some(result),
        Err(poisoned) => *poisoned.into_inner() = Some(result),
    }
}

fn clear_animated_preview_result(animated_result: &Arc<Mutex<Option<WorkerMsg>>>) {
    match animated_result.lock() {
        Ok(mut current) => *current = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

fn preview_task_file_snapshot_matches(task: &VideoPreviewTask) -> bool {
    preview_file_snapshot_matches(&task.path, task.expected_size, task.expected_modified)
}

fn animated_preview_file_snapshot_matches(task: &AnimatedPreviewTask) -> bool {
    preview_file_snapshot_matches(&task.path, task.expected_size, task.expected_modified)
}

fn static_preview_file_snapshot_matches(task: &StaticPreviewTask) -> bool {
    preview_file_snapshot_matches(&task.path, task.expected_size, task.expected_modified)
}

fn preview_file_snapshot_matches(
    path: &Path,
    expected_size: u64,
    expected_modified: Option<SystemTime>,
) -> bool {
    let Ok(current) = FileSnapshot::read(path) else {
        return false;
    };
    if matches!(book_source_kind(path), BookSourceKind::Folder) {
        current.modified == expected_modified
    } else {
        current.size == expected_size && current.modified == expected_modified
    }
}

async fn video_worker_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<VideoReq>,
    mut background_rx: tokio::sync::mpsc::UnboundedReceiver<VideoReq>,
    shared: Arc<WorkerShared>,
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
    thumbnail_sem: Arc<Semaphore>,
    visible_artifact_ids: Arc<Mutex<HashSet<BookId>>>,
) {
    let mut video_tasks = tokio::task::JoinSet::new();
    let mut background_open = true;
    loop {
        let req = match tokio::select! {
            biased;
            req = rx.recv() => VideoLoopEvent::VisibleRequest(req),
            req = background_rx.recv(), if background_open => VideoLoopEvent::BackgroundRequest(req),
            joined = video_tasks.join_next(), if !video_tasks.is_empty() => {
                VideoLoopEvent::TaskFinished(joined)
            }
        } {
            VideoLoopEvent::TaskFinished(Some(Err(error))) => {
                tracing::error!("video task join failed: {error}");
                continue;
            }
            VideoLoopEvent::TaskFinished(_) => continue,
            VideoLoopEvent::VisibleRequest(Some(req))
            | VideoLoopEvent::BackgroundRequest(Some(req)) => req,
            VideoLoopEvent::VisibleRequest(None) => break,
            VideoLoopEvent::BackgroundRequest(None) => {
                background_open = false;
                continue;
            }
        };

        let (task, task_gen, origin, _pending) = match req {
            VideoReq::Task(task, task_gen, origin) => (
                task,
                task_gen,
                origin,
                PendingRequestGuard::new(Arc::clone(&shared.lanes), ThumbnailLane::Video),
            ),
            VideoReq::ClearCache => {
                shared.lanes.reset_transient();
                shared.mem_cache.clear();
                shared.clear_video_in_flight();
                tracing::debug!("video memory cache cleared");
                continue;
            }
            VideoReq::RemoveCache(id) => {
                shared.lanes.reset_transient();
                let removed = shared.mem_cache.remove_by_book_id(&id);
                shared.remove_in_flight_by_book_id(&id);
                shared.remove_pruned_revisions_by_book_id(&id);
                tracing::debug!(id = %id.0.to_hex(), removed, "video cache removed");
                continue;
            }
            VideoReq::Shutdown => {
                shared.lanes.reset_transient();
                break;
            }
        };
        if task_gen != generation.load(Ordering::Relaxed) {
            continue;
        }
        let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
        match lookup_video_cache(
            &task,
            &shared,
            &generation,
            task_gen,
            task_artifact_generation,
            origin,
        ) {
            VideoWorkResult::Ready { ready, .. } => {
                if shared.artifact_generation.load(Ordering::Relaxed) != task_artifact_generation {
                    continue;
                }
                handle_video_result(
                    task,
                    Ok(VideoWorkResult::Ready { ready, webp: None }),
                    VideoResultContext {
                        task_gen,
                        task_artifact_generation,
                        shared: &shared,
                        generation: &generation,
                        tx: &tx,
                        repaint: &repaint,
                        origin,
                        visible_artifact_ids: &visible_artifact_ids,
                    },
                );
                continue;
            }
            VideoWorkResult::Stale => {
                if shared.artifact_generation.load(Ordering::Relaxed) != task_artifact_generation {
                    continue;
                }
                send_video_stale(
                    &task,
                    task_gen,
                    &tx,
                    &repaint,
                    &visible_artifact_ids,
                    origin,
                );
                continue;
            }
            VideoWorkResult::FailedPermanent => {
                if shared.artifact_generation.load(Ordering::Relaxed) != task_artifact_generation {
                    continue;
                }
                emit_video_permanent_failure(
                    &shared,
                    &task,
                    &tx,
                    &repaint,
                    &visible_artifact_ids,
                    origin,
                );
                continue;
            }
            VideoWorkResult::Failed => {}
        }
        if shared.artifact_generation.load(Ordering::Relaxed) != task_artifact_generation {
            continue;
        }
        tracing::debug!(
            id = %task.book_id.0.to_hex(),
            path = %task.path.display(),
            "video lane enqueue"
        );
        let Some(flight) = shared.begin_video_task(&task) else {
            tracing::trace!(id = %task.book_id.0.to_hex(), "duplicate video thumb task skipped");
            continue;
        };
        let Some((permit, video_running)) = acquire_thumbnail_permit(
            Arc::clone(&thumbnail_sem),
            Arc::clone(&shared.lanes),
            ThumbnailLane::Video,
            None,
            false,
        )
        .await
        else {
            tracing::error!("thumbnail semaphore closed");
            drop(flight);
            break;
        };
        let shared_for_task = Arc::clone(&shared);
        let tx_for_task = tx.clone();
        let repaint_for_task = repaint.clone();
        let generation_for_task = Arc::clone(&generation);
        let visible_artifact_ids_for_task = Arc::clone(&visible_artifact_ids);
        video_tasks.spawn(async move {
            let task_for_blocking = task.clone();
            let generation_for_blocking = Arc::clone(&generation_for_task);
            tracing::debug!(
                id = %task.book_id.0.to_hex(),
                path = %task.path.display(),
                "video decode start"
            );
            let result = tokio::task::spawn_blocking(move || {
                process_video_thumb(task_for_blocking, &generation_for_blocking, task_gen)
            })
            .await;
            handle_video_result(
                task,
                result.map_err(|error| error.to_string()),
                VideoResultContext {
                    task_gen,
                    task_artifact_generation,
                    shared: &shared_for_task,
                    generation: &generation_for_task,
                    tx: &tx_for_task,
                    repaint: &repaint_for_task,
                    origin,
                    visible_artifact_ids: &visible_artifact_ids_for_task,
                },
            );
            drop(permit);
            drop(video_running);
            drop(flight);
        });
    }

    while let Some(joined) = video_tasks.join_next().await {
        if let Err(error) = joined {
            tracing::error!("video task join failed during shutdown: {error}");
        }
    }
}

struct VideoResultContext<'a> {
    task_gen: u64,
    task_artifact_generation: u64,
    shared: &'a WorkerShared,
    generation: &'a Arc<AtomicU64>,
    tx: &'a std::sync::mpsc::Sender<WorkerMsg>,
    repaint: &'a RepaintNotifier,
    origin: RequestOrigin,
    visible_artifact_ids: &'a Arc<Mutex<HashSet<BookId>>>,
}

fn handle_video_result(
    task: VideoThumbTask,
    result: Result<VideoWorkResult, String>,
    context: VideoResultContext<'_>,
) {
    let VideoResultContext {
        task_gen,
        task_artifact_generation,
        shared,
        generation,
        tx,
        repaint,
        origin,
        visible_artifact_ids,
    } = context;

    if task_gen != generation.load(Ordering::Relaxed)
        || task_artifact_generation != shared.artifact_generation.load(Ordering::Relaxed)
    {
        return;
    }
    let currently_visible = background_artifact_is_visible(visible_artifact_ids, &task.book_id);
    let should_notify = origin.is_visible() || currently_visible;
    match result {
        Ok(VideoWorkResult::Ready { ready, webp }) => {
            if !thumb_task_file_snapshot_matches_video(&task) {
                send_video_stale(&task, task_gen, tx, repaint, visible_artifact_ids, origin);
                return;
            }
            let revision =
                SourceRevision::from_file_state(task.expected_size, task.expected_modified);
            if should_notify {
                shared.mem_cache.put_for_revision(
                    task.book_id.clone(),
                    task.target_width,
                    Thumbnail {
                        width: ready.width,
                        height: ready.height,
                        pixels: Arc::clone(&ready.pixels),
                    },
                    revision,
                );
            }
            clear_thumbnail_failure_for_video(shared, &task);
            tracing::debug!(
                id = %task.book_id.0.to_hex(),
                path = %task.path.display(),
                "video thumb success"
            );
            send_video_ready(
                &task,
                ready,
                task_gen,
                tx,
                repaint,
                visible_artifact_ids,
                origin,
            );
            if let Some(webp) = webp {
                let deferred = DeferredCache {
                    generation: Arc::clone(generation),
                    artifact_generation: Arc::clone(&shared.artifact_generation),
                    artifact_gate: Arc::clone(&shared.artifact_gate),
                    page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
                    task_generation: task_gen,
                    task_artifact_generation,
                    disk_cache: Arc::clone(&shared.disk_cache),
                    id: task.book_id.clone(),
                    source_path: Arc::clone(&task.path),
                    file_size: task.expected_size,
                    modified: task.expected_modified,
                    thumb: Some(DeferredThumbWrite { webp }),
                    page_map: None,
                };
                tokio::spawn(async move {
                    deferred.execute().await;
                });
            }
        }
        Ok(VideoWorkResult::Stale) => {
            send_video_stale(&task, task_gen, tx, repaint, visible_artifact_ids, origin)
        }
        Ok(VideoWorkResult::Failed) | Err(_) => {
            if !thumb_task_file_snapshot_matches_video(&task) {
                send_video_stale(&task, task_gen, tx, repaint, visible_artifact_ids, origin);
                return;
            }
            tracing::debug!(
                id = %task.book_id.0.to_hex(),
                path = %task.path.display(),
                "video thumbnail failed"
            );
            if should_notify {
                let _ = tx.send(WorkerMsg::FailedWithRevision {
                    book_id: task.book_id.clone(),
                    expected_size: task.expected_size,
                    expected_modified: task.expected_modified,
                });
            }
            if currently_visible {
                repaint.request_repaint();
            }
        }
        Ok(VideoWorkResult::FailedPermanent) => {
            if !thumb_task_file_snapshot_matches_video(&task) {
                send_video_stale(&task, task_gen, tx, repaint, visible_artifact_ids, origin);
                return;
            }
            tracing::debug!(
                id = %task.book_id.0.to_hex(),
                path = %task.path.display(),
                "video permanent failure"
            );
            emit_video_permanent_failure(shared, &task, tx, repaint, visible_artifact_ids, origin);
        }
    }
}

fn lookup_video_cache(
    task: &VideoThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    task_artifact_generation: u64,
    origin: RequestOrigin,
) -> VideoWorkResult {
    if task_artifact_generation != shared.artifact_generation.load(Ordering::Relaxed)
        || !thumb_task_file_snapshot_matches_video(task)
    {
        tracing::debug!(id = %task.book_id.0.to_hex(), "video thumbnail stale before cache lookup");
        return VideoWorkResult::Stale;
    }
    let revision = SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    shared.schedule_video_artifact_prune(&task.book_id, Arc::clone(&task.path), revision.clone());
    let _ = shared
        .mem_cache
        .prune_revisions_except(&task.book_id, &revision);
    if let Some(thumb) =
        shared
            .mem_cache
            .get_for_revision(&task.book_id, task.target_width, &revision)
    {
        if generation.load(Ordering::Relaxed) != task_generation
            || shared.artifact_generation.load(Ordering::Relaxed) != task_artifact_generation
            || !thumb_task_file_snapshot_matches_video(task)
        {
            return VideoWorkResult::Stale;
        }
        tracing::debug!(id = %task.book_id.0.to_hex(), "video thumbnail memory hit");
        clear_thumbnail_failure_for_video(shared, task);
        return VideoWorkResult::Ready {
            ready: ReadyThumb {
                book_id: task.book_id.clone(),
                pixels: thumb.pixels,
                width: thumb.width,
                height: thumb.height,
                expected_size: task.expected_size,
                expected_modified: task.expected_modified,
            },
            webp: None,
        };
    }
    if let Some(webp) =
        shared
            .disk_cache
            .get_thumb(&task.book_id, task.expected_size, task.expected_modified)
    {
        if let Ok(decoded) = img::decode_webp(&webp) {
            if generation.load(Ordering::Relaxed) != task_generation
                || shared.artifact_generation.load(Ordering::Relaxed) != task_artifact_generation
                || !thumb_task_file_snapshot_matches_video(task)
            {
                return VideoWorkResult::Stale;
            }
            let ready = ready_from_decoded(decoded, task.clone());
            if origin.is_visible() {
                shared.mem_cache.put_for_revision(
                    task.book_id.clone(),
                    task.target_width,
                    Thumbnail {
                        width: ready.width,
                        height: ready.height,
                        pixels: Arc::clone(&ready.pixels),
                    },
                    revision,
                );
            }
            tracing::debug!(id = %task.book_id.0.to_hex(), "video thumbnail disk hit");
            clear_thumbnail_failure_for_video(shared, task);
            return VideoWorkResult::Ready { ready, webp: None };
        }
        tracing::debug!(id = %task.book_id.0.to_hex(), "video thumbnail disk cache decode miss");
    }
    if shared.artifact_failure_cache.as_ref().is_some_and(|cache| {
        cache.has_failure_for_revision(&task.book_id, &revision, ArtifactKind::Thumbnail)
    }) {
        tracing::debug!(id = %task.book_id.0.to_hex(), "video thumbnail failure-cache hit");
        return VideoWorkResult::FailedPermanent;
    }
    tracing::debug!(id = %task.book_id.0.to_hex(), "video thumbnail cache miss");
    VideoWorkResult::Failed
}

fn send_video_ready(
    task: &VideoThumbTask,
    ready: ReadyThumb,
    task_gen: u64,
    tx: &std::sync::mpsc::Sender<WorkerMsg>,
    repaint: &RepaintNotifier,
    visible_artifact_ids: &Arc<Mutex<HashSet<BookId>>>,
    origin: RequestOrigin,
) {
    let currently_visible = background_artifact_is_visible(visible_artifact_ids, &task.book_id);
    if origin.is_visible() || currently_visible {
        let _ = tx.send(WorkerMsg::VideoReady(VideoReady {
            ready,
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            generation: task_gen,
        }));
    }
    if currently_visible {
        repaint.request_repaint();
    }
}

fn send_video_stale(
    task: &VideoThumbTask,
    task_gen: u64,
    tx: &std::sync::mpsc::Sender<WorkerMsg>,
    repaint: &RepaintNotifier,
    visible_artifact_ids: &Arc<Mutex<HashSet<BookId>>>,
    origin: RequestOrigin,
) {
    let currently_visible = background_artifact_is_visible(visible_artifact_ids, &task.book_id);
    tracing::debug!(
        id = %task.book_id.0.to_hex(),
        expected_size = task.expected_size,
        "video stale"
    );
    if origin.is_visible() || currently_visible {
        let _ = tx.send(WorkerMsg::VideoStale {
            book_id: task.book_id.clone(),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
            generation: task_gen,
        });
    }
    if currently_visible {
        repaint.request_repaint();
    }
}

fn emit_video_permanent_failure(
    shared: &WorkerShared,
    task: &VideoThumbTask,
    tx: &std::sync::mpsc::Sender<WorkerMsg>,
    repaint: &RepaintNotifier,
    visible_artifact_ids: &Arc<Mutex<HashSet<BookId>>>,
    origin: RequestOrigin,
) {
    let currently_visible = background_artifact_is_visible(visible_artifact_ids, &task.book_id);
    mark_thumbnail_failure_for_video(shared, task);
    if origin.is_visible() || currently_visible {
        let _ = tx.send(WorkerMsg::FailedPermanentWithRevision {
            book_id: task.book_id.clone(),
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
        });
    }
    if currently_visible {
        repaint.request_repaint();
    }
}

fn process_video_thumb(
    task: VideoThumbTask,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
) -> VideoWorkResult {
    if !thumb_task_file_snapshot_matches_video(&task) {
        return VideoWorkResult::Stale;
    }
    if task.expected_size == 0 {
        return VideoWorkResult::FailedPermanent;
    }

    let mut decoder = match VideoDecoder::open(task.path.as_ref())
        .output_format(PixelFormat::Rgba)
        .build()
    {
        Ok(decoder) => decoder,
        Err(error) => {
            return if video_decode_error_is_temporary(&error) {
                VideoWorkResult::Failed
            } else {
                VideoWorkResult::FailedPermanent
            };
        }
    };
    let Some(duration) = decoder
        .duration_opt()
        .filter(|duration| !duration.is_zero())
    else {
        return VideoWorkResult::FailedPermanent;
    };
    let position = duration.mul_f64(VIDEO_THUMB_POSITION_RATIO);
    tracing::debug!(
        id = %task.book_id.0.to_hex(),
        path = %task.path.display(),
        position_ms = position.as_millis(),
        "video seek"
    );
    if let Err(error) = decoder.seek(position, SeekMode::Keyframe) {
        return video_decode_failure(&error);
    }
    let Some(frame) = (match decoder.decode_one() {
        Ok(frame) => frame,
        Err(error) => return video_decode_failure(&error),
    }) else {
        return VideoWorkResult::FailedPermanent;
    };
    if frame.format() != PixelFormat::Rgba || frame.width() == 0 || frame.height() == 0 {
        return VideoWorkResult::FailedPermanent;
    }
    let Some(data) = frame.data_ref() else {
        return VideoWorkResult::FailedPermanent;
    };
    let Some(row_bytes) = (frame.width() as usize).checked_mul(4) else {
        return VideoWorkResult::FailedPermanent;
    };
    let Some(raw_bytes) = row_bytes.checked_mul(frame.height() as usize) else {
        return VideoWorkResult::FailedPermanent;
    };
    if raw_bytes > MAX_THUMB_RAW_BYTES {
        return VideoWorkResult::FailedPermanent;
    }
    let stride = frame.stride(0).unwrap_or(row_bytes);
    let Some(required_bytes) = stride
        .checked_mul(frame.height() as usize - 1)
        .and_then(|bytes| bytes.checked_add(row_bytes))
    else {
        return VideoWorkResult::FailedPermanent;
    };
    if stride < row_bytes || data.len() < required_bytes {
        return VideoWorkResult::FailedPermanent;
    }
    let mut pixels = Vec::with_capacity(raw_bytes);
    for row in 0..frame.height() as usize {
        let start = row * stride;
        pixels.extend_from_slice(&data[start..start + row_bytes]);
    }
    let decoded = img::DecodedImage {
        width: frame.width(),
        height: frame.height(),
        pixels,
    };
    let resized = match img::resize_to_width(decoded, task.target_width as u32) {
        Ok(image) => image,
        Err(_) => return VideoWorkResult::FailedPermanent,
    };
    if generation.load(Ordering::Relaxed) != task_generation
        || !thumb_task_file_snapshot_matches_video(&task)
    {
        return VideoWorkResult::Stale;
    }
    VideoWorkResult::Ready {
        ready: ready_from_decoded(resized.clone(), task),
        webp: img::encode_webp(&resized).ok(),
    }
}

fn process_video_preview_scene(
    decoder: &mut VideoDecoder,
    duration: Duration,
    task: &VideoPreviewTask,
) -> Result<img::DecodedImage, VideoWorkResult> {
    let position = duration.mul_f64(task.scene_percent as f64 / 100.0);
    if let Err(error) = decoder.seek(position, SeekMode::Keyframe) {
        return Err(video_decode_failure(&error));
    }
    let Some(frame) = (match decoder.decode_one() {
        Ok(frame) => frame,
        Err(error) => return Err(video_decode_failure(&error)),
    }) else {
        return Err(VideoWorkResult::FailedPermanent);
    };
    if frame.format() != PixelFormat::Rgba || frame.width() == 0 || frame.height() == 0 {
        return Err(VideoWorkResult::FailedPermanent);
    }
    let Some(data) = frame.data_ref() else {
        return Err(VideoWorkResult::FailedPermanent);
    };
    let Some(row_bytes) = (frame.width() as usize).checked_mul(4) else {
        return Err(VideoWorkResult::FailedPermanent);
    };
    let Some(raw_bytes) = row_bytes.checked_mul(frame.height() as usize) else {
        return Err(VideoWorkResult::FailedPermanent);
    };
    if raw_bytes > MAX_THUMB_RAW_BYTES {
        return Err(VideoWorkResult::FailedPermanent);
    }
    let stride = frame.stride(0).unwrap_or(row_bytes);
    let Some(required_bytes) = stride
        .checked_mul(frame.height() as usize - 1)
        .and_then(|bytes| bytes.checked_add(row_bytes))
    else {
        return Err(VideoWorkResult::FailedPermanent);
    };
    if stride < row_bytes || data.len() < required_bytes {
        return Err(VideoWorkResult::FailedPermanent);
    }
    let mut pixels = Vec::with_capacity(raw_bytes);
    for row in 0..frame.height() as usize {
        let start = row * stride;
        pixels.extend_from_slice(&data[start..start + row_bytes]);
    }
    let decoded = img::DecodedImage {
        width: frame.width(),
        height: frame.height(),
        pixels,
    };
    img::resize_to_width(decoded, task.target_width as u32)
        .map_err(|_| VideoWorkResult::FailedPermanent)
}

enum VideoWorkResult {
    Ready {
        ready: ReadyThumb,
        webp: Option<Vec<u8>>,
    },
    Stale,
    Failed,
    FailedPermanent,
}

fn video_decode_error_is_temporary(error: &ff_decode::DecodeError) -> bool {
    matches!(
        error,
        ff_decode::DecodeError::FileNotFound { .. }
            | ff_decode::DecodeError::Io(_)
            | ff_decode::DecodeError::DecodingFailed { .. }
            | ff_decode::DecodeError::SeekFailed { .. }
    )
}

fn video_decode_failure(error: &ff_decode::DecodeError) -> VideoWorkResult {
    if video_decode_error_is_temporary(error) {
        VideoWorkResult::Failed
    } else {
        VideoWorkResult::FailedPermanent
    }
}

fn thumb_task_file_snapshot_matches_video(task: &VideoThumbTask) -> bool {
    let Ok(current) = FileSnapshot::read(&task.path) else {
        return false;
    };
    current.size == task.expected_size && current.modified == task.expected_modified
}

fn failed_thumb_msg(id: BookId, path: &Path, err: &anyhow::Error) -> WorkerMsg {
    if is_permanent_thumb_error(Some(path), None, err) {
        WorkerMsg::FailedPermanent(id)
    } else {
        WorkerMsg::Failed(id)
    }
}

fn failed_thumb_msg_for_image_decode(id: BookId, raw: &[u8], err: &anyhow::Error) -> WorkerMsg {
    let fmt = ImageFormatHint::from_magic(raw);
    if is_permanent_thumb_error(None, Some(fmt), err) {
        WorkerMsg::FailedPermanent(id)
    } else {
        WorkerMsg::Failed(id)
    }
}

fn is_permanent_thumb_error(
    _path: Option<&Path>,
    image_format: Option<ImageFormatHint>,
    err: &anyhow::Error,
) -> bool {
    #[cfg(feature = "avif")]
    let _ = image_format;
    let err_text = format!("{err:#}").to_ascii_lowercase();

    // rar feature 無効時だけ RAR/CBR を恒久失敗にする。feature 有効時は一時失敗として扱う。
    #[cfg(not(feature = "rar"))]
    if _path
        .map(|path| matches!(book_source_kind(path), BookSourceKind::Rar))
        .unwrap_or(false)
        || err_text.contains("rar サポートが無効")
        || err_text.contains("rar support is disabled")
    {
        return true;
    }

    // avif feature 無効時だけ AVIF を恒久失敗にする。feature 有効時は通常の decode error として扱う。
    #[cfg(not(feature = "avif"))]
    if matches!(image_format, Some(ImageFormatHint::Avif))
        || err_text.contains("format avif is not supported")
        || err_text.contains("avif is not supported")
    {
        return true;
    }

    // 形式や内容として確定的に失敗しているものは恒久失敗として扱う。
    err_text.contains("アーカイブに画像がありません")
        || err_text.contains("no image in archive")
        || err_text.contains("corrupt deflate stream")
        || err_text.contains("invalid zip archive")
        || err_text.contains("unsupported archive")
        || err_text.contains("epub encrypted/drm package is not supported")
        || err_text.contains("meta-inf/encryption.xml found")
        || err_text.contains("unsupported image format")
        || (err_text.contains("the image format") && err_text.contains("is not supported"))
}

fn thumb_task_file_snapshot_matches(task: &ThumbTask) -> bool {
    let Ok(current) = FileSnapshot::read(&task.path) else {
        return false;
    };
    if matches!(book_source_kind(&task.path), BookSourceKind::Folder) {
        current.modified == task.expected_modified
    } else {
        current.size == task.expected_size && current.modified == task.expected_modified
    }
}

// ── 共有状態 ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThumbnailLane {
    Image,
    Video,
}

/// Global permit acquisition and completion-credit state for the two existing thumbnail lanes.
/// `start_pending` is a one-shot starvation guard, not a reserved slot.
#[derive(Default)]
struct ThumbnailLaneState {
    progress: Mutex<LaneProgressState>,
    wake: Notify,
}

#[derive(Default)]
struct LaneProgressState {
    image: LaneProgress,
    video: LaneProgress,
    base_goal: usize,
    global_goal: usize,
    start_pending: Option<ThumbnailLane>,
    image_completion_credit: usize,
    video_completion_credit: usize,
}

#[derive(Default)]
struct LaneProgress {
    /// Requests accepted by the public lane API until the lane worker retires them.
    pending_requests: usize,
    waiting: usize,
    running: usize,
}

impl ThumbnailLaneState {
    fn new(base_goal: usize) -> Self {
        Self {
            progress: Mutex::new(LaneProgressState {
                base_goal,
                global_goal: base_goal,
                ..LaneProgressState::default()
            }),
            wake: Notify::new(),
        }
    }

    fn update_global_goal(&self, path: &Path) {
        let medium = detect_storage_medium_cached(path);
        let medium_name = match medium {
            StorageMedium::Hdd => "hdd",
            StorageMedium::Ssd => "ssd",
            StorageMedium::Unknown => "unknown",
        };
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let base_goal = state.base_goal;
        let global_goal = match medium {
            StorageMedium::Hdd => base_goal.min(2),
            StorageMedium::Ssd | StorageMedium::Unknown => base_goal,
        }
        .max(1);
        if state.global_goal == global_goal {
            return;
        }
        state.global_goal = global_goal;
        tracing::info!(
            path = %path.display(),
            medium = medium_name,
            base_goal,
            global_goal,
            "thumbnail global goal updated"
        );
        drop(state);
        self.wake.notify_waiters();
    }

    fn progress(state: &LaneProgressState, lane: ThumbnailLane) -> &LaneProgress {
        match lane {
            ThumbnailLane::Image => &state.image,
            ThumbnailLane::Video => &state.video,
        }
    }

    fn progress_mut(state: &mut LaneProgressState, lane: ThumbnailLane) -> &mut LaneProgress {
        match lane {
            ThumbnailLane::Image => &mut state.image,
            ThumbnailLane::Video => &mut state.video,
        }
    }

    fn other_lane(lane: ThumbnailLane) -> ThumbnailLane {
        match lane {
            ThumbnailLane::Image => ThumbnailLane::Video,
            ThumbnailLane::Video => ThumbnailLane::Image,
        }
    }

    fn completion_credit(state: &LaneProgressState, lane: ThumbnailLane) -> usize {
        match lane {
            ThumbnailLane::Image => state.image_completion_credit,
            ThumbnailLane::Video => state.video_completion_credit,
        }
    }

    fn completion_credit_mut(state: &mut LaneProgressState, lane: ThumbnailLane) -> &mut usize {
        match lane {
            ThumbnailLane::Image => &mut state.image_completion_credit,
            ThumbnailLane::Video => &mut state.video_completion_credit,
        }
    }

    fn clear_completion_credit_at_empty_boundary(
        state: &mut LaneProgressState,
        lane: ThumbnailLane,
    ) {
        let progress = Self::progress(state, lane);
        if progress.pending_requests == 0 && progress.waiting == 0 && progress.running == 0 {
            *Self::completion_credit_mut(state, lane) = 0;
        }
    }

    fn refresh_start_pending(state: &mut LaneProgressState) {
        if state.start_pending.is_some_and(|lane| {
            Self::progress(state, lane).waiting == 0
                || Self::progress(state, Self::other_lane(lane)).running == 0
        }) {
            state.start_pending = None;
        }
        if state.start_pending.is_none() {
            for lane in [ThumbnailLane::Image, ThumbnailLane::Video] {
                let progress = Self::progress(state, lane);
                if progress.waiting > 0
                    && progress.running == 0
                    && Self::progress(state, Self::other_lane(lane)).running > 0
                {
                    state.start_pending = Some(lane);
                    break;
                }
            }
        }
    }

    fn mark_waiting(&self, lane: ThumbnailLane) {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::progress_mut(&mut state, lane).waiting += 1;
        Self::refresh_start_pending(&mut state);
    }

    fn mark_request_pending(&self, lane: ThumbnailLane) {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::progress_mut(&mut state, lane).pending_requests += 1;
    }

    fn retire_request_pending(&self, lane: ThumbnailLane) {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::progress_mut(&mut state, lane).pending_requests -= 1;
        Self::clear_completion_credit_at_empty_boundary(&mut state, lane);
        Self::refresh_start_pending(&mut state);
        self.wake.notify_waiters();
    }

    fn cancel_waiting(&self, lane: ThumbnailLane) {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::progress_mut(&mut state, lane).waiting -= 1;
        Self::clear_completion_credit_at_empty_boundary(&mut state, lane);
        Self::refresh_start_pending(&mut state);
        self.wake.notify_waiters();
    }

    fn may_start(&self, lane: ThumbnailLane) -> bool {
        let state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if Self::running_total(&state) >= state.global_goal {
            return false;
        }
        if let Some(start_pending) = state.start_pending {
            // Existing cross-lane starvation rescue has priority over completion credits.
            return start_pending == lane;
        }
        let image_credit = Self::completion_credit(&state, ThumbnailLane::Image);
        let video_credit = Self::completion_credit(&state, ThumbnailLane::Video);
        (image_credit == 0 && video_credit == 0) || Self::completion_credit(&state, lane) > 0
    }

    fn running_total(state: &LaneProgressState) -> usize {
        state.image.running + state.video.running
    }

    fn try_started(self: &Arc<Self>, lane: ThumbnailLane) -> Option<LaneRunningGuard> {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if Self::running_total(&state) >= state.global_goal {
            return None;
        }
        {
            let progress = Self::progress_mut(&mut state, lane);
            progress.waiting -= 1;
            progress.running += 1;
        }
        if state.start_pending == Some(lane) {
            state.start_pending = None;
        }
        if Self::completion_credit(&state, lane) > 0 {
            // A successful acquire consumes exactly one credit for its lane.
            *Self::completion_credit_mut(&mut state, lane) -= 1;
        }
        Self::clear_completion_credit_at_empty_boundary(&mut state, lane);
        Self::refresh_start_pending(&mut state);
        self.wake.notify_waiters();
        Some(LaneRunningGuard {
            lanes: Arc::clone(self),
            lane,
        })
    }

    fn finished(&self, lane: ThumbnailLane) {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::progress_mut(&mut state, lane).running -= 1;
        if Self::progress(&state, lane).pending_requests > 0 {
            // Preserve every completion credit while this lane still has accepted requests in its backlog.
            *Self::completion_credit_mut(&mut state, lane) += 1;
        }
        Self::clear_completion_credit_at_empty_boundary(&mut state, lane);
        Self::refresh_start_pending(&mut state);
        self.wake.notify_waiters();
    }

    fn reset_transient(&self) {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.start_pending = None;
        state.image_completion_credit = 0;
        state.video_completion_credit = 0;
        self.wake.notify_waiters();
    }
}

struct PendingRequestGuard {
    lanes: Arc<ThumbnailLaneState>,
    lane: ThumbnailLane,
}

impl PendingRequestGuard {
    fn new(lanes: Arc<ThumbnailLaneState>, lane: ThumbnailLane) -> Self {
        Self { lanes, lane }
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        self.lanes.retire_request_pending(self.lane);
    }
}

async fn acquire_thumbnail_permit(
    semaphore: Arc<Semaphore>,
    lanes: Arc<ThumbnailLaneState>,
    lane: ThumbnailLane,
    display_mailbox: Option<Arc<DisplayThumbMailbox>>,
    display_only: bool,
) -> Option<(tokio::sync::OwnedSemaphorePermit, LaneRunningGuard)> {
    lanes.mark_waiting(lane);
    loop {
        let notified = lanes.wake.notified();
        if let Some(display_mailbox) = display_mailbox.as_ref() {
            let display_notified = display_mailbox.wake.notified();
            let has_display_work = display_mailbox.has_pending();
            if display_only && !has_display_work {
                lanes.cancel_waiting(lane);
                return None;
            }
            if !display_only && has_display_work {
                display_notified.await;
                continue;
            }
            if lanes.may_start(lane) {
                match Arc::clone(&semaphore).try_acquire_owned() {
                    Ok(permit) => {
                        if let Some(running) = lanes.try_started(lane) {
                            return Some((permit, running));
                        }
                        drop(permit);
                    }
                    Err(tokio::sync::TryAcquireError::Closed) => {
                        lanes.cancel_waiting(lane);
                        return None;
                    }
                    Err(tokio::sync::TryAcquireError::NoPermits) => {}
                }
            }
            tokio::select! {
                _ = notified => {}
                _ = display_notified => {}
            }
            continue;
        }
        if lanes.may_start(lane) {
            match Arc::clone(&semaphore).try_acquire_owned() {
                Ok(permit) => {
                    if let Some(running) = lanes.try_started(lane) {
                        return Some((permit, running));
                    }
                    drop(permit);
                }
                Err(tokio::sync::TryAcquireError::Closed) => {
                    lanes.cancel_waiting(lane);
                    return None;
                }
                Err(tokio::sync::TryAcquireError::NoPermits) => {}
            }
        }
        notified.await;
    }
}

struct LaneRunningGuard {
    lanes: Arc<ThumbnailLaneState>,
    lane: ThumbnailLane,
}

impl Drop for LaneRunningGuard {
    fn drop(&mut self) {
        self.lanes.finished(self.lane);
    }
}

struct WorkerShared {
    mem_cache: ThumbMemCache,
    disk_cache: Arc<DiskCache>, // バックグラウンド書き込みと共有する。
    page_map_cache: Option<Arc<PageMapDiskCache>>,
    artifact_failure_cache: Option<Arc<ArtifactFailureDiskCache>>,
    page_map_coordinator: Arc<PageMapCoordinator>,
    artifact_generation: Arc<AtomicU64>,
    artifact_gate: Arc<RwLock<()>>,
    lanes: Arc<ThumbnailLaneState>,
    next_flight_id: AtomicU64,
    in_flight: Arc<Mutex<HashSet<(ThumbTaskKey, u64)>>>,
    video_in_flight: Arc<Mutex<HashSet<(VideoThumbTaskKey, u64)>>>,
    pruned_revisions: Arc<Mutex<HashSet<ArtifactPruneKey>>>,
    req_tx: tokio::sync::mpsc::UnboundedSender<WorkerReq>,
}

impl WorkerShared {
    fn begin_task(&self, task: &ThumbTask) -> Option<TaskFlightGuard> {
        let key = ThumbTaskKey::from_task(task);
        let mut guard = match self.in_flight.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::error!("thumb worker in-flight mutex poisoned");
                return None;
            }
        };
        if guard.iter().any(|(existing, _)| existing == &key) {
            return None;
        }
        let flight_id = self.next_flight_id.fetch_add(1, Ordering::Relaxed);
        guard.insert((key.clone(), flight_id));
        Some(TaskFlightGuard::new(
            Arc::clone(&self.in_flight),
            (key, flight_id),
        ))
    }

    fn begin_video_task(&self, task: &VideoThumbTask) -> Option<VideoTaskFlightGuard> {
        let key = VideoThumbTaskKey::from_task(task);
        let mut guard = match self.video_in_flight.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::error!("thumb worker video in-flight mutex poisoned");
                return None;
            }
        };
        if guard.iter().any(|(existing, _)| existing == &key) {
            return None;
        }
        let flight_id = self.next_flight_id.fetch_add(1, Ordering::Relaxed);
        guard.insert((key.clone(), flight_id));
        Some(VideoTaskFlightGuard::new(
            Arc::clone(&self.video_in_flight),
            (key, flight_id),
        ))
    }

    fn clear_in_flight(&self) {
        if let Ok(mut guard) = self.in_flight.lock() {
            guard.clear();
        }
        if let Ok(mut guard) = self.video_in_flight.lock() {
            guard.clear();
        }
    }

    fn clear_video_in_flight(&self) {
        if let Ok(mut guard) = self.video_in_flight.lock() {
            guard.clear();
        }
    }

    fn remove_in_flight_by_book_id(&self, id: &BookId) {
        if let Ok(mut guard) = self.in_flight.lock() {
            guard.retain(|(key, _)| &key.book_id != id);
        }
        if let Ok(mut guard) = self.video_in_flight.lock() {
            guard.retain(|(key, _)| &key.book_id != id);
        }
    }

    fn schedule_artifact_prune(
        &self,
        id: &BookId,
        source_path: Arc<Path>,
        source_revision: SourceRevision,
    ) {
        let key = ArtifactPruneKey {
            book_id: id.clone(),
            source_revision: source_revision.clone(),
        };
        let mut guard = match self.pruned_revisions.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if !guard.insert(key.clone()) {
            return;
        }
        if self
            .req_tx
            .send(WorkerReq::PruneObsoleteArtifacts {
                id: id.clone(),
                source_path,
                source_revision,
            })
            .is_err()
        {
            guard.remove(&key);
        }
    }

    fn schedule_video_artifact_prune(
        &self,
        id: &BookId,
        source_path: Arc<Path>,
        source_revision: SourceRevision,
    ) {
        let key = ArtifactPruneKey {
            book_id: id.clone(),
            source_revision: source_revision.clone(),
        };
        let mut guard = match self.pruned_revisions.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if !guard.insert(key.clone()) {
            return;
        }
        if self
            .req_tx
            .send(WorkerReq::PruneVideoObsoleteArtifacts {
                id: id.clone(),
                source_path,
                source_revision,
            })
            .is_err()
        {
            guard.remove(&key);
        }
    }

    fn clear_pruned_revisions(&self) {
        if let Ok(mut guard) = self.pruned_revisions.lock() {
            guard.clear();
        }
    }

    fn remove_pruned_revisions_by_book_id(&self, id: &BookId) {
        if let Ok(mut guard) = self.pruned_revisions.lock() {
            guard.retain(|key| &key.book_id != id);
        }
    }

    fn prune_obsolete_artifacts(
        &self,
        id: &BookId,
        source_path: &Path,
        source_revision: &SourceRevision,
    ) {
        let is_folder_book = matches!(book_source_kind(source_path), BookSourceKind::Folder);
        let Ok(snapshot) = FileSnapshot::read(source_path) else {
            return;
        };
        let snapshot_size = if is_folder_book { 0 } else { snapshot.size };
        if SourceRevision::from_file_state(snapshot_size, snapshot.modified) != *source_revision {
            return;
        }

        let _gate = self.artifact_gate.write();
        let Ok(snapshot) = FileSnapshot::read(source_path) else {
            return;
        };
        let snapshot_size = if is_folder_book { 0 } else { snapshot.size };
        if SourceRevision::from_file_state(snapshot_size, snapshot.modified) != *source_revision {
            return;
        }

        let thumbs = self
            .disk_cache
            .prune_thumbs_except(id, snapshot_size, snapshot.modified);
        let page_maps = self
            .page_map_cache
            .as_ref()
            .map(|cache| cache.prune_page_maps_except_revision(id, source_revision));
        let failures = self
            .artifact_failure_cache
            .as_ref()
            .map(|cache| cache.prune_failures_except_revision(id, source_revision));

        let thumb_removed = log_prune_result("thumbnail", id, thumbs);
        let page_map_removed = page_maps
            .map(|result| log_prune_result("page-map", id, result))
            .unwrap_or(0);
        let failure_removed = failures
            .map(|result| log_prune_result("artifact failure", id, result))
            .unwrap_or(0);
        if thumb_removed + page_map_removed + failure_removed > 0 {
            tracing::debug!(
                id = %id.0.to_hex(),
                thumb_removed,
                page_map_removed,
                failure_removed,
                "obsolete artifact revisions pruned"
            );
        }
    }

    fn prune_video_obsolete_artifacts(
        &self,
        id: &BookId,
        source_path: &Path,
        source_revision: &SourceRevision,
    ) {
        let Ok(snapshot) = FileSnapshot::read(source_path) else {
            return;
        };
        if SourceRevision::from_file_state(snapshot.size, snapshot.modified) != *source_revision {
            return;
        }

        let _gate = self.artifact_gate.write();
        let Ok(snapshot) = FileSnapshot::read(source_path) else {
            return;
        };
        if SourceRevision::from_file_state(snapshot.size, snapshot.modified) != *source_revision {
            return;
        }

        let thumbs = self
            .disk_cache
            .prune_thumbs_except(id, snapshot.size, snapshot.modified);
        let failures = self
            .artifact_failure_cache
            .as_ref()
            .map(|cache| cache.prune_failures_except_revision(id, source_revision));
        let thumb_removed = log_prune_result("video thumbnail", id, thumbs);
        let failure_removed = failures
            .map(|result| log_prune_result("video artifact failure", id, result))
            .unwrap_or(0);
        if thumb_removed + failure_removed > 0 {
            tracing::debug!(
                id = %id.0.to_hex(),
                thumb_removed,
                failure_removed,
                "obsolete video artifact revisions pruned"
            );
        }
    }
}

fn log_prune_result(artifact: &str, id: &BookId, result: anyhow::Result<usize>) -> usize {
    match result {
        Ok(removed) => removed,
        Err(error) => {
            tracing::debug!(
                id = %id.0.to_hex(),
                artifact,
                error = %error,
                "obsolete artifact revision prune failed"
            );
            0
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ThumbTaskKey {
    book_id: BookId,
    target_width: u16,
    expected_size: u64,
    expected_modified: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct VideoThumbTaskKey {
    book_id: BookId,
    target_width: u16,
    expected_size: u64,
    expected_modified: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ArtifactPruneKey {
    book_id: BookId,
    source_revision: SourceRevision,
}

impl ThumbTaskKey {
    fn from_task(task: &ThumbTask) -> Self {
        Self {
            book_id: task.book_id.clone(),
            target_width: task.target_width,
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
        }
    }
}

impl VideoThumbTaskKey {
    fn from_task(task: &VideoThumbTask) -> Self {
        Self {
            book_id: task.book_id.clone(),
            target_width: task.target_width,
            expected_size: task.expected_size,
            expected_modified: task.expected_modified,
        }
    }
}

struct TaskFlightGuard {
    in_flight: Arc<Mutex<HashSet<(ThumbTaskKey, u64)>>>,
    key: Option<(ThumbTaskKey, u64)>,
}

impl TaskFlightGuard {
    fn new(in_flight: Arc<Mutex<HashSet<(ThumbTaskKey, u64)>>>, key: (ThumbTaskKey, u64)) -> Self {
        Self {
            in_flight,
            key: Some(key),
        }
    }
}

impl Drop for TaskFlightGuard {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        if let Ok(mut guard) = self.in_flight.lock() {
            guard.remove(&key);
        }
    }
}

struct VideoTaskFlightGuard {
    in_flight: Arc<Mutex<HashSet<(VideoThumbTaskKey, u64)>>>,
    key: Option<(VideoThumbTaskKey, u64)>,
}

impl VideoTaskFlightGuard {
    fn new(
        in_flight: Arc<Mutex<HashSet<(VideoThumbTaskKey, u64)>>>,
        key: (VideoThumbTaskKey, u64),
    ) -> Self {
        Self {
            in_flight,
            key: Some(key),
        }
    }
}

impl Drop for VideoTaskFlightGuard {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        if let Ok(mut guard) = self.in_flight.lock() {
            guard.remove(&key);
        }
    }
}

struct ZipThumbnailLaneResult {
    compression: crate::infra::archive::zip::ZipCompressionMethod,
    compressed_size: u64,
    uncompressed_size: u64,
    decoded: img::DecodedImage,
    elapsed: Duration,
}

struct EpubThumbnailLaneResult {
    decoded: img::DecodedImage,
    elapsed: Duration,
}

struct FolderThumbnailLaneResult {
    decoded: img::DecodedImage,
    elapsed: Duration,
}

struct DeferredThumbWrite {
    webp: Vec<u8>,
}

enum DeferredPageMap {
    Cached(PageMapStatus),
    Fast(PageMapFastPersistRequest),
    Ready(PageMapReadyPersistRequest),
    Complete { request: PageMapCompleteRequest },
}

// ── バックグラウンドディスクキャッシュ書き込み ────────────────────────────────

/// UI 応答を止めないための後段永続化タスク。
/// thumb の WebP 保存と Page Map 保存をまとめる。
struct DeferredCache {
    generation: Arc<AtomicU64>,
    artifact_generation: Arc<AtomicU64>,
    artifact_gate: Arc<RwLock<()>>,
    page_map_coordinator: Arc<PageMapCoordinator>,
    task_generation: u64,
    task_artifact_generation: u64,
    disk_cache: Arc<DiskCache>,
    id: BookId,
    source_path: Arc<Path>,
    file_size: u64,
    modified: Option<SystemTime>,
    thumb: Option<DeferredThumbWrite>,
    page_map: Option<DeferredPageMap>,
}

impl DeferredCache {
    async fn execute(self) {
        let DeferredCache {
            generation,
            artifact_generation,
            artifact_gate,
            page_map_coordinator,
            task_generation,
            task_artifact_generation,
            disk_cache,
            id,
            source_path,
            file_size,
            modified,
            thumb,
            page_map,
        } = self;
        let is_video_thumbnail = page_map.is_none();
        if let Some(thumb) = thumb {
            let id = id.clone();
            let disk_cache = Arc::clone(&disk_cache);
            let generation = Arc::clone(&generation);
            let artifact_generation = Arc::clone(&artifact_generation);
            let artifact_gate = Arc::clone(&artifact_gate);
            let source_path = Arc::clone(&source_path);
            let webp = thumb.webp;
            let is_folder_book = matches!(book_source_kind(&source_path), BookSourceKind::Folder);
            let _ = tokio::task::spawn_blocking(move || {
                let _gate = artifact_gate.read();
                if generation.load(Ordering::Relaxed) != task_generation {
                    return;
                }
                if artifact_generation.load(Ordering::Relaxed) != task_artifact_generation {
                    return;
                }
                let Ok(snapshot) = FileSnapshot::read(&source_path) else {
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        path = %source_path.display(),
                        "deferred cache skipped because source path disappeared"
                    );
                    return;
                };
                let snapshot_matches = if is_folder_book {
                    snapshot.modified == modified
                } else {
                    snapshot.size == file_size && snapshot.modified == modified
                };
                if !snapshot_matches {
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        path = %source_path.display(),
                        expected_size = file_size,
                        actual_size = snapshot.size,
                        "deferred cache skipped because source snapshot changed"
                    );
                    return;
                }
                let write_result = disk_cache.put_thumb(&id, file_size, modified, &webp);
                match write_result {
                    Ok(()) if is_video_thumbnail => tracing::debug!(
                        id = %id.0.to_hex(),
                        "video cache persist"
                    ),
                    Ok(()) => {}
                    Err(e) => tracing::warn!("disk cache write: {e}"),
                }
            })
            .await;
        }

        match page_map {
            Some(DeferredPageMap::Cached(status)) => {
                page_map_coordinator.notify_page_map_cache_hit(status);
            }
            Some(DeferredPageMap::Fast(request)) => {
                page_map_coordinator.complete_fast(request).await;
            }
            Some(DeferredPageMap::Ready(request)) => {
                page_map_coordinator.complete_ready(request).await;
            }
            Some(DeferredPageMap::Complete { request }) => {
                page_map_coordinator.complete(request).await;
            }
            None => {}
        }
    }
}

// ── サムネイル生成処理 ────────────────────────────────────────────────────────

fn process_thumb(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    origin: RequestOrigin,
) -> (WorkerMsg, Option<DeferredCache>) {
    let id = &task.book_id;
    let source_revision =
        SourceRevision::from_file_state(task.expected_size, task.expected_modified);

    // 要求後に差し替わった古い結果は UI に返さない。
    if !thumb_task_file_snapshot_matches(&task) {
        let id_hex = id.0.to_hex();
        tracing::debug!(
            id = &id_hex[..8],
            path = %task.path.display(),
            "thumbnail task stale; file snapshot changed"
        );
        return (WorkerMsg::Stale(id.clone()), None);
    }

    shared.schedule_artifact_prune(id, Arc::clone(&task.path), source_revision.clone());

    let source_kind = book_source_kind(&task.path);
    let is_folder_book = matches!(source_kind, BookSourceKind::Folder);
    let is_zip_like = matches!(source_kind, BookSourceKind::Zip);
    let is_epub = matches!(source_kind, BookSourceKind::Epub);
    let is_page_map_supported_source = matches!(
        source_kind,
        BookSourceKind::Folder | BookSourceKind::Zip | BookSourceKind::Rar | BookSourceKind::Epub
    );
    let page_map_cache = if is_page_map_supported_source {
        shared.page_map_cache.as_ref()
    } else {
        None
    };
    let page_map_cached_page_count = if !task.bypass_cache {
        page_map_cache.and_then(|cache| {
            cache
                .get_page_map_for_revision(id, &source_revision)
                .map(|page_map| page_map.page_count())
        })
    } else {
        None
    };
    let page_map_cached = page_map_cached_page_count.is_some();
    let page_map_cache_hit = page_map_cached_page_count.map(|page_count| PageMapStatus {
        book_id: task.book_id.clone(),
        source_revision: source_revision.clone(),
        task_generation,
        task_artifact_generation: shared.artifact_generation.load(Ordering::Relaxed),
        failed: false,
        page_count: Some(page_count),
    });
    let task_for_page_map_cache_hit = task.clone();
    let attach_page_map_cache_hit =
        |result: (WorkerMsg, Option<DeferredCache>), status: Option<PageMapStatus>| {
            let (msg, deferred) = result;
            let deferred = match (deferred, status) {
                (Some(mut deferred), Some(status)) => {
                    deferred.page_map = Some(DeferredPageMap::Cached(status));
                    Some(deferred)
                }
                (None, Some(status)) => Some(DeferredCache {
                    generation: Arc::clone(generation),
                    artifact_generation: Arc::clone(&shared.artifact_generation),
                    artifact_gate: Arc::clone(&shared.artifact_gate),
                    page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
                    task_generation,
                    task_artifact_generation: status.task_artifact_generation,
                    disk_cache: Arc::clone(&shared.disk_cache),
                    id: task_for_page_map_cache_hit.book_id.clone(),
                    source_path: Arc::clone(&task_for_page_map_cache_hit.path),
                    file_size: task_for_page_map_cache_hit.expected_size,
                    modified: task_for_page_map_cache_hit.expected_modified,
                    thumb: None,
                    page_map: Some(DeferredPageMap::Cached(status)),
                }),
                (deferred, None) => deferred,
            };
            (msg, deferred)
        };
    let page_map_failed = !task.bypass_cache
        && shared.artifact_failure_cache.as_ref().is_some_and(|cache| {
            cache.has_failure_for_revision(id, &source_revision, ArtifactKind::PageMap)
        });
    if page_map_failed {
        tracing::debug!(
            id = %id.0.to_hex(),
            path = %task.path.display(),
            source_revision = ?source_revision,
            "thumbnail request skips page-map generation by failure cache"
        );
    }

    if !task.bypass_cache {
        if let Some(thumb) = shared.mem_cache.get(id, task.target_width) {
            if generation.load(Ordering::Relaxed) != task_generation {
                return (WorkerMsg::Stale(id.clone()), None);
            }
            let deferred = if !page_map_cached && !page_map_failed {
                page_map_cache.and_then(|cache| {
                    page_map_cache_miss_deferred(
                        &task,
                        &source_revision,
                        shared,
                        generation,
                        task_generation,
                        Arc::clone(cache),
                    )
                })
            } else {
                None
            };
            return attach_page_map_cache_hit(
                (
                    WorkerMsg::Ready(ReadyThumb {
                        book_id: id.clone(),
                        pixels: thumb.pixels,
                        width: thumb.width,
                        height: thumb.height,
                        expected_size: task.expected_size,
                        expected_modified: task.expected_modified,
                    }),
                    deferred,
                ),
                page_map_cache_hit,
            );
        }
    }

    if !task.bypass_cache {
        if let Some(webp_bytes) =
            shared
                .disk_cache
                .get_thumb(id, task.expected_size, task.expected_modified)
        {
            match img::decode_webp(&webp_bytes) {
                Ok(decoded) => {
                    if generation.load(Ordering::Relaxed) != task_generation {
                        return (WorkerMsg::Stale(id.clone()), None);
                    }
                    let deferred = if !page_map_cached && !page_map_failed {
                        page_map_cache.and_then(|cache| {
                            page_map_cache_miss_deferred(
                                &task,
                                &source_revision,
                                shared,
                                generation,
                                task_generation,
                                Arc::clone(cache),
                            )
                        })
                    } else {
                        None
                    };
                    return attach_page_map_cache_hit(
                        (
                            store_and_ready(decoded, task, shared, origin.is_visible()),
                            deferred,
                        ),
                        page_map_cache_hit,
                    );
                }
                Err(_) => {
                    let id_hex = id.0.to_hex();
                    tracing::warn!(id = &id_hex[..8], "broken disk cache entry, re-generating");
                }
            }
        }
    }

    if !task.bypass_cache
        && shared.artifact_failure_cache.as_ref().is_some_and(|cache| {
            cache.has_failure_for_revision(id, &source_revision, ArtifactKind::Thumbnail)
        })
    {
        tracing::debug!(
            id = %id.0.to_hex(),
            path = %task.path.display(),
            source_revision = ?source_revision,
            "thumbnail request skipped by failure cache"
        );
        return (WorkerMsg::FailedPermanent(id.clone()), None);
    }

    if is_folder_book {
        if let Some(page_map_cache) =
            page_map_cache.filter(|_| !page_map_cached && !page_map_failed)
        {
            return attach_page_map_cache_hit(
                process_folder_book_artifacts(
                    task,
                    shared,
                    generation,
                    task_generation,
                    origin.is_visible(),
                    Arc::clone(page_map_cache),
                ),
                page_map_cache_hit,
            );
        }
        return attach_page_map_cache_hit(
            process_folder_thumbnail_only(
                task,
                shared,
                generation,
                task_generation,
                origin.is_visible(),
            ),
            page_map_cache_hit,
        );
    }

    if is_zip_like {
        if let Some(page_map_cache) =
            page_map_cache.filter(|_| !page_map_cached && !page_map_failed)
        {
            return attach_page_map_cache_hit(
                process_zip_book_artifacts(
                    task,
                    shared,
                    generation,
                    task_generation,
                    origin.is_visible(),
                    Arc::clone(page_map_cache),
                ),
                page_map_cache_hit,
            );
        }
        return attach_page_map_cache_hit(
            process_zip_thumbnail_only(
                task,
                shared,
                generation,
                task_generation,
                origin.is_visible(),
            ),
            page_map_cache_hit,
        );
    }

    if is_epub {
        if let Some(page_map_cache) =
            page_map_cache.filter(|_| !page_map_cached && !page_map_failed)
        {
            return attach_page_map_cache_hit(
                process_epub_book_artifacts(
                    task,
                    shared,
                    generation,
                    task_generation,
                    origin.is_visible(),
                    Arc::clone(page_map_cache),
                ),
                page_map_cache_hit,
            );
        }
        return attach_page_map_cache_hit(
            process_epub_thumbnail_only(
                task,
                shared,
                generation,
                task_generation,
                origin.is_visible(),
            ),
            page_map_cache_hit,
        );
    }

    let raw = match read_thumb_source_bytes(&task.path) {
        Ok(raw) => raw,
        Err(e) => {
            let msg = failed_thumb_msg(id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "thumb source read: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "thumb source read: {e:#}");
            }
            return (msg, None);
        }
    };

    if raw.len() > MAX_THUMB_RAW_BYTES {
        tracing::info!(
            path    = %task.path.display(),
            raw_mb  = raw.len() / 1_048_576,
            "thumbnail raw image too large, skipping"
        );
        return (WorkerMsg::Failed(id.clone()), None);
    }

    let decoded =
        match img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32) {
            Ok(d) => d,
            Err(e) => {
                let msg = failed_thumb_msg_for_image_decode(id.clone(), &raw, &e);
                if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                    tracing::info!(path = %task.path.display(), "decode: {e:#}");
                } else {
                    tracing::warn!(path = %task.path.display(), "decode: {e:#}");
                }
                return (msg, None);
            }
        };

    let resized = match img::resize_to_width(decoded, task.target_width as u32) {
        Ok(r) => r,
        Err(e) => {
            let msg = failed_thumb_msg(id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "resize: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "resize: {e:#}");
            }
            return (msg, None);
        }
    };
    let webp = img::encode_webp(&resized).ok();

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(id.clone()), None);
    }
    if !thumb_task_file_snapshot_matches(&task) {
        return (WorkerMsg::Stale(id.clone()), None);
    }

    // decode/resize 完了後は UI を先に返し、WebP 保存は DeferredCache に分離する。
    let msg = store_and_ready(resized, task.clone(), shared, origin.is_visible());
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    let deferred = DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: task.book_id.clone(),
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: webp.map(|webp| DeferredThumbWrite { webp }),
        page_map: if !page_map_cached && !page_map_failed {
            page_map_cache.and_then(|cache| {
                let request = build_page_map_complete_request(
                    &task,
                    &source_revision,
                    shared,
                    task_generation,
                    Arc::clone(cache),
                );
                if shared
                    .page_map_coordinator
                    .reserve_page_map_complete_request(&request)
                {
                    Some(DeferredPageMap::Complete { request })
                } else {
                    None
                }
            })
        } else {
            None
        },
    };
    attach_page_map_cache_hit((msg, Some(deferred)), page_map_cache_hit)
}

fn page_map_cache_miss_deferred(
    task: &ThumbTask,
    source_revision: &SourceRevision,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    page_map_cache: Arc<PageMapDiskCache>,
) -> Option<DeferredCache> {
    let request = build_page_map_complete_request(
        task,
        source_revision,
        shared,
        task_generation,
        page_map_cache,
    );
    if !shared
        .page_map_coordinator
        .reserve_page_map_complete_request(&request)
    {
        return None;
    }
    Some(DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation: request.task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: task.book_id.clone(),
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: None,
        page_map: Some(DeferredPageMap::Complete { request }),
    })
}

fn page_map_cache_miss_deferred_for_task(
    task: &ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
) -> Option<DeferredCache> {
    if !thumb_task_file_snapshot_matches(task) {
        return None;
    }

    let source_kind = book_source_kind(&task.path);
    let is_page_map_supported_source = matches!(
        source_kind,
        BookSourceKind::Folder | BookSourceKind::Zip | BookSourceKind::Rar | BookSourceKind::Epub
    );
    if !is_page_map_supported_source {
        return None;
    }

    let page_map_cache = shared.page_map_cache.as_ref()?.clone();
    let source_revision =
        SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    if page_map_cache
        .get_page_map_for_revision(&task.book_id, &source_revision)
        .is_some()
    {
        return None;
    }
    if shared.artifact_failure_cache.as_ref().is_some_and(|cache| {
        cache.has_failure_for_revision(&task.book_id, &source_revision, ArtifactKind::PageMap)
    }) {
        return None;
    }

    shared.schedule_artifact_prune(
        &task.book_id,
        Arc::clone(&task.path),
        source_revision.clone(),
    );
    page_map_cache_miss_deferred(
        task,
        &source_revision,
        shared,
        generation,
        task_generation,
        page_map_cache,
    )
}

fn build_page_map_complete_request(
    task: &ThumbTask,
    source_revision: &SourceRevision,
    shared: &WorkerShared,
    task_generation: u64,
    page_map_cache: Arc<PageMapDiskCache>,
) -> PageMapCompleteRequest {
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    PageMapCompleteRequest {
        book_id: task.book_id.clone(),
        source_path: Arc::clone(&task.path),
        source_revision: source_revision.clone(),
        task_generation,
        task_artifact_generation,
        page_count: None,
        reason: None,
        page_map_cache,
    }
}

fn process_zip_thumbnail_only(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    cache_in_memory: bool,
) -> (WorkerMsg, Option<DeferredCache>) {
    let book_id = task.book_id.clone();
    let zip_scan_started = Instant::now();
    let reader = match crate::infra::archive::zip::ZipReader::open(&task.path) {
        Ok(reader) => reader,
        Err(e) => {
            let msg = failed_thumb_msg(book_id.clone(), &task.path, &e);
            return (msg, None);
        }
    };
    let zip_scan_ms = zip_scan_started.elapsed();
    let thumb_started = Instant::now();

    let raw = match reader.read_page_n(0) {
        Ok(raw) => raw,
        Err(e) => {
            let msg = failed_thumb_msg(book_id.clone(), &task.path, &e);
            return (msg, None);
        }
    };
    let decoded =
        match img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32) {
            Ok(d) => d,
            Err(e) => {
                let msg = failed_thumb_msg_for_image_decode(book_id.clone(), &raw, &e);
                return (msg, None);
            }
        };
    let resized = match img::resize_to_width(decoded, task.target_width as u32) {
        Ok(r) => r,
        Err(e) => {
            let msg = failed_thumb_msg(book_id.clone(), &task.path, &e);
            return (msg, None);
        }
    };
    let webp = img::encode_webp(&resized).ok();

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(book_id.clone()), None);
    }
    if !thumb_task_file_snapshot_matches(&task) {
        return (WorkerMsg::Stale(book_id.clone()), None);
    }

    let msg = store_and_ready(resized, task.clone(), shared, cache_in_memory);
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    let deferred = DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: book_id.clone(),
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: webp.map(|webp| DeferredThumbWrite { webp }),
        page_map: None,
    };
    let book_id_hex = book_id.0.to_hex();
    tracing::debug!(
        id = &book_id_hex[..8],
        path = %task.path.display(),
        zip_scan_ms = zip_scan_ms.as_millis(),
        thumb_ms = thumb_started.elapsed().as_millis(),
        "zip thumbnail only complete"
    );
    (msg, Some(deferred))
}

fn build_epub_thumbnail_lane(
    reader: &EpubImageReader,
    task: &ThumbTask,
) -> anyhow::Result<EpubThumbnailLaneResult> {
    let started = Instant::now();
    let raw = reader.read_page_n(0)?;
    let decoded = img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32)?;
    let decoded = img::resize_to_width(decoded, task.target_width as u32)?;

    Ok(EpubThumbnailLaneResult {
        decoded,
        elapsed: started.elapsed(),
    })
}

fn process_epub_thumbnail_only(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    cache_in_memory: bool,
) -> (WorkerMsg, Option<DeferredCache>) {
    let book_id = task.book_id.clone();
    let raw = match read_thumb_source_bytes(&task.path) {
        Ok(raw) => raw,
        Err(e) => {
            let msg = failed_thumb_msg(book_id.clone(), &task.path, &e);
            return (msg, None);
        }
    };

    if raw.len() > MAX_THUMB_RAW_BYTES {
        tracing::info!(
            path = %task.path.display(),
            raw_mb = raw.len() / 1_048_576,
            "thumbnail raw image too large, skipping"
        );
        return (WorkerMsg::Failed(book_id.clone()), None);
    }

    let decoded =
        match img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32) {
            Ok(d) => d,
            Err(e) => {
                let msg = failed_thumb_msg_for_image_decode(book_id.clone(), &raw, &e);
                return (msg, None);
            }
        };
    let resized = match img::resize_to_width(decoded, task.target_width as u32) {
        Ok(r) => r,
        Err(e) => {
            let msg = failed_thumb_msg(book_id.clone(), &task.path, &e);
            return (msg, None);
        }
    };
    let webp = img::encode_webp(&resized).ok();

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(book_id.clone()), None);
    }
    if !thumb_task_file_snapshot_matches(&task) {
        return (WorkerMsg::Stale(book_id.clone()), None);
    }

    let msg = store_and_ready(resized, task.clone(), shared, cache_in_memory);
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    let deferred = DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: book_id,
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: webp.map(|webp| DeferredThumbWrite { webp }),
        page_map: None,
    };
    (msg, Some(deferred))
}

fn process_epub_book_artifacts(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    cache_in_memory: bool,
    page_map_cache: Arc<PageMapDiskCache>,
) -> (WorkerMsg, Option<DeferredCache>) {
    let id = task.book_id.clone();
    let source_revision =
        SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    let artifact_started = Instant::now();
    let reader = match EpubImageReader::open(&task.path) {
        Ok(reader) => reader,
        Err(e) => {
            let msg = failed_thumb_msg(id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "epub open: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "epub open: {e:#}");
            }
            return (msg, None);
        }
    };
    let page_count = reader.page_count();

    let (thumb_result, page_map_result) = thread::scope(|scope| {
        let thumb_handle = scope.spawn(|| build_epub_thumbnail_lane(&reader, &task));
        let page_map_handle = scope
            .spawn(|| build_book_page_map_fast_from_epub_reader(&reader, source_revision.clone()));

        let thumb_result = match thumb_handle.join() {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("epub thumbnail lane panicked")),
        };
        let page_map_result = match page_map_handle.join() {
            Ok(result) => result,
            Err(_) => EpubPageMapFastOutcome::RequiresComplete,
        };
        (thumb_result, page_map_result)
    });

    let EpubThumbnailLaneResult {
        decoded,
        elapsed: thumb_lane_elapsed,
    } = match thumb_result {
        Ok(result) => result,
        Err(e) => {
            let msg = failed_thumb_msg(id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "epub thumbnail lane: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "epub thumbnail lane: {e:#}");
            }
            return (msg, None);
        }
    };

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(id.clone()), None);
    }
    if !thumb_task_file_snapshot_matches(&task) {
        return (WorkerMsg::Stale(id.clone()), None);
    }

    let webp = img::encode_webp(&decoded).ok();
    let msg = store_and_ready(decoded, task.clone(), shared, cache_in_memory);
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    let page_map_fast_ready = matches!(page_map_result, EpubPageMapFastOutcome::Ready(_));
    let page_map = match page_map_result {
        EpubPageMapFastOutcome::Ready(page_map) => {
            Some(DeferredPageMap::Ready(PageMapReadyPersistRequest {
                book_id: task.book_id.clone(),
                source_path: Arc::clone(&task.path),
                source_revision: source_revision.clone(),
                task_generation,
                task_artifact_generation,
                page_map,
                page_map_cache: Arc::clone(&page_map_cache),
            }))
        }
        EpubPageMapFastOutcome::RequiresComplete => {
            let request = build_page_map_complete_request(
                &task,
                &source_revision,
                shared,
                task_generation,
                Arc::clone(&page_map_cache),
            );
            if shared
                .page_map_coordinator
                .reserve_page_map_complete_request(&request)
            {
                Some(DeferredPageMap::Complete { request })
            } else {
                None
            }
        }
    };

    let id_hex = id.0.to_hex();
    tracing::debug!(
        id = &id_hex[..8],
        path = %task.path.display(),
        page_count = page_count,
        page_map_fast_ready = page_map_fast_ready,
        thumb_lane_ms = thumb_lane_elapsed.as_millis(),
        artifact_total_ms = artifact_started.elapsed().as_millis(),
        "epub thumbnail/page-map lanes complete"
    );

    let deferred = DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: id.clone(),
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: webp.map(|webp| DeferredThumbWrite { webp }),
        page_map,
    };
    (msg, Some(deferred))
}

fn build_zip_thumbnail_lane(
    reader: &crate::infra::archive::zip::ZipReader,
    task: &ThumbTask,
) -> anyhow::Result<ZipThumbnailLaneResult> {
    let started = Instant::now();
    let page0_info = reader
        .page_map_image_entry_infos()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no image in zip archive"))?;
    let raw = reader.read_page_n(page0_info.page_index)?;

    let decoded = img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32)?;
    let decoded = img::resize_to_width(decoded, task.target_width as u32)?;

    Ok(ZipThumbnailLaneResult {
        compression: page0_info.compression,
        compressed_size: page0_info.compressed_size,
        uncompressed_size: page0_info.uncompressed_size,
        decoded,
        elapsed: started.elapsed(),
    })
}

fn process_zip_book_artifacts(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    cache_in_memory: bool,
    page_map_cache: Arc<PageMapDiskCache>,
) -> (WorkerMsg, Option<DeferredCache>) {
    let id = task.book_id.clone();
    let source_revision =
        SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    let artifact_started = Instant::now();
    let zip_scan_started = artifact_started;
    let reader = match crate::infra::archive::zip::ZipReader::open(&task.path) {
        Ok(reader) => reader,
        Err(e) => {
            let msg = failed_thumb_msg(id, &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "zip open: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "zip open: {e:#}");
            }
            return (msg, None);
        }
    };
    let page_count = reader.page_count();
    let zip_scan_ms = zip_scan_started.elapsed();

    let (thumb_result, page_map_result) = thread::scope(|scope| {
        let thumb_handle = scope.spawn(|| build_zip_thumbnail_lane(&reader, &task));
        let page_map_handle = scope.spawn(|| build_zip_page_map_fast_lanes(&reader));

        let thumb_result = match thumb_handle.join() {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("zip thumbnail lane panicked")),
        };
        let page_map_result = match page_map_handle.join() {
            Ok(result) => result,
            Err(_) => ZipPageMapFastOutput {
                status: ZipPageMapFastStatus::Failed(ZipPageMapIssueReason::ZipStructure),
                pages: Vec::new(),
                issue: None,
                compressed_bytes_seen: 0,
                uncompressed_bytes_seen: 0,
                lightweight_pages: 0,
                compressed_bytes_touched: 0,
                uncompressed_bytes_produced: 0,
                slow_fallback_pages: 0,
                slow_fallback_failed_pages: 0,
                slow_fallback_ms: Duration::default(),
                slowest_fallback_entry: None,
                elapsed: Duration::default(),
            },
        };
        (thumb_result, page_map_result)
    });

    let ZipThumbnailLaneResult {
        decoded,
        compression: thumb_compression,
        compressed_size: thumb_compressed_size,
        uncompressed_size: thumb_uncompressed_size,
        elapsed: thumb_lane_elapsed,
        ..
    } = match thumb_result {
        Ok(result) => result,
        Err(e) => {
            let msg = failed_thumb_msg(id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "zip thumbnail lane: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "zip thumbnail lane: {e:#}");
            }
            return (msg, None);
        }
    };

    let ZipPageMapFastOutput {
        status: fast_lane_status,
        pages: page_map_pages,
        compressed_bytes_seen: page_map_compressed_bytes_seen,
        uncompressed_bytes_seen: page_map_uncompressed_bytes_seen,
        lightweight_pages: page_map_lightweight_pages,
        compressed_bytes_touched: page_map_compressed_bytes_touched,
        uncompressed_bytes_produced: page_map_uncompressed_bytes_produced,
        slow_fallback_pages: page_map_slow_fallback_pages,
        slow_fallback_failed_pages: page_map_slow_fallback_failed_pages,
        slow_fallback_ms: page_map_slow_fallback_ms,
        slowest_fallback_entry: page_map_slowest_fallback_entry,
        issue: page_map_issue,
        elapsed: page_map_elapsed,
    } = page_map_result;

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(id.clone()), None);
    }

    let webp = img::encode_webp(&decoded).ok();

    let id_hex = id.0.to_hex();
    tracing::debug!(
        id = &id_hex[..8],
        path = %task.path.display(),
        page_count = page_count,
        zip_scan_ms = zip_scan_ms.as_millis(),
        thumb_lane_ms = thumb_lane_elapsed.as_millis(),
        thumb_compression = ?thumb_compression,
        thumb_compressed_size = thumb_compressed_size,
        thumb_uncompressed_size = thumb_uncompressed_size,
        page_map_lane_ms = page_map_elapsed.as_millis(),
        artifact_total_ms = artifact_started.elapsed().as_millis(),
        page_map_lane_status = ?fast_lane_status,
        page_map_pages = page_map_pages.len(),
        page_map_compressed_bytes_seen = page_map_compressed_bytes_seen,
        page_map_uncompressed_bytes_seen = page_map_uncompressed_bytes_seen,
        page_map_lightweight_pages = page_map_lightweight_pages,
        page_map_compressed_bytes_touched = page_map_compressed_bytes_touched,
        page_map_uncompressed_bytes_produced = page_map_uncompressed_bytes_produced,
        page_map_slow_fallback_pages = page_map_slow_fallback_pages,
        page_map_slow_fallback_failed_pages = page_map_slow_fallback_failed_pages,
        page_map_slow_fallback_ms = page_map_slow_fallback_ms.as_millis(),
        slowest_fallback_entry = ?page_map_slowest_fallback_entry,
        page_map_issue = ?page_map_issue,
        "zip thumbnail/page-map lanes complete"
    );

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(id.clone()), None);
    }
    if !thumb_task_file_snapshot_matches(&task) {
        return (WorkerMsg::Stale(id.clone()), None);
    }

    let msg = store_and_ready(decoded, task.clone(), shared, cache_in_memory);
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    let page_map = Some(DeferredPageMap::Fast(PageMapFastPersistRequest {
        book_id: task.book_id.clone(),
        source_path: Arc::clone(&task.path),
        source_revision: source_revision.clone(),
        task_generation,
        task_artifact_generation,
        page_count,
        fast_lane_status,
        fast_lane_pages: page_map_pages,
        page_map_cache,
    }));
    let deferred = DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: id.clone(),
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: webp.map(|webp| DeferredThumbWrite { webp }),
        page_map,
    };
    (msg, Some(deferred))
}

fn build_folder_thumbnail_lane(
    reader: &FolderImageReader,
    task: &ThumbTask,
) -> anyhow::Result<FolderThumbnailLaneResult> {
    let started = Instant::now();
    let Some(page0_info) = reader.page_map_image_entry_infos().next() else {
        return Err(anyhow::anyhow!("no image in folder book"));
    };
    let raw = reader.read_page_n(page0_info.page_index)?;
    let decoded = img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32)?;
    let decoded = img::resize_to_width(decoded, task.target_width as u32)?;

    Ok(FolderThumbnailLaneResult {
        decoded,
        elapsed: started.elapsed(),
    })
}

fn process_folder_thumbnail_only(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    cache_in_memory: bool,
) -> (WorkerMsg, Option<DeferredCache>) {
    let book_id = task.book_id.clone();
    let raw = match read_thumb_source_bytes(&task.path) {
        Ok(raw) => raw,
        Err(e) => {
            let msg = failed_thumb_msg(book_id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "folder thumb source read: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "folder thumb source read: {e:#}");
            }
            return (msg, None);
        }
    };

    if raw.len() > MAX_THUMB_RAW_BYTES {
        tracing::info!(
            path    = %task.path.display(),
            raw_mb  = raw.len() / 1_048_576,
            "thumbnail raw image too large, skipping"
        );
        return (WorkerMsg::Failed(book_id.clone()), None);
    }

    let decoded =
        match img::decode_for_thumb(&raw, ImageFormatHint::Unknown, task.target_width as u32) {
            Ok(d) => d,
            Err(e) => {
                let msg = failed_thumb_msg_for_image_decode(book_id.clone(), &raw, &e);
                if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                    tracing::info!(path = %task.path.display(), "decode: {e:#}");
                } else {
                    tracing::warn!(path = %task.path.display(), "decode: {e:#}");
                }
                return (msg, None);
            }
        };

    let resized = match img::resize_to_width(decoded, task.target_width as u32) {
        Ok(r) => r,
        Err(e) => {
            let msg = failed_thumb_msg(book_id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "resize: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "resize: {e:#}");
            }
            return (msg, None);
        }
    };
    let webp = img::encode_webp(&resized).ok();

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(book_id.clone()), None);
    }
    if !thumb_task_file_snapshot_matches(&task) {
        return (WorkerMsg::Stale(book_id.clone()), None);
    }

    let msg = store_and_ready(resized, task.clone(), shared, cache_in_memory);
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    let deferred = DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: book_id,
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: webp.map(|webp| DeferredThumbWrite { webp }),
        page_map: None,
    };
    (msg, Some(deferred))
}

fn process_folder_book_artifacts(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    cache_in_memory: bool,
    page_map_cache: Arc<PageMapDiskCache>,
) -> (WorkerMsg, Option<DeferredCache>) {
    let id = task.book_id.clone();
    let source_revision =
        SourceRevision::from_file_state(task.expected_size, task.expected_modified);
    let artifact_started = Instant::now();
    let reader = match FolderImageReader::open(&task.path) {
        Ok(reader) => reader,
        Err(e) => {
            let msg = failed_thumb_msg(id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "folder open: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "folder open: {e:#}");
            }
            return (msg, None);
        }
    };

    let (thumb_result, page_map_result) = thread::scope(|scope| {
        let thumb_handle = scope.spawn(|| build_folder_thumbnail_lane(&reader, &task));
        let page_map_handle = scope.spawn(|| build_folder_page_map_fast_lanes(&reader));

        let thumb_result = match thumb_handle.join() {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("folder thumbnail lane panicked")),
        };
        let page_map_result = match page_map_handle.join() {
            Ok(result) => result,
            Err(_) => FolderPageMapFastLaneOutput {
                status: FolderPageMapFastStatus::Failed,
                pages: Vec::new(),
            },
        };
        (thumb_result, page_map_result)
    });

    let FolderThumbnailLaneResult {
        decoded,
        elapsed: thumb_lane_elapsed,
    } = match thumb_result {
        Ok(result) => result,
        Err(e) => {
            let msg = failed_thumb_msg(id.clone(), &task.path, &e);
            if matches!(msg, WorkerMsg::FailedPermanent(_)) {
                tracing::info!(path = %task.path.display(), "folder thumbnail lane: {e:#}");
            } else {
                tracing::warn!(path = %task.path.display(), "folder thumbnail lane: {e:#}");
            }
            return (msg, None);
        }
    };

    let FolderPageMapFastLaneOutput {
        status: fast_lane_status,
        pages: fast_lane_pages,
    } = page_map_result;

    if generation.load(Ordering::Relaxed) != task_generation {
        return (WorkerMsg::Stale(id.clone()), None);
    }
    if !thumb_task_file_snapshot_matches(&task) {
        return (WorkerMsg::Stale(id.clone()), None);
    }

    let webp = img::encode_webp(&decoded).ok();
    let msg = store_and_ready(decoded, task.clone(), shared, cache_in_memory);
    let task_artifact_generation = shared.artifact_generation.load(Ordering::Relaxed);
    let fast_lane_page_count = fast_lane_pages.len();

    let page_map = match fast_lane_status {
        FolderPageMapFastStatus::Ready => {
            Some(DeferredPageMap::Ready(PageMapReadyPersistRequest {
                book_id: task.book_id.clone(),
                source_path: Arc::clone(&task.path),
                source_revision: source_revision.clone(),
                task_generation,
                task_artifact_generation,
                page_map: BookPageMap::new(source_revision.clone(), fast_lane_pages),
                page_map_cache: Arc::clone(&page_map_cache),
            }))
        }
        FolderPageMapFastStatus::RequiresComplete => {
            let request = build_page_map_complete_request(
                &task,
                &source_revision,
                shared,
                task_generation,
                Arc::clone(&page_map_cache),
            );
            if shared
                .page_map_coordinator
                .reserve_page_map_complete_request(&request)
            {
                Some(DeferredPageMap::Complete { request })
            } else {
                None
            }
        }
        FolderPageMapFastStatus::Failed => {
            shared
                .page_map_coordinator
                .record_page_map_terminal_failure(
                    &task.book_id,
                    &source_revision,
                    task_generation,
                    task_artifact_generation,
                );
            None
        }
    };

    let id_hex = id.0.to_hex();
    tracing::debug!(
        id = &id_hex[..8],
        path = %task.path.display(),
        page_map_pages = fast_lane_page_count,
        page_map_fast_status = ?fast_lane_status,
        thumb_lane_ms = thumb_lane_elapsed.as_millis(),
        artifact_total_ms = artifact_started.elapsed().as_millis(),
        "folder thumbnail/page-map lanes complete"
    );

    let deferred = DeferredCache {
        generation: Arc::clone(generation),
        artifact_generation: Arc::clone(&shared.artifact_generation),
        artifact_gate: Arc::clone(&shared.artifact_gate),
        page_map_coordinator: Arc::clone(&shared.page_map_coordinator),
        task_generation,
        task_artifact_generation,
        disk_cache: Arc::clone(&shared.disk_cache),
        id: id.clone(),
        source_path: Arc::clone(&task.path),
        file_size: task.expected_size,
        modified: task.expected_modified,
        thumb: webp.map(|webp| DeferredThumbWrite { webp }),
        page_map,
    };
    (msg, Some(deferred))
}

fn open_book_reader_for_thumb_worker(
    path: &Path,
) -> anyhow::Result<Box<dyn crate::infra::archive::BookReader>> {
    open_book_reader(path)
}

fn read_thumb_source_bytes(path: &Path) -> anyhow::Result<bytes::Bytes> {
    if is_supported_image_path(path) {
        return std::fs::read(path)
            .map(bytes::Bytes::from)
            .map_err(|e| anyhow::anyhow!("read image file: {}: {e}", path.display()));
    }

    tracing::debug!(
        path = %path.display(),
        "archive open"
    );
    let reader = open_book_reader_for_thumb_worker(path)?;
    reader.read_first_image()
}

fn store_and_ready(
    decoded: img::DecodedImage,
    task: ThumbTask,
    shared: &WorkerShared,
    cache_in_memory: bool,
) -> WorkerMsg {
    let pixels: Arc<[u8]> = decoded.pixels.into();
    let (w, h) = (decoded.width as u16, decoded.height as u16);

    if cache_in_memory {
        shared.mem_cache.put(
            task.book_id.clone(),
            task.target_width,
            Thumbnail {
                width: w,
                height: h,
                pixels: Arc::clone(&pixels),
            },
        );
    }

    WorkerMsg::Ready(ReadyThumb {
        book_id: task.book_id,
        pixels,
        width: w,
        height: h,
        expected_size: task.expected_size,
        expected_modified: task.expected_modified,
    })
}

fn ready_from_decoded(decoded: img::DecodedImage, task: VideoThumbTask) -> ReadyThumb {
    let pixels: Arc<[u8]> = decoded.pixels.into();
    ReadyThumb {
        book_id: task.book_id,
        pixels,
        width: decoded.width as u16,
        height: decoded.height as u16,
        expected_size: task.expected_size,
        expected_modified: task.expected_modified,
    }
}
