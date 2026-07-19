//! アプリ全体設定。
//!
//! 保存先: `%LOCALAPPDATA%\cbz-viewer\settings.json`
//! サムネイルは固定保存サイズと可変表示サイズを分け、サイズ変更時に再生成しない。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::domain::performance::{SPAD_RAM_RATIO_MAX_PERCENT, SPAD_RAM_RATIO_MIN_PERCENT};

// ── 定数 ─────────────────────────────────────────────────────────────────────

/// ディスクキャッシュ保存サイズ（固定）
pub const THUMB_STORAGE_WIDTH: u16 = 320;

/// 表示サイズの最小値
pub const THUMB_DISPLAY_MIN: u16 = 80;
/// 表示サイズの最大値（= ストレージサイズ）
pub const THUMB_DISPLAY_MAX: u16 = 320;
/// 表示サイズのステップ
pub const THUMB_DISPLAY_STEP: u16 = 10;
/// 表示サイズのデフォルト（標準）
pub const THUMB_DISPLAY_DEFAULT: u16 = 200;
pub const VIEWER_L1_VRAM_CACHE_MAX_MB_DEFAULT: u16 = 256;
pub const VIEWER_RGBA_CACHE_MAX_MB_DEFAULT: u16 = 256;
pub const VIEWER_BACKGROUND_WORKER_COUNT_DEFAULT: u16 = 2;
pub const EXTERNAL_TOOLS_MAX: usize = 3;

pub fn normalize_external_tool_executable(s: &str) -> String {
    s.trim().trim_matches('"').trim().to_owned()
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewerQuality {
    Speed,
    Balanced,
    Quality,
    Original,
}

impl Default for ViewerQuality {
    fn default() -> Self {
        VIEWER_QUALITY_DEFAULT
    }
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewerOpenMode {
    Windowed,
    Fullscreen,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReadingDirection {
    #[default]
    RightToLeft,
    LeftToRight,
}

/// ライブラリグリッドの HUD 表示モード。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibraryHudMode {
    Off,
    On,
}

/// ライブラリカード HUD の配色プリセット。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibraryHudStyle {
    Default,
    White,
    Blue,
    HighContrast,
    Amber,
    Rose,
    Violet,
}

impl LibraryHudStyle {
    pub const ALL: [Self; 7] = [
        Self::Default,
        Self::White,
        Self::Blue,
        Self::Amber,
        Self::Rose,
        Self::Violet,
        Self::HighContrast,
    ];

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }
}

/// ライブラリカード選択状態の配色プリセット。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibraryCardSelectionStyle {
    Default,
    Violet,
    Amber,
    Rose,
    HighContrast,
}

impl LibraryCardSelectionStyle {
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Violet,
        Self::Amber,
        Self::Rose,
        Self::HighContrast,
    ];

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalToolShortcut {
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    N,
    O,
    P,
    Q,
    R,
    T,
    U,
    V,
    X,
    Y,
    Z,
}

impl ExternalToolShortcut {
    pub const fn as_char(self) -> char {
        match self {
            Self::E => 'E',
            Self::F => 'F',
            Self::G => 'G',
            Self::H => 'H',
            Self::I => 'I',
            Self::J => 'J',
            Self::K => 'K',
            Self::L => 'L',
            Self::N => 'N',
            Self::O => 'O',
            Self::P => 'P',
            Self::Q => 'Q',
            Self::R => 'R',
            Self::T => 'T',
            Self::U => 'U',
            Self::V => 'V',
            Self::X => 'X',
            Self::Y => 'Y',
            Self::Z => 'Z',
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExternalTool {
    pub name: String,
    pub executable: String,
    pub args: String,
    #[serde(with = "crate::domain::app_settings_codec::external_tool_shortcut_serde")]
    pub shortcut: ExternalToolShortcut,
    pub background: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum UiLanguage {
    #[default]
    English,
    Japanese,
}

impl UiLanguage {
    pub const ALL: [Self; 2] = [Self::English, Self::Japanese];

    pub const fn as_code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Japanese => "ja",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::English),
            "ja" => Some(Self::Japanese),
            _ => None,
        }
    }

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }
}

impl LibraryHudMode {
    /// ツールバーの HUD ボタンで使う 2 状態循環。
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::On,
            Self::On => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "HUD: OFF",
            Self::On => "HUD: ON",
        }
    }
}

pub const VIEWER_QUALITY_DEFAULT: ViewerQuality = ViewerQuality::Balanced;
pub const LIBRARY_HUD_MODE_DEFAULT: LibraryHudMode = LibraryHudMode::On;
pub const LIBRARY_HUD_STYLE_DEFAULT: LibraryHudStyle = LibraryHudStyle::Default;
pub const LIBRARY_CARD_SELECTION_STYLE_DEFAULT: LibraryCardSelectionStyle =
    LibraryCardSelectionStyle::Default;
