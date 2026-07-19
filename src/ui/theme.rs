use eframe::egui::Color32;

// ── カラー（ライトテーマ）────────────────────────────────────────────────────────
pub const WINDOW_BG: Color32 = Color32::from_rgb(243, 243, 243); // #F3F3F3
pub const TOOLBAR_BG: Color32 = Color32::from_rgb(233, 233, 233); // #E9E9E9
pub const SURFACE_BG: Color32 = Color32::from_rgb(246, 246, 246); // #F6F6F6
pub const BUTTON_HOVER: Color32 = Color32::from_rgb(218, 218, 218); // #DADADA
pub const BUTTON_ACTIVE: Color32 = Color32::from_rgb(191, 203, 248); // #BFCBF8
pub const SIDEBAR_HOVER_BG: Color32 = Color32::from_rgb(232, 232, 232); // #E8E8E8
pub const SIDEBAR_SELECTED_BG: Color32 = Color32::from_rgb(216, 216, 216); // #D8D8D8
pub const HOVER_BORDER: Color32 = Color32::from_rgb(176, 176, 176); // hover 境界（標準）
pub const HOVER_BORDER_WEAK: Color32 = Color32::from_rgb(198, 198, 198); // hover 境界（弱）
// hover 時の style / font metrics 差分を吸収し、レイアウト揺れを防ぐ横方向余白。
pub const ICON_BUTTON_HOVER_GUARD_X: f32 = 4.0;
pub const ACCENT: Color32 = Color32::from_rgb(74, 144, 226); // #4A90E2
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(107, 170, 239); // #6BAAEF
pub const ACCENT_ACTIVE: Color32 = Color32::from_rgb(47, 117, 209); // #2F75D1
pub const BORDER: Color32 = Color32::from_rgb(213, 213, 213); // #D5D5D5
pub const SEPARATOR_WEAK: Color32 = Color32::from_rgb(227, 227, 227); // #E3E3E3
pub const TEXT_MAIN: Color32 = Color32::from_rgb(34, 34, 34); // #222222
pub const TEXT_SUBTLE: Color32 = Color32::from_rgb(102, 102, 102); // #666666
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(181, 181, 181); // #B5B5B5
pub const DELETE_RED: Color32 = Color32::from_rgb(211, 47, 47); // #D32F2F
pub const PROGRESS_BG: Color32 = Color32::from_rgb(224, 224, 224); // rail / 未到達
pub const PROGRESS_FILL: Color32 = Color32::from_rgb(188, 210, 242); // fill / 到達済み
pub const PROGRESS_ACTIVE: Color32 = Color32::from_rgb(95, 145, 214); // thumb / 現在位置

// ── External Tool Button Colors ─────────────────────────────────────────────
pub const EXTERNAL_TOOL_IDLE_BG: Color32 = Color32::TRANSPARENT;
pub const EXTERNAL_TOOL_PROGRESS_BG: Color32 = ACCENT;
pub const EXTERNAL_TOOL_SUCCESS_BG: Color32 = Color32::from_rgb(76, 175, 80);
pub const EXTERNAL_TOOL_FAILED_BG: Color32 = DELETE_RED;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalToolButtonState {
    Idle,
    Running,
    Success,
    Failed,
}

pub fn external_tool_button_bg(state: ExternalToolButtonState) -> Color32 {
    match state {
        ExternalToolButtonState::Idle => EXTERNAL_TOOL_IDLE_BG,
        ExternalToolButtonState::Running => EXTERNAL_TOOL_PROGRESS_BG,
        ExternalToolButtonState::Success => EXTERNAL_TOOL_SUCCESS_BG,
        ExternalToolButtonState::Failed => EXTERNAL_TOOL_FAILED_BG,
    }
}

// 既存参照名との互換 alias。
// `PLACEHOLDER_BG` / `TEXT_ON_DARK` 参照を残す限り維持する。
pub const PLACEHOLDER_BG: Color32 = BORDER;
pub const TEXT_ON_DARK: Color32 = Color32::from_rgb(230, 230, 230);

// ── レイアウト定数 ────────────────────────────────────────────────────────────
pub const THUMB_W: f32 = 180.0;
pub const THUMB_H: f32 = 260.0;
pub const GRID_GAP: f32 = 10.0;
pub const SIDEBAR_W: f32 = 200.0;
pub const SIDEBAR_INNER_MARGIN: f32 = 10.0;
// 標準コントロール高さ。
pub const CONTROL_HEIGHT: f32 = 24.0;
pub const FONT_SIZE_TINY: f32 = 10.0;
pub const FONT_SIZE_SMALL: f32 = 11.5;
pub const FONT_SIZE_BODY: f32 = 12.0;
pub const FONT_SIZE_LARGE: f32 = 13.0;
pub const FONT_SIZE_EMPTY: f32 = 16.0;
