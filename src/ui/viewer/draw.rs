#[cfg(debug_assertions)]
use std::sync::OnceLock;
use std::time::Duration;

use eframe::egui::{self, pos2, vec2, Color32, Rect};

use super::icons;
use super::progress::render_page_progress_bar;
use super::theme;
use super::toolbar::{render_viewer_toolbar, ViewerToolbarContext};
use super::ExternalToolButtonModel;
use super::ExternalToolToolbarState;
use super::OverlayRenderResult;
use super::ToolbarEvents;
use super::ViewerDeleteRangeSelection;
use super::ViewerState;
use super::ViewerUiCapabilities;
use crate::domain::app_settings::ReadingDirection;
use crate::domain::app_settings::UiLanguage;
use crate::infra::ipc::ViewerFavoriteState;
use crate::ui::i18n::{tr, TextKey};
use crate::ui::thumb_cache::LoadedDiskThumb;

#[cfg(debug_assertions)]
use super::working_set::{page_render_signature_rank, DisplayRequirement, RenderSignature};
#[cfg(debug_assertions)]
use crate::domain::archive_settings::SpreadMode;

const FULLSCREEN_OVERLAY_EDGE_THRESHOLD: f32 = 56.0;
const FULLSCREEN_OVERLAY_TOP_H: f32 = 38.0;
const FULLSCREEN_OVERLAY_BOTTOM_H: f32 = 34.0;
const FULLSCREEN_OVERLAY_TOP_MARGIN: f32 = 15.0;
const FULLSCREEN_OVERLAY_TOP_HOVER_PADDING: f32 = 16.0;
const KEY_FEEDBACK_FONT_SIZE: f32 = 22.0;
const HELP_KEY_COL_WIDTH: usize = 10;
const DELETE_RANGE_OVERLAY_FONT_SIZE: f32 = theme::FONT_SIZE_BODY * 2.0;
const DELETE_RANGE_OVERLAY_BAND_H: f32 = DELETE_RANGE_OVERLAY_FONT_SIZE * 2.0;
const DELETE_RANGE_SELECTING_FILL_ALPHA: u8 = 120;
const DELETE_RANGE_COMPLETE_FILL_ALPHA: u8 = 120;
const DELETE_RANGE_TEXT: Color32 = Color32::WHITE;
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_MARGIN_X: f32 = 8.0;
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_MARGIN_Y: f32 = 8.0;
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_TOP_RATIO: f32 = 0.38;
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_SAFE_BOTTOM_GAP: f32 = 96.0;
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_WIDTH: f32 = 560.0;
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_LINE_HEIGHT: f32 = 19.0;
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_PADDING_Y: f32 = 18.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoundaryPreviewCardAction {
    Close,
    PreviousBook,
    NextBook,
}

pub(super) fn draw_pages(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    area: &Rect,
    effective_spread: bool,
) -> Duration {
    let ctx = ui.ctx().clone();
    let mut min_remaining = Duration::from_secs(3600);
    let current_page = state.persistent.displayed_page;
    let visible_pages = state.current_view_pages(current_page);
    let visible_page_bounds = visible_page_bounds(visible_pages);
    let delete_range_selection = state.delete_range_selection();

    if effective_spread {
        let reading_direction = state.effective_reading_direction();
        let leading_cover_blank_spread = state.is_leading_cover_blank_spread(current_page);
        let (screen_left_page, screen_right_page) =
            spread_screen_pages(visible_pages, reading_direction);
        let (left_size, right_size) = match reading_direction {
            ReadingDirection::RightToLeft => (
                state
                    .display_assets
                    .content_right
                    .as_ref()
                    .map(|c| c.texture().size_vec2()),
                state
                    .display_assets
                    .content_left
                    .as_ref()
                    .map(|c| c.texture().size_vec2()),
            ),
            ReadingDirection::LeftToRight => (
                state
                    .display_assets
                    .content_left
                    .as_ref()
                    .map(|c| c.texture().size_vec2()),
                state
                    .display_assets
                    .content_right
                    .as_ref()
                    .map(|c| c.texture().size_vec2()),
            ),
        };
        let (ldraw, rdraw) = if leading_cover_blank_spread {
            let cover_size = state
                .display_assets
                .content_left
                .as_ref()
                .map(|c| c.texture().size_vec2())
                .or(left_size)
                .or(right_size);
            match cover_size {
                Some(size) => compute_spread_rects(area, Some(size), Some(size)),
                None => (None, None),
            }
        } else {
            compute_spread_rects(area, left_size, right_size)
        };

        if let (Some(lr), Some(_)) = (&ldraw, &rdraw) {
            let x = lr.max.x;
            ui.painter().line_segment(
                [pos2(x, area.min.y), pos2(x, area.max.y)],
                egui::Stroke::new(1.0, theme::BORDER.linear_multiply(0.75)),
            );
        }

        match reading_direction {
            ReadingDirection::RightToLeft => {
                if leading_cover_blank_spread {
                    if let Some(rect) = ldraw {
                        ui.painter()
                            .rect_filled(rect, egui::CornerRadius::ZERO, Color32::WHITE);
                    }
                } else if let Some(c) = &mut state.display_assets.content_right {
                    let r = c.tick("viewer_right", &ctx);
                    min_remaining = min_remaining.min(r);
                    if let Some(rect) = ldraw {
                        draw_image_at_rect(ui, c.texture(), &rect);
                        if let Some(page) = screen_left_page {
                            if let Some(overlay) = delete_range_overlay_for_page(
                                delete_range_selection,
                                visible_page_bounds,
                                page,
                                false,
                            ) {
                                draw_delete_range_image_overlay(
                                    ui,
                                    &rect,
                                    delete_range_selection,
                                    overlay,
                                );
                            }
                        }
                    }
                }
                if let Some(c) = &mut state.display_assets.content_left {
                    let r = c.tick("viewer_left", &ctx);
                    min_remaining = min_remaining.min(r);
                    if let Some(rect) = rdraw {
                        draw_image_at_rect(ui, c.texture(), &rect);
                        if let Some(page) = screen_right_page {
                            if let Some(overlay) = delete_range_overlay_for_page(
                                delete_range_selection,
                                visible_page_bounds,
                                page,
                                false,
                            ) {
                                draw_delete_range_image_overlay(
                                    ui,
                                    &rect,
                                    delete_range_selection,
                                    overlay,
                                );
                            }
                        }
                    }
                } else if leading_cover_blank_spread {
                    if let Some(rect) = rdraw {
                        ui.painter()
                            .rect_filled(rect, egui::CornerRadius::ZERO, Color32::WHITE);
                    }
                }
            }
            ReadingDirection::LeftToRight => {
                if let Some(c) = &mut state.display_assets.content_left {
                    let r = c.tick("viewer_left", &ctx);
                    min_remaining = min_remaining.min(r);
                    if let Some(rect) = ldraw {
                        draw_image_at_rect(ui, c.texture(), &rect);
                        if let Some(page) = screen_left_page {
                            if let Some(overlay) = delete_range_overlay_for_page(
                                delete_range_selection,
                                visible_page_bounds,
                                page,
                                false,
                            ) {
                                draw_delete_range_image_overlay(
                                    ui,
                                    &rect,
                                    delete_range_selection,
                                    overlay,
                                );
                            }
                        }
                    }
                } else if leading_cover_blank_spread {
                    if let Some(rect) = ldraw {
                        ui.painter()
                            .rect_filled(rect, egui::CornerRadius::ZERO, Color32::WHITE);
                    }
                }
                if leading_cover_blank_spread {
                    if let Some(rect) = rdraw {
                        ui.painter()
                            .rect_filled(rect, egui::CornerRadius::ZERO, Color32::WHITE);
                    }
                } else if let Some(c) = &mut state.display_assets.content_right {
                    let r = c.tick("viewer_right", &ctx);
                    min_remaining = min_remaining.min(r);
                    if let Some(rect) = rdraw {
                        draw_image_at_rect(ui, c.texture(), &rect);
                        if let Some(page) = screen_right_page {
                            if let Some(overlay) = delete_range_overlay_for_page(
                                delete_range_selection,
                                visible_page_bounds,
                                page,
                                false,
                            ) {
                                draw_delete_range_image_overlay(
                                    ui,
                                    &rect,
                                    delete_range_selection,
                                    overlay,
                                );
                            }
                        }
                    }
                }
            }
        }
    } else {
        if let Some(c) = &mut state.display_assets.content_left {
            let r = c.tick("viewer_page", &ctx);
            min_remaining = min_remaining.min(r);
            if let Some(rect) = draw_page_in_rect(ui, Some(c.texture()), area) {
                if let Some(page) = visible_pages.0.or(visible_pages.1) {
                    if let Some(overlay) = delete_range_overlay_for_page(
                        delete_range_selection,
                        visible_page_bounds,
                        page,
                        false,
                    ) {
                        draw_delete_range_image_overlay(ui, &rect, delete_range_selection, overlay);
                    }
                }
            }
        } else {
            let _ = draw_page_in_rect(ui, None, area);
        }
    }

    min_remaining
}

