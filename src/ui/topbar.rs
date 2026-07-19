//! トップバーの UI。
//!
//! パス入力、ソート、フィルタ、viewer 起動モードの切替だけを扱う。
use std::path::{Component, Path, PathBuf};

use eframe::egui;
use eframe::egui::text::{CCursor, CCursorRange};

use crate::domain::{
    app_settings::{UiLanguage, ViewerOpenMode},
    sort::{SortKey, SortOrder},
};

use super::{
    common::paint_quiet_hover_border,
    i18n::{TextKey, tr},
    icons,
    library::{LibraryScope, LibraryState},
    theme,
};

const FILTER_INPUT_WIDTH: f32 = 200.0;
const FILTER_INPUT_HEIGHT: f32 = 24.0;
const TRAILING_UI_RESERVE_WIDTH: f32 = 680.0;
const TOPBAR_HEIGHT: f32 = 32.0;
const TOPBAR_GAP_SMALL: f32 = 2.0;
const PATH_MIN_WIDTH: f32 = 120.0;
const ICON_SIZE_MENU_SETTINGS: f32 = 18.0;
const ICON_SIZE_NAV: f32 = 16.0;
const ICON_SIZE_SEARCH: f32 = 15.0;
const ICON_SIZE_FILTER_CLEAR: f32 = 14.0;
const TOPBAR_ICON_BUTTON_BASE_WIDTH: f32 = 26.0;
const TOPBAR_ICON_BUTTON_SIZE: egui::Vec2 = egui::vec2(
    TOPBAR_ICON_BUTTON_BASE_WIDTH + theme::ICON_BUTTON_HOVER_GUARD_X,
    theme::CONTROL_HEIGHT,
);
const TOPBAR_SMALL_TEXT_BUTTON_SIZE: egui::Vec2 = egui::vec2(44.0, 24.0);
const TOPBAR_MODE_BUTTON_SIZE: egui::Vec2 = egui::vec2(88.0, 24.0);
const TOPBAR_SORT_BUTTON_SIZE: egui::Vec2 = egui::vec2(34.0, 24.0);
const LABEL_FONT_SIZE: f32 = theme::FONT_SIZE_BODY;
const SORT_COMBO_WIDTH: f32 = 90.0;
const FILTER_CLEAR_BUTTON_SIZE: f32 = 18.0;
const FILTER_CLEAR_BUTTON_RIGHT_PADDING: f32 = 4.0;

