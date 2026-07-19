use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::domain::ipc_contract::{LibraryToViewer, ViewerToLibrary};

/// JSON payload の最大サイズ。末尾の改行は含まない。
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_WIRE_BYTES: usize = MAX_MESSAGE_BYTES + 1;
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const PIPE_SERVER_DEFAULT_TIMEOUT_MS: u32 = 5_000;

pub fn make_pipe_name() -> String {
    let pid = std::process::id();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(r"\\.\pipe\{}-{pid:x}-{now:x}", crate::app_identity::APP_ID)
}

pub struct IpcServer {
    #[cfg(windows)]
    pipe_name: String,
    #[cfg(windows)]
    pipe_handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
// SAFETY:
// `IpcServer` は OS handle だけを持ち、並行アクセス用の内部参照は持たない。
// 実際の I/O は所有権を移した先で行い、Drop では `CloseHandle` を 1 回だけ呼ぶ。
unsafe impl Send for IpcServer {}

impl IpcServer {
    pub fn new(pipe_name: String) -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
            };
            use windows_sys::Win32::System::Pipes::{
                CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
                PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
            };

            let pipe_name_w = utf16z(&pipe_name);
            // SAFETY:
            // pipe 名は UTF-16 NUL 終端済みで、この呼び出し中生存する。
            // 失敗時は戻り値を検査し、無効 handle はそのまま捨てる。
            let handle = unsafe {
                CreateNamedPipeW(
                    pipe_name_w.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    PIPE_BUFFER_BYTES,
                    PIPE_BUFFER_BYTES,
                    PIPE_SERVER_DEFAULT_TIMEOUT_MS,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("CreateNamedPipeW failed: {pipe_name}"));
            }
            Ok(Self {
                pipe_name,
                pipe_handle: handle,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = pipe_name;
            anyhow::bail!("ipc server is only supported on windows");
        }
    }

    pub fn with_generated_name() -> anyhow::Result<Self> {
        Self::new(make_pipe_name())
    }

    pub fn pipe_name(&self) -> &str {
        #[cfg(windows)]
        {
            &self.pipe_name
        }
        #[cfg(not(windows))]
        {
            ""
        }
    }

    pub fn accept(self) -> anyhow::Result<IpcConnection> {
        #[cfg(windows)]
        {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED;
            use windows_sys::Win32::Foundation::GetLastError;
            use windows_sys::Win32::System::Pipes::ConnectNamedPipe;

            // SAFETY: `pipe_handle` は `CreateNamedPipeW` 成功で得た未接続 handle。
            let connected = unsafe { ConnectNamedPipe(self.pipe_handle, std::ptr::null_mut()) };
            if connected == 0 {
                // SAFETY: 直前の Win32 失敗を問い合わせるだけで追加の前提はない。
                let err = unsafe { GetLastError() };
                if err != ERROR_PIPE_CONNECTED {
                    return Err(std::io::Error::from_raw_os_error(err as i32))
                        .with_context(|| format!("ConnectNamedPipe failed: {}", self.pipe_name));
                }
            }

            // SAFETY:
            // `pipe_handle` の所有権を `File` へ移し、その後 `self` は `forget` して二重 close を防ぐ。
            let file = unsafe { std::fs::File::from_raw_handle(self.pipe_handle as *mut _) };
            std::mem::forget(self);
            IpcConnection::from_file(file)
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("ipc accept is only supported on windows");
        }
    }
}

#[cfg(windows)]
impl Drop for IpcServer {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: `pipe_handle` は有効 handle のときだけこの型が所有し、Drop は 1 回だけ走る。
            windows_sys::Win32::Foundation::CloseHandle(self.pipe_handle);
        }
    }
}

pub struct IpcClient;

impl IpcClient {
    pub fn connect(pipe_name: &str, timeout: Duration) -> anyhow::Result<IpcConnection> {
        #[cfg(windows)]
        {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::Storage::FileSystem::{
                CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
                OPEN_EXISTING,
            };
            use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

            let pipe_name_w = utf16z(pipe_name);
            // SAFETY: pipe 名は UTF-16 NUL 終端済みで、この呼び出し中生存する。
            let wait_ok =
                unsafe { WaitNamedPipeW(pipe_name_w.as_ptr(), timeout.as_millis() as u32) };
            if wait_ok == 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("WaitNamedPipeW failed: {pipe_name}"));
            }

            // SAFETY:
            // `pipe_name_w` は有効な UTF-16 文字列で、成功時 handle 所有権は `File` へ移す。
            let handle = unsafe {
                CreateFileW(
                    pipe_name_w.as_ptr(),
                    FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                    0,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("CreateFileW failed: {pipe_name}"));
            }

            // SAFETY: `handle` の所有権を `File` へ移し、この関数では以後 close しない。
            let file = unsafe { std::fs::File::from_raw_handle(handle as *mut _) };
            IpcConnection::from_file(file)
        }
        #[cfg(not(windows))]
        {
            let _ = (pipe_name, timeout);
            anyhow::bail!("ipc client is only supported on windows");
        }
    }
}

pub struct IpcConnection {
    reader: BufReader<std::fs::File>,
    writer: std::fs::File,
}

impl IpcConnection {
    fn from_file(file: std::fs::File) -> anyhow::Result<Self> {
        let writer = file
            .try_clone()
            .context("failed to clone pipe handle for writer")?;
        Ok(Self {
            reader: BufReader::new(file),
            writer,
        })
    }

    pub fn send_to_library(&mut self, msg: &ViewerToLibrary) -> anyhow::Result<()> {
        self.send_line(msg)
    }

    pub fn send_to_viewer(&mut self, msg: &LibraryToViewer) -> anyhow::Result<()> {
        self.send_line(msg)
    }

    pub fn recv_from_viewer(&mut self) -> anyhow::Result<ViewerToLibrary> {
        self.recv_line()
    }

    pub fn recv_from_library(&mut self) -> anyhow::Result<LibraryToViewer> {
        self.recv_line()
    }

    fn send_line<T: Serialize>(&mut self, msg: &T) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(msg).context("serialize ipc message")?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            anyhow::bail!("ipc message too large: {} bytes", bytes.len());
        }
        bytes.push(b'\n');
        self.writer
            .write_all(&bytes)
            .context("write ipc message to pipe")?;
        Ok(())
    }

    fn recv_line<T: for<'de> Deserialize<'de>>(&mut self) -> anyhow::Result<T> {
        let mut line = Vec::new();
        // `read_until` 単体では改行が来るまで無制限に確保するため、wire 上の改行を含めて
        // 1 MiB + 1 byte までに制限する。上限に達しても改行がなければ接続は不正として扱う。
        let read = self
            .reader
            .by_ref()
            .take(MAX_MESSAGE_WIRE_BYTES as u64)
            .read_until(b'\n', &mut line)
            .context("read ipc message from pipe")?;
        if read == 0 {
            anyhow::bail!("ipc disconnected");
        }
        let has_newline = matches!(line.last(), Some(b'\n'));
        if !has_newline && line.len() == MAX_MESSAGE_WIRE_BYTES {
            anyhow::bail!("ipc message too large: {} bytes", line.len());
        }
        if has_newline {
            let _ = line.pop();
        }
        if line.len() > MAX_MESSAGE_BYTES {
            anyhow::bail!("ipc message too large: {} bytes", line.len());
        }
        serde_json::from_slice::<T>(&line).context("decode ipc json")
    }
}

#[cfg(windows)]
fn utf16z(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
