#![cfg_attr(test, allow(dead_code))]
//! Viewer ページローダー。
//!
//! 最新勝ちの request queue、reader cache、frame cache、animation stream をまとめて持つ。
//! interactive worker 2 本と background worker 群を起動し、viewer 用 decode を返す。

use std::{
    collections::HashMap,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::Instant,
};

#[cfg(test)]
use std::collections::VecDeque;

use anyhow::Context;
use bytes::Bytes;
use eframe::egui;
use lru::LruCache;

use crate::{
    domain::app_settings::ViewerQuality,
    domain::page::ImageFormatHint,
    infra::{archive::folder::FolderImageReader, archive::open_book_reader, image::decode as img},
};

/// frame_cache は worker 数に連動させず 2 件固定。
pub const FRAME_CACHE_CAP: usize = 2;

pub fn frame_cache_cap_from_worker_count(_worker_count: usize) -> usize {
    FRAME_CACHE_CAP
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ViewerFrameStage {
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerRequestKind {
    Display,
    AnimationStreamStart,
    AnimationStreamFill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerResultKind {
    Display,
    AnimationFramesChunk,
}

// ── 公開型 ────────────────────────────────────────────────────────────────────

/// ページロードリクエスト。
#[derive(Clone)]
pub struct ViewerRequest {
    /// リクエスト識別子。古い結果を捨てるために使う。
    pub id: u64,
    /// アーカイブのパス
    pub path: Arc<Path>,
    /// 左ページ（単ページ時 or 見開き左）。None = ブランク
    pub page_left: Option<u32>,
    /// 右ページ（見開き右）。None = なし
    pub page_right: Option<u32>,
    /// 表示幅（px）。mozjpeg DCT スケール選択に使用。0 = スケーリングなし
    pub display_w: u32,
    /// 表示高さ（px）。縦長ページの target 推定に使用。0 = 高さ制約なし
    pub display_h: u32,
    /// ビューア画質プロファイル
    pub quality: ViewerQuality,
    /// GPU テクスチャ上限（wgpu max_texture_dimension_2d）。0 = 8192 として扱う
    pub max_tex_side: u32,
    /// ワーカーフレームキャッシュ上限（件数）
    pub frame_cache_cap: usize,
    /// リクエスト種別
    pub kind: ViewerRequestKind,
    /// UI 側 view index（ログ相関用）
    pub view_idx: u32,
    /// enqueue 時刻（queue wait 計測用）
    pub enqueued_at: Instant,
    /// 同一ナビゲーション相関ID
    pub nav_id: u64,
    /// true=表示要求, false=background
    pub interactive: bool,
}

/// ページロード結果。
#[derive(Clone, Debug)]
pub struct ViewerResult {
    /// 対応するリクエスト ID（古い結果は UI 側で破棄）
    pub request_id: u64,
    /// 左ページのデコード済みフレーム列（None = ブランク or エラー）
    /// Arc 共有: キャッシュヒット時のコピーコストなし
    pub left: Option<Arc<Vec<img::FrameData>>>,
    /// 右ページのデコード済みフレーム列
    pub right: Option<Arc<Vec<img::FrameData>>>,
    /// アーカイブのページ数（初回ロード時に確定）
    pub page_count: u32,
    /// 左ページの元の幅（リサイズ前）
    pub left_orig_w: u32,
    /// 左ページの元の高さ（リサイズ前）
    pub left_orig_h: u32,
    /// 右ページの元の幅（リサイズ前）
    pub right_orig_w: u32,
    /// 右ページの元の高さ（リサイズ前）
    pub right_orig_h: u32,
    /// エラーメッセージ（Some の場合は表示に失敗）
    pub error: Option<String>,
    /// 結果種別
    pub kind: ViewerResultKind,
    /// 左ページが animation stream の初回チャンクか
    pub left_is_animation_stream: bool,
    /// 右ページが animation stream の初回チャンクか
    pub right_is_animation_stream: bool,
    /// animation stream chunk の exhausted 状態（左）
    pub left_stream_exhausted: bool,
    /// animation stream chunk の exhausted 状態（右）
    pub right_stream_exhausted: bool,
    /// worker queue wait
    pub queue_wait_ms: u128,
    /// worker decode 時間（合算）
    pub decode_ms: u128,
    /// UI 側 view index（ログ相関用）
    pub view_idx: u32,
    /// 同一ナビゲーション相関ID
    pub nav_id: u64,
    /// 要求時ページ（stale時のcache判断用）
    pub page_left: Option<u32>,
    pub page_right: Option<u32>,
    /// ワーカー要求時の表示条件（RGBA cache key整合性用）
    pub request_display_w: u32,
    pub request_display_h: u32,
    pub request_quality: ViewerQuality,
    pub request_max_tex_side: u32,
    /// 実行worker名
    pub worker: String,
    /// true の場合は UI が直接発行した interactive リクエストの結果。
    #[allow(dead_code)]
    pub interactive: bool,
}

#[derive(Clone)]
pub struct ViewerLoadRequest {
    pub path: Arc<Path>,
    pub view_idx: u32,
    pub page_left: Option<u32>,
    pub page_right: Option<u32>,
    pub display_w: u32,
    pub display_h: u32,
    pub quality: ViewerQuality,
    pub max_tex_side: u32,
    pub frame_cache_cap: usize,
    pub nav_id: u64,
    pub interactive: bool,
}

// ── ViewerLoader ──────────────────────────────────────────────────────────────

/// バックグラウンドスレッドへのハンドル。
pub struct ViewerLoader {
    interactive_even_shared: Arc<SharedQueue>,
    interactive_odd_shared: Arc<SharedQueue>,
    background_shards: Vec<Arc<SharedQueue>>,
    background_worker_count: usize,
    interactive_result_rx: Mutex<mpsc::Receiver<ViewerResult>>,
    background_result_rx: Mutex<mpsc::Receiver<ViewerResult>>,
    #[cfg(test)]
    test_interactive_result_rx: Mutex<VecDeque<ViewerResult>>,
    #[cfg(test)]
    test_background_result_rx: Mutex<VecDeque<ViewerResult>>,
    next_id: AtomicU64,
}

#[derive(Debug)]
pub enum ViewerLoaderInitError {
    ThreadSpawn {
        worker: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for ViewerLoaderInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ThreadSpawn { worker, source } => {
                write!(f, "viewer loader thread spawn failed ({worker}): {source}")
            }
        }
    }
}

impl std::error::Error for ViewerLoaderInitError {}

impl Drop for ViewerLoader {
    fn drop(&mut self) {
        self.interactive_even_shared.shutdown();
        self.interactive_odd_shared.shutdown();
        for shared in &self.background_shards {
            shared.shutdown();
        }
    }
}

impl ViewerLoader {
    /// ローダースレッドを起動して `ViewerLoader` を返す。
    pub fn spawn(
        ctx: egui::Context,
        background_worker_count: usize,
    ) -> Result<Self, ViewerLoaderInitError> {
        let background_worker_count = background_worker_count.max(1);
        let interactive_even_shared = Arc::new(SharedQueue {
            pending: Mutex::new(None),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let interactive_odd_shared = Arc::new(SharedQueue {
            pending: Mutex::new(None),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let background_shards: Vec<Arc<SharedQueue>> = (0..background_worker_count)
            .map(|_| {
                Arc::new(SharedQueue {
                    pending: Mutex::new(None),
                    condvar: Condvar::new(),
                    shutdown: AtomicBool::new(false),
                })
            })
            .collect();
        let (interactive_result_tx, interactive_result_rx) = mpsc::channel();
        let (background_result_tx, background_result_rx) = mpsc::channel();
        let mut started_queues: Vec<Arc<SharedQueue>> = Vec::new();

        let interactive_even_shared2 = Arc::clone(&interactive_even_shared);
        let result_tx_interactive_even = interactive_result_tx.clone();
        let ctx_interactive_even = ctx.clone();
        if let Err(e) = thread::Builder::new()
            .name("viewer-loader-interactive-even".into())
            .spawn(move || {
                worker_loop(
                    interactive_even_shared2,
                    result_tx_interactive_even,
                    ctx_interactive_even,
                    "interactive-even".to_string(),
                    true,
                )
            })
        {
            shutdown_started_queues(&started_queues);
            return Err(ViewerLoaderInitError::ThreadSpawn {
                worker: "interactive-even".to_owned(),
                source: e,
            });
        }
        started_queues.push(Arc::clone(&interactive_even_shared));

        let interactive_odd_shared2 = Arc::clone(&interactive_odd_shared);
        let result_tx_interactive_odd = interactive_result_tx.clone();
        let ctx_interactive_odd = ctx.clone();
        if let Err(e) = thread::Builder::new()
            .name("viewer-loader-interactive-odd".into())
            .spawn(move || {
                worker_loop(
                    interactive_odd_shared2,
                    result_tx_interactive_odd,
                    ctx_interactive_odd,
                    "interactive-odd".to_string(),
                    true,
                )
            })
        {
            shutdown_started_queues(&started_queues);
            return Err(ViewerLoaderInitError::ThreadSpawn {
                worker: "interactive-odd".to_owned(),
                source: e,
            });
        }
        started_queues.push(Arc::clone(&interactive_odd_shared));

        for (shard_idx, background_shared) in background_shards.iter().enumerate() {
            let background_shared = Arc::clone(background_shared);
            let background_shared_for_thread = Arc::clone(&background_shared);
            let result_tx_bg = background_result_tx.clone();
            let ctx_bg = ctx.clone();
            let worker_name = format!("background-{}", shard_idx);
            let thread_name = format!("viewer-loader-{worker_name}");
            if let Err(e) = thread::Builder::new().name(thread_name).spawn(move || {
                worker_loop(
                    background_shared_for_thread,
                    result_tx_bg,
                    ctx_bg,
                    worker_name,
                    false,
                )
            }) {
                shutdown_started_queues(&started_queues);
                return Err(ViewerLoaderInitError::ThreadSpawn {
                    worker: format!("background-{shard_idx}"),
                    source: e,
                });
            }
            started_queues.push(Arc::clone(&background_shared));
        }

        Ok(Self {
            interactive_even_shared,
            interactive_odd_shared,
            background_shards,
            background_worker_count,
            interactive_result_rx: Mutex::new(interactive_result_rx),
            background_result_rx: Mutex::new(background_result_rx),
            #[cfg(test)]
            test_interactive_result_rx: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            test_background_result_rx: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
        })
    }

    #[cfg(test)]
    pub(crate) fn spawn_for_tests(background_worker_count: usize) -> Self {
        let background_worker_count = background_worker_count.max(1);
        let interactive_even_shared = Arc::new(SharedQueue {
            pending: Mutex::new(None),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let interactive_odd_shared = Arc::new(SharedQueue {
            pending: Mutex::new(None),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let background_shards: Vec<Arc<SharedQueue>> = (0..background_worker_count)
            .map(|_| {
                Arc::new(SharedQueue {
                    pending: Mutex::new(None),
                    condvar: Condvar::new(),
                    shutdown: AtomicBool::new(false),
                })
            })
            .collect();
        let (_interactive_result_tx, interactive_result_rx) = mpsc::channel();
        let (_background_result_tx, background_result_rx) = mpsc::channel();

        Self {
            interactive_even_shared,
            interactive_odd_shared,
            background_shards,
            background_worker_count,
            interactive_result_rx: Mutex::new(interactive_result_rx),
            background_result_rx: Mutex::new(background_result_rx),
            #[cfg(test)]
            test_interactive_result_rx: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            test_background_result_rx: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// フルデコードリクエストを送信し、割り当てられた ID を返す。
    /// 未処理の古いリクエストは上書きされる（最新勝ち）。
    pub fn send_request(&self, request: ViewerLoadRequest) -> u64 {
        self.enqueue(Self::build_request(
            request,
            ViewerRequestKind::Display,
            self.next_id.fetch_add(1, Ordering::Relaxed),
        ))
    }

    pub fn send_animation_stream_start(&self, request: ViewerLoadRequest) -> u64 {
        self.enqueue(Self::build_request(
            request,
            ViewerRequestKind::AnimationStreamStart,
            self.next_id.fetch_add(1, Ordering::Relaxed),
        ))
    }

    pub fn send_animation_stream_fill(&self, request: ViewerLoadRequest) -> u64 {
        self.enqueue(Self::build_request(
            request,
            ViewerRequestKind::AnimationStreamFill,
            self.next_id.fetch_add(1, Ordering::Relaxed),
        ))
    }

    fn build_request(
        request: ViewerLoadRequest,
        kind: ViewerRequestKind,
        id: u64,
    ) -> ViewerRequest {
        ViewerRequest {
            id,
            path: request.path,
            page_left: request.page_left,
            page_right: request.page_right,
            display_w: request.display_w,
            display_h: request.display_h,
            quality: request.quality,
            max_tex_side: request.max_tex_side,
            frame_cache_cap: request.frame_cache_cap,
            kind,
            view_idx: request.view_idx,
            enqueued_at: Instant::now(),
            nav_id: request.nav_id,
            interactive: request.interactive,
        }
    }

    fn enqueue(&self, req: ViewerRequest) -> u64 {
        let id = req.id;
        let page = req.page_left.or(req.page_right).unwrap_or(0);
        let shared = if req.interactive {
            if page % 2 == 0 {
                &self.interactive_even_shared
            } else {
                &self.interactive_odd_shared
            }
        } else {
            &self.background_shards[page as usize % self.background_worker_count]
        };
        {
            let mut guard = match shared.pending.lock() {
                Ok(g) => g,
                Err(_) => {
                    tracing::error!("viewer_loader pending mutex poisoned");
                    return id;
                }
            };
            *guard = Some(req);
        }
        shared.condvar.notify_one();
        id
    }

    /// 完了した結果を 1 件取り出す（ノンブロッキング）。
    pub fn try_recv_interactive(&self) -> Option<ViewerResult> {
        #[cfg(test)]
        if let Ok(mut rx) = self.test_interactive_result_rx.lock() {
            if let Some(result) = rx.pop_front() {
                return Some(result);
            }
        }
        match self.interactive_result_rx.lock() {
            Ok(rx) => rx.try_recv().ok(),
            Err(_) => {
                tracing::error!("viewer_loader interactive_result_rx mutex poisoned");
                None
            }
        }
    }

    pub fn try_recv_background(&self) -> Option<ViewerResult> {
        #[cfg(test)]
        if let Ok(mut rx) = self.test_background_result_rx.lock() {
            if let Some(result) = rx.pop_front() {
                return Some(result);
            }
        }
        match self.background_result_rx.lock() {
            Ok(rx) => rx.try_recv().ok(),
            Err(_) => {
                tracing::error!("viewer_loader background_result_rx mutex poisoned");
                None
            }
        }
    }

    /// チャネル内の未受信結果を全て破棄する。
    /// ViewerState を drop する前に呼ぶことで Arc<Vec<FrameData>> を確実に解放する。
    pub fn flush(&self) {
        while self.try_recv_interactive().is_some() {}
        while self.try_recv_background().is_some() {}
    }

    #[cfg(test)]
    pub(crate) fn push_test_result(&self, result: ViewerResult) {
        if result.interactive {
            if let Ok(mut rx) = self.test_interactive_result_rx.lock() {
                rx.push_back(result);
            }
        } else if let Ok(mut rx) = self.test_background_result_rx.lock() {
            rx.push_back(result);
        }
    }

    pub fn peek_next_request_id(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed)
    }
}

// ── 内部型・ワーカー ──────────────────────────────────────────────────────────

struct SharedQueue {
    pending: Mutex<Option<ViewerRequest>>,
    condvar: Condvar,
    shutdown: AtomicBool,
}

impl SharedQueue {
    fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.condvar.notify_all();
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

/// キャッシュキー: (page_n, display_w, display_h, quality)
type PageKey = (u32, u32, u32, ViewerQuality, ViewerFrameStage);

/// BookReader を再利用するためのキャッシュエントリ。
struct CachedReader {
    path: PathBuf,
    reader: Box<dyn crate::infra::archive::BookReader>,

    /// プローブで解凍済みのバイト列キャッシュ: (page_n, bytes)。
    /// フルデコード時に消費して Deflate 二重展開を防ぐ。
    raw_cache: Option<(u32, Bytes)>,

    /// デコード済みフレーム LRU キャッシュ: (page_n, display_w) → Arc<frames>。
    /// キャッシュヒット時はフル ZIP 解凍 + JPEG デコードをスキップ（ページ戻り等に有効）。
    frame_cache: LruCache<PageKey, Arc<Vec<img::FrameData>>>,
    /// animated WebP の逐次フレーム供給状態。
    animation_streams: HashMap<AnimationStreamKey, CachedAnimationStream>,
}

type AnimationStreamKey = (u32, u32, u32, ViewerQuality);

struct CachedAnimationStream {
    source: img::WebpAnimFrameSource,
}

const FAILED_PAGE_TEXT: &str = "PAGE LOAD FAILED";
const FAILED_PAGE_BG_RGBA: [u8; 4] = [224, 224, 224, 255];
const FAILED_PAGE_FG_RGBA: [u8; 4] = [80, 80, 80, 255];
const FAILED_PAGE_FONT_W: u32 = 5;
const FAILED_PAGE_FONT_H: u32 = 7;
// 文字サイズはページ高さの約 1/2 まで下げる目安。
const FAILED_PAGE_TEXT_HEIGHT_SCALE_DIVISOR: u32 = 6;
// 幅側も同じく縮めて、幅基準で scale が支配されても小さくする。
const FAILED_PAGE_TEXT_WIDTH_SCALE_DIVISOR: u32 = 2;
const FAILED_PAGE_TEXT_MIN_SCALE: u32 = 2;

#[derive(Clone)]
struct FailedPageSpec {
    frames: Arc<Vec<img::FrameData>>,
    width: u32,
    height: u32,
}

struct PageLoadOutcome {
    page: DisplayPage,
}

fn worker_loop(
    shared: Arc<SharedQueue>,
    result_tx: mpsc::Sender<ViewerResult>,
    ctx: egui::Context,
    worker_name: String,
    check_superseded: bool,
) {
    let mut cache: Option<CachedReader> = None;

    loop {
        // リクエストが届くまで待機（Condvar）
        let req: ViewerRequest = {
            let mut guard = match shared.pending.lock() {
                Ok(g) => g,
                Err(_) => {
                    tracing::error!("viewer_loader pending mutex poisoned");
                    break;
                }
            };
            loop {
                if let Some(r) = guard.take() {
                    break r;
                }
                if shared.is_shutdown() {
                    return;
                }
                guard = match shared.condvar.wait(guard) {
                    Ok(g) => g,
                    Err(_) => {
                        tracing::error!("viewer_loader condvar wait poisoned");
                        return;
                    }
                };
            }
        };
        if check_superseded {
            let superseded = {
                match shared.pending.lock() {
                    Ok(guard) => guard
                        .as_ref()
                        .map(|pending| (pending.id, pending.nav_id, pending.view_idx)),
                    Err(_) => None,
                }
            };
            if let Some((latest_req, latest_nav_id, latest_view)) = superseded {
                if latest_req != req.id {
                    let queue_wait_ms = Instant::now()
                        .saturating_duration_since(req.enqueued_at)
                        .as_millis();
                    tracing::trace!(
                        "[viewer-request-skip] nav_id={} req={} view={} reason=superseded_before_decode latest_nav_id={} latest_view={} latest_req={} req_kind={:?} interactive={} queue_wait_ms={} worker={}",
                        req.nav_id,
                        req.id,
                        req.view_idx,
                        latest_nav_id,
                        latest_view,
                        latest_req,
                        req.kind,
                        if req.interactive { "interactive" } else { "background" },
                        queue_wait_ms,
                        worker_name
                    );
                    continue;
                }
            }
        }
        tracing::trace!(
            "[viewer-request-start] nav_id={} req={} view={} kind={} worker={}",
            req.nav_id,
            req.id,
            req.view_idx,
            if req.interactive {
                "interactive"
            } else {
                "background"
            },
            worker_name
        );

        let result = process_request(&req, &mut cache, &worker_name);
        tracing::trace!(
            request_id = result.request_id,
            page_count  = result.page_count,
            left  = result.left.is_some(),
            right = result.right.is_some(),
            error = ?result.error,
            "viewer_loader: request done"
        );

        // 結果送信（受信側が既にドロップしていればスレッドを終了）
        if result_tx.send(result).is_err() {
            break;
        }
        tracing::trace!("viewer_loader: request_repaint sent");
        ctx.request_repaint();
    }
}

fn process_request(
    req: &ViewerRequest,
    cache: &mut Option<CachedReader>,
    worker_name: &str,
) -> ViewerResult {
    let req_started = Instant::now();
    let queue_wait_ms = req_started
        .saturating_duration_since(req.enqueued_at)
        .as_millis();
    tracing::trace!(
        "[viewer-worker-start] nav_id={} req={} view={} queue_wait_ms={} kind={} worker={}",
        req.nav_id,
        req.id,
        req.view_idx,
        queue_wait_ms,
        if req.interactive {
            "interactive"
        } else {
            "background"
        },
        worker_name
    );
    if !req.interactive {
        if let Some(page) = req.page_left.or(req.page_right) {
            let shard = if page % 2 == 0 { "even" } else { "odd" };
            tracing::trace!(
                "[prefetch-page-start] req={} page={} shard={} view={}",
                req.id,
                page,
                shard,
                req.view_idx
            );
        }
    }
    // ── ZipReader キャッシュ確認 ─────────────────────────────────────────────
    let reader_ok = cache
        .as_ref()
        .map(|c| c.path.as_path() == req.path.as_ref())
        .unwrap_or(false);

    if !reader_ok {
        let open_started = Instant::now();
        match open_book_reader_for_viewer_worker(req.path.as_ref()) {
            Ok(r) => {
                tracing::debug!(
                    path = %req.path.display(),
                    elapsed_ms = open_started.elapsed().as_millis(),
                    "viewer_loader: open_book_reader complete"
                );
                *cache = Some(CachedReader {
                    path: req.path.as_ref().to_owned(),
                    reader: r,
                    raw_cache: None,
                    frame_cache: LruCache::new(
                        NonZeroUsize::new(req.frame_cache_cap.max(1)).unwrap_or(NonZeroUsize::MIN),
                    ),
                    animation_streams: HashMap::new(),
                });
            }
            Err(e) => {
                tracing::error!("viewer_loader: open_book_reader: {e:#}");
                return ViewerResult {
                    request_id: req.id,
                    left: None,
                    right: None,
                    page_count: 0,
                    left_orig_w: 0,
                    left_orig_h: 0,
                    right_orig_w: 0,
                    right_orig_h: 0,
                    error: Some(format!("{e:#}")),
                    kind: ViewerResultKind::Display,
                    left_is_animation_stream: false,
                    right_is_animation_stream: false,
                    left_stream_exhausted: false,
                    right_stream_exhausted: false,
                    queue_wait_ms,
                    decode_ms: 0,
                    view_idx: req.view_idx,
                    nav_id: req.nav_id,
                    page_left: req.page_left,
                    page_right: req.page_right,
                    request_display_w: req.display_w,
                    request_display_h: req.display_h,
                    request_quality: req.quality,
                    request_max_tex_side: req.max_tex_side,
                    worker: worker_name.to_owned(),
                    interactive: req.interactive,
                };
            }
        }
    }

    let Some(cache_ref) = cache.as_ref() else {
        return ViewerResult {
            request_id: req.id,
            left: None,
            right: None,
            page_count: 0,
            left_orig_w: 0,
            left_orig_h: 0,
            right_orig_w: 0,
            right_orig_h: 0,
            error: Some("viewer cache unavailable".to_owned()),
            kind: ViewerResultKind::Display,
            left_is_animation_stream: false,
            right_is_animation_stream: false,
            left_stream_exhausted: false,
            right_stream_exhausted: false,
            queue_wait_ms,
            decode_ms: 0,
            view_idx: req.view_idx,
            nav_id: req.nav_id,
            page_left: req.page_left,
            page_right: req.page_right,
            request_display_w: req.display_w,
            request_display_h: req.display_h,
            request_quality: req.quality,
            request_max_tex_side: req.max_tex_side,
            worker: worker_name.to_owned(),
            interactive: req.interactive,
        };
    };
    let page_count = cache_ref.reader.page_count();
    if page_count == 0 {
        tracing::warn!(
            path = %req.path.display(),
            kind = ?req.kind,
            "viewer_loader: page_count is zero"
        );
    }

    if let Some(cached) = cache.as_mut() {
        let cap = NonZeroUsize::new(req.frame_cache_cap.max(1)).unwrap_or(NonZeroUsize::MIN);
        if cached.frame_cache.cap() != cap {
            cached.frame_cache.resize(cap);
        }
    }

    let mut result = ViewerResult {
        request_id: req.id,
        left: None,
        right: None,
        page_count,
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
        queue_wait_ms,
        decode_ms: 0,
        view_idx: req.view_idx,
        nav_id: req.nav_id,
        page_left: req.page_left,
        page_right: req.page_right,
        request_display_w: req.display_w,
        request_display_h: req.display_h,
        request_quality: req.quality,
        request_max_tex_side: req.max_tex_side,
        worker: worker_name.to_owned(),
        interactive: req.interactive,
    };

    if matches!(
        req.kind,
        ViewerRequestKind::AnimationStreamStart | ViewerRequestKind::AnimationStreamFill
    ) {
        match cache.as_mut() {
            Some(cached) => process_animation_stream_request(req, cached, result, req_started),
            None => {
                result.error = Some("viewer cache unavailable".to_owned());
                result
            }
        }
    } else {
        // ── 表示デコード（animated WebP は初回から stream 開始） ───────────────

        if let Some(pn) = req.page_left.filter(|pn| *pn < page_count) {
            let Some(cached) = cache.as_mut() else {
                result.error = Some("viewer cache unavailable".to_owned());
                return result;
            };
            match get_display_page_or_failed(
                cached,
                pn,
                req.display_w,
                req.display_h,
                req.quality,
                req.max_tex_side,
            ) {
                Ok(PageLoadOutcome {
                    page: DisplayPage::Static(page),
                }) => {
                    result.left_orig_w = page.orig_w;
                    result.left_orig_h = page.orig_h;
                    result.left = Some(page.frames);
                    result.decode_ms = result.decode_ms.saturating_add(page.decode_ms);
                }
                Ok(PageLoadOutcome {
                    page: DisplayPage::AnimationStream(page),
                }) => {
                    result.left_orig_w = page.orig_w;
                    result.left_orig_h = page.orig_h;
                    result.left = Some(page.frames);
                    result.left_is_animation_stream = true;
                    result.left_stream_exhausted = page.exhausted;
                }
                Err(e) => {
                    tracing::error!("viewer_loader: decode page {pn}: {e:#}");
                    result.error = Some(format!("{e:#}"));
                }
            }
        }

        if let Some(pn) = req.page_right.filter(|pn| *pn < page_count) {
            let Some(cached) = cache.as_mut() else {
                result.error = Some("viewer cache unavailable".to_owned());
                return result;
            };
            match get_display_page_or_failed(
                cached,
                pn,
                req.display_w,
                req.display_h,
                req.quality,
                req.max_tex_side,
            ) {
                Ok(PageLoadOutcome {
                    page: DisplayPage::Static(page),
                }) => {
                    result.right_orig_w = page.orig_w;
                    result.right_orig_h = page.orig_h;
                    result.right = Some(page.frames);
                    result.decode_ms = result.decode_ms.saturating_add(page.decode_ms);
                }
                Ok(PageLoadOutcome {
                    page: DisplayPage::AnimationStream(page),
                }) => {
                    result.right_orig_w = page.orig_w;
                    result.right_orig_h = page.orig_h;
                    result.right = Some(page.frames);
                    result.right_is_animation_stream = true;
                    result.right_stream_exhausted = page.exhausted;
                }
                Err(e) => {
                    tracing::error!("viewer_loader: decode page {pn}: {e:#}");
                    if result.error.is_none() {
                        result.error = Some(format!("{e:#}"));
                    }
                }
            }
        }

        tracing::trace!(
            request_id = req.id,
            spread = req.page_right.is_some(),
            elapsed_ms = req_started.elapsed().as_millis(),
            "viewer_loader: request timing"
        );
        let result_pages = u8::from(result.left.is_some()) + u8::from(result.right.is_some());
        tracing::trace!(
            "[viewer-worker-done] nav_id={} req={} view={} decode_ms={} result_pages={} kind={} worker={}",
            req.nav_id,
            req.id,
            req.view_idx,
            result.decode_ms,
            result_pages,
            if req.interactive { "interactive" } else { "background" },
            worker_name
        );
        result.kind = ViewerResultKind::Display;
        result
    }
}

fn shutdown_started_queues(queues: &[Arc<SharedQueue>]) {
    for shared in queues {
        shared.shutdown();
    }
}

fn open_book_reader_for_viewer_worker(
    path: &Path,
) -> anyhow::Result<Box<dyn crate::infra::archive::BookReader>> {
    if path.is_dir() {
        return Ok(Box::new(FolderImageReader::open_for_viewer(path)?));
    }
    open_book_reader(path)
}

const ANIMATION_STREAM_CHUNK_FRAMES: usize = 8;

fn process_animation_stream_request(
    req: &ViewerRequest,
    cached: &mut CachedReader,
    mut result: ViewerResult,
    req_started: Instant,
) -> ViewerResult {
    if let Some(pn) = req.page_left.filter(|pn| *pn < result.page_count) {
        match get_animation_stream_chunk_or_failed(
            cached,
            pn,
            req.display_w,
            req.display_h,
            req.quality,
            req.kind,
        ) {
            Ok(PageLoadOutcome {
                page: DisplayPage::AnimationStream(chunk),
            }) => {
                result.left_stream_exhausted = chunk.exhausted;
                result.left_orig_w = chunk.orig_w;
                result.left_orig_h = chunk.orig_h;
                result.left = Some(chunk.frames);
            }
            Ok(PageLoadOutcome {
                page: DisplayPage::Static(page),
            }) => {
                result.left_orig_w = page.orig_w;
                result.left_orig_h = page.orig_h;
                result.left = Some(page.frames);
            }
            Err(e) => {
                tracing::error!("viewer_loader: animation stream left page {pn}: {e:#}");
                result.error = Some(format!("{e:#}"));
            }
        }
    }

    if let Some(pn) = req.page_right.filter(|pn| *pn < result.page_count) {
        match get_animation_stream_chunk_or_failed(
            cached,
            pn,
            req.display_w,
            req.display_h,
            req.quality,
            req.kind,
        ) {
            Ok(PageLoadOutcome {
                page: DisplayPage::AnimationStream(chunk),
            }) => {
                result.right_stream_exhausted = chunk.exhausted;
                result.right_orig_w = chunk.orig_w;
                result.right_orig_h = chunk.orig_h;
                result.right = Some(chunk.frames);
            }
            Ok(PageLoadOutcome {
                page: DisplayPage::Static(page),
            }) => {
                result.right_orig_w = page.orig_w;
                result.right_orig_h = page.orig_h;
                result.right = Some(page.frames);
            }
            Err(e) => {
                tracing::error!("viewer_loader: animation stream right page {pn}: {e:#}");
                if result.error.is_none() {
                    result.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    tracing::trace!(
        request_id = req.id,
        kind = ?req.kind,
        left = result.left.is_some(),
        right = result.right.is_some(),
        elapsed_ms = req_started.elapsed().as_millis(),
        "viewer_loader: animation stream request timing"
    );
    result.kind = ViewerResultKind::AnimationFramesChunk;
    result
}

struct AnimationStreamChunkPage {
    frames: Arc<Vec<img::FrameData>>,
    exhausted: bool,
    orig_w: u32,
    orig_h: u32,
}

fn get_display_page(
    cached: &mut CachedReader,
    pn: u32,
    display_w: u32,
    display_h: u32,
    quality: ViewerQuality,
    max_tex_side: u32,
) -> anyhow::Result<DisplayPage> {
    let full_key = (pn, display_w, display_h, quality, ViewerFrameStage::Full);
    if let Some(frames) = cached.frame_cache.get(&full_key) {
        let (orig_w, orig_h) = frames
            .first()
            .map(|f| (f.image.width, f.image.height))
            .unwrap_or((0, 0));
        tracing::trace!(
            "[viewer-worker-frame-cache] page={} hit=true display_w={} display_h={} quality={:?} stage=full",
            pn,
            display_w,
            display_h,
            quality
        );
        return Ok(DisplayPage::Static(DecodedPage {
            frames: Arc::clone(frames),
            orig_w,
            orig_h,
            decode_ms: 0,
        }));
    }

    let raw_data = get_page_raw(cached, pn)?;
    if ImageFormatHint::from_magic(&raw_data.raw) == ImageFormatHint::WebP
        && img::is_animated_webp_fast(&raw_data.raw)
    {
        let chunk =
            start_animation_stream(cached, pn, raw_data.raw, display_w, display_h, quality)?;
        return Ok(DisplayPage::AnimationStream(chunk));
    }

    cached.raw_cache = Some((pn, raw_data.raw));
    let page = get_and_decode(
        cached,
        pn,
        display_w,
        display_h,
        quality,
        max_tex_side,
        ViewerRequestKind::Display,
    )?;
    Ok(DisplayPage::Static(page))
}

fn get_display_page_or_failed(
    cached: &mut CachedReader,
    pn: u32,
    display_w: u32,
    display_h: u32,
    quality: ViewerQuality,
    max_tex_side: u32,
) -> anyhow::Result<PageLoadOutcome> {
    match get_display_page(cached, pn, display_w, display_h, quality, max_tex_side) {
        Ok(page) => Ok(PageLoadOutcome { page }),
        Err(e) => {
            tracing::warn!("viewer_loader: decode page {pn} failed, using synthetic page: {e:#}");
            Ok(PageLoadOutcome {
                page: DisplayPage::Static(build_failed_decoded_page(
                    cached, pn, display_w, display_h, quality,
                )),
            })
        }
    }
}

fn start_animation_stream(
    cached: &mut CachedReader,
    pn: u32,
    raw: Bytes,
    display_w: u32,
    display_h: u32,
    quality: ViewerQuality,
) -> anyhow::Result<AnimationStreamChunkPage> {
    let key = (pn, display_w, display_h, quality);
    let source = img::WebpAnimFrameSource::new(raw.to_vec())?;
    cached
        .animation_streams
        .insert(key, CachedAnimationStream { source });
    read_animation_stream_chunk(cached, &key, pn)
}

fn read_animation_stream_chunk(
    cached: &mut CachedReader,
    key: &AnimationStreamKey,
    pn: u32,
) -> anyhow::Result<AnimationStreamChunkPage> {
    let stream = cached
        .animation_streams
        .get_mut(key)
        .context("animation stream missing")?;
    let started = Instant::now();
    let chunk = stream.source.decode_chunk(ANIMATION_STREAM_CHUNK_FRAMES)?;
    tracing::trace!(
        page_n = pn,
        frame_count = chunk.frames.len(),
        exhausted = chunk.exhausted,
        elapsed_ms = started.elapsed().as_millis(),
        total_frames = chunk.frame_count,
        "viewer_loader: animation stream chunk ready"
    );

    if chunk.exhausted {
        cached.animation_streams.remove(key);
    }

    Ok(AnimationStreamChunkPage {
        frames: Arc::new(chunk.frames),
        exhausted: chunk.exhausted,
        orig_w: chunk.width,
        orig_h: chunk.height,
    })
}

fn get_animation_stream_chunk(
    cached: &mut CachedReader,
    pn: u32,
    display_w: u32,
    display_h: u32,
    quality: ViewerQuality,
    kind: ViewerRequestKind,
) -> anyhow::Result<AnimationStreamChunkPage> {
    let key = (pn, display_w, display_h, quality);

    if kind == ViewerRequestKind::AnimationStreamStart
        || !cached.animation_streams.contains_key(&key)
    {
        let raw_data = get_page_raw(cached, pn)?;
        return start_animation_stream(cached, pn, raw_data.raw, display_w, display_h, quality);
    }
    read_animation_stream_chunk(cached, &key, pn)
}

fn get_animation_stream_chunk_or_failed(
    cached: &mut CachedReader,
    pn: u32,
    display_w: u32,
    display_h: u32,
    quality: ViewerQuality,
    kind: ViewerRequestKind,
) -> anyhow::Result<PageLoadOutcome> {
    match get_animation_stream_chunk(cached, pn, display_w, display_h, quality, kind) {
        Ok(chunk) => Ok(PageLoadOutcome {
            page: DisplayPage::AnimationStream(chunk),
        }),
        Err(e) => {
            let key = (pn, display_w, display_h, quality);
            cached.animation_streams.remove(&key);
            tracing::warn!(
                "viewer_loader: animation page {pn} failed, using synthetic page: {e:#}"
            );
            Ok(PageLoadOutcome {
                page: DisplayPage::Static(build_failed_decoded_page(
                    cached, pn, display_w, display_h, quality,
                )),
            })
        }
    }
}

struct DecodedPage {
    frames: Arc<Vec<img::FrameData>>,
    orig_w: u32,
    orig_h: u32,
    decode_ms: u128,
}

enum DisplayPage {
    Static(DecodedPage),
    AnimationStream(AnimationStreamChunkPage),
}

struct RawPageData {
    raw: Bytes,
    raw_source: &'static str,
    raw_cache_hit: bool,
    raw_read_ms: u128,
    raw_total_ms: u128,
}

fn build_failed_decoded_page(
    cached: &mut CachedReader,
    pn: u32,
    display_w: u32,
    display_h: u32,
    quality: ViewerQuality,
) -> DecodedPage {
    let spec = build_failed_page_spec(display_w, display_h);
    cached.frame_cache.put(
        (pn, display_w, display_h, quality, ViewerFrameStage::Full),
        Arc::clone(&spec.frames),
    );
    cached.raw_cache = None;
    DecodedPage {
        frames: spec.frames,
        orig_w: spec.width,
        orig_h: spec.height,
        decode_ms: 0,
    }
}

fn build_failed_page_spec(display_w: u32, display_h: u32) -> FailedPageSpec {
    let width = display_w.max(1);
    let height = display_h.max(1);
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&FAILED_PAGE_BG_RGBA);
    }
    draw_failed_page_label(&mut pixels, width, height);
    FailedPageSpec {
        frames: Arc::new(vec![img::FrameData {
            image: img::DecodedImage {
                width,
                height,
                pixels,
            },
            delay_ms: 0,
        }]),
        width,
        height,
    }
}

fn draw_failed_page_label(pixels: &mut [u8], width: u32, height: u32) {
    let glyph_count = FAILED_PAGE_TEXT.len() as u32;
    let base_text_w = glyph_count
        .saturating_mul(FAILED_PAGE_FONT_W + 1)
        .saturating_sub(1);
    let scale_from_height = (
        height / (FAILED_PAGE_FONT_H.saturating_mul(FAILED_PAGE_TEXT_HEIGHT_SCALE_DIVISOR)).max(1)
    )
        .max(FAILED_PAGE_TEXT_MIN_SCALE);
    let width_divisor = FAILED_PAGE_TEXT_WIDTH_SCALE_DIVISOR.max(1);
    let scale_from_width =
        (width / base_text_w.saturating_mul(width_divisor).max(1)).max(1);
    let scale = scale_from_height.min(scale_from_width).max(1);
    let text_w = base_text_w.saturating_mul(scale);
    let text_h = FAILED_PAGE_FONT_H.saturating_mul(scale);
    let start_x = width.saturating_sub(text_w) / 2;
    let start_y = height.saturating_sub(text_h) / 2;

    for (idx, ch) in FAILED_PAGE_TEXT.bytes().enumerate() {
        let x = start_x + (idx as u32).saturating_mul((FAILED_PAGE_FONT_W + 1).saturating_mul(scale));
        draw_failed_page_glyph(pixels, width, height, x, start_y, scale, ch);
    }
}

fn draw_failed_page_glyph(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    scale: u32,
    ch: u8,
) {
    let glyph = failed_page_glyph(ch);
    for (row, bits) in glyph.iter().enumerate() {
        for col in 0..FAILED_PAGE_FONT_W as usize {
            if (bits >> (FAILED_PAGE_FONT_W as usize - 1 - col)) & 1 == 0 {
                continue;
            }
            let px = x + col as u32 * scale;
            let py = y + row as u32 * scale;
            fill_rect_rgba(pixels, width, height, px, py, scale, scale, FAILED_PAGE_FG_RGBA);
        }
    }
}

fn fill_rect_rgba(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    rect_w: u32,
    rect_h: u32,
    rgba: [u8; 4],
) {
    let x_end = x.saturating_add(rect_w).min(width);
    let y_end = y.saturating_add(rect_h).min(height);
    for py in y..y_end {
        for px in x..x_end {
            let offset = ((py as usize * width as usize) + px as usize) * 4;
            if let Some(dst) = pixels.get_mut(offset..offset + 4) {
                dst.copy_from_slice(&rgba);
            }
        }
    }
}

fn failed_page_glyph(ch: u8) -> [u8; FAILED_PAGE_FONT_H as usize] {
    match ch {
        b'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        b'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        b'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        b'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        b'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        b'I' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1F],
        b'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        b'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        b'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        _ => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x00, 0x08],
    }
}

/// ページをデコードして `(Arc<frames>, orig_w, orig_h)` を返す。
///
/// 1. LRU フレームキャッシュをチェック → ヒットなら即返却（ZIP 解凍・デコードなし）
/// 2. キャッシュミス: プローブキャッシュ(raw_cache)を優先使用し Deflate 二重展開を防ぐ
/// 3. デコード結果を LRU に保存して次回ヒットに備える
fn get_and_decode(
    cached: &mut CachedReader,
    pn: u32,
    display_w: u32,
    display_h: u32,
    quality: ViewerQuality,
    max_tex_side: u32,
    kind: ViewerRequestKind,
) -> anyhow::Result<DecodedPage> {
    let full_key = (pn, display_w, display_h, quality, ViewerFrameStage::Full);

    // ── LRU キャッシュヒット ───────────────────────────────────────────────
    if let Some(frames) = cached.frame_cache.get(&full_key) {
        // ヒット: orig_w/h はデコード済み画像の寸法から再取得（アスペクト比は保持される）
        let (orig_w, orig_h) = frames
            .first()
            .map(|f| (f.image.width, f.image.height))
            .unwrap_or((0, 0));
        tracing::trace!(
            "[viewer-worker-frame-cache] page={} hit=true display_w={} display_h={} quality={:?} stage=full",
            pn,
            display_w,
            display_h,
            quality
        );
        return Ok(DecodedPage {
            frames: Arc::clone(frames),
            orig_w,
            orig_h,
            decode_ms: 0,
        });
    }
    tracing::trace!(
        "[viewer-worker-frame-cache] page={} hit=false display_w={} display_h={} quality={:?} stage=full",
        pn,
        display_w,
        display_h,
        quality
    );

    // ── キャッシュミス: 解凍 + デコード ────────────────────────────────────
    let started = Instant::now();
    let raw_data = get_page_raw(cached, pn)?;
    let decode_started = Instant::now();
    debug_assert_eq!(kind, ViewerRequestKind::Display);
    let vf = img::decode_for_viewer_frames(
        &raw_data.raw,
        ImageFormatHint::Unknown,
        display_w,
        display_h,
        quality,
        max_tex_side,
    )?;
    let decode_ms = decode_started.elapsed().as_millis();
    let total_ms = started.elapsed().as_millis();
    tracing::trace!(
        "[viewer-worker-page-decode] page={} display_w={} display_h={} quality={:?} max_tex_side={} raw_source={} raw_cache_hit={} raw_read_ms={} raw_total_ms={} decode_ms={} total_ms={} raw_bytes={} orig_w={} orig_h={} frame_count={}",
        pn,
        display_w,
        display_h,
        quality,
        max_tex_side,
        raw_data.raw_source,
        raw_data.raw_cache_hit,
        raw_data.raw_read_ms,
        raw_data.raw_total_ms,
        decode_ms,
        total_ms,
        raw_data.raw.len(),
        vf.orig_w,
        vf.orig_h,
        vf.frames.len()
    );

    let frames = Arc::new(vf.frames);
    // LRU に保存（Arc::clone で所有権を共有: コピーコストなし）
    cached.frame_cache.put(
        (pn, display_w, display_h, quality, ViewerFrameStage::Full),
        Arc::clone(&frames),
    );

    Ok(DecodedPage {
        frames,
        orig_w: vf.orig_w,
        orig_h: vf.orig_h,
        decode_ms,
    })
}

/// ページの生バイトを返す。
/// プローブキャッシュ(raw_cache)がある場合はそれを消費して返し、Deflate 二重展開を防ぐ。
fn get_page_raw(cached: &mut CachedReader, pn: u32) -> anyhow::Result<RawPageData> {
    let started = Instant::now();
    if cached
        .raw_cache
        .as_ref()
        .map(|(n, _)| *n == pn)
        .unwrap_or(false)
    {
        // キャッシュヒット: 消費して返す（以後 raw_cache はなし）
        let Some((_, raw)) = cached.raw_cache.take() else {
            anyhow::bail!("raw_cache missing for cached page");
        };
        let raw_total_ms = started.elapsed().as_millis();
        Ok(RawPageData {
            raw,
            raw_source: "raw_cache",
            raw_cache_hit: true,
            raw_read_ms: 0,
            raw_total_ms,
        })
    } else {
        // キャッシュミス: 新規解凍
        let read_started = Instant::now();
        let raw = cached.reader.read_page_n(pn)?;
        let raw_read_ms = read_started.elapsed().as_millis();
        let raw_total_ms = started.elapsed().as_millis();
        Ok(RawPageData {
            raw,
            raw_source: "read_page",
            raw_cache_hit: false,
            raw_read_ms,
            raw_total_ms,
        })
    }
}
