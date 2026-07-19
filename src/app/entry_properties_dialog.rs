use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use eframe::egui::{self, Key};

use super::EntryProperties;
use super::ui_helpers::{DialogButtonSpec, dialog_button_row};
use crate::domain::app_settings::UiLanguage;
use crate::ui::{
    i18n::{TextKey, tr},
    theme,
};

const ENTRY_PROPERTIES_DIALOG_W: f32 = 500.0;
const ENTRY_PROPERTY_LABEL_W: f32 = 72.0;
const ENTRY_PROPERTY_VALUE_W: f32 = 280.0;
// Keep rendered Label lines safely narrower than the fixed value cell.
// A natural-width egui Label can otherwise request a little more width than
// the measured text width and push the following action cell.
const ENTRY_PROPERTY_TEXT_SAFE_MARGIN: f32 = 36.0;
const ENTRY_PROPERTY_ACTION_W: f32 = 74.0;
const ENTRY_PROPERTY_CELL_GAP: f32 = 8.0;
const ENTRY_PROPERTY_ROW_GAP: f32 = 2.0;
const ENTRY_PROPERTY_MULTILINE_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryPropertyRowHeight {
    Single,
    ThreeLines,
}

#[derive(Clone, Debug)]
struct EntryPropertyRow {
    label: String,
    value: String,
    copy_label: Option<String>,
    height: EntryPropertyRowHeight,
}

pub(super) enum EntryPropertiesAction {
    CopyText(String),
    Close,
}

pub(super) fn render(
    ctx: &egui::Context,
    ui_language: UiLanguage,
    props: &EntryProperties,
) -> Vec<EntryPropertiesAction> {
    let mut open = true;
    let mut close_requested = false;
    let mut actions = Vec::new();

    egui::Window::new(tr(ui_language, TextKey::PropertiesTitle))
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .min_width(ENTRY_PROPERTIES_DIALOG_W)
        .max_width(ENTRY_PROPERTIES_DIALOG_W)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(ENTRY_PROPERTIES_DIALOG_W);
            ui.set_max_width(ENTRY_PROPERTIES_DIALOG_W);
            egui::Frame::new()
                .fill(theme::SURFACE_BG)
                .inner_margin(egui::Margin::symmetric(22, 20))
                .corner_radius(egui::CornerRadius::same(7))
                .show(ui, |ui| {
                    ui.set_min_width(entry_property_grid_width());
                    ui.set_max_width(entry_property_grid_width());

                    let mut rows = vec![
                        EntryPropertyRow {
                            label: tr(ui_language, TextKey::NameLabel).to_owned(),
                            value: props.name.clone(),
                            copy_label: Some(tr(ui_language, TextKey::Copy).to_owned()),
                            height: EntryPropertyRowHeight::ThreeLines,
                        },
                        EntryPropertyRow {
                            label: tr(ui_language, TextKey::PathLabel).to_owned(),
                            value: props.path.clone(),
                            copy_label: Some(tr(ui_language, TextKey::Copy).to_owned()),
                            height: EntryPropertyRowHeight::ThreeLines,
                        },
                        EntryPropertyRow {
                            label: tr(ui_language, TextKey::TypeLabel).to_owned(),
                            value: props.kind.clone(),
                            copy_label: None,
                            height: EntryPropertyRowHeight::Single,
                        },
                    ];

                    if let Some(size_bytes) = props.size_bytes {
                        rows.push(EntryPropertyRow {
                            label: tr(ui_language, TextKey::SizeLabel).to_owned(),
                            value: format_entry_info_file_size(size_bytes),
                            copy_label: None,
                            height: EntryPropertyRowHeight::Single,
                        });
                    }
                    if let Some(modified) = props.modified {
                        rows.push(EntryPropertyRow {
                            label: tr(ui_language, TextKey::ModifiedAt).to_owned(),
                            value: format_entry_info_modified(modified),
                            copy_label: None,
                            height: EntryPropertyRowHeight::Single,
                        });
                    }
                    if let Some(page_count) = props.page_count {
                        rows.push(EntryPropertyRow {
                            label: tr(ui_language, TextKey::PageCountLabel).to_owned(),
                            value: page_count.to_string(),
                            copy_label: None,
                            height: EntryPropertyRowHeight::Single,
                        });
                    }

                    render_entry_property_grid(ui, &rows, &mut actions);

                    ui.add_space(12.0);
                    let close_button_w = 132.0;
                    let mut close_clicked = false;
                    ui.horizontal(|ui| {
                        ui.add_space(
                            ((entry_property_grid_width() - close_button_w) / 2.0).max(0.0),
                        );
                        let buttons = dialog_button_row(
                            ui,
                            31.0,
                            &[DialogButtonSpec {
                                id: ui.id().with(("properties_dialog", "close")),
                                label: tr(ui_language, TextKey::Close),
                                width: close_button_w,
                                is_default: true,
                            }],
                        );
                        close_clicked = buttons[0].clicked;
                    });
                    if close_clicked
                        || ui.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter))
                    {
                        close_requested = true;
                    }
                });
        });

    if close_requested || !open {
        actions.push(EntryPropertiesAction::Close);
    }
    actions
}

