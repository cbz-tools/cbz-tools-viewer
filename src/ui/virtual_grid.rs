//! 仮想スクロール付きのライブラリグリッド。
//!
//! 選択、コンテキストメニュー、キーナビ、ドラッグの入口をこの層に集約し、
//! Library 側の状態更新と分ける。

use std::collections::HashMap;
use std::collections::HashSet;

use eframe::egui::{
    self, pos2, vec2, Color32, CornerRadius, Key, Popup, PopupCloseBehavior, Rect, Sense, Stroke,
    Vec2,
};

use crate::domain::{
    app_settings::{LibraryCardSelectionStyle, LibraryHudMode, LibraryHudStyle, UiLanguage},
    archive::{BookId, BookMeta, FolderMeta, LibraryEntry},
    filename_parser::{parse_filename, FilenamePartRole},
};

use super::{
    common::paint_favorite_star,
    i18n::{tr, TextKey},
    icons,
    library::{BookViewState, ReadingHudState},
    theme,
};

const SELECTION_ACCENT: Color32 = Color32::from_rgb(40, 84, 222); // #2854de

// ── 公開型 ────────────────────────────────────────────────────────────────────

/// Ctrl/Shift クリックの種別
pub enum MultiClick {
    Ctrl(usize),
    Shift(usize),
}

pub enum KeyboardSelection {
    Plain(usize),
    Shift(usize),
}

/// コンテキストメニューからの操作
#[derive(Debug, Clone, PartialEq)]
pub enum ContextAction {
    Open,
    MoveToFolder,
    Rename,
    Delete,
    Copy,
    OpenInExplorer,
    ToggleFavorite,
    ApplyFilterToken(String),
    RunExternalTool(usize),
    SetGroup,
    ClearBookSettings,
}

#[derive(Clone, Debug)]
pub struct ExternalToolMenuItem {
    pub tool_index: usize,
    pub name: String,
    pub shortcut: char,
}

pub struct GridResult {
    /// シングルクリックまたはキーナビによる新選択（修飾キーなし）
    pub selected: Option<KeyboardSelection>,
    /// Ctrl/Shift クリックによる複数選択操作
    pub multi_select: Option<MultiClick>,
    /// 開くべきインデックス（ダブルクリックまたは Enter 時のみ Some）
    pub opened: Option<usize>,
    /// 今フレームの垂直スクロール量（セッション保存用）
    pub scroll_y: f32,
    /// キーナビ後にスクロール追従が必要な場合の目標 offset（次フレームに適用）
    pub request_scroll_y: Option<f32>,
    /// コンテキストメニューからの操作（対象 idx, 操作種別）
    pub context_action: Option<(usize, ContextAction)>,
    /// 選択中ファイルの外部ドラッグ開始
    pub drag_started: Option<usize>,
}

pub struct GridViewContext<'a> {
    pub entries: &'a [LibraryEntry],
    pub book_states: &'a HashMap<BookId, BookViewState>,
    pub selected_idx: Option<usize>,
    pub selected_set: &'a HashSet<usize>,
    pub is_favorite: &'a dyn Fn(&LibraryEntry) -> bool,
    pub reading_hud_state: &'a dyn Fn(&LibraryEntry) -> ReadingHudState,
    pub interaction_enabled: bool,
    pub external_tools: &'a [ExternalToolMenuItem],
    pub external_tool_busy: bool,
    pub language: UiLanguage,
}

pub struct GridViewConfig {
    pub restore_scroll: Option<f32>,
    pub thumb_size: Vec2,
    pub wheel_scroll_multiplier: f32,
    pub hud_mode: LibraryHudMode,
    pub hud_style: LibraryHudStyle,
    pub selection_style: LibraryCardSelectionStyle,
    pub hud_font_size: f32,
    pub reset_context_menu_cache: bool,
}

#[derive(Clone, Copy, Default)]
struct PopupKeyInput {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    esc: bool,
}

#[derive(Default)]
struct GridNavInput {
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    pgup: bool,
    pgdn: bool,
    home: bool,
    end: bool,
    enter: bool,
    shift: bool,
}

#[derive(Clone, Copy)]
struct GridLayout {
    cols: usize,
    rows: usize,
    row_h: f32,
    visible_rows: usize,
}

struct ThumbCellContext<'a> {
    entry: &'a LibraryEntry,
    thumb_state: ThumbCellState<'a>,
    thumb_size: Vec2,
    interaction_enabled: bool,
    popup_keys: PopupKeyInput,
    can_start_external_drag: bool,
}

#[derive(Clone, Copy)]
struct ThumbCellSelectionState {
    is_selected: bool,
    is_in_set: bool,
    is_multi_selection: bool,
}

struct ThumbCellMenuRenderState<'a> {
    open_enabled: bool,
    rename_enabled: bool,
    show_open_menu_item: bool,
    show_rename_menu_item: bool,
    show_open_in_explorer: bool,
    show_context_header: bool,
    context_header: &'a str,
    show_token_menu_frame: bool,
    can_toggle_favorite: bool,
    book_target_count: usize,
    book_settings_target_count: usize,
    external_tools: &'a [ExternalToolMenuItem],
    external_tool_busy: bool,
    is_favorite: bool,
}

// ── show_grid ─────────────────────────────────────────────────────────────────