fn operation_help_text(
    _language: UiLanguage,
    capabilities: ViewerUiCapabilities,
    state: &ViewerState,
) -> String {
    let mut lines = Vec::with_capacity(10);
    let delete_range = state.delete_range_selection();
    lines.push(help_row("←→ / Wheel", "Page"));
    if let Some(boundary_preview_help) = boundary_preview_enter_help(capabilities, state) {
        lines.push(boundary_preview_help);
    }
    if capabilities.allow_book_navigation {
        lines.push(help_row("↑↓", "Book"));
    }
    lines.push(help_row("Home / End", "First / Last"));
    lines.push(help_row("Space", "Slideshow"));
    lines.push(help_row("F11", "Fullscreen"));
    if capabilities.allow_delete {
        match (delete_range.start, delete_range.end) {
            (None, None) => {
                lines.push(help_row("M", "Mark Start"));
                lines.push(help_row("Del", "Del Book"));
            }
            (Some(_), None) => {
                lines.push(help_row("M", "Mark End"));
                lines.push(help_row("Del", "Del Book"));
            }
            (Some(_), Some(_)) => {
                lines.push(help_row("M", "Restart Range"));
                lines.push(help_row("Del", "Del Range"));
            }
            (None, Some(_)) => {}
        }
    }
    if delete_range.has_any() {
        lines.push(help_row("Esc", "Clear Range"));
    } else if capabilities.allow_book_navigation {
        lines.push(help_row("Esc", "Library"));
    } else {
        lines.push(help_row("Esc", "Close"));
    }
    if let Some(range_text) = delete_range_text(delete_range) {
        lines.push(help_row("Range", &range_text));
    }
    lines.join("\n")
}

fn boundary_preview_enter_help(
    capabilities: ViewerUiCapabilities,
    state: &ViewerState,
) -> Option<String> {
    if !state.boundary_preview_visible(capabilities.allow_book_navigation, false) {
        return None;
    }
    let view = state.boundary_preview_ready_view()?;
    let text = match view.direction {
        super::BoundaryPreviewDirection::Previous => help_row("Enter", "Prev Book"),
        super::BoundaryPreviewDirection::Next => help_row("Enter", "Next Book"),
    };
    Some(text)
}

fn help_row(key: &str, action: &str) -> String {
    format!("{key:<HELP_KEY_COL_WIDTH$}: {action}")
}

fn delete_range_text(selection: ViewerDeleteRangeSelection) -> Option<String> {
    match (selection.start, selection.end) {
        (Some(start), Some(end)) => Some(format!("S={} E={}", start + 1, end + 1)),
        (Some(start), None) => Some(format!("S={}", start + 1)),
        (None, None) => None,
        (None, Some(_)) => None,
    }
}

