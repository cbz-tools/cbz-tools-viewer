use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CbzRebuildTransactionStage {
    Plan,
    SelectEntries,
    WriteTemp,
    ValidateBeforeCommit,
    RenameInputToBackup,
    RenameTempToOutput,
    RollbackBackupToInput,
    RemoveBackup,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CbzRebuildCommitState {
    NotCommitted,
    Committed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CbzRebuildArtifactPresence {
    Present,
    Absent,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CbzRebuildTransactionPaths {
    pub input: PathBuf,
    pub output: PathBuf,
    pub temp: PathBuf,
    pub backup: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CbzRebuildArtifactSnapshot {
    pub input: CbzRebuildArtifactPresence,
    pub output: CbzRebuildArtifactPresence,
    pub temp: CbzRebuildArtifactPresence,
    pub backup: CbzRebuildArtifactPresence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CbzRebuildPrecondition {
    InputPresent,
    TempPresent,
    BackupAbsent,
    DistinctOutputAbsent,
}

#[derive(Debug)]
struct CbzRebuildPreconditionError {
    condition: CbzRebuildPrecondition,
    path: PathBuf,
}

impl fmt::Display for CbzRebuildPreconditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.condition {
            CbzRebuildPrecondition::InputPresent => write!(
                f,
                "cbz rebuild finalize failed before rename: old archive missing: {}",
                self.path.display()
            ),
            CbzRebuildPrecondition::TempPresent => write!(
                f,
                "cbz rebuild finalize failed before rename: tmp archive missing: {}",
                self.path.display()
            ),
            CbzRebuildPrecondition::BackupAbsent => write!(
                f,
                "cbz rebuild finalize failed before rename: backup path already exists: {}",
                self.path.display()
            ),
            CbzRebuildPrecondition::DistinctOutputAbsent => write!(
                f,
                "cbz rebuild finalize failed before rename: output path already exists: {}",
                self.path.display()
            ),
        }
    }
}

impl Error for CbzRebuildPreconditionError {}

#[derive(Debug)]
enum CbzRebuildOperationErrorSource {
    Io(io::Error),
    Precondition(CbzRebuildPreconditionError),
}

impl fmt::Display for CbzRebuildOperationErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(f),
            Self::Precondition(source) => source.fmt(f),
        }
    }
}

impl Error for CbzRebuildOperationErrorSource {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Precondition(source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub(crate) struct CbzRebuildOperationError {
    pub stage: CbzRebuildTransactionStage,
    source: CbzRebuildOperationErrorSource,
}

impl CbzRebuildOperationError {
    fn io(stage: CbzRebuildTransactionStage, source: io::Error) -> Self {
        Self {
            stage,
            source: CbzRebuildOperationErrorSource::Io(source),
        }
    }

    fn precondition(condition: CbzRebuildPrecondition, path: PathBuf) -> Self {
        Self {
            stage: CbzRebuildTransactionStage::ValidateBeforeCommit,
            source: CbzRebuildOperationErrorSource::Precondition(CbzRebuildPreconditionError {
                condition,
                path,
            }),
        }
    }
}

impl fmt::Display for CbzRebuildOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.stage {
            CbzRebuildTransactionStage::RenameInputToBackup => write!(
                f,
                "cbz rebuild finalize failed at old->backup rename: {}",
                self.source
            ),
            CbzRebuildTransactionStage::RenameTempToOutput => write!(
                f,
                "cbz rebuild finalize failed at tmp->output rename: {}",
                self.source
            ),
            CbzRebuildTransactionStage::RollbackBackupToInput => write!(
                f,
                "cbz rebuild finalize failed during rollback backup->old rename: {}",
                self.source
            ),
            CbzRebuildTransactionStage::RemoveBackup => write!(
                f,
                "cbz rebuild finalize failed after output commit at backup delete: {}",
                self.source
            ),
            CbzRebuildTransactionStage::Plan
            | CbzRebuildTransactionStage::SelectEntries
            | CbzRebuildTransactionStage::WriteTemp
            | CbzRebuildTransactionStage::ValidateBeforeCommit => self.source.fmt(f),
        }
    }
}

impl Error for CbzRebuildOperationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CbzRebuildCleanupOperation {
    RemoveBackup,
    RemoveTemp,
}

#[derive(Debug)]
pub(crate) struct CbzRebuildCleanupIssue {
    pub operation: CbzRebuildCleanupOperation,
    pub path: PathBuf,
    source: io::Error,
}

