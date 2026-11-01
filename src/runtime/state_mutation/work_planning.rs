//! Explicit scheduler acquisition; pure decisions use resident Work/input facts.
use super::*;
use crate::runtime::resident_scheduling::CapturedScheduling;
use crate::runtime_db::repositories::scheduler_latest_runtime_error_on;
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
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

pub(crate) struct PreparedSchedulerRead {
    pub state: Arc<AgentState>,
    pub projection: SchedulerProjection,
    pub work_queue: WorkQueueReadModel,
    pub work: CapturedWork,
    pub inputs: super::super::resident_input_evidence::CapturedInputEvidence,
    pub scheduling: super::super::resident_scheduling::CapturedScheduling,
    pub tasks: super::super::resident_task::CapturedTasks,
    /// Original consumed-reply/owner predicate, acquired under this reservation.
    pub recovered_replies: BTreeSet<String>,
    pub due_rechecks: Vec<WorkItemRecord>,
    pub scheduling_posture: crate::types::AgentPostureProjection,
    pub active_waits: Vec<WaitConditionRecord>,
    pub advisories: Vec<super::super::scheduler::SchedulingAdvisory>,
    pub observability_briefs:
        Vec<Arc<super::super::resident_input_evidence::QualifiedInputEvidence>>,
    _brief_budget: Arc<BudgetViewGuard>,
    _task_selector_budget: Arc<BudgetViewGuard>,
    pub(crate) _owned_budget: Arc<BudgetViewGuard>,
    pub(crate) _task_owned_budget: Arc<BudgetViewGuard>,
    pub(crate) acquisition: crate::diagnostics::resource_io::ResourceReadMaterial,
}

pub(crate) struct ColdSchedulerFacts {
    pub projection: SchedulerProjection,
    pub posture: crate::types::AgentPostureProjection,
    pub active_waits: Vec<WaitConditionRecord>,
    pub briefs: Vec<Arc<super::super::resident_input_evidence::QualifiedInputEvidence>>,
    pub guards: Vec<Arc<BudgetViewGuard>>,
}

impl PreparedSchedulerRead {
    pub(crate) fn into_cold_facts(self) -> Result<(ColdSchedulerFacts, CapturedScheduling)> {
        let guards = self.acquisition.read(|| {
            let mut guards = Vec::new();
            for guard in [
                self._brief_budget,
                self._task_selector_budget,
                self._owned_budget,
                self._task_owned_budget,
            ] {
                crate::runtime_db::repositories::reserve_source_vec_slot(&mut guards)?;
                guards.push(guard);
            }
            Ok(guards)
        })?;
        Ok((
            ColdSchedulerFacts {
                projection: self.projection,
                active_waits: self.active_waits,
                posture: self.scheduling_posture,
                briefs: self.observability_briefs,
                guards,
            },
            self.scheduling,
        ))
    }
}

// The original command preparation still checks its durable adapter view.
// Ordinary child observability already holds one published owner root.
enum SchedulerAdapterRoot {
    Revalidate,
    HeldPublication,
    PublishedCurrent,
}

