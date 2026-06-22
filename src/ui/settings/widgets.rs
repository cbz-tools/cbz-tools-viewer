use eframe::egui;

use crate::domain::app_settings::UiLanguage;

use super::super::i18n::{tr, TextKey};
use super::super::theme;
use super::SETTINGS_BUTTON_HEIGHT;

pub(super) const SETTINGS_SMALL_BUTTON_SIZE: egui::Vec2 = egui::vec2(72.0, SETTINGS_BUTTON_HEIGHT);
pub(super) const SETTINGS_RESET_BUTTON_SIZE: egui::Vec2 = egui::vec2(56.0, SETTINGS_BUTTON_HEIGHT);
pub(super) const SETTINGS_TOOL_DELETE_BUTTON_SIZE: egui::Vec2 =
    egui::vec2(72.0, SETTINGS_BUTTON_HEIGHT);
pub(super) const PERFORMANCE_SELECT_WIDTH: f32 = 180.0;

pub(super) struct SliderRange {
    pub min: u16,
    pub max: u16,
    pub step: u16,
}

pub(super) struct SliderRowConfig<'a> {
    pub suffix: &'a str,
    pub default: u16,
    pub hover_text: Option<String>,
    pub range: SliderRange,
}

pub(super) fn section_header(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(title)
            .size(theme::FONT_SIZE_LARGE)
            .color(theme::TEXT_MAIN)
            .strong(),
    );
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
}

pub(super) fn setting_block<R>(
    ui: &mut egui::Ui,
    label: &str,
    add_body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let resp = egui::Frame::new()
        .fill(theme::SURFACE_BG)
        .stroke(egui::Stroke::new(1.0, theme::SEPARATOR_WEAK))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_MAIN)
                    .strong(),
            );
            ui.add_space(4.0);
            add_body(ui)
        });
    ui.add_space(8.0);
    resp.inner
}

pub(super) fn subtle_text(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_SUBTLE),
    );
}

pub(super) fn subsection_header(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(title)
            .size(theme::FONT_SIZE_BODY)
            .color(theme::TEXT_MAIN)
            .strong(),
    );
    ui.add_space(4.0);
}

pub(super) fn inline_reset_button(
    ui: &mut egui::Ui,
    language: UiLanguage,
    enabled: bool,
    hover_text: Option<String>,
    on_click: impl FnOnce(),
) {
    ui.add_enabled_ui(enabled, |ui| {
        let mut btn = ui.add(
            egui::Button::new(tr(language, TextKey::StandardLabel))
                .min_size(SETTINGS_RESET_BUTTON_SIZE)
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0, egui::Color32::TRANSPARENT)),
        );
        if let Some(text) = hover_text {
            btn = btn.on_hover_text(text);
        }
        if btn.clicked() {
            on_click();
        }
    });
}

pub(super) fn slider_row_with_reset(
    ui: &mut egui::Ui,
    language: UiLanguage,
    value: &mut u16,
    config: SliderRowConfig<'_>,
) {
    ui.horizontal(|ui| {
        let mut drag_value = *value as i32;
        let drag = egui::DragValue::new(&mut drag_value)
            .range(config.range.min as i32..=config.range.max as i32)
            .speed(config.range.step as f64)
            .suffix(config.suffix);
        if ui.add_sized([92.0, SETTINGS_BUTTON_HEIGHT], drag).changed() {
            let step_i = config.range.step as i32;
            drag_value = ((drag_value + step_i / 2) / step_i) * step_i;
            *value = drag_value.clamp(config.range.min as i32, config.range.max as i32) as u16;
        }

        ui.add_space(8.0);
        ui.spacing_mut().slider_width = 330.0;
        let mut slider_value = *value as f32;
        let changed = ui
            .scope(|ui| {
                ui.visuals_mut().widgets.inactive.bg_fill = theme::PROGRESS_BG;
                ui.visuals_mut().widgets.inactive.bg_stroke =
                    egui::Stroke::new(1.0, theme::SEPARATOR_WEAK);
                ui.visuals_mut().widgets.active.bg_fill = theme::PROGRESS_ACTIVE;
                ui.visuals_mut().widgets.active.bg_stroke =
                    egui::Stroke::new(1.0, theme::PROGRESS_ACTIVE);
                ui.visuals_mut().widgets.hovered.bg_fill = theme::PROGRESS_FILL;
                ui.visuals_mut().widgets.hovered.bg_stroke =
                    egui::Stroke::new(1.0, theme::HOVER_BORDER_WEAK);
                let slider = egui::Slider::new(
                    &mut slider_value,
                    config.range.min as f32..=config.range.max as f32,
                )
                .step_by(config.range.step as f64)
                .show_value(false)
                .trailing_fill(true);
                ui.add(slider).changed()
            })
            .inner;
        if changed {
            *value = slider_value.round() as u16;
        }

        right_reset_button(
            ui,
            language,
            *value != config.default,
            config.hover_text,
            || {
                *value = config.default;
            },
        );
    });
}

pub(super) fn right_reset_button(
    ui: &mut egui::Ui,
    language: UiLanguage,
    enabled: bool,
    hover_text: Option<String>,
    on_click: impl FnOnce(),
) {
    ui.add_space(6.0);
    ui.add_space(ui.available_width().max(0.0));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let mut clicked = false;
        ui.add_enabled_ui(enabled, |ui| {
            let mut btn = ui.add(
                egui::Button::new(tr(language, TextKey::StandardLabel))
                    .min_size(SETTINGS_RESET_BUTTON_SIZE)
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::new(1.0, egui::Color32::TRANSPARENT)),
            );
            if let Some(text) = hover_text {
                btn = btn.on_hover_text(text);
            }
            clicked = btn.clicked();
        });
        if clicked {
            on_click();
        }
    });
}

pub(super) fn combo_row_with_reset<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    language: UiLanguage,
    value: &mut T,
    default: T,
    hover_text: Option<String>,
    add_combo: impl FnOnce(&mut egui::Ui, &mut T),
) {
    ui.horizontal(|ui| {
        add_combo(ui, value);
        right_reset_button(ui, language, *value != default, hover_text, || {
            *value = default;
        });
    });
}

pub(super) fn format_bytes_label(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes == 0 {
        "Unavailable".to_owned()
    } else if bytes % GIB == 0 {
        format!("{} GiB", bytes / GIB)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

pub(super) fn format_mib_label(mib: u16) -> String {
    if mib % 1024 == 0 {
        format!("{} GiB", mib / 1024)
    } else {
        format!("{} MiB", mib)
    }
}
