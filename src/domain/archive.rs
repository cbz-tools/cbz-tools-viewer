use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use blake3::Hash;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::infra::archive::{
    collect_cbz_rebuild_archive_selection, write_cbz_rebuild_tmp_archive,
    CbzRebuildArchiveSelection,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookId(pub Hash);

impl BookId {
    pub fn from_path(path: &Path) -> Self {
        let bytes = path.to_string_lossy();
        Self(blake3::hash(bytes.as_bytes()))
    }
}

impl Serialize for BookId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.to_hex().as_str())
    }
}

impl<'de> Deserialize<'de> for BookId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let hash = Hash::from_hex(s).map_err(serde::de::Error::custom)?;
        Ok(Self(hash))
    }
}

#[derive(Clone, Debug)]
pub struct BookMeta {
    pub id: BookId,
    pub path: Arc<Path>,
    pub title: Arc<str>,
    pub size: u64,
    pub modified: SystemTime,
    pub page_count: Option<u32>, // 未スキャン時は None
}

#[derive(Clone, Debug)]
pub struct FolderMeta {
    pub path: Arc<Path>,
    pub title: Arc<str>,
    pub modified: SystemTime,
}

#[derive(Clone, Debug)]
pub struct ImageFileMeta {
    pub path: Arc<Path>,
    pub title: Arc<str>,
    pub size: u64,
    pub modified: SystemTime,
}

#[derive(Clone, Debug)]
/// Library の1件分の実体。
///
/// `Archive` は書庫、`FolderBook` は直下に画像を持つフォルダ、`ImageFile` は
/// 画像本への入口、`Folder` はナビゲーション対象外の通常フォルダ。
pub enum LibraryEntry {
    Folder(FolderMeta),
    FolderBook(FolderMeta),
    ImageFile(ImageFileMeta),
    Archive(BookMeta),
}

impl LibraryEntry {
    /// お気に入りやグループ設定のような「本単位」の操作に乗るのは、
    /// 現状は Archive と FolderBook だけ。
    pub fn is_favorite_target(&self) -> bool {
        matches!(self, LibraryEntry::Archive(_) | LibraryEntry::FolderBook(_))
    }

    /// サムネイル / navigation で本を同一視するための安定キー。
    /// Folder は本ではないので None、ImageFile は親フォルダの画像本と分けるため
    /// 自身の path をキーにする。
    pub fn thumb_id(&self) -> Option<BookId> {
        match self {
            LibraryEntry::Archive(entry) => Some(entry.id.clone()),
            LibraryEntry::FolderBook(entry) => Some(BookId::from_path(entry.path.as_ref())),
            LibraryEntry::ImageFile(entry) => Some(BookId::from_path(entry.path.as_ref())),
            LibraryEntry::Folder(_) => None,
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            LibraryEntry::Folder(entry) => entry.path.as_ref(),
            LibraryEntry::FolderBook(entry) => entry.path.as_ref(),
            LibraryEntry::ImageFile(entry) => entry.path.as_ref(),
            LibraryEntry::Archive(entry) => entry.path.as_ref(),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            LibraryEntry::Folder(entry) => entry.title.as_ref(),
            LibraryEntry::FolderBook(entry) => entry.title.as_ref(),
            LibraryEntry::ImageFile(entry) => entry.title.as_ref(),
            LibraryEntry::Archive(entry) => entry.title.as_ref(),
        }
    }