fn draw_operation_help_overlay(
    ui: &egui::Ui,
    area: &Rect,
    language: UiLanguage,
    capabilities: ViewerUiCapabilities,
    state: &ViewerState,
) {
    let help_text = operation_help_text(language, capabilities, state);
    ui.painter().text(
        pos2(area.min.x + 8.0, area.max.y - 6.0),
        egui::Align2::LEFT_BOTTOM,
        help_text,
        egui::FontId::monospace(theme::FONT_SIZE_SMALL),
        theme::TEXT_SUBTLE,
    );
}

#[cfg(debug_assertions)]
fn debug_cache_overlay_enabled() -> bool {
    static DEBUG_OVERLAY_ENABLED: OnceLock<bool> = OnceLock::new();
    *DEBUG_OVERLAY_ENABLED.get_or_init(|| {
        std::env::var("RUST_LOG")
            .map(|v| {
                let target = crate::app_identity::LOG_TARGET;
                v.contains(&format!("{target}=debug")) || v.contains(&format!("{target}=trace"))
            })
            .unwrap_or(false)
    })
}

#[cfg(debug_assertions)]
fn format_page_list(pages: &[u32], max_items: usize) -> String {
    if pages.is_empty() {
        return "-".to_owned();
    }
    let mut cells: Vec<String> = pages
        .iter()
        .take(max_items)
        .map(|page| page.to_string())
        .collect();
    if pages.len() > max_items {
        cells.push(format!("...(+{})", pages.len() - max_items));
    }
    cells.join(", ")
}

#[cfg(debug_assertions)]
fn format_spread_mode_tag(mode: SpreadMode) -> &'static str {
    match mode {
        SpreadMode::Auto => "AUTO",
        SpreadMode::Single => "1P",
        SpreadMode::Spread => "2P",
    }
}

#[cfg(debug_assertions)]
fn page_range_text(pages: &[u32]) -> String {
    match (pages.iter().min(), pages.iter().max()) {
        (Some(first), Some(last)) => format!("{first}..{last}"),
        _ => "-".to_owned(),
    }
}

#[cfg(debug_assertions)]
fn format_mib(bytes: usize) -> String {
    const MIB: u128 = 1024 * 1024;
    (((bytes as u128) + MIB / 2) / MIB).to_string()
}

#[cfg(debug_assertions)]
fn format_mib_pair(current_bytes: usize, max_bytes: usize) -> String {
    format!(
        "{} MiB / {} MiB",
        format_mib(current_bytes),
        format_mib(max_bytes)
    )
}

#[cfg(debug_assertions)]
#[derive(Clone)]
struct DebugTextureCandidate {
    page: u32,
    bytes: usize,
    signature: RenderSignature,
    source: &'static str,
}