impl RuntimeMutation {
    pub(in crate::runtime) fn accepts_ordinary_scheduler_preparation(&self) -> bool {
        // Cleanup closes admission under this same owner reservation. Checking
        // before acquiring it lets an old loop republish domains after retirement.
        self.runtime.accepts_execution_admission()
    }

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
        Box::pin(self.prepare_scheduler_read_inner(message, input_roots, interrupted_sources, now))
    }

    async fn prepare_scheduler_read_inner(
        &mut self,
        message: Option<&MessageEnvelope>,
        input_roots: BTreeSet<Key>,
        interrupted_sources: BTreeSet<String>,
        now: chrono::DateTime<Utc>,
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
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
            &self.runtime.inner.input_evidence_budget,
            &self.input_evidence_scope(),
        )?;
        let mut request = WorkCaptureRequest::current();
        let mut recovery_source_ids = BTreeSet::new();
        for source in self.queue.shared_messages().map(Arc::as_ref).chain(message) {
            anyhow::ensure!(
                source.agent_id == self.state.id,
                "scheduler input scope differs"
            );
            request
                .required_trigger_message_ids
                .insert(source.id.clone());
            if super::super::task_reads::scheduler_recovery_source_required(source) {
                recovery_source_ids.insert(source.id.clone());
            }
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
                query: None,
                windows: Vec::new(),
                mandatory_roots: roots,
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                record_limit,
                byte_limit,
            };
            self.prepare_owner_input_request_with_material(
                &binding_request,
                config.recent_episode_candidates,
                &material,
            )
            .await?;
            material.read(|| {
                let inputs = self
                    .input_evidence
                    .as_ref()
                    .context("scheduler Input domain is unpublished")?;
                let captured;
                let current = match inputs.lookup_prepared_request_root(
                    &binding_request,
                    &material,
                    Family::Message,
                    &binding.source_message_id,
                )? {
                    Some(current) => current,
                    None => {
                        // Inconclusive preparation proof still uses the exact
                        // binding closure, never the whole resident history.
                        captured = inputs
                            .capture_for_request_with_material(&binding_request, &material)?;
                        captured
                            .lookup_borrowed(Family::Message, &binding.source_message_id)?
                            .context("scheduler current source is unavailable")?
                    }
                };
                let PreparedInputEvidence::Message(current) = &current.record else {
                    anyhow::bail!("scheduler current source family differs");
                };
                if super::super::task_reads::scheduler_recovery_source_required(current) {
                    crate::diagnostics::resource_io::admit_cloned_value(&current.id)?;
                    recovery_source_ids.insert(current.id.clone());
                }
                if current.kind == MessageKind::TaskResult {
                    if let MessageOrigin::Task { task_id } = &current.origin {
                        if current.task_id.as_ref() == Some(task_id)
                            && !request.required_task_ids.contains(task_id)
                        {
                            crate::diagnostics::resource_io::admit_cloned_value(task_id)?;
                            request.required_task_ids.insert(task_id.clone());
                        }
                    }
                }
                Ok(())
            })?;
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
                Some(&recovery_source_ids),
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
            load.capture.recovery_source_ids = tasks.recovery_source_ids;
            load.recovery_at = (!load.capture.recovery_source_ids.is_empty()).then_some(now);
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
        // Owner preparation below retains all Queue roots independently. This
        // read needs the actual source, not every queued Message's source index.
        if let Some(source) = message {
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
        let mut input_request = InputEvidenceLoadRequest {
            query: None,
            windows,
            mandatory_roots: roots,
            interrupted_sources,
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        drop(work);
        self.prepare_owner_input_request_with_material(
            &input_request,
            config.recent_episode_candidates,
            &material,
        )
        .await?;
        let mut read = material.read(|| {
            let inputs = self
                .input_evidence
                .as_ref()
                .context("scheduler Input domain is unpublished")?;
            input_request.extend_admitted_roots(|| inputs.scheduler_execution_roots())?;
            self.capture_scheduler_read_with_material(
                &request,
                Some(&input_request),
                Some(&recovery_source_ids),
                now,
                SchedulerAdapterRoot::Revalidate,
                &material,
            )
        })?;
        if let Some(source) = message.filter(|source| {
            matches!(&source.origin, MessageOrigin::System { subsystem }
                if subsystem == "task_result_recovery")
        }) {
            read.recovered_replies =
                self.runtime
                    .with_idle_read(Some(&read.state), &read.acquisition, |idle| {
                        let mut eligible = BTreeSet::new();
                        idle.validate_task_observation(read.tasks.revision)?;
                        let exact = super::super::task_reads::exact_recovery_metadata(
                            source,
                            &read.tasks,
                            &read.work,
                        )?;
                        let candidates = if let Some(exact) = exact {
                            let mut selected = [None;
                                crate::runtime_db::task_result_settlement::TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT];
                            selected[0] = Some(exact);
                            selected
                        } else {
                            super::super::task_reads::recovery_result_candidates(
                                &read.tasks,
                                source.work_item_id.as_deref(),
                            )?
                        };
                        idle.prepare_result_sources(
                            candidates
                                .iter()
                                .flatten()
                                .filter(|row| row.reply_message_id.is_some())
                                .map(|row| row.result_identity.as_str()),
                        )?;
                        let mut source_records = [None;
                            crate::runtime_db::task_result_settlement::TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT];
                        for (slot, metadata) in source_records.iter_mut().zip(
                            candidates
                                .iter()
                                .flatten()
                                .filter(|row| row.reply_message_id.is_some()),
                        ) {
                            *slot = Some(super::super::task_reads::result_record(
                                &read.tasks,
                                metadata,
                            )?);
                        }
                        idle.prepare_reply_source_work_records(
                            source_records.into_iter().flatten(),
                        )?;
                        for metadata in candidates.into_iter().flatten() {
                            if metadata.reply_message_id.is_some() {
                                let record =
                                    super::super::task_reads::result_record(&read.tasks, metadata)?;
                                let exact_waiter = exact.is_some_and(|exact| {
                                    exact.result_identity == record.result_identity
                                });
                                if idle.reply_source_recoverable(record)?
                                    || (exact_waiter
                                        && idle.reply_source_available_for_exact_wait(record)?)
                                {
                                    crate::diagnostics::resource_io::admit_cloned_value(
                                        &metadata.result_identity,
                                    )?;
                                    eligible.insert(metadata.result_identity.clone());
                                }
                            }
                        }
                        Ok(eligible)
                    })?;
        }
        Ok(read)
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
        let scope = self.input_evidence_scope();
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
            &self.runtime.inner.input_evidence_budget,
            &scope,
        )?;
        material.read(|| {
            self.capture_scheduler_read_with_material(
                request,
                None,
                None,
                now,
                adapter_root,
                &material,
            )
        })
    }

    fn capture_scheduler_read_with_material(
        &self,
        request: &WorkCaptureRequest,
        input_request: Option<&InputEvidenceLoadRequest>,
        recovery_source_ids: Option<&BTreeSet<String>>,
        now: chrono::DateTime<Utc>,
        adapter_root: SchedulerAdapterRoot,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<PreparedSchedulerRead> {
        self.runtime.capture_scheduler_read_from_root(
            self.capture_published_root()?.as_ref(),
            self.queue.len(),
            request,
            input_request,
            recovery_source_ids,
            now,
            adapter_root,
            material,
        )
    }
}