    pub fn modified(&self) -> SystemTime {
        match self {
            LibraryEntry::Folder(entry) => entry.modified,
            LibraryEntry::FolderBook(entry) => entry.modified,
            LibraryEntry::ImageFile(entry) => entry.modified,
            LibraryEntry::Archive(entry) => entry.modified,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            LibraryEntry::ImageFile(entry) => entry.size,
            LibraryEntry::Archive(entry) => entry.size,
            LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) => 0,
        }
    }

    pub fn page_count(&self) -> Option<u32> {
        match self {
            LibraryEntry::Archive(entry) => entry.page_count,
            LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => {
                None
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbzRebuildPlanOptions {
    pub delete_entries: Vec<String>,
    /// 将来の実アーカイブ走査結果を受けるためのフック。
    /// `Some(0)` が渡されたら「画像 entry が 0 件になる」エラーにする。
    pub remaining_image_entries_after_delete: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbzRebuildPlan {
    pub input_path: PathBuf,
    pub output_path: PathBuf,
    pub tmp_path: PathBuf,
    pub backup_path: PathBuf,
    pub delete_entries: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbzRebuildPreparedTmp {
    pub plan: CbzRebuildPlan,
    pub selection: CbzRebuildArchiveSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbzRebuildCompleted {
    pub plan: CbzRebuildPlan,
    pub selection: CbzRebuildArchiveSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CbzRebuildPlanError {
    UnsupportedLibraryEntryKind,
    EpubNotSupported,
    EmptyDeleteEntries,
    NoImageEntriesAfterDelete,
    OutputPathAlreadyExists(PathBuf),
    TmpPathAlreadyExists(PathBuf),
    BackupPathAlreadyExists(PathBuf),
}

#[derive(Debug)]
pub enum CbzRebuildFinalizeError {
    OldArchiveMissing(PathBuf),
    TmpArchiveMissing(PathBuf),
    BackupPathAlreadyExists(PathBuf),
    OutputPathAlreadyExists(PathBuf),
    RenameOldToBackup {
        old_path: PathBuf,
        backup_path: PathBuf,
        source: std::io::Error,
    },
    RenameTmpToOutput {
        tmp_path: PathBuf,
        output_path: PathBuf,
        source: std::io::Error,
    },
    RemoveBackup {
        backup_path: PathBuf,
        source: std::io::Error,
    },
    RollbackRenameBackupToOld {
        backup_path: PathBuf,
        old_path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for CbzRebuildPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedLibraryEntryKind => {
                write!(
                    f,
                    "cbz rebuild only supports zip/cbz/rar/cbr library archives"
                )
            }
            Self::EpubNotSupported => write!(f, "cbz rebuild does not support epub"),
            Self::EmptyDeleteEntries => write!(f, "delete_entries must not be empty"),
            Self::NoImageEntriesAfterDelete => {
                write!(f, "cbz rebuild would leave no image entries")
            }
            Self::OutputPathAlreadyExists(path) => {
                write!(
                    f,
                    "cbz rebuild output path already exists: {}",
                    path.display()
                )
            }
            Self::TmpPathAlreadyExists(path) => {
                write!(f, "cbz rebuild tmp path already exists: {}", path.display())
            }
            Self::BackupPathAlreadyExists(path) => {
                write!(
                    f,
                    "cbz rebuild backup path already exists: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for CbzRebuildPlanError {}

impl std::fmt::Display for CbzRebuildFinalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OldArchiveMissing(path) => {
                write!(
                    f,
                    "cbz rebuild finalize failed before rename: old archive missing: {}",
                    path.display()
                )
            }
            Self::TmpArchiveMissing(path) => {
                write!(
                    f,
                    "cbz rebuild finalize failed before rename: tmp archive missing: {}",
                    path.display()
                )
            }
            Self::BackupPathAlreadyExists(path) => {
                write!(
                    f,
                    "cbz rebuild finalize failed before rename: backup path already exists: {}",
                    path.display()
                )
            }
            Self::OutputPathAlreadyExists(path) => {
                write!(
                    f,
                    "cbz rebuild finalize failed before rename: output path already exists: {}",
                    path.display()
                )
            }
            Self::RenameOldToBackup {
                old_path,
                backup_path,
                source,
            } => write!(
                f,
                "cbz rebuild finalize failed at old->backup rename: {} -> {}: {}",
                old_path.display(),
                backup_path.display(),
                source
            ),
            Self::RenameTmpToOutput {
                tmp_path,
                output_path,
                source,
            } => write!(
                f,
                "cbz rebuild finalize failed at tmp->output rename: {} -> {}: {}",
                tmp_path.display(),
                output_path.display(),
                source
            ),
            Self::RemoveBackup {
                backup_path,
                source,
            } => write!(
                f,
                "cbz rebuild finalize failed after output commit at backup delete: {}: {}",
                backup_path.display(),
                source
            ),
            Self::RollbackRenameBackupToOld {
                backup_path,
                old_path,
                source,
            } => write!(
                f,
                "cbz rebuild finalize failed during rollback backup->old rename: {} -> {}: {}",
                backup_path.display(),
                old_path.display(),
                source
            ),
        }
    }
}

impl std::error::Error for CbzRebuildFinalizeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RenameOldToBackup { source, .. }
            | Self::RenameTmpToOutput { source, .. }
            | Self::RemoveBackup { source, .. }
            | Self::RollbackRenameBackupToOld { source, .. } => Some(source),
            Self::OldArchiveMissing(_)
            | Self::TmpArchiveMissing(_)
            | Self::BackupPathAlreadyExists(_)
            | Self::OutputPathAlreadyExists(_) => None,
        }
    }
}

pub fn plan_cbz_rebuild_for_library_entry(
    entry: &LibraryEntry,
    options: CbzRebuildPlanOptions,
) -> Result<CbzRebuildPlan, CbzRebuildPlanError> {
    if options.delete_entries.is_empty() {
        return Err(CbzRebuildPlanError::EmptyDeleteEntries);
    }
    if options.remaining_image_entries_after_delete == Some(0) {
        return Err(CbzRebuildPlanError::NoImageEntriesAfterDelete);
    }

    let input_path = match entry {
        LibraryEntry::Archive(entry) => entry.path.as_ref().to_path_buf(),
        LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => {
            return Err(CbzRebuildPlanError::UnsupportedLibraryEntryKind);
        }
    };

    // ZIP/CBZ inputs are rebuilt in place with the same extension; RAR/CBR inputs are rebuilt as CBZ.
    let output_path = match input_path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("zip") | Some("cbz") => input_path.clone(),
        Some("rar") | Some("cbr") => {
            let mut path = input_path.clone();
            path.set_extension("cbz");
            path
        }
        Some("epub") => return Err(CbzRebuildPlanError::EpubNotSupported),
        _ => return Err(CbzRebuildPlanError::UnsupportedLibraryEntryKind),
    };

    let mut tmp_path = OsString::from(output_path.as_os_str());
    tmp_path.push(".rebuild.tmp");
    let tmp_path = PathBuf::from(tmp_path);

    let mut backup_path = OsString::from(input_path.as_os_str());
    backup_path.push(".rebuild_backup");
    let backup_path = PathBuf::from(backup_path);

    if output_path != input_path && output_path.exists() {
        return Err(CbzRebuildPlanError::OutputPathAlreadyExists(output_path));
    }
    if tmp_path.exists() {
        return Err(CbzRebuildPlanError::TmpPathAlreadyExists(tmp_path));
    }
    if backup_path.exists() {
        return Err(CbzRebuildPlanError::BackupPathAlreadyExists(backup_path));
    }

    Ok(CbzRebuildPlan {
        input_path,
        output_path,
        tmp_path,
        backup_path,
        delete_entries: options.delete_entries,
    })
}

pub fn prepare_cbz_rebuild_tmp_for_library_entry(
    entry: &LibraryEntry,
    options: CbzRebuildPlanOptions,
) -> anyhow::Result<CbzRebuildPreparedTmp> {
    let plan = plan_cbz_rebuild_for_library_entry(entry, options).map_err(anyhow::Error::from)?;
    let selection = collect_cbz_rebuild_archive_selection(&plan.input_path, &plan.delete_entries)?;
    write_cbz_rebuild_tmp_archive(&plan.input_path, &plan.tmp_path, &selection)?;
    Ok(CbzRebuildPreparedTmp { plan, selection })
}

pub fn finalize_cbz_rebuild_for_library_entry(
    prepared: CbzRebuildPreparedTmp,
) -> Result<CbzRebuildCompleted, CbzRebuildFinalizeError> {
    finalize_cbz_rebuild_plan(&prepared.plan)?;
    Ok(CbzRebuildCompleted {
        plan: prepared.plan,
        selection: prepared.selection,
    })
}

pub fn rebuild_cbz_for_library_entry(
    entry: &LibraryEntry,
    options: CbzRebuildPlanOptions,
) -> anyhow::Result<CbzRebuildCompleted> {
    let prepared = prepare_cbz_rebuild_tmp_for_library_entry(entry, options)?;
    let completed =
        finalize_cbz_rebuild_for_library_entry(prepared).map_err(anyhow::Error::from)?;
    Ok(completed)
}

fn finalize_cbz_rebuild_plan(plan: &CbzRebuildPlan) -> Result<(), CbzRebuildFinalizeError> {
    if !plan.input_path.exists() {
        return Err(CbzRebuildFinalizeError::OldArchiveMissing(
            plan.input_path.clone(),
        ));
    }
    if !plan.tmp_path.exists() {
        return Err(CbzRebuildFinalizeError::TmpArchiveMissing(
            plan.tmp_path.clone(),
        ));
    }
    if plan.backup_path.exists() {
        return Err(CbzRebuildFinalizeError::BackupPathAlreadyExists(
            plan.backup_path.clone(),
        ));
    }
    if plan.output_path != plan.input_path && plan.output_path.exists() {
        return Err(CbzRebuildFinalizeError::OutputPathAlreadyExists(
            plan.output_path.clone(),
        ));
    }

    std::fs::rename(&plan.input_path, &plan.backup_path).map_err(|source| {
        CbzRebuildFinalizeError::RenameOldToBackup {
            old_path: plan.input_path.clone(),
            backup_path: plan.backup_path.clone(),
            source,
        }
    })?;

    if let Err(source) = std::fs::rename(&plan.tmp_path, &plan.output_path) {
        let rollback_result = std::fs::rename(&plan.backup_path, &plan.input_path);
        return match rollback_result {
            Ok(()) => Err(CbzRebuildFinalizeError::RenameTmpToOutput {
                tmp_path: plan.tmp_path.clone(),
                output_path: plan.output_path.clone(),
                source,
            }),
            Err(rollback_source) => Err(CbzRebuildFinalizeError::RollbackRenameBackupToOld {
                backup_path: plan.backup_path.clone(),
                old_path: plan.input_path.clone(),
                source: rollback_source,
            }),
        };
    }

    std::fs::remove_file(&plan.backup_path).map_err(|source| {
        CbzRebuildFinalizeError::RemoveBackup {
            backup_path: plan.backup_path.clone(),
            source,
        }
    })?;

    Ok(())
}