/// エントリ一覧をグリッドで描画する。
///
/// - `selected_idx`  : 現在の主選択インデックス（選択枠描画用）
/// - `selected_set`  : 複数選択セット（Ctrl/Shift 選択）
/// - `restore_scroll`: セッション復元や選択追従のスクロール offset（Some の場合に適用）
/// - `thumb_size`    : サムネイルサイズ（設定変更で可変）
pub fn show_grid(
    ui: &mut egui::Ui,
    context: GridViewContext<'_>,
    config: GridViewConfig,
) -> GridResult {
    let GridViewContext {
        entries,
        book_states,
        selected_idx,
        selected_set,
        is_favorite,
        reading_hud_state,
        interaction_enabled,
        external_tools,
        external_tool_busy,
        language,
    } = context;
    let GridViewConfig {
        restore_scroll,
        thumb_size,
        wheel_scroll_multiplier,
        hud_mode,
        hud_style,
        selection_style,
        hud_font_size,
        reset_context_menu_cache,
    } = config;
    let gap = theme::GRID_GAP;
    let (popup_open_state_id, popup_input_blocked, popup_keys) =
        take_popup_key_input(ui, reset_context_menu_cache);
    let layout = build_grid_layout(ui, entries.len(), thumb_size, hud_mode, gap);
    let nav_input = take_grid_navigation_input(
        ui,
        interaction_enabled,
        popup_input_blocked,
        !entries.is_empty(),
    );

    let count = entries.len();
    let any_nav = nav_input.left
        || nav_input.right
        || nav_input.up
        || nav_input.down
        || nav_input.pgup
        || nav_input.pgdn
        || nav_input.home
        || nav_input.end;

    let key_selected: Option<KeyboardSelection> = if any_nav && count > 0 {
        let sel = selected_idx.unwrap_or(0);
        let new_sel = if nav_input.right { (sel + 1).min(count - 1) }
            else if nav_input.left  { sel.saturating_sub(1) }
            else if nav_input.down  { (sel + layout.cols).min(count - 1) }
            else if nav_input.up    { sel.saturating_sub(layout.cols) }
            else if nav_input.pgdn  { (sel + layout.cols * layout.visible_rows).min(count - 1) }
            else if nav_input.pgup  { sel.saturating_sub(layout.cols * layout.visible_rows) }
            else if nav_input.end   { count - 1 }
            else                    { 0 }  // home
        ;
        Some(if nav_input.shift {
            KeyboardSelection::Shift(new_sel)
        } else {
            KeyboardSelection::Plain(new_sel)
        })
    } else {
        None
    };

    let keyboard_opened: Option<usize> = if nav_input.enter {
        let idx = key_selected
            .as_ref()
            .map(|sel| match sel {
                KeyboardSelection::Plain(idx) | KeyboardSelection::Shift(idx) => *idx,
            })
            .or(selected_idx);
        idx.filter(|idx| is_entry_openable(entries, book_states, *idx))
    } else {
        None
    };

    // ── グリッド描画 ──────────────────────────────────────────────────────────
    let mut click_selected: Option<usize> = None;
    let mut click_ctrl: Option<usize> = None;
    let mut click_shift: Option<usize> = None;
    let mut click_opened: Option<usize> = None;
    let mut ctx_action: Option<(usize, ContextAction)> = None;
    let mut drag_started: Option<usize> = None;
    let mut any_context_menu_open = false;

    let mut sa = egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .wheel_scroll_multiplier(vec2(1.0, wheel_scroll_multiplier.max(0.0)));
    if let Some(y) = restore_scroll {
        sa = sa.vertical_scroll_offset(y);
    }

    let cell_menu_context = CellMenuContext {
        entries,
        book_states,
        selected_idx,
        selected_set,
        interaction_enabled,
        language,
    };

    let scroll_out = sa.show_rows(ui, layout.row_h, layout.rows, |ui, row_range| {
        for row in row_range {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for col in 0..layout.cols {
                    let idx = row * layout.cols + col;
                    if let Some(entry) = entries.get(idx) {
                        let is_primary = selected_idx == Some(idx);
                        let is_in_set = selected_set.contains(&idx);

                        let menu_state = build_cell_menu_state(&cell_menu_context, idx, entry);
                        let can_toggle_favorite = entry.is_favorite_target();
                        let favorite_state = is_favorite(entry);
                        let reading_state = reading_hud_state(entry);

                        let thumb_state = thumb_cell_state(entry, book_states);
                        let action = draw_thumb_cell(
                            ui,
                            ThumbCellContext {
                                entry,
                                thumb_state,
                                thumb_size,
                                interaction_enabled,
                                popup_keys,
                                can_start_external_drag: menu_state.can_start_external_drag,
                            },
                            ThumbCellSelectionState {
                                is_selected: is_primary,
                                is_in_set,
                                is_multi_selection: menu_state.is_multi_selection,
                            },
                            ThumbCellMenuRenderState {
                                open_enabled: menu_state.open_enabled,
                                rename_enabled: menu_state.rename_enabled,
                                show_open_menu_item: menu_state.show_open_menu_item,
                                show_rename_menu_item: menu_state.show_rename_menu_item,
                                show_open_in_explorer: menu_state.show_open_in_explorer,
                                show_context_header: menu_state.show_context_header,
                                context_header: &menu_state.context_header,
                                show_token_menu_frame: menu_state.show_token_menu_frame,
                                can_toggle_favorite,
                                book_target_count: menu_state.book_target_count,
                                book_settings_target_count: menu_state.book_settings_target_count,
                                external_tools,
                                external_tool_busy,
                                is_favorite: favorite_state,
                            },
                            ThumbCellRenderState {
                                hud_mode,
                                hud_style,
                                selection_style,
                                hud_font_size,
                                hud_selected: is_primary || is_in_set,
                                reading_state,
                                is_favorite: favorite_state,
                                language,
                            },
                        );

                        if action.plain_click {
                            click_selected = Some(idx);
                        }
                        if action.ctrl_click {
                            click_ctrl = Some(idx);
                        }
                        if action.shift_click {
                            click_shift = Some(idx);
                        }
                        if action.double_click {
                            click_opened = Some(idx);
                        }
                        if action.drag_started {
                            drag_started = Some(idx);
                        }
                        if action.context_menu_open {
                            any_context_menu_open = true;
                        }

                        if ctx_action.is_none() {
                            ctx_action = context_action_from_cell_action(idx, &action);
                        }
                    }
                }
            });
        }
    });

    // ── スクロール追従 ────────────────────────────────────────────────────────
    let request_scroll_y = compute_request_scroll_y(
        key_selected.as_ref(),
        layout.cols,
        layout.row_h,
        scroll_out.state.offset.y,
        scroll_out.inner_rect.height(),
    );

    assemble_grid_result(
        ui,
        popup_open_state_id,
        any_context_menu_open,
        GridResultParts {
            click_selected,
            click_ctrl,
            click_shift,
            click_opened,
            key_selected,
            keyboard_opened,
            scroll_y: scroll_out.state.offset.y,
            request_scroll_y,
            ctx_action,
            drag_started,
        },
    )
}

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum ThumbCellState<'a> {
    Folder,
    Loading,
    Ready(&'a egui::TextureHandle),
    Failed,
}

impl ThumbCellState<'_> {
    fn is_ready(self) -> bool {
        matches!(self, ThumbCellState::Ready(_))
    }
}

fn thumb_cell_state<'a>(
    entry: &LibraryEntry,
    book_states: &'a HashMap<BookId, BookViewState>,
) -> ThumbCellState<'a> {
    match entry {
        LibraryEntry::Folder(_) => ThumbCellState::Folder,
        LibraryEntry::Archive(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => {
            let Some(book_id) = entry.thumb_id() else {
                return ThumbCellState::Loading;
            };
            let Some(state) = book_states.get(&book_id) else {
                return ThumbCellState::Loading;
            };
            if let Some(tex) = state.texture.as_ref() {
                ThumbCellState::Ready(tex)
            } else if state.thumb_failed {
                ThumbCellState::Failed
            } else {
                ThumbCellState::Loading
            }
        }
    }
}

fn is_entry_openable(
    entries: &[LibraryEntry],
    book_states: &HashMap<BookId, BookViewState>,
    idx: usize,
) -> bool {
    entries
        .get(idx)
        .map(|entry| match entry {
            LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => {
                true
            }
            LibraryEntry::Archive(entry) => book_states
                .get(&entry.id)
                .is_some_and(|state| state.texture.is_some() && !state.thumb_failed),
        })
        .unwrap_or(false)
}

struct CellAction {
    plain_click: bool,
    ctrl_click: bool,
    shift_click: bool,
    double_click: bool,
    drag_started: bool,
    ctx_open: bool,
    ctx_move_to_folder: bool,
    ctx_rename: bool,
    ctx_delete: bool,
    ctx_copy: bool,
    ctx_open_in_explorer: bool,
    ctx_toggle_favorite: bool,
    ctx_set_group: bool,
    ctx_clear_book_settings: bool,
    ctx_external_tool: Option<usize>,
    filter_token: Option<String>,
    context_menu_open: bool,
}

struct CellMenuState {
    book_target_count: usize,
    book_settings_target_count: usize,
    rename_enabled: bool,
    can_start_external_drag: bool,
    show_token_menu_frame: bool,
    is_multi_selection: bool,
    open_enabled: bool,
    show_open_menu_item: bool,
    show_rename_menu_item: bool,
    show_open_in_explorer: bool,
    show_context_header: bool,
    context_header: String,
}

struct CellMenuContext<'a> {
    entries: &'a [LibraryEntry],
    book_states: &'a HashMap<BookId, BookViewState>,
    selected_idx: Option<usize>,
    selected_set: &'a HashSet<usize>,
    interaction_enabled: bool,
    language: UiLanguage,
}

#[derive(Clone, Copy)]
struct ThumbCellRenderState {
    hud_mode: LibraryHudMode,
    hud_style: LibraryHudStyle,
    selection_style: LibraryCardSelectionStyle,
    hud_font_size: f32,
    hud_selected: bool,
    reading_state: ReadingHudState,
    is_favorite: bool,
    language: UiLanguage,
}

