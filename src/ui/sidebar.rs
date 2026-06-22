use std::{collections::HashMap, path::PathBuf};

use chrono::{Local, TimeZone};

use eframe::egui;
use eframe::egui::text::LayoutJob;

use crate::{
    domain::app_settings::UiLanguage,
    infra::cache::disk::DiskCache,
    session::{unix_ns_to_system_time, HistoryEntry, LeftPaneTab},
    ui::thumb_cache::load_disk_thumb_texture,
};

use super::{
    common::{paint_favorite_star_in_rect, paint_quiet_hover_border},
    i18n::{tr, TextKey},
    icons,
    library::{LibraryScope, LibraryState, ReadingHudState},
    theme,
};

// `Open*` を揃えることで、呼び出し側 dispatch 時に操作意図を variants 名だけで判別できる。
#[allow(clippy::enum_variant_names)]
pub enum SidebarAction {
    OpenFavorite(PathBuf),
    OpenInExplorer(PathBuf),
    OpenHistory(PathBuf),
}

pub struct SidebarViewContext<'a> {
    pub state: &'a mut LibraryState,
    pub favorites: &'a mut Vec<PathBuf>,
    pub left_pane_tab: &'a mut LeftPaneTab,
    pub language: UiLanguage,
    pub history: &'a [HistoryEntry],
    pub history_textures: &'a mut HashMap<String, egui::TextureHandle>,
    pub disk_cache: Option<&'a DiskCache>,
}

const SIDEBAR_TAB_BUTTON_SIZE: egui::Vec2 = egui::vec2(112.0, 24.0);
const SIDEBAR_FAVORITE_ROW_HEIGHT: f32 = 24.0;
const SIDEBAR_HISTORY_ROW_HEIGHT: f32 = 72.0;
const SIDEBAR_SELECTION_BAR_WIDTH: f32 = 4.0;
const SIDEBAR_STATUS_ICON_SIZE: f32 = 12.0;
const SIDEBAR_STATUS_ICON_LEFT: f32 = 4.0;
const SIDEBAR_STATUS_ICON_TEXT_GAP: f32 = 6.0;

fn paint_sidebar_data_row_state(ui: &egui::Ui, rect: egui::Rect, selected: bool, hovered: bool) {
    if selected {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(4),
            theme::SIDEBAR_SELECTED_BG,
        );
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.min.x + SIDEBAR_SELECTION_BAR_WIDTH, rect.max.y),
            ),
            0,
            theme::ACCENT_ACTIVE,
        );
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    } else if hovered {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(4), theme::SIDEBAR_HOVER_BG);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(3.0, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    }
}

fn sidebar_data_selectable_label(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    let width = ui.available_width().max(0.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, SIDEBAR_FAVORITE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    paint_sidebar_data_row_state(ui, rect, selected, resp.hovered());
    ui.painter().text(
        rect.left_center() + egui::vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(theme::FONT_SIZE_BODY),
        theme::TEXT_MAIN,
    );
    resp
}

fn paint_reading_status_icon(
    ui: &egui::Ui,
    rect: egui::Rect,
    state: ReadingHudState,
    selected: bool,
    hovered: bool,
) {
    let painter = ui.painter();
    let line_color = if selected {
        theme::ACCENT_ACTIVE
    } else if hovered {
        theme::TEXT_MAIN
    } else {
        theme::TEXT_SUBTLE
    };
    match state {
        ReadingHudState::Unread => {
            let stroke = egui::Stroke::new(1.0, line_color);
            let x0 = rect.min.x;
            let x1 = rect.max.x;
            let y0 = rect.min.y;
            let y1 = rect.max.y;
            let dash = 3.0;
            let gap = 2.0;
            let mut x = x0;
            while x < x1 {
                let x_end = (x + dash).min(x1);
                painter.line_segment([egui::pos2(x, y0), egui::pos2(x_end, y0)], stroke);
                painter.line_segment([egui::pos2(x, y1), egui::pos2(x_end, y1)], stroke);
                x += dash + gap;
            }
            let mut y = y0;
            while y < y1 {
                let y_end = (y + dash).min(y1);
                painter.line_segment([egui::pos2(x0, y), egui::pos2(x0, y_end)], stroke);
                painter.line_segment([egui::pos2(x1, y), egui::pos2(x1, y_end)], stroke);
                y += dash + gap;
            }
        }
        ReadingHudState::Reading | ReadingHudState::ReadingPercent(_) => {
            painter.rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::WHITE);
            let mid_y = rect.center().y;
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(rect.min.x, mid_y), rect.max),
                egui::CornerRadius::ZERO,
                theme::TEXT_MAIN,
            );
            painter.rect_stroke(
                rect,
                egui::CornerRadius::ZERO,
                egui::Stroke::new(1.0, line_color),
                egui::StrokeKind::Inside,
            );
        }
        ReadingHudState::Read => {
            painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::TEXT_MAIN);
            painter.rect_stroke(
                rect,
                egui::CornerRadius::ZERO,
                egui::Stroke::new(1.0, line_color),
                egui::StrokeKind::Inside,
            );
        }
    }
}