#[cfg(debug_assertions)]
fn best_debug_texture_candidate(
    candidates: &[DebugTextureCandidate],
    page: u32,
    requirement: DisplayRequirement,
) -> Option<&DebugTextureCandidate> {
    let mut best: Option<(DebugTextureCandidateRank, &DebugTextureCandidate)> = None;
    for candidate in candidates {
        let Some(rank) = page_render_signature_rank(
            candidate.page,
            candidate.signature,
            page,
            requirement,
            candidate.bytes,
        ) else {
            continue;
        };
        if best.as_ref().is_none_or(|(prev, _)| rank < *prev) {
            best = Some((rank, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

#[cfg(debug_assertions)]
fn debug_hit_source_for_page(
    page: u32,
    requirement: DisplayRequirement,
    interactive: &[DebugTextureCandidate],
    history: &[DebugTextureCandidate],
    future: &[DebugTextureCandidate],
) -> Option<&'static str> {
    if best_debug_texture_candidate(interactive, page, requirement).is_some() {
        return Some("Interactive");
    }

    let history = best_debug_texture_candidate(history, page, requirement)?;
    let future = best_debug_texture_candidate(future, page, requirement);
    match future {
        Some(future) => {
            let history_score = history.signature.target_w.abs_diff(requirement.required_w) as u64
                + history.signature.target_h.abs_diff(requirement.required_h) as u64;
            let future_score = future.signature.target_w.abs_diff(requirement.required_w) as u64
                + future.signature.target_h.abs_diff(requirement.required_h) as u64;
            if history_score <= future_score {
                Some(history.source)
            } else {
                Some(future.source)
            }
        }
        None => Some(history.source),
    }
}

#[cfg(debug_assertions)]
fn debug_overlay_state_label(
    bg: Option<&super::worker_manager::ViewerWorkerManagerDebugState>,
    configured_workers: usize,
) -> &'static str {
    let Some(bg) = bg else {
        return "Stopped";
    };
    if bg.inflight_by_request_id == 0 && bg.fifo_len == 0 {
        return "Idle";
    }
    let saturated = bg.inflight_by_request_id >= configured_workers.max(1) && bg.fifo_len > 0
        || matches!(
            bg.dispatch_limit_reason,
            Some(
                "no_worker_capacity"
                    | "cache_limit_unavailable"
                    | "cache_full_no_priority_improvement"
            )
        );
    if saturated {
        "Saturated"
    } else {
        "Running"
    }
}

#[cfg(debug_assertions)]
fn draw_debug_cache_overlay(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    area: &Rect,
    display_w: u32,
    display_h: u32,
    max_tex_side: u32,
) {
    if !debug_cache_overlay_enabled() {
        return;
    }
    ui.ctx().request_repaint_after(Duration::from_millis(250));

    let current_page = state.persistent.displayed_page;
    let visible_pages = state.current_view_pages(current_page);
    let visible_pages_list: Vec<u32> = [visible_pages.0, visible_pages.1]
        .into_iter()
        .flatten()
        .collect();
    let current_requirement =
        state.display_requirement_for_request(display_w, display_h, max_tex_side);
    let gpu_history = state.gpu_texture_history_snapshot();
    let gpu_warmup = state.gpu_warmup_cache_snapshot();
    let gpu_warmup_plan = state.gpu_warmup_plan_snapshot();
    let bg_debug_state = state.request.worker_manager.debug_state();
    let bg_rgba_cache = state.request.worker_manager.bg_rgba_cache();
    let interactive_candidates = state
        .display_assets
        .interactive_rgba_cache
        .ready_entry_snapshots()
        .into_iter()
        .map(|entry| DebugTextureCandidate {
            page: entry.page,
            bytes: entry.bytes,
            signature: entry.signature,
            source: "Interactive",
        })
        .collect::<Vec<_>>();
    let history_candidates = state
        .display_assets
        .gpu_texture_history
        .entry_snapshots()
        .into_iter()
        .map(|entry| DebugTextureCandidate {
            page: entry.page,
            bytes: entry.bytes,
            signature: entry.key.render_signature,
            source: "History",
        })
        .collect::<Vec<_>>();
    let future_candidates = state
        .display_assets
        .gpu_warmup_cache
        .entry_snapshots()
        .into_iter()
        .map(|entry| DebugTextureCandidate {
            page: entry.page,
            bytes: entry.bytes,
            signature: entry.key.render_signature,
            source: "Future",
        })
        .collect::<Vec<_>>();
    let hit_label = if visible_pages_list.is_empty() {
        "-"
    } else {
        visible_pages_list
            .iter()
            .copied()
            .find_map(|page| {
                debug_hit_source_for_page(
                    page,
                    current_requirement,
                    &interactive_candidates,
                    &history_candidates,
                    &future_candidates,
                )
            })
            .unwrap_or("Miss")
    };
    let page_mode = format_spread_mode_tag(state.persistent.spread_setting.clone());
    let display_unit = if visible_pages_list.len() >= 2 {
        "2P"
    } else {
        "1P"
    };
    let configured_workers = state.request.background_worker_count;
    let bg_state_label = debug_overlay_state_label(bg_debug_state.as_ref(), configured_workers);
    let l2_missing = gpu_warmup_plan.pending_uploads;
    let (l2_pages, l2_bytes, l2_cap, l2_range) = bg_rgba_cache
        .read()
        .ok()
        .map(|cache| {
            let pages = cache.entry_count();
            let bytes = cache.current_bytes();
            let cap = cache.max_bytes();
            let range = page_range_text(&cache.page_order());
            (pages, bytes, cap, range)
        })
        .unwrap_or_else(|| (0, 0, 0, "-".to_owned()));

    let page_section = vec![
        "Page".to_owned(),
        format!("  Current : {}", current_page),
        format!("  Visible : {}", format_page_list(&visible_pages_list, 8)),
        format!("  Mode    : {} / {}", page_mode, display_unit),
    ];
    let l1_section = vec![
        "L1 VRAM".to_owned(),
        format!(
            "  Future  : {} pages / {}",
            gpu_warmup.entry_count,
            format_mib_pair(gpu_warmup.current_bytes, gpu_warmup.max_bytes)
        ),
        format!(
            "  History : {} pages / {}",
            gpu_history.entry_count,
            format_mib_pair(gpu_history.current_bytes, gpu_history.max_bytes)
        ),
        format!("  Hit     : {}", hit_label),
        format!(
            "  Upload  : {}",
            gpu_warmup_plan
                .upload_page
                .map(|page| page.to_string())
                .unwrap_or_else(|| "-".to_owned())
        ),
        format!("  Replace : {}", gpu_warmup_plan.replacement_count),
    ];
    let l2_section = vec![
        "L2 RAM".to_owned(),
        format!(
            "  Cache   : {} pages / {} / {}",
            l2_pages,
            format_mib(l2_bytes),
            format_mib(l2_cap)
        ),
        format!("  Range   : {}", l2_range),
        format!(
            "  Inflight: {}",
            bg_debug_state
                .as_ref()
                .map(|bg| bg.inflight_by_request_id)
                .unwrap_or(0)
        ),
        format!("  Missing : {}", l2_missing),
    ];
    let bg_section = vec![
        "BG".to_owned(),
        format!("  Workers : {}", configured_workers),
        format!(
            "  Active  : {}",
            bg_debug_state
                .as_ref()
                .map(|bg| bg.inflight_by_request_id)
                .unwrap_or(0)
        ),
        format!(
            "  Queue   : {}",
            bg_debug_state.as_ref().map(|bg| bg.fifo_len).unwrap_or(0)
        ),
        format!("  State   : {}", bg_state_label),
    ];

    let sections = [page_section, l1_section, l2_section, bg_section];
    let debug_text = sections
        .iter()
        .map(|section| section.join("\n"))
        .collect::<Vec<_>>()
        .join("\n\n");

    let overlay_w = DEBUG_OVERLAY_WIDTH;
    let x = area.min.x + DEBUG_OVERLAY_MARGIN_X;
    let line_count = sections.iter().map(|section| section.len()).sum::<usize>() + 3;
    let overlay_h = line_count as f32 * DEBUG_OVERLAY_LINE_HEIGHT + DEBUG_OVERLAY_PADDING_Y;
    let target_top = area.min.y + area.height() * DEBUG_OVERLAY_TOP_RATIO;
    let max_top = area.max.y - overlay_h - DEBUG_OVERLAY_SAFE_BOTTOM_GAP;
    let y = target_top
        .min(max_top)
        .max(area.min.y + DEBUG_OVERLAY_MARGIN_Y);
    let overlay_rect = Rect::from_min_size(pos2(x, y), vec2(overlay_w, overlay_h));
    let resp = ui.scope_builder(egui::UiBuilder::new().max_rect(overlay_rect), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
        ui.visuals_mut().override_text_color = Some(theme::TEXT_SUBTLE);
        for (index, section) in sections.iter().enumerate() {
            for line in section {
                ui.monospace(line);
            }
            if index + 1 != sections.len() {
                ui.add_space(6.0);
            }
        }
        ui.allocate_rect(overlay_rect, egui::Sense::click())
    });
    let resp = resp.inner;
    resp.context_menu(|ui| {
        if ui.button("Copy debug overlay").clicked() {
            ui.ctx().copy_text(debug_text.clone());
            ui.close();
        }
    });
}

fn should_show_fullscreen_overlay(ctx: &egui::Context, area: &Rect) -> bool {
    let pointer_pos = match ctx.input(|i| i.pointer.hover_pos()) {
        Some(pos) if area.contains(pos) => pos,
        _ => return false,
    };
    let top_region_end = area.min.y
        + FULLSCREEN_OVERLAY_TOP_MARGIN
        + FULLSCREEN_OVERLAY_TOP_H
        + FULLSCREEN_OVERLAY_TOP_HOVER_PADDING;
    let near_top_overlay_region =
        pointer_pos.y <= (area.min.y + FULLSCREEN_OVERLAY_EDGE_THRESHOLD).max(top_region_end);
    let near_bottom = pointer_pos.y >= area.max.y - FULLSCREEN_OVERLAY_EDGE_THRESHOLD;
    near_top_overlay_region || near_bottom
}

pub(super) fn draw_fullscreen_overlay(
    ui: &mut egui::Ui,
    overlay: FullscreenOverlayContext<'_>,
    toolbar_events: &mut ToolbarEvents,
) -> OverlayRenderResult {
    let FullscreenOverlayContext {
        state,
        language,
        area,
        favorite_state,
        favorite_toggle_pending,
        interaction_blocked,
        external_tools,
        external_tool_state,
        global_quality,
        capabilities,
    } = overlay;
    let _near_overlay = should_show_fullscreen_overlay(ui.ctx(), area);
    let screen_rect = ui.ctx().content_rect();
    let top_toolbar_rect = Rect::from_min_size(
        pos2(area.min.x, area.min.y + FULLSCREEN_OVERLAY_TOP_MARGIN),
        vec2(area.width(), FULLSCREEN_OVERLAY_TOP_H.min(area.height())),
    );
    let top_overlay_rect = Rect::from_min_max(
        pos2(screen_rect.min.x, screen_rect.min.y),
        pos2(screen_rect.max.x, top_toolbar_rect.max.y.min(area.max.y)),
    );
    let bottom_h = FULLSCREEN_OVERLAY_BOTTOM_H.min(area.height());
    let bottom_widget_rect = Rect::from_min_size(
        pos2(area.min.x, area.max.y - bottom_h),
        vec2(area.width(), bottom_h),
    );
    let bottom_overlay_rect = Rect::from_min_max(
        pos2(screen_rect.min.x, bottom_widget_rect.min.y),
        pos2(screen_rect.max.x, screen_rect.max.y),
    );
    let overlay_fill = theme::TOOLBAR_BG;
    if state.transition_logs_active() {
        tracing::trace!(
            frame = state.ui_runtime.show_seq,
            top_overlay_rect = ?top_overlay_rect,
            bottom_overlay_rect = ?bottom_overlay_rect,
            top_toolbar_rect = ?top_toolbar_rect,
            bottom_widget_rect = ?bottom_widget_rect,
            screen_rect = ?ui.ctx().content_rect(),
            transition = state.ui_runtime.viewport_transition_active || state.ui_runtime.fullscreen_transition_frames > 0,
            "viewer_ui: fullscreen overlay draw"
        );
    }

    ui.painter()
        .rect_filled(top_overlay_rect, egui::CornerRadius::ZERO, overlay_fill);
    let top_resp = ui.scope_builder(egui::UiBuilder::new().max_rect(top_toolbar_rect), |ui| {
        egui::Frame::default()
            .fill(overlay_fill)
            .stroke(egui::Stroke::new(1.0, theme::SEPARATOR_WEAK))
            .show(ui, |ui| {
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
                    toolbar_events,
                );
            });
    });
    let mut new_view = None;
    let show_pending = state.show_follow_placeholder();
    ui.painter()
        .rect_filled(bottom_overlay_rect, egui::CornerRadius::ZERO, overlay_fill);
    let bottom_resp = ui.scope_builder(egui::UiBuilder::new().max_rect(bottom_widget_rect), |ui| {
        egui::Frame::default().fill(overlay_fill).show(ui, |ui| {
            new_view = render_page_progress_bar(ui, state, show_pending, language);
        });
    });
    OverlayRenderResult {
        new_view,
        interacting: top_resp.response.hovered()
            || top_resp.response.contains_pointer()
            || bottom_resp.response.hovered()
            || bottom_resp.response.contains_pointer()
            || ui.ctx().egui_wants_pointer_input(),
    }
}

pub(super) fn fullscreen_overlay_near(ctx: &egui::Context, area: &Rect) -> bool {
    should_show_fullscreen_overlay(ctx, area)
}

pub(super) fn compute_spread_rects(
    area: &Rect,
    lsize: Option<egui::Vec2>,
    rsize: Option<egui::Vec2>,
) -> (Option<Rect>, Option<Rect>) {
    let h = area.height();
    let w = area.width();
    if h <= 0.0 || w <= 0.0 {
        return (None, None);
    }

    match (lsize, rsize) {
        (None, None) => (None, None),

        (Some(ls), None) => {
            let scale = (w / ls.x).min(h / ls.y);
            let disp = ls * scale;
            let off = (vec2(w, h) - disp) * 0.5;
            (Some(Rect::from_min_size(area.min + off, disp)), None)
        }

        (None, Some(rs)) => {
            let scale = (w / rs.x).min(h / rs.y);
            let disp = rs * scale;
            let off = (vec2(w, h) - disp) * 0.5;
            (None, Some(Rect::from_min_size(area.min + off, disp)))
        }

        (Some(ls), Some(rs)) => {
            let ldw = ls.x * (h / ls.y);
            let rdw = rs.x * (h / rs.y);
            let total_w = ldw + rdw;
            let factor = if total_w > w { w / total_w } else { 1.0 };
            let (fldw, fldh) = (ldw * factor, h * factor);
            let (frdw, frdh) = (rdw * factor, h * factor);
            let start_x = area.min.x + (w - fldw - frdw) / 2.0;
            let center_y = area.center().y;
            let lrect = Rect::from_min_size(pos2(start_x, center_y - fldh / 2.0), vec2(fldw, fldh));
            let rrect = Rect::from_min_size(
                pos2(start_x + fldw, center_y - frdh / 2.0),
                vec2(frdw, frdh),
            );
            (Some(lrect), Some(rrect))
        }
    }
}

fn draw_image_at_rect(ui: &egui::Ui, tex: &egui::TextureHandle, rect: &Rect) {
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter().image(tex.id(), *rect, uv, Color32::WHITE);
}

fn draw_page_in_rect(
    ui: &egui::Ui,
    tex: Option<&egui::TextureHandle>,
    area: &Rect,
) -> Option<Rect> {
    match tex {
        None => {
            ui.painter()
                .rect_filled(*area, egui::CornerRadius::ZERO, Color32::WHITE);
            None
        }
        Some(t) => {
            let rect = fit_image_rect(area, t.size_vec2());
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            if let Some(rect) = rect {
                ui.painter().image(t.id(), rect, uv, Color32::WHITE);
                Some(rect)
            } else {
                None
            }
        }
    }
}

fn fit_image_rect(area: &Rect, tex_size: egui::Vec2) -> Option<Rect> {
    if area.width() <= 0.0 || area.height() <= 0.0 || tex_size.x <= 0.0 || tex_size.y <= 0.0 {
        return None;
    }
    let scale = (area.width() / tex_size.x).min(area.height() / tex_size.y);
    let disp = tex_size * scale;
    let off = (area.size() - disp) * 0.5;
    Some(Rect::from_min_size(area.min + off, disp))
}

#[derive(Clone, Copy)]
enum DeleteRangeImageOverlayKind {
    Start,
    Selecting,
    Complete,
}

fn delete_range_overlay_for_page(
    selection: ViewerDeleteRangeSelection,
    visible_page_bounds: Option<(u32, u32)>,
    page: u32,
    is_blank_slot: bool,
) -> Option<DeleteRangeImageOverlayKind> {
    if is_blank_slot {
        return None;
    }

    match (selection.start, selection.end) {
        (Some(start), Some(end)) => {
            let low = start.min(end);
            let high = start.max(end);
            if (low..=high).contains(&page) {
                Some(DeleteRangeImageOverlayKind::Complete)
            } else {
                None
            }
        }
        (Some(start), None) => {
            let (visible_min, visible_max) = visible_page_bounds?;
            let low = start.min(visible_min);
            let high = start.max(visible_max);
            if !(low..=high).contains(&page) {
                return None;
            }
            if page == start {
                Some(DeleteRangeImageOverlayKind::Start)
            } else {
                Some(DeleteRangeImageOverlayKind::Selecting)
            }
        }
        _ => None,
    }
}

fn draw_delete_range_image_overlay(
    ui: &egui::Ui,
    image_rect: &Rect,
    selection: ViewerDeleteRangeSelection,
    overlay: DeleteRangeImageOverlayKind,
) {
    let text = delete_range_overlay_text(selection, overlay);
    match overlay {
        DeleteRangeImageOverlayKind::Start => draw_top_band_overlay(
            ui,
            image_rect,
            DELETE_RANGE_OVERLAY_BAND_H,
            &text,
            delete_range_selecting_fill(),
            DELETE_RANGE_TEXT,
        ),
        DeleteRangeImageOverlayKind::Selecting => draw_top_band_overlay(
            ui,
            image_rect,
            DELETE_RANGE_OVERLAY_BAND_H,
            &text,
            delete_range_selecting_fill(),
            DELETE_RANGE_TEXT,
        ),
        DeleteRangeImageOverlayKind::Complete => draw_top_band_overlay(
            ui,
            image_rect,
            DELETE_RANGE_OVERLAY_BAND_H,
            &text,
            delete_range_complete_fill(),
            DELETE_RANGE_TEXT,
        ),
    }
}

fn draw_top_band_overlay(
    ui: &egui::Ui,
    image_rect: &Rect,
    band_height: f32,
    text: &str,
    fill: Color32,
    text_color: Color32,
) {
    let band_h = band_height.min(image_rect.height());
    let band_rect = Rect::from_min_size(image_rect.min, vec2(image_rect.width(), band_h));
    draw_band_overlay(ui, band_rect, text, fill, text_color);
}

fn draw_band_overlay(
    ui: &egui::Ui,
    band_rect: Rect,
    text: &str,
    fill: Color32,
    text_color: Color32,
) {
    let painter = ui.painter();
    painter.rect_filled(band_rect, egui::CornerRadius::ZERO, fill);
    painter.text(
        band_rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(DELETE_RANGE_OVERLAY_FONT_SIZE),
        text_color,
    );
}

fn delete_range_overlay_text(
    selection: ViewerDeleteRangeSelection,
    overlay: DeleteRangeImageOverlayKind,
) -> String {
    match overlay {
        DeleteRangeImageOverlayKind::Start => match selection.start {
            Some(start) => format!(
                "Delete Range: Start {}",
                format_delete_range_page_number(start)
            ),
            None => String::new(),
        },
        DeleteRangeImageOverlayKind::Selecting => match selection.start {
            Some(start) => format!(
                "Delete Range: Start {} …",
                format_delete_range_page_number(start)
            ),
            None => String::new(),
        },
        DeleteRangeImageOverlayKind::Complete => match (selection.start, selection.end) {
            (Some(start), Some(end)) => format!(
                "Delete Range: {} - {}",
                format_delete_range_page_number(start),
                format_delete_range_page_number(end)
            ),
            _ => String::new(),
        },
    }
}

fn format_delete_range_page_number(page: u32) -> String {
    format!("{}p", page.saturating_add(1))
}

fn delete_range_selecting_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(217, 119, 6, DELETE_RANGE_SELECTING_FILL_ALPHA)
}

