//! Only the original metadata phase may replace heads in an already held Task view.
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use crate::runtime_db::{
    input_mutation_receipts::InputMutationResult,
    task_status_metadata::{TaskStatusMetadataInstallMode, TaskStatusMetadataReadView},
};

pub(crate) fn read_task_status_metadata_recovery_tx(
    tx: &Transaction<'_>,
    view: &TaskStatusMetadataReadView<'_, '_>,
    scope: &InputEvidenceScope,
    before: &TaskCoverage,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    storage_replayed: bool,
) -> Result<(CommittedTaskDelta, Vec<Arc<BudgetViewGuard>>)> {
    view.validate_tx(tx, scope)?;
    ensure!(
        view.mode() == TaskStatusMetadataInstallMode::Current,
        "Status recovery requires its current original scope"
    );
    let InputMutationResult::TaskStatusMetadata { ack } = &view.receipt().outcome.result else {
        anyhow::bail!("Status recovery result missing")
    };
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(ledger));
    let delta = read_status_metadata_using_material_tx(
        tx,
        scope,
        before,
        ack.task_revision,
        storage_replayed,
        &mut material,
    )?;
    Ok((delta, material.into_guards()))
}

/// Business-neutral metadata readback shared only by the two sealed source actors.
/// Each caller proves its actual fresh TX, original receipt and related scope first.
pub(in crate::runtime_db) fn read_status_metadata_using_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    before: &TaskCoverage,
    expected_revision: Option<u64>,
    storage_replayed: bool,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<CommittedTaskDelta> {
    let current = material
        .read_current_metadata_for_scope(tx, scope)?
        .context("original Status Task view disappeared")?;
    if storage_replayed && current == *before {
        return Ok(CommittedTaskDelta {
            scope: scope.clone(),
            base_revision: before.revision,
            revision: before.revision,
            coverage: current,
            records: Vec::new(),
            stop_records: Vec::new(),
            removals: BTreeMap::new(),
        });
    }
    ensure!(
        current.revision == expected_revision.unwrap_or(before.revision),
        "Status recovery current revision differs from its original ACK"
    );
    // The original phase changes no records, sources, selectors or relation sets.
    // Native metadata was fully admitted before these bounded validation copies.
    let TaskCoverage {
        revision: _,
        status: _,
        active_complete,
        unsettled_complete,
        active_task_ids,
        unsettled_result_ids,
        task_metadata,
        result_metadata,
        result_dependencies,
        task_result_ids,
        activation_result_ids,
        reply_result_ids,
        required_keys,
        observations,
        terminal,
        stop,
        output,
        query,
        running,
    } = before;
    ensure!(
        *active_complete == current.active_complete
            && *unsettled_complete == current.unsettled_complete
            && active_task_ids == &current.active_task_ids
            && unsettled_result_ids == &current.unsettled_result_ids
            && task_metadata == &current.task_metadata
            && result_metadata == &current.result_metadata
            && result_dependencies == &current.result_dependencies
            && task_result_ids == &current.task_result_ids
            && activation_result_ids == &current.activation_result_ids
            && reply_result_ids == &current.reply_result_ids
            && required_keys == &current.required_keys
            && observations == &current.observations
            && terminal == &current.terminal
            && stop == &current.stop
            && output == &current.output
            && query == &current.query
            && running == &current.running,
        "Status recovery changed domains outside its original heads"
    );
    for (key, availability) in &before.observations {
        if *availability != TaskAvailability::Present {
            continue;
        }
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4 AND availability='present')",params![scope.agent_id,scope.identity_incarnation,family_name(key.family),key.id],|r|r.get(0))?;
        ensure!(exact, "Status recovery original Task alias disappeared");
    }
    Ok(CommittedTaskDelta {
        scope: scope.clone(),
        base_revision: before.revision,
        revision: current.revision,
        coverage: current,
        records: Vec::new(),
        stop_records: Vec::new(),
        removals: BTreeMap::new(),
    })
}
