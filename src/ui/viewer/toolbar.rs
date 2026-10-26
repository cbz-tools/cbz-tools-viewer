use eframe::egui::{self, Key};

use crate::domain::app_settings::{ReadingDirection, UiLanguage, ViewerQuality};
use crate::domain::archive_settings::{SLIDESHOW_INTERVAL_CHOICES, SpreadMode};
use crate::domain::ipc_contract::ViewerFavoriteState;
use crate::ui::common::{DELETE_ICON_DRAW_SIZE, paint_delete_icon, reading_direction_label};
use crate::ui::i18n::{TextKey, tr};

use super::ExternalToolButtonModel;
use super::ExternalToolToolbarState;
use super::ToolbarEvents;
use super::ViewerState;
use super::ViewerUiCapabilities;
use super::icons;
use super::theme;

pub(super) struct ViewerToolbarContext<'a> {
    pub(super) state: &'a ViewerState,
    pub(super) language: UiLanguage,
    pub(super) favorite_state: ViewerFavoriteState,
    pub(super) favorite_toggle_pending: bool,
    pub(super) interaction_blocked: bool,
    pub(super) external_tools: &'a [ExternalToolButtonModel],
    pub(super) external_tool_state: &'a ExternalToolToolbarState,
    pub(super) external_tool_active_counts: &'a [usize],
    pub(super) global_quality: ViewerQuality,
    pub(super) capabilities: ViewerUiCapabilities,
}

const VIEWER_TOOLBAR_ICON_BUTTON_BASE_WIDTH: f32 = 26.0;
const VIEWER_TOOLBAR_ICON_BUTTON_SIZE: egui::Vec2 = egui::vec2(
    VIEWER_TOOLBAR_ICON_BUTTON_BASE_WIDTH + theme::ICON_BUTTON_HOVER_GUARD_X,
    theme::CONTROL_HEIGHT,
);
const VIEWER_FAVORITE_BUTTON_SIZE: egui::Vec2 = egui::vec2(
    VIEWER_TOOLBAR_ICON_BUTTON_BASE_WIDTH + theme::ICON_BUTTON_HOVER_GUARD_X,
    theme::CONTROL_HEIGHT,
);
const VIEWER_PLAY_BUTTON_SIZE: egui::Vec2 = egui::vec2(
    VIEWER_TOOLBAR_ICON_BUTTON_BASE_WIDTH + theme::ICON_BUTTON_HOVER_GUARD_X,
    theme::CONTROL_HEIGHT,
);
const VIEWER_FAVORITE_ICON_SIZE: f32 = 16.0;
const VIEWER_PLAY_ICON_SIZE: f32 = 16.0;
const VIEWER_ACTION_ICON_SIZE: f32 = 16.0;
const VIEWER_TOOLBAR_EXTERNAL_TOOL_BUTTON_SIZE: egui::Vec2 =
    egui::vec2(30.0, theme::CONTROL_HEIGHT);
const VIEWER_TOOLBAR_TEXT_BUTTON_SIZE: egui::Vec2 = egui::vec2(88.0, 24.0);

#[derive(Clone, Copy)]
enum ViewerToolbarIcon {
    FavoriteFilled,
    FavoriteOutline,
    Play,
    Pause,
    Fullscreen,
    Delete,
}