fn delete_range_complete_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(220, 38, 38, DELETE_RANGE_COMPLETE_FILL_ALPHA)
}

fn visible_page_bounds(visible_pages: (Option<u32>, Option<u32>)) -> Option<(u32, u32)> {
    match (visible_pages.0, visible_pages.1) {
        (Some(left), Some(right)) => Some((left.min(right), left.max(right))),
        (Some(page), None) | (None, Some(page)) => Some((page, page)),
        (None, None) => None,
    }
}

fn spread_screen_pages(
    visible_pages: (Option<u32>, Option<u32>),
    reading_direction: ReadingDirection,
) -> (Option<u32>, Option<u32>) {
    match reading_direction {
        ReadingDirection::RightToLeft => (visible_pages.1, visible_pages.0),
        ReadingDirection::LeftToRight => (visible_pages.0, visible_pages.1),
    }
}

fn draw_center_text(ui: &egui::Ui, area: &Rect, text: &str, color: Color32) {
    ui.painter().text(
        area.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(theme::FONT_SIZE_EMPTY),
        color,
    );
}

fn draw_follow_placeholder(ui: &egui::Ui, state: &ViewerState, area: &Rect) {
    ui.painter()
        .rect_filled(*area, egui::CornerRadius::ZERO, Color32::WHITE);
    let _ = state;
}