/// トップバーの描画結果
pub struct TopbarResult {
    /// パス入力でスキャンするディレクトリ（D&D は app.rs 側で集約管理）
    pub scan_dir: Option<PathBuf>,
    /// ライブラリオーバーレイの開閉要求
    pub toggle_sidebar: bool,
    /// ⚙ ボタンが押されたら true
    pub settings_requested: bool,
    /// HUD 表示モードが変更されたら true
    pub hud_mode_changed: bool,
    /// Viewer 起動モードが変更されたら true
    pub viewer_open_mode_changed: bool,
    pub nav_back: bool,
    pub nav_forward: bool,
    pub nav_up: bool,
    pub nav_reload: bool,
    pub breadcrumb_nav: Option<PathBuf>,
    pub path_commit: Option<String>,
    pub path_cancelled: bool,
    pub path_blank_clicked: bool,
    pub path_edit_rect: Option<egui::Rect>,
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut LibraryState,
    language: UiLanguage,
    viewer_open_mode: &mut ViewerOpenMode,
    _ignore_external_drop: bool,
) -> TopbarResult {
    let mut toggle_sidebar = false;
    let mut settings_requested = false;
    let mut hud_mode_changed = false;
    let mut viewer_open_mode_changed = false;
    let mut nav_back = false;
    let mut nav_forward = false;
    let mut nav_up = false;
    let mut nav_reload = false;
    let mut breadcrumb_nav = None;
    let mut path_commit = None;
    let mut path_cancelled = false;
    let mut path_blank_clicked = false;
    let mut path_edit_rect = None;
    state.path_input_focused = false;
    let quiet_stroke = egui::Stroke::new(1.0_f32, egui::Color32::TRANSPARENT);
    let selected_stroke = egui::Stroke::new(1.0_f32, theme::ACCENT_ACTIVE);

    ui.horizontal(|ui| {
        ui.set_height(TOPBAR_HEIGHT);

        // ── パス直接入力 ──────────────────────────────────────────────────────
        let menu_resp = ui
            .add_sized(
                TOPBAR_ICON_BUTTON_SIZE,
                egui::Button::new(icons::icon(icons::ICON_MENU, ICON_SIZE_MENU_SETTINGS))
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(quiet_stroke),
            )
            .on_hover_text(tr(language, TextKey::ShowLibrary));
        paint_quiet_hover_border(ui, &menu_resp);
        if menu_resp.clicked() {
            toggle_sidebar = true;
        }

        ui.add_space(TOPBAR_GAP_SMALL);

        let hud_selected = matches!(
            state.hud_mode,
            crate::domain::app_settings::LibraryHudMode::On
        );
        let hud_resp = ui
            .add_sized(
                TOPBAR_SMALL_TEXT_BUTTON_SIZE,
                egui::Button::new(egui::RichText::new("HUD").size(LABEL_FONT_SIZE))
                    .fill(if hud_selected {
                        theme::BUTTON_ACTIVE
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .stroke(if hud_selected {
                        selected_stroke
                    } else {
                        quiet_stroke
                    }),
            )
            .on_hover_text(tr(language, TextKey::CurrentHud).replace("{}", state.hud_mode.label()));
        if hud_resp.clicked() {
            state.hud_mode = state.hud_mode.next();
            hud_mode_changed = true;
        }

        ui.add_space(TOPBAR_GAP_SMALL);
        let nav_back_resp = ui
            .add_sized(
                TOPBAR_ICON_BUTTON_SIZE,
                egui::Button::new(icons::icon(icons::ICON_ARROW_BACK, ICON_SIZE_NAV))
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(quiet_stroke),
            )
            .on_hover_text(format!("{} (Alt+←)", tr(language, TextKey::Back)));
        paint_quiet_hover_border(ui, &nav_back_resp);
        nav_back = nav_back_resp.clicked();
        let nav_forward_resp = ui
            .add_sized(
                TOPBAR_ICON_BUTTON_SIZE,
                egui::Button::new(icons::icon(icons::ICON_ARROW_FORWARD, ICON_SIZE_NAV))
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(quiet_stroke),
            )
            .on_hover_text(format!("{} (Alt+→)", tr(language, TextKey::Forward)));
        paint_quiet_hover_border(ui, &nav_forward_resp);
        nav_forward = nav_forward_resp.clicked();
        let nav_up_resp = ui
            .add_sized(
                TOPBAR_ICON_BUTTON_SIZE,
                egui::Button::new(icons::icon(icons::ICON_ARROW_UPWARD, ICON_SIZE_NAV))
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(quiet_stroke),
            )
            .on_hover_text(format!("{} (Alt+↑)", tr(language, TextKey::ParentFolder)));
        paint_quiet_hover_border(ui, &nav_up_resp);
        nav_up = nav_up_resp.clicked();
        let nav_reload_resp = ui
            .add_sized(
                TOPBAR_ICON_BUTTON_SIZE,
                egui::Button::new(icons::icon(icons::ICON_REFRESH, ICON_SIZE_NAV))
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(quiet_stroke),
            )
            .on_hover_text(format!("{} (F5)", tr(language, TextKey::Reload)));
        paint_quiet_hover_border(ui, &nav_reload_resp);
        nav_reload = nav_reload_resp.clicked();
        ui.add_space(TOPBAR_GAP_SMALL);
        ui.separator();

        // path領域を先に確保して、右側UI描画で潰れないようにする。
        let path_width = (ui.available_width() - TRAILING_UI_RESERVE_WIDTH).max(PATH_MIN_WIDTH);
        let mut breadcrumb_segment_clicked = false;
        ui.allocate_ui_with_layout(
            egui::vec2(path_width, FILTER_INPUT_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if state.is_path_editing {
                    let textedit_id = ui.id().with("path_edit_buffer");
                    let output = egui::TextEdit::singleline(&mut state.path_edit_buffer)
                        .id(textedit_id)
                        .hint_text(tr(language, TextKey::EnterPathHint))
                        .desired_width(path_width)
                        .font(egui::TextStyle::Monospace)
                        .show(ui);
                    let path_resp = output.response;
                    path_edit_rect = Some(path_resp.rect);
                    if !state.path_input_focused {
                        path_resp.request_focus();
                    }
                    if state.path_edit_select_all_pending && path_resp.has_focus() {
                        let mut text_edit_state = output.state;
                        let ccursor_range = CCursorRange::two(
                            CCursor::new(0),
                            CCursor::new(state.path_edit_buffer.chars().count()),
                        );
                        text_edit_state.cursor.set_char_range(Some(ccursor_range));
                        text_edit_state.store(ui.ctx(), textedit_id);
                        state.path_edit_select_all_pending = false;
                    }
                    state.path_input_focused = path_resp.has_focus();
                    let lost_focus = path_resp.lost_focus();
                    let mut enter_pressed = false;
                    let mut esc_pressed = false;
                    if path_resp.has_focus() {
                        ui.input_mut(|i| {
                            enter_pressed = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                            esc_pressed = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
                            i.consume_key(egui::Modifiers::CTRL, egui::Key::C);
                            i.consume_key(egui::Modifiers::CTRL, egui::Key::V);
                            i.consume_key(egui::Modifiers::CTRL, egui::Key::A);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Home);
                            i.consume_key(egui::Modifiers::NONE, egui::Key::End);
                        });
                    }
                    if path_resp.has_focus() && enter_pressed {
                        path_commit = Some(state.path_edit_buffer.trim().to_owned());
                    } else if (path_resp.has_focus() && esc_pressed) || lost_focus {
                        path_cancelled = true;
                    }
                } else if let Some(current_dir) = state.current_dir.as_deref() {
                    for (idx, (label, target)) in
                        breadcrumb_segments(current_dir).into_iter().enumerate()
                    {
                        if idx > 0 {
                            ui.label(">");
                        }
                        if ui.link(label).clicked() {
                            breadcrumb_segment_clicked = true;
                            breadcrumb_nav = Some(target);
                        }
                    }

                    // グループフィルタ表示（is_path_editing == false のとき）
                    let active_group_label = match &state.filter.scope {
                        LibraryScope::Any => None,
                        LibraryScope::Favorites => {
                            Some(tr(language, TextKey::FavoritesScopeLabel).to_string())
                        }
                        LibraryScope::Unread => Some(tr(language, TextKey::Unread).to_string()),
                        LibraryScope::Reading => Some(tr(language, TextKey::Reading).to_string()),
                        LibraryScope::Read => Some(tr(language, TextKey::Read).to_string()),
                        LibraryScope::NamedGroup(name) => Some(name.clone()),
                        LibraryScope::Uncategorized => {
                            Some(tr(language, TextKey::Uncategorized).to_string())
                        }
                    };
                    if let Some(label) = active_group_label {
                        ui.label(
                            egui::RichText::new("›")
                                .size(theme::FONT_SIZE_BODY)
                                .color(theme::TEXT_SUBTLE),
                        );
                        ui.label(
                            egui::RichText::new(&label)
                                .size(theme::FONT_SIZE_BODY)
                                .color(theme::TEXT_MAIN),
                        );
                        let close_id = ui.id().with("group_filter_clear");
                        let close_rect = egui::Rect::from_center_size(
                            ui.cursor().left_center() + egui::vec2(6.0, 0.0),
                            egui::vec2(12.0, 12.0),
                        );
                        let close_resp = ui.interact(close_rect, close_id, egui::Sense::click());
                        ui.painter().text(
                            close_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            icons::ICON_CLOSE.codepoint,
                            egui::FontId::new(
                                theme::FONT_SIZE_TINY,
                                icons::ICON_CLOSE.font_family(),
                            ),
                            theme::TEXT_MAIN,
                        );
                        ui.advance_cursor_after_rect(close_rect);
                        if close_resp.clicked() {
                            state.filter.scope = LibraryScope::Any;
                            state.mark_filter_dirty();
                        }
                        paint_quiet_hover_border(ui, &close_resp);
                    }

                    let blank_width = ui.available_width().max(0.0);
                    if blank_width > 0.0 {
                        let (_, blank_resp) = ui.allocate_exact_size(
                            egui::vec2(blank_width, FILTER_INPUT_HEIGHT),
                            egui::Sense::click(),
                        );
                        if blank_resp.clicked() && !breadcrumb_segment_clicked {
                            path_blank_clicked = true;
                        }
                    }
                } else {
                    ui.label(
                        egui::RichText::new(tr(language, TextKey::NoFolder))
                            .size(theme::FONT_SIZE_BODY)
                            .color(theme::TEXT_SUBTLE),
                    );
                    let blank_width = ui.available_width().max(0.0);
                    if blank_width > 0.0 {
                        let (_, blank_resp) = ui.allocate_exact_size(
                            egui::vec2(blank_width, FILTER_INPUT_HEIGHT),
                            egui::Sense::click(),
                        );
                        if blank_resp.clicked() {
                            path_blank_clicked = true;
                        }
                    }
                }
            },
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // 設定ボタン
            let settings_resp = ui
                .add_sized(
                    TOPBAR_ICON_BUTTON_SIZE,
                    egui::Button::new(icons::icon(icons::ICON_SETTINGS, ICON_SIZE_MENU_SETTINGS))
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(quiet_stroke),
                )
                .on_hover_text(tr(language, TextKey::Settings));
            paint_quiet_hover_border(ui, &settings_resp);
            if settings_resp.clicked() {
                settings_requested = true;
            }

            let prev = state.filter.keyword.clone();
            let placeholder = search_placeholder_text(language, state.current_dir.as_deref());
            let resp = ui
                .push_id("filter_input", |ui| {
                    ui.add_sized(
                        [FILTER_INPUT_WIDTH, FILTER_INPUT_HEIGHT],
                        egui::TextEdit::singleline(&mut state.filter.keyword)
                            .hint_text(placeholder),
                    )
                })
                .inner;
            if state.filter_focus_request {
                resp.request_focus();
                state.filter_focus_request = false;
            }
            ui.label(icons::icon(icons::ICON_SEARCH, ICON_SIZE_SEARCH));
            if !state.filter.keyword.is_empty() {
                let clear_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        resp.rect.right()
                            - FILTER_CLEAR_BUTTON_RIGHT_PADDING
                            - FILTER_CLEAR_BUTTON_SIZE * 0.5,
                        resp.rect.center().y,
                    ),
                    egui::vec2(FILTER_CLEAR_BUTTON_SIZE, FILTER_CLEAR_BUTTON_SIZE),
                );
                let clear_resp = ui.interact(
                    clear_rect,
                    resp.id.with("filter_clear_button"),
                    egui::Sense::click(),
                );
                ui.painter().text(
                    clear_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    icons::ICON_CLOSE.codepoint,
                    egui::FontId::new(ICON_SIZE_FILTER_CLEAR, icons::ICON_CLOSE.font_family()),
                    theme::TEXT_MAIN,
                );
                if clear_resp.clicked() {
                    state.filter.clear_keyword();
                    state.mark_filter_dirty();
                    resp.request_focus();
                }
                paint_quiet_hover_border(ui, &clear_resp);
            }
            state.filter_input_focused = resp.has_focus();
            if resp.changed() || state.filter.keyword != prev {
                state.mark_filter_dirty();
            }

            ui.separator();

            // ── ソートセレクタ ───────────────────────────────────────────
            let mut sort_changed = false;
            let order_label = match state.sort_order {
                SortOrder::Asc => "↑",
                SortOrder::Desc => "↓",
            };
            if ui
                .add_sized(
                    TOPBAR_SORT_BUTTON_SIZE,
                    egui::Button::new(order_label)
                        .fill(theme::BUTTON_ACTIVE)
                        .stroke(selected_stroke),
                )
                .on_hover_text(tr(language, TextKey::ToggleSortOrder))
                .clicked()
            {
                state.sort_order = match state.sort_order {
                    SortOrder::Asc => SortOrder::Desc,
                    SortOrder::Desc => SortOrder::Asc,
                };
                sort_changed = true;
            }
            egui::ComboBox::from_id_salt("sort_combo")
                .selected_text(sort_label(language, &state.sort_key))
                .width(SORT_COMBO_WIDTH)
                .show_ui(ui, |ui| {
                    for key in [SortKey::NameNatural, SortKey::Modified, SortKey::Size] {
                        if ui
                            .selectable_value(
                                &mut state.sort_key,
                                key.clone(),
                                sort_label(language, &key),
                            )
                            .clicked()
                        {
                            sort_changed = true;
                        }
                    }
                });
            ui.label(
                egui::RichText::new(tr(language, TextKey::SortLabel))
                    .size(LABEL_FONT_SIZE)
                    .color(theme::TEXT_SUBTLE),
            );
            if sort_changed {
                state.mark_filter_dirty();
            }

            ui.separator();

            // ── Viewer mode ───────────────────────────────────────────────
            egui::Frame::new()
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0_f32, theme::SEPARATOR_WEAK))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(2, 2))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.horizontal(|ui| {
                        let windowed_selected = *viewer_open_mode == ViewerOpenMode::Windowed;
                        let fullscreen_selected = *viewer_open_mode == ViewerOpenMode::Fullscreen;

                        let windowed_resp = ui.add_sized(
                            TOPBAR_MODE_BUTTON_SIZE,
                            egui::Button::new(tr(language, TextKey::Windowed))
                                .selected(windowed_selected)
                                .fill(if windowed_selected {
                                    theme::BUTTON_ACTIVE
                                } else {
                                    egui::Color32::TRANSPARENT
                                })
                                .stroke(if windowed_selected {
                                    selected_stroke
                                } else {
                                    quiet_stroke
                                }),
                        );
                        if !windowed_selected {
                            paint_quiet_hover_border(ui, &windowed_resp);
                        }
                        if windowed_resp.clicked() && !windowed_selected {
                            *viewer_open_mode = ViewerOpenMode::Windowed;
                            viewer_open_mode_changed = true;
                        }

                        ui.separator();

                        let fullscreen_resp = ui.add_sized(
                            TOPBAR_MODE_BUTTON_SIZE,
                            egui::Button::new(tr(language, TextKey::Fullscreen))
                                .selected(fullscreen_selected)
                                .fill(if fullscreen_selected {
                                    theme::BUTTON_ACTIVE
                                } else {
                                    egui::Color32::TRANSPARENT
                                })
                                .stroke(if fullscreen_selected {
                                    selected_stroke
                                } else {
                                    quiet_stroke
                                }),
                        );
                        if !fullscreen_selected {
                            paint_quiet_hover_border(ui, &fullscreen_resp);
                        }
                        if fullscreen_resp.clicked() && !fullscreen_selected {
                            *viewer_open_mode = ViewerOpenMode::Fullscreen;
                            viewer_open_mode_changed = true;
                        }
                    });
                });
            ui.label(
                egui::RichText::new(tr(language, TextKey::Viewer))
                    .size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_SUBTLE),
            );
            ui.separator();
        });
        ui.separator();
    });

    // Drag & Drop の実処理は app.rs に集約する。
    // topbar は UI だけを担当する。

    TopbarResult {
        scan_dir: None,
        toggle_sidebar,
        settings_requested,
        hud_mode_changed,
        viewer_open_mode_changed,
        nav_back,
        nav_forward,
        nav_up,
        nav_reload,
        breadcrumb_nav,
        path_commit,
        path_cancelled,
        path_blank_clicked,
        path_edit_rect,
    }
}

