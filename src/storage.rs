use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use crate::{
    runtime_db::RuntimeDb,
    types::{
        AgentIdentityRecord, AgentPostureProjection, AgentSchedulingPosture, AgentState,
        AgentStatus, AuditEvent, BriefRecord, ContextEpisodeRecord, DeliverySummaryRecord,
        ExternalTriggerRecord, ExternalWaitRecoverability, MessageEnvelope, OperatorDeliveryRecord,
        OperatorNotificationRecord, OperatorTransportBinding, QueueEntryRecord, QueueEntryStatus,
        TaskRecord, TaskStatus, TimerRecord, TodoItem, TodoItemState, ToolExecutionRecord,
        TranscriptEntry, TurnRecord, WaitConditionKind, WaitConditionRecord, WaitConditionStatus,
        WaitingIntentRecord, WaitingIntentScope, WaitingIntentStatus, WakeSource,
        WorkItemDelegationRecord, WorkItemDelegationState, WorkItemReadiness, WorkItemRecord,
        WorkItemSchedulingState, WorkItemState, WorkingMemoryDelta, WorkspaceEntry,
        WorkspaceOccupancyRecord,
    },
};

const RUNTIME_DIR: &str = ".holon";
const RUNTIME_STATE_DIR: &str = "state";
const RUNTIME_LEDGER_DIR: &str = "ledger";
const RUNTIME_INDEXES_DIR: &str = "indexes";
const RUNTIME_CACHE_DIR: &str = "cache";

#[derive(Debug, Clone, Default)]
pub struct WorkQueuePromptProjection {
    pub current: Option<WorkItemRecord>,
    pub queued_blocked: Vec<WorkItemRecord>,
    pub readiness: Vec<WorkItemReadinessProjection>,
    pub current_runnable: Option<WorkItemReadinessProjection>,
    pub triggered_blocked: Vec<WorkItemReadinessProjection>,
    pub queued_runnable: Vec<WorkItemReadinessProjection>,
    pub waiting_for_operator: Vec<WorkItemReadinessProjection>,
    pub blocked: Vec<WorkItemReadinessProjection>,
    pub completed_recent: Vec<WorkItemReadinessProjection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventLogPageOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub(crate) struct EventLogPage {
    pub(crate) events: Vec<AuditEvent>,
    pub(crate) has_older: bool,
    pub(crate) has_newer: bool,
}

impl WorkQueuePromptProjection {
    pub fn has_non_current_candidates(&self) -> bool {
        self.triggered_blocked.iter().any(|item| !item.is_current)
            || self.queued_runnable.iter().any(|item| !item.is_current)
            || self
                .waiting_for_operator
                .iter()
                .any(|item| !item.is_current)
            || self.blocked.iter().any(|item| !item.is_current)
            || self.completed_recent.iter().any(|item| !item.is_current)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItemReadinessProjection {
    pub work_item: WorkItemRecord,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub is_current: bool,
    pub has_active_waits: bool,
    pub has_active_task_waits: bool,
    pub has_triggered_waits: bool,
    pub last_triggered_at: Option<DateTime<Utc>>,
    pub current_todo: Option<TodoItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItemCandidateClass {
    CurrentRunnable,
    TriggeredBlocked,
    QueuedRunnable,
    WaitingForOperator,
    Blocked,
    CompletedRecent,
}

#[derive(Debug, Default)]
struct ActiveWaitConditionStates {
    task: bool,
    external: bool,
    operator: bool,
    timer: bool,
    system: bool,
}

impl ActiveWaitConditionStates {
    fn record(&mut self, kind: WaitConditionKind) {
        match kind {
            WaitConditionKind::Task => self.task = true,
            WaitConditionKind::External => self.external = true,
            WaitConditionKind::Operator => self.operator = true,
            WaitConditionKind::Timer => self.timer = true,
            WaitConditionKind::System => self.system = true,
        }
    }

    fn scheduling_state(&self) -> Option<WorkItemSchedulingState> {
        if self.task {
            Some(WorkItemSchedulingState::WaitingTask)
        } else if self.operator {
            Some(WorkItemSchedulingState::WaitingOperator)
        } else if self.timer {
            Some(WorkItemSchedulingState::WaitingTimer)
        } else if self.external {
            Some(WorkItemSchedulingState::WaitingExternal)
        } else if self.system {
            Some(WorkItemSchedulingState::WaitingSystem)
        } else {
            None
        }
    }
}

impl WorkItemReadinessProjection {
    pub fn record(&self) -> &WorkItemRecord {
        &self.work_item
    }

    fn posture_reason(&self) -> String {
        let label = if self.is_current {
            "current WorkItem"
        } else {
            "queued WorkItem"
        };
        format!(
            "{label} {} is {:?}",
            self.work_item.id, self.scheduling_state
        )
    }
}

#[derive(Debug, Clone)]
pub struct RecoverySnapshot {
    pub agent: Option<AgentState>,
    pub replay_messages: Vec<MessageEnvelope>,
    pub active_tasks: Vec<TaskRecord>,
    pub active_timers: Vec<TimerRecord>,
    pub work_items: Vec<WorkItemRecord>,
    pub work_item_delegations: Vec<WorkItemDelegationRecord>,
}

#[derive(Debug, Clone)]
pub struct AppStorage {
    data_dir: PathBuf,
    events_path: PathBuf,
    briefs_path: PathBuf,
    messages_path: PathBuf,
    tasks_path: PathBuf,
    work_items_path: PathBuf,
    delivery_summaries_path: PathBuf,
    work_item_delegations_path: PathBuf,
    timers_path: PathBuf,
    tools_path: PathBuf,
    turns_path: PathBuf,
    transcript_path: PathBuf,
    queue_entries_path: PathBuf,
    waiting_intents_path: PathBuf,
    wait_conditions_path: PathBuf,
    external_triggers_path: PathBuf,
    operator_notifications_path: PathBuf,
    operator_transport_bindings_path: PathBuf,
    operator_delivery_records_path: PathBuf,
    working_memory_deltas_path: PathBuf,
    context_episodes_path: PathBuf,
    workspaces_path: PathBuf,
    occupancies_path: PathBuf,
    agent_identities_path: PathBuf,
    agent_path: PathBuf,
    append_mutex: Arc<Mutex<()>>,
    event_seq_counter: Arc<Mutex<u64>>,
    message_seq_counter: Arc<Mutex<u64>>,
    transcript_seq_counter: Arc<Mutex<u64>>,
    audit_event_index: Arc<Mutex<Option<AuditEventIndexSink>>>,
    scheduler_control_plane_db: Arc<Mutex<Option<RuntimeDb>>>,
}

#[derive(Debug, Clone)]
struct AuditEventIndexSink {
    runtime_db: RuntimeDb,
    agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileActivityMarker {
    pub exists: bool,
    pub len: u64,
    pub modified_unix_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollActivityMarker {
    pub briefs: FileActivityMarker,
    pub tasks: FileActivityMarker,
    pub tools: FileActivityMarker,
    pub events: FileActivityMarker,
    pub transcript: FileActivityMarker,
}

impl AppStorage {
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create {}", data_dir.display()))?;
        let runtime_dir = data_dir.join(RUNTIME_DIR);
        let state_dir = runtime_dir.join(RUNTIME_STATE_DIR);
        let ledger_dir = runtime_dir.join(RUNTIME_LEDGER_DIR);
        for dir in [
            &state_dir,
            &ledger_dir,
            &runtime_dir.join(RUNTIME_INDEXES_DIR),
            &runtime_dir.join(RUNTIME_CACHE_DIR),
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }

        let events_path = ledger_dir.join("events.jsonl");
        let messages_path = ledger_dir.join("messages.jsonl");
        let transcript_path = ledger_dir.join("transcript.jsonl");
        let event_seq_counter = migrate_events_ledger(&events_path)?;
        let message_seq_counter = max_jsonl_u64_field(&messages_path, "message_seq")?;
        let transcript_seq_counter = max_jsonl_u64_field(&transcript_path, "transcript_seq")?;

        Ok(Self {
            events_path,
            briefs_path: ledger_dir.join("briefs.jsonl"),
            messages_path,
            tasks_path: ledger_dir.join("tasks.jsonl"),
            work_items_path: ledger_dir.join("work_items.jsonl"),
            delivery_summaries_path: ledger_dir.join("delivery_summaries.jsonl"),
            work_item_delegations_path: ledger_dir.join("work_item_delegations.jsonl"),
            timers_path: ledger_dir.join("timers.jsonl"),
            tools_path: ledger_dir.join("tools.jsonl"),
            turns_path: ledger_dir.join("turns.jsonl"),
            transcript_path,
            queue_entries_path: ledger_dir.join("queue_entries.jsonl"),
            waiting_intents_path: ledger_dir.join("waiting_intents.jsonl"),
            wait_conditions_path: ledger_dir.join("wait_conditions.jsonl"),
            external_triggers_path: ledger_dir.join("external_triggers.jsonl"),
            operator_notifications_path: ledger_dir.join("operator_notifications.jsonl"),
            operator_transport_bindings_path: ledger_dir.join("operator_transport_bindings.jsonl"),
            operator_delivery_records_path: ledger_dir.join("operator_delivery_records.jsonl"),
            working_memory_deltas_path: ledger_dir.join("working_memory_deltas.jsonl"),
            context_episodes_path: ledger_dir.join("context_episodes.jsonl"),
            workspaces_path: ledger_dir.join("workspaces.jsonl"),
            occupancies_path: ledger_dir.join("workspace_occupancies.jsonl"),
            agent_identities_path: ledger_dir.join("agent_identities.jsonl"),
            agent_path: state_dir.join("agent.json"),
            append_mutex: Arc::new(Mutex::new(())),
            event_seq_counter: Arc::new(Mutex::new(event_seq_counter)),
            message_seq_counter: Arc::new(Mutex::new(message_seq_counter)),
            transcript_seq_counter: Arc::new(Mutex::new(transcript_seq_counter)),
            audit_event_index: Arc::new(Mutex::new(None)),
            scheduler_control_plane_db: Arc::new(Mutex::new(None)),
            data_dir,
        })
    }

    pub(crate) fn enable_audit_event_index(
        &self,
        runtime_db: RuntimeDb,
        agent_id: Option<String>,
    ) -> Result<()> {
        let mut guard = self
            .audit_event_index
            .lock()
            .map_err(|_| anyhow::anyhow!("audit event index mutex poisoned"))?;
        *guard = Some(AuditEventIndexSink {
            runtime_db,
            agent_id,
        });
        Ok(())
    }

    pub(crate) fn enable_scheduler_control_plane_db(&self, runtime_db: RuntimeDb) -> Result<()> {
        let agent_id = self.current_agent_id()?;
        {
            let mut counter = self
                .message_seq_counter
                .lock()
                .map_err(|_| anyhow::anyhow!("message sequence counter mutex poisoned"))?;
            *counter = (*counter).max(runtime_db.messages().max_message_seq(agent_id.as_deref())?);
        }
        {
            let mut counter = self
                .transcript_seq_counter
                .lock()
                .map_err(|_| anyhow::anyhow!("transcript sequence counter mutex poisoned"))?;
            *counter = (*counter).max(
                runtime_db
                    .transcript_entries()
                    .max_transcript_seq(agent_id.as_deref())?,
            );
        }
        {
            let mut counter = self
                .event_seq_counter
                .lock()
                .map_err(|_| anyhow::anyhow!("event sequence counter mutex poisoned"))?;
            *counter = (*counter).max(
                runtime_db
                    .audit_events()
                    .max_event_seq(agent_id.as_deref())?,
            );
        }
        let mut guard = self
            .scheduler_control_plane_db
            .lock()
            .map_err(|_| anyhow::anyhow!("scheduler control-plane db mutex poisoned"))?;
        *guard = Some(runtime_db);
        Ok(())
    }

    fn scheduler_control_plane_db(&self) -> Result<Option<RuntimeDb>> {
        Ok(self
            .scheduler_control_plane_db
            .lock()
            .map_err(|_| anyhow::anyhow!("scheduler control-plane db mutex poisoned"))?
            .clone())
    }

    fn current_agent_id(&self) -> Result<Option<String>> {
        if let Some(agent) = self.read_agent_file()? {
            return Ok(Some(agent.id));
        }
        let parent_is_agents_dir = self
            .data_dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("agents");
        if !parent_is_agents_dir {
            return Ok(None);
        }
        Ok(self
            .data_dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(ToString::to_string))
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.data_dir.join(RUNTIME_DIR)
    }

    pub fn state_dir(&self) -> PathBuf {
        self.runtime_dir().join(RUNTIME_STATE_DIR)
    }

    pub fn ledger_dir(&self) -> PathBuf {
        self.runtime_dir().join(RUNTIME_LEDGER_DIR)
    }

    pub fn indexes_dir(&self) -> PathBuf {
        self.runtime_dir().join(RUNTIME_INDEXES_DIR)
    }

    // Shared search projections live at host data scope. They are rebuildable
    // indexes, not canonical runtime state.
    pub fn shared_indexes_dir(&self) -> PathBuf {
        self.data_dir
            .parent()
            .filter(|agents_dir| agents_dir.file_name().is_some_and(|name| name == "agents"))
            .and_then(|agents_dir| agents_dir.parent())
            .map(|host_data_dir| host_data_dir.join(RUNTIME_DIR).join(RUNTIME_INDEXES_DIR))
            .unwrap_or_else(|| self.indexes_dir())
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.runtime_dir().join(RUNTIME_CACHE_DIR)
    }

    pub(crate) fn runtime_db(&self) -> Result<Option<RuntimeDb>> {
        self.scheduler_control_plane_db()
    }

    pub fn poll_activity_marker(&self) -> Result<PollActivityMarker> {
        Ok(PollActivityMarker {
            briefs: file_activity_marker(&self.briefs_path)?,
            tasks: file_activity_marker(&self.tasks_path)?,
            tools: file_activity_marker(&self.tools_path)?,
            events: self.audit_events_activity_marker()?,
            transcript: file_activity_marker(&self.transcript_path)?,
        })
    }

    fn audit_events_activity_marker(&self) -> Result<FileActivityMarker> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            let latest_seq = runtime_db
                .audit_events()
                .latest_event_seq(self.current_agent_id()?.as_deref())?
                .unwrap_or(0);
            return Ok(FileActivityMarker {
                exists: latest_seq > 0,
                len: latest_seq,
                modified_unix_ms: u128::from(latest_seq),
            });
        }
        file_activity_marker(&self.events_path)
    }

    pub fn append_event(&self, event: &AuditEvent) -> Result<()> {
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        self.append_event_with_append_mutex_held(event)
    }

    fn append_event_with_append_mutex_held(&self, event: &AuditEvent) -> Result<()> {
        let mut event = event.clone();
        let mut counter = self
            .event_seq_counter
            .lock()
            .map_err(|_| anyhow::anyhow!("event sequence counter mutex poisoned"))?;
        *counter += 1;
        event.event_seq = *counter;
        if let Some(sink) = self
            .audit_event_index
            .lock()
            .map_err(|_| anyhow::anyhow!("audit event index mutex poisoned"))?
            .clone()
        {
            sink.runtime_db
                .audit_events()
                .append(sink.agent_id.as_deref(), &event)?;
            return Ok(());
        }
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            if runtime_db.storage_domain_is_complete("audit_events", "db")? {
                runtime_db
                    .audit_events()
                    .append(self.current_agent_id()?.as_deref(), &event)?;
                return Ok(());
            }
        }
        let line = serde_json::to_string(&event)?;
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        append_jsonl_bytes(&self.events_path, &bytes)?;
        Ok(())
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.evidence().append_brief(brief)?;
            return self.mark_memory_index_dirty();
        }
        self.append_jsonl(&self.briefs_path, brief)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        let mut message = message.clone();
        let mut counter = self
            .message_seq_counter
            .lock()
            .map_err(|_| anyhow::anyhow!("message sequence counter mutex poisoned"))?;
        *counter += 1;
        message.message_seq = Some(*counter);
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.messages().upsert(&message)?;
            return Ok(());
        }
        let bytes = jsonl_bytes(&message)?;
        append_jsonl_bytes(&self.messages_path, &bytes)
    }

