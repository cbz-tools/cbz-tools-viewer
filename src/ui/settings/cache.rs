use eframe::egui;

use crate::domain::app_settings::{
    AppSettings, LIBRARY_HUD_FONT_LEVEL_DEFAULT, LIBRARY_HUD_FONT_LEVEL_MAX,
    LIBRARY_HUD_FONT_LEVEL_MIN, LIBRARY_WHEEL_SPEED_DEFAULT, LIBRARY_WHEEL_SPEED_MAX,
    LIBRARY_WHEEL_SPEED_MIN, THUMB_DISPLAY_DEFAULT, THUMB_DISPLAY_MAX, THUMB_DISPLAY_MIN,
    THUMB_DISPLAY_STEP,
};
use crate::domain::app_settings::{LibraryCardSelectionStyle, LibraryHudStyle, UiLanguage};

use super::super::i18n::{tr, TextKey};
use super::widgets::{
    combo_row_with_reset, section_header, setting_block, slider_row_with_reset, subtle_text,
    SliderRange, SliderRowConfig,
};
use super::SettingsEvent;

pub(super) fn show_library_tab(
    ui: &mut egui::Ui,
    language: UiLanguage,
    settings: &mut AppSettings,
    event: &mut SettingsEvent,
) {
    section_header(ui, tr(language, TextKey::List));
    setting_block(ui, tr(language, TextKey::Size), |ui| {
        let old_w = settings.thumb_display_w;
        slider_row_with_reset(
            ui,
            language,
            &mut settings.thumb_display_w,
            SliderRowConfig {
                suffix: " px",
                default: THUMB_DISPLAY_DEFAULT,
                hover_text: Some(format!(
                    "{} ({}px)",
                    tr(language, TextKey::BackToDefaultSize),
                    THUMB_DISPLAY_DEFAULT
                )),
                range: SliderRange {
                    min: THUMB_DISPLAY_MIN,
                    max: THUMB_DISPLAY_MAX,
                    step: THUMB_DISPLAY_STEP,
                },
            },
        );
        ui.horizontal(|ui| {
            ui.add_space(108.0);
            subtle_text(ui, tr(language, TextKey::SizeMin));
            ui.add_space(12.0);
            subtle_text(ui, tr(language, TextKey::SizeDefault));
            ui.add_space(12.0);
            subtle_text(ui, tr(language, TextKey::SizeMax));
        });
        subtle_text(ui, tr(language, TextKey::SizeRealtimeNote));
        if settings.thumb_display_w != old_w {
            *event = SettingsEvent::ThumbSizeChanged;
        }
    });

    setting_block(ui, tr(language, TextKey::WheelSpeed), |ui| {
        slider_row_with_reset(
            ui,
            language,
            &mut settings.library_wheel_speed,
            SliderRowConfig {
                suffix: "",
                default: LIBRARY_WHEEL_SPEED_DEFAULT,
                hover_text: Some(
                    tr(language, TextKey::WheelSpeedReset)
                        .replacen("{}", &LIBRARY_WHEEL_SPEED_DEFAULT.to_string(), 1)
                        .replacen("{}", "4.0", 1),
                ),
                range: SliderRange {
                    min: LIBRARY_WHEEL_SPEED_MIN,
                    max: LIBRARY_WHEEL_SPEED_MAX,
                    step: 1,
                },
            },
        );
        subtle_text(
            ui,
            &tr(language, TextKey::WheelSpeedDescription)
                .replacen("{}", &settings.clamped_library_wheel_speed().to_string(), 1)
                .replacen(
                    "{}",
                    &format!("{:.1}", settings.library_wheel_multiplier()),
                    1,
                ),
        );
    });

    section_header(ui, tr(language, TextKey::CardDisplay));

    setting_block(ui, tr(language, TextKey::HudText), |ui| {
        slider_row_with_reset(
            ui,
            language,
            &mut settings.library_hud_font_level,
            SliderRowConfig {
                suffix: "",
                default: LIBRARY_HUD_FONT_LEVEL_DEFAULT,
                hover_text: Some(
                    tr(language, TextKey::HudTextReset)
                        .replacen("{}", &LIBRARY_HUD_FONT_LEVEL_DEFAULT.to_string(), 1)
                        .replacen("{}", &format!("{:.0}", 12.0), 1),
                ),
                range: SliderRange {
                    min: LIBRARY_HUD_FONT_LEVEL_MIN,
                    max: LIBRARY_HUD_FONT_LEVEL_MAX,
                    step: 1,
                },
            },
        );
        subtle_text(
            ui,
            &tr(language, TextKey::HudTextDescription)
                .replacen(
                    "{}",
                    &settings.clamped_library_hud_font_level().to_string(),
                    1,
                )
                .replacen("{}", &format!("{:.0}", settings.library_hud_font_size()), 1),
        );
    });

    setting_block(ui, tr(language, TextKey::HudStyle), |ui| {
        combo_row_with_reset(
            ui,
            language,
            &mut settings.library_hud_style,
            crate::domain::app_settings::LIBRARY_HUD_STYLE_DEFAULT,
            None,
            |ui, value| {
                egui::ComboBox::from_id_salt("library_card_hud_style")
                    .selected_text(hud_style_label(language, *value))
                    .show_ui(ui, |ui| {
                        for &style in LibraryHudStyle::all() {
                            ui.selectable_value(value, style, hud_style_label(language, style));
                        }
                    });
            },
        );
    });

    setting_block(ui, tr(language, TextKey::CardSelectionStyle), |ui| {
        combo_row_with_reset(
            ui,
            language,
            &mut settings.library_card_selection_style,
            crate::domain::app_settings::LIBRARY_CARD_SELECTION_STYLE_DEFAULT,
            None,
            |ui, value| {
                egui::ComboBox::from_id_salt("library_card_selection_style")
                    .selected_text(selection_style_label(language, *value))
                    .show_ui(ui, |ui| {
                        for &style in LibraryCardSelectionStyle::all() {
                            ui.selectable_value(
                                value,
                                style,
                                selection_style_label(language, style),
                            );
                        }
                    });
            },
        );
    });

    section_header(ui, tr(language, TextKey::ImageFolder));
    ui.checkbox(
        &mut settings.folder_book_open_as_viewer,
        tr(language, TextKey::ImageFolderOpenAsBook),
    );
    subtle_text(ui, tr(language, TextKey::ImageFolderDescription));
}

