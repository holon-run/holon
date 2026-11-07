//! Sealed positive sparse ACK evidence from the original finished attempt's fresh view.
use super::*;
use crate::runtime_db::input_mutation_receipts::{
    with_fresh_input_mutation_preparation_and_preflight, InputMutationReceipt,
    InputMutationResolution, OriginalInputMutationAttempt,
};
use crate::types::AgentIdentityRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskStopSparseStatePresence {
    Absent,
    PresentUnprepared,
    NotReadRetired,
}

/// No Clone/Serialize/Deserialize or public construction. This evidence grants
/// only explicit unprepared metadata resolution, never State/Task/exit authority.
#[derive(Debug)]
pub(crate) struct TaskStopSparseRecoveryEvidence {
    attempt: OriginalInputMutationAttempt,
    receipt: InputMutationReceipt,
    identity: Option<AgentIdentityRecord>,
    mode: TaskStopPhaseInstallMode,
    state_presence: TaskStopSparseStatePresence,
    _budget_views: Vec<Arc<BudgetViewGuard>>,
}
impl TaskStopSparseRecoveryEvidence {
    pub(crate) fn matches_original(&self, attempt: &OriginalInputMutationAttempt) -> bool {
        self.attempt.same_attempt(attempt) && self.receipt.prepared == *attempt.prepared()
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        &self.receipt
    }
    pub(crate) fn identity(&self) -> Option<&AgentIdentityRecord> {
        self.identity.as_ref()
    }
    pub(crate) fn mode(&self) -> TaskStopPhaseInstallMode {
        self.mode
    }
    pub(crate) fn state_presence(&self) -> TaskStopSparseStatePresence {
        self.state_presence
    }
}
/// This borrow exists only inside the authenticated fresh-view callback. It is
/// neither a Ready proof nor permission to read a successor State.
pub(crate) struct TaskStopSparseReadView<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    evidence: &'tx TaskStopSparseRecoveryEvidence,
}
impl TaskStopSparseReadView<'_, '_> {
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.evidence.attempt.prepared().scopes()[0]
    }
    pub(crate) fn mode(&self) -> TaskStopPhaseInstallMode {
        self.evidence.mode
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        &self.evidence.receipt
    }
    pub(crate) fn validate_read_view_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx)
                && scope == self.scope()
                && self.mode() == TaskStopPhaseInstallMode::Incomplete,
            "Stop partial read requires its same fresh incomplete view"
        );
        Ok(())
    }
}
impl crate::runtime_db::RuntimeDb {
    /// Current complete uses the existing joint-domain resolver. Absence is not
    /// a sparse positive proof and keeps the original family's negative path.
    #[cfg(test)]
    pub(crate) fn observe_task_stop_sparse_recovery(
        &self,
        attempt: &OriginalInputMutationAttempt,
        request: &super::super::task_snapshot::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Option<TaskStopSparseRecoveryEvidence>> {
        Ok(self
            .with_task_stop_sparse_recovery(attempt, request, ledger, |_, _| Ok(()))?
            .map(|(evidence, ())| evidence))
    }

    /// The original owner may build partial State/Queue/Work/Input installation
    /// in this same fresh view. This never prepares Task or grants Ready/exit.
    pub(crate) fn with_task_stop_sparse_recovery<T>(
        &self,
        attempt: &OriginalInputMutationAttempt,
        request: &super::super::task_snapshot::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        read_current: impl FnOnce(&Transaction<'_>, &TaskStopSparseReadView<'_, '_>) -> Result<T>,
    ) -> Result<Option<(TaskStopSparseRecoveryEvidence, T)>> {
        ensure!(
            attempt.prepared().family() == InputMutationFamily::Task
                && attempt.prepared().scopes().len() == 1,
            "Stop sparse recovery requires its original parent group"
        );
        let scope = &attempt.prepared().scopes()[0];
        let mut connection = self.connection()?;
        let receipt_view = std::cell::RefCell::new(None);
        with_fresh_input_mutation_preparation_and_preflight(
            &mut connection,
            attempt,
            |tx| {
                let guard = super::super::transitions::task_result_receipt_budget_tx(
                    tx,
                    attempt.prepared(),
                    scope,
                    request,
                    ledger,
                )?;
                receipt_view.replace(Some(guard.clone()));
                Ok(guard)
            },
            |tx, resolution| {
                let InputMutationResolution::Present(receipt) = resolution else {
                    return Ok(None);
                };
                let InputMutationResult::TaskStopPhase { ack } = &receipt.outcome.result else {
                    return Ok(None);
                };
                ensure!(
                    &ack.binding.scope == scope,
                    "Stop sparse recovery scope differs"
                );
                let source = load_task_stop_by_sequence_tx(
                    tx,
                    scope,
                    ack.binding.effect_seq,
                    request.byte_limit,
                    ledger,
                )?;
                let original = source
                    .records
                    .first()
                    .context("Stop sparse original row missing")?;
                ensure!(
                    original.receipt_binding() == ack.binding
                        && original.prepared.command_nonce() == ack.original_nonce,
                    "Stop sparse recovery initial whole receipt differs"
                );
                let mut guards = vec![
                    receipt_view
                        .borrow_mut()
                        .take()
                        .context("Stop sparse receipt guard missing")?,
                    source._budget.clone(),
                ];
                let bytes = task_stop_phase_identity_material_tx(tx, scope)?
                    .checked_mul(8)
                    .context("Stop sparse identity material overflow")?;
                ensure!(
                    bytes <= request.byte_limit.min(MAX_BYTES)
                        && 8 <= request.record_limit.min(MAX_RECORDS),
                    super::super::task_snapshot::TaskSnapshotError::ResourceExhausted(
                        "Stop sparse identity material"
                    )
                );
                guards.push(ledger.register_owned_bytes(
                    scope,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: u64::try_from(bytes)?,
                        metadata_estimate_bytes: 0,
                    },
                )?);
                let mode = observe_task_stop_phase_mode_tx(tx, scope)?;
                if mode == TaskStopPhaseInstallMode::Current {
                    return Ok(None);
                }
                let row:Option<(String,String,String)>=tx.query_row("SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",
                    [&scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                let identity = row
                    .map(|(agent, status, raw)| -> Result<_> {
                        let identity =
                            super::super::repositories::decode_agent_identity_payload(&raw)?;
                        ensure!(
                            agent == scope.agent_id
                                && identity.agent_id == agent
                                && identity.incarnation > 0
                                && serde_json::to_value(&identity.status)?.as_str()
                                    == Some(status.as_str()),
                            "Stop sparse identity projection corrupt"
                        );
                        Ok(identity)
                    })
                    .transpose()?;
                let state_presence = match mode {
                    TaskStopPhaseInstallMode::NoState => TaskStopSparseStatePresence::Absent,
                    TaskStopPhaseInstallMode::Incomplete => {
                        TaskStopSparseStatePresence::PresentUnprepared
                    }
                    TaskStopPhaseInstallMode::Retired => {
                        TaskStopSparseStatePresence::NotReadRetired
                    }
                    TaskStopPhaseInstallMode::Current => unreachable!(),
                };
                let evidence = TaskStopSparseRecoveryEvidence {
                    attempt: attempt.clone(),
                    receipt,
                    identity,
                    mode,
                    state_presence,
                    _budget_views: guards,
                };
                let current = read_current(
                    tx,
                    &TaskStopSparseReadView {
                        tx,
                        evidence: &evidence,
                    },
                )?;
                Ok(Some((evidence, current)))
            },
        )
    }
}
