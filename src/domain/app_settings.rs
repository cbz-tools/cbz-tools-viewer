//! アプリ全体設定。
//!
//! 保存先: `%LOCALAPPDATA%\cbz-viewer\settings.json`
//! サムネイルは固定保存サイズと可変表示サイズを分け、サイズ変更時に再生成しない。

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::performance::{
    PerformanceResources, PerformanceSettingsResolved, PERFORMANCE_CACHE_MIN_MIB,
};

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
    #[serde(with = "external_tool_shortcut_serde")]
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

const APP_SETTINGS_SCHEMA_VERSION: u16 = 1;

mod ui_language_serde {
    use super::UiLanguage;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &UiLanguage, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(value.as_code())
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<UiLanguage, D::Error>
    where
        D: Deserializer<'de>,
    {
        let code = String::deserialize(deserializer)?;
        UiLanguage::from_code(&code).ok_or_else(|| serde::de::Error::custom("invalid ui_language"))
    }
}

#[allow(dead_code)]
mod viewer_quality_serde {
    use super::ViewerQuality;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Value {
        Speed,
        Balanced,
        Quality,
        Original,
    }

    impl From<ViewerQuality> for Value {
        fn from(value: ViewerQuality) -> Self {
            match value {
                ViewerQuality::Speed => Self::Speed,
                ViewerQuality::Balanced => Self::Balanced,
                ViewerQuality::Quality => Self::Quality,
                ViewerQuality::Original => Self::Original,
            }
        }
    }

    impl From<Value> for ViewerQuality {
        fn from(value: Value) -> Self {
            match value {
                Value::Speed => Self::Speed,
                Value::Balanced => Self::Balanced,
                Value::Quality => Self::Quality,
                Value::Original => Self::Original,
            }
        }
    }

    pub fn serialize<S>(value: &ViewerQuality, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Value::from(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ViewerQuality, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Into::into)
    }
}

#[allow(dead_code)]
mod viewer_open_mode_serde {
    use super::ViewerOpenMode;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Value {
        Windowed,
        Fullscreen,
    }

    impl From<ViewerOpenMode> for Value {
        fn from(value: ViewerOpenMode) -> Self {
            match value {
                ViewerOpenMode::Windowed => Self::Windowed,
                ViewerOpenMode::Fullscreen => Self::Fullscreen,
            }
        }
    }

    impl From<Value> for ViewerOpenMode {
        fn from(value: Value) -> Self {
            match value {
                Value::Windowed => Self::Windowed,
                Value::Fullscreen => Self::Fullscreen,
            }
        }
    }

    pub fn serialize<S>(value: &ViewerOpenMode, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Value::from(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ViewerOpenMode, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Into::into)
    }
}

#[allow(dead_code)]
mod reading_direction_serde {
    use super::ReadingDirection;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Value {
        RightToLeft,
        LeftToRight,
    }

    impl From<ReadingDirection> for Value {
        fn from(value: ReadingDirection) -> Self {
            match value {
                ReadingDirection::RightToLeft => Self::RightToLeft,
                ReadingDirection::LeftToRight => Self::LeftToRight,
            }
        }
    }

    impl From<Value> for ReadingDirection {
        fn from(value: Value) -> Self {
            match value {
                Value::RightToLeft => Self::RightToLeft,
                Value::LeftToRight => Self::LeftToRight,
            }
        }
    }

    pub fn serialize<S>(value: &ReadingDirection, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Value::from(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ReadingDirection, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Into::into)
    }
}

#[allow(dead_code)]
mod library_hud_mode_serde {
    use super::LibraryHudMode;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Value {
        Off,
        On,
    }

    impl From<LibraryHudMode> for Value {
        fn from(value: LibraryHudMode) -> Self {
            match value {
                LibraryHudMode::Off => Self::Off,
                LibraryHudMode::On => Self::On,
            }
        }
    }

    impl From<Value> for LibraryHudMode {
        fn from(value: Value) -> Self {
            match value {
                Value::Off => Self::Off,
                Value::On => Self::On,
            }
        }
    }

    pub fn serialize<S>(value: &LibraryHudMode, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Value::from(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<LibraryHudMode, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Into::into)
    }
}

#[allow(dead_code)]
mod library_hud_style_serde {
    use super::LibraryHudStyle;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &LibraryHudStyle, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match value {
            LibraryHudStyle::Default => "default",
            LibraryHudStyle::White => "white",
            LibraryHudStyle::Blue => "blue",
            LibraryHudStyle::HighContrast => "high_contrast",
            LibraryHudStyle::Amber => "amber",
            LibraryHudStyle::Rose => "rose",
            LibraryHudStyle::Violet => "violet",
        })
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<LibraryHudStyle, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        super::parse_library_hud_style_name(&value)
            .ok_or_else(|| serde::de::Error::custom("invalid library_hud_style"))
    }
}

