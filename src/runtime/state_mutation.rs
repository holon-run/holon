//! Serialized private candidates and exact post-commit runtime installation.

use super::{
    mutation_owner::{IdentityMutationFacts, KnownCommit, MutationReservation, PublicationToken},
    *,
};
use crate::runtime_db::{persistence_certainty, PersistenceCertainty};

#[path = "state_mutation/prepared_evidence.rs"]
mod prepared_evidence;

#[path = "state_mutation/work_planning.rs"]
mod work_planning;
pub(crate) use work_planning::{
    derive_scheduler_read, ColdSchedulerFacts, PreparedSchedulerRead, SchedulerReadDomains,
};

#[path = "state_mutation/work_preparation.rs"]
mod work_preparation;

#[path = "state_mutation/scheduling_preparation.rs"]
mod scheduling_preparation;
#[path = "state_mutation/task_output.rs"]
mod task_output;
#[path = "state_mutation/task_preparation.rs"]
mod task_preparation;
#[path = "state_mutation/timer_preparation.rs"]
mod timer_preparation;
#[path = "state_mutation/wait_preparation.rs"]
mod wait_preparation;
pub(in crate::runtime) use wait_preparation::WaitSchedulingRead;

#[path = "state_mutation/task_results.rs"]
mod task_results;
#[path = "state_mutation/task_stop_memory.rs"]
mod task_stop_memory;
#[path = "state_mutation/task_stop_readonly.rs"]
mod task_stop_readonly;
#[path = "state_mutation/task_stops.rs"]
mod task_stops;
pub(crate) use task_stop_memory::attach_original_child_terminal_memory_to_owner;
pub(crate) use task_stop_memory::attach_original_command_terminal_memory_to_owner;
pub(crate) use task_stop_memory::attach_task_stop_phase_memory_to_original_owner;
#[path = "state_mutation/task_transitions.rs"]
mod task_transitions;
pub(in crate::runtime) use task_stop_readonly::InitialTaskStopReadonlyMember;

#[path = "state_mutation/brief_images.rs"]
mod brief_images;
mod idle_input;
mod input_recovery;
mod live_input;
#[path = "state_mutation/scheduler_audit.rs"]
mod scheduler_audit;
#[path = "state_mutation/turn_barrier.rs"]
mod turn_barrier;
#[path = "state_mutation/turn_inputs.rs"]
mod turn_inputs;
#[path = "state_mutation/work_transitions.rs"]
mod work_transitions;

#[derive(Debug, thiserror::Error)]
#[error("prepared input operation already has its original receipt")]
struct PreparedInputReplay(
    crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
    Vec<Arc<super::input_evidence_budget::BudgetViewGuard>>,
);

#[derive(Debug, thiserror::Error)]
#[error("agent mutation identity incarnation is no longer current: agent {agent_id}, expected incarnation {expected_incarnation}, actual incarnation {actual_incarnation}, status {status:?}")]
pub(super) struct RuntimeMutationIdentityRetired {
    agent_id: String,
    expected_incarnation: u64,
    actual_incarnation: u64,
    status: crate::types::AgentRegistryStatus,
}