fn breadcrumb_segments(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut cur = PathBuf::new();
    let mut iter = path.components().peekable();

    if let Some(Component::Prefix(prefix)) = iter.peek().copied() {
        cur.push(prefix.as_os_str());
        iter.next();
        if matches!(iter.peek(), Some(Component::RootDir)) {
            cur.push(std::path::MAIN_SEPARATOR_STR);
            iter.next();
        }
        out.push((
            prefix.as_os_str().to_string_lossy().into_owned(),
            cur.clone(),
        ));
    } else if matches!(iter.peek(), Some(Component::RootDir)) {
        cur.push(std::path::MAIN_SEPARATOR_STR);
        iter.next();
        out.push((std::path::MAIN_SEPARATOR.to_string(), cur.clone()));
    }

    for comp in iter {
        if let Component::Normal(part) = comp {
            cur.push(part);
            out.push((part.to_string_lossy().into_owned(), cur.clone()));
        }
    }

    out
}

fn search_placeholder_text(language: UiLanguage, current_dir: Option<&Path>) -> String {
    current_folder_name(current_dir)
        .map(|name| format!("{name}{}", tr(language, TextKey::SearchInFolder)))
        .unwrap_or_else(|| tr(language, TextKey::SearchAll).to_owned())
}

fn current_folder_name(current_dir: Option<&Path>) -> Option<String> {
    let current_dir = current_dir?;
    let name = current_dir.file_name()?.to_string_lossy().trim().to_owned();
    if name.is_empty() { None } else { Some(name) }
}

fn sort_label(language: UiLanguage, key: &SortKey) -> &'static str {
    match key {
        SortKey::NameNatural => tr(language, TextKey::SortByName),
        SortKey::Modified => tr(language, TextKey::SortByModified),
        SortKey::Size => tr(language, TextKey::SortBySize),
        SortKey::PageCount => tr(language, TextKey::SortByPageCount),
    }
}
