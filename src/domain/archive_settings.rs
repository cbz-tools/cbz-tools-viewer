//! 本ごとの設定ストア。
//!
//! 見開き、表紙ブランク、読書状態などを `%LOCALAPPDATA%\cbz-viewer\book_state.json` へ保存する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::domain::app_settings::{ReadingDirection, ViewerQuality};
use crate::util::archive_path::is_supported_image_path;

const BOOK_STATE_SCHEMA_VERSION: u16 = 1;

// ── 設定値 ────────────────────────────────────────────────────────────────────

/// 読書状態。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingState {
    #[default]
    Unread,
    Reading,
    Read,
}

/// 見開きモード設定
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SpreadMode {
    /// 自動判定（表示候補の 2 ページが両方とも h/w ≥ 1.1 なら見開き）
    #[default]
    Auto,
    /// 常に単ページ
    Single,
    /// 常に見開き
    Spread,
}

mod spread_mode_serde {
    use super::SpreadMode;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Value {
        Auto,
        Single,
        Spread,
    }

    impl From<SpreadMode> for Value {
        fn from(value: SpreadMode) -> Self {
            match value {
                SpreadMode::Auto => Self::Auto,
                SpreadMode::Single => Self::Single,
                SpreadMode::Spread => Self::Spread,
            }
        }
    }

    impl From<Value> for SpreadMode {
        fn from(value: Value) -> Self {
            match value {
                Value::Auto => Self::Auto,
                Value::Single => Self::Single,
                Value::Spread => Self::Spread,
            }
        }
    }

    pub fn serialize<S>(value: &SpreadMode, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Value::from(value.clone()).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SpreadMode, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Into::into)
    }
}

mod option_viewer_quality_serde {
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

    pub fn serialize<S>(value: &Option<ViewerQuality>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mapped = value.map(Value::from);
        mapped.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<ViewerQuality>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<Value>::deserialize(deserializer).map(|value| value.map(Into::into))
    }
}

mod option_reading_direction_serde {
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

    pub fn serialize<S>(value: &Option<ReadingDirection>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mapped = value.map(Value::from);
        mapped.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<ReadingDirection>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<Value>::deserialize(deserializer).map(|value| value.map(Into::into))
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FileSettings {
    /// 見開きモードで表紙の前に仮想ブランクページを挿入する
    pub cover_blank: bool,
    /// 見開きモード設定
    #[serde(with = "spread_mode_serde")]
    pub spread_mode: SpreadMode,
    /// スライドショー間隔（秒）
    pub slideshow_interval_secs: f32,
    /// 読書状態
    #[serde(default)]
    pub reading_state: ReadingState,
    /// 最後に読んだページからの再開位置
    #[serde(default)]
    pub resume_page: Option<usize>,
    /// 読書中に到達した総ページ数
    #[serde(default)]
    pub reading_page_count: Option<usize>,
    /// 本ごとの画質 override（None はグローバル設定に従う）
    #[serde(default, with = "option_viewer_quality_serde")]
    pub quality_override: Option<ViewerQuality>,
    /// 本ごとのページ開き override（None はグローバル設定に従う）
    #[serde(default, with = "option_reading_direction_serde")]
    pub reading_direction_override: Option<ReadingDirection>,
}

pub const SLIDESHOW_INTERVAL_CHOICES: [f32; 12] = [
    0.5, 1.0, 2.0, 3.0, 5.0, 7.0, 10.0, 15.0, 20.0, 30.0, 45.0, 60.0,
];
pub const DEFAULT_SLIDESHOW_INTERVAL_SECS: f32 = 0.5;

pub fn clamp_slideshow_interval_secs(secs: f32) -> f32 {
    if SLIDESHOW_INTERVAL_CHOICES.contains(&secs) {
        secs
    } else {
        DEFAULT_SLIDESHOW_INTERVAL_SECS
    }
}

impl Default for FileSettings {
    fn default() -> Self {
        Self {
            cover_blank: false,
            spread_mode: SpreadMode::Auto,
            slideshow_interval_secs: DEFAULT_SLIDESHOW_INTERVAL_SECS,
            reading_state: ReadingState::Unread,
            resume_page: None,
            reading_page_count: None,
            quality_override: None,
            reading_direction_override: None,
        }
    }
}

#[derive(serde::Serialize)]
struct BookStateEnvelope {
    schema_version: u16,
    books: HashMap<PathBuf, FileSettings>,
}

// ── SettingsStore ─────────────────────────────────────────────────────────────

pub struct SettingsStore {
    data: HashMap<PathBuf, FileSettings>,
    file_path: PathBuf,
    dirty: bool,
}

impl SettingsStore {
    pub fn load() -> Self {
        let file_path = settings_json_path();
        let data = load_book_state_books(&file_path);
        Self {
            data,
            file_path,
            dirty: false,
        }
    }

    pub fn get(&self, path: &Path) -> FileSettings {
        self.data.get(path).cloned().unwrap_or_default()
    }

    pub fn set_cover_blank(&mut self, path: PathBuf, value: bool) {
        self.data.entry(path).or_default().cover_blank = value;
        self.dirty = true;
        self.flush();
    }

    pub fn set_spread_mode(&mut self, path: PathBuf, value: SpreadMode) {
        self.data.entry(path).or_default().spread_mode = value;
        self.dirty = true;
        self.flush();
    }

    pub fn set_slideshow_interval_secs(&mut self, path: PathBuf, value: f32) {
        self.data.entry(path).or_default().slideshow_interval_secs =
            clamp_slideshow_interval_secs(value);
        self.dirty = true;
        self.flush();
    }

    pub fn set_quality_override(&mut self, path: PathBuf, value: Option<ViewerQuality>) {
        self.data.entry(path).or_default().quality_override = value;
        self.dirty = true;
        self.flush();
    }

    pub fn set_reading_direction_override(
        &mut self,
        path: PathBuf,
        value: Option<ReadingDirection>,
    ) {
        self.data
            .entry(path)
            .or_default()
            .reading_direction_override = value;
        self.dirty = true;
        self.flush();
    }

    pub fn remove_path_from_disk(path: &Path) {
        let file_path = settings_json_path();
        let mut data = load_book_state_books(&file_path);
        if data.remove(path).is_none() {
            return;
        }
        write_book_state(&file_path, data);
    }

    pub fn rename_path_on_disk(old_path: &Path, new_path: &Path) {
        let file_path = settings_json_path();
        let mut data = load_book_state_books(&file_path);
        let Some(settings) = data.remove(old_path) else {
            return;
        };
        data.insert(new_path.to_path_buf(), settings);
        write_book_state(&file_path, data);
    }

    pub fn update_reading_session_on_disk(
        path: &Path,
        displayed_any_page: bool,
        reached_end: bool,
        resume_page: Option<usize>,
        page_count: usize,
    ) {
        let file_path = settings_json_path();
        let mut data = load_book_state_books(&file_path);
        let book_path = reading_session_book_path(path);
        let mut settings = data.get(&book_path).cloned().unwrap_or_default();
        if !apply_reading_session_update(
            &mut settings,
            displayed_any_page,
            reached_end,
            resume_page,
            page_count,
        ) {
            return;
        }
        data.insert(book_path, settings);
        write_book_state(&file_path, data);
    }

    /// ダーティなら保存（update ループ末尾などで呼ぶ）
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let envelope = BookStateEnvelope {
            schema_version: BOOK_STATE_SCHEMA_VERSION,
            books: self.data.clone(),
        };
        if let Ok(content) = serde_json::to_string_pretty(&envelope) {
            if crate::infra::config_io::atomic_write(&self.file_path, content.as_bytes()).is_ok() {
                self.dirty = false;
            } else {
                tracing::warn!(path = %self.file_path.display(), "failed to save book settings json");
            }
        } else {
            tracing::warn!(path = %self.file_path.display(), "failed to serialize book settings json");
        }
    }
}

// ── 内部 ─────────────────────────────────────────────────────────────────────

fn settings_json_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join(crate::app_identity::app_data_dir())
        .join("book_state.json")
}