fn paint_viewer_toolbar_icon(
    ui: &egui::Ui,
    button_rect: egui::Rect,
    icon: ViewerToolbarIcon,
    color: egui::Color32,
) {
    let icon_size = match icon {
        ViewerToolbarIcon::FavoriteFilled | ViewerToolbarIcon::FavoriteOutline => {
            VIEWER_FAVORITE_ICON_SIZE
        }
        ViewerToolbarIcon::Play | ViewerToolbarIcon::Pause => VIEWER_PLAY_ICON_SIZE,
        ViewerToolbarIcon::Fullscreen => VIEWER_ACTION_ICON_SIZE,
        ViewerToolbarIcon::Delete => DELETE_ICON_DRAW_SIZE,
    };
    let icon_rect =
        egui::Rect::from_center_size(button_rect.center(), egui::vec2(icon_size, icon_size));
    let painter = ui.painter();

    match icon {
        ViewerToolbarIcon::FavoriteOutline => {
            painter.add(egui::Shape::closed_line(
                star_points(icon_rect).to_vec(),
                egui::Stroke::new(1.5, color),
            ));
        }
        ViewerToolbarIcon::FavoriteFilled => {
            let points = star_points(icon_rect);
            let center = icon_rect.center();
            for tip_index in (0..points.len()).step_by(2) {
                let previous_inner = points[(tip_index + points.len() - 1) % points.len()];
                let next_inner = points[(tip_index + 1) % points.len()];
                painter.add(egui::Shape::convex_polygon(
                    vec![center, previous_inner, points[tip_index], next_inner],
                    color,
                    egui::Stroke::NONE,
                ));
            }
        }
        ViewerToolbarIcon::Play => {
            let center = icon_rect.center();
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(icon_rect.left() + 4.0, icon_rect.top() + 2.5),
                    egui::pos2(icon_rect.right() - 3.0, center.y),
                    egui::pos2(icon_rect.left() + 4.0, icon_rect.bottom() - 2.5),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        ViewerToolbarIcon::Pause => {
            let bar_width = 3.0;
            let bar_gap = 3.0;
            let bar_height = 10.5;
            let left = icon_rect.center().x - (bar_width * 2.0 + bar_gap) * 0.5;
            let top = icon_rect.center().y - bar_height * 0.5;
            for bar_index in 0..2 {
                let bar_left = left + bar_index as f32 * (bar_width + bar_gap);
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(bar_left, top),
                        egui::vec2(bar_width, bar_height),
                    ),
                    egui::CornerRadius::ZERO,
                    color,
                );
            }
        }
        ViewerToolbarIcon::Fullscreen => {
            let inset = 3.0;
            let corner_length = 3.0;
            let left = icon_rect.left() + inset;
            let top = icon_rect.top() + inset;
            let right = icon_rect.right() - inset;
            let bottom = icon_rect.bottom() - inset;
            let stroke = egui::Stroke::new(1.5, color);
            for (start, corner, end) in [
                (
                    egui::pos2(left, top + corner_length),
                    egui::pos2(left, top),
                    egui::pos2(left + corner_length, top),
                ),
                (
                    egui::pos2(right - corner_length, top),
                    egui::pos2(right, top),
                    egui::pos2(right, top + corner_length),
                ),
                (
                    egui::pos2(left, bottom - corner_length),
                    egui::pos2(left, bottom),
                    egui::pos2(left + corner_length, bottom),
                ),
                (
                    egui::pos2(right - corner_length, bottom),
                    egui::pos2(right, bottom),
                    egui::pos2(right, bottom - corner_length),
                ),
            ] {
                painter.line_segment([start, corner], stroke);
                painter.line_segment([corner, end], stroke);
            }
        }
        ViewerToolbarIcon::Delete => {
            paint_delete_icon(painter, icon_rect, color);
        }
    }
}

fn star_points(rect: egui::Rect) -> [egui::Pos2; 10] {
    let center = rect.center();
    let outer_radius = rect.width() * 0.4375;
    let inner_radius = outer_radius * 0.4;
    std::array::from_fn(|index| {
        let angle = -std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::TAU / 10.0;
        let radius = if index % 2 == 0 {
            outer_radius
        } else {
            inner_radius
        };
        egui::pos2(
            center.x + angle.cos() * radius,
            center.y + angle.sin() * radius,
        )
    })
}

