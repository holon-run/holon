//! Explicit scheduler acquisition; pure decisions use resident Work/input facts.
use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
            OwnedBytesEstimate,
        },
        resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, InputWindowSelector,
            PreparedInputEvidence,
        },
        resident_work::{
            CanonicalWorkRecord, CapturedWork, WorkCaptureRequest, WorkFamily, WorkKey,
        },
        scheduler::{SchedulerAgentSnapshot, SchedulerObservations, SchedulerProjection},
        task_reads::active_task_records,
    },
    runtime_db::{
        context_inputs::ContextDurableRead, input_evidence_snapshot::InputEvidenceLoadRequest,
    },
};
use crate::{
    storage::WorkQueueReadModel,
    types::{AgentRegistryStatus, WaitConditionKind},
};
use std::collections::{BTreeMap, BTreeSet};

pub(in crate::runtime) struct PreparedSchedulerRead {
    material: (u64, usize),
    additional_material: (u64, usize),
    _sources: SchedulerSelectedMaterial,
    _trigger_budget: Arc<BudgetViewGuard>,
    _event_window_role:
        Option<Arc<super::super::current_event_queries::CurrentOrdinaryEventQueryLease>>,
    pub state: Arc<AgentState>,
    pub current: crate::runtime::current_read_facts::CapturedCurrentReadFacts,
    pub recent_brief_count: usize,
    pub(in crate::runtime) resource_stamp: crate::runtime::resource_refresh::WorkResourceReadStamp,
    pub projection: SchedulerProjection,
    pub work_queue: WorkQueueReadModel,
    pub work: CapturedWork,
    pub scheduling: super::super::resident_scheduling::CapturedScheduling,
    pub tasks: super::super::resident_task::CapturedTasks,
    pub due_rechecks: Vec<WorkItemRecord>,
    pub scheduling_posture: crate::types::AgentPostureProjection,
    pub active_waits: Vec<WaitConditionRecord>,
    pub advisories: Vec<super::super::scheduler::SchedulingAdvisory>,
    pub observability_briefs:
        Vec<Arc<super::super::resident_input_evidence::QualifiedInputEvidence>>,
    _brief_budget: Arc<BudgetViewGuard>,
    _task_selector_budget: Arc<BudgetViewGuard>,
    pub(in crate::runtime) _owned_budget: Arc<BudgetViewGuard>,
    pub(in crate::runtime) _task_owned_budget: Arc<BudgetViewGuard>,
}

struct SchedulerSelectedMaterial {
    _inputs: super::super::resident_input_evidence::CapturedInputEvidence,
    _state: Arc<BudgetViewGuard>,
    _input: super::super::resident_input_evidence::InputCaptureMaterial,
    _task: super::super::resident_task::TaskCaptureMaterial,
    _scheduling: super::super::resident_scheduling::SchedulingCaptureMaterial,
}
#[path = "scheduler_output_material.rs"]
mod output_material;

#[path = "scheduler_read_material.rs"]
mod material;
#[path = "scheduler_read_sources.rs"]
mod sources;
pub(in crate::runtime) use sources::SchedulerReadBodyLoans;
impl PreparedSchedulerRead {
    /// Retained selected domains and original owned/selector guards, no new charge.
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok(self.material)
    }
    pub(in crate::runtime) fn additional_material_estimate(&self) -> Result<(u64, usize)> {
        Ok(self.additional_material)
    }
    pub(in crate::runtime) fn summary_output_material_estimate(&self) -> Result<(u64, usize)> {
        material::summary_output(&self.state, &self.scheduling_posture, &self.active_waits)
    }
}

// The original command preparation still checks its durable adapter view.
// Ordinary child observability already holds one published owner root.
enum SchedulerAdapterRoot {
    Revalidate,
    #[cfg(test)]
    HeldPublication,
}

impl RuntimeMutation {
    #[inline(never)]
    pub(in crate::runtime) fn prepare_scheduler_read<'a>(
        &'a mut self,
        message: Option<&'a MessageEnvelope>,
        input_roots: BTreeSet<Key>,
        interrupted_sources: BTreeSet<String>,
        now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<PreparedSchedulerRead>> + Send + 'a>,
    > {
        self.prepare_scheduler_read_for_event_role(
            message,
            input_roots,
            interrupted_sources,
            now,
            None,
        )
    }

