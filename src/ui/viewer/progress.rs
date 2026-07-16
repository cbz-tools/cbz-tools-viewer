use eframe::egui;

use crate::domain::app_settings::ReadingDirection;
use crate::domain::app_settings::UiLanguage;
use crate::ui::i18n::format_page_count_label;

use super::theme;
use super::ViewerDeleteRangeSelection;
use super::ViewerState;

const DELETE_RANGE_OVERLAY_MIN_WIDTH: f32 = 5.0;
const DELETE_RANGE_OVERLAY_FILL_ALPHA: u8 = 84;
const DELETE_RANGE_OVERLAY_EDGE_ALPHA: u8 = 236;

pub(super) fn render_page_progress_bar(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    show_pending: bool,
    language: UiLanguage,
) -> Option<u32> {
    let mut new_view: Option<u32> = None;
    let reading_direction = state.effective_reading_direction();
    if state.transition_logs_active() {
        tracing::trace!(
            frame = state.ui_runtime.show_seq,
            progress_rect = ?ui.max_rect(),
            transition = state.ui_runtime.viewport_transition_active || state.ui_runtime.fullscreen_transition_frames > 0,
            "viewer_ui: progress draw"
        );
    }
    ui.separator();
    ui.horizontal(|ui| {
        let page_count = state.persistent.page_count;
        if page_count > 1 {
            let label_w = 90.0;
            ui.spacing_mut().slider_width = (ui.available_width() - label_w).max(40.0);
            let max = page_count.saturating_sub(1);
            let selected_physical_page = state.nav_target().min(max);
            let selected_visual_page =
                visual_page_from_physical(selected_physical_page, max, reading_direction);
            let last_page_visible = if page_count > 0 {
                let (visible_left, visible_right) =
                    state.current_view_pages(state.persistent.displayed_page);
                [visible_left, visible_right]
                    .into_iter()
                    .flatten()
                    .any(|page| page == max)
            } else {
                false
            };
            let mut visual_page = selected_visual_page;
            let resp = ui
                .scope(|ui| {
                    // 標準 Slider のトラック塗りには依存せず、進捗面は後段で手描きする。
                    ui.visuals_mut().widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
                    ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.inactive.fg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.hovered.bg_fill = egui::Color32::TRANSPARENT;
                    ui.visuals_mut().widgets.hovered.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.hovered.fg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().selection.bg_fill = egui::Color32::TRANSPARENT;
                    ui.visuals_mut().selection.stroke = egui::Stroke::NONE;
                    // 標準 thumb は弱めて、現在位置は後段で手描きする。
                    ui.visuals_mut().widgets.active.bg_fill = egui::Color32::TRANSPARENT;
                    ui.visuals_mut().widgets.active.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.active.fg_stroke = egui::Stroke::NONE;
                    ui.add(
                        egui::Slider::new(&mut visual_page, 0..=max)
                            .show_value(false)
                            .trailing_fill(true),
                    )
                })
                .inner;
            let actual_physical_page =
                physical_page_from_visual(visual_page, max, reading_direction);

            let slider_rect = resp.rect;
            let hovered_now = resp.hovered();
            let dragged_now = resp.dragged();
            let interaction_committed = resp.drag_stopped() || resp.clicked();
            let progress_physical_page = if dragged_now || interaction_committed {
                actual_physical_page
            } else if last_page_visible {
                max
            } else {
                selected_physical_page
            };
            let track_h = (slider_rect.height() - 10.0).clamp(6.0, 10.0);
            let track_rect = egui::Rect::from_center_size(
                slider_rect.center(),
                egui::vec2(slider_rect.width(), track_h),
            );
            let progress_ratio = if max > 0 {
                (progress_physical_page as f32) / (max as f32)
            } else {
                1.0
            }
            .clamp(0.0, 1.0);
            let filled_w = track_rect.width() * progress_ratio;
            let (filled_rect, thumb_x) = match reading_direction {
                ReadingDirection::RightToLeft => (
                    egui::Rect::from_min_max(
                        egui::pos2(track_rect.max.x - filled_w, track_rect.min.y),
                        track_rect.max,
                    ),
                    track_rect.max.x - filled_w,
                ),
                ReadingDirection::LeftToRight => (
                    egui::Rect::from_min_max(
                        track_rect.min,
                        egui::pos2(track_rect.min.x + filled_w, track_rect.max.y),
                    ),
                    track_rect.min.x + filled_w,
                ),
            };

            let painter = ui.painter();
            painter.rect_filled(track_rect, 3.0, theme::PROGRESS_BG);
            if filled_w > 0.0 {
                painter.rect_filled(filled_rect, 3.0, theme::PROGRESS_FILL);
            }
            draw_delete_range_overlay(
                painter,
                track_rect,
                state.delete_range_selection(),
                reading_direction,
                max,
            );
            let thumb_w = 6.0;
            let thumb_h = (track_rect.height() + 4.0).clamp(8.0, 12.0);
            let thumb_rect = egui::Rect::from_center_size(
                egui::pos2(thumb_x, track_rect.center().y),
                egui::vec2(thumb_w, thumb_h),
            );
            painter.rect_filled(thumb_rect, 2.0, theme::PROGRESS_ACTIVE);
            painter.rect_stroke(
                thumb_rect,
                2.0,
                egui::Stroke::new(1.0, theme::ACCENT_ACTIVE),
                egui::StrokeKind::Inside,
            );
            if hovered_now {
                painter.rect_stroke(
                    track_rect.expand(0.5),
                    3.0,
                    egui::Stroke::new(1.0, theme::HOVER_BORDER_WEAK),
                    egui::StrokeKind::Outside,
                );
            }
            let drag_fraction_milli = if max > 0 {
                let fraction_page = match reading_direction {
                    ReadingDirection::RightToLeft => actual_physical_page,
                    ReadingDirection::LeftToRight => visual_page,
                };
                Some(((fraction_page * 1000) / max).min(1000) as u16)
            } else {
                Some(1000)
            };
            let current_visual = state.pending_visual_state_for_progress(
                show_pending,
                hovered_now,
                dragged_now,
                if dragged_now {
                    drag_fraction_milli
                } else {
                    None
                },
            );
            state.update_pending_visual_state(current_visual);
            let pending_visible = current_visual.show_pending;

            let label = if pending_visible {
                build_physical_page_label(language, actual_physical_page, page_count)
            } else {
                build_page_label_ui(language, state)
            };
            ui.label(
                egui::RichText::new(label)
                    .size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_MAIN),
            );
            if interaction_committed && actual_physical_page != state.nav_target() {
                state.stop_slideshow();
                new_view = Some(actual_physical_page);
            }
        } else {
            let lbl = build_page_label(language, state);
            ui.label(
                egui::RichText::new(lbl)
                    .size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_SUBTLE),
            );
        }
    });
    new_view
}

