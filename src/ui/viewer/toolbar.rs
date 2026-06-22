use eframe::egui::{self, Key};
use egui_material_icons::MaterialIcon;

use crate::domain::app_settings::{ReadingDirection, UiLanguage, ViewerQuality};
use crate::domain::archive_settings::{SpreadMode, SLIDESHOW_INTERVAL_CHOICES};
use crate::infra::ipc::ViewerFavoriteState;
use crate::ui::common::reading_direction_label;
use crate::ui::i18n::{tr, TextKey};

use super::icons;
use super::theme;
use super::ExternalToolButtonModel;
use super::ExternalToolToolbarState;
use super::ToolbarEvents;
use super::ViewerState;
use super::ViewerUiCapabilities;

pub(super) struct ViewerToolbarContext<'a> {
    pub(super) state: &'a ViewerState,
    pub(super) language: UiLanguage,
    pub(super) favorite_state: ViewerFavoriteState,
    pub(super) favorite_toggle_pending: bool,
    pub(super) interaction_blocked: bool,
    pub(super) external_tools: &'a [ExternalToolButtonModel],
    pub(super) external_tool_state: &'a ExternalToolToolbarState,
    pub(super) global_quality: ViewerQuality,
    pub(super) capabilities: ViewerUiCapabilities,
}

const VIEWER_TOOLBAR_ICON_BUTTON_SIZE: egui::Vec2 = egui::vec2(
    26.0 + theme::ICON_BUTTON_HOVER_GUARD_X,
    theme::CONTROL_HEIGHT,
);
const FAVORITE_BUTTON_BASE_WIDTH: f32 = 26.0;
const FAVORITE_BUTTON_SIZE: egui::Vec2 = egui::vec2(
    FAVORITE_BUTTON_BASE_WIDTH + theme::ICON_BUTTON_HOVER_GUARD_X,
    theme::CONTROL_HEIGHT,
);
const VIEWER_TOOLBAR_TEXT_BUTTON_SIZE: egui::Vec2 = egui::vec2(88.0, 24.0);
const FAVORITE_ICON_SIZE: f32 = 17.0;

