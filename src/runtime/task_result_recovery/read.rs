//! Named recovery acquisition. Decisions retain selected facts, never an owner.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_input_evidence::{
        CapturedInputEvidence, InputEvidenceFamily as Family, InputEvidenceKey as Key,
        InputEvidenceScope, PreparedInputEvidence,
    },
    resident_task::{
        CanonicalTaskFact, CapturedTasks, TaskCaptureRequest, TaskFamily, TaskKey, TaskReadError,
        TaskResultMetadata,
    },
    resident_work::{
        CanonicalWorkRecord, CapturedWork, WorkCaptureRequest, WorkFamily, WorkKey, WorkReadError,
    },
    state_mutation::RuntimeMutation,
};
use crate::runtime_db::{
    context_inputs::ContextDurableRead,
    input_evidence_snapshot::InputEvidenceLoadRequest,
    task_result_settlement::{TaskResultSettlementRecord, TaskResultSettlementState},
};
use crate::types::AgentRegistryStatus;
use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::BTreeSet;

const ACTIVE_QUEUE_PREDICATE: &str =
    "m.agent_id=?1 AND q.status NOT IN ('processed','aborted','dropped','quarantined')";

/// Fixed-size borrowed selection preserves the original SQL LIMIT without an
/// all-unsettled temporary allocation. The metadata namespace remains complete.
pub(in crate::runtime) fn due_results(
    tasks: &CapturedTasks,
    now: chrono::DateTime<Utc>,
) -> Result<[Option<&TaskResultMetadata>; RESULT_RECHECK_LIMIT]> {
    anyhow::ensure!(
        tasks.coverage.unsettled_complete,
        TaskReadError::IncompleteResults
    );
    let mut selected = [None; RESULT_RECHECK_LIMIT];
    for id in &tasks.coverage.unsettled_result_ids {
        let row = tasks.coverage.result_metadata.get(id).ok_or_else(|| {
            TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::ResultSettlement, id))
        })?;
        anyhow::ensure!(
            row.result_identity == *id,
            "Task result metadata identity differs"
        );
        if row
            .next_recheck_at
            .is_none_or(|at| at.timestamp_millis() > now.timestamp_millis())
            || !matches!(
                row.state,
                TaskResultSettlementState::PersistedPending
                    | TaskResultSettlementState::CallerAdmitted
            )
            || (row.state == TaskResultSettlementState::CallerAdmitted && row.activation_open)
        {
            continue;
        }
        let before = |old: &TaskResultMetadata| {
            row.next_recheck_at
                .expect("selected due deadline")
                .timestamp_millis()
                .cmp(
                    &old.next_recheck_at
                        .expect("selected due deadline")
                        .timestamp_millis(),
                )
                .then_with(|| {
                    row.created_at
                        .timestamp_millis()
                        .cmp(&old.created_at.timestamp_millis())
                })
                .then_with(|| row.result_identity.cmp(&old.result_identity))
                .is_lt()
        };
        if let Some(index) = selected.iter().position(|old| old.is_none_or(before)) {
            selected[index..].rotate_right(1);
            selected[index] = Some(row);
        }
    }
    Ok(selected)
}

struct QueueObservation {
    /// Original Message SQL order reversed, restricted to still-active Queue IDs.
    ids: Vec<String>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}

