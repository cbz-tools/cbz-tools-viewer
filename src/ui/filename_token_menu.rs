use eframe::egui::{self, Color32, Sense};

use crate::domain::{
    app_settings::UiLanguage,
    archive::BookMeta,
    filename_parser::{FilenamePartRole, parse_filename},
};
use crate::infra::web_search::WebSearchMenuItem;

use super::{
    i18n::{TextKey, tr},
    theme,
};

const SELECTION_ACCENT: Color32 = Color32::from_rgb(40, 84, 222);

#[derive(Clone, Copy, Default)]
pub(crate) struct PopupKeyInput {
    pub(crate) up: bool,
    pub(crate) down: bool,
    pub(crate) left: bool,
    pub(crate) right: bool,
    pub(crate) esc: bool,
}

#[derive(Default)]
pub(crate) struct FilenameTokenMenuResult {
    pub(crate) filter_token: Option<String>,
    pub(crate) clear_filter: bool,
    pub(crate) web_search: Option<(usize, String)>,
    pub(crate) rendered: bool,
}

pub(crate) fn show_filename_token_menu_frame(
    ui: &mut egui::Ui,
    entry: &BookMeta,
    popup_keys: PopupKeyInput,
    language: UiLanguage,
    filter_enabled: bool,
    web_searches: &[WebSearchMenuItem],
) -> FilenameTokenMenuResult {
    let filename = entry
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| entry.title.to_string());

    if filename.trim().is_empty() {
        return FilenameTokenMenuResult::default();
    }

    let parsed = parse_filename(&filename);
    let extension = split_extension(&filename);
    let (segments, selectable_tokens) =
        build_filename_segments(&parsed.parts, extension.as_deref());
    if selectable_tokens.is_empty() {
        return FilenameTokenMenuResult::default();
    }

    let state_key = ui
        .id()
        .with("filename-token-selected")
        .with(entry.id.0.to_hex().to_string());
    let mut selected_idx = ui
        .ctx()
        .data_mut(|data| data.get_temp::<usize>(state_key))
        .filter(|idx| *idx < selectable_tokens.len())
        .unwrap_or_else(|| default_selected_token_index(&selectable_tokens));
    ui.set_min_width(520.0);
    ui.set_max_width(720.0);
    if popup_keys.up || popup_keys.down || popup_keys.left || popup_keys.right || popup_keys.esc {
        ui.close();
        return FilenameTokenMenuResult::default();
    }

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 0.0),
        egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for segment in &segments {
                match segment {
                    FilenameSegment::Text(text) => {
                        ui.label(egui::RichText::new(text).color(theme::TEXT_MAIN));
                    }
                    FilenameSegment::Token { token_idx, text } => {
                        let is_selected = selected_idx == *token_idx;
                        let mut rich =
                            egui::RichText::new(text).color(token_text_color(is_selected));
                        if is_selected {
                            rich = rich.background_color(SELECTION_ACCENT);
                        }
                        let label = egui::Label::new(rich).sense(Sense::click()).wrap();
                        let resp = ui.add(label);
                        if resp.hovered() && !is_selected {
                            let _ = resp.clone().highlight();
                        }
                        if resp.clicked() {
                            selected_idx = *token_idx;
                        }
                    }
                }
            }
        },
    );

    ui.separator();

    let selected_text = &selectable_tokens[selected_idx].text;
    let can_apply = !selected_text.trim().is_empty();
    let filter_enabled = filter_enabled && can_apply;
    let filter_label = tr(language, TextKey::FilterToken).replacen("{}", selected_text, 1);
    let filter_row = ContextMenuRowSpec {
        label: &filter_label,
        shortcut: "",
        enabled: filter_enabled,
        icon: None,
        label_color: if filter_enabled {
            theme::TEXT_MAIN
        } else {
            theme::TEXT_DISABLED
        },
        shortcut_color: theme::TEXT_SUBTLE,
        icon_color: theme::TEXT_MAIN,
    };
    if draw_context_menu_row(ui, &filter_row) {
        ui.close();
        return FilenameTokenMenuResult {
            filter_token: Some(selected_text.clone()),
            clear_filter: false,
            web_search: None,
            rendered: true,
        };
    }

    let copy_label = tr(language, TextKey::CopyToken).replacen("{}", selected_text, 1);
    let copy_row = ContextMenuRowSpec {
        label: &copy_label,
        shortcut: "",
        enabled: can_apply,
        icon: None,
        label_color: if can_apply {
            theme::TEXT_MAIN
        } else {
            theme::TEXT_DISABLED
        },
        shortcut_color: theme::TEXT_SUBTLE,
        icon_color: theme::TEXT_MAIN,
    };
    if draw_context_menu_row(ui, &copy_row) {
        ui.ctx().copy_text(selected_text.clone());
        ui.close();
    }

    for search in web_searches {
        let search_label = tr(language, TextKey::WebSearchToken)
            .replacen("{}", selected_text, 1)
            .replacen("{}", &search.display, 1);
        let search_row = ContextMenuRowSpec {
            label: &search_label,
            shortcut: "",
            enabled: can_apply,
            icon: None,
            label_color: if can_apply {
                theme::TEXT_MAIN
            } else {
                theme::TEXT_DISABLED
            },
            shortcut_color: theme::TEXT_SUBTLE,
            icon_color: theme::TEXT_MAIN,
        };
        if draw_context_menu_row(ui, &search_row) {
            ui.close();
            return FilenameTokenMenuResult {
                filter_token: None,
                clear_filter: false,
                web_search: Some((search.search_index, selected_text.clone())),
                rendered: true,
            };
        }
    }

    let clear_filter_row = ContextMenuRowSpec {
        label: tr(language, TextKey::ClearFilter),
        shortcut: "",
        enabled: true,
        label_color: theme::TEXT_MAIN,
        shortcut_color: theme::TEXT_SUBTLE,
        icon_color: theme::TEXT_MAIN,
        icon: None,
    };
    if draw_context_menu_row(ui, &clear_filter_row) {
        ui.close();
        return FilenameTokenMenuResult {
            filter_token: None,
            clear_filter: true,
            web_search: None,
            rendered: true,
        };
    }

    ui.ctx().data_mut(|data| {
        data.insert_temp(state_key, selected_idx);
    });

    FilenameTokenMenuResult {
        filter_token: None,
        clear_filter: false,
        web_search: None,
        rendered: true,
    }
}