fn draw_delete_range_overlay(
    painter: &egui::Painter,
    track_rect: egui::Rect,
    selection: ViewerDeleteRangeSelection,
    reading_direction: ReadingDirection,
    max_page: u32,
) {
    let Some(start) = selection.start else {
        return;
    };
    match selection.end {
        None => {
            let start_color = egui::Color32::from_rgba_unmultiplied(
                37,
                99,
                235,
                DELETE_RANGE_OVERLAY_EDGE_ALPHA,
            );
            let x = progress_x_for_physical_page(track_rect, start, max_page, reading_direction);
            painter.line_segment(
                [
                    egui::pos2(x, track_rect.min.y),
                    egui::pos2(x, track_rect.max.y),
                ],
                egui::Stroke::new(1.5, start_color),
            );
        }
        Some(end) => {
            let edge_color = egui::Color32::from_rgba_unmultiplied(
                theme::DELETE_RED.r(),
                theme::DELETE_RED.g(),
                theme::DELETE_RED.b(),
                DELETE_RANGE_OVERLAY_EDGE_ALPHA,
            );
            let start_visual = visual_page_from_physical(start, max_page, reading_direction);
            let end_visual = visual_page_from_physical(end, max_page, reading_direction);
            let left_x =
                progress_x_for_visual_page(track_rect, start_visual.min(end_visual), max_page);
            let right_x =
                progress_x_for_visual_page(track_rect, start_visual.max(end_visual), max_page);
            let mut left = left_x.min(right_x);
            let mut right = left_x.max(right_x);
            if right - left < DELETE_RANGE_OVERLAY_MIN_WIDTH {
                let center = (left + right) * 0.5;
                left = (center - DELETE_RANGE_OVERLAY_MIN_WIDTH * 0.5).clamp(
                    track_rect.min.x,
                    (track_rect.max.x - DELETE_RANGE_OVERLAY_MIN_WIDTH).max(track_rect.min.x),
                );
                right = (left + DELETE_RANGE_OVERLAY_MIN_WIDTH).min(track_rect.max.x);
            }
            let overlay_rect = egui::Rect::from_min_max(
                egui::pos2(left, track_rect.min.y + 0.5),
                egui::pos2(right, track_rect.max.y - 0.5),
            );
            let fill_color = egui::Color32::from_rgba_unmultiplied(
                theme::DELETE_RED.r(),
                theme::DELETE_RED.g(),
                theme::DELETE_RED.b(),
                DELETE_RANGE_OVERLAY_FILL_ALPHA,
            );
            painter.rect_filled(overlay_rect, 2.0, fill_color);
            painter.line_segment(
                [
                    egui::pos2(overlay_rect.min.x, track_rect.min.y),
                    egui::pos2(overlay_rect.min.x, track_rect.max.y),
                ],
                egui::Stroke::new(1.5, edge_color),
            );
            painter.line_segment(
                [
                    egui::pos2(overlay_rect.max.x, track_rect.min.y),
                    egui::pos2(overlay_rect.max.x, track_rect.max.y),
                ],
                egui::Stroke::new(1.5, edge_color),
            );
        }
    }
}