#[allow(dead_code)]
mod library_card_selection_style_serde {
    use super::LibraryCardSelectionStyle;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Value {
        Default,
        Violet,
        Amber,
        Rose,
        HighContrast,
    }

    impl From<LibraryCardSelectionStyle> for Value {
        fn from(value: LibraryCardSelectionStyle) -> Self {
            match value {
                LibraryCardSelectionStyle::Default => Self::Default,
                LibraryCardSelectionStyle::Violet => Self::Violet,
                LibraryCardSelectionStyle::Amber => Self::Amber,
                LibraryCardSelectionStyle::Rose => Self::Rose,
                LibraryCardSelectionStyle::HighContrast => Self::HighContrast,
            }
        }
    }

    impl From<Value> for LibraryCardSelectionStyle {
        fn from(value: Value) -> Self {
            match value {
                Value::Default => Self::Default,
                Value::Violet => Self::Violet,
                Value::Amber => Self::Amber,
                Value::Rose => Self::Rose,
                Value::HighContrast => Self::HighContrast,
            }
        }
    }

    pub fn serialize<S>(value: &LibraryCardSelectionStyle, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Value::from(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<LibraryCardSelectionStyle, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Into::into)
    }
}

mod external_tool_shortcut_serde {
    use super::ExternalToolShortcut;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Value {
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

    impl From<ExternalToolShortcut> for Value {
        fn from(value: ExternalToolShortcut) -> Self {
            match value {
                ExternalToolShortcut::E => Self::E,
                ExternalToolShortcut::F => Self::F,
                ExternalToolShortcut::G => Self::G,
                ExternalToolShortcut::H => Self::H,
                ExternalToolShortcut::I => Self::I,
                ExternalToolShortcut::J => Self::J,
                ExternalToolShortcut::K => Self::K,
                ExternalToolShortcut::L => Self::L,
                ExternalToolShortcut::N => Self::N,
                ExternalToolShortcut::O => Self::O,
                ExternalToolShortcut::P => Self::P,
                ExternalToolShortcut::Q => Self::Q,
                ExternalToolShortcut::R => Self::R,
                ExternalToolShortcut::T => Self::T,
                ExternalToolShortcut::U => Self::U,
                ExternalToolShortcut::V => Self::V,
                ExternalToolShortcut::X => Self::X,
                ExternalToolShortcut::Y => Self::Y,
                ExternalToolShortcut::Z => Self::Z,
            }
        }
    }

    impl From<Value> for ExternalToolShortcut {
        fn from(value: Value) -> Self {
            match value {
                Value::E => Self::E,
                Value::F => Self::F,
                Value::G => Self::G,
                Value::H => Self::H,
                Value::I => Self::I,
                Value::J => Self::J,
                Value::K => Self::K,
                Value::L => Self::L,
                Value::N => Self::N,
                Value::O => Self::O,
                Value::P => Self::P,
                Value::Q => Self::Q,
                Value::R => Self::R,
                Value::T => Self::T,
                Value::U => Self::U,
                Value::V => Self::V,
                Value::X => Self::X,
                Value::Y => Self::Y,
                Value::Z => Self::Z,
            }
        }
    }

    pub fn serialize<S>(value: &ExternalToolShortcut, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Value::from(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ExternalToolShortcut, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Into::into)
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

// ── AppSettings ───────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct AppSettings {
    /// サムネイル表示幅（px）— 160〜320、10刻み
    #[serde(default = "default_thumb_display_w")]
    pub thumb_display_w: u16,
    /// UI 表示言語
    #[serde(default = "default_ui_language", with = "ui_language_serde")]
    pub ui_language: UiLanguage,
    /// ビューア画質プロファイル
    #[serde(default = "default_viewer_quality", with = "viewer_quality_serde")]
    pub viewer_quality: ViewerQuality,
    /// L1 VRAM Cache 上限（MiB）
    #[serde(default = "default_viewer_l1_vram_cache_max_mb")]
    pub viewer_l1_vram_cache_max_mb: u16,
    /// L2 RAM Cache 上限（MiB）
    #[serde(
        default = "default_viewer_rgba_cache_max_mb",
        deserialize_with = "deserialize_viewer_rgba_cache_max_mb"
    )]
    pub viewer_rgba_cache_max_mb: u16,
    /// ビューア先読みバックグラウンド処理数
    #[serde(
        default = "default_viewer_background_worker_count",
        deserialize_with = "deserialize_viewer_background_worker_count"
    )]
    pub viewer_background_worker_count: u16,
    /// Danger Zone を有効にする
    #[serde(default = "default_viewer_danger_zone_enabled")]
    pub viewer_danger_zone_enabled: bool,
    /// ライブラリから Viewer を開くときの起動モード
    #[serde(default = "default_viewer_open_mode", with = "viewer_open_mode_serde")]
    pub viewer_open_mode: ViewerOpenMode,
    /// ページ開きのグローバル既定値
    #[serde(
        default = "default_reading_direction",
        with = "reading_direction_serde"
    )]
    pub reading_direction: ReadingDirection,
    /// ライブラリグリッドの HUD 表示モード
    #[serde(default = "default_library_hud_mode", with = "library_hud_mode_serde")]
    pub library_hud_mode: LibraryHudMode,
    /// ライブラリカード HUD の配色プリセット
    #[serde(
        default = "default_library_hud_style",
        with = "library_hud_style_serde"
    )]
    pub library_hud_style: LibraryHudStyle,
    /// ライブラリカード選択状態の配色プリセット
    #[serde(
        default = "default_library_card_selection_style",
        with = "library_card_selection_style_serde"
    )]
    pub library_card_selection_style: LibraryCardSelectionStyle,
    /// ライブラリ画面のホイールスクロール速度レベル（1〜10）
    #[serde(
        default = "default_library_wheel_speed",
        deserialize_with = "deserialize_u16_clamped_library_wheel_speed"
    )]
    pub library_wheel_speed: u16,
    /// ライブラリ HUD のフォントサイズレベル（1〜9、標準=5）
    #[serde(
        default = "default_library_hud_font_level",
        deserialize_with = "deserialize_library_hud_font_level"
    )]
    pub library_hud_font_level: u16,
    /// 画像フォルダを本として開く
    #[serde(default = "default_folder_book_open_as_viewer")]
    pub folder_book_open_as_viewer: bool,
    /// 最後に読んだ位置から再開する
    #[serde(default = "default_resume_from_last_reading_position")]
    pub resume_from_last_reading_position: bool,
    /// 外部ツール設定（最大3件）
    #[serde(default = "default_external_tools")]
    pub external_tools: Vec<ExternalTool>,
}

