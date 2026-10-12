//! Explicit storage preparation for immutable, request-scoped context input.
//! This adapter still performs I/O; its token is not a coherent owner revision.
use super::*;
use crate::{
    runtime::provider_recovery::ProviderRecoverySource,
    runtime_db::task_result_settlement::TaskResultSettlementRecord, storage::WorkQueueReadModel,
    types::WorkItemPlanArtifact,
};
use anyhow::Result;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextReadStage {
    pub name: String,
    pub logical_reads: u64,
    pub returned_records: u64,
    pub elapsed_ns: u64,
}

/// Logical adapter operations, not SQL statements, filesystem syscalls, or
/// disjoint end-to-end timings. Composite stages may include nested reads.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextAcquisitionReport {
    pub logical_reads: u64,
    pub returned_records: u64,
    pub stages: Vec<ContextReadStage>,
}

impl ContextAcquisitionReport {
    fn read<T>(
        &mut self,
        name: &str,
        read: impl FnOnce() -> Result<T>,
        count: impl FnOnce(&T) -> usize,
    ) -> Result<T> {
        let started = Instant::now();
        let value = read();
        let rows = value.as_ref().map(count).unwrap_or_default() as u64;
        self.logical_reads += 1;
        self.returned_records += rows;
        let index = self.stages.iter().position(|stage| stage.name == name);
        let stage = if let Some(index) = index {
            &mut self.stages[index]
        } else {
            self.stages.push(ContextReadStage {
                name: name.to_owned(),
                ..Default::default()
            });
            self.stages.last_mut().expect("inserted stage")
        };
        stage.logical_reads += 1;
        stage.returned_records += rows;
        stage.elapsed_ns = stage
            .elapsed_ns
            .saturating_add(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceKind {
    Message,
    Turn,
    Brief,
    ToolExecution,
    Episode,
    WorkItem,
}

/// Read-only descriptor; optional metadata preserves domain meanings. The ref
/// is an alias unless the domain supplies an actual immutable version.
#[derive(Debug, Clone, Serialize)]
pub struct ContextSourceDescriptor {
    pub reference: String,
    pub kind: ContextSourceKind,
    pub agent_id: String,
    pub work_item_id: Option<String>,
    pub revision: Option<u64>,
    pub origin: Option<MessageOrigin>,
    pub authority_class: Option<AuthorityClass>,
    pub delivery_surface: Option<crate::types::MessageDeliverySurface>,
    pub admission_context: Option<crate::types::AdmissionContext>,
    /// Preserve named provenance, including opaque ids that are not runtime refs.
    pub provenance_refs: BTreeMap<String, String>,
    pub content_source: Option<crate::types::BriefContentSource>,
    pub body_digest: Option<String>,
    pub source_refs: Vec<String>,
    pub body_locator: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    id: u64,
    pub(super) data: Arc<ContextSnapshotData>,
    acquisition: ContextAcquisitionReport,
    descriptors: Arc<[ContextSourceDescriptor]>,
}
impl ContextSnapshot {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn acquisition(&self) -> &ContextAcquisitionReport {
        &self.acquisition
    }
    pub fn descriptors(&self) -> &[ContextSourceDescriptor] {
        &self.descriptors
    }
}

#[derive(Debug)]
pub(super) struct ContextSnapshotData {
    pub agent: AgentState,
    pub execution: ExecutionSnapshot,
    pub skills: SkillsRuntimeView,
    pub current_message: MessageEnvelope,
    pub continuation: Option<ContinuationResolution>,
    pub config: ContextConfig,
    pub history: Arc<HistorySnapshot>,
    pub recovery_source: Option<ProviderRecoverySource>,
    pub continuation_anchor_messages: Vec<MessageEnvelope>,
    pub active_wait_conditions: Vec<WaitConditionRecord>,
    pub episodes: Vec<ContextEpisodeRecord>,
    pub work_queue_projection: WorkQueueReadModel,
    pub current_work_item: Option<WorkItemRecord>,
    pub default_ingress: Option<ExternalTriggerRecord>,
    pub active_task_count: usize,
    pub active_tasks: Vec<TaskRecord>,
    pub completion_reports: BTreeMap<String, CompletionReportProjection>,
    pub plan_observations: BTreeMap<String, WorkItemPlanArtifact>,
    pub notes_content: Option<String>,
    pub pending_task_results: Vec<PendingTaskResult>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct HistorySnapshot {
    pub turn_records: Vec<TurnRecord>,
    pub messages: Vec<MessageEnvelope>,
    pub briefs: Vec<BriefRecord>,
    pub tools: Vec<ToolExecutionRecord>,
    pub transcript: Vec<TranscriptEntry>,
    pub observations: HistoryObservations,
}
#[derive(Debug, Clone, Default)]
pub(super) struct HistoryObservations {
    brief_text: BTreeMap<String, String>,
    valid_trigger_summary: BTreeMap<String, bool>,
}
impl HistoryObservations {
    pub(super) fn capture(
        storage: &AppStorage,
        turns: &[TurnRecord],
        briefs: &[BriefRecord],
    ) -> Self {
        let brief_text = briefs
            .iter()
            .map(|brief| (brief.id.clone(), resolved_brief_text(storage, brief)))
            .collect();
        let valid_trigger_summary = turns.iter().map(|record| {
            let valid = record.trigger.as_ref().and_then(|trigger| trigger.message_id.as_ref()).is_none_or(|message_id| {
                match storage.read_message_by_id(message_id) {
                    Ok(Some(message)) => turn_record_owns_object(record, message.turn_id.as_deref()),
                    Ok(None) => true,
                    Err(error) => {
                        tracing::warn!(turn_id = %record.turn_id, message_id = %message_id, %error, "context preparation could not validate persisted trigger summary");
                        false
                    }
                }
            });
            (record.turn_id.clone(), valid)
        }).collect();
        Self {
            brief_text,
            valid_trigger_summary,
        }
    }
    pub(super) fn brief_text<'a>(&'a self, brief: &'a BriefRecord) -> &'a str {
        self.brief_text
            .get(&brief.id)
            .map(String::as_str)
            .unwrap_or(&brief.text)
    }
    pub(super) fn trigger_summary_valid(&self, turn: &TurnRecord) -> bool {
        self.valid_trigger_summary
            .get(&turn.turn_id)
            .copied()
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub(super) struct PendingTaskResult {
    pub record: TaskResultSettlementRecord,
    pub message: Option<MessageEnvelope>,
    pub reply: Option<MessageEnvelope>,
}

/// Transitional adapter: sequential persistence reads do not claim the atomic
/// publication guarantee of the future AgentStateStore. Success prepares all
/// I/O dependencies used by the current renderer, including optional history.
#[allow(clippy::too_many_arguments)]
pub fn capture_context_from_storage(
    storage: &AppStorage,
    agent: &AgentState,
    execution: &ExecutionSnapshot,
    skills: &SkillsRuntimeView,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    config: &ContextConfig,
    agent_home: &Path,
    default_external_ingress_override: Option<&ExternalTriggerRecord>,
) -> Result<ContextSnapshot> {
    let _timer = crate::diagnostics::attribution::CONTEXT_ACQUIRE.start();
    let mut report = ContextAcquisitionReport::default();
    macro_rules! read {
        ($name:literal, $read:expr, $count:expr) => {
            report.read($name, || $read, $count)?
        };
    }
    let recovery_source = read!(
        "recovery",
        crate::runtime::provider_recovery::resolve_for_context(storage, current_message),
        |v| usize::from(v.is_some())
    );
    let continuation_anchor_messages = if recovery_source.is_some() {
        Vec::new()
    } else {
        read!("anchor_messages", storage.read_all_messages(), Vec::len)
    };
    let mut briefs = read!(
        "briefs",
        storage.read_recent_briefs(config.recent_briefs),
        Vec::len
    );
    let mut tools = read!(
        "tools",
        storage.read_recent_tool_executions(config.recent_messages),
        Vec::len
    );
    let mut turn_records = history_turns_without_current_input(
        read!(
            "turns",
            storage.read_recent_turns(config.recent_messages.saturating_add(1)),
            Vec::len
        ),
        &current_message.id,
        config.recent_messages,
    );
    if let Some(source) = recovery_source.as_ref() {
        if !turn_records
            .iter()
            .any(|record| record.turn_id == source.source_turn.turn_id)
        {
            turn_records.push(source.source_turn.clone());
        }
    }
    let mut messages = Vec::new();
    read!(
        "history_references",
        hydrate_recent_turn_references(
            storage,
            &turn_records,
            &mut messages,
            &mut briefs,
            &mut tools
        ),
        |_| 0
    );
    let mut transcript = read!(
        "transcript",
        storage.read_recent_transcript(config.recent_messages),
        Vec::len
    );
    if let Some(source) = recovery_source.as_ref() {
        for entry in read!(
            "recovery_transcript",
            storage.read_transcript_for_turns(&[source.source_turn.turn_id.clone()]),
            Vec::len
        ) {
            if !transcript.iter().any(|existing| existing == &entry) {
                transcript.push(entry);
            }
        }
    }
    let active_wait_conditions = read!(
        "waits",
        storage.active_wait_conditions_for_agent(&agent.id),
        Vec::len
    )
    .into_iter()
    .filter(|condition| condition.work_item_id.is_some())
    .collect();
    let episodes = read!(
        "episodes",
        storage.read_recent_context_episodes(config.recent_episode_candidates),
        Vec::len
    );
    let work_queue_projection = read!("work_queue", storage.work_queue_prompt_projection(), |_| 0);
    let execution_work_item = agent
        .current_execution_binding
        .as_ref()
        .and_then(|binding| binding.work_item_id.as_deref())
        .map(|id| {
            report.read(
                "execution_work_item",
                || storage.latest_work_item(id),
                |v| usize::from(v.is_some()),
            )
        })
        .transpose()?
        .flatten();
    let current_work_item =
        if agent.current_execution_binding.is_some() || recovery_source.is_some() {
            execution_work_item
        } else {
            work_queue_projection.current.clone()
        };
    let default_ingress = match default_external_ingress_override {
        Some(record) => Some(record.clone()),
        None => read!(
            "default_ingress",
            default_external_ingress(storage, &agent.id),
            |v| usize::from(v.is_some())
        ),
    };
    let active_task_count = read!(
        "active_task_count",
        storage.active_task_count_for_agent(&agent.id),
        |_| 0
    );
    let active_tasks = if active_task_count > 0 {
        read!(
            "active_tasks",
            storage.latest_active_task_records_for_agent(&agent.id, ACTIVE_TASKS_CONTEXT_LIMIT),
            Vec::len
        )
    } else {
        Vec::new()
    };
    let completion_reports = read!(
        "completion_reports",
        latest_completion_report_by_work_item(storage, &agent.id),
        BTreeMap::len
    );
    let observations = read!(
        "history_observations",
        Ok(HistoryObservations::capture(
            storage,
            &turn_records,
            &briefs
        )),
        |_| 0
    );
    // Preserve existing creation/error handling at the explicit I/O boundary.
    // WorkItem/file workflows will own creation once resource residency lands.
    let mut plan_observations = BTreeMap::new();
    if let Some(item) = current_work_item.as_ref() {
        let artifact = read!(
            "plan_observations",
            Ok(crate::work_item_plan::ensure_plan_artifact(storage.data_dir(), item, None).ok()),
            |v| usize::from(v.is_some())
        );
        if let Some(artifact) = artifact {
            plan_observations.insert(item.id.clone(), artifact);
        }
    }
    for item in candidate_items(&work_queue_projection) {
        if current_work_item
            .as_ref()
            .is_some_and(|current| current.id == item.id)
            || item.plan_artifact.is_some()
            || plan_observations.contains_key(&item.id)
        {
            continue;
        }
        if item.state == crate::types::WorkItemState::Completed
            && completion_report_for_work_item(&completion_reports, item).is_none()
        {
            continue;
        }
        let artifact = read!(
            "plan_observations",
            Ok(crate::work_item_plan::ensure_plan_artifact(storage.data_dir(), item, None).ok()),
            |v| usize::from(v.is_some())
        );
        if let Some(artifact) = artifact {
            plan_observations.insert(item.id.clone(), artifact);
        }
    }
    let notes_content = read!(
        "notes_observation",
        Ok(crate::notes_catalog::render_agent_home_notes_catalog_section(agent_home)),
        |v| usize::from(v.is_some())
    );
    let pending_task_results = read!(
        "pending_task_results",
        load_pending_task_results(storage, agent, current_message),
        Vec::len
    );
    let history = Arc::new(HistorySnapshot {
        turn_records,
        messages,
        briefs,
        tools,
        transcript,
        observations,
    });
    let data = ContextSnapshotData {
        agent: agent.clone(),
        execution: execution.clone(),
        skills: skills.clone(),
        current_message: current_message.clone(),
        continuation: continuation.cloned(),
        config: config.clone(),
        history,
        recovery_source,
        continuation_anchor_messages,
        active_wait_conditions,
        episodes,
        work_queue_projection,
        current_work_item,
        default_ingress,
        active_task_count,
        active_tasks,
        completion_reports,
        plan_observations,
        notes_content,
        pending_task_results,
    };
    let descriptors = descriptors(&data).into();
    Ok(ContextSnapshot {
        id: NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed),
        data: Arc::new(data),
        acquisition: report,
        descriptors,
    })
}

fn candidate_items(projection: &WorkQueueReadModel) -> impl Iterator<Item = &WorkItemRecord> {
    projection
        .triggered_blocked
        .iter()
        .chain(projection.current_runnable.iter())
        .chain(projection.queued_runnable.iter())
        .chain(projection.yielded.iter())
        .chain(projection.waiting_for_operator.iter())
        .chain(projection.blocked.iter())
        .chain(projection.completing.iter())
        .chain(projection.completed_recent.iter())
        .map(|item| item.record())
}

pub(super) fn load_pending_task_results(
    storage: &AppStorage,
    agent: &AgentState,
    current_message: &MessageEnvelope,
) -> Result<Vec<PendingTaskResult>> {
    let Some(activation_id) = agent
        .current_execution_binding
        .as_ref()
        .and_then(|binding| binding.activation_id.as_deref())
    else {
        return Ok(Vec::new());
    };
    let Some(db) = storage.runtime_db()? else {
        return Ok(Vec::new());
    };
    let mut pending = Vec::new();
    let mut seen_replies = std::collections::BTreeSet::new();
    for record in db
        .task_result_settlements()
        .admitted_for_activation(&agent.id, activation_id)?
    {
        if record.message_id == current_message.id {
            continue;
        }
        let message = storage.read_message_by_id(&record.message_id)?;
        if message
            .as_ref()
            .and_then(crate::wake_contract::agent_message_reply_reference)
            == Some(current_message.id.as_str())
        {
            continue;
        }
        // Match the renderer's bound and deduplication before touching reply storage.
        let reply = if pending.len() < PENDING_TASK_RESULTS_CONTEXT_LIMIT {
            message
                .as_ref()
                .and_then(crate::wake_contract::agent_message_reply_reference)
                .filter(|id| seen_replies.insert((*id).to_owned()))
                .map(|id| storage.read_message_by_id(id))
                .transpose()?
                .flatten()
        } else {
            None
        };
        pending.push(PendingTaskResult {
            record,
            message,
            reply,
        });
    }
    Ok(pending)
}

pub(crate) fn prepare_work_item_scoped_history(
    storage: &AppStorage,
    reprojection: &RecentTurnsReprojection,
    window_limit: usize,
) -> Result<RecentTurnsReprojection> {
    let Some(work_item) = reprojection.current_work_item.as_ref() else {
        return Ok(reprojection.clone());
    };
    if reprojection.scoped_window == Some(window_limit) {
        return Ok(reprojection.clone());
    }
    let _timer = crate::diagnostics::attribution::CONTEXT_HISTORY_PREPARE.start();
    let owner = crate::types::TurnOwner::WorkItem {
        work_item_id: work_item.id.clone(),
    };
    let mut turn_records = history_turns_without_current_input(
        storage.read_recent_turns_for_owner(&owner, window_limit.saturating_add(1))?,
        &reprojection.current_message.id,
        window_limit,
    );
    if let Some(source) = crate::runtime::provider_recovery::resolve_for_context(
        storage,
        &reprojection.current_message,
    )? {
        if !turn_records
            .iter()
            .any(|record| record.turn_id == source.source_turn.turn_id)
        {
            turn_records.push(source.source_turn);
        }
    }
    let (mut messages, mut briefs, mut tools) = (Vec::new(), Vec::new(), Vec::new());
    hydrate_recent_turn_references(
        storage,
        &turn_records,
        &mut messages,
        &mut briefs,
        &mut tools,
    )?;
    let ids = turn_records
        .iter()
        .map(|record| record.turn_id.clone())
        .collect::<Vec<_>>();
    let transcript = storage.read_transcript_for_turns(&ids)?;
    let observations = HistoryObservations::capture(storage, &turn_records, &briefs);
    let mut prepared = reprojection.clone();
    prepared.history = Arc::new(HistorySnapshot {
        turn_records,
        messages,
        briefs,
        tools,
        transcript,
        observations,
    });
    prepared.scoped_window = Some(window_limit);
    prepared.snapshot_id = NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
    Ok(prepared)
}

fn descriptors(data: &ContextSnapshotData) -> Vec<ContextSourceDescriptor> {
    let mut out = BTreeMap::new();
    let base = |reference: String, kind, agent_id: &str, work_item_id: Option<String>| {
        ContextSourceDescriptor {
            reference,
            kind,
            agent_id: agent_id.to_owned(),
            work_item_id,
            revision: None,
            origin: None,
            authority_class: None,
            delivery_surface: None,
            admission_context: None,
            provenance_refs: BTreeMap::new(),
            content_source: None,
            body_digest: None,
            source_refs: Vec::new(),
            body_locator: None,
        }
    };
    for message in data
        .history
        .messages
        .iter()
        .chain(data.continuation_anchor_messages.iter())
        .chain(std::iter::once(&data.current_message))
        .chain(
            data.recovery_source
                .iter()
                .map(|source| &source.root_message),
        )
    {
        let mut item = base(
            format!("message:{}", message.id),
            ContextSourceKind::Message,
            &message.agent_id,
            message.work_item_id.clone(),
        );
        item.origin = Some(message.origin.clone());
        item.authority_class = Some(message.authority_class);
        item.delivery_surface = message.delivery_surface;
        item.admission_context = message.admission_context;
        item.provenance_refs = message.source_refs.clone();
        item.source_refs = message
            .source_refs
            .values()
            .filter(|reference| crate::memory::refs::RuntimeRef::parse(reference).is_ok())
            .cloned()
            .collect();
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for turn in &data.history.turn_records {
        let mut item = base(
            format!("turn:{}", turn.turn_id),
            ContextSourceKind::Turn,
            &turn.agent_id,
            turn.current_work_item_id.clone(),
        );
        item.source_refs = turn
            .input_message_ids
            .iter()
            .map(|id| format!("message:{id}"))
            .chain(
                turn.produced_brief_ids
                    .iter()
                    .map(|id| format!("brief:{id}")),
            )
            .chain(
                turn.tool_execution_ids
                    .iter()
                    .map(|id| format!("tool_execution:{id}:output")),
            )
            .collect();
        out.insert(item.reference.clone(), item);
    }
    for brief in &data.history.briefs {
        let mut item = base(
            format!("brief:{}", brief.id),
            ContextSourceKind::Brief,
            &brief.agent_id,
            brief.work_item_id.clone(),
        );
        item.source_refs = brief
            .related_message_id
            .iter()
            .map(|id| format!("message:{id}"))
            .collect();
        item.content_source = Some(brief.content_source.clone());
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for tool in &data.history.tools {
        let mut item = base(
            format!("tool_execution:{}:output", tool.id),
            ContextSourceKind::ToolExecution,
            &tool.agent_id,
            tool.work_item_id.clone(),
        );
        item.authority_class = Some(tool.authority_class);
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for episode in &data.episodes {
        let mut item = base(
            format!("episode:{}", episode.id),
            ContextSourceKind::Episode,
            &episode.agent_id,
            episode.current_work_item_id.clone(),
        );
        item.source_refs = episode.source_refs.clone();
        item.body_locator = Some(item.reference.clone());
        out.insert(item.reference.clone(), item);
    }
    for work in data
        .current_work_item
        .iter()
        .chain(candidate_items(&data.work_queue_projection))
    {
        let mut item = base(
            format!("work_item:{}", work.id),
            ContextSourceKind::WorkItem,
            &work.agent_id,
            Some(work.id.clone()),
        );
        item.revision = Some(work.revision);
        item.source_refs = work
            .work_refs
            .iter()
            .map(|source| source.ref_id.clone())
            .collect();
        // A plan path is an observed mutable resource, not a new runtime namespace
        // or a promise that the historical bytes can be retrieved later.
        if let Some(plan) = data
            .plan_observations
            .get(&work.id)
            .or(work.plan_artifact.as_ref())
        {
            item.body_locator = Some(plan.path.display().to_string());
            item.body_digest = Some(plan.hash.clone());
        }
        out.insert(item.reference.clone(), item);
    }
    out.into_values().collect()
}