fn favorite_button_and_hover(
    language: UiLanguage,
    favorite_state: ViewerFavoriteState,
    favorite_toggle_pending: bool,
) -> (egui::widgets::Button<'static>, &'static str, bool) {
    // ON/OFF は色ではなく形で区別する。Unknown/Disabled は OFF と同じ枠線星を使う。
    let icon = favorite_icon_for_state(favorite_state);
    let label = egui::Button::new(icons::icon(icon, FAVORITE_ICON_SIZE))
        .min_size(FAVORITE_BUTTON_SIZE)
        .fill(if matches!(favorite_state, ViewerFavoriteState::On) {
            theme::BUTTON_ACTIVE
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(if matches!(favorite_state, ViewerFavoriteState::On) {
            egui::Stroke::new(1.0, theme::ACCENT_ACTIVE)
        } else {
            egui::Stroke::new(1.0, egui::Color32::TRANSPARENT)
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

fn favorite_icon_for_state(favorite_state: ViewerFavoriteState) -> MaterialIcon {
    match favorite_state {
        ViewerFavoriteState::On => icons::ICON_STAR,
        ViewerFavoriteState::Off | ViewerFavoriteState::Unknown => {
            icons::ICON_STAR_BORDER.outlined()
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
        global_quality,
        capabilities,
    } = toolbar;
    fn paint_quiet_hover_border(ui: &egui::Ui, resp: &egui::Response) {
        if resp.hovered() {
            ui.painter().rect_stroke(
                resp.rect,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0, theme::HOVER_BORDER_WEAK),
                egui::StrokeKind::Inside,
            );
        }
    }

    let quiet_stroke = egui::Stroke::new(1.0, egui::Color32::TRANSPARENT);
    let selected_stroke = egui::Stroke::new(1.0, theme::ACCENT_ACTIVE);
    let current_book_path = state.persistent.entry.path.as_ref();
    ui.horizontal(|ui| {
        let (favorite_button, favorite_hover, favorite_disabled) =
            favorite_button_and_hover(language, favorite_state, favorite_toggle_pending);
        let favorite_enabled = capabilities.allow_favorite_toggle
            && !interaction_blocked
            && !favorite_disabled
            && !matches!(favorite_state, ViewerFavoriteState::Unknown);
        let favorite_resp = ui.add_enabled(
            favorite_enabled,
            favorite_button.stroke(if matches!(favorite_state, ViewerFavoriteState::On) {
                selected_stroke
            } else {
                quiet_stroke
            }),
        );
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
            .add_enabled(
                !interaction_blocked,
                egui::Button::new(spread_label)
                    .min_size(VIEWER_TOOLBAR_TEXT_BUTTON_SIZE)
                    .fill(match state.persistent.spread_setting {
                        SpreadMode::Auto => theme::ACCENT.linear_multiply(0.2),
                        _ => theme::BUTTON_ACTIVE,
                    })
                    .stroke(selected_stroke),
            )
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

        let blank_label = if state.persistent.cover_blank {
            format!("{} ✓", tr(language, TextKey::CoverBlank))
        } else {
            tr(language, TextKey::CoverBlank).to_owned()
        };
        let blank_enabled = matches!(
            state.persistent.spread_setting,
            SpreadMode::Auto | SpreadMode::Spread
        ) && !interaction_blocked;
        let blank_resp = ui.add_enabled(
            blank_enabled,
            egui::Button::new(egui::RichText::new(blank_label).size(theme::FONT_SIZE_SMALL))
                .min_size(VIEWER_TOOLBAR_TEXT_BUTTON_SIZE)
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
        );
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
            icons::ICON_PAUSE
        } else {
            icons::ICON_PLAY_ARROW
        };
        let play_resp = ui
            .add_enabled(
                !interaction_blocked,
                egui::Button::new(icons::icon(play_icon, 17.0))
                    .min_size(VIEWER_TOOLBAR_ICON_BUTTON_SIZE)
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
            .on_hover_text(if state.slideshow_active() {
                tr(language, TextKey::SlideshowPause)
            } else {
                tr(language, TextKey::SlideshowPlay)
            });
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
            let all_disabled = interaction_blocked
                || matches!(
                    external_tool_state,
                    ExternalToolToolbarState::Running { .. }
                );
            if let ExternalToolToolbarState::Running {
                tool_index, path, ..
            } = external_tool_state
            {
                let _ = (tool_index, path);
            }
            for tool in external_tools {
                let mut button_state = theme::ExternalToolButtonState::Idle;
                match external_tool_state {
                    ExternalToolToolbarState::Success { tool_index, path }
                        if *tool_index == tool.tool_index
                            && path.as_path() == current_book_path =>
                    {
                        button_state = theme::ExternalToolButtonState::Success;
                    }
                    ExternalToolToolbarState::Failed { tool_index, path }
                        if *tool_index == tool.tool_index
                            && path.as_path() == current_book_path =>
                    {
                        button_state = theme::ExternalToolButtonState::Failed;
                    }
                    ExternalToolToolbarState::Running { .. } => {
                        button_state = theme::ExternalToolButtonState::Running;
                    }
                    _ => {}
                }
                let fill = theme::external_tool_button_bg(button_state);
                let resp = ui
                    .add_enabled_ui(!all_disabled, |ui| {
                        ui.add_sized(
                            VIEWER_TOOLBAR_ICON_BUTTON_SIZE,
                            egui::Button::new(
                                egui::RichText::new(tool.shortcut.to_string())
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
        let viewer_title = state
            .persistent
            .entry
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&state.persistent.entry.title);
        ui.label(
            egui::RichText::new(viewer_title)
                .size(theme::FONT_SIZE_BODY)
                .color(theme::TEXT_MAIN)
                .strong(),
        );

        ui.add_space(ui.available_width().max(0.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let fullscreen_resp = ui
                .add_enabled(
                    !interaction_blocked,
                    egui::Button::new(icons::icon(icons::ICON_FULLSCREEN, 17.0))
                        .min_size(VIEWER_TOOLBAR_ICON_BUTTON_SIZE)
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(quiet_stroke),
                )
                .on_hover_text(tr(language, TextKey::Fullscreen));
            paint_quiet_hover_border(ui, &fullscreen_resp);
            if fullscreen_resp.clicked() {
                events.toggle_fullscreen = true;
            }

            if capabilities.allow_delete {
                let delete_resp = ui
                    .add_enabled(
                        !interaction_blocked,
                        egui::Button::new(
                            icons::icon(icons::ICON_DELETE, 17.0).color(theme::DELETE_RED),
                        )
                        .min_size(VIEWER_TOOLBAR_ICON_BUTTON_SIZE)
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(quiet_stroke),
                    )
                    .on_hover_text(tr(language, TextKey::Delete));
                paint_quiet_hover_border(ui, &delete_resp);
                if delete_resp.clicked() {
                    events.delete = true;
                }
            }
        });
    });
}
