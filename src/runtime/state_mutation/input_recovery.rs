//! Persist recovered input evidence through the original owner receipt.
use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    domain::execution_protocol::ExecutionAttempt,
    runtime::input_consumption::read::QualifiedTurnRead,
    runtime_db::transitions::{
        LiveInputRecoveryCommand, PreparedInputTransition, ProviderResultHandoffCommand,
    },
};

impl RuntimeMutation {
    pub(in crate::runtime) async fn commit_idle_recovery_batch(
        &mut self,
        prior: QualifiedTurnRead,
        turn: TurnRecord,
        material: &ResourceReadMaterial,
    ) -> Result<()> {
        let prepared = material.read(|| {
            crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::turn_idle_batch(
                self.input_evidence_scope(),
                crate::ids::audit_event_id(),
                self.runtime.now(),
                &prior,
                turn,
                &self.runtime.inner.input_evidence_budget,
            )
        })?;
        self.commit_prepared_evidence(prepared, None).await?;
        Ok(())
    }

    pub(in crate::runtime) async fn commit_provider_result_handoff(
        &mut self,
        expected_attempt: ExecutionAttempt,
        expected_turn: TurnRecord,
        at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        let prepared = PreparedInputTransition::provider_result_handoff(
            self.input_evidence_scope(),
            crate::ids::audit_event_id(),
            at,
            ProviderResultHandoffCommand {
                expected_attempt,
                expected_turn,
                at,
                fault: self.runtime.take_transition_fault(),
            },
        )?;
        let commit = self.commit_prepared_input_transition(prepared).await?;
        let applied = commit.applied;
        self.runtime.apply_transition_commit(commit).await;
        Ok(applied)
    }

    pub(in crate::runtime) async fn commit_live_input_recovery(
        &mut self,
        expected_attempt: ExecutionAttempt,
        expected_turn: TurnRecord,
        predecessor: TurnRecord,
    ) -> Result<bool> {
        let prepared = PreparedInputTransition::live_input_recovery(
            self.input_evidence_scope(),
            crate::ids::audit_event_id(),
            self.runtime.now(),
            LiveInputRecoveryCommand {
                expected_attempt,
                expected_turn,
                predecessor,
                fault: self.runtime.take_transition_fault(),
            },
        )?;
        let commit = self.commit_prepared_input_transition(prepared).await?;
        let applied = commit.applied;
        self.runtime.apply_transition_commit(commit).await;
        Ok(applied)
    }
}
