//! サムネイル生成と Page Map 反映をまとめる worker。
//!
//! UI には先にサムネイルを返し、永続化と Page Map 反映は後段で処理する。
//! complete / slow Page Map の実処理は `PageMapCoordinator` に委譲する。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use parking_lot::RwLock;
use tokio::sync::{Semaphore, oneshot};

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
    PageMapReadyPersistRequest,
};
use crate::repaint::RepaintNotifier;
use crate::util::archive_path::is_supported_image_path;

/// 通常スロットのタイムアウト。PNG デコード等の長時間処理を許容するため 15s に延ばす。
const NORMAL_TIMEOUT: Duration = Duration::from_secs(15);
/// サイズ/更新日時が変化している間は転送中扱いとして、この間隔で再確認する。
const RETRY_TRANSFER_CHECK: Duration = Duration::from_secs(5);
/// サイズ/更新日時が安定している場合の再生成間隔。
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
];
/// OOM と長時間ブロックを避けるための thumb 用 raw データ上限。
const MAX_THUMB_RAW_BYTES: usize = 256 * 1024 * 1024;
/// Library thumbnail cache budget を CPU 側 ThumbMemCache にも適用する。
const THUMB_MEM_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;

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

/// Worker → UI への成功レスポンス
pub struct ReadyThumb {
    pub book_id: BookId,
    pub pixels: Arc<[u8]>,
    pub width: u16,
    pub height: u16,
}

/// Worker → UI へのメッセージ
pub enum WorkerMsg {
    Ready(ReadyThumb),
    Failed(BookId),
    /// サムネイル生成の恒久失敗。retry queue に入れず UI へ Failed として返す。
    /// rar / avif feature 無効時や、内容として確定的に失敗しているケースを含む。
    FailedPermanent(BookId),
    /// 要求後に同じ path/id のファイル内容が変わった古いタスク。UI へ失敗状態としては反映しない。
    Stale(BookId),
}

// ── ThumbWorker ───────────────────────────────────────────────────────────────

pub struct ThumbWorker {
    req_tx: tokio::sync::mpsc::UnboundedSender<WorkerReq>,
    resp_rx: std::sync::Mutex<std::sync::mpsc::Receiver<WorkerMsg>>,
    generation: Arc<AtomicU64>,
    artifact_generation: Arc<AtomicU64>,
}

enum WorkerReq {
    Task(ThumbTask, u64),
    PruneObsoleteArtifacts {
        id: BookId,
        source_path: Arc<Path>,
        source_revision: SourceRevision,
    },
    ClearPending,
    ClearCaches,
    RemoveArchiveCache(BookId),
    Shutdown,
}