fn favorite_button_and_hover(
    language: UiLanguage,
    favorite_state: ViewerFavoriteState,
    favorite_toggle_pending: bool,
) -> (egui::widgets::Button<'static>, &'static str, bool) {
    // ON/OFF は色ではなく形で区別する。Unknown/Disabled は OFF と同じ枠線星を使う。
    let label = egui::Button::new("")
        .fill(if matches!(favorite_state, ViewerFavoriteState::On) {
            theme::BUTTON_ACTIVE
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(if matches!(favorite_state, ViewerFavoriteState::On) {
            egui::Stroke::new(1.0_f32, theme::ACCENT_ACTIVE)
        } else {
            egui::Stroke::new(1.0_f32, egui::Color32::TRANSPARENT)
        });
    let hover = if favorite_toggle_pending {
        tr(language, TextKey::FavoriteUpdating)
    } else {
        match favorite_state {
            ViewerFavoriteState::Unknown => tr(language, TextKey::FavoriteChecking),
            ViewerFavoriteState::On => tr(language, TextKey::RemoveFromFavorites),
            ViewerFavoriteState::Off => tr(language, TextKey::AddToFavorites),
        }
    };
    (label, hover, favorite_toggle_pending)
}

fn favorite_icon_for_state(favorite_state: ViewerFavoriteState) -> ViewerToolbarIcon {
    match favorite_state {
        ViewerFavoriteState::On => ViewerToolbarIcon::FavoriteFilled,
        ViewerFavoriteState::Off | ViewerFavoriteState::Unknown => {
            ViewerToolbarIcon::FavoriteOutline
        }
    }
}

fn format_slideshow_interval_label(secs: f32) -> String {
    if secs.fract().abs() < f32::EPSILON {
        format!("{secs:.0}s")
    } else {
        format!("{secs:.1}s")
    }
}

fn reading_direction_selected_label(
    language: UiLanguage,
    global_reading_direction: ReadingDirection,
    reading_direction_override: Option<ReadingDirection>,
) -> String {
    match reading_direction_override {
        None => format!(
            "{}（{}）",
            tr(language, TextKey::DefaultLabel),
            reading_direction_label(language, global_reading_direction)
        ),
        Some(direction) if direction == global_reading_direction => format!(
            "{}（{}）",
            tr(language, TextKey::DefaultLabel),
            reading_direction_label(language, global_reading_direction)
        ),
        Some(direction) => reading_direction_label(language, direction).to_owned(),
    }
}

fn opposite_reading_direction(direction: ReadingDirection) -> ReadingDirection {
    match direction {
        ReadingDirection::RightToLeft => ReadingDirection::LeftToRight,
        ReadingDirection::LeftToRight => ReadingDirection::RightToLeft,
    }
}

pub(super) fn is_reserved_viewer_key(key: Key) -> bool {
    matches!(
        key,
        Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::PageUp
            | Key::PageDown
            | Key::Home
            | Key::End
            | Key::Delete
            | Key::Space
            | Key::Escape
            | Key::M
            | Key::A
            | Key::D
            | Key::W
            | Key::S
            | Key::F11
    )
}

pub(super) fn render_viewer_toolbar(
    ui: &mut egui::Ui,
    toolbar: ViewerToolbarContext<'_>,
    events: &mut ToolbarEvents,
) {
    let ViewerToolbarContext {
        state,
        language,
        favorite_state,
        favorite_toggle_pending,
        interaction_blocked,
        external_tools,
        external_tool_state,
        external_tool_active_counts,
        global_quality,
        capabilities,
    } = toolbar;
    fn paint_quiet_hover_border(ui: &egui::Ui, resp: &egui::Response) {
        if resp.hovered() {
            ui.painter().rect_stroke(
                resp.rect,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0_f32, theme::HOVER_BORDER_WEAK),
                egui::StrokeKind::Inside,
            );
        }
    }

    let quiet_stroke = egui::Stroke::new(1.0_f32, egui::Color32::TRANSPARENT);
    let selected_stroke = egui::Stroke::new(1.0_f32, theme::ACCENT_ACTIVE);
    let current_book_path = state.persistent.entry.path.as_ref();
    ui.horizontal(|ui| {
        let favorite_icon = favorite_icon_for_state(favorite_state);
        let (favorite_button, favorite_hover, favorite_disabled) =
            favorite_button_and_hover(language, favorite_state, favorite_toggle_pending);
        let favorite_enabled = capabilities.allow_favorite_toggle
            && !interaction_blocked
            && !favorite_disabled
            && !matches!(favorite_state, ViewerFavoriteState::Unknown);
        let favorite_resp = ui
            .add_enabled_ui(favorite_enabled, |ui| {
                ui.add_sized(
                    VIEWER_FAVORITE_BUTTON_SIZE,
                    favorite_button.stroke(if matches!(favorite_state, ViewerFavoriteState::On) {
                        selected_stroke
                    } else {
                        quiet_stroke
                    }),
                )
            })
            .inner;
        let favorite_icon_color = if favorite_enabled {
            ui.visuals().widgets.style(&favorite_resp).text_color()
        } else {
            theme::TEXT_DISABLED
        };
        paint_viewer_toolbar_icon(ui, favorite_resp.rect, favorite_icon, favorite_icon_color);
        let _ = if favorite_enabled {
            favorite_resp.clone().on_hover_text(favorite_hover)
        } else {
            favorite_resp.clone().on_disabled_hover_text(favorite_hover)
        };
        if favorite_resp.clicked() {
            events.toggle_favorite = true;
        }

        let spread_label = match state.persistent.spread_setting {
            SpreadMode::Auto => icons::icon_label(
                ui,
                icons::ICON_AUTO_STORIES,
                15.0,
                tr(language, TextKey::DisplayModeAuto),
            ),
            SpreadMode::Single => icons::icon_label(
                ui,
                icons::ICON_ARTICLE,
                15.0,
                tr(language, TextKey::DisplayModeSingle),
            ),
            SpreadMode::Spread => icons::icon_label(
                ui,
                icons::ICON_BOOK_2,
                15.0,
                tr(language, TextKey::DisplayModeSpread),
            ),
        };
        let spread_resp = ui
            .add_enabled_ui(!interaction_blocked, |ui| {
                ui.add_sized(
                    VIEWER_TOOLBAR_TEXT_BUTTON_SIZE,
                    egui::Button::new(spread_label)
                        .fill(match state.persistent.spread_setting {
                            SpreadMode::Auto => theme::ACCENT.linear_multiply(0.2),
                            _ => theme::BUTTON_ACTIVE,
                        })
                        .stroke(selected_stroke),
                )
            })
            .inner
            .on_hover_text(format!(
                "{}: {}",
                tr(language, TextKey::DisplayMode),
                tr(
                    language,
                    match state.persistent.spread_setting {
                        SpreadMode::Auto => TextKey::DisplayModeAuto,
                        SpreadMode::Single => TextKey::DisplayModeSingle,
                        SpreadMode::Spread => TextKey::DisplayModeSpread,
                    }
                )
            ));
        if spread_resp.clicked() {
            events.toggle_spread = true;
        }

        let global_reading_direction = state.persistent.global_reading_direction;
        let opposite_direction = opposite_reading_direction(global_reading_direction);
        let reading_direction_selected = reading_direction_selected_label(
            language,
            global_reading_direction,
            state.persistent.reading_direction_override,
        );
        ui.add_enabled_ui(!interaction_blocked, |ui| {
            egui::ComboBox::from_id_salt("viewer_reading_direction_override_combo")
                .selected_text(reading_direction_selected)
                .width(128.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            state.persistent.reading_direction_override.is_none()
                                || state.persistent.reading_direction_override
                                    == Some(global_reading_direction),
                            format!(
                                "{}（{}）",
                                tr(language, TextKey::DefaultLabel),
                                reading_direction_label(language, global_reading_direction)
                            ),
                        )
                        .clicked()
                    {
                        events.reading_direction_override_change = Some(None);
                    }
                    if ui
                        .selectable_label(
                            state.persistent.reading_direction_override == Some(opposite_direction),
                            reading_direction_label(language, opposite_direction),
                        )
                        .clicked()
                    {
                        events.reading_direction_override_change = Some(Some(opposite_direction));
                    }
                })
                .response
                .on_hover_text(tr(language, TextKey::ReadingDirectionTooltip));
        });

        let blank_label = tr(language, TextKey::CoverBlank).to_owned();
        let blank_enabled = matches!(
            state.persistent.spread_setting,
            SpreadMode::Auto | SpreadMode::Spread
        ) && !interaction_blocked;
        let blank_resp = ui
            .add_enabled_ui(blank_enabled, |ui| {
                ui.add_sized(
                    VIEWER_TOOLBAR_TEXT_BUTTON_SIZE,
                    egui::Button::new(
                        egui::RichText::new(blank_label).size(theme::FONT_SIZE_SMALL),
                    )
                    .fill(if state.persistent.cover_blank {
                        theme::BUTTON_ACTIVE
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .stroke(if state.persistent.cover_blank {
                        selected_stroke
                    } else {
                        quiet_stroke
                    }),
                )
            })
            .inner;
        let _ = blank_resp.clone().on_hover_text(
            if matches!(state.persistent.spread_setting, SpreadMode::Auto) {
                tr(language, TextKey::CoverBlankAutoHint)
            } else {
                tr(language, TextKey::CoverBlankHint)
            },
        );
        if blank_enabled && !state.persistent.cover_blank {
            paint_quiet_hover_border(ui, &blank_resp);
        }
        if !matches!(
            state.persistent.spread_setting,
            SpreadMode::Auto | SpreadMode::Spread
        ) {
            let _ = blank_resp
                .clone()
                .on_disabled_hover_text(tr(language, TextKey::CoverBlankHint));
        }
        if blank_enabled && blank_resp.clicked() {
            events.toggle_cover_blank = true;
        }

        let override_quality = state.persistent.quality_override;
        let _effective_quality = override_quality.unwrap_or(global_quality);
        let selected_label = match state.persistent.quality_override.as_ref() {
            None => tr(language, TextKey::QualityLabel).to_owned(),
            Some(ViewerQuality::Speed) => tr(language, TextKey::QualitySpeed).to_owned(),
            Some(ViewerQuality::Balanced) => tr(language, TextKey::QualityBalanced).to_owned(),
            Some(ViewerQuality::Quality) => tr(language, TextKey::QualityQuality).to_owned(),
            Some(ViewerQuality::Original) => tr(language, TextKey::QualityOriginal).to_owned(),
        };
        let selected_text = egui::RichText::new(selected_label);
        ui.add_enabled_ui(!interaction_blocked, |ui| {
            egui::ComboBox::from_id_salt("viewer_quality_override_combo")
                .selected_text(selected_text)
                .width(96.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            state.persistent.quality_override.is_none(),
                            tr(language, TextKey::QualityLabel),
                        )
                        .clicked()
                    {
                        events.quality_override_change = Some(None);
                    }
                    if ui
                        .selectable_label(
                            override_quality == Some(ViewerQuality::Speed),
                            tr(language, TextKey::QualitySpeed),
                        )
                        .clicked()
                    {
                        events.quality_override_change = Some(Some(ViewerQuality::Speed));
                    }
                    if ui
                        .selectable_label(
                            override_quality == Some(ViewerQuality::Balanced),
                            tr(language, TextKey::QualityBalanced),
                        )
                        .clicked()
                    {
                        events.quality_override_change = Some(Some(ViewerQuality::Balanced));
                    }
                    if ui
                        .selectable_label(
                            override_quality == Some(ViewerQuality::Quality),
                            tr(language, TextKey::QualityQuality),
                        )
                        .clicked()
                    {
                        events.quality_override_change = Some(Some(ViewerQuality::Quality));
                    }
                    if ui
                        .selectable_label(
                            override_quality == Some(ViewerQuality::Original),
                            tr(language, TextKey::QualityOriginal),
                        )
                        .clicked()
                    {
                        events.quality_override_change = Some(Some(ViewerQuality::Original));
                    }
                });
        });

        ui.separator();
        let play_icon = if state.slideshow_active() {
            ViewerToolbarIcon::Pause
        } else {
            ViewerToolbarIcon::Play
        };
        let play_resp = ui
            .add_enabled_ui(!interaction_blocked, |ui| {
                ui.add_sized(
                    VIEWER_PLAY_BUTTON_SIZE,
                    egui::Button::new("")
                        .fill(if state.slideshow_active() {
                            theme::BUTTON_ACTIVE
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .stroke(if state.slideshow_active() {
                            selected_stroke
                        } else {
                            quiet_stroke
                        }),
                )
            })
            .inner
            .on_hover_text(if state.slideshow_active() {
                tr(language, TextKey::SlideshowPause)
            } else {
                tr(language, TextKey::SlideshowPlay)
            });
        let play_icon_color = if interaction_blocked {
            theme::TEXT_DISABLED
        } else {
            ui.visuals().widgets.style(&play_resp).text_color()
        };
        paint_viewer_toolbar_icon(ui, play_resp.rect, play_icon, play_icon_color);
        if !state.slideshow_active() {
            paint_quiet_hover_border(ui, &play_resp);
        }
        if play_resp.clicked() {
            events.toggle_slideshow = true;
        }
        egui::ComboBox::from_id_salt("viewer_slideshow_interval_combo")
            .selected_text(format_slideshow_interval_label(
                state.slideshow_interval_secs(),
            ))
            .width(72.0)
            .show_ui(ui, |ui| {
                for value in SLIDESHOW_INTERVAL_CHOICES {
                    let label = format_slideshow_interval_label(value);
                    if ui
                        .selectable_label(value == state.slideshow_interval_secs(), label)
                        .clicked()
                    {
                        events.interval_change = Some(value);
                    }
                }
            });

        if !external_tools.is_empty() {
            ui.separator();
            let all_disabled = interaction_blocked;
            for tool in external_tools {
                let active_count = external_tool_active_counts
                    .get(tool.tool_index)
                    .copied()
                    .unwrap_or(0);
                let button_state = if active_count >= 2 {
                    theme::ExternalToolButtonState::Running
                } else {
                    match external_tool_state {
                        ExternalToolToolbarState::Success { tool_index, path }
                            if *tool_index == tool.tool_index
                                && path.as_path() == current_book_path =>
                        {
                            theme::ExternalToolButtonState::Success
                        }
                        ExternalToolToolbarState::Failed { tool_index, path }
                            if *tool_index == tool.tool_index
                                && path.as_path() == current_book_path =>
                        {
                            theme::ExternalToolButtonState::Failed
                        }
                        ExternalToolToolbarState::Running { tool_index, path }
                            if *tool_index == tool.tool_index
                                && path.as_path() == current_book_path =>
                        {
                            theme::ExternalToolButtonState::Running
                        }
                        _ => theme::ExternalToolButtonState::Idle,
                    }
                };
                let label = match active_count {
                    0 | 1 => tool.shortcut.to_string(),
                    2..=9 => active_count.to_string(),
                    _ => "9+".to_owned(),
                };
                let fill = theme::external_tool_button_bg(button_state);
                let resp = ui
                    .add_enabled_ui(!all_disabled, |ui| {
                        ui.add_sized(
                            VIEWER_TOOLBAR_EXTERNAL_TOOL_BUTTON_SIZE,
                            egui::Button::new(
                                egui::RichText::new(label)
                                    .size(theme::FONT_SIZE_BODY)
                                    .color(theme::TEXT_MAIN),
                            )
                            .fill(fill)
                            .stroke(quiet_stroke),
                        )
                    })
                    .inner
                    .on_hover_text(&tool.name);
                if button_state == theme::ExternalToolButtonState::Idle {
                    paint_quiet_hover_border(ui, &resp);
                }
                if resp.clicked() {
                    events.external_tool_click = Some(tool.tool_index);
                }
            }
        }

        ui.separator();
        let book_file_name = state
            .persistent
            .entry
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| state.persistent.entry.title.to_string());
        let title_text = |text: &str| {
            egui::RichText::new(text)
                .size(theme::FONT_SIZE_BODY)
                .color(theme::TEXT_MAIN)
                .strong()
        };
        let blank_label = tr(language, TextKey::BlankPage);
        if let Some(current_image_title) = state.current_toolbar_title(blank_label) {
            ui.label(title_text(&book_file_name));
            ui.add_space(28.0);
            ui.label(title_text("|"));
            ui.add_space(28.0);
            ui.label(title_text(&current_image_title));
        } else {
            ui.label(title_text(&book_file_name));
        }

        ui.add_space(ui.available_width().max(0.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let fullscreen_resp = ui
                .add_enabled_ui(!interaction_blocked, |ui| {
                    ui.add_sized(
                        VIEWER_TOOLBAR_ICON_BUTTON_SIZE,
                        egui::Button::new("")
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(quiet_stroke),
                    )
                })
                .inner
                .on_hover_text(tr(language, TextKey::Fullscreen));
            let fullscreen_icon_color = if interaction_blocked {
                theme::TEXT_DISABLED
            } else {
                ui.visuals().widgets.style(&fullscreen_resp).text_color()
            };
            paint_viewer_toolbar_icon(
                ui,
                fullscreen_resp.rect,
                ViewerToolbarIcon::Fullscreen,
                fullscreen_icon_color,
            );
            paint_quiet_hover_border(ui, &fullscreen_resp);
            if fullscreen_resp.clicked() {
                events.toggle_fullscreen = true;
            }

            if capabilities.allow_delete {
                let delete_resp = ui
                    .add_enabled_ui(!interaction_blocked, |ui| {
                        ui.add_sized(
                            VIEWER_TOOLBAR_ICON_BUTTON_SIZE,
                            egui::Button::new("")
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(quiet_stroke),
                        )
                    })
                    .inner
                    .on_hover_text(tr(language, TextKey::Delete));
                paint_viewer_toolbar_icon(
                    ui,
                    delete_resp.rect,
                    ViewerToolbarIcon::Delete,
                    theme::DELETE_RED,
                );
                paint_quiet_hover_border(ui, &delete_resp);
                if delete_resp.clicked() {
                    events.delete = true;
                }
            }
        });
    });
}
