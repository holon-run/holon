//! One original owner cut registers all complete report sources together.
use super::{
    current_event_queries::CurrentEventBirthSession,
    input_evidence_budget::InputEvidenceBudgetLedger,
    input_queries::{InputBirthSession, InputQueryError},
    mutation_owner::{MutationReservation, PostureObservation},
    task_queries::TaskBirthSession,
    RuntimeHandle,
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

#[path = "run_once_read.rs"]
mod run_read;
pub(crate) use run_read::{
    RunFinalRead, RunFinalResponseSource, RunPollRead, RunRenderedFinal, RunResponseTasks,
};

#[path = "run_birth_state.rs"]
mod state;
pub(crate) use state::{RetainedRunTerminalText, RunBirthState};

#[path = "run_birth_capture.rs"]
mod capture;
pub(crate) use capture::CapturedRunBirth;

#[path = "run_birth_report_projection.rs"]
mod report;
pub(crate) use report::{RunBirthReportProjection, RunPollActivity, RunPollFacts};

/// Only bounded counters and live leases survive the registration owner.
/// No Root, Runtime or database is held by this caller-owned report role.
pub(crate) struct RunBirthSessions {
    pub(in crate::runtime) tasks: TaskBirthSession,
    pub(in crate::runtime) inputs: InputBirthSession,
    pub(in crate::runtime) events: CurrentEventBirthSession,
    pub(crate) baseline: RunBirthBaseline,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunBirthBaseline {
    pub(crate) turn_index: u64,
    pub(crate) total_input_tokens: u64,
    pub(crate) total_output_tokens: u64,
    pub(crate) total_model_rounds: u64,
    pub(crate) task_cursor: u64,
    pub(crate) input_cursor: u64,
    pub(crate) event_cursor: u64,
}
pub(in crate::runtime) fn add_material(a: (u64, usize), b: (u64, usize)) -> Result<(u64, usize)> {
    let exhausted = || InputQueryError::ResourceExhausted("joint birth material overflow");
    Ok((
        a.0.checked_add(b.0).context(exhausted())?,
        a.1.checked_add(b.1).context(exhausted())?,
    ))
}
/// Pure arithmetic over actual held allocations, never a new budget or grant.
pub(in crate::runtime) fn remaining_material(
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
    prior: (u64, usize),
) -> Result<(usize, usize)> {
    let actual = ledger.limits();
    let bytes = usize::try_from(
        actual
            .history_per_agent_bytes
            .checked_add(actual.pending_per_agent_bytes)
            .context(InputQueryError::ResourceExhausted(
                "joint birth limit overflow",
            ))?,
    )?
    .min(64 * 1024 * 1024)
    .min(limits.0);
    let records = usize::try_from(actual.record_limit)?
        .min(65_536)
        .min(limits.1);
    let exhausted = || InputQueryError::ResourceExhausted("held birth sessions");
    let bytes = bytes
        .checked_sub(usize::try_from(prior.0)?)
        .context(exhausted())?;
    let records = records.checked_sub(prior.1).context(exhausted())?;
    ensure!(bytes > 0 && records > 0, exhausted());
    Ok((bytes, records))
}
impl RuntimeHandle {
    pub(crate) async fn begin_run_birth_sessions(&self) -> Result<RunBirthSessions> {
        let held = self.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("Run report root is unpublished")?;
        self.register_run_birth_sessions_from_snapshot(&held, &root, (usize::MAX, usize::MAX))
    }
    pub(in crate::runtime) fn register_run_birth_sessions_from_snapshot(
        &self,
        held: &MutationReservation,
        root: &Arc<PostureObservation>,
        limits: (usize, usize),
    ) -> Result<RunBirthSessions> {
        // Each original registration revalidates this exact held publication.
        // A later failure drops all earlier new roles instead of returning a
        // partial/empty baseline. Each next factory gets true remaining space.
        let tasks =
            self.register_task_birth_session_from_snapshot_with_prior(held, root, limits, (0, 0))?;
        let task_material = tasks.material_estimate();
        let inputs = self.register_input_birth_session_from_snapshot_with_prior(
            held,
            root,
            limits,
            task_material,
        )?;
        let prior = add_material(task_material, inputs.material_estimate())?;
        let events = self.register_current_event_birth_session_from_snapshot_with_prior(
            held, root, limits, prior,
        )?;
        let baseline = RunBirthBaseline {
            turn_index: root.state.turn_index,
            total_input_tokens: root.state.total_input_tokens,
            total_output_tokens: root.state.total_output_tokens,
            total_model_rounds: root.state.total_model_rounds,
            task_cursor: tasks.cursor(),
            input_cursor: inputs.cursor,
            event_cursor: events.cursor,
        };
        Ok(RunBirthSessions {
            tasks,
            inputs,
            events,
            baseline,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;
    #[tokio::test]
    async fn unified_begin_preserves_incomplete_input_and_releases_prior_task_role() -> Result<()> {
        let h = LifecycleHarness::new();
        let runtime = h.runtime();
        runtime.quiesce_context_resource_observer_task_for_test().await?;
        // A real unsupported evidence ACK invalidates the genuine bootstrap
        // cursor. This exercises the missing Source gate without editing a DTO.
        let entry=crate::types::TranscriptEntry::new(runtime.instance_agent_id(),
            crate::types::TranscriptEntryKind::AssistantRound,Some(0),None,
            serde_json::json!({"text":"actual ACK establishes incomplete Input"}));
        runtime.persist_transcript_evidence(&entry).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = runtime
            .begin_run_birth_sessions()
            .await
            .err()
            .context("unprepared Input source became a complete report baseline")?;
        ensure!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::Incomplete)
            ),
            "{error:#}"
        );
        let held = runtime.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("fixture Root missing")?;
        let scope = root.tasks.as_ref().context("Task missing")?.scope();
        let roles = runtime.inner.task_queries.snapshot(
            scope,
            root.token.owner_epoch,
            &runtime.inner.input_evidence_budget,
            65_536,
            64 * 1024 * 1024,
        )?;
        assert_eq!(roles.selection, Default::default());
        drop(roles);
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        Ok(())
    }
    #[tokio::test]
    async fn original_registration_debits_actual_prior_before_factory_allocation() -> Result<()> {
        let h = LifecycleHarness::new();
        let runtime = h.runtime();
        let _pause = runtime.pause_resource_observer_for_test().await;
        let held = runtime.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("fixture Root missing")?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let first = runtime.register_task_birth_session_from_snapshot_with_prior(
            &held,
            &root,
            (64 * 1024 * 1024, 65_536),
            (0, 0),
        )?;
        let material = first.material_estimate();
        // The prior is this real guarded lease, not an invented estimate.
        for limits in [
            (
                usize::try_from(material.0.checked_mul(2).context("fixture overflow")?)? - 1,
                65_536,
            ),
            (64 * 1024 * 1024, material.1 * 2 - 1),
        ] {
            assert!(usize::try_from(material.0)? < limits.0 && material.1 < limits.1);
            let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
            let error = runtime
                .register_task_birth_session_from_snapshot_with_prior(
                    &held, &root, limits, material,
                )
                .err()
                .context("original Task lease got an independent full cap")?;
            ensure!(
                matches!(
                    error.downcast_ref::<super::super::resident_task::TaskReadError>(),
                    Some(super::super::resident_task::TaskReadError::ResourceExhausted(_))
                ) || matches!(
                    error.downcast_ref::<InputQueryError>(),
                    Some(InputQueryError::ResourceExhausted(_))
                ),
                "{error:#}"
            );
            assert_eq!(
                runtime.inner.input_evidence_budget.report()?.observed_bytes,
                before
            );
        }
        let second = runtime.register_task_birth_session_from_snapshot_with_prior(
            &held,
            &root,
            (usize::try_from(material.0 * 2)?, material.1 * 2),
            material,
        )?;
        assert_eq!(second.material_estimate(), material);
        drop((first, second));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        let other = LifecycleHarness::new();
        let other_held = other.runtime().inner.mutation_owner.reserve().await?;
        let other_root = other_held
            .capture_ready_snapshot()?
            .context("foreign Root missing")?;
        assert!(runtime
            .register_run_birth_sessions_from_snapshot(
                &other_held,
                &other_root,
                (64 * 1024 * 1024, 65_536)
            )
            .is_err());
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        Ok(())
    }
}