#[derive(Serialize)]
struct AppSettingsEnvelope {
    schema_version: u16,
    #[serde(flatten)]
    settings: AppSettings,
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
fn default_external_tools() -> Vec<ExternalTool> {
    Vec::new()
}

#[allow(dead_code)]
fn deserialize_u16_clamped_library_wheel_speed<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let level = u16::deserialize(deserializer)?;
    Ok(level.clamp(LIBRARY_WHEEL_SPEED_MIN, LIBRARY_WHEEL_SPEED_MAX))
}

#[allow(dead_code)]
fn deserialize_library_hud_font_level<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let level = u16::deserialize(deserializer)?;
    Ok(level.clamp(LIBRARY_HUD_FONT_LEVEL_MIN, LIBRARY_HUD_FONT_LEVEL_MAX))
}

#[allow(dead_code)]
fn deserialize_viewer_rgba_cache_max_mb<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = u16::deserialize(deserializer)?;
    Ok(raw.max(PERFORMANCE_CACHE_MIN_MIB))
}

#[allow(dead_code)]
fn deserialize_viewer_background_worker_count<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = u16::deserialize(deserializer)?;
    Ok(raw.max(1))
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
            viewer_open_mode: VIEWER_OPEN_MODE_DEFAULT,
            reading_direction: READING_DIRECTION_DEFAULT,
            library_hud_mode: LIBRARY_HUD_MODE_DEFAULT,
            library_hud_style: LIBRARY_HUD_STYLE_DEFAULT,
            library_card_selection_style: LIBRARY_CARD_SELECTION_STYLE_DEFAULT,
            library_wheel_speed: LIBRARY_WHEEL_SPEED_DEFAULT,
            library_hud_font_level: LIBRARY_HUD_FONT_LEVEL_DEFAULT,
            folder_book_open_as_viewer: FOLDER_BOOK_OPEN_AS_VIEWER_DEFAULT,
            resume_from_last_reading_position: false,
            external_tools: default_external_tools(),
        }
    }
}

impl AppSettings {
    #[allow(dead_code)]
    pub fn load() -> Self {
        let resources = crate::infra::system_resources::detect_pc_resources();
        Self::load_with_resources(&resources)
    }

