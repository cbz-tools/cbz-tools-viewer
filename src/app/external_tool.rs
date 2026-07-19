use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key};

use crate::domain::app_settings::{
    AppSettings, ExternalToolShortcut, normalize_external_tool_executable,
};
use crate::infra::worker::external_tool_worker::{ExternalToolRunRequest, ExternalToolRunResult};
use crate::ui::viewer::{ExternalToolButtonModel, ExternalToolTrigger};

use super::App;

pub(super) struct ExternalToolRunning {
    request_id: u64,
    tool_index: usize,
    tool_name: String,
    path: PathBuf,
    background: bool,
}

pub(super) enum ExternalToolUiState {
    Idle,
    Running,
    Success { until: Instant },
    Failed,
}

impl App {
    pub(super) fn is_external_tool_busy(&self) -> bool {
        self.external_tool_running.is_some()
    }

    pub(super) fn request_external_tool_run_paths(
        &mut self,
        tool_index: usize,
        target_paths: Vec<PathBuf>,
    ) -> bool {
        let Some(primary_path) = first_target_path(&target_paths) else {
            log::warn!(
                "[external-tool] request rejected empty-target-paths tool_index={tool_index}"
            );
            return false;
        };
        let Some(tool) = self.external_tool_config_for_request(tool_index, &primary_path) else {
            return false;
        };

        if let Some(running) = &self.external_tool_running {
            log::warn!(
                "[external-tool] request rejected busy running_tool={} running_path={} new_tool={} new_path={} background_running={} background_new={}",
                running.tool_name,
                running.path.display(),
                tool.name,
                primary_path.display(),
                running.background,
                tool.background
            );
            return false;
        };

        let request_id = self.external_tool_next_request_id;
        self.external_tool_next_request_id = self.external_tool_next_request_id.saturating_add(1);
        let accepted_at = Instant::now();
        let normalized_executable = normalize_external_tool_executable(&tool.executable);
        let req = ExternalToolRunRequest {
            request_id,
            tool_index,
            tool_name: tool.name.clone(),
            executable: normalized_executable,
            args: tool.args,
            background: tool.background,
            target_path: primary_path.clone(),
            target_paths: target_paths.clone(),
            accepted_at,
        };

        if !self.external_tool_worker.request(req) {
            log::warn!(
                "[external-tool] request rejected worker-down tool={} path={} background={}",
                tool.name,
                primary_path.display(),
                tool.background
            );
            return false;
        }

        self.external_tool_running = Some(ExternalToolRunning {
            request_id,
            tool_index,
            tool_name: tool.name.clone(),
            path: primary_path.clone(),
            background: tool.background,
        });
        self.external_tool_ui_state = ExternalToolUiState::Running;
        log::info!(
            "[external-tool] request accepted request_id={} tool_index={} tool={} path={} background={} elapsed_origin=accepted_at",
            request_id,
            tool_index,
            tool.name,
            primary_path.display(),
            tool.background
        );
        true
    }

    pub(super) fn request_external_tool_run_from_trigger(
        &mut self,
        tool_index: usize,
        target_path: PathBuf,
        trigger: ExternalToolTrigger,
    ) -> bool {
        self.request_external_tool_run_paths_from_trigger(tool_index, vec![target_path], trigger)
    }

    pub(super) fn request_external_tool_run_paths_from_trigger(
        &mut self,
        tool_index: usize,
        target_paths: Vec<PathBuf>,
        trigger: ExternalToolTrigger,
    ) -> bool {
        let Some(primary_path) = first_target_path(&target_paths) else {
            return false;
        };
        let trigger_label = trigger_source_label(&trigger);
        let (tool_name, background) = self.external_tool_trigger_meta(tool_index);
        log::info!(
            "[external-tool] trigger source={} tool={} tool_index={} path={} background={}",
            trigger_label,
            tool_name,
            tool_index,
            primary_path.display(),
            background
        );
        if primary_path.as_os_str().is_empty() {
            if let ExternalToolTrigger::Shortcut { key } = trigger {
                log::warn!(
                    "[external-tool] shortcut ignored no current book path key={}",
                    key
                );
            }
            return false;
        }
        if self.is_external_tool_busy() {
            if let ExternalToolTrigger::Shortcut { key } = trigger {
                log::warn!(
                    "[external-tool] shortcut ignored busy key={} tool={}",
                    key,
                    tool_name
                );
            }
            return false;
        }
        self.request_external_tool_run_paths(tool_index, target_paths)
    }

    fn external_tool_config_for_request(
        &self,
        tool_index: usize,
        primary_path: &std::path::Path,
    ) -> Option<crate::domain::app_settings::ExternalTool> {
        let Some(tool) = self.app_settings.external_tools.get(tool_index).cloned() else {
            log::warn!(
                "[external-tool] request rejected invalid-tool-index tool_index={} path={}",
                tool_index,
                primary_path.display()
            );
            return None;
        };
        Some(tool)
    }

