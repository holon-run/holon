//! Metadata-only original Message ACKs refresh already observed Task heads.
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_input_evidence::{CommittedInputEvidenceDelta, PreparedInputEvidence},
};

/// Only the enclosing transaction without another Task finalizer uses this API.
/// It preserves all Task/Stop bodies, selectors and negative observations.
pub(crate) fn register_task_source_metadata_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    input: &CommittedInputEvidenceDelta,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut super::super::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Option<(CommittedTaskDelta, Vec<Arc<BudgetViewGuard>>)>> {
    if !input
        .records
        .iter()
        .any(|record| matches!(record.record, PreparedInputEvidence::Message(_)))
        && !input
            .removals
            .iter()
            .any(|removed| removed.key.family == InputEvidenceFamily::Message)
        && input.output_recall_invalidation.is_none()
    {
        return Ok(None);
    }
    if let Some(invalidation) = &input.output_recall_invalidation {
        ensure!(
            invalidation.belongs_to(scope),
            "focused Output prune crossed scope"
        );
    }
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(ledger))
        .with_memory_budget(memory);
    let Some(mut coverage) = material.read_current_metadata(tx)? else {
        return Ok(None);
    };
    if coverage.output.window.is_none() {
        return Ok(None);
    }
    let identity_size:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(status AS BLOB))+length(CAST(agent_id AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|row|row.get(0))?;
    material.charge_ids(
        identity_size
            .checked_add(scope.agent_id.len() + 256)
            .context("Task source scope bytes overflow")?,
        1,
    )?;
    maintenance::require_existing_maintenance_scope_tx(tx, scope)?;
    let has_state: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id=?1)",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    if !has_state {
        // Existing NoState actors keep TaskNone; this metadata helper cannot
        // acquire a Current domain or install its historical Task/Stop bodies.
        return Ok(None);
    }
    for record in &input.records {
        if let PreparedInputEvidence::Message(message) = &record.record {
            ensure!(
                record.locator.scope == *scope
                    && record.locator.key
                        == InputEvidenceKey::new(InputEvidenceFamily::Message, &message.id),
                "Task source ACK crossed scope/key"
            );
            let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3 AND availability='present' AND revision=?4 AND content_digest=?5)",params![scope.agent_id,scope.identity_incarnation,message.id,record.locator.revision,record.locator.content_digest],|row|row.get(0))?;
            ensure!(exact, "Task source ACK differs from its actual Input alias");
        }
    }
    for removed in &input.removals {
        if removed.key.family == InputEvidenceFamily::Message {
            ensure!(removed.scope == *scope, "Task source removal crossed scope");
            let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3 AND availability='pruned' AND revision=?4)",params![scope.agent_id,scope.identity_incarnation,removed.key.id,removed.revision],|row|row.get(0))?;
            ensure!(exact, "Task source removal lacks its actual pruning ACK");
        }
    }
    let base_revision = coverage.revision;
    material.charge_ids(
        coverage
            .output
            .heads
            .len()
            .checked_mul(std::mem::size_of::<&TaskFactLocator>())
            .context("Task source selector slots overflow")?,
        coverage.output.heads.len(),
    )?;
    let tasks = coverage
        .output
        .heads
        .values()
        .map(|head| &head.task)
        .collect::<Vec<_>>();
    for task in &tasks {
        ensure!(
            task.scope == *scope
                && coverage.observations.get(&task.key) == Some(&TaskAvailability::Present),
            "Task source head lacks original Task observation"
        );
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3 AND availability='present' AND revision=?4 AND content_digest=?5 AND projection_digest=?6)",params![scope.agent_id,scope.identity_incarnation,task.key.id,task.revision,task.content_digest,task.projection_digest],|row|row.get(0))?;
        ensure!(exact, "Task source head has a changed qualified Task alias");
    }
    let output = query_sources::observe_output_heads_tx(tx, scope, &tasks, |bytes, count| {
        material.charge_ids(bytes, count)
    })?;
    if output == coverage.output {
        return Ok(None);
    }
    coverage.output = output;
    coverage.revision = base_revision
        .checked_add(1)
        .context("Task source revision overflow")?;
    material.charge_final_metadata(
        read_preparation::encoded_metadata_bytes(&coverage)?,
        coverage.owned_metadata_record_count()?,
    )?;
    loader::persist_coverage(tx, scope, &coverage)?;
    Ok(Some((
        CommittedTaskDelta {
            scope: scope.clone(),
            base_revision,
            revision: coverage.revision,
            coverage,
            records: Vec::new(),
            stop_records: Vec::new(),
            removals: BTreeMap::new(),
        },
        material.into_guards(),
    )))
}
