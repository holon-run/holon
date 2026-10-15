//! Sequential selection from one immutable publication; prior grants no keys.
use super::*;
use crate::runtime::{
    current_read_facts::{CapturedCurrentReadFacts, CurrentReadError, CurrentReadRequest},
    input_evidence_budget::{borrowed_json_material, InputEvidenceBudgetLedger},
    mutation_owner::{PostureObservation, PublicationToken},
    resident_input_evidence::{CapturedInputEvidence, InputCaptureMaterial},
    resident_scheduling::{
        CapturedScheduling, SchedulingCaptureMaterial, SchedulingCaptureRequest,
    },
    resident_task::{CapturedTasks, TaskCaptureMaterial, TaskReadError},
    run_birth_sessions::{add_material, CapturedRunBirth},
    task_reads::{scheduler_task_request_with_material, SchedulerTaskSelectorMaterial},
};

/// Borrowed source allocations stay with the original outer report. No Root,
/// registry or Runtime is retained, and ID/content equality never discounts a body.
pub(in crate::runtime) struct SchedulerReadBodyLoans<'a> {
    source: &'a CapturedRunBirth,
}
impl<'a> SchedulerReadBodyLoans<'a> {
    pub(in crate::runtime) fn from_run_birth(
        runtime: &RuntimeHandle,
        root: &Arc<PostureObservation>,
        source: &'a CapturedRunBirth,
    ) -> Result<Self> {
        source.validate_material_binding(runtime, root)?;
        let inputs = root
            .input_evidence
            .as_ref()
            .context("scheduler Input loan domain unpublished")?;
        inputs.validate_scheduler_body_loan(
            &source.inputs.input,
            &runtime.inner.input_evidence_budget,
        )?;
        Ok(Self { source })
    }
    fn validate(
        &self,
        runtime: &RuntimeHandle,
        root: &Arc<PostureObservation>,
        prior: (u64, usize),
    ) -> Result<()> {
        self.source.validate_material_binding(runtime, root)?;
        root.input_evidence
            .as_ref()
            .context("scheduler Input loan domain unpublished")?
            .validate_scheduler_body_loan(
                &self.source.inputs.input,
                &runtime.inner.input_evidence_budget,
            )?;
        let held = self.source.material_estimate()?;
        anyhow::ensure!(
            prior.0 >= held.0 && prior.1 >= held.1,
            "scheduler prior omits its actual original source"
        );
        Ok(())
    }
}

/// Only selected bodies, original material and finite new headers leave this
/// phase. Projection must admit every further copy before constructing its DTO.
pub(super) struct SchedulerSelectedSources {
    pub(super) token: PublicationToken,
    pub(super) state: Arc<AgentState>,
    pub(super) work: CapturedWork,
    pub(super) inputs: CapturedInputEvidence,
    pub(super) tasks: CapturedTasks,
    pub(super) scheduling: CapturedScheduling,
    pub(super) current: CapturedCurrentReadFacts,
    pub(super) selector: SchedulerTaskSelectorMaterial,
    pub(super) event_window_role:
        Option<Arc<crate::runtime::current_event_queries::CurrentOrdinaryEventQueryLease>>,
    pub(super) own: (u64, usize),
    pub(super) additional: (u64, usize),
    pub(super) _state: Arc<BudgetViewGuard>,
    pub(super) _input: InputCaptureMaterial,
    pub(super) _task: TaskCaptureMaterial,
    pub(super) _scheduling: SchedulingCaptureMaterial,
}
impl SchedulerSelectedSources {
    pub(super) fn material_estimate(&self) -> (u64, usize) {
        self.own
    }
    pub(super) fn additional_material_estimate(&self) -> (u64, usize) {
        self.additional
    }
}
fn check(
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
    prior: (u64, usize),
    additional: (u64, usize),
) -> Result<()> {
    let configured = ledger.limits();
    let bytes = u64::try_from(limits.0.min(64 * 1024 * 1024))?.min(
        configured
            .history_per_agent_bytes
            .checked_add(configured.pending_per_agent_bytes)
            .context(CurrentReadError::ResourceExhausted)?,
    );
    let nodes = limits
        .1
        .min(65536)
        .min(usize::try_from(configured.record_limit)?);
    anyhow::ensure!(
        prior
            .0
            .checked_add(additional.0)
            .is_some_and(|n| n <= bytes)
            && prior
                .1
                .checked_add(additional.1)
                .is_some_and(|n| n <= nodes),
        CurrentReadError::ResourceExhausted
    );
    Ok(())
}

