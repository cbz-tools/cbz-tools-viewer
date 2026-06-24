#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod app_identity;
mod domain;
mod infra;
mod platform;
mod session;
mod ui;
mod util;

use crate::infra::archive::folder::FolderImageReader;
use crate::util::archive_path::{is_supported_archive_path, is_supported_image_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupMode {
    Library,
    ViewerStandalone,
    ViewerLibrary,
}

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub mode: StartupMode,
    pub initial_library_dir: Option<std::path::PathBuf>,
    pub startup_select_path: Option<std::path::PathBuf>,
    pub pipe_name: Option<String>,
    pub viewer_snapshot_only_ipc: bool,
    pub viewer_start_page: Option<u32>,
    pub map_make_skip: bool,
    pub viewer_offline: bool,
    pub start_fullscreen: bool,
    /// Library ウィンドウの左上位置。保存済み Viewer 矩形がない初回 Windowed 起動のフォールバック。
    pub viewer_window_pos: Option<[f32; 2]>,
    /// Library ウィンドウ中心が属するモニターの rcMonitor。保存済み Viewer モニター取得失敗時のフォールバック。
    pub viewer_monitor_rect: Option<[f32; 4]>,
    /// Library から直接 Fullscreen 起動するときの初期 Viewport 矩形。モニター選択情報ではない。
    pub viewer_fullscreen_target: Option<[f32; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PositionalPathResolution {
    Exact,
    TrailingQuote,
    TrailingSeparator,
    TrailingQuoteThenSeparator,
    UnresolvedRaw,
}

impl std::fmt::Display for PositionalPathResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exact => f.write_str("exact"),
            Self::TrailingQuote => f.write_str("trailing_quote"),
            Self::TrailingSeparator => f.write_str("trailing_separator"),
            Self::TrailingQuoteThenSeparator => f.write_str("trailing_quote_then_separator"),
            Self::UnresolvedRaw => f.write_str("unresolved_raw"),
        }
    }
}

#[derive(Debug, Default)]
struct LaunchCliState {
    pipe_name: Option<String>,
    positional_raw: Option<String>,
    start_fullscreen: bool,
    viewer_snapshot_only_ipc: bool,
    viewer_start_page: Option<u32>,
    map_make_skip: bool,
    viewer_offline: bool,
    viewer_window_pos: Option<[f32; 2]>,
    viewer_monitor_rect: Option<[f32; 4]>,
    viewer_fullscreen_target: Option<[f32; 4]>,
}

impl LaunchCliState {
    fn has_viewer_flags(&self) -> bool {
        self.start_fullscreen
            || self.viewer_snapshot_only_ipc
            || self.viewer_start_page.is_some()
            || self.map_make_skip
            || self.viewer_offline
            || self.viewer_window_pos.is_some()
            || self.viewer_monitor_rect.is_some()
            || self.viewer_fullscreen_target.is_some()
    }
}

#[derive(Debug)]
struct ResolvedLaunchTarget {
    raw_path: String,
    path: std::path::PathBuf,
    resolution: PositionalPathResolution,
}

struct ViewerLaunchRequest {
    mode: StartupMode,
    initial_library_dir: Option<std::path::PathBuf>,
    startup_select_path: std::path::PathBuf,
    pipe_name: Option<String>,
    viewer_snapshot_only_ipc: bool,
    viewer_start_page: Option<u32>,
    viewer_offline: bool,
    allow_fullscreen_target: bool,
}

