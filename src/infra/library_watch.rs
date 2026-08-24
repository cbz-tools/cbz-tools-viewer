//! Minimal filesystem change notification used to gate Library's periodic scan.
//!
//! Windows receives recursive directory notifications and exposes only one
//! coalesced dirty bit to the UI. Other platforms keep the existing periodic
//! scan behavior; they do not start a watcher thread.

use std::path::Path;

#[cfg(windows)]
use std::sync::{Arc, Mutex, atomic::AtomicBool};

use crate::repaint::RepaintNotifier;

pub(crate) struct LibraryChangeWatcher {
    #[cfg(windows)]
    state: Arc<WindowsWatcherState>,
}

#[cfg(windows)]
#[derive(Clone, Default)]
struct WatchTarget {
    path: Option<std::path::PathBuf>,
    generation: u64,
}

#[cfg(windows)]
struct WindowsWatcherState {
    target: Mutex<WatchTarget>,
    dirty: AtomicBool,
    stopped: std::sync::atomic::AtomicBool,
    repaint: RepaintNotifier,
}

impl LibraryChangeWatcher {
    pub(crate) fn new(repaint: RepaintNotifier) -> Self {
        #[cfg(windows)]
        {
            let state = Arc::new(WindowsWatcherState {
                target: Mutex::new(WatchTarget::default()),
                dirty: AtomicBool::new(false),
                stopped: std::sync::atomic::AtomicBool::new(false),
                repaint,
            });
            let worker_state = Arc::clone(&state);
            std::thread::Builder::new()
                .name("library-change-watch".to_owned())
                .spawn(move || windows_watch_loop(worker_state))
                .expect("failed to start Library change watcher");
            Self { state }
        }

        #[cfg(not(windows))]
        {
            let _ = repaint;
            Self {}
        }
    }

    pub(crate) fn set_path(&self, path: Option<&Path>) {
        #[cfg(windows)]
        {
            let next = path.map(Path::to_path_buf);
            let mut target = self.state.target.lock().unwrap_or_else(|e| e.into_inner());
            if target.path != next {
                target.path = next;
                target.generation = target.generation.saturating_add(1);
                // A path rebind starts a fresh notification window. Events
                // from the old path must not trigger a scan for the new path.
                self.state
                    .dirty
                    .store(false, std::sync::atomic::Ordering::Release);
            }
        }
        #[cfg(not(windows))]
        let _ = path;
    }

    /// Atomically consumes the coalesced notification state.
    pub(crate) fn take_dirty(&self) -> bool {
        #[cfg(windows)]
        {
            use std::sync::atomic::Ordering;
            self.state.dirty.swap(false, Ordering::AcqRel)
        }

        #[cfg(not(windows))]
        {
            // Keep the pre-existing unconditional periodic scan on non-Windows
            // targets; this feature is intentionally Windows-only.
            true
        }
    }

    /// Returns whether a Windows notification is waiting without consuming it.
    pub(crate) fn has_dirty(&self) -> bool {
        #[cfg(windows)]
        {
            use std::sync::atomic::Ordering;
            self.state.dirty.load(Ordering::Acquire)
        }

        #[cfg(not(windows))]
        false
    }

