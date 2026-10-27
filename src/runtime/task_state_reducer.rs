use super::{scheduler, *};
#[cfg(test)]
use crate::types::BriefKind;
use crate::types::{ExecutionAdmissionProvenance, WaitConditionKind, WakeSource};
use sha2::{Digest, Sha256};

const TASK_TRANSITION_MAX_ATTEMPTS: usize = 3;

#[cfg(test)]
pub(super) struct TaskTransitionSnapshotPause {
    task_id: String,
    snapshots: tokio::sync::mpsc::UnboundedSender<(
        Option<TaskRecord>,
        TaskRecord,
        tokio::sync::oneshot::Sender<()>,
    )>,
    audit_events: tokio::sync::mpsc::UnboundedSender<AuditEvent>,
    commits: tokio::sync::mpsc::UnboundedSender<()>,
}

#[derive(Debug)]
pub(super) struct TaskTransitionRetryExhausted {
    source: anyhow::Error,
}

impl std::fmt::Display for TaskTransitionRetryExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "task transition retry budget exhausted: {}",
            self.source
        )
    }
}

impl std::error::Error for TaskTransitionRetryExhausted {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(crate) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    scheduler::is_terminal_task_status(status)
}

pub(super) fn should_ignore_task_update(latest: Option<TaskRecord>, task: &TaskRecord) -> bool {
    should_ignore_task_update_ref(latest.as_ref(), task)
}

pub(crate) fn should_ignore_task_update_ref(
    latest: Option<&TaskRecord>,
    task: &TaskRecord,
) -> bool {
    let Some(latest) = latest else {
        return false;
    };

    if is_terminal_task_status(&latest.status)
        && is_terminal_task_status(&task.status)
        && latest.status != task.status
    {
        return true;
    }

    task_status_phase(&latest.status) > task_status_phase(&task.status)
}

fn task_status_phase(status: &TaskStatus) -> u8 {
    match status {
        TaskStatus::Queued => 0,
        TaskStatus::Running => 1,
        TaskStatus::Cancelling => 2,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 3,
    }
}

pub(super) struct TaskTransition<'a> {
    pub(super) task: &'a TaskRecord,
    pub(super) event_kind: &'static str,
    pub(super) message_evidence: Option<&'a MessageEnvelope>,
    audit_evidence: Option<&'a AuditEvent>,
    pub(super) admit_result_message: bool,
    pub(super) agent_deletion: Option<crate::runtime_db::repositories::AgentDeletionRequest>,
}

