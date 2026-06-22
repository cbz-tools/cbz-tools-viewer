/// Viewer UI の入口。
/// 非同期ロード、表示サイズ連動 request、見開き/アニメーション描画の接続点だけを持つ。
/// 責務の本体は `state` / `worker_manager` / `draw` に分ける。
use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, pos2, vec2, Color32, Key, Rect};

use crate::domain::app_settings::{ReadingDirection, UiLanguage, ViewerQuality};
use crate::domain::archive_settings::SpreadMode;
use crate::infra::image::decode as img;
use crate::infra::ipc::ViewerFavoriteState;
use crate::ui::i18n::{tr, TextKey};

use self::draw::{
    draw_boundary_preview_card, draw_follow_placeholder_panel, draw_fullscreen_overlay,
    draw_key_feedback, draw_pages, draw_status_message, draw_viewer_overlays,
    fullscreen_overlay_near, BoundaryPreviewCardAction, FullscreenOverlayContext,
    ViewerOverlayContext,
};
use self::progress::render_page_progress_bar;
use self::state::{now_ms, OverlayRenderResult};
use self::toolbar::{is_reserved_viewer_key, render_viewer_toolbar, ViewerToolbarContext};
use super::{icons, theme};

mod auto_spread_plan;
mod draw;
mod gpu_texture_history;
mod gpu_warmup_cache;
mod gpu_warmup_planner;
mod progress;
mod state;
mod streaming_cache;
mod toolbar;
mod worker_manager;
mod working_set;
pub use self::state::{
    BoundaryPreviewDirection, FullEquivalentSizeHint, FullEquivalentSizeHintSource, ViewerState,
    ViewerStateInit,
};

pub enum ViewerAction {
    None,
    ToggleFullscreen,
    RequestDelete,
    ToggleFavorite,
    PreviousBook,
    NextBook,
    RunExternalTool {
        tool_index: usize,
        target_path: std::path::PathBuf,
        trigger: ExternalToolTrigger,
    },
}

#[derive(Clone, Copy)]
pub enum ExternalToolTrigger {
    Toolbar,
    Shortcut { key: char },
}

#[derive(Clone)]
pub struct ExternalToolButtonModel {
    pub tool_index: usize,
    pub name: String,
    pub shortcut: char,
    pub key: Key,
}

#[derive(Clone)]
pub enum ExternalToolToolbarState {
    Idle,
    Running {
        tool_index: usize,
        path: std::path::PathBuf,
    },
    Success {
        tool_index: usize,
        path: std::path::PathBuf,
    },
    Failed {
        tool_index: usize,
        path: std::path::PathBuf,
    },
}

// ── 見開き計算ヘルパー ────────────────────────────────────────────────────────

#[derive(Default)]
struct ToolbarEvents {
    delete: bool,
    toggle_favorite: bool,
    toggle_fullscreen: bool,
    toggle_spread: bool,
    toggle_cover_blank: bool,
    toggle_slideshow: bool,
    interval_change: Option<f32>,
    reading_direction_override_change: Option<Option<ReadingDirection>>,
    quality_override_change: Option<Option<ViewerQuality>>,
    external_tool_click: Option<usize>,
    external_tool_shortcut: Option<(usize, char, std::path::PathBuf)>,
}

fn apply_toolbar_state_actions(
    state: &mut ViewerState,
    toolbar_events: &ToolbarEvents,
    runtime: ToolbarApplyContext<'_>,
    settings_sink: &mut ViewerSettingsChangeSink<'_>,
) {
    if !runtime.in_viewport_transition && toolbar_events.toggle_spread {
        state.toggle_spread(
            runtime.display_w,
            runtime.display_h,
            runtime.max_tex_side,
            runtime.ctx,
        );
        *settings_sink.spread = Some(state.persistent.spread_setting.clone());
    }
    if !runtime.in_viewport_transition && toolbar_events.toggle_cover_blank {
        state.toggle_cover_blank(
            runtime.display_w,
            runtime.display_h,
            runtime.max_tex_side,
            runtime.ctx,
        );
        *settings_sink.cover_blank = Some(state.persistent.cover_blank);
    }
    if let Some(interval_secs) = toolbar_events.interval_change {
        state.set_slideshow_interval_secs(interval_secs, runtime.now);
        *settings_sink.slideshow_interval = Some(state.slideshow_interval_secs());
    }
    if let Some(reading_direction_override) = toolbar_events.reading_direction_override_change {
        state.persistent.reading_direction_override = reading_direction_override;
        *settings_sink.reading_direction_override = Some(reading_direction_override);
    }
    if let Some(quality_override) = toolbar_events.quality_override_change {
        state.persistent.quality_override = quality_override;
        *settings_sink.quality_override = Some(quality_override);
        let effective = quality_override.unwrap_or(runtime.global_quality);
        if state.current_quality() != effective {
            state.set_quality(effective);
            state.reload_current_view(runtime.ctx);
        }
    }
    if toolbar_events.toggle_slideshow {
        state.toggle_slideshow(runtime.now);
    }
    if toolbar_events.toggle_favorite {
        *runtime.action = ViewerAction::ToggleFavorite;
    }
    if runtime.capabilities.allow_delete && toolbar_events.delete {
        state.stop_slideshow();
        *runtime.action = ViewerAction::RequestDelete;
    }
}

fn apply_external_tool_toolbar_actions(
    state: &ViewerState,
    toolbar_events: &mut ToolbarEvents,
    external_tool_state: &ExternalToolToolbarState,
    action: &mut ViewerAction,
) {
    if let Some(tool_index) = toolbar_events.external_tool_click {
        log::info!(
            "[external-tool] toolbar clicked tool_index={} path={}",
            tool_index,
            state.persistent.entry.path.display()
        );
        if matches!(
            external_tool_state,
            ExternalToolToolbarState::Running { .. }
        ) {
            log::warn!(
                "[external-tool] toolbar ignored busy before enqueue tool_index={}",
                tool_index
            );
        } else {
            *action = ViewerAction::RunExternalTool {
                tool_index,
                target_path: state.persistent.entry.path.as_ref().to_path_buf(),
                trigger: ExternalToolTrigger::Toolbar,
            };
        }
    }
    if let Some((tool_index, key, target_path)) = toolbar_events.external_tool_shortcut.take() {
        if matches!(
            external_tool_state,
            ExternalToolToolbarState::Running { .. }
        ) {
        } else {
            *action = ViewerAction::RunExternalTool {
                tool_index,
                target_path,
                trigger: ExternalToolTrigger::Shortcut { key },
            };
        }
    }
}

