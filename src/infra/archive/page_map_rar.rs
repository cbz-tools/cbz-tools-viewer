use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crate::domain::page_map::{BookPageMap, PageDescriptor, PageFormat, SourceRevision};
use crate::infra::image::page_map::{
    read_image_metadata, read_image_metadata_lightweight_first, LightweightImageMetadataOutcome,
};
use crate::util::natural_sort;

use super::page_map_format_for_name;

type Result<T, E> = std::result::Result<T, E>;

const RAR_DEFERRED_LARGE_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const RAR_DEFERRED_PARALLEL_WORKERS_MAX: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RarPageMapSlowFailureReason {
    OpenArchive,
    ReadHeader,
    ProcessFile,
    Metadata,
    UnsupportedFormat,
    NoImageEntries,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RarPageMapSlowFailure {
    pub page_index: Option<u32>,
    pub entry_index: Option<u32>,
    pub reason: RarPageMapSlowFailureReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RarPageMapSlowOutcome {
    Success(BookPageMap),
    Failure(RarPageMapSlowFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RarTriState {
    Yes,
    No,
}

#[derive(Clone, Debug)]
struct RarArchivePlan {
    archive_path: PathBuf,
    archive_size: u64,
    total_raw_entries: usize,
    solid: RarTriState,
    encrypted: RarTriState,
    multi_volume: RarTriState,
    image_entries: Vec<RarImageEntry>,
}

#[derive(Clone, Debug)]
struct RarImageEntry {
    raw_entry_index: u32,
    name: String,
    format_hint: Option<PageFormat>,
}

#[derive(Clone, Debug)]
struct RarIngestedEntry {
    raw_entry_index: u32,
    name: String,
    format: PageFormat,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct RarProcessCallbackState {
    current_bytes: Vec<u8>,
}

struct RarArchiveHandle {
    handle: *const unrar_sys::Handle,
    flags: u32,
}

impl Drop for RarArchiveHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let _ = unsafe { unrar_sys::RARCloseArchive(self.handle) };
            self.handle = std::ptr::null();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RarAdaptiveStrategy {
    Sequential,
    Parallel { workers: usize },
}

pub(crate) fn build_book_page_map_slow_from_rar_path(
    path: &Path,
    revision: SourceRevision,
) -> RarPageMapSlowOutcome {
    match build_rar_page_map_adaptive(path, revision) {
        Ok(page_map) => RarPageMapSlowOutcome::Success(page_map),
        Err(failure) => RarPageMapSlowOutcome::Failure(failure),
    }
}

fn build_rar_page_map_adaptive(
    path: &Path,
    revision: SourceRevision,
) -> Result<BookPageMap, RarPageMapSlowFailure> {
    let plan = build_rar_archive_plan(path)?;
    if plan.image_entries.is_empty() {
        return Err(RarPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: RarPageMapSlowFailureReason::NoImageEntries,
        });
    }

    let strategy = choose_rar_strategy(&plan);
    tracing::debug!(
        path = %path.display(),
        archive_size = plan.archive_size,
        total_raw_entries = plan.total_raw_entries,
        image_entries = plan.image_entries.len(),
        solid = matches!(plan.solid, RarTriState::Yes),
        encrypted = matches!(plan.encrypted, RarTriState::Yes),
        multi_volume = matches!(plan.multi_volume, RarTriState::Yes),
        strategy = ?strategy,
        "rar page-map plan"
    );

    match strategy {
        RarAdaptiveStrategy::Sequential => build_rar_page_map_sequential(&plan, revision),
        RarAdaptiveStrategy::Parallel { workers } => {
            match build_rar_page_map_parallel(&plan, revision.clone(), workers) {
                Ok(page_map) => Ok(page_map),
                Err(parallel_failure) => {
                    tracing::debug!(
                        path = %path.display(),
                        reason = ?parallel_failure.reason,
                        "rar page-map parallel ingest failed, falling back to sequential"
                    );
                    build_rar_page_map_sequential(&plan, revision)
                }
            }
        }
    }
}

fn build_rar_archive_plan(path: &Path) -> Result<RarArchivePlan, RarPageMapSlowFailure> {
    let archive = open_rar_archive(path)?;
    let archive_size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let mut total_raw_entries = 0usize;
    let mut image_entries = Vec::new();
    let mut solid = if archive.flags & unrar_sys::ROADF_SOLID != 0 {
        RarTriState::Yes
    } else {
        RarTriState::No
    };
    let mut encrypted = if archive.flags & unrar_sys::ROADF_ENCHEADERS != 0 {
        RarTriState::Yes
    } else {
        RarTriState::No
    };
    let mut multi_volume = if archive.flags & unrar_sys::ROADF_VOLUME != 0 {
        RarTriState::Yes
    } else {
        RarTriState::No
    };

    loop {
        let mut header = unrar_sys::HeaderDataEx::default();
        let read_started = Instant::now();
        let read_ret = unsafe { unrar_sys::RARReadHeaderEx(archive.handle, &mut header as *mut _) };
        let _ = read_started.elapsed();
        match read_ret {
            unrar_sys::ERAR_SUCCESS => {
                let raw_entry_index = total_raw_entries as u32;
                total_raw_entries += 1;
                let name = header_filename(&header);
                let flags = header.flags;
                if flags & unrar_sys::RHDF_SOLID != 0 {
                    solid = RarTriState::Yes;
                }
                if flags & unrar_sys::RHDF_ENCRYPTED != 0 {
                    encrypted = RarTriState::Yes;
                }
                if flags & (unrar_sys::RHDF_SPLITBEFORE | unrar_sys::RHDF_SPLITAFTER) != 0 {
                    multi_volume = RarTriState::Yes;
                }

                if flags & unrar_sys::RHDF_DIRECTORY != 0 {
                    rar_skip_current_file(archive.handle)?;
                    continue;
                }

                let Some(format_hint) = page_map_format_for_name(&name) else {
                    rar_skip_current_file(archive.handle)?;
                    continue;
                };

                image_entries.push(RarImageEntry {
                    raw_entry_index,
                    name,
                    format_hint: Some(format_hint),
                });
                rar_skip_current_file(archive.handle)?;
            }
            unrar_sys::ERAR_END_ARCHIVE => break,
            _ => {
                return Err(RarPageMapSlowFailure {
                    page_index: image_entries.len().try_into().ok(),
                    entry_index: None,
                    reason: RarPageMapSlowFailureReason::ReadHeader,
                });
            }
        }
    }

    Ok(RarArchivePlan {
        archive_path: path.to_path_buf(),
        archive_size,
        total_raw_entries,
        solid,
        encrypted,
        multi_volume,
        image_entries,
    })
}

fn choose_rar_strategy(plan: &RarArchivePlan) -> RarAdaptiveStrategy {
    let eligible = plan.archive_size >= RAR_DEFERRED_LARGE_ARCHIVE_BYTES
        && plan.image_entries.len() >= 256
        && matches!(plan.solid, RarTriState::No)
        && matches!(plan.encrypted, RarTriState::No)
        && matches!(plan.multi_volume, RarTriState::No);
    if !eligible {
        return RarAdaptiveStrategy::Sequential;
    }

    let available = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, RAR_DEFERRED_PARALLEL_WORKERS_MAX);
    let workers = available.min(plan.image_entries.len()).max(1);
    if workers >= 2 {
        RarAdaptiveStrategy::Parallel { workers }
    } else {
        RarAdaptiveStrategy::Sequential
    }
}

fn build_rar_page_map_sequential(
    plan: &RarArchivePlan,
    revision: SourceRevision,
) -> Result<BookPageMap, RarPageMapSlowFailure> {
    let worker = run_rar_worker(plan, 0, plan.image_entries.len())?;
    assemble_rar_page_map(revision, worker.entries)
}

fn build_rar_page_map_parallel(
    plan: &RarArchivePlan,
    revision: SourceRevision,
    workers: usize,
) -> Result<BookPageMap, RarPageMapSlowFailure> {
    let plan = Arc::new(plan.clone());
    let ranges = split_ranges(plan.image_entries.len(), workers);
    let mut handles = Vec::with_capacity(ranges.len());
    for (range_start, range_end) in ranges.into_iter() {
        let plan = Arc::clone(&plan);
        handles.push(thread::spawn(move || {
            run_rar_worker(plan.as_ref(), range_start, range_end)
        }));
    }

    let mut entries = Vec::new();
    let mut first_failure = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(worker)) => entries.extend(worker.entries),
            Ok(Err(failure)) => {
                if first_failure.is_none() {
                    first_failure = Some(failure);
                }
            }
            Err(_) => {
                if first_failure.is_none() {
                    first_failure = Some(RarPageMapSlowFailure {
                        page_index: None,
                        entry_index: None,
                        reason: RarPageMapSlowFailureReason::ProcessFile,
                    });
                }
            }
        }
    }

    if let Some(failure) = first_failure {
        return Err(failure);
    }

    assemble_rar_page_map(revision, entries)
}