impl<'a> TaskTransition<'a> {
    pub(super) fn new(task: &'a TaskRecord, event_kind: &'static str) -> Self {
        Self {
            task,
            event_kind,
            message_evidence: None,
            audit_evidence: None,
            admit_result_message: false,
            agent_deletion: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_message_evidence(mut self, message: &'a MessageEnvelope) -> Self {
        self.message_evidence = Some(message);
        self
    }

    pub(super) fn with_terminal_result(mut self, message: &'a MessageEnvelope) -> Self {
        self.message_evidence = Some(message);
        self.admit_result_message = true;
        self
    }

    pub(super) fn with_audit_evidence(mut self, event: &'a AuditEvent) -> Self {
        self.audit_evidence = Some(event);
        self
    }

    pub(super) fn with_agent_deletion(
        mut self,
        request: crate::runtime_db::repositories::AgentDeletionRequest,
    ) -> Self {
        self.agent_deletion = Some(request);
        self
    }
}

impl RuntimeHandle {
    async fn admit_task_result_with_projection(
        &self,
        message: &MessageEnvelope,
        activation_id: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        use super::resident_input_evidence::{
            InputEvidenceFamily, InputEvidenceKey, PreparedInputEvidence,
        };
        let mut mutation = self.begin_mutation().await?;
        let (mut roots, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        let key = InputEvidenceKey::new(InputEvidenceFamily::Attempt, activation_id);
        roots.insert(key.clone());
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: roots,
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        let episode_limit = self
            .inner
            .config_snapshot
            .load_full()
            .base_context_config
            .recent_episode_candidates;
        mutation
            .prepare_owner_input_request(&request, episode_limit)
            .await?;
        let inputs = {
            let root = mutation.capture_published_root()?;
            root.input_evidence
                .as_ref()
                .context("task result admission Input domain missing")?
                .capture_for_request(&request)?
        };
        drop(mutation);
        let projection = match inputs.lookup(&key)?.map(|row| &row.record) {
            Some(PreparedInputEvidence::Attempt(attempt))
                if attempt.state
                    == crate::domain::execution_protocol::ExecutionAttemptState::Open
                    && attempt.agent_id == message.agent_id
                    && attempt.source_message_id.as_deref() == Some(message.id.as_str()) =>
            {
                attempt.input_projection.as_ref()
            }
            _ => None,
        };
        self.apply_task_result_operation((&message.id, activation_id, projection, at), || {
            crate::runtime_db::transitions::TaskResultOperation::AdmitMessage {
                message_id: message.id.clone(),
                activation_id: activation_id.to_owned(),
                projection: projection.cloned(),
                at,
            }
        })
        .await?
        .record()?;
        Ok(())
    }

    async fn has_canonical_exact_task_wait(
        &self,
        message: &MessageEnvelope,
        task: &TaskRecord,
        provenance: Option<&ExecutionAdmissionProvenance>,
    ) -> Result<bool> {
        use super::resident_input_evidence::{
            InputEvidenceFamily, InputEvidenceKey, ObservedWorkExecution, PreparedInputEvidence,
        };
        let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = provenance else {
            return Ok(false);
        };
        let mut mutation = self.begin_mutation().await?;
        let mut work_request = super::work_reads::live_work_request();
        work_request
            .required_trigger_message_ids
            .insert(message.id.clone());
        mutation.prepare_owner_work_request(&work_request).await?;
        let (mut roots, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        roots.insert(InputEvidenceKey::new(
            InputEvidenceFamily::Attempt,
            activation_id,
        ));
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: roots,
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        let episode_limit = self
            .inner
            .config_snapshot
            .load_full()
            .base_context_config
            .recent_episode_candidates;
        mutation
            .prepare_owner_input_request(&request, episode_limit)
            .await?;
        let root = mutation.capture_published_root()?;
        let work = root
            .work
            .as_ref()
            .context("exact task wait Work domain was not prepared")?
            .capture(&work_request)?;
        let inputs = root
            .input_evidence
            .as_ref()
            .context("exact task wait Input domain was not prepared")?
            .capture_for_request(&request)?;
        let key = InputEvidenceKey::new(InputEvidenceFamily::Attempt, activation_id);
        let Some(PreparedInputEvidence::Attempt(attempt)) =
            inputs.lookup(&key)?.map(|row| &row.record)
        else {
            return Ok(false);
        };
        let owner_work = match &attempt.binding {
            crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } => {
                match inputs.execution_work_item(work_item_id)? {
                    ObservedWorkExecution::Present(record) => Some(record),
                    ObservedWorkExecution::Absent => None,
                    ObservedWorkExecution::Pruned => {
                        anyhow::bail!("exact task wait Work execution was not prepared")
                    }
                }
            }
            _ => None,
        };
        anyhow::ensure!(
            work.live_complete,
            "exact task wait live coverage is incomplete"
        );
        // The trigger relation includes resolved waits; the live set rejects
        // replacement waits without admitting unrelated historical bodies.
        work.waits_triggered_by(&message.id)?;
        let waits = work.records.iter().filter_map(|row| match &row.record {
            super::resident_work::CanonicalWorkRecord::Wait(wait) => Some(wait),
            _ => None,
        });
        Ok(canonical_exact_task_wait_matches(
            message,
            task,
            activation_id,
            attempt,
            owner_work,
            waits,
        ))
    }

    pub(super) async fn apply_task_transition(&self, transition: TaskTransition<'_>) -> Result<()> {
        self.apply_task_transition_inner(transition, true).await
    }

    async fn apply_task_transition_inner(
        &self,
        transition: TaskTransition<'_>,
        emit_event: bool,
    ) -> Result<()> {
        for attempt in 0..TASK_TRANSITION_MAX_ATTEMPTS {
            match self
                .apply_task_transition_attempt(&transition, emit_event)
                .await
            {
                Ok(()) => return Ok(()),
                Err(error)
                    if task_transition_error_is_retryable_conflict(&error)
                        && attempt + 1 < TASK_TRANSITION_MAX_ATTEMPTS =>
                {
                    if transition.admit_result_message {
                        // The attempt future has returned, so its local agent
                        // guard is dropped before this refresh re-locks it.
                        let agent_id = transition.task.agent_id.as_str();
                        if !self.refresh_enqueue_agent_state_baseline(agent_id).await? {
                            return Err(error);
                        }
                    }
                    continue;
                }
                Err(error) if task_transition_error_is_retryable_conflict(&error) => {
                    return Err(TaskTransitionRetryExhausted { source: error }.into());
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("task transition attempts are non-empty")
    }

    async fn apply_task_transition_attempt(
        &self,
        transition: &TaskTransition<'_>,
        emit_event: bool,
    ) -> Result<()> {
        let task = transition.task;
        let agent_id = self.agent_id().await?;
        let mut agent_guard = Some(
            self.begin_mutation_with_agent_deletion(transition.agent_deletion.as_ref())
                .await?,
        );
        let request = super::resident_task::TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            recovery_source_ids: Default::default(),
            required_keys: std::collections::BTreeSet::from([super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::Task,
                &task.id,
            )]),
        };
        let mutation = agent_guard.as_mut().expect("prepared mutation");
        mutation.prepare_owner_task_request(&request).await?;
        let task_read = mutation.capture_task_read(&request)?;
        let latest_task = super::task_reads::task_record(&task_read.tasks, &task.id)?.cloned();
        if transition.admit_result_message
            && task.is_child_agent_task()
            && task
                .detail
                .as_ref()
                .and_then(|detail| detail["source_message_id"].as_str())
                .is_some()
        {
            if let Some(latest) = latest_task
                .as_ref()
                .filter(|latest| is_terminal_task_status(&latest.status))
            {
                anyhow::ensure!(
                    latest.status == task.status,
                    "conflicting supervised invocation terminal status"
                );
                for key in [
                    "source_message_id",
                    "target_incarnation",
                    "activation_id",
                    "turn_id",
                    "completion_ref",
                    "output_summary",
                ] {
                    anyhow::ensure!(
                        latest.detail.as_ref().and_then(|detail| detail.get(key))
                            == task.detail.as_ref().and_then(|detail| detail.get(key)),
                        "conflicting supervised invocation terminal evidence"
                    );
                }
                // The first publication committed task/result/obligation together.
                // Do not create a new result or reopen a consumed queue entry.
                return Ok(());
            }
        }
        if should_ignore_task_update(latest_task.clone(), task) {
            return Ok(());
        }
        let repeated_terminal = latest_task.as_ref().is_some_and(|latest| {
            is_terminal_task_status(&latest.status)
                && is_terminal_task_status(&task.status)
                && latest.status == task.status
        });
        let persisted_task = if repeated_terminal {
            latest_task.clone().expect("repeated terminal task exists")
        } else {
            task.clone()
        };
        let task_will_change = if repeated_terminal {
            false
        } else {
            latest_task
                .as_ref()
                .map(|latest| {
                    crate::runtime_db::repositories::task_transition(latest, task).map(|outcome| {
                        outcome == crate::runtime_db::repositories::StateTransitionOutcome::Applied
                    })
                })
                .transpose()?
                .unwrap_or(true)
        };

        #[cfg(test)]
        let snapshot_pause = self
            .inner
            .task_transition_snapshot_pause
            .lock()
            .unwrap()
            .clone()
            .filter(|pause| pause.task_id == task.id);
        #[cfg(test)]
        if let Some(pause) = snapshot_pause.as_ref() {
            let (release, resume) = tokio::sync::oneshot::channel();
            pause
                .snapshots
                .send((latest_task.clone(), persisted_task.clone(), release))
                .expect("snapshot observer must remain alive");
            resume.await.expect("snapshot release must remain alive");
        }

        let (mut state, expected_state) = if let Some(guard) = agent_guard.as_ref() {
            (guard.state.clone(), guard.last_persisted_state.clone())
        } else {
            let state = self.agent_state().await?;
            let expected_state = state.clone();
            (state, expected_state)
        };
        if !matches!(state.status, AgentStatus::Stopped) && state.current_run_id.is_none() {
            scheduler::apply_idle_projection(&mut state, &self.inner.storage)?;
        }
        let expected_wait_conditions = Vec::new();
        let wait_conditions = Vec::new();
        let work_items = Vec::new();
        let mut audit_events = Vec::new();
        let mut index_changes = Vec::new();
        let _audit_evidence_budget = transition
            .audit_evidence
            .filter(|_| emit_event)
            .map(|event| {
                self.inner.input_evidence_budget.register_owned_bytes(
                    &agent_guard
                        .as_ref()
                        .expect("prepared mutation")
                        .input_evidence_scope(),
                    super::input_evidence_budget::InputMemoryCategory::Candidate,
                    super::input_evidence_budget::OwnedBytesEstimate {
                        payload_bytes: super::input_evidence_budget::serialized_bytes(event)?
                            .checked_mul(8)
                            .context("Task Audit evidence material overflow")?,
                        metadata_estimate_bytes: 1024,
                    },
                )
            })
            .transpose()?;
        if task_will_change {
            index_changes.extend(self.inner.storage.index_changes_for_task(task)?);
        }
        if emit_event {
            let payload = TaskLifecycleAuditEvent::from_task(&persisted_task);
            let mut skip_event = false;
            let mut event =
                if let Some(kind) = RuntimeEventKind::from_wire_name(transition.event_kind) {
                    AuditEvent::typed(kind, &payload)?
                } else {
                    AuditEvent::legacy(transition.event_kind, to_json_value(&payload))
                };
            if is_terminal_task_status(&persisted_task.status) {
                event.id = stable_terminal_task_event_id(transition.event_kind, &persisted_task);
                event.created_at = persisted_task.updated_at;
                // When a terminal transition is repeated (e.g. duplicate
                // task_result delivery after a concurrent writer modified the
                // stored task), the stable event id collides with the original
                // emission but the payload may differ.  Skip re-emission when
                // the event already exists to avoid a content conflict.
                skip_event = repeated_terminal
                    && self
                        .inner
                        .runtime_db
                        .audit_events()
                        .has_event_by_id(&event.id)?;
            }
            if !skip_event {
                #[cfg(test)]
                if let Some(pause) = snapshot_pause.as_ref() {
                    pause
                        .audit_events
                        .send(event.clone())
                        .expect("audit observer must remain alive");
                }
                audit_events.push(event);
            }
        }
        if let Some(event) = transition.audit_evidence.filter(|_| emit_event) {
            audit_events.push(event.clone());
        }
        #[cfg(test)]
        self.inject_task_transition_conflict_if_armed().await?;
        #[cfg(test)]
        if transition.admit_result_message {
            self.inject_terminal_task_transition_conflict_if_armed()?;
        }
        let existing_queue_entry = transition
            .message_evidence
            .filter(|_| transition.admit_result_message)
            .map(|message| self.inner.runtime_db.queue_entries().latest(&message.id))
            .transpose()?
            .flatten();
        let result_message_is_new = transition
            .message_evidence
            .filter(|_| transition.admit_result_message)
            .map(|message| self.inner.storage.read_message_by_id(&message.id))
            .transpose()?
            .flatten()
            .is_none();
        let queue_entry = transition
            .message_evidence
            .filter(|_| transition.admit_result_message)
            .filter(|_| {
                existing_queue_entry.as_ref().is_none_or(|entry| {
                    matches!(
                        entry.status,
                        QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                    )
                })
            })
            .map(|message| QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: existing_queue_entry
                    .as_ref()
                    .map_or(message.created_at, |entry| entry.created_at),
                updated_at: self.now(),
            });
        if let Some(message) = transition
            .message_evidence
            .filter(|_| queue_entry.is_some() && result_message_is_new)
        {
            audit_events.extend([
                AuditEvent::legacy(
                    "message_admitted",
                    serde_json::json!({
                        "message_id": message.id,
                        "agent_id": message.agent_id,
                        "kind": message.kind,
                        "origin": message.origin,
                        "authority_class": message.authority_class,
                        "delivery_surface": message.delivery_surface,
                        "admission_context": message.admission_context,
                        "trigger_kind": message.trigger_kind,
                        "work_item_id": message.work_item_id,
                        "task_id": message.task_id,
                        "source_refs": message.source_refs,
                        "correlation_id": message.correlation_id,
                        "causation_id": message.causation_id,
                    }),
                ),
                AuditEvent::typed(
                    RuntimeEventKind::MessageEnqueued,
                    &MessageLifecycleAuditEvent::from_message(message),
                )?,
            ]);
        }
        let queue_needs_push = queue_entry.as_ref().is_some_and(|entry| {
            agent_guard.as_ref().is_some_and(|guard| {
                guard
                    .queue
                    .peek_next_matching(|message| message.id == entry.message_id)
                    .is_none()
            })
        });
        if queue_needs_push {
            state.pending = agent_guard
                .as_ref()
                .map_or(state.pending.saturating_add(1), |guard| {
                    guard.queue.len().saturating_add(1)
                });
            state.last_wake_reason = Some("TaskResult".into());
            state.total_message_count = self
                .inner
                .storage
                .count_messages()?
                .saturating_add(usize::from(result_message_is_new));
            scheduler::apply_message_wake_projection(&mut state);
        }
        let agent_state =
            (state != expected_state).then(|| crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(expected_state)),
                record: Box::new(state.clone()),
            });
        let message_evidence = transition
            .message_evidence
            .map(|message| {
                let mut message = message.clone();
                message.normalize_admission_fields();
                message
            })
            .into_iter()
            .collect();
        let task_result_settlement = transition
            .message_evidence
            .filter(|_| {
                transition.admit_result_message && is_terminal_task_status(&persisted_task.status)
            })
            .map(|message| {
                crate::runtime_db::TaskResultSettlementRecord::pending(
                    &persisted_task,
                    message,
                    self.now(),
                )
            })
            .transpose()?
            .flatten();
        let _control_registration = matches!(
            persisted_task.status,
            TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
        )
        .then(|| {
            self.inner.control_cancellations.before_active_task_sql(
                &persisted_task,
                self.inner.identity_incarnation,
                self.inner.mutation_owner.owner_epoch(),
            )
        });
        let command = crate::runtime_db::transitions::TaskTransitionCommand {
            agent_id,
            expected_owner_incarnation: self.inner.identity_incarnation,
            task: persisted_task,
            agent_deletion: transition.agent_deletion.clone(),
            task_result_settlement,
            queue_entry,
            work_items,
            expected_wait_conditions,
            wait_conditions,
            agent_state,
            message_evidence,
            audit_events,
            index_changes,
            notify_scheduler: queue_needs_push,
            commit_on_idempotent: emit_event
                && !task_will_change
                && is_terminal_task_status(&task.status),
            fault: self.take_transition_fault(),
        };
        let (commit, _original_deletion) = agent_guard
            .as_mut()
            .expect("prepared mutation")
            .commit_task_transition(
                crate::ids::audit_event_id(),
                command,
                Default::default(),
                true,
            )
            .await?;
        #[cfg(test)]
        if let Some(pause) = snapshot_pause.as_ref() {
            pause
                .commits
                .send(())
                .expect("commit observer must remain alive");
        }
        drop(agent_guard);
        self.apply_transition_commit(commit).await;
        Ok(())
    }

    #[cfg(test)]
    async fn inject_task_transition_conflict_if_armed(&self) -> Result<()> {
        let mut remaining = self
            .inner
            .task_transition_conflicts_remaining
            .load(Ordering::SeqCst);
        loop {
            if remaining == 0 {
                return Ok(());
            }
            match self
                .inner
                .task_transition_conflicts_remaining
                .compare_exchange_weak(remaining, remaining - 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(observed) => remaining = observed,
            }
        }
        let mut guard = self.inner.agent.lock().await;
        guard.state.pending_wake_hint = Some(crate::types::PendingWakeHint {
            reason: "test_conflict".into(),
            description: Some(format!(
                "concurrent task transition test attempt {remaining}"
            )),
            source: Some("test".into()),
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        guard.persist_state(&self.inner.storage)
    }

    #[cfg(test)]
    pub(crate) fn inject_task_transition_conflicts(&self, count: usize) {
        self.inner
            .task_transition_conflicts_remaining
            .store(count, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn inject_terminal_task_transition_conflict_if_armed(&self) -> Result<()> {
        let mut remaining = self
            .inner
            .terminal_task_transition_conflicts_remaining
            .load(Ordering::SeqCst);
        loop {
            if remaining == 0 {
                return Ok(());
            }
            match self
                .inner
                .terminal_task_transition_conflicts_remaining
                .compare_exchange_weak(remaining, remaining - 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(observed) => remaining = observed,
            }
        }
        let mut state = self
            .inner
            .runtime_db
            .agent_states()
            .latest(&self.inner.default_agent_id)?
            .expect("test runtime agent state");
        state.pending_wake_hint = Some(crate::types::PendingWakeHint {
            reason: "test_terminal_conflict".into(),
            description: Some(format!(
                "concurrent terminal task transition test attempt {remaining}"
            )),
            source: Some("test".into()),
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        self.inner.storage.write_agent_for_fixture(&state)
    }

    #[cfg(test)]
    pub(crate) fn inject_terminal_task_transition_conflicts(&self, count: usize) {
        self.inner
            .terminal_task_transition_conflicts_remaining
            .store(count, Ordering::SeqCst);
    }

    pub(super) async fn persist_task_transition(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
    ) -> Result<()> {
        self.apply_task_transition(TaskTransition::new(task, event_kind))
            .await?;
        if matches!(
            task.status,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            if let Err(error) = self.record_decision_outcome_for_task(task).await {
                tracing::warn!(
                    error = %error,
                    task_id = %task.id,
                    "failed to append decision outcome telemetry"
                );
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn persist_task_transition_with_message(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
        message: &MessageEnvelope,
    ) -> Result<()> {
        self.apply_task_transition(
            TaskTransition::new(task, event_kind).with_message_evidence(message),
        )
        .await
    }

    pub(super) async fn commit_terminal_task_result(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
        message: &MessageEnvelope,
    ) -> Result<()> {
        self.commit_terminal_task_result_with_agent_deletion(task, event_kind, message, None)
            .await
    }

    pub(super) async fn commit_terminal_task_result_with_agent_deletion(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
        message: &MessageEnvelope,
        agent_deletion: Option<crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> Result<()> {
        let mut message = message.clone();
        message.normalize_admission_fields();
        let mut transition = TaskTransition::new(task, event_kind).with_terminal_result(&message);
        if let Some(agent_deletion) = agent_deletion {
            transition = transition.with_agent_deletion(agent_deletion);
        }
        match self.apply_task_transition(transition).await {
            Err(error) => {
                // A competing terminal winner can be observed before the
                // writer's target-fence check. Deletion-related losers converge.
                if let Some(target) = task.target() {
                    let db = self.inner.runtime_db.clone();
                    if db
                        .with_read_async(move |connection| {
                            crate::runtime_db::repositories::task_target_is_deleted(
                                connection, &target,
                            )
                        })
                        .await?
                        && self.task_record(&task.id).await?.is_some_and(|latest| {
                            latest.agent_id == task.agent_id
                                && is_terminal_task_status(&latest.status)
                        })
                    {
                        return Ok(());
                    }
                }
                if error
                    .downcast_ref::<crate::runtime_db::repositories::TaskTargetDeleted>()
                    .is_some()
                {
                    self.commit_deleted_target_cancellation(task, &message)
                        .await
                } else {
                    Err(error)
                }
            }
            result => result,
        }
    }

    async fn commit_deleted_target_cancellation(
        &self,
        task: &TaskRecord,
        original_message: &MessageEnvelope,
    ) -> Result<()> {
        if self
            .task_record(&task.id)
            .await?
            .is_some_and(|latest| is_terminal_task_status(&latest.status))
        {
            return Ok(());
        }
        let target = task
            .target()
            .ok_or_else(|| anyhow!("deleted task target binding is missing"))?;
        let db = self.inner.runtime_db.clone();
        let checked = target.clone();
        anyhow::ensure!(
            db.with_read_async(move |connection| {
                crate::runtime_db::repositories::task_target_is_deleted(connection, &checked)
            })
            .await?,
            "task target deletion evidence changed"
        );
        let incarnation = target.incarnation.unwrap_or(1);
        let text = format!(
            "Agent {} incarnation {incarnation}: target agent was deleted; invocation cancelled.",
            target.agent_id
        );
        let mut detail = task.detail.clone().unwrap_or_else(|| serde_json::json!({}));
        detail["target_deleted"] = serde_json::json!(true);
        detail["target_incarnation"] = serde_json::json!(incarnation);
        detail["cancellation_reason"] = serde_json::json!("agent_deleted");
        detail["output_summary"] = serde_json::json!(text);
        {
            let mut mutation = self.begin_mutation().await?;
            let _ = mutation
                .retain_task_owned_worktree_for_target_deletion_in_detail(&task.id, &mut detail)
                .await?;
        }
        let mut message = original_message.clone();
        let prefix = if task.kind == TaskKind::AgentMessageWait {
            "agent-message-wait"
        } else {
            "child-invocation-result"
        };
        message.id = format!("message:{prefix}:{}", task.id);
        message.turn_id = Some(format!("turn:{prefix}:{}", task.id));
        message.created_at = task.created_at;
        message.body = MessageBody::Text { text };
        message.authority_class = AuthorityClass::RuntimeInstruction;
        message.metadata = Some(serde_json::json!({
            "task_id": task.id, "task_kind": task.kind, "task_status":"cancelled",
            "task_summary": task.summary, "task_recovery": task.recovery,
            "work_item_id":task.work_item_id, "task_detail":detail,
        }));
        let cancelled = super::tasks::task_with_result_message(
            task,
            TaskStatus::Cancelled,
            Some(detail),
            &message,
        );
        message.metadata.as_mut().unwrap()["task_detail"] =
            serde_json::to_value(&cancelled.detail)?;
        let result = self
            .apply_task_transition(
                TaskTransition::new(&cancelled, "task_cancelled_by_agent_deletion")
                    .with_terminal_result(&message),
            )
            .await;
        if result.is_err()
            && self
                .task_record(&task.id)
                .await?
                .is_some_and(|latest| is_terminal_task_status(&latest.status))
        {
            return Ok(());
        }
        result
    }

    #[cfg(test)]
    pub(super) async fn commit_terminal_child_task_result(
        &self,
        task: &TaskRecord,
        event_kind: &'static str,
        message: &MessageEnvelope,
        deletion: Option<crate::runtime_db::repositories::AgentDeletionRequest>,
    ) -> Result<bool> {
        let deletion_requested = deletion.is_some();
        match self
            .commit_terminal_task_result_with_agent_deletion(
                task,
                event_kind,
                message,
                deletion,
            )
            .await
        {
            Ok(()) => Ok(deletion_requested),
            Err(error)
                if deletion_requested
                    && crate::runtime_db::repositories::
                        is_terminal_child_deletion_admission_rejected(&error) =>
            {
                tracing::warn!(
                    task_id = %task.id,
                    error = %error,
                    "terminal child deletion admission rejected; settling task without deletion"
                );
                self.commit_terminal_task_result(task, event_kind, message)
                    .await?;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn reduce_task_status_message(&self, task: TaskRecord) -> Result<()> {
        self.persist_task_transition(&task, "task_status_updated")
            .await
    }

    #[cfg(test)]
    pub(super) async fn reduce_task_result_message(
        &self,
        message: &MessageEnvelope,
        task: TaskRecord,
        model_reentry: bool,
        continuation_resolution: Option<&ContinuationResolution>,
    ) -> Result<()> {
        let transition = self
            .reduce_task_result_message_deferred(
                message,
                task,
                model_reentry,
                continuation_resolution,
                None,
            )
            .await?;
        self.persist_terminal_transition(&transition).await?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn reduce_task_result_message_deferred(
        &self,
        message: &MessageEnvelope,
        task: TaskRecord,
        model_reentry: bool,
        continuation_resolution: Option<&ContinuationResolution>,
        execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
    ) -> Result<turn::TurnTerminalTransition> {
        Box::pin(self.reduce_task_result_for_dispatch(
            message,
            task,
            model_reentry,
            continuation_resolution,
            execution_admission_provenance,
            None,
        ))
        .await
    }

    pub(super) async fn reduce_task_result_for_dispatch(
        &self,
        message: &MessageEnvelope,
        task: TaskRecord,
        model_reentry: bool,
        continuation_resolution: Option<&ContinuationResolution>,
        execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
        dispatch_prior_closure: Option<&ClosureDecision>,
    ) -> Result<turn::TurnTerminalTransition> {
        let result_at = self.now();
        let settlement_read = self
            .apply_task_result_operation((&task.id, message, result_at), || {
                crate::runtime_db::transitions::TaskResultOperation::EnsurePending {
                    task_id: task.id.clone(),
                    message: Box::new(message.clone()),
                    at: result_at,
                }
            })
            .await?;
        let settlement = settlement_read.record()?;
        if settlement.is_some() && model_reentry {
            if let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) =
                execution_admission_provenance.as_ref()
            {
                let admitted_at = self.now();
                self.admit_task_result_with_projection(message, activation_id, admitted_at)
                    .await?;
            }
        }
        if let Some(settlement) = settlement.as_ref() {
            let owner = self.acquire_task_result_owner(settlement).await?;
            let unavailable = owner.disposition()?;
            drop(owner);
            if let Some(disposition) = unavailable {
                let settled_at = self.now();
                self.apply_task_result_operation((&message.id, disposition, settled_at), || {
                    crate::runtime_db::transitions::TaskResultOperation::SettleOwnerUnavailable {
                        message_id: message.id.clone(),
                        disposition,
                        at: settled_at,
                    }
                })
                .await?
                .changed()?;
            }
        }
        let routing = self.acquire_task_result_routing(message, &task).await?;
        if routing.should_ignore() {
            drop(routing);
            self.begin_reducer_only_turn(message, dispatch_prior_closure)
                .await?;
            return self
                .build_reducer_only_terminal_transition("reducer_only/duplicate_task_result", false)
                .await;
        }
        let suppress_delivered_parent_reentry = routing.parent_delivered()?
            && !(model_reentry
                && self
                    .has_canonical_exact_task_wait(
                        message,
                        &task,
                        execution_admission_provenance.as_ref(),
                    )
                    .await?);
        drop(routing);
        if !model_reentry || suppress_delivered_parent_reentry {
            let (operator_binding_id, operator_reply_route_id) =
                Self::operator_transport_from_message(message);
            // Suppressing model reentry must not discard an admitted settlement.
            self.begin_interactive_turn_with_provenance(
                Some(message),
                operator_binding_id.as_deref(),
                operator_reply_route_id.as_deref(),
                execution_admission_provenance.clone(),
            )
            .await?;
        }
        self.persist_task_transition(&task, "task_result_received")
            .await?;

        let task_status_label = match task.status {
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Interrupted => "interrupted",
            TaskStatus::Cancelling => "cancelling",
            TaskStatus::Running => "running",
            TaskStatus::Queued => "queued",
        };
        let emit_result_brief =
            should_emit_task_result_brief(&task) && !suppress_delivered_parent_reentry;
        let result_text = match &message.body {
            MessageBody::Text { text } => {
                format!("Task {} {}: {}", task.id, task_status_label, text)
            }
            MessageBody::Json { value } => {
                format!("Task {} {}: {}", task.id, task_status_label, value)
            }
            MessageBody::Brief { text, .. } => {
                format!("Task {} {}: {}", task.id, task_status_label, text)
            }
        };
        if model_reentry && !suppress_delivered_parent_reentry {
            if let Some(work_item_id) = message
                .work_item_id
                .clone()
                .or_else(|| task.effective_work_item_id().map(ToString::to_string))
            {
                let mut guard = self.begin_mutation().await?;
                guard.state.current_turn_work_item_id = Some(work_item_id);
                guard.persist_state(&self.inner.storage).await?;
            }
            let trace_context = message.trace_context.clone();
            let attributes = crate::observability::TraceAttributes {
                agent_id: Some(message.agent_id.clone()),
                message_id: Some(message.id.clone()),
                turn_id: message.turn_id.clone(),
                work_item_id: message.work_item_id.clone(),
                task_id: message.task_id.clone(),
                ..Default::default()
            };
            if let Some(parent) = trace_context.as_ref() {
                let queue_context = parent.child();
                crate::observability::record_span(
                    &queue_context,
                    crate::observability::completed_span(
                        "holon.scheduler.queue_wait",
                        &queue_context,
                        Some(parent.span_id.clone()),
                        message.created_at,
                        crate::observability::TraceSpanStatus::Ok,
                        attributes.clone(),
                    ),
                );
            }
            let turn_context = trace_context
                .as_ref()
                .map(crate::observability::TraceContext::child);
            let turn_started_at = chrono::Utc::now();
            let result = self
                .process_interactive_dispatch_with_cleanup(
                    message,
                    continuation_resolution,
                    execution_admission_provenance,
                    LoopControlOptions {
                        max_tool_rounds: None,
                    },
                    turn_context.clone(),
                    dispatch_prior_closure,
                )
                .await;
            if let Some(context) = turn_context.as_ref() {
                let status = if result.is_ok() {
                    crate::observability::TraceSpanStatus::Ok
                } else {
                    crate::observability::TraceSpanStatus::Error
                };
                crate::observability::record_span(
                    context,
                    crate::observability::completed_span(
                        "holon.turn",
                        context,
                        trace_context.as_ref().map(|parent| parent.span_id.clone()),
                        turn_started_at,
                        status,
                        attributes,
                    ),
                );
            }
            return result;
        } else if !model_reentry {
            if emit_result_brief {
                let mut brief = brief::make_task_result(&message.agent_id, &task.id, &result_text);
                brief.related_message_id = Some(message.id.clone());
                self.persist_brief(&brief).await?;
            }
        } else {
            self.append_original_audit_event(AuditEvent::legacy(
                "stale_task_result_rejoin_suppressed",
                serde_json::json!({
                    "agent_id": message.agent_id,
                    "task_id": task.id,
                    "parent_turn_id": task_parent_turn_id(&task),
                    "reason": "parent_turn_already_delivered",
                }),
            ))
            .await?;
        }
        self.build_reducer_only_terminal_transition(
            if suppress_delivered_parent_reentry {
                "reducer_only/parent_turn_already_delivered"
            } else {
                "reducer_only/task_result_without_model_reentry"
            },
            emit_result_brief,
        )
        .await
    }
}

fn task_transition_error_is_retryable_conflict(error: &anyhow::Error) -> bool {
    crate::runtime_db::retryable_state_transition_conflict(error).is_some()
}

pub(crate) fn stable_terminal_task_event_id(event_kind: &str, task: &TaskRecord) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event_kind.as_bytes());
    hasher.update([0]);
    hasher.update(task.id.as_bytes());
    hasher.update([0]);
    if let Some(message_id) = task.parent_message_id.as_deref() {
        hasher.update(message_id.as_bytes());
    }
    hasher.update([0]);
    let status = match task.status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    };
    hasher.update(status.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("event_{}", &digest[..15])
}

fn should_emit_task_result_brief(task: &TaskRecord) -> bool {
    task.kind != TaskKind::CommandTask && task.kind != TaskKind::AgentMessageWait
}

fn task_parent_turn_id(task: &TaskRecord) -> Option<&str> {
    task.detail
        .as_ref()
        .and_then(|detail| detail.get("parent_turn_id"))
        .and_then(serde_json::Value::as_str)
}

#[cfg(test)]
fn task_result_has_canonical_exact_wait(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
    task: &TaskRecord,
    provenance: Option<&ExecutionAdmissionProvenance>,
) -> Result<bool> {
    let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = provenance else {
        return Ok(false);
    };
    let Some(state) = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized(&message.agent_id)?
    else {
        return Ok(false);
    };
    let Some(attempt) = state.attempts.get(activation_id) else {
        return Ok(false);
    };
    let owner_work = match &attempt.binding {
        crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } => {
            state.work_items.get(work_item_id)
        }
        _ => None,
    };
    let waits = runtime
        .inner
        .storage
        .latest_wait_conditions_for_agent(&message.agent_id)?;
    Ok(canonical_exact_task_wait_matches(
        message,
        task,
        activation_id,
        attempt,
        owner_work,
        waits.iter(),
    ))
}

fn canonical_exact_task_wait_matches<'a>(
    message: &MessageEnvelope,
    task: &TaskRecord,
    activation_id: &str,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    owner_work: Option<&crate::domain::execution_protocol::WorkItemExecutionRecord>,
    waits: impl Iterator<Item = &'a WaitConditionRecord> + Clone,
) -> bool {
    use crate::domain::execution_protocol::{
        ExecutionAttemptState, ExecutionBinding, ExecutionSourceIdentity, WorkItemExecutionState,
    };
    if attempt.state != ExecutionAttemptState::Open
        || attempt.agent_id != message.agent_id
        || attempt.source_message_id.as_deref() != Some(message.id.as_str())
        || task.agent_id != message.agent_id
        || message.kind != MessageKind::TaskResult
        || message.task_id.as_deref() != Some(task.id.as_str())
        || !matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &task.id)
    {
        return false;
    }
    let owner = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => {
            if !owner_work.is_some_and(|work| {
                matches!(&work.state, WorkItemExecutionState::InFlight { attempt_id, .. } if attempt_id == activation_id)
            }) {
                return false;
            }
            Some(work_item_id.as_str())
        }
        ExecutionBinding::AgentLifecycle { agent_id } if agent_id != &message.agent_id => {
            return false;
        }
        _ => None,
    };
    let wait_id = match &attempt.source.identity {
        ExecutionSourceIdentity::TriggeredWait {
            wait_id,
            trigger_message_id,
            ..
        } if trigger_message_id == &message.id => Some(wait_id.as_str()),
        ExecutionSourceIdentity::TaskResult {
            task_id,
            result_message_id,
        } if task_id == &task.id
            && result_message_id == &message.id
            && owner.is_some()
            && task.effective_work_item_id() == owner
            && message.work_item_id.as_deref() == owner =>
        {
            None
        }
        _ => return false,
    };
    // A replacement wait must not lend its predecessor's resolved trigger.
    if waits.clone().any(|wait| {
        wait.work_item_id.as_deref() == owner
            && matches!(
                wait.status,
                WaitConditionStatus::Active | WaitConditionStatus::Triggered
            )
    }) {
        return false;
    }
    let mut matching = waits.filter(|wait| {
        wait_id.is_none_or(|wait_id| wait.id == wait_id)
                && wait.work_item_id.as_deref() == owner
                && wait.status == WaitConditionStatus::Resolved
                && wait.kind == WaitConditionKind::Task
                && wait.trigger_message_id() == Some(message.id.as_str())
                && wait.wake_sources.iter().any(|source| {
                    matches!(source, WakeSource::TaskResult { task_id } if task_id == &task.id)
                })
    });
    matching.next().is_some() && matching.next().is_none()
}

#[cfg(test)]
fn task_result_parent_turn_already_delivered(
    storage: &AppStorage,
    task: &TaskRecord,
) -> Result<bool> {
    let Some(parent_turn_id) = task_parent_turn_id(task) else {
        return Ok(false);
    };
    let Some(parent_turn) = storage.read_turn_by_id(parent_turn_id)? else {
        return Ok(false);
    };
    Ok(storage
        .read_briefs_by_ids(&parent_turn.produced_brief_ids)?
        .iter()
        .any(|brief| brief.kind == BriefKind::Result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::ContextConfig,
        domain::execution_protocol::{
            AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding,
            ExecutionOrigin, ExecutionPriority, ExecutionProtocolState, ExecutionProvenance,
            ExecutionSource, ExecutionSourceIdentity, ExecutionTrust, WorkItemExecutionRecord,
            WorkItemExecutionState,
        },
        provider::StubProvider,
        types::{AuthorityClass, MessageKind, MessageOrigin, Priority},
    };
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::{tempdir, TempDir};

    fn task_with_kind(id: &str, status: TaskStatus, blocking: bool, kind: TaskKind) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind,
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(format!("task {id}")),
            detail: blocking.then(|| json!({ "wait_policy": "blocking" })),
            recovery: None,
        }
    }

    fn task(id: &str, status: TaskStatus, blocking: bool) -> TaskRecord {
        task_with_kind(id, status, blocking, TaskKind::ChildAgentTask)
    }

    fn scheduler_blocking_task(id: &str, status: TaskStatus) -> TaskRecord {
        task_with_kind(id, status, true, TaskKind::SleepJob)
    }

    struct RuntimeFixture {
        runtime: RuntimeHandle,
        _dir: TempDir,
        _workspace: TempDir,
    }

    impl std::ops::Deref for RuntimeFixture {
        type Target = RuntimeHandle;

        fn deref(&self) -> &Self::Target {
            &self.runtime
        }
    }

    fn runtime() -> RuntimeFixture {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        RuntimeFixture {
            runtime,
            _dir: dir,
            _workspace: workspace,
        }
    }

    fn task_result_message(task_id: &str) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task_id.into(),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "task finished".into(),
            },
        );
        message.task_id = Some(task_id.into());
        message
    }

    struct ExactTaskWaitFixture {
        runtime: RuntimeFixture,
        task: TaskRecord,
        message: MessageEnvelope,
        execution: ExecutionProtocolState,
        waits: Vec<WaitConditionRecord>,
        provenance: ExecutionAdmissionProvenance,
    }

    impl ExactTaskWaitFixture {
        fn new(triggered_wait: bool) -> Self {
            let runtime = runtime();
            let mut task = task_with_kind(
                "task-exact",
                TaskStatus::Completed,
                false,
                TaskKind::CommandTask,
            );
            task.work_item_id = Some("task-owner".into());
            task.detail = Some(json!({"parent_turn_id": "turn-final-wait"}));
            let mut message = task_result_message(&task.id);
            message.work_item_id = task.work_item_id.clone();
            let owner = if triggered_wait {
                "wait-owner"
            } else {
                "task-owner"
            };
            let now = Utc::now();
            let wait = WaitConditionRecord {
                id: "wait-exact".into(),
                agent_id: message.agent_id.clone(),
                work_item_id: Some(owner.into()),
                status: WaitConditionStatus::Resolved,
                kind: WaitConditionKind::Task,
                source: None,
                subject_ref: Some(task.id.clone()),
                waiting_for: "exact command completion".into(),
                wake_sources: vec![WakeSource::TaskResult {
                    task_id: task.id.clone(),
                }],
                continuation: Some(json!({"delivery": "final"})),
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: Some(now),
                cancelled_at: None,
                turn_id: Some("turn-final-wait".into()),
                trigger_message_id: Some(message.id.clone()),
                triggered_at: Some(now),
            };
            let mut execution = ExecutionProtocolState::empty(&message.agent_id);
            execution.work_items.insert(
                owner.into(),
                WorkItemExecutionRecord {
                    source_revision: 1,
                    state: WorkItemExecutionState::InFlight {
                        generation: 1,
                        attempt_id: "activation-exact".into(),
                    },
                },
            );
            execution.attempts.insert(
                "activation-exact".into(),
                ExecutionAttempt {
                    input_projection: None,
                    attempt_id: "activation-exact".into(),
                    agent_id: message.agent_id.clone(),
                    source_message_id: Some(message.id.clone()),
                    source: ExecutionSource {
                        identity: if triggered_wait {
                            ExecutionSourceIdentity::TriggeredWait {
                                wait_id: wait.id.clone(),
                                trigger_message_id: message.id.clone(),
                                ingress: None,
                            }
                        } else {
                            ExecutionSourceIdentity::TaskResult {
                                task_id: task.id.clone(),
                                result_message_id: message.id.clone(),
                            }
                        },
                        generation: 1,
                    },
                    binding: ExecutionBinding::WorkItem {
                        work_item_id: owner.into(),
                    },
                    provenance: ExecutionProvenance {
                        origin: ExecutionOrigin::Task,
                        trust: ExecutionTrust::RuntimeInstruction,
                        priority: ExecutionPriority::Normal,
                        correlation_id: None,
                        causation_id: None,
                    },
                    admitted_fences: AdmittedFences {
                        source_revision: 1,
                        work_item_source_revision: Some(1),
                        work_item_generation: Some(1),
                        rejoin: None,
                        agent_control_revision: 1,
                        host_registry_revision: 1,
                    },
                    state: ExecutionAttemptState::Open,
                    run_id: None,
                    turn_id: None,
                    recovery_of_attempt_id: None,
                    terminal_outcome_id: None,
                    admitted_at: now.to_rfc3339(),
                    terminal_at: None,
                },
            );
            Self {
                runtime,
                task,
                message,
                execution,
                waits: vec![wait],
                provenance: ExecutionAdmissionProvenance::Canonical {
                    scenario_class: if triggered_wait {
                        scheduler::EXACT_WAIT_RESUME_SCENARIO
                    } else {
                        scheduler::EXACT_TASK_REJOIN_SCENARIO
                    },
                    activation_id: "activation-exact".into(),
                },
            }
        }

        fn has_exact_wait(&self) -> bool {
            for wait in &self.waits {
                self.runtime.storage().append_wait_condition(wait).unwrap();
            }
            self.runtime
                .runtime_db()
                .transaction(|tx| {
                    crate::runtime_db::transitions::persist_state_tx(tx, &self.execution)
                })
                .unwrap();
            task_result_has_canonical_exact_wait(
                &self.runtime,
                &self.message,
                &self.task,
                Some(&self.provenance),
            )
            .unwrap()
        }
    }

    #[tokio::test]
    async fn same_owner_final_wait_task_result_exempts_delivered_parent_suppression() {
        let fixture = ExactTaskWaitFixture::new(false);
        let mut delivered = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Waiting for the exact command.",
            None,
            None,
        );
        delivered.turn_id = Some("turn-final-wait".into());
        fixture.runtime.storage().append_brief(&delivered).unwrap();
        let mut turn = TurnRecord::new("default", "turn-final-wait", 1);
        turn.produced_brief_ids = vec![delivered.id];
        fixture.runtime.storage().append_turn(&turn).unwrap();

        assert!(task_result_parent_turn_already_delivered(
            fixture.runtime.storage(),
            &fixture.task
        )
        .unwrap());
        assert!(fixture.has_exact_wait());
        assert!(!task_result_has_canonical_exact_wait(
            &fixture.runtime,
            &fixture.message,
            &fixture.task,
            None
        )
        .unwrap());
    }

    #[tokio::test]
    async fn cross_owner_triggered_wait_retains_exact_task_exemption() {
        assert!(ExactTaskWaitFixture::new(true).has_exact_wait());
    }

    #[tokio::test]
    async fn canonical_task_wait_exemption_rejects_missing_replaced_and_mismatched_evidence() {
        for case in [
            "no_wait",
            "cancelled_wait",
            "unresolved_wait",
            "sibling_wait",
            "wrong_wait_kind",
            "wrong_trigger",
            "wrong_wake_task",
            "wrong_message_task",
            "wrong_message_origin",
            "wrong_message_kind",
            "wrong_message_owner",
            "wrong_task_owner",
            "wrong_task_agent",
            "missing_activation",
            "wrong_activation_message",
            "wrong_source_task",
            "wrong_source_message",
            "wrong_source_kind",
            "replacement_wait",
        ] {
            let mut fixture = ExactTaskWaitFixture::new(false);
            let attempt = fixture
                .execution
                .attempts
                .get_mut("activation-exact")
                .unwrap();
            match case {
                "no_wait" => fixture.waits.clear(),
                "cancelled_wait" => fixture.waits[0].status = WaitConditionStatus::Cancelled,
                "unresolved_wait" => fixture.waits[0].status = WaitConditionStatus::Triggered,
                "sibling_wait" => fixture.waits[0].work_item_id = Some("sibling".into()),
                "wrong_wait_kind" => fixture.waits[0].kind = WaitConditionKind::External,
                "wrong_trigger" => fixture.waits[0].trigger_message_id = Some("other".into()),
                "wrong_wake_task" => {
                    fixture.waits[0].wake_sources = vec![WakeSource::TaskResult {
                        task_id: "other".into(),
                    }];
                }
                "wrong_message_task" => fixture.message.task_id = Some("other".into()),
                "wrong_message_origin" => {
                    fixture.message.origin = MessageOrigin::Task {
                        task_id: "other".into(),
                    };
                }
                "wrong_message_kind" => fixture.message.kind = MessageKind::InternalFollowup,
                "wrong_message_owner" => fixture.message.work_item_id = Some("sibling".into()),
                "wrong_task_owner" => fixture.task.work_item_id = Some("sibling".into()),
                "wrong_task_agent" => fixture.task.agent_id = "other".into(),
                "missing_activation" => {
                    fixture.provenance = ExecutionAdmissionProvenance::Canonical {
                        scenario_class: scheduler::EXACT_TASK_REJOIN_SCENARIO,
                        activation_id: "other".into(),
                    };
                }
                "wrong_activation_message" => attempt.source_message_id = Some("other".into()),
                "wrong_source_task" => {
                    attempt.source.identity = ExecutionSourceIdentity::TaskResult {
                        task_id: "other".into(),
                        result_message_id: fixture.message.id.clone(),
                    };
                }
                "wrong_source_message" => {
                    attempt.source.identity = ExecutionSourceIdentity::TaskResult {
                        task_id: fixture.task.id.clone(),
                        result_message_id: "other".into(),
                    };
                }
                "wrong_source_kind" => {
                    attempt.source.identity = ExecutionSourceIdentity::QueueMessage {
                        message_id: fixture.message.id.clone(),
                    };
                }
                "replacement_wait" => {
                    let mut replacement = fixture.waits[0].clone();
                    replacement.id = "wait-replacement".into();
                    replacement.status = WaitConditionStatus::Active;
                    replacement.trigger_message_id = None;
                    replacement.triggered_at = None;
                    replacement.resolved_at = None;
                    fixture.waits.push(replacement);
                }
                _ => unreachable!(),
            }
            assert!(!fixture.has_exact_wait(), "unexpected exemption: {case}");
        }
    }

    #[tokio::test]
    async fn canonical_task_wait_trigger_cannot_be_duplicated() {
        let fixture = ExactTaskWaitFixture::new(false);
        assert!(fixture.has_exact_wait());
        let mut duplicate = fixture.waits[0].clone();
        duplicate.id = "wait-duplicate".into();
        let error = fixture
            .runtime
            .storage()
            .append_wait_condition(&duplicate)
            .unwrap_err();
        assert!(error.to_string().contains("UNIQUE constraint failed"));
    }

    #[tokio::test]
    async fn triggered_task_wait_exemption_rejects_sibling_and_mismatched_source() {
        for case in ["sibling", "wait_id", "trigger_message"] {
            let mut fixture = ExactTaskWaitFixture::new(true);
            match case {
                "sibling" => fixture.waits[0].work_item_id = Some("sibling".into()),
                "wait_id" => fixture.waits[0].id = "other-wait".into(),
                "trigger_message" => {
                    let attempt = fixture
                        .execution
                        .attempts
                        .get_mut("activation-exact")
                        .unwrap();
                    attempt.source.identity = ExecutionSourceIdentity::TriggeredWait {
                        wait_id: fixture.waits[0].id.clone(),
                        trigger_message_id: "other".into(),
                        ingress: None,
                    };
                }
                _ => unreachable!(),
            }
            assert!(!fixture.has_exact_wait(), "unexpected exemption: {case}");
        }
    }

    fn authentic_task_result_fixture_source(task: &TaskRecord) -> Result<MessageEnvelope> {
        let mut message = task_result_message(&task.id);
        message.authority_class = AuthorityClass::RuntimeInstruction;
        message.admission_context = Some(crate::types::AdmissionContext::RuntimeOwned);
        message.delivery_surface = Some(crate::types::MessageDeliverySurface::TaskRejoin);
        message.work_item_id = task.effective_work_item_id().map(ToString::to_string);
        message.metadata = Some(
            json!({"task_id":task.id,"task_kind":task.kind,"task_status":task.status,
            "task_summary":task.summary,"work_item_id":message.work_item_id}),
        );
        if let Some(detail) = task.detail.as_ref() {
            message.metadata.as_mut().unwrap()["task_detail"] = detail.clone();
        }
        if let Some(recovery) = task.recovery.as_ref() {
            message.metadata.as_mut().unwrap()["task_recovery"] = serde_json::to_value(recovery)?;
        }
        Ok(message)
    }

    fn dispatch_task_result_message<'a>(
        runtime: &'a RuntimeHandle,
        task: TaskRecord,
        model_reentry: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut message = authentic_task_result_fixture_source(&task)?;
            // A first Task still originates in the actual Source parser. Use a
            // requested original parent-message address as that real Source ID.
            if runtime.runtime_db().tasks().latest(&task.id)?.is_none() {
                if let Some(id) = task.parent_message_id.as_ref() {
                    message.id = id.clone();
                }
            }
            let admitted = runtime.enqueue(message).await?;
            let scheduler_executor::RunLoopPoll::Message(scheduled) =
                scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                    .poll()
                    .await?
            else {
                bail!("TaskResult fixture must claim its actual queued source");
            };
            assert_eq!(scheduled.message.id, admitted.id);
            assert!(scheduled.message.message_seq.is_some_and(|seq| seq > 0));
            let scheduler_executor::ScheduledMessage {
                message,
                running_state,
                mut dispatch_plan,
                mut scheduler_decision,
                run_completion,
                _reply_owner_budget,
            } = scheduled;
            let original_run = running_state
                .current_run_id
                .as_ref()
                .expect("actual claim owns a run");
            // Keep the explicit kind/status/clocks/reentry inputs while deriving
            // every Source relationship through the real production parser.
            let mut parsed = super::super::tasks::task_from_message(&message, &task.agent_id)?;
            parsed.created_at = task.created_at;
            parsed.updated_at = task.updated_at;
            assert_eq!(parsed.id, task.id);
            assert_eq!(parsed.kind, task.kind);
            assert_eq!(parsed.status, task.status);
            dispatch_plan.task = Ok(Some(parsed));
            scheduler_decision.model_reentry = model_reentry;
            let transition = runtime
                .process_message_with_plan_deferred(
                    message.clone(),
                    dispatch_plan,
                    &scheduler_decision,
                )
                .await?;
            runtime
                .finish_scheduled_message_processing(
                    &message,
                    original_run,
                    std::time::Instant::now(),
                    Ok(transition),
                )
                .await?;
            assert_eq!(
                runtime
                    .runtime_db()
                    .queue_entries()
                    .latest(&message.id)?
                    .expect("actual claim has a queue entry")
                    .status,
                QueueEntryStatus::Processed,
            );
            // Keep original physical processing owned through Queue/Memory ACK.
            drop(run_completion);
            Ok(())
        })
    }