    pub fn load_with_resources(resources: &PerformanceResources) -> Self {
        let path = Self::settings_path();
        let mut settings = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => match load_app_settings_from_value(value, resources) {
                    Some(settings) => settings,
                    None => {
                        tracing::warn!(
                            path = %path.display(),
                            setting = "app_settings",
                            "invalid app settings schema or root shape; using default"
                        );
                        Self::default_for_resources(resources)
                    }
                },
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        path = %path.display(),
                        setting = "app_settings",
                        "failed to parse json settings; using default"
                    );
                    Self::default_for_resources(resources)
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Self::default_for_resources(resources)
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    path = %path.display(),
                    setting = "app_settings",
                    "failed to read json settings; using default"
                );
                Self::default_for_resources(resources)
            }
        };
        settings.normalize_for_resources(resources);
        settings.sanitize_external_tools();
        settings
    }

    #[allow(dead_code)]
    pub fn save(&self) {
        let resources = crate::infra::system_resources::detect_pc_resources();
        self.save_with_resources(&resources);
    }

    pub fn save_with_resources(&self, resources: &PerformanceResources) {
        let mut normalized = self.clone();
        normalized.normalize_for_resources(resources);
        normalized.sanitize_external_tools();
        let path = Self::settings_path();
        let envelope = AppSettingsEnvelope {
            schema_version: APP_SETTINGS_SCHEMA_VERSION,
            settings: normalized,
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&envelope) {
            let _ = std::fs::write(&path, json);
        }
    }

    pub fn default_for_resources(resources: &PerformanceResources) -> Self {
        let mut settings = Self::default();
        let defaults = resources.default_performance_settings();
        settings.viewer_l1_vram_cache_max_mb = defaults.l1_vram_cache_max_mib;
        settings.viewer_rgba_cache_max_mb = defaults.l2_ram_cache_max_mib;
        settings.viewer_background_worker_count = defaults.background_worker_count as u16;
        settings
    }

    pub fn normalized_performance_settings(
        &self,
        resources: &PerformanceResources,
    ) -> PerformanceSettingsResolved {
        resources.resolved_performance_settings(
            self.viewer_l1_vram_cache_max_mb,
            self.viewer_rgba_cache_max_mb,
            self.viewer_background_worker_count,
            self.viewer_danger_zone_enabled,
        )
    }

    pub fn normalize_for_resources(&mut self, resources: &PerformanceResources) {
        self.viewer_l1_vram_cache_max_mb = resources.normalize_l1_mib(
            self.viewer_l1_vram_cache_max_mb,
            self.viewer_danger_zone_enabled,
        );
        self.viewer_rgba_cache_max_mb = resources.normalize_l2_mib(
            self.viewer_rgba_cache_max_mb,
            self.viewer_danger_zone_enabled,
        );
        self.viewer_background_worker_count = resources.normalize_bg_workers(
            self.viewer_background_worker_count,
            self.viewer_danger_zone_enabled,
        );
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

    pub fn settings_path() -> PathBuf {
        let local = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        local
            .join(crate::app_identity::app_data_dir())
            .join("settings.json")
    }
}

fn load_app_settings_from_value(
    value: serde_json::Value,
    resources: &PerformanceResources,
) -> Option<AppSettings> {
    let obj = value.as_object()?;
    let schema_version = obj.get("schema_version")?.as_u64()?;
    if schema_version != APP_SETTINGS_SCHEMA_VERSION as u64 {
        return None;
    }

    let mut settings = AppSettings::default_for_resources(resources);

    if let Some(value) = obj.get("thumb_display_w").and_then(parse_u16) {
        settings.thumb_display_w = value;
    }
    if let Some(value) = obj.get("ui_language").and_then(parse_ui_language) {
        settings.ui_language = value;
    }
    if let Some(value) = obj.get("viewer_quality").and_then(parse_viewer_quality) {
        settings.viewer_quality = value;
    }
    if let Some(value) = obj.get("viewer_l1_vram_cache_max_mb").and_then(parse_u16) {
        settings.viewer_l1_vram_cache_max_mb = value;
    }
    if let Some(value) = obj.get("viewer_rgba_cache_max_mb").and_then(parse_u16) {
        settings.viewer_rgba_cache_max_mb = value;
    }
    if let Some(value) = obj
        .get("viewer_background_worker_count")
        .and_then(parse_u16)
    {
        settings.viewer_background_worker_count = value;
    }
    if let Some(value) = obj
        .get("viewer_danger_zone_enabled")
        .and_then(serde_json::Value::as_bool)
    {
        settings.viewer_danger_zone_enabled = value;
    }
    if let Some(value) = obj.get("viewer_open_mode").and_then(parse_viewer_open_mode) {
        settings.viewer_open_mode = value;
    }
    if let Some(value) = obj
        .get("reading_direction")
        .and_then(parse_reading_direction)
    {
        settings.reading_direction = value;
    }
    if let Some(value) = obj.get("library_hud_mode").and_then(parse_library_hud_mode) {
        settings.library_hud_mode = value;
    }
    if let Some(value) = obj
        .get("library_hud_style")
        .and_then(parse_library_hud_style)
    {
        settings.library_hud_style = value;
    }
    if let Some(value) = obj
        .get("library_card_selection_style")
        .and_then(parse_library_card_selection_style)
    {
        settings.library_card_selection_style = value;
    }
    if let Some(value) = obj.get("library_wheel_speed").and_then(parse_u16) {
        settings.library_wheel_speed = value;
    }
    if let Some(value) = obj.get("library_hud_font_level").and_then(parse_u16) {
        settings.library_hud_font_level = value;
    }
    if let Some(value) = obj
        .get("folder_book_open_as_viewer")
        .and_then(serde_json::Value::as_bool)
    {
        settings.folder_book_open_as_viewer = value;
    }
    if let Some(value) = obj
        .get("resume_from_last_reading_position")
        .and_then(serde_json::Value::as_bool)
    {
        settings.resume_from_last_reading_position = value;
    }
    if let Some(value) = obj.get("external_tools").and_then(parse_external_tools) {
        settings.external_tools = value;
    }

    Some(settings)
}