fn paint_folder_icon(ui: &egui::Ui, rect: egui::Rect, _selected: bool, _hovered: bool) {
    let painter = ui.painter();
    let color = theme::TEXT_MAIN;
    let min = rect.min;
    let body = egui::Rect::from_min_max(
        egui::pos2(min.x + 0.5, min.y + 5.5),
        egui::pos2(min.x + 11.5, min.y + 10.5),
    );
    let back = egui::Rect::from_min_max(
        egui::pos2(min.x + 2.5, min.y + 3.5),
        egui::pos2(min.x + 11.5, min.y + 7.5),
    );
    let tab = egui::Rect::from_min_max(
        egui::pos2(min.x + 0.5, min.y + 2.5),
        egui::pos2(min.x + 7.5, min.y + 6.5),
    );
    painter.rect_filled(body, egui::CornerRadius::same(1), color);
    painter.rect_filled(back, egui::CornerRadius::same(1), color);
    painter.rect_filled(tab, egui::CornerRadius::same(1), color);
}

fn sidebar_data_selectable_icon_label(
    ui: &mut egui::Ui,
    selected: bool,
    reading_state: ReadingHudState,
    label: &str,
) -> egui::Response {
    let width = ui.available_width().max(0.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, SIDEBAR_FAVORITE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    paint_sidebar_data_row_state(ui, rect, selected, resp.hovered());
    let icon_rect = egui::Rect::from_min_size(
        rect.left_center() + egui::vec2(SIDEBAR_STATUS_ICON_LEFT, -SIDEBAR_STATUS_ICON_SIZE / 2.0),
        egui::vec2(SIDEBAR_STATUS_ICON_SIZE, SIDEBAR_STATUS_ICON_SIZE),
    );
    paint_reading_status_icon(ui, icon_rect, reading_state, selected, resp.hovered());
    ui.painter().text(
        rect.left_center()
            + egui::vec2(
                SIDEBAR_STATUS_ICON_LEFT + SIDEBAR_STATUS_ICON_SIZE + SIDEBAR_STATUS_ICON_TEXT_GAP,
                0.0,
            ),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(theme::FONT_SIZE_BODY),
        theme::TEXT_MAIN,
    );
    resp
}

fn sidebar_data_selectable_favorite_label(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
) -> egui::Response {
    let width = ui.available_width().max(0.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, SIDEBAR_FAVORITE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    paint_sidebar_data_row_state(ui, rect, selected, resp.hovered());
    let icon_rect = egui::Rect::from_min_size(
        rect.left_center() + egui::vec2(SIDEBAR_STATUS_ICON_LEFT, -SIDEBAR_STATUS_ICON_SIZE / 2.0),
        egui::vec2(SIDEBAR_STATUS_ICON_SIZE, SIDEBAR_STATUS_ICON_SIZE),
    );
    paint_favorite_star_in_rect(ui.painter(), icon_rect, theme::TEXT_MAIN);
    ui.painter().text(
        rect.left_center()
            + egui::vec2(
                SIDEBAR_STATUS_ICON_LEFT + SIDEBAR_STATUS_ICON_SIZE + SIDEBAR_STATUS_ICON_TEXT_GAP,
                0.0,
            ),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(theme::FONT_SIZE_BODY),
        theme::TEXT_MAIN,
    );
    resp
}

#[derive(PartialEq, Eq)]
enum HistoryDateGroup {
    Today,
    Yesterday,
    Date(chrono::NaiveDate),
}

fn classify_date_local(opened_at_ms: u64, today: chrono::NaiveDate) -> HistoryDateGroup {
    let dt = Local
        .timestamp_millis_opt(opened_at_ms as i64)
        .single()
        .unwrap_or_else(Local::now);
    match (today - dt.date_naive()).num_days() {
        0 => HistoryDateGroup::Today,
        1 => HistoryDateGroup::Yesterday,
        _ => HistoryDateGroup::Date(dt.date_naive()),
    }
}

pub fn show(ui: &mut egui::Ui, context: SidebarViewContext<'_>) -> Option<SidebarAction> {
    let SidebarViewContext {
        state,
        favorites,
        left_pane_tab,
        language,
        history,
        history_textures,
        disk_cache,
    } = context;
    let mut action: Option<SidebarAction> = None;
    let quiet_stroke = egui::Stroke::new(1.0, egui::Color32::TRANSPARENT);

    ui.horizontal(|ui| {
        let is_library = *left_pane_tab == LeftPaneTab::Library;
        let is_history = *left_pane_tab == LeftPaneTab::History;
        if ui
            .add_sized(
                SIDEBAR_TAB_BUTTON_SIZE,
                egui::Button::new(format!("📁 {}", tr(language, TextKey::LibraryTab)))
                    .selected(is_library),
            )
            .clicked()
        {
            *left_pane_tab = LeftPaneTab::Library;
        }
        if ui
            .add_sized(
                SIDEBAR_TAB_BUTTON_SIZE,
                egui::Button::new(format!("🕐 {}", tr(language, TextKey::HistoryTab)))
                    .selected(is_history),
            )
            .clicked()
        {
            *left_pane_tab = LeftPaneTab::History;
        }
    });

    ui.separator();
    if *left_pane_tab == LeftPaneTab::History {
        let today = Local::now().date_naive();
        let mut last_group: Option<HistoryDateGroup> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for entry in history {
                let group = classify_date_local(entry.opened_at_ms, today);
                if last_group.as_ref() != Some(&group) {
                    let title = match &group {
                        HistoryDateGroup::Today => tr(language, TextKey::Today).to_owned(),
                        HistoryDateGroup::Yesterday => tr(language, TextKey::Yesterday).to_owned(),
                        HistoryDateGroup::Date(d) => super::i18n::format_history_date(language, *d),
                    };
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(title)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_SUBTLE),
                    );
                    ui.add_space(2.0);
                    last_group = Some(group);
                }

                let exists = entry.path.exists();
                let label = entry
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.path.to_string_lossy().into_owned());
                let text_color = if exists {
                    theme::TEXT_MAIN
                } else {
                    theme::TEXT_SUBTLE
                };
                let key = format!(
                    "{}:{}:{}",
                    entry.normalized_path,
                    entry.file_size.unwrap_or(0),
                    entry.modified_unix_ns.unwrap_or(0)
                );
                if !history_textures.contains_key(&key) {
                    if let (Some(cache), Some(file_size)) = (disk_cache, entry.file_size) {
                        let modified = entry.modified_unix_ns.and_then(unix_ns_to_system_time);
                        if let Some(thumb) = load_disk_thumb_texture(
                            ui.ctx(),
                            cache,
                            &entry.path,
                            file_size,
                            modified,
                            format!("history_thumb_{}", key),
                        ) {
                            history_textures.insert(key.clone(), thumb.texture);
                        }
                    }
                }

                let row_width = ui.available_width().max(0.0);
                let (_, row_rect) =
                    ui.allocate_space(egui::vec2(row_width, SIDEBAR_HISTORY_ROW_HEIGHT));
                let row_id =
                    ui.id()
                        .with(("history_row", &entry.normalized_path, entry.opened_at_ms));
                let row_resp = ui.interact(row_rect, row_id, egui::Sense::click());
                if row_resp.hovered() {
                    let history_hover_bg = egui::Color32::from_rgb(224, 224, 224);
                    ui.painter().rect_filled(
                        row_rect,
                        egui::CornerRadius::same(4),
                        history_hover_bg,
                    );
                }

                let painter = ui.painter_at(row_rect);
                let inner_rect = row_rect.shrink2(egui::vec2(8.0, 0.0));
                let thumb_rect = egui::Rect::from_min_size(inner_rect.min, egui::vec2(52.0, 72.0));
                if let Some(tex) = history_textures.get(&key) {
                    painter.image(
                        tex.id(),
                        thumb_rect,
                        egui::Rect::from_min_max(
                            egui::Pos2::new(0.0, 0.0),
                            egui::Pos2::new(1.0, 1.0),
                        ),
                        if exists {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_gray(110)
                        },
                    );
                } else {
                    painter.rect_filled(thumb_rect, 4.0, egui::Color32::from_gray(70));
                }

                let text_x = thumb_rect.max.x + 8.0;
                let text_w = (inner_rect.max.x - text_x - 4.0).max(16.0);
                let text_rect = egui::Rect::from_min_size(
                    egui::pos2(text_x, inner_rect.min.y),
                    egui::vec2(text_w, inner_rect.height()),
                );
                let mut job = LayoutJob::single_section(
                    label.clone(),
                    egui::TextFormat {
                        color: text_color,
                        ..Default::default()
                    },
                );
                job.wrap.max_width = text_w;
                job.wrap.max_rows = 4;
                job.wrap.break_anywhere = true;
                let galley = painter.layout_job(job);
                painter.galley(text_rect.min, galley, text_color);

                if exists && row_resp.clicked() {
                    action = Some(SidebarAction::OpenHistory(entry.path.clone()));
                }
            }
        });
        return action;
    }

    let add_resp = ui
        .add_sized(
            SIDEBAR_TAB_BUTTON_SIZE,
            egui::Button::new(tr(language, TextKey::AddFolder))
                .fill(egui::Color32::TRANSPARENT)
                .stroke(quiet_stroke),
        )
        .on_hover_text(tr(language, TextKey::AddFolderHint));
    paint_quiet_hover_border(ui, &add_resp);
    if add_resp.clicked() {
        if let Some(dir) = &state.current_dir {
            if !favorites.contains(dir) {
                favorites.push(dir.clone());
            }
        }
    }

    ui.separator();
    let mut remove_idx: Option<usize> = None;
    egui::ScrollArea::vertical()
        .id_salt("library_sidebar_scroll")
        .show(ui, |ui| {
            if favorites.is_empty() {
                ui.label(
                    egui::RichText::new(tr(language, TextKey::FolderDropHint))
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_SUBTLE),
                );
            } else {
                for (i, path) in favorites.iter().enumerate() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned());

                    let is_current = state.current_dir.as_deref() == Some(path.as_path());
                    let row_width = ui.available_width().max(0.0);
                    let resp = ui
                        .add_sized(
                            [row_width, SIDEBAR_FAVORITE_ROW_HEIGHT],
                            egui::Button::new("")
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(quiet_stroke),
                        )
                        .on_hover_text(path.to_string_lossy());
                    paint_sidebar_data_row_state(ui, resp.rect, is_current, resp.hovered());
                    let text_pos = resp.rect.left_center() + egui::vec2(22.0, 0.0);
                    let icon_rect = egui::Rect::from_min_size(
                        resp.rect.left_center() + egui::vec2(4.0, -6.0),
                        egui::vec2(12.0, 12.0),
                    );
                    paint_folder_icon(ui, icon_rect, is_current, resp.hovered());
                    ui.painter().text(
                        text_pos,
                        egui::Align2::LEFT_CENTER,
                        &name,
                        egui::FontId::proportional(14.0),
                        ui.visuals().text_color(),
                    );

                    if resp.clicked() {
                        action = Some(SidebarAction::OpenFavorite(path.clone()));
                    }

                    resp.context_menu(|ui| {
                        ui.label(
                            egui::RichText::new(path.to_string_lossy())
                                .size(theme::FONT_SIZE_SMALL)
                                .color(theme::TEXT_SUBTLE),
                        );
                        ui.separator();
                        if ui
                            .button(icons::icon_label(
                                ui,
                                icons::ICON_FOLDER_OPEN,
                                15.0,
                                tr(language, TextKey::OpenInExplorer),
                            ))
                            .clicked()
                        {
                            action = Some(SidebarAction::OpenInExplorer(path.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(icons::icon_label(
                                ui,
                                icons::ICON_DELETE,
                                15.0,
                                tr(language, TextKey::RemoveFromFavorites),
                            ))
                            .clicked()
                        {
                            remove_idx = Some(i);
                            ui.close();
                        }
                    });
                }
            }

            ui.separator();

            let favorite_count = state.favorite_count();
            let is_favorites_selected = state.filter.scope == LibraryScope::Favorites;
            if sidebar_data_selectable_favorite_label(
                ui,
                is_favorites_selected,
                &format!("{} ({favorite_count})", tr(language, TextKey::Favorites)),
            )
            .clicked()
            {
                state.filter.scope = if is_favorites_selected {
                    LibraryScope::Any
                } else {
                    LibraryScope::Favorites
                };
                state.mark_filter_dirty();
                state.reset_context_menu_cache = true;
            }

            ui.separator();

            let is_unread_selected = state.filter.scope == LibraryScope::Unread;
            let unread_count = state.reading_unread_count();
            if sidebar_data_selectable_icon_label(
                ui,
                is_unread_selected,
                ReadingHudState::Unread,
                &format!("{} ({unread_count})", tr(language, TextKey::Unread)),
            )
            .clicked()
            {
                state.filter.scope = if is_unread_selected {
                    LibraryScope::Any
                } else {
                    LibraryScope::Unread
                };
                state.mark_filter_dirty();
                state.reset_context_menu_cache = true;
            }

            let is_reading_selected = state.filter.scope == LibraryScope::Reading;
            let reading_count = state.reading_reading_count();
            if sidebar_data_selectable_icon_label(
                ui,
                is_reading_selected,
                ReadingHudState::Reading,
                &format!("{} ({reading_count})", tr(language, TextKey::Reading)),
            )
            .clicked()
            {
                state.filter.scope = if is_reading_selected {
                    LibraryScope::Any
                } else {
                    LibraryScope::Reading
                };
                state.mark_filter_dirty();
                state.reset_context_menu_cache = true;
            }

            let is_read_selected = state.filter.scope == LibraryScope::Read;
            let read_count = state.reading_read_count();
            if sidebar_data_selectable_icon_label(
                ui,
                is_read_selected,
                ReadingHudState::Read,
                &format!("{} ({read_count})", tr(language, TextKey::Read)),
            )
            .clicked()
            {
                state.filter.scope = if is_read_selected {
                    LibraryScope::Any
                } else {
                    LibraryScope::Read
                };
                state.mark_filter_dirty();
                state.reset_context_menu_cache = true;
            }

            ui.separator();

            if let Some(err) = state.kind_config_error() {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    tr(language, TextKey::KindGroupsError),
                );
                let _ = err;
                return;
            }

            ui.label(
                egui::RichText::new(tr(language, TextKey::Groups))
                    .size(theme::FONT_SIZE_LARGE)
                    .color(theme::TEXT_MAIN),
            );

            let groups_snapshot: HashMap<String, Vec<String>> = state
                .kind_groups()
                .iter()
                .map(|(k, v)| (k.clone(), v.children.clone()))
                .collect();
            let parent_counts_snapshot = state.parent_group_counts().clone();
            let leaf_counts_snapshot = state.leaf_group_counts().clone();

            let child_groups: std::collections::HashSet<&str> = groups_snapshot
                .values()
                .flat_map(|children| children.iter().map(|s| s.as_str()))
                .collect();

            let mut all_top_level: Vec<String> = groups_snapshot
                .keys()
                .cloned()
                .chain(leaf_counts_snapshot.keys().filter_map(|g| {
                    if !child_groups.contains(g.as_str())
                        && !groups_snapshot.contains_key(g.as_str())
                    {
                        Some(g.clone())
                    } else {
                        None
                    }
                }))
                .collect();
            all_top_level.sort();
            all_top_level.dedup();

            for group_name in &all_top_level {
                if let Some(children) = groups_snapshot.get(group_name.as_str()) {
                    let count = parent_counts_snapshot
                        .get(group_name.as_str())
                        .copied()
                        .unwrap_or(0);
                    let label = format!("{group_name} ({count})");
                    let is_selected = matches!(
                        &state.filter.scope,
                        LibraryScope::NamedGroup(n) if n == group_name
                    );
                    if sidebar_data_selectable_label(ui, is_selected, &label).clicked() {
                        state.filter.scope = if is_selected {
                            LibraryScope::Any
                        } else {
                            LibraryScope::NamedGroup(group_name.clone())
                        };
                        state.mark_filter_dirty();
                        state.reset_context_menu_cache = true;
                    }

                    let mut sorted_children = children.clone();
                    sorted_children.sort();
                    for child in &sorted_children {
                        let count = leaf_counts_snapshot.get(child).copied().unwrap_or(0);
                        let label = format!("  └ {child} ({count})");
                        let is_selected = matches!(
                            &state.filter.scope,
                            LibraryScope::NamedGroup(n) if n == child
                        );
                        if sidebar_data_selectable_label(ui, is_selected, &label).clicked() {
                            state.filter.scope = if is_selected {
                                LibraryScope::Any
                            } else {
                                LibraryScope::NamedGroup(child.clone())
                            };
                            state.mark_filter_dirty();
                            state.reset_context_menu_cache = true;
                        }
                    }
                } else {
                    let count = leaf_counts_snapshot
                        .get(group_name.as_str())
                        .copied()
                        .unwrap_or(0);
                    let label = format!("{group_name} ({count})");
                    let is_selected = matches!(
                        &state.filter.scope,
                        LibraryScope::NamedGroup(n) if n == group_name
                    );
                    if sidebar_data_selectable_label(ui, is_selected, &label).clicked() {
                        state.filter.scope = if is_selected {
                            LibraryScope::Any
                        } else {
                            LibraryScope::NamedGroup(group_name.clone())
                        };
                        state.mark_filter_dirty();
                        state.reset_context_menu_cache = true;
                    }
                }
            }

            let uncategorized_count = state.uncategorized_count();
            if uncategorized_count > 0 {
                let label = format!(
                    "{} ({uncategorized_count})",
                    tr(language, TextKey::Uncategorized)
                );
                let is_selected = state.filter.scope == LibraryScope::Uncategorized;
                if sidebar_data_selectable_label(ui, is_selected, &label).clicked() {
                    state.filter.scope = if is_selected {
                        LibraryScope::Any
                    } else {
                        LibraryScope::Uncategorized
                    };
                    state.mark_filter_dirty();
                    state.reset_context_menu_cache = true;
                }
            }
        });

    if let Some(i) = remove_idx {
        let removed = favorites.get(i).map(|p| p.display().to_string());
        favorites.remove(i);
        tracing::debug!(
            removed = ?removed,
            count = favorites.len(),
            favorites = ?favorites.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "sidebar: favorite removed"
        );
    }

    action
}
