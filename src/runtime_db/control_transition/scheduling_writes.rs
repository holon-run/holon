//! Scheduling facts share Control's existing SQL, receipt and cancellation ACK.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
    resident_scheduling::{SchedulingFamily, SchedulingKey},
};
use crate::runtime_db::scheduling_snapshot::{self as scheduling, SchedulingDomainLoadRequest};

pub(super) struct ControlSchedulingWrites<'tx, 'conn> {
    proof: scheduling::ExistingSchedulingMaintenanceProof<'tx, 'conn>,
    request: SchedulingDomainLoadRequest,
    actual: BTreeSet<SchedulingKey>,
    qualified: BTreeSet<SchedulingKey>,
    guards: Vec<Arc<BudgetViewGuard>>,
}
pub(super) struct ControlSchedulingFacts {
    pub(super) facts: scheduling::CommittedSchedulingFacts,
    pub(super) actual: BTreeSet<SchedulingKey>,
}
pub(super) fn prepare_control_scheduling_writes_tx<'tx, 'conn>(
    tx: &'tx Transaction<'conn>,
    command: &PreparedControl,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ControlSchedulingWrites<'tx, 'conn>> {
    let scope = InputEvidenceScope {
        agent_id: command.key.agent_id.clone(),
        identity_incarnation: command.key.identity_incarnation,
    };
    // The original complete Control OCC snapshot may discover a concurrent
    // target; bound its material before that snapshot decodes any new body.
    let (count, bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(material_bytes),0) FROM (
           SELECT length(CAST(payload_json AS BLOB))+256 AS material_bytes FROM timers WHERE agent_id=?1 AND status IN ('active','scheduled')
           UNION ALL SELECT length(CAST(payload_json AS BLOB))+256 FROM external_triggers WHERE target_agent_id=?1 AND status='active'
           UNION ALL SELECT length(CAST(w.timer_id AS BLOB))+length(CAST(w.message_id AS BLOB))+length(CAST(w.status AS BLOB))+length(CAST(w.created_at AS BLOB))+length(CAST(w.updated_at AS BLOB))+COALESCE(length(CAST(w.incorporated_at AS BLOB)),0)+COALESCE(length(CAST(w.cancelled_at AS BLOB)),0)+256 FROM timer_wakes w JOIN timers t ON t.timer_id=w.timer_id WHERE t.agent_id=?1 AND w.status='pending'
         )", [&scope.agent_id], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    let frozen = usize::try_from(serialized_bytes(&(
        &scope,
        &command.expected.timers,
        &command.expected.pending_timer_wakes,
        &command.expected.external_triggers,
        &request.capture.required_keys,
    ))?)?;
    let bytes = bytes
        .checked_add(frozen)
        .and_then(|n| n.checked_add(256))
        .and_then(|n| n.checked_mul(8))
        .context("Control Scheduling bytes overflow")?;
    let records = count
        .checked_add(command.expected.timers.len())
        .and_then(|n| n.checked_add(command.expected.pending_timer_wakes.len()))
        .and_then(|n| n.checked_add(command.expected.external_triggers.len()))
        .and_then(|n| n.checked_add(request.capture.required_keys.len()))
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(8))
        .context("Control Scheduling count overflow")?;
    let remaining_bytes = request
        .byte_limit
        .min(64 * 1024 * 1024)
        .checked_sub(bytes)
        .filter(|n| *n > 0)
        .ok_or(scheduling::SchedulingSnapshotError::ResourceExhausted(
            "Control original Scheduling snapshot",
        ))?;
    let remaining_records = request
        .record_limit
        .min(65_536)
        .checked_sub(records)
        .filter(|n| *n > 0)
        .ok_or(scheduling::SchedulingSnapshotError::ResourceExhausted(
            "Control original Scheduling snapshot",
        ))?;
    let guard = ledger.register_owned_bytes(
        &scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let request = SchedulingDomainLoadRequest {
        capture: request.capture.clone(),
        byte_limit: remaining_bytes,
        record_limit: remaining_records,
    };
    let mut keys = BTreeSet::new();
    keys.extend(
        command
            .expected
            .timers
            .iter()
            .map(|t| SchedulingKey::new(SchedulingFamily::Timer, &t.id)),
    );
    for wake in &command.expected.pending_timer_wakes {
        keys.insert(SchedulingKey::new(SchedulingFamily::Timer, &wake.timer_id));
        keys.insert(SchedulingKey::wake(&wake.timer_id, &wake.message_id));
    }
    keys.extend(
        command
            .expected
            .external_triggers
            .iter()
            .map(|t| SchedulingKey::new(SchedulingFamily::ExternalTrigger, &t.external_trigger_id)),
    );
    let proof = scheduling::prepare_existing_scheduling_maintenance_tx(
        tx, &scope, &keys, &request, ledger,
    )?;
    let qualified = proof.qualified_keys().cloned().collect();
    Ok(ControlSchedulingWrites {
        proof,
        request,
        actual: BTreeSet::new(),
        qualified,
        guards: vec![guard],
    })
}
impl ControlSchedulingWrites<'_, '_> {
    pub(super) fn timer_written(&mut self, id: &str) {
        self.actual
            .insert(SchedulingKey::new(SchedulingFamily::Timer, id));
    }
    pub(super) fn wake_written(&mut self, timer: &str, message: &str) {
        self.actual.insert(SchedulingKey::wake(timer, message));
    }
    pub(super) fn trigger_written(&mut self, id: &str) {
        self.actual
            .insert(SchedulingKey::new(SchedulingFamily::ExternalTrigger, id));
    }
    pub(super) fn finish(
        self,
        tx: &Transaction<'_>,
        state: &AgentState,
    ) -> Result<ControlSchedulingFacts> {
        let actual = self.actual.intersection(&self.qualified).cloned().collect();
        let mut facts = scheduling::register_existing_scheduling_cleanup_tx(
            tx,
            self.proof,
            &self.actual,
            state,
            &self.request,
        )?;
        // Move the complete original snapshot carrier through the whole ACK.
        facts.append_budget_views(self.guards);
        Ok(ControlSchedulingFacts { facts, actual })
    }
}