fn entry_property_grid_width() -> f32 {
    ENTRY_PROPERTY_LABEL_W
        + ENTRY_PROPERTY_CELL_GAP
        + ENTRY_PROPERTY_VALUE_W
        + ENTRY_PROPERTY_CELL_GAP
        + ENTRY_PROPERTY_ACTION_W
}

fn entry_property_text_max_width() -> f32 {
    (ENTRY_PROPERTY_VALUE_W - ENTRY_PROPERTY_TEXT_SAFE_MARGIN).max(1.0)
}

fn render_entry_property_grid(
    ui: &mut egui::Ui,
    rows: &[EntryPropertyRow],
    actions: &mut Vec<EntryPropertiesAction>,
) {
    ui.set_min_width(entry_property_grid_width());
    ui.set_max_width(entry_property_grid_width());

    let line_h = ui.spacing().interact_size.y;
    let font = egui::FontId::proportional(theme::FONT_SIZE_BODY);

    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = ENTRY_PROPERTY_ROW_GAP;
        for row in rows {
            render_entry_property_row(ui, row, line_h, &font, actions);
        }
    });
}

fn render_entry_property_row(
    ui: &mut egui::Ui,
    row: &EntryPropertyRow,
    line_h: f32,
    font: &egui::FontId,
    actions: &mut Vec<EntryPropertiesAction>,
) {
    let row_h = entry_property_row_height(row.height, line_h);
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = ENTRY_PROPERTY_CELL_GAP;
        ui.spacing_mut().item_spacing.y = 0.0;
        render_entry_property_label_cell(ui, &row.label, row.height, line_h);
        render_entry_property_value_cell(ui, row, line_h, font);
        render_entry_property_action_cell(ui, row, line_h, actions);
        ui.allocate_space(egui::vec2(0.0, row_h));
    });
}

fn entry_property_row_height(row_height: EntryPropertyRowHeight, line_h: f32) -> f32 {
    match row_height {
        EntryPropertyRowHeight::Single => line_h,
        EntryPropertyRowHeight::ThreeLines => line_h * ENTRY_PROPERTY_MULTILINE_ROWS as f32,
    }
}

fn render_entry_property_label_cell(
    ui: &mut egui::Ui,
    label: &str,
    row_height: EntryPropertyRowHeight,
    line_h: f32,
) {
    let row_h = entry_property_row_height(row_height, line_h);
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_LABEL_W, row_h),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_width(ENTRY_PROPERTY_LABEL_W);
            ui.set_max_width(ENTRY_PROPERTY_LABEL_W);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(egui::RichText::new(label).color(theme::TEXT_MAIN));
            let remaining_h = (line_h - ui.min_rect().height()).max(0.0);
            if remaining_h > 0.0 {
                ui.add_space(remaining_h);
            }
        },
    );
}

fn render_entry_property_value_cell(
    ui: &mut egui::Ui,
    row: &EntryPropertyRow,
    line_h: f32,
    font: &egui::FontId,
) {
    let row_h = entry_property_row_height(row.height, line_h);
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_VALUE_W, row_h),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            match row.height {
                EntryPropertyRowHeight::Single => {
                    let text = truncate_entry_property_line(
                        ui.painter(),
                        &row.value,
                        entry_property_text_max_width(),
                        font,
                    );
                    render_entry_property_text_line(ui, &text, line_h);
                }
                EntryPropertyRowHeight::ThreeLines => {
                    let lines = split_entry_property_lines(
                        ui.painter(),
                        &row.value,
                        entry_property_text_max_width(),
                        ENTRY_PROPERTY_MULTILINE_ROWS,
                        font,
                    );
                    for line_idx in 0..ENTRY_PROPERTY_MULTILINE_ROWS {
                        let line = lines.get(line_idx).map(String::as_str).unwrap_or("");
                        render_entry_property_text_line(ui, line, line_h);
                    }
                }
            }
        },
    );
}