#[derive(Default)]
struct ContextMenuSectionState {
    needs_separator: bool,
}

struct CellClickState {
    modifiers: egui::Modifiers,
    clicked: bool,
    double_click: bool,
    drag_started: bool,
}

struct GridResultParts {
    click_selected: Option<usize>,
    click_ctrl: Option<usize>,
    click_shift: Option<usize>,
    click_opened: Option<usize>,
    key_selected: Option<KeyboardSelection>,
    keyboard_opened: Option<usize>,
    scroll_y: f32,
    request_scroll_y: Option<f32>,
    ctx_action: Option<(usize, ContextAction)>,
    drag_started: Option<usize>,
}

#[derive(Default)]
struct CellContextMenuActions {
    open: bool,
    move_to_folder: bool,
    rename: bool,
    delete: bool,
    copy: bool,
    open_in_explorer: bool,
    toggle_favorite: bool,
    set_group: bool,
    clear_book_settings: bool,
    external_tool: Option<usize>,
    filter_token: Option<String>,
}

struct ContextMenuRenderContext<'a> {
    entry: &'a LibraryEntry,
    language: UiLanguage,
    popup_keys: PopupKeyInput,
}

struct OpenSectionState {
    show_open_menu_item: bool,
    open_enabled: bool,
    show_open_in_explorer: bool,
}

struct FavoriteGroupSectionState {
    can_toggle_favorite: bool,
    is_multi_selection: bool,
    is_favorite: bool,
    book_target_count: usize,
}

fn build_cell_menu_state(
    context: &CellMenuContext<'_>,
    idx: usize,
    entry: &LibraryEntry,
) -> CellMenuState {
    let is_primary = context.selected_idx == Some(idx);
    let is_in_set = context.selected_set.contains(&idx);
    let cell_in_selection = is_primary || is_in_set;
    let selection_count = context.selected_set.len()
        + context
            .selected_idx
            .map_or(0, |si| usize::from(!context.selected_set.contains(&si)));
    let rename_enabled = if cell_in_selection {
        selection_count <= 1
    } else {
        true
    };
    let can_start_external_drag =
        context.interaction_enabled && cell_in_selection && selection_count >= 1;
    let is_archive = matches!(entry, LibraryEntry::Archive(_));
    let show_token_menu_frame = context.interaction_enabled
        && if cell_in_selection {
            selection_count == 1
        } else {
            true
        }
        && is_archive
        && !entry_title(entry).trim().is_empty();
    let context_targets = if cell_in_selection {
        collect_context_targets(context.selected_idx, context.selected_set)
    } else {
        vec![idx]
    };
    let book_target_count = context_targets
        .iter()
        .filter(|target_idx| {
            matches!(
                context.entries.get(**target_idx),
                Some(LibraryEntry::Archive(_))
            )
        })
        .count();
    let book_settings_target_count = context_targets
        .iter()
        .filter(|target_idx| {
            matches!(
                context.entries.get(**target_idx),
                Some(LibraryEntry::Archive(_) | LibraryEntry::FolderBook(_))
            )
        })
        .count();
    let is_multi_selection = selection_count > 1;
    let show_context_header = selection_count > 1 && cell_in_selection;
    let context_header = if show_context_header {
        tr(context.language, TextKey::SelectedCount).replacen("{}", &selection_count.to_string(), 1)
    } else {
        String::new()
    };

    CellMenuState {
        book_target_count,
        book_settings_target_count,
        rename_enabled,
        can_start_external_drag,
        show_token_menu_frame,
        is_multi_selection,
        open_enabled: !is_multi_selection
            && is_entry_openable(context.entries, context.book_states, idx),
        show_open_menu_item: !is_multi_selection,
        show_rename_menu_item: !is_multi_selection && is_archive,
        show_open_in_explorer: !is_multi_selection,
        show_context_header,
        context_header,
    }
}

fn collect_context_targets(
    selected_idx: Option<usize>,
    selected_set: &HashSet<usize>,
) -> Vec<usize> {
    let mut targets: Vec<usize> = selected_set.iter().copied().collect();
    if let Some(primary) = selected_idx {
        if !selected_set.contains(&primary) {
            targets.push(primary);
        }
    }
    targets.sort_unstable();
    targets.dedup();
    targets
}

fn context_action_from_cell_action(
    idx: usize,
    action: &CellAction,
) -> Option<(usize, ContextAction)> {
    if let Some(token) = action.filter_token.clone() {
        Some((idx, ContextAction::ApplyFilterToken(token)))
    } else if action.ctx_open {
        Some((idx, ContextAction::Open))
    } else if action.ctx_move_to_folder {
        Some((idx, ContextAction::MoveToFolder))
    } else if action.ctx_rename {
        Some((idx, ContextAction::Rename))
    } else if action.ctx_delete {
        Some((idx, ContextAction::Delete))
    } else if action.ctx_copy {
        Some((idx, ContextAction::Copy))
    } else if action.ctx_open_in_explorer {
        Some((idx, ContextAction::OpenInExplorer))
    } else if action.ctx_toggle_favorite {
        Some((idx, ContextAction::ToggleFavorite))
    } else if action.ctx_set_group {
        Some((idx, ContextAction::SetGroup))
    } else if action.ctx_clear_book_settings {
        Some((idx, ContextAction::ClearBookSettings))
    } else {
        action
            .ctx_external_tool
            .map(|tool_index| (idx, ContextAction::RunExternalTool(tool_index)))
    }
}

fn take_popup_key_input(
    ui: &mut egui::Ui,
    reset_context_menu_cache: bool,
) -> (egui::Id, bool, PopupKeyInput) {
    let popup_open_state_id = ui.id().with("virtual-grid-context-menu-open");
    if reset_context_menu_cache {
        ui.ctx().data_mut(|data| {
            data.insert_temp(popup_open_state_id, false);
        });
    }
    let was_context_menu_open = ui
        .ctx()
        .data_mut(|data| data.get_temp::<bool>(popup_open_state_id))
        .unwrap_or(false);
    let popup_input_blocked = ui.ctx().any_popup_open() || was_context_menu_open;
    let popup_keys = if popup_input_blocked {
        ui.input_mut(|i| PopupKeyInput {
            up: i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            down: i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            left: i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft),
            right: i.consume_key(egui::Modifiers::NONE, Key::ArrowRight),
            esc: i.consume_key(egui::Modifiers::NONE, Key::Escape),
        })
    } else {
        PopupKeyInput::default()
    };
    (popup_open_state_id, popup_input_blocked, popup_keys)
}

fn build_grid_layout(
    ui: &egui::Ui,
    entry_count: usize,
    thumb_size: Vec2,
    hud_mode: LibraryHudMode,
    gap: f32,
) -> GridLayout {
    let cell_size = grid_cell_size(thumb_size, hud_mode);
    let avail = ui.available_width();
    let cols = ((avail + gap) / (cell_size.x + gap)).floor().max(1.0) as usize;
    let rows = entry_count.div_ceil(cols);
    let row_h = cell_size.y + gap;
    let visible_rows = (ui.available_height() / row_h).floor().max(1.0) as usize;

    GridLayout {
        cols,
        rows,
        row_h,
        visible_rows,
    }
}

fn take_grid_navigation_input(
    ui: &egui::Ui,
    interaction_enabled: bool,
    popup_input_blocked: bool,
    has_entries: bool,
) -> GridNavInput {
    let has_text_focus = ui.ctx().memory(|m| m.focused().is_some());
    if interaction_enabled && !has_text_focus && has_entries && !popup_input_blocked {
        ui.input(|i| GridNavInput {
            left: i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::A),
            right: i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::D),
            up: i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::W),
            down: i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::S),
            pgup: i.key_pressed(Key::PageUp),
            pgdn: i.key_pressed(Key::PageDown),
            home: i.key_pressed(Key::Home),
            end: i.key_pressed(Key::End),
            enter: i.key_pressed(Key::Enter),
            shift: i.modifiers.shift,
        })
    } else {
        GridNavInput::default()
    }
}

