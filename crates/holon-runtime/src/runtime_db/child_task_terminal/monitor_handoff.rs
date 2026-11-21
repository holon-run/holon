//! Read-only transfer to the exact durable Stop; local cancellation is not itself an ACK.
use super::OriginalChildTaskAddress;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    OriginalChildTaskProducer,
};
use crate::runtime_db::{
    command_task_terminal as owner, task_snapshot as tasks,
    task_stop::{self, TaskStopSignal},
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use std::sync::Arc;
pub(crate) struct OriginalChildMonitorStopHandoff {
    original: OriginalChildTaskAddress,
    _effect_seq: u64,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl OriginalChildMonitorStopHandoff {
    pub(crate) fn matches_original(&self, source: &OriginalChildTaskProducer) -> bool {
        &self.original == source.address()
    }
}
pub(crate) fn observe_original_child_monitor_stop_handoff_tx(
    tx: &Transaction<'_>,
    source: &OriginalChildTaskProducer,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<OriginalChildMonitorStopHandoff>> {
    source.validate_material_ledger(ledger)?;
    source.validate_original_monitor_cancellation()?;
    let scope = &source.address().scope;
    let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    let mut guards = Vec::new();
    fn charge(
        phase: &mut crate::runtime_db::task_status_metadata::PhaseMaterial<'_>,
        guards: &mut Vec<Arc<BudgetViewGuard>>,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        bytes: usize,
        records: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        let guard = owner::charge_material(scope, bytes, records, &phase.remaining()?, ledger)?;
        phase.include(bytes, records)?;
        guards.push(guard.clone());
        Ok(guard)
    }
    // No JSON parse or owned selector precedes the real raw candidate guard.
    let (bytes,count):(usize,usize)=tx.query_row("SELECT COALESCE(SUM(length(CAST(intent_json AS BLOB))+length(CAST(task_id AS BLOB))+256),0),COUNT(*) FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3",params![scope.agent_id,scope.identity_incarnation,source.address().admitted.key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    charge(
        &mut phase,
        &mut guards,
        scope,
        ledger,
        bytes
            .checked_mul(8)
            .context("child Stop selector bytes overflow")?,
        count
            .checked_mul(8)
            .context("child Stop selector count overflow")?,
    )?;
    let nodes:usize=tx.query_row("SELECT COUNT(*) FROM task_stop_effects s,json_tree(s.intent_json) WHERE s.agent_id=?1 AND s.identity_incarnation=?2 AND s.task_id=?3",params![scope.agent_id,scope.identity_incarnation,source.address().admitted.key.id],|r|r.get(0))?;
    charge(
        &mut phase,
        &mut guards,
        scope,
        ledger,
        nodes
            .checked_mul(8 * 256)
            .context("child Stop selector nodes overflow")?,
        nodes
            .checked_mul(8)
            .context("child Stop selector node count overflow")?,
    )?;
    let selected:Option<u64>=tx.query_row("SELECT MAX(effect_seq) FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3 AND json_extract(intent_json,'$.lease_id')=?4 AND json_extract(intent_json,'$.owner_epoch')=?5",params![scope.agent_id,scope.identity_incarnation,source.address().admitted.key.id,source.address().lease_id,source.address().owner_epoch],|r|r.get(0)).optional()?.flatten();
    let Some(seq) = selected else { return Ok(None) };
    let mut receipt_phase = false;
    let records = task_stop::observe_task_stops_tx(
        tx,
        scope,
        Some(seq),
        false,
        |raw, count| {
            let guard = charge(
                &mut phase,
                &mut guards,
                scope,
                ledger,
                raw.checked_add(
                    count
                        .checked_mul(512)
                        .context("child Stop row metadata overflow")?,
                )
                .and_then(|n| n.checked_mul(8))
                .context("child Stop raw material overflow")?,
                count
                    .checked_mul(32 * 8)
                    .context("child Stop row count overflow")?,
            )?;
            // Both independently decoded whole receipts and every full Stop JSON
            // column share this same remaining allowance before the common loader.
            let native: usize = if receipt_phase {
                tx.query_row("SELECT
              (SELECT COUNT(*) FROM task_stop_effects s JOIN input_mutation_receipts r ON r.command_nonce=json_extract(s.intent_json,'$.result.phase_nonce'),json_tree(r.prepared_json) WHERE s.effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s JOIN input_mutation_receipts r ON r.command_nonce=json_extract(s.intent_json,'$.result.phase_nonce'),json_tree(r.outcome_json) WHERE s.effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s JOIN input_mutation_receipts r ON r.command_nonce=json_extract(s.intent_json,'$.result.phase_nonce'),json_tree(r.scopes_json) WHERE s.effect_seq=?1)",[seq],|r|r.get(0))?
            } else {
                receipt_phase = true;
                tx.query_row("SELECT
              (SELECT COUNT(*) FROM task_stop_effects s,json_tree(s.prepared_json) WHERE effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s,json_tree(s.intent_json) WHERE effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s,json_tree(s.progress_json) WHERE effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s,json_tree(s.ack_task_json) WHERE effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s JOIN input_mutation_receipts r ON r.command_nonce=s.command_nonce,json_tree(r.prepared_json) WHERE s.effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s JOIN input_mutation_receipts r ON r.command_nonce=s.command_nonce,json_tree(r.outcome_json) WHERE s.effect_seq=?1)
             +(SELECT COUNT(*) FROM task_stop_effects s JOIN input_mutation_receipts r ON r.command_nonce=s.command_nonce,json_tree(r.scopes_json) WHERE s.effect_seq=?1)",[seq],|r|r.get(0))?
            };
            charge(
                &mut phase,
                &mut guards,
                scope,
                ledger,
                native
                    .checked_mul(8 * 256)
                    .context("child Stop native material overflow")?,
                native
                    .checked_mul(8)
                    .context("child Stop native count overflow")?,
            )?;
            Ok(guard)
        },
        None,
    )?;
    let record = records
        .records
        .first()
        .context("original child Stop effect vanished")?;
    let view = source.source_view()?;
    // The sealed actual ACK preserves the binding present when Stop froze.
    // The pre-spawn admitted Task is only the original generation source.
    record.intent.validate_original(&record.ack_task)?;
    ensure!(
        record.intent.task.scope == *scope
            && record.intent.task.key == source.address().admitted.key
            && record.intent.owner_epoch == source.address().owner_epoch
            && record.intent.lease_id.as_deref() == Some(source.address().lease_id.as_str())
            && record.intent.signal == TaskStopSignal::Abort
            && record.ack_task.status == crate::types::TaskStatus::Cancelled,
        "original child monitor Stop handoff differs from its exact lease/generation"
    );
    view.validate_generation(&record.ack_task)?;
    drop(view);
    if !source.original_stop_covers_actual_monitor_facets(
        &record.intent,
        &record.ack_task,
        &record.progress,
    )? {
        // A legitimate older Stop may know less than the actual spawned stage.
        // Keep the sole source responsibility until a complete durable handoff.
        return Ok(None);
    }
    charge(
        &mut phase,
        &mut guards,
        scope,
        ledger,
        usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
            source.address(),
        )?)?
        .checked_add(256)
        .and_then(|n| n.checked_mul(8))
        .context("child Stop handoff address overflow")?,
        8,
    )?;
    Ok(Some(OriginalChildMonitorStopHandoff {
        original: source.address().clone(),
        _effect_seq: seq,
        _guards: guards,
    }))
}