impl fmt::Display for CbzRebuildCleanupIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.operation {
            CbzRebuildCleanupOperation::RemoveBackup => write!(
                f,
                "cbz rebuild finalize failed after output commit at backup delete: {}: {}",
                self.path.display(),
                self.source
            ),
            CbzRebuildCleanupOperation::RemoveTemp => write!(
                f,
                "cbz rebuild temp cleanup failed: {}: {}",
                self.path.display(),
                self.source
            ),
        }
    }
}

impl Error for CbzRebuildCleanupIssue {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct CbzRebuildCommitted<T> {
    pub completed: T,
    pub commit_state: CbzRebuildCommitState,
    pub paths: CbzRebuildTransactionPaths,
    pub cleanup_warnings: Vec<CbzRebuildCleanupIssue>,
}

#[derive(Debug)]
pub(crate) struct CbzRebuildFailureContext {
    pub paths: CbzRebuildTransactionPaths,
    pub artifacts: CbzRebuildArtifactSnapshot,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum CbzRebuildTransactionFailure {
    NotCommitted {
        stage: CbzRebuildTransactionStage,
        primary_error: CbzRebuildOperationError,
        context: Box<CbzRebuildFailureContext>,
    },
    RolledBack {
        primary_error: CbzRebuildOperationError,
        context: Box<CbzRebuildFailureContext>,
    },
    RecoveryRequired {
        commit_state: CbzRebuildCommitState,
        primary_error: CbzRebuildOperationError,
        recovery_error: Option<CbzRebuildOperationError>,
        context: Box<CbzRebuildFailureContext>,
    },
}

impl fmt::Display for CbzRebuildTransactionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotCommitted {
                primary_error,
                context,
                ..
            }
            | Self::RolledBack {
                primary_error,
                context,
            } => fmt_operation_with_paths(f, primary_error, &context.paths),
            Self::RecoveryRequired {
                commit_state,
                primary_error,
                recovery_error,
                context,
            } => {
                write!(
                    f,
                    "cbz rebuild recovery required (commit_state={commit_state:?}): "
                )?;
                fmt_operation_with_paths(f, primary_error, &context.paths)?;
                if let Some(recovery_error) = recovery_error {
                    write!(f, "; recovery error: ")?;
                    fmt_operation_with_paths(f, recovery_error, &context.paths)?;
                }
                write!(f, "; artifacts={:?}", context.artifacts)
            }
        }
    }
}

impl Error for CbzRebuildTransactionFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotCommitted { primary_error, .. }
            | Self::RolledBack { primary_error, .. }
            | Self::RecoveryRequired { primary_error, .. } => Some(primary_error),
        }
    }
}

pub(crate) fn finalize_cbz_rebuild_transaction<T>(
    paths: CbzRebuildTransactionPaths,
    completed: T,
) -> Result<CbzRebuildCommitted<T>, CbzRebuildTransactionFailure> {
    validate_preconditions(&paths)?;

    if let Err(source) = fs::rename(&paths.input, &paths.backup) {
        let primary_error =
            CbzRebuildOperationError::io(CbzRebuildTransactionStage::RenameInputToBackup, source);
        let artifacts = snapshot_artifacts(&paths);
        return if artifacts.input == CbzRebuildArtifactPresence::Present
            && artifacts.backup == CbzRebuildArtifactPresence::Absent
        {
            Err(CbzRebuildTransactionFailure::NotCommitted {
                stage: CbzRebuildTransactionStage::RenameInputToBackup,
                primary_error,
                context: failure_context(&paths, artifacts),
            })
        } else {
            Err(CbzRebuildTransactionFailure::RecoveryRequired {
                commit_state: CbzRebuildCommitState::NotCommitted,
                primary_error,
                recovery_error: None,
                context: failure_context(&paths, artifacts),
            })
        };
    }

    if let Err(source) = fs::rename(&paths.temp, &paths.output) {
        let primary_error =
            CbzRebuildOperationError::io(CbzRebuildTransactionStage::RenameTempToOutput, source);
        return match fs::rename(&paths.backup, &paths.input) {
            Ok(()) => {
                let artifacts = snapshot_artifacts(&paths);
                if artifacts.input == CbzRebuildArtifactPresence::Present
                    && artifacts.backup == CbzRebuildArtifactPresence::Absent
                {
                    Err(CbzRebuildTransactionFailure::RolledBack {
                        primary_error,
                        context: failure_context(&paths, artifacts),
                    })
                } else {
                    Err(CbzRebuildTransactionFailure::RecoveryRequired {
                        commit_state: CbzRebuildCommitState::NotCommitted,
                        primary_error,
                        recovery_error: None,
                        context: failure_context(&paths, artifacts),
                    })
                }
            }
            Err(source) => {
                let recovery_error = CbzRebuildOperationError::io(
                    CbzRebuildTransactionStage::RollbackBackupToInput,
                    source,
                );
                Err(CbzRebuildTransactionFailure::RecoveryRequired {
                    commit_state: CbzRebuildCommitState::NotCommitted,
                    primary_error,
                    recovery_error: Some(recovery_error),
                    context: failure_context(&paths, snapshot_artifacts(&paths)),
                })
            }
        };
    }

    let mut cleanup_warnings = Vec::new();
    if let Err(source) = fs::remove_file(&paths.backup) {
        cleanup_warnings.push(CbzRebuildCleanupIssue {
            operation: CbzRebuildCleanupOperation::RemoveBackup,
            path: paths.backup.clone(),
            source,
        });
    }

    Ok(CbzRebuildCommitted {
        completed,
        commit_state: CbzRebuildCommitState::Committed,
        paths,
        cleanup_warnings,
    })
}