fn load_book_state_books(path: &Path) -> HashMap<PathBuf, FileSettings> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => match load_book_state_from_value(value) {
                Some(books) => sanitize_book_settings(books),
                None => {
                    tracing::warn!(
                        path = %path.display(),
                        setting = "book_state",
                        "invalid book state schema or root shape; using default"
                    );
                    HashMap::new()
                }
            },
            Err(err) => {
                tracing::warn!(
                    ?err,
                    path = %path.display(),
                    setting = "book_state",
                    "failed to parse json settings; using default"
                );
                HashMap::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(err) => {
            tracing::warn!(
                ?err,
                path = %path.display(),
                setting = "book_state",
                "failed to read json settings; using default"
            );
            HashMap::new()
        }
    }
}

fn load_book_state_from_value(value: serde_json::Value) -> Option<HashMap<PathBuf, FileSettings>> {
    let obj = value.as_object()?;
    let schema_version = obj.get("schema_version")?.as_u64()?;
    if schema_version != BOOK_STATE_SCHEMA_VERSION as u64 {
        return None;
    }

    let books_value = obj.get("books")?;
    let books_obj = match books_value.as_object() {
        Some(value) => value,
        None => return Some(HashMap::new()),
    };

    let mut books = HashMap::with_capacity(books_obj.len());
    for (key, value) in books_obj {
        if key.is_empty() || key.contains('\0') {
            continue;
        }
        let Ok(path) = serde_json::from_value::<PathBuf>(serde_json::Value::String(key.clone()))
        else {
            continue;
        };
        let Ok(settings) = serde_json::from_value::<FileSettings>(value.clone()) else {
            continue;
        };
        books.insert(path, settings);
    }
    Some(books)
}

fn reading_session_book_path(path: &Path) -> PathBuf {
    book_settings_path(path)
}

pub(crate) fn book_settings_path(path: &Path) -> PathBuf {
    if is_supported_image_path(path) {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn apply_reading_session_update(
    settings: &mut FileSettings,
    displayed_any_page: bool,
    reached_end: bool,
    resume_page: Option<usize>,
    page_count: usize,
) -> bool {
    let before = settings.clone();
    if displayed_any_page {
        settings.reading_state = if reached_end {
            ReadingState::Read
        } else {
            ReadingState::Reading
        };
        if page_count > 0 {
            settings.reading_page_count = Some(page_count);
        }
    }
    if let Some(resume_page) = resume_page {
        settings.resume_page = Some(resume_page);
    }
    *settings != before
}

fn sanitize_book_settings(
    mut data: HashMap<PathBuf, FileSettings>,
) -> HashMap<PathBuf, FileSettings> {
    for settings in data.values_mut() {
        settings.slideshow_interval_secs =
            clamp_slideshow_interval_secs(settings.slideshow_interval_secs);
    }
    data
}

fn write_book_state(path: &Path, data: HashMap<PathBuf, FileSettings>) {
    let envelope = BookStateEnvelope {
        schema_version: BOOK_STATE_SCHEMA_VERSION,
        books: data,
    };
    if let Ok(content) = serde_json::to_string_pretty(&envelope) {
        if let Err(error) = crate::infra::config_io::atomic_write(path, content.as_bytes()) {
            tracing::warn!(path = %path.display(), %error, "failed to save book settings json");
        }
    }
}