impl RuntimeHandle {
    pub(in crate::runtime) fn capture_operator_scheduler_from_root(
        &self,
        root: &super::super::resident_state::AgentSnapshot,
        now: chrono::DateTime<Utc>,
    ) -> Result<PreparedSchedulerRead> {
        let scope = root
            .work
            .as_ref()
            .ok_or(super::super::resident_work::WorkReadError::IncompleteLive)?
            .scope();
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
            &self.inner.input_evidence_budget,
            scope,
        )?;
        material.read(|| {
            self.capture_scheduler_read_from_root(
                root,
                root.state.pending,
                &WorkCaptureRequest::current(),
                None,
                None,
                now,
                SchedulerAdapterRoot::PublishedCurrent,
                &material,
            )
        })
    }

    fn scheduler_input_limits(&self) -> Result<(BTreeSet<Key>, usize, usize)> {
        let limits = self.inner.input_evidence_budget.limits();
        Ok((
            Default::default(),
            usize::try_from(limits.record_limit)?,
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("input evidence byte limit overflow")?,
            )?,
        ))
    }

    fn capture_scheduler_read_from_root(
        &self,
        root: &super::super::resident_state::AgentSnapshot,
        queue_len: usize,
        request: &WorkCaptureRequest,
        input_request: Option<&InputEvidenceLoadRequest>,
        recovery_source_ids: Option<&BTreeSet<String>>,
        now: chrono::DateTime<Utc>,
        adapter_root: SchedulerAdapterRoot,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<PreparedSchedulerRead> {
        crate::diagnostics::resource_io::admit_cloned_value(&self.inner.mutation_owner.agent_id())?;
        let scope = super::super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        // Task/Work/Input/State all come from one final publication.
        let (_, records, bytes) = self.scheduler_input_limits()?;
        let (task_request, task_selector_budget) =
            super::super::task_reads::scheduler_task_request(
                &scope,
                request,
                root.tasks.as_ref().map(|tasks| tasks.coverage()),
                recovery_source_ids,
                &self.inner.input_evidence_budget,
                records,
                bytes,
            )?;
        if let Some(source) = task_request.recovery_source_ids.iter().next() {
            let tasks = root
                .tasks
                .as_ref()
                .ok_or(super::super::resident_task::TaskReadError::IncompleteActive)?;
            anyhow::ensure!(
                tasks.coverage().recovery_at == Some(now),
                super::super::resident_task::TaskReadError::IncompleteRecoverySource(
                    source.clone()
                )
            );
        }
        let (state, work, inputs, tasks) = root.capture_scheduler_task_domains(
            &scope,
            request,
            input_request,
            &task_request,
            material,
        )?;
        anyhow::ensure!(
            work.scope == scope && inputs.scope == scope && tasks.scope == scope,
            "scheduler domains differ"
        );
        let scheduling = super::super::scheduling_reads::SchedulingReadSnapshot {
            token: root.token,
            state: root.state.clone(),
            scheduling: root
                .scheduling
                .as_ref()
                .ok_or(super::super::resident_scheduling::SchedulingReadError::IncompleteLive)?
                .capture(&super::super::resident_scheduling::SchedulingCaptureRequest::current())?,
        };
        anyhow::ensure!(
            scheduling.state == state && scheduling.scheduling.scope == scope,
            "scheduler Scheduling publication differs"
        );
        let (has_interrupted_replay, has_queued_input, latest_error) = if matches!(
            adapter_root,
            SchedulerAdapterRoot::PublishedCurrent
        ) {
            let query = root
                .current
                .as_ref()
                .and_then(|current| current.query.as_ref())
                .context("Current scheduler metadata needs preparation")?;
            query.validate_binding(&scope, &self.inner.input_evidence_budget)?;
            let metadata = query.scheduler;
            (
                metadata.has_interrupted_replay,
                metadata.has_queued_input,
                metadata.latest_runtime_error,
            )
        } else {
            self.inner.runtime_db.read_for_preparation(|tx| {
            let durable = ContextDurableRead::new(tx, &state.id);
            if matches!(adapter_root, SchedulerAdapterRoot::Revalidate) {
                let scope_read = durable.scope_read()?;
                let facts = scope_read.facts();
                anyhow::ensure!(
                    facts.state.as_deref() == Some(state.as_ref())
                        && facts.control_revision.is_some(),
                    "scheduler adapter State changed"
                );
                let identity = facts
                    .identity
                    .as_ref()
                    .context("scheduler adapter identity missing")?;
                anyhow::ensure!(
                    identity.agent_id == scope.agent_id
                        && identity.incarnation == scope.identity_incarnation
                        && identity.status == AgentRegistryStatus::Active,
                    "scheduler adapter identity differs"
                );
                crate::runtime_db::work_snapshot::validate_work_domain_observation_with_scope_tx(
                    tx,
                    &scope,
                    work.revision,
                    &scope_read,
                )?;
                crate::runtime_db::task_snapshot::validate_task_domain_observation_with_scope_tx(
                    tx,
                    &scope,
                    tasks.revision,
                    &scope_read,
                )?;
            }
            let interrupted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND status='interrupted')", [&state.id], |row| row.get(0))?;
            let queued: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND status='queued')",
                [&state.id],
                |row| row.get(0),
            )?;
            let latest_error = scheduler_latest_runtime_error_on(tx, &state.id)?;
            Ok((interrupted, queued, latest_error))
            })?
        };
        derive_scheduler_read(
            SchedulerReadDomains {
                state,
                work,
                inputs,
                tasks,
                scheduling: scheduling.scheduling,
                task_selector_budget,
            },
            request,
            queue_len,
            now,
            (has_interrupted_replay, has_queued_input, latest_error),
            &self.inner.input_evidence_budget,
            material,
        )
    }
}