fn compute_request_scroll_y(
    key_selected: Option<&KeyboardSelection>,
    cols: usize,
    row_h: f32,
    cur_offset: f32,
    inner_height: f32,
) -> Option<f32> {
    let new_sel = key_selected.map(|sel| match sel {
        KeyboardSelection::Plain(idx) | KeyboardSelection::Shift(idx) => *idx,
    })?;
    let sel_row = new_sel / cols;
    let sel_y_top = sel_row as f32 * row_h;
    let sel_y_bot = sel_y_top + row_h;
    let visible_h = inner_height.max(row_h);

    if sel_y_top < cur_offset {
        Some(sel_y_top)
    } else if sel_y_bot > cur_offset + visible_h {
        Some((sel_y_bot - visible_h).max(0.0))
    } else {
        None
    }
}

fn assemble_grid_result(
    ui: &egui::Ui,
    popup_open_state_id: egui::Id,
    any_context_menu_open: bool,
    parts: GridResultParts,
) -> GridResult {
    let multi_select = parts
        .click_ctrl
        .map(MultiClick::Ctrl)
        .or_else(|| parts.click_shift.map(MultiClick::Shift));
    ui.ctx()
        .data_mut(|data| data.insert_temp(popup_open_state_id, any_context_menu_open));

    GridResult {
        selected: parts
            .click_selected
            .map(KeyboardSelection::Plain)
            .or(parts.key_selected),
        multi_select,
        opened: parts.click_opened.or(parts.keyboard_opened),
        scroll_y: parts.scroll_y,
        request_scroll_y: parts.request_scroll_y,
        context_action: parts.ctx_action,
        drag_started: parts.drag_started,
    }
}

/// セルを描画し CellAction を返す。
fn draw_thumb_cell(
    ui: &mut egui::Ui,
    cell: ThumbCellContext<'_>,
    selection: ThumbCellSelectionState,
    menu: ThumbCellMenuRenderState<'_>,
    render_state: ThumbCellRenderState,
) -> CellAction {
    let sense = if cell.interaction_enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let cell_size = grid_cell_size(cell.thumb_size, render_state.hud_mode);
    let (rect, resp) = ui.allocate_exact_size(cell_size, sense);

    // ── コンテキストメニュー ──────────────────────────────────────────────────
    let mut menu_actions = CellContextMenuActions::default();
    let mut context_menu_open = false;

    if cell.interaction_enabled {
        let menu_context = ContextMenuRenderContext {
            entry: cell.entry,
            language: render_state.language,
            popup_keys: cell.popup_keys,
        };
        Popup::context_menu(&resp)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let old_item_spacing = ui.spacing().item_spacing;
                let old_padding = ui.spacing().button_padding;
                let old_interact_size = ui.spacing().interact_size;
                ui.spacing_mut().item_spacing =
                    egui::vec2(old_item_spacing.x, old_item_spacing.y + 2.0);
                ui.spacing_mut().button_padding =
                    egui::vec2(old_padding.x + 2.0, old_padding.y + 2.0);
                ui.spacing_mut().interact_size.y = old_interact_size.y + 4.0;

                let old_separator = ui.visuals().widgets.noninteractive.bg_stroke;
                ui.visuals_mut().widgets.noninteractive.bg_stroke =
                    egui::Stroke::new(1.0, theme::SEPARATOR_WEAK);

                egui::Frame::new()
                    .fill(theme::SURFACE_BG)
                    .stroke(egui::Stroke::new(1.0, theme::SEPARATOR_WEAK))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        render_context_menu_header(
                            ui,
                            &menu_context,
                            menu.show_context_header,
                            menu.context_header,
                            menu.show_token_menu_frame,
                            &mut menu_actions,
                        );

                        let show_external_tools_section =
                            !menu.external_tools.is_empty() && menu.book_target_count >= 1;
                        let mut section_state = ContextMenuSectionState::default();

                        render_context_menu_open_section(
                            ui,
                            &menu_context,
                            OpenSectionState {
                                show_open_menu_item: menu.show_open_menu_item,
                                open_enabled: menu.open_enabled,
                                show_open_in_explorer: menu.show_open_in_explorer,
                            },
                            &mut section_state,
                            &mut menu_actions,
                        );
                        render_context_menu_favorite_group_section(
                            ui,
                            &menu_context,
                            FavoriteGroupSectionState {
                                can_toggle_favorite: menu.can_toggle_favorite,
                                is_multi_selection: selection.is_multi_selection,
                                is_favorite: menu.is_favorite,
                                book_target_count: menu.book_target_count,
                            },
                            &mut section_state,
                            &mut menu_actions,
                        );
                        render_context_menu_clear_settings_section(
                            ui,
                            menu.book_settings_target_count,
                            &mut section_state,
                            &mut menu_actions,
                            render_state.language,
                        );
                        render_context_menu_rename_copy_delete_section(
                            ui,
                            selection.is_multi_selection,
                            menu.show_rename_menu_item,
                            menu.rename_enabled,
                            &mut section_state,
                            &mut menu_actions,
                            render_state.language,
                        );
                        render_context_menu_external_tools_section(
                            ui,
                            menu.external_tools,
                            menu.external_tool_busy,
                            show_external_tools_section,
                            &mut section_state,
                            &mut menu_actions,
                            render_state.language,
                        );
                    });

                ui.visuals_mut().widgets.noninteractive.bg_stroke = old_separator;
            });
        context_menu_open = resp.context_menu_opened();
    }

    if !ui.is_rect_visible(rect) {
        let click_state = read_cell_click_state(ui, &resp);
        return build_cell_action(
            cell.entry,
            cell.thumb_state,
            cell.can_start_external_drag,
            click_state,
            menu_actions,
            context_menu_open,
            true,
        );
    }

    let painter = ui.painter();

    // ── 背景 ─────────────────────────────────────────────────────────────────
    painter.rect_filled(rect, CornerRadius::same(5), theme::PLACEHOLDER_BG);

    // 選択枠は「実画像」ではなく、固定サムネイル表示エリアに合わせる。
    // これにより縦長/横長画像でも選択枠サイズが揺れない。
    let thumb_rect = Rect::from_min_size(rect.min, cell.thumb_size);
    let selection_rect = Rect::from_min_max(
        pos2(thumb_rect.min.x - 3.0, thumb_rect.min.y - 3.0),
        pos2(thumb_rect.max.x + 3.0, thumb_rect.max.y + 3.0),
    );
    let selection_rounding = CornerRadius::same(5);
    let selected_or_in_set = selection.is_selected || selection.is_in_set;

    let folder_cell = render_thumb_cell_visual(
        painter,
        thumb_rect,
        cell.entry,
        cell.thumb_state,
        render_state,
    );

    // ── HUD ──────────────────────────────────────────────────────────────────
    if !folder_cell {
        render_thumb_cell_hud(painter, thumb_rect, cell.entry, render_state);
    }

    // ── 選択枠 / 複数選択枠 / ホバー枠 ──────────────────────────────────────
    // 選択状態を見やすくするため、セルサイズは変えずに固定サムネイル表示エリアへ枠を描く。
    // 実画像サイズに追従させないことで、表紙縦横比による枠サイズの揺れを避ける。
    draw_cell_selection_border(
        painter,
        selection_rect,
        selection_rounding,
        render_state.selection_style,
        selected_or_in_set,
        resp.hovered(),
    );

    // ── クリック判定 ─────────────────────────────────────────────────────────
    let click_state = read_cell_click_state(ui, &resp);

    build_cell_action(
        cell.entry,
        cell.thumb_state,
        cell.can_start_external_drag,
        click_state,
        menu_actions,
        context_menu_open,
        false,
    )
}