struct RarWorkerReport {
    entries: Vec<RarIngestedEntry>,
}

fn run_rar_worker(
    plan: &RarArchivePlan,
    range_start: usize,
    range_end: usize,
) -> Result<RarWorkerReport, RarPageMapSlowFailure> {
    if range_start >= range_end {
        return Ok(RarWorkerReport {
            entries: Vec::new(),
        });
    }

    let archive = open_rar_archive(&plan.archive_path)?;
    let mut callback_state = RarProcessCallbackState::default();
    let callback_user_data = (&mut callback_state as *mut RarProcessCallbackState) as isize;
    unsafe {
        unrar_sys::RARSetCallback(
            archive.handle,
            Some(rar_process_data_callback),
            callback_user_data,
        );
    }

    let mut raw_entry_index = 0usize;
    let mut image_ordinal = 0usize;
    let mut entries = Vec::with_capacity(range_end - range_start);

    loop {
        let mut header = unrar_sys::HeaderDataEx::default();
        let read_ret = unsafe { unrar_sys::RARReadHeaderEx(archive.handle, &mut header as *mut _) };
        match read_ret {
            unrar_sys::ERAR_SUCCESS => {
                let current_raw_entry_index = raw_entry_index;
                raw_entry_index += 1;
                let name = header_filename(&header);
                let is_directory = header.flags & unrar_sys::RHDF_DIRECTORY != 0;
                if page_map_format_for_name(&name).is_none() {
                    rar_skip_current_file(archive.handle)?;
                    continue;
                }
                if is_directory {
                    rar_skip_current_file(archive.handle)?;
                    continue;
                }

                let current_image_ordinal = image_ordinal;
                image_ordinal += 1;
                if current_image_ordinal < range_start {
                    rar_skip_current_file(archive.handle)?;
                    continue;
                }
                if current_image_ordinal >= range_end {
                    rar_skip_current_file(archive.handle)?;
                    break;
                }

                let Some(planned) = plan.image_entries.get(current_image_ordinal) else {
                    return Err(RarPageMapSlowFailure {
                        page_index: None,
                        entry_index: Some(
                            current_raw_entry_index.try_into().ok().unwrap_or(u32::MAX),
                        ),
                        reason: RarPageMapSlowFailureReason::ReadHeader,
                    });
                };
                debug_assert_eq!(
                    planned.raw_entry_index,
                    current_raw_entry_index.try_into().ok().unwrap_or(u32::MAX)
                );
                debug_assert_eq!(planned.name, name);
                let unpacked_size = unpack_size(header.unp_size, header.unp_size_high);
                let bytes = extract_rar_image_bytes(
                    archive.handle,
                    &mut callback_state,
                    unpacked_size,
                    current_image_ordinal,
                    current_raw_entry_index,
                )?;

                let metadata_started = Instant::now();
                let (format, width, height) =
                    match read_image_metadata_lightweight_first(&bytes, planned.format_hint) {
                        LightweightImageMetadataOutcome::Ready {
                            format,
                            width,
                            height,
                        } => (format, width, height),
                        LightweightImageMetadataOutcome::FallbackRequired
                        | LightweightImageMetadataOutcome::Unsupported => {
                            match read_image_metadata(&bytes) {
                                Ok(Some((format, width, height))) => (format, width, height),
                                Ok(None) => {
                                    return Err(RarPageMapSlowFailure {
                                        page_index: None,
                                        entry_index: Some(
                                            current_raw_entry_index
                                                .try_into()
                                                .ok()
                                                .unwrap_or(u32::MAX),
                                        ),
                                        reason: RarPageMapSlowFailureReason::UnsupportedFormat,
                                    });
                                }
                                Err(_) => {
                                    return Err(RarPageMapSlowFailure {
                                        page_index: None,
                                        entry_index: Some(
                                            current_raw_entry_index
                                                .try_into()
                                                .ok()
                                                .unwrap_or(u32::MAX),
                                        ),
                                        reason: RarPageMapSlowFailureReason::Metadata,
                                    });
                                }
                            }
                        }
                    };
                let _ = metadata_started.elapsed();
                debug_assert!(width > 0 && height > 0);
                entries.push(RarIngestedEntry {
                    raw_entry_index: current_raw_entry_index.try_into().ok().unwrap_or(u32::MAX),
                    name,
                    format,
                    width,
                    height,
                });
            }
            unrar_sys::ERAR_END_ARCHIVE => break,
            _ => {
                return Err(RarPageMapSlowFailure {
                    page_index: entries.len().try_into().ok(),
                    entry_index: None,
                    reason: RarPageMapSlowFailureReason::ReadHeader,
                });
            }
        }
    }

    Ok(RarWorkerReport { entries })
}

