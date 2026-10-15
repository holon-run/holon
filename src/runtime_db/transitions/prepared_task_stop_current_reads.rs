//! Exact original Stop Result sources join Queue/Audit in its own business ACK.
use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadKey, CurrentReadRequest},
    input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate},
};
use crate::runtime_db::current_read_snapshot::{self as reads, CurrentReadBusinessTransaction};

pub(super) fn prepare_result_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    source: &crate::runtime_db::task_stop::OriginalTaskStopResultView<'_>,
    before_task: &TaskRecord,
    before: &AgentState,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<CurrentReadBusinessTransaction<'tx, 'conn>> {
    let scope = &source.descriptor().scopes()[0];
    source.validate_tx(tx, scope, before_task)?;
    let limits = ledger.limits();
    let request = CurrentReadRequest::current(
        usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("Stop Current allowance overflow")?,
        )?,
        usize::try_from(limits.record_limit)?,
    );
    let id = &source.message().id;
    let bytes = id
        .len()
        .checked_mul(3)
        .and_then(|n| n.checked_add(scope.agent_id.len()))
        .and_then(|n| n.checked_add(2048))
        .and_then(|n| n.checked_mul(8))
        .context("Stop Current keys overflow")?;
    let records = 64;
    request.check_material(bytes, records)?;
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(bytes)?,
        },
    )?;
    let queue = CurrentReadKey::queue(id);
    let selected = reads::qualified_existing_cleanup_tx(
        tx,
        scope,
        &BTreeSet::from([queue.clone()]),
        &request,
        ledger,
        (bytes, records),
        vec![guard],
    )?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1)",
        [id],
        |r| r.get(0),
    )?;
    let mut keys = selected.keys;
    if !exists {
        keys.insert(queue);
    }
    keys.insert(CurrentReadKey::event(wait_trigger_audit_id(
        id,
        WaitTriggerAuditKind::Triggered,
    )));
    keys.insert(CurrentReadKey::event(wait_trigger_audit_id(
        id,
        WaitTriggerAuditKind::Stale,
    )));
    let mut business = CurrentReadBusinessTransaction::prepare_original_maintenance_tx(
        tx,
        scope,
        before,
        &keys,
        &request,
        ledger,
        selected.material,
        selected.guards,
    )?;
    business.observe_original_stop_queue_tx(tx, source)?;
    Ok(business)
}
pub(super) fn finish_result(
    current: CurrentReadBusinessTransaction<'_, '_>,
    value: &mut TransitionCommit,
) -> Result<()> {
    let state = value
        .effects
        .agent_state
        .as_ref()
        .context("Stop delivered Result omitted actual State")?
        .record
        .as_ref();
    value.effects.current_read_maintenance =
        Some(Arc::new(current.finish_original_maintenance(state)?));
    Ok(())
}