fn pending_reason_label(state: &ViewerState) -> &'static str {
    if state.suppress_pending_for_animation() {
        "animated"
    } else {
        "frame_draw"
    }
}

fn compute_first_paint_cache_hit(
    state: &mut ViewerState,
    page_left: Option<u32>,
    page_right: Option<u32>,
    page_decode_w: u32,
    page_decode_h: u32,
    max_tex_side: u32,
) -> (bool, bool) {
    let bg_rgba_cache = state.request.worker_manager.bg_rgba_cache();
    let left_cached = page_left.is_some_and(|page| {
        let key = state.rgba_cache_key(page, page_decode_w, page_decode_h, max_tex_side);
        state
            .display_assets
            .interactive_rgba_cache
            .get(&key)
            .is_some()
            || bg_rgba_cache
                .read()
                .ok()
                .is_some_and(|cache| cache.contains(&key))
    });
    let right_cached = page_right.is_some_and(|page| {
        let key = state.rgba_cache_key(page, page_decode_w, page_decode_h, max_tex_side);
        state
            .display_assets
            .interactive_rgba_cache
            .get(&key)
            .is_some()
            || bg_rgba_cache
                .read()
                .ok()
                .is_some_and(|cache| cache.contains(&key))
    });
    (left_cached, right_cached)
}

fn boundary_preview_input_enabled(
    state: &ViewerState,
    capabilities: ViewerUiCapabilities,
    is_fullscreen: bool,
) -> bool {
    state.boundary_preview_enabled(capabilities.allow_book_navigation, is_fullscreen)
}

enum BoundaryPreviewPageMoveEffect {
    Close,
    Replace(BoundaryPreviewDirection),
    Keep,
}

fn boundary_preview_page_move_effect(
    state: &ViewerState,
    boundary_preview_enabled: bool,
    moved: bool,
    direction: BoundaryPreviewDirection,
) -> BoundaryPreviewPageMoveEffect {
    if moved {
        return BoundaryPreviewPageMoveEffect::Close;
    }
    if boundary_preview_enabled && state.boundary_preview_can_trigger(direction) {
        return BoundaryPreviewPageMoveEffect::Replace(direction);
    }
    BoundaryPreviewPageMoveEffect::Keep
}

fn apply_boundary_preview_after_page_move(
    state: &mut ViewerState,
    boundary_preview_enabled: bool,
    moved: bool,
    direction: BoundaryPreviewDirection,
) {
    match boundary_preview_page_move_effect(state, boundary_preview_enabled, moved, direction) {
        BoundaryPreviewPageMoveEffect::Close => {
            state.close_boundary_preview_on_successful_page_move(true);
        }
        BoundaryPreviewPageMoveEffect::Replace(direction) => {
            state.begin_boundary_preview(direction);
        }
        BoundaryPreviewPageMoveEffect::Keep => {}
    }
}

// ── アニメーションコンテンツ ──────────────────────────────────────────────────

/// 1 ページ分の表示コンテンツ。
pub enum PageContent {
    Static(egui::TextureHandle),
    AnimatedReady {
        /// LRU とピクセル列を共有し、コピーを避ける。
        frames: Arc<Vec<img::FrameData>>,
        current: usize,
        next_frame_at: Instant,
        texture: egui::TextureHandle,
    },
    AnimatedStream {
        queue: VecDeque<img::FrameData>,
        next_frame_at: Instant,
        texture: egui::TextureHandle,
        exhausted: bool,
        fill_in_flight: bool,
    },
}

#[derive(Clone, Copy)]
pub struct ViewerUiCapabilities {
    pub allow_delete: bool,
    pub allow_book_navigation: bool,
    pub allow_favorite_toggle: bool,
}

impl ViewerUiCapabilities {}

pub struct ViewerShowContext<'a> {
    pub state: &'a mut ViewerState,
    pub language: UiLanguage,
    pub favorite_state: ViewerFavoriteState,
    pub favorite_toggle_pending: bool,
    pub interaction_blocked: bool,
    pub is_fullscreen: bool,
    pub external_tools: &'a [ExternalToolButtonModel],
    pub external_tool_state: &'a ExternalToolToolbarState,
    pub global_quality: ViewerQuality,
    pub capabilities: ViewerUiCapabilities,
    pub boundary_preview_thumb_size: egui::Vec2,
    pub boundary_preview_hud_font_size: f32,
}

pub struct ViewerSettingsChangeSink<'a> {
    pub cover_blank: &'a mut Option<bool>,
    pub spread: &'a mut Option<SpreadMode>,
    pub slideshow_interval: &'a mut Option<f32>,
    pub reading_direction_override: &'a mut Option<Option<ReadingDirection>>,
    pub quality_override: &'a mut Option<Option<ViewerQuality>>,
}

struct ToolbarApplyContext<'a> {
    global_quality: ViewerQuality,
    now: Instant,
    in_viewport_transition: bool,
    display_w: u32,
    display_h: u32,
    max_tex_side: u32,
    ctx: &'a egui::Context,
    capabilities: ViewerUiCapabilities,
    action: &'a mut ViewerAction,
}

const ANIMATED_STREAM_FILL_LOW_WATERMARK: usize = 4;
const ANIMATED_STREAM_FILL_HIGH_WATERMARK: usize = 16;

impl PageContent {
    pub fn texture(&self) -> &egui::TextureHandle {
        match self {
            Self::Static(t) => t,
            Self::AnimatedReady { texture, .. } => texture,
            Self::AnimatedStream { texture, .. } => texture,
        }
    }