    fn external_tool_trigger_meta(&self, tool_index: usize) -> (&str, bool) {
        let tool_name = self
            .app_settings
            .external_tools
            .get(tool_index)
            .map(|t| t.name.as_str())
            .unwrap_or("<unknown>");
        let background = self
            .app_settings
            .external_tools
            .get(tool_index)
            .map(|t| t.background)
            .unwrap_or(false);
        (tool_name, background)
    }

    pub(super) fn poll_external_tool_results(&mut self, ctx: &egui::Context) {
        let mut received_any = false;
        while let Some(result) = self.external_tool_worker.try_recv() {
            received_any = true;
            log::info!(
                "[external-tool] result received request_id={} tool={} path={} success={} background={} elapsed_ms={}",
                result.request_id,
                result.tool_name,
                result.target_path.display(),
                result.success,
                result.background,
                result.elapsed_ms
            );
            self.handle_external_tool_result(result);
        }
        if received_any {
            ctx.request_repaint();
        }
    }

    pub(super) fn handle_external_tool_result(&mut self, result: ExternalToolRunResult) {
        let Some(running) = &self.external_tool_running else {
            log::warn!(
                "[external-tool] stale result ignored request_id={} tool={} path={} reason=no-running",
                result.request_id,
                result.tool_name,
                result.target_path.display()
            );
            return;
        };

        if running.request_id != result.request_id
            || running.path != result.target_path
            || running.tool_index != result.tool_index
        {
            log::warn!(
                "[external-tool] stale result ignored request_id={} tool={} path={} tool_index={} running_request_id={} running_path={} running_tool_index={}",
                result.request_id,
                result.tool_name,
                result.target_path.display(),
                result.tool_index,
                running.request_id,
                running.path.display(),
                running.tool_index
            );
            return;
        }

        self.external_tool_running = None;
        let path_is_current = self.is_current_library_selection(result.target_path.as_path());
        if result.success {
            self.apply_external_tool_success(&result, path_is_current);
        } else {
            self.apply_external_tool_failure(&result, path_is_current);
        }
    }

    fn apply_external_tool_success(
        &mut self,
        result: &ExternalToolRunResult,
        path_is_current: bool,
    ) {
        if result.background {
            log::info!(
                "[external-tool] success request_id={} tool={} path={} background=true elapsed_ms={}",
                result.request_id,
                result.tool_name,
                result.target_path.display(),
                result.elapsed_ms
            );
        } else {
            log::info!(
                "[external-tool] spawned foreground request_id={} tool={} path={} elapsed_ms={}",
                result.request_id,
                result.tool_name,
                result.target_path.display(),
                result.elapsed_ms
            );
        }
        if path_is_current {
            self.external_tool_ui_state = ExternalToolUiState::Success {
                until: Instant::now() + Duration::from_secs(3),
            };
        } else {
            self.external_tool_ui_state = ExternalToolUiState::Idle;
            log::warn!(
                "[external-tool] result stale path mismatch result_path={} current_path=<none>",
                result.target_path.display()
            );
        }
    }

    fn apply_external_tool_failure(
        &mut self,
        result: &ExternalToolRunResult,
        path_is_current: bool,
    ) {
        log::warn!(
            "[external-tool] failed request_id={} tool={} path={} background={} elapsed_ms={} err={}",
            result.request_id,
            result.tool_name,
            result.target_path.display(),
            result.background,
            result.elapsed_ms,
            result.message.as_deref().unwrap_or("unknown")
        );
        if path_is_current {
            self.external_tool_ui_state = ExternalToolUiState::Failed;
            log::warn!(
                "[external-tool] ui state failed request_id={} tool_index={} path={}",
                result.request_id,
                result.tool_index,
                result.target_path.display()
            );
        } else {
            self.external_tool_ui_state = ExternalToolUiState::Idle;
            log::warn!(
                "[external-tool] result stale path mismatch result_path={} current_path=<none>",
                result.target_path.display()
            );
        }
    }

    pub(super) fn tick_external_tool_ui_state(&mut self) {
        if let ExternalToolUiState::Success { until, .. } = self.external_tool_ui_state {
            if Instant::now() >= until {
                self.external_tool_ui_state = ExternalToolUiState::Idle;
            }
        }
    }

    pub(super) fn schedule_external_tool_state_repaint(&self, ctx: &egui::Context) {
        if self.is_external_tool_busy() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if let ExternalToolUiState::Success { until, .. } = self.external_tool_ui_state {
            let now = Instant::now();
            if now < until {
                ctx.request_repaint_after(until.saturating_duration_since(now));
            }
        }
    }

