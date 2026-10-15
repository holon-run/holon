//! Initial Stop's exact readonly child is an observation, never a child writer.
use super::super::input_mutation_receipts::{InputMutationReceipt, OriginalInputMutationAttempt};
use super::*;
use crate::types::{AgentIdentityRecord, AgentRegistryStatus};

/// Shared by the original SQL gate and the named owner adapter.
pub(crate) fn validate_task_stop_readonly_child_tx(
    tx: &Transaction<'_>,
    child: &TaskStopChildTarget,
) -> Result<()> {
    let matches: bool = tx.query_row(
        "SELECT COALESCE((SELECT agent_id=?1 AND kind='child' AND visibility='private' AND ownership='parent_supervised' AND status IN ('active','deleting','deleted') AND parent_agent_id=?3 AND delegated_from_task_id=?4 AND json_extract(payload_json,'$.agent_id')=agent_id AND json_extract(payload_json,'$.kind')=kind AND json_extract(payload_json,'$.visibility')=visibility AND json_extract(payload_json,'$.ownership')=ownership AND json_extract(payload_json,'$.status')=status AND json_extract(payload_json,'$.parent_agent_id')=parent_agent_id AND json_extract(payload_json,'$.delegated_from_task_id')=delegated_from_task_id AND COALESCE(json_extract(payload_json,'$.incarnation'),1)=?2 AND (json_type(payload_json,'$.incarnation') IS NULL OR json_type(payload_json,'$.incarnation')='integer') FROM agent_identities WHERE agent_id=?1),0)",
        params![child.scope.agent_id,child.scope.identity_incarnation,child.parent_agent_id,child.creation_task_id],
        |row| row.get(0),
    )?;
    ensure!(
        matches,
        "TaskStop child is not the original identity/parent/creation Task"
    );
    Ok(())
}

#[derive(Debug)]
pub(crate) struct TaskStopReadonlyIdentity {
    target: TaskStopChildTarget,
    identity: Arc<AgentIdentityRecord>,
    state_present: Option<bool>,
    _material: Arc<BudgetViewGuard>,
}
impl TaskStopReadonlyIdentity {
    pub(crate) fn target(&self) -> &TaskStopChildTarget {
        &self.target
    }
    pub(crate) fn identity(&self) -> &Arc<AgentIdentityRecord> {
        &self.identity
    }
    pub(crate) fn identity_material(&self) -> Arc<BudgetViewGuard> {
        self._material.clone()
    }
    // None means retired: no successor/retired State query was made.
    pub(crate) fn state_present(&self) -> Option<bool> {
        self.state_present
    }
}

pub(crate) fn observe_task_stop_readonly_identity_tx(
    tx: &Transaction<'_>,
    child: &TaskStopChildTarget,
    request: &super::super::task_snapshot::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<TaskStopReadonlyIdentity> {
    let row_bytes:usize=tx.query_row("SELECT COALESCE(SUM(COALESCE(length(CAST(agent_id AS BLOB)),0)+COALESCE(length(CAST(status AS BLOB)),0)+COALESCE(length(CAST(payload_json AS BLOB)),0)),0) FROM agent_identities WHERE agent_id=?1",[&child.scope.agent_id],|r|r.get(0))?;
    let bytes = row_bytes
        .checked_add(usize::try_from(serialized_bytes(child)?)?)
        .and_then(|v| v.checked_mul(8))
        .context("readonly child material overflow")?;
    ensure!(
        bytes <= request.byte_limit.min(MAX_BYTES) && 8 <= request.record_limit.min(MAX_RECORDS),
        super::super::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "Stop readonly child identity"
        )
    );
    let guard = ledger.register_owned_bytes(
        &child.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    validate_task_stop_readonly_child_tx(tx, child)?;
    let raw: String = tx.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [&child.scope.agent_id],
        |r| r.get(0),
    )?;
    let identity = super::super::repositories::decode_agent_identity_payload(&raw)?;
    ensure!(
        identity.agent_id == child.scope.agent_id
            && identity.incarnation == child.scope.identity_incarnation,
        "readonly child decoded namespace differs"
    );
    let state_present = if matches!(
        identity.status,
        AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
    ) {
        Some(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id=?1)",
            [&child.scope.agent_id],
            |r| r.get(0),
        )?)
    } else {
        None
    };
    Ok(TaskStopReadonlyIdentity {
        target: child.clone(),
        identity: Arc::new(identity),
        state_present,
        _material: guard,
    })
}

/// Only an actual fresh initial whole Stop receipt can mint this path.
#[derive(Debug)]
pub(crate) struct InitialTaskStopReadonlyRecovery {
    attempt: OriginalInputMutationAttempt,
    child: TaskStopReadonlyIdentity,
    _source_material: Arc<BudgetViewGuard>,
}
impl InitialTaskStopReadonlyRecovery {
    pub(crate) fn matches_original(&self, attempt: &OriginalInputMutationAttempt) -> bool {
        self.attempt.same_attempt(attempt) && self.attempt.prepared() == attempt.prepared()
    }
    pub(crate) fn child(&self) -> &TaskStopReadonlyIdentity {
        &self.child
    }
}

pub(crate) fn observe_initial_task_stop_readonly_recovery_tx(
    tx: &Transaction<'_>,
    attempt: &OriginalInputMutationAttempt,
    receipt: &InputMutationReceipt,
    expected: &TaskStopChildTarget,
    request: &super::super::task_snapshot::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InitialTaskStopReadonlyRecovery> {
    ensure!(
        attempt.original_attempt_finished() && receipt.prepared == *attempt.prepared(),
        "readonly recovery requires the original finished whole receipt"
    );
    let InputMutationResult::Task {
        task_id,
        stop: Some(binding),
        deletion: None,
    } = &receipt.outcome.result
    else {
        anyhow::bail!("readonly recovery is not an initial Stop receipt");
    };
    let source = load_task_stop_by_sequence_tx(
        tx,
        &binding.scope,
        binding.effect_seq,
        request.byte_limit,
        ledger,
    )?;
    let record = source
        .records
        .first()
        .context("initial readonly Stop source missing")?;
    ensure!(
        record.prepared == receipt.prepared
            && record.receipt_binding() == *binding
            && record.intent.task.key.id == *task_id
            && record.intent.child.as_ref() == Some(expected)
            && receipt.prepared.scopes().len() == 2
            && receipt
                .prepared
                .scopes()
                .iter()
                .any(|s| s == &expected.scope),
        "readonly recovery differs from the original Stop/child group"
    );
    let outcome = receipt
        .outcome
        .scopes
        .iter()
        .find(|v| v.scope == expected.scope)
        .context("readonly child original outcome missing")?;
    ensure!(
        outcome.install_keys == Default::default()
            && outcome.audit_events.is_empty()
            && !outcome.notify_memory_index
            && !outcome.notify_scheduler,
        "initial readonly child has mutation effects"
    );
    let child = observe_task_stop_readonly_identity_tx(tx, expected, request, ledger)?;
    Ok(InitialTaskStopReadonlyRecovery {
        attempt: attempt.clone(),
        child,
        _source_material: source._budget.clone(),
    })
}
