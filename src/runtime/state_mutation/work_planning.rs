//! Explicit scheduler acquisition; pure decisions use resident Work/input facts.
use super::*;
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
    pub state: Arc<AgentState>,
    pub projection: SchedulerProjection,
    pub work_queue: WorkQueueReadModel,
    pub work: CapturedWork,
    pub due_rechecks: Vec<WorkItemRecord>,
    pub scheduling_posture: crate::types::AgentPostureProjection,
    pub active_waits: Vec<WaitConditionRecord>,
    pub advisories: Vec<super::super::scheduler::SchedulingAdvisory>,
    pub(in crate::runtime) _owned_budget: Arc<BudgetViewGuard>,
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
        self.capture_scheduler_read(&request, now)
    }

    /// Ordinary read: preparation debt remains a typed error. No Work/Input
    /// loader or historical lookup is hidden behind this capture.
    pub(in crate::runtime) fn capture_scheduler_read(
        &self,
        request: &WorkCaptureRequest,
        now: chrono::DateTime<Utc>,
    ) -> Result<PreparedSchedulerRead> {
        // Capture both domains again from their final single root.
        let (_, state, work) = self.capture_work_with_state(&request)?;
        let inputs = self.capture_input_evidence()?;
        let scope = self.input_evidence_scope();
        anyhow::ensure!(
            work.scope == scope && inputs.scope == scope,
            "scheduler domains differ"
        );
        let (
            active_tasks,
            triggers,
            active_timers,
            has_interrupted_replay,
            has_queued_input,
            events,
        ) = {
            let mut connection = self.runtime.inner.runtime_db.connection()?;
            let tx =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
            let durable = ContextDurableRead::new(&tx, &state.id);
            let facts = durable.scope_facts()?;
            anyhow::ensure!(
                facts.state.as_ref() == Some(state.as_ref()) && facts.control_revision.is_some(),
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
            crate::runtime_db::work_snapshot::validate_work_domain_observation_tx(
                &tx,
                &scope,
                work.revision,
            )?;
            // D5 Task and D6 Trigger/Timer remain explicit same-view adapters.
            let tasks = durable.active_tasks(&state.id)?;
            let triggers = durable.external_triggers()?;
            let mut query = tx
                .prepare("SELECT payload_json FROM timers WHERE agent_id=?1 AND status='active'")?;
            let mut timers = 0usize;
            for row in query.query_map([&state.id], |row| row.get::<_, String>(0))? {
                let timer: TimerRecord = serde_json::from_str(&row?)?;
                anyhow::ensure!(
                    timer.agent_id == state.id && timer.status == TimerStatus::Active,
                    "scheduler Timer projection differs"
                );
                timers += 1;
            }
            drop(query);
            let interrupted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND status='interrupted')", [&state.id], |row| row.get(0))?;
            let queued: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND status='queued')",
                [&state.id],
                |row| row.get(0),
            )?;
            // D7 event evidence remains bounded and explicit; Brief bodies are resident.
            let mut query = tx.prepare("SELECT data_json FROM audit_events WHERE agent_id=?1 ORDER BY event_seq DESC,created_at DESC LIMIT 64")?;
            let mut events = query
                .query_map([&state.id], |row| row.get::<_, String>(0))?
                .map(|row| serde_json::from_str::<AuditEvent>(&row?).map_err(Into::into))
                .collect::<Result<Vec<_>>>()?;
            events.reverse();
            drop(query);
            tx.commit()?;
            (tasks, triggers, timers, interrupted, queued, events)
        };
        let unresolved = work.unresolved_wait_conditions()?;
        let active_waits = work
            .context_active_wait_conditions()?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut activation_waits = unresolved
            .into_iter()
            .map(|wait| (wait.id.clone(), wait.clone()))
            .collect::<BTreeMap<_, _>>();
        // Only accepted/current sources can use resolved Task Waits. Full live
        // unresolved coverage is separate and cannot prove historical absence.
        for id in &request.required_task_ids {
            for wait in work.waits_for_task(id)? {
                if wait.status == WaitConditionStatus::Resolved
                    && wait.kind == WaitConditionKind::Task
                {
                    activation_waits.insert(wait.id.clone(), wait.clone());
                }
            }
        }
        for id in &request.required_trigger_message_ids {
            for wait in work.waits_triggered_by(id)? {
                if wait.status == WaitConditionStatus::Resolved
                    && wait.kind == WaitConditionKind::Task
                {
                    activation_waits.insert(wait.id.clone(), wait.clone());
                }
            }
        }
        let latest_error = events
            .iter()
            .filter(|event| event.kind == "runtime_error")
            .map(|event| event.created_at)
            .max();
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
        let work_queue = work.work_queue(&state, triggers)?;
        let due_rechecks = work
            .due_work_rechecks(now)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let execution_snapshot = inputs.execution_state_if_initialized()?;
        let mut activation_waits = activation_waits.into_values().collect::<Vec<_>>();
        // latest_for_agent orders the canonical activation set by ID.
        activation_waits.sort_by(|left, right| left.id.cmp(&right.id));
        let mut owned = scheduler_owned_bytes(
            &work_queue,
            &active_tasks,
            &active_waits,
            &activation_waits,
            &due_rechecks,
            &execution_snapshot,
        )?;
        let scheduling_posture = crate::storage::derive_agent_posture(
            &state,
            has_queued_input,
            &work_queue,
            &active_waits,
        );
        let projection = SchedulerProjection::from_observations(
            &SchedulerAgentSnapshot::from_state(&state),
            self.queue.len(),
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
        );
        let advisories = super::super::scheduler::scheduling_advisories_for_facts(
            &state,
            &projection,
            &scheduling_posture,
            &work_queue,
            &active_waits,
        );
        // Keep the separate retained Active Wait Vec and derived owned DTOs
        // visible, in addition to the constructor's transient Active Wait copy.
        owned.payload_bytes = owned.payload_bytes.saturating_add(serialized_bytes(&(
            &active_waits,
            &scheduling_posture,
            &advisories,
        ))?);
        let budget = self
            .runtime
            .inner
            .input_evidence_budget
            .register_owned_bytes(&scope, InputMemoryCategory::Pins, owned)?;
        Ok(PreparedSchedulerRead {
            state,
            projection,
            work_queue,
            work,
            due_rechecks,
            scheduling_posture,
            active_waits,
            advisories,
            _owned_budget: budget,
        })
    }
}

impl RuntimeHandle {
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
}

fn scheduler_owned_bytes(
    queue: &WorkQueueReadModel,
    tasks: &[TaskRecord],
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
        .saturating_add(serialized_bytes(&(
            tasks,
            active_waits,
            waits,
            due,
            execution,
        ))?);
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