    fn persist_delivered_parent_fixture<'a>(
        runtime: &'a RuntimeHandle,
        mut delivered: BriefRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = BriefRecord> + Send + 'a>> {
        Box::pin(async move {
            use crate::runtime_db::prepared_input_evidence::PreparedInputEvidence;
            let mut source = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: Some("test".into()),
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "original operator request".into(),
                },
            );
            source.id = "operator-message".into();
            let mut mutation = runtime.begin_mutation().await.unwrap();
            let prepared = PreparedInputEvidence::message(
                mutation.input_evidence_scope(),
                crate::ids::audit_event_id(),
                Utc::now(),
                source,
                Vec::new(),
            )
            .unwrap();
            mutation
                .commit_prepared_evidence(prepared, None)
                .await
                .unwrap();
            let source = runtime
                .storage()
                .read_message_by_id("operator-message")
                .unwrap()
                .unwrap();
            assert!(source.message_seq.is_some_and(|seq| seq > 0));
            let mut parent = TurnRecord::new("default", "turn-parent", 1);
            parent.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
            parent.input_message_ids = vec![source.id.clone()];
            let prepared = PreparedInputEvidence::turn(
                mutation.input_evidence_scope(),
                crate::ids::audit_event_id(),
                Utc::now(),
                parent.clone(),
            )
            .unwrap();
            mutation
                .commit_prepared_evidence(prepared, None)
                .await
                .unwrap();
            let event = crate::types::brief_created_event_for(&delivered).unwrap();
            let prepared = PreparedInputEvidence::brief_created(
                mutation.input_evidence_scope(),
                event.id.clone(),
                delivered.created_at,
                delivered.clone(),
                event,
                Vec::new(),
            )
            .unwrap();
            mutation
                .commit_prepared_evidence(prepared, None)
                .await
                .unwrap();
            parent.produced_brief_ids = vec![delivered.id.clone()];
            let prepared = PreparedInputEvidence::turn(
                mutation.input_evidence_scope(),
                crate::ids::audit_event_id(),
                Utc::now(),
                parent,
            )
            .unwrap();
            mutation
                .commit_prepared_evidence(prepared, None)
                .await
                .unwrap();
            drop(mutation);
            delivered = runtime
                .storage()
                .read_brief_by_id(&delivered.id)
                .unwrap()
                .unwrap();
            assert!(delivered.created_event_seq.is_some());
            delivered
        })
    }

    fn original_v81_concurrent_terminal_boundary(
        original: &RuntimeFixture,
    ) -> Result<RuntimeFixture> {
        use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
        let dir = tempdir()?;
        let workspace = tempdir()?;
        let path = dir.path().join(".holon/state/runtime.sqlite");
        std::fs::create_dir_all(path.parent().unwrap())?;
        let mut connection = rusqlite::Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
            apply_migration(&mut connection, migration)?;
        }
        connection.execute(
            "ATTACH DATABASE ?1 AS original_ack",
            [original
                .runtime_db()
                .path()
                .to_str()
                .context("fixture path is not UTF8")?],
        )?;
        // Copy this tiny quiescent canonical fixture into a genuine v81 DB.
        // Preserve its original events/seqs/State, without any D5 qualification.
        connection.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let tables: Vec<String> = {
            let mut q=connection.prepare("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('schema_migrations','schema_migration_baselines')")?;
            let rows = q
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            rows
        };
        for table in tables {
            anyhow::ensure!(
                table
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "fixture table name changed"
            );
            let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM original_ack.sqlite_master WHERE type='table' AND name=?1)",[&table],|r|r.get(0))?;
            if !exists {
                continue;
            }
            let columns = |db: &str| -> Result<Vec<String>> {
                let mut q = connection.prepare(&format!("PRAGMA {db}.table_info({table})"))?;
                let rows = q
                    .query_map([], |r| r.get(1))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(rows)
            };
            let source_columns = columns("original_ack")?;
            let columns: Vec<_> = columns("main")?
                .into_iter()
                .filter(|c| source_columns.contains(c))
                .collect();
            if columns.is_empty() {
                continue;
            }
            anyhow::ensure!(
                columns
                    .iter()
                    .all(|c| c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
                "fixture column name changed"
            );
            let columns = columns
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(",");
            connection.execute(&format!("DELETE FROM {table}"), [])?;
            // v82/v84 fixed-boundary markers did not exist in this actual v81
            // database. Copying only those markers would suppress migration
            // capture while their new continuity/eligibility tables are absent.
            let before_cutover = if table == "runtime_metadata" {
                " WHERE key NOT IN ('episode_continuity/v1/upgrade_boundary','input_evidence/v1/fixed_upgrade')"
            } else {
                ""
            };
            connection.execute(
                &format!(
                    "INSERT INTO {table}({columns}) SELECT {columns} FROM original_ack.{table}{before_cutover}"
                ),
                [],
            )?;
        }
        connection.execute_batch("DETACH DATABASE original_ack;")?;
        // This is the pre-cutover concurrent old writer, not a post-D5 restore.
        connection.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.detail',json('{\"version\":2}')) WHERE task_id='task-1'",[])?;
        assert_eq!(connection.query_row("SELECT json_extract(payload_json,'$.detail.version') FROM tasks WHERE task_id='task-1'",[],|r|r.get::<_,u64>(0))?,2);
        assert_eq!(
            connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, u64>(0))?,
            81
        );
        assert_eq!(connection.query_row(
            "SELECT COUNT(*) FROM runtime_metadata WHERE key IN ('episode_continuity/v1/upgrade_boundary','input_evidence/v1/fixed_upgrade')",
            [],|row|row.get::<_,usize>(0))?,0);
        drop(connection);
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )?;
        let binding:usize=runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM agent_episode_continuity WHERE agent_id='default' AND mode='legacy_adopted'",
            [],|row|row.get(0))?;
        assert_eq!(
            binding, 1,
            "actual v82 migration must bind the original namespace"
        );
        let boundaries:usize=runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM runtime_metadata WHERE key IN ('episode_continuity/v1/upgrade_boundary','input_evidence/v1/fixed_upgrade')",
            [],|row|row.get(0))?;
        assert_eq!(
            boundaries, 2,
            "actual fixed upgrades must capture, not reuse future markers"
        );
        Ok(RuntimeFixture {
            runtime,
            _dir: dir,
            _workspace: workspace,
        })
    }

    #[test]
    fn stale_non_terminal_updates_are_ignored_after_terminal_status_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Completed, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let stale = task("task-1", TaskStatus::Running, true);
        assert!(should_ignore_task_update(latest, &stale));
    }

    #[test]
    fn conflicting_terminal_updates_are_ignored_after_terminal_status_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Completed, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let late_terminal = task("task-1", TaskStatus::Failed, true);
        assert!(should_ignore_task_update(latest, &late_terminal));
    }

    #[test]
    fn repeated_same_terminal_updates_are_preserved_for_result_events() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Failed, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let repeated_terminal = task("task-1", TaskStatus::Failed, true);
        assert!(!should_ignore_task_update(latest, &repeated_terminal));
    }

    #[test]
    fn stale_running_update_is_ignored_after_cancelling() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("task-1", TaskStatus::Cancelling, true))
            .unwrap();

        let latest = storage.latest_task_record("task-1").unwrap();
        let stale = task("task-1", TaskStatus::Running, true);
        assert!(should_ignore_task_update(latest, &stale));
    }

    #[tokio::test]
    async fn concurrent_running_snapshot_cannot_overwrite_cancelling() {
        let runtime = runtime();
        let running = task_with_kind(
            "task-cancel-race",
            TaskStatus::Running,
            false,
            TaskKind::CommandTask,
        );
        runtime
            .persist_task_transition(&running, "task_created")
            .await
            .unwrap();
        let mut cancelling = running.clone();
        cancelling.status = TaskStatus::Cancelling;
        cancelling.updated_at += chrono::Duration::seconds(1);
        cancelling.detail = Some(json!({"cancel_requested": true, "accepts_input": false}));
        let mut late_running = running.clone();
        // The runner can construct its update after stop constructed Cancelling,
        // while both reducers still observe the original Running record.
        late_running.updated_at = cancelling.updated_at + chrono::Duration::seconds(1);

        let (snapshots, mut observed_snapshots) = tokio::sync::mpsc::unbounded_channel();
        let (audit_events, _observed_events) = tokio::sync::mpsc::unbounded_channel();
        let (commits, _observed_commits) = tokio::sync::mpsc::unbounded_channel();
        *runtime.inner.task_transition_snapshot_pause.lock().unwrap() =
            Some(Arc::new(TaskTransitionSnapshotPause {
                task_id: running.id.clone(),
                snapshots,
                audit_events,
                commits,
            }));
        let writer = {
            let runtime = runtime.runtime.clone();
            tokio::spawn(async move {
                runtime
                    .persist_task_transition(&late_running, "task_status_updated")
                    .await
            })
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let (latest, _, release) = tokio::time::timeout_at(deadline, observed_snapshots.recv())
            .await
            .expect("runner must pause after reading Running")
            .unwrap();
        assert_eq!(latest, Some(running.clone()));
        *runtime.inner.task_transition_snapshot_pause.lock().unwrap() = None;
        // The live owner now serializes reducers. Fixture a competing durable
        // writer at the captured boundary rather than deadlock on that owner.
        runtime.runtime_db().tasks().upsert(&cancelling).unwrap();
        assert_eq!(
            runtime.task_record(&running.id).await.unwrap(),
            Some(cancelling.clone())
        );
        let event_count = runtime.recent_events(100).await.unwrap().len();

        release.send(()).unwrap();
        tokio::time::timeout_at(deadline, writer)
            .await
            .expect("runner must finish after release")
            .unwrap()
            .unwrap();

        assert_eq!(
            runtime.task_record(&running.id).await.unwrap(),
            Some(cancelling)
        );
        let output = runtime.task_output(&running.id, false, 0).await.unwrap();
        assert_eq!(output.task.status, TaskStatus::Cancelling);
        assert_eq!(
            output.retrieval_status,
            crate::types::TaskOutputRetrievalStatus::NotReady
        );
        assert_eq!(runtime.recent_events(100).await.unwrap().len(), event_count);
    }

    #[test]
    fn active_tasks_do_not_block_from_legacy_wait_policy_payloads() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&scheduler_blocking_task("blocking", TaskStatus::Running))
            .unwrap();
        storage
            .append_task(&task("background", TaskStatus::Running, false))
            .unwrap();

        let active = storage
            .latest_active_task_records_for_agent("default", usize::MAX)
            .unwrap();
        assert!(active.iter().any(|task| task.id == "blocking"));
        assert!(active.iter().any(|task| task.id == "background"));
        assert!(!active.iter().any(TaskRecord::is_blocking));
    }

    #[test]
    fn active_task_projection_ignores_terminal_latest_records() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        storage
            .append_task(&task("stale", TaskStatus::Running, true))
            .unwrap();
        storage
            .append_task(&task("stale", TaskStatus::Completed, true))
            .unwrap();

        let active = storage
            .latest_active_task_records_for_agent("default", usize::MAX)
            .unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn task_record_work_item_id_falls_back_to_detail_for_old_records() {
        let mut record = task("task-1", TaskStatus::Running, true);
        record.detail = Some(serde_json::json!({
            "wait_policy": "blocking",
            "work_item_id": "work-old",
        }));

        assert_eq!(record.effective_work_item_id(), Some("work-old"));

        record.work_item_id = Some("work-new".into());
        assert_eq!(record.effective_work_item_id(), Some("work-new"));
    }

    #[tokio::test]
    async fn non_terminal_task_updates_are_visible_without_scheduler_wait() {
        let runtime = runtime();

        runtime
            .reduce_task_status_message(scheduler_blocking_task("task-1", TaskStatus::Running))
            .await
            .unwrap();

        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(active_tasks.iter().any(|task| task.id == "task-1"));
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
    }

    #[tokio::test]
    async fn task_transition_preserves_active_run_id_during_turn() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::AwakeRunning;
            guard.state.current_run_id = Some("run-1".into());
            guard.persist_state(&runtime.inner.storage).unwrap();
        }

        runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, true),
                "task_status_updated",
            ))
            .await
            .unwrap();

        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.current_run_id.as_deref(), Some("run-1"));
        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(active_tasks.iter().any(|task| task.id == "task-1"));
    }

    #[tokio::test]
    async fn task_transition_without_agent_state_change_preserves_preceding_owned_agent_write() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::AwakeIdle;
            guard.persist_state(&runtime.inner.storage).unwrap();
        }
        runtime.inject_task_transition_conflicts(1);
        runtime
            .inject_task_transition_conflict_if_armed()
            .await
            .unwrap();

        runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, false),
                "task_status_updated",
            ))
            .await
            .unwrap();

        assert!(runtime.task_record("task-1").await.unwrap().is_some());
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(
            state
                .pending_wake_hint
                .as_ref()
                .map(|hint| hint.reason.as_str()),
            Some("test_conflict")
        );
    }

    #[tokio::test]
    async fn task_transition_recomputes_from_latest_agent_state_after_conflict() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::Booting;
            guard.persist_state(&runtime.inner.storage).unwrap();
        }
        runtime.inject_task_transition_conflicts(1);

        runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, false),
                "task_status_updated",
            ))
            .await
            .unwrap();

        assert!(runtime.task_record("task-1").await.unwrap().is_some());
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(
            state
                .pending_wake_hint
                .as_ref()
                .and_then(|hint| hint.description.as_deref()),
            Some("concurrent task transition test attempt 1")
        );
    }

    #[tokio::test]
    async fn terminal_task_result_retry_releases_agent_lock_before_refresh() {
        let runtime = runtime();
        runtime.inject_terminal_task_transition_conflicts(1);
        let message = task_result_message("task-1");

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            runtime.commit_terminal_task_result(
                &task("task-1", TaskStatus::Completed, true),
                "task_completed",
                &message,
            ),
        )
        .await
        .expect("terminal task transition retry should not deadlock")
        .unwrap();

        assert_eq!(
            runtime
                .agent_state()
                .await
                .unwrap()
                .pending_wake_hint
                .as_ref()
                .map(|hint| hint.reason.as_str()),
            Some("test_terminal_conflict")
        );
        assert_eq!(
            runtime
                .storage()
                .read_message_by_id(&message.id)
                .unwrap()
                .map(|stored| stored.id),
            Some(message.id)
        );
    }

    #[tokio::test]
    async fn terminal_publication_is_fenced_by_owner_incarnation() {
        use crate::runtime::tests::support::LifecycleHarness;
        use crate::types::AgentRegistryStatus::{Active, Deleted, Deleting};

        for replacement in [
            None,
            Some((Active, 2)),
            Some((Deleting, 1)),
            Some((Deleted, 1)),
        ] {
            let harness = LifecycleHarness::new();
            let runtime = harness.runtime();
            let work = runtime
                .create_work_item("owner fence contract".into(), None, None, Vec::new())
                .await
                .unwrap();
            let mut running = task("owner-fence", TaskStatus::Running, false);
            running.work_item_id = Some(work.id.clone());
            running.detail = Some(json!({
                "parent_turn_id": "parent-turn",
                "rejoin_obligation_id": running.id,
                "rejoin_generation": 1,
            }));
            runtime
                .persist_task_transition(&running, "task_created")
                .await
                .unwrap();
            runtime
                .register_wait_for(
                    "default",
                    Some(work.id),
                    crate::runtime::WaitForWakeKind::TaskResult,
                    Some(running.id.clone()),
                    "owner fence result".into(),
                    None,
                )
                .await
                .unwrap();
            let mut terminal = running.clone();
            terminal.status = TaskStatus::Completed;
            let mut message = task_result_message(&running.id);
            message.work_item_id = running.work_item_id.clone();
            terminal.parent_message_id = Some(message.id.clone());
            let memory_before = runtime.agent_state().await.unwrap();

            let (snapshots, mut observed) = tokio::sync::mpsc::unbounded_channel();
            let (audit_events, _events) = tokio::sync::mpsc::unbounded_channel();
            let (commits, _commits) = tokio::sync::mpsc::unbounded_channel();
            *runtime.inner.task_transition_snapshot_pause.lock().unwrap() =
                Some(Arc::new(TaskTransitionSnapshotPause {
                    task_id: running.id.clone(),
                    snapshots,
                    audit_events,
                    commits,
                }));
            let writer = {
                let runtime = runtime.clone();
                let message = message.clone();
                tokio::spawn(async move {
                    runtime
                        .commit_terminal_task_result(&terminal, "task_completed", &message)
                        .await
                })
            };
            let (_, _, release) =
                tokio::time::timeout(std::time::Duration::from_secs(10), observed.recv())
                    .await
                    .expect("publisher must reach the prepared snapshot")
                    .unwrap();
            *runtime.inner.task_transition_snapshot_pause.lock().unwrap() = None;
            let db = &runtime.inner.runtime_db;
            let mut owner = db.agent_identities().latest("default").unwrap().unwrap();
            // Fixture the deletion/replacement boundary after the publisher's
            // snapshot. No timing sleeps or live lifecycle operations are needed.
            db.transaction(|tx| {
                tx.execute("DELETE FROM tasks WHERE task_id = ?1", [&running.id])?;
                if replacement.is_none() {
                    tx.execute(
                        "DELETE FROM agent_identities WHERE agent_id = 'default'",
                        [],
                    )?;
                }
                Ok(())
            })
            .unwrap();
            if let Some((status, incarnation)) = replacement {
                owner.status = status;
                owner.incarnation = incarnation;
                owner.revision += 1;
                owner.updated_at = Utc::now();
                db.agent_identities().upsert(&owner).unwrap();
            }
            let before = harness.snapshot();
            release.send(()).unwrap();
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), writer)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("task owner incarnation fence rejected"),
                "{error:#}"
            );
            harness.assert_unchanged(&before);
            assert_eq!(runtime.agent_state().await.unwrap(), memory_before);
            assert!(db
                .task_result_settlements()
                .latest_for_message(&message.id)
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn enqueue_baseline_refresh_requires_current_owner_incarnation() {
        use crate::types::AgentRegistryStatus::{Active, Deleted, Deleting};

        for (status, incarnation, allowed) in [
            (Active, 1, true),
            (Active, 2, false),
            (Deleting, 1, false),
            (Deleted, 1, false),
        ] {
            let runtime = runtime();
            let before = runtime.agent_state().await.unwrap();
            let db = &runtime.inner.runtime_db;
            let mut owner = db.agent_identities().latest("default").unwrap().unwrap();
            owner.status = status;
            owner.incarnation = incarnation;
            owner.revision += 1;
            owner.updated_at = Utc::now();
            db.agent_identities().upsert(&owner).unwrap();
            let mut updated = before.clone();
            updated.pending_wake_hint = Some(crate::types::PendingWakeHint {
                reason: "owner_fence_contract".into(),
                description: None,
                source: None,
                scope: None,
                external_trigger_id: None,
                resource: None,
                body: None,
                content_type: None,
                correlation_id: None,
                causation_id: None,
                created_at: Utc::now(),
            });
            db.agent_states().upsert_for_fixture(&updated).unwrap();
            assert_eq!(
                runtime
                    .refresh_enqueue_agent_state_baseline("default")
                    .await
                    .unwrap(),
                allowed
            );
            assert_eq!(
                runtime.agent_state().await.unwrap(),
                if allowed { updated } else { before }
            );
        }
    }

    #[tokio::test]
    async fn same_owner_terminal_publication_and_cancellation_remain_idempotent() {
        for status in [TaskStatus::Completed, TaskStatus::Cancelled] {
            let runtime = runtime();
            let mut terminal = task("owner-fence-positive", status.clone(), false);
            let message = task_result_message(&terminal.id);
            terminal.parent_message_id = Some(message.id.clone());
            runtime
                .commit_terminal_task_result(&terminal, "task_completed", &message)
                .await
                .unwrap();
            let event_count = runtime.recent_events(100).await.unwrap().len();
            runtime
                .commit_terminal_task_result(&terminal, "task_completed", &message)
                .await
                .unwrap();
            assert_eq!(
                runtime
                    .task_record(&terminal.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                status
            );
            assert_eq!(runtime.recent_events(100).await.unwrap().len(), event_count);
            assert_eq!(
                runtime
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest_all()
                    .unwrap()
                    .len(),
                1
            );
        }
    }

    #[tokio::test]
    async fn terminal_publication_is_linearized_with_target_deletion() {
        use crate::types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        };
        for (deletion_wins, proposed_status) in [
            (true, TaskStatus::Completed),
            (true, TaskStatus::Failed),
            (true, TaskStatus::Interrupted),
            (false, TaskStatus::Completed),
        ] {
            let runtime = runtime();
            let identity = AgentIdentityRecord::new(
                "target",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            runtime
                .inner
                .runtime_db
                .agent_identities()
                .upsert(&identity)
                .unwrap();
            let mut running = task("deletion-race", TaskStatus::Running, false);
            running.detail = Some(json!({
                "target":{"kind":"child_agent","agent_id":"target","incarnation":1},
                "source_message_id":"original-source",
                "parent_turn_id":"parent-turn", "rejoin_obligation_id":"deletion-race", "rejoin_generation":1,
            }));
            runtime
                .persist_task_transition(&running, "task_created")
                .await
                .unwrap();
            let mut terminal = running.clone();
            terminal.status = proposed_status;
            terminal.updated_at = Utc::now();
            terminal.detail.as_mut().unwrap()["output_summary"] = json!("copied result");
            let message = task_result_message(&running.id);
            terminal.parent_message_id = Some(message.id.clone());
            if !deletion_wins {
                runtime
                    .commit_terminal_task_result(&terminal, "task_status_updated", &message)
                    .await
                    .unwrap();
            }
            runtime
                .inner
                .runtime_db
                .agent_deletions()
                .begin("target", identity.revision, "test", false)
                .unwrap();
            let publication = runtime
                .commit_terminal_task_result(&terminal, "task_status_updated", &message)
                .await;
            publication.unwrap();
            let stored = runtime
                .inner
                .runtime_db
                .tasks()
                .latest(&running.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                stored.status,
                if deletion_wins {
                    TaskStatus::Cancelled
                } else {
                    TaskStatus::Completed
                }
            );
            // A stale cancellation/stop publisher must converge on a success
            // which committed before the deletion fence, without another result.
            if !deletion_wins {
                let mut losing = running.clone();
                losing.status = TaskStatus::Cancelled;
                losing.detail.as_mut().unwrap()["cancellation_reason"] = json!("agent_deleted");
                let loser_message = task_result_message(&running.id);
                losing.parent_message_id = Some(loser_message.id.clone());
                runtime
                    .commit_terminal_task_result(&losing, "task_status_updated", &loser_message)
                    .await
                    .unwrap();
                assert!(runtime
                    .inner
                    .storage
                    .read_message_by_id(&loser_message.id)
                    .unwrap()
                    .is_none());
            }
            // Replay the winning durable result, not a losing cached success.
            let winner = runtime
                .inner
                .storage
                .read_message_by_id(stored.parent_message_id.as_deref().unwrap())
                .unwrap()
                .unwrap();
            runtime
                .commit_terminal_task_result(&stored, "task_status_updated", &winner)
                .await
                .unwrap();
            assert_eq!(
                runtime
                    .inner
                    .runtime_db
                    .tasks()
                    .latest(&running.id)
                    .unwrap(),
                Some(stored)
            );
        }
    }

    #[tokio::test]
    async fn supervised_terminal_publication_is_atomic_and_preserves_settlement() {
        use crate::runtime_db::task_result_settlement::TaskResultSettlementState;
        use crate::runtime_db::transitions::TransitionFaultPoint;

        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let runtime = runtime();
            let mut running = task("task-source-bound", TaskStatus::Running, false);
            running.detail = Some(json!({
                "source_message_id": "original-source",
                "target_incarnation": 1,
                "parent_turn_id": "parent-turn",
                "rejoin_obligation_id": "task-source-bound",
                "rejoin_generation": 1,
            }));
            runtime
                .persist_task_transition(&running, "task_created")
                .await
                .unwrap();
            let mut terminal = running.clone();
            terminal.status = TaskStatus::Completed;
            terminal.detail.as_mut().unwrap()["activation_id"] = json!("original-attempt");
            terminal.detail.as_mut().unwrap()["turn_id"] = json!("original-turn");
            terminal.detail.as_mut().unwrap()["output_summary"] = json!("original-result");
            let mut message = task_result_message(&running.id);
            message.id = format!("message:child-invocation-result:{}", running.id);
            terminal.parent_message_id = Some(message.id.clone());
            runtime.inject_next_transition_fault(fault);
            assert!(
                runtime
                    .commit_terminal_task_result(&terminal, "task_status_updated", &message,)
                    .await
                    .is_err(),
                "{fault:?}"
            );
            assert_eq!(
                runtime
                    .inner
                    .storage
                    .latest_task_record(&running.id)
                    .unwrap()
                    .unwrap()
                    .status,
                TaskStatus::Running
            );
            assert!(runtime
                .inner
                .storage
                .read_message_by_id(&message.id)
                .unwrap()
                .is_none());
            assert!(runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&message.id)
                .unwrap()
                .is_none());
            assert!(runtime
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(&message.id)
                .unwrap()
                .is_none());
            // Two publishers converge on the same atomic task/result/obligation.
            let (first, second) = tokio::join!(
                runtime.commit_terminal_task_result(&terminal, "task_status_updated", &message),
                runtime.commit_terminal_task_result(&terminal, "task_status_updated", &message),
            );
            first.unwrap();
            second.unwrap();
            let repository = runtime.inner.runtime_db.task_result_settlements();
            let mut settlement = repository.latest_for_message(&message.id).unwrap().unwrap();
            let identity = settlement.result_identity.clone();
            for state in [
                TaskResultSettlementState::PersistedPending,
                TaskResultSettlementState::CallerAdmitted,
                TaskResultSettlementState::Settled,
            ] {
                match state {
                    TaskResultSettlementState::PersistedPending => {}
                    TaskResultSettlementState::CallerAdmitted => {
                        repository
                            .admit_message("default", &message.id, "parent-attempt", Utc::now())
                            .unwrap()
                            .expect("original result must admit");
                    }
                    TaskResultSettlementState::Settled => {
                        assert_eq!(repository.settle_activation("default", "parent-attempt",
                            crate::runtime_db::task_result_settlement::TaskResultSettlementDisposition::ModelDelivered,
                            Utc::now()).unwrap(), 1);
                    }
                }
                settlement = repository.latest_for_message(&message.id).unwrap().unwrap();
                assert_eq!(settlement.state, state);
                let queue = runtime
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&message.id)
                    .unwrap();
                runtime
                    .commit_terminal_task_result(&terminal, "task_status_updated", &message)
                    .await
                    .unwrap();
                assert_eq!(
                    repository.latest_for_message(&message.id).unwrap(),
                    Some(settlement.clone())
                );
                assert_eq!(settlement.result_identity, identity);
                assert_eq!(
                    runtime
                        .inner
                        .runtime_db
                        .queue_entries()
                        .latest(&message.id)
                        .unwrap(),
                    queue
                );
            }
            for key in [
                "source_message_id",
                "target_incarnation",
                "activation_id",
                "turn_id",
                "completion_ref",
                "output_summary",
            ] {
                let mut conflicting = terminal.clone();
                conflicting.detail.as_mut().unwrap()[key] = json!("different-evidence");
                assert!(
                    runtime
                        .commit_terminal_task_result(&conflicting, "task_status_updated", &message,)
                        .await
                        .is_err(),
                    "{key}"
                );
            }
            let mut conflicting = terminal.clone();
            conflicting.status = TaskStatus::Failed;
            assert!(runtime
                .commit_terminal_task_result(&conflicting, "task_status_updated", &message,)
                .await
                .is_err());
            assert_eq!(
                repository.latest_for_message(&message.id).unwrap(),
                Some(settlement)
            );
        }
    }

    #[tokio::test]
    async fn exhausted_task_transition_conflicts_only_requeue_task_results() {
        let runtime = runtime();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::Booting;
            guard.persist_state(&runtime.inner.storage).unwrap();
        }
        runtime.inject_task_transition_conflicts(TASK_TRANSITION_MAX_ATTEMPTS);

        let error = runtime
            .apply_task_transition(TaskTransition::new(
                &task("task-1", TaskStatus::Running, false),
                "task_status_updated",
            ))
            .await
            .expect_err("retry budget should be exhausted");
        assert!(error.chain().any(|source| source
            .downcast_ref::<TaskTransitionRetryExhausted>()
            .is_some()));

        assert_eq!(
            runtime_error_queue_settlement(&MessageKind::TaskResult, &error),
            (
                QueueEntryStatus::Interrupted,
                "task_transition_retry_exhausted"
            )
        );
        assert_eq!(
            runtime_error_queue_settlement(&MessageKind::OperatorPrompt, &error),
            (QueueEntryStatus::Aborted, "runtime_error")
        );
        assert!(runtime.task_record("task-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn terminal_result_keeps_scheduler_idle_with_other_running_tasks() {
        let runtime = runtime();
        runtime
            .reduce_task_status_message(scheduler_blocking_task("task-1", TaskStatus::Running))
            .await
            .unwrap();
        runtime
            .reduce_task_status_message(scheduler_blocking_task("task-2", TaskStatus::Running))
            .await
            .unwrap();

        dispatch_task_result_message(
            &runtime,
            scheduler_blocking_task("task-1", TaskStatus::Completed),
            false,
        )
        .await
        .unwrap();
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        let active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(!active_tasks.iter().any(|task| task.id == "task-1"));
        assert!(active_tasks.iter().any(|task| task.id == "task-2"));

        dispatch_task_result_message(
            &runtime,
            scheduler_blocking_task("task-2", TaskStatus::Completed),
            false,
        )
        .await
        .unwrap();
        let final_state = runtime.agent_state().await.unwrap();
        assert_eq!(final_state.status, AgentStatus::AwakeIdle);
        let final_active_tasks = runtime.active_tasks(10).await.unwrap();
        assert!(!final_active_tasks.iter().any(|task| task.id == "task-2"));
    }

    #[tokio::test]
    async fn non_model_reentry_task_results_emit_a_result_brief_without_reopening_turn() {
        let runtime = runtime();
        runtime
            .reduce_task_status_message(task("task-1", TaskStatus::Running, false))
            .await
            .unwrap();

        dispatch_task_result_message(
            &runtime,
            task("task-1", TaskStatus::Completed, false),
            false,
        )
        .await
        .unwrap();

        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| {
            brief.kind == crate::types::BriefKind::Result
                && brief.related_task_id.as_deref() == Some("task-1")
                && brief.related_message_id.is_some()
                && brief.text.contains("Task task-1 completed")
        }));
        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        assert!(transcript.is_empty());
    }

    #[tokio::test]
    async fn delivered_parent_turn_suppresses_stale_task_result_reentry_and_brief() {
        let runtime = runtime();
        let mut delivered = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Original operator answer.",
            Some("operator-message".into()),
            None,
        );
        delivered.turn_id = Some("turn-parent".into());
        let delivered = persist_delivered_parent_fixture(&runtime, delivered).await;

        let mut stale = task("task-1", TaskStatus::Completed, false);
        stale.detail = Some(json!({"parent_turn_id": "turn-parent"}));
        dispatch_task_result_message(&runtime, stale, true)
            .await
            .unwrap();

        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert_eq!(briefs, vec![delivered]);
        assert!(runtime
            .recent_events(20)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == "stale_task_result_rejoin_suppressed"));
        let terminal = runtime
            .agent_state()
            .await
            .unwrap()
            .last_turn_terminal
            .expect("suppressed TaskResult should still complete its reducer-only turn");
        assert_eq!(terminal.kind, TurnTerminalKind::Completed);
        assert_eq!(
            terminal.reason.as_deref(),
            Some("reducer_only/parent_turn_already_delivered")
        );
        assert!(terminal.last_assistant_message.is_none());
    }

    #[tokio::test]
    async fn accepted_incoming_parent_suppresses_reentry_with_different_existing_task_parent() {
        let runtime = runtime();
        let mut delivered = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Original operator answer.",
            Some("operator-message".into()),
            None,
        );
        delivered.turn_id = Some("turn-parent".into());
        let delivered = persist_delivered_parent_fixture(&runtime, delivered).await;
        let mut existing = task("incoming-parent-task", TaskStatus::Running, false);
        existing.detail = Some(json!({"parent_turn_id":"older-task-parent"}));
        runtime.reduce_task_status_message(existing).await.unwrap();
        let mut incoming = task("incoming-parent-task", TaskStatus::Completed, false);
        incoming.detail = Some(json!({"parent_turn_id":"turn-parent"}));
        dispatch_task_result_message(&runtime, incoming, true)
            .await
            .unwrap();
        assert_eq!(
            runtime.storage().read_recent_briefs(10).unwrap(),
            vec![delivered]
        );
        let terminal = runtime
            .agent_state()
            .await
            .unwrap()
            .last_turn_terminal
            .unwrap();
        assert_eq!(
            terminal.reason.as_deref(),
            Some("reducer_only/parent_turn_already_delivered")
        );
    }

    #[tokio::test]
    async fn repeated_terminal_task_result_skips_transition_but_processes_message() {
        let runtime = runtime();
        let mut recorded = task("task-1", TaskStatus::Completed, false);
        recorded.parent_message_id = Some("original-parent".into());
        recorded.detail = Some(json!({"source": "completion"}));
        runtime
            .reduce_task_status_message(recorded.clone())
            .await
            .unwrap();

        let mut repeated = task("task-1", TaskStatus::Completed, false);
        repeated.parent_message_id = Some("task-result-message".into());
        repeated.detail = Some(json!({
            "source": "message",
            "parent_turn_id": "turn-1",
        }));
        let mut redispatched = repeated.clone();
        redispatched.created_at += chrono::Duration::seconds(1);
        redispatched.updated_at += chrono::Duration::seconds(1);
        dispatch_task_result_message(&runtime, repeated, false)
            .await
            .unwrap();
        dispatch_task_result_message(&runtime, redispatched, false)
            .await
            .unwrap();

        let latest = runtime.task_record("task-1").await.unwrap().unwrap();
        assert_eq!(latest.parent_message_id.as_deref(), Some("original-parent"));
        assert_eq!(latest.detail, recorded.detail);
        let events = runtime.recent_events(100).await.unwrap();
        let result_events = events
            .iter()
            .filter(|event| event.kind == "task_result_received")
            .collect::<Vec<_>>();
        assert_eq!(result_events.len(), 1);
        assert_eq!(result_events[0].created_at, recorded.updated_at);
        let payload =
            serde_json::from_value::<TaskLifecycleAuditEvent>(result_events[0].data.clone())
                .unwrap();
        assert_eq!(payload, TaskLifecycleAuditEvent::from_task(&recorded));
        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| {
            brief.kind == crate::types::BriefKind::Result
                && brief.text.contains("Task task-1 completed")
        }));
    }

    #[tokio::test]
    async fn terminal_task_result_freezes_task_after_mutation_reservation() -> Result<()> {
        let runtime = runtime();
        let mut running = task("task-reservation-race", TaskStatus::Running, false);
        running.detail = Some(json!({
            "rejoin_obligation_id": running.id,
            "rejoin_generation": 1,
            "parent_turn_id": "original-parent-turn",
        }));
        runtime
            .persist_task_transition(&running, "task_status_updated")
            .await?;
        let mut incoming = running.clone();
        incoming.status = TaskStatus::Completed;
        let message = authentic_task_result_fixture_source(&incoming)?;
        incoming.parent_message_id = Some(message.id.clone());
        let mut winner = incoming.clone();
        winner.updated_at += chrono::Duration::seconds(1);

        let mut mutation = runtime.begin_mutation().await?;
        let competing =
            runtime.commit_terminal_task_result(&incoming, "task_status_updated", &message);
        tokio::pin!(competing);
        // Freeze the losing attempt's interleaving without sleeps or a raw writer.
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(competing.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let (commit, _) = mutation
            .commit_task_transition(
                crate::ids::audit_event_id(),
                crate::runtime_db::transitions::TaskTransitionCommand {
                    agent_id: running.agent_id.clone(),
                    expected_owner_incarnation: runtime.inner.identity_incarnation,
                    task: winner.clone(),
                    agent_deletion: None,
                    task_result_settlement: None,
                    queue_entry: None,
                    work_items: vec![],
                    expected_wait_conditions: vec![],
                    wait_conditions: vec![],
                    agent_state: None,
                    message_evidence: vec![],
                    audit_events: vec![],
                    index_changes: vec![],
                    notify_scheduler: false,
                    commit_on_idempotent: false,
                    fault: None,
                },
                Default::default(),
                true,
            )
            .await?;
        drop(mutation);
        runtime.apply_transition_commit(commit).await;
        competing.await?;
        assert_eq!(
            runtime.runtime_db().tasks().latest(&winner.id)?,
            Some(winner.clone())
        );
        let settlement = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&message.id)?
            .expect("original Result ACK");
        assert_eq!(settlement.task_id, winner.id);
        assert_eq!(settlement.rejoin_generation, 1);
        assert_eq!(settlement.parent_turn_id, "original-parent-turn");
        runtime
            .commit_terminal_task_result(&incoming, "task_status_updated", &message)
            .await?;
        assert_eq!(
            runtime.runtime_db().tasks().latest(&winner.id)?,
            Some(winner)
        );
        assert_eq!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&message.id)?,
            Some(settlement)
        );
        assert_eq!(
            runtime
                .storage()
                .read_recent_events(100)?
                .iter()
                .filter(|event| event.kind == "task_status_updated")
                .count(),
            2
        );
        assert!(runtime
            .runtime_db()
            .queue_entries()
            .latest(&message.id)?
            .is_some());
        Ok(())
    }

    #[tokio::test]
    async fn repeated_terminal_after_concurrent_update_does_not_emit_conflicting_event() {
        let runtime = runtime();
        let mut original = task("task-1", TaskStatus::Completed, false);
        original.parent_message_id = Some("parent-1".into());
        original.detail = Some(json!({"version": 1}));
        dispatch_task_result_message(&runtime, original.clone(), false)
            .await
            .unwrap();

        // The original legacy writer's changed body is fixed before D5
        // qualification. Post-cutover raw corruption remains a strict error.
        let upgraded = original_v81_concurrent_terminal_boundary(&runtime).unwrap();
        let runtime = &upgraded;

        // Re-dispatch the same task result.  Before the fix this emitted an
        // audit event with the same stable ID but content derived from the
        // concurrently-updated DB record, causing a conflict error.
        dispatch_task_result_message(&runtime, original.clone(), false)
            .await
            .unwrap();

        let events = runtime.recent_events(100).await.unwrap();
        let result_events: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "task_result_received")
            .collect();
        // Only the original emission should exist.
        assert_eq!(result_events.len(), 1);
        let payload =
            serde_json::from_value::<TaskLifecycleAuditEvent>(result_events[0].data.clone())
                .unwrap();
        assert_eq!(payload, TaskLifecycleAuditEvent::from_task(&original));
    }

    #[tokio::test]
    async fn model_reentry_task_result_requires_canonical_provenance() {
        let runtime = runtime();
        let mut task = task("task-1", TaskStatus::Completed, false);
        task.work_item_id = Some("work-1".into());
        let message = authentic_task_result_fixture_source(&task).unwrap();
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let prepared = crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::message(
            mutation.input_evidence_scope(),
            crate::ids::audit_event_id(),
            Utc::now(),
            message.clone(),
            Vec::new(),
        )
        .unwrap();
        mutation
            .commit_prepared_evidence(prepared, None)
            .await
            .unwrap();
        mutation
            .prepare_owner_task_request(&crate::runtime::resident_task::TaskCaptureRequest {
                required_keys: std::collections::BTreeSet::from([
                    crate::runtime::resident_task::TaskKey::new(
                        crate::runtime::resident_task::TaskFamily::Task,
                        &task.id,
                    ),
                ]),
                ..crate::runtime::resident_task::TaskCaptureRequest::current()
            })
            .await
            .unwrap();
        mutation
            .prepare_owner_work_request(
                &crate::runtime::resident_work::WorkCaptureRequest::current(),
            )
            .await
            .unwrap();
        drop(mutation);
        let message = runtime
            .storage()
            .read_message_by_id(&message.id)
            .unwrap()
            .unwrap();
        assert!(message.message_seq.is_some_and(|seq| seq > 0));
        let mut parsed = super::super::tasks::task_from_message(&message, &task.agent_id).unwrap();
        parsed.created_at = task.created_at;
        parsed.updated_at = task.updated_at;
        let task = parsed;

        let error = runtime
            .reduce_task_result_message(&message, task, true, None)
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("model turn requires canonical execution admission provenance"));
    }

    #[tokio::test]
    async fn command_task_results_do_not_emit_result_briefs() {
        let runtime = runtime();
        let mut command_task = task("task-1", TaskStatus::Running, false);
        command_task.kind = TaskKind::CommandTask;
        runtime
            .reduce_task_status_message(command_task.clone())
            .await
            .unwrap();

        command_task.status = TaskStatus::Completed;
        command_task.updated_at = Utc::now();
        dispatch_task_result_message(&runtime, command_task, false)
            .await
            .unwrap();

        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.is_empty());
    }

    #[tokio::test]
    async fn rejected_terminal_child_deletion_still_settles_parent_task() {
        let runtime = runtime();
        let running = task("task-1", TaskStatus::Running, false);
        runtime
            .reduce_task_status_message(running.clone())
            .await
            .unwrap();

        let mut terminal = running;
        terminal.status = TaskStatus::Failed;
        terminal.updated_at = Utc::now();
        let message = task_result_message(&terminal.id);
        let deletion = crate::runtime_db::repositories::AgentDeletionRequest {
            agent_id: "default".into(),
            admission:
                crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild {
                    parent_agent_id: "default".into(),
                    task_id: terminal.id.clone(),
                },
            requested_by: "parent_task_terminal".into(),
            cascade_private_children: false,
        };

        assert!(!runtime
            .commit_terminal_child_task_result(
                &terminal,
                "task_status_updated",
                &message,
                Some(deletion),
            )
            .await
            .unwrap());
        assert_eq!(
            runtime
                .task_record(&terminal.id)
                .await
                .unwrap()
                .expect("terminal task should remain persisted")
                .status,
            TaskStatus::Failed
        );
    }
}