    /// 次回再描画時刻を管理する。静止画は長周期で固定する。
    pub fn tick(&mut self, label: &str, ctx: &egui::Context) -> Duration {
        match self {
            Self::Static(_) => Duration::from_secs(3600),
            Self::AnimatedReady {
                frames,
                current,
                next_frame_at,
                texture,
            } => {
                if frames.len() <= 1 {
                    return Duration::from_secs(3600);
                }
                let now = Instant::now();
                if now >= *next_frame_at {
                    *current = (*current + 1) % frames.len();
                    let img = &frames[*current].image;
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [img.width as usize, img.height as usize],
                        &img.pixels,
                    );
                    *texture = ctx.load_texture(label, color, egui::TextureOptions::LINEAR);
                    let delay = frames[*current].delay_ms.max(img::MIN_FRAME_DELAY_MS) as u64;
                    *next_frame_at = now + Duration::from_millis(delay);
                }
                next_frame_at.saturating_duration_since(Instant::now())
            }
            Self::AnimatedStream {
                queue,
                next_frame_at,
                texture,
                exhausted,
                ..
            } => {
                if queue.is_empty() {
                    return Duration::from_millis(if *exhausted { 3_600_000 } else { 16 });
                }
                let now = Instant::now();
                if now >= *next_frame_at {
                    if queue.len() > 1 {
                        queue.pop_front();
                        if let Some(next) = queue.front() {
                            let img = &next.image;
                            let color = egui::ColorImage::from_rgba_unmultiplied(
                                [img.width as usize, img.height as usize],
                                &img.pixels,
                            );
                            *texture = ctx.load_texture(label, color, egui::TextureOptions::LINEAR);
                            let delay = next.delay_ms.max(img::MIN_FRAME_DELAY_MS) as u64;
                            *next_frame_at = now + Duration::from_millis(delay);
                        }
                    } else {
                        // 次チャンク待ち/ループ再始動待ちの間に 0ms 再描画ループへ落ちないよう抑える。
                        return Duration::from_millis(16);
                    }
                }
                next_frame_at.saturating_duration_since(Instant::now())
            }
        }
    }

    /// フレーム列を表示用コンテンツへ変換する。Arc 共有でピクセルコピーを避ける。
    pub fn from_frames(frames: Arc<Vec<img::FrameData>>, label: &str, ctx: &egui::Context) -> Self {
        let started = Instant::now();
        // 空ベクタ安全ガード（decode_for_viewer_frames は常に ≥1 を返すはずだが念のため）
        if frames.is_empty() {
            let color = egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]);
            let texture = ctx.load_texture(label, color, egui::TextureOptions::LINEAR);
            tracing::debug!(
                label,
                frame_count = 0,
                elapsed_ms = started.elapsed().as_millis(),
                "viewer_ui: texture upload complete"
            );
            return Self::Static(texture);
        }
        Self::build_texture_content(frames, label, ctx, started)
    }

    fn build_texture_content(
        frames: Arc<Vec<img::FrameData>>,
        label: &str,
        ctx: &egui::Context,
        started: Instant,
    ) -> Self {
        let first = &frames[0].image;
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [first.width as usize, first.height as usize],
            &first.pixels,
        );
        let texture = ctx.load_texture(label, color, egui::TextureOptions::LINEAR);
        tracing::trace!(
            label,
            frame_count = frames.len(),
            width = first.width,
            height = first.height,
            elapsed_ms = started.elapsed().as_millis(),
            "viewer_ui: texture upload complete"
        );

        if frames.len() > 1 {
            let delay = frames[0].delay_ms.max(img::MIN_FRAME_DELAY_MS) as u64;
            Self::AnimatedReady {
                next_frame_at: Instant::now() + Duration::from_millis(delay),
                frames,
                current: 0,
                texture,
            }
        } else {
            Self::Static(texture)
        }
    }

    pub fn from_stream_chunk(
        frames: Arc<Vec<img::FrameData>>,
        exhausted: bool,
        label: &str,
        ctx: &egui::Context,
    ) -> Self {
        let started = Instant::now();
        let queue: VecDeque<img::FrameData> = frames
            .iter()
            .map(|f| img::FrameData {
                image: img::DecodedImage {
                    width: f.image.width,
                    height: f.image.height,
                    pixels: f.image.pixels.clone(),
                },
                delay_ms: f.delay_ms,
            })
            .collect();
        if queue.is_empty() {
            let color = egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]);
            let texture = ctx.load_texture(label, color, egui::TextureOptions::LINEAR);
            return Self::AnimatedStream {
                queue,
                next_frame_at: Instant::now() + Duration::from_secs(3600),
                texture,
                exhausted,
                fill_in_flight: false,
            };
        }
        let Some(first_frame) = queue.front() else {
            let color = egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]);
            let texture = ctx.load_texture(label, color, egui::TextureOptions::LINEAR);
            return Self::AnimatedStream {
                queue,
                next_frame_at: Instant::now() + Duration::from_secs(3600),
                texture,
                exhausted,
                fill_in_flight: false,
            };
        };
        let first = &first_frame.image;
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [first.width as usize, first.height as usize],
            &first.pixels,
        );
        let texture = ctx.load_texture(label, color, egui::TextureOptions::LINEAR);
        tracing::debug!(
            label,
            frame_count = queue.len(),
            exhausted,
            elapsed_ms = started.elapsed().as_millis(),
            "viewer_ui: animation stream texture upload complete"
        );
        let delay = queue
            .front()
            .map(|f| f.delay_ms.max(img::MIN_FRAME_DELAY_MS) as u64)
            .unwrap_or(3_600_000);
        Self::AnimatedStream {
            queue,
            next_frame_at: Instant::now() + Duration::from_millis(delay),
            texture,
            exhausted,
            fill_in_flight: false,
        }
    }

    pub fn append_stream_chunk(&mut self, frames: Arc<Vec<img::FrameData>>, exhausted: bool) {
        if let Self::AnimatedStream {
            queue,
            exhausted: stream_exhausted,
            fill_in_flight,
            ..
        } = self
        {
            let available = ANIMATED_STREAM_FILL_HIGH_WATERMARK.saturating_sub(queue.len());
            if available > 0 {
                queue.extend(frames.iter().take(available).map(|f| img::FrameData {
                    image: img::DecodedImage {
                        width: f.image.width,
                        height: f.image.height,
                        pixels: f.image.pixels.clone(),
                    },
                    delay_ms: f.delay_ms,
                }));
            } else {
                tracing::trace!(
                    "viewer_ui: animation stream chunk dropped (queue at high watermark)"
                );
            }
            *stream_exhausted = exhausted;
            *fill_in_flight = false;
        }
    }

    pub fn stream_should_fill(&self) -> bool {
        match self {
            Self::AnimatedStream {
                queue,
                exhausted,
                fill_in_flight,
                ..
            } => {
                !*exhausted && !*fill_in_flight && queue.len() <= ANIMATED_STREAM_FILL_LOW_WATERMARK
            }
            _ => false,
        }
    }

    pub fn stream_should_restart(&self) -> bool {
        match self {
            Self::AnimatedStream {
                queue,
                exhausted,
                fill_in_flight,
                ..
            } => {
                *exhausted && !*fill_in_flight && queue.len() <= ANIMATED_STREAM_FILL_LOW_WATERMARK
            }
            _ => false,
        }
    }

    pub fn mark_stream_fill_in_flight(&mut self) {
        if let Self::AnimatedStream { fill_in_flight, .. } = self {
            *fill_in_flight = true;
        }
    }
}