    pub(super) fn external_tool_button_models(&self) -> Vec<ExternalToolButtonModel> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (idx, tool) in self.app_settings.external_tools.iter().enumerate() {
            if !is_external_tool_enabled(tool) {
                continue;
            }
            let key = external_tool_shortcut_to_egui_key(tool.shortcut);
            if !is_allowed_external_tool_shortcut(tool.shortcut) {
                log::warn!(
                    "[external-tool] shortcut ignored invalid key={} tool={} tool_index={}",
                    tool.shortcut.as_char(),
                    tool.name,
                    idx
                );
                continue;
            }
            if !seen.insert(key) {
                log::warn!(
                    "[external-tool] duplicate shortcut ignored key={} tool={} tool_index={}",
                    tool.shortcut.as_char(),
                    tool.name,
                    idx
                );
                continue;
            }
            out.push(ExternalToolButtonModel {
                tool_index: idx,
                name: tool.name.clone(),
                shortcut: tool.shortcut.as_char(),
                key,
            });
        }
        out
    }

    pub(super) fn external_tool_menu_items_for_library(
        &self,
    ) -> Vec<crate::ui::virtual_grid::ExternalToolMenuItem> {
        self.external_tool_button_models()
            .into_iter()
            .map(|tool| crate::ui::virtual_grid::ExternalToolMenuItem {
                tool_index: tool.tool_index,
                name: tool.name,
                shortcut: tool.shortcut,
            })
            .collect()
    }

    pub(super) fn trigger_external_tool_from_library(
        &mut self,
        tool_index: usize,
        targets: &[usize],
    ) {
        if self.is_external_tool_busy() {
            log::warn!(
                "[external-tool] library ignored busy tool_index={} targets={}",
                tool_index,
                targets.len()
            );
            return;
        }
        let book_paths: Vec<PathBuf> = targets
            .iter()
            .filter_map(|&idx| self.book_entry_at(idx))
            .map(|entry| entry.path.as_ref().to_path_buf())
            .collect();
        if book_paths.is_empty() {
            return;
        }

        let trigger = ExternalToolTrigger::Toolbar;
        let _ = self.request_external_tool_run_paths_from_trigger(tool_index, book_paths, trigger);
    }

    fn is_current_library_selection(&self, path: &std::path::Path) -> bool {
        self.selected_entry_path()
            .is_some_and(|selected| selected.as_path() == path)
    }

    pub(super) fn drain_pending_external_tool_runs(&mut self, ctx: &egui::Context) {
        let mut queued = Vec::new();
        {
            let mut guard = self.pending_external_tool_runs.lock();
            if guard.is_empty() {
                return;
            }
            queued.extend(guard.drain(..));
        }
        for (tool_index, target_path, trigger) in queued {
            let _ = self.request_external_tool_run_from_trigger(tool_index, target_path, trigger);
            ctx.request_repaint();
        }
        if !self.pending_external_tool_runs.lock().is_empty() {
            ctx.request_repaint();
        }
    }
}

pub(super) fn external_tool_shortcut_to_egui_key(shortcut: ExternalToolShortcut) -> Key {
    match shortcut {
        ExternalToolShortcut::E => Key::E,
        ExternalToolShortcut::F => Key::F,
        ExternalToolShortcut::G => Key::G,
        ExternalToolShortcut::H => Key::H,
        ExternalToolShortcut::I => Key::I,
        ExternalToolShortcut::J => Key::J,
        ExternalToolShortcut::K => Key::K,
        ExternalToolShortcut::L => Key::L,
        ExternalToolShortcut::N => Key::N,
        ExternalToolShortcut::O => Key::O,
        ExternalToolShortcut::P => Key::P,
        ExternalToolShortcut::Q => Key::Q,
        ExternalToolShortcut::R => Key::R,
        ExternalToolShortcut::T => Key::T,
        ExternalToolShortcut::U => Key::U,
        ExternalToolShortcut::V => Key::V,
        ExternalToolShortcut::X => Key::X,
        ExternalToolShortcut::Y => Key::Y,
        ExternalToolShortcut::Z => Key::Z,
    }
}

fn first_target_path(target_paths: &[PathBuf]) -> Option<PathBuf> {
    target_paths.first().cloned()
}

fn trigger_source_label(trigger: &ExternalToolTrigger) -> &'static str {
    match trigger {
        ExternalToolTrigger::Toolbar => "toolbar",
        ExternalToolTrigger::Shortcut { .. } => "shortcut",
    }
}

fn is_allowed_external_tool_shortcut(shortcut: ExternalToolShortcut) -> bool {
    AppSettings::external_tool_shortcut_candidates().contains(&shortcut)
}

fn is_external_tool_enabled(tool: &crate::domain::app_settings::ExternalTool) -> bool {
    !tool.name.trim().is_empty()
        && !tool.executable.trim().is_empty()
        && is_allowed_external_tool_shortcut(tool.shortcut)
}