pub const VIEWER_OPEN_MODE_DEFAULT: ViewerOpenMode = ViewerOpenMode::Windowed;
pub const READING_DIRECTION_DEFAULT: ReadingDirection = ReadingDirection::RightToLeft;

pub const LIBRARY_WHEEL_SPEED_MIN: u16 = 1;
pub const LIBRARY_WHEEL_SPEED_MAX: u16 = 10;
pub const LIBRARY_WHEEL_SPEED_DEFAULT: u16 = 6;

/// ライブラリ HUD フォントサイズレベル（1〜9）
pub const LIBRARY_HUD_FONT_LEVEL_MIN: u16 = 1;
pub const LIBRARY_HUD_FONT_LEVEL_MAX: u16 = 9;
/// 標準は 5 段目。現在の HUD フォントサイズを標準とする。
pub const LIBRARY_HUD_FONT_LEVEL_DEFAULT: u16 = 5;
pub const FOLDER_BOOK_OPEN_AS_VIEWER_DEFAULT: bool = true;
pub const OPEN_REBUILT_CBZ_IN_NEW_VIEWER_DEFAULT: bool = false;

// ── AppSettings ───────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct AppSettings {
    /// サムネイル表示幅（px）— 160〜320、10刻み
    #[serde(default = "default_thumb_display_w")]
    pub thumb_display_w: u16,
    /// UI 表示言語
    #[serde(
        default = "default_ui_language",
        with = "crate::domain::app_settings_codec::ui_language_serde"
    )]
    pub ui_language: UiLanguage,
    /// ビューア画質プロファイル
    #[serde(
        default = "default_viewer_quality",
        with = "crate::domain::app_settings_codec::viewer_quality_serde"
    )]
    pub viewer_quality: ViewerQuality,
    /// L1 VRAM Cache 上限（MiB）
    #[serde(default = "default_viewer_l1_vram_cache_max_mb")]
    pub viewer_l1_vram_cache_max_mb: u16,
    /// L2 RAM Cache 上限（MiB）
    #[serde(
        default = "default_viewer_rgba_cache_max_mb",
        deserialize_with = "crate::domain::app_settings_codec::deserialize_viewer_rgba_cache_max_mb"
    )]
    pub viewer_rgba_cache_max_mb: u16,
    /// ビューア先読みバックグラウンド処理数
    #[serde(
        default = "default_viewer_background_worker_count",
        deserialize_with = "crate::domain::app_settings_codec::deserialize_viewer_background_worker_count"
    )]
    pub viewer_background_worker_count: u16,
    /// Danger Zone を有効にする
    #[serde(default = "default_viewer_danger_zone_enabled")]
    pub viewer_danger_zone_enabled: bool,
    /// Danger Zone時の隣接本1冊あたりSPAD RAM割合（%）
    #[serde(
        default = "default_viewer_spad_ram_ratio_percent",
        deserialize_with = "crate::domain::app_settings_codec::deserialize_viewer_spad_ram_ratio_percent"
    )]
    pub viewer_spad_ram_ratio_percent: u8,
    /// ライブラリから Viewer を開くときの起動モード
    #[serde(
        default = "default_viewer_open_mode",
        with = "crate::domain::app_settings_codec::viewer_open_mode_serde"
    )]
    pub viewer_open_mode: ViewerOpenMode,
    /// ページ開きのグローバル既定値
    #[serde(
        default = "default_reading_direction",
        with = "crate::domain::app_settings_codec::reading_direction_serde"
    )]
    pub reading_direction: ReadingDirection,
    /// ライブラリグリッドの HUD 表示モード
    #[serde(
        default = "default_library_hud_mode",
        with = "crate::domain::app_settings_codec::library_hud_mode_serde"
    )]
    pub library_hud_mode: LibraryHudMode,
    /// ライブラリカード HUD の配色プリセット
    #[serde(
        default = "default_library_hud_style",
        with = "crate::domain::app_settings_codec::library_hud_style_serde"
    )]
    pub library_hud_style: LibraryHudStyle,
    /// ライブラリカード選択状態の配色プリセット
    #[serde(
        default = "default_library_card_selection_style",
        with = "crate::domain::app_settings_codec::library_card_selection_style_serde"
    )]
    pub library_card_selection_style: LibraryCardSelectionStyle,
    /// ライブラリ画面のホイールスクロール速度レベル（1〜10）
    #[serde(
        default = "default_library_wheel_speed",
        deserialize_with = "crate::domain::app_settings_codec::deserialize_u16_clamped_library_wheel_speed"
    )]
    pub library_wheel_speed: u16,
    /// ライブラリ HUD のフォントサイズレベル（1〜9、標準=5）
    #[serde(
        default = "default_library_hud_font_level",
        deserialize_with = "crate::domain::app_settings_codec::deserialize_library_hud_font_level"
    )]
    pub library_hud_font_level: u16,
    /// 画像フォルダを本として開く
    #[serde(default = "default_folder_book_open_as_viewer")]
    pub folder_book_open_as_viewer: bool,
    /// 最後に読んだ位置から再開する
    #[serde(default = "default_resume_from_last_reading_position")]
    pub resume_from_last_reading_position: bool,
    /// 再構築後に新しい CBZ を別 Viewer で開く
    #[serde(default = "default_open_rebuilt_cbz_in_new_viewer")]
    pub open_rebuilt_cbz_in_new_viewer: bool,
    /// 外部ツール設定（最大3件）
    #[serde(default = "default_external_tools")]
    pub external_tools: Vec<ExternalTool>,
}

