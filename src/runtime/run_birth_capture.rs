//! One owner publication supplies every original RunOnce report domain.
use super::{add_material, RetainedRunTerminalText, RunBirthSessions, RunBirthState};
use crate::runtime::{
    current_event_queries::CapturedCurrentEventQuery,
    input_queries::CapturedInputQuery,
    mutation_owner::{PostureObservation, PublicationToken},
    resident_task::CapturedTasks,
    task_queries::TaskBirthCaptureMaterial,
    RuntimeHandle,
};
use anyhow::{Context, Result};
use std::sync::Arc;

/// Only selected records, finite State fields and the actual leases survive.
/// Their original guards stay here through projection and asynchronous use.
pub(crate) struct CapturedRunBirth {
    pub(crate) token: PublicationToken,
    pub(super) baseline: super::RunBirthBaseline,
    pub(crate) state: RunBirthState,
    pub(crate) tasks: CapturedTasks,
    pub(crate) inputs: CapturedInputQuery,
    pub(crate) events: CapturedCurrentEventQuery,
    _task_material: TaskBirthCaptureMaterial,
    _retained_terminal: Option<Arc<RetainedRunTerminalText>>,
}
impl CapturedRunBirth {
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        add_material(
            self._retained_terminal
                .as_ref()
                .map_or((0, 0), |text| text.material_estimate()),
            add_material(
                add_material(
                    self.state.material_estimate(),
                    self._task_material.material_estimate()?,
                )?,
                add_material(
                    self.inputs.material_estimate(),
                    self.events.material_estimate()?,
                )?,
            )?,
        )
    }
    /// Borrow actual private guards before another same-cut domain accepts this
    /// phase as prior. This accounting observation grants no body membership.
    pub(crate) fn validate_material_binding(
        &self,
        runtime: &RuntimeHandle,
        root: &Arc<PostureObservation>,
    ) -> Result<()> {
        anyhow::ensure!(
            self.token == root.token,
            "Run material belongs to another publication"
        );
        self.validate_render_accounting(runtime)?;
        let ledger = self.inputs.actual_material_ledger(&self.token)?;
        let scope = &self.inputs.input.scope;
        self._task_material
            .validate_material_binding(runtime, root, &self.tasks)?;
        self.events.validate_material_binding(runtime, root)?;
        if let Some(text) = &self._retained_terminal {
            text.validate_accounting(scope, ledger, self.token)?;
        }
        Ok(())
    }
    /// Already-selected data may render after its publication is superseded.
    /// The recipient must still use this original owner incarnation and ledger;
    /// this is not a fresh Root lookup or a new read/mutation grant.
    pub(in crate::runtime) fn validate_render_accounting(
        &self,
        runtime: &RuntimeHandle,
    ) -> Result<()> {
        let ledger = self.inputs.actual_material_ledger(&self.token)?;
        let scope = &self.inputs.input.scope;
        anyhow::ensure!(
            ledger.same_ledger(&runtime.inner.input_evidence_budget)
                && scope.agent_id == runtime.inner.mutation_owner.agent_id()
                && scope.identity_incarnation == runtime.inner.identity_incarnation
                && self.token.identity_incarnation == scope.identity_incarnation
                && self.token.owner_epoch == runtime.inner.mutation_owner.owner_epoch()
                && self.tasks.scope == *scope
                && self.events.selected.scope == *scope,
            "Run render belongs to another original owner or actual ledger"
        );
        self.state.validate_material_binding(scope, ledger)?;
        if let Some(text) = &self._retained_terminal {
            text.validate_accounting(scope, ledger, self.token)?;
        }
        Ok(())
    }
    pub(crate) fn retain_terminal_text(&mut self) -> Option<Arc<RetainedRunTerminalText>> {
        self.state
            .take_terminal_text_after(self.baseline.turn_index, self.token)
            .map(Arc::new)
    }
}

impl RuntimeHandle {
    pub(crate) fn capture_run_birth_sessions(
        &self,
        sessions: &RunBirthSessions,
    ) -> Result<CapturedRunBirth> {
        self.capture_run_birth_sessions_with_terminal_text(sessions, (usize::MAX, usize::MAX), None)
    }
    pub(crate) fn capture_run_birth_sessions_with_terminal_text(
        &self,
        sessions: &RunBirthSessions,
        limits: (usize, usize),
        retained: Option<Arc<RetainedRunTerminalText>>,
    ) -> Result<CapturedRunBirth> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Run report root unpublished")?;
        self.capture_run_birth_sessions_from_snapshot_with_terminal_text(
            sessions, &root, limits, retained,
        )
    }
    pub(in crate::runtime) fn capture_run_birth_sessions_from_snapshot(
        &self,
        sessions: &RunBirthSessions,
        root: &Arc<PostureObservation>,
        limits: (usize, usize),
    ) -> Result<CapturedRunBirth> {
        self.capture_run_birth_sessions_from_snapshot_with_terminal_text(
            sessions, root, limits, None,
        )
    }
    pub(in crate::runtime) fn capture_run_birth_sessions_from_snapshot_with_terminal_text(
        &self,
        sessions: &RunBirthSessions,
        root: &Arc<PostureObservation>,
        limits: (usize, usize),
        retained: Option<Arc<RetainedRunTerminalText>>,
    ) -> Result<CapturedRunBirth> {
        let prior = if let Some(text) = retained.as_ref() {
            let tasks = root
                .tasks
                .as_ref()
                .context("retained Run text scope unpublished")?;
            text.validate_accounting(tasks.scope(), &self.inner.input_evidence_budget, root.token)?;
            text.material_estimate()
        } else {
            (0, 0)
        };
        // These original validators allocate nothing. Validate every real loan
        // before its counters can reduce the first selected allocation's cap.
        sessions.tasks.validate_held_material(self, root)?;
        sessions.inputs.validate_held_material(self, root)?;
        sessions.events.validate_held_material(self, root)?;
        let task_lease = sessions.tasks.material_estimate();
        let input_lease = sessions.inputs.material_estimate();
        let event_lease = sessions.events.material_estimate()?;
        let all_leases = add_material(add_material(task_lease, input_lease)?, event_lease)?;
        let state =
            self.capture_run_state_from_snapshot(root, limits, add_material(all_leases, prior)?)?;
        let state_material = add_material(state.material_estimate(), prior)?;

        // Each capture adds its own lease exactly once. Prior contains only
        // other live domains and the already selected finite State header.
        let task_prior = add_material(state_material, add_material(input_lease, event_lease)?)?;
        let (tasks, task_material) = self.capture_task_birth_session_from_snapshot_with_prior(
            &sessions.tasks,
            root,
            limits,
            task_prior,
        )?;
        let event_prior = add_material(
            state_material,
            add_material(task_material.material_estimate()?, input_lease)?,
        )?;
        let events = self.capture_current_event_birth_session_from_snapshot_with_prior(
            &sessions.events,
            root,
            limits,
            event_prior,
        )?;
        let input_prior = add_material(
            state_material,
            add_material(
                task_material.material_estimate()?,
                events.material_estimate()?,
            )?,
        )?;
        let inputs = self.capture_input_birth_session_from_snapshot_with_limits_and_prior(
            &sessions.inputs,
            root,
            limits,
            input_prior,
        )?;
        Ok(CapturedRunBirth {
            token: root.token,
            baseline: sessions.baseline,
            state,
            tasks,
            inputs,
            events,
            _task_material: task_material,
            _retained_terminal: retained,
        })
    }
}