fn assemble_rar_page_map(
    revision: SourceRevision,
    mut entries: Vec<RarIngestedEntry>,
) -> Result<BookPageMap, RarPageMapSlowFailure> {
    if entries.is_empty() {
        return Err(RarPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: RarPageMapSlowFailureReason::NoImageEntries,
        });
    }

    entries.sort_by(|a, b| {
        natural_sort::compare(&a.name, &b.name)
            .then_with(|| a.raw_entry_index.cmp(&b.raw_entry_index))
    });
    let mut pages = Vec::with_capacity(entries.len());
    for entry in entries {
        debug_assert!(entry.width > 0 && entry.height > 0);
        pages.push(PageDescriptor {
            format: entry.format,
            width: entry.width,
            height: entry.height,
        });
    }

    Ok(BookPageMap::new(revision, pages))
}

fn extract_rar_image_bytes(
    handle: *const unrar_sys::Handle,
    callback_state: &mut RarProcessCallbackState,
    unpacked_size: u64,
    page_index: usize,
    entry_index: usize,
) -> Result<Vec<u8>, RarPageMapSlowFailure> {
    callback_state.current_bytes.clear();
    if let Ok(capacity) = usize::try_from(unpacked_size) {
        callback_state.current_bytes.reserve(capacity);
    }
    let user_data = (callback_state as *mut RarProcessCallbackState) as isize;
    unsafe {
        unrar_sys::RARSetCallback(handle, Some(rar_process_data_callback), user_data);
    }
    let process_ret = unsafe {
        unrar_sys::RARProcessFileW(
            handle,
            unrar_sys::RAR_TEST,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if process_ret != unrar_sys::ERAR_SUCCESS {
        return Err(RarPageMapSlowFailure {
            page_index: Some(page_index.try_into().ok().unwrap_or(u32::MAX)),
            entry_index: Some(entry_index.try_into().ok().unwrap_or(u32::MAX)),
            reason: RarPageMapSlowFailureReason::ProcessFile,
        });
    }
    Ok(std::mem::take(&mut callback_state.current_bytes))
}

fn rar_skip_current_file(handle: *const unrar_sys::Handle) -> Result<(), RarPageMapSlowFailure> {
    let ret = unsafe {
        unrar_sys::RARProcessFileW(
            handle,
            unrar_sys::RAR_SKIP,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ret == unrar_sys::ERAR_SUCCESS {
        Ok(())
    } else {
        Err(RarPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: RarPageMapSlowFailureReason::ProcessFile,
        })
    }
}

fn open_rar_archive(path: &Path) -> Result<RarArchiveHandle, RarPageMapSlowFailure> {
    // SAFETY: C struct は全フィールドを明示的に埋めてから unrar へ渡し、未初期化のまま読まない。
    let mut open_data = unsafe { std::mem::zeroed::<unrar_sys::OpenArchiveDataEx>() };
    #[cfg(windows)]
    let archive_name_w = {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    #[cfg(not(windows))]
    let archive_name_c = {
        use std::os::unix::ffi::OsStrExt as _;
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| RarPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: RarPageMapSlowFailureReason::OpenArchive,
        })?
    };

    #[cfg(windows)]
    {
        open_data.archive_name = std::ptr::null();
        open_data.archive_name_w = archive_name_w.as_ptr();
    }
    #[cfg(not(windows))]
    {
        open_data.archive_name = archive_name_c.as_ptr();
        open_data.archive_name_w = std::ptr::null();
    }
    open_data.open_mode = unrar_sys::RAR_OM_EXTRACT;
    open_data.open_result = 0;
    open_data.comment_buffer = std::ptr::null_mut();
    open_data.comment_buffer_size = 0;
    open_data.comment_size = 0;
    open_data.comment_state = 0;
    open_data.flags = 0;
    open_data.callback = None;
    open_data.user_data = 0;
    open_data.op_flags = 0;
    open_data.comment_buffer_w = std::ptr::null_mut();
    open_data.reserved = [0; 25];

    // SAFETY:
    // archive path バッファはこの呼び出し中生存し、`open_data` は unrar が要求するレイアウトで初期化済み。
    let handle = unsafe { unrar_sys::RAROpenArchiveEx(&mut open_data as *mut _) };
    if handle.is_null() || open_data.open_result != unrar_sys::ERAR_SUCCESS as u32 {
        return Err(RarPageMapSlowFailure {
            page_index: None,
            entry_index: None,
            reason: RarPageMapSlowFailureReason::OpenArchive,
        });
    }

    Ok(RarArchiveHandle {
        handle,
        flags: open_data.flags,
    })
}

extern "C" fn rar_process_data_callback(
    msg: unrar_sys::UINT,
    user_data: unrar_sys::LPARAM,
    p1: unrar_sys::LPARAM,
    p2: unrar_sys::LPARAM,
) -> std::os::raw::c_int {
    if msg != unrar_sys::UCM_PROCESSDATA {
        return 0;
    }
    // SAFETY:
    // `user_data` には callback 登録時に `RarProcessCallbackState` の mutable pointer を渡している。
    // callback 実行中は unrar が同じ pointer をそのまま返す前提で使う。
    let state = unsafe { &mut *(user_data as *mut RarProcessCallbackState) };
    let len = match usize::try_from(p2) {
        Ok(len) => len,
        Err(_) => return 0,
    };
    if len == 0 {
        return 0;
    }
    let ptr = p1 as *const u8;
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: `ptr` は null を除外済みで、長さ `len` は unrar の callback 引数そのまま使う。
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    state.current_bytes.extend_from_slice(slice);
    0
}

fn header_filename(header: &unrar_sys::HeaderDataEx) -> String {
    #[cfg(windows)]
    {
        let len = header
            .filename_w
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(header.filename_w.len());
        let wide = &header.filename_w[..len];
        // SAFETY: `filename_w` は UTF-16 相当の固定長配列で、先頭 `len` 要素だけを読む。
        let wide = unsafe { std::slice::from_raw_parts(wide.as_ptr(), wide.len()) };
        String::from_utf16_lossy(wide)
    }
    #[cfg(not(windows))]
    {
        let len = header
            .filename_w
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(header.filename_w.len());
        let mut out = String::new();
        for &ch in &header.filename_w[..len] {
            if let Some(ch) = char::from_u32(ch as u32) {
                out.push(ch);
            } else {
                out.push('\u{FFFD}');
            }
        }
        out
    }
}

fn unpack_size(low: u32, high: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn split_ranges(len: usize, workers: usize) -> Vec<(usize, usize)> {
    if len == 0 {
        return Vec::new();
    }
    let workers = workers.max(1).min(len);
    let chunk = len.div_ceil(workers);
    let mut ranges = Vec::with_capacity(workers);
    let mut start = 0usize;
    while start < len {
        let end = (start + chunk).min(len);
        ranges.push((start, end));
        start = end;
    }
    ranges
}