pub(super) fn draw_viewer_overlays(ui: &mut egui::Ui, overlay: ViewerOverlayContext<'_>) {
    let ViewerOverlayContext {
        state: _state,
        area,
        language,
        display_w: _display_w,
        display_h: _display_h,
        max_tex_side: _max_tex_side,
        capabilities,
    } = overlay;
    #[cfg(debug_assertions)]
    {
        draw_debug_cache_overlay(ui, _state, area, _display_w, _display_h, _max_tex_side);
    }
    draw_operation_help_overlay(ui, area, language, capabilities, _state);
}

pub(super) fn draw_status_message(ui: &egui::Ui, area: &Rect, text: &str, color: Color32) {
    draw_center_text(ui, area, text, color);
}

pub(super) fn draw_key_feedback(ui: &egui::Ui, area: &Rect, text: &str) {
    ui.painter().text(
        area.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(KEY_FEEDBACK_FONT_SIZE),
        theme::TEXT_MAIN,
    );
}

pub(super) fn draw_follow_placeholder_panel(ui: &egui::Ui, state: &ViewerState, area: &Rect) {
    draw_follow_placeholder(ui, state, area);
}

pub(super) fn draw_boundary_preview_card(
    ui: &mut egui::Ui,
    state: &ViewerState,
    language: UiLanguage,
    area: &Rect,
    thumb_size: egui::Vec2,
    hud_font_size: f32,
) -> Option<BoundaryPreviewCardAction> {
    let view = state.boundary_preview_ready_view()?;
    let card_rect = boundary_preview_card_rect(
        area,
        thumb_size,
        hud_font_size,
        view.direction,
        state.effective_reading_direction(),
    );
    let card_resp = ui.allocate_rect(card_rect, egui::Sense::click());
    let hover = card_resp.hovered();
    let border = if hover {
        theme::HOVER_BORDER
    } else {
        theme::BORDER
    };
    ui.painter()
        .rect_filled(card_rect, egui::CornerRadius::same(8), theme::SURFACE_BG);
    ui.painter().rect_stroke(
        card_rect,
        egui::CornerRadius::same(8),
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );

    let header_h = 24.0;
    let pad = 8.0;
    let title_font = hud_font(hud_font_size);
    let line_h = (hud_font_size + 2.0).max(14.0);
    let title_lines = 3usize;
    let title_h = line_h * title_lines as f32 + 6.0;
    let thumb_rect = Rect::from_min_size(
        pos2(card_rect.min.x + pad, card_rect.min.y + header_h + pad),
        thumb_size,
    );
    let title_rect = Rect::from_min_max(
        pos2(card_rect.min.x + pad, thumb_rect.max.y + 6.0),
        pos2(card_rect.max.x - pad, card_rect.max.y - pad),
    );

    let close_clicked = draw_boundary_preview_header(
        ui,
        card_rect,
        language,
        view.direction,
        hover,
        view.book.title.as_ref(),
        header_h,
    );
    let painter = ui.painter();
    draw_boundary_preview_thumb(ui, view.thumbnail, thumb_rect);
    draw_boundary_preview_title(
        painter,
        title_rect,
        view.book.title.as_ref(),
        title_font,
        title_h,
    );

    if close_clicked {
        return Some(BoundaryPreviewCardAction::Close);
    }
    if card_resp.clicked() {
        return Some(match view.direction {
            super::BoundaryPreviewDirection::Previous => BoundaryPreviewCardAction::PreviousBook,
            super::BoundaryPreviewDirection::Next => BoundaryPreviewCardAction::NextBook,
        });
    }
    None
}