#[allow(dead_code)]
fn default_thumb_display_w() -> u16 {
    THUMB_DISPLAY_DEFAULT
}
#[allow(dead_code)]
fn default_ui_language() -> UiLanguage {
    UiLanguage::default()
}
#[allow(dead_code)]
fn default_viewer_quality() -> ViewerQuality {
    VIEWER_QUALITY_DEFAULT
}
#[allow(dead_code)]
fn default_viewer_l1_vram_cache_max_mb() -> u16 {
    VIEWER_L1_VRAM_CACHE_MAX_MB_DEFAULT
}
#[allow(dead_code)]
fn default_viewer_rgba_cache_max_mb() -> u16 {
    VIEWER_RGBA_CACHE_MAX_MB_DEFAULT
}
#[allow(dead_code)]
fn default_viewer_background_worker_count() -> u16 {
    VIEWER_BACKGROUND_WORKER_COUNT_DEFAULT
}
#[allow(dead_code)]
fn default_viewer_danger_zone_enabled() -> bool {
    false
}
#[allow(dead_code)]
fn default_viewer_spad_ram_ratio_percent() -> u8 {
    SPAD_RAM_RATIO_MIN_PERCENT
}
#[allow(dead_code)]
fn default_viewer_open_mode() -> ViewerOpenMode {
    VIEWER_OPEN_MODE_DEFAULT
}
#[allow(dead_code)]
fn default_reading_direction() -> ReadingDirection {
    READING_DIRECTION_DEFAULT
}
#[allow(dead_code)]
fn default_library_hud_mode() -> LibraryHudMode {
    LIBRARY_HUD_MODE_DEFAULT
}
#[allow(dead_code)]
fn default_library_hud_style() -> LibraryHudStyle {
    LIBRARY_HUD_STYLE_DEFAULT
}
#[allow(dead_code)]
fn default_library_card_selection_style() -> LibraryCardSelectionStyle {
    LIBRARY_CARD_SELECTION_STYLE_DEFAULT
}
#[allow(dead_code)]
fn default_library_wheel_speed() -> u16 {
    LIBRARY_WHEEL_SPEED_DEFAULT
}
#[allow(dead_code)]
fn default_library_hud_font_level() -> u16 {
    LIBRARY_HUD_FONT_LEVEL_DEFAULT
}
#[allow(dead_code)]
fn default_folder_book_open_as_viewer() -> bool {
    FOLDER_BOOK_OPEN_AS_VIEWER_DEFAULT
}
#[allow(dead_code)]
fn default_resume_from_last_reading_position() -> bool {
    false
}
#[allow(dead_code)]
fn default_open_rebuilt_cbz_in_new_viewer() -> bool {
    OPEN_REBUILT_CBZ_IN_NEW_VIEWER_DEFAULT
}
#[allow(dead_code)]
fn default_external_tools() -> Vec<ExternalTool> {
    Vec::new()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            thumb_display_w: THUMB_DISPLAY_DEFAULT,
            ui_language: UiLanguage::default(),
            viewer_quality: VIEWER_QUALITY_DEFAULT,
            viewer_l1_vram_cache_max_mb: VIEWER_L1_VRAM_CACHE_MAX_MB_DEFAULT,
            viewer_rgba_cache_max_mb: VIEWER_RGBA_CACHE_MAX_MB_DEFAULT,
            viewer_background_worker_count: VIEWER_BACKGROUND_WORKER_COUNT_DEFAULT,
            viewer_danger_zone_enabled: false,
            viewer_spad_ram_ratio_percent: SPAD_RAM_RATIO_MIN_PERCENT,
            viewer_open_mode: VIEWER_OPEN_MODE_DEFAULT,
            reading_direction: READING_DIRECTION_DEFAULT,
            library_hud_mode: LIBRARY_HUD_MODE_DEFAULT,
            library_hud_style: LIBRARY_HUD_STYLE_DEFAULT,
            library_card_selection_style: LIBRARY_CARD_SELECTION_STYLE_DEFAULT,
            library_wheel_speed: LIBRARY_WHEEL_SPEED_DEFAULT,
            library_hud_font_level: LIBRARY_HUD_FONT_LEVEL_DEFAULT,
            folder_book_open_as_viewer: FOLDER_BOOK_OPEN_AS_VIEWER_DEFAULT,
            resume_from_last_reading_position: false,
            open_rebuilt_cbz_in_new_viewer: OPEN_REBUILT_CBZ_IN_NEW_VIEWER_DEFAULT,
            external_tools: default_external_tools(),
        }
    }
}