// ── ヘルパー ──────────────────────────────────────────────────────────────────

fn grid_cell_size(thumb_size: Vec2, _hud_mode: LibraryHudMode) -> Vec2 {
    // HUD はすべてサムネイル上のオーバーレイとして描画する。
    // セル高さを変えないことで、行数・スクロール位置・仮想グリッド計算を安定させる。
    thumb_size
}

fn render_context_menu_header(
    ui: &mut egui::Ui,
    context: &ContextMenuRenderContext<'_>,
    show_context_header: bool,
    context_header: &str,
    show_token_menu_frame: bool,
    menu_actions: &mut CellContextMenuActions,
) {
    if show_context_header {
        ui.label(
            egui::RichText::new(context_header)
                .color(theme::TEXT_SUBTLE)
                .size(theme::FONT_SIZE_SMALL),
        );
        ui.separator();
    }

    if show_token_menu_frame {
        if let LibraryEntry::Archive(entry) = context.entry {
            menu_actions.filter_token =
                show_filename_token_menu_frame(ui, entry, context.popup_keys, context.language);
        }
        ui.separator();
    }
}

fn begin_context_menu_section(ui: &mut egui::Ui, section_state: &mut ContextMenuSectionState) {
    if section_state.needs_separator {
        ui.separator();
    } else {
        section_state.needs_separator = true;
    }
}

fn render_context_menu_open_section(
    ui: &mut egui::Ui,
    context: &ContextMenuRenderContext<'_>,
    section: OpenSectionState,
    section_state: &mut ContextMenuSectionState,
    menu_actions: &mut CellContextMenuActions,
) {
    if !section.show_open_menu_item {
        return;
    }

    begin_context_menu_section(ui, section_state);
    let open_icon = match context.entry {
        LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) => icons::ICON_FOLDER_OPEN,
        LibraryEntry::Archive(_) | LibraryEntry::ImageFile(_) => icons::ICON_FILE_OPEN,
    };
    if context_menu_item(
        ui,
        open_icon,
        tr(context.language, TextKey::Open),
        Some("Enter"),
        section.open_enabled,
        false,
    ) {
        menu_actions.open = true;
        ui.close();
    }
    if section.show_open_in_explorer
        && context_menu_item(
            ui,
            icons::ICON_FOLDER_OPEN,
            tr(context.language, TextKey::OpenInExplorer),
            None,
            true,
            false,
        )
    {
        menu_actions.open_in_explorer = true;
        ui.close();
    }
    if matches!(context.entry, LibraryEntry::FolderBook(_))
        && context_menu_item(
            ui,
            icons::ICON_FOLDER,
            tr(context.language, TextKey::MoveToFolder),
            None,
            true,
            false,
        )
    {
        menu_actions.move_to_folder = true;
        ui.close();
    }
}

fn render_context_menu_favorite_group_section(
    ui: &mut egui::Ui,
    context: &ContextMenuRenderContext<'_>,
    section: FavoriteGroupSectionState,
    section_state: &mut ContextMenuSectionState,
    menu_actions: &mut CellContextMenuActions,
) {
    if section.can_toggle_favorite && !section.is_multi_selection {
        begin_context_menu_section(ui, section_state);
        let favorite_icon = if section.is_favorite {
            icons::ICON_STAR
        } else {
            icons::ICON_STAR_BORDER.outlined()
        };
        if context_menu_item(
            ui,
            favorite_icon,
            if section.is_favorite {
                tr(context.language, TextKey::RemoveFromFavorites)
            } else {
                tr(context.language, TextKey::AddToFavorites)
            },
            None,
            true,
            false,
        ) {
            menu_actions.toggle_favorite = true;
            ui.close();
        }
        if section.book_target_count >= 1
            && context_menu_item(
                ui,
                icons::ICON_LABEL,
                tr(context.language, TextKey::SetGroup),
                None,
                true,
                false,
            )
        {
            menu_actions.set_group = true;
            ui.close();
        }
    } else if section.book_target_count >= 1 {
        begin_context_menu_section(ui, section_state);
        if context_menu_item(
            ui,
            icons::ICON_LABEL,
            tr(context.language, TextKey::SetGroup),
            None,
            true,
            false,
        ) {
            menu_actions.set_group = true;
            ui.close();
        }
    }
}

fn render_context_menu_clear_settings_section(
    ui: &mut egui::Ui,
    book_settings_target_count: usize,
    section_state: &mut ContextMenuSectionState,
    menu_actions: &mut CellContextMenuActions,
    language: UiLanguage,
) {
    if book_settings_target_count < 1 {
        return;
    }

    begin_context_menu_section(ui, section_state);
    if context_menu_item(
        ui,
        icons::ICON_REFRESH,
        tr(language, TextKey::ClearBookSettings),
        None,
        true,
        false,
    ) {
        menu_actions.clear_book_settings = true;
        ui.close();
    }
}

fn render_context_menu_rename_copy_delete_section(
    ui: &mut egui::Ui,
    is_multi_selection: bool,
    show_rename_menu_item: bool,
    rename_enabled: bool,
    section_state: &mut ContextMenuSectionState,
    menu_actions: &mut CellContextMenuActions,
    language: UiLanguage,
) {
    begin_context_menu_section(ui, section_state);
    if !is_multi_selection && show_rename_menu_item {
        if context_menu_item(
            ui,
            icons::ICON_EDIT,
            tr(language, TextKey::Rename),
            Some("F2"),
            rename_enabled,
            false,
        ) {
            menu_actions.rename = true;
            ui.close();
        }
        if !rename_enabled {
            ui.label(
                egui::RichText::new(tr(language, TextKey::MultipleSelectionUnavailable))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_DISABLED),
            );
        }
    }
    if context_menu_item(
        ui,
        icons::ICON_CONTENT_COPY,
        tr(language, TextKey::Copy),
        Some("Ctrl+C"),
        true,
        false,
    ) {
        menu_actions.copy = true;
        ui.close();
    }
    if context_menu_item(
        ui,
        icons::ICON_DELETE,
        tr(language, TextKey::Delete),
        Some("Del"),
        true,
        true,
    ) {
        menu_actions.delete = true;
        ui.close();
    }
}

fn render_context_menu_external_tools_section(
    ui: &mut egui::Ui,
    external_tools: &[ExternalToolMenuItem],
    external_tool_busy: bool,
    show_external_tools_section: bool,
    section_state: &mut ContextMenuSectionState,
    menu_actions: &mut CellContextMenuActions,
    language: UiLanguage,
) {
    if !show_external_tools_section {
        return;
    }

    begin_context_menu_section(ui, section_state);
    ui.add_enabled_ui(!external_tool_busy, |ui| {
        ui.menu_button(tr(language, TextKey::ExternalToolsMenu), |ui| {
            ui.set_min_width(200.0);
            ui.set_max_width(200.0);
            for tool in external_tools {
                let shortcut = tool.shortcut.to_string();
                if external_tool_menu_item(ui, &tool.name, shortcut.as_str(), true) {
                    menu_actions.external_tool = Some(tool.tool_index);
                    ui.close();
                }
            }
        });
    });
}

fn read_cell_click_state(ui: &egui::Ui, resp: &egui::Response) -> CellClickState {
    CellClickState {
        modifiers: ui.input(|i| i.modifiers),
        clicked: resp.clicked(),
        double_click: resp.double_clicked(),
        drag_started: resp.drag_started(),
    }
}

