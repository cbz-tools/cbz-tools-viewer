// RAR サポート（unrar bindings）
// --features rar でビルドした場合のみ有効
use anyhow::Result;
use bytes::Bytes;
use std::io::{Seek, Write};
use std::path::Path;
#[cfg(feature = "rar")]
use std::{path::PathBuf, time::Instant};

#[cfg(feature = "rar")]
use super::{
    write_cbz_rebuild_directory_entry, write_cbz_rebuild_file_entry, CbzRebuildArchiveEntryKind,
};
use super::{BookReader, CbzRebuildArchiveEntry};
#[cfg(feature = "rar")]
use crate::util::archive_path::is_supported_image_name;
#[cfg(feature = "rar")]
use crate::util::natural_sort;
use zip_writer::ZipWriter;

// ── RarReader ─────────────────────────────────────────────────────────────────

pub struct RarReader {
    #[cfg(feature = "rar")]
    path: PathBuf,
    /// natural sort 済み画像エントリ名（アーカイブ内パス）
    #[cfg(feature = "rar")]
    image_names: Vec<String>,
}

impl RarReader {
    pub fn open(path: &Path) -> Result<Self> {
        #[cfg(feature = "rar")]
        return open_impl(path);

        #[cfg(not(feature = "rar"))]
        {
            let _ = path;
            anyhow::bail!("RAR サポートは無効です（--features rar でビルドしてください）")
        }
    }

    pub(crate) fn page_display_labels(&self) -> Vec<String> {
        #[cfg(feature = "rar")]
        {
            self.image_names
                .iter()
                .map(|name| {
                    name.rsplit(['/', '\\'])
                        .find(|part| !part.is_empty())
                        .unwrap_or(name.as_str())
                        .to_owned()
                })
                .collect()
        }

        #[cfg(not(feature = "rar"))]
        {
            Vec::new()
        }
    }

    pub(crate) fn page_entry_names(&self) -> Vec<String> {
        #[cfg(feature = "rar")]
        {
            self.image_names.clone()
        }

        #[cfg(not(feature = "rar"))]
        {
            Vec::new()
        }
    }
}

pub(crate) fn list_cbz_rebuild_entries(path: &Path) -> Result<Vec<CbzRebuildArchiveEntry>> {
    #[cfg(feature = "rar")]
    {
        list_cbz_rebuild_entries_impl(path)
    }

    #[cfg(not(feature = "rar"))]
    {
        let _ = path;
        anyhow::bail!("RAR サポートは無効です（--features rar でビルドしてください）")
    }
}

pub(crate) fn write_cbz_rebuild_keep_entries<W: Write + Seek>(
    path: &Path,
    keep_entries: &[CbzRebuildArchiveEntry],
    writer: &mut ZipWriter<W>,
) -> Result<()> {
    #[cfg(feature = "rar")]
    {
        for entry in keep_entries {
            match entry.kind {
                CbzRebuildArchiveEntryKind::Directory => {
                    write_cbz_rebuild_directory_entry(writer, &entry.name)?;
                }
                CbzRebuildArchiveEntryKind::Image | CbzRebuildArchiveEntryKind::NonImage => {
                    let bytes = read_entry_impl(path, &entry.name)?;
                    write_cbz_rebuild_file_entry(writer, &entry.name, &bytes)?;
                }
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "rar"))]
    {
        let _ = (path, keep_entries, writer);
        anyhow::bail!("RAR サポートは無効です（--features rar でビルドしてください）")
    }
}

// ── rar feature 有効時の実装 ──────────────────────────────────────────────────

#[cfg(feature = "rar")]
fn open_impl(path: &Path) -> Result<RarReader> {
    use unrar::Archive;

    ensure_unrar_dll_for_current_target()?;
    let started = Instant::now();

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF8 パス: {}", path.display()))?;

    let archive = match Archive::new(path_str).open_for_listing() {
        Ok(archive) => archive,
        Err(e) => {
            #[cfg(windows)]
            {
                let dll_name = expected_unrar_dll_name();
                let dll_path = Path::new(dll_name);
                tracing::warn!(
                    path = %path.display(),
                    code = ?e.code,
                    dll_path = %dll_path.display(),
                    dll_exists = dll_path.exists(),
                    "rar_reader: open failed"
                );
            }
            #[cfg(not(windows))]
            {
                tracing::warn!(
                    path = %path.display(),
                    code = ?e.code,
                    "rar_reader: open failed"
                );
            }
            return Err(anyhow::anyhow!("{}", format_rar_open_error(path, e.code)));
        }
    };

    let mut names: Vec<String> = Vec::new();
    let mut entry_count: u32 = 0;
    for entry in archive.into_iter() {
        entry_count += 1;
        let e =
            entry.map_err(|err| anyhow::anyhow!("{}", format_rar_listing_error(path, err.code)))?;
        if e.is_directory() {
            tracing::trace!(
                path = %path.display(),
                accepted = false,
                reason = "directory",
                name = %e.filename.to_string_lossy(),
                "rar_reader: image filter"
            );
            continue;
        }
        // NOTE:
        // `to_string_lossy()` は不正なシーケンスを置換するため、
        // ファイル名の厳密な同一性は将来課題。
        let name = e.filename.to_string_lossy().into_owned();
        if is_image_name(&name) {
            tracing::trace!(
                path = %path.display(),
                accepted = true,
                extension = %extension_of(&name),
                name = %name,
                "rar_reader: image filter"
            );
            names.push(name);
        } else {
            tracing::trace!(
                path = %path.display(),
                accepted = false,
                extension = %extension_of(&name),
                name = %name,
                "rar_reader: image filter"
            );
        }
    }

    names.sort_by(|a, b| natural_sort::compare(a, b));
    let image_count = names.len();
    tracing::debug!(
        path = %path.display(),
        entry_count,
        image_count,
        elapsed_ms = started.elapsed().as_millis(),
        "rar_reader: open complete"
    );
    if image_count == 0 {
        tracing::warn!(
            path = %path.display(),
            entry_count,
            "rar_reader: page_count is zero after image filter"
        );
    }

    Ok(RarReader {
        path: path.to_path_buf(),
        image_names: names,
    })
}