impl RuntimeHandle {
    pub(super) fn capture_scheduler_sources_from_snapshot_with_material(
        &self,
        root: &Arc<PostureObservation>,
        request: &WorkCaptureRequest,
        limits: (usize, usize),
        caller_prior: (u64, usize),
        loans: Option<&SchedulerReadBodyLoans<'_>>,
        event_window_role: Option<
            Arc<crate::runtime::current_event_queries::CurrentOrdinaryEventQueryLease>,
        >,
    ) -> Result<SchedulerSelectedSources> {
        // Preserve the original availability/identity order, before a scope
        // String, selector, State Arc or selected coverage allocation is copied.
        let identity = root.active_identity()?;
        self.inner.mutation_owner.input_queries()?;
        let work_domain = root
            .work
            .as_ref()
            .ok_or(crate::runtime::resident_work::WorkReadError::IncompleteLive)?;
        let input_domain = root.input_evidence.as_ref().ok_or_else(||
            crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(
                "resident input domain has not been prepared".into()))?;
        let task_domain = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        let scheduling_domain = root
            .scheduling
            .as_ref()
            .ok_or(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive)?;
        let current_domain = root
            .current_reads
            .as_ref()
            .ok_or(CurrentReadError::NeedsPreparation)?;
        let scope = task_domain.scope();
        let ledger = &self.inner.input_evidence_budget;
        anyhow::ensure!(
            root.token.owner_epoch == self.inner.mutation_owner.owner_epoch()
                && root.token.identity_incarnation == self.inner.identity_incarnation
                && scope.agent_id == self.inner.mutation_owner.agent_id()
                && scope.identity_incarnation == root.token.identity_incarnation
                && identity.agent_id == scope.agent_id
                && identity.incarnation == scope.identity_incarnation,
            "scheduler publication belongs to another owner"
        );
        anyhow::ensure!(
            work_domain.scope() == scope
                && input_domain.scope() == scope
                && scheduling_domain.scope() == scope
                && current_domain.scope() == scope
                && task_domain.shares_budget(ledger)
                && current_domain.uses_ledger(ledger),
            "scheduler published domains differ"
        );
        current_domain.validate_state(&root.state)?;
        current_domain.validate_identity(identity)?;
        work_domain.validate_capture_material_binding(ledger)?;
        input_domain.validate_capture_material_binding(ledger)?;
        task_domain.validate_capture_material_binding(ledger)?;
        scheduling_domain.validate_capture_material_binding(ledger)?;
        current_domain.validate_capture_material_binding(ledger)?;
        if let Some(loans) = loans {
            loans.validate(self, root, caller_prior)?;
        }
        let role = if let Some(role) = event_window_role.as_ref() {
            self.inner
                .mutation_owner
                .current_event_queries()?
                .validate_ordinary_lease(role, scope, root.token.owner_epoch, ledger)?;
            let cost = role.material_estimate();
            (u64::try_from(cost.0)?, cost.1)
        } else {
            (0, 0)
        };
        let prior = add_material(caller_prior, role)?;
        let (state_bytes, state_nodes) = borrowed_json_material(root.state.as_ref())?;
        let (scope_bytes, scope_nodes) = borrowed_json_material(scope)?;
        let own_state = (
            state_bytes
                .checked_add(
                    u64::try_from(state_nodes)?
                        .checked_mul(256)
                        .context(CurrentReadError::ResourceExhausted)?,
                )
                .and_then(|n| n.checked_add(scope_bytes.checked_mul(4)?))
                .and_then(|n| n.checked_add(8192))
                .context(CurrentReadError::ResourceExhausted)?,
            state_nodes
                .checked_add(
                    scope_nodes
                        .checked_mul(4)
                        .context(CurrentReadError::ResourceExhausted)?,
                )
                .and_then(|n| n.checked_add(64))
                .context(CurrentReadError::ResourceExhausted)?,
        );
        check(ledger, limits, prior, own_state)?;
        let state_guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: own_state.0,
            },
        )?;
        let state = root.state.clone();
        let mut additional = add_material(role, own_state)?;
        let mut standalone = additional;
        let (task_request, selector) = scheduler_task_request_with_material(
            scope,
            request,
            Some(task_domain.coverage()),
            ledger,
            limits,
            add_material(caller_prior, additional)?,
        )?;
        additional = add_material(additional, selector.material_estimate())?;
        standalone = add_material(standalone, selector.material_estimate())?;
        let work_prior = add_material(caller_prior, additional)?;
        let (work, work_usage) = work_domain.capture_with_prior(
            request,
            ledger,
            (usize::try_from(work_prior.0)?, work_prior.1),
            limits,
        )?;
        let own_work = (
            u64::try_from(work_usage.0)?
                .checked_sub(work_prior.0)
                .context("scheduler Work prior exceeds usage")?,
            work_usage
                .1
                .checked_sub(work_prior.1)
                .context("scheduler Work prior exceeds nodes")?,
        );
        additional = add_material(additional, own_work)?;
        standalone = add_material(standalone, own_work)?;
        let (inputs, input_material) = input_domain.capture_with_material(
            ledger,
            limits,
            add_material(caller_prior, additional)?,
            loans.map(|loan| &loan.source.inputs.input),
        )?;
        additional = add_material(additional, input_material.additional_material_estimate()?)?;
        standalone = add_material(standalone, input_material.material_estimate()?)?;
        let (tasks, task_material) = task_domain.capture_with_material(
            &task_request,
            ledger,
            limits,
            add_material(caller_prior, additional)?,
            loans.map(|loan| &loan.source.tasks),
        )?;
        additional = add_material(additional, task_material.additional_material_estimate()?)?;
        standalone = add_material(standalone, task_material.material_estimate()?)?;
        let (scheduling, scheduling_material) = scheduling_domain.capture_with_material(
            &SchedulingCaptureRequest::current(),
            ledger,
            limits,
            add_material(caller_prior, additional)?,
        )?;
        additional = add_material(additional, scheduling_material.material_estimate()?)?;
        standalone = add_material(standalone, scheduling_material.material_estimate()?)?;
        let mut current_request = CurrentReadRequest::current(limits.0, limits.1);
        current_request.include_pending_queue = false;
        if let Some(role) = &event_window_role {
            let (body, headers) = role.window_limits();
            current_request.event_limit = body;
            current_request.event_count_limit = headers;
        }
        let current = current_domain.capture_with_material(
            &current_request,
            add_material(caller_prior, additional)?,
            loans.map(|loan| &loan.source.events.selected),
        )?;
        additional = add_material(additional, current.additional_material_estimate()?)?;
        standalone = add_material(standalone, current.material_estimate()?)?;
        check(ledger, limits, caller_prior, additional)?;
        Ok(SchedulerSelectedSources {
            token: root.token,
            state,
            work,
            inputs,
            tasks,
            scheduling,
            current,
            selector,
            event_window_role,
            own: standalone,
            additional,
            _state: state_guard,
            _input: input_material,
            _task: task_material,
            _scheduling: scheduling_material,
        })
    }
}

#[cfg(test)]
#[path = "scheduler_read_sources_tests.rs"]
mod tests;