/// The adapter acquires only active queue addresses; Message bodies are qualified
/// Input facts. Scope/State material is reserved before its existing decoder.
fn queue_observation_tx(
    tx: &Transaction<'_>,
    runtime: &RuntimeHandle,
    scope: &InputEvidenceScope,
    state: &AgentState,
    record_limit: usize,
    byte_limit: usize,
    work_revision: Option<u64>,
    task_revision: Option<u64>,
) -> Result<QueueObservation> {
    let (count, id_bytes): (usize, u64) = tx.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(m.message_id AS BLOB))),0) FROM messages m JOIN queue_entries q ON q.message_id=m.message_id WHERE {ACTIVE_QUEUE_PREDICATE}"),
        [&scope.agent_id], |r| Ok((r.get(0)?,r.get(1)?)))?;
    let scope_bytes: u64 = tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",
        [&scope.agent_id], |r| r.get(0))?;
    let bytes = id_bytes
        .checked_add(
            u64::try_from(count)?
                .checked_mul(128)
                .context("recovery Queue nodes overflow")?,
        )
        .and_then(|n| n.checked_add(scope_bytes))
        .and_then(|n| n.checked_mul(8))
        .context("recovery Queue material overflow")?;
    anyhow::ensure!(
        count
            .checked_mul(8)
            .context("recovery Queue count overflow")?
            <= record_limit
            && bytes <= u64::try_from(byte_limit)?,
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "recovery Queue adapter"
        )
    );
    let guard = runtime.inner.input_evidence_budget.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?;
    let scope_read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
    let facts = scope_read.facts();
    anyhow::ensure!(
        facts.state.as_deref() == Some(state) && facts.control_revision.is_some(),
        "recovery adapter State changed"
    );
    let identity = facts
        .identity
        .as_ref()
        .context("recovery adapter identity missing")?;
    anyhow::ensure!(
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && identity.status == AgentRegistryStatus::Active,
        "recovery adapter identity differs"
    );
    if let Some(revision) = work_revision {
        crate::runtime_db::work_snapshot::validate_work_domain_observation_with_scope_tx(
            tx,
            scope,
            revision,
            &scope_read,
        )?;
    }
    if let Some(revision) = task_revision {
        crate::runtime_db::task_snapshot::validate_task_domain_observation_with_scope_tx(
            tx,
            scope,
            revision,
            &scope_read,
        )?;
    }
    let corrupt:bool=tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM messages m JOIN queue_entries q ON q.message_id=m.message_id WHERE {ACTIVE_QUEUE_PREDICATE} AND CASE WHEN json_valid(q.payload_json) THEN json_extract(q.payload_json,'$.message_id') IS NOT q.message_id OR json_extract(q.payload_json,'$.agent_id') IS NOT q.agent_id OR json_extract(q.payload_json,'$.status') IS NOT q.status ELSE 1 END)"),
        [&scope.agent_id],|r|r.get(0))?;
    anyhow::ensure!(
        !corrupt,
        "recovery Queue normalized observation differs from payload"
    );
    let foreign: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM messages m JOIN queue_entries q ON q.message_id=m.message_id WHERE {ACTIVE_QUEUE_PREDICATE} AND q.agent_id<>?1)"),
        [&scope.agent_id], |r| r.get(0))?;
    anyhow::ensure!(!foreign, "recovery Queue address belongs to another agent");
    let mut query = tx.prepare(&format!(
        "SELECT m.message_id FROM messages m JOIN queue_entries q ON q.message_id=m.message_id WHERE {ACTIVE_QUEUE_PREDICATE} ORDER BY m.message_seq IS NULL ASC,m.message_seq DESC,m.created_at DESC,m.message_id DESC"))?;
    let ids = query
        .query_map([&scope.agent_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        ids.len() == count
            && ids.iter().all(|id| !id.is_empty())
            && ids.iter().try_fold(0u64, |n, id| n
                .checked_add(id.len() as u64)
                .context("recovery Queue bytes overflow"))?
                == id_bytes,
        "recovery Queue scalar observation differs"
    );
    Ok(QueueObservation {
        ids,
        _guards: vec![guard],
    })
}

fn source_message<'a>(inputs: &'a CapturedInputEvidence, id: &str) -> Result<&'a MessageEnvelope> {
    let key = Key::new(Family::Message, id);
    match inputs.lookup(&key)?.map(|row| &row.record) {
        Some(PreparedInputEvidence::Message(message)) => Ok(message),
        _ => anyhow::bail!("recovery source Message is unavailable: {id}"),
    }
}