const CONTEXT_MENU_ROW_CORNER_RADIUS: f32 = 3.0;
const CONTEXT_MENU_ROW_PADDING_X: f32 = 10.0;
const CONTEXT_MENU_ROW_ICON_WIDTH: f32 = 20.0;
const CONTEXT_MENU_ROW_LABEL_FONT_SIZE: f32 = theme::FONT_SIZE_LARGE;
const CONTEXT_MENU_ROW_SHORTCUT_FONT_SIZE: f32 = 10.5;
const CONTEXT_MENU_ROW_ICON_FONT_SIZE: f32 = 15.0;

pub(crate) struct ContextMenuRowSpec<'a> {
    pub(crate) label: &'a str,
    pub(crate) shortcut: &'a str,
    pub(crate) enabled: bool,
    pub(crate) icon: Option<egui_material_icons::MaterialIcon>,
    pub(crate) label_color: Color32,
    pub(crate) shortcut_color: Color32,
    pub(crate) icon_color: Color32,
}

pub(crate) fn draw_context_menu_row(ui: &mut egui::Ui, row: &ContextMenuRowSpec<'_>) -> bool {
    let width = ui.available_width().max(1.0);
    let height = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click());

    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CONTEXT_MENU_ROW_CORNER_RADIUS, theme::BUTTON_HOVER);
    }

    let label_x = if row.icon.is_some() {
        CONTEXT_MENU_ROW_PADDING_X + CONTEXT_MENU_ROW_ICON_WIDTH + 4.0
    } else {
        CONTEXT_MENU_ROW_PADDING_X
    };
    ui.painter().text(
        rect.left_center() + egui::vec2(label_x, 0.0),
        egui::Align2::LEFT_CENTER,
        row.label,
        egui::FontId::proportional(CONTEXT_MENU_ROW_LABEL_FONT_SIZE),
        row.label_color,
    );

    if let Some(icon) = row.icon {
        let icon_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(CONTEXT_MENU_ROW_PADDING_X, 0.0),
            egui::vec2(CONTEXT_MENU_ROW_ICON_WIDTH, rect.height()),
        );
        ui.painter().text(
            icon_rect.center(),
            egui::Align2::CENTER_CENTER,
            icon.codepoint,
            egui::FontId::new(CONTEXT_MENU_ROW_ICON_FONT_SIZE, icon.font_family()),
            row.icon_color,
        );
    }

    if !row.shortcut.is_empty() {
        ui.painter().text(
            rect.right_center() - egui::vec2(CONTEXT_MENU_ROW_PADDING_X, 0.0),
            egui::Align2::RIGHT_CENTER,
            row.shortcut,
            egui::FontId::proportional(CONTEXT_MENU_ROW_SHORTCUT_FONT_SIZE),
            row.shortcut_color,
        );
    }

    row.enabled && resp.clicked()
}

