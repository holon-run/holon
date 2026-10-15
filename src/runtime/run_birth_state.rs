//! Selected RunOnce State fields; no Root or complete AgentState survives.
use super::{remaining_material, InputQueryError};
use crate::{
    config::ModelRouteRef,
    runtime::{
        input_evidence_budget::{
            borrowed_json_material, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
        },
        mutation_owner::PostureObservation,
        RuntimeHandle,
    },
    types::{AgentStatus, FailureArtifact},
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

pub(crate) struct RunTerminalState {
    pub(crate) turn_index: u64,
    pub(crate) last_assistant_message: Option<String>,
}
/// The original selected State allocation supplies this String without copying.
/// Its original guard survives every poll which still uses this result.
pub(crate) struct RetainedRunTerminalText {
    text: String,
    material: (u64, usize),
    token: crate::runtime::mutation_owner::PublicationToken,
    _guard: Arc<BudgetViewGuard>,
}
impl RetainedRunTerminalText {
    pub(crate) fn as_str(&self) -> &str {
        self.text.trim()
    }
    pub(super) fn material_estimate(&self) -> (u64, usize) {
        self.material
    }
    pub(super) fn validate_accounting(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        current: crate::runtime::mutation_owner::PublicationToken,
    ) -> Result<()> {
        ensure!(
            self.token.owner_epoch == current.owner_epoch
                && self.token.identity_incarnation == current.identity_incarnation,
            "retained Run text belongs to another owner incarnation"
        );
        self._guard
            .validate_owned_material_binding(scope, ledger, self.material.0)?;
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::runtime) fn guard_for_test(&self) -> &Arc<BudgetViewGuard> {
        &self._guard
    }
}
pub(crate) struct RunFailureState {
    pub(crate) summary: String,
    pub(crate) failure_artifact: Option<FailureArtifact>,
}
pub(crate) struct RunBirthState {
    pub(crate) status: AgentStatus,
    pub(crate) current_run_id: Option<String>,
    pub(crate) pending: usize,
    pub(crate) pending_wake: bool,
    pub(crate) turn_index: u64,
    pub(crate) total_input_tokens: u64,
    pub(crate) total_output_tokens: u64,
    pub(crate) total_model_rounds: u64,
    pub(crate) last_turn_terminal: Option<RunTerminalState>,
    pub(crate) last_runtime_failure: Option<RunFailureState>,
    pub(crate) last_requested_model: Option<ModelRouteRef>,
    pub(crate) last_active_model: Option<ModelRouteRef>,
    material: (u64, usize),
    _guard: Arc<BudgetViewGuard>,
}
impl RunBirthState {
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.material
    }
    pub(super) fn validate_material_binding(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<()> {
        self._guard
            .validate_owned_material_binding(scope, ledger, self.material.0)?;
        Ok(())
    }
    pub(super) fn take_terminal_text_after(
        &mut self,
        baseline: u64,
        token: crate::runtime::mutation_owner::PublicationToken,
    ) -> Option<RetainedRunTerminalText> {
        let record = self
            .last_turn_terminal
            .as_mut()
            .filter(|record| record.turn_index > baseline)?;
        let text = record.last_assistant_message.take()?;
        if text.trim().is_empty() {
            return None;
        }
        Some(RetainedRunTerminalText {
            text,
            material: self.material,
            token,
            _guard: self._guard.clone(),
        })
    }
}

impl RuntimeHandle {
    pub(in crate::runtime) fn capture_run_state_from_snapshot(
        &self,
        root: &Arc<PostureObservation>,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<RunBirthState> {
        // Registry access validates the live owner epoch without preparing Input.
        self.inner.mutation_owner.input_queries()?;
        let tasks = root
            .tasks
            .as_ref()
            .context("Run State Task scope unpublished")?;
        let scope = tasks.scope();
        let ledger = &self.inner.input_evidence_budget;
        ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && scope.agent_id == root.state.id
                && scope.identity_incarnation == root.token.identity_incarnation
                && root.token.identity_incarnation == self.inner.identity_incarnation
                && root.token.owner_epoch == self.inner.mutation_owner.owner_epoch()
                && tasks.shares_budget(ledger),
            "Run State publication scope or ledger differs"
        );
        let state = &root.state;
        // Count only the original borrowed route components before any
        // selected field is cloned.
        fn route_parts(route: &ModelRouteRef) -> (&str, &str, &str) {
            (
                route.provider.as_str(),
                route.endpoint.as_str(),
                route.model.as_str(),
            )
        }
        let terminal = state
            .last_turn_terminal
            .as_ref()
            .map(|record| (record.turn_index, record.last_assistant_message.as_deref()));
        let failure = state
            .last_runtime_failure
            .as_ref()
            .map(|record| (record.summary.as_str(), record.failure_artifact.as_ref()));
        let (encoded, nodes) = borrowed_json_material(&(
            &state.status,
            state.current_run_id.as_deref(),
            state.pending,
            state.pending_wake_hint.is_some(),
            state.turn_index,
            state.total_input_tokens,
            state.total_output_tokens,
            state.total_model_rounds,
            terminal,
            failure,
            state.last_requested_model.as_ref().map(route_parts),
            state.last_active_model.as_ref().map(route_parts),
        ))?;
        let exhausted = || InputQueryError::ResourceExhausted("selected Run State material");
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context(exhausted())?,
            )
            .and_then(|value| value.checked_add(4096))
            .and_then(|value| value.checked_mul(8))
            .context(exhausted())?;
        let count = nodes
            .checked_add(16)
            .and_then(|n| n.checked_mul(8))
            .context(exhausted())?;
        let remaining = remaining_material(ledger, limits, prior)?;
        ensure!(
            bytes <= u64::try_from(remaining.0)? && count <= remaining.1,
            exhausted()
        );
        let guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok(RunBirthState {
            status: state.status.clone(),
            current_run_id: state.current_run_id.clone(),
            pending: state.pending,
            pending_wake: state.pending_wake_hint.is_some(),
            turn_index: state.turn_index,
            total_input_tokens: state.total_input_tokens,
            total_output_tokens: state.total_output_tokens,
            total_model_rounds: state.total_model_rounds,
            last_turn_terminal: state
                .last_turn_terminal
                .as_ref()
                .map(|record| RunTerminalState {
                    turn_index: record.turn_index,
                    last_assistant_message: record.last_assistant_message.clone(),
                }),
            last_runtime_failure: state.last_runtime_failure.as_ref().map(|record| {
                RunFailureState {
                    summary: record.summary.clone(),
                    failure_artifact: record.failure_artifact.clone(),
                }
            }),
            last_requested_model: state.last_requested_model.clone(),
            last_active_model: state.last_active_model.clone(),
            material: (bytes, count),
            _guard: guard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;

    #[tokio::test]
    async fn actual_run_state_counts_selected_fields_before_clone_and_excludes_unselected_body(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime
            .update_agent_state(|state| {
                state.last_wake_reason = Some("x".repeat(256 * 1024));
                state.last_requested_model = Some(ModelRouteRef::parse("openai@default/gpt-test")?);
                Ok(())
            })
            .await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let root = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("Root missing")?;
        let ledger = &runtime.inner.input_evidence_budget;
        let strong = Arc::strong_count(&root);
        let view =
            runtime.capture_run_state_from_snapshot(&root, (64 * 1024 * 1024, 65536), (0, 0))?;
        assert_eq!(Arc::strong_count(&root), strong);
        assert_eq!(view.current_run_id, root.state.current_run_id);
        assert_eq!(view.last_requested_model, root.state.last_requested_model);
        let own = view.material_estimate();
        assert!(
            own.0 < 256 * 1024,
            "unselected State body entered report material"
        );
        drop(view);
        let prior = (128 * 1024, 32usize);
        let scope = root.tasks.as_ref().unwrap().scope();
        let prior_guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: prior.0,
                metadata_estimate_bytes: 0,
            },
        )?;
        let held = vec![9u8; usize::try_from(prior.0)?];
        let baseline = ledger.report()?.observed_bytes;
        let total = (usize::try_from(own.0 + prior.0)?, own.1 + prior.1);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE agent_states RENAME TO run_state_forbidden_states;
             ALTER TABLE tasks RENAME TO run_state_forbidden_tasks;",
        )?;
        for cap in [(total.0 - 1, total.1), (total.0, total.1 - 1)] {
            let error = runtime
                .capture_run_state_from_snapshot(&root, cap, prior)
                .err()
                .context("selected State ignored actual prior")?;
            assert!(
                matches!(
                    error.downcast_ref::<InputQueryError>(),
                    Some(InputQueryError::ResourceExhausted(_))
                ),
                "{error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, baseline);
        }
        let view = runtime.capture_run_state_from_snapshot(&root, total, prior)?;
        assert_eq!(view.material_estimate(), own);
        assert!(ledger.report()?.observed_bytes > baseline);
        drop(view);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop((prior_guard, held));
        Ok(())
    }

    #[tokio::test]
    async fn run_state_rejects_foreign_runtime_before_a_new_guard() -> Result<()> {
        let first = LifecycleHarness::new();
        let other = LifecycleHarness::new();
        let root = first
            .runtime()
            .inner
            .mutation_owner
            .capture()?
            .context("Root missing")?;
        let ledger = &other.runtime().inner.input_evidence_budget;
        let before = ledger.report()?.observed_bytes;
        assert!(other
            .runtime()
            .capture_run_state_from_snapshot(&root, (64 * 1024 * 1024, 65536), (0, 0))
            .is_err());
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    }
}