fn outstanding<'a>(
    inputs: &'a CapturedInputEvidence,
    queue: &'a QueueObservation,
    work_item_id: Option<&str>,
    owner_key: &str,
) -> Result<Option<&'a str>> {
    for id in &queue.ids {
        let message = source_message(inputs, id)?;
        if matches!(&message.origin, MessageOrigin::System{subsystem} if subsystem=="task_result_recovery")
            && message.authority_class == AuthorityClass::RuntimeInstruction
            && message.work_item_id.as_deref() == work_item_id
            && message
                .source_refs
                .get("task_result_owner_key")
                .map(String::as_str)
                == Some(owner_key)
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

pub(super) struct RecoveryRead {
    pub state: Arc<AgentState>,
    pub tasks: CapturedTasks,
    pub work: CapturedWork,
    pub inputs: CapturedInputEvidence,
    queue: QueueObservation,
    processed_replies: BTreeSet<String>,
    recoverable_replies: BTreeSet<String>,
    _selectors: Vec<Arc<BudgetViewGuard>>,
}

impl RecoveryRead {
    pub(super) fn record<'a>(
        &'a self,
        metadata: &TaskResultMetadata,
    ) -> Result<&'a TaskResultSettlementRecord> {
        let key = TaskKey::new(TaskFamily::ResultSettlement, &metadata.result_identity);
        let row = self
            .tasks
            .lookup(&key)?
            .ok_or(TaskReadError::NeedsPreparation(key))?;
        row.validate(&self.tasks.scope)?;
        let CanonicalTaskFact::ResultSettlement(record) = &row.fact else {
            anyhow::bail!("recovery Result family differs")
        };
        anyhow::ensure!(
            metadata.matches_record(record),
            "recovery Result metadata differs from body"
        );
        Ok(record)
    }
    pub(super) fn source_message(&self, id: &str) -> Result<&MessageEnvelope> {
        source_message(&self.inputs, id)
    }
    pub(super) fn invalid_result(&self, id: &str) -> Result<bool> {
        Ok(matches!(
            &self.tasks.result_dependency(id)?.relation,
            crate::runtime::resident_task::TaskResultDependencyRelation::Invalid { .. }
        ))
    }
    pub(super) fn outstanding(
        &self,
        work_item_id: Option<&str>,
        owner_key: &str,
    ) -> Result<Option<&str>> {
        outstanding(&self.inputs, &self.queue, work_item_id, owner_key)
    }
    pub(super) fn reply_processed(&self, id: &str) -> bool {
        self.processed_replies.contains(id)
    }
    pub(super) fn reply_recoverable(&self, id: &str) -> bool {
        self.recoverable_replies.contains(id)
    }
    pub(super) fn execution_lane_busy(&self) -> Result<bool> {
        for id in &self.inputs.execution.open_attempt_ids {
            let key = Key::new(Family::Attempt, id);
            anyhow::ensure!(
                matches!(self.inputs.lookup(&key)?.map(|r| &r.record),
                Some(PreparedInputEvidence::Attempt(row)) if row.state==crate::domain::execution_protocol::ExecutionAttemptState::Open),
                "recovery Open Attempt frontier is incomplete"
            );
        }
        Ok(!self.inputs.execution.open_attempt_ids.is_empty())
    }
    pub(super) fn owner_has_unresolved_wait(&self, owner: Option<&str>) -> Result<bool> {
        anyhow::ensure!(self.work.live_complete, WorkReadError::IncompleteLive);
        for id in &self.work.unresolved_wait_ids {
            let wait = self
                .work
                .wait_condition(id)?
                .context("recovery unresolved Wait is unavailable")?;
            anyhow::ensure!(
                matches!(
                    wait.status,
                    WaitConditionStatus::Active | WaitConditionStatus::Triggered
                ),
                "recovery Wait frontier differs"
            );
            if wait.work_item_id.as_deref() == owner {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// prepare_owner_input_request preserves old mandatory roots. Account those
// independent request copies too, without retaining the resident/root map.
fn input_selector_guard(
    runtime: &RuntimeHandle,
    mutation: &RuntimeMutation,
    request: &InputEvidenceLoadRequest,
) -> Result<Arc<BudgetViewGuard>> {
    let root = mutation.capture_published_root()?;
    let old = root
        .input_evidence
        .as_ref()
        .map(|inputs| inputs.mandatory_roots());
    let count = request
        .mandatory_roots
        .len()
        .checked_add(old.map_or(0, BTreeSet::len))
        .context("recovery Input selector count overflow")?;
    let bytes = serialized_bytes(&request.mandatory_roots)?
        .checked_add(serialized_bytes(&old)?)
        .and_then(|n| n.checked_add(u64::try_from(count).ok()?.checked_mul(128)?))
        .and_then(|n| n.checked_add(1024))
        .and_then(|n| n.checked_mul(8))
        .context("recovery Input selector bytes overflow")?;
    anyhow::ensure!(
        count
            .checked_mul(8)
            .context("recovery Input selector copies overflow")?
            <= request.record_limit
            && bytes <= u64::try_from(request.byte_limit)?,
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "recovery Input selectors"
        )
    );
    Ok(runtime.inner.input_evidence_budget.register_owned_bytes(
        &mutation.input_evidence_scope(),
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?)
}

impl RuntimeMutation {
    fn capture_recovery_domains(
        &self,
        work_request: &WorkCaptureRequest,
        task_request: &TaskCaptureRequest,
        input_request: &InputEvidenceLoadRequest,
        queue: QueueObservation,
        selectors: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<RecoveryRead> {
        let root = self.capture_published_root()?;
        let scope = self.input_evidence_scope();
        let tasks = root
            .tasks
            .as_ref()
            .ok_or(TaskReadError::IncompleteResults)?;
        let work = root.work.as_ref().ok_or(WorkReadError::IncompleteLive)?;
        let inputs = root
            .input_evidence
            .as_ref()
            .context("recovery Input domain was not prepared")?;
        anyhow::ensure!(
            tasks.scope() == &scope && work.scope() == &scope && inputs.scope() == &scope,
            "recovery domains differ"
        );
        Ok(RecoveryRead {
            state: root.state.clone(),
            tasks: tasks.capture(task_request)?,
            work: work.capture(work_request)?,
            inputs: inputs.capture_for_request(input_request)?,
            queue,
            processed_replies: Default::default(),
            recoverable_replies: Default::default(),
            _selectors: selectors,
        })
    }

    #[inline(never)]
    pub(super) fn prepare_task_recovery_read<'a>(
        &'a mut self,
        runtime: &'a RuntimeHandle,
        now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<Option<RecoveryRead>>> + Send + 'a>,
    > {
        Box::pin(self.prepare_task_recovery_read_inner(runtime, now))
    }

    async fn prepare_task_recovery_read_inner(
        &mut self,
        runtime: &RuntimeHandle,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<RecoveryRead>> {
        if self.state.status == AgentStatus::Stopped {
            return Ok(None);
        }
        let metadata_request = TaskCaptureRequest {
            include_active: false,
            include_unsettled: true,
            terminal_limit: 0,
            recovery_source_ids: Default::default(),
            required_keys: Default::default(),
        };
        self.prepare_owner_task_request(&metadata_request).await?;
        let metadata = self.capture_task_read(&TaskCaptureRequest {
            include_unsettled: false,
            ..metadata_request
        })?;
        let due = due_results(&metadata.tasks, now)?;
        if due[0].is_none() {
            return Ok(None);
        }
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let bytes = serialized_bytes(&due)?
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("recovery selector bytes overflow")?;
        anyhow::ensure!(
            due.iter()
                .flatten()
                .count()
                .checked_mul(8)
                .context("recovery selector count overflow")?
                <= record_limit
                && bytes <= u64::try_from(byte_limit)?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "recovery selectors"
            )
        );
        let selectors = runtime.inner.input_evidence_budget.register_owned_bytes(
            &metadata.tasks.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let queue = runtime.inner.runtime_db.transaction(|tx| {
            queue_observation_tx(
                tx,
                runtime,
                &metadata.tasks.scope,
                &metadata.state,
                record_limit,
                byte_limit,
                None,
                Some(metadata.tasks.revision),
            )
        })?;
        let mut task_request = TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            recovery_source_ids: Default::default(),
            required_keys: Default::default(),
        };
        let mut work_request = super::super::work_reads::live_work_request();
        let (mut roots, _, _) = self.context_input_evidence_policy()?;
        for row in due.into_iter().flatten() {
            task_request.required_keys.insert(TaskKey::new(
                TaskFamily::ResultSettlement,
                &row.result_identity,
            ));
            roots.insert(Key::new(Family::Message, &row.message_id));
        }
        roots.extend(queue.ids.iter().map(|id| Key::new(Family::Message, id)));
        if let Some(binding) = &metadata.state.current_execution_binding {
            roots.insert(Key::new(Family::Message, &binding.source_message_id));
            roots.insert(Key::new(Family::Turn, &binding.turn_id));
            if let Some(id) = &binding.activation_id {
                roots.insert(Key::new(Family::Attempt, id));
            }
        }
        drop(metadata);
        self.prepare_owner_task_request(&task_request).await?;
        // A Result's raw Work hint becomes an owning root only through its
        // qualified current Task relation. Invalid cleanup must remain possible
        // without reading a foreign/unqualified Work declaration.
        let selected = self.capture_task_read(&task_request)?;
        for row in due_results(&selected.tasks, now)?.into_iter().flatten() {
            if matches!(
                &selected
                    .tasks
                    .result_dependency(&row.result_identity)?
                    .relation,
                crate::runtime::resident_task::TaskResultDependencyRelation::QualifiedCurrent { .. }
            ) {
                if let Some(id) = &row.work_item_id {
                    work_request
                        .mandatory_roots
                        .insert(WorkKey::new(WorkFamily::Work, id));
                    roots.insert(Key::new(Family::WorkExecution, id));
                }
            }
        }
        drop(selected);
        self.prepare_owner_work_request(&work_request).await?;
        let work = self.capture_work(&work_request)?;
        let (work_keys, work_bytes) =
            work.records
                .iter()
                .try_fold((0usize, 0u64), |(count, bytes), record| {
                    if let CanonicalWorkRecord::Work(row) = &record.record {
                        Ok((
                            count
                                .checked_add(1)
                                .context("recovery Work selector count overflow")?,
                            bytes
                                .checked_add(serialized_bytes(&row.id)?)
                                .and_then(|n| n.checked_add(128))
                                .context("recovery Work selectors overflow")?,
                        ))
                    } else {
                        Ok::<_, anyhow::Error>((count, bytes))
                    }
                })?;
        let work_bytes = work_bytes
            .checked_mul(8)
            .context("recovery Work selector copies overflow")?;
        anyhow::ensure!(
            work_keys
                .checked_mul(8)
                .context("recovery Work selector count overflow")?
                <= record_limit
                && work_bytes <= u64::try_from(byte_limit)?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "recovery Work selectors"
            )
        );
        let work_selectors = runtime.inner.input_evidence_budget.register_owned_bytes(
            &work.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: work_bytes,
            },
        )?;
        for record in &work.records {
            if let CanonicalWorkRecord::Work(row) = &record.record {
                roots.insert(Key::new(Family::WorkExecution, &row.id));
            }
        }
        drop(work);
        let mut input_request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: roots,
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        // The run loop can proceed directly to its ordinary Idle closure after
        // an ineligible probe. Preserve that actor's existing Brief policy.
        super::super::scheduler::include_scheduler_brief_window(&mut input_request.windows);
        let episode_limit = runtime
            .inner
            .config_snapshot
            .load_full()
            .base_context_config
            .recent_episode_candidates;
        let input_selectors = input_selector_guard(runtime, self, &input_request)?;
        self.prepare_owner_input_request(&input_request, episode_limit)
            .await?;
        let mut capture_request = input_request.clone();
        capture_request.windows.clear();
        let mut read = self.capture_recovery_domains(
            &work_request,
            &task_request,
            &capture_request,
            queue,
            vec![selectors, work_selectors, input_selectors],
        )?;
        runtime.inner.runtime_db.transaction(|tx| {
            let verified=queue_observation_tx(tx,runtime,&read.tasks.scope,&read.state,record_limit,byte_limit,Some(read.work.revision),Some(read.tasks.revision))?;
            anyhow::ensure!(verified.ids==read.queue.ids,"recovery Queue observation changed during named preparation");
            read.queue._guards.extend(verified._guards);
            for row in due_results(&read.tasks,now)?.into_iter().flatten() {
                if let Some(id)=&row.reply_message_id {
                    let status:Option<(bool,bool,bool)>=tx.query_row("SELECT agent_id=?1,status='processed',CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.message_id') IS message_id AND json_extract(payload_json,'$.agent_id') IS agent_id AND json_extract(payload_json,'$.status') IS status ELSE 0 END FROM queue_entries WHERE message_id=?2",params![read.state.id,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                    if let Some((same,processed,canonical))=status {
                        anyhow::ensure!(same && canonical,"recovery reply Queue scope/projection differs");
                        if processed {read.processed_replies.insert(id.clone());}
                    }
                }
            }
            Ok(())
        })?;
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
            &runtime.inner.input_evidence_budget,
            &read.tasks.scope,
        )?;
        let recoverable = runtime.with_idle_read(Some(&read.state), &material, |idle| {
            let mut recoverable = BTreeSet::new();
            idle.validate_task_observation(read.tasks.revision)?;
            let due = due_results(&read.tasks, now)?;
            idle.prepare_result_sources(due.iter().flatten()
                .filter(|row| row.reply_message_id.is_some())
                .map(|row| row.result_identity.as_str()))?;
            for row in due.into_iter().flatten() {
                if row.reply_message_id.is_some() {
                    let record = read.record(&row)?;
                    if record.state != crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
                        && idle.source_matches(record, true)? {
                        crate::diagnostics::resource_io::admit_cloned_value(&row.result_identity)?;
                        recoverable.insert(row.result_identity.clone());
                    }
                }
            }
            Ok(recoverable)
        })?;
        read.recoverable_replies = recoverable;
        Ok(Some(read))
    }
}

pub(in crate::runtime) struct OutstandingRecoveryMessage {
    id: String,
    _inputs: CapturedInputEvidence,
    _queue: QueueObservation,
    _input_selectors: Arc<BudgetViewGuard>,
}
impl OutstandingRecoveryMessage {
    pub(in crate::runtime) fn id(&self) -> &str {
        &self.id
    }
}

pub(in crate::runtime) async fn prepare_outstanding_recovery_message_id(
    runtime: &RuntimeHandle,
    mutation: &mut RuntimeMutation,
    work_item_id: Option<&str>,
    owner_key: &str,
) -> Result<Option<OutstandingRecoveryMessage>> {
    let scope = mutation.input_evidence_scope();
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let queue = runtime.inner.runtime_db.transaction(|tx| {
        queue_observation_tx(
            tx,
            runtime,
            &scope,
            &mutation.state,
            record_limit,
            byte_limit,
            None,
            None,
        )
    })?;
    if queue.ids.is_empty() {
        return Ok(None);
    }
    let mut request = InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: queue
            .ids
            .iter()
            .map(|id| Key::new(Family::Message, id))
            .collect(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit,
        byte_limit,
    };
    super::super::scheduler::include_scheduler_brief_window(&mut request.windows);
    let episode_limit = runtime
        .inner
        .config_snapshot
        .load_full()
        .base_context_config
        .recent_episode_candidates;
    let input_selectors = input_selector_guard(runtime, mutation, &request)?;
    mutation
        .prepare_owner_input_request(&request, episode_limit)
        .await?;
    let mut capture_request = request.clone();
    capture_request.windows.clear();
    let inputs = {
        let root = mutation.capture_published_root()?;
        let inputs = root
            .input_evidence
            .as_ref()
            .context("recovery admission Input domain was not prepared")?;
        anyhow::ensure!(
            inputs.scope() == &scope,
            "recovery admission Input scope differs"
        );
        inputs.capture_for_request(&capture_request)?
    };
    let verified = runtime.inner.runtime_db.transaction(|tx| {
        queue_observation_tx(
            tx,
            runtime,
            &scope,
            &mutation.state,
            record_limit,
            byte_limit,
            None,
            None,
        )
    })?;
    anyhow::ensure!(
        verified.ids == queue.ids,
        "recovery admission Queue observation changed"
    );
    let id = outstanding(&inputs, &queue, work_item_id, owner_key)?.map(str::to_owned);
    let Some(id) = id else {
        return Ok(None);
    };
    let mut queue = queue;
    queue._guards.extend(verified._guards);
    Ok(Some(OutstandingRecoveryMessage {
        id,
        _inputs: inputs,
        _queue: queue,
        _input_selectors: input_selectors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

    async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
        let home = tempdir()?;
        let runtime = RuntimeHandle::new(
            "default",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )?;
        Ok((home, runtime))
    }
    fn recovery(owner: &str, subsystem: &str) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: subsystem.into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Background,
            MessageBody::Text {
                text: "actual active Queue source".into(),
            },
        );
        message
            .source_refs
            .insert("task_result_owner_key".into(), owner.into());
        message
    }

    #[tokio::test]
    async fn outstanding_recovery_uses_only_captured_active_sources_after_tables_removed(
    ) -> Result<()> {
        let (_home, runtime) = fixture().await?;
        let first = recovery("owner-a", "task_result_recovery");
        let first_id = first.id.clone();
        runtime.enqueue(first).await?;
        runtime
            .enqueue(recovery("owner-a", "other_subsystem"))
            .await?;
        let second = recovery("owner-b", "task_result_recovery");
        let second_id = second.id.clone();
        runtime.enqueue(second).await?;
        let (first, second) = {
            let mut mutation = runtime.begin_mutation().await?;
            (
                prepare_outstanding_recovery_message_id(&runtime, &mut mutation, None, "owner-a")
                    .await?
                    .context("actual first source")?,
                prepare_outstanding_recovery_message_id(&runtime, &mut mutation, None, "owner-b")
                    .await?
                    .context("actual second source")?,
            )
        };
        runtime.inner.runtime_db.transaction(|tx| {
            tx.execute_batch("DROP TABLE queue_entries;DROP TABLE messages")?;
            Ok(())
        })?;
        assert_eq!(first.id(), first_id);
        assert_eq!(second.id(), second_id);
        assert_eq!(
            outstanding(&first._inputs, &first._queue, None, "owner-a")?,
            Some(first_id.as_str())
        );
        assert_eq!(
            outstanding(&second._inputs, &second._queue, None, "owner-b")?,
            Some(second_id.as_str())
        );
        let mut first = first;
        first
            ._inputs
            .records
            .retain(|r| r.locator.key != Key::new(Family::Message, &first_id));
        assert!(
            outstanding(&first._inputs, &first._queue, None, "owner-a").is_err(),
            "missing selected body is not an empty active queue"
        );
        Ok(())
    }

    #[tokio::test]
    async fn recovery_queue_id_budget_precedes_malformed_source_decode() -> Result<()> {
        let (_home, runtime) = fixture().await?;
        let source = recovery("owner", "task_result_recovery");
        let id = source.id.clone();
        runtime.enqueue(source).await?;
        let mutation = runtime.begin_mutation().await?;
        let scope = mutation.input_evidence_scope();
        runtime.inner.runtime_db.transaction(|tx| {
            let normal = queue_observation_tx(
                tx,
                &runtime,
                &scope,
                &mutation.state,
                4096,
                256 * 1024,
                None,
                None,
            )?;
            assert_eq!(normal.ids, vec![id.clone()]);
            Ok(())
        })?;
        let before = runtime
            .inner
            .input_evidence_budget
            .report()?
            .owned
            .metadata_estimate_bytes;
        runtime.inner.runtime_db.transaction(|tx| {
            let huge = "q".repeat(512 * 1024);
            // Inject corruption below the JSON expression index, not through it.
            tx.execute_batch("DROP INDEX idx_messages_agent_trusted_operator;")?;
            tx.execute(
                "UPDATE messages SET message_id=?1,payload_json='{' WHERE message_id=?2",
                params![huge, id],
            )?;
            tx.execute(
                "UPDATE queue_entries SET message_id=?1 WHERE message_id=?2",
                params![huge, id],
            )?;
            let error = queue_observation_tx(
                tx,
                &runtime,
                &scope,
                &mutation.state,
                4096,
                256 * 1024,
                None,
                None,
            )
            .err()
            .context("oversize rejected")?;
            assert!(matches!(
                error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
                Some(
                    crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                        "recovery Queue adapter"
                    )
                )
            ));
            Ok(())
        })?;
        assert_eq!(
            runtime
                .inner
                .input_evidence_budget
                .report()?
                .owned
                .metadata_estimate_bytes,
            before
        );
        Ok(())
    }
}