fn render_entry_property_text_line(ui: &mut egui::Ui, text: &str, line_h: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_VALUE_W, line_h),
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            ui.set_min_width(ENTRY_PROPERTY_VALUE_W);
            ui.set_max_width(ENTRY_PROPERTY_VALUE_W);
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add(egui::Label::new(
                egui::RichText::new(text).color(theme::TEXT_MAIN),
            ));
        },
    );
}

fn render_entry_property_action_cell(
    ui: &mut egui::Ui,
    row: &EntryPropertyRow,
    line_h: f32,
    actions: &mut Vec<EntryPropertiesAction>,
) {
    let row_h = entry_property_row_height(row.height, line_h);
    ui.allocate_ui_with_layout(
        egui::vec2(ENTRY_PROPERTY_ACTION_W, row_h),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if let Some(copy_label) = &row.copy_label {
                if ui
                    .add_sized(
                        [ENTRY_PROPERTY_ACTION_W, line_h],
                        egui::Button::new(copy_label),
                    )
                    .clicked()
                {
                    actions.push(EntryPropertiesAction::CopyText(row.value.clone()));
                }
            } else {
                ui.add_space(line_h);
            }
        },
    );
}

fn split_entry_property_lines(
    painter: &egui::Painter,
    value: &str,
    max_width: f32,
    max_lines: usize,
    font: &egui::FontId,
) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    if value.is_empty() {
        return vec![String::new()];
    }

    let chars: Vec<char> = value.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;

    while start < chars.len() && lines.len() < max_lines {
        let is_last_line = lines.len() + 1 == max_lines;
        let mut end =
            best_fit_entry_property_end(painter, &chars, start, chars.len(), max_width, font);
        if end <= start {
            end = (start + 1).min(chars.len());
        }

        if is_last_line && end < chars.len() {
            lines.push(fit_entry_property_line_with_ellipsis(
                painter,
                &chars[start..],
                max_width,
                font,
            ));
            break;
        }

        lines.push(chars[start..end].iter().collect());
        start = end;
    }

    lines
}

fn truncate_entry_property_line(
    painter: &egui::Painter,
    value: &str,
    max_width: f32,
    font: &egui::FontId,
) -> String {
    if measured_entry_property_text_width(painter, value, font) <= max_width {
        return value.to_owned();
    }
    let chars: Vec<char> = value.chars().collect();
    fit_entry_property_line_with_ellipsis(painter, &chars, max_width, font)
}

fn best_fit_entry_property_end(
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
        if measured_entry_property_text_width(painter, &text, font) <= max_width {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid.saturating_sub(1);
        }
    }

    best
}

fn fit_entry_property_line_with_ellipsis(
    painter: &egui::Painter,
    chars: &[char],
    max_width: f32,
    font: &egui::FontId,
) -> String {
    let ellipsis = "…";
    if measured_entry_property_text_width(painter, ellipsis, font) > max_width {
        return String::new();
    }

    let mut lo = 0;
    let mut hi = chars.len();
    let mut best = 0;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let mut text: String = chars[..mid].iter().collect();
        text.push_str(ellipsis);
        if measured_entry_property_text_width(painter, &text, font) <= max_width {
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

fn measured_entry_property_text_width(
    painter: &egui::Painter,
    text: &str,
    font: &egui::FontId,
) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_MAIN)
        .size()
        .x
}

fn format_entry_info_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    if bytes == 0 {
        return String::from("0 B");
    }
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_entry_info_modified(modified: SystemTime) -> String {
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => Local
            .timestamp_opt(duration.as_secs() as i64, duration.subsec_nanos())
            .single()
            .map(|dt| dt.format("%Y/%m/%d %H:%M").to_string())
            .unwrap_or_else(|| String::from("-")),
        Err(_) => String::from("-"),
    }
}
