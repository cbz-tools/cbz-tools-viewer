use serde::{Deserialize, Serialize};

use crate::domain::app_settings::{
    AppSettings, ExternalTool, ExternalToolShortcut, LIBRARY_HUD_FONT_LEVEL_MAX,
    LIBRARY_HUD_FONT_LEVEL_MIN, LIBRARY_WHEEL_SPEED_MAX, LIBRARY_WHEEL_SPEED_MIN,
    LibraryCardSelectionStyle, LibraryHudMode, LibraryHudStyle, ReadingDirection, UiLanguage,
    ViewerOpenMode, ViewerQuality,
};
use crate::domain::performance::{
    PERFORMANCE_CACHE_MIN_MIB, SPAD_RAM_RATIO_MAX_PERCENT, SPAD_RAM_RATIO_MIN_PERCENT,
};

const APP_SETTINGS_SCHEMA_VERSION: u16 = 1;

pub(super) mod ui_language_serde {
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
pub(super) mod viewer_quality_serde {
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
pub(super) mod viewer_open_mode_serde {
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
pub(super) mod reading_direction_serde {
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
pub(super) mod library_hud_mode_serde {
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
pub(super) mod library_hud_style_serde {
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
pub(super) mod library_card_selection_style_serde {
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

pub(super) mod external_tool_shortcut_serde {
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

#[derive(Serialize)]
struct AppSettingsEnvelope {
    schema_version: u16,
    #[serde(flatten)]
    settings: AppSettings,
}

#[allow(dead_code)]
pub(super) fn deserialize_u16_clamped_library_wheel_speed<'de, D>(
    deserializer: D,
) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let level = u16::deserialize(deserializer)?;
    Ok(level.clamp(LIBRARY_WHEEL_SPEED_MIN, LIBRARY_WHEEL_SPEED_MAX))
}

#[allow(dead_code)]
pub(super) fn deserialize_library_hud_font_level<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let level = u16::deserialize(deserializer)?;
    Ok(level.clamp(LIBRARY_HUD_FONT_LEVEL_MIN, LIBRARY_HUD_FONT_LEVEL_MAX))
}

#[allow(dead_code)]
pub(super) fn deserialize_viewer_rgba_cache_max_mb<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = u16::deserialize(deserializer)?;
    Ok(raw.max(PERFORMANCE_CACHE_MIN_MIB))
}

#[allow(dead_code)]
pub(super) fn deserialize_viewer_background_worker_count<'de, D>(
    deserializer: D,
) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = u16::deserialize(deserializer)?;
    Ok(raw.max(1))
}

#[allow(dead_code)]
pub(super) fn deserialize_viewer_spad_ram_ratio_percent<'de, D>(
    deserializer: D,
) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = u16::deserialize(deserializer)?;
    Ok((raw.min(u8::MAX as u16) as u8)
        .clamp(SPAD_RAM_RATIO_MIN_PERCENT, SPAD_RAM_RATIO_MAX_PERCENT))
}

pub(crate) fn decode_settings_json(
    text: &str,
    defaults: AppSettings,
) -> Result<Option<AppSettings>, serde_json::Error> {
    serde_json::from_str::<serde_json::Value>(text)
        .map(|value| load_app_settings_from_value(value, defaults))
}

pub(crate) fn encode_settings_json(settings: AppSettings) -> Result<String, serde_json::Error> {
    let envelope = AppSettingsEnvelope {
        schema_version: APP_SETTINGS_SCHEMA_VERSION,
        settings,
    };
    serde_json::to_string_pretty(&envelope)
}

fn load_app_settings_from_value(
    value: serde_json::Value,
    mut settings: AppSettings,
) -> Option<AppSettings> {
    let obj = value.as_object()?;
    let schema_version = obj.get("schema_version")?.as_u64()?;
    if schema_version != APP_SETTINGS_SCHEMA_VERSION as u64 {
        return None;
    }

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
    if let Some(value) = obj.get("viewer_spad_ram_ratio_percent").and_then(parse_u8) {
        settings.viewer_spad_ram_ratio_percent = value;
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
    if let Some(value) = obj
        .get("open_rebuilt_cbz_in_new_viewer")
        .and_then(serde_json::Value::as_bool)
    {
        settings.open_rebuilt_cbz_in_new_viewer = value;
    }
    if let Some(value) = obj.get("external_tools").and_then(parse_external_tools) {
        settings.external_tools = value;
    }

    settings.viewer_spad_ram_ratio_percent = settings
        .viewer_spad_ram_ratio_percent
        .clamp(SPAD_RAM_RATIO_MIN_PERCENT, SPAD_RAM_RATIO_MAX_PERCENT);

    Some(settings)
}

fn parse_u16(value: &serde_json::Value) -> Option<u16> {
    value.as_u64().and_then(|raw| u16::try_from(raw).ok())
}

fn parse_u8(value: &serde_json::Value) -> Option<u8> {
    value.as_u64().map(|raw| raw.min(u8::MAX as u64) as u8)
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