fn boundary_preview_card_rect(
    area: &Rect,
    thumb_size: egui::Vec2,
    hud_font_size: f32,
    direction: super::BoundaryPreviewDirection,
    reading_direction: crate::domain::app_settings::ReadingDirection,
) -> Rect {
    let header_h = 24.0;
    let pad = 8.0;
    let line_h = (hud_font_size + 2.0).max(14.0);
    let title_h = line_h * 3.0 + 6.0;
    let card_size = vec2(
        thumb_size.x + pad * 2.0,
        header_h + pad + thumb_size.y + 6.0 + title_h + pad,
    );
    let margin_x = 18.0;
    let margin_y = 0.0;
    let x = match (reading_direction, direction) {
        (
            crate::domain::app_settings::ReadingDirection::RightToLeft,
            super::BoundaryPreviewDirection::Previous,
        )
        | (
            crate::domain::app_settings::ReadingDirection::LeftToRight,
            super::BoundaryPreviewDirection::Next,
        ) => area.max.x - margin_x - card_size.x,
        _ => area.min.x + margin_x,
    }
    .clamp(
        area.min.x + 6.0,
        (area.max.x - card_size.x - 6.0).max(area.min.x),
    );
    let y = (area.center().y - card_size.y * 0.5).clamp(
        area.min.y + margin_y,
        (area.max.y - margin_y - card_size.y).max(area.min.y),
    );
    Rect::from_min_size(pos2(x, y), card_size)
}

