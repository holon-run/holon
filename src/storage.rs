use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use crate::types::{
    AgentIdentityRecord, AgentState, AuditEvent, BriefRecord, ContextEpisodeRecord,
    DeliverySummaryRecord, ExternalTriggerRecord, MessageEnvelope, OperatorDeliveryRecord,
    OperatorNotificationRecord, OperatorTransportBinding, QueueEntryRecord, TaskRecord,
    TimerRecord, ToolExecutionRecord, TranscriptEntry, WaitingIntentRecord,
    WorkItemDelegationRecord, WorkItemDelegationState, WorkItemRecord, WorkItemState,
    WorkPlanSnapshot, WorkingMemoryDelta, WorkspaceEntry, WorkspaceOccupancyRecord,
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
}

#[derive(Debug, Clone)]
pub struct RecoverySnapshot {
    pub agent: Option<AgentState>,
    pub replay_messages: Vec<MessageEnvelope>,
    pub active_tasks: Vec<TaskRecord>,
    pub active_timers: Vec<TimerRecord>,
    pub work_items: Vec<WorkItemRecord>,
    pub work_plans: Vec<WorkPlanSnapshot>,
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
    work_plans_path: PathBuf,
    work_item_delegations_path: PathBuf,
    timers_path: PathBuf,
    tools_path: PathBuf,
    transcript_path: PathBuf,
    queue_entries_path: PathBuf,
    waiting_intents_path: PathBuf,
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

        Ok(Self {
            events_path: ledger_dir.join("events.jsonl"),
            briefs_path: ledger_dir.join("briefs.jsonl"),
            messages_path: ledger_dir.join("messages.jsonl"),
            tasks_path: ledger_dir.join("tasks.jsonl"),
            work_items_path: ledger_dir.join("work_items.jsonl"),
            delivery_summaries_path: ledger_dir.join("delivery_summaries.jsonl"),
            work_plans_path: ledger_dir.join("work_plans.jsonl"),
            work_item_delegations_path: ledger_dir.join("work_item_delegations.jsonl"),
            timers_path: ledger_dir.join("timers.jsonl"),
            tools_path: ledger_dir.join("tools.jsonl"),
            transcript_path: ledger_dir.join("transcript.jsonl"),
            queue_entries_path: ledger_dir.join("queue_entries.jsonl"),
            waiting_intents_path: ledger_dir.join("waiting_intents.jsonl"),
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
            data_dir,
        })
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

    pub fn cache_dir(&self) -> PathBuf {
        self.runtime_dir().join(RUNTIME_CACHE_DIR)
    }

    pub fn poll_activity_marker(&self) -> Result<PollActivityMarker> {
        Ok(PollActivityMarker {
            briefs: file_activity_marker(&self.briefs_path)?,
            tasks: file_activity_marker(&self.tasks_path)?,
            tools: file_activity_marker(&self.tools_path)?,
            events: file_activity_marker(&self.events_path)?,
            transcript: file_activity_marker(&self.transcript_path)?,
        })
    }

    pub fn append_event(&self, event: &AuditEvent) -> Result<()> {
        append_jsonl(&self.events_path, event)
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        append_jsonl(&self.briefs_path, brief)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        append_jsonl(&self.messages_path, message)
    }

    pub fn append_task(&self, task: &TaskRecord) -> Result<()> {
        append_jsonl(&self.tasks_path, task)
    }

    pub fn append_work_item(&self, record: &WorkItemRecord) -> Result<()> {
        append_jsonl(&self.work_items_path, record)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        append_jsonl(&self.delivery_summaries_path, record)
    }

    pub fn append_work_plan(&self, snapshot: &WorkPlanSnapshot) -> Result<()> {
        append_jsonl(&self.work_plans_path, snapshot)
    }

    pub fn append_work_item_delegation(&self, record: &WorkItemDelegationRecord) -> Result<()> {
        append_jsonl(&self.work_item_delegations_path, record)
    }

    pub fn append_timer(&self, timer: &TimerRecord) -> Result<()> {
        append_jsonl(&self.timers_path, timer)
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        append_jsonl(&self.tools_path, record)
    }

    pub fn append_transcript_entry(&self, entry: &TranscriptEntry) -> Result<()> {
        append_jsonl(&self.transcript_path, entry)
    }

