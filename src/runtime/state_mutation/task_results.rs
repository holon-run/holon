//! Standalone Result phases use the same original owner/witness/receipt protocol.
use super::*;
use crate::runtime_db::{
    task_result_settlement::TaskResultWriteOutcome,
    transitions::{PreparedTaskResultTransition, TaskResultOperation},
};

pub(in crate::runtime) struct TaskResultOutcomeRead {
    outcome: TaskResultWriteOutcome,
    _owned_budget: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
}
impl TaskResultOutcomeRead {
    pub(in crate::runtime) fn record(
        &self,
    ) -> Result<&Option<crate::runtime_db::TaskResultSettlementRecord>> {
        match &self.outcome {
            TaskResultWriteOutcome::Record(record) => Ok(record),
            _ => bail!("TaskResult original return type differs"),
        }
    }
    pub(in crate::runtime) fn records(
        &self,
    ) -> Result<&[crate::runtime_db::TaskResultSettlementRecord]> {
        match &self.outcome {
            TaskResultWriteOutcome::Records(records) => Ok(records),
            _ => bail!("TaskResult original return type differs"),
        }
    }
    pub(in crate::runtime) fn changed(self) -> Result<bool> {
        match self.outcome {
            TaskResultWriteOutcome::Changed(value) => Ok(value),
            _ => bail!("TaskResult original return type differs"),
        }
    }
    pub(in crate::runtime) fn count(self) -> Result<usize> {
        match self.outcome {
            TaskResultWriteOutcome::Count(value) => Ok(value),
            _ => bail!("TaskResult original return type differs"),
        }
    }
}
impl RuntimeMutation {
    pub(in crate::runtime) async fn commit_task_result_operation(
        &mut self,
        operation: TaskResultOperation,
    ) -> Result<TaskResultOutcomeRead> {
        let command =
            PreparedTaskResultTransition::new(self.input_evidence_scope().agent_id, operation)?;
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::task_result(
            self.input_evidence_scope(),
            crate::ids::audit_event_id(),
            self.runtime.now(),
            command,
        )?;
        self.commit_prepared_task_result(prepared).await
    }
    pub(in crate::runtime) async fn commit_prepared_task_result(
        &mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<TaskResultOutcomeRead> {
        anyhow::ensure!(
            prepared.descriptor().family()
                == crate::runtime_db::input_mutation_receipts::InputMutationFamily::TaskResult,
            "TaskResult original descriptor has another family"
        );
        let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
        let crate::runtime_db::input_mutation_receipts::InputMutationResult::TaskResult { outcome } =
            result
        else {
            bail!("TaskResult ACK has another original result family")
        };
        // Original SQL/replay guards still live in commit until this distinct
        // returned owned outcome has its own observation, before any await.
        let bytes = super::super::input_evidence_budget::serialized_bytes(outcome.as_ref())?;
        let guard = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &self.input_evidence_scope(),
                super::super::input_evidence_budget::InputMemoryCategory::Pins,
                super::super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: bytes,
                    metadata_estimate_bytes: 128,
                },
            )?;
        let outcome = TaskResultOutcomeRead {
            outcome: *outcome,
            _owned_budget: guard,
        };
        self.runtime.apply_transition_commit(commit).await;
        Ok(outcome)
    }
}
impl RuntimeHandle {
    #[inline(never)]
    pub(in crate::runtime) fn apply_task_result_operation<'a, S, F>(
        &'a self,
        borrowed_material: S,
        freeze: F,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<TaskResultOutcomeRead>> + Send + 'a>,
    >
    where
        S: serde::Serialize + Send + 'a,
        F: FnOnce() -> TaskResultOperation + Send + 'a,
    {
        Box::pin(async move {
            // Count borrowed fields before cloning the frozen source/IDs. The
            // guard survives the owner wait and hands off after original ACK.
            let bytes = super::super::input_evidence_budget::serialized_bytes(&borrowed_material)?
                .checked_mul(4)
                .and_then(|bytes| bytes.checked_add(512))
                .context("TaskResult frozen operation bytes overflow")?;
            let limits = self.inner.input_evidence_budget.limits();
            let byte_limit = limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("TaskResult frozen operation limit overflow")?
                .min(64 * 1024 * 1024);
            anyhow::ensure!(
                bytes <= byte_limit && limits.record_limit >= 4,
                crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                    "frozen Result operation"
                )
            );
            let scope = super::super::resident_input_evidence::InputEvidenceScope {
                agent_id: self.inner.mutation_owner.agent_id().to_owned(),
                identity_incarnation: self.inner.identity_incarnation,
            };
            let _operation_budget = self.inner.input_evidence_budget.register_owned_bytes(
                &scope,
                super::super::input_evidence_budget::InputMemoryCategory::Candidate,
                super::super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: bytes,
                    metadata_estimate_bytes: 128,
                },
            )?;
            let operation = freeze();
            let mut mutation = self.begin_mutation().await?;
            mutation.commit_task_result_operation(operation).await
        })
    }
}