fn progress_x_for_visual_page(track_rect: egui::Rect, visual_page: u32, max_page: u32) -> f32 {
    if max_page == 0 {
        return track_rect.left();
    }
    track_rect.left() + track_rect.width() * (visual_page as f32 / max_page as f32)
}

fn progress_x_for_physical_page(
    track_rect: egui::Rect,
    physical_page: u32,
    max_page: u32,
    reading_direction: ReadingDirection,
) -> f32 {
    let visual_page = visual_page_from_physical(physical_page, max_page, reading_direction);
    progress_x_for_visual_page(track_rect, visual_page, max_page)
}

fn build_page_label(language: UiLanguage, state: &ViewerState) -> String {
    build_page_label_at(language, state, state.persistent.displayed_page)
}

fn build_page_label_ui(language: UiLanguage, state: &ViewerState) -> String {
    if state.has_pending_target() {
        build_page_label_at(language, state, state.nav_target())
    } else {
        build_page_label(language, state)
    }
}

fn build_physical_page_label(language: UiLanguage, page: u32, page_count: u32) -> String {
    if page_count > 0 {
        format_page_count_label(language, &(page + 1).to_string(), page_count)
    } else {
        "…".into()
    }
}

fn visual_page_from_physical(
    physical_page: u32,
    max_page: u32,
    reading_direction: ReadingDirection,
) -> u32 {
    match reading_direction {
        ReadingDirection::RightToLeft => max_page.saturating_sub(physical_page),
        ReadingDirection::LeftToRight => physical_page,
    }
}

fn physical_page_from_visual(
    visual_page: u32,
    max_page: u32,
    reading_direction: ReadingDirection,
) -> u32 {
    match reading_direction {
        ReadingDirection::RightToLeft => max_page.saturating_sub(visual_page),
        ReadingDirection::LeftToRight => visual_page,
    }
}

fn build_page_label_at(language: UiLanguage, state: &ViewerState, view: u32) -> String {
    if state.persistent.page_count == 0 {
        return "…".into();
    }
    let (lp, rp) = state.current_view_pages(view);
    if state.is_leading_cover_blank_spread(view) && state.persistent.page_count > 1 {
        return format_page_count_label(language, "1-2", state.persistent.page_count);
    }
    let pages_str = match (lp, rp) {
        (None, Some(r)) => format!("{}", r + 1),
        (Some(l), None) => format!("{}", l + 1),
        (Some(l), Some(r)) => format!("{}-{}", l + 1, r + 1),
        (None, None) => "…".into(),
    };
    format_page_count_label(language, &pages_str, state.persistent.page_count)
}