impl RuntimeHandle {
    #[inline(never)]
    pub(in crate::runtime) fn prepare_idle_scheduler_projection(
        &self,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<Option<PreparedSchedulerRead>>> + Send + '_>,
    > {
        Box::pin(async move {
            let mut mutation = self.begin_mutation().await?;
            if !mutation.accepts_ordinary_scheduler_preparation()
                || !mutation.queue.is_empty()
                || mutation.state.current_run_id.is_some()
            {
                return Ok(None);
            }
            // An intervening producer can replace the Context window after poll.
            // The idle actor prepares its own policy; ordinary captures stay pure.
            mutation
                .prepare_scheduler_read(None, Default::default(), Default::default(), self.now())
                .await
                .map(Some)
        })
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
        let mutation = self.begin_mutation().await?;
        mutation.capture_scheduler_read(&WorkCaptureRequest::current(), self.now())
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
            let mutation = self.begin_mutation().await?;
            mutation.capture_child_observability_read(self.now())
        })
    }
}

pub(crate) struct SchedulerReadDomains {
    pub state: Arc<AgentState>,
    pub work: CapturedWork,
    pub inputs: super::super::resident_input_evidence::CapturedInputEvidence,
    pub tasks: super::super::resident_task::CapturedTasks,
    pub scheduling: CapturedScheduling,
    pub task_selector_budget: Arc<BudgetViewGuard>,
}