#[cfg(feature = "rar")]
fn list_cbz_rebuild_entries_impl(path: &Path) -> Result<Vec<CbzRebuildArchiveEntry>> {
    use unrar::Archive;

    ensure_unrar_dll_for_current_target()?;

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF8 パス: {}", path.display()))?;

    let archive = Archive::new(path_str)
        .open_for_listing()
        .map_err(|e| anyhow::anyhow!("{}", format_rar_open_error(path, e.code)))?;

    let mut entries = Vec::new();
    for entry in archive.into_iter() {
        let entry =
            entry.map_err(|err| anyhow::anyhow!("{}", format_rar_listing_error(path, err.code)))?;
        let name = entry.filename.to_string_lossy().into_owned();
        let kind = if entry.is_directory() {
            CbzRebuildArchiveEntryKind::Directory
        } else if is_image_name(&name) {
            CbzRebuildArchiveEntryKind::Image
        } else {
            CbzRebuildArchiveEntryKind::NonImage
        };
        entries.push(CbzRebuildArchiveEntry { name, kind });
    }
    Ok(entries)
}

#[cfg(feature = "rar")]
fn read_entry_impl(path: &Path, target: &str) -> Result<Bytes> {
    use unrar::Archive;

    ensure_unrar_dll_for_current_target()?;

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF8 パス"))?;

    let mut archive = match Archive::new(path_str).open_for_processing() {
        Ok(archive) => archive,
        Err(e) => {
            #[cfg(windows)]
            {
                let dll_name = expected_unrar_dll_name();
                let dll_path = Path::new(dll_name);
                tracing::warn!(
                    path = %path.display(),
                    code = ?e.code,
                    dll_path = %dll_path.display(),
                    dll_exists = dll_path.exists(),
                    "rar_reader: open_for_processing failed"
                );
            }
            #[cfg(not(windows))]
            {
                tracing::warn!(
                    path = %path.display(),
                    code = ?e.code,
                    "rar_reader: open_for_processing failed"
                );
            }
            return Err(anyhow::anyhow!("{}", format_rar_open_error(path, e.code)));
        }
    };

    loop {
        let header_read = archive.read_header();
        archive = match header_read {
            Ok(None) => {
                tracing::trace!(
                    path = %path.display(),
                    target = %target,
                    status = "end_archive",
                    "rar_reader: read_header result"
                );
                anyhow::bail!("エントリ '{}' が RAR 内に見つかりません", target)
            }
            Ok(Some(header)) => {
                tracing::trace!(
                    path = %path.display(),
                    target = %target,
                    status = "success",
                    "rar_reader: read_header result"
                );
                let name = header.entry().filename.to_string_lossy().into_owned();
                if name == target {
                    let (data, _next) = header.read().map_err(|e| {
                        anyhow::anyhow!(
                            "{}",
                            format_rar_extract_error(path, target, e.code, "read_data")
                        )
                    })?;
                    return Ok(Bytes::from(data));
                }
                header.skip().map_err(|e| {
                    anyhow::anyhow!("{}", format_rar_extract_error(path, target, e.code, "skip"))
                })?
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    target = %target,
                    status = "error",
                    code = ?e.code,
                    "rar_reader: read_header result"
                );
                return Err(anyhow::anyhow!(
                    "{}",
                    format_rar_extract_error(path, target, e.code, "read_header")
                ));
            }
        }
    }
}