    pub fn append_queue_entry(&self, record: &QueueEntryRecord) -> Result<()> {
        append_jsonl(&self.queue_entries_path, record)
    }

    pub fn append_waiting_intent(&self, record: &WaitingIntentRecord) -> Result<()> {
        append_jsonl(&self.waiting_intents_path, record)
    }

    pub fn append_external_trigger(&self, record: &ExternalTriggerRecord) -> Result<()> {
        append_jsonl(&self.external_triggers_path, record)
    }

    pub fn append_operator_notification(&self, record: &OperatorNotificationRecord) -> Result<()> {
        append_jsonl(&self.operator_notifications_path, record)
    }

    pub fn append_operator_transport_binding(
        &self,
        record: &OperatorTransportBinding,
    ) -> Result<()> {
        append_jsonl(&self.operator_transport_bindings_path, record)
    }

    pub fn append_operator_delivery_record(&self, record: &OperatorDeliveryRecord) -> Result<()> {
        append_jsonl(&self.operator_delivery_records_path, record)
    }

    pub fn append_working_memory_delta(&self, record: &WorkingMemoryDelta) -> Result<()> {
        append_jsonl(&self.working_memory_deltas_path, record)
    }

    pub fn append_context_episode(&self, record: &ContextEpisodeRecord) -> Result<()> {
        append_jsonl(&self.context_episodes_path, record)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_workspace_entry(&self, entry: &WorkspaceEntry) -> Result<()> {
        append_jsonl(&self.workspaces_path, entry)?;
        self.mark_memory_index_dirty()
    }

    pub fn append_workspace_occupancy(&self, entry: &WorkspaceOccupancyRecord) -> Result<()> {
        append_jsonl(&self.occupancies_path, entry)
    }

    pub fn append_agent_identity(&self, entry: &AgentIdentityRecord) -> Result<()> {
        append_jsonl(&self.agent_identities_path, entry)
    }

    pub fn mark_memory_index_dirty(&self) -> Result<()> {
        fs::create_dir_all(self.indexes_dir())?;
        fs::write(self.indexes_dir().join("memory.dirty"), b"dirty")
            .with_context(|| "failed to mark memory index dirty")
    }

    pub fn write_agent(&self, agent: &AgentState) -> Result<()> {
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
        read_recent_jsonl(&self.events_path, limit)
    }

    pub fn read_recent_briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        read_recent_jsonl(&self.briefs_path, limit)
    }

    pub fn read_recent_messages(&self, limit: usize) -> Result<Vec<MessageEnvelope>> {
        read_recent_jsonl(&self.messages_path, limit)
    }

    /// Reads messages at or after `offset`, then returns only the most recent
    /// `limit` entries from that range.
    ///
    /// This is not equivalent to returning the first `limit` messages starting
    /// at `offset`; it preserves recent-message window semantics.
    pub fn read_messages_from(&self, offset: usize, limit: usize) -> Result<Vec<MessageEnvelope>> {
        read_jsonl_from(&self.messages_path, offset, limit)
    }

    pub fn read_all_messages(&self) -> Result<Vec<MessageEnvelope>> {
        read_recent_jsonl(&self.messages_path, usize::MAX)
    }