fn build_cell_action(
    entry: &LibraryEntry,
    thumb_state: ThumbCellState<'_>,
    can_start_external_drag: bool,
    click_state: CellClickState,
    menu_actions: CellContextMenuActions,
    context_menu_open: bool,
    allow_image_file_double_click: bool,
) -> CellAction {
    CellAction {
        plain_click: click_state.clicked
            && !click_state.modifiers.ctrl
            && !click_state.modifiers.shift
            && !click_state.double_click,
        ctrl_click: click_state.clicked && click_state.modifiers.ctrl && !click_state.double_click,
        shift_click: click_state.clicked
            && click_state.modifiers.shift
            && !click_state.double_click,
        double_click: (click_state.double_click
            && (thumb_state.is_ready()
                || matches!(entry, LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_))))
            || (allow_image_file_double_click
                && click_state.double_click
                && matches!(entry, LibraryEntry::ImageFile(_))),
        drag_started: click_state.drag_started && can_start_external_drag,
        ctx_open: menu_actions.open,
        ctx_move_to_folder: menu_actions.move_to_folder,
        ctx_rename: menu_actions.rename,
        ctx_delete: menu_actions.delete,
        ctx_copy: menu_actions.copy,
        ctx_open_in_explorer: menu_actions.open_in_explorer,
        ctx_toggle_favorite: menu_actions.toggle_favorite,
        ctx_set_group: menu_actions.set_group,
        ctx_clear_book_settings: menu_actions.clear_book_settings,
        ctx_external_tool: menu_actions.external_tool,
        filter_token: menu_actions.filter_token,
        context_menu_open,
    }
}

