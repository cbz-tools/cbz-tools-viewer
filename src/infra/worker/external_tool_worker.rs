use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use crate::domain::app_settings::normalize_external_tool_executable;
use serde::Deserialize;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
pub struct ExternalToolRunRequest {
    pub request_id: u64,
    pub tool_index: usize,
    pub tool_name: String,
    pub executable: String,
    pub args: String,
    pub background: bool,
    pub target_path: PathBuf,
    pub target_paths: Vec<PathBuf>,
    pub accepted_at: Instant,
}

#[derive(Clone, Debug)]
pub struct ExternalToolRunResult {
    pub request_id: u64,
    pub tool_index: usize,
    pub tool_name: String,
    pub background: bool,
    pub target_path: PathBuf,
    pub success: bool,
    pub message: Option<String>,
    pub elapsed_ms: u128,
}

pub struct ExternalToolWorker {
    req_tx: mpsc::Sender<ExternalToolWorkerReq>,
    resp_rx: std::sync::Mutex<mpsc::Receiver<ExternalToolRunResult>>,
}

enum ExternalToolWorkerReq {
    Run(ExternalToolRunRequest),
    Shutdown,
}

impl ExternalToolWorker {
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ExternalToolWorkerReq>();
        let (resp_tx, resp_rx) = mpsc::channel::<ExternalToolRunResult>();

        thread::Builder::new()
            .name("external-tool-worker".to_owned())
            .spawn(move || worker_main(req_rx, resp_tx))
            .map_err(|e| {
                tracing::error!("failed to spawn external-tool-worker thread: {e}");
                e
            })
            .ok();

        Self {
            req_tx,
            resp_rx: std::sync::Mutex::new(resp_rx),
        }
    }

    pub fn request(&self, req: ExternalToolRunRequest) -> bool {
        self.req_tx.send(ExternalToolWorkerReq::Run(req)).is_ok()
    }

    pub fn shutdown(&self) {
        let _ = self.req_tx.send(ExternalToolWorkerReq::Shutdown);
    }

    pub fn try_recv(&self) -> Option<ExternalToolRunResult> {
        match self.resp_rx.lock() {
            Ok(rx) => rx.try_recv().ok(),
            Err(_) => {
                tracing::error!("external tool resp_rx mutex poisoned");
                None
            }
        }
    }
}

impl Drop for ExternalToolWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_main(
    req_rx: mpsc::Receiver<ExternalToolWorkerReq>,
    resp_tx: mpsc::Sender<ExternalToolRunResult>,
) {
    while let Ok(req) = req_rx.recv() {
        match req {
            ExternalToolWorkerReq::Run(req) => {
                let result = run_one(req);
                let _ = resp_tx.send(result);
            }
            ExternalToolWorkerReq::Shutdown => break,
        }
    }
}

fn run_one(req: ExternalToolRunRequest) -> ExternalToolRunResult {
    let worker_started = Instant::now();
    let queue_delay_ms = worker_started
        .saturating_duration_since(req.accepted_at)
        .as_millis();
    log::info!(
        "[external-tool] worker start request_id={} tool={} path={} background={} queue_delay_ms={}",
        req.request_id,
        req.tool_name,
        req.target_path.display(),
        req.background,
        queue_delay_ms
    );

    let parsed_args = match build_args_from_template(&req.args, &req.target_paths) {
        Ok(v) => v,
        Err(err) => {
            return ExternalToolRunResult {
                request_id: req.request_id,
                tool_index: req.tool_index,
                tool_name: req.tool_name,
                background: req.background,
                target_path: req.target_path,
                success: false,
                message: Some(format!("args parse error: {err}")),
                elapsed_ms: Instant::now()
                    .saturating_duration_since(req.accepted_at)
                    .as_millis(),
            };
        }
    };

    let executable = normalize_external_tool_executable(&req.executable);
    log::info!(
        "[external-tool] command build tool={} executable={} args={:?} background={}",
        req.tool_name,
        executable,
        parsed_args,
        req.background
    );

    let mut cmd = Command::new(&executable);
    cmd.args(parsed_args);
    cmd.stdin(Stdio::null());

    if req.background {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
    } else {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    }

    if req.background {
        run_background(req, cmd)
    } else {
        run_foreground(req, cmd)
    }
}