    pub fn read_recent_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        read_recent_jsonl(&self.tasks_path, limit)
    }

    pub fn read_recent_work_items(&self, limit: usize) -> Result<Vec<WorkItemRecord>> {
        read_recent_jsonl(&self.work_items_path, limit)
    }

    pub fn read_recent_delivery_summaries(
        &self,
        limit: usize,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        read_recent_jsonl(&self.delivery_summaries_path, limit)
    }

    pub fn read_recent_work_plans(&self, limit: usize) -> Result<Vec<WorkPlanSnapshot>> {
        read_recent_jsonl(&self.work_plans_path, limit)
    }

    pub fn read_recent_work_item_delegations(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkItemDelegationRecord>> {
        read_recent_jsonl(&self.work_item_delegations_path, limit)
    }

    pub fn read_recent_timers(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        read_recent_jsonl(&self.timers_path, limit)
    }

    pub fn read_recent_tool_executions(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>> {
        read_recent_jsonl(&self.tools_path, limit)
    }

    pub fn read_recent_transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        read_recent_jsonl(&self.transcript_path, limit)
    }

    pub fn read_all_transcript(&self) -> Result<Vec<TranscriptEntry>> {
        read_recent_jsonl(&self.transcript_path, usize::MAX)
    }

    pub fn read_recent_waiting_intents(&self, limit: usize) -> Result<Vec<WaitingIntentRecord>> {
        read_recent_jsonl(&self.waiting_intents_path, limit)
    }

    pub fn read_recent_queue_entries(&self, limit: usize) -> Result<Vec<QueueEntryRecord>> {
        read_recent_jsonl(&self.queue_entries_path, limit)
    }

    pub fn read_recent_external_triggers(
        &self,
        limit: usize,
    ) -> Result<Vec<ExternalTriggerRecord>> {
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
        read_recent_jsonl(&self.workspaces_path, limit)
    }

    pub fn read_recent_workspace_occupancies(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkspaceOccupancyRecord>> {
        read_recent_jsonl(&self.occupancies_path, limit)
    }

    pub fn read_recent_agent_identities(&self, limit: usize) -> Result<Vec<AgentIdentityRecord>> {
        read_recent_jsonl(&self.agent_identities_path, limit)
    }

    pub fn latest_task_records(&self) -> Result<Vec<TaskRecord>> {
        let records = self.read_recent_tasks(usize::MAX)?;
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

    pub fn latest_task_record(&self, task_id: &str) -> Result<Option<TaskRecord>> {
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

    pub fn work_queue_prompt_projection(&self) -> Result<WorkQueuePromptProjection> {
        if !self.work_items_path.exists() {
            return Ok(WorkQueuePromptProjection::default());
        }
        let current_work_item_id = self
            .read_agent()?
            .and_then(|agent| agent.current_work_item_id);

        let content = fs::read_to_string(&self.work_items_path)
            .with_context(|| format!("failed to read {}", self.work_items_path.display()))?;
        let mut latest = std::collections::HashMap::<String, WorkItemRecord>::new();
        for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
            let record: WorkItemRecord = serde_json::from_str(line).with_context(|| {
                format!(
                    "failed to decode line from {}",
                    self.work_items_path.display()
                )
            })?;
            latest.entry(record.id.clone()).or_insert(record);
        }

        let current = current_work_item_id
            .as_deref()
            .and_then(|id| latest.get(id))
            .filter(|item| item.state == WorkItemState::Open)
            .cloned();

        let mut queued_blocked = latest
            .values()
            .filter(|item| {
                item.state == WorkItemState::Open
                    && Some(item.id.as_str()) != current_work_item_id.as_deref()
            })
            .cloned()
            .collect::<Vec<_>>();
        queued_blocked.sort_by(compare_queue_display_order);

        Ok(WorkQueuePromptProjection {
            current,
            queued_blocked,
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

    pub fn latest_work_item(&self, work_item_id: &str) -> Result<Option<WorkItemRecord>> {
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

    pub fn latest_work_plans(&self) -> Result<Vec<WorkPlanSnapshot>> {
        let records = self.read_recent_work_plans(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.work_item_id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_work_plan(&self, work_item_id: &str) -> Result<Option<WorkPlanSnapshot>> {
        if !self.work_plans_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.work_plans_path)
            .with_context(|| format!("failed to read {}", self.work_plans_path.display()))?;
        for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
            let record: WorkPlanSnapshot = serde_json::from_str(line).with_context(|| {
                format!(
                    "failed to decode line from {}",
                    self.work_plans_path.display()
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

    pub fn latest_timer_records(&self) -> Result<Vec<TimerRecord>> {
        let records = self.read_recent_timers(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.id.clone(), record);
        }
        Ok(latest.into_values().collect())
    }

    pub fn latest_waiting_intents(&self) -> Result<Vec<WaitingIntentRecord>> {
        let records = self.read_recent_waiting_intents(usize::MAX)?;
        let mut latest = std::collections::BTreeMap::new();
        for record in records {
            latest.insert(record.id.clone(), record);
        }
        Ok(latest.into_values().collect())
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

    pub fn recovery_snapshot(&self) -> Result<RecoverySnapshot> {
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
                | crate::types::QueueEntryStatus::Dropped => None,
            })
            .collect::<Vec<_>>();
        replay_messages.sort_by(|left, right| left.created_at.cmp(&right.created_at));

        let active_tasks = self
            .latest_task_records()?
            .into_iter()
            .filter(|record| {
                matches!(
                    record.status,
                    crate::types::TaskStatus::Queued
                        | crate::types::TaskStatus::Running
                        | crate::types::TaskStatus::Cancelling
                )
            })
            .collect();
        let active_timers = self
            .latest_timer_records()?
            .into_iter()
            .filter(|record| record.status == crate::types::TimerStatus::Active)
            .collect();
        let work_items = self.latest_work_items()?;
        let work_plans = self.latest_work_plans()?;
        let work_item_delegations = self.latest_work_item_delegations()?;

        Ok(RecoverySnapshot {
            agent,
            replay_messages,
            active_tasks,
            active_timers,
            work_items,
            work_plans,
            work_item_delegations,
        })
    }

    pub fn count_briefs(&self) -> Result<usize> {
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

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let line = serde_json::to_string(value)?;
    writeln!(file, "{line}")?;
    Ok(())
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

fn compare_timestamp_asc(left: DateTime<Utc>, right: DateTime<Utc>) -> std::cmp::Ordering {
    left.cmp(&right)
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
        AgentState, AgentStatus, EpisodeBoundaryReason, Priority, QueueEntryRecord,
        QueueEntryStatus, TranscriptEntry, TranscriptEntryKind, WorkItemRecord, WorkItemState,
        WorkPlanItem, WorkPlanSnapshot, WorkPlanStepStatus,
    };

    use super::*;

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
            crate::types::TrustLevel::TrustedIntegration,
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
            crate::types::TrustLevel::TrustedIntegration,
            Priority::Normal,
            crate::types::MessageBody::Text {
                text: "done".into(),
            },
        );
        storage.append_message(&queued).unwrap();
        storage.append_message(&done).unwrap();
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

        let snapshot = storage.recovery_snapshot().unwrap();
        assert_eq!(snapshot.replay_messages.len(), 1);
        assert_eq!(snapshot.replay_messages[0].id, queued.id);
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
    fn storage_latest_work_plan_returns_latest_snapshot_per_work_item() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let first = WorkPlanSnapshot::new(
            "default",
            "work_1",
            vec![WorkPlanItem {
                step: "inspect".into(),
                status: WorkPlanStepStatus::Pending,
            }],
        );
        let second = WorkPlanSnapshot::new(
            "default",
            "work_1",
            vec![WorkPlanItem {
                step: "inspect".into(),
                status: WorkPlanStepStatus::Completed,
            }],
        );

        storage.append_work_plan(&first).unwrap();
        storage.append_work_plan(&second).unwrap();

        let latest = storage.latest_work_plan("work_1").unwrap().unwrap();
        assert_eq!(latest.items.len(), 1);
        assert_eq!(latest.items[0].status, WorkPlanStepStatus::Completed);
    }

    #[test]
    fn storage_recovery_snapshot_includes_work_items_and_plans() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let work_item = WorkItemRecord::new("default", "fix issue #223", WorkItemState::Open);
        let work_plan = WorkPlanSnapshot::new(
            "default",
            work_item.id.clone(),
            vec![WorkPlanItem {
                step: "persist work item store".into(),
                status: WorkPlanStepStatus::InProgress,
            }],
        );

        storage.append_work_item(&work_item).unwrap();
        storage.append_work_plan(&work_plan).unwrap();

        let snapshot = storage.recovery_snapshot().unwrap();
        assert_eq!(snapshot.work_items.len(), 1);
        assert_eq!(snapshot.work_items[0].id, work_item.id);
        assert_eq!(snapshot.work_plans.len(), 1);
        assert_eq!(snapshot.work_plans[0].work_item_id, work_plan.work_item_id);
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

        let completed = WorkItemRecord::new("default", "completed", WorkItemState::Done);

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
                .map(|item| item.delivery_target.as_str()),
            Some("current item")
        );
        let rendered = projection
            .queued_blocked
            .iter()
            .map(|item| item.delivery_target.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            vec!["queued first", "queued second", "waiting item"]
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
            anchor.as_ref().map(|item| item.delivery_target.as_str()),
            Some("newer waiting anchor")
        );
        let projection = storage.work_queue_prompt_projection().unwrap();
        let rendered = projection
            .queued_blocked
            .iter()
            .map(|item| item.delivery_target.as_str())
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