fn draw_cell_selection_border(
    painter: &egui::Painter,
    selection_rect: Rect,
    selection_rounding: CornerRadius,
    selection_style: LibraryCardSelectionStyle,
    selected_or_in_set: bool,
    hovered: bool,
) {
    if selected_or_in_set {
        let palette = card_selection_palette(selection_style);
        painter.rect_stroke(
            selection_rect,
            selection_rounding,
            Stroke::new(6.0, palette.border),
            egui::StrokeKind::Inside,
        );
    } else if hovered {
        painter.rect_stroke(
            selection_rect,
            selection_rounding,
            Stroke::new(2.0, theme::ACCENT.linear_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
    }
}

fn render_thumb_cell_visual(
    painter: &egui::Painter,
    thumb_rect: Rect,
    entry: &LibraryEntry,
    thumb_state: ThumbCellState<'_>,
    render_state: ThumbCellRenderState,
) -> bool {
    match thumb_state {
        ThumbCellState::Folder => {
            let palette = hud_overlay_palette(
                render_state.hud_style,
                render_state.hud_selected,
                render_state.selection_style,
                185,
            );
            draw_status_icon(
                painter,
                thumb_rect,
                icons::ICON_FOLDER,
                (status_icon_size(thumb_rect) * 1.25).min(72.0),
                theme::TEXT_SUBTLE,
            );
            draw_multi_line_title_overlay(
                painter,
                thumb_rect,
                entry_title(entry),
                render_state.hud_font_size,
                palette,
            );
            true
        }
        ThumbCellState::Ready(tex) => {
            let fit = aspect_fit(tex.size_vec2(), thumb_rect);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(tex.id(), fit, uv, Color32::WHITE);
            false
        }
        ThumbCellState::Loading => {
            draw_status_icon(
                painter,
                thumb_rect,
                icons::ICON_SYNC,
                status_icon_size(thumb_rect),
                theme::TEXT_SUBTLE,
            );
            false
        }
        ThumbCellState::Failed => {
            draw_status_icon(
                painter,
                thumb_rect,
                icons::ICON_BROKEN_IMAGE,
                status_icon_size(thumb_rect),
                theme::TEXT_SUBTLE,
            );
            false
        }
    }
}

fn render_thumb_cell_hud(
    painter: &egui::Painter,
    thumb_rect: Rect,
    entry: &LibraryEntry,
    render_state: ThumbCellRenderState,
) {
    match render_state.hud_mode {
        LibraryHudMode::Off => {}
        LibraryHudMode::On => {
            let badge_palette = hud_overlay_palette(
                render_state.hud_style,
                render_state.hud_selected,
                render_state.selection_style,
                165,
            );
            let title_palette = hud_overlay_palette(
                render_state.hud_style,
                render_state.hud_selected,
                render_state.selection_style,
                185,
            );
            if let Some(badge) = thumb_hud_badge(entry) {
                draw_size_badge(
                    painter,
                    thumb_rect,
                    badge,
                    render_state.hud_font_size,
                    badge_palette,
                );
            }
            if let Some(label) =
                reading_hud_label(render_state.reading_state, render_state.language)
            {
                draw_hud_badge(
                    painter,
                    thumb_rect,
                    &[label],
                    render_state.is_favorite,
                    BadgeAnchor::TopRight,
                    render_state.hud_font_size,
                    badge_palette,
                );
            } else if render_state.is_favorite {
                draw_hud_badge(
                    painter,
                    thumb_rect,
                    &[],
                    true,
                    BadgeAnchor::TopRight,
                    render_state.hud_font_size,
                    badge_palette,
                );
            }
            draw_multi_line_title_overlay(
                painter,
                thumb_rect,
                entry_title(entry),
                render_state.hud_font_size,
                title_palette,
            );
        }
    }
}

fn hud_font(font_size: f32) -> egui::FontId {
    egui::FontId::proportional(font_size.clamp(8.0, 20.0))
}

fn draw_multi_line_title_overlay(
    painter: &egui::Painter,
    thumb_rect: Rect,
    title: &str,
    font_size: f32,
    palette: HudPalette,
) {
    let font = hud_font(font_size);
    let line_h = (font_size + 2.0).max(14.0);
    let label_h = line_h * 3.0 + 6.0;
    let label_rect = Rect::from_min_max(
        pos2(thumb_rect.min.x, thumb_rect.max.y - label_h),
        thumb_rect.max,
    );
    draw_title_overlay_background(painter, label_rect, palette);
    draw_title_lines_left(painter, label_rect, title, 3, font, palette.foreground);
}

fn draw_title_overlay_background(painter: &egui::Painter, label_rect: Rect, palette: HudPalette) {
    painter.rect_filled(
        label_rect,
        CornerRadius {
            sw: 5,
            se: 5,
            ..Default::default()
        },
        palette.background,
    );
}

fn draw_title_lines_left(
    painter: &egui::Painter,
    label_rect: Rect,
    title: &str,
    max_lines: usize,
    font: egui::FontId,
    text_color: Color32,
) {
    let pad_x = 5.0;
    let pad_y = if max_lines <= 1 { 4.0 } else { 3.0 };
    let line_h = (font.size + 2.0).max(14.0);
    let text_rect = label_rect.shrink2(vec2(pad_x, 1.0));
    let clipped = painter.with_clip_rect(text_rect);
    let lines = layout_title_lines(painter, title, max_lines, text_rect.width(), font.clone());

    for (i, line) in lines.iter().take(max_lines).enumerate() {
        clipped.text(
            pos2(
                label_rect.min.x + pad_x,
                label_rect.min.y + pad_y + i as f32 * line_h,
            ),
            egui::Align2::LEFT_TOP,
            line,
            font.clone(),
            text_color,
        );
    }
}

#[derive(Clone, Copy)]
struct HudPalette {
    background: Color32,
    foreground: Color32,
}

#[derive(Clone, Copy)]
struct CardSelectionPalette {
    border: Color32,
}

fn selection_palette_from_border(border: Color32) -> CardSelectionPalette {
    CardSelectionPalette { border }
}

fn card_selection_palette(style: LibraryCardSelectionStyle) -> CardSelectionPalette {
    match style {
        LibraryCardSelectionStyle::Default => selection_palette_from_border(SELECTION_ACCENT),
        LibraryCardSelectionStyle::Violet => {
            selection_palette_from_border(Color32::from_rgb(115, 89, 217))
        }
        LibraryCardSelectionStyle::Amber => {
            selection_palette_from_border(Color32::from_rgb(198, 142, 57))
        }
        LibraryCardSelectionStyle::Rose => {
            selection_palette_from_border(Color32::from_rgb(194, 103, 131))
        }
        LibraryCardSelectionStyle::HighContrast => {
            selection_palette_from_border(Color32::from_rgb(0, 0, 0))
        }
    }
}

fn selection_overlay_color(style: LibraryCardSelectionStyle, alpha: u8) -> Color32 {
    let border = card_selection_palette(style).border;
    Color32::from_rgba_unmultiplied(border.r(), border.g(), border.b(), alpha)
}

fn hud_overlay_palette(
    style: LibraryHudStyle,
    selected: bool,
    selection_style: LibraryCardSelectionStyle,
    alpha: u8,
) -> HudPalette {
    match (style, selected) {
        (LibraryHudStyle::Default, false) => HudPalette {
            background: Color32::from_rgba_unmultiplied(0, 0, 0, 165),
            foreground: theme::TEXT_ON_DARK,
        },
        (LibraryHudStyle::Default, true) => HudPalette {
            background: selection_overlay_color(selection_style, alpha),
            foreground: theme::TEXT_ON_DARK,
        },
        (LibraryHudStyle::White, false) => HudPalette {
            background: Color32::from_rgba_unmultiplied(255, 255, 255, 214),
            foreground: Color32::from_rgb(28, 28, 28),
        },
        (LibraryHudStyle::White, true) => HudPalette {
            background: selection_overlay_color(selection_style, alpha),
            foreground: Color32::from_rgb(18, 18, 18),
        },
        (LibraryHudStyle::Blue, false) => HudPalette {
            background: Color32::from_rgba_unmultiplied(
                theme::ACCENT.r(),
                theme::ACCENT.g(),
                theme::ACCENT.b(),
                196,
            ),
            foreground: theme::TEXT_ON_DARK,
        },
        (LibraryHudStyle::Blue, true) => HudPalette {
            background: selection_overlay_color(selection_style, alpha),
            foreground: theme::TEXT_ON_DARK,
        },
        (LibraryHudStyle::HighContrast, false) => HudPalette {
            background: Color32::from_rgba_unmultiplied(0, 0, 0, 240),
            foreground: Color32::from_rgb(255, 255, 255),
        },
        (LibraryHudStyle::HighContrast, true) => HudPalette {
            background: selection_overlay_color(selection_style, alpha),
            foreground: Color32::from_rgb(255, 255, 255),
        },
        (LibraryHudStyle::Amber, false) => HudPalette {
            background: Color32::from_rgba_unmultiplied(152, 106, 42, 214),
            foreground: Color32::from_rgb(250, 240, 220),
        },
        (LibraryHudStyle::Amber, true) => HudPalette {
            background: selection_overlay_color(selection_style, alpha),
            foreground: Color32::from_rgb(252, 245, 228),
        },
        (LibraryHudStyle::Rose, false) => HudPalette {
            background: Color32::from_rgba_unmultiplied(120, 72, 92, 214),
            foreground: Color32::from_rgb(246, 233, 239),
        },
        (LibraryHudStyle::Rose, true) => HudPalette {
            background: selection_overlay_color(selection_style, alpha),
            foreground: Color32::from_rgb(251, 243, 247),
        },
        (LibraryHudStyle::Violet, false) => HudPalette {
            background: Color32::from_rgba_unmultiplied(78, 68, 126, 214),
            foreground: Color32::from_rgb(242, 240, 250),
        },
        (LibraryHudStyle::Violet, true) => HudPalette {
            background: selection_overlay_color(selection_style, alpha),
            foreground: Color32::from_rgb(248, 246, 253),
        },
    }
}

enum ThumbHudBadge {
    Archive { size: u64, ext: String },
    FolderBook,
    ImageFile,
}

fn thumb_hud_badge(entry: &LibraryEntry) -> Option<ThumbHudBadge> {
    match entry {
        LibraryEntry::Archive(entry) => Some(ThumbHudBadge::Archive {
            size: entry.size,
            ext: entry
                .path
                .extension()
                .map(|ext| ext.to_string_lossy().to_ascii_uppercase())
                .filter(|ext| !ext.is_empty())
                .unwrap_or_else(|| "ARC".to_string()),
        }),
        LibraryEntry::FolderBook(_) => Some(ThumbHudBadge::FolderBook),
        LibraryEntry::ImageFile(_) => Some(ThumbHudBadge::ImageFile),
        LibraryEntry::Folder(_) => None,
    }
}

#[derive(Clone, Copy)]
enum BadgeAnchor {
    TopLeft,
    TopRight,
}

fn badge_text_lines(badge: ThumbHudBadge) -> Vec<String> {
    match badge {
        ThumbHudBadge::Archive { size, ext } => vec![format_file_size(size), ext],
        ThumbHudBadge::FolderBook => vec!["DIR".to_string()],
        ThumbHudBadge::ImageFile => vec!["IMAGE".to_string()],
    }
}

fn reading_hud_label(state: ReadingHudState, language: UiLanguage) -> Option<String> {
    match state {
        ReadingHudState::Unread => None,
        ReadingHudState::Reading => Some(tr(language, TextKey::Reading).to_string()),
        ReadingHudState::ReadingPercent(percent) => Some(format!("{percent}%")),
        ReadingHudState::Read => Some(tr(language, TextKey::Read).to_string()),
    }
}

fn draw_size_badge(
    painter: &egui::Painter,
    thumb_rect: Rect,
    badge: ThumbHudBadge,
    font_size: f32,
    palette: HudPalette,
) {
    draw_hud_badge(
        painter,
        thumb_rect,
        &badge_text_lines(badge),
        false,
        BadgeAnchor::TopLeft,
        font_size,
        palette,
    );
}

fn draw_hud_badge(
    painter: &egui::Painter,
    thumb_rect: Rect,
    lines: &[String],
    favorite: bool,
    anchor: BadgeAnchor,
    font_size: f32,
    palette: HudPalette,
) {
    if lines.is_empty() && !favorite {
        return;
    }
    let font = hud_font(font_size);
    let line_h = (font.size + 2.0).max(14.0);
    let pad_x = 6.0;
    let pad_y = 4.0;
    let max_text_w = lines
        .iter()
        .map(|line| measured_text_width(painter, line, &font))
        .fold(0.0, f32::max);
    let star_radius = (line_h * 0.48).clamp(5.5, 9.5);
    let star_size = star_radius * 2.0;
    let content_w = if favorite {
        if lines.is_empty() {
            star_size
        } else {
            max_text_w + 6.0 + star_size
        }
    } else {
        max_text_w
    };
    let content_h = if lines.is_empty() {
        star_size
    } else {
        line_h * lines.len() as f32
    };
    let badge_w = (content_w + pad_x * 2.0).clamp(24.0, thumb_rect.width() * 0.62);
    let badge_h = ((content_h.max(star_size)) + pad_y * 2.0).clamp(16.0, 40.0);
    let margin = vec2(5.0, 5.0);
    let badge_rect = match anchor {
        BadgeAnchor::TopLeft => {
            Rect::from_min_size(thumb_rect.min + margin, vec2(badge_w, badge_h))
        }
        BadgeAnchor::TopRight => Rect::from_min_size(
            pos2(
                thumb_rect.max.x - badge_w - margin.x,
                thumb_rect.min.y + margin.y,
            ),
            vec2(badge_w, badge_h),
        ),
    };
    painter.rect_filled(badge_rect, CornerRadius::same(4), palette.background);

    let text_origin = badge_rect.min + vec2(pad_x, pad_y);
    for (idx, line) in lines.iter().enumerate() {
        painter.text(
            pos2(text_origin.x, text_origin.y + idx as f32 * line_h),
            egui::Align2::LEFT_TOP,
            line,
            font.clone(),
            palette.foreground,
        );
    }
    if favorite {
        let star_center = if lines.is_empty() {
            badge_rect.center()
        } else {
            pos2(
                badge_rect.max.x - pad_x - star_radius,
                badge_rect.center().y,
            )
        };
        paint_favorite_star(painter, star_center, star_radius, palette.foreground);
    }
}

fn layout_title_lines(
    painter: &egui::Painter,
    title: &str,
    max_lines: usize,
    max_width: f32,
    font: egui::FontId,
) -> Vec<String> {
    if max_lines == 0 || title.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = title.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;

    while start < chars.len() && lines.len() < max_lines {
        let is_last_line = lines.len() + 1 == max_lines;
        let mut end = best_fit_end(painter, &chars, start, chars.len(), max_width, &font);
        if end <= start {
            end = (start + 1).min(chars.len());
        }

        let line: String = chars[start..end].iter().collect();
        if is_last_line && end < chars.len() {
            lines.push(fit_with_ellipsis(
                painter,
                &chars[start..],
                max_width,
                &font,
            ));
            break;
        }

        lines.push(line);
        start = end;
    }

    lines
}

fn best_fit_end(
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
        if measured_text_width(painter, &text, font) <= max_width {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid.saturating_sub(1);
        }
    }

    best
}

fn fit_with_ellipsis(
    painter: &egui::Painter,
    chars: &[char],
    max_width: f32,
    font: &egui::FontId,
) -> String {
    let ellipsis = "…";
    if measured_text_width(painter, ellipsis, font) > max_width {
        return String::new();
    }

    let mut lo = 0;
    let mut hi = chars.len();
    let mut best = 0;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let mut text: String = chars[..mid].iter().collect();
        text.push_str(ellipsis);
        if measured_text_width(painter, &text, font) <= max_width {
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

fn measured_text_width(painter: &egui::Painter, text: &str, font: &egui::FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_ON_DARK)
        .size()
        .x
}

fn token_text_color(selected: bool) -> Color32 {
    if selected {
        theme::TEXT_ON_DARK
    } else {
        theme::TEXT_MAIN
    }
}
fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}GB", b / GB)
    } else if b >= MB {
        format!("{:.1}MB", b / MB)
    } else {
        format!("{:.0}KB", (b / KB).max(1.0))
    }
}