// ── 描画 ─────────────────────────────────────────────────────────────────────

pub fn show(
    ui: &mut egui::Ui,
    viewer: ViewerShowContext<'_>,
    settings_sink: &mut ViewerSettingsChangeSink<'_>,
) -> ViewerAction {
    let ViewerShowContext {
        state,
        language,
        favorite_state,
        favorite_toggle_pending,
        interaction_blocked,
        is_fullscreen,
        external_tools,
        external_tool_state,
        global_quality,
        capabilities,
        boundary_preview_thumb_size,
        boundary_preview_hud_font_size,
    } = viewer;
    const SLIDER_H: f32 = 36.0;
    const LOADING_REPAINT_INTERVAL: Duration = Duration::from_millis(16);
    const FULLSCREEN_OVERLAY_AUTO_HIDE_DELAY: Duration = Duration::from_millis(1000);
    const WHEEL_STEP: f32 = 12.0;
    const WHEEL_COOLDOWN: Duration = Duration::from_millis(120);
    let mut action = ViewerAction::None;
    let mut toolbar_events = ToolbarEvents::default();
    let ctx = ui.ctx().clone();
    state.update_full_equivalent_size_hint_from_viewer(ctx.input(|i| i.viewport().monitor_size));
    let now = Instant::now();
    let since_last_show_ms = state
        .ui_runtime
        .last_show_at
        .map(|last| now.saturating_duration_since(last).as_millis())
        .unwrap_or(0);
    state.ui_runtime.last_show_at = Some(now);
    state.ui_runtime.show_seq = state.ui_runtime.show_seq.saturating_add(1);
    tracing::trace!(
        show_seq = state.ui_runtime.show_seq,
        since_last_show_ms,
        loading = state.ui_runtime.loading,
        displayed_page = state.persistent.displayed_page,
        requested_page = state.persistent.requested_page,
        spread = state.persistent.spread_mode,
        "viewer_ui: show start"
    );

    // ── display_w / max_tex_side を確定 ─────────────────────────────────────
    let measured_display_w = ui.available_width() as u32;
    let max_tex_side = ctx
        .input(|i| i.raw.max_texture_side)
        .unwrap_or(img::DEFAULT_MAX_TEXTURE_SIDE as usize) as u32;

    // ── ツールバー（Windowed） ──────────────────────────────────────────────
    let pre_in_fullscreen_transition = state.ui_runtime.fullscreen_transition_frames > 0;
    if !is_fullscreen && !pre_in_fullscreen_transition {
        render_viewer_toolbar(
            ui,
            ViewerToolbarContext {
                state,
                language,
                favorite_state,
                favorite_toggle_pending,
                interaction_blocked,
                external_tools,
                external_tool_state,
                global_quality,
                capabilities,
            },
            &mut toolbar_events,
        );
        ui.separator();
        if toolbar_events.toggle_fullscreen {
            state.stop_slideshow();
            return ViewerAction::ToggleFullscreen;
        }
    }

    // ── 画像エリア ＋ スライダーに分割 ───────────────────────────────────────
    let content = ui.available_rect_before_wrap();
    let slider_h = if is_fullscreen { 0.0 } else { SLIDER_H };
    let img_h = (content.height() - slider_h).max(0.0);
    let measured_display_h = img_h as u32;
    let in_fullscreen_transition = state.ui_runtime.fullscreen_transition_frames > 0;
    if state.ui_runtime.fullscreen_transition_frames > 0 {
        state.ui_runtime.fullscreen_transition_frames = state
            .ui_runtime
            .fullscreen_transition_frames
            .saturating_sub(1);
    }
    let in_viewport_transition =
        in_fullscreen_transition || state.ui_runtime.viewport_transition_active;
    let display_w = if in_viewport_transition && state.ui_runtime.last_stable_display_w > 0 {
        state.ui_runtime.last_stable_display_w
    } else {
        measured_display_w
    };
    let display_h = if in_viewport_transition && state.ui_runtime.last_stable_display_h > 0 {
        state.ui_runtime.last_stable_display_h
    } else {
        measured_display_h
    };
    if !in_viewport_transition && display_w > 0 && display_h > 0 {
        state.ui_runtime.last_stable_display_w = display_w;
        state.ui_runtime.last_stable_display_h = display_h;
    }
    if state.transition_logs_active() {
        tracing::trace!(
            frame = state.ui_runtime.show_seq,
            is_fullscreen,
            in_viewport_transition,
            fullscreen_transition_frames = state.ui_runtime.fullscreen_transition_frames,
            display_w,
            display_h,
            measured_display_w,
            measured_display_h,
            available_width = ui.available_width(),
            available_height = ui.available_height(),
            content_rect = ?content,
            slider_h,
            img_h,
            current_page = state.persistent.requested_page,
            spread_mode = state.persistent.spread_mode,
            "viewer_ui: show frame trace"
        );
    }
    let display_layout = state.view_layout_for_with_caller(
        state.persistent.requested_page,
        display_w,
        display_h,
        true,
    );
    let display_target_w = display_layout.page_decode_w;
    let display_target_h = display_layout.page_decode_h;
    state.update_display_target_stability(display_target_w, display_target_h);
    state.publish_worker_manager_state(display_w, display_h, max_tex_side);
    #[cfg(debug_assertions)]
    {
        let size_tuple = (display_w, display_h, display_target_w, max_tex_side);
        let changed = state
            .ui_runtime
            .debug_last_display_size_log
            .is_none_or(|prev| prev != size_tuple);
        if changed || state.ui_runtime.show_seq <= 8 {
            tracing::trace!(
                "[viewer-display-size] reason=update frame={} is_fullscreen={} in_fullscreen_transition={} available_w={} available_h={} image_area_w={} image_area_h={} display_w={} display_h={} last_stable_display_w={} last_stable_display_h={} target_w={} target_h={} spread_mode={} page_left={:?} page_right={:?}",
                state.ui_runtime.show_seq,
                is_fullscreen,
                in_fullscreen_transition,
                ui.available_width() as u32,
                ui.available_height() as u32,
                content.width() as u32,
                img_h as u32,
                display_w,
                display_h,
                state.ui_runtime.last_stable_display_w,
                state.ui_runtime.last_stable_display_h,
                display_target_w,
                display_target_h,
                state.persistent.spread_mode,
                display_layout.page_left,
                display_layout.page_right
            );
            state.ui_runtime.debug_last_display_size_log = Some(size_tuple);
        }
    }

    // 初回リクエストは表示領域が確定してから送信
    if !in_viewport_transition && state.request.initial_load_pending {
        state.start_initial_load(display_w, display_h, max_tex_side, &ctx);
    }

    // ローダー polling
    if !in_viewport_transition {
        state.poll_loader(&ctx, display_w, display_h, max_tex_side);
    }
    if !in_viewport_transition {
        state.poll_worker_manager_notifications(display_w, display_h, max_tex_side);
    }
    // pending 可視状態は poll_loader 後の最新状態で判定する。
    state.update_pending_progress_state(now);

    // viewport/fullscreen 遷移中は古い表示サイズのコンテンツを新しい領域に描かない。
    // decode/request 抑止と合わせて、背景のみ表示して安定フレームを待つ。
    if in_viewport_transition {
        let rect = ui.available_rect_before_wrap();
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::ZERO, Color32::WHITE);
        if state.transition_logs_active() {
            tracing::trace!(
                frame = state.ui_runtime.show_seq,
                rect = ?rect,
                measured_display_w,
                measured_display_h,
                display_w,
                display_h,
                "viewer_ui: transition paint suppressed"
            );
            state.ui_runtime.transition_log_frames_left = state
                .ui_runtime
                .transition_log_frames_left
                .saturating_sub(1);
        }
        ctx.request_repaint();
        return ViewerAction::None;
    }

    // 「表示後開始」: 表示可能状態になったら次回送り時刻をアーム
    if state.playback.slideshow_active
        && state.playback.slideshow_arm_on_display
        && !state.ui_runtime.loading
        && (state.display_assets.content_left.is_some()
            || state.display_assets.content_right.is_some())
    {
        state.arm_slideshow_from_now(now);
    }

    // slideshow tick: interval 到達で次へ。進めない場合は末尾到達として停止。
    if !in_viewport_transition
        && !interaction_blocked
        && state.playback.slideshow_active
        && state
            .playback
            .slideshow_next_slide_at
            .is_some_and(|deadline| now >= deadline)
    {
        let before = state.nav_target();
        state.register_nav_input(now);
        state.go_next(display_w, display_h, max_tex_side, &ctx, "Slideshow");
        if state.nav_target() == before {
            state.stop_slideshow();
        } else {
            state.close_boundary_preview_on_successful_page_move(true);
            state.mark_slideshow_wait_display();
        }
    }

    // ── キーボード / ホイール ─────────────────────────────────────────────────
    // スクロール入力は smooth_scroll_delta を正規ルートにする。
    if !in_viewport_transition && !interaction_blocked {
        let shortcut_input_blocked = ctx.egui_wants_keyboard_input() || ctx.any_popup_open();
        let boundary_preview_enabled =
            boundary_preview_input_enabled(state, capabilities, is_fullscreen);
        if shortcut_input_blocked {
        } else {
            let mut seen = HashSet::with_capacity(external_tools.len());
            for tool in external_tools {
                if !seen.insert(tool.key) {
                    log::warn!(
                        "[external-tool] duplicate shortcut ignored key={} tool={} tool_index={}",
                        tool.shortcut,
                        tool.name,
                        tool.tool_index
                    );
                    continue;
                }
                if is_reserved_viewer_key(tool.key) {
                    log::warn!(
                        "[external-tool] shortcut ignored reserved key={} tool={}",
                        tool.shortcut,
                        tool.name
                    );
                    continue;
                }
                let pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, tool.key));
                if pressed {
                    log::info!(
                        "[external-tool] shortcut consumed key={} tool={} path={}",
                        tool.shortcut,
                        tool.name,
                        state.persistent.entry.path.display()
                    );
                    if matches!(
                        external_tool_state,
                        ExternalToolToolbarState::Running { .. }
                    ) {
                        log::warn!(
                            "[external-tool] shortcut ignored busy before enqueue key={} tool={}",
                            tool.shortcut,
                            tool.name
                        );
                        break;
                    }
                    toolbar_events.external_tool_shortcut = Some((
                        tool.tool_index,
                        tool.shortcut,
                        state.persistent.entry.path.as_ref().to_path_buf(),
                    ));
                    break;
                }
            }
        }

        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if let Some(until) = state.ui_runtime.wheel_cooldown_until {
            if now < until {
                state.ui_runtime.scroll_accum = 0.0;
                if wheel.abs() > f32::EPSILON {
                    tracing::trace!(
                        wheel,
                        cooldown_remaining_ms = until.saturating_duration_since(now).as_millis(),
                        "viewer_ui: wheel input suppressed"
                    );
                }
            } else {
                state.ui_runtime.wheel_cooldown_until = None;
            }
        }
        if state.ui_runtime.wheel_cooldown_until.is_none() {
            if wheel.abs() > f32::EPSILON {
                tracing::trace!(
                    wheel,
                    accum_before = state.ui_runtime.scroll_accum,
                    "viewer_ui: wheel input"
                );
            }
            state.ui_runtime.scroll_accum += wheel;
            if state.ui_runtime.scroll_accum <= -WHEEL_STEP {
                state.stop_slideshow();
                let prev_nav_mode = state.ui_runtime.nav_mode;
                let prev_last_nav_input_at = state.ui_runtime.last_nav_input_at;
                let prev_nav_consecutive_count = state.ui_runtime.nav_consecutive_count;
                state.register_nav_input(now);
                let before = state.nav_target();
                state.go_next(display_w, display_h, max_tex_side, &ctx, "WheelNext");
                state.ui_runtime.scroll_accum = 0.0;
                tracing::trace!(
                    direction = "next",
                    accum_after = state.ui_runtime.scroll_accum,
                    target_before = before,
                    target_after = state.nav_target(),
                    "viewer_ui: wheel step applied"
                );
                let moved = state.nav_target() != before;
                if !moved {
                    state.ui_runtime.scroll_accum = 0.0;
                    state.ui_runtime.nav_mode = prev_nav_mode;
                    state.ui_runtime.last_nav_input_at = prev_last_nav_input_at;
                    state.ui_runtime.nav_consecutive_count = prev_nav_consecutive_count;
                } else {
                    state.ui_runtime.wheel_cooldown_until = Some(now + WHEEL_COOLDOWN);
                }
                apply_boundary_preview_after_page_move(
                    state,
                    boundary_preview_enabled,
                    moved,
                    BoundaryPreviewDirection::Next,
                );
            } else if state.ui_runtime.scroll_accum >= WHEEL_STEP {
                state.stop_slideshow();
                let prev_nav_mode = state.ui_runtime.nav_mode;
                let prev_last_nav_input_at = state.ui_runtime.last_nav_input_at;
                let prev_nav_consecutive_count = state.ui_runtime.nav_consecutive_count;
                state.register_nav_input(now);
                let before = state.nav_target();
                state.go_prev(display_w, display_h, max_tex_side, &ctx, "WheelPrev");
                state.ui_runtime.scroll_accum = 0.0;
                tracing::trace!(
                    direction = "prev",
                    accum_after = state.ui_runtime.scroll_accum,
                    target_before = before,
                    target_after = state.nav_target(),
                    "viewer_ui: wheel step applied"
                );
                let moved = state.nav_target() != before;
                if !moved {
                    state.ui_runtime.scroll_accum = 0.0;
                    state.ui_runtime.nav_mode = prev_nav_mode;
                    state.ui_runtime.last_nav_input_at = prev_last_nav_input_at;
                    state.ui_runtime.nav_consecutive_count = prev_nav_consecutive_count;
                } else {
                    state.ui_runtime.wheel_cooldown_until = Some(now + WHEEL_COOLDOWN);
                }
                apply_boundary_preview_after_page_move(
                    state,
                    boundary_preview_enabled,
                    moved,
                    BoundaryPreviewDirection::Previous,
                );
            }
        }

        let reading_direction = state.effective_reading_direction();

        let (
            delete,
            next,
            prev,
            first,
            last,
            previous_book,
            next_book,
            toggle_slideshow,
            toggle_fullscreen_key,
        ) = ctx.input_mut(|i| {
            let left_side_input = i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft)
                || i.consume_key(egui::Modifiers::NONE, Key::A);
            let right_side_input = i.consume_key(egui::Modifiers::NONE, Key::ArrowRight)
                || i.consume_key(egui::Modifiers::NONE, Key::D);
            let next_input = match reading_direction {
                ReadingDirection::RightToLeft => left_side_input,
                ReadingDirection::LeftToRight => right_side_input,
            };
            let prev_input = match reading_direction {
                ReadingDirection::RightToLeft => right_side_input,
                ReadingDirection::LeftToRight => left_side_input,
            };
            (
                i.consume_key(egui::Modifiers::NONE, Key::Delete),
                next_input || i.consume_key(egui::Modifiers::NONE, Key::PageDown),
                prev_input || i.consume_key(egui::Modifiers::NONE, Key::PageUp),
                i.consume_key(egui::Modifiers::NONE, Key::Home),
                i.consume_key(egui::Modifiers::NONE, Key::End),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp)
                    || i.consume_key(egui::Modifiers::NONE, Key::W),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown)
                    || i.consume_key(egui::Modifiers::NONE, Key::S),
                i.consume_key(egui::Modifiers::NONE, Key::Space),
                i.consume_key(egui::Modifiers::NONE, Key::F11),
            )
        });
        if toggle_slideshow {
            state.toggle_slideshow(now);
        }
        if toggle_fullscreen_key {
            state.stop_slideshow();
            action = ViewerAction::ToggleFullscreen;
        } else if capabilities.allow_delete && delete {
            state.stop_slideshow();
            action = ViewerAction::RequestDelete;
        } else {
            if next {
                state.stop_slideshow();
                let prev_nav_mode = state.ui_runtime.nav_mode;
                let prev_last_nav_input_at = state.ui_runtime.last_nav_input_at;
                let prev_nav_consecutive_count = state.ui_runtime.nav_consecutive_count;
                let before = state.nav_target();
                state.register_nav_input(now);
                state.go_next(display_w, display_h, max_tex_side, &ctx, "KeyLeft");
                let moved = state.nav_target() != before;
                if !moved {
                    state.ui_runtime.nav_mode = prev_nav_mode;
                    state.ui_runtime.last_nav_input_at = prev_last_nav_input_at;
                    state.ui_runtime.nav_consecutive_count = prev_nav_consecutive_count;
                }
                apply_boundary_preview_after_page_move(
                    state,
                    boundary_preview_enabled,
                    moved,
                    BoundaryPreviewDirection::Next,
                );
            }
            if prev {
                state.stop_slideshow();
                let prev_nav_mode = state.ui_runtime.nav_mode;
                let prev_last_nav_input_at = state.ui_runtime.last_nav_input_at;
                let prev_nav_consecutive_count = state.ui_runtime.nav_consecutive_count;
                let before = state.nav_target();
                state.register_nav_input(now);
                state.go_prev(display_w, display_h, max_tex_side, &ctx, "KeyRight");
                let moved = state.nav_target() != before;
                if !moved {
                    state.ui_runtime.nav_mode = prev_nav_mode;
                    state.ui_runtime.last_nav_input_at = prev_last_nav_input_at;
                    state.ui_runtime.nav_consecutive_count = prev_nav_consecutive_count;
                }
                apply_boundary_preview_after_page_move(
                    state,
                    boundary_preview_enabled,
                    moved,
                    BoundaryPreviewDirection::Previous,
                );
            }
            if first {
                state.stop_slideshow();
                let prev_nav_mode = state.ui_runtime.nav_mode;
                let prev_last_nav_input_at = state.ui_runtime.last_nav_input_at;
                let prev_nav_consecutive_count = state.ui_runtime.nav_consecutive_count;
                let before = state.nav_target();
                state.register_nav_input(now);
                state.go_first(display_w, display_h, max_tex_side, &ctx, "KeyHome");
                let moved = state.nav_target() != before;
                if !moved {
                    state.ui_runtime.nav_mode = prev_nav_mode;
                    state.ui_runtime.last_nav_input_at = prev_last_nav_input_at;
                    state.ui_runtime.nav_consecutive_count = prev_nav_consecutive_count;
                }
                state.close_boundary_preview_on_successful_page_move(moved);
            }
            if last {
                state.stop_slideshow();
                let prev_nav_mode = state.ui_runtime.nav_mode;
                let prev_last_nav_input_at = state.ui_runtime.last_nav_input_at;
                let prev_nav_consecutive_count = state.ui_runtime.nav_consecutive_count;
                let before = state.nav_target();
                state.register_nav_input(now);
                state.go_last(display_w, display_h, max_tex_side, &ctx, "KeyEnd");
                let moved = state.nav_target() != before;
                if !moved {
                    state.ui_runtime.nav_mode = prev_nav_mode;
                    state.ui_runtime.last_nav_input_at = prev_last_nav_input_at;
                    state.ui_runtime.nav_consecutive_count = prev_nav_consecutive_count;
                }
                state.close_boundary_preview_on_successful_page_move(moved);
            }
            if capabilities.allow_book_navigation && previous_book {
                state.stop_slideshow();
                action = ViewerAction::PreviousBook;
            }
            if capabilities.allow_book_navigation && next_book {
                state.stop_slideshow();
                action = ViewerAction::NextBook;
            }
        }
    } else {
        state.ui_runtime.scroll_accum = 0.0;
        state.ui_runtime.wheel_cooldown_until = None;
    }

    let (used_rect, _) = ui.allocate_exact_size(vec2(content.width(), img_h), egui::Sense::hover());

    let has_visible_content =
        state.display_assets.content_left.is_some() || state.display_assets.content_right.is_some();
    if !state.ui_runtime.first_paint_logged {
        let layout = state.view_layout_for_with_caller(
            state.persistent.requested_page,
            display_w,
            display_h,
            false,
        );
        state.log_view_layout("cache-lookup", &layout);
        let (left_cached, right_cached) = compute_first_paint_cache_hit(
            state,
            layout.page_left,
            layout.page_right,
            layout.page_decode_w,
            layout.page_decode_h,
            max_tex_side,
        );
        tracing::trace!(
            frame = state.ui_runtime.show_seq,
            page = state.persistent.requested_page,
            has_texture = has_visible_content,
            has_rgba_cache = (left_cached || right_cached),
            display_w = layout.page_display_w,
            display_h = layout.page_display_h,
            measured_display_w,
            measured_display_h,
            at_ms = now_ms(),
            "viewer-first-paint"
        );
        if has_visible_content {
            state.ui_runtime.first_paint_logged = true;
        }
    }
    ui.painter()
        .rect_filled(used_rect, egui::CornerRadius::ZERO, Color32::WHITE);

    let pending_reason = pending_reason_label(state);
    let show_pending = state.show_follow_placeholder();
    let pending_display_state = state.pending_display_state(show_pending);
    if state.update_pending_display_state(pending_display_state) {
        tracing::trace!(
            "[viewer-pending-display] target_page={} displayed_page={} requested_page={} show_pending={} immediate_commit=false cache_hit=unknown reason={}",
            pending_display_state.target_page,
            pending_display_state.displayed_page,
            pending_display_state.requested_page,
            pending_display_state.show_pending,
            pending_reason
        );
    }

    if in_fullscreen_transition {
    } else if let Some(msg) = &state.ui_runtime.error.clone() {
        draw_status_message(
            ui,
            &used_rect,
            &format!("{}: {msg}", tr(language, TextKey::ErrorTitle)),
            theme::DELETE_RED,
        );
    } else if show_pending {
        draw_follow_placeholder_panel(ui, state, &used_rect);
    } else if state.ui_runtime.loading && !has_visible_content {
        draw_status_message(
            ui,
            &used_rect,
            tr(language, TextKey::ViewerLoading),
            theme::TEXT_SUBTLE,
        );
    } else {
        let show_operation_help = !is_fullscreen && !interaction_blocked;
        if show_operation_help {
            draw_viewer_overlays(
                ui,
                ViewerOverlayContext {
                    state,
                    area: &used_rect,
                    language,
                    display_w,
                    display_h,
                    max_tex_side,
                    capabilities,
                },
            );
        }
        let draw_layout = state.view_layout_for_with_caller(
            state.persistent.displayed_page,
            display_w,
            display_h,
            false,
        );
        state.log_view_layout("current-draw", &draw_layout);
        let anim_remaining = draw_pages(ui, state, &used_rect, draw_layout.effective_spread);
        if anim_remaining < Duration::from_secs(60) {
            ctx.request_repaint_after(anim_remaining);
        }
    }
    if let Some(text) = state.key_feedback_text(now) {
        draw_key_feedback(ui, &used_rect, text);
        ctx.request_repaint_after(Duration::from_millis(60));
    }
    let mut overlay_interacting = false;
    let fullscreen_overlay_near =
        is_fullscreen && !in_fullscreen_transition && fullscreen_overlay_near(&ctx, &used_rect);
    let mut show_fullscreen_overlay = false;
    if is_fullscreen && !in_fullscreen_transition {
        let overlay_held = state
            .ui_runtime
            .fullscreen_overlay_visible_until
            .is_some_and(|until| now <= until);
        show_fullscreen_overlay = fullscreen_overlay_near || overlay_held;
    } else {
        state.ui_runtime.fullscreen_overlay_visible_until = None;
    }
    if show_fullscreen_overlay {
        let overlay_result = draw_fullscreen_overlay(
            ui,
            FullscreenOverlayContext {
                state,
                language,
                area: &used_rect,
                favorite_state,
                favorite_toggle_pending,
                interaction_blocked,
                external_tools,
                external_tool_state,
                global_quality,
                capabilities,
            },
            &mut toolbar_events,
        );
        overlay_interacting = overlay_result.interacting;
        if !in_viewport_transition {
            if let Some(v) = overlay_result.new_view {
                state.request_view(v, display_w, display_h, max_tex_side, &ctx, "Toolbar");
            }
        }
    }
    if is_fullscreen && !in_fullscreen_transition {
        if fullscreen_overlay_near || overlay_interacting {
            state.ui_runtime.fullscreen_overlay_visible_until =
                Some(now + FULLSCREEN_OVERLAY_AUTO_HIDE_DELAY);
        } else if state
            .ui_runtime
            .fullscreen_overlay_visible_until
            .is_some_and(|until| now > until)
        {
            state.ui_runtime.fullscreen_overlay_visible_until = None;
        }
        if show_fullscreen_overlay && !fullscreen_overlay_near && !overlay_interacting {
            if let Some(until) = state.ui_runtime.fullscreen_overlay_visible_until {
                if now < until {
                    ctx.request_repaint_after(until.saturating_duration_since(now));
                }
            }
        }
    }

    if state.ui_runtime.loading {
        ctx.request_repaint_after(LOADING_REPAINT_INTERVAL);
    }
    if in_fullscreen_transition {
        ctx.request_repaint();
    }

    // ── 下部スライダー（Fullscreen では非表示） ──────────────────────────────
    if !is_fullscreen && !in_fullscreen_transition {
        let sld_rect = Rect::from_min_size(
            pos2(content.min.x, content.min.y + img_h),
            vec2(content.width(), SLIDER_H),
        );
        let mut new_view: Option<u32> = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(sld_rect), |ui| {
            new_view = render_page_progress_bar(ui, state, show_pending, language);
        });
        if !in_viewport_transition {
            if let Some(v) = new_view {
                state.request_view(v, display_w, display_h, max_tex_side, &ctx, "Toolbar");
                state.close_boundary_preview_on_successful_page_move(true);
            }
        }
    }

    let boundary_preview_visible =
        state.boundary_preview_visible(capabilities.allow_book_navigation, is_fullscreen);
    if boundary_preview_visible {
        if let Some(card_action) = draw_boundary_preview_card(
            ui,
            state,
            language,
            &used_rect,
            boundary_preview_thumb_size,
            boundary_preview_hud_font_size,
        ) {
            match card_action {
                BoundaryPreviewCardAction::Close => {
                    state.close_boundary_preview();
                }
                BoundaryPreviewCardAction::PreviousBook => {
                    action = ViewerAction::PreviousBook;
                }
                BoundaryPreviewCardAction::NextBook => {
                    action = ViewerAction::NextBook;
                }
            }
        }
    }

    apply_toolbar_state_actions(
        state,
        &toolbar_events,
        ToolbarApplyContext {
            global_quality,
            now,
            in_viewport_transition,
            display_w,
            display_h,
            max_tex_side,
            ctx: &ctx,
            capabilities,
            action: &mut action,
        },
        settings_sink,
    );
    if toolbar_events.toggle_fullscreen {
        state.stop_slideshow();
        action = ViewerAction::ToggleFullscreen;
    }
    apply_external_tool_toolbar_actions(
        state,
        &mut toolbar_events,
        external_tool_state,
        &mut action,
    );

    if !in_viewport_transition {
        let _ = state.maybe_request_animation_stream_fill(display_w, display_h, max_tex_side);
    }
    if !in_viewport_transition && state.request.animation_stream_request_id.is_some() {
        ctx.request_repaint_after(LOADING_REPAINT_INTERVAL);
    }
    if state.playback.slideshow_active {
        let repaint_after = state
            .playback
            .slideshow_next_slide_at
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
            .unwrap_or(LOADING_REPAINT_INTERVAL);
        ctx.request_repaint_after(repaint_after.min(Duration::from_secs(1)));
    }
    if !in_viewport_transition {
        let _ = state.maybe_run_gpu_warmup(&ctx, display_w, display_h, max_tex_side);
    }
    if state.transition_logs_active() {
        state.ui_runtime.transition_log_frames_left = state
            .ui_runtime
            .transition_log_frames_left
            .saturating_sub(1);
    }

    action
}
