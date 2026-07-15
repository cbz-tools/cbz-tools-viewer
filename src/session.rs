//! セッション永続化。
//!
//! 終了時に JSON へ保存し、次回起動時に復元する。
//! 保存先: `%LOCALAPPDATA%\cbz-viewer\session.json`

use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::domain::app_settings::{
    ViewerQuality, VIEWER_BACKGROUND_WORKER_COUNT_DEFAULT, VIEWER_QUALITY_DEFAULT,
    VIEWER_RGBA_CACHE_MAX_MB_DEFAULT,
};
use crate::domain::sort::{SortKey, SortOrder};
use crate::util::path_eq::normalize_path_for_selection;

// ── SessionState ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct SessionState {
    /// ウィンドウ左上 X（スクリーン座標、論理ピクセル）
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
    /// ウィンドウサイズ（最大化状態を除く）
    pub window_w: f32,
    pub window_h: f32,
    /// Viewer 用ウィンドウ左上 X（スクリーン座標、論理ピクセル）
    #[serde(default)]
    pub viewer_window_x: Option<f32>,
    #[serde(default)]
    pub viewer_window_y: Option<f32>,
    /// Viewer 用ウィンドウサイズ（最大化状態を除く）
    #[serde(default)]
    pub viewer_window_w: Option<f32>,
    #[serde(default)]
    pub viewer_window_h: Option<f32>,
    /// Viewer 用ウィンドウ最大化状態
    #[serde(default)]
    pub viewer_window_maximized: Option<bool>,

    /// 最後に開いていたディレクトリ
    pub last_dir: Option<String>,
    /// ソートキー（文字列形式）
    pub sort_key: String,
    /// ソート順（文字列形式: asc / desc）
    #[serde(default)]
    pub sort_order: String,
    /// グリッドの垂直スクロール量（論理ピクセル）
    pub grid_scroll_y: f32,
    /// 選択中ファイルの絶対パス（起動時に存在確認して復元）
    pub selected_path: Option<String>,
    /// サイドバーのライブラリ一覧
    #[serde(default)]
    pub favorite_dirs: Vec<String>,
    /// ライブラリ画面のフィルタ文字列
    #[serde(default)]
    pub filter_text: String,
    /// ビューア画質設定
    #[serde(default = "default_viewer_quality")]
    pub viewer_quality: ViewerQuality,
    /// ビューアRGBAキャッシュ上限（MB）
    #[serde(default = "default_viewer_rgba_cache_max_mb")]
    pub viewer_rgba_cache_max_mb: u16,
    /// ビューア先読みバックグラウンド処理数
    #[serde(default = "default_viewer_background_worker_count")]
    pub viewer_background_worker_count: u16,
    #[serde(default)]
    pub left_pane_tab: LeftPaneTab,
    #[serde(default)]
    pub history: VecDeque<HistoryEntry>,
}

#[derive(Default, serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum LeftPaneTab {
    #[default]
    Library,
    History,
}

#[derive(Default, serde::Serialize, serde::Deserialize, Clone)]
pub struct HistoryEntry {
    pub path: PathBuf,
    #[serde(default)]
    pub normalized_path: String,
    pub opened_at_ms: u64,
    pub file_size: Option<u64>,
    pub modified_unix_ns: Option<u128>,
}

fn default_viewer_quality() -> ViewerQuality {
    VIEWER_QUALITY_DEFAULT
}
fn default_viewer_rgba_cache_max_mb() -> u16 {
    VIEWER_RGBA_CACHE_MAX_MB_DEFAULT
}
fn default_viewer_background_worker_count() -> u16 {
    VIEWER_BACKGROUND_WORKER_COUNT_DEFAULT
}

impl SessionState {
    const DEFAULT_W: f32 = 1200.0;
    const DEFAULT_H: f32 = 800.0;

    /// ファイルから読み込む（失敗時はデフォルト）
    pub fn load() -> Self {
        let mut state = crate::infra::config_io::load_json_or_default::<Self>(
            &Self::file_path(),
            "session_state",
        );
        state.sanitize_history_entries();
        state
    }