fn status_icon_size(rect: Rect) -> f32 {
    (rect.width().min(rect.height()) * 0.34).clamp(18.0, 56.0)
}

fn draw_status_icon(
    painter: &egui::Painter,
    rect: Rect,
    icon: egui_material_icons::MaterialIcon,
    size: f32,
    color: Color32,
) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon.codepoint,
        egui::FontId::new(size, icon.font_family()),
        color,
    );
}

fn aspect_fit(tex: Vec2, cell: Rect) -> Rect {
    if tex.x == 0.0 || tex.y == 0.0 {
        return cell;
    }
    let scale = (cell.width() / tex.x).min(cell.height() / tex.y);
    let (nw, nh) = (tex.x * scale, tex.y * scale);
    Rect::from_center_size(cell.center(), vec2(nw, nh))
}

fn show_filename_token_menu_frame(
    ui: &mut egui::Ui,
    entry: &BookMeta,
    popup_keys: PopupKeyInput,
    language: UiLanguage,
) -> Option<String> {
    let filename = entry
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| entry.title.to_string());

    if filename.trim().is_empty() {
        return None;
    }

    let parsed = parse_filename(&filename);
    let extension = split_extension(&filename);
    let (segments, selectable_tokens) =
        build_filename_segments(&parsed.parts, extension.as_deref());
    if selectable_tokens.is_empty() {
        return None;
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
    let up = popup_keys.up;
    let down = popup_keys.down;
    let left = popup_keys.left;
    let right = popup_keys.right;
    let esc = popup_keys.esc;
    if up || down || left || right || esc {
        ui.close();
        return None;
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

    let filter_label = tr(language, TextKey::FilterToken).replacen("{}", selected_text, 1);
    let filter_row = ContextMenuRowSpec {
        label: &filter_label,
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
    if draw_context_menu_row(ui, &filter_row) {
        ui.close();
        return Some(selected_text.clone());
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

    ui.ctx().data_mut(|data| {
        data.insert_temp(state_key, selected_idx);
    });

    None
}

fn context_menu_item(
    ui: &mut egui::Ui,
    icon: egui_material_icons::MaterialIcon,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
    delete_item: bool,
) -> bool {
    let icon_color = if enabled {
        if delete_item {
            theme::DELETE_RED
        } else {
            theme::TEXT_MAIN
        }
    } else {
        theme::TEXT_DISABLED
    };
    let row = ContextMenuRowSpec {
        label,
        shortcut: shortcut.unwrap_or(""),
        enabled,
        icon: Some(icon),
        label_color: if enabled {
            theme::TEXT_MAIN
        } else {
            theme::TEXT_DISABLED
        },
        shortcut_color: if enabled {
            theme::TEXT_SUBTLE
        } else {
            theme::TEXT_DISABLED
        },
        icon_color,
    };
    draw_context_menu_row(ui, &row)
}

fn external_tool_menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let row = ContextMenuRowSpec {
        label,
        shortcut,
        enabled,
        icon: None,
        label_color: if enabled {
            theme::TEXT_MAIN
        } else {
            theme::TEXT_DISABLED
        },
        shortcut_color: if enabled {
            theme::TEXT_SUBTLE
        } else {
            theme::TEXT_DISABLED
        },
        icon_color: theme::TEXT_MAIN,
    };
    draw_context_menu_row(ui, &row)
}

const CONTEXT_MENU_ROW_CORNER_RADIUS: f32 = 3.0;
const CONTEXT_MENU_ROW_PADDING_X: f32 = 10.0;
const CONTEXT_MENU_ROW_ICON_WIDTH: f32 = 20.0;
const CONTEXT_MENU_ROW_LABEL_FONT_SIZE: f32 = theme::FONT_SIZE_LARGE;
const CONTEXT_MENU_ROW_SHORTCUT_FONT_SIZE: f32 = 10.5;
const CONTEXT_MENU_ROW_ICON_FONT_SIZE: f32 = 15.0;

struct ContextMenuRowSpec<'a> {
    label: &'a str,
    shortcut: &'a str,
    enabled: bool,
    icon: Option<egui_material_icons::MaterialIcon>,
    label_color: Color32,
    shortcut_color: Color32,
    icon_color: Color32,
}

fn draw_context_menu_row(ui: &mut egui::Ui, row: &ContextMenuRowSpec<'_>) -> bool {
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
    if text.is_empty() {
        return;
    }
    segments.push(FilenameSegment::Text(text.to_string()));
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

fn entry_title(entry: &LibraryEntry) -> &str {
    match entry {
        LibraryEntry::Archive(entry) => entry.title.as_ref(),
        LibraryEntry::Folder(FolderMeta { title, .. })
        | LibraryEntry::FolderBook(FolderMeta { title, .. }) => title.as_ref(),
        LibraryEntry::ImageFile(entry) => entry.title.as_ref(),
    }
}