    /// The real continuation admission already holds its original owner.
    /// Register a finite read role here, never from an ordinary capture miss.
    pub(in crate::runtime) async fn prepare_scheduler_continuation_read(
        &mut self,
        now: chrono::DateTime<Utc>,
    ) -> Result<PreparedSchedulerRead> {
        let root = self.capture_published_root()?;
        root.active_identity()?;
        let scope = self.input_evidence_scope();
        let (_, records, bytes) = self.context_input_evidence_policy()?;
        let lease = self
            .runtime
            .inner
            .mutation_owner
            .current_event_queries()?
            .register_ordinary_window(
                &scope,
                self.reservations[self.primary_index].owner_epoch(),
                crate::runtime::current_read_facts::CURRENT_EVENT_WINDOW,
                crate::runtime::current_read_facts::CURRENT_EVENT_WINDOW,
                &self.runtime.inner.input_evidence_budget,
                (bytes, records),
            )?;
        drop(root);
        self.prepare_scheduler_read_for_event_role(
            None,
            Default::default(),
            Default::default(),
            now,
            Some(Arc::new(lease)),
        )
        .await
    }

    #[inline(never)]
    fn prepare_scheduler_read_for_event_role<'a>(
        &'a mut self,
        message: Option<&'a MessageEnvelope>,
        input_roots: BTreeSet<Key>,
        interrupted_sources: BTreeSet<String>,
        now: chrono::DateTime<Utc>,
        event_window_role: Option<
            Arc<super::super::current_event_queries::CurrentOrdinaryEventQueryLease>,
        >,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<PreparedSchedulerRead>> + Send + 'a>,
    > {
        Box::pin(self.prepare_scheduler_read_inner(
            message,
            input_roots,
            interrupted_sources,
            now,
            event_window_role,
        ))
    }