    /// ファイルへ保存する
    pub fn save(&self) {
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let path = Self::file_path();
        if let Err(error) = crate::infra::config_io::atomic_write(&path, json.as_bytes()) {
            tracing::warn!(path = %path.display(), %error, "failed to save session state");
        }
    }

    fn file_path() -> PathBuf {
        app_base_dir().join("session.json")
    }

    // ── ウィンドウジオメトリ ──────────────────────────────────────────────────

    /// バリデーション済みの初期ウィンドウ位置とサイズを返す。
    /// 保存された位置がモニタ上にない場合はプライマリモニタ中央にフォールバック。
    pub fn valid_window_geometry(&self) -> (eframe::egui::Pos2, eframe::egui::Vec2) {
        let w = if self.window_w >= 400.0 {
            self.window_w
        } else {
            Self::DEFAULT_W
        };
        let h = if self.window_h >= 300.0 {
            self.window_h
        } else {
            Self::DEFAULT_H
        };

        if let (Some(x), Some(y)) = (self.window_x, self.window_y) {
            if is_pos_on_screen(x, y, w, h) {
                return (eframe::egui::pos2(x, y), eframe::egui::vec2(w, h));
            }
        }

        // プライマリモニタ中央へフォールバック
        let (mw, mh) = primary_monitor_size();
        let cx = ((mw - w) * 0.5).max(0.0);
        let cy = ((mh - h) * 0.5).max(0.0);
        (eframe::egui::pos2(cx, cy), eframe::egui::vec2(w, h))
    }

    /// Viewer 用に保存した初期ウィンドウ位置とサイズを返す。
    /// 保存値が不足している場合やサイズが不正な場合は `None`。
    pub fn valid_viewer_window_geometry(&self) -> Option<(eframe::egui::Pos2, eframe::egui::Vec2)> {
        let (Some(x), Some(y), Some(w), Some(h)) = (
            self.viewer_window_x,
            self.viewer_window_y,
            self.viewer_window_w,
            self.viewer_window_h,
        ) else {
            return None;
        };
        if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
            return None;
        }
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        Some((eframe::egui::pos2(x, y), eframe::egui::vec2(w, h)))
    }

    #[cfg(windows)]
    pub fn viewer_monitor_rect_from_saved_geometry(&self) -> Option<[f32; 4]> {
        let (Some(x), Some(y), Some(w), Some(h)) = (
            self.viewer_window_x,
            self.viewer_window_y,
            self.viewer_window_w,
            self.viewer_window_h,
        ) else {
            return None;
        };
        if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
            return None;
        }
        if w <= 0.0 || h <= 0.0 {
            return None;
        }

        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        };

        let center = POINT {
            x: (x + w * 0.5).round() as i32,
            y: (y + h * 0.5).round() as i32,
        };
        // SAFETY:
        // 中心点から nearest monitor を引き、成功時だけ `MONITORINFO` を読み取る。
        // `cbSize` は Win32 要件どおり設定済みで、失敗時は `None` を返す。
        unsafe {
            let monitor = MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST);
            if monitor.0.is_null() {
                return None;
            }
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            let rc = info.rcMonitor;
            Some([
                rc.left as f32,
                rc.top as f32,
                (rc.right - rc.left) as f32,
                (rc.bottom - rc.top) as f32,
            ])
        }
    }

    #[cfg(not(windows))]
    pub fn viewer_monitor_rect_from_saved_geometry(&self) -> Option<[f32; 4]> {
        None
    }

    // ── ソートキー変換 ────────────────────────────────────────────────────────

    pub fn parse_sort_key(&self) -> SortKey {
        match self.sort_key.as_str() {
            "modified" => SortKey::Modified,
            "size" => SortKey::Size,
            "page_count" => SortKey::PageCount,
            _ => SortKey::NameNatural,
        }
    }

    pub fn parse_sort_order(&self) -> SortOrder {
        match self.sort_order.as_str() {
            "asc" => SortOrder::Asc,
            "desc" => SortOrder::Desc,
            _ => default_sort_order_for_key(&self.parse_sort_key()),
        }
    }

    pub fn push_history(&mut self, path: PathBuf, opened_at_ms: u64) {
        const MAX_HISTORY: usize = 500;
        let normalized = normalize_path_for_selection(&path);
        self.history.retain(|e| e.normalized_path != normalized);
        let (file_size, modified_unix_ns) = std::fs::metadata(&path)
            .map(|m| {
                (
                    Some(m.len()),
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_nanos())),
                )
            })
            .unwrap_or((None, None));
        self.history.push_front(HistoryEntry {
            normalized_path: normalized,
            path,
            opened_at_ms,
            file_size,
            modified_unix_ns,
        });
        self.history.truncate(MAX_HISTORY);
    }

    fn sanitize_history_entries(&mut self) {
        for entry in &mut self.history {
            if entry.normalized_path.is_empty() {
                entry.normalized_path = normalize_path_for_selection(&entry.path);
            }
        }
    }
}