pub(crate) fn derive_scheduler_read(
    domains: SchedulerReadDomains,
    request: &WorkCaptureRequest,
    queue_len: usize,
    now: chrono::DateTime<Utc>,
    metadata: (bool, bool, Option<chrono::DateTime<Utc>>),
    ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    material: &crate::diagnostics::resource_io::ResourceReadMaterial,
) -> Result<PreparedSchedulerRead> {
    let SchedulerReadDomains {
        state,
        work,
        inputs,
        tasks,
        scheduling,
        task_selector_budget,
    } = domains;
    let scope = work.scope.clone();
    anyhow::ensure!(
        inputs.scope == scope
            && tasks.scope == scope
            && scheduling.scope == scope
            && state.id == scope.agent_id,
        "scheduler selected domains differ"
    );
    let limits = ledger.limits();
    let records = usize::try_from(limits.record_limit)?;
    let bytes = usize::try_from(
        limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("scheduler byte limit overflow")?,
    )?;
    let triggers = scheduling.clone_trigger_records(ledger, records, bytes)?;
    let active_timers = scheduling.active_timer_count()?;
    let (has_interrupted_replay, has_queued_input, latest_error) = metadata;
    let task_refs = active_task_records(&tasks)?;
    // Register the exact owned Task Vec before its first body clone. This
    // guard follows the projection, independently of the selected Arc view.
    let task_owned_budget =
        super::super::task_reads::projection_budget(ledger, &tasks, &task_refs, 1)?;
    crate::diagnostics::resource_io::admit_cloned_value(&task_refs)?;
    let active_tasks = task_refs.into_iter().cloned().collect::<Vec<_>>();
    let unresolved = work.unresolved_wait_conditions()?;
    let active_wait_refs = work.context_active_wait_conditions()?;
    crate::diagnostics::resource_io::admit_cloned_value(&active_wait_refs)?;
    let active_waits = active_wait_refs.into_iter().cloned().collect::<Vec<_>>();
    crate::diagnostics::resource_io::admit_cloned_value(&unresolved)?;
    crate::diagnostics::resource_io::charge_owned_metadata(
        u64::try_from(unresolved.len())?
            .checked_mul(std::mem::size_of::<(&str, WaitConditionRecord)>() as u64)
            .context("scheduler activation map slots overflow")?,
        unresolved.len(),
    )?;
    let mut activation_waits = unresolved
        .into_iter()
        .map(|wait| (wait.id.as_str(), wait.clone()))
        .collect::<BTreeMap<_, _>>();
    // Only accepted/current sources can use resolved Task Waits. Full live
    // unresolved coverage is separate and cannot prove historical absence.
    for id in &request.required_task_ids {
        for wait in work.waits_for_task(id)? {
            if wait.status == WaitConditionStatus::Resolved && wait.kind == WaitConditionKind::Task
            {
                crate::diagnostics::resource_io::admit_cloned_value(wait)?;
                crate::diagnostics::resource_io::charge_owned_metadata(
                    std::mem::size_of::<(&str, WaitConditionRecord)>() as u64,
                    1,
                )?;
                activation_waits.insert(wait.id.as_str(), wait.clone());
            }
        }
    }
    for id in &request.required_trigger_message_ids {
        for wait in work.waits_triggered_by(id)? {
            if wait.status == WaitConditionStatus::Resolved && wait.kind == WaitConditionKind::Task
            {
                crate::diagnostics::resource_io::admit_cloned_value(wait)?;
                crate::diagnostics::resource_io::charge_owned_metadata(
                    std::mem::size_of::<(&str, WaitConditionRecord)>() as u64,
                    1,
                )?;
                activation_waits.insert(wait.id.as_str(), wait.clone());
            }
        }
    }
    let briefs = inputs.window(
        &InputWindowSelector::RecentBriefs,
        super::super::scheduler::SCHEDULER_BRIEF_LIMIT,
    )?;
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
    let work_queue = work.work_queue(&state, triggers.records)?;
    let recheck_refs = work.due_work_rechecks(now)?;
    crate::diagnostics::resource_io::admit_cloned_value(&recheck_refs)?;
    let due_rechecks = recheck_refs.into_iter().cloned().collect::<Vec<_>>();
    let execution_snapshot = inputs.execution_state_if_initialized()?;
    crate::diagnostics::resource_io::charge_owned_metadata(
        u64::try_from(activation_waits.len())?
            .checked_mul(std::mem::size_of::<WaitConditionRecord>() as u64)
            .context("scheduler activation Vec slots overflow")?,
        activation_waits.len(),
    )?;
    let mut activation_waits = activation_waits.into_values().collect::<Vec<_>>();
    // latest_for_agent orders the canonical activation set by ID.
    activation_waits.sort_by(|left, right| left.id.cmp(&right.id));
    let mut owned = scheduler_owned_bytes(
        &work_queue,
        &active_waits,
        &activation_waits,
        &due_rechecks,
        &execution_snapshot,
    )?;
    let scheduling_posture =
        crate::storage::derive_agent_posture(&state, has_queued_input, &work_queue, &active_waits)?;
    SchedulerProjection::admit_retained_work(&std::borrow::Cow::Borrowed(&work_queue), material)?;
    SchedulerProjection::admit_canonical_slots(execution_snapshot.as_ref(), material)?;
    crate::diagnostics::resource_io::admit_cloned_value(&(&state.id, &state.current_run_id))?;
    let projection = SchedulerProjection::from_borrowed_observations(
        SchedulerAgentSnapshot::from_state(&state),
        queue_len,
        &work_queue,
        SchedulerObservations {
            active_tasks,
            has_interrupted_replay,
            active_wait_conditions: &active_waits,
            activation_waits,
            execution_snapshot,
            active_timers,
            runtime_error,
        },
        now,
    );
    let advisories = super::super::scheduler::scheduling_advisories_for_facts(
        &state,
        &projection,
        &scheduling_posture,
        &work_queue,
        &active_waits,
    );
    // Keep the retained Active Wait Vec and derived owned DTOs visible;
    // scheduler derivation only borrows these wait bodies.
    owned.payload_bytes = owned.payload_bytes.saturating_add(serialized_bytes(&(
        &active_waits,
        &scheduling_posture,
        &advisories,
    ))?);
    // Reuse the original window allocation, retaining only the exact last
    // 16 bodies needed by child observability. Its capacity remains charged.
    let mut observability_briefs = briefs;
    let omitted = observability_briefs.len().saturating_sub(16);
    observability_briefs.drain(..omitted);
    owned.metadata_estimate_bytes = owned
        .metadata_estimate_bytes
        .checked_add(
            u64::try_from(observability_briefs.capacity())?
                .checked_mul(u64::try_from(std::mem::size_of::<
                    Arc<super::super::resident_input_evidence::QualifiedInputEvidence>,
                >())?)
                .context("scheduler Brief slots overflow")?,
        )
        .context("scheduler Brief metadata overflow")?;
    let budget = ledger.register_owned_bytes(&scope, InputMemoryCategory::Pins, owned)?;
    let brief_budget =
        ledger.register_view(&scope, InputMemoryCategory::Pins, &observability_briefs)?;
    Ok(PreparedSchedulerRead {
        state,
        projection,
        work_queue,
        work,
        inputs,
        scheduling,
        tasks,
        recovered_replies: BTreeSet::new(),
        due_rechecks,
        scheduling_posture,
        active_waits,
        advisories,
        observability_briefs,
        _brief_budget: brief_budget,
        _task_selector_budget: task_selector_budget,
        _owned_budget: budget,
        _task_owned_budget: task_owned_budget,
        acquisition: material.clone(),
    })
}