fn resolve_positional_path(
    raw_path: &str,
) -> Option<(std::path::PathBuf, PositionalPathResolution)> {
    let raw = std::path::PathBuf::from(raw_path);
    if raw.exists() {
        return Some((raw, PositionalPathResolution::Exact));
    }

    let mut candidates = Vec::with_capacity(4);

    if raw_path.ends_with('"') {
        let stripped_quote = raw_path.trim_end_matches('"');
        if !stripped_quote.is_empty() {
            candidates.push((
                std::path::PathBuf::from(stripped_quote),
                PositionalPathResolution::TrailingQuote,
            ));
        }
    }

    if matches!(raw_path.chars().last(), Some('\\') | Some('/')) {
        let stripped_separator = raw_path.trim_end_matches(['\\', '/']);
        if !stripped_separator.is_empty() {
            candidates.push((
                std::path::PathBuf::from(stripped_separator),
                PositionalPathResolution::TrailingSeparator,
            ));
        }
    }

    if raw_path.ends_with('"') {
        let stripped_quote = raw_path.trim_end_matches('"');
        let stripped_quote_then_separator = stripped_quote.trim_end_matches(['\\', '/']);
        if !stripped_quote_then_separator.is_empty() {
            candidates.push((
                std::path::PathBuf::from(stripped_quote_then_separator),
                PositionalPathResolution::TrailingQuoteThenSeparator,
            ));
        }
    }

    candidates
        .into_iter()
        .find(|(candidate, _)| candidate.exists())
}