#[derive(Default)]
pub(super) struct OwnedMutationTasks {
    closed: bool,
    drain: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl OwnedMutationTasks {
    pub(super) fn all_finished(&self) -> bool {
        self.handles.iter().all(|task| task.is_finished())
            && self
                .drain
                .try_lock()
                .is_ok_and(|tasks| tasks.iter().all(|task| task.is_finished()))
    }
}

#[derive(Debug, Clone)]
struct DeferredRunAbort {
    handle: CurrentRunAbortHandle,
    reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedControlCancellation {
    pub(super) key: crate::runtime_db::control_transition::ControlKey,
    pub(super) digest: String,
    pub(super) owner_epoch: u64,
    pub(super) facts: Arc<super::control::PreparedControlCancellation>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeMutationFacts {
    owner_epoch: u64,
    pub(crate) state: Arc<AgentState>,
    episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
    input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    work: Option<Arc<super::resident_work::ResidentWorkDomain>>,
    tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
    scheduling: Option<Arc<super::resident_scheduling::ResidentSchedulingDomain>>,
    queue: RuntimeQueue,
    pub(crate) current: Option<Arc<crate::runtime_db::current_read_writes::CommittedCurrentWrites>>,
    _queue_budget_view: Option<Arc<super::input_evidence_budget::BudgetViewGuard>>,
    _readonly_queue_metadata: Option<Arc<super::input_evidence_budget::BudgetViewGuard>>,
    current_run_abort: Option<CurrentRunAbortHandle>,
    deferred_abort: Option<DeferredRunAbort>,
    control_cancellation: Option<SavedControlCancellation>,
    effects: crate::runtime_db::transitions::PostCommitEffects,
    recovered_install:
        Option<Arc<crate::runtime_db::input_recovery_install::InputRecoveryInstallFacts>>,
    plan_preparation: Option<Arc<super::resource_refresh::WorkPlanPreparation>>,
}

impl RuntimeMutationFacts {
    pub(super) fn finish_plan_preparation(&self) {
        if let Some(preparation) = &self.plan_preparation {
            preparation.source_terminal();
        }
    }

    /// Only the extra container clone owned by binding facts, not shared bodies.
    pub(crate) fn original_child_binding_queue_material(
        &self,
    ) -> Result<crate::runtime_db::task_snapshot::TaskMaintenanceMaterial> {
        anyhow::ensure!(
            self._readonly_queue_metadata.is_some(),
            "binding facts have no owned queue guard"
        );
        readonly_queue_material(self.queue.len())
    }
    pub(in crate::runtime) fn for_task_status_metadata_recovery(
        &self,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
    ) -> Result<Self> {
        anyhow::ensure!(
            tasks
                .as_ref()
                .is_none_or(|t| t.scope().agent_id == self.state.id),
            "Status recovery Task scope differs"
        );
        let mut facts = self.clone();
        facts.tasks = tasks;
        facts.effects = Default::default();
        facts.deferred_abort = None;
        facts.control_cancellation = None;
        facts.recovered_install = None;
        Ok(facts)
    }
    pub(in crate::runtime) fn task_status_metadata_known(self: &Arc<Self>) -> KnownCommit {
        KnownCommit::State {
            state: self.state.clone(),
            episodes: self.episodes.clone(),
            input_evidence: self.input_evidence.clone(),
            work: self.work.clone(),
            tasks: self.tasks.clone(),
            scheduling: self.scheduling.clone(),
            runtime: Some(self.clone()),
        }
    }

    pub(crate) fn for_cold_external_input(
        owner_epoch: u64,
        state: Arc<AgentState>,
        episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    ) -> Self {
        Self {
            owner_epoch,
            state,
            episodes,
            input_evidence,
            work: None,
            tasks: None,
            scheduling: None,
            current: None,
            queue: RuntimeQueue::default(),
            _queue_budget_view: None,
            plan_preparation: None,
            _readonly_queue_metadata: None,
            current_run_abort: None,
            deferred_abort: None,
            control_cancellation: None,
            effects: Default::default(),
            recovered_install: None,
        }
    }

    pub(crate) fn with_external_input_commit(
        mut self,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
        work: Option<Arc<super::resident_work::ResidentWorkDomain>>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
        scheduling: Option<Arc<super::resident_scheduling::ResidentSchedulingDomain>>,
        effects: crate::runtime_db::transitions::PostCommitEffects,
    ) -> Result<Self> {
        if let Some(inputs) = &input_evidence {
            anyhow::ensure!(
                inputs.scope().agent_id == self.state.id,
                "external input commit changed its agent scope"
            );
        }
        anyhow::ensure!(
            work.as_ref()
                .is_none_or(|work| work.scope().agent_id == self.state.id),
            "external Work commit scope differs"
        );
        anyhow::ensure!(
            tasks.as_ref().is_none_or(|tasks| {
                tasks.scope().agent_id == self.state.id
                    && input_evidence
                        .as_ref()
                        .is_none_or(|inputs| inputs.scope() == tasks.scope())
                    && work
                        .as_ref()
                        .is_none_or(|work| work.scope() == tasks.scope())
            }),
            "external Task commit scope differs"
        );
        anyhow::ensure!(
            scheduling
                .as_ref()
                .is_none_or(|domain| domain.scope().agent_id == self.state.id
                    && input_evidence
                        .as_ref()
                        .is_none_or(|inputs| inputs.scope() == domain.scope())
                    && work
                        .as_ref()
                        .is_none_or(|work| work.scope() == domain.scope())
                    && tasks
                        .as_ref()
                        .is_none_or(|tasks| tasks.scope() == domain.scope())),
            "external Scheduling commit scope differs"
        );
        self.scheduling = scheduling;
        self.input_evidence = input_evidence;
        self.work = work;
        self.tasks = tasks;
        // Cold/external installation does not attest the old Current body cut.
        self.current = None;
        self.recovered_install = None;
        self.effects = effects;
        strip_input_evidence_payloads(&mut self.effects);
        Ok(self)
    }

    pub(crate) fn external_effects(&self) -> &crate::runtime_db::transitions::PostCommitEffects {
        &self.effects
    }
    pub(super) fn for_input_recovery(
        owner_epoch: u64,
        state: Arc<AgentState>,
        episodes: Arc<super::resident_state::ResidentEpisodes>,
        inputs: Arc<super::resident_input_evidence::ResidentInputEvidence>,
        work: Arc<super::resident_work::ResidentWorkDomain>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
        scheduling: Arc<super::resident_scheduling::ResidentSchedulingDomain>,
        messages: Vec<MessageEnvelope>,
        budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        original: Option<&Self>,
        outcome: &crate::runtime_db::input_mutation_receipts::InputMutationScopeOutcome,
        applied: bool,
        identity: Option<crate::types::AgentIdentityRecord>,
        cache: crate::runtime_db::input_recovery_install::InputRecoveryInstallFacts,
    ) -> Result<Self> {
        anyhow::ensure!(
            outcome.scope.agent_id == state.id
                && inputs.belongs_to(&outcome.scope)
                && work.scope() == &outcome.scope
                && tasks
                    .as_ref()
                    .is_none_or(|tasks| tasks.scope() == &outcome.scope)
                && scheduling.scope() == &outcome.scope
                && cache.scope == outcome.scope
                && cache.keys == outcome.install_keys,
            "input recovery scope differs"
        );
        let mut queue = RuntimeQueue::default();
        for message in messages {
            anyhow::ensure!(message.agent_id == state.id, "input recovery queue differs");
            queue.push(message);
        }
        let queue_view = budget.register_queue_view(
            &outcome.scope,
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            queue.shared_messages(),
        )?;
        let current_run_abort = original
            .filter(|original| original.owner_epoch == owner_epoch)
            .and_then(|original| original.current_run_abort.clone())
            .filter(|handle| state.current_run_id.as_ref() == Some(&handle.run_id));
        Ok(Self {
            owner_epoch,
            state,
            episodes: Some(episodes),
            input_evidence: Some(inputs),
            work: Some(work),
            tasks,
            scheduling: Some(scheduling),
            current: None,
            queue,
            _queue_budget_view: Some(queue_view),
            _readonly_queue_metadata: None,
            current_run_abort,
            deferred_abort: applied
                .then(|| original.and_then(|original| original.deferred_abort.clone()))
                .flatten(),
            control_cancellation: original
                .and_then(|original| original.control_cancellation.clone()),
            plan_preparation: original.and_then(|original| original.plan_preparation.clone()),
            recovered_install: Some(Arc::new(cache)),
            effects: crate::runtime_db::transitions::PostCommitEffects {
                audit_events: outcome.audit_events.clone(),
                notify_memory_index: outcome.notify_memory_index,
                notify_scheduler: outcome.notify_scheduler,
                agent_identities: identity.into_iter().collect(),
                ..Default::default()
            },
        })
    }

    pub(in crate::runtime) fn with_task_stop_queue_metadata(
        mut self,
        guard: Arc<super::input_evidence_budget::BudgetViewGuard>,
    ) -> Self {
        self._readonly_queue_metadata = Some(guard);
        self
    }
    #[cfg(test)]
    pub(crate) fn task_stop_queue_metadata_test_weak(
        &self,
    ) -> Option<std::sync::Weak<super::input_evidence_budget::BudgetViewGuard>> {
        self._readonly_queue_metadata.as_ref().map(Arc::downgrade)
    }
    pub(in crate::runtime) fn for_unchanged_task_stop_partial(&self) -> Result<Self> {
        anyhow::ensure!(
            self.tasks.is_none(),
            "metadata-only Stop cannot discard a prepared Task domain"
        );
        let mut facts = self.clone();
        facts.effects = Default::default();
        facts.deferred_abort = None;
        facts.control_cancellation = None;
        facts.recovered_install = None;
        Ok(facts)
    }
    pub(in crate::runtime) fn partial_task_stop_known(self: &Arc<Self>) -> KnownCommit {
        KnownCommit::State {
            state: self.state.clone(),
            episodes: self.episodes.clone(),
            input_evidence: self.input_evidence.clone(),
            work: self.work.clone(),
            tasks: None,
            scheduling: self.scheduling.clone(),
            runtime: Some(self.clone()),
        }
    }

    pub(super) fn suppress_observed_input_replay_publication(&mut self) {
        // Fresh current installation is still necessary. The new invocation
        // cannot reissue old notifications or signal a newly prepared abort.
        self.effects.audit_events.clear();
        self.effects.notify_memory_index = false;
        self.effects.notify_scheduler = false;
        self.effects.fault = None;
        self.deferred_abort = None;
        self.control_cancellation = None;
    }

    pub(super) fn for_control_recovery(
        owner_epoch: u64,
        state: Arc<AgentState>,
        episodes: Arc<super::resident_state::ResidentEpisodes>,
        domains: crate::runtime_db::control_transition::ControlRecoveryDomains,
        budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        facts: crate::runtime_db::control_transition::ControlRecoveryFacts,
        original_run: Option<CurrentRunAbortHandle>,
        cancellation: Option<SavedControlCancellation>,
    ) -> Result<Self> {
        anyhow::ensure!(
            domains.work.scope().agent_id == state.id
                && domains.inputs.belongs_to(domains.work.scope())
                && domains.tasks.scope() == domains.work.scope()
                && domains.scheduling.scope() == domains.work.scope()
                && episodes.belongs_to(&state.id, domains.work.scope().identity_incarnation),
            "Control recovery domains changed namespace"
        );
        anyhow::ensure!(
            facts
                .workspace_occupancy
                .as_ref()
                .is_none_or(|occupancy| occupancy.holder_agent_id == state.id),
            "Control recovery occupancy scope differs"
        );
        tracing::debug!(agent_id = %state.id, waits = facts.waits.len(),
            timer_wakes = facts.timer_wakes.len(),
            workspace_occupancy = facts.workspace_occupancy.is_some(),
            "installing coherent Control recovery facts");
        let mut queue = RuntimeQueue::default();
        for message in facts.current_queue_messages {
            anyhow::ensure!(
                message.agent_id == state.id,
                "Control recovery queue scope differs"
            );
            queue.push(message);
        }
        let charges = queue
            .shared_messages()
            .map(|message| {
                Ok(super::input_evidence_budget::PendingBodyCharge {
                    message_id: message.id.clone(),
                    body_bytes: super::input_evidence_budget::serialized_bytes(&message.body)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        budget.install_actual_pending(domains.work.scope(), &charges)?;
        let queue_view = budget.register_queue_view(
            domains.work.scope(),
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            queue.shared_messages(),
        )?;
        let current_run_abort =
            original_run.filter(|handle| state.current_run_id.as_ref() == Some(&handle.run_id));
        let mut effects = *facts.transition.effects;
        strip_input_evidence_payloads(&mut effects);
        Ok(Self {
            owner_epoch,
            state,
            episodes: Some(episodes),
            input_evidence: Some(domains.inputs),
            work: Some(domains.work),
            tasks: Some(domains.tasks),
            scheduling: Some(domains.scheduling),
            current: None,
            queue,
            _queue_budget_view: Some(queue_view),
            _readonly_queue_metadata: None,
            current_run_abort,
            deferred_abort: None,
            control_cancellation: cancellation,
            effects,
            recovered_install: None,
            plan_preparation: None,
        })
    }
}

pub(super) struct RuntimeMutation {
    runtime: RuntimeHandle,
    reservations: Vec<MutationReservation>,
    // Declaration order emits the wake after the reservation releases.
    _resource_owner_wake: super::resource_refresh::ResourceOwnerWake,
    primary_index: usize,
    metadata: Vec<Option<Arc<IdentityMutationFacts>>>,
    task_stop_readonly: Vec<Arc<InitialTaskStopReadonlyMember>>,
    episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
    input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    input_state_decode: Option<crate::runtime_db::context_inputs::ContextStateDecode>,
    work: Option<Arc<super::resident_work::ResidentWorkDomain>>,
    tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
    scheduling: Option<Arc<super::resident_scheduling::ResidentSchedulingDomain>>,
    candidate: Box<RuntimeAgent>,
    current: Option<Arc<crate::runtime_db::current_read_writes::CommittedCurrentWrites>>,
    base: Box<RuntimeAgent>,
    deferred_abort: Option<DeferredRunAbort>,
    control_cancellation: Option<SavedControlCancellation>,
    measurement_started: Option<std::time::Instant>,
    _candidate_budget_views: Vec<Arc<super::input_evidence_budget::BudgetViewGuard>>,
    plan_preparation: Option<Arc<super::resource_refresh::WorkPlanPreparation>>,
}

impl std::ops::Deref for RuntimeMutation {
    type Target = RuntimeAgent;

    fn deref(&self) -> &Self::Target {
        &self.candidate
    }
}

impl std::ops::DerefMut for RuntimeMutation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.candidate
    }
}

impl RuntimeMutation {
    pub(super) fn context_state_decode(
        &self,
    ) -> Option<&crate::runtime_db::context_inputs::ContextStateDecode> {
        self.input_state_decode.as_ref()
    }

    pub(super) fn attach_plan_preparation(
        &mut self,
        preparation: Arc<super::resource_refresh::WorkPlanPreparation>,
    ) -> Result<()> {
        preparation.validate_scope(&self.input_evidence_scope())?;
        self.plan_preparation = Some(preparation);
        Ok(())
    }

    pub(super) fn input_evidence_scope(
        &self,
    ) -> super::resident_input_evidence::InputEvidenceScope {
        let reservation = &self.reservations[self.primary_index];
        super::resident_input_evidence::InputEvidenceScope {
            agent_id: reservation.agent_id().to_owned(),
            identity_incarnation: reservation.identity_incarnation(),
        }
    }

    pub(in crate::runtime) fn validate_original_command_source_scope(
        &self,
        scope: &super::resident_input_evidence::InputEvidenceScope,
        owner_epoch: u64,
    ) -> Result<()> {
        let reservation = &self.reservations[self.primary_index];
        reservation.validate_owner(&self.runtime.inner.mutation_owner)?;
        anyhow::ensure!(
            reservation.agent_id() == scope.agent_id
                && reservation.identity_incarnation() == scope.identity_incarnation
                && reservation.owner_epoch() == owner_epoch,
            "Command registration original scope/owner differs"
        );
        Ok(())
    }

    /// Non-context planners derive their default policy once at this explicit
    /// boundary. Loaded context supplies its already frozen request directly.
    pub(super) async fn prepare_owner_inputs(
        &mut self,
        extra_mandatory_roots: std::collections::BTreeSet<
            super::resident_input_evidence::InputEvidenceKey,
        >,
        interrupted_sources: std::collections::BTreeSet<String>,
    ) -> Result<()> {
        let version = self.runtime.inner.config_snapshot.load_full();
        let config = if version.provider_reconfig.is_some() {
            version.model_catalog.resolved_context_config(
                &version.base_context_config,
                self.candidate.state.model_override.as_ref(),
            )
        } else {
            version.base_context_config.clone()
        };
        let model = version
            .model_catalog
            .effective_model(self.candidate.state.model_override.as_ref());
        let history = super::turn::context_history_preparation(&self.candidate.state.id, &model);
        let (mut roots, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        roots.extend(extra_mandatory_roots);
        let mut request = super::context_capture::context_input_evidence_request(
            &self.candidate.state,
            None,
            &config,
            history,
            roots,
            record_limit,
            byte_limit,
        )?;
        request.interrupted_sources = interrupted_sources;
        self.prepare_owner_input_request(&request, config.recent_episode_candidates)
            .await
    }

    /// Same-reservation preparation preserves the caller's policy and the
    /// runtime's fixed scheduler dependency. It never reselects a model.
    #[inline(never)]
    pub(super) fn prepare_owner_input_request<'a>(
        &'a mut self,
        request: &'a crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        recent_episode_limit: usize,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.prepare_owner_input_request_inner(request, recent_episode_limit, None, None)
                .await
                .map(|_| ())
        })
    }

    #[inline(never)]
    pub(super) fn prepare_owner_input_request_with_material<'a>(
        &'a mut self,
        request: &'a crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        recent_episode_limit: usize,
        material: &'a crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.prepare_owner_input_request_inner(
                request,
                recent_episode_limit,
                None,
                Some(material),
            )
            .await
            .map(|_| ())
        })
    }

    async fn prepare_owner_input_request_inner(
        &mut self,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        recent_episode_limit: usize,
        scheduling_capture: Option<&super::resident_scheduling::SchedulingCaptureRequest>,
        material: Option<&crate::diagnostics::resource_io::ResourceReadMaterial>,
    ) -> Result<()> {
        use super::input_preparation::{
            prepare_reserved_input_domains_with_scheduling, InputPreparation, InputPreparationCause,
        };
        use super::resident_input_evidence::InputEvidenceFamily as Family;
        let root = self
            .runtime
            .inner
            .mutation_owner
            .capture()?
            .context("input preparation requires a published owner root")?;
        anyhow::ensure!(
            root.state.as_ref() == &self.candidate.state,
            "input preparation cannot publish a private candidate"
        );
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        anyhow::ensure!(
            request.record_limit > 0
                && request.record_limit <= record_limit
                && request.byte_limit > 0
                && request.byte_limit <= byte_limit,
            "input preparation request exceeds the held owner's budget"
        );
        let input_roles = self.owner_input_query_roles()?;
        if let Some(query) = &request.query {
            anyhow::ensure!(
                query
                    .ledger
                    .same_ledger(&self.runtime.inner.input_evidence_budget),
                "Input query preparation changed owner ledger"
            );
        }
        // Owner expansion is a selector union, not a new owning read. Reuse a
        // request already covering the owner; only actual growth needs a clone.
        let query_covered = request.query.as_ref().map_or_else(
            || {
                input_roles.selection
                    == super::resident_input_evidence::InputQuerySelection::default()
            },
            |query| {
                input_roles
                    .selection
                    .required_keys
                    .is_subset(&query.selection.required_keys)
                    && input_roles.selection.birth_after.is_none_or(|after| {
                        query.selection.birth_after.is_some_and(|old| old <= after)
                    })
            },
        );
        let state = &self.candidate.state;
        let owner_covered =
            query_covered
                && request.windows.iter().any(|window| {
                    window.selector
                        == super::resident_input_evidence::InputWindowSelector::RecentBriefs
                        && window.limit >= super::scheduler::SCHEDULER_BRIEF_LIMIT
                })
                && self.input_evidence.as_ref().is_none_or(|inputs| {
                    inputs.mandatory_roots().is_subset(&request.mandatory_roots)
                })
                && self
                    .candidate
                    .queue
                    .shared_messages()
                    .all(|message| request.contains_root(Family::Message, &message.id))
                && state
                    .current_turn_id
                    .as_ref()
                    .is_none_or(|turn| request.contains_root(Family::Turn, turn))
                && state
                    .current_execution_binding
                    .as_ref()
                    .is_none_or(|binding| {
                        request.contains_root(Family::Message, &binding.source_message_id)
                            && request.contains_root(Family::Turn, &binding.turn_id)
                            && binding.activation_id.as_ref().is_none_or(|attempt| {
                                request.contains_root(Family::Attempt, attempt)
                            })
                            && binding.work_item_id.as_ref().is_none_or(|work| {
                                request.contains_root(Family::WorkExecution, work)
                            })
                    });
        let request = if owner_covered {
            std::borrow::Cow::Borrowed(request)
        } else {
            let _resource_io = material.map(|material| material.enter());
            crate::diagnostics::resource_io::admit_cloned_value(request)?;
            let mut request = request.clone();
            // Replacing Input must not invalidate the scheduler on this root.
            if !request.windows.iter().any(|window| {
                window.selector == super::resident_input_evidence::InputWindowSelector::RecentBriefs
            }) {
                crate::runtime_db::repositories::reserve_source_vec_slot(&mut request.windows)?;
            }
            super::scheduler::include_scheduler_brief_window(&mut request.windows);
            if request.query.is_some()
                || input_roles.selection
                    != super::resident_input_evidence::InputQuerySelection::default()
            {
                let query = request.query.get_or_insert_with(|| {
                    super::resident_input_evidence::InputQueryRead {
                        selection: Default::default(),
                        ledger: self.runtime.inner.input_evidence_budget.clone(),
                    }
                });
                anyhow::ensure!(
                    query
                        .ledger
                        .same_ledger(&self.runtime.inner.input_evidence_budget),
                    "Input query preparation changed owner ledger"
                );
                for key in &input_roles.selection.required_keys {
                    if !query.selection.required_keys.contains(key) {
                        crate::diagnostics::resource_io::admit_cloned_value(key)?;
                        query.selection.required_keys.insert(key.clone());
                    }
                }
                if let Some(after) = input_roles.selection.birth_after {
                    query.selection.birth_after = Some(
                        query
                            .selection
                            .birth_after
                            .map_or(after, |old| old.min(after)),
                    );
                }
            }
            if let Some(inputs) = &self.input_evidence {
                request.extend_admitted_roots(|| {
                    inputs
                        .mandatory_roots()
                        .iter()
                        .map(|key| (key.family, key.id.as_str()))
                })?;
            }
            request.extend_admitted_roots(|| {
                self.candidate
                    .queue
                    .shared_messages()
                    .map(|message| (Family::Message, message.id.as_str()))
            })?;
            if let Some(turn) = &self.candidate.state.current_turn_id {
                request.insert_admitted_root(Family::Turn, turn)?;
            }
            if let Some(binding) = &self.candidate.state.current_execution_binding {
                request.insert_admitted_root(Family::Message, &binding.source_message_id)?;
                request.insert_admitted_root(Family::Turn, &binding.turn_id)?;
                if let Some(attempt) = &binding.activation_id {
                    request.insert_admitted_root(Family::Attempt, attempt)?;
                }
                if let Some(work) = &binding.work_item_id {
                    request.insert_admitted_root(Family::WorkExecution, work)?;
                }
            }
            std::borrow::Cow::Owned(request)
        };
        // Owner is held; this chooses an explicit preparation policy only.
        // The preparation transaction revalidates exact scope and State.
        let scope = self.input_evidence_scope();
        let deleting = self
            .runtime
            .inner
            .runtime_db
            .agent_identities()
            .latest_for_mutation(&scope.agent_id)?
            .is_some_and(|identity| {
                identity.incarnation == scope.identity_incarnation
                    && identity.status == crate::types::AgentRegistryStatus::Deleting
            });
        let cause = if deleting {
            InputPreparationCause::ExistingAdmissionCleanup
        } else if self.input_evidence.is_none() {
            InputPreparationCause::ValidatedRecovery
        } else {
            InputPreparationCause::OwnerChange
        };
        let default_scheduling = super::resident_scheduling::SchedulingCaptureRequest::current();
        let current_roles = self.runtime.inner.current_queries.snapshot(
            &scope,
            self.reservations[self.primary_index].owner_epoch(),
            self.current
                .as_ref()
                .and_then(|writes| writes.query.as_ref())
                .map(|query| &query.selection),
            &self.runtime.inner.input_evidence_budget,
        )?;
        let prepare_current = current_roles.has_live_roles()
            || self
                .current
                .as_ref()
                .is_some_and(|writes| writes.query.is_some());
        let prepared = {
            let _resource_io = material.map(|material| material.enter());
            prepare_reserved_input_domains_with_scheduling(
                &self.runtime.inner.runtime_db,
                &self.reservations[self.primary_index],
                &root,
                cause,
                &request,
                recent_episode_limit,
                &self.runtime.inner.input_evidence_budget,
                scheduling_capture.unwrap_or(&default_scheduling),
                prepare_current.then_some(&current_roles),
            )?
        };
        let InputPreparation::Prepared(prepared) = prepared else {
            return Ok(());
        };
        anyhow::ensure!(
            prepared.original_token == root.token
                && prepared.facts.state.as_deref() == Some(prepared.state.as_ref()),
            "prepared input installation changed State"
        );
        self.episodes = Some(prepared.episodes);
        self.input_evidence = Some(prepared.inputs);
        self.input_state_decode = prepared.decoded_state;
        self.current = prepared.current;
        self.scheduling = Some(prepared.scheduling);
        self.candidate.queue = prepared.queue;
        self.candidate._queue_budget_view = Some(prepared.queue_budget_view);
        let facts = Arc::new(self.facts(Default::default()));
        let reservation = &mut self.reservations[self.primary_index];
        reservation.begin(crate::ids::audit_event_id())?;
        MutationReservation::committed_all(
            std::slice::from_mut(reservation),
            vec![KnownCommit::State {
                state: facts.state.clone(),
                episodes: facts.episodes.clone(),
                input_evidence: facts.input_evidence.clone(),
                work: facts.work.clone(),
                tasks: facts.tasks.clone(),
                scheduling: facts.scheduling.clone(),
                runtime: Some(facts.clone()),
            }],
        )?;
        // Save exact installation facts synchronously before the first await.
        self.runtime.install_mutation_facts(&facts).await?;
        let observations = MutationReservation::publish_all_known(std::slice::from_mut(
            &mut self.reservations[self.primary_index],
        ))?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.runtime.inner.resources.notify();
        self.base = self.candidate.clone();
        Ok(())
    }

    pub(super) fn capture_input_evidence(
        &self,
    ) -> Result<super::resident_input_evidence::CapturedInputEvidence> {
        self.input_evidence.as_ref().ok_or_else(|| {
            crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(
                "resident input domain has not been prepared".into())
        })?.capture()
    }

    fn owner_input_query_roles(&self) -> Result<super::input_query_roles::InputQueryRoles> {
        let (_, records, bytes) = self.context_input_evidence_policy()?;
        self.runtime.inner.input_queries.snapshot(
            &self.input_evidence_scope(),
            self.reservations[self.primary_index].owner_epoch(),
            &self.runtime.inner.input_evidence_budget,
            (records, bytes),
        )
    }

    fn owner_input_query_source(
        &self,
    ) -> Result<Option<super::resident_input_evidence::OriginalInputQuerySource>> {
        let roles = self.owner_input_query_roles()?;
        if let Some(inputs) = &self.input_evidence {
            inputs.original_query_source_for_roles(roles)
        } else {
            anyhow::ensure!(
                roles.selection == super::resident_input_evidence::InputQuerySelection::default(),
                super::resident_input_evidence::InputQueryError::NeedsPreparation
            );
            Ok(None)
        }
    }

    fn owner_current_query_source(
        &self,
    ) -> Result<Option<Arc<crate::runtime_db::current_query::CurrentQuerySource>>> {
        self.runtime.inner.current_queries.source(
            &self.input_evidence_scope(),
            self.reservations[self.primary_index].owner_epoch(),
            self.current
                .as_ref()
                .and_then(|writes| writes.query.as_ref()),
            &self.runtime.inner.input_evidence_budget,
        )
    }

    pub(super) async fn prepare_current_query(
        &mut self,
        selection: Option<&crate::runtime_db::current_query::CurrentSelection>,
    ) -> Result<()> {
        let scope = self.input_evidence_scope();
        let (_, records, bytes) = self.context_input_evidence_policy()?;
        let root = self
            .runtime
            .inner
            .mutation_owner
            .capture()?
            .context("Current preparation root is unpublished")?;
        anyhow::ensure!(
            root.state.as_ref() == &self.candidate.state,
            "Current preparation cannot publish private State"
        );
        let roles = self.runtime.inner.current_queries.snapshot(
            &scope,
            self.reservations[self.primary_index].owner_epoch(),
            selection,
            &self.runtime.inner.input_evidence_budget,
        )?;
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::new(
            &self.runtime.inner.input_evidence_budget,
            &scope,
        )?;
        let query = material.read(|| {
            self.runtime.inner.runtime_db.transaction(|tx| {
                crate::runtime_db::current_query::prepare(
                    tx,
                    &scope,
                    Some(roles.selection()),
                    &self.runtime.inner.input_evidence_budget,
                    (bytes, records),
                )
            })
        })?;
        self.current = Some(Arc::new(
            crate::runtime_db::current_read_writes::CommittedCurrentWrites::prepared_query(query),
        ));
        let facts = Arc::new(self.facts(Default::default()));
        let reservation = &mut self.reservations[self.primary_index];
        reservation.begin(crate::ids::audit_event_id())?;
        MutationReservation::committed_all(
            std::slice::from_mut(reservation),
            vec![KnownCommit::State {
                state: facts.state.clone(),
                episodes: facts.episodes.clone(),
                input_evidence: facts.input_evidence.clone(),
                work: facts.work.clone(),
                tasks: facts.tasks.clone(),
                scheduling: facts.scheduling.clone(),
                runtime: Some(facts.clone()),
            }],
        )?;
        self.runtime.install_mutation_facts(&facts).await?;
        let observations = MutationReservation::publish_all_known(std::slice::from_mut(
            &mut self.reservations[self.primary_index],
        ))?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.base = self.candidate.clone();
        Ok(())
    }

    pub(super) fn context_input_evidence_policy(
        &self,
    ) -> Result<(
        std::collections::BTreeSet<super::resident_input_evidence::InputEvidenceKey>,
        usize,
        usize,
    )> {
        let limits = self.runtime.inner.input_evidence_budget.limits();
        let bytes = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("input evidence byte limit overflow")?;
        Ok((
            Default::default(),
            usize::try_from(limits.record_limit)?,
            usize::try_from(bytes)?,
        ))
    }

    #[cfg(test)]
    pub(super) fn capture_context_with_inputs(
        &self,
        episode_limit: usize,
        request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    ) -> Result<super::resident_state::CapturedAgentContext> {
        let root = self.capture_published_root()?;
        let mut captured = root.capture_context(episode_limit)?;
        captured.input_evidence = Some(root.input_evidence.as_ref().ok_or_else(|| {
            crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(
                "resident input domain has not been prepared".into())
        })?.capture_for_request(request)?);
        Ok(captured)
    }

    pub(super) fn defer_abort_current_run(&mut self, reason: impl Into<String>) {
        self.deferred_abort =
            self.candidate
                .current_run_abort
                .clone()
                .map(|handle| DeferredRunAbort {
                    handle,
                    reason: reason.into(),
                });
    }

    #[inline(never)]
    pub(super) fn persist_state<'a>(
        &'a mut self,
        _storage: &AppStorage,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.persist_state_inner(None, None, None))
    }

    pub(super) async fn persist_state_with_started_turn(&mut self, turn: TurnRecord) -> Result<()> {
        self.persist_state_inner(Some(turn), None, None).await
    }

    pub(super) async fn persist_started_dispatch(
        &mut self,
        turn: TurnRecord,
        start: crate::runtime_db::dispatch_memory::DispatchMemoryStart,
        original_claim: Option<super::started_dispatch::OriginalDispatchClaim>,
    ) -> Result<()> {
        Box::pin(self.persist_state_inner(Some(turn), Some(start), original_claim)).await
    }

    async fn persist_state_inner(
        &mut self,
        started_turn: Option<TurnRecord>,
        dispatch_start: Option<crate::runtime_db::dispatch_memory::DispatchMemoryStart>,
        original_claim: Option<super::started_dispatch::OriginalDispatchClaim>,
    ) -> Result<()> {
        self.validate_candidate_identity()?;
        if self.candidate.state == self.base.state && started_turn.is_none() {
            return self.install_transient().await;
        }
        let started = std::time::Instant::now();
        let event = AuditEvent::legacy(
            "agent_state_mutated",
            serde_json::json!({
                "agent_id": self.candidate.state.id,
            }),
        );
        let command = crate::runtime_db::transitions::AgentPostureTransitionCommand {
            agent_id: self.candidate.state.id.clone(),
            expected_incarnation: Some(self.runtime.inner.identity_incarnation),
            agent_state: crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(self.base.last_persisted_state.clone())),
                record: Box::new(self.candidate.state.clone()),
            },
            audit_events: vec![event.clone()],
            fault: self.runtime.take_transition_fault(),
        };
        let intended_state = command.agent_state.record.clone();
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::posture(
            self.input_evidence_scope(),
            event.id,
            Utc::now(),
            command,
            started_turn,
        )?;
        let prepared = match dispatch_start {
            Some(start) => prepared.with_dispatch_memory(
                crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Started {
                    start: Box::new(start),
                },
            )?,
            None => prepared,
        };
        let prepared = match original_claim {
            Some(claim) => prepared.with_original_dispatch_claim(claim)?,
            None => prepared,
        };
        let result = self.commit_prepared_input_transition(prepared).await;
        crate::diagnostics::record_storage_persist_state(started.elapsed());
        let commit = result?;
        if !commit.applied && intended_state.as_ref() != &self.base.last_persisted_state {
            return Err(
                crate::runtime_db::RuntimeStateTransitionConflict::concurrent_mutation(
                    "agent_state",
                    &self.candidate.state.id,
                )
                .into(),
            );
        }
        self.runtime.apply_transition_commit(commit).await;
        Ok(())
    }

    // The closure adapts an existing typed repository command. It cannot
    // observe or mutate the live RuntimeAgent while SQL is in progress.
    #[inline(never)]
    pub(super) fn commit_transition<'a>(
        &'a mut self,
        operation_id: String,
        commit: impl FnOnce(&super::resident_input_evidence::InputEvidenceScope) -> Result<TransitionCommit>
            + Send
            + 'a,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        // Keep transaction future construction out of nested runtime polls.
        let scope = self.input_evidence_scope();
        Box::pin(self.commit_transition_inner(operation_id, None, move || commit(&scope)))
    }

    pub(super) async fn commit_prepared_queue(
        &mut self,
        operation_id: String,
        mut queue: crate::runtime_db::transitions::PreparedQueueTransition,
    ) -> Result<TransitionCommit> {
        use crate::runtime_db::transitions::QueueMutation;
        let entry = match &queue.command.mutation {
            QueueMutation::Consume(entry)
            | QueueMutation::Upsert(entry)
            | QueueMutation::SettleWithProviderRecovery { record: entry, .. }
            | QueueMutation::CompareAndSet { record: entry, .. }
            | QueueMutation::ParkProviderDispatch { record: entry, .. }
            | QueueMutation::ConsumeSettledTaskResultRecovery { record: entry, .. } => entry,
        };
        if matches!(
            entry.status,
            QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
        ) && !queue
            .command
            .message_evidence
            .iter()
            .any(|message| message.id == entry.message_id)
        {
            let message = if let Some(message) = self
                .candidate
                .queue
                .peek_next_matching(|message| message.id == entry.message_id)
                .or_else(|| {
                    self.base
                        .queue
                        .peek_next_matching(|message| message.id == entry.message_id)
                }) {
                message.clone()
            } else {
                use super::resident_input_evidence::{
                    InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
                };
                let captured = self.capture_input_evidence()?;
                let record = captured
                    .lookup(&Key::new(Family::Message, &entry.message_id))?
                    .context("pending mutation canonical Message was pruned")?;
                let PreparedInputEvidence::Message(message) = &record.record else {
                    bail!("pending Message has a different family");
                };
                message.clone()
            };
            queue.command.message_evidence.push(message);
        }
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::queue(
            self.input_evidence_scope(),
            operation_id,
            Utc::now(),
            queue,
        )?;
        self.commit_prepared_input_transition(prepared).await
    }

    pub(super) async fn commit_prepared_terminal(
        &mut self,
        operation_id: String,
        command: crate::runtime_db::transitions::TurnTerminalTransitionCommand,
    ) -> Result<TransitionCommit> {
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::terminal(
            self.input_evidence_scope(),
            operation_id,
            Utc::now(),
            command,
        )?;
        self.commit_prepared_input_transition(prepared).await
    }

    pub(super) async fn append_original_audit_event(&mut self, event: AuditEvent) -> Result<()> {
        self.runtime.inner.storage.event_log().ensure_writable()?;
        anyhow::ensure!(
            self.state == self.last_persisted_state,
            "independent Audit requires the original acknowledged State"
        );
        let scope = self.input_evidence_scope();
        if let Some(event) = self
            .runtime
            .inner
            .runtime_db
            .audit_events()
            .append_retiring_observation(&scope, &event)?
        {
            // Existing cleanup may observe retirement, never admit a new body.
            if let Err(error) = self
                .runtime
                .inner
                .storage
                .event_log()
                .publish_committed_event(Some(scope.agent_id), &event)
            {
                tracing::warn!(error = %error, event_id = %event.id, "failed to publish retiring Audit observation");
            }
            return Ok(());
        }
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::audit(
            scope,
            event.id.clone(),
            Utc::now(),
            event,
            self.runtime.take_transition_fault(),
        )?;
        let commit = self.commit_prepared_input_transition(prepared).await?;
        self.runtime.apply_transition_commit(commit).await;
        Ok(())
    }

    pub(super) async fn append_original_audit_events(
        &mut self,
        events: Vec<AuditEvent>,
    ) -> Result<()> {
        self.runtime.inner.storage.event_log().ensure_writable()?;
        anyhow::ensure!(
            self.state == self.last_persisted_state,
            "independent Audit requires the original acknowledged State"
        );
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::audit_many(
            self.input_evidence_scope(),
            crate::ids::audit_event_id(),
            Utc::now(),
            events,
            self.runtime.take_transition_fault(),
        )?;
        let commit = self.commit_prepared_input_transition(prepared).await?;
        self.runtime.apply_transition_commit(commit).await;
        Ok(())
    }

    /// Freeze original command/targets before SQL. A receipt replay supplies no
    /// historical installation candidate and is resolved from a fresh read view.
    #[inline(never)]
    pub(super) fn commit_prepared_input_transition<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        Box::pin(async move {
            anyhow::ensure!(
                prepared.descriptor().family()
                    != crate::runtime_db::input_mutation_receipts::InputMutationFamily::Task,
                "Task receipt requires its typed runtime adapter"
            );
            anyhow::ensure!(
                prepared.descriptor().family()
                    != crate::runtime_db::input_mutation_receipts::InputMutationFamily::ExternalTrigger,
                "Trigger receipt requires its typed runtime adapter"
            );
            let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
            anyhow::ensure!(
                matches!(
                    result,
                    crate::runtime_db::input_mutation_receipts::InputMutationResult::Transition
                ),
                "transition receipt has another result family"
            );
            Ok(commit)
        })
    }

    pub(in crate::runtime) fn scheduling_writer_request(
        &self,
    ) -> Result<crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest> {
        let work = self.owner_work_load_request()?;
        Ok(
            crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
                capture: super::resident_scheduling::SchedulingCaptureRequest::current(),
                record_limit: work.record_limit,
                byte_limit: work.byte_limit,
            },
        )
    }
    pub(super) fn commit_prepared_external_trigger_transition<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(
                        TransitionCommit,
                        crate::runtime_db::transitions::ExternalTriggerMutationOutcome,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            anyhow::ensure!(prepared.descriptor().family()==
                crate::runtime_db::input_mutation_receipts::InputMutationFamily::ExternalTrigger,
                "Trigger receipt requires its typed runtime adapter");
            let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
            let crate::runtime_db::input_mutation_receipts::InputMutationResult::ExternalTrigger {
                outcome,
            } = result
            else {
                anyhow::bail!("Trigger receipt has another result family");
            };
            Ok((commit, *outcome))
        })
    }

    pub(super) async fn commit_context_budget_stop_recovery(
        &mut self,
        command: crate::domain::execution_protocol::RecheckContextBudgetStop,
        audit_events: Vec<AuditEvent>,
    ) -> Result<
        crate::runtime_db::transitions::execution_protocol_repository::ExecutionProtocolRecoveryCommitOutcome,
    >{
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::context_budget_stop_recovery(
                self.input_evidence_scope(),
                crate::ids::audit_event_id(),
                Utc::now(),
                command,
                audit_events,
            )?;
        let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
        let crate::runtime_db::input_mutation_receipts::InputMutationResult::ContextBudgetStopRecovery {
            outcome,
        } = result
        else {
            anyhow::bail!("context budget recovery receipt has another result family");
        };
        self.runtime.apply_transition_commit(commit).await;
        Ok(outcome)
    }

    pub(super) fn commit_prepared_timer_transition<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(
                        TransitionCommit,
                        crate::runtime_db::transitions::TimerMutationOutcome,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            anyhow::ensure!(
                prepared.descriptor().family()
                    == crate::runtime_db::input_mutation_receipts::InputMutationFamily::Timer,
                "Timer receipt requires its typed runtime adapter"
            );
            let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
            let crate::runtime_db::input_mutation_receipts::InputMutationResult::Timer { outcome } =
                result
            else {
                anyhow::bail!("Timer receipt has another result family");
            };
            Ok((commit, *outcome))
        })
    }

    pub(super) fn commit_prepared_queue_progress<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(
                        TransitionCommit,
                        Option<crate::runtime_db::transitions::QueueHeadNoProgressOutcome>,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
            let crate::runtime_db::input_mutation_receipts::InputMutationResult::QueueHeadNoProgress { outcome } = result else {
                bail!("queue progress receipt has another result family");
            };
            Ok((commit, outcome))
        })
    }

    #[inline(never)]
    fn commit_prepared_input_with_result<'a>(
        &'a mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(
                        TransitionCommit,
                        crate::runtime_db::input_mutation_receipts::InputMutationResult,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(self.commit_prepared_input_transition_inner(prepared))
    }

    fn attach_actual_dispatch_memory(
        &self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<crate::runtime_db::transitions::PreparedInputTransition> {
        use crate::runtime_db::dispatch_memory::{
            load_dispatch_memory_binding_by_turn_tx, load_dispatch_memory_binding_tx,
        };
        let scope = self.input_evidence_scope();
        if let Some(ack) = prepared.stopped_provider_pause_ack() {
            let memory = self.runtime.inner.runtime_db.transaction(|tx| {
                crate::runtime_db::provider_admission::load_stopped_provider_pause_memory_tx(
                    tx,
                    &ack,
                    &scope,
                    &self.runtime.inner.input_evidence_budget,
                )
            })?;
            return prepared.with_stopped_provider_pause_memory(memory);
        }
        self.runtime.attach_dispatch_memory_with_binding(prepared, |tx, prepared| {
            if let Some(turn_id) = prepared.actual_memory_terminal_turn_id() {
                if let Some(source) = prepared
                    .claim_only_queue_terminal_source(&self.base.last_persisted_state)
                {
                    let state = &self.base.last_persisted_state;
                    let run_id = state.current_run_id.as_deref().context("claim run missing")?;
                    let lease = self.runtime.inner.control_cancellations.run_for(run_id)
                        .context("claim-only terminal has no original processing lease")?;
                    anyhow::ensure!(lease.source_message_id() == source,
                        "claim-only terminal differs from its original source");
                    let actual: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM queue_entries q JOIN messages m ON m.message_id=q.message_id AND m.agent_id=q.agent_id JOIN agent_states s ON s.agent_id=q.agent_id WHERE q.agent_id=?1 AND q.message_id=?2 AND q.status='dequeued' AND json_extract(q.payload_json,'$.status')='dequeued' AND s.current_run_id=?3 AND json_extract(s.payload_json,'$.current_run_id')=?3 AND json_extract(s.payload_json,'$.current_turn_id') IS NULL AND json_extract(s.payload_json,'$.current_execution_binding') IS NULL AND s.turn_index=?4) AND NOT EXISTS (SELECT 1 FROM turn_records WHERE turn_id=?5) AND NOT EXISTS (SELECT 1 FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?6 AND state!='done')",
                        rusqlite::params![scope.agent_id, source, run_id, state.turn_index, turn_id, scope.identity_incarnation],
                        |row| row.get(0),
                    )?;
                    anyhow::ensure!(actual,"claim-only terminal has an actual Turn or changed canonical claim");
                    return Ok(None);
                }
                load_dispatch_memory_binding_by_turn_tx(tx, &scope, turn_id)
            } else if let Some(turn_id) = prepared
                .inactive_queue_memory_turn_id(&self.base.last_persisted_state)
                .filter(|_| self.current_run_abort.is_none())
            {
                // Bootstrap has no physical producer. Ordinary business
                // commands still require their exact live run below.
                let state = &self.base.last_persisted_state;
                let unchanged: bool = tx.query_row(
                    "SELECT payload_json=?2 FROM agent_states WHERE agent_id=?1",
                    rusqlite::params![scope.agent_id, serde_json::to_string(state)?],
                    |row| row.get(0),
                )?;
                anyhow::ensure!(unchanged, "bootstrap memory original State changed");
                load_dispatch_memory_binding_by_turn_tx(tx, &scope, turn_id)
            } else {
                // Work and Focus candidates may already change fields. The
                // original published State, never that candidate, locates the seed.
                load_dispatch_memory_binding_tx(tx, &scope, &self.base.last_persisted_state)
            }
        })
    }

    async fn commit_prepared_input_transition_inner(
        &mut self,
        prepared: crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<(
        TransitionCommit,
        crate::runtime_db::input_mutation_receipts::InputMutationResult,
    )> {
        use super::input_evidence_budget::{serialized_bytes, PendingAdmission};
        use crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt;
        use crate::runtime_db::transitions::InputMutationExecution;
        self.validate_candidate_identity()?;
        let scopes = self
            .reservations
            .iter()
            .map(
                |reservation| super::resident_input_evidence::InputEvidenceScope {
                    agent_id: reservation.agent_id().to_owned(),
                    identity_incarnation: reservation.identity_incarnation(),
                },
            )
            .collect::<Vec<_>>();
        let prepared = prepared.with_scope_group(scopes)?;
        self.validate_initial_readonly_stop(&prepared)?;
        let prepared = self.attach_actual_dispatch_memory(prepared)?;
        let _owned = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(
                &self.input_evidence_scope(),
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: prepared.owned_payload_bytes()?,
                    metadata_estimate_bytes: 0,
                },
            )?;
        let mut saved_runtime = vec![Arc::new(self.facts(Default::default()))];
        for (index, reservation) in self.reservations.iter().enumerate() {
            if index == self.primary_index {
                continue;
            }
            if let Some(member) = self
                .task_stop_readonly
                .iter()
                .find(|member| member.target().scope.agent_id == reservation.agent_id())
            {
                if let Some(facts) = member.runtime() {
                    saved_runtime.push(facts.clone());
                }
                continue;
            }
            if let Some(bridge) = &self.runtime.inner.host_bridge {
                if let Some(runtime) = bridge.runtime_for_mutation(reservation).await? {
                    let state = self.metadata[index]
                        .as_ref()
                        .and_then(|facts| facts.state.clone())
                        .context("related loaded runtime has no State")?;
                    saved_runtime.push(Arc::new(
                        runtime
                            .prepare_external_input_facts(
                                reservation,
                                state,
                                reservation.prepared_episodes_for_state_only_commit()?,
                                reservation.prepared_inputs_for_state_only_commit()?,
                            )
                            .await?,
                    ));
                }
            }
        }
        let work_request = self.owner_work_load_request()?;
        let (task_request, _task_request_budget) = self.owner_task_load_request()?;
        let ledger = self.runtime.inner.input_evidence_budget.clone();
        let input_query_source = self.owner_input_query_source()?;
        let current_query_source = self.owner_current_query_source()?;
        let mut pending = Vec::new();
        for message in prepared.pending_messages()? {
            match ledger.reserve_pending(
                &self.input_evidence_scope(),
                &message.id,
                serialized_bytes(&message.body)?,
            ) {
                Ok(PendingAdmission::Reserved(ticket)) => pending.push((ledger.clone(), ticket)),
                Ok(PendingAdmission::ExistingAccepted) => {}
                Err(error) => {
                    for (ledger, ticket) in &pending {
                        ledger.definitely_not_committed(ticket)?;
                    }
                    return Err(error.into());
                }
            }
        }
        let operation_id = prepared.descriptor().command_nonce().to_owned();
        let pending_cleanup = pending.clone();
        let witness = match super::mutation_witness::MutationWitness::input_mutation(
            operation_id.clone(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            pending,
        )
        .and_then(|witness| witness.with_input_runtime(saved_runtime))
        .and_then(|witness| {
            witness.with_initial_task_stop_readonly(self.task_stop_readonly.clone())
        }) {
            Ok(witness) => witness,
            Err(error) => {
                for (ledger, ticket) in &pending_cleanup {
                    ledger.definitely_not_committed(ticket)?;
                }
                return Err(error);
            }
        };
        let db = self.runtime.inner.runtime_db.clone();
        let scope = self.input_evidence_scope();
        let replay_observation = witness.clone();
        let mut original_result = None;
        let mut original_material_views = Vec::new();
        let result = self
            .commit_transition_inner(operation_id, Some(witness), || {
                match db
                    .transitions_in_scope(&scope)
                    .commit_prepared_transition_with_input_query(
                        &prepared,
                        &work_request,
                        &ledger,
                        &task_request,
                        input_query_source.as_ref(),
                        current_query_source.as_ref(),
                    )? {
                    InputMutationExecution::Committed { value, receipt } => {
                        replay_observation.mark_original_input_receipt_pending(&receipt, false)?;
                        original_material_views
                            .extend(value.effects.task_material_budget_views.iter().cloned());
                        original_material_views.extend(
                            value
                                .effects
                                .scheduling_material_budget_views
                                .iter()
                                .cloned(),
                        );
                        original_result = Some(receipt.outcome.result);
                        Ok(value)
                    }
                    InputMutationExecution::Replayed {
                        receipt,
                        _material_budget_views,
                    } => {
                        replay_observation.mark_input_storage_replayed()?;
                        Err(PreparedInputReplay(receipt, _material_budget_views).into())
                    }
                }
            })
            .await;
        match result {
            Err(error) if error.downcast_ref::<PreparedInputReplay>().is_some() => {
                let replay = error
                    .downcast::<PreparedInputReplay>()
                    .expect("checked replay");
                let PreparedInputReplay(receipt, replay_material) = replay;
                self.runtime
                    .reconcile_reserved_mutations(&mut self.reservations)
                    .await?;
                self.candidate = Box::new(self.runtime.inner.agent.lock().await.clone());
                self.base = self.candidate.clone();
                let root = self.runtime.inner.mutation_owner.capture()?;
                self.episodes = root.as_ref().and_then(|root| root.episodes.clone());
                self.input_evidence = root.as_ref().and_then(|root| root.input_evidence.clone());
                self.work = root.as_ref().and_then(|root| root.work.clone());
                self.tasks = root.and_then(|root| root.tasks.clone());
                self.deferred_abort = None;
                Ok((
                    TransitionCommit {
                        applied: receipt.outcome.applied,
                        delivery_receipt: receipt.outcome.delivery_receipt,
                        effects: Box::new(crate::runtime_db::transitions::PostCommitEffects {
                            task_material_budget_views: replay_material,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    receipt.outcome.result,
                ))
            }
            result => result.and_then(|mut commit| {
                commit
                    .effects
                    .task_material_budget_views
                    .extend(original_material_views);
                Ok((
                    commit,
                    original_result.context("input transaction omitted its original result")?,
                ))
            }),
        }
    }

    pub(super) async fn commit_input_evidence<T: Send>(
        &mut self,
        operation_id: String,
        commit: impl FnOnce(
                &super::resident_input_evidence::InputEvidenceScope,
            )
                -> Result<crate::runtime_db::input_evidence_writes::InputEvidenceCommit<T>>
            + Send,
    ) -> Result<T> {
        let mut record = None;
        let committed = self
            .commit_transition(operation_id, |scope| {
                let acknowledged = commit(scope)?;
                record = Some(acknowledged.record);
                Ok(TransitionCommit {
                    applied: true,
                    effects: Box::new(crate::runtime_db::transitions::PostCommitEffects {
                        current_read_writes: acknowledged.current_read_writes,
                        agent_state: acknowledged.agent_state,
                        input_evidence_delta: acknowledged.input_delta,
                        work_delta: acknowledged.work_delta,
                        work_delta_requires_prepared_domain: true,
                        task_delta: acknowledged.task_delta,
                        task_delta_requires_prepared_domain: true,
                        task_material_budget_views: acknowledged.task_material_budget_views,
                        notify_memory_index: true,
                        notify_scheduler: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            })
            .await?;
        self.runtime.apply_transition_commit(committed).await;
        record.context("input evidence commit omitted its acknowledged result")
    }

    async fn commit_transition_inner(
        &mut self,
        operation_id: String,
        witness: Option<super::mutation_witness::MutationWitness>,
        commit: impl FnOnce() -> Result<TransitionCommit>,
    ) -> Result<TransitionCommit> {
        self.validate_candidate_identity()?;
        let prior_publications = self
            .reservations
            .iter()
            .map(MutationReservation::capture_ready_snapshot)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let input_witness = witness.as_ref().filter(|witness| {
            witness.kind() == super::mutation_witness::MutationWitnessKind::InputMutation
        });
        let keys_result = match witness.as_ref() {
            Some(witness) => MutationReservation::begin_all_with_witness(
                &mut self.reservations,
                operation_id,
                witness.clone(),
            ),
            None => MutationReservation::begin_all(&mut self.reservations, operation_id),
        };
        let keys = match keys_result {
            Ok(keys) => keys,
            Err(error) => {
                if let Some(witness) = input_witness {
                    witness.release_pending_not_committed(false)?;
                }
                return Err(error.into());
            }
        };
        if let Some(witness) = input_witness {
            if let Err(error) = witness.mark_pending_started() {
                witness.release_pending_not_committed(false)?;
                MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
                return Err(error);
            }
        }
        let result = commit();
        // The synchronous attempt has ended even when its COMMIT ACK is lost.
        // Persist this fact before any async installation or caller cancellation.
        if let Some(witness) = &witness {
            witness.finish_original_attempt();
        }
        let mut committed = match result {
            Ok(committed) => committed,
            Err(error) => {
                if persistence_certainty(&error)
                    == Some(PersistenceCertainty::DefinitelyNotCommitted)
                {
                    if let Some(witness) = input_witness {
                        witness.release_pending_not_committed(false)?;
                    }
                    MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
                    self.candidate = self.base.clone();
                    self.deferred_abort = None;
                    self.control_cancellation = None;
                } else if let Some(witness) = input_witness {
                    witness.mark_pending_unknown()?;
                }
                return Err(error);
            }
        };
        if let Some(witness) = input_witness {
            witness.mark_pending_result(committed.applied, false)?;
        }
        if !committed.applied && input_witness.is_none() {
            MutationReservation::definitely_not_committed_all(&mut self.reservations)?;
            self.candidate = self.base.clone();
            self.deferred_abort = None;
            self.control_cancellation = None;
            return Ok(committed);
        }
        if !committed.applied {
            self.candidate = self.base.clone();
            self.deferred_abort = None;
            self.control_cancellation = None;
        }
        let mut projection_error = None;
        let has_current_effects = !committed.effects.queued_messages.is_empty()
            || !committed.effects.removed_queue_message_ids.is_empty()
            || !committed.effects.current_queue_write_ids.is_empty()
            || !committed.effects.audit_events.is_empty();
        if let Some(mut current) = committed.effects.current_read_writes.take() {
            let install: Result<()> = (|| {
                current.validate_binding(
                    &self.input_evidence_scope(),
                    &self.runtime.inner.input_evidence_budget,
                )?;
                if let Some(publication) = &current.query_publication {
                    anyhow::ensure!(
                        self.current
                            .as_ref()
                            .and_then(|prior| prior.query.as_ref())
                            .is_some_and(|prior| Arc::ptr_eq(prior, &publication.baseline)),
                        "Current publication belongs to another baseline"
                    );
                }
                // Move only the original qualified record, not the installed
                // query or its baseline, into the post-commit notification.
                for event in &mut committed.effects.audit_events {
                    current.qualify_notification(event)?;
                }
                Ok(())
            })();
            if let Err(error) = install {
                // SQL is acknowledged. Fence this projection, then save Known
                // responsibility with all other authoritative commit effects.
                self.current = None;
                projection_error = Some(error);
            } else {
                // The baseline is an installation fence, not historical retention.
                current.query_publication = None;
                if !current.is_empty() || current.query.is_some() {
                    // Move admitted maps; missing addresses remain unprepared.
                    self.current = Some(Arc::new(current));
                } else if has_current_effects {
                    self.current = None;
                }
            }
        } else if has_current_effects {
            self.current = None;
        }
        self.runtime.inner.input_evidence_budget.release_accepted(
            &self.input_evidence_scope(),
            &committed.effects.removed_queue_message_ids,
        )?;
        if let Some(mutation) = committed.effects.agent_state.as_ref() {
            anyhow::ensure!(
                mutation.record.id == self.candidate.state.id,
                "committed runtime state identity mismatch"
            );
            self.candidate.state = mutation.record.as_ref().clone();
            self.candidate.last_persisted_state = self.candidate.state.clone();
        } else {
            anyhow::ensure!(
                self.candidate.state == self.base.state,
                "typed commit omitted a changed agent state"
            );
        }
        for message_id in &committed.effects.removed_queue_message_ids {
            self.candidate
                .queue
                .pop_next_matching(|message| &message.id == message_id);
        }
        #[cfg(test)]
        if !committed.effects.queued_messages.is_empty() {
            let checkpoint = self
                .runtime
                .inner
                .queued_message_projection_checkpoint
                .lock()
                .expect("queue projection checkpoint lock poisoned")
                .take();
            if let Some(checkpoint) = checkpoint {
                checkpoint.reached.notify_one();
                checkpoint.release.notified().await;
            }
        }
        for message in &committed.effects.queued_messages {
            if self
                .candidate
                .queue
                .peek_next_matching(|queued| queued.id == message.id)
                .is_none()
            {
                self.candidate.queue.push(message.clone());
            }
        }
        if !committed.effects.context_episodes.is_empty()
            || !committed.effects.removed_context_episode_ids.is_empty()
        {
            if let Some(episodes) = self.episodes.as_ref() {
                match episodes.with_committed(
                    self.runtime.inner.identity_incarnation,
                    &committed.effects.context_episodes,
                    &committed.effects.removed_context_episode_ids,
                ) {
                    Ok(next) => self.episodes = next.map(Arc::new),
                    Err(error) => {
                        self.episodes = None;
                        projection_error.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(inputs) = self.input_evidence.as_ref() {
            let next = runtime_input_mandatory_roots(
                inputs,
                &committed.effects.input_evidence_delta,
                &self.candidate.state,
                &self.candidate.queue,
            )
            .and_then(|mandatory| {
                inputs.with_committed(&committed.effects.input_evidence_delta, mandatory)
            });
            match next {
                Ok(next) => self.input_evidence = next.map(Arc::new),
                Err(error) => {
                    self.input_evidence = None;
                    projection_error.get_or_insert(error);
                }
            }
        }
        if let Some(delta) = &committed.effects.work_delta {
            match self.install_committed_work_delta(
                delta,
                committed.effects.work_delta_requires_prepared_domain,
            ) {
                Ok(()) => {}
                Err(error) => {
                    self.work = None;
                    projection_error.get_or_insert(error);
                }
            }
        }
        if let Some(delta) = &committed.effects.task_delta {
            if let Err(error) = self.install_committed_task_delta(delta) {
                self.tasks = None;
                projection_error.get_or_insert(error);
            }
        }
        if let Some(delta) = &committed.effects.scheduling_delta {
            if let Err(error) = self.install_committed_scheduling_delta(delta) {
                self.scheduling = None;
                projection_error.get_or_insert(error);
            }
        }
        match self
            .runtime
            .inner
            .input_evidence_budget
            .register_queue_view(
                &self.input_evidence_scope(),
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                self.candidate.queue.shared_messages(),
            ) {
            Ok(view) => self.candidate._queue_budget_view = Some(view),
            Err(error) => {
                projection_error.get_or_insert(error.into());
            }
        }
        let facts = Arc::new(self.facts(committed.effects.as_ref().clone()));
        // Retain all authoritative installation facts before the first await
        // after SQL. Cancellation leaves a known commit for the next owner call.
        self.record_committed_facts(facts.clone(), &committed.effects, &prior_publications)?;
        if let Some(error) = projection_error {
            // SQL is acknowledged. Keep its exact state, queue and effects in
            // Known recovery even if a derived domain must be prepared again.
            return Err(
                error.context("runtime mutation committed; input publication requires preparation")
            );
        }
        if committed.effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            bail!(
                "runtime mutation committed but publication requires reconciliation: {}",
                keys[self.primary_index].operation_id
            );
        }
        self.runtime.install_known_group(&self.reservations).await?;
        let observations = MutationReservation::publish_all_known(&mut self.reservations)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.runtime.inner.resources.notify();
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        if let Some(cancellation) = self.control_cancellation.take() {
            self.runtime
                .resume_saved_control_cancellation(&cancellation)?;
        }
        strip_authoritative_effects(&mut committed.effects);
        if let Some(started) = self.measurement_started.take() {
            crate::diagnostics::io_probe::record_timing(
                crate::diagnostics::io_probe::TimingKind::CommitPublication,
                started.elapsed(),
            );
        }
        self.measurement_started = crate::diagnostics::io_probe::start_timer();
        Ok(committed)
    }

    pub(in crate::runtime) fn capture_published_root(
        &self,
    ) -> Result<Arc<super::resident_state::AgentSnapshot>> {
        let root = self
            .runtime
            .inner
            .mutation_owner
            .capture()?
            .ok_or_else(|| anyhow!("runtime state has not been published"))?;
        anyhow::ensure!(
            root.state.as_ref() == &self.candidate.state,
            "context capture cannot observe a private state candidate"
        );
        Ok(root)
    }

    pub(super) fn capture_context(
        &self,
        episode_limit: usize,
    ) -> Result<super::resident_state::CapturedAgentContext> {
        self.capture_published_root()?
            .capture_context(episode_limit)
    }

    pub(super) fn capture_work(
        &self,
        request: &super::resident_work::WorkCaptureRequest,
    ) -> Result<super::resident_work::CapturedWork> {
        self.capture_work_with_state(request)
            .map(|(_, _, work)| work)
    }

    pub(super) fn capture_work_with_state(
        &self,
        request: &super::resident_work::WorkCaptureRequest,
    ) -> Result<(
        mutation_owner::PublicationToken,
        Arc<AgentState>,
        super::resident_work::CapturedWork,
    )> {
        let root = self.capture_published_root()?;
        let work = root
            .work
            .as_ref()
            .ok_or(super::resident_work::WorkReadError::IncompleteLive)?
            .capture(request)?;
        anyhow::ensure!(
            work.scope == self.input_evidence_scope(),
            "Work capture scope differs"
        );
        Ok((root.token, root.state.clone(), work))
    }

    pub(super) async fn commit_episode_memory(
        &mut self,
        command: &crate::runtime_db::episode_memory::EpisodeMemoryTransitionCommand,
    ) -> Result<crate::runtime_db::episode_memory::EpisodeMemoryReceipt> {
        use crate::runtime_db::episode_memory::EpisodeMemoryCommit;
        let operation_id = crate::ids::audit_event_id();
        let witness = super::mutation_witness::MutationWitness::episode_memory(
            operation_id.clone(),
            command,
        )?;
        let db = self.runtime.inner.runtime_db.clone();
        let mut receipt = None;
        let commit = self
            .commit_transition_inner(operation_id, Some(witness), || {
                match db.commit_episode_memory(command)? {
                    EpisodeMemoryCommit::Applied {
                        receipt: saved,
                        mut transition,
                        episode,
                    } => {
                        receipt = Some(saved);
                        transition.effects.context_episodes.extend(episode);
                        Ok(transition)
                    }
                    EpisodeMemoryCommit::Replayed { receipt: saved } => {
                        // Return the original result without reinstalling its old candidate.
                        receipt = Some(saved);
                        Ok(TransitionCommit::default())
                    }
                }
            })
            .await?;
        self.runtime.apply_transition_commit(commit).await;
        receipt.ok_or_else(|| anyhow!("episode commit omitted its original result"))
    }

    pub(super) async fn commit_control(
        &mut self,
        prepared: &super::control::PreparedRuntimeControl,
    ) -> Result<crate::runtime_db::control_transition::ControlReceipt> {
        use crate::runtime_db::control_transition::ControlCommit;
        let command = &prepared.command;
        let saved = SavedControlCancellation {
            key: command.key().clone(),
            digest: command.command_digest().to_owned(),
            owner_epoch: self.reservations[self.primary_index].owner_epoch(),
            facts: prepared.cancellation.clone(),
        };
        let operation_id = crate::ids::audit_event_id();
        let witness = super::mutation_witness::MutationWitness::control(
            operation_id.clone(),
            command,
            saved.facts.clone(),
            saved.owner_epoch,
        )?;
        let work_request = self.owner_work_load_request()?;
        let (task_request, _task_request_budget) = self.owner_task_load_request()?;
        let task_ledger = self.runtime.inner.input_evidence_budget.clone();
        let input_query_source = self.owner_input_query_source()?;
        let current_query_source = self.owner_current_query_source()?;
        let db = self.runtime.inner.runtime_db.clone();
        let mut receipt = None;
        // Read-only preparation may refuse before any original attempt exists.
        // Only an admitted attempt owns a retained cancellation responsibility.
        self.runtime.inner.control_cancellations.retain_prepared(
            &saved.key,
            &saved.digest,
            saved.owner_epoch,
            saved.facts.clone(),
        )?;
        self.control_cancellation = Some(saved.clone());
        if command.invocation().action == ControlAction::Stop {
            self.candidate.current_run_abort = None;
        }
        let result = self
            .commit_transition_inner(operation_id, Some(witness), || {
                match db.commit_control_with_input_query(
                    command,
                    &work_request,
                    &task_request,
                    &task_ledger,
                    input_query_source.as_ref(),
                    current_query_source.as_ref(),
                )? {
                    ControlCommit::Applied {
                        receipt: original,
                        delta,
                    } => {
                        validate_control_publication(&delta, &saved.key.agent_id)?;
                        receipt = Some(original);
                        Ok(delta.transition)
                    }
                    ControlCommit::Replayed { receipt: original } => {
                        receipt = Some(original);
                        Ok(TransitionCommit::default())
                    }
                }
            })
            .await;
        match result {
            Ok(commit) => {
                if !commit.applied {
                    self.runtime.inner.control_cancellations.release_prepared(
                        &saved.key,
                        &saved.digest,
                        saved.owner_epoch,
                    )?;
                }
                self.runtime.apply_transition_commit(commit).await;
                receipt.ok_or_else(|| anyhow!("Control commit omitted its original result"))
            }
            Err(error) => {
                if persistence_certainty(&error)
                    == Some(PersistenceCertainty::DefinitelyNotCommitted)
                {
                    self.runtime.inner.control_cancellations.release_prepared(
                        &saved.key,
                        &saved.digest,
                        saved.owner_epoch,
                    )?;
                }
                Err(error)
            }
        }
    }

    pub(super) async fn install_transient(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate.state == self.base.state
                && self.candidate.last_persisted_state == self.base.last_persisted_state,
            "transient installation cannot change durable agent state"
        );
        self.install_durable_readback().await
    }

    pub(super) async fn install_durable_readback(&mut self) -> Result<()> {
        self.validate_candidate_identity()?;
        anyhow::ensure!(
            self.candidate.state == self.candidate.last_persisted_state,
            "durable readback must install one exact persisted state"
        );
        MutationReservation::begin_all(&mut self.reservations, crate::ids::audit_event_id())?;
        let facts = Arc::new(self.facts(Default::default()));
        self.record_committed_facts(facts.clone(), &Default::default(), &[])?;
        self.runtime.install_known_group(&self.reservations).await?;
        let observations = MutationReservation::publish_all_known(&mut self.reservations)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        self.runtime.inner.resources.notify();
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        Ok(())
    }

    // A caller that owns a committed operation may finish its saved installation
    // without releasing the reservation or guessing the result of unknown SQL.
    pub(super) async fn reconcile_known_commit(&mut self) -> Result<bool> {
        let known = match MutationReservation::validate_group_known(&self.reservations) {
            Ok(known) => known,
            Err(mutation_owner::MutationOwnerError::NeedsReconcile(_)) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if known.iter().all(Option::is_none) {
            return Ok(false);
        }
        self.runtime
            .reconcile_reserved_mutations(&mut self.reservations)
            .await?;
        for (metadata, saved) in self.metadata.iter_mut().zip(known) {
            if let Some(KnownCommit::Metadata(facts)) = saved {
                *metadata = Some(facts);
            }
        }
        self.candidate = Box::new(self.runtime.inner.agent.lock().await.clone());
        let root = self.runtime.inner.mutation_owner.capture()?;
        self.episodes = root.as_ref().and_then(|root| root.episodes.clone());
        self.input_evidence = root.as_ref().and_then(|root| root.input_evidence.clone());
        self.current = root.as_ref().and_then(|root| root.current.clone());
        self.work = root.as_ref().and_then(|root| root.work.clone());
        self.tasks = root.as_ref().and_then(|root| root.tasks.clone());
        self.base = self.candidate.clone();
        self.deferred_abort = None;
        self.control_cancellation = None;
        Ok(true)
    }

    fn validate_candidate_identity(&self) -> Result<()> {
        let agent_id = self.runtime.inner.mutation_owner.agent_id();
        anyhow::ensure!(
            self.candidate.state.id == agent_id && self.base.state.id == agent_id,
            "cannot write agent state for `{}` through storage scoped to `{agent_id}`",
            self.candidate.state.id
        );
        Ok(())
    }

    fn record_committed_facts(
        &mut self,
        facts: Arc<RuntimeMutationFacts>,
        effects: &crate::runtime_db::transitions::PostCommitEffects,
        prior_publications: &[Option<Arc<super::mutation_owner::PostureObservation>>],
    ) -> Result<()> {
        for delta in &effects.related_task_deltas {
            anyhow::ensure!(
                self.reservations
                    .iter()
                    .enumerate()
                    .any(|(index, reservation)| {
                        index != self.primary_index
                            && delta.scope.agent_id == reservation.agent_id()
                            && delta.scope.identity_incarnation
                                == reservation.identity_incarnation()
                    }),
                "related Task delta has no reserved owner"
            );
        }
        let known =
            self.reservations
                .iter()
                .enumerate()
                .map(|(index, reservation)| {
                    if index == self.primary_index {
                        return Ok(KnownCommit::State {
                            state: facts.state.clone(),
                            episodes: facts.episodes.clone(),
                            input_evidence: facts.input_evidence.clone(),
                            work: facts.work.clone(),
                            tasks: facts.tasks.clone(),
                            scheduling: facts.scheduling.clone(),
                            runtime: Some(facts.clone()),
                        });
                    }
                    if let Some(member) = self
                        .task_stop_readonly
                        .iter()
                        .find(|member| member.target().scope.agent_id == reservation.agent_id())
                    {
                        anyhow::ensure!(
                            !effects
                                .agent_identities
                                .iter()
                                .any(|identity| identity.agent_id == reservation.agent_id())
                                && effects
                                    .agent_state
                                    .as_ref()
                                    .is_none_or(|state| state.record.id != reservation.agent_id())
                                && !effects
                                    .tasks
                                    .iter()
                                    .any(|task| task.agent_id == reservation.agent_id())
                                && !effects.related_task_deltas.iter().any(|delta| {
                                    delta.scope.agent_id == reservation.agent_id()
                                }),
                            "initial Stop cannot mutate its readonly child"
                        );
                        return Ok(member.known());
                    }
                    let mut metadata = self.metadata[index]
                        .as_ref()
                        .ok_or_else(|| anyhow!("related runtime metadata snapshot is missing"))?
                        .as_ref()
                        .clone();
                    if let Some(identity) = effects
                        .agent_identities
                        .iter()
                        .find(|identity| identity.agent_id == reservation.agent_id())
                    {
                        anyhow::ensure!(
                            identity.incarnation == reservation.identity_incarnation(),
                            "committed related identity incarnation mismatch"
                        );
                        metadata.identity = Some(Arc::new(identity.clone()));
                    }
                    let deltas = effects
                        .related_task_deltas
                        .iter()
                        .filter(|delta| delta.scope.agent_id == reservation.agent_id())
                        .collect::<Vec<_>>();
                    if !deltas.is_empty() {
                        anyhow::ensure!(
                            metadata.identity.as_ref().is_some_and(|identity| {
                                identity.incarnation == reservation.identity_incarnation()
                                    && identity.status == crate::types::AgentRegistryStatus::Active
                            }),
                            "related Task delta requires its active identity"
                        );
                        let prior = prior_publications
                            .get(index)
                            .context("related Task publication was not frozen")?
                            .as_ref();
                        let mut tasks = prior.and_then(|root| root.tasks.clone());
                        for delta in deltas {
                            tasks = tasks
                                .as_ref()
                                .map(|tasks| tasks.with_committed(delta).map(Arc::new))
                                .transpose()?;
                        }
                        return Ok(KnownCommit::State {
                            state: metadata
                                .state
                                .clone()
                                .context("related Task owner has no State")?,
                            episodes: prior.and_then(|root| root.episodes.clone()),
                            input_evidence: prior.and_then(|root| root.input_evidence.clone()),
                            work: prior.and_then(|root| root.work.clone()),
                            tasks,
                            scheduling: prior.and_then(|root| root.scheduling.clone()),
                            runtime: None,
                        });
                    }
                    Ok(KnownCommit::Metadata(Arc::new(metadata)))
                })
                .collect::<Result<Vec<_>>>()?;
        let metadata = known
            .iter()
            .enumerate()
            .map(|(index, known)| match known {
                KnownCommit::Metadata(facts) => Some(facts.clone()),
                KnownCommit::State { .. } => self.metadata[index].clone(),
            })
            .collect();
        MutationReservation::committed_all(&mut self.reservations, known)?;
        self.metadata = metadata;
        Ok(())
    }

    pub(super) async fn restore_bootstrap_replay_message(
        &mut self,
        storage: &AppStorage,
        message: &MessageEnvelope,
    ) -> Result<()> {
        if self
            .candidate
            .queue
            .peek_next_matching(|queued| queued.id == message.id)
            .is_none()
        {
            self.candidate.queue.push(message.clone());
        }
        let queued_messages = self.candidate.queue.len();
        self.candidate.state.pending = queued_messages;
        scheduler_executor::apply_bootstrap_recovered_projection(
            &mut self.candidate.state,
            scheduler_executor::BootstrapRecoveryFacts { queued_messages },
        );
        self.persist_state(storage).await
    }

    fn facts(
        &self,
        mut effects: crate::runtime_db::transitions::PostCommitEffects,
    ) -> RuntimeMutationFacts {
        // The authoritative domain/queue already own the exact allocations.
        // Recovery notifications retain only their original event responsibility.
        strip_input_evidence_payloads(&mut effects);
        RuntimeMutationFacts {
            owner_epoch: self.reservations[self.primary_index].owner_epoch(),
            state: Arc::new(self.candidate.state.clone()),
            episodes: self.episodes.clone(),
            input_evidence: self.input_evidence.clone(),
            work: self.work.clone(),
            tasks: self.tasks.clone(),
            scheduling: self.scheduling.clone(),
            queue: self.candidate.queue.clone(),
            current: self.current.clone(),
            _queue_budget_view: self.candidate._queue_budget_view.clone(),
            _readonly_queue_metadata: None,
            current_run_abort: self.candidate.current_run_abort.clone(),
            deferred_abort: self.deferred_abort.clone(),
            control_cancellation: self.control_cancellation.clone(),
            effects,
            recovered_install: None,
            plan_preparation: self.plan_preparation.clone(),
        }
    }
}

fn runtime_input_mandatory_roots(
    inputs: &super::resident_input_evidence::ResidentInputEvidence,
    delta: &super::resident_input_evidence::CommittedInputEvidenceDelta,
    state: &AgentState,
    queue: &RuntimeQueue,
) -> Result<std::collections::BTreeSet<super::resident_input_evidence::InputEvidenceKey>> {
    use super::resident_input_evidence::{InputEvidenceFamily as Family, InputEvidenceKey as Key};
    let mut roots = queue
        .shared_messages()
        .map(|message| Key::new(Family::Message, &message.id))
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(turn_id) = &state.current_turn_id {
        roots.insert(Key::new(Family::Turn, turn_id));
    }
    if let Some(binding) = &state.current_execution_binding {
        roots.insert(Key::new(Family::Message, &binding.source_message_id));
        roots.insert(Key::new(Family::Turn, &binding.turn_id));
        if let Some(id) = &binding.activation_id {
            roots.insert(Key::new(Family::Attempt, id));
        }
        if let Some(id) = &binding.work_item_id {
            roots.insert(Key::new(Family::WorkExecution, id));
        }
    }
    roots.extend(inputs.nonterminal_execution_roots_after(delta)?);
    Ok(roots)
}

fn strip_input_evidence_payloads(effects: &mut crate::runtime_db::transitions::PostCommitEffects) {
    effects.current_read_writes = None;
    effects.current_queue_write_ids.clear();
    effects.input_evidence_delta = Default::default();
    effects.messages.clear();
    effects.queued_messages.clear();
    effects.turns.clear();
    effects.tool_executions.clear();
    effects.briefs.clear();
    effects.transcript_entries.clear();
    effects.execution_protocol = None;
    effects.work_items.clear();
    effects.work_delta = None;
    effects.work_delta_requires_prepared_domain = false;
    effects.work_input_preparation = None;
    effects.task_delta = None;
    effects.task_delta_requires_prepared_domain = false;
    effects.task_write_keys.clear();
    effects.task_observation_keys.clear();
    effects.task_material_budget_views.clear();
    effects.scheduling_delta = None;
    effects.scheduling_delta_requires_prepared_domain = false;
    effects.scheduling_write_keys.clear();
    effects.scheduling_material_budget_views.clear();
}

fn strip_authoritative_effects(effects: &mut crate::runtime_db::transitions::PostCommitEffects) {
    strip_input_evidence_payloads(effects);
    effects.agent_state = None;
    effects.queued_messages.clear();
    effects.removed_queue_message_ids.clear();
    effects.work_items.clear();
    effects.tasks.clear();
    effects.timers.clear();
    effects.external_triggers.clear();
    effects.agent_identities.clear();
    effects.context_episodes.clear();
    effects.removed_context_episode_ids.clear();
}

fn readonly_queue_material(
    slots: usize,
) -> Result<crate::runtime_db::task_snapshot::TaskMaintenanceMaterial> {
    Ok(crate::runtime_db::task_snapshot::TaskMaintenanceMaterial {
        bytes: std::mem::size_of::<RuntimeQueue>()
            .checked_add(
                slots
                    .checked_mul(std::mem::size_of::<Arc<MessageEnvelope>>())
                    .context("readonly queue slots overflow")?,
            )
            .and_then(|bytes| bytes.checked_mul(4))
            .context("readonly queue metadata overflow")?,
        records: slots
            .checked_add(1)
            .and_then(|n| n.checked_mul(4))
            .context("readonly queue record overflow")?,
    })
}

impl RuntimeHandle {
    pub(crate) async fn prepare_external_input_facts(
        &self,
        reservation: &MutationReservation,
        state: Arc<AgentState>,
        episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
    ) -> Result<RuntimeMutationFacts> {
        self.prepare_external_input_facts_with_queue_material(
            reservation,
            state,
            episodes,
            input_evidence,
            |_| Ok(()),
        )
        .await
    }

    pub(crate) async fn prepare_original_child_binding_facts(
        &self,
        reservation: &MutationReservation,
        state: Arc<AgentState>,
        episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
        remaining: &mut crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
    ) -> Result<RuntimeMutationFacts> {
        let (records, bytes) =
            crate::runtime_db::task_snapshot::TaskDomainLoadRequest::bounded_material_limits(
                remaining.record_limit,
                remaining.byte_limit,
            );
        remaining.record_limit = records;
        remaining.byte_limit = bytes;
        let mut facts = self
            .prepare_external_input_facts_with_queue_material(
                reservation,
                state,
                episodes,
                input_evidence,
                |material| {
                    anyhow::ensure!(
                        material.bytes <= remaining.byte_limit
                            && material.records <= remaining.record_limit,
                        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                            "binding queue container material"
                        )
                    );
                    remaining.byte_limit = remaining
                        .byte_limit
                        .checked_sub(material.bytes)
                        .context("binding queue byte allowance underflow")?;
                    remaining.record_limit = remaining
                        .record_limit
                        .checked_sub(material.records)
                        .context("binding queue record allowance underflow")?;
                    Ok(())
                },
            )
            .await?;
        // The binding writer changes only its original Task. Its sparse
        // recovery preserves this held Current publication, never a readback.
        facts.current = reservation
            .capture_ready_snapshot()?
            .and_then(|root| root.current.clone());
        Ok(facts)
    }

    async fn prepare_external_input_facts_with_queue_material(
        &self,
        reservation: &MutationReservation,
        state: Arc<AgentState>,
        episodes: Option<Arc<super::resident_state::ResidentEpisodes>>,
        input_evidence: Option<Arc<super::resident_input_evidence::ResidentInputEvidence>>,
        before_clone: impl FnOnce(crate::runtime_db::task_snapshot::TaskMaintenanceMaterial) -> Result<()>
            + Send,
    ) -> Result<RuntimeMutationFacts> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        anyhow::ensure!(
            state.id == reservation.agent_id(),
            "external input state scope differs"
        );
        let agent = self.inner.agent.lock().await;
        anyhow::ensure!(
            agent.state == *state,
            "external input preparation saw a different runtime state"
        );
        let slots = agent.queue.len();
        let material = readonly_queue_material(slots)?;
        let metadata_bytes = material.bytes;
        before_clone(material)?;
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            slots <= usize::try_from(limits.record_limit)?
                && u64::try_from(metadata_bytes)?
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("readonly queue limit overflow")?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "Stop readonly queue metadata"
            )
        );
        let queue_metadata = self.inner.input_evidence_budget.register_owned_bytes(
            &super::resident_input_evidence::InputEvidenceScope {
                agent_id: reservation.agent_id().to_owned(),
                identity_incarnation: reservation.identity_incarnation(),
            },
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(metadata_bytes)?,
            },
        )?;
        let queue_view = self.inner.input_evidence_budget.register_queue_view(
            &super::resident_input_evidence::InputEvidenceScope {
                agent_id: reservation.agent_id().to_owned(),
                identity_incarnation: reservation.identity_incarnation(),
            },
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            agent.queue.shared_messages(),
        )?;
        Ok(RuntimeMutationFacts {
            owner_epoch: reservation.owner_epoch(),
            state,
            episodes,
            input_evidence,
            work: reservation.prepared_work_for_state_only_commit()?,
            tasks: reservation.prepared_tasks_for_state_only_commit()?,
            scheduling: reservation.prepared_scheduling_for_state_only_commit()?,
            current: None,
            queue: agent.queue.clone(),
            _queue_budget_view: Some(queue_view),
            _readonly_queue_metadata: Some(queue_metadata),
            plan_preparation: None,
            current_run_abort: agent.current_run_abort.clone(),
            deferred_abort: None,
            control_cancellation: None,
            effects: Default::default(),
            recovered_install: None,
        })
    }

    pub(crate) async fn commit_input_evidence<T: Send + 'static>(
        &self,
        operation_id: String,
        commit: impl FnOnce(
                &super::resident_input_evidence::InputEvidenceScope,
            )
                -> Result<crate::runtime_db::input_evidence_writes::InputEvidenceCommit<T>>
            + Send
            + 'static,
    ) -> Result<T> {
        let admission = self.execution_admission_lease()?;
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = async {
                let mut mutation = runtime.begin_mutation().await?;
                mutation.commit_input_evidence(operation_id, commit).await
            }
            .await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("input evidence worker ended before returning its result")?
    }

    pub(crate) fn resume_saved_control_cancellation(
        &self,
        saved: &SavedControlCancellation,
    ) -> Result<()> {
        if let Some(facts) = self.inner.control_cancellations.confirm_prepared(
            &saved.key,
            &saved.digest,
            saved.owner_epoch,
        )? {
            anyhow::ensure!(
                Arc::ptr_eq(&facts, &saved.facts),
                "Control original targets changed"
            );
        } else {
            self.inner.control_cancellations.retain_prepared(
                &saved.key,
                &saved.digest,
                saved.owner_epoch,
                saved.facts.clone(),
            )?;
            self.inner.control_cancellations.confirm_prepared(
                &saved.key,
                &saved.digest,
                saved.owner_epoch,
            )?;
        }
        saved.facts.signal();
        saved
            .facts
            .start_completion(self, &saved.key, &saved.digest, saved.owner_epoch);
        Ok(())
    }

    pub(super) async fn resume_original_control_cancellation(
        &self,
        receipt: &crate::runtime_db::control_transition::ControlReceipt,
    ) -> Result<()> {
        let mut reservations =
            mutation_owner::MutationOwner::reserve_all(vec![self.inner.mutation_owner.clone()])
                .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        let original_plan = self
            .inner
            .control_cancellations
            .prepared_for_receipt(&receipt.key, &receipt.command_digest)?;
        drop(reservations);
        if let Some((owner_epoch, facts)) = original_plan {
            self.resume_saved_control_cancellation(&SavedControlCancellation {
                key: receipt.key.clone(),
                digest: receipt.command_digest.clone(),
                owner_epoch,
                facts,
            })?;
        } else {
            self.reconcile_loaded_control_obligations().await?;
        }
        Ok(())
    }

    // Save the original physical targets before reconcile_all clears its ticket.
    // The caller may signal this plan only after the complete group publishes.
    pub(crate) fn control_cancellation_for_known(
        &self,
        reservation: &MutationReservation,
        known: &KnownCommit,
    ) -> Result<Option<SavedControlCancellation>> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        if let KnownCommit::State {
            runtime: Some(facts),
            ..
        } = known
        {
            if facts.owner_epoch == reservation.owner_epoch() {
                if let Some(saved) = &facts.control_cancellation {
                    return Ok(Some(saved.clone()));
                }
            }
        }
        let Some(recovery) = reservation.recovery()? else {
            return Ok(None);
        };
        let Some(witness) = recovery.witness else {
            return Ok(None);
        };
        let super::mutation_witness::MutationWitnessFamily::Control {
            key,
            command_digest,
            ..
        } = witness.family()
        else {
            return Ok(None);
        };
        let Some(facts) = witness.control_cancellation() else {
            return Ok(None);
        };
        Ok(Some(SavedControlCancellation {
            key: key.clone(),
            digest: command_digest.clone(),
            owner_epoch: witness
                .control_owner_epoch()
                .ok_or_else(|| anyhow!("Control original owner epoch missing"))?,
            facts,
        }))
    }

    pub(crate) fn spawn_owned_mutation(
        &self,
        mutation: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let mut tasks = self
            .inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned");
        if tasks.closed {
            return Err(super::execution_admission::closed_error());
        }
        tasks.handles.retain(|task| !task.is_finished());
        // Registration and closing share one gate. Shutdown cannot miss a task
        // spawned immediately after taking the previous handles to drain.
        tasks.handles.push(tokio::spawn(mutation));
        Ok(())
    }

    /// A closed owned-task registry does not accept more tasks. Its original
    /// Control lease remains the supervisor of an already observed terminal.
    pub(in crate::runtime) fn try_register_owned_original_child_terminal(
        &self,
        source: &Arc<super::OriginalChildTaskProducer>,
        mutation: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<bool> {
        source.validate_material_ledger(&self.inner.input_evidence_budget)?;
        let address = source.address();
        anyhow::ensure!(
            address.scope.agent_id == self.instance_agent_id()
                && address.scope.identity_incarnation == self.instance_incarnation()
                && address.owner_epoch == self.instance_owner_epoch(),
            "accepted child continuation crossed original instance"
        );
        let lease = source.original_lease()?;
        anyhow::ensure!(
            lease
                .original_child()?
                .as_ref()
                .is_some_and(|installed| Arc::ptr_eq(installed, source))
                && source.observed_terminal().is_some(),
            "accepted child continuation lacks its original observed lease"
        );
        let mut tasks = self
            .inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned");
        if tasks.closed {
            return Ok(false);
        }
        tasks.handles.retain(|task| !task.is_finished());
        tasks.handles.push(tokio::spawn(mutation));
        Ok(true)
    }

    pub(super) fn close_mutation_admission(&self) {
        self.inner.resources.close();
        self.inner
            .mutation_tasks
            .lock()
            .expect("mutation tasks poisoned")
            .closed = true;
    }

    pub(crate) async fn join_owned_mutations(&self) -> Result<()> {
        let joined = self.drain_original_owned_mutation_tasks().await;
        let reconciled = self.reconcile_posture_observation().await;
        joined.and(reconciled)
    }

    async fn drain_original_owned_mutation_tasks(&self) -> Result<()> {
        self.close_mutation_admission();
        let observer = self.join_resource_worker().await;
        let drain = {
            let tasks = self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned");
            tasks.drain.clone()
        };
        let mut draining = drain.lock().await;
        draining.extend(std::mem::take(
            &mut self
                .inner
                .mutation_tasks
                .lock()
                .expect("mutation tasks poisoned")
                .handles,
        ));
        let mut failure = None;
        while let Some(task) = draining.last_mut() {
            // The supervisor retains the handle across await. Cancelling this
            // drainer leaves it for the next drainer rather than detaching it.
            let result = task.await;
            draining.pop();
            if let Err(error) = result {
                failure.get_or_insert_with(|| anyhow::Error::new(error));
            }
        }
        drop(draining);
        observer.and(failure.map_or(Ok(()), Err))
    }

    async fn reconcile_retiring_original_scope(&self) -> Result<()> {
        match self.reconcile_posture_observation().await {
            Err(error)
                if matches!(
                    error.downcast_ref::<mutation_owner::MutationOwnerError>(),
                    Some(mutation_owner::MutationOwnerError::Replaced)
                ) && !self.inner.execution_admission.is_open() =>
            {
                // Old instances still own physical resources. Only the Host's
                // current owner can resolve their original durable scope.
                let Some(bridge) = self.inner.host_bridge.as_ref() else {
                    return Err(error);
                };
                bridge
                    .reconcile_retiring_original_scope(
                        self.inner.mutation_owner.agent_id(),
                        self.inner.identity_incarnation,
                    )
                    .await
            }
            result => result,
        }
    }

    // Physical retirement preserves durable Task/Timer intent for startup
    // recovery. It stops and joins only this instance's original producers.
    pub(crate) async fn join_retiring_resources(&self) -> Result<()> {
        self.join_retiring_resources_inner(false).await.map(|_| ())
    }
    pub(crate) async fn join_retiring_resources_with_process_exit(
        &self,
    ) -> Result<Option<super::control_cancellation::RetiredProducerExit>> {
        anyhow::ensure!(
            !self.accepts_execution_admission(),
            "process exit proof requires original admission closed"
        );
        self.join_retiring_resources_inner(true).await
    }
    fn join_retiring_resources_inner(
        &self,
        observe_exit: bool,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Option<super::control_cancellation::RetiredProducerExit>>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.inner.control_cancellations.cancel_local_producers();
            let joined = self.drain_original_owned_mutation_tasks().await;
            let reconciled = self.reconcile_retiring_original_scope().await;
            joined.and(reconciled)?;
            // Accepted registration workers may have reached their original lease
            // after the first snapshot; local closing also fences later attachment.
            self.inner.control_cancellations.cancel_local_producers();
            let original_producers = if observe_exit {
                Some(
                    self.inner
                        .control_cancellations
                        .capture_retiring_producers(
                            &super::resident_input_evidence::InputEvidenceScope {
                                agent_id: self.inner.mutation_owner.agent_id().to_owned(),
                                identity_incarnation: self.inner.identity_incarnation,
                            },
                            self.inner.mutation_owner.owner_epoch(),
                            &self.inner.input_evidence_budget,
                        )?,
                )
            } else {
                None
            };
            self.inner
                .control_cancellations
                .drain_for_original_runtime(self, true)
                .await?;
            let mut draining = self.inner.owned_task_drain.lock().await;
            loop {
                draining.extend(std::mem::take(
                    &mut *self
                        .inner
                        .owned_task_handles
                        .lock()
                        .expect("owned tasks poisoned"),
                ));
                let Some(task) = draining.last_mut() else {
                    break;
                };
                // These are cancellable sleep/discovery observers. Resource-owning
                // Task/Command/Timer monitors are joined by the registry above.
                task.abort();
                let result = task.await;
                draining.pop();
                if let Err(error) = result {
                    if !error.is_cancelled() {
                        return Err(error.into());
                    }
                }
            }
            match original_producers {
                Some(snapshot) if self.inner.execution_admission.is_closed_and_quiescent() => {
                    snapshot.finish(&self.inner.control_cancellations)
                }
                Some(_) => Ok(None),
                None => Ok(None),
            }
        })
    }

    pub(crate) fn original_process_registry_id(&self) -> uuid::Uuid {
        self.inner
            .control_cancellations
            .original_process_registry_id()
    }
    pub(crate) fn original_process_registration_revision(&self) -> u64 {
        self.inner
            .control_cancellations
            .original_process_registration_revision()
    }
    pub(crate) fn original_registered_groups_quiescent(&self) -> bool {
        self.inner.execution_admission.is_closed_and_quiescent()
            && self
                .inner
                .control_cancellations
                .original_registered_groups_quiescent()
    }

    pub(crate) fn instance_owner_epoch(&self) -> u64 {
        self.inner.mutation_owner.owner_epoch()
    }

    #[inline(never)]
    pub(super) fn begin_mutation(
        &self,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<RuntimeMutation>> + Send + '_>>
    {
        Box::pin(self.begin_mutation_with_owners(vec![self.inner.mutation_owner.clone()]))
    }

    pub(super) async fn begin_supervised_invocation_mutation(
        &self,
        parent_task: &TaskRecord,
        child_incarnation: u64,
    ) -> Result<(
        RuntimeMutation,
        super::resident_input_evidence::InputEvidenceScope,
    )> {
        anyhow::ensure!(
            child_incarnation == self.inner.identity_incarnation
                && parent_task.agent_id != self.inner.mutation_owner.agent_id(),
            "supervised invocation differs from original child owner"
        );
        let parent = self
            .inner
            .runtime_db
            .agent_identities()
            .latest(&parent_task.agent_id)?
            .context("supervised invocation parent identity is missing")?;
        anyhow::ensure!(
            parent.status == crate::types::AgentRegistryStatus::Active
                && parent.deleted_at.is_none(),
            "supervised invocation parent identity is not active"
        );
        let parent_scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: parent.agent_id.clone(),
            identity_incarnation: parent.incarnation,
        };
        let owner = match self.inner.host_bridge.as_ref() {
            Some(bridge) => bridge.mutation_owner_for(&parent.agent_id, parent.incarnation)?,
            None => self
                .inner
                .standalone_related_mutation_owners
                .current_owner(&parent.agent_id, parent.incarnation)?,
        };
        let mutation = self
            .begin_mutation_with_owners(vec![self.inner.mutation_owner.clone(), owner])
            .await?;
        Ok((mutation, parent_scope))
    }

    pub(super) fn validate_mutation_identity(&self) -> Result<()> {
        if let Some(identity) = self
            .inner
            .runtime_db
            .agent_identities()
            .latest_for_mutation(self.inner.mutation_owner.agent_id())?
        {
            if identity.incarnation != self.inner.identity_incarnation
                || identity.status == crate::types::AgentRegistryStatus::Deleted
            {
                return Err(RuntimeMutationIdentityRetired {
                    agent_id: identity.agent_id,
                    expected_incarnation: self.inner.identity_incarnation,
                    actual_incarnation: identity.incarnation,
                    status: identity.status,
                }
                .into());
            }
        }
        Ok(())
    }

    #[inline(never)]
    pub(super) fn begin_mutation_with_agent_deletion<'a>(
        &'a self,
        deletion: Option<&'a crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<RuntimeMutation>> + Send + 'a>>
    {
        Box::pin(self.begin_mutation_with_agent_deletion_inner(deletion))
    }

    async fn begin_mutation_with_agent_deletion_inner(
        &self,
        deletion: Option<&crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> Result<RuntimeMutation> {
        let Some(deletion) = deletion else {
            return self.begin_mutation().await;
        };
        let identity = self
            .inner
            .runtime_db
            .agent_identities()
            .latest(&deletion.agent_id)?;
        let Some(identity) = identity else {
            return self.begin_mutation().await;
        };
        let owner = if identity.agent_id == self.inner.mutation_owner.agent_id()
            && identity.incarnation == self.inner.identity_incarnation
        {
            self.inner.mutation_owner.clone()
        } else {
            match self.inner.host_bridge.as_ref() {
                Some(bridge) => {
                    bridge.mutation_owner_for(&identity.agent_id, identity.incarnation)?
                }
                None => self
                    .inner
                    .standalone_related_mutation_owners
                    .current_owner(&identity.agent_id, identity.incarnation)?,
            }
        };
        self.begin_mutation_with_owners(vec![self.inner.mutation_owner.clone(), owner])
            .await
    }

    async fn begin_mutation_with_owners(
        &self,
        owners: Vec<mutation_owner::MutationOwner>,
    ) -> Result<RuntimeMutation> {
        self.begin_mutation_with_owners_inner(owners, None).await
    }

    async fn begin_mutation_with_owners_inner(
        &self,
        owners: Vec<mutation_owner::MutationOwner>,
        readonly_child: Option<&crate::runtime_db::task_stop::TaskStopChildTarget>,
    ) -> Result<RuntimeMutation> {
        let measurement_started = crate::diagnostics::io_probe::start_timer();
        // reserve_all also acquires every member of a pending recovery group.
        let requested = owners.clone();
        let mut reservations = mutation_owner::MutationOwner::reserve_all(owners).await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        // Recovery can add scopes from an older operation. After atomic recovery
        // the new command owns only its explicit write set.
        reservations.retain(|reservation| {
            requested
                .iter()
                .any(|owner| reservation.validate_owner(owner).is_ok())
        });
        let primary_index = reservations
            .iter()
            .position(|reservation| {
                reservation
                    .validate_owner(&self.inner.mutation_owner)
                    .is_ok()
            })
            .ok_or_else(|| anyhow!("runtime mutation reservation is missing"))?;
        let observation = self.inner.mutation_owner.capture()?;
        if let Some(observation) = observation.as_ref() {
            anyhow::ensure!(
                observation.state.id == self.inner.mutation_owner.agent_id(),
                "published runtime state identity mismatch"
            );
        }
        self.validate_mutation_identity()?;
        let mut metadata = Vec::with_capacity(reservations.len());
        let mut task_stop_readonly = Vec::new();
        for (index, reservation) in reservations.iter().enumerate() {
            if index == primary_index {
                metadata.push(None);
                continue;
            }
            if let Some(child) =
                readonly_child.filter(|child| child.scope.agent_id == reservation.agent_id())
            {
                let member = self
                    .prepare_initial_stop_readonly_member(reservation, child)
                    .await?;
                metadata.push(match member.known() {
                    KnownCommit::Metadata(facts) => Some(facts),
                    KnownCommit::State { .. } => None,
                });
                task_stop_readonly.push(member);
                continue;
            }
            let identity = self
                .inner
                .runtime_db
                .agent_identities()
                .latest(reservation.agent_id())?;
            if let Some(identity) = &identity {
                anyhow::ensure!(
                    identity.incarnation == reservation.identity_incarnation(),
                    "related mutation identity incarnation is no longer current"
                );
            }
            let state = self
                .inner
                .runtime_db
                .agent_states()
                .latest(reservation.agent_id())?;
            metadata.push(Some(Arc::new(IdentityMutationFacts {
                identity: identity.map(Arc::new),
                state: state.map(Arc::new),
            })));
        }
        let candidate = Box::new(self.inner.agent.lock().await.clone());
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let mut candidate_budget_views =
            vec![self.inner.input_evidence_budget.register_queue_view(
                &scope,
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                candidate.queue.shared_messages(),
            )?];
        if let Some(inputs) = observation
            .as_ref()
            .and_then(|root| root.input_evidence.as_ref())
        {
            candidate_budget_views.push(self.inner.input_evidence_budget.register_view(
                &scope,
                super::input_evidence_budget::InputMemoryCategory::Candidate,
                &inputs.budget_records(),
            )?);
        }
        if let Some(work) = observation.as_ref().and_then(|root| root.work.as_ref()) {
            if let Some(view) = work.candidate_budget_view()? {
                candidate_budget_views.push(view);
            }
        }
        if let Some(tasks) = observation.as_ref().and_then(|root| root.tasks.as_ref()) {
            if let Some(view) = tasks.candidate_budget_view()? {
                candidate_budget_views.push(view);
            }
            if let Some(view) = tasks.candidate_stop_budget_view()? {
                candidate_budget_views.push(view);
            }
        }
        if let Some(domain) = observation
            .as_ref()
            .and_then(|root| root.scheduling.as_ref())
        {
            if let Some(view) = domain.candidate_budget_view()? {
                candidate_budget_views.push(view);
            }
        }
        let mutation = RuntimeMutation {
            runtime: self.clone(),
            reservations,
            _resource_owner_wake: super::resource_refresh::ResourceOwnerWake(Arc::downgrade(
                &self.inner,
            )),
            primary_index,
            metadata,
            task_stop_readonly,
            episodes: observation.as_ref().and_then(|root| root.episodes.clone()),
            input_evidence: observation
                .as_ref()
                .and_then(|root| root.input_evidence.clone()),
            input_state_decode: None,
            work: observation.as_ref().and_then(|root| root.work.clone()),
            tasks: observation.as_ref().and_then(|root| root.tasks.clone()),
            current: observation.as_ref().and_then(|root| root.current.clone()),
            scheduling: observation.and_then(|root| root.scheduling.clone()),
            base: candidate.clone(),
            candidate,
            deferred_abort: None,
            control_cancellation: None,
            measurement_started,
            _candidate_budget_views: candidate_budget_views,
            plan_preparation: None,
        };
        Ok(mutation)
    }

    async fn install_mutation_facts(&self, facts: &RuntimeMutationFacts) -> Result<()> {
        // The reservation remains held; state/cache readers can only see the
        // old publication or the complete new publication after this block.
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: facts.state.id.clone(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        if let Some(current) = &facts.recovered_install {
            anyhow::ensure!(
                current.scope == scope,
                "recovered cache installation scope differs"
            );
        }
        let queue_budget_view = self.inner.input_evidence_budget.register_queue_view(
            &scope,
            super::input_evidence_budget::InputMemoryCategory::Current,
            facts.queue.shared_messages(),
        )?;
        {
            let mut agent = self.inner.agent.lock().await;
            let operator_input_changed = facts.queue.shared_messages().any(|message| {
                super::scheduler::is_operator_interjection_message(message)
                    && !agent
                        .queue
                        .shared_messages()
                        .any(|previous| previous.id == message.id)
            });
            if agent.state.active_workspace_entry != facts.state.active_workspace_entry
                || agent.state.active_skills != facts.state.active_skills
            {
                self.inner.resources.registry.invalidate(
                    super::resource_observation::ResourceInvalidation::WorkspaceChange,
                )?;
            }
            {
                let mut wait = self
                    .inner
                    .provider_cooldown_wait
                    .lock()
                    .map_err(|_| anyhow!("provider cooldown wait poisoned"))?;
                let terminal_matches =
                    facts
                        .state
                        .last_turn_terminal
                        .as_ref()
                        .is_some_and(|terminal| {
                            wait.as_ref().is_some_and(|wait| {
                                wait.turn_id.as_deref() == Some(terminal.turn_id.as_str())
                            })
                        });
                let run_ended =
                    agent.current_run_abort.is_some() && facts.current_run_abort.is_none();
                if terminal_matches
                    || run_ended
                    || facts.state.last_runtime_failure != agent.state.last_runtime_failure
                {
                    *wait = None;
                }
            }
            if operator_input_changed {
                // Fence prepared continuations at source installation, before
                // any await. Stripped notification effects cannot carry this.
                self.inner
                    .operator_interjection_revision
                    .fetch_add(1, Ordering::AcqRel);
            }
            agent.state = facts.state.as_ref().clone();
            agent.last_persisted_state = facts.state.as_ref().clone();
            agent.queue = facts.queue.clone();
            agent._queue_budget_view = Some(queue_budget_view);
            agent.current_run_abort = facts.current_run_abort.clone();
            if operator_input_changed {
                self.inner.operator_interjection_notify.notify_waiters();
            }
            if let Some(abort) = &facts.deferred_abort {
                if let Ok(mut reason) = abort.handle.reason.lock() {
                    *reason = abort.reason.clone();
                }
                abort.handle.token.cancel();
            }
        }
        #[cfg(test)]
        {
            // Preserve the original deterministic after-SQL/before-publish seam.
            let _installation = self.inner.installation_barrier.lock().await;
        }
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            for identity in facts
                .effects
                .agent_identities
                .iter()
                .filter(|identity| identity.agent_id == self.inner.mutation_owner.agent_id())
            {
                bridge
                    .install_mutation_metadata(
                        &identity.agent_id,
                        self.inner.identity_incarnation,
                        &IdentityMutationFacts {
                            identity: Some(Arc::new(identity.clone())),
                            state: Some(facts.state.clone()),
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn reconcile_posture_observation(&self) -> Result<()> {
        let mut reservations =
            mutation_owner::MutationOwner::reserve_all(vec![self.inner.mutation_owner.clone()])
                .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        Ok(())
    }

    async fn install_known_group(&self, reservations: &[MutationReservation]) -> Result<()> {
        let known = MutationReservation::validate_group_known(reservations)?;
        for (reservation, saved) in reservations.iter().zip(&known) {
            let Some(saved) = saved else {
                continue;
            };
            if let KnownCommit::Metadata(metadata) = saved {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    bridge
                        .install_mutation_metadata(
                            reservation.agent_id(),
                            reservation.identity_incarnation(),
                            metadata,
                        )
                        .await?;
                }
            }
            let runtime = if reservation
                .validate_owner(&self.inner.mutation_owner)
                .is_ok()
            {
                Some(self.clone())
            } else {
                match self.inner.host_bridge.as_ref() {
                    Some(bridge) => bridge.runtime_for_mutation(reservation).await?,
                    None => None,
                }
            };
            if let Some(runtime) = runtime {
                runtime.install_saved_mutation(reservation, saved).await?;
            }
        }
        Ok(())
    }

    // The slice must contain the whole pending group. No member becomes readable
    // until every saved installation has completed and all gates validate.
    pub(crate) async fn reconcile_reserved_mutations(
        &self,
        reservations: &mut [MutationReservation],
    ) -> Result<bool> {
        let version = self.inner.config_snapshot.load_full();
        let task_queries = self.task_recovery_queries_for_group(reservations).await?;
        super::input_recovery::resolve_input_recovery_with_queries(
            &self.inner.runtime_db,
            reservations,
            &self.inner.input_evidence_budget,
            &version.base_context_config,
            &version.model_catalog,
            &task_queries,
        )?;
        super::control_recovery::resolve_control_recovery(
            &self.inner.runtime_db,
            reservations,
            self.current_context_config()
                .await
                .recent_episode_candidates,
            Some(self),
            &self.inner.input_evidence_budget,
        )
        .await?;
        super::episode_recovery::resolve_episode_recovery(
            &self.inner.runtime_db,
            reservations,
            self.current_context_config()
                .await
                .recent_episode_candidates,
        )?;
        let known = MutationReservation::validate_group_known(reservations).map_err(|error| {
            match error {
                mutation_owner::MutationOwnerError::NeedsReconcile(key) => anyhow!(
                    "runtime commit outcome is unknown; no durable receipt proves the original operation {}", key.operation_id),
                other => other.into(),
            }
        })?;
        if known.iter().all(Option::is_none) {
            return Ok(false);
        }
        let mut notifications = Vec::new();
        let mut cancellations = Vec::new();
        for (reservation, saved) in reservations.iter().zip(&known) {
            let Some(saved) = saved else {
                continue;
            };
            if let Some(recovery) = reservation.recovery()? {
                tracing::warn!(agent_id = %recovery.key.agent_id,
                    operation_id = %recovery.key.operation_id, phase = ?recovery.phase,
                    original_outcome_known = true,
                    "restoring committed runtime mutation group without replay");
            }
            if let KnownCommit::Metadata(metadata) = saved {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    bridge
                        .install_mutation_metadata(
                            reservation.agent_id(),
                            reservation.identity_incarnation(),
                            metadata,
                        )
                        .await?;
                }
            }
            let runtime = if reservation
                .validate_owner(&self.inner.mutation_owner)
                .is_ok()
            {
                Some(self.clone())
            } else {
                match self.inner.host_bridge.as_ref() {
                    Some(bridge) => bridge.runtime_for_mutation(reservation).await?,
                    None => None,
                }
            };
            if let Some(runtime) = runtime {
                if let Some(plan) = runtime.control_cancellation_for_known(reservation, saved)? {
                    cancellations.push((runtime.clone(), plan));
                }
                if let Some(facts) = runtime.install_saved_mutation(reservation, saved).await? {
                    notifications.push((runtime, facts));
                }
            }
        }
        let observations = MutationReservation::reconcile_all(reservations, known)?;
        for observation in observations.into_iter().flatten() {
            trace_publication(observation.token);
        }
        for (runtime, plan) in cancellations {
            runtime.resume_saved_control_cancellation(&plan)?;
        }
        for (runtime, facts) in notifications {
            runtime
                .reissue_mutation_notifications(facts.external_effects())
                .await;
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) async fn prepare_known_host_installation_fixture(
        &self,
    ) -> Result<(AgentState, CancellationToken, String)> {
        let agent_id = self.agent_state().await?.id;
        let message = self
            .enqueue(MessageEnvelope::new(
                agent_id.clone(),
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: "known Host installation fixture".into(),
                },
            ))
            .await?;
        let token = CancellationToken::new();
        {
            let mut mutation = self.begin_mutation().await?;
            mutation.state.status = AgentStatus::AwakeRunning;
            mutation.state.current_run_id = Some("host-installation-fixture".into());
            mutation.current_run_abort = Some(CurrentRunAbortHandle {
                run_id: "host-installation-fixture".into(),
                token: token.clone(),
                reason: Arc::new(StdMutex::new("fixture-active".into())),
            });
            mutation.persist_state(&self.inner.storage).await?;
        }
        let mut mutation = self.begin_mutation().await?;
        mutation.defer_abort_current_run("fixture-committed-abort");
        mutation.current_run_abort = None;
        mutation.queue.clear();
        mutation.state.pending = 0;
        mutation.state.status = AgentStatus::AwakeIdle;
        mutation.state.current_run_id = None;
        let expected_state = mutation.state.clone();
        let expected_queue = self
            .inner
            .runtime_db
            .queue_entries()
            .latest(&message.id)?
            .context("fixture queue entry is missing")?;
        let mut aborted_queue = expected_queue.clone();
        aborted_queue.status = QueueEntryStatus::Aborted;
        aborted_queue.updated_at = self.now();
        let event = AuditEvent::legacy("host_installation_fixture", serde_json::json!({}));
        let command = crate::runtime_db::transitions::QueueTransitionCommand {
            agent_id,
            operation: crate::runtime_db::transitions::QueueOperation::Discard,
            mutation: crate::runtime_db::transitions::QueueMutation::Upsert(aborted_queue),
            scheduler_claim_work_item: None,
            agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(mutation.last_persisted_state.clone())),
                record: Box::new(expected_state.clone()),
            }),
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: vec![event.clone()],
            notify_scheduler: false,
            fault: Some(TransitionFaultPoint::BeforeCacheUpdate),
            brief_evidence: Vec::new(),
        };
        let result = mutation
            .commit_transition(event.id, |scope| {
                self.inner
                    .runtime_db
                    .transitions_in_scope(scope)
                    .commit_queue(&command)
            })
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => bail!("fixture must leave publication pending"),
        };
        anyhow::ensure!(
            error
                .to_string()
                .contains("committed but publication requires reconciliation"),
            "fixture must reach the known publication fault: {error:#}"
        );
        let recovery = mutation.reservations[mutation.primary_index]
            .recovery()?
            .context("fixture recovery facts are missing")?;
        anyhow::ensure!(
            recovery.committed_state.as_deref() == Some(&expected_state)
                && recovery.committed_runtime.is_some(),
            "fixture must retain exact known runtime facts"
        );
        Ok((expected_state, token, message.id))
    }

    #[cfg(test)]
    pub(crate) async fn host_installation_snapshot(&self) -> (AgentState, usize, bool) {
        let agent = self.inner.agent.lock().await;
        (
            agent.state.clone(),
            agent.queue.len(),
            agent.current_run_abort.is_none(),
        )
    }

    pub(crate) fn instance_agent_id(&self) -> &str {
        self.inner.mutation_owner.agent_id()
    }

    pub(crate) fn owns_mutation_reservation(&self, reservation: &MutationReservation) -> bool {
        reservation
            .validate_owner(&self.inner.mutation_owner)
            .is_ok()
    }

    pub(crate) async fn install_saved_mutation(
        &self,
        reservation: &MutationReservation,
        known: &KnownCommit,
    ) -> Result<Option<Arc<RuntimeMutationFacts>>> {
        // Old-runtime handles and abort tokens do not cross an owner replacement.
        if reservation
            .validate_owner(&self.inner.mutation_owner)
            .is_err()
        {
            return Ok(None);
        }
        match known {
            KnownCommit::State {
                state,
                runtime: facts,
                ..
            } => {
                if let Some(facts) = facts
                    .as_ref()
                    .filter(|facts| facts.owner_epoch == reservation.owner_epoch())
                {
                    self.install_mutation_facts(facts).await?;
                    // Notifications borrow the original ACK and retain its
                    // resident/material holders until the whole group settles.
                    return Ok(Some(facts.clone()));
                }
                self.install_exact_durable_readback(state).await?;
            }
            KnownCommit::Metadata(metadata) => {
                if let Some(state) = &metadata.state {
                    self.install_exact_durable_readback(state).await?;
                }
            }
        }
        Ok(None)
    }

    pub(crate) async fn reissue_mutation_notifications(
        &self,
        effects: &crate::runtime_db::transitions::PostCommitEffects,
    ) {
        // This read-only path cannot install authoritative effects or repeat SQL.
        // BeforeCacheUpdate is not a notification fault.
        self.apply_transition_notifications(effects).await;
    }

    async fn install_exact_durable_readback(&self, state: &Arc<AgentState>) -> Result<()> {
        let mut agent = self.inner.agent.lock().await;
        anyhow::ensure!(
            agent.state == agent.last_persisted_state,
            "runtime recovery cannot discard unpersisted state"
        );
        anyhow::ensure!(
            agent
                .current_run_abort
                .as_ref()
                .is_none_or(|handle| { state.current_run_id.as_ref() == Some(&handle.run_id) }),
            "runtime recovery conflicts with a live run handle"
        );
        agent.state = state.as_ref().clone();
        agent.last_persisted_state = state.as_ref().clone();
        Ok(())
    }
}

fn validate_control_publication(
    delta: &crate::runtime_db::control_transition::ControlCommittedDelta,
    agent_id: &str,
) -> Result<()> {
    let effects = &delta.transition.effects;
    anyhow::ensure!(
        delta.state.id == agent_id
            && effects
                .agent_state
                .as_ref()
                .is_none_or(|change| change.record.as_ref() == &delta.state)
            && effects.tasks == delta.tasks
            && effects.timers == delta.timers
            && effects.external_triggers == delta.external_triggers
            && effects.messages == delta.result_messages
            && effects.turns == delta.turns
            && effects.execution_protocol == delta.execution_protocol,
        "Control publication differs from its canonical committed facts"
    );
    anyhow::ensure!(
        effects
            .removed_queue_message_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            == delta
                .queue_entries
                .iter()
                .map(|entry| &entry.message_id)
                .collect(),
        "Control publication omitted a canonical queue removal"
    );
    anyhow::ensure!(
        delta
            .waits
            .iter()
            .all(|wait| wait.agent_id == agent_id && wait.status == WaitConditionStatus::Cancelled)
            && delta
                .timer_wakes
                .iter()
                .all(|wake| wake.status == "cancelled")
            && delta
                .workspace_occupancy
                .as_ref()
                .is_none_or(|occupancy| occupancy.holder_agent_id == agent_id),
        "Control publication cancellation scope differs"
    );
    tracing::debug!(
        agent_id,
        waits = delta.waits.len(),
        timer_wakes = delta.timer_wakes.len(),
        workspace_occupancy = delta.workspace_occupancy.is_some(),
        "validated canonical Control cancellation facts"
    );
    Ok(())
}

fn trace_publication(token: PublicationToken) {
    tracing::trace!(
        identity_incarnation = token.identity_incarnation,
        owner_epoch = token.owner_epoch,
        publication_generation = token.generation,
        "published committed posture observation"
    );
}

#[cfg(test)]
#[path = "state_mutation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "state_mutation/context_preparation_tests.rs"]
mod context_preparation_tests;

#[cfg(test)]
#[path = "state_mutation/dispatch_memory_auto_tests.rs"]
mod dispatch_memory_auto_tests;

#[cfg(test)]
#[path = "state_mutation/recovery_effects_tests.rs"]
mod recovery_effects_tests;

#[cfg(test)]
mod plan_preparation_tests {
    use super::*;
    use crate::runtime_db::{
        input_mutation_receipts::OriginalInputMutationAttempt,
        transitions::{PreparedInputTransition, WorkItemMutation, WorkItemTransitionCommand},
    };

    #[tokio::test]
    async fn original_plan_witness_unknown_fresh_dnc_releases_writer_not_historical_material(
    ) -> Result<()> {
        for with_pending in [false, true] {
            let home = tempfile::tempdir()?;
            let runtime = RuntimeHandle::new(
                "default",
                home.path().to_owned(),
                None::<crate::types::WorkspaceEntry>,
                String::new(),
                Arc::new(crate::provider::StubProvider::new("unused")),
                "default".into(),
                crate::runtime::tests::support::context_config(),
            )?;
            runtime.await_initial_resources().await;
            let mut record =
                WorkItemRecord::new("default", "original plan witness", WorkItemState::Open);
            let (_, preparation) = runtime
                .prepare_work_plan_file(&mut record, Some("REAL_PLAN_BODY"))
                .await?;
            let material = Arc::downgrade(&preparation.material_for_test());
            let mut mutation = runtime.begin_mutation().await?;
            let scope = mutation.input_evidence_scope();
            let mut wrong_scope = scope.clone();
            wrong_scope.identity_incarnation += 1;
            assert!(preparation.validate_scope(&wrong_scope).is_err());
            mutation.attach_plan_preparation(preparation.clone())?;
            let facts = Arc::new(mutation.facts(Default::default()));
            let historical = facts.clone();
            let nonce = crate::ids::audit_event_id();
            let prepared = PreparedInputTransition::work(
                scope.clone(),
                nonce.clone(),
                Utc::now(),
                WorkItemTransitionCommand {
                    agent_id: "default".into(),
                    mutation: WorkItemMutation::Insert { record },
                    agent_state: None,
                    brief_evidence: vec![],
                    audit_events: vec![],
                    index_changes: vec![],
                    notify_scheduler: false,
                    fault: None,
                },
                Default::default(),
                false,
            )?;
            let ledger = runtime.inner.input_evidence_budget.clone();
            let pending = if with_pending {
                let super::super::input_evidence_budget::PendingAdmission::Reserved(ticket) =
                    ledger.reserve_pending(&scope, "original-plan-ticket", 1)?
                else {
                    unreachable!()
                };
                vec![(ledger.clone(), ticket)]
            } else {
                vec![]
            };
            let witness = super::super::mutation_witness::MutationWitness::input_mutation(
                nonce.clone(),
                OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
                pending,
            )?
            .with_input_runtime(vec![facts])?;
            let saved = witness.clone();
            let error = mutation
                .commit_transition_inner(nonce, Some(witness), || {
                    anyhow::bail!("controlled lost original SQL result")
                })
                .await
                .unwrap_err();
            assert!(error.to_string().contains("controlled lost"));
            assert!(saved.original_attempt_finished());
            assert!(runtime.inner.resources.gate.generation().is_err());
            if with_pending {
                // Unknown cannot be downgraded by a non-fresh negative proof.
                assert!(saved.release_pending_not_committed(false).is_err());
                assert!(runtime.inner.resources.gate.generation().is_err());
            }
            runtime
                .reconcile_reserved_mutations(&mut mutation.reservations)
                .await?;
            assert!(runtime.inner.resources.gate.generation().is_ok());
            assert_eq!(ledger.report()?.pending.unresolved_count, 0);
            assert_eq!(
                saved.release_pending_not_committed(true).is_err(),
                with_pending
            );
            assert!(runtime.inner.resources.gate.generation().is_ok());
            drop((saved, historical, preparation, mutation));
            assert!(material.upgrade().is_none());
            runtime.join_owned_mutations().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn original_work_ack_and_dnc_release_writer_while_facts_clones_hold_material(
    ) -> Result<()> {
        for fault in [None, Some(TransitionFaultPoint::BeforeCommit)] {
            let home = tempfile::tempdir()?;
            let runtime = RuntimeHandle::new(
                "default",
                home.path().to_owned(),
                None::<crate::types::WorkspaceEntry>,
                String::new(),
                Arc::new(crate::provider::StubProvider::new("unused")),
                "default".into(),
                crate::runtime::tests::support::context_config(),
            )?;
            runtime.await_initial_resources().await;
            let mut record =
                WorkItemRecord::new("default", "real original Work ACK", WorkItemState::Open);
            let (_, preparation) = runtime
                .prepare_work_plan_file(&mut record, Some("REAL_ORIGINAL_BODY"))
                .await?;
            let material = Arc::downgrade(&preparation.material_for_test());
            let mut mutation = runtime.begin_mutation().await?;
            mutation.attach_plan_preparation(preparation.clone())?;
            let historical = Arc::new(mutation.facts(Default::default()));
            let historical_clone = historical.clone();
            let result = mutation
                .commit_work_transition(
                    crate::ids::audit_event_id(),
                    WorkItemTransitionCommand {
                        agent_id: "default".into(),
                        mutation: WorkItemMutation::Insert { record },
                        agent_state: None,
                        brief_evidence: vec![],
                        audit_events: vec![],
                        index_changes: vec![],
                        notify_scheduler: false,
                        fault,
                    },
                    Default::default(),
                    false,
                )
                .await;
            assert_eq!(result.is_err(), fault.is_some());
            assert!(runtime.inner.resources.gate.generation().is_ok());
            assert!(material.upgrade().is_some());
            drop((mutation, preparation, historical));
            assert!(material.upgrade().is_some());
            runtime.join_owned_mutations().await?;
            drop((runtime, historical_clone));
            assert!(material.upgrade().is_none());
        }
        Ok(())
    }
}