fn scheduler_owned_bytes(
    queue: &WorkQueueReadModel,
    active_waits: &[WaitConditionRecord],
    waits: &[WaitConditionRecord],
    due: &[WorkItemRecord],
    execution: &Option<crate::domain::execution_protocol::ExecutionProtocolState>,
) -> Result<OwnedBytesEstimate> {
    let queue_bytes = serialized_bytes(&(
        &queue.current,
        &queue.queued_blocked,
        &queue.items,
        &queue.current_runnable,
        &queue.triggered_blocked,
        &queue.queued_runnable,
        &queue.yielded,
        &queue.waiting_for_operator,
        &queue.blocked,
        &queue.completing,
        &queue.completed_recent,
    ))?;
    // The projection clones only a subset of queue records; twice the complete
    // queue is an explicit conservative estimate, not allocator/RSS accounting.
    let payload_bytes = queue_bytes
        .saturating_mul(2)
        .saturating_add(serialized_bytes(&(active_waits, waits, due, execution))?);
    Ok(OwnedBytesEstimate {
        payload_bytes,
        metadata_estimate_bytes: (std::mem::size_of::<PreparedSchedulerRead>() as u64)
            .saturating_add(execution.as_ref().map_or(0, |state| {
                (state.work_items.len() as u64).saturating_mul(96)
            })),
    })
}

#[cfg(test)]
#[path = "work_planning_tests.rs"]
mod tests;