fn hud_style_label(language: UiLanguage, style: LibraryHudStyle) -> &'static str {
    match style {
        LibraryHudStyle::Default => tr(language, TextKey::HudStyleDefault),
        LibraryHudStyle::White => tr(language, TextKey::HudStyleWhite),
        LibraryHudStyle::Blue => tr(language, TextKey::HudStyleBlue),
        LibraryHudStyle::Amber => tr(language, TextKey::HudStyleAmber),
        LibraryHudStyle::Rose => tr(language, TextKey::HudStyleRose),
        LibraryHudStyle::Violet => tr(language, TextKey::HudStyleViolet),
        LibraryHudStyle::HighContrast => tr(language, TextKey::HudStyleHighContrast),
    }
}

fn selection_style_label(language: UiLanguage, style: LibraryCardSelectionStyle) -> &'static str {
    match style {
        LibraryCardSelectionStyle::Default => tr(language, TextKey::DefaultLabel),
        LibraryCardSelectionStyle::Violet => tr(language, TextKey::CardSelectionStyleViolet),
        LibraryCardSelectionStyle::Amber => tr(language, TextKey::CardSelectionStyleAmber),
        LibraryCardSelectionStyle::Rose => tr(language, TextKey::CardSelectionStyleRose),
        LibraryCardSelectionStyle::HighContrast => {
            tr(language, TextKey::CardSelectionStyleHighContrast)
        }
    }
}