fn parse_u16(value: &serde_json::Value) -> Option<u16> {
    value.as_u64().and_then(|raw| u16::try_from(raw).ok())
}

fn parse_ui_language(value: &serde_json::Value) -> Option<UiLanguage> {
    UiLanguage::from_code(value.as_str()?)
}

fn parse_viewer_quality(value: &serde_json::Value) -> Option<ViewerQuality> {
    match value.as_str()? {
        "speed" => Some(ViewerQuality::Speed),
        "balanced" => Some(ViewerQuality::Balanced),
        "quality" => Some(ViewerQuality::Quality),
        "original" => Some(ViewerQuality::Original),
        _ => None,
    }
}

fn parse_viewer_open_mode(value: &serde_json::Value) -> Option<ViewerOpenMode> {
    match value.as_str()? {
        "windowed" => Some(ViewerOpenMode::Windowed),
        "fullscreen" => Some(ViewerOpenMode::Fullscreen),
        _ => None,
    }
}

fn parse_reading_direction(value: &serde_json::Value) -> Option<ReadingDirection> {
    match value.as_str()? {
        "right_to_left" => Some(ReadingDirection::RightToLeft),
        "left_to_right" => Some(ReadingDirection::LeftToRight),
        _ => None,
    }
}

fn parse_library_hud_mode(value: &serde_json::Value) -> Option<LibraryHudMode> {
    match value.as_str()? {
        "off" => Some(LibraryHudMode::Off),
        "on" => Some(LibraryHudMode::On),
        _ => None,
    }
}

fn parse_library_hud_style(value: &serde_json::Value) -> Option<LibraryHudStyle> {
    parse_library_hud_style_name(value.as_str()?)
}

fn parse_library_hud_style_name(value: &str) -> Option<LibraryHudStyle> {
    match value {
        "default" => Some(LibraryHudStyle::Default),
        "white" => Some(LibraryHudStyle::White),
        "blue" => Some(LibraryHudStyle::Blue),
        "high_contrast" => Some(LibraryHudStyle::HighContrast),
        "amber" => Some(LibraryHudStyle::Amber),
        "rose" => Some(LibraryHudStyle::Rose),
        "violet" => Some(LibraryHudStyle::Violet),
        _ => None,
    }
}

fn parse_library_card_selection_style(
    value: &serde_json::Value,
) -> Option<LibraryCardSelectionStyle> {
    match value.as_str()? {
        "default" => Some(LibraryCardSelectionStyle::Default),
        "violet" => Some(LibraryCardSelectionStyle::Violet),
        "amber" => Some(LibraryCardSelectionStyle::Amber),
        "rose" => Some(LibraryCardSelectionStyle::Rose),
        "high_contrast" => Some(LibraryCardSelectionStyle::HighContrast),
        _ => None,
    }
}