    pub fn append_task(&self, task: &TaskRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.tasks().upsert(task)?;
            return self.mark_memory_index_dirty();
        }
        self.append_jsonl(&self.tasks_path, task)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_work_item(&self, record: &WorkItemRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            let current_focus = self
                .read_agent()?
                .and_then(|agent| agent.current_work_item_id)
                .as_deref()
                == Some(record.id.as_str());
            runtime_db.work_items().upsert(record, current_focus)?;
            return self.mark_memory_index_dirty();
        }
        self.append_jsonl(&self.work_items_path, record)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.evidence().append_delivery_summary(record)?;
            return Ok(());
        }
        self.append_jsonl(&self.delivery_summaries_path, record)
    }

    pub fn append_work_item_delegation(&self, record: &WorkItemDelegationRecord) -> Result<()> {
        self.append_jsonl(&self.work_item_delegations_path, record)
    }

    pub fn append_timer(&self, timer: &TimerRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.timers().upsert(timer)?;
            return Ok(());
        }
        self.append_jsonl(&self.timers_path, timer)
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.evidence().append_tool_execution(record)?;
            if matches!(
                record.tool_name.as_str(),
                "ExecCommand" | "ExecCommandBatch"
            ) {
                self.mark_memory_index_dirty()?;
            }
            return Ok(());
        }
        self.append_jsonl(&self.tools_path, record)?;
        if matches!(
            record.tool_name.as_str(),
            "ExecCommand" | "ExecCommandBatch"
        ) {
            self.mark_memory_index_dirty()?;
        }
        Ok(())
    }

    pub fn append_turn(&self, record: &TurnRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.turn_records().upsert(record)?;
            return Ok(());
        }
        self.append_jsonl(&self.turns_path, record)
    }

    pub fn append_transcript_entry(&self, entry: &TranscriptEntry) -> Result<()> {
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        let mut entry = entry.clone();
        let mut counter = self
            .transcript_seq_counter
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript sequence counter mutex poisoned"))?;
        *counter += 1;
        entry.transcript_seq = Some(*counter);
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.transcript_entries().upsert(&entry)?;
            return Ok(());
        }
        let bytes = jsonl_bytes(&entry)?;
        append_jsonl_bytes(&self.transcript_path, &bytes)
    }

    pub fn append_queue_entry(&self, record: &QueueEntryRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.queue_entries().upsert(record)?;
            return Ok(());
        }
        self.append_jsonl(&self.queue_entries_path, record)
    }

    pub fn append_waiting_intent(&self, record: &WaitingIntentRecord) -> Result<()> {
        let wait_condition = wait_condition_from_waiting_intent(record);
        let event = external_wait_recoverability_event(&wait_condition);
        let waiting_intent_bytes = jsonl_bytes(record)?;
        let wait_condition_bytes = jsonl_bytes(&wait_condition)?;

        // Compatibility migration: keep the legacy waiting-intents ledger as the
        // first durable write, then mirror the same state into the internal wait
        // condition ledger while holding the storage append mutex so no other
        // append can observe or interleave with the ordered pair. A filesystem
        // failure on the second write can still leave the legacy append present;
        // callers should treat the returned error as a mirror consistency failure,
        // not proof that the legacy intent was absent.
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            append_jsonl_bytes(&self.waiting_intents_path, &waiting_intent_bytes)?;
            runtime_db.wait_conditions().upsert(&wait_condition)?;
            if let Some(event) = event.as_ref() {
                self.append_event_with_append_mutex_held(event)?;
            }
            return Ok(());
        }
        append_jsonl_bytes(&self.waiting_intents_path, &waiting_intent_bytes)?;
        append_jsonl_bytes(&self.wait_conditions_path, &wait_condition_bytes)?;
        if let Some(event) = event.as_ref() {
            self.append_event_with_append_mutex_held(event)?;
        }
        Ok(())
    }

    pub fn append_wait_condition(&self, record: &WaitConditionRecord) -> Result<()> {
        let event = external_wait_recoverability_event(record);
        let wait_condition_bytes = jsonl_bytes(record)?;

        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.wait_conditions().upsert(record)?;
            if let Some(event) = event.as_ref() {
                self.append_event_with_append_mutex_held(event)?;
            }
            return Ok(());
        }
        append_jsonl_bytes(&self.wait_conditions_path, &wait_condition_bytes)?;
        if let Some(event) = event.as_ref() {
            self.append_event_with_append_mutex_held(event)?;
        }
        Ok(())
    }

    pub fn append_external_trigger(&self, record: &ExternalTriggerRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.external_triggers().upsert(record)?;
            return Ok(());
        }
        self.append_jsonl(&self.external_triggers_path, record)
    }

    pub fn append_operator_notification(&self, record: &OperatorNotificationRecord) -> Result<()> {
        self.append_jsonl(&self.operator_notifications_path, record)
    }

    pub fn append_operator_transport_binding(
        &self,
        record: &OperatorTransportBinding,
    ) -> Result<()> {
        self.append_jsonl(&self.operator_transport_bindings_path, record)
    }

    pub fn append_operator_delivery_record(&self, record: &OperatorDeliveryRecord) -> Result<()> {
        self.append_jsonl(&self.operator_delivery_records_path, record)
    }

    pub fn append_working_memory_delta(&self, record: &WorkingMemoryDelta) -> Result<()> {
        self.append_jsonl(&self.working_memory_deltas_path, record)
    }

    pub fn append_context_episode(&self, record: &ContextEpisodeRecord) -> Result<()> {
        self.append_jsonl(&self.context_episodes_path, record)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_workspace_entry(&self, entry: &WorkspaceEntry) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.workspace_entries().upsert(entry)?;
            return self.mark_memory_index_dirty();
        }
        self.append_jsonl(&self.workspaces_path, entry)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_workspace_occupancy(&self, entry: &WorkspaceOccupancyRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.workspace_occupancies().upsert(entry)?;
            return Ok(());
        }
        self.append_jsonl(&self.occupancies_path, entry)
    }

    pub fn append_agent_identity(&self, entry: &AgentIdentityRecord) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.agent_identities().upsert(entry)?;
            return Ok(());
        }
        self.append_jsonl(&self.agent_identities_path, entry)
    }

    fn append_jsonl<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let bytes = jsonl_bytes(value)?;

        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        append_jsonl_bytes(path, &bytes)
    }

    pub fn mark_memory_index_dirty(&self) -> Result<()> {
        let agent_id = self.storage_agent_id()?;
        let dirty_path = self.shared_indexes_dir().join(format!(
            "memory.{}.dirty",
            memory_index_agent_key(&agent_id)
        ));
        if dirty_path.exists() {
            return Ok(());
        }
        fs::create_dir_all(self.shared_indexes_dir())?;
        fs::write(&dirty_path, b"dirty").with_context(|| "failed to mark memory index dirty")
    }

    fn storage_agent_id(&self) -> Result<String> {
        Ok(self.current_agent_id()?.unwrap_or_else(|| "unknown".into()))
    }

    pub fn write_agent(&self, agent: &AgentState) -> Result<()> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            runtime_db.agent_states().upsert(agent)?;
        }
        let content = serde_json::to_vec_pretty(agent)?;
        let tmp_path = self
            .agent_path
            .with_file_name(format!(".agent.json.{}.tmp", uuid::Uuid::new_v4().simple()));
        fs::write(&tmp_path, content)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &self.agent_path).with_context(|| {
            format!(
                "failed to replace {} with {}",
                self.agent_path.display(),
                tmp_path.display()
            )
        })
    }

    pub fn read_agent(&self) -> Result<Option<AgentState>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            if let Some(agent_id) = self.current_agent_id()? {
                if let Some(agent) = runtime_db.agent_states().latest(&agent_id)? {
                    return Ok(Some(agent));
                }
            }
        }
        self.read_agent_file()
    }

    fn read_agent_file(&self) -> Result<Option<AgentState>> {
        let path = if self.agent_path.exists() {
            &self.agent_path
        } else {
            return Ok(None);
        };
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn read_recent_events(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .audit_events()
                .recent(self.current_agent_id()?.as_deref(), limit);
        }
        read_recent_jsonl(&self.events_path, limit)
    }

    pub(crate) fn read_legacy_events_jsonl(&self) -> Result<Vec<AuditEvent>> {
        read_recent_jsonl(&self.events_path, usize::MAX)
    }

    pub fn latest_event_seq(&self) -> Result<Option<u64>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .audit_events()
                .latest_event_seq(self.current_agent_id()?.as_deref());
        }
        let mut latest = None;
        scan_jsonl_reverse::<AuditEvent, _>(&self.events_path, |event| {
            latest = Some(event.event_seq);
            false
        })?;
        Ok(latest)
    }

    pub(crate) fn read_event_page_matching<F>(
        &self,
        before_seq: Option<u64>,
        after_seq: Option<u64>,
        limit: usize,
        order: EventLogPageOrder,
        mut matches: F,
    ) -> Result<EventLogPage>
    where
        F: FnMut(&AuditEvent) -> bool,
    {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            if limit == 0 {
                return Ok(EventLogPage {
                    events: Vec::new(),
                    has_older: false,
                    has_newer: false,
                });
            }
            let descending = matches!(order, EventLogPageOrder::Desc);
            let mut page = Vec::with_capacity(limit.saturating_add(1).min(1024));
            let agent_id = self.current_agent_id()?;
            let chunk_limit = limit.saturating_add(1).clamp(64, 1024);
            let mut next_before_seq = before_seq;
            let mut next_after_seq = after_seq;
            loop {
                let chunk = runtime_db.audit_events().range(
                    agent_id.as_deref(),
                    next_before_seq,
                    next_after_seq,
                    descending,
                    chunk_limit,
                )?;
                let Some(last_seq) = chunk.last().map(|event| event.event_seq) else {
                    break;
                };
                for event in chunk {
                    if matches(&event) {
                        page.push(event);
                    }
                    if page.len() > limit {
                        break;
                    }
                }
                if page.len() > limit {
                    break;
                }
                if descending {
                    next_before_seq = Some(last_seq);
                } else {
                    next_after_seq = Some(last_seq);
                }
            }
            let has_more = page.len() > limit;
            if has_more {
                page.truncate(limit);
            }
            return Ok(match order {
                EventLogPageOrder::Desc => EventLogPage {
                    events: page,
                    has_older: has_more,
                    has_newer: false,
                },
                EventLogPageOrder::Asc => EventLogPage {
                    events: page,
                    has_older: false,
                    has_newer: has_more,
                },
            });
        }
        if limit == 0 || !self.events_path.exists() {
            return Ok(EventLogPage {
                events: Vec::new(),
                has_older: false,
                has_newer: false,
            });
        }

        let lower = after_seq.unwrap_or(0);
        let upper = before_seq.unwrap_or(u64::MAX);
        let mut page = Vec::with_capacity(limit.saturating_add(1).min(1024));
        match order {
            EventLogPageOrder::Desc => {
                scan_jsonl_reverse::<AuditEvent, _>(&self.events_path, |event| {
                    if event.event_seq >= upper {
                        return true;
                    }
                    if event.event_seq <= lower {
                        return false;
                    }
                    if matches(&event) {
                        page.push(event);
                    }
                    page.len() <= limit
                })?;
                let has_older = page.len() > limit;
                if has_older {
                    page.truncate(limit);
                }
                Ok(EventLogPage {
                    events: page,
                    has_older,
                    has_newer: false,
                })
            }
            EventLogPageOrder::Asc => {
                let file = fs::File::open(&self.events_path)
                    .with_context(|| format!("failed to read {}", self.events_path.display()))?;
                for line in BufReader::new(file).lines() {
                    let line = line.with_context(|| {
                        format!("failed to read {}", self.events_path.display())
                    })?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event: AuditEvent = serde_json::from_str(&line).with_context(|| {
                        format!("failed to decode line from {}", self.events_path.display())
                    })?;
                    if event.event_seq <= lower {
                        continue;
                    }
                    if event.event_seq >= upper {
                        break;
                    }
                    if matches(&event) {
                        page.push(event);
                    }
                    if page.len() > limit {
                        break;
                    }
                }
                let has_newer = page.len() > limit;
                if has_newer {
                    page.truncate(limit);
                }
                Ok(EventLogPage {
                    events: page,
                    has_older: false,
                    has_newer,
                })
            }
        }
    }

    pub fn read_recent_briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .evidence()
                .recent_briefs(&self.storage_agent_id()?, limit);
        }
        read_recent_jsonl(&self.briefs_path, limit)
    }

    pub fn read_recent_messages(&self, limit: usize) -> Result<Vec<MessageEnvelope>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .messages()
                .recent(self.current_agent_id()?.as_deref(), limit);
        }
        read_recent_jsonl(&self.messages_path, limit)
    }

    /// Reads messages at or after `offset`, then returns only the most recent
    /// `limit` entries from that range.
    ///
    /// This is not equivalent to returning the first `limit` messages starting
    /// at `offset`; it preserves recent-message window semantics.
    pub fn read_messages_from(&self, offset: usize, limit: usize) -> Result<Vec<MessageEnvelope>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .messages()
                .from(self.current_agent_id()?.as_deref(), offset, limit);
        }
        read_jsonl_from(&self.messages_path, offset, limit)
    }

    pub fn read_all_messages(&self) -> Result<Vec<MessageEnvelope>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .messages()
                .all(self.current_agent_id()?.as_deref());
        }
        read_recent_jsonl(&self.messages_path, usize::MAX)
    }

    pub fn read_all_message_values(&self) -> Result<Vec<Value>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .messages()
                .all_values(self.current_agent_id()?.as_deref());
        }
        read_recent_jsonl(&self.messages_path, usize::MAX)
    }

    pub fn read_recent_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(take_recent(runtime_db.tasks().latest_all()?, limit));
        }
        read_recent_jsonl(&self.tasks_path, limit)
    }

    pub fn read_recent_work_items(&self, limit: usize) -> Result<Vec<WorkItemRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(take_recent(runtime_db.work_items().latest_all()?, limit));
        }
        read_recent_jsonl(&self.work_items_path, limit)
    }

    pub fn read_recent_delivery_summaries(
        &self,
        limit: usize,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .evidence()
                .recent_delivery_summaries(&self.storage_agent_id()?, limit);
        }
        read_recent_jsonl(&self.delivery_summaries_path, limit)
    }

    pub fn read_recent_work_item_delegations(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkItemDelegationRecord>> {
        read_recent_jsonl(&self.work_item_delegations_path, limit)
    }

    pub fn read_recent_timers(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.timers().recent(limit);
        }
        read_recent_jsonl(&self.timers_path, limit)
    }

    pub fn read_recent_tool_executions(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .evidence()
                .recent_tool_executions(&self.storage_agent_id()?, limit);
        }
        read_recent_jsonl(&self.tools_path, limit)
    }

    pub fn read_recent_turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.turn_records().recent(limit);
        }
        read_recent_jsonl(&self.turns_path, limit)
    }

    pub fn read_recent_transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .transcript_entries()
                .recent(self.current_agent_id()?.as_deref(), limit);
        }
        read_recent_jsonl(&self.transcript_path, limit)
    }

    pub fn read_all_transcript(&self) -> Result<Vec<TranscriptEntry>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .transcript_entries()
                .all(self.current_agent_id()?.as_deref());
        }
        read_recent_jsonl(&self.transcript_path, usize::MAX)
    }

    pub fn read_recent_waiting_intents(&self, limit: usize) -> Result<Vec<WaitingIntentRecord>> {
        read_recent_jsonl(&self.waiting_intents_path, limit)
    }

    pub fn read_recent_wait_conditions(&self, limit: usize) -> Result<Vec<WaitConditionRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.wait_conditions().recent(limit);
        }
        read_recent_jsonl(&self.wait_conditions_path, limit)
    }

    pub fn read_recent_queue_entries(&self, limit: usize) -> Result<Vec<QueueEntryRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .queue_entries()
                .recent(self.current_agent_id()?.as_deref(), limit);
        }
        read_recent_jsonl(&self.queue_entries_path, limit)
    }

    pub fn read_recent_external_triggers(
        &self,
        limit: usize,
    ) -> Result<Vec<ExternalTriggerRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            if let Some(agent_id) = self.current_agent_id()? {
                return runtime_db
                    .external_triggers()
                    .latest_for_agent_limit(&agent_id, limit);
            }
        }
        read_recent_jsonl(&self.external_triggers_path, limit)
    }

    pub fn read_recent_operator_notifications(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorNotificationRecord>> {
        read_recent_jsonl(&self.operator_notifications_path, limit)
    }

    pub fn read_recent_operator_transport_bindings(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorTransportBinding>> {
        read_recent_jsonl(&self.operator_transport_bindings_path, limit)
    }

    pub fn read_recent_operator_delivery_records(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorDeliveryRecord>> {
        read_recent_jsonl(&self.operator_delivery_records_path, limit)
    }

    pub fn read_recent_working_memory_deltas(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkingMemoryDelta>> {
        read_recent_jsonl(&self.working_memory_deltas_path, limit)
    }

    pub fn read_recent_context_episodes(&self, limit: usize) -> Result<Vec<ContextEpisodeRecord>> {
        read_recent_jsonl(&self.context_episodes_path, limit)
    }

    pub fn read_recent_workspace_entries(&self, limit: usize) -> Result<Vec<WorkspaceEntry>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(take_recent(
                runtime_db.workspace_entries().latest_all()?,
                limit,
            ));
        }
        read_recent_jsonl(&self.workspaces_path, limit)
    }

    pub fn read_recent_workspace_occupancies(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkspaceOccupancyRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(take_recent(
                runtime_db.workspace_occupancies().latest_all()?,
                limit,
            ));
        }
        read_recent_jsonl(&self.occupancies_path, limit)
    }

    pub fn read_recent_agent_identities(&self, limit: usize) -> Result<Vec<AgentIdentityRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(take_recent(
                runtime_db.agent_identities().latest_all()?,
                limit,
            ));
        }
        read_recent_jsonl(&self.agent_identities_path, limit)
    }

    pub fn latest_task_records(&self) -> Result<Vec<TaskRecord>> {
        self.latest_task_records_from_recent(usize::MAX)
    }

    pub fn latest_task_records_from_recent(&self, history_limit: usize) -> Result<Vec<TaskRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(take_recent(runtime_db.tasks().latest_all()?, history_limit));
        }
        let records = self.read_recent_tasks(history_limit)?;
        let mut latest = std::collections::BTreeMap::<String, TaskRecord>::new();
        for record in records {
            if let Some(previous) = latest.get(&record.id) {
                let mut merged = record.clone();
                if merged.recovery.is_none() {
                    merged.recovery = previous.recovery.clone();
                }
                latest.insert(record.id.clone(), merged);
            } else {
                latest.insert(record.id.clone(), record);
            }
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_active_task_records(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(take_recent(
                runtime_db
                    .tasks()
                    .latest_all()?
                    .into_iter()
                    .filter(|record| is_active_task_status(&record.status))
                    .collect(),
                limit,
            ));
        }
        if limit == 0 || !self.tasks_path.exists() {
            return Ok(Vec::new());
        }

        let mut seen = std::collections::BTreeSet::<String>::new();
        let mut pending_recovery = std::collections::BTreeMap::<String, usize>::new();
        let mut records = Vec::<TaskRecord>::new();

        scan_jsonl_reverse::<TaskRecord, _>(&self.tasks_path, |record| {
            if let Some(index) = pending_recovery.get(&record.id).copied() {
                if records[index].recovery.is_none() {
                    records[index].recovery = record.recovery.clone();
                }
                if records[index].recovery.is_some() {
                    pending_recovery.remove(&record.id);
                }
            }

            if seen.contains(&record.id) {
                return records.len() < limit || !pending_recovery.is_empty();
            }
            seen.insert(record.id.clone());

            if records.len() < limit && is_active_task_status(&record.status) {
                records.push(record);
                let index = records.len() - 1;
                if records[index].recovery.is_none() {
                    pending_recovery.insert(records[index].id.clone(), index);
                }
            }

            records.len() < limit || !pending_recovery.is_empty()
        })?;

        records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(records)
    }

    pub fn latest_active_task_records_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.tasks().active_for_agent(agent_id, limit);
        }
        if limit == 0 || !self.tasks_path.exists() {
            return Ok(Vec::new());
        }

        let mut seen = std::collections::BTreeSet::<String>::new();
        let mut pending_recovery = std::collections::BTreeMap::<String, usize>::new();
        let mut records = Vec::<TaskRecord>::new();

        scan_jsonl_reverse::<TaskRecord, _>(&self.tasks_path, |record| {
            if let Some(index) = pending_recovery.get(&record.id).copied() {
                if records[index].recovery.is_none() {
                    records[index].recovery = record.recovery.clone();
                }
                if records[index].recovery.is_some() {
                    pending_recovery.remove(&record.id);
                }
            }

            if seen.contains(&record.id) {
                return records.len() < limit || !pending_recovery.is_empty();
            }
            seen.insert(record.id.clone());

            if records.len() < limit
                && record.agent_id == agent_id
                && is_active_task_status(&record.status)
            {
                records.push(record);
                let index = records.len() - 1;
                if records[index].recovery.is_none() {
                    pending_recovery.insert(records[index].id.clone(), index);
                }
            }

            records.len() < limit || !pending_recovery.is_empty()
        })?;

        records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(records)
    }

    pub fn active_task_count_for_agent(&self, agent_id: &str) -> Result<usize> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return Ok(runtime_db
                .tasks()
                .active_for_agent(agent_id, usize::MAX)?
                .len());
        }
        if !self.tasks_path.exists() {
            return Ok(0);
        }

        let mut seen = std::collections::BTreeSet::<String>::new();
        let mut count = 0usize;

        scan_jsonl_reverse::<TaskRecord, _>(&self.tasks_path, |record| {
            if !seen.insert(record.id.clone()) {
                return true;
            }
            if record.agent_id == agent_id && is_active_task_status(&record.status) {
                count += 1;
            }
            true
        })?;

        Ok(count)
    }

    pub fn latest_task_record(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.tasks().latest(task_id);
        }
        if !self.tasks_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.tasks_path)
            .with_context(|| format!("failed to read {}", self.tasks_path.display()))?;
        let mut latest: Option<TaskRecord> = None;
        for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
            let record: TaskRecord = serde_json::from_str(line).with_context(|| {
                format!("failed to decode line from {}", self.tasks_path.display())
            })?;
            if record.id != task_id {
                continue;
            }
            match latest.as_mut() {
                Some(existing) => {
                    if existing.recovery.is_none() {
                        existing.recovery = record.recovery.clone();
                    }
                    if existing.recovery.is_some() {
                        break;
                    }
                }
                None => {
                    latest = Some(record);
                    if latest
                        .as_ref()
                        .and_then(|item| item.recovery.as_ref())
                        .is_some()
                    {
                        break;
                    }
                }
            }
        }

        Ok(latest)
    }

    pub fn latest_work_items(&self) -> Result<Vec<WorkItemRecord>> {
        let records = self.read_recent_work_items(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_work_items_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.work_items().latest_for_agent(agent_id, limit);
        }
        if limit == 0 || !self.work_items_path.exists() {
            return Ok(Vec::new());
        }

        let mut seen = std::collections::BTreeSet::<String>::new();
        let mut records = Vec::<WorkItemRecord>::new();
        scan_jsonl_reverse::<WorkItemRecord, _>(&self.work_items_path, |record| {
            if !seen.insert(record.id.clone()) {
                return records.len() < limit;
            }
            if record.agent_id == agent_id {
                records.push(record);
            }
            records.len() < limit
        })?;
        Ok(records)
    }

    pub fn work_queue_prompt_projection(&self) -> Result<WorkQueuePromptProjection> {
        let db_enabled = self.scheduler_control_plane_db()?.is_some();
        if !db_enabled && !self.work_items_path.exists() {
            return Ok(WorkQueuePromptProjection::default());
        }
        let current_work_item_id = self
            .read_agent()?
            .and_then(|agent| agent.current_work_item_id);
        let mut latest = std::collections::HashMap::<String, WorkItemRecord>::new();
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            if let Some(agent_id) = self.current_agent_id()? {
                for record in runtime_db
                    .work_items()
                    .latest_for_agent(&agent_id, usize::MAX)?
                {
                    latest.insert(record.id.clone(), record);
                }
            }
        } else {
            let content = fs::read_to_string(&self.work_items_path)
                .with_context(|| format!("failed to read {}", self.work_items_path.display()))?;
            for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
                let record: WorkItemRecord = serde_json::from_str(line).with_context(|| {
                    format!(
                        "failed to decode line from {}",
                        self.work_items_path.display()
                    )
                })?;
                latest.entry(record.id.clone()).or_insert(record);
            }
        }

        let current = current_work_item_id
            .as_deref()
            .and_then(|id| latest.get(id))
            .filter(|item| item.state == WorkItemState::Open)
            .cloned();

        let active_waits = self
            .latest_waiting_intents()?
            .into_iter()
            .filter(|intent| intent.status == WaitingIntentStatus::Active)
            .filter(|intent| intent.scope == WaitingIntentScope::WorkItem)
            .filter_map(|intent| intent.work_item_id.map(|id| (id, intent.last_triggered_at)))
            .fold(
                std::collections::BTreeMap::<String, Option<DateTime<Utc>>>::new(),
                |mut acc, (id, triggered_at)| {
                    let slot = acc.entry(id).or_insert(None);
                    *slot = match (*slot, triggered_at) {
                        (Some(left), Some(right)) => Some(left.max(right)),
                        (None, Some(right)) => Some(right),
                        (existing, None) => existing,
                    };
                    acc
                },
            );
        let active_wait_conditions = self
            .latest_wait_conditions()?
            .into_iter()
            .filter(|condition| condition.status == WaitConditionStatus::Active)
            .filter_map(|condition| condition.work_item_id.map(|id| (id, condition.kind)))
            .fold(
                std::collections::BTreeMap::<String, ActiveWaitConditionStates>::new(),
                |mut acc, (id, kind)| {
                    acc.entry(id).or_default().record(kind);
                    acc
                },
            );
        let active_task_waits = self
            .latest_active_task_records(usize::MAX)?
            .into_iter()
            .filter(|task| task.is_blocking())
            .filter_map(|task| task.effective_work_item_id().map(str::to_string))
            .collect::<std::collections::BTreeSet<_>>();
        let mut readiness = latest
            .values()
            .cloned()
            .map(|item| {
                let is_current = current_work_item_id.as_deref() == Some(item.id.as_str())
                    && item.state == WorkItemState::Open;
                let last_triggered_at = active_waits.get(&item.id).copied().flatten();
                let wait_condition = active_wait_conditions.get(&item.id);
                let wait_condition_state =
                    wait_condition.and_then(ActiveWaitConditionStates::scheduling_state);
                let has_active_waits =
                    active_waits.contains_key(&item.id) || wait_condition_state.is_some();
                let has_active_task_waits = active_task_waits.contains(&item.id)
                    || wait_condition.is_some_and(|states| states.task);
                let has_triggered_waits = last_triggered_at.is_some();
                let scheduling_state = item.scheduling_state(if has_active_task_waits {
                    Some(WorkItemSchedulingState::WaitingTask)
                } else {
                    wait_condition_state.or_else(|| {
                        active_waits
                            .contains_key(&item.id)
                            .then_some(WorkItemSchedulingState::WaitingExternal)
                    })
                });
                let readiness = readiness_for_scheduling_state(scheduling_state);
                let candidate_class =
                    if is_current && scheduling_state == WorkItemSchedulingState::Runnable {
                        WorkItemCandidateClass::CurrentRunnable
                    } else if item.state == WorkItemState::Completed {
                        WorkItemCandidateClass::CompletedRecent
                    } else if has_triggered_waits && item.blocked_by.is_some() {
                        WorkItemCandidateClass::TriggeredBlocked
                    } else if scheduling_state == WorkItemSchedulingState::Runnable {
                        WorkItemCandidateClass::QueuedRunnable
                    } else if scheduling_state == WorkItemSchedulingState::WaitingOperator {
                        WorkItemCandidateClass::WaitingForOperator
                    } else {
                        WorkItemCandidateClass::Blocked
                    };
                WorkItemReadinessProjection {
                    current_todo: current_todo(&item),
                    work_item: item,
                    scheduling_state,
                    readiness,
                    candidate_class,
                    is_current,
                    has_active_waits,
                    has_active_task_waits,
                    has_triggered_waits,
                    last_triggered_at,
                }
            })
            .collect::<Vec<_>>();

        readiness.sort_by(compare_readiness_projection_order);
        let current_runnable = readiness
            .iter()
            .find(|item| item.candidate_class == WorkItemCandidateClass::CurrentRunnable)
            .cloned();
        let triggered_blocked = readiness
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::TriggeredBlocked)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        let queued_runnable = readiness
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::QueuedRunnable)
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        let waiting_for_operator = readiness
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::WaitingForOperator)
            .cloned()
            .collect::<Vec<_>>();
        let blocked = readiness
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::Blocked)
            .filter(|item| item.scheduling_state != WorkItemSchedulingState::WaitingTask)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        let completed_recent = readiness
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::CompletedRecent)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();

        let mut queued_blocked = latest
            .values()
            .filter(|item| {
                item.state == WorkItemState::Open
                    && Some(item.id.as_str()) != current_work_item_id.as_deref()
                    && !active_task_waits.contains(&item.id)
                    && !active_wait_conditions
                        .get(&item.id)
                        .is_some_and(|states| states.task)
            })
            .cloned()
            .collect::<Vec<_>>();
        queued_blocked.sort_by(compare_queue_display_order);

        Ok(WorkQueuePromptProjection {
            current,
            queued_blocked,
            readiness,
            current_runnable,
            triggered_blocked,
            queued_runnable,
            waiting_for_operator,
            blocked,
            completed_recent,
        })
    }

    pub fn agent_posture_projection(&self, agent: &AgentState) -> Result<AgentPostureProjection> {
        if matches!(agent.status, AgentStatus::Stopped) {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::Archived,
                reason: "agent lifecycle is stopped".into(),
                work_item_id: None,
                waiting_intent_id: None,
                task_id: None,
                run_id: None,
            });
        }

        if let Some(run_id) = agent.current_run_id.clone() {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::ActiveTurn,
                reason: "agent has an active turn".into(),
                work_item_id: agent.current_turn_work_item_id.clone(),
                waiting_intent_id: None,
                task_id: None,
                run_id: Some(run_id),
            });
        }

        if self
            .latest_queue_entries()?
            .iter()
            .any(|entry| entry.agent_id == agent.id && entry.status == QueueEntryStatus::Queued)
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::HasQueuedInput,
                reason: "agent has queued input".into(),
                work_item_id: None,
                waiting_intent_id: None,
                task_id: None,
                run_id: None,
            });
        }

        let work_queue = self.work_queue_prompt_projection()?;
        if let Some(item) = work_queue
            .current_runnable
            .as_ref()
            .or_else(|| work_queue.queued_runnable.first())
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::HasRunnableWork,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                waiting_intent_id: None,
                task_id: None,
                run_id: None,
            });
        }

        if let Some(item) = work_queue
            .readiness
            .iter()
            .find(|item| item.scheduling_state == WorkItemSchedulingState::WaitingTask)
        {
            let task_id = self
                .latest_wait_conditions()?
                .into_iter()
                .find(|condition| {
                    condition.status == WaitConditionStatus::Active
                        && condition.kind == WaitConditionKind::Task
                        && condition.work_item_id.as_deref() == Some(item.work_item.id.as_str())
                })
                .and_then(|condition| condition.subject_ref);
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::WaitingForTask,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                waiting_intent_id: None,
                task_id,
                run_id: None,
            });
        }

        if let Some(item) = work_queue
            .readiness
            .iter()
            .find(|item| item.scheduling_state == WorkItemSchedulingState::WaitingExternal)
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::WaitingForExternal,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                waiting_intent_id: self
                    .latest_waiting_intents()?
                    .into_iter()
                    .find(|intent| {
                        intent.status == WaitingIntentStatus::Active
                            && intent.scope == WaitingIntentScope::WorkItem
                            && intent.work_item_id.as_deref() == Some(item.work_item.id.as_str())
                    })
                    .map(|intent| intent.id),
                task_id: None,
                run_id: None,
            });
        }

        if let Some(item) = work_queue.waiting_for_operator.first() {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::WaitingForOperator,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                waiting_intent_id: None,
                task_id: None,
                run_id: None,
            });
        }

        if let Some(item) = work_queue
            .blocked
            .first()
            .or_else(|| work_queue.triggered_blocked.first())
        {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::Blocked,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
                waiting_intent_id: None,
                task_id: None,
                run_id: None,
            });
        }

        Ok(AgentPostureProjection {
            posture: AgentSchedulingPosture::Idle,
            reason: "no queued input, active turn, runnable work, or active waits".into(),
            work_item_id: None,
            waiting_intent_id: None,
            task_id: None,
            run_id: None,
        })
    }

    pub fn waiting_contract_anchor(&self) -> Result<Option<WorkItemRecord>> {
        let projection = self.work_queue_prompt_projection()?;
        Ok(projection.current.or_else(|| {
            projection
                .queued_blocked
                .into_iter()
                .filter(|item| item.blocked_by.is_some())
                .max_by(|left, right| {
                    left.updated_at
                        .cmp(&right.updated_at)
                        .then_with(|| left.created_at.cmp(&right.created_at))
                        .then_with(|| left.id.cmp(&right.id))
                })
        }))
    }

    pub fn due_blocked_work_item_rechecks(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<WorkItemRecord>> {
        let mut due = self
            .latest_work_items()?
            .into_iter()
            .filter(|item| item.agent_id == agent_id)
            .filter(|item| item.state == WorkItemState::Open)
            .filter(|item| item.blocked_by.is_some())
            .filter(|item| item.recheck_at.is_some_and(|recheck_at| recheck_at <= now))
            .filter(|item| {
                item.recheck_consumed_at
                    .zip(item.recheck_at)
                    .is_none_or(|(consumed_at, recheck_at)| consumed_at < recheck_at)
            })
            .collect::<Vec<_>>();
        due.sort_by(|left, right| {
            left.recheck_at
                .cmp(&right.recheck_at)
                .then_with(|| compare_queue_display_order(left, right))
        });
        Ok(due)
    }

    pub fn next_blocked_work_item_recheck_at(
        &self,
        agent_id: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .latest_work_items()?
            .into_iter()
            .filter(|item| item.agent_id == agent_id)
            .filter(|item| item.state == WorkItemState::Open)
            .filter(|item| item.blocked_by.is_some())
            .filter_map(|item| {
                let recheck_at = item.recheck_at?;
                if item
                    .recheck_consumed_at
                    .is_some_and(|consumed_at| consumed_at >= recheck_at)
                {
                    None
                } else {
                    Some(recheck_at)
                }
            })
            .min())
    }

    pub fn latest_work_item(&self, work_item_id: &str) -> Result<Option<WorkItemRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.work_items().latest(work_item_id);
        }
        if !self.work_items_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.work_items_path)
            .with_context(|| format!("failed to read {}", self.work_items_path.display()))?;
        for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
            let record: WorkItemRecord = serde_json::from_str(line).with_context(|| {
                format!(
                    "failed to decode line from {}",
                    self.work_items_path.display()
                )
            })?;
            if record.id == work_item_id {
                return Ok(Some(record));
            }
        }

        Ok(None)
    }

    pub fn latest_delivery_summary(
        &self,
        work_item_id: &str,
    ) -> Result<Option<DeliverySummaryRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .evidence()
                .latest_delivery_summary(&self.storage_agent_id()?, work_item_id);
        }
        if !self.delivery_summaries_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.delivery_summaries_path).with_context(|| {
            format!("failed to read {}", self.delivery_summaries_path.display())
        })?;
        for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
            let record: DeliverySummaryRecord = serde_json::from_str(line).with_context(|| {
                format!(
                    "failed to decode line from {}",
                    self.delivery_summaries_path.display()
                )
            })?;
            if record.work_item_id == work_item_id {
                return Ok(Some(record));
            }
        }

        Ok(None)
    }

    pub fn latest_work_item_delegations(&self) -> Result<Vec<WorkItemDelegationRecord>> {
        let records = self.read_recent_work_item_delegations(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.delegation_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn open_work_item_delegation_for_child(
        &self,
        child_agent_id: &str,
    ) -> Result<Option<WorkItemDelegationRecord>> {
        Ok(self
            .latest_work_item_delegations()?
            .into_iter()
            .filter(|record| {
                record.child_agent_id == child_agent_id
                    && record.state == WorkItemDelegationState::Open
            })
            .max_by(|left, right| left.updated_at.cmp(&right.updated_at)))
    }

    pub fn latest_work_item_delegation_for_child(
        &self,
        child_agent_id: &str,
    ) -> Result<Option<WorkItemDelegationRecord>> {
        read_latest_jsonl_matching(
            &self.work_item_delegations_path,
            |record: &WorkItemDelegationRecord| record.child_agent_id == child_agent_id,
        )
    }

    pub fn latest_timer_records(&self) -> Result<Vec<TimerRecord>> {
        let records = self.read_recent_timers(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_timer_record(&self, timer_id: &str) -> Result<Option<TimerRecord>> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db.timers().latest(timer_id);
        }
        read_latest_jsonl_matching(&self.timers_path, |record: &TimerRecord| {
            record.id == timer_id
        })
    }

    pub fn latest_waiting_intents(&self) -> Result<Vec<WaitingIntentRecord>> {
        let records = self.read_recent_waiting_intents(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_wait_conditions(&self) -> Result<Vec<WaitConditionRecord>> {
        let records = self.read_recent_wait_conditions(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_active_wait_conditions_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<WaitConditionRecord>> {
        Ok(self
            .latest_wait_conditions()?
            .into_iter()
            .filter(|record| record.agent_id == agent_id)
            .filter(|record| record.status == WaitConditionStatus::Active)
            .collect())
    }

    pub fn latest_active_wait_conditions_for_work_item(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<Vec<WaitConditionRecord>> {
        Ok(self
            .latest_active_wait_conditions_for_agent(agent_id)?
            .into_iter()
            .filter(|record| record.work_item_id.as_deref() == Some(work_item_id))
            .collect())
    }

    pub fn latest_waiting_intent(
        &self,
        agent_id: &str,
        waiting_intent_id: &str,
    ) -> Result<Option<WaitingIntentRecord>> {
        read_latest_jsonl_matching(
            &self.waiting_intents_path,
            |record: &WaitingIntentRecord| {
                record.agent_id == agent_id && record.id == waiting_intent_id
            },
        )
    }

    pub fn latest_external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        let records = self.read_recent_external_triggers(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.external_trigger_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_operator_transport_bindings(&self) -> Result<Vec<OperatorTransportBinding>> {
        let records = self.read_recent_operator_transport_bindings(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.binding_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_operator_delivery_records(&self) -> Result<Vec<OperatorDeliveryRecord>> {
        let records = self.read_recent_operator_delivery_records(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.delivery_intent_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_workspace_entries(&self) -> Result<Vec<WorkspaceEntry>> {
        let records = self.read_recent_workspace_entries(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.workspace_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_workspace_occupancies(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        let records = self.read_recent_workspace_occupancies(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.occupancy_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_agent_identities(&self) -> Result<Vec<AgentIdentityRecord>> {
        let records = self.read_recent_agent_identities(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.agent_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_queue_entries(&self) -> Result<Vec<QueueEntryRecord>> {
        let records = self.read_recent_queue_entries(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.message_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn recovery_snapshot(&self, agent_id: &str) -> Result<RecoverySnapshot> {
        let agent = self.read_agent()?;
        let mut messages_by_id = std::collections::BTreeMap::new();
        for message in self.read_all_messages()? {
            messages_by_id.insert(message.id.clone(), message);
        }

        let mut replay_messages = self
            .latest_queue_entries()?
            .into_iter()
            .filter_map(|entry| match entry.status {
                crate::types::QueueEntryStatus::Queued
                | crate::types::QueueEntryStatus::Dequeued => {
                    messages_by_id.get(&entry.message_id).cloned()
                }
                crate::types::QueueEntryStatus::Processed
                | crate::types::QueueEntryStatus::Interjected
                | crate::types::QueueEntryStatus::Aborted
                | crate::types::QueueEntryStatus::Dropped => None,
            })
            .collect::<Vec<_>>();
        replay_messages.sort_by(|left, right| match (left.message_seq, right.message_seq) {
            (Some(left_seq), Some(right_seq)) => left_seq
                .cmp(&right_seq)
                .then_with(|| left.created_at.cmp(&right.created_at)),
            _ => left.created_at.cmp(&right.created_at),
        });

        let active_tasks = self.latest_active_task_records_for_agent(agent_id, usize::MAX)?;
        let active_timers = self
            .latest_timer_records()?
            .into_iter()
            .filter(|record| {
                record.agent_id == agent_id && record.status == crate::types::TimerStatus::Active
            })
            .collect();
        let work_items = self.latest_work_items()?;
        let work_item_delegations = self.latest_work_item_delegations()?;

        Ok(RecoverySnapshot {
            agent,
            replay_messages,
            active_tasks,
            active_timers,
            work_items,
            work_item_delegations,
        })
    }

    pub fn count_briefs(&self) -> Result<usize> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .evidence()
                .count_briefs(&self.storage_agent_id()?);
        }
        if !self.briefs_path.exists() {
            return Ok(0);
        }
        let content = fs::read_to_string(&self.briefs_path)?;
        Ok(content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count())
    }

    pub fn count_messages(&self) -> Result<usize> {
        if let Some(runtime_db) = self.scheduler_control_plane_db()? {
            return runtime_db
                .messages()
                .count(self.current_agent_id()?.as_deref());
        }
        if !self.messages_path.exists() {
            return Ok(0);
        }
        let content = fs::read_to_string(&self.messages_path)?;
        Ok(content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count())
    }
}

fn memory_index_agent_key(agent_id: &str) -> String {
    agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn migrate_events_ledger(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if let Some(seq) = read_tail_event_seq(path)? {
        return Ok(seq);
    }

    let timestamp = Utc::now().format("%Y%m%d%H%M%S%3f");
    let tmp_path = path.with_file_name(format!(".events.jsonl.{timestamp}.tmp"));
    let file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut tmp = fs::File::create(&tmp_path)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    let mut max_seq = 0;
    let mut changed = false;

    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_str(&line)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("event ledger line is not a JSON object"))?;
        match object.get("event_seq").and_then(Value::as_u64) {
            Some(seq) if seq > max_seq => {
                max_seq = seq;
            }
            Some(seq) => {
                anyhow::bail!(
                    "event ledger sequence must be strictly increasing; found {seq} after {max_seq}"
                );
            }
            None => {
                max_seq += 1;
                object.insert("event_seq".to_string(), Value::from(max_seq));
                changed = true;
            }
        }
        writeln!(tmp, "{}", serde_json::to_string(&value)?)?;
    }

    if !changed {
        let _ = fs::remove_file(&tmp_path);
        return Ok(max_seq);
    }

    let backup_path = path.with_file_name(format!("events.jsonl.bak.{timestamp}"));
    fs::copy(path, &backup_path).with_context(|| {
        format!(
            "failed to back up {} to {}",
            path.display(),
            backup_path.display()
        )
    })?;
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to replace {} with {}",
            path.display(),
            tmp_path.display()
        )
    })?;
    Ok(max_seq)
}

fn read_tail_event_seq(path: &Path) -> Result<Option<u64>> {
    let Some(value) = read_latest_jsonl_matching::<Value, _>(path, |_| true)? else {
        return Ok(Some(0));
    };
    Ok(value.get("event_seq").and_then(Value::as_u64))
}

fn max_jsonl_u64_field(path: &Path, field: &str) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut max_value = None;
    scan_jsonl_reverse::<Value, _>(path, |value| {
        if let Some(sequence) = value.get(field).and_then(Value::as_u64) {
            max_value = Some(sequence);
            return false;
        }
        true
    })?;
    Ok(max_value.unwrap_or(0))
}

fn jsonl_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let line = serde_json::to_string(value)?;
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    Ok(bytes)
}

fn append_jsonl_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

pub(crate) fn is_active_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    )
}

fn read_recent_jsonl<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<Vec<T>> {
    if !path.exists() || limit == 0 {
        return Ok(Vec::new());
    }

    let file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut recent = VecDeque::with_capacity(limit.min(1024));
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        if recent.len() == limit {
            recent.pop_front();
        }
        recent.push_back(line);
    }
    recent
        .into_iter()
        .map(|line| serde_json::from_str::<T>(&line).map_err(Into::into))
        .collect()
}

fn take_recent<T>(mut records_desc: Vec<T>, limit: usize) -> Vec<T> {
    if limit == 0 {
        return Vec::new();
    }
    if records_desc.len() > limit {
        records_desc.truncate(limit);
    }
    records_desc.reverse();
    records_desc
}

fn read_jsonl_from<T: DeserializeOwned>(
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut lines = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .skip(offset)
        .map(|line| serde_json::from_str::<T>(line))
        .collect::<Result<Vec<_>, _>>()?;

    if lines.len() > limit {
        lines.drain(0..(lines.len() - limit));
    }
    Ok(lines)
}

fn read_latest_jsonl_matching<T, F>(path: &Path, mut matches: F) -> Result<Option<T>>
where
    T: DeserializeOwned,
    F: FnMut(&T) -> bool,
{
    if !path.exists() {
        return Ok(None);
    }

    const CHUNK_SIZE: u64 = 8192;
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut cursor = file.seek(SeekFrom::End(0))?;
    let mut prefix = Vec::new();

    while cursor > 0 {
        let read_len = cursor.min(CHUNK_SIZE);
        cursor -= read_len;
        file.seek(SeekFrom::Start(cursor))?;

        let mut chunk = vec![0; read_len as usize];
        file.read_exact(&mut chunk)
            .with_context(|| format!("failed to read {}", path.display()))?;
        chunk.extend_from_slice(&prefix);

        let mut line_end = chunk.len();
        for idx in (0..chunk.len()).rev() {
            if chunk[idx] != b'\n' {
                continue;
            }
            if let Some(record) =
                parse_jsonl_match(&chunk[(idx + 1)..line_end], path, &mut matches)?
            {
                return Ok(Some(record));
            }
            line_end = idx;
        }
        prefix = chunk[..line_end].to_vec();
    }

    parse_jsonl_match(&prefix, path, &mut matches)
}

fn wait_condition_from_waiting_intent(record: &WaitingIntentRecord) -> WaitConditionRecord {
    let updated_at = record
        .cancelled_at
        .or(record.last_triggered_at)
        .unwrap_or(record.created_at);
    WaitConditionRecord {
        id: format!("waiting_intent:{}", record.id),
        agent_id: record.agent_id.clone(),
        work_item_id: record.work_item_id.clone(),
        status: match record.status {
            WaitingIntentStatus::Active => WaitConditionStatus::Active,
            WaitingIntentStatus::Cancelled => WaitConditionStatus::Cancelled,
        },
        kind: WaitConditionKind::External,
        source: Some(record.source.clone()),
        subject_ref: record.resource.clone(),
        waiting_for: record
            .condition
            .clone()
            .unwrap_or_else(|| record.description.clone()),
        wake_sources: vec![WakeSource::ExternalIngress {
            external_trigger_id: Some(record.external_trigger_id.clone()),
        }],
        continuation: Some(serde_json::json!({
            "waiting_intent_id": record.id,
            "external_trigger_id": record.external_trigger_id,
            "scope": record.scope,
            "delivery_mode": record.delivery_mode,
        })),
        created_at: record.created_at,
        updated_at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: record.cancelled_at,
        turn_id: None,
    }
}

fn external_wait_recoverability_event(record: &WaitConditionRecord) -> Option<AuditEvent> {
    match record.external_recoverability()? {
        ExternalWaitRecoverability::Weak => Some(AuditEvent::new(
            "external_wait_without_recovery",
            serde_json::json!({
                "wait_condition_id": record.id,
                "work_item_id": record.work_item_id,
                "source": record.source,
                "subject_ref": record.subject_ref,
                "waiting_for": record.waiting_for,
                "external_recoverability": "weak",
                "wake_sources": record.wake_sources,
            }),
        )),
        ExternalWaitRecoverability::ExplicitNoFallback => Some(AuditEvent::new(
            "external_wait_without_recovery",
            serde_json::json!({
                "wait_condition_id": record.id,
                "work_item_id": record.work_item_id,
                "source": record.source,
                "subject_ref": record.subject_ref,
                "waiting_for": record.waiting_for,
                "external_recoverability": "explicit_no_fallback",
                "no_fallback_reason": record.no_fallback_reason(),
                "wake_sources": record.wake_sources,
            }),
        )),
        ExternalWaitRecoverability::Recoverable => None,
    }
}

fn scan_jsonl_reverse<T, F>(path: &Path, mut visit: F) -> Result<()>
where
    T: DeserializeOwned,
    F: FnMut(T) -> bool,
{
    if !path.exists() {
        return Ok(());
    }

    const CHUNK_SIZE: u64 = 8192;
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut cursor = file.seek(SeekFrom::End(0))?;
    let mut prefix = Vec::new();

    while cursor > 0 {
        let read_len = cursor.min(CHUNK_SIZE);
        cursor -= read_len;
        file.seek(SeekFrom::Start(cursor))?;

        let mut chunk = vec![0; read_len as usize];
        file.read_exact(&mut chunk)
            .with_context(|| format!("failed to read {}", path.display()))?;
        chunk.extend_from_slice(&prefix);

        let mut line_end = chunk.len();
        for idx in (0..chunk.len()).rev() {
            if chunk[idx] != b'\n' {
                continue;
            }
            if !parse_jsonl_visit(&chunk[(idx + 1)..line_end], path, &mut visit)? {
                return Ok(());
            }
            line_end = idx;
        }
        prefix = chunk[..line_end].to_vec();
    }

    let _ = parse_jsonl_visit(&prefix, path, &mut visit)?;
    Ok(())
}

fn parse_jsonl_visit<T, F>(line: &[u8], path: &Path, visit: &mut F) -> Result<bool>
where
    T: DeserializeOwned,
    F: FnMut(T) -> bool,
{
    let line = std::str::from_utf8(line)
        .with_context(|| format!("failed to decode UTF-8 from {}", path.display()))?;
    if line.trim().is_empty() {
        return Ok(true);
    }
    let record: T = serde_json::from_str(line)
        .with_context(|| format!("failed to decode line from {}", path.display()))?;
    Ok(visit(record))
}

fn parse_jsonl_match<T, F>(line: &[u8], path: &Path, matches: &mut F) -> Result<Option<T>>
where
    T: DeserializeOwned,
    F: FnMut(&T) -> bool,
{
    let line = std::str::from_utf8(line)
        .with_context(|| format!("failed to decode UTF-8 from {}", path.display()))?;
    if line.trim().is_empty() {
        return Ok(None);
    }
    let record: T = serde_json::from_str(line)
        .with_context(|| format!("failed to decode line from {}", path.display()))?;
    if matches(&record) {
        Ok(Some(record))
    } else {
        Ok(None)
    }
}

fn compare_readiness_projection_order(
    left: &WorkItemReadinessProjection,
    right: &WorkItemReadinessProjection,
) -> std::cmp::Ordering {
    candidate_class_rank(left.candidate_class)
        .cmp(&candidate_class_rank(right.candidate_class))
        .then_with(|| match left.candidate_class {
            WorkItemCandidateClass::TriggeredBlocked => {
                compare_timestamp_desc_option(left.last_triggered_at, right.last_triggered_at)
                    .then_with(|| {
                        compare_timestamp_desc(
                            left.work_item.updated_at,
                            right.work_item.updated_at,
                        )
                    })
            }
            WorkItemCandidateClass::QueuedRunnable => {
                compare_timestamp_asc(left.work_item.updated_at, right.work_item.updated_at)
                    .then_with(|| {
                        compare_timestamp_asc(left.work_item.created_at, right.work_item.created_at)
                    })
            }
            WorkItemCandidateClass::WaitingForOperator
            | WorkItemCandidateClass::Blocked
            | WorkItemCandidateClass::CompletedRecent => {
                compare_timestamp_desc(left.work_item.updated_at, right.work_item.updated_at)
            }
            WorkItemCandidateClass::CurrentRunnable => std::cmp::Ordering::Equal,
        })
        .then_with(|| left.work_item.id.cmp(&right.work_item.id))
}

fn compare_queue_display_order(
    left: &WorkItemRecord,
    right: &WorkItemRecord,
) -> std::cmp::Ordering {
    blocked_rank(left)
        .cmp(&blocked_rank(right))
        .then_with(|| compare_timestamp_asc(left.created_at, right.created_at))
        .then_with(|| compare_timestamp_asc(left.updated_at, right.updated_at))
        .then_with(|| left.id.cmp(&right.id))
}

fn blocked_rank(record: &WorkItemRecord) -> u8 {
    u8::from(record.blocked_by.is_some())
}

fn readiness_for_scheduling_state(state: WorkItemSchedulingState) -> WorkItemReadiness {
    match state {
        WorkItemSchedulingState::Runnable => WorkItemReadiness::Runnable,
        WorkItemSchedulingState::WaitingOperator => WorkItemReadiness::WaitingForOperator,
        WorkItemSchedulingState::WaitingTask
        | WorkItemSchedulingState::WaitingExternal
        | WorkItemSchedulingState::WaitingTimer
        | WorkItemSchedulingState::WaitingSystem
        | WorkItemSchedulingState::Blocked => WorkItemReadiness::Blocked,
        WorkItemSchedulingState::Completed => WorkItemReadiness::Completed,
    }
}

fn candidate_class_rank(class: WorkItemCandidateClass) -> u8 {
    match class {
        WorkItemCandidateClass::CurrentRunnable => 0,
        WorkItemCandidateClass::TriggeredBlocked => 1,
        WorkItemCandidateClass::QueuedRunnable => 2,
        WorkItemCandidateClass::WaitingForOperator => 3,
        WorkItemCandidateClass::Blocked => 4,
        WorkItemCandidateClass::CompletedRecent => 5,
    }
}

fn compare_timestamp_asc(left: DateTime<Utc>, right: DateTime<Utc>) -> std::cmp::Ordering {
    left.cmp(&right)
}

fn compare_timestamp_desc(left: DateTime<Utc>, right: DateTime<Utc>) -> std::cmp::Ordering {
    right.cmp(&left)
}

fn compare_timestamp_desc_option(
    left: Option<DateTime<Utc>>,
    right: Option<DateTime<Utc>>,
) -> std::cmp::Ordering {
    right.cmp(&left)
}

fn current_todo(record: &WorkItemRecord) -> Option<TodoItem> {
    record
        .todo_list
        .iter()
        .find(|item| item.state == TodoItemState::InProgress)
        .or_else(|| {
            record
                .todo_list
                .iter()
                .find(|item| item.state == TodoItemState::Pending)
        })
        .cloned()
}

fn file_activity_marker(path: &Path) -> Result<FileActivityMarker> {
    if !path.exists() {
        return Ok(FileActivityMarker {
            exists: false,
            len: 0,
            modified_unix_ms: 0,
        });
    }

    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);

    Ok(FileActivityMarker {
        exists: true,
        len: metadata.len(),
        modified_unix_ms,
    })
}

pub fn to_json_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use chrono::Utc;

    use crate::types::{
        AgentState, AgentStatus, AuthorityClass, BriefKind, CallbackDeliveryMode,
        EpisodeBoundaryReason, ExternalTriggerScope, ExternalTriggerStatus, MessageBody,
        MessageEnvelope, MessageKind, MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus,
        TaskKind, TaskRecord, TaskRecoverySpec, TaskStatus, TodoItem, TodoItemState,
        ToolExecutionStatus, TranscriptEntry, TranscriptEntryKind, WorkItemPlanStatus,
        WorkItemState,
    };

    use super::*;

    #[test]
    fn append_event_assigns_monotonic_event_seq() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        storage
            .append_event(&AuditEvent::new(
                "test_event",
                serde_json::json!({ "n": 1 }),
            ))
            .unwrap();
        storage
            .append_event(&AuditEvent::new(
                "test_event",
                serde_json::json!({ "n": 2 }),
            ))
            .unwrap();

        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_seq)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let reopened = AppStorage::new(dir.path()).unwrap();
        reopened
            .append_event(&AuditEvent::new(
                "test_event",
                serde_json::json!({ "n": 3 }),
            ))
            .unwrap();
        let events = reopened.read_recent_events(10).unwrap();
        assert_eq!(events.last().map(|event| event.event_seq), Some(3));
    }

    #[test]
    fn storage_indexes_live_audit_events_when_sink_is_enabled() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        runtime_db
            .audit_events()
            .import_legacy(
                Some("agent-test"),
                storage.read_recent_events(usize::MAX).unwrap(),
            )
            .unwrap();
        storage
            .enable_audit_event_index(runtime_db.clone(), Some("agent-test".to_string()))
            .unwrap();

        storage
            .append_event(&AuditEvent::new(
                "live_event",
                serde_json::json!({ "source": "storage" }),
            ))
            .unwrap();

        let indexed = runtime_db
            .audit_events()
            .page_after(Some("agent-test"), 0, 10)
            .unwrap();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0].kind, "live_event");
        assert_eq!(indexed[0].event_seq, 1);
    }

    #[test]
    fn audit_events_use_runtime_db_after_cutover_without_live_jsonl_append() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("agent-test")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        runtime_db
            .audit_events()
            .import_legacy(Some("agent-test"), Vec::new())
            .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();
        storage
            .enable_audit_event_index(runtime_db.clone(), Some("agent-test".to_string()))
            .unwrap();

        let legacy_len_before = std::fs::metadata(&storage.events_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        storage
            .append_event(&AuditEvent::new(
                "db_canonical_event",
                serde_json::json!({ "source": "runtime_db" }),
            ))
            .unwrap();

        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "db_canonical_event");
        assert_eq!(events[0].event_seq, 1);
        let legacy_len_after = std::fs::metadata(&storage.events_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        assert_eq!(legacy_len_after, legacy_len_before);
    }

    #[test]
    fn audit_events_use_runtime_db_after_cutover_before_sink_is_enabled() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("agent-test")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        runtime_db
            .audit_events()
            .import_legacy(Some("agent-test"), Vec::new())
            .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();

        let legacy_len_before = std::fs::metadata(&storage.events_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        storage
            .append_event(&AuditEvent::new(
                "db_canonical_bootstrap_event",
                serde_json::json!({ "source": "bootstrap_gap" }),
            ))
            .unwrap();

        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "db_canonical_bootstrap_event");
        assert_eq!(events[0].event_seq, 1);
        let legacy_len_after = std::fs::metadata(&storage.events_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        assert_eq!(legacy_len_after, legacy_len_before);
    }

    #[test]
    fn scheduler_control_plane_storage_uses_runtime_db_after_cutover() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        runtime_db
            .wait_conditions()
            .import_legacy(Vec::new())
            .unwrap();
        runtime_db
            .queue_entries()
            .import_legacy(Vec::new())
            .unwrap();
        runtime_db.timers().import_legacy(Vec::new()).unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();

        let now = Utc::now();
        let wait_condition = WaitConditionRecord {
            id: "wait-1".into(),
            agent_id: "default".into(),
            work_item_id: Some("work-1".into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("github".into()),
            subject_ref: Some("pr-1".into()),
            waiting_for: "checks passed".into(),
            wake_sources: vec![WakeSource::ExternalIngress {
                external_trigger_id: Some("trigger-1".into()),
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
        };
        let queue_entry = QueueEntryRecord {
            message_id: "msg-1".into(),
            agent_id: "default".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let timer = TimerRecord {
            id: "timer-1".into(),
            agent_id: "default".into(),
            created_at: now,
            duration_ms: 1000,
            interval_ms: None,
            repeat: false,
            status: crate::types::TimerStatus::Active,
            summary: Some("wake later".into()),
            next_fire_at: Some(now + chrono::Duration::seconds(1)),
            last_fired_at: None,
            fire_count: 0,
        };

        storage.append_wait_condition(&wait_condition).unwrap();
        storage.append_queue_entry(&queue_entry).unwrap();
        storage.append_timer(&timer).unwrap();

        let mut later_wait_condition = wait_condition.clone();
        later_wait_condition.id = "wait-2".into();
        later_wait_condition.updated_at = now + chrono::Duration::seconds(1);
        let mut latest_wait_condition = wait_condition.clone();
        latest_wait_condition.id = "wait-3".into();
        latest_wait_condition.updated_at = now + chrono::Duration::seconds(2);
        storage
            .append_wait_condition(&later_wait_condition)
            .unwrap();
        storage
            .append_wait_condition(&latest_wait_condition)
            .unwrap();

        let mut later_queue_entry = queue_entry.clone();
        later_queue_entry.message_id = "msg-2".into();
        later_queue_entry.updated_at = now + chrono::Duration::seconds(1);
        let mut latest_queue_entry = queue_entry.clone();
        latest_queue_entry.message_id = "msg-3".into();
        latest_queue_entry.updated_at = now + chrono::Duration::seconds(2);
        storage.append_queue_entry(&later_queue_entry).unwrap();
        storage.append_queue_entry(&latest_queue_entry).unwrap();

        let mut later_timer = timer.clone();
        later_timer.id = "timer-2".into();
        later_timer.next_fire_at = Some(now + chrono::Duration::seconds(2));
        let mut latest_timer = timer.clone();
        latest_timer.id = "timer-3".into();
        latest_timer.next_fire_at = Some(now + chrono::Duration::seconds(3));
        storage.append_timer(&later_timer).unwrap();
        storage.append_timer(&latest_timer).unwrap();

        assert!(!storage.wait_conditions_path.exists());
        assert!(!storage.queue_entries_path.exists());
        assert!(!storage.timers_path.exists());

        fs::write(
            &storage.wait_conditions_path,
            "{jsonl compat ignored after db cutover}\n",
        )
        .unwrap();
        fs::write(
            &storage.queue_entries_path,
            "{jsonl compat ignored after db cutover}\n",
        )
        .unwrap();
        fs::write(
            &storage.timers_path,
            "{jsonl compat ignored after db cutover}\n",
        )
        .unwrap();

        assert_eq!(
            storage.latest_wait_conditions().unwrap(),
            vec![
                wait_condition.clone(),
                later_wait_condition,
                latest_wait_condition
            ]
        );
        assert_eq!(
            storage.latest_queue_entries().unwrap(),
            vec![queue_entry.clone(), later_queue_entry, latest_queue_entry]
        );
        assert_eq!(
            storage.latest_timer_record("timer-1").unwrap(),
            Some(timer.clone())
        );
        assert_eq!(
            storage.latest_timer_records().unwrap(),
            vec![timer, later_timer, latest_timer]
        );
        assert_eq!(
            storage
                .read_recent_wait_conditions(2)
                .unwrap()
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            vec!["wait-2", "wait-3"]
        );
        assert_eq!(
            storage
                .read_recent_queue_entries(2)
                .unwrap()
                .into_iter()
                .map(|record| record.message_id)
                .collect::<Vec<_>>(),
            vec!["msg-2", "msg-3"]
        );
        assert_eq!(
            storage
                .read_recent_timers(2)
                .unwrap()
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            vec!["timer-2", "timer-3"]
        );
    }

    #[test]
    fn runtime_db_state_writes_skip_live_jsonl_after_cutover() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        runtime_db.tasks().import_legacy(Vec::new()).unwrap();
        runtime_db
            .work_items()
            .import_legacy(Vec::new(), None)
            .unwrap();
        runtime_db
            .wait_conditions()
            .import_legacy(Vec::new())
            .unwrap();
        runtime_db
            .queue_entries()
            .import_legacy(Vec::new())
            .unwrap();
        runtime_db.timers().import_legacy(Vec::new()).unwrap();
        runtime_db
            .external_triggers()
            .import_legacy(Vec::new())
            .unwrap();
        runtime_db
            .workspace_entries()
            .import_legacy(Vec::new())
            .unwrap();
        runtime_db
            .workspace_occupancies()
            .import_legacy(Vec::new())
            .unwrap();
        runtime_db
            .agent_identities()
            .import_legacy(Vec::new())
            .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();

        let now = Utc::now();
        let task = TaskRecord {
            id: "task-db-only".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("db only task".into()),
            detail: None,
            recovery: None,
        };
        let work_item = WorkItemRecord::new("default", "db only work", WorkItemState::Open);
        let wait_condition = WaitConditionRecord {
            id: "wait-db-only".into(),
            agent_id: "default".into(),
            work_item_id: Some(work_item.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("github".into()),
            subject_ref: Some("pr-1".into()),
            waiting_for: "checks".into(),
            wake_sources: vec![],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
        };
        let queue_entry = QueueEntryRecord {
            message_id: "message-db-only".into(),
            agent_id: "default".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        let timer = TimerRecord {
            id: "timer-db-only".into(),
            agent_id: "default".into(),
            created_at: now,
            duration_ms: 1000,
            interval_ms: None,
            repeat: false,
            status: crate::types::TimerStatus::Active,
            summary: Some("timer".into()),
            next_fire_at: Some(now + chrono::Duration::seconds(1)),
            last_fired_at: None,
            fire_count: 0,
        };
        let trigger = ExternalTriggerRecord {
            external_trigger_id: "trigger-db-only".into(),
            target_agent_id: "default".into(),
            waiting_intent_id: None,
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            trigger_url: Some("http://localhost/callback".into()),
            token_hash: "token-hash".into(),
            status: ExternalTriggerStatus::Active,
            created_at: now,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        };
        let mut workspace =
            WorkspaceEntry::new("ws-db-only", std::path::PathBuf::from("/repo"), None);
        workspace.workspace_alias = Some("repo".into());
        workspace.updated_at = now;
        let occupancy = WorkspaceOccupancyRecord {
            occupancy_id: "occ-db-only".into(),
            execution_root_id: "root-db-only".into(),
            workspace_id: workspace.workspace_id.clone(),
            holder_agent_id: "default".into(),
            access_mode: crate::system::types::WorkspaceAccessMode::ExclusiveWrite,
            acquired_at: now,
            released_at: None,
        };
        let identity = AgentIdentityRecord::new(
            "default",
            crate::types::AgentKind::Named,
            crate::types::AgentVisibility::Public,
            crate::types::AgentOwnership::SelfOwned,
            crate::types::AgentProfilePreset::PublicNamed,
            None,
            None,
        );

        storage.append_task(&task).unwrap();
        storage.append_work_item(&work_item).unwrap();
        storage.append_wait_condition(&wait_condition).unwrap();
        storage.append_queue_entry(&queue_entry).unwrap();
        storage.append_timer(&timer).unwrap();
        storage.append_external_trigger(&trigger).unwrap();
        storage.append_workspace_entry(&workspace).unwrap();
        storage.append_workspace_occupancy(&occupancy).unwrap();
        storage.append_agent_identity(&identity).unwrap();

        for file_name in [
            "tasks.jsonl",
            "work_items.jsonl",
            "wait_conditions.jsonl",
            "queue_entries.jsonl",
            "timers.jsonl",
            "external_triggers.jsonl",
            "workspaces.jsonl",
            "workspace_occupancies.jsonl",
            "agent_identities.jsonl",
        ] {
            assert!(
                !storage.ledger_dir().join(file_name).exists(),
                "{file_name} should not be a live compat export after db cutover"
            );
        }

        assert_eq!(
            storage.latest_task_record("task-db-only").unwrap(),
            Some(task)
        );
        assert_eq!(
            storage.latest_work_item(&work_item.id).unwrap(),
            Some(work_item)
        );
        assert_eq!(
            storage.latest_wait_conditions().unwrap(),
            vec![wait_condition]
        );
        assert_eq!(storage.latest_queue_entries().unwrap(), vec![queue_entry]);
        assert_eq!(storage.latest_timer_records().unwrap(), vec![timer]);
        assert_eq!(storage.latest_external_triggers().unwrap(), vec![trigger]);
        assert_eq!(storage.latest_workspace_entries().unwrap(), vec![workspace]);
        assert_eq!(
            storage.latest_workspace_occupancies().unwrap(),
            vec![occupancy]
        );
        assert_eq!(storage.latest_agent_identities().unwrap(), vec![identity]);
    }

    #[test]
    fn append_turn_persists_lightweight_turn_record() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut record = TurnRecord::new("default", " turn-1 ", 7);
        record.input_message_ids = vec!["msg-1".into()];
        record.tool_execution_ids = vec!["tool-1".into()];
        record.produced_brief_ids = vec!["brief-1".into()];
        record.delivery_summary_ids = vec!["delivery-1".into()];
        record.completed_work_item_ids = vec!["work-1".into()];
        record.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            completed_at: Utc::now(),
            duration_ms: 42,
        });

        storage.append_turn(&record).unwrap();

        let turns = storage.read_recent_turns(10).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn_id, "turn-1");
        assert_eq!(turns[0].input_message_ids, vec!["msg-1"]);
        assert_eq!(turns[0].tool_execution_ids, vec!["tool-1"]);
        assert_eq!(turns[0].produced_brief_ids, vec!["brief-1"]);
        assert_eq!(turns[0].delivery_summary_ids, vec!["delivery-1"]);
        assert_eq!(turns[0].completed_work_item_ids, vec!["work-1"]);
        assert_eq!(
            turns[0].terminal.as_ref().map(|terminal| terminal.kind),
            Some(crate::types::TurnTerminalKind::Completed)
        );
    }

    #[test]
    fn append_turn_uses_runtime_db_after_cutover_without_turns_jsonl() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();
        let record = TurnRecord::new("default", "turn-db", 9);

        storage.append_turn(&record).unwrap();

        assert!(!storage.ledger_dir().join("turns.jsonl").exists());
        let turns = storage.read_recent_turns(10).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn_id, "turn-db");
        assert_eq!(
            runtime_db
                .turn_records()
                .recent_for_agent("default", 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn append_message_uses_runtime_db_after_cutover_without_messages_jsonl() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();

        storage
            .append_message(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "db only".into(),
                },
            ))
            .unwrap();

        assert!(!storage.ledger_dir().join("messages.jsonl").exists());
        let messages = storage.read_recent_messages(10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_seq, Some(1));
        assert_eq!(runtime_db.messages().count(None).unwrap(), 1);
    }

    #[test]
    fn read_messages_from_preserves_recent_window_semantics_after_db_cutover() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db)
            .unwrap();

        for text in ["one", "two", "three", "four"] {
            storage
                .append_message(&MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator { actor_id: None },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text { text: text.into() },
                ))
                .unwrap();
        }

        let messages = storage.read_messages_from(1, 2).unwrap();
        let texts = messages
            .into_iter()
            .map(|message| match message.body {
                MessageBody::Text { text } => text,
                _ => panic!("expected text body"),
            })
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["three", "four"]);
    }

    #[test]
    fn append_transcript_entry_uses_runtime_db_after_cutover_without_transcript_jsonl() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();

        storage
            .append_transcript_entry(&TranscriptEntry::new(
                "default",
                TranscriptEntryKind::IncomingMessage,
                None,
                Some("message-1".into()),
                serde_json::json!({ "text": "db only" }),
            ))
            .unwrap();

        assert!(!storage.ledger_dir().join("transcript.jsonl").exists());
        let transcript = storage.read_recent_transcript(10).unwrap();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].transcript_seq, Some(1));
        assert_eq!(runtime_db.transcript_entries().all(None).unwrap().len(), 1);
    }

    #[test]
    fn runtime_db_evidence_writes_skip_live_jsonl_after_cutover() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();
        let brief = BriefRecord::new("default", BriefKind::Result, "db brief", None, None);
        let tool = ToolExecutionRecord {
            id: "tool-db-evidence".into(),
            agent_id: "default".into(),
            work_item_id: Some("work-db-evidence".into()),
            turn_index: 0,
            turn_id: Some("turn-db-evidence".into()),
            tool_name: "ExecCommand".into(),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: 1,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "cmd": "echo db evidence" }),
            output: serde_json::json!({ "exit_code": 0 }),
            summary: "command exited".into(),
            invocation_surface: None,
        };
        let delivery = DeliverySummaryRecord::new(
            "default",
            "work-db-evidence",
            "delivery evidence",
            None,
            None,
        );

        storage.append_brief(&brief).unwrap();
        storage.append_tool_execution(&tool).unwrap();
        storage.append_delivery_summary(&delivery).unwrap();

        assert!(!storage.ledger_dir().join("briefs.jsonl").exists());
        assert!(!storage.ledger_dir().join("tools.jsonl").exists());
        assert!(!storage
            .ledger_dir()
            .join("delivery_summaries.jsonl")
            .exists());
        assert_eq!(storage.read_recent_briefs(10).unwrap(), vec![brief]);
        assert_eq!(storage.read_recent_tool_executions(10).unwrap(), vec![tool]);
        assert_eq!(
            storage
                .latest_delivery_summary("work-db-evidence")
                .unwrap()
                .map(|record| record.text),
            Some("delivery evidence".into())
        );
        assert_eq!(storage.count_briefs().unwrap(), 1);
    }

    #[test]
    fn runtime_db_evidence_reads_use_directory_agent_id_without_agent_json() {
        let dir = tempdir().unwrap();
        let agent_dir = dir.path().join("agents/default");
        fs::create_dir_all(&agent_dir).unwrap();
        let storage = AppStorage::new(&agent_dir).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db)
            .unwrap();

        let brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "directory agent id",
            None,
            None,
        );
        storage.append_brief(&brief).unwrap();

        assert_eq!(storage.read_recent_briefs(10).unwrap(), vec![brief]);
        assert_eq!(storage.count_briefs().unwrap(), 1);
        assert!(storage
            .shared_indexes_dir()
            .join(format!(
                "memory.{}.dirty",
                memory_index_agent_key("default")
            ))
            .exists());
    }

    #[test]
    fn runtime_db_message_and_transcript_import_is_idempotent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "legacy message".into(),
            },
        );
        message.message_seq = Some(7);
        let entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(message.id.clone()),
            serde_json::json!({ "text": "legacy transcript" }),
        );

        runtime_db
            .messages()
            .import_legacy(vec![serde_json::to_value(&message).unwrap()])
            .unwrap();
        runtime_db
            .transcript_entries()
            .import_legacy(vec![entry.clone()])
            .unwrap();
        runtime_db
            .messages()
            .import_legacy(vec![serde_json::to_value(&message).unwrap()])
            .unwrap();
        runtime_db
            .transcript_entries()
            .import_legacy(vec![entry])
            .unwrap();

        assert_eq!(runtime_db.messages().count(None).unwrap(), 1);
        assert_eq!(runtime_db.transcript_entries().all(None).unwrap().len(), 1);
        assert_eq!(
            runtime_db
                .storage_domain("messages")
                .unwrap()
                .expect("messages storage domain")
                .canonical_source,
            "db"
        );
        assert_eq!(
            runtime_db
                .storage_domain("transcript_entries")
                .unwrap()
                .expect("transcript storage domain")
                .canonical_source,
            "db"
        );
    }

    #[test]
    fn runtime_db_message_import_failure_records_retryable_domain_state() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();

        let error = runtime_db
            .messages()
            .import_legacy(vec![serde_json::json!({ "turn_index": 1 })])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("importing legacy storage domain messages"));
        assert_eq!(runtime_db.messages().count(None).unwrap(), 0);

        let failed = runtime_db
            .storage_domain("messages")
            .unwrap()
            .expect("failed messages storage domain");
        let checkpoint: serde_json::Value = serde_json::from_str(
            failed
                .source_checkpoint_json
                .as_deref()
                .expect("messages failure checkpoint"),
        )
        .unwrap();
        assert_eq!(failed.import_status, "failed");
        assert_eq!(failed.canonical_source, "jsonl");
        assert_eq!(runtime_db.messages().count(None).unwrap(), 0);
        assert_eq!(
            checkpoint.get("retry").and_then(serde_json::Value::as_str),
            Some("restart runtime to retry legacy import")
        );
    }

    #[test]
    fn message_and_transcript_reads_ignore_legacy_jsonl_after_db_cutover() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();
        let db_message = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "test".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "from db".into(),
            },
        );
        let db_entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::AssistantRound,
            None,
            Some(db_message.id.clone()),
            serde_json::json!({ "text": "from db" }),
        );
        runtime_db.messages().upsert(&db_message).unwrap();
        runtime_db.transcript_entries().upsert(&db_entry).unwrap();

        fs::write(
            storage.ledger_dir().join("messages.jsonl"),
            serde_json::to_string(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "from jsonl".into(),
                },
            ))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            storage.ledger_dir().join("transcript.jsonl"),
            serde_json::to_string(&TranscriptEntry::new(
                "default",
                TranscriptEntryKind::IncomingMessage,
                None,
                Some("legacy-message".into()),
                serde_json::json!({ "text": "from jsonl" }),
            ))
            .unwrap(),
        )
        .unwrap();

        let messages = storage.read_recent_messages(10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, db_message.id);
        let transcript = storage.read_recent_transcript(10).unwrap();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].id, db_entry.id);
    }

    #[test]
    fn message_seq_counter_resumes_from_existing_ledger() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_message(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "first".into(),
                },
            ))
            .unwrap();

        let reopened = AppStorage::new(dir.path()).unwrap();
        reopened
            .append_message(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "second".into(),
                },
            ))
            .unwrap();

        let messages = reopened.read_recent_messages(10).unwrap();
        assert_eq!(
            messages.last().and_then(|message| message.message_seq),
            Some(2)
        );
    }

    #[test]
    fn message_seq_counter_resumes_from_latest_tail_sequence() {
        let dir = tempdir().unwrap();
        let ledger_dir = dir.path().join(".holon/ledger");
        std::fs::create_dir_all(&ledger_dir).unwrap();
        let messages_path = ledger_dir.join("messages.jsonl");
        let mut sequenced = serde_json::to_value(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "sequenced".into(),
            },
        ))
        .unwrap();
        sequenced
            .as_object_mut()
            .unwrap()
            .insert("message_seq".to_string(), serde_json::json!(7));
        let mut trailing_legacy = serde_json::to_value(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "legacy".into(),
            },
        ))
        .unwrap();
        trailing_legacy
            .as_object_mut()
            .unwrap()
            .remove("message_seq");
        std::fs::write(&messages_path, format!("{sequenced}\n{trailing_legacy}\n")).unwrap();

        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_message(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "next".into(),
                },
            ))
            .unwrap();

        let messages = storage.read_recent_messages(10).unwrap();
        assert_eq!(
            messages.last().and_then(|message| message.message_seq),
            Some(8)
        );
    }

    #[test]
    fn storage_backfills_legacy_events_jsonl_on_open() {
        let dir = tempdir().unwrap();
        let ledger_dir = dir.path().join(".holon/ledger");
        std::fs::create_dir_all(&ledger_dir).unwrap();
        let events_path = ledger_dir.join("events.jsonl");
        std::fs::write(
            &events_path,
            [
                serde_json::json!({
                    "id": "evt-old-1",
                    "created_at": "2026-05-20T00:00:00Z",
                    "kind": "test_event",
                    "data": { "n": 1 }
                })
                .to_string(),
                serde_json::json!({
                    "id": "evt-old-2",
                    "created_at": "2026-05-20T00:00:01Z",
                    "kind": "test_event",
                    "data": { "n": 2 }
                })
                .to_string(),
            ]
            .join("\n"),
        )
        .unwrap();

        let storage = AppStorage::new(dir.path()).unwrap();
        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| (event.id.as_str(), event.event_seq))
                .collect::<Vec<_>>(),
            vec![("evt-old-1", 1), ("evt-old-2", 2)]
        );
        assert!(std::fs::read_dir(&ledger_dir).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("events.jsonl.bak.")));

        storage
            .append_event(&AuditEvent::new(
                "test_event",
                serde_json::json!({ "n": 3 }),
            ))
            .unwrap();
        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(events.last().map(|event| event.event_seq), Some(3));
    }

    #[test]
    fn storage_uses_tail_event_seq_without_rewriting_migrated_ledger() {
        let dir = tempdir().unwrap();
        let ledger_dir = dir.path().join(".holon/ledger");
        std::fs::create_dir_all(&ledger_dir).unwrap();
        let events_path = ledger_dir.join("events.jsonl");
        let first = AuditEvent {
            id: "evt-new-1".into(),
            event_seq: 41,
            created_at: Utc::now(),
            kind: "test_event".into(),
            data: serde_json::json!({ "n": 1 }),
        };
        std::fs::write(
            &events_path,
            format!("{}\n", serde_json::to_string(&first).unwrap()),
        )
        .unwrap();

        let storage = AppStorage::new(dir.path()).unwrap();
        assert!(!std::fs::read_dir(&ledger_dir).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("events.jsonl.bak.")));

        storage
            .append_event(&AuditEvent::new(
                "test_event",
                serde_json::json!({ "n": 2 }),
            ))
            .unwrap();
        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(events.last().map(|event| event.event_seq), Some(42));
    }

    #[test]
    fn storage_round_trip_agent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        storage.write_agent(&agent).unwrap();

        let restored = storage.read_agent().unwrap().unwrap();
        assert_eq!(restored.status, AgentStatus::Asleep);
        assert!(dir.path().join(".holon/state/agent.json").is_file());
        assert!(!dir.path().join("agent.json").exists());
    }

    #[test]
    fn write_agent_with_runtime_db_keeps_legacy_agent_json_export_current() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        runtime_db
            .agent_states()
            .import_legacy(storage.read_agent_file().unwrap())
            .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db)
            .unwrap();

        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Stopped;
        agent.turn_index = 7;
        storage.write_agent(&agent).unwrap();

        let reopened_without_db = AppStorage::new(dir.path()).unwrap();
        let restored = reopened_without_db.read_agent().unwrap().unwrap();
        assert_eq!(restored.status, AgentStatus::Stopped);
        assert_eq!(restored.turn_index, 7);
    }

    #[test]
    fn read_agent_maps_legacy_paused_status_to_stopped() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Stopped;
        let mut value = serde_json::to_value(&agent).unwrap();
        value["status"] = serde_json::json!("paused");
        std::fs::write(
            dir.path().join(".holon/state/agent.json"),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();

        let restored = storage.read_agent().unwrap().unwrap();
        assert_eq!(restored.status, AgentStatus::Stopped);
    }

    #[test]
    fn latest_active_task_records_reduce_by_id_and_filter_terminal_tasks() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();
        let task = |id: &str, status: TaskStatus, offset: i64| TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: status.clone(),
            created_at: now + chrono::Duration::seconds(offset),
            updated_at: now + chrono::Duration::seconds(offset),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(format!("{id} {status:?}")),
            detail: None,
            recovery: None,
        };

        storage
            .append_task(&task("completed-after-queue", TaskStatus::Queued, 0))
            .unwrap();
        storage
            .append_task(&task("running", TaskStatus::Running, 1))
            .unwrap();
        storage
            .append_task(&task("completed-after-queue", TaskStatus::Completed, 2))
            .unwrap();
        storage
            .append_task(&task("cancelling", TaskStatus::Cancelling, 3))
            .unwrap();

        let active = storage.latest_active_task_records(10).unwrap();
        let rendered = active
            .iter()
            .map(|task| (task.id.as_str(), task.status.clone()))
            .collect::<Vec<_>>();

        assert_eq!(
            rendered,
            vec![
                ("cancelling", TaskStatus::Cancelling),
                ("running", TaskStatus::Running)
            ]
        );
    }

    #[test]
    fn latest_task_records_from_recent_reduces_only_bounded_history() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();
        let task = |id: &str, summary: &str, offset: i64| TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now + chrono::Duration::seconds(offset),
            updated_at: now + chrono::Duration::seconds(offset),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(summary.into()),
            detail: None,
            recovery: None,
        };

        storage
            .append_task(&task("bounded-old", "outside bounded history", 0))
            .unwrap();
        storage
            .append_task(&task("bounded-repeat", "older repeat snapshot", 1))
            .unwrap();
        storage
            .append_task(&task("bounded-other", "other recent snapshot", 2))
            .unwrap();
        storage
            .append_task(&task("bounded-repeat", "latest repeat snapshot", 3))
            .unwrap();

        let latest = storage.latest_task_records_from_recent(3).unwrap();
        let rendered = latest
            .iter()
            .map(|task| {
                (
                    task.id.as_str(),
                    task.summary.as_deref().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            rendered,
            vec![
                ("bounded-other", "other recent snapshot"),
                ("bounded-repeat", "latest repeat snapshot")
            ]
        );
    }

    #[test]
    fn mark_memory_index_dirty_does_not_rewrite_existing_marker() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let dirty_path = storage.indexes_dir().join("memory.default.dirty");

        storage.mark_memory_index_dirty().unwrap();
        fs::write(&dirty_path, b"already dirty").unwrap();
        storage.mark_memory_index_dirty().unwrap();

        assert_eq!(fs::read(&dirty_path).unwrap(), b"already dirty");
    }

    #[test]
    fn latest_active_task_records_applies_limit_after_reverse_reduction() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();

        for index in 0..5 {
            storage
                .append_task(&TaskRecord {
                    id: format!("task-{index}"),
                    agent_id: "default".into(),
                    kind: TaskKind::CommandTask,
                    status: TaskStatus::Running,
                    created_at: now + chrono::Duration::seconds(index),
                    updated_at: now + chrono::Duration::seconds(index),
                    parent_message_id: None,
                    work_item_id: None,
                    summary: None,
                    detail: None,
                    recovery: None,
                })
                .unwrap();
        }

        let active = storage.latest_active_task_records(2).unwrap();
        let ids = active
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["task-4", "task-3"]);
    }

    #[test]
    fn latest_active_task_records_for_agent_scopes_limit_and_count() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();

        let task = |id: &str, agent_id: &str, status: TaskStatus, offset: i64| TaskRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            kind: TaskKind::CommandTask,
            status: status.clone(),
            created_at: now + chrono::Duration::seconds(offset),
            updated_at: now + chrono::Duration::seconds(offset),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(format!("{id} {status:?}")),
            detail: None,
            recovery: None,
        };

        storage
            .append_task(&task("default-old", "default", TaskStatus::Running, 0))
            .unwrap();
        storage
            .append_task(&task("other-new", "other", TaskStatus::Running, 1))
            .unwrap();
        storage
            .append_task(&task("default-new", "default", TaskStatus::Running, 2))
            .unwrap();
        storage
            .append_task(&task("default-old", "default", TaskStatus::Completed, 3))
            .unwrap();

        let active = storage
            .latest_active_task_records_for_agent("default", 1)
            .unwrap();
        let ids = active
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["default-new"]);
        assert_eq!(storage.active_task_count_for_agent("default").unwrap(), 1);
        assert_eq!(storage.active_task_count_for_agent("other").unwrap(), 1);
    }

    #[test]
    fn latest_active_task_records_preserves_current_child_agent_recovery_from_older_snapshot() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();
        let mut task = TaskRecord {
            id: "task-recovery".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: Some(TaskRecoverySpec::ChildAgentTask {
                summary: "recover".into(),
                prompt: "resume with artifact".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
            }),
        };
        storage.append_task(&task).unwrap();

        task.updated_at = now + chrono::Duration::seconds(1);
        task.recovery = None;
        storage.append_task(&task).unwrap();

        let active = storage.latest_active_task_records(1).unwrap();
        assert_eq!(active.len(), 1);
        assert!(matches!(
            active[0].recovery,
            Some(TaskRecoverySpec::ChildAgentTask { .. })
        ));
    }

    #[test]
    fn cloned_storage_serializes_concurrent_large_task_appends() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();
        let thread_count = 8;
        let records_per_thread = 40;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(thread_count));
        let mut handles = Vec::new();

        for thread_index in 0..thread_count {
            let storage = storage.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for record_index in 0..records_per_thread {
                    storage
                        .append_task(&TaskRecord {
                            id: format!("task-{thread_index}-{record_index}"),
                            agent_id: "default".into(),
                            kind: TaskKind::CommandTask,
                            status: TaskStatus::Completed,
                            created_at: now,
                            updated_at: now,
                            parent_message_id: None,
                            work_item_id: None,
                            summary: Some("large concurrent append".into()),
                            detail: Some(serde_json::json!({
                                "payload": "x".repeat(16 * 1024),
                                "thread": thread_index,
                                "record": record_index,
                            })),
                            recovery: None,
                        })
                        .unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let content = fs::read_to_string(storage.ledger_dir().join("tasks.jsonl")).unwrap();
        let mut parsed = 0usize;
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            serde_json::from_str::<TaskRecord>(line).unwrap();
            parsed += 1;
        }
        assert_eq!(parsed, thread_count * records_per_thread);
    }

    #[test]
    fn storage_round_trip_transcript_entries() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some("message-1".into()),
            serde_json::json!({ "text": "hello" }),
        );
        storage.append_transcript_entry(&entry).unwrap();

        let restored = storage.read_recent_transcript(10).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].kind, TranscriptEntryKind::IncomingMessage);
        assert_eq!(restored[0].related_message_id.as_deref(), Some("message-1"));
        assert!(dir.path().join(".holon/ledger/transcript.jsonl").is_file());
        assert!(!dir.path().join("transcript.jsonl").exists());
        assert!(storage.indexes_dir().is_dir());
        assert!(storage.cache_dir().is_dir());
    }

    #[test]
    fn append_message_assigns_monotonic_message_seq() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        for text in ["first", "second"] {
            storage
                .append_message(&MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator { actor_id: None },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text { text: text.into() },
                ))
                .unwrap();
        }

        let messages = storage.read_recent_messages(10).unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.message_seq)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2)]
        );
    }

    #[test]
    fn append_transcript_entry_assigns_monotonic_transcript_seq() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        for related in ["message-1", "message-2"] {
            storage
                .append_transcript_entry(&TranscriptEntry::new(
                    "default",
                    TranscriptEntryKind::IncomingMessage,
                    None,
                    Some(related.into()),
                    serde_json::json!({ "text": related }),
                ))
                .unwrap();
        }

        let entries = storage.read_recent_transcript(10).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.transcript_seq)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2)]
        );
    }

    #[test]
    fn missing_ledger_sequence_fields_are_backward_compatible() {
        let mut legacy_message = serde_json::to_value(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "hello".into(),
            },
        ))
        .unwrap();
        legacy_message
            .as_object_mut()
            .unwrap()
            .remove("message_seq");

        let mut legacy_entry = serde_json::to_value(TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some("legacy-message".into()),
            serde_json::json!({ "text": "hello" }),
        ))
        .unwrap();
        legacy_entry
            .as_object_mut()
            .unwrap()
            .remove("transcript_seq");

        let message: MessageEnvelope = serde_json::from_value(legacy_message).unwrap();
        let entry: TranscriptEntry = serde_json::from_value(legacy_entry).unwrap();
        assert_eq!(message.message_seq, None);
        assert_eq!(entry.transcript_seq, None);
    }

    #[test]
    fn read_recent_jsonl_only_parses_requested_tail() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let path = storage.ledger_dir().join("briefs.jsonl");
        fs::write(
            &path,
            [
                "{not valid json}".to_string(),
                serde_json::to_string(&TranscriptEntry::new(
                    "default",
                    TranscriptEntryKind::IncomingMessage,
                    None,
                    Some("older".into()),
                    serde_json::json!({ "text": "older" }),
                ))
                .unwrap(),
                serde_json::to_string(&TranscriptEntry::new(
                    "default",
                    TranscriptEntryKind::IncomingMessage,
                    None,
                    Some("newer".into()),
                    serde_json::json!({ "text": "newer" }),
                ))
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();

        let entries = read_recent_jsonl::<TranscriptEntry>(&path, 1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].related_message_id.as_deref(), Some("newer"));
    }

    #[test]
    fn latest_work_item_delegation_for_child_scans_from_tail() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        fs::write(
            &storage.work_item_delegations_path,
            "{not valid json and should not be parsed}\n",
        )
        .unwrap();

        let other = WorkItemDelegationRecord::new(
            "parent-agent",
            "parent-work-other",
            "other-child",
            "child-work-other",
        );
        let older = WorkItemDelegationRecord::new(
            "parent-agent",
            "parent-work-1",
            "target-child",
            "child-work-1",
        );
        let latest = WorkItemDelegationRecord {
            state: WorkItemDelegationState::Completed,
            result_summary: Some("done".into()),
            updated_at: Utc::now(),
            ..older.clone()
        };

        storage.append_work_item_delegation(&other).unwrap();
        storage.append_work_item_delegation(&older).unwrap();
        storage.append_work_item_delegation(&latest).unwrap();

        let found = storage
            .latest_work_item_delegation_for_child("target-child")
            .unwrap()
            .expect("target child delegation should be found");
        assert_eq!(found.delegation_id, older.delegation_id);
        assert_eq!(found.state, WorkItemDelegationState::Completed);
        assert_eq!(found.result_summary.as_deref(), Some("done"));
    }

    #[test]
    fn latest_waiting_intent_scans_from_tail_for_agent_and_id() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        fs::write(
            &storage.waiting_intents_path,
            "{not valid json and should not be parsed}\n",
        )
        .unwrap();
        let now = Utc::now();
        let older = WaitingIntentRecord {
            id: "wait-1".into(),
            agent_id: "default".into(),
            scope: WaitingIntentScope::WorkItem,
            work_item_id: Some("work-old".into()),
            description: "older wait".into(),
            source: "test".into(),
            resource: None,
            condition: None,
            delivery_mode: crate::types::CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "trigger-1".into(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        };
        let other_agent = WaitingIntentRecord {
            agent_id: "other".into(),
            work_item_id: Some("work-other".into()),
            ..older.clone()
        };
        let latest = WaitingIntentRecord {
            work_item_id: Some("work-new".into()),
            trigger_count: 1,
            last_triggered_at: Some(now),
            ..older.clone()
        };

        storage.append_waiting_intent(&older).unwrap();
        storage.append_waiting_intent(&other_agent).unwrap();
        storage.append_waiting_intent(&latest).unwrap();

        let found = storage
            .latest_waiting_intent("default", "wait-1")
            .unwrap()
            .expect("latest waiting intent should be found");
        assert_eq!(found.work_item_id.as_deref(), Some("work-new"));
        assert_eq!(found.trigger_count, 1);
    }

    #[test]
    fn append_waiting_intent_mirrors_internal_wait_condition_ledger() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();
        let active = WaitingIntentRecord {
            id: "wait-1".into(),
            agent_id: "default".into(),
            scope: WaitingIntentScope::WorkItem,
            work_item_id: Some("work-1".into()),
            description: "waiting for github".into(),
            source: "github".into(),
            resource: Some("pr-1".into()),
            condition: Some("ci passed".into()),
            delivery_mode: crate::types::CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "trigger-1".into(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        };
        let cancelled = WaitingIntentRecord {
            status: WaitingIntentStatus::Cancelled,
            cancelled_at: Some(now + chrono::Duration::seconds(10)),
            ..active.clone()
        };

        storage.append_waiting_intent(&active).unwrap();
        let mirrored = storage.latest_wait_conditions().unwrap();
        assert_eq!(mirrored.len(), 1);
        assert_eq!(mirrored[0].id, "waiting_intent:wait-1");
        assert_eq!(mirrored[0].status, WaitConditionStatus::Active);
        assert_eq!(mirrored[0].kind, WaitConditionKind::External);
        assert_eq!(mirrored[0].source.as_deref(), Some("github"));
        assert_eq!(mirrored[0].subject_ref.as_deref(), Some("pr-1"));
        assert_eq!(mirrored[0].waiting_for, "ci passed");
        assert_eq!(
            mirrored[0].wake_sources,
            vec![WakeSource::ExternalIngress {
                external_trigger_id: Some("trigger-1".into()),
            }]
        );
        assert_eq!(
            mirrored[0]
                .continuation
                .as_ref()
                .and_then(|value| value.get("waiting_intent_id").and_then(|id| id.as_str())),
            Some("wait-1")
        );

        storage.append_waiting_intent(&cancelled).unwrap();
        let active_for_work = storage
            .latest_active_wait_conditions_for_work_item("default", "work-1")
            .unwrap();
        assert!(active_for_work.is_empty());
        let latest = storage.latest_wait_conditions().unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].status, WaitConditionStatus::Cancelled);
        assert_eq!(latest[0].cancelled_at, cancelled.cancelled_at);
    }

    #[test]
    fn external_wait_recoverability_is_derived_and_audited() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();

        for record in [
            WaitConditionRecord {
                id: "weak".into(),
                agent_id: "default".into(),
                work_item_id: Some("work-weak".into()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("github".into()),
                subject_ref: Some("pr-1".into()),
                waiting_for: "merged".into(),
                wake_sources: vec![WakeSource::ExternalIngress {
                    external_trigger_id: Some("trigger-1".into()),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            },
            WaitConditionRecord {
                id: "recoverable".into(),
                agent_id: "default".into(),
                work_item_id: Some("work-recoverable".into()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("github".into()),
                subject_ref: Some("pr-2".into()),
                waiting_for: "checks".into(),
                wake_sources: vec![
                    WakeSource::ExternalIngress {
                        external_trigger_id: Some("trigger-2".into()),
                    },
                    WakeSource::Timer {
                        wake_at: now + chrono::Duration::hours(1),
                    },
                ],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            },
            WaitConditionRecord {
                id: "explicit".into(),
                agent_id: "default".into(),
                work_item_id: Some("work-explicit".into()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("github".into()),
                subject_ref: Some("pr-3".into()),
                waiting_for: "manual merge".into(),
                wake_sources: vec![WakeSource::ExternalIngress {
                    external_trigger_id: Some("trigger-3".into()),
                }],
                continuation: Some(serde_json::json!({
                    "no_fallback_reason": "provider has no poll API"
                })),
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            },
        ] {
            storage.append_wait_condition(&record).unwrap();
        }

        let mut conditions = storage.latest_wait_conditions().unwrap();
        conditions.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(
            conditions
                .iter()
                .find(|condition| condition.id == "weak")
                .and_then(WaitConditionRecord::external_recoverability),
            Some(ExternalWaitRecoverability::Weak)
        );
        assert_eq!(
            conditions
                .iter()
                .find(|condition| condition.id == "recoverable")
                .and_then(WaitConditionRecord::external_recoverability),
            Some(ExternalWaitRecoverability::Recoverable)
        );
        let explicit = conditions
            .iter()
            .find(|condition| condition.id == "explicit")
            .unwrap();
        assert_eq!(
            explicit.external_recoverability(),
            Some(ExternalWaitRecoverability::ExplicitNoFallback)
        );
        assert_eq!(
            explicit.no_fallback_reason().as_deref(),
            Some("provider has no poll API")
        );

        let events = storage.read_recent_events(10).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "external_wait_without_recovery"
                && event.data["wait_condition_id"] == "weak"
                && event.data["external_recoverability"] == "weak"
        }));
        assert!(events.iter().any(|event| {
            event.kind == "external_wait_without_recovery"
                && event.data["wait_condition_id"] == "explicit"
                && event.data["external_recoverability"] == "explicit_no_fallback"
                && event.data["no_fallback_reason"] == "provider has no poll API"
        }));
        assert!(!events.iter().any(|event| {
            event.kind == "external_wait_without_recovery"
                && event.data["wait_condition_id"] == "recoverable"
        }));
        let emitted = events
            .iter()
            .filter(|event| event.kind == "external_wait_without_recovery")
            .collect::<Vec<_>>();
        assert_eq!(emitted.len(), 2);
        assert!(emitted[0].event_seq > 0);
        assert!(emitted[1].event_seq > emitted[0].event_seq);

        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "legacy-weak".into(),
                agent_id: "default".into(),
                scope: WaitingIntentScope::Agent,
                work_item_id: Some("work-legacy".into()),
                description: "legacy weak external wait".into(),
                source: "github".into(),
                resource: Some("pr-4".into()),
                condition: Some("merged".into()),
                delivery_mode: CallbackDeliveryMode::WakeHint,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "trigger-4".into(),
                created_at: now,
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
        let legacy_events = storage.read_recent_events(10).unwrap();
        let legacy_event = legacy_events
            .iter()
            .find(|event| event.data["wait_condition_id"] == "waiting_intent:legacy-weak")
            .expect("legacy mirror should emit recoverability audit event");
        assert!(legacy_event.event_seq > emitted[1].event_seq);
    }

    #[test]
    fn work_queue_projection_uses_internal_wait_conditions_for_wait_state() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();

        let mut task_wait = WorkItemRecord::new("default", "task wait", WorkItemState::Open);
        task_wait.blocked_by = Some("command task".into());
        task_wait.updated_at = now;
        let mut external_wait =
            WorkItemRecord::new("default", "external wait", WorkItemState::Open);
        external_wait.blocked_by = Some("ci".into());
        external_wait.updated_at = now + chrono::Duration::seconds(1);
        let mut operator_wait =
            WorkItemRecord::new("default", "operator wait", WorkItemState::Open);
        operator_wait.blocked_by = Some("operator input".into());
        operator_wait.updated_at = now + chrono::Duration::seconds(2);
        let mut timer_wait = WorkItemRecord::new("default", "timer wait", WorkItemState::Open);
        timer_wait.blocked_by = Some("timer".into());
        timer_wait.updated_at = now + chrono::Duration::seconds(3);
        let mut system_wait = WorkItemRecord::new("default", "system wait", WorkItemState::Open);
        system_wait.blocked_by = Some("system tick".into());
        system_wait.updated_at = now + chrono::Duration::seconds(4);

        storage.append_work_item(&task_wait).unwrap();
        storage.append_work_item(&external_wait).unwrap();
        storage.append_work_item(&operator_wait).unwrap();
        storage.append_work_item(&timer_wait).unwrap();
        storage.append_work_item(&system_wait).unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "task-condition".into(),
                agent_id: "default".into(),
                work_item_id: Some(task_wait.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::Task,
                source: None,
                subject_ref: Some("task-1".into()),
                waiting_for: "task result".into(),
                wake_sources: vec![WakeSource::TaskResult {
                    task_id: "task-1".into(),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            })
            .unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "external-condition".into(),
                agent_id: "default".into(),
                work_item_id: Some(external_wait.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("github".into()),
                subject_ref: Some("pr-1".into()),
                waiting_for: "ci".into(),
                wake_sources: vec![WakeSource::ExternalIngress {
                    external_trigger_id: Some("trigger-1".into()),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            })
            .unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "operator-condition".into(),
                agent_id: "default".into(),
                work_item_id: Some(operator_wait.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::Operator,
                source: None,
                subject_ref: None,
                waiting_for: "operator input".into(),
                wake_sources: vec![WakeSource::OperatorInput],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            })
            .unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "timer-condition".into(),
                agent_id: "default".into(),
                work_item_id: Some(timer_wait.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::Timer,
                source: None,
                subject_ref: None,
                waiting_for: "timer".into(),
                wake_sources: vec![WakeSource::Timer {
                    wake_at: now + chrono::Duration::minutes(5),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            })
            .unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "system-condition".into(),
                agent_id: "default".into(),
                work_item_id: Some(system_wait.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::System,
                source: None,
                subject_ref: None,
                waiting_for: "system tick".into(),
                wake_sources: vec![WakeSource::SystemTick],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            })
            .unwrap();

        let projection = storage.work_queue_prompt_projection().unwrap();
        let state_for = |id: &str| {
            projection
                .readiness
                .iter()
                .find(|item| item.work_item.id == id)
                .map(|item| item.scheduling_state)
                .unwrap()
        };
        assert_eq!(
            state_for(&operator_wait.id),
            WorkItemSchedulingState::WaitingOperator
        );
        assert_eq!(
            state_for(&timer_wait.id),
            WorkItemSchedulingState::WaitingTimer
        );
        assert_eq!(
            state_for(&system_wait.id),
            WorkItemSchedulingState::WaitingSystem
        );
        assert_eq!(
            projection
                .blocked
                .iter()
                .map(|item| item.work_item.objective.as_str())
                .collect::<Vec<_>>(),
            vec!["system wait", "timer wait", "external wait"]
        );
        assert!(projection
            .queued_blocked
            .iter()
            .all(|item| item.objective != "task wait"));
    }

    #[test]
    fn agent_posture_projection_derives_precedence_from_runtime_facts() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        storage.write_agent(&agent).unwrap();

        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::Idle
        );

        let mut blocked = WorkItemRecord::new("default", "blocked", WorkItemState::Open);
        blocked.blocked_by = Some("unstructured blocker".into());
        storage.append_work_item(&blocked).unwrap();
        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::Blocked
        );

        let mut external = WorkItemRecord::new("default", "external", WorkItemState::Open);
        external.blocked_by = Some("github".into());
        external.updated_at = now + chrono::Duration::seconds(1);
        storage.append_work_item(&external).unwrap();
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait-external".into(),
                agent_id: "default".into(),
                scope: WaitingIntentScope::WorkItem,
                work_item_id: Some(external.id.clone()),
                description: "external callback".into(),
                source: "github".into(),
                resource: Some("pull_request:1".into()),
                condition: Some("merged".into()),
                delivery_mode: CallbackDeliveryMode::WakeHint,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "trigger-external".into(),
                created_at: now,
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::WaitingForExternal
        );

        let mut needs_input = WorkItemRecord::new("default", "operator", WorkItemState::Open);
        needs_input.plan_status = WorkItemPlanStatus::NeedsInput;
        needs_input.updated_at = now + chrono::Duration::seconds(2);
        storage.append_work_item(&needs_input).unwrap();
        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::WaitingForExternal,
            "external wait has precedence over operator wait"
        );

        let mut task_wait = WorkItemRecord::new("default", "task", WorkItemState::Open);
        task_wait.updated_at = now + chrono::Duration::seconds(3);
        storage.append_work_item(&task_wait).unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "task-condition".into(),
                agent_id: "default".into(),
                work_item_id: Some(task_wait.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::Task,
                source: None,
                subject_ref: Some("task-1".into()),
                waiting_for: "task result".into(),
                wake_sources: vec![WakeSource::TaskResult {
                    task_id: "task-1".into(),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,

                turn_id: None,
            })
            .unwrap();
        let task_projection = storage.agent_posture_projection(&agent).unwrap();
        assert_eq!(
            task_projection.posture,
            AgentSchedulingPosture::WaitingForTask
        );
        assert_eq!(task_projection.task_id.as_deref(), Some("task-1"));

        let runnable = WorkItemRecord::new("default", "runnable", WorkItemState::Open);
        storage.append_work_item(&runnable).unwrap();
        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::HasRunnableWork
        );

        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: "queued-message".into(),
                agent_id: "default".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::HasQueuedInput
        );

        agent.current_run_id = Some("run-1".into());
        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::ActiveTurn
        );

        agent.current_run_id = None;
        agent.status = AgentStatus::Stopped;
        assert_eq!(
            storage.agent_posture_projection(&agent).unwrap().posture,
            AgentSchedulingPosture::Archived
        );
    }

    #[test]
    fn agent_posture_projection_acceptance_states_are_directly_derived() {
        let posture_for = |storage: &AppStorage, agent: &AgentState| {
            storage.agent_posture_projection(agent).unwrap().posture
        };
        let append_wait_condition =
            |storage: &AppStorage, work_item_id: &str, kind: WaitConditionKind| {
                let wait_kind = format!("{kind:?}");
                let wake_sources = match kind {
                    WaitConditionKind::Timer => vec![WakeSource::Timer {
                        wake_at: Utc::now() + chrono::Duration::minutes(5),
                    }],
                    WaitConditionKind::System => vec![WakeSource::SystemTick],
                    _ => unreachable!("helper is only used for timer/system waits"),
                };
                storage
                    .append_wait_condition(&WaitConditionRecord {
                        id: format!("{kind:?}-condition"),
                        agent_id: "default".into(),
                        work_item_id: Some(work_item_id.into()),
                        status: WaitConditionStatus::Active,
                        kind,
                        source: None,
                        subject_ref: None,
                        waiting_for: wait_kind,
                        wake_sources,
                        continuation: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        expires_at: None,
                        resolved_at: None,
                        cancelled_at: None,

                        turn_id: None,
                    })
                    .unwrap();
            };

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        assert_eq!(posture_for(&storage, &agent), AgentSchedulingPosture::Idle);

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: "queued-message".into(),
                agent_id: "default".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .unwrap();
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::HasQueuedInput
        );

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        let runnable = WorkItemRecord::new("default", "current runnable", WorkItemState::Open);
        agent.current_work_item_id = Some(runnable.id.clone());
        storage.write_agent(&agent).unwrap();
        storage.append_work_item(&runnable).unwrap();
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::HasRunnableWork
        );

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        let mut needs_input =
            WorkItemRecord::new("default", "operator decision", WorkItemState::Open);
        needs_input.plan_status = WorkItemPlanStatus::NeedsInput;
        storage.append_work_item(&needs_input).unwrap();
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::WaitingForOperator
        );

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        let mut external = WorkItemRecord::new("default", "external wait", WorkItemState::Open);
        external.blocked_by = Some("github".into());
        storage.append_work_item(&external).unwrap();
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait-external".into(),
                agent_id: "default".into(),
                scope: WaitingIntentScope::WorkItem,
                work_item_id: Some(external.id.clone()),
                description: "external callback".into(),
                source: "github".into(),
                resource: Some("pull_request:1".into()),
                condition: Some("merged".into()),
                delivery_mode: CallbackDeliveryMode::WakeHint,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "trigger-external".into(),
                created_at: Utc::now(),
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::WaitingForExternal
        );

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        let mut blocked = WorkItemRecord::new("default", "blocked", WorkItemState::Open);
        blocked.blocked_by = Some("unstructured blocker".into());
        storage.append_work_item(&blocked).unwrap();
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::Blocked
        );

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        let timer_wait = WorkItemRecord::new("default", "timer wait", WorkItemState::Open);
        storage.append_work_item(&timer_wait).unwrap();
        append_wait_condition(&storage, &timer_wait.id, WaitConditionKind::Timer);
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::Blocked
        );

        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        let system_wait = WorkItemRecord::new("default", "system wait", WorkItemState::Open);
        storage.append_work_item(&system_wait).unwrap();
        append_wait_condition(&storage, &system_wait.id, WaitConditionKind::System);
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::Blocked
        );
    }

    #[test]
    fn storage_recovery_snapshot_replays_latest_unprocessed_messages() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let queued = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::WebhookEvent,
            crate::types::MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            crate::types::AuthorityClass::IntegrationSignal,
            Priority::Normal,
            crate::types::MessageBody::Text {
                text: "queued".into(),
            },
        );
        let done = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::WebhookEvent,
            crate::types::MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            crate::types::AuthorityClass::IntegrationSignal,
            Priority::Normal,
            crate::types::MessageBody::Text {
                text: "done".into(),
            },
        );
        let dequeued = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::WebhookEvent,
            crate::types::MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            crate::types::AuthorityClass::IntegrationSignal,
            Priority::Normal,
            crate::types::MessageBody::Text {
                text: "dequeued".into(),
            },
        );
        storage.append_message(&queued).unwrap();
        storage.append_message(&done).unwrap();
        storage.append_message(&dequeued).unwrap();
        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: queued.id.clone(),
                agent_id: "default".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: queued.created_at,
                updated_at: Utc::now(),
            })
            .unwrap();
        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: done.id.clone(),
                agent_id: "default".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Processed,
                created_at: done.created_at,
                updated_at: Utc::now(),
            })
            .unwrap();
        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: dequeued.id.clone(),
                agent_id: "default".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Dequeued,
                created_at: dequeued.created_at,
                updated_at: Utc::now(),
            })
            .unwrap();

        let snapshot = storage.recovery_snapshot("default").unwrap();
        assert_eq!(snapshot.replay_messages.len(), 2);
        assert_eq!(snapshot.replay_messages[0].id, queued.id);
        assert_eq!(snapshot.replay_messages[1].id, dequeued.id);
    }

    #[test]
    fn recovery_snapshot_orders_message_replay_by_sequence_when_timestamps_tie() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let created_at = Utc::now();
        let mut second = MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            AuthorityClass::IntegrationSignal,
            Priority::Normal,
            MessageBody::Text {
                text: "second".into(),
            },
        );
        second.created_at = created_at;
        let mut first = MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            AuthorityClass::IntegrationSignal,
            Priority::Normal,
            MessageBody::Text {
                text: "first".into(),
            },
        );
        first.created_at = created_at;

        storage.append_message(&first).unwrap();
        storage.append_message(&second).unwrap();
        for message in [&second, &first] {
            storage
                .append_queue_entry(&QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: "default".into(),
                    priority: Priority::Normal,
                    status: QueueEntryStatus::Queued,
                    created_at: message.created_at,
                    updated_at: Utc::now(),
                })
                .unwrap();
        }

        let snapshot = storage.recovery_snapshot("default").unwrap();
        assert_eq!(
            snapshot
                .replay_messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.id.as_str(), second.id.as_str()]
        );
    }

    #[test]
    fn recovery_snapshot_orders_message_replay_by_sequence_before_timestamp() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let created_at = Utc::now();
        let mut later = MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            AuthorityClass::IntegrationSignal,
            Priority::Normal,
            MessageBody::Text {
                text: "later".into(),
            },
        );
        later.created_at = created_at + chrono::Duration::seconds(1);
        let mut earlier = MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: None,
            },
            AuthorityClass::IntegrationSignal,
            Priority::Normal,
            MessageBody::Text {
                text: "earlier".into(),
            },
        );
        earlier.created_at = created_at;

        storage.append_message(&later).unwrap();
        storage.append_message(&earlier).unwrap();
        for message in [&later, &earlier] {
            storage
                .append_queue_entry(&QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: "default".into(),
                    priority: Priority::Normal,
                    status: QueueEntryStatus::Queued,
                    created_at: message.created_at,
                    updated_at: Utc::now(),
                })
                .unwrap();
        }

        let snapshot = storage.recovery_snapshot("default").unwrap();
        assert_eq!(
            snapshot
                .replay_messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec![later.id.as_str(), earlier.id.as_str()]
        );
    }

    #[test]
    fn recovery_snapshot_scopes_active_tasks_to_agent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();
        let task = |id: &str, agent_id: &str, offset: i64| TaskRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Running,
            created_at: now + chrono::Duration::seconds(offset),
            updated_at: now + chrono::Duration::seconds(offset),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: None,
            recovery: Some(TaskRecoverySpec::ChildAgentTask {
                summary: id.into(),
                prompt: "resume".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: crate::types::ChildAgentWorkspaceMode::Worktree,
            }),
        };

        storage
            .append_task(&task("parent-task", "default", 0))
            .unwrap();
        storage
            .append_task(&task("child-task", "child", 1))
            .unwrap();

        let snapshot = storage.recovery_snapshot("child").unwrap();
        assert_eq!(snapshot.active_tasks.len(), 1);
        assert_eq!(snapshot.active_tasks[0].id, "child-task");
    }

    #[test]
    fn storage_latest_work_items_returns_latest_record_per_id() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let item = WorkItemRecord::new("default", "fix issue #223", WorkItemState::Open);
        let mut updated = item.clone();
        updated.blocked_by = Some("working".into());
        updated.updated_at = Utc::now();

        storage.append_work_item(&item).unwrap();
        storage.append_work_item(&updated).unwrap();

        let latest = storage.latest_work_items().unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].id, item.id);
        assert_eq!(latest[0].state, WorkItemState::Open);
        assert_eq!(latest[0].blocked_by.as_deref(), Some("working"));
    }

    #[test]
    fn storage_latest_work_items_for_agent_scans_tail_until_limit() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        fs::write(
            &storage.work_items_path,
            "{not valid json and should not be parsed}\n",
        )
        .unwrap();
        let older = WorkItemRecord::new("default", "older", WorkItemState::Open);
        let mut updated = older.clone();
        updated.objective = "updated".into();
        updated.updated_at = Utc::now();
        let other_agent = WorkItemRecord::new("other", "other agent", WorkItemState::Open);
        let newest = WorkItemRecord::new("default", "newest", WorkItemState::Open);

        storage.append_work_item(&older).unwrap();
        storage.append_work_item(&updated).unwrap();
        storage.append_work_item(&other_agent).unwrap();
        storage.append_work_item(&newest).unwrap();

        let latest = storage.latest_work_items_for_agent("default", 2).unwrap();
        let objectives = latest
            .iter()
            .map(|item| item.objective.as_str())
            .collect::<Vec<_>>();

        assert_eq!(objectives, vec!["newest", "updated"]);
    }

    #[test]
    fn storage_latest_timer_record_scans_from_tail_for_id() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        fs::write(
            &storage.timers_path,
            "{not valid json and should not be parsed}\n",
        )
        .unwrap();
        let now = Utc::now();
        let older = TimerRecord {
            id: "timer-1".into(),
            agent_id: "default".into(),
            created_at: now,
            duration_ms: 1000,
            interval_ms: None,
            repeat: false,
            status: crate::types::TimerStatus::Active,
            summary: Some("older".into()),
            next_fire_at: Some(now),
            last_fired_at: None,
            fire_count: 0,
        };
        let mut updated = older.clone();
        updated.status = crate::types::TimerStatus::Completed;
        updated.summary = Some("updated".into());
        let other = TimerRecord {
            id: "timer-2".into(),
            summary: Some("other".into()),
            ..older.clone()
        };

        storage.append_timer(&older).unwrap();
        storage.append_timer(&other).unwrap();
        storage.append_timer(&updated).unwrap();

        let latest = storage
            .latest_timer_record("timer-1")
            .unwrap()
            .expect("timer should be found");
        assert_eq!(latest.status, crate::types::TimerStatus::Completed);
        assert_eq!(latest.summary.as_deref(), Some("updated"));
    }

    #[test]
    fn storage_recovery_snapshot_includes_work_item_plan_and_todo_list() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut work_item = WorkItemRecord::new("default", "fix issue #223", WorkItemState::Open);
        work_item.plan_artifact = Some(
            crate::work_item_plan::ensure_plan_artifact(
                dir.path(),
                &work_item,
                Some("Implement the new WorkItem model."),
            )
            .unwrap(),
        );
        work_item.todo_list = vec![TodoItem {
            text: "persist work item store".into(),
            state: TodoItemState::InProgress,
        }];

        storage.append_work_item(&work_item).unwrap();

        let snapshot = storage.recovery_snapshot("default").unwrap();
        assert_eq!(snapshot.work_items.len(), 1);
        assert_eq!(snapshot.work_items[0].id, work_item.id);
        assert_eq!(
            snapshot.work_items[0]
                .plan_artifact
                .as_ref()
                .map(|artifact| artifact.preview.as_str()),
            Some("Implement the new WorkItem model.")
        );
        assert_eq!(
            snapshot.work_items[0].todo_list[0].state,
            TodoItemState::InProgress
        );
    }

    #[test]
    fn storage_reads_legacy_result_checkpoint_episode_boundary() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let episode = serde_json::json!({
            "id": "ep_legacy",
            "agent_id": "default",
            "workspace_id": "agent_home",
            "created_at": "2026-04-20T00:00:00Z",
            "finalized_at": "2026-04-20T00:01:00Z",
            "start_turn_index": 3,
            "end_turn_index": 4,
            "start_message_count": 6,
            "end_message_count": 8,
            "boundary_reason": "result_checkpoint",
            "summary": "legacy episode",
        });
        fs::write(
            dir.path().join(".holon/ledger/context_episodes.jsonl"),
            format!("{}\n", serde_json::to_string(&episode).unwrap()),
        )
        .unwrap();

        let episodes = storage.read_recent_context_episodes(4).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(
            episodes[0].boundary_reason,
            EpisodeBoundaryReason::LegacyResultCheckpoint
        );
    }

    #[test]
    fn storage_work_queue_prompt_projection_uses_current_and_orders_queue() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut current = WorkItemRecord::new("default", "current item", WorkItemState::Open);
        current.updated_at = Utc::now();

        let mut waiting = WorkItemRecord::new("default", "waiting item", WorkItemState::Open);
        waiting.blocked_by = Some("external review".into());
        waiting.created_at = Utc::now() + chrono::Duration::minutes(2);
        waiting.updated_at = waiting.created_at;

        let mut queued_early = WorkItemRecord::new("default", "queued first", WorkItemState::Open);
        queued_early.created_at = Utc::now();
        queued_early.updated_at = queued_early.created_at;

        let mut queued_late = WorkItemRecord::new("default", "queued second", WorkItemState::Open);
        queued_late.created_at = Utc::now() + chrono::Duration::minutes(1);
        queued_late.updated_at = queued_late.created_at;

        let completed = WorkItemRecord::new("default", "completed", WorkItemState::Completed);

        storage.append_work_item(&current).unwrap();
        storage.append_work_item(&waiting).unwrap();
        storage.append_work_item(&queued_late).unwrap();
        storage.append_work_item(&queued_early).unwrap();
        storage.append_work_item(&completed).unwrap();
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(current.id.clone());
        storage.write_agent(&agent).unwrap();

        let projection = storage.work_queue_prompt_projection().unwrap();
        assert_eq!(
            projection
                .current
                .as_ref()
                .map(|item| item.objective.as_str()),
            Some("current item")
        );
        let rendered = projection
            .queued_blocked
            .iter()
            .map(|item| item.objective.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            vec!["queued first", "queued second", "waiting item"]
        );
    }

    #[test]
    fn db_backed_work_queue_prompt_projection_filters_by_current_agent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        runtime_db
            .work_items()
            .import_legacy(Vec::new(), None)
            .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();

        let mut current = WorkItemRecord::new("default", "current agent item", WorkItemState::Open);
        current.updated_at = Utc::now();
        let mut other_agent =
            WorkItemRecord::new("other-agent", "other agent item", WorkItemState::Open);
        other_agent.updated_at = current.updated_at + chrono::Duration::seconds(1);
        storage.append_work_item(&current).unwrap();
        storage.append_work_item(&other_agent).unwrap();
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(current.id.clone());
        storage.write_agent(&agent).unwrap();
        storage.append_work_item(&current).unwrap();

        let projection = storage.work_queue_prompt_projection().unwrap();
        assert_eq!(
            projection
                .current
                .as_ref()
                .map(|item| item.objective.as_str()),
            Some("current agent item")
        );
        assert!(projection
            .queued_blocked
            .iter()
            .all(|item| item.objective != "other agent item"));
    }

    #[test]
    fn storage_work_queue_projection_derives_candidate_classes_and_current_todo() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = Utc::now();

        let mut current = WorkItemRecord::new("default", "current runnable", WorkItemState::Open);
        current.updated_at = now;
        current.todo_list = vec![
            TodoItem {
                text: "pending current".into(),
                state: TodoItemState::Pending,
            },
            TodoItem {
                text: "active current".into(),
                state: TodoItemState::InProgress,
            },
        ];
        let mut triggered =
            WorkItemRecord::new("default", "triggered blocked", WorkItemState::Open);
        triggered.blocked_by = Some("waiting for callback".into());
        triggered.updated_at = now - chrono::Duration::minutes(1);
        let mut queued = WorkItemRecord::new("default", "queued runnable", WorkItemState::Open);
        queued.updated_at = now - chrono::Duration::minutes(3);
        let mut waiting = WorkItemRecord::new("default", "operator decision", WorkItemState::Open);
        waiting.plan_status = WorkItemPlanStatus::NeedsInput;
        waiting.updated_at = now - chrono::Duration::minutes(2);
        let mut blocked = WorkItemRecord::new("default", "plain blocked", WorkItemState::Open);
        blocked.blocked_by = Some("external review".into());
        blocked.updated_at = now - chrono::Duration::minutes(4);
        let mut completed =
            WorkItemRecord::new("default", "recently completed", WorkItemState::Completed);
        completed.updated_at = now - chrono::Duration::minutes(5);

        for item in [
            &current, &triggered, &queued, &waiting, &blocked, &completed,
        ] {
            storage.append_work_item(item).unwrap();
        }
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait-triggered".into(),
                agent_id: "default".into(),
                scope: WaitingIntentScope::WorkItem,
                work_item_id: Some(triggered.id.clone()),
                description: "triggered wait".into(),
                source: "test".into(),
                resource: None,
                condition: None,
                delivery_mode: crate::types::CallbackDeliveryMode::WakeHint,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "trigger-1".into(),
                created_at: now,
                cancelled_at: None,
                last_triggered_at: Some(now),
                trigger_count: 1,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(current.id.clone());
        storage.write_agent(&agent).unwrap();

        let projection = storage.work_queue_prompt_projection().unwrap();

        assert_eq!(
            projection
                .current_runnable
                .as_ref()
                .map(|item| item.work_item.objective.as_str()),
            Some("current runnable")
        );
        assert_eq!(
            projection
                .current_runnable
                .as_ref()
                .and_then(|item| item.current_todo.as_ref())
                .map(|todo| todo.text.as_str()),
            Some("active current")
        );
        assert_eq!(
            projection
                .triggered_blocked
                .iter()
                .map(|item| item.work_item.objective.as_str())
                .collect::<Vec<_>>(),
            vec!["triggered blocked"]
        );
        assert_eq!(
            projection
                .queued_runnable
                .iter()
                .map(|item| item.work_item.objective.as_str())
                .collect::<Vec<_>>(),
            vec!["queued runnable"]
        );
        assert_eq!(
            projection
                .waiting_for_operator
                .iter()
                .map(|item| item.work_item.objective.as_str())
                .collect::<Vec<_>>(),
            vec!["operator decision"]
        );
        assert_eq!(
            projection
                .blocked
                .iter()
                .map(|item| item.work_item.objective.as_str())
                .collect::<Vec<_>>(),
            vec!["plain blocked"]
        );
        assert_eq!(
            projection
                .completed_recent
                .iter()
                .map(|item| item.work_item.objective.as_str())
                .collect::<Vec<_>>(),
            vec!["recently completed"]
        );
    }

    #[test]
    fn storage_waiting_contract_anchor_falls_back_to_latest_waiting_when_no_active_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut waiting_old =
            WorkItemRecord::new("default", "older waiting anchor", WorkItemState::Open);
        waiting_old.blocked_by = Some("old wait".into());
        waiting_old.updated_at = Utc::now() - chrono::Duration::minutes(2);

        let mut waiting_new =
            WorkItemRecord::new("default", "newer waiting anchor", WorkItemState::Open);
        waiting_new.blocked_by = Some("new wait".into());
        waiting_new.updated_at = Utc::now();

        let mut queued = WorkItemRecord::new("default", "queued follow-up", WorkItemState::Open);
        queued.created_at = Utc::now() + chrono::Duration::minutes(1);
        queued.updated_at = queued.created_at;

        storage.append_work_item(&waiting_old).unwrap();
        storage.append_work_item(&waiting_new).unwrap();
        storage.append_work_item(&queued).unwrap();

        let anchor = storage.waiting_contract_anchor().unwrap();
        assert_eq!(
            anchor.as_ref().map(|item| item.objective.as_str()),
            Some("newer waiting anchor")
        );
        let projection = storage.work_queue_prompt_projection().unwrap();
        let rendered = projection
            .queued_blocked
            .iter()
            .map(|item| item.objective.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            vec![
                "queued follow-up",
                "older waiting anchor",
                "newer waiting anchor"
            ]
        );
        assert!(projection.current.is_none());
    }
}