fn parse_f32_arg(
    iter: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> anyhow::Result<f32> {
    let Some(value) = iter.next() else {
        anyhow::bail!("missing value for {flag}");
    };
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid {flag}: {value}"))
}

fn parse_u32_arg(
    iter: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> anyhow::Result<u32> {
    let Some(value) = iter.next() else {
        anyhow::bail!("missing value for {flag}");
    };
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid {flag}: {value}"))
}

fn parse_pipe_arg(
    state: &mut LaunchCliState,
    iter: &mut impl Iterator<Item = String>,
) -> anyhow::Result<()> {
    let Some(value) = iter.next() else {
        anyhow::bail!("missing value for --pipe");
    };
    if state.pipe_name.is_some() {
        anyhow::bail!("--pipe specified multiple times");
    }
    state.pipe_name = Some(value);
    Ok(())
}

fn parse_viewer_window_args(
    state: &mut LaunchCliState,
    arg: &str,
    iter: &mut impl Iterator<Item = String>,
) -> anyhow::Result<bool> {
    match arg {
        "--viewer-window-x" => {
            let x = parse_f32_arg(iter, "--viewer-window-x")?;
            let y = state.viewer_window_pos.map(|p| p[1]).unwrap_or(0.0);
            state.viewer_window_pos = Some([x, y]);
            Ok(true)
        }
        "--viewer-window-y" => {
            let y = parse_f32_arg(iter, "--viewer-window-y")?;
            let x = state.viewer_window_pos.map(|p| p[0]).unwrap_or(0.0);
            state.viewer_window_pos = Some([x, y]);
            Ok(true)
        }
        "--viewer-monitor-x" => {
            let x = parse_f32_arg(iter, "--viewer-monitor-x")?;
            let [_, y, w, h] = state.viewer_monitor_rect.unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_monitor_rect = Some([x, y, w, h]);
            Ok(true)
        }
        "--viewer-monitor-y" => {
            let y = parse_f32_arg(iter, "--viewer-monitor-y")?;
            let [x, _, w, h] = state.viewer_monitor_rect.unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_monitor_rect = Some([x, y, w, h]);
            Ok(true)
        }
        "--viewer-monitor-w" => {
            let w = parse_f32_arg(iter, "--viewer-monitor-w")?;
            let [x, y, _, h] = state.viewer_monitor_rect.unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_monitor_rect = Some([x, y, w, h]);
            Ok(true)
        }
        "--viewer-monitor-h" => {
            let h = parse_f32_arg(iter, "--viewer-monitor-h")?;
            let [x, y, w, _] = state.viewer_monitor_rect.unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_monitor_rect = Some([x, y, w, h]);
            Ok(true)
        }
        "--viewer-full-x" => {
            let x = parse_f32_arg(iter, "--viewer-full-x")?;
            let [_, y, w, h] = state
                .viewer_fullscreen_target
                .unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_fullscreen_target = Some([x, y, w, h]);
            Ok(true)
        }
        "--viewer-full-y" => {
            let y = parse_f32_arg(iter, "--viewer-full-y")?;
            let [x, _, w, h] = state
                .viewer_fullscreen_target
                .unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_fullscreen_target = Some([x, y, w, h]);
            Ok(true)
        }
        "--viewer-full-w" => {
            let w = parse_f32_arg(iter, "--viewer-full-w")?;
            let [x, y, _, h] = state
                .viewer_fullscreen_target
                .unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_fullscreen_target = Some([x, y, w, h]);
            Ok(true)
        }
        "--viewer-full-h" => {
            let h = parse_f32_arg(iter, "--viewer-full-h")?;
            let [x, y, w, _] = state
                .viewer_fullscreen_target
                .unwrap_or([0.0, 0.0, 0.0, 0.0]);
            state.viewer_fullscreen_target = Some([x, y, w, h]);
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_viewer_flag_args(
    state: &mut LaunchCliState,
    arg: &str,
    iter: &mut impl Iterator<Item = String>,
) -> anyhow::Result<bool> {
    match arg {
        "--fullscreen" => {
            state.start_fullscreen = true;
            Ok(true)
        }
        "--viewer-snapshot-only-ipc" => {
            state.viewer_snapshot_only_ipc = true;
            Ok(true)
        }
        "--viewer-offline" => {
            state.viewer_offline = true;
            Ok(true)
        }
        "--viewer-start-page" => {
            state.viewer_start_page = Some(parse_u32_arg(iter, "--viewer-start-page")?);
            Ok(true)
        }
        "--map-make-skip" => {
            state.map_make_skip = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_cli_args(raw_args: &[String]) -> anyhow::Result<LaunchCliState> {
    let mut state = LaunchCliState::default();
    let mut iter = raw_args.iter().cloned();
    let _exe = iter.next();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--pipe" => parse_pipe_arg(&mut state, &mut iter)?,
            _ if parse_viewer_flag_args(&mut state, arg.as_str(), &mut iter)? => {}
            _ if parse_viewer_window_args(&mut state, arg.as_str(), &mut iter)? => {}
            _ if arg.starts_with("--") => anyhow::bail!("unknown option: {arg}"),
            _ => {
                if state.positional_raw.is_some() {
                    anyhow::bail!("multiple positional paths are not supported");
                }
                state.positional_raw = Some(arg);
            }
        }
    }

    Ok(state)
}

fn resolve_positional_target(
    state: &LaunchCliState,
    raw_args: &[String],
) -> anyhow::Result<ResolvedLaunchTarget> {
    let Some(raw_path) = state.positional_raw.clone() else {
        anyhow::bail!("internal error: positional target is missing");
    };
    let (path, resolution) = resolve_positional_path(raw_path.as_str()).unwrap_or_else(|| {
        (
            std::path::PathBuf::from(&raw_path),
            PositionalPathResolution::UnresolvedRaw,
        )
    });
    tracing::info!(
        raw_args = ?raw_args,
        positional_raw = %raw_path,
        positional = %path.display(),
        resolution = %resolution,
        exists = path.exists(),
        is_dir = path.is_dir(),
        is_file = path.is_file(),
        "launch candidate resolved"
    );
    Ok(ResolvedLaunchTarget {
        raw_path,
        path,
        resolution,
    })
}

fn build_library_launch(initial_library_dir: Option<std::path::PathBuf>) -> LaunchOptions {
    LaunchOptions {
        mode: StartupMode::Library,
        initial_library_dir,
        startup_select_path: None,
        pipe_name: None,
        viewer_snapshot_only_ipc: false,
        viewer_start_page: None,
        map_make_skip: false,
        viewer_offline: false,
        start_fullscreen: false,
        viewer_window_pos: None,
        viewer_monitor_rect: None,
        viewer_fullscreen_target: None,
    }
}

fn build_viewer_launch(state: &LaunchCliState, request: ViewerLaunchRequest) -> LaunchOptions {
    LaunchOptions {
        mode: request.mode,
        initial_library_dir: request.initial_library_dir,
        startup_select_path: Some(request.startup_select_path),
        pipe_name: request.pipe_name,
        viewer_snapshot_only_ipc: request.viewer_snapshot_only_ipc,
        viewer_start_page: request.viewer_start_page,
        map_make_skip: state.map_make_skip,
        viewer_offline: request.viewer_offline,
        start_fullscreen: state.start_fullscreen,
        viewer_window_pos: state.viewer_window_pos,
        viewer_monitor_rect: state.viewer_monitor_rect,
        viewer_fullscreen_target: request
            .allow_fullscreen_target
            .then_some(state.viewer_fullscreen_target)
            .flatten(),
    }
}

fn build_directory_launch(
    state: &LaunchCliState,
    path: std::path::PathBuf,
) -> anyhow::Result<LaunchOptions> {
    match (state.pipe_name.clone(), state.viewer_offline) {
        (Some(_), true) => {
            anyhow::bail!("--pipe cannot be combined with --viewer-offline for a directory path")
        }
        (Some(pipe), false) => Ok(build_viewer_launch(
            state,
            ViewerLaunchRequest {
                mode: StartupMode::ViewerLibrary,
                initial_library_dir: path.parent().map(std::path::Path::to_path_buf),
                startup_select_path: path,
                pipe_name: Some(pipe),
                viewer_snapshot_only_ipc: state.viewer_snapshot_only_ipc,
                viewer_start_page: state.viewer_start_page,
                viewer_offline: false,
                allow_fullscreen_target: true,
            },
        )),
        (None, true) => Ok(build_viewer_launch(
            state,
            ViewerLaunchRequest {
                mode: StartupMode::ViewerStandalone,
                initial_library_dir: path.parent().map(std::path::Path::to_path_buf),
                startup_select_path: path,
                pipe_name: None,
                viewer_snapshot_only_ipc: false,
                viewer_start_page: state.viewer_start_page,
                viewer_offline: true,
                allow_fullscreen_target: true,
            },
        )),
        (None, false) => {
            if state.has_viewer_flags() {
                anyhow::bail!("viewer-only options cannot be used when starting in library mode");
            }
            Ok(build_library_launch(Some(path)))
        }
    }
}

fn build_archive_launch(state: &LaunchCliState, path: std::path::PathBuf) -> LaunchOptions {
    match state.pipe_name.clone() {
        Some(pipe) => build_viewer_launch(
            state,
            ViewerLaunchRequest {
                mode: StartupMode::ViewerLibrary,
                initial_library_dir: path.parent().map(std::path::Path::to_path_buf),
                startup_select_path: path,
                pipe_name: Some(pipe),
                viewer_snapshot_only_ipc: state.viewer_snapshot_only_ipc,
                viewer_start_page: None,
                viewer_offline: false,
                allow_fullscreen_target: true,
            },
        ),
        None => build_viewer_launch(
            state,
            ViewerLaunchRequest {
                mode: StartupMode::ViewerStandalone,
                initial_library_dir: path.parent().map(std::path::Path::to_path_buf),
                startup_select_path: path,
                pipe_name: None,
                viewer_snapshot_only_ipc: false,
                viewer_start_page: None,
                viewer_offline: false,
                allow_fullscreen_target: false,
            },
        ),
    }
}

fn build_image_launch(
    state: &LaunchCliState,
    path: std::path::PathBuf,
) -> anyhow::Result<LaunchOptions> {
    if state.pipe_name.is_some() {
        anyhow::bail!("--pipe cannot be used with an image file path");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("image file requires a parent folder: {}", path.display()))?
        .to_path_buf();
    let reader = FolderImageReader::open(parent.as_path()).map_err(|e| anyhow::anyhow!(e))?;
    let start_page = reader.page_index_for_path(path.as_path()).ok_or_else(|| {
        anyhow::anyhow!("image file not found in folder reader: {}", path.display())
    })?;
    Ok(build_viewer_launch(
        state,
        ViewerLaunchRequest {
            mode: StartupMode::ViewerStandalone,
            initial_library_dir: Some(parent.clone()),
            startup_select_path: parent,
            pipe_name: None,
            viewer_snapshot_only_ipc: false,
            viewer_start_page: Some(start_page),
            viewer_offline: true,
            allow_fullscreen_target: false,
        },
    ))
}

fn build_launch_mode(
    state: &LaunchCliState,
    target: Option<ResolvedLaunchTarget>,
) -> anyhow::Result<LaunchOptions> {
    let Some(target) = target else {
        if state.pipe_name.is_some() {
            anyhow::bail!("--pipe requires a positional archive path");
        }
        if state.has_viewer_flags() {
            anyhow::bail!("viewer-only options require a positional archive path");
        }
        return Ok(build_library_launch(None));
    };

    let _ = (&target.raw_path, target.resolution);
    let path = target.path;
    if !path.exists() {
        anyhow::bail!("path does not exist: {}", path.display());
    }
    if path.is_dir() {
        return build_directory_launch(state, path);
    }
    if path.is_file() && is_supported_archive_path(path.as_path()) {
        return Ok(build_archive_launch(state, path));
    }
    if path.is_file() && is_supported_image_path(path.as_path()) {
        return build_image_launch(state, path);
    }
    anyhow::bail!("unsupported file path: {}", path.display())
}

fn parse_launch_options<I>(args: I) -> anyhow::Result<LaunchOptions>
where
    I: IntoIterator<Item = String>,
{
    let raw_args: Vec<String> = args.into_iter().collect();
    let state = parse_cli_args(&raw_args)?;
    let target = state
        .positional_raw
        .as_ref()
        .map(|_| resolve_positional_target(&state, &raw_args))
        .transpose()?;
    build_launch_mode(&state, target)
}

fn cleanup_old_logs_by_prefix(prefix: &str, keep_count: usize) {
    let Ok(entries) = std::fs::read_dir(".") else {
        return;
    };

    let mut logs: Vec<_> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.starts_with(prefix) && s.ends_with(".log"))
                .unwrap_or(false)
        })
        .collect();

    logs.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });

    if logs.len() <= keep_count {
        return;
    }

    for entry in &logs[..logs.len() - keep_count] {
        let _ = std::fs::remove_file(entry.path());
    }
}

fn cleanup_old_viewer_logs() {
    const KEEP_COUNT: usize = 5;
    cleanup_old_logs_by_prefix(crate::app_identity::IPC_LOG_PREFIX, KEEP_COUNT);
    cleanup_old_logs_by_prefix(
        crate::app_identity::VIEWER_STANDALONE_LOG_PREFIX,
        KEEP_COUNT,
    );
}

fn app_window_icon() -> Option<eframe::egui::IconData> {
    let bytes = include_bytes!("../assets/viewer_icon_256.png");
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some(eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn select_log_file_name_from_args(args: &[String]) -> String {
    let mut has_pipe = false;
    let mut first_positional: Option<&str> = None;
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--pipe" {
            has_pipe = true;
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--") {
            let needs_value = matches!(
                arg.as_str(),
                "--viewer-window-x"
                    | "--viewer-window-y"
                    | "--viewer-monitor-x"
                    | "--viewer-monitor-y"
                    | "--viewer-monitor-w"
                    | "--viewer-monitor-h"
                    | "--viewer-start-page"
                    | "--viewer-full-x"
                    | "--viewer-full-y"
                    | "--viewer-full-w"
                    | "--viewer-full-h"
            );
            if needs_value {
                let _ = iter.next();
            }
            continue;
        }
        if first_positional.is_none() {
            first_positional = Some(arg.as_str());
        }
    }

    if has_pipe {
        format!(
            "{}{}.log",
            crate::app_identity::IPC_LOG_PREFIX,
            std::process::id()
        )
    } else if let Some(path) = first_positional {
        if std::path::Path::new(path).is_dir() {
            "cbz-library.log".to_owned()
        } else {
            format!(
                "{}{}.log",
                crate::app_identity::VIEWER_STANDALONE_LOG_PREFIX,
                std::process::id()
            )
        }
    } else {
        "cbz-library.log".to_owned()
    }
}

fn main() -> anyhow::Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let log_file_name = select_log_file_name_from_args(&raw_args);

    // デフォルトログレベルは debug build で debug、release build で warn。
    let default_level = if cfg!(debug_assertions) {
        format!("{}=debug", crate::app_identity::LOG_TARGET)
    } else {
        format!("{}=warn", crate::app_identity::LOG_TARGET)
    };
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));

    if log_file_name.starts_with(crate::app_identity::APP_ID) {
        cleanup_old_viewer_logs();
    }

    if let Ok(log_file) = std::fs::File::create(&log_file_name) {
        tracing_subscriber::fmt()
            .with_writer(log_file)
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(env_filter)
            .init();
        eprintln!("failed to create {log_file_name}; fallback to stderr logging");
    }

    tracing::info!(log_file = %log_file_name, "log output initialized");

    let launch = match parse_launch_options(raw_args) {
        Ok(launch) => launch,
        Err(err) => {
            eprintln!("Error: {err}");
            tracing::error!(error = %err, "startup argument parse failed");
            return Err(err);
        }
    };

    tracing::debug!(
        mode = ?launch.mode,
        initial_library_dir = ?launch.initial_library_dir,
        startup_select_path = ?launch.startup_select_path,
        has_pipe = launch.pipe_name.is_some(),
        viewer_snapshot_only_ipc = launch.viewer_snapshot_only_ipc,
        viewer_start_page = ?launch.viewer_start_page,
        viewer_offline = launch.viewer_offline,
        start_fullscreen = launch.start_fullscreen,
        viewer_window_pos = ?launch.viewer_window_pos,
        viewer_monitor_rect = ?launch.viewer_monitor_rect,
        viewer_fullscreen_target = ?launch.viewer_fullscreen_target,
        "startup launch options parsed"
    );

    // セッション読み込み（失敗時はデフォルト）
    let sess = session::SessionState::load();
    let (win_pos, win_size) = sess.valid_window_geometry();
    let viewer_window_geometry = sess.valid_viewer_window_geometry();
    let saved_viewer_center = viewer_window_geometry
        .map(|(pos, size)| eframe::egui::pos2(pos.x + size.x * 0.5, pos.y + size.y * 0.5));
    let saved_viewer_monitor_rect = sess.viewer_monitor_rect_from_saved_geometry();
    let restore_maximized = sess.viewer_window_maximized.unwrap_or(false);
    let is_valid_monitor_rect = |rect: [f32; 4]| {
        rect[2].is_finite() && rect[3].is_finite() && rect[2] > 1.0 && rect[3] > 1.0
    };
    // Windowed の最大化復元は、保存済み Viewer モニターを最優先にし、なければ Library 由来情報へ落とす。
    let startup_maximize_monitor_rect = if restore_maximized {
        saved_viewer_monitor_rect
            .map(|rect| ("saved-viewer", rect))
            .or_else(|| {
                launch
                    .viewer_monitor_rect
                    .filter(|rect| is_valid_monitor_rect(*rect))
                    .map(|rect| ("library-fallback", rect))
            })
            .or_else(|| {
                viewer_window_geometry.map(|(viewer_pos, viewer_size)| {
                    (
                        "saved-viewer-rect",
                        [viewer_pos.x, viewer_pos.y, viewer_size.x, viewer_size.y],
                    )
                })
            })
    } else {
        None
    };
    // Fullscreen は開始時矩形が別用途なので、保存済み Viewer モニター→Fullscreen 専用矩形→Library モニターの順で選ぶ。
    let fullscreen_monitor_rect = if launch.start_fullscreen {
        saved_viewer_monitor_rect
            .map(|rect| ("saved-viewer", rect))
            .or_else(|| {
                launch
                    .viewer_fullscreen_target
                    .filter(|rect| is_valid_monitor_rect(*rect))
                    .map(|rect| ("library-fallback", rect))
            })
            .or_else(|| {
                launch
                    .viewer_monitor_rect
                    .filter(|rect| is_valid_monitor_rect(*rect))
                    .map(|rect| ("library-fallback", rect))
            })
    } else {
        None
    };

    let mut viewport =
        eframe::egui::ViewportBuilder::default().with_title(crate::app_identity::PRODUCT_NAME);
    if let Some(icon) = app_window_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    tracing::debug!(
        viewer_window_pos = ?launch.viewer_window_pos,
        viewer_monitor_rect = ?launch.viewer_monitor_rect,
        start_fullscreen = launch.start_fullscreen,
        "viewer startup launch geometry parsed"
    );
    tracing::debug!(
        saved_viewer_center = ?saved_viewer_center.map(|p| [p.x, p.y]),
        saved_viewer_monitor_rect = ?saved_viewer_monitor_rect,
        restore_maximized,
        "viewer startup monitor source evaluated"
    );
    match launch.mode {
        StartupMode::ViewerLibrary | StartupMode::ViewerStandalone if !launch.start_fullscreen => {
            if restore_maximized {
                // 最大化 Windowed は monitor を優先し、保存済み矩形が無い場合だけ通常矩形へ戻す。
                if let Some((source, [mx, my, mw, mh])) = startup_maximize_monitor_rect {
                    tracing::debug!(
                        source,
                        selected_monitor_rect = ?[mx, my, mw, mh],
                        "viewer startup maximize monitor selected"
                    );
                    viewport = viewport
                        .with_position(eframe::egui::pos2(mx, my))
                        .with_inner_size(eframe::egui::vec2(mw, mh))
                        .with_maximized(false);
                } else if let Some((viewer_pos, viewer_size)) = viewer_window_geometry {
                    tracing::debug!(
                        source = "saved-viewer-rect",
                        selected_monitor_rect =
                            ?[viewer_pos.x, viewer_pos.y, viewer_size.x, viewer_size.y],
                        "viewer startup maximize monitor selected"
                    );
                    viewport = viewport
                        .with_position(viewer_pos)
                        .with_inner_size(viewer_size)
                        .with_maximized(false);
                } else {
                    tracing::debug!(
                        source = "default",
                        selected_monitor_rect = ?[win_pos.x, win_pos.y, win_size.x, win_size.y],
                        "viewer startup maximize monitor selected"
                    );
                    if let Some([x, y]) = launch.viewer_window_pos {
                        viewport = viewport.with_position(eframe::egui::pos2(x, y));
                    } else {
                        viewport = viewport.with_position(win_pos);
                    }
                    viewport = viewport.with_inner_size(win_size);
                    viewport = viewport.with_maximized(false);
                }
            } else {
                // 通常 Windowed は保存済み Viewer 矩形を優先し、なければ Library の起動位置へ落とす。
                if let Some((viewer_pos, viewer_size)) = viewer_window_geometry {
                    viewport = viewport
                        .with_position(viewer_pos)
                        .with_inner_size(viewer_size)
                        .with_maximized(false);
                } else {
                    if let Some([x, y]) = launch.viewer_window_pos {
                        viewport = viewport.with_position(eframe::egui::pos2(x, y));
                    } else {
                        viewport = viewport.with_position(win_pos);
                    }
                    viewport = viewport.with_inner_size(win_size);
                    viewport = viewport.with_maximized(false);
                }
            }
        }
        StartupMode::ViewerLibrary if launch.start_fullscreen => {
            // Library から直接 Fullscreen に入るときだけ、起動直後の矩形を渡す。
            if let Some((source, [x, y, w, h])) = fullscreen_monitor_rect {
                tracing::debug!(
                    source,
                    selected_monitor_rect = ?[x, y, w, h],
                    "viewer startup fullscreen monitor selected"
                );
                if w > 1.0 && h > 1.0 {
                    viewport = viewport
                        .with_position(eframe::egui::pos2(x, y))
                        .with_inner_size(eframe::egui::vec2(w, h));
                }
            } else {
                tracing::debug!(
                    source = "default",
                    selected_monitor_rect = ?[win_pos.x, win_pos.y, win_size.x, win_size.y],
                    "viewer startup fullscreen monitor selected"
                );
                viewport = viewport.with_position(win_pos).with_inner_size(win_size);
            }
        }
        _ => {
            viewport = viewport.with_position(win_pos).with_inner_size(win_size);
        }
    }

    let options = eframe::NativeOptions {
        viewport,

        // セッション管理は自前で行う（eframe の自動保存は無効）
        persist_window: false,

        renderer: eframe::Renderer::Glow,

        ..Default::default()
    };

    eframe::run_native(
        crate::app_identity::PRODUCT_NAME,
        options,
        Box::new(move |cc| match launch.mode {
            StartupMode::Library => Ok(Box::new(app::App::new(cc, sess, launch.clone()))),
            StartupMode::ViewerStandalone | StartupMode::ViewerLibrary => Ok(Box::new(
                app::viewer_app::ViewerApp::new(cc, sess, launch.clone())?,
            )),
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
