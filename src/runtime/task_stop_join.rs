//! A successful join of the frozen original monitor is a process-local proof.
use std::sync::Arc;

use anyhow::{ensure, Result};

use super::control_cancellation::ControlJoinLease;
use super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use crate::runtime_db::task_stop::TaskStopRecord;

pub(crate) struct TaskStopMonitorJoin {
    original: Arc<ControlJoinLease>,
    scope: super::resident_input_evidence::InputEvidenceScope,
    effect_seq: u64,
    command_nonce: String,
    intent_digest: String,
    generation_digest: String,
    _budget: Arc<BudgetViewGuard>,
}

impl TaskStopMonitorJoin {
    /// This does not cancel, resolve a new handle, or prove child/OS exit.
    pub(super) async fn join_original(
        record: &TaskStopRecord,
        original: Arc<ControlJoinLease>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        ensure!(
            record.intent.lease_id.as_deref() == Some(original.lease_id())
                && original.target_id() == record.intent.task.key.id
                && original.matches_task_generation(&record.ack_task),
            "TaskStop join differs from the frozen original lease/generation"
        );
        original.join().await?;
        let metadata_estimate_bytes = serialized_bytes(&(
            &record.intent.task.scope,
            record.effect_seq,
            record.prepared.command_nonce(),
            &record.intent_digest,
            &record.generation_digest,
        ))?
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("TaskStop join metadata overflow"))?;
        let budget = ledger.register_owned_bytes(
            &record.intent.task.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes,
            },
        )?;
        Ok(Self {
            original,
            scope: record.intent.task.scope.clone(),
            effect_seq: record.effect_seq,
            command_nonce: record.prepared.command_nonce().to_owned(),
            intent_digest: record.intent_digest.clone(),
            generation_digest: record.generation_digest.clone(),
            _budget: budget,
        })
    }

    pub(crate) fn matches(&self, record: &TaskStopRecord) -> bool {
        self.scope == record.intent.task.scope
            && self.effect_seq == record.effect_seq
            && self.command_nonce == record.prepared.command_nonce()
            && self.intent_digest == record.intent_digest
            && self.generation_digest == record.generation_digest
            && record.intent.lease_id.as_deref() == Some(self.original.lease_id())
    }
}

#[cfg(test)]
pub(crate) async fn join_fixture(
    record: &TaskStopRecord,
    wrong_lease: bool,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<TaskStopMonitorJoin> {
    let lease = ControlJoinLease::task_stop_fixture(
        &record.ack_task,
        if wrong_lease {
            "another-lease"
        } else {
            record.intent.lease_id.as_deref().unwrap_or("")
        },
    );
    lease.attach(tokio::spawn(async {}));
    TaskStopMonitorJoin::join_original(record, lease, ledger).await
}
