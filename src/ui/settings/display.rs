use eframe::egui;

use crate::domain::app_settings::UiLanguage;
use crate::domain::app_settings::{AppSettings, ReadingDirection, ViewerQuality};

use super::super::common::reading_direction_label;
use super::super::i18n::{tr, ui_language_choice_key, TextKey};
use super::widgets::{section_header, setting_block, subtle_text};

pub(super) fn show_general_tab(
    ui: &mut egui::Ui,
    language: UiLanguage,
    settings: &mut AppSettings,
) {
    section_header(ui, tr(language, TextKey::App));
    setting_block(ui, tr(language, TextKey::Language), |ui| {
        ui.horizontal(|ui| {
            ui.set_height(26.0);
            egui::ComboBox::from_id_salt("ui_language")
                .selected_text(tr(language, ui_language_choice_key(settings.ui_language)))
                .width(220.0)
                .show_ui(ui, |ui| {
                    for &candidate in UiLanguage::all() {
                        ui.selectable_value(
                            &mut settings.ui_language,
                            candidate,
                            tr(language, ui_language_choice_key(candidate)),
                        );
                    }
                });
        });
    });
}

pub(super) fn show_viewer_tab(ui: &mut egui::Ui, language: UiLanguage, settings: &mut AppSettings) {
    section_header(ui, tr(language, TextKey::Display));
    setting_block(ui, tr(language, TextKey::QualityGlobal), |ui| {
        ui.horizontal(|ui| {
            ui.set_height(26.0);
            egui::ComboBox::from_id_salt("viewer_quality")
                .selected_text(match settings.viewer_quality {
                    ViewerQuality::Speed => tr(language, TextKey::QualitySpeed),
                    ViewerQuality::Balanced => tr(language, TextKey::QualityBalanced),
                    ViewerQuality::Quality => tr(language, TextKey::QualityQuality),
                    ViewerQuality::Original => tr(language, TextKey::QualityOriginal),
                })
                .width(220.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut settings.viewer_quality,
                        ViewerQuality::Speed,
                        tr(language, TextKey::QualitySpeed),
                    );
                    ui.selectable_value(
                        &mut settings.viewer_quality,
                        ViewerQuality::Balanced,
                        tr(language, TextKey::QualityBalanced),
                    );
                    ui.selectable_value(
                        &mut settings.viewer_quality,
                        ViewerQuality::Quality,
                        tr(language, TextKey::QualityQuality),
                    );
                    ui.selectable_value(
                        &mut settings.viewer_quality,
                        ViewerQuality::Original,
                        tr(language, TextKey::QualityOriginal),
                    );
                });
        });
        let quality_desc = match settings.viewer_quality {
            ViewerQuality::Speed => tr(language, TextKey::QualitySpeedDesc),
            ViewerQuality::Balanced => tr(language, TextKey::QualityBalancedDesc),
            ViewerQuality::Quality => tr(language, TextKey::QualityQualityDesc),
            ViewerQuality::Original => tr(language, TextKey::QualityOriginalDesc),
        };
        subtle_text(ui, quality_desc);
        subtle_text(ui, tr(language, TextKey::QualityByBookNote));
        subtle_text(ui, tr(language, TextKey::QualityAnimationNote));
    });

    section_header(ui, tr(language, TextKey::ReadingSection));
    setting_block(ui, tr(language, TextKey::DefaultReadingDirection), |ui| {
        ui.horizontal(|ui| {
            ui.set_height(26.0);
            egui::ComboBox::from_id_salt("reading_direction_default")
                .selected_text(reading_direction_label(
                    language,
                    settings.reading_direction,
                ))
                .width(220.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut settings.reading_direction,
                        ReadingDirection::RightToLeft,
                        tr(language, TextKey::RightOpen),
                    );
                    ui.selectable_value(
                        &mut settings.reading_direction,
                        ReadingDirection::LeftToRight,
                        tr(language, TextKey::LeftOpen),
                    );
                });
        });
        subtle_text(ui, tr(language, TextKey::DefaultReadingDirectionNote));
    });

    ui.checkbox(
        &mut settings.resume_from_last_reading_position,
        tr(language, TextKey::ResumeFromLastReadingPosition),
    );
}