    #[cfg(windows)]
    fn stop(&self) {
        self.state
            .stopped
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for LibraryChangeWatcher {
    fn drop(&mut self) {
        #[cfg(windows)]
        self.stop();
    }
}

#[cfg(windows)]
fn target_snapshot(state: &WindowsWatcherState) -> WatchTarget {
    state
        .target
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(windows)]
fn target_is_current(state: &WindowsWatcherState, path: &Path, generation: u64) -> bool {
    let target = target_snapshot(state);
    target.generation == generation && target.path.as_deref() == Some(path)
}

#[cfg(windows)]
fn mark_dirty(state: &WindowsWatcherState, path: &Path, generation: u64) {
    // Serialize the target check with set_path's dirty reset. Without holding
    // this lock, an old-path completion could set dirty after a rebind cleared
    // it and spuriously trigger a scan for the new path.
    let target = state.target.lock().unwrap_or_else(|e| e.into_inner());
    if target.generation == generation && target.path.as_deref() == Some(path) {
        if !state.dirty.swap(true, std::sync::atomic::Ordering::AcqRel) {
            state.repaint.request_repaint();
        }
    }
}

#[cfg(windows)]
fn windows_watch_loop(state: Arc<WindowsWatcherState>) {
    use std::os::windows::ffi::OsStrExt;
    use std::{ffi::c_void, mem::MaybeUninit, ptr, time::Duration};
    use windows_sys::Win32::{
        Foundation::{
            ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_TIMEOUT, GetLastError,
            INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
            FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
            FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
        },
        System::IO::{CancelIoEx, GetOverlappedResultEx, OVERLAPPED},
    };

    const BUFFER_SIZE: u32 = 64 * 1024;
    const WAIT_MS: u32 = 250;
    const RETRY_AFTER_FAILURE: Duration = Duration::from_millis(500);
    let mut watched: Option<WatchTarget> = None;
    let mut failed_generation = None;

    while !state.stopped.load(std::sync::atomic::Ordering::Acquire) {
        let target = target_snapshot(&state);
        if watched
            .as_ref()
            .is_none_or(|current| current.generation != target.generation)
        {
            watched = Some(target.clone());
            failed_generation = None;
        }

        let Some(current) = watched.as_ref() else {
            std::thread::sleep(RETRY_AFTER_FAILURE);
            continue;
        };
        let Some(path) = current.path.as_ref() else {
            std::thread::sleep(RETRY_AFTER_FAILURE);
            continue;
        };
        if failed_generation == Some(current.generation) {
            // A failed watcher is stopped for this path. It will be retried
            // only after Library binds a different current_dir.
            std::thread::sleep(RETRY_AFTER_FAILURE);
            continue;
        }

        let path_wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: path_wide is NUL-terminated and remains alive for this call;
        // the returned handle is owned by this loop until it is closed below.
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            mark_dirty(&state, path, current.generation);
            failed_generation = Some(current.generation);
            log::warn!(
                "[library-watch] unable to watch current Library path={}",
                path.display()
            );
            continue;
        }

        let mut buffer = vec![0u8; BUFFER_SIZE as usize];
        let mut failed = false;
        let mut rebind = false;
        loop {
            if state.stopped.load(std::sync::atomic::Ordering::Acquire)
                || !target_is_current(&state, path, current.generation)
            {
                rebind = true;
                break;
            }

            let overlapped = MaybeUninit::<OVERLAPPED>::zeroed();
            // SAFETY: zeroed OVERLAPPED is the documented initial state.
            let mut overlapped = unsafe { overlapped.assume_init() };
            let started = unsafe {
                ReadDirectoryChangesW(
                    handle,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    BUFFER_SIZE,
                    1,
                    FILE_NOTIFY_CHANGE_FILE_NAME
                        | FILE_NOTIFY_CHANGE_DIR_NAME
                        | FILE_NOTIFY_CHANGE_LAST_WRITE
                        | FILE_NOTIFY_CHANGE_SIZE,
                    ptr::null_mut(),
                    &mut overlapped,
                    None,
                )
            };
            if started == 0 {
                let error = unsafe { GetLastError() };
                if error != ERROR_IO_PENDING {
                    mark_dirty(&state, path, current.generation);
                    failed = true;
                    break;
                }
            }

            let mut cancel_pending = false;
            loop {
                let mut bytes = 0u32;
                // SAFETY: handle and overlapped remain valid for this operation;
                // the bounded wait observes a current_dir rebind or shutdown.
                let completed =
                    unsafe { GetOverlappedResultEx(handle, &overlapped, &mut bytes, WAIT_MS, 0) };
                if completed != 0 {
                    // Event details and byte counts are intentionally ignored.
                    // A zero-byte completion (buffer overflow) is also just dirty.
                    let _ = bytes;
                    mark_dirty(&state, path, current.generation);
                    break;
                }

                let error = unsafe { GetLastError() };
                if error == WAIT_TIMEOUT || error == ERROR_TIMEOUT || error == ERROR_IO_INCOMPLETE {
                    if state.stopped.load(std::sync::atomic::Ordering::Acquire)
                        || !target_is_current(&state, path, current.generation)
                    {
                        cancel_pending = true;
                        break;
                    }
                    continue;
                }

                // Completion errors, including directory notification buffer
                // overflow, conservatively trigger one later diff-scan.
                failed = true;
                mark_dirty(&state, path, current.generation);
                break;
            }

            if cancel_pending {
                // SAFETY: cancel only the request owned by this loop, then wait
                // until the OVERLAPPED operation has completed before dropping
                // its stack storage or closing the directory handle.
                unsafe {
                    let _ = CancelIoEx(handle, &overlapped);
                    let mut bytes = 0u32;
                    let _ = GetOverlappedResultEx(handle, &overlapped, &mut bytes, u32::MAX, 0);
                }
                rebind = true;
                break;
            }
            if failed {
                break;
            }
        }

        // SAFETY: handle was returned by CreateFileW and is closed once here.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        if failed {
            failed_generation = Some(current.generation);
            log::warn!(
                "[library-watch] watcher stopped for current Library path={}",
                path.display()
            );
        }
        if rebind {
            continue;
        }
    }
}