    async fn prepare_scheduler_read_inner(
        &mut self,
        message: Option<&MessageEnvelope>,
        input_roots: BTreeSet<Key>,
        interrupted_sources: BTreeSet<String>,
        now: chrono::DateTime<Utc>,
        event_window_role: Option<
            Arc<super::super::current_event_queries::CurrentOrdinaryEventQueryLease>,
        >,
    ) -> Result<PreparedSchedulerRead> {
        let version = self.runtime.inner.config_snapshot.load_full();
        let config = if version.provider_reconfig.is_some() {
            version.model_catalog.resolved_context_config(
                &version.base_context_config,
                self.state.model_override.as_ref(),
            )
        } else {
            version.base_context_config.clone()
        };
        let mut request = WorkCaptureRequest::current();
        for source in self.queue.shared_messages().map(Arc::as_ref).chain(message) {
            anyhow::ensure!(
                source.agent_id == self.state.id,
                "scheduler input scope differs"
            );
            request
                .required_trigger_message_ids
                .insert(source.id.clone());
            if source.kind == MessageKind::TaskResult {
                if let MessageOrigin::Task { task_id } = &source.origin {
                    if source.task_id.as_ref() == Some(task_id) {
                        request.required_task_ids.insert(task_id.clone());
                    }
                }
            }
        }
        if let Some(source) = message.filter(|source| source.kind == MessageKind::TaskResult) {
            if let Some(task_id) = &source.task_id {
                request.required_task_ids.insert(task_id.clone());
            }
        }
        // Queue payloads can still retain isolated legacy caller hints. Only
        // the effective current message and validated protocol roots name Work.
        if let Some(id) = message.and_then(|source| source.work_item_id.as_ref()) {
            request
                .mandatory_roots
                .insert(WorkKey::new(WorkFamily::Work, id));
        }
        if let Some(binding) = self.state.current_execution_binding.clone() {
            request.required_turn_ids.insert(binding.turn_id.clone());
            request
                .required_trigger_message_ids
                .insert(binding.source_message_id.clone());
            // Episode receipt recovery can preserve this binding while
            // invalidating Input/Work. Explicitly prepare its qualified source
            // before using that source to name a focused Task relation.
            let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
            let mut roots = BTreeSet::from([
                Key::new(Family::Message, &binding.source_message_id),
                Key::new(Family::Turn, &binding.turn_id),
            ]);
            if let Some(attempt) = &binding.activation_id {
                roots.insert(Key::new(Family::Attempt, attempt));
            }
            let binding_request = InputEvidenceLoadRequest {
                windows: Vec::new(),
                mandatory_roots: roots,
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                record_limit,
                byte_limit,
            };
            self.prepare_owner_input_request(&binding_request, config.recent_episode_candidates)
                .await?;
            let inputs = self.capture_input_evidence()?;
            let current = super::super::provider_recovery::captured_message(
                &inputs,
                &binding.source_message_id,
            )?
            .context("scheduler current source is unavailable")?;
            if current.kind == MessageKind::TaskResult {
                if let MessageOrigin::Task { task_id } = &current.origin {
                    if current.task_id.as_ref() == Some(task_id) {
                        request.required_task_ids.insert(task_id.clone());
                    }
                }
            }
        }
        for key in &input_roots {
            if key.family == Family::WorkExecution {
                request
                    .mandatory_roots
                    .insert(WorkKey::new(WorkFamily::Work, &key.id));
            }
        }
        {
            let (_, records, bytes) = self.context_input_evidence_policy()?;
            let (tasks, _selectors) = super::super::task_reads::scheduler_task_request(
                &self.input_evidence_scope(),
                &request,
                self.tasks.as_ref().map(|tasks| tasks.coverage()),
                &self.runtime.inner.input_evidence_budget,
                records,
                bytes,
            )?;
            // Explicit acquisition for this actual message's finite Task/Wait
            // addresses. Ordinary capture below never prepares on a miss.
            let (mut load, _live_queries) = self.owner_task_load_request()?;
            load.capture
                .required_keys
                .extend(tasks.required_keys.iter().cloned());
            self.prepare_owner_task_domain_request(&load).await?;
        }
        self.prepare_owner_work_request(&request).await?;
        let work = self.capture_work(&request)?;
        let (mut roots, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        roots.extend(input_roots);
        // Exact Work observations, including terminal or explicit absent execution,
        // must accompany every Work body used by canonical scheduler decisions.
        roots.extend(
            work.records
                .iter()
                .filter_map(|record| match &record.record {
                    CanonicalWorkRecord::Work(work) => {
                        Some(Key::new(Family::WorkExecution, &work.id))
                    }
                    _ => None,
                }),
        );
        for source in self.queue.shared_messages().map(Arc::as_ref).chain(message) {
            roots.insert(Key::new(Family::Message, &source.id));
        }
        if let Some(binding) = &self.state.current_execution_binding {
            roots.insert(Key::new(Family::Message, &binding.source_message_id));
            roots.insert(Key::new(Family::Turn, &binding.turn_id));
            if let Some(attempt) = &binding.activation_id {
                roots.insert(Key::new(Family::Attempt, attempt));
            }
        }
        // This is a scheduler preparation, not prompt capture. Invalid recovery
        // metadata must still reach the canonical rejection planner.
        let mut windows = Vec::new();
        super::super::scheduler::include_scheduler_brief_window(&mut windows);
        let input_request = InputEvidenceLoadRequest {
            windows,
            mandatory_roots: roots,
            interrupted_sources,
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        drop(work);
        self.prepare_owner_input_request(&input_request, config.recent_episode_candidates)
            .await?;
        match event_window_role {
            Some(role) => {
                let root = self.capture_published_root()?;
                capture_scheduler_root(&self.runtime, &root, &request, now, Some(role))
            }
            None => self.capture_scheduler_read(&request, now),
        }
    }

    /// Ordinary read: preparation debt remains a typed error. No Work/Input
    /// loader or historical lookup is hidden behind this capture.
    pub(in crate::runtime) fn capture_scheduler_read(
        &self,
        request: &WorkCaptureRequest,
        now: chrono::DateTime<Utc>,
    ) -> Result<PreparedSchedulerRead> {
        self.capture_scheduler_read_inner(request, now, SchedulerAdapterRoot::Revalidate)
    }

    #[cfg(test)]
    fn capture_child_observability_read(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<PreparedSchedulerRead> {
        self.capture_scheduler_read_inner(
            &WorkCaptureRequest::current(),
            now,
            SchedulerAdapterRoot::HeldPublication,
        )
    }

    fn capture_scheduler_read_inner(
        &self,
        request: &WorkCaptureRequest,
        now: chrono::DateTime<Utc>,
        adapter_root: SchedulerAdapterRoot,
    ) -> Result<PreparedSchedulerRead> {
        let root = self.capture_published_root()?;
        // Actual command preparation keeps the original durable OCC gate. The
        // ordinary held-owner entry below never executes this branch.
        if matches!(adapter_root, SchedulerAdapterRoot::Revalidate) {
            let scope = self.input_evidence_scope();
            let mut connection = self.runtime.inner.runtime_db.connection()?;
            let tx =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
            let facts = ContextDurableRead::new(&tx, &root.state.id).scope_facts()?;
            anyhow::ensure!(
                facts.state.as_ref() == Some(root.state.as_ref())
                    && facts.control_revision.is_some(),
                "scheduler adapter State changed"
            );
            let identity = facts
                .identity
                .context("scheduler adapter identity missing")?;
            anyhow::ensure!(
                identity.agent_id == scope.agent_id
                    && identity.incarnation == scope.identity_incarnation
                    && identity.status == AgentRegistryStatus::Active,
                "scheduler adapter identity differs"
            );
            let work = root
                .work
                .as_ref()
                .ok_or(super::super::resident_work::WorkReadError::IncompleteLive)?;
            let tasks = root
                .tasks
                .as_ref()
                .ok_or(super::super::resident_task::TaskReadError::IncompleteActive)?;
            crate::runtime_db::work_snapshot::validate_work_domain_observation_tx(
                &tx,
                &scope,
                work.coverage().revision,
            )?;
            crate::runtime_db::task_snapshot::validate_task_domain_observation_tx(
                &tx,
                &scope,
                tasks.coverage().revision,
            )?;
            tx.commit()?;
        }
        capture_scheduler_root(&self.runtime, &root, request, now, None)
    }
}

/// The caller holds the original reservation. This constructor selects only
/// this immutable publication; it cannot prepare, reconcile, or read SQL.
fn capture_scheduler_root(
    runtime: &RuntimeHandle,
    root: &Arc<super::super::mutation_owner::PostureObservation>,
    request: &WorkCaptureRequest,
    now: chrono::DateTime<Utc>,
    event_window_role: Option<
        Arc<super::super::current_event_queries::CurrentOrdinaryEventQueryLease>,
    >,
) -> Result<PreparedSchedulerRead> {
    capture_scheduler_root_with_material(
        runtime,
        root,
        request,
        now,
        (usize::MAX, usize::MAX),
        (0, 0),
        None,
        event_window_role,
    )
}

fn capture_scheduler_root_with_material(
    runtime: &RuntimeHandle,
    root: &Arc<super::super::mutation_owner::PostureObservation>,
    request: &WorkCaptureRequest,
    now: chrono::DateTime<Utc>,
    limits: (usize, usize),
    caller_prior: (u64, usize),
    loans: Option<&SchedulerReadBodyLoans<'_>>,
    event_window_role: Option<
        Arc<super::super::current_event_queries::CurrentOrdinaryEventQueryLease>,
    >,
) -> Result<PreparedSchedulerRead> {
    use crate::runtime::run_birth_sessions::add_material;
    let sources = runtime.capture_scheduler_sources_from_snapshot_with_material(
        root,
        request,
        limits,
        caller_prior,
        loans,
        event_window_role,
    )?;
    anyhow::ensure!(
        sources.token == root.token,
        "scheduler source publication changed"
    );
    let source_material = sources.material_estimate();
    let source_additional = sources.additional_material_estimate();
    let state = &sources.state;
    let work = &sources.work;
    let tasks = &sources.tasks;
    let inputs = &sources.inputs;
    let scheduling = &sources.scheduling;
    let current = &sources.current;
    let scope = &work.scope;
    let ledger = &runtime.inner.input_evidence_budget;
    let mut output = output_material::SchedulerOutputMaterial::new(
        ledger,
        scope,
        limits,
        add_material(caller_prior, source_additional)?,
    )?;
    let resource_stamp = runtime.capture_work_resource_read_stamp(root, state.as_ref(), work)?;
    let ProjectedSchedulerFields {
        projection,
        work_queue,
        due_rechecks,
        scheduling_posture,
        active_waits,
        advisories,
        observability_briefs,
        recent_brief_count,
        brief_budget,
        trigger_budget,
    } = project_selected_scheduler_with_output(
        ledger, state, work, tasks, inputs, scheduling, current, request, now, limits, &mut output,
    )?;
    let (output_material, budget) = output.finish();
    let material = add_material(source_material, output_material)?;
    let additional_material = add_material(source_additional, output_material)?;
    let sources::SchedulerSelectedSources {
        state,
        work,
        inputs,
        tasks,
        scheduling,
        current,
        selector,
        event_window_role,
        _state,
        _input,
        _task,
        _scheduling,
        ..
    } = sources;
    Ok(PreparedSchedulerRead {
        resource_stamp,
        _event_window_role: event_window_role,
        current,
        recent_brief_count,
        material,
        additional_material,
        state,
        projection,
        work_queue,
        work,
        scheduling,
        tasks,
        due_rechecks,
        scheduling_posture,
        active_waits,
        advisories,
        observability_briefs,
        _brief_budget: brief_budget,
        _task_selector_budget: selector.guard,
        _task_owned_budget: budget.clone(),
        _owned_budget: budget,
        _trigger_budget: trigger_budget,
        _sources: SchedulerSelectedMaterial {
            _inputs: inputs,
            _state,
            _input,
            _task,
            _scheduling,
        },
    })
}

/// Only already-qualified finite captures and their caller-owned admission
/// counter enter this original projection body. No Root/Runtime/DB is retained.
struct ProjectedSchedulerFields {
    projection: SchedulerProjection,
    work_queue: WorkQueueReadModel,
    due_rechecks: Vec<WorkItemRecord>,
    scheduling_posture: crate::types::AgentPostureProjection,
    active_waits: Vec<WaitConditionRecord>,
    advisories: Vec<super::super::scheduler::SchedulingAdvisory>,
    observability_briefs: Vec<Arc<super::super::resident_input_evidence::QualifiedInputEvidence>>,
    recent_brief_count: usize,
    brief_budget: Arc<BudgetViewGuard>,
    trigger_budget: Arc<BudgetViewGuard>,
}

// Shared by the loaded scheduler and the following qualified cold-child
// adapter. This is the original algorithm, not another selection policy.
fn project_selected_scheduler_with_output(
    ledger: &InputEvidenceBudgetLedger,
    state: &AgentState,
    work: &CapturedWork,
    tasks: &super::super::resident_task::CapturedTasks,
    inputs: &super::super::resident_input_evidence::CapturedInputEvidence,
    scheduling: &super::super::resident_scheduling::CapturedScheduling,
    current: &crate::runtime::current_read_facts::CapturedCurrentReadFacts,
    request: &WorkCaptureRequest,
    now: chrono::DateTime<Utc>,
    limits: (usize, usize),
    output: &mut output_material::SchedulerOutputMaterial<'_>,
) -> Result<ProjectedSchedulerFields> {
    let scope = &work.scope;
    let triggers =
        scheduling.clone_trigger_records_with_prior(ledger, limits, Some(output.prior()?))?;
    output.borrowed_original(&triggers.material, triggers.cost)?;
    let active_timers = scheduling.active_timer_count()?;
    let has_interrupted_replay = current.has_interrupted();
    let has_queued_input = current.has_queued();
    // The original helper builds borrowed refs and temporary exact Task keys.
    output.json(&tasks.coverage.active_task_ids)?;
    output.slots(tasks.coverage.active_task_ids.len().saturating_mul(4))?;
    let task_refs = active_task_records(tasks)?;
    output.json(&task_refs)?;
    let active_tasks = task_refs.into_iter().cloned().collect::<Vec<_>>();
    // Preserve the raw unresolved/Triggered predicate and the separate Active
    // Open-owner predicate. Their temporary keys/ref vectors are admitted first.
    for _ in 0..3 {
        output.json(&work.unresolved_wait_ids)?;
    }
    output.slots(work.unresolved_wait_ids.len().saturating_mul(4))?;
    for record in &work.records {
        if let CanonicalWorkRecord::Wait(wait) = &record.record {
            if work.unresolved_wait_ids.contains(&wait.id) {
                output.json(&wait.work_item_id)?;
            }
        }
    }
    let unresolved = work.unresolved_wait_conditions()?;
    output.json(&unresolved)?;
    for wait in &unresolved {
        output.json(&wait.id)?;
    }
    let mut activation_waits = unresolved
        .into_iter()
        .map(|wait| (wait.id.clone(), wait.clone()))
        .collect::<BTreeMap<_, _>>();
    let active = work.context_active_wait_conditions()?;
    output.json(&active)?;
    output.json(&active)?; // retained Vec and the original constructor's copy
    let active_waits = active.into_iter().cloned().collect::<Vec<_>>();
    for id in &request.required_task_ids {
        if let Some(ids) = work.task_wait_keys.get(id) {
            output.json(ids)?;
        }
        for wait in work.waits_for_task(id)? {
            if wait.status == WaitConditionStatus::Resolved
                && wait.kind == WaitConditionKind::Task
                && !activation_waits.contains_key(&wait.id)
            {
                output.json(&(wait, &wait.id))?;
                activation_waits.insert(wait.id.clone(), wait.clone());
            }
        }
    }
    for id in &request.required_trigger_message_ids {
        if let Some(ids) = work.trigger_wait_keys.get(id) {
            output.json(ids)?;
        }
        for wait in work.waits_triggered_by(id)? {
            if wait.status == WaitConditionStatus::Resolved
                && wait.kind == WaitConditionKind::Task
                && !activation_waits.contains_key(&wait.id)
            {
                output.json(&(wait, &wait.id))?;
                activation_waits.insert(wait.id.clone(), wait.clone());
            }
        }
    }
    let latest_error = current
        .events
        .iter()
        .filter_map(|record| match &record.fact {
            crate::runtime::current_read_facts::CurrentReadFact::Event(event)
                if event.kind == "runtime_error" =>
            {
                Some(event.created_at)
            }
            _ => None,
        })
        .max();
    let brief_limit = super::super::scheduler::SCHEDULER_BRIEF_LIMIT;
    // Arc slots, window capacity and this capture's new pin memberships; body
    // allocations remain the same selected Input Arcs, already admitted once.
    let window_count = inputs
        .windows
        .iter()
        .find(|window| {
            window.selector == InputWindowSelector::RecentBriefs && brief_limit <= window.limit
        })
        .ok_or(super::super::resident_input_evidence::InputEvidenceReadError::WindowNotPrepared)?
        .keys
        .len()
        .min(brief_limit);
    output.slots(window_count.saturating_mul(4).saturating_add(8))?;
    let briefs = inputs.window(&InputWindowSelector::RecentBriefs, brief_limit)?;
    let recent_brief_count = briefs.len().min(50);
    let latest_success = briefs
        .iter()
        .filter_map(|record| match &record.record {
            PreparedInputEvidence::Brief(brief) if brief.kind.is_success() => {
                Some(brief.created_at)
            }
            _ => None,
        })
        .max();
    let runtime_error =
        latest_error.is_some_and(|at| latest_success.is_none_or(|success| at > success));
    let work_queue = {
        let mut observer = |bytes, nodes| output.observe(bytes, nodes);
        work.work_queue_observed(state, triggers.records, &mut Some(&mut observer))?
    };
    output.json(&work.live_work_ids)?;
    output.slots(work.live_work_ids.len().saturating_mul(4))?;
    let due = work.due_work_rechecks(now)?;
    output.json(&due)?;
    let due_rechecks = due.into_iter().cloned().collect::<Vec<_>>();
    let execution_snapshot = {
        let mut observer = |bytes, nodes| output.observe(bytes, nodes);
        inputs.execution_state_if_initialized_observed(&mut Some(&mut observer))?
    };
    output.slots(activation_waits.len().saturating_mul(2))?;
    let mut activation_waits = activation_waits.into_values().collect::<Vec<_>>();
    activation_waits.sort_by(|left, right| left.id.cmp(&right.id));
    let scheduling_posture = {
        let mut observer = |bytes, nodes| output.observe(bytes, nodes);
        crate::storage::derive_agent_posture_observed(
            state,
            has_queued_input,
            &work_queue,
            &active_waits,
            &mut Some(&mut observer),
        )?
    };
    output.queue(&work_queue)?;
    output.json(&(
        &state.id,
        &state.current_run_id,
        state
            .last_turn_terminal
            .as_ref()
            .map(|terminal| &terminal.kind),
    ))?;
    let snapshot = SchedulerAgentSnapshot::from_state(state);
    let projection = {
        let mut observer = |bytes, nodes| output.observe(bytes, nodes);
        SchedulerProjection::from_observations_observed(
            &snapshot,
            usize::try_from(
                current
                    .queue_counts
                    .get("queued")
                    .copied()
                    .unwrap_or(0)
                    .checked_add(
                        current
                            .queue_counts
                            .get("interrupted")
                            .copied()
                            .unwrap_or(0),
                    )
                    .context("scheduler pending queue count overflow")?,
            )?,
            work_queue.clone(),
            SchedulerObservations {
                active_tasks,
                has_interrupted_replay,
                active_wait_conditions: active_waits.clone(),
                activation_waits,
                execution_snapshot,
                active_timers,
                runtime_error,
            },
            now,
            &mut Some(&mut observer),
        )?
    };
    let advisories = {
        let mut observer = |bytes, nodes| output.observe(bytes, nodes);
        super::super::scheduler::scheduling_advisories_for_facts_observed(
            state,
            &projection,
            &scheduling_posture,
            &work_queue,
            &active_waits,
            &mut Some(&mut observer),
        )?
    };
    let mut observability_briefs = briefs;
    let omitted = observability_briefs.len().saturating_sub(16);
    observability_briefs.drain(..omitted);
    let brief_budget =
        ledger.register_view(scope, InputMemoryCategory::Pins, &observability_briefs)?;
    Ok(ProjectedSchedulerFields {
        projection,
        work_queue,
        due_rechecks,
        scheduling_posture,
        active_waits,
        advisories,
        observability_briefs,
        recent_brief_count,
        brief_budget,
        trigger_budget: triggers.material,
    })
}

impl RuntimeHandle {
    /// Pure same-publication selection. All caller loans remain with the caller;
    /// only actual same-Arc bodies are discounted, never new DTOs or metadata.
    pub(in crate::runtime) fn capture_scheduler_read_from_snapshot_with_material(
        &self,
        root: &Arc<super::super::mutation_owner::PostureObservation>,
        request: &WorkCaptureRequest,
        now: chrono::DateTime<Utc>,
        limits: (usize, usize),
        caller_prior: (u64, usize),
        loans: Option<&SchedulerReadBodyLoans<'_>>,
    ) -> Result<PreparedSchedulerRead> {
        capture_scheduler_root_with_material(
            self,
            root,
            request,
            now,
            limits,
            caller_prior,
            loans,
            None,
        )
    }

    pub(in crate::runtime) fn capture_scheduler_read_from_owner(
        &self,
        owner: &mutation_owner::MutationReservation,
        request: &WorkCaptureRequest,
        now: chrono::DateTime<Utc>,
    ) -> Result<(
        Arc<super::super::resident_state::AgentSnapshot>,
        PreparedSchedulerRead,
    )> {
        owner.validate_owner(&self.inner.mutation_owner)?;
        let root = owner
            .capture_ready_snapshot()?
            .ok_or(crate::runtime::current_read_facts::CurrentReadError::RetiredScope)?;
        let read = capture_scheduler_root(self, &root, request, now, None)?;
        Ok((root, read))
    }
    #[inline(never)]
    pub(in crate::runtime) fn capture_scheduler_projection(
        &self,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<PreparedSchedulerRead>> + Send + '_>,
    > {
        Box::pin(self.capture_scheduler_projection_inner())
    }

    async fn capture_scheduler_projection_inner(&self) -> Result<PreparedSchedulerRead> {
        let owner = self.inner.mutation_owner.reserve().await?;
        self.capture_scheduler_read_from_owner(&owner, &WorkCaptureRequest::current(), self.now())
            .map(|(_, read)| read)
    }

    #[inline(never)]
    pub(in crate::runtime) fn capture_child_observability_projection(
        &self,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<PreparedSchedulerRead>> + Send + '_>,
    > {
        Box::pin(async move {
            // No preparation or SQL root refresh: State/Task/Work/Input are
            // selected from this one Ready publication under its actual owner.
            let owner = self.inner.mutation_owner.reserve().await?;
            self.capture_scheduler_read_from_owner(
                &owner,
                &WorkCaptureRequest::current(),
                self.now(),
            )
            .map(|(_, read)| read)
        })
    }
}

#[cfg(test)]
#[path = "work_planning_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "scheduler_projection_material_tests.rs"]
mod projection_material_tests;

#[cfg(test)]
#[path = "scheduler_selected_projector_tests.rs"]
mod selected_projector_tests;