impl AppSettings {
    pub(crate) fn normalize_persisted_values(&mut self) {
        self.viewer_spad_ram_ratio_percent = self
            .viewer_spad_ram_ratio_percent
            .clamp(SPAD_RAM_RATIO_MIN_PERCENT, SPAD_RAM_RATIO_MAX_PERCENT);
    }

    pub const fn external_tool_shortcut_candidates() -> &'static [ExternalToolShortcut] {
        &[
            ExternalToolShortcut::E,
            ExternalToolShortcut::F,
            ExternalToolShortcut::G,
            ExternalToolShortcut::H,
            ExternalToolShortcut::I,
            ExternalToolShortcut::J,
            ExternalToolShortcut::K,
            ExternalToolShortcut::L,
            ExternalToolShortcut::N,
            ExternalToolShortcut::O,
            ExternalToolShortcut::P,
            ExternalToolShortcut::Q,
            ExternalToolShortcut::R,
            ExternalToolShortcut::T,
            ExternalToolShortcut::U,
            ExternalToolShortcut::V,
            ExternalToolShortcut::X,
            ExternalToolShortcut::Y,
            ExternalToolShortcut::Z,
        ]
    }

    pub fn next_available_external_tool_shortcut(
        tools: &[ExternalTool],
    ) -> Option<ExternalToolShortcut> {
        let used: HashSet<ExternalToolShortcut> = tools.iter().map(|tool| tool.shortcut).collect();
        Self::external_tool_shortcut_candidates()
            .iter()
            .copied()
            .find(|shortcut| !used.contains(shortcut))
    }

    pub fn sanitize_external_tools(&mut self) {
        let mut seen = HashSet::with_capacity(self.external_tools.len());
        self.external_tools = self
            .external_tools
            .iter()
            .filter(|tool| seen.insert(tool.shortcut))
            .take(EXTERNAL_TOOLS_MAX)
            .cloned()
            .map(|mut tool| {
                tool.executable = normalize_external_tool_executable(&tool.executable);
                tool
            })
            .collect();
    }

    /// 表示サイズを 10px 刻みにクランプする
    pub fn clamped_display_w(&self) -> u16 {
        let v = self
            .thumb_display_w
            .clamp(THUMB_DISPLAY_MIN, THUMB_DISPLAY_MAX);
        // 10の倍数に丸める
        (v / THUMB_DISPLAY_STEP) * THUMB_DISPLAY_STEP
    }

    /// サムネイル表示幅（px）
    pub fn thumb_w(&self) -> f32 {
        self.clamped_display_w() as f32
    }

    /// サムネイル表示高さ（px）— 縦横比 260:180 を維持
    pub fn thumb_h(&self) -> f32 {
        self.thumb_w() * (260.0 / 180.0)
    }

    /// ライブラリ画面のホイール速度レベルをクランプする
    pub fn clamped_library_wheel_speed(&self) -> u16 {
        self.library_wheel_speed
            .clamp(LIBRARY_WHEEL_SPEED_MIN, LIBRARY_WHEEL_SPEED_MAX)
    }

    /// ライブラリ HUD フォントサイズレベルをクランプする
    pub fn clamped_library_hud_font_level(&self) -> u16 {
        self.library_hud_font_level
            .clamp(LIBRARY_HUD_FONT_LEVEL_MIN, LIBRARY_HUD_FONT_LEVEL_MAX)
    }

    /// ライブラリ HUD フォントサイズ。標準レベル 5 は現在サイズ 12px。
    pub fn library_hud_font_size(&self) -> f32 {
        12.0 + (self.clamped_library_hud_font_level() as f32
            - LIBRARY_HUD_FONT_LEVEL_DEFAULT as f32)
    }

    /// ライブラリ画面で使うホイールスクロール倍率
    pub fn library_wheel_multiplier(&self) -> f32 {
        match self.clamped_library_wheel_speed() {
            1 => 1.0,
            2 => 1.5,
            3 => 2.0,
            4 => 2.5,
            5 => 3.0,
            6 => 4.0,
            7 => 5.0,
            8 => 6.0,
            9 => 7.0,
            10 => 8.0,
            _ => 4.0,
        }
    }

    /// ワーカーへ渡す target_width（常に最大サイズ固定）
    pub fn storage_width() -> u16 {
        THUMB_STORAGE_WIDTH
    }
}
