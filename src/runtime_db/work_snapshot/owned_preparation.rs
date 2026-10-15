//! Explicit by-ID Work source observation, with no alias or Ready installation.
use super::read_preparation::WorkReadMaterial;
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use crate::types::WorkItemRecord;

pub(crate) enum WorkItemSourceRead {
    Missing,
    Foreign,
    Present(WorkItemRecord, Vec<Arc<BudgetViewGuard>>),
}
/// The ordinary resident hit never enters this named hydration boundary.
/// Deleting can be observed for an original completion; mutation admission is
/// still checked by its existing command proof and final transaction.
pub(crate) fn read_owned_work_item_source_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    id: &str,
    request: &WorkDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<WorkItemSourceRead> {
    let owner: Option<bool> = tx
        .query_row(
            "SELECT agent_id=?2 FROM work_items WHERE work_item_id=?1",
            params![id, scope.agent_id],
            |row| row.get(0),
        )
        .optional()?;
    match owner {
        None => return Ok(WorkItemSourceRead::Missing),
        Some(false) => return Ok(WorkItemSourceRead::Foreign),
        Some(true) => {}
    }
    let mut material = WorkReadMaterial::new(scope, request, ledger, None)?;
    material.input_ids([id])?;
    let key = WorkKey::new(WorkFamily::Work, id);
    let facts = material
        .scope_tx(tx, true)?
        .ok_or(WorkSnapshotError::RetiredScope)?;
    ensure!(
        facts.state.is_some(),
        WorkSnapshotError::NeedsPreparation(key.clone())
    );
    let seed = material.binding_tx(tx)?;
    if let Some(alias) = material.alias_tx(tx, &key)? {
        ensure!(
            alias.availability == WorkAvailability::Present,
            WorkSnapshotError::NeedsPreparation(key.clone())
        );
    } else {
        ensure!(
            material.eligible_tx(tx, seed.as_deref(), &key)?.is_some(),
            WorkSnapshotError::NeedsPreparation(key.clone())
        );
    }
    let stored = material
        .record_tx(tx, &key)?
        .context("observed Work source disappeared")?;
    material.validate_member_tx(tx, seed.as_deref(), &key, &stored)?;
    let CanonicalWorkRecord::Work(record) = stored.record else {
        anyhow::bail!("Work source family differs")
    };
    Ok(WorkItemSourceRead::Present(record, material.into_guards()))
}