fn validate_preconditions(
    paths: &CbzRebuildTransactionPaths,
) -> Result<(), CbzRebuildTransactionFailure> {
    let artifacts = snapshot_artifacts(paths);
    let failed = if artifacts.input != CbzRebuildArtifactPresence::Present {
        Some((CbzRebuildPrecondition::InputPresent, paths.input.clone()))
    } else if artifacts.temp != CbzRebuildArtifactPresence::Present {
        Some((CbzRebuildPrecondition::TempPresent, paths.temp.clone()))
    } else if artifacts.backup != CbzRebuildArtifactPresence::Absent {
        Some((CbzRebuildPrecondition::BackupAbsent, paths.backup.clone()))
    } else if paths.output != paths.input && artifacts.output != CbzRebuildArtifactPresence::Absent
    {
        Some((
            CbzRebuildPrecondition::DistinctOutputAbsent,
            paths.output.clone(),
        ))
    } else {
        None
    };

    let Some((condition, path)) = failed else {
        return Ok(());
    };
    let primary_error = CbzRebuildOperationError::precondition(condition, path);
    Err(CbzRebuildTransactionFailure::NotCommitted {
        stage: CbzRebuildTransactionStage::ValidateBeforeCommit,
        primary_error,
        context: failure_context(paths, artifacts),
    })
}

fn failure_context(
    paths: &CbzRebuildTransactionPaths,
    artifacts: CbzRebuildArtifactSnapshot,
) -> Box<CbzRebuildFailureContext> {
    Box::new(CbzRebuildFailureContext {
        paths: paths.clone(),
        artifacts,
    })
}

fn fmt_operation_with_paths(
    f: &mut fmt::Formatter<'_>,
    error: &CbzRebuildOperationError,
    paths: &CbzRebuildTransactionPaths,
) -> fmt::Result {
    match error.stage {
        CbzRebuildTransactionStage::RenameInputToBackup => write!(
            f,
            "cbz rebuild finalize failed at old->backup rename: {} -> {}: {}",
            paths.input.display(),
            paths.backup.display(),
            error.source
        ),
        CbzRebuildTransactionStage::RenameTempToOutput => write!(
            f,
            "cbz rebuild finalize failed at tmp->output rename: {} -> {}: {}",
            paths.temp.display(),
            paths.output.display(),
            error.source
        ),
        CbzRebuildTransactionStage::RollbackBackupToInput => write!(
            f,
            "cbz rebuild finalize failed during rollback backup->old rename: {} -> {}: {}",
            paths.backup.display(),
            paths.input.display(),
            error.source
        ),
        CbzRebuildTransactionStage::RemoveBackup => write!(
            f,
            "cbz rebuild finalize failed after output commit at backup delete: {}: {}",
            paths.backup.display(),
            error.source
        ),
        CbzRebuildTransactionStage::Plan
        | CbzRebuildTransactionStage::SelectEntries
        | CbzRebuildTransactionStage::WriteTemp
        | CbzRebuildTransactionStage::ValidateBeforeCommit => write!(f, "{error}"),
    }
}

fn snapshot_artifacts(paths: &CbzRebuildTransactionPaths) -> CbzRebuildArtifactSnapshot {
    CbzRebuildArtifactSnapshot {
        input: artifact_presence(&paths.input),
        output: artifact_presence(&paths.output),
        temp: artifact_presence(&paths.temp),
        backup: artifact_presence(&paths.backup),
    }
}

fn artifact_presence(path: &Path) -> CbzRebuildArtifactPresence {
    match fs::symlink_metadata(path) {
        Ok(_) => CbzRebuildArtifactPresence::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => CbzRebuildArtifactPresence::Absent,
        Err(_) => CbzRebuildArtifactPresence::Unknown,
    }
}