fn parse_external_tools(value: &serde_json::Value) -> Option<Vec<ExternalTool>> {
    let items = value.as_array()?;
    let mut tools = Vec::with_capacity(items.len());
    for item in items {
        if let Ok(tool) = serde_json::from_value::<ExternalTool>(item.clone()) {
            tools.push(tool);
        }
    }
    Some(tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::archive_settings::FileSettings;
    use std::{ffi::OsString, path::Path, sync::Mutex};

    static SAVE_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct LocalAppDataGuard {
        previous: Option<OsString>,
    }

    impl LocalAppDataGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("LOCALAPPDATA");
            std::env::set_var("LOCALAPPDATA", path);
            Self { previous }
        }
    }

    impl Drop for LocalAppDataGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("LOCALAPPDATA", value),
                None => std::env::remove_var("LOCALAPPDATA"),
            }
        }
    }

    fn tool(shortcut: ExternalToolShortcut) -> ExternalTool {
        ExternalTool {
            name: format!("Tool-{}", shortcut.as_char()),
            executable: "tool.exe".to_owned(),
            args: "--json \"{path}\"".to_owned(),
            shortcut,
            background: true,
        }
    }

    #[test]
    fn sanitize_external_tools_deduplicates_and_limits_to_max() {
        let mut settings = AppSettings {
            external_tools: vec![
                tool(ExternalToolShortcut::E),
                tool(ExternalToolShortcut::F),
                tool(ExternalToolShortcut::E),
                tool(ExternalToolShortcut::G),
                tool(ExternalToolShortcut::H),
            ],
            ..AppSettings::default()
        };
        settings.sanitize_external_tools();

        assert_eq!(settings.external_tools.len(), EXTERNAL_TOOLS_MAX);
        assert_eq!(settings.external_tools[0].shortcut, ExternalToolShortcut::E);
        assert_eq!(settings.external_tools[1].shortcut, ExternalToolShortcut::F);
        assert_eq!(settings.external_tools[2].shortcut, ExternalToolShortcut::G);
    }

    #[test]
    fn next_available_shortcut_works() {
        let tools = vec![tool(ExternalToolShortcut::E), tool(ExternalToolShortcut::F)];
        assert_eq!(
            AppSettings::next_available_external_tool_shortcut(&tools),
            Some(ExternalToolShortcut::G)
        );
    }

    #[test]
    fn normalize_executable_strips_outer_quotes_and_spaces() {
        assert_eq!(
            normalize_external_tool_executable(" \"C:\\Program Files\\a.exe\" "),
            r"C:\Program Files\a.exe"
        );
        assert_eq!(
            normalize_external_tool_executable(r"C:\tool.exe"),
            r"C:\tool.exe"
        );
    }

    #[test]
    fn sanitize_external_tools_normalizes_executable() {
        let mut settings = AppSettings {
            external_tools: vec![ExternalTool {
                name: "opt".to_owned(),
                executable: "\"C:\\cbz-tools\\cbz-opt.exe\"".to_owned(),
                args: "\"{path}\"".to_owned(),
                shortcut: ExternalToolShortcut::E,
                background: false,
            }],
            ..AppSettings::default()
        };
        settings.sanitize_external_tools();
        assert_eq!(
            settings.external_tools[0].executable,
            r"C:\cbz-tools\cbz-opt.exe"
        );
        assert_eq!(settings.external_tools[0].args, "\"{path}\"");
    }

    #[test]
    fn save_paths_use_versioned_envelopes_and_stable_enum_values() {
        let _guard = SAVE_TEST_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let _local_app_data = LocalAppDataGuard::set(temp_dir.path());

        assert_eq!(
            AppSettings::default().viewer_quality,
            VIEWER_QUALITY_DEFAULT
        );
        assert_eq!(AppSettings::default().ui_language, UiLanguage::English);
        assert_eq!(
            AppSettings::default().viewer_open_mode,
            VIEWER_OPEN_MODE_DEFAULT
        );
        assert_eq!(
            AppSettings::default().reading_direction,
            READING_DIRECTION_DEFAULT
        );
        assert_eq!(
            FileSettings::default().spread_mode,
            crate::domain::archive_settings::SpreadMode::Auto
        );
        assert_eq!(FileSettings::default().quality_override, None);
        assert_eq!(FileSettings::default().reading_direction_override, None);

        let resources = PerformanceResources {
            physical_ram_bytes: 8 * 1024 * 1024 * 1024,
            dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            logical_cpu_count: 8,
            gpu_adapter_name: None,
        };

        let settings = AppSettings {
            viewer_quality: ViewerQuality::Original,
            viewer_open_mode: ViewerOpenMode::Fullscreen,
            reading_direction: ReadingDirection::LeftToRight,
            library_hud_mode: LibraryHudMode::Off,
            library_hud_style: LibraryHudStyle::Rose,
            library_card_selection_style: LibraryCardSelectionStyle::Amber,
            ui_language: UiLanguage::Japanese,
            external_tools: vec![ExternalTool {
                name: "Tool".to_owned(),
                executable: "tool.exe".to_owned(),
                args: "--json".to_owned(),
                shortcut: ExternalToolShortcut::Q,
                background: false,
            }],
            ..AppSettings::default()
        };
        settings.save_with_resources(&resources);

        let data_dir = temp_dir.path().join(crate::app_identity::app_data_dir());
        let settings_path = data_dir.join("settings.json");
        let settings_json = std::fs::read_to_string(&settings_path).expect("read settings.json");
        let settings_value: serde_json::Value =
            serde_json::from_str(&settings_json).expect("parse settings.json");
        assert_eq!(settings_value["schema_version"], serde_json::json!(1));
        assert_eq!(
            settings_value["viewer_quality"],
            serde_json::json!("original")
        );
        assert_eq!(
            settings_value["viewer_open_mode"],
            serde_json::json!("fullscreen")
        );
        assert_eq!(settings_value["ui_language"], serde_json::json!("ja"));
        assert_eq!(
            settings_value["reading_direction"],
            serde_json::json!("left_to_right")
        );
        assert_eq!(settings_value["library_hud_mode"], serde_json::json!("off"));
        assert_eq!(
            settings_value["library_hud_style"],
            serde_json::json!("rose")
        );
        assert_eq!(
            settings_value["library_card_selection_style"],
            serde_json::json!("amber")
        );
        assert_eq!(
            settings_value["external_tools"][0]["shortcut"],
            serde_json::json!("q")
        );

        let book_path = PathBuf::from(r"C:\books\alpha.cbz");
        let mut store = crate::domain::archive_settings::SettingsStore::load();
        store.set_spread_mode(
            book_path.clone(),
            crate::domain::archive_settings::SpreadMode::Spread,
        );
        store.set_quality_override(book_path.clone(), Some(ViewerQuality::Balanced));
        store
            .set_reading_direction_override(book_path.clone(), Some(ReadingDirection::LeftToRight));

        let file_settings = store.get(&book_path);
        assert_eq!(
            file_settings.spread_mode,
            crate::domain::archive_settings::SpreadMode::Spread
        );
        assert_eq!(
            file_settings.quality_override,
            Some(ViewerQuality::Balanced)
        );
        assert_eq!(
            file_settings.reading_direction_override,
            Some(ReadingDirection::LeftToRight)
        );

        let book_state_path = data_dir.join("book_state.json");
        let book_state_json =
            std::fs::read_to_string(&book_state_path).expect("read book_state.json");
        let book_state_value: serde_json::Value =
            serde_json::from_str(&book_state_json).expect("parse book_state.json");
        assert_eq!(book_state_value["schema_version"], serde_json::json!(1));
        let books = book_state_value["books"]
            .as_object()
            .expect("books object in book_state.json");
        assert_eq!(books.len(), 1);
        let book_value = books.values().next().expect("single book entry");
        assert_eq!(book_value["spread_mode"], serde_json::json!("spread"));
        assert_eq!(
            book_value["quality_override"],
            serde_json::json!("balanced")
        );
        assert_eq!(
            book_value["reading_direction_override"],
            serde_json::json!("left_to_right")
        );
    }

    #[test]
    fn load_app_settings_recovers_individual_fields() {
        let _guard = SAVE_TEST_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let _local_app_data = LocalAppDataGuard::set(temp_dir.path());

        let resources = PerformanceResources {
            physical_ram_bytes: 8 * 1024 * 1024 * 1024,
            dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            logical_cpu_count: 8,
            gpu_adapter_name: None,
        };

        let settings_path = AppSettings::settings_path();
        let json = r#"{
            "schema_version": 1,
            "viewer_quality": "unknown",
            "viewer_l1_vram_cache_max_mb": 1024,
            "viewer_rgba_cache_max_mb": 2048,
            "library_wheel_speed": "invalid",
            "viewer_open_mode": "fullscreen",
            "reading_direction": "left_to_right",
            "library_hud_mode": "off",
            "library_hud_style": "violet",
            "library_card_selection_style": "high_contrast",
            "external_tools": [
                {
                    "name": "ok",
                    "executable": "tool.exe",
                    "args": "--ok",
                    "shortcut": "q",
                    "background": true
                },
                {
                    "name": "bad",
                    "executable": "tool.exe",
                    "args": "--bad",
                    "shortcut": "unknown",
                    "background": true
                },
                {
                    "name": "also-ok",
                    "executable": "tool2.exe",
                    "args": "--ok2",
                    "shortcut": "r",
                    "background": false
                }
            ]
        }"#;
        if let Some(dir) = settings_path.parent() {
            std::fs::create_dir_all(dir).expect("create settings dir");
        }
        std::fs::write(&settings_path, json).expect("write settings.json");

        let settings = AppSettings::load_with_resources(&resources);
        assert_eq!(settings.viewer_quality, VIEWER_QUALITY_DEFAULT);
        assert_eq!(settings.viewer_l1_vram_cache_max_mb, 1024);
        assert_eq!(settings.viewer_rgba_cache_max_mb, 2048);
        assert_eq!(settings.library_wheel_speed, LIBRARY_WHEEL_SPEED_DEFAULT);
        assert_eq!(settings.viewer_open_mode, ViewerOpenMode::Fullscreen);
        assert_eq!(settings.reading_direction, ReadingDirection::LeftToRight);
        assert_eq!(settings.library_hud_mode, LibraryHudMode::Off);
        assert_eq!(settings.library_hud_style, LibraryHudStyle::Violet);
        assert_eq!(
            settings.library_card_selection_style,
            LibraryCardSelectionStyle::HighContrast
        );
        assert_eq!(settings.external_tools.len(), 2);
        assert_eq!(settings.external_tools[0].shortcut, ExternalToolShortcut::Q);
        assert_eq!(settings.external_tools[1].shortcut, ExternalToolShortcut::R);
    }

    #[test]
    fn load_app_settings_recovers_ui_language_field_individually() {
        let _guard = SAVE_TEST_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let _local_app_data = LocalAppDataGuard::set(temp_dir.path());

        let resources = PerformanceResources {
            physical_ram_bytes: 8 * 1024 * 1024 * 1024,
            dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            logical_cpu_count: 8,
            gpu_adapter_name: None,
        };

        let missing = serde_json::json!({"schema_version": 1});
        assert_eq!(
            load_app_settings_from_value(missing, &resources)
                .expect("load missing ui_language")
                .ui_language,
            UiLanguage::English
        );
        assert_eq!(
            load_app_settings_from_value(serde_json::json!({"schema_version": 1}), &resources)
                .expect("load missing hud style")
                .library_hud_style,
            LIBRARY_HUD_STYLE_DEFAULT
        );
        assert_eq!(
            load_app_settings_from_value(serde_json::json!({"schema_version": 1}), &resources)
                .expect("load missing selection style")
                .library_card_selection_style,
            LIBRARY_CARD_SELECTION_STYLE_DEFAULT
        );

        let invalid_type = serde_json::json!({
            "schema_version": 1,
            "ui_language": 123
        });
        assert_eq!(
            load_app_settings_from_value(invalid_type, &resources)
                .expect("load invalid ui_language type")
                .ui_language,
            UiLanguage::English
        );

        let unknown_value = serde_json::json!({
            "schema_version": 1,
            "ui_language": "zh-Hans"
        });
        assert_eq!(
            load_app_settings_from_value(unknown_value, &resources)
                .expect("load unknown ui_language")
                .ui_language,
            UiLanguage::English
        );

        let japanese = serde_json::json!({
            "schema_version": 1,
            "ui_language": "ja"
        });
        assert_eq!(
            load_app_settings_from_value(japanese, &resources)
                .expect("load japanese ui_language")
                .ui_language,
            UiLanguage::Japanese
        );
    }

    #[test]
    fn load_app_settings_rejects_unknown_schema_version() {
        let _guard = SAVE_TEST_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let _local_app_data = LocalAppDataGuard::set(temp_dir.path());

        let resources = PerformanceResources {
            physical_ram_bytes: 8 * 1024 * 1024 * 1024,
            dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            logical_cpu_count: 8,
            gpu_adapter_name: None,
        };

        let settings_path = AppSettings::settings_path();
        if let Some(dir) = settings_path.parent() {
            std::fs::create_dir_all(dir).expect("create settings dir");
        }
        std::fs::write(
            &settings_path,
            r#"{"schema_version":2,"viewer_quality":"original"}"#,
        )
        .expect("write settings.json");

        let settings = AppSettings::load_with_resources(&resources);
        assert_eq!(settings.viewer_quality, VIEWER_QUALITY_DEFAULT);
        assert_eq!(settings.viewer_open_mode, VIEWER_OPEN_MODE_DEFAULT);
        assert_eq!(settings.reading_direction, READING_DIRECTION_DEFAULT);
    }

    #[test]
    fn load_app_settings_defaults_on_broken_root_json() {
        let _guard = SAVE_TEST_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let _local_app_data = LocalAppDataGuard::set(temp_dir.path());

        let settings_path = AppSettings::settings_path();
        if let Some(dir) = settings_path.parent() {
            std::fs::create_dir_all(dir).expect("create settings dir");
        }
        std::fs::write(
            &settings_path,
            r#"{"schema_version":1,"thumb_display_w":200"#,
        )
        .expect("write broken settings.json");

        let resources = PerformanceResources {
            physical_ram_bytes: 8 * 1024 * 1024 * 1024,
            dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
            logical_cpu_count: 8,
            gpu_adapter_name: None,
        };
        let settings = AppSettings::load_with_resources(&resources);
        assert_eq!(settings.viewer_quality, VIEWER_QUALITY_DEFAULT);
        assert_eq!(settings.external_tools.len(), 0);
    }
}