#[derive(Clone)]
struct SelectableToken {
    role: FilenamePartRole,
    text: String,
}

enum FilenameSegment {
    Text(String),
    Token { token_idx: usize, text: String },
}

fn default_selected_token_index(tokens: &[SelectableToken]) -> usize {
    tokens
        .iter()
        .position(|t| t.role == FilenamePartRole::Author)
        .or_else(|| {
            tokens
                .iter()
                .position(|t| t.role == FilenamePartRole::Title)
        })
        .unwrap_or(0)
}

fn build_filename_segments(
    parts: &[crate::domain::filename_parser::FilenamePart],
    extension: Option<&str>,
) -> (Vec<FilenameSegment>, Vec<SelectableToken>) {
    let mut segments = Vec::new();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut title_seen = false;

    while i < parts.len() {
        match parts[i].role {
            FilenamePartRole::Kind => {
                let lead = if segments.is_empty() { "(" } else { " (" };
                push_text_segment(&mut segments, lead);
                push_token_segment(
                    &mut segments,
                    &mut tokens,
                    FilenamePartRole::Kind,
                    &parts[i].text,
                );
                push_text_segment(&mut segments, ")");
                i += 1;
            }
            FilenamePartRole::Author => {
                let lead = if segments.is_empty() { "[" } else { " [" };
                push_text_segment(&mut segments, lead);
                push_token_segment(
                    &mut segments,
                    &mut tokens,
                    FilenamePartRole::Author,
                    &parts[i].text,
                );
                if i + 1 < parts.len() && parts[i + 1].role == FilenamePartRole::AuthorAlias {
                    push_text_segment(&mut segments, " (");
                    push_token_segment(
                        &mut segments,
                        &mut tokens,
                        FilenamePartRole::AuthorAlias,
                        &parts[i + 1].text,
                    );
                    push_text_segment(&mut segments, ")");
                    i += 1;
                }
                push_text_segment(&mut segments, "]");
                i += 1;
            }
            FilenamePartRole::Title => {
                let lead = if segments.is_empty() { "" } else { " " };
                push_text_segment(&mut segments, lead);
                push_token_segment(
                    &mut segments,
                    &mut tokens,
                    FilenamePartRole::Title,
                    &parts[i].text,
                );
                title_seen = true;
                i += 1;
            }
            FilenamePartRole::Work => {
                push_text_segment(&mut segments, if title_seen { " (" } else { "(" });
                push_token_segment(
                    &mut segments,
                    &mut tokens,
                    FilenamePartRole::Work,
                    &parts[i].text,
                );
                push_text_segment(&mut segments, ")");
                i += 1;
            }
            FilenamePartRole::Edition => {
                let lead = if segments.is_empty() { "[" } else { " [" };
                push_text_segment(&mut segments, lead);
                push_token_segment(
                    &mut segments,
                    &mut tokens,
                    FilenamePartRole::Edition,
                    &parts[i].text,
                );
                push_text_segment(&mut segments, "]");
                i += 1;
            }
            FilenamePartRole::AuthorAlias | FilenamePartRole::Extra => {
                let lead = if segments.is_empty() { "" } else { " " };
                push_text_segment(&mut segments, lead);
                push_token_segment(&mut segments, &mut tokens, parts[i].role, &parts[i].text);
                i += 1;
            }
        }
    }

    if let Some(ext) = extension {
        push_text_segment(&mut segments, ext);
    }

    (segments, tokens)
}

fn push_text_segment(segments: &mut Vec<FilenameSegment>, text: &str) {
    if !text.is_empty() {
        segments.push(FilenameSegment::Text(text.to_string()));
    }
}

fn push_token_segment(
    segments: &mut Vec<FilenameSegment>,
    tokens: &mut Vec<SelectableToken>,
    role: FilenamePartRole,
    text: &str,
) {
    let token_text = text.to_string();
    let idx = tokens.len();
    tokens.push(SelectableToken {
        role,
        text: token_text.clone(),
    });
    segments.push(FilenameSegment::Token {
        token_idx: idx,
        text: token_text,
    });
}

fn split_extension(filename: &str) -> Option<String> {
    let mut dot_pos = None;
    for (idx, ch) in filename.char_indices().rev() {
        if ch == '.' {
            dot_pos = Some(idx);
            break;
        }
        if ch == '/' || ch == '\\' {
            break;
        }
    }
    let idx = dot_pos?;
    if idx == 0 || idx >= filename.len() {
        return None;
    }
    Some(filename[idx..].to_string())
}

fn token_text_color(selected: bool) -> Color32 {
    if selected {
        theme::TEXT_ON_DARK
    } else {
        theme::TEXT_MAIN
    }
}