impl ThumbWorker {
    pub fn spawn(repaint: RepaintNotifier, artifact_gate: Arc<RwLock<()>>) -> Self {
        let (req_tx, req_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerReq>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<WorkerMsg>();
        let generation = Arc::new(AtomicU64::new(0));
        let artifact_generation = Arc::new(AtomicU64::new(0));

        std::thread::Builder::new()
            .name("thumb-worker".into())
            .spawn({
                let generation = Arc::clone(&generation);
                let artifact_generation = Arc::clone(&artifact_generation);
                let artifact_gate = Arc::clone(&artifact_gate);
                let req_tx = req_tx.clone();
                move || {
                    worker_main(
                        req_rx,
                        req_tx,
                        resp_tx,
                        repaint,
                        generation,
                        artifact_generation,
                        artifact_gate,
                    )
                }
            })
            .map_err(|e| {
                tracing::error!("failed to spawn thumb-worker thread: {e}");
                e
            })
            .ok();

        Self {
            req_tx,
            resp_rx: std::sync::Mutex::new(resp_rx),
            generation,
            artifact_generation,
        }
    }

    pub fn request(&self, task: ThumbTask) {
        let generation = self.generation.load(Ordering::Relaxed);
        let _ = self.req_tx.send(WorkerReq::Task(task, generation));
    }

    pub fn clear_pending_tasks(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.req_tx.send(WorkerReq::ClearPending);
    }

    pub fn clear_cache_state(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.artifact_generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.req_tx.send(WorkerReq::ClearCaches);
    }

    pub fn remove_book_cache(&self, id: BookId) {
        self.artifact_generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.req_tx.send(WorkerReq::RemoveArchiveCache(id));
    }

    pub fn shutdown(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.req_tx.send(WorkerReq::Shutdown);
    }

    pub fn try_recv(&self) -> Option<WorkerMsg> {
        match self.resp_rx.lock() {
            Ok(rx) => rx.try_recv().ok(),
            Err(_) => {
                tracing::error!("thumb worker resp_rx mutex poisoned");
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

fn hdd_permit_weight(path: &Path, hdd_weight: u32) -> u32 {
    let medium = detect_storage_medium_cached(path);
    if hdd_weight > 1 && medium == StorageMedium::Hdd {
        hdd_weight
    } else {
        1
    }
}

// ── ワーカースレッド本体 ──────────────────────────────────────────────────────

fn worker_main(
    mut req_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerReq>,
    req_tx: tokio::sync::mpsc::UnboundedSender<WorkerReq>,
    resp_tx: std::sync::mpsc::Sender<WorkerMsg>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
    artifact_generation: Arc<AtomicU64>,
    artifact_gate: Arc<RwLock<()>>,
) {
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
    let page_map_coordinator = Arc::new(PageMapCoordinator::new(
        Arc::clone(&generation),
        Arc::clone(&artifact_generation),
        Arc::clone(&artifact_gate),
        artifact_failure_cache.as_ref().map(Arc::clone),
    ));
    let shared = Arc::new(WorkerShared {
        mem_cache: ThumbMemCache::new(THUMB_MEM_CACHE_MAX_BYTES),
        disk_cache: Arc::clone(&disk_cache),
        page_map_cache,
        artifact_failure_cache,
        page_map_coordinator,
        artifact_generation: Arc::clone(&artifact_generation),
        artifact_gate: Arc::clone(&artifact_gate),
        in_flight: Arc::new(Mutex::new(HashSet::new())),
        pruned_revisions: Arc::new(Mutex::new(HashSet::new())),
        req_tx,
    });

    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8);

    let max_blocking = (n * 4).max(32);
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
        let normal_slots = n;
        let normal_sem = Arc::new(Semaphore::new(normal_slots));
        // 表示要求に追従する旧世代掃除は 1 本に絞り、並列 I/O を増やさない。
        let prune_sem = Arc::new(Semaphore::new(1));
        let hdd_normal_permits: u32 = if normal_slots >= 2 { 2 } else { 1 };
        let (retry_tx, retry_rx) = tokio::sync::mpsc::unbounded_channel::<(ThumbTask, u64)>();

        tracing::info!(
            normal_slots,
            hdd_normal_permits,
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
            let normal_sem = Arc::clone(&normal_sem);
            let prune_sem = Arc::clone(&prune_sem);
            let retry_tx = retry_tx.clone();
            let generation = Arc::clone(&generation);
            async move {
                while let Some(req) = req_rx.recv().await {
                    match req {
                        WorkerReq::ClearPending => {
                            shared.clear_in_flight();
                            shared.page_map_coordinator.clear_all();
                            continue;
                        }
                        WorkerReq::ClearCaches => {
                            shared.mem_cache.clear();
                            shared.clear_in_flight();
                            shared.clear_pruned_revisions();
                            shared.page_map_coordinator.clear_all();
                            continue;
                        }
                        WorkerReq::RemoveArchiveCache(id) => {
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
                        WorkerReq::Shutdown => {
                            break;
                        }
                        WorkerReq::Task(task, task_gen) => {
                            if task_gen != generation.load(Ordering::Relaxed) {
                                continue;
                            }
                            let Some(flight) = shared.begin_task(&task) else {
                                tracing::debug!(
                                    id = %task.book_id.0.to_hex(),
                                    width = task.target_width,
                                    "duplicate thumb task skipped"
                                );
                                continue;
                            };
                            let permits = hdd_permit_weight(task.path.as_ref(), hdd_normal_permits);
                            let Ok(permit) =
                                Arc::clone(&normal_sem).acquire_many_owned(permits).await
                            else {
                                drop(flight);
                                tracing::error!("normal semaphore closed");
                                break;
                            };
                            let tx = resp_tx.clone();
                            let rtx = retry_tx.clone();
                            let repaint = repaint.clone();
                            let generation = Arc::clone(&generation);
                            tokio::spawn({
                                let shared = Arc::clone(&shared);
                                async move {
                                    run_thumb_task(
                                        task,
                                        ThumbTaskRuntime {
                                            shared,
                                            tx,
                                            retry_tx: Some(rtx),
                                            repaint,
                                            generation,
                                        },
                                        permit,
                                        Some(NORMAL_TIMEOUT),
                                        "normal",
                                        task_gen,
                                        flight,
                                    )
                                    .await;
                                }
                            });
                        }
                    }
                }
            }
        });

        // ── リトライキュー処理ループ ─────────────────────────────────────────
        // 一時失敗だけを 1 本の低優先 worker で再試行し、上限到達分だけ UI へ返す。
        let retry_loop = tokio::spawn({
            let shared = Arc::clone(&shared);
            let resp_tx = resp_tx.clone();
            let repaint = repaint.clone();
            let generation = Arc::clone(&generation);
            async move {
                retry_worker_loop(retry_rx, shared, resp_tx, repaint, generation).await;
            }
        });

        drop(retry_tx);
        let _ = tokio::join!(normal_loop, retry_loop);
    });
}

// ── タスク実行（normal / slow 共通）──────────────────────────────────────────

/// permit を保持したまま decode/resize を進め、結果は UI へ先に送る。
/// normal は timeout で離脱しても背景継続し、slow / retry は完了または実エラーまで待つ。
async fn run_thumb_task(
    task: ThumbTask,
    runtime: ThumbTaskRuntime,
    permit: tokio::sync::OwnedSemaphorePermit,
    timeout: Option<Duration>,
    _label: &'static str,
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
            ArtifactScope::ThumbnailAndPageMap,
        )
    });

    let (done_tx, done_rx) = oneshot::channel::<()>();
    let tx_for_watch = runtime.tx.clone();
    let retry_tx_for_watch = runtime.retry_tx.clone();
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
                        retry_tx: retry_tx_for_watch,
                        repaint: repaint_for_watch,
                        generation: generation_for_watch,
                    },
                )
                .await;
            }
            Err(join_err) => {
                tracing::error!(path = %path_disp_for_watch, "spawn_blocking panic: {join_err}");
                if let Some(rtx) = runtime.retry_tx {
                    let _ = rtx.send((task, task_gen));
                } else {
                    let _ = runtime.tx.send(WorkerMsg::Failed(task.book_id));
                    runtime.repaint.request_repaint();
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
    match msg {
        WorkerMsg::Ready(_) => {
            clear_thumbnail_failure(&runtime.shared, &task);
            let _ = runtime.tx.send(msg);
            runtime.repaint.request_repaint();
            // UI を先に返し、WebP 保存は後段で実行する。
            if let Some(dc) = deferred {
                tokio::spawn(async move {
                    dc.execute().await;
                });
            }
        }
        WorkerMsg::Stale(_) => {
            // 古い結果は UI にも retry queue にも流さない。
        }
        WorkerMsg::Failed(_) => {
            if let Some(rtx) = runtime.retry_tx {
                tracing::warn!(
                    path = %task.path.display(),
                    "thumb task failed → retry queue"
                );
                let _ = rtx.send((task, runtime.task_gen));
            } else {
                mark_thumbnail_failure(&runtime.shared, &task);
                let _ = runtime.tx.send(WorkerMsg::Failed(task.book_id));
                runtime.repaint.request_repaint();
            }
        }
        WorkerMsg::FailedPermanent(_) => {
            tracing::info!(
                path = %task.path.display(),
                "thumb task permanent failed → no retry"
            );
            mark_thumbnail_failure(&runtime.shared, &task);
            let _ = runtime.tx.send(WorkerMsg::Failed(task.book_id));
            runtime.repaint.request_repaint();
        }
    }
}

struct ThumbTaskRuntime {
    shared: Arc<WorkerShared>,
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    retry_tx: Option<tokio::sync::mpsc::UnboundedSender<(ThumbTask, u64)>>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
}

struct ThumbTaskResultContext {
    shared: Arc<WorkerShared>,
    task_gen: u64,
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    retry_tx: Option<tokio::sync::mpsc::UnboundedSender<(ThumbTask, u64)>>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
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

struct RetryThumbJob {
    task: ThumbTask,
    generation: u64,
    retry_count: usize,
    next_retry_at: Instant,
    last_snapshot: FileSnapshot,
}

async fn retry_worker_loop(
    mut retry_rx: tokio::sync::mpsc::UnboundedReceiver<(ThumbTask, u64)>,
    shared: Arc<WorkerShared>,
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    repaint: RepaintNotifier,
    generation: Arc<AtomicU64>,
) {
    let mut jobs: HashMap<BookId, RetryThumbJob> = HashMap::new();
    let tick = Duration::from_millis(200);

    loop {
        tokio::select! {
            Some((task, task_gen)) = retry_rx.recv() => {
                if task_gen != generation.load(Ordering::Relaxed) {
                    continue;
                }
                if let Err(task) = enqueue_retry_job(&mut jobs, task, task_gen) {
                    let _ = tx.send(WorkerMsg::Failed(task.book_id));
                    repaint.request_repaint();
                }
            }
            _ = tokio::time::sleep(tick) => {}
            else => {
                if jobs.is_empty() {
                    break;
                }
            }
        }

        let now = Instant::now();
        let Some(id) = jobs
            .iter()
            .filter(|(_, job)| job.next_retry_at <= now)
            .min_by_key(|(_, job)| job.next_retry_at)
            .map(|(id, _)| id.clone())
        else {
            continue;
        };

        let Some(mut job) = jobs.remove(&id) else {
            continue;
        };
        if job.generation != generation.load(Ordering::Relaxed) {
            continue;
        }
        let current = match FileSnapshot::read(&job.task.path) {
            Ok(s) => s,
            Err(e) => {
                let book_id_hex = job.task.book_id.0.to_hex();
                tracing::warn!(
                    id = &book_id_hex[..8],
                    path = %job.task.path.display(),
                    "retry metadata read failed → final failed: {e}"
                );
                let _ = tx.send(WorkerMsg::Failed(job.task.book_id));
                repaint.request_repaint();
                continue;
            }
        };

        if current != job.last_snapshot {
            let book_id_hex = job.task.book_id.0.to_hex();
            tracing::debug!(
                id = &book_id_hex[..8],
                path = %job.task.path.display(),
                old_size = job.last_snapshot.size,
                new_size = current.size,
                "retry: file is still changing; postpone"
            );
            job.last_snapshot = current;
            job.next_retry_at = Instant::now() + RETRY_TRANSFER_CHECK;
            jobs.insert(id, job);
            continue;
        }

        let book_id_hex = job.task.book_id.0.to_hex();
        tracing::debug!(
            id = &book_id_hex[..8],
            path = %job.task.path.display(),
            retry_count = job.retry_count,
            "retry: thumbnail task start"
        );

        let task_for_blocking = job.task.clone();
        let shared_for_blocking = Arc::clone(&shared);
        let generation_for_blocking = Arc::clone(&generation);
        let task_generation = job.generation;
        let handle = tokio::task::spawn_blocking(move || {
            process_thumb(
                task_for_blocking,
                &shared_for_blocking,
                &generation_for_blocking,
                task_generation,
                ArtifactScope::ThumbnailOnly,
            )
        });

        match handle.await {
            Ok((WorkerMsg::Ready(ready), deferred)) => {
                clear_thumbnail_failure(&shared, &job.task);
                let _ = tx.send(WorkerMsg::Ready(ready));
                repaint.request_repaint();
                if let Some(dc) = deferred {
                    tokio::spawn(async move {
                        dc.execute().await;
                    });
                }
            }
            Ok((WorkerMsg::Stale(_), _)) => {
                // 差し替え後の古い retry task。新しい scan/request 側に任せる。
            }
            Ok((WorkerMsg::FailedPermanent(_), _)) => {
                let book_id_hex = job.task.book_id.0.to_hex();
                tracing::info!(
                    id = &book_id_hex[..8],
                    path = %job.task.path.display(),
                    "retry: permanent failed"
                );
                mark_thumbnail_failure(&shared, &job.task);
                let _ = tx.send(WorkerMsg::Failed(job.task.book_id.clone()));
                repaint.request_repaint();
            }
            Ok((WorkerMsg::Failed(_), _)) | Err(_) => {
                job.retry_count += 1;
                if job.retry_count >= RETRY_DELAYS.len() {
                    let book_id_hex = job.task.book_id.0.to_hex();
                    tracing::warn!(
                        id = &book_id_hex[..8],
                        path = %job.task.path.display(),
                        retry_count = job.retry_count,
                        "retry: final failed"
                    );
                    mark_thumbnail_failure(&shared, &job.task);
                    let _ = tx.send(WorkerMsg::Failed(job.task.book_id.clone()));
                    repaint.request_repaint();
                } else {
                    let delay = RETRY_DELAYS[job.retry_count];
                    job.next_retry_at = Instant::now() + delay;
                    jobs.insert(id, job);
                }
            }
        }
    }
}

fn enqueue_retry_job(
    jobs: &mut HashMap<BookId, RetryThumbJob>,
    task: ThumbTask,
    generation: u64,
) -> Result<(), ThumbTask> {
    let id = task.book_id.clone();
    let snapshot = match FileSnapshot::read(&task.path) {
        Ok(s) => s,
        Err(e) => {
            let id_hex = id.0.to_hex();
            tracing::warn!(
                id = &id_hex[..8],
                path = %task.path.display(),
                "retry enqueue skipped: metadata read failed: {e}"
            );
            return Err(task);
        }
    };

    jobs.entry(id)
        .and_modify(|job| {
            job.task = task.clone();
            job.generation = generation;
            job.last_snapshot = snapshot;
            // 失敗回数は引き継ぐ。同じ不具合でキュー寿命を伸ばしすぎない。
        })
        .or_insert_with(|| RetryThumbJob {
            task,
            generation,
            retry_count: 0,
            next_retry_at: Instant::now() + RETRY_DELAYS[0],
            last_snapshot: snapshot,
        });
    Ok(())
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

    // rar feature 無効時だけ RAR/CBR を恒久失敗にする。feature 有効時は retry queue に任せる。
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

    // 形式や内容として確定的に失敗しているものは retry しても改善しない。
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
    current.size == task.expected_size && current.modified == task.expected_modified
}

// ── 共有状態 ──────────────────────────────────────────────────────────────────

struct WorkerShared {
    mem_cache: ThumbMemCache,
    disk_cache: Arc<DiskCache>, // バックグラウンド書き込みと共有する。
    page_map_cache: Option<Arc<PageMapDiskCache>>,
    artifact_failure_cache: Option<Arc<ArtifactFailureDiskCache>>,
    page_map_coordinator: Arc<PageMapCoordinator>,
    artifact_generation: Arc<AtomicU64>,
    artifact_gate: Arc<RwLock<()>>,
    in_flight: Arc<Mutex<HashSet<ThumbTaskKey>>>,
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
        if !guard.insert(key.clone()) {
            return None;
        }
        Some(TaskFlightGuard::new(Arc::clone(&self.in_flight), key))
    }

    fn clear_in_flight(&self) {
        if let Ok(mut guard) = self.in_flight.lock() {
            guard.clear();
        }
    }

    fn remove_in_flight_by_book_id(&self, id: &BookId) {
        if let Ok(mut guard) = self.in_flight.lock() {
            guard.retain(|key| &key.book_id != id);
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
    bypass_cache: bool,
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
            bypass_cache: task.bypass_cache,
        }
    }
}

struct TaskFlightGuard {
    in_flight: Arc<Mutex<HashSet<ThumbTaskKey>>>,
    key: Option<ThumbTaskKey>,
}

impl TaskFlightGuard {
    fn new(in_flight: Arc<Mutex<HashSet<ThumbTaskKey>>>, key: ThumbTaskKey) -> Self {
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
        if let Some(thumb) = thumb {
            let id = id.clone();
            let disk_cache = Arc::clone(&disk_cache);
            let generation = Arc::clone(&generation);
            let artifact_generation = Arc::clone(&artifact_generation);
            let artifact_gate = Arc::clone(&artifact_gate);
            let source_path = Arc::clone(&source_path);
            let webp = thumb.webp;
            let _ = tokio::task::spawn_blocking(move || {
                let _gate = artifact_gate.read();
                if generation.load(Ordering::Relaxed) != task_generation {
                    return;
                }
                if artifact_generation.load(Ordering::Relaxed) != task_artifact_generation {
                    return;
                }
                if !source_path.exists() {
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        path = %source_path.display(),
                        "deferred cache skipped because source path disappeared"
                    );
                    return;
                }
                let write_result = disk_cache.put_thumb(&id, file_size, modified, &webp);
                if let Err(e) = write_result {
                    tracing::warn!("disk cache write: {e}");
                }
            })
            .await;
        }

        match page_map {
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

#[derive(Clone, Copy)]
enum ArtifactScope {
    ThumbnailAndPageMap,
    ThumbnailOnly,
}

fn process_thumb(
    task: ThumbTask,
    shared: &WorkerShared,
    generation: &Arc<AtomicU64>,
    task_generation: u64,
    artifact_scope: ArtifactScope,
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

    if matches!(artifact_scope, ArtifactScope::ThumbnailAndPageMap) {
        shared.schedule_artifact_prune(id, Arc::clone(&task.path), source_revision.clone());
    }

    let source_kind = book_source_kind(&task.path);
    let is_folder_book = matches!(source_kind, BookSourceKind::Folder);
    let is_zip_like = matches!(source_kind, BookSourceKind::Zip);
    let is_epub = matches!(source_kind, BookSourceKind::Epub);
    let is_page_map_supported_source = matches!(
        source_kind,
        BookSourceKind::Folder | BookSourceKind::Zip | BookSourceKind::Rar | BookSourceKind::Epub
    );
    let page_map_cache = if matches!(artifact_scope, ArtifactScope::ThumbnailAndPageMap)
        && is_page_map_supported_source
    {
        shared.page_map_cache.as_ref()
    } else {
        None
    };
    let page_map_cached = !task.bypass_cache
        && page_map_cache.is_some_and(|cache| {
            cache
                .get_page_map_for_revision(id, &source_revision)
                .is_some()
        });
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
            return (
                WorkerMsg::Ready(ReadyThumb {
                    book_id: id.clone(),
                    pixels: thumb.pixels,
                    width: thumb.width,
                    height: thumb.height,
                }),
                deferred,
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
                    return (store_and_ready(decoded, task, shared), deferred);
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
            return process_folder_book_artifacts(
                task,
                shared,
                generation,
                task_generation,
                Arc::clone(page_map_cache),
            );
        }
        return process_folder_thumbnail_only(task, shared, generation, task_generation);
    }

    if is_zip_like {
        if let Some(page_map_cache) =
            page_map_cache.filter(|_| !page_map_cached && !page_map_failed)
        {
            return process_zip_book_artifacts(
                task,
                shared,
                generation,
                task_generation,
                Arc::clone(page_map_cache),
            );
        }
        return process_zip_thumbnail_only(task, shared, generation, task_generation);
    }

    if is_epub {
        if let Some(page_map_cache) =
            page_map_cache.filter(|_| !page_map_cached && !page_map_failed)
        {
            return process_epub_book_artifacts(
                task,
                shared,
                generation,
                task_generation,
                Arc::clone(page_map_cache),
            );
        }
        return process_epub_thumbnail_only(task, shared, generation, task_generation);
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
    let msg = store_and_ready(resized, task.clone(), shared);
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
    (msg, Some(deferred))
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

    let msg = store_and_ready(resized, task.clone(), shared);
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

    let msg = store_and_ready(resized, task.clone(), shared);
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
    let msg = store_and_ready(decoded, task.clone(), shared);
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

    let msg = store_and_ready(decoded, task.clone(), shared);
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

    let msg = store_and_ready(resized, task.clone(), shared);
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
    let msg = store_and_ready(decoded, task.clone(), shared);
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
                .record_page_map_terminal_failure(&task.book_id, &source_revision);
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
) -> WorkerMsg {
    let pixels: Arc<[u8]> = decoded.pixels.into();
    let (w, h) = (decoded.width as u16, decoded.height as u16);

    shared.mem_cache.put(
        task.book_id.clone(),
        task.target_width,
        Thumbnail {
            width: w,
            height: h,
            pixels: Arc::clone(&pixels),
        },
    );

    WorkerMsg::Ready(ReadyThumb {
        book_id: task.book_id,
        pixels,
        width: w,
        height: h,
    })
}

// ── テスト ────────────────────────────────────────────────────────────────────