#[cfg(feature = "rar")]
fn format_rar_open_error(path: &Path, code: unrar::error::Code) -> String {
    use unrar::error::Code;
    match code {
        Code::MissingPassword | Code::BadPassword => format!(
            "RAR open failure (password required): path={} code={:?}",
            path.display(),
            code
        ),
        // DLL なし/ロード失敗を unrar 側で厳密には区別できないため、open failure として扱う。
        Code::EOpen => format!(
            "RAR open failure (possible DLL missing/load failure): path={} code={:?}",
            path.display(),
            code
        ),
        _ => format!(
            "RAR archive open failure: path={} code={:?}",
            path.display(),
            code
        ),
    }
}

#[cfg(all(feature = "rar", windows))]
fn expected_unrar_dll_name() -> &'static str {
    #[cfg(target_pointer_width = "64")]
    {
        "UnRAR64.dll"
    }
    #[cfg(target_pointer_width = "32")]
    {
        "UnRAR.dll"
    }
}

#[cfg(all(feature = "rar", windows))]
fn ensure_unrar_dll_for_current_target() -> Result<()> {
    let dll_name = expected_unrar_dll_name();
    let current_exe = std::env::current_exe().ok();
    let exe_dir_dll_path = current_exe
        .as_ref()
        .and_then(|path| path.parent().map(|dir| dir.join(dll_name)));
    let cwd_dll_path = Path::new(dll_name).to_path_buf();
    let resolved_dll_path = exe_dir_dll_path
        .as_ref()
        .filter(|path| path.exists())
        .cloned()
        .or_else(|| cwd_dll_path.exists().then_some(cwd_dll_path.clone()));
    tracing::debug!(
        current_exe = ?current_exe.as_ref().map(|p| p.display().to_string()),
        exe_dir_dll_path = ?exe_dir_dll_path.as_ref().map(|p| p.display().to_string()),
        cwd_dll_path = %cwd_dll_path.display(),
        resolved_dll_path = ?resolved_dll_path.as_ref().map(|p| p.display().to_string()),
        exists = resolved_dll_path.is_some(),
        "rar_reader: dll check"
    );
    if resolved_dll_path.is_some() {
        Ok(())
    } else {
        anyhow::bail!(
            "RAR open failure (DLL missing): expected '{}' next to the executable (fallback: current working directory '{}') for this build target (pointer width {})",
            exe_dir_dll_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| format!("<unknown-exe-dir>\\{dll_name}")),
            cwd_dll_path.display(),
            std::mem::size_of::<usize>() * 8
        )
    }
}

#[cfg(all(feature = "rar", not(windows)))]
fn ensure_unrar_dll_for_current_target() -> Result<()> {
    Ok(())
}

#[cfg(feature = "rar")]
fn format_rar_listing_error(path: &Path, code: unrar::error::Code) -> String {
    use unrar::error::Code;
    match code {
        Code::MissingPassword | Code::BadPassword => format!(
            "RAR listing failure (password required): path={} code={:?}",
            path.display(),
            code
        ),
        _ => format!(
            "RAR listing failure: path={} code={:?}",
            path.display(),
            code
        ),
    }
}

#[cfg(feature = "rar")]
fn format_rar_extract_error(
    path: &Path,
    target: &str,
    code: unrar::error::Code,
    phase: &str,
) -> String {
    use unrar::error::Code;
    match code {
        Code::MissingPassword | Code::BadPassword => format!(
            "RAR extract failure (password required): path={} entry={} phase={} code={:?}",
            path.display(),
            target,
            phase,
            code
        ),
        _ => format!(
            "RAR extract failure: path={} entry={} phase={} code={:?}",
            path.display(),
            target,
            phase,
            code
        ),
    }
}

// ── BookReader impl ────────────────────────────────────────────────────────────

impl BookReader for RarReader {
    fn read_first_image(&self) -> Result<Bytes> {
        #[cfg(feature = "rar")]
        {
            let name = self
                .image_names
                .first()
                .ok_or_else(|| anyhow::anyhow!("アーカイブに画像がありません"))?;
            read_entry_impl(&self.path, name)
        }
        #[cfg(not(feature = "rar"))]
        anyhow::bail!("RAR サポートが無効です")
    }

    fn page_count(&self) -> u32 {
        #[cfg(feature = "rar")]
        {
            self.image_names.len() as u32
        }
        #[cfg(not(feature = "rar"))]
        0
    }

    fn read_page_n(&self, n: u32) -> Result<Bytes> {
        #[cfg(feature = "rar")]
        {
            let name = self.image_names.get(n as usize).ok_or_else(|| {
                anyhow::anyhow!("ページ {} が範囲外 (total {})", n, self.image_names.len())
            })?;
            read_entry_impl(&self.path, name)
        }
        #[cfg(not(feature = "rar"))]
        {
            let _ = n;
            anyhow::bail!("RAR サポートが無効です")
        }
    }
}

// ── ヘルパー ──────────────────────────────────────────────────────────────────

#[cfg(feature = "rar")]
fn is_image_name(name: &str) -> bool {
    is_supported_image_name(name.rsplit('.').next().unwrap_or(""))
}

#[cfg(feature = "rar")]
fn extension_of(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or("")
}
