use crate::domain::archive::{CbzRebuildPlan, CbzRebuildPlanError};

use super::{
    CbzRebuildArchiveSelection,
    cbz_rebuild_transaction::{
        CbzRebuildCommitted, CbzRebuildTransactionFailure, CbzRebuildTransactionPaths,
        finalize_cbz_rebuild_transaction,
    },
    collect_cbz_rebuild_archive_selection, write_cbz_rebuild_tmp_archive,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CbzRebuildPreparedTmp {
    pub plan: CbzRebuildPlan,
    pub selection: CbzRebuildArchiveSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CbzRebuildCompleted {
    pub plan: CbzRebuildPlan,
    pub selection: CbzRebuildArchiveSelection,
}

pub(crate) fn prepare_cbz_rebuild_tmp(
    plan: CbzRebuildPlan,
) -> anyhow::Result<CbzRebuildPreparedTmp> {
    validate_cbz_rebuild_plan_paths(&plan).map_err(anyhow::Error::from)?;
    let selection = collect_cbz_rebuild_archive_selection(&plan.input_path, &plan.delete_entries)?;
    write_cbz_rebuild_tmp_archive(&plan.input_path, &plan.tmp_path, &selection)?;
    Ok(CbzRebuildPreparedTmp { plan, selection })
}

pub(crate) fn finalize_cbz_rebuild_prepared_tmp(
    prepared: CbzRebuildPreparedTmp,
) -> Result<CbzRebuildCommitted<CbzRebuildCompleted>, CbzRebuildTransactionFailure> {
    let paths = CbzRebuildTransactionPaths {
        input: prepared.plan.input_path.clone(),
        output: prepared.plan.output_path.clone(),
        temp: prepared.plan.tmp_path.clone(),
        backup: prepared.plan.backup_path.clone(),
    };
    let completed = CbzRebuildCompleted {
        plan: prepared.plan,
        selection: prepared.selection,
    };
    finalize_cbz_rebuild_transaction(paths, completed)
}

fn validate_cbz_rebuild_plan_paths(plan: &CbzRebuildPlan) -> Result<(), CbzRebuildPlanError> {
    if plan.output_path != plan.input_path && plan.output_path.exists() {
        return Err(CbzRebuildPlanError::OutputPathAlreadyExists(
            plan.output_path.clone(),
        ));
    }
    if plan.tmp_path.exists() {
        return Err(CbzRebuildPlanError::TmpPathAlreadyExists(
            plan.tmp_path.clone(),
        ));
    }
    if plan.backup_path.exists() {
        return Err(CbzRebuildPlanError::BackupPathAlreadyExists(
            plan.backup_path.clone(),
        ));
    }
    Ok(())
}