pub fn unix_ns_to_system_time(ns: u128) -> Option<SystemTime> {
    let ns_u64 = u64::try_from(ns).ok()?;
    let duration = Duration::from_nanos(ns_u64);
    UNIX_EPOCH.checked_add(duration)
}

pub(crate) fn app_base_dir() -> PathBuf {
    // 現在は Windows 前提で LOCALAPPDATA を使用する。
    // 将来的に macOS/Linux 対応を行う際は dirs クレートの config_dir への移行を検討する。
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join(crate::app_identity::app_data_dir())
}

pub fn sort_key_to_str(key: &SortKey) -> &'static str {
    match key {
        SortKey::NameNatural => "name",
        SortKey::Modified => "modified",
        SortKey::Size => "size",
        SortKey::PageCount => "page_count",
    }
}

pub fn sort_order_to_str(order: &SortOrder) -> &'static str {
    match order {
        SortOrder::Asc => "asc",
        SortOrder::Desc => "desc",
    }
}

fn default_sort_order_for_key(key: &SortKey) -> SortOrder {
    match key {
        SortKey::NameNatural => SortOrder::Asc,
        SortKey::Modified | SortKey::Size | SortKey::PageCount => SortOrder::Desc,
    }
}

// ── モニタ判定 ────────────────────────────────────────────────────────────────

/// ウィンドウの中心点が仮想スクリーン内にあるか（全モニタの合算領域）
fn is_pos_on_screen(x: f32, y: f32, w: f32, h: f32) -> bool {
    #[cfg(windows)]
    {
        let (vx, vy, vw, vh) = virtual_screen_rect();
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        cx >= vx && cx < vx + vw && cy >= vy && cy < vy + vh
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y, w, h);
        true
    }
}

fn primary_monitor_size() -> (f32, f32) {
    #[cfg(windows)]
    // SM_CXSCREEN = 0, SM_CYSCREEN = 1
    // SAFETY: `GetSystemMetrics` は読み取り専用 API で、引数は固定の system metric ID。
    unsafe {
        (GetSystemMetrics(0) as f32, GetSystemMetrics(1) as f32)
    }
    #[cfg(not(windows))]
    {
        (1920.0, 1080.0)
    }
}

#[cfg(windows)]
fn virtual_screen_rect() -> (f32, f32, f32, f32) {
    // SM_XVIRTUALSCREEN=76, SM_YVIRTUALSCREEN=77
    // SM_CXVIRTUALSCREEN=78, SM_CYVIRTUALSCREEN=79
    // SAFETY: `GetSystemMetrics` は読み取り専用 API で、引数は仮想スクリーンの fixed ID。
    unsafe {
        (
            GetSystemMetrics(76) as f32,
            GetSystemMetrics(77) as f32,
            GetSystemMetrics(78) as f32,
            GetSystemMetrics(79) as f32,
        )
    }
}

#[cfg(windows)]
#[link(name = "User32")]
extern "system" {
    fn GetSystemMetrics(nIndex: i32) -> i32;
}