fn run_foreground(req: ExternalToolRunRequest, mut cmd: Command) -> ExternalToolRunResult {
    match cmd.spawn() {
        Ok(_child) => {
            let elapsed = Instant::now()
                .saturating_duration_since(req.accepted_at)
                .as_millis();
            log::info!(
                "[external-tool] spawned foreground request_id={} tool={} path={} elapsed_ms={}",
                req.request_id,
                req.tool_name,
                req.target_path.display(),
                elapsed
            );
            ExternalToolRunResult {
                request_id: req.request_id,
                tool_index: req.tool_index,
                tool_name: req.tool_name,
                background: false,
                target_path: req.target_path,
                success: true,
                message: None,
                elapsed_ms: elapsed,
            }
        }
        Err(err) => {
            let elapsed = Instant::now()
                .saturating_duration_since(req.accepted_at)
                .as_millis();
            log::warn!(
                "[external-tool] failed request_id={} tool={} path={} background=false elapsed_ms={} err={}",
                req.request_id,
                req.tool_name,
                req.target_path.display(),
                elapsed,
                err
            );
            ExternalToolRunResult {
                request_id: req.request_id,
                tool_index: req.tool_index,
                tool_name: req.tool_name,
                background: false,
                target_path: req.target_path,
                success: false,
                message: Some(format!("spawn failed: {err}")),
                elapsed_ms: elapsed,
            }
        }
    }
}

fn run_background(req: ExternalToolRunRequest, mut cmd: Command) -> ExternalToolRunResult {
    let output = match cmd.output() {
        Ok(output) => output,
        Err(err) => {
            return ExternalToolRunResult {
                request_id: req.request_id,
                tool_index: req.tool_index,
                tool_name: req.tool_name,
                background: true,
                target_path: req.target_path,
                success: false,
                message: Some(format!("spawn failed: {err}")),
                elapsed_ms: Instant::now()
                    .saturating_duration_since(req.accepted_at)
                    .as_millis(),
            };
        }
    };

    let elapsed = Instant::now()
        .saturating_duration_since(req.accepted_at)
        .as_millis();
    log::info!(
        "[external-tool] child exited request_id={} tool={} path={} background=true exit_success={} elapsed_ms={}",
        req.request_id,
        req.tool_name,
        req.target_path.display(),
        output.status.success(),
        elapsed
    );

    let exit_ok = output.status.success();
    let json_judgement = parse_ok_json(&output.stdout);
    let (success, message) = match json_judgement {
        Ok(Some((ok, msg))) => (ok, msg),
        Ok(None) => {
            if exit_ok {
                (true, None)
            } else {
                let stderr_text = lossy_trimmed(&output.stderr);
                let msg = if stderr_text.is_empty() {
                    Some("process failed (non-zero exit)".to_owned())
                } else {
                    Some(format!("process failed (non-zero exit): {stderr_text}"))
                };
                (false, msg)
            }
        }
        Err(err) => {
            if exit_ok {
                (true, None)
            } else {
                let stderr_text = lossy_trimmed(&output.stderr);
                let msg = if stderr_text.is_empty() {
                    Some(format!("json parse error: {err}"))
                } else {
                    Some(format!("json parse error: {err}; stderr={stderr_text}"))
                };
                (false, msg)
            }
        }
    };

    ExternalToolRunResult {
        request_id: req.request_id,
        tool_index: req.tool_index,
        tool_name: req.tool_name,
        background: true,
        target_path: req.target_path,
        success,
        message,
        elapsed_ms: elapsed,
    }
}

#[derive(Deserialize)]
struct ToolJsonResult {
    ok: bool,
    message: Option<String>,
}

fn parse_ok_json(bytes: &[u8]) -> Result<Option<(bool, Option<String>)>, String> {
    let text = String::from_utf8_lossy(bytes).trim().to_owned();
    if text.is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<ToolJsonResult>(&text) {
        Ok(v) => Ok(Some((v.ok, v.message))),
        Err(e) => Err(e.to_string()),
    }
}

fn lossy_trimmed(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn build_args_from_template(args: &str, target_paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let first = target_paths
        .first()
        .ok_or_else(|| "no target paths".to_owned())?;
    let parsed = split_args_safely(args)?;
    let mut out = Vec::with_capacity(parsed.len() + target_paths.len());
    for token in parsed {
        if token == "{path}" || token == "{paths}" {
            for path in target_paths {
                out.push(path.to_string_lossy().into_owned());
            }
        } else {
            let replaced_path = token.replace("{path}", &first.to_string_lossy());
            let replaced = replaced_path.replace("{paths}", &first.to_string_lossy());
            out.push(replaced);
        }
    }
    Ok(out)
}

pub fn split_args_safely(input: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '\\' if in_double => {
                if let Some(next) = chars.peek().copied() {
                    if next == '"' || next == '\\' {
                        let _ = chars.next();
                        cur.push(next);
                    } else {
                        cur.push('\\');
                    }
                } else {
                    cur.push('\\');
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }

    if in_single || in_double {
        return Err("unterminated quote".to_owned());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}