fn draw_boundary_preview_header(
    ui: &mut egui::Ui,
    card_rect: Rect,
    language: UiLanguage,
    direction: super::BoundaryPreviewDirection,
    hovered: bool,
    title: &str,
    header_h: f32,
) -> bool {
    let painter = ui.painter();
    let header_rect = Rect::from_min_size(
        card_rect.min + vec2(8.0, 8.0),
        vec2(card_rect.width() - 16.0, header_h),
    );
    let heading = match direction {
        super::BoundaryPreviewDirection::Previous => tr(language, TextKey::PreviousBook),
        super::BoundaryPreviewDirection::Next => tr(language, TextKey::NextBook),
    };
    painter.text(
        header_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        heading,
        egui::FontId::proportional(theme::FONT_SIZE_BODY),
        theme::TEXT_MAIN,
    );

    let close_size = egui::vec2(14.0, 14.0);
    let close_rect = Rect::from_center_size(
        pos2(card_rect.max.x - 16.0, header_rect.center().y),
        close_size,
    );
    let close_id = ui.id().with(("boundary_preview_close", title, direction));
    let close_resp = ui.interact(close_rect, close_id, egui::Sense::click());
    painter.text(
        close_rect.center(),
        egui::Align2::CENTER_CENTER,
        icons::ICON_CLOSE.codepoint,
        egui::FontId::new(theme::FONT_SIZE_TINY, icons::ICON_CLOSE.font_family()),
        theme::TEXT_MAIN,
    );
    if hovered {
        painter.rect_stroke(
            close_rect.expand(1.0),
            egui::CornerRadius::same(3),
            egui::Stroke::new(1.0, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    }
    paint_hover_border(ui, &close_resp);
    close_resp.clicked()
}

fn draw_boundary_preview_thumb(ui: &egui::Ui, thumb: &LoadedDiskThumb, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(4), theme::PLACEHOLDER_BG);
    let fit = aspect_fit(thumb.texture.size_vec2(), rect);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    painter.image(thumb.texture.id(), fit, uv, Color32::WHITE);
}

fn draw_boundary_preview_title(
    painter: &egui::Painter,
    rect: Rect,
    title: &str,
    font: egui::FontId,
    _title_h: f32,
) {
    let text_color = theme::TEXT_MAIN;
    let clipped = painter.with_clip_rect(rect);
    let lines = layout_title_lines(painter, title, 3, rect.width(), font.clone());
    let line_h = (font.size + 2.0).max(14.0);
    let mut y = rect.min.y;
    for line in lines.into_iter().take(3) {
        clipped.text(
            pos2(rect.min.x, y),
            egui::Align2::LEFT_TOP,
            line,
            font.clone(),
            text_color,
        );
        y += line_h;
    }
}

fn hud_font(font_size: f32) -> egui::FontId {
    egui::FontId::proportional(font_size.clamp(8.0, 20.0))
}

fn aspect_fit(src: egui::Vec2, dst: Rect) -> Rect {
    if src.x <= 0.0 || src.y <= 0.0 || dst.width() <= 0.0 || dst.height() <= 0.0 {
        return dst;
    }
    let scale = (dst.width() / src.x).min(dst.height() / src.y);
    let size = src * scale;
    let offset = (dst.size() - size) * 0.5;
    Rect::from_min_size(dst.min + offset, size)
}

fn layout_title_lines(
    painter: &egui::Painter,
    title: &str,
    max_lines: usize,
    max_width: f32,
    font: egui::FontId,
) -> Vec<String> {
    if max_lines == 0 || title.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = title.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;

    while start < chars.len() && lines.len() < max_lines {
        let is_last_line = lines.len() + 1 == max_lines;
        let mut end = best_fit_end(painter, &chars, start, chars.len(), max_width, &font);
        if end <= start {
            end = (start + 1).min(chars.len());
        }

        let line: String = chars[start..end].iter().collect();
        if is_last_line && end < chars.len() {
            lines.push(fit_with_ellipsis(
                painter,
                &chars[start..],
                max_width,
                &font,
            ));
            break;
        }

        lines.push(line);
        start = end;
    }

    lines
}

fn best_fit_end(
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
        if measured_text_width(painter, &text, font) <= max_width {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid.saturating_sub(1);
        }
    }

    best
}

fn fit_with_ellipsis(
    painter: &egui::Painter,
    chars: &[char],
    max_width: f32,
    font: &egui::FontId,
) -> String {
    let ellipsis = "…";
    if measured_text_width(painter, ellipsis, font) > max_width {
        return String::new();
    }

    let mut lo = 0;
    let mut hi = chars.len();
    let mut best = 0;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let mut text: String = chars[..mid].iter().collect();
        text.push_str(ellipsis);
        if measured_text_width(painter, &text, font) <= max_width {
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

fn measured_text_width(painter: &egui::Painter, text: &str, font: &egui::FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_MAIN)
        .size()
        .x
}

fn paint_hover_border(ui: &egui::Ui, resp: &egui::Response) {
    if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    }
}
#[cfg(debug_assertions)]
type DebugTextureCandidateRank = (u64, u32, u32, usize);

pub(super) struct FullscreenOverlayContext<'a> {
    pub(super) state: &'a mut ViewerState,
    pub(super) language: UiLanguage,
    pub(super) area: &'a Rect,
    pub(super) favorite_state: ViewerFavoriteState,
    pub(super) favorite_toggle_pending: bool,
    pub(super) interaction_blocked: bool,
    pub(super) external_tools: &'a [ExternalToolButtonModel],
    pub(super) external_tool_state: &'a ExternalToolToolbarState,
    pub(super) global_quality: crate::domain::app_settings::ViewerQuality,
    pub(super) capabilities: ViewerUiCapabilities,
}

pub(super) struct ViewerOverlayContext<'a> {
    pub(super) state: &'a mut ViewerState,
    pub(super) area: &'a Rect,
    pub(super) language: UiLanguage,
    pub(super) display_w: u32,
    pub(super) display_h: u32,
    pub(super) max_tex_side: u32,
    pub(super) capabilities: ViewerUiCapabilities,
}
