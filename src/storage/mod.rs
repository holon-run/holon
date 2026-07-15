use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, Notify};

use crate::{
    memory::index::enqueue_memory_index_upsert,
    runtime_db::{RuntimeDb, RuntimeIndexChange, RuntimeIndexOperation},
    tool::helpers::{command_output_source_ref, command_receipt_source_ref},
    types::{
        AgentIdentityRecord, AgentPostureProjection, AgentSchedulingPosture, AgentState,
        AgentStatus, AuditEvent, BriefKind, BriefRecord, ContextEpisodeRecord,
        DeliverySummaryRecord, ExternalTriggerRecord, MessageEnvelope, OperatorDeliveryRecord,
        OperatorNotificationRecord, OperatorTransportBinding, QueueEntryRecord, TaskRecord,
        TaskStatus, TimerRecord, ToolExecutionRecord, TranscriptEntry, TurnRecord,
        WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WorkItemContinuationFrame,
        WorkItemDelegationRecord, WorkItemDelegationState, WorkItemRecord, WorkItemSchedulingState,
        WorkItemState, WorkspaceEntry, WorkspaceOccupancyRecord,
    },
};

const RUNTIME_DIR: &str = ".holon";
const RUNTIME_STATE_DIR: &str = "state";
const RUNTIME_LEDGER_DIR: &str = "ledger";
const RUNTIME_INDEXES_DIR: &str = "indexes";
const RUNTIME_CACHE_DIR: &str = "cache";

mod activity;
mod events;
mod memory;
mod recovery;
mod work_queue;

pub use activity::{FileActivityMarker, PollActivityMarker};
pub(crate) use events::{EventBus, EventLogPage, EventLogPageOrder, PublishedAuditEvent};
pub use recovery::RecoverySnapshot;
pub use work_queue::{
    WorkItemCandidateClass, WorkItemReadinessProjection, WorkQueuePromptProjection,
};

// Re-import submodule functions so existing impl AppStorage method bodies compile unchanged.

/// Truncate a descending-ordered Vec to `limit` items and reverse to ascending order.
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

use memory::memory_index_agent_key;
use recovery::external_wait_recoverability_event;
use work_queue::{
    compare_queue_display_order, compare_readiness_projection_order, current_todo,
    readiness_for_scheduling_state,
};

use work_queue::ActiveWaitConditionStates;

#[derive(Debug, Clone)]
pub struct AppStorage {
    data_dir: PathBuf,
    agent_id: Option<String>,
    read_only: bool,
    agent_path: PathBuf,
    append_mutex: Arc<Mutex<()>>,
    event_seq_counter: Arc<Mutex<u64>>,
    message_seq_counter: Arc<Mutex<u64>>,
    transcript_seq_counter: Arc<Mutex<u64>>,
    audit_event_index: Arc<Mutex<Option<AuditEventIndexSink>>>,
    runtime_db: RuntimeDb,
    event_bus: Arc<Mutex<Option<EventBus>>>,
    /// Optional shared notify for the daemon-level memory indexer.
    memory_index_notify: Arc<Mutex<Option<Arc<Notify>>>>,
}

#[derive(Debug, Clone)]
struct AuditEventIndexSink {
    runtime_db: RuntimeDb,
    agent_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageOpenMode {
    ReadWrite,
    ReadOnly,
}

impl AppStorage {
    pub fn new(data_dir: impl Into<PathBuf>, runtime_db: RuntimeDb) -> Result<Self> {
        let data_dir = data_dir.into();
        let agent_id = infer_agent_id_from_data_dir(&data_dir);
        Self::new_with_scope(data_dir, agent_id, runtime_db)
    }

    /// Test-only constructor that opens its own RuntimeDb from the data dir.
    pub fn new_for_test(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        let runtime_dir = data_dir.join(RUNTIME_DIR);
        let runtime_db = RuntimeDb::open_and_migrate(
            runtime_dir.join("state/runtime.sqlite"),
            runtime_dir.join("state/runtime.lock"),
        )?;
        Self::new_for_agent(data_dir, "default".to_string(), runtime_db)
    }

    /// Test-only constructor that opens its own RuntimeDb and scopes storage
    /// to the given agent id. Use this when the test writes agent state for a
    /// non-`default` session id.
    pub fn new_for_test_with_agent(
        data_dir: impl Into<PathBuf>,
        agent_id: impl Into<String>,
    ) -> Result<Self> {
        let data_dir = data_dir.into();
        let runtime_dir = data_dir.join(RUNTIME_DIR);
        let runtime_db = RuntimeDb::open_and_migrate(
            runtime_dir.join("state/runtime.sqlite"),
            runtime_dir.join("state/runtime.lock"),
        )?;
        Self::new_for_agent(data_dir, agent_id, runtime_db)
    }
    pub fn new_for_agent_for_test(
        data_dir: impl Into<PathBuf>,
        agent_id: impl Into<String>,
    ) -> Result<Self> {
        let data_dir = data_dir.into();
        let runtime_dir = data_dir.join(RUNTIME_DIR);
        let runtime_db = RuntimeDb::open_and_migrate(
            runtime_dir.join("state/runtime.sqlite"),
            runtime_dir.join("state/runtime.lock"),
        )?;
        Self::new_for_agent(data_dir, agent_id, runtime_db)
    }

    /// Test-only global constructor that opens its own RuntimeDb.
    pub fn new_global_for_test(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        let runtime_dir = data_dir.join(RUNTIME_DIR);
        let runtime_db = RuntimeDb::open_and_migrate(
            runtime_dir.join("state/runtime.sqlite"),
            runtime_dir.join("state/runtime.lock"),
        )?;
        Self::new_global(data_dir, runtime_db)
    }

    pub fn new_for_agent(
        data_dir: impl Into<PathBuf>,
        agent_id: impl Into<String>,
        runtime_db: RuntimeDb,
    ) -> Result<Self> {
        let agent_id = agent_id.into();
        anyhow::ensure!(
            !agent_id.trim().is_empty(),
            "agent-scoped storage requires a non-empty agent id"
        );
        Self::new_with_scope(data_dir, Some(agent_id), runtime_db)
    }

    pub fn open_read_only_for_agent(
        data_dir: impl Into<PathBuf>,
        agent_id: impl Into<String>,
        runtime_db: RuntimeDb,
    ) -> Result<Self> {
        let agent_id = agent_id.into();
        anyhow::ensure!(
            !agent_id.trim().is_empty(),
            "agent-scoped storage requires a non-empty agent id"
        );
        Self::new_with_scope_options(
            data_dir,
            Some(agent_id),
            StorageOpenMode::ReadOnly,
            runtime_db,
        )
    }

    pub fn new_global(data_dir: impl Into<PathBuf>, runtime_db: RuntimeDb) -> Result<Self> {
        Self::new_with_scope(data_dir, None, runtime_db)
    }

    fn new_with_scope(
        data_dir: impl Into<PathBuf>,
        agent_id: Option<String>,
        runtime_db: RuntimeDb,
    ) -> Result<Self> {
        Self::new_with_scope_options(data_dir, agent_id, StorageOpenMode::ReadWrite, runtime_db)
    }

    fn new_with_scope_options(
        data_dir: impl Into<PathBuf>,
        agent_id: Option<String>,
        mode: StorageOpenMode,
        runtime_db: RuntimeDb,
    ) -> Result<Self> {
        let data_dir = data_dir.into();
        let runtime_dir = data_dir.join(RUNTIME_DIR);
        let state_dir = runtime_dir.join(RUNTIME_STATE_DIR);
        let ledger_dir = runtime_dir.join(RUNTIME_LEDGER_DIR);
        if mode == StorageOpenMode::ReadWrite {
            fs::create_dir_all(&data_dir)
                .with_context(|| format!("failed to create {}", data_dir.display()))?;
            for dir in [
                &state_dir,
                &ledger_dir,
                &runtime_dir.join(RUNTIME_INDEXES_DIR),
                &runtime_dir.join(RUNTIME_CACHE_DIR),
            ] {
                fs::create_dir_all(dir)
                    .with_context(|| format!("failed to create {}", dir.display()))?;
            }
        }

        let event_seq_counter = runtime_db
            .audit_events()
            .max_event_seq(agent_id.as_deref())?;
        let message_seq_counter = runtime_db.messages().max_message_seq(agent_id.as_deref())?;
        let transcript_seq_counter = runtime_db
            .transcript_entries()
            .max_transcript_seq(agent_id.as_deref())?;

        Ok(Self {
            agent_id,
            read_only: mode == StorageOpenMode::ReadOnly,
            agent_path: state_dir.join("agent.json"),
            append_mutex: Arc::new(Mutex::new(())),
            event_seq_counter: Arc::new(Mutex::new(event_seq_counter)),
            message_seq_counter: Arc::new(Mutex::new(message_seq_counter)),
            transcript_seq_counter: Arc::new(Mutex::new(transcript_seq_counter)),
            audit_event_index: Arc::new(Mutex::new(None)),
            runtime_db,
            event_bus: Arc::new(Mutex::new(None)),
            data_dir,
            memory_index_notify: Arc::new(Mutex::new(None)),
        })
    }

    fn ensure_writable(&self) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "cannot write through read-only runtime storage"
        );
        Ok(())
    }

    pub(crate) fn enable_audit_event_index(
        &self,
        runtime_db: RuntimeDb,
        agent_id: Option<String>,
    ) -> Result<()> {
        self.ensure_writable()?;
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

    pub(crate) fn enable_event_bus(&self, event_bus: EventBus) -> Result<()> {
        let mut guard = self
            .event_bus
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus mutex poisoned"))?;
        *guard = Some(event_bus);
        Ok(())
    }

    /// Attach a shared memory-index notify so the daemon-level indexer is
    /// woken immediately after new evidence is enqueued for indexing.
    pub(crate) fn enable_memory_index_notify(&self, notify: Arc<Notify>) -> Result<()> {
        let mut guard = self
            .memory_index_notify
            .lock()
            .map_err(|_| anyhow::anyhow!("memory index notify mutex poisoned"))?;
        *guard = Some(notify);
        Ok(())
    }

    pub(crate) fn subscribe_events(
        &self,
    ) -> Result<Option<broadcast::Receiver<PublishedAuditEvent>>> {
        Ok(self
            .event_bus
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus mutex poisoned"))?
            .as_ref()
            .map(EventBus::subscribe))
    }

    fn publish_event(&self, agent_id: Option<String>, event: &AuditEvent) -> Result<()> {
        if let Some(event_bus) = self
            .event_bus
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus mutex poisoned"))?
            .clone()
        {
            event_bus.publish(PublishedAuditEvent {
                agent_id,
                event: event.clone(),
            });
        }
        Ok(())
    }

    #[cfg(test)]
    fn flush_audit_event_writes_for_tests(&self) -> Result<()> {
        if let Some(sink) = self
            .audit_event_index
            .lock()
            .map_err(|_| anyhow::anyhow!("audit event index mutex poisoned"))?
            .clone()
        {
            sink.runtime_db.flush_background_writes_for_tests()?;
        }
        self.runtime_db.flush_background_writes_for_tests()?;
        Ok(())
    }

    pub(crate) fn current_agent_id(&self) -> Result<Option<String>> {
        Ok(self.agent_id.clone())
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
        Ok(Some(self.runtime_db.clone()))
    }

    pub fn poll_activity_marker(&self) -> Result<PollActivityMarker> {
        let empty = FileActivityMarker::default();
        Ok(PollActivityMarker {
            briefs: empty.clone(),
            tasks: self.tasks_activity_marker()?,
            tools: empty.clone(),
            events: self.audit_events_activity_marker()?,
            transcript: empty,
        })
    }

    fn tasks_activity_marker(&self) -> Result<FileActivityMarker> {
        let runtime_db = self.runtime_db.clone();
        let (task_count, latest_updated_ms) = runtime_db
            .tasks()
            .activity_watermark_for_agent(self.current_agent_id()?.as_deref())?;
        return Ok(FileActivityMarker {
            exists: task_count > 0,
            len: task_count,
            modified_unix_ms: latest_updated_ms,
        });
    }

    fn audit_events_activity_marker(&self) -> Result<FileActivityMarker> {
        let runtime_db = self.runtime_db.clone();
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

    pub fn append_event(&self, event: &AuditEvent) -> Result<()> {
        let started = std::time::Instant::now();
        self.ensure_writable()?;
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        let result = self.append_event_with_append_mutex_held(event);
        crate::diagnostics::record_storage_append_event(started.elapsed());
        result
    }

    fn append_event_with_append_mutex_held(&self, event: &AuditEvent) -> Result<()> {
        self.ensure_writable()?;
        let mut event = event.clone();
        let mut counter = self
            .event_seq_counter
            .lock()
            .map_err(|_| anyhow::anyhow!("event sequence counter mutex poisoned"))?;
        event.event_seq = counter
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("event sequence counter overflow"))?;
        let sink = self
            .audit_event_index
            .lock()
            .map_err(|_| anyhow::anyhow!("audit event index mutex poisoned"))?
            .clone();
        let agent_id = if let Some(sink) = sink {
            let agent_id = sink.agent_id.clone();
            sink.runtime_db
                .audit_events()
                .append(agent_id.as_deref(), &event)?;
            agent_id
        } else {
            let agent_id = self.current_agent_id()?;
            self.runtime_db
                .audit_events()
                .append(agent_id.as_deref(), &event)?;
            agent_id
        };
        *counter = event.event_seq;
        if let Err(error) = self.publish_event(agent_id.clone(), &event) {
            tracing::warn!(
                error = %error,
                event_id = %event.id,
                event_kind = %event.kind,
                event_seq = event.event_seq,
                agent_id = agent_id.as_deref().unwrap_or("<global>"),
                "failed to publish committed audit event"
            );
        }
        Ok(())
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let changes = self.index_changes_for_brief(brief)?;
        runtime_db
            .evidence()
            .append_brief_with_index_changes(brief, &changes)?;
        self.enqueue_memory_index_brief_best_effort(brief)
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        let mut message = message.clone();
        {
            let _guard = self
                .append_mutex
                .lock()
                .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
            let mut counter = self
                .message_seq_counter
                .lock()
                .map_err(|_| anyhow::anyhow!("message sequence counter mutex poisoned"))?;
            *counter += 1;
            message.message_seq = Some(*counter);
            drop(counter);
            let runtime_db = self.runtime_db.clone();
            let changes = self.index_changes_for_message(&message)?;
            runtime_db
                .messages()
                .upsert_with_index_changes(&message, &changes)?;
        }
        self.enqueue_memory_index_message_best_effort(&message)
    }

    pub fn append_task(&self, task: &TaskRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let changes = self.index_changes_for_task(task)?;
        runtime_db
            .tasks()
            .upsert_with_index_changes(task, &changes)?;
        self.enqueue_memory_index_task_best_effort(task)
    }

    pub fn append_work_item(&self, record: &WorkItemRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let current_focus = self
            .read_agent()?
            .and_then(|agent| agent.current_work_item_id)
            .as_deref()
            == Some(record.id.as_str());
        let changes = self.index_changes_for_work_item(record)?;
        runtime_db
            .work_items()
            .upsert_with_index_changes(record, current_focus, &changes)?;
        self.enqueue_memory_index_work_item_best_effort(record)
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.evidence().append_delivery_summary(record)?;
        return Ok(());
    }

    pub fn append_work_item_delegation(&self, record: &WorkItemDelegationRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.work_item_delegations().upsert(record)?;
        return Ok(());
    }

    pub fn append_work_item_continuation(&self, record: &WorkItemContinuationFrame) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.work_item_continuations().upsert(record)?;
        return Ok(());
    }

    pub fn append_timer(&self, timer: &TimerRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.timers().upsert(timer)?;
        return Ok(());
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let changes = self.index_changes_for_tool_execution(record)?;
        runtime_db
            .evidence()
            .append_tool_execution_with_index_changes(record, &changes)?;
        self.enqueue_memory_index_tool_execution_best_effort(record)
    }

    pub fn append_turn(&self, record: &TurnRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.turn_records().upsert(record)?;
        return Ok(());
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
        let runtime_db = self.runtime_db.clone();
        runtime_db.transcript_entries().upsert(&entry)?;
        return Ok(());
    }

    pub fn append_queue_entry(&self, record: &QueueEntryRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.queue_entries().upsert(record)?;
        return Ok(());
    }

    pub fn try_claim_queued_message(&self, record: &QueueEntryRecord) -> Result<bool> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.queue_entries().try_claim_queued_message(record);
    }

    pub fn append_wait_condition(&self, record: &WaitConditionRecord) -> Result<()> {
        let event = external_wait_recoverability_event(record);

        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        let runtime_db = self.runtime_db.clone();
        runtime_db.wait_conditions().upsert(record)?;
        if let Some(event) = event.as_ref() {
            self.append_event_with_append_mutex_held(event)?;
        }
        return Ok(());
    }

    pub fn append_external_trigger(&self, record: &ExternalTriggerRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.external_triggers().upsert(record)?;
        Ok(())
    }

    pub fn append_operator_notification(&self, record: &OperatorNotificationRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let agent_id = self.current_agent_id()?.ok_or_else(|| {
            anyhow::anyhow!("agent_id is required for operator_notification persistence")
        })?;
        runtime_db
            .operator_notifications()
            .insert(&agent_id, record)
    }

    pub fn append_operator_transport_binding(
        &self,
        record: &OperatorTransportBinding,
    ) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let agent_id = self.current_agent_id()?.ok_or_else(|| {
            anyhow::anyhow!("agent_id is required for operator_transport_binding persistence")
        })?;
        runtime_db
            .operator_transport_bindings()
            .upsert(&agent_id, record)
    }

    pub fn append_operator_delivery_record(&self, record: &OperatorDeliveryRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let agent_id = self.current_agent_id()?.ok_or_else(|| {
            anyhow::anyhow!("agent_id is required for operator_delivery_record persistence")
        })?;
        runtime_db
            .operator_delivery_records()
            .upsert(&agent_id, record)
    }

    pub fn append_context_episode(&self, record: &ContextEpisodeRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let changes = self.index_changes_for_context_episode(record)?;
        runtime_db
            .context_episodes()
            .upsert_with_index_changes(record, &changes)?;
        self.enqueue_memory_index_context_episode_best_effort(record)
    }

    pub fn append_workspace_entry(&self, entry: &WorkspaceEntry) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        let changes = self.index_changes_for_workspace_entry(entry)?;
        runtime_db
            .workspace_entries()
            .upsert_with_index_changes(entry, &changes)?;
        self.enqueue_memory_index_workspace_entry_best_effort(entry)
    }

    pub fn append_workspace_occupancy(&self, entry: &WorkspaceOccupancyRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.workspace_occupancies().upsert(entry)?;
        Ok(())
    }

    pub fn append_agent_identity(&self, entry: &AgentIdentityRecord) -> Result<()> {
        let runtime_db = self.runtime_db.clone();
        runtime_db.agent_identities().upsert(entry)
    }

    pub fn mark_memory_index_dirty(&self) -> Result<()> {
        self.ensure_writable()?;
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

    fn runtime_index_upsert(
        &self,
        agent_id: impl Into<String>,
        source_kind: impl Into<String>,
        source_id: impl Into<String>,
        source_ref: impl Into<String>,
        source_updated_at: Option<DateTime<Utc>>,
        reason: &'static str,
    ) -> Result<RuntimeIndexChange> {
        Ok(RuntimeIndexChange {
            agent_id: agent_id.into(),
            source_kind: source_kind.into(),
            source_id: source_id.into(),
            source_ref: source_ref.into(),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at,
            reason: reason.into(),
        })
    }

    fn index_changes_for_brief(&self, brief: &BriefRecord) -> Result<Vec<RuntimeIndexChange>> {
        if brief.kind == BriefKind::Ack || brief.text.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![self.runtime_index_upsert(
            brief.agent_id.clone(),
            "brief",
            brief.id.clone(),
            format!("brief:{}", brief.id),
            Some(brief.created_at),
            "brief_written",
        )?])
    }

    fn index_changes_for_message(
        &self,
        message: &MessageEnvelope,
    ) -> Result<Vec<RuntimeIndexChange>> {
        Ok(vec![self.runtime_index_upsert(
            message.agent_id.clone(),
            "message",
            message.id.clone(),
            format!("message:{}", message.id),
            Some(message.created_at),
            "message_written",
        )?])
    }

    fn index_changes_for_task(&self, task: &TaskRecord) -> Result<Vec<RuntimeIndexChange>> {
        Ok(vec![self.runtime_index_upsert(
            task.agent_id.clone(),
            "task",
            task.id.clone(),
            format!("task:{}", task.id),
            Some(task.updated_at),
            "task_written",
        )?])
    }

    fn index_changes_for_work_item(
        &self,
        record: &WorkItemRecord,
    ) -> Result<Vec<RuntimeIndexChange>> {
        Ok(vec![self.runtime_index_upsert(
            record.agent_id.clone(),
            "work_item",
            record.id.clone(),
            format!("work_item:{}", record.id),
            Some(record.updated_at),
            "work_item_written",
        )?])
    }

    fn index_changes_for_context_episode(
        &self,
        record: &ContextEpisodeRecord,
    ) -> Result<Vec<RuntimeIndexChange>> {
        Ok(vec![self.runtime_index_upsert(
            record.agent_id.clone(),
            "context_episode",
            record.id.clone(),
            format!("episode:{}", record.id),
            Some(record.finalized_at),
            "context_episode_written",
        )?])
    }

    fn index_changes_for_workspace_entry(
        &self,
        entry: &WorkspaceEntry,
    ) -> Result<Vec<RuntimeIndexChange>> {
        let Some(agent_id) = entry
            .owner_agent_id
            .clone()
            .or_else(|| self.storage_agent_id().ok())
        else {
            return Ok(Vec::new());
        };
        Ok(vec![self.runtime_index_upsert(
            agent_id,
            "workspace_profile",
            entry.workspace_id.clone(),
            format!("workspace_profile:{}", entry.workspace_id),
            Some(entry.updated_at),
            "workspace_profile_written",
        )?])
    }

    fn index_changes_for_tool_execution(
        &self,
        record: &ToolExecutionRecord,
    ) -> Result<Vec<RuntimeIndexChange>> {
        let mut changes = Vec::new();
        match record.tool_name.as_str() {
            crate::tool::names::EXEC_COMMAND => {
                if record.input.get("cmd").and_then(Value::as_str).is_some() {
                    let source_ref = command_receipt_source_ref(&record.id, None);
                    changes.push(self.runtime_index_upsert(
                        record.agent_id.clone(),
                        "tool_command_receipt",
                        source_ref.clone(),
                        source_ref,
                        record.completed_at.or(Some(record.created_at)),
                        "tool_command_receipt_written",
                    )?);
                }
            }
            crate::tool::names::EXEC_COMMAND_BATCH => {
                if let Some(items) = record.input.get("items").and_then(Value::as_array) {
                    for (offset, item) in items.iter().enumerate() {
                        if item.get("cmd").and_then(Value::as_str).is_some() {
                            let index = offset + 1;
                            let source_ref = command_receipt_source_ref(&record.id, Some(index));
                            changes.push(self.runtime_index_upsert(
                                record.agent_id.clone(),
                                "tool_command_receipt",
                                source_ref.clone(),
                                source_ref,
                                record.completed_at.or(Some(record.created_at)),
                                "tool_command_receipt_written",
                            )?);
                        }
                    }
                }
            }
            _ => {
                let source_ref = command_output_source_ref(&record.id, None, "output");
                changes.push(self.runtime_index_upsert(
                    record.agent_id.clone(),
                    "tool_execution_output_preview",
                    source_ref.clone(),
                    source_ref,
                    record.completed_at.or(Some(record.created_at)),
                    "tool_execution_output_preview_written",
                )?);
            }
        }
        Ok(changes)
    }

    fn enqueue_memory_index_brief(&self, brief: &BriefRecord) -> Result<()> {
        self.enqueue_memory_index_source("brief", &brief.id, &format!("brief:{}", brief.id))
    }

    fn enqueue_memory_index_brief_best_effort(&self, brief: &BriefRecord) -> Result<()> {
        let result = self.enqueue_memory_index_brief(brief);
        self.finish_memory_index_enqueue(result, "brief", &brief.id, &format!("brief:{}", brief.id))
    }

    fn enqueue_memory_index_message(&self, message: &MessageEnvelope) -> Result<()> {
        self.enqueue_memory_index_source("message", &message.id, &format!("message:{}", message.id))
    }

    fn enqueue_memory_index_message_best_effort(&self, message: &MessageEnvelope) -> Result<()> {
        let result = self.enqueue_memory_index_message(message);
        self.finish_memory_index_enqueue(
            result,
            "message",
            &message.id,
            &format!("message:{}", message.id),
        )
    }

    fn enqueue_memory_index_task(&self, task: &TaskRecord) -> Result<()> {
        self.enqueue_memory_index_source("task", &task.id, &format!("task:{}", task.id))
    }

    fn enqueue_memory_index_task_best_effort(&self, task: &TaskRecord) -> Result<()> {
        let result = self.enqueue_memory_index_task(task);
        self.finish_memory_index_enqueue(result, "task", &task.id, &format!("task:{}", task.id))
    }

    fn enqueue_memory_index_work_item(&self, record: &WorkItemRecord) -> Result<()> {
        self.enqueue_memory_index_source(
            "work_item",
            &record.id,
            &format!("work_item:{}", record.id),
        )
    }

    fn enqueue_memory_index_work_item_best_effort(&self, record: &WorkItemRecord) -> Result<()> {
        let result = self.enqueue_memory_index_work_item(record);
        self.finish_memory_index_enqueue(
            result,
            "work_item",
            &record.id,
            &format!("work_item:{}", record.id),
        )
    }

    fn enqueue_memory_index_context_episode(&self, record: &ContextEpisodeRecord) -> Result<()> {
        self.enqueue_memory_index_source(
            "context_episode",
            &record.id,
            &format!("episode:{}", record.id),
        )
    }

    fn enqueue_memory_index_context_episode_best_effort(
        &self,
        record: &ContextEpisodeRecord,
    ) -> Result<()> {
        let result = self.enqueue_memory_index_context_episode(record);
        self.finish_memory_index_enqueue(
            result,
            "context_episode",
            &record.id,
            &format!("episode:{}", record.id),
        )
    }

    fn enqueue_memory_index_workspace_entry(&self, entry: &WorkspaceEntry) -> Result<()> {
        self.enqueue_memory_index_source(
            "workspace_profile",
            &entry.workspace_id,
            &format!("workspace_profile:{}", entry.workspace_id),
        )
    }

    fn enqueue_memory_index_workspace_entry_best_effort(
        &self,
        entry: &WorkspaceEntry,
    ) -> Result<()> {
        let result = self.enqueue_memory_index_workspace_entry(entry);
        self.finish_memory_index_enqueue(
            result,
            "workspace_profile",
            &entry.workspace_id,
            &format!("workspace_profile:{}", entry.workspace_id),
        )
    }

    fn enqueue_memory_index_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        match record.tool_name.as_str() {
            crate::tool::names::EXEC_COMMAND => {
                if record.input.get("cmd").and_then(Value::as_str).is_some() {
                    let source_ref = command_receipt_source_ref(&record.id, None);
                    self.enqueue_memory_index_source(
                        "tool_command_receipt",
                        &source_ref,
                        &source_ref,
                    )?;
                }
            }
            crate::tool::names::EXEC_COMMAND_BATCH => {
                if let Some(items) = record.input.get("items").and_then(Value::as_array) {
                    for (offset, item) in items.iter().enumerate() {
                        if item.get("cmd").and_then(Value::as_str).is_some() {
                            let source_ref =
                                command_receipt_source_ref(&record.id, Some(offset + 1));
                            self.enqueue_memory_index_source(
                                "tool_command_receipt",
                                &source_ref,
                                &source_ref,
                            )?;
                        }
                    }
                }
            }
            _ => {
                let source_ref = command_output_source_ref(&record.id, None, "output");
                self.enqueue_memory_index_source(
                    "tool_execution_output",
                    &source_ref,
                    &source_ref,
                )?;
            }
        }
        Ok(())
    }

    fn enqueue_memory_index_tool_execution_best_effort(
        &self,
        record: &ToolExecutionRecord,
    ) -> Result<()> {
        let result = self.enqueue_memory_index_tool_execution(record);
        self.finish_memory_index_enqueue(result, &record.tool_name, &record.id, &record.id)
    }

    fn finish_memory_index_enqueue(
        &self,
        result: Result<()>,
        source_kind: &str,
        source_id: &str,
        source_ref: &str,
    ) -> Result<()> {
        if let Err(error) = result {
            tracing::warn!(
                error = %error,
                agent_id = self.agent_id.as_deref().unwrap_or("<global>"),
                source_kind,
                source_id,
                source_ref,
                "memory index enqueue failed after canonical storage write"
            );
            if let Err(dirty_error) = self.mark_memory_index_dirty() {
                tracing::warn!(
                    error = %dirty_error,
                    agent_id = self.agent_id.as_deref().unwrap_or("<global>"),
                    source_kind,
                    source_id,
                    source_ref,
                    "failed to mark memory index dirty after enqueue failure"
                );
            }
        }

        // Wake the daemon-level memory indexer if one is attached so it can
        // pick up the newly enqueued outbox rows without waiting for a poll.
        if let Ok(guard) = self.memory_index_notify.lock() {
            if let Some(notify) = guard.as_ref() {
                notify.notify_one();
            }
        }
        Ok(())
    }

    fn enqueue_memory_index_source(
        &self,
        source_kind: &str,
        source_id: &str,
        source_ref: &str,
    ) -> Result<()> {
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        enqueue_memory_index_upsert(self, source_kind, source_id, source_ref)
    }

    fn storage_agent_id(&self) -> Result<String> {
        self.agent_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("agent-scoped storage operation requires an agent id"))
    }

    pub fn write_agent(&self, agent: &AgentState) -> Result<()> {
        self.ensure_writable()?;
        if let Some(storage_agent_id) = self.agent_id.as_deref() {
            anyhow::ensure!(
                agent.id == storage_agent_id,
                "agent-scoped storage for `{}` cannot write agent state for `{}`",
                storage_agent_id,
                agent.id
            );
        }
        let runtime_db = self.runtime_db.clone();
        runtime_db.agent_states().upsert(agent)?;
        return Ok(());
    }

    pub fn read_agent(&self) -> Result<Option<AgentState>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db.agent_states().latest(&agent_id);
        }
        return Ok(None);
    }

    pub(crate) fn read_legacy_agent_for_import(&self) -> Result<Option<AgentState>> {
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
        #[cfg(test)]
        self.flush_audit_event_writes_for_tests()?;
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .audit_events()
            .recent(self.current_agent_id()?.as_deref(), limit);
    }

    pub fn latest_event_seq(&self) -> Result<Option<u64>> {
        #[cfg(test)]
        self.flush_audit_event_writes_for_tests()?;
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .audit_events()
            .latest_event_seq(self.current_agent_id()?.as_deref());
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
        #[cfg(test)]
        self.flush_audit_event_writes_for_tests()?;
        let runtime_db = self.runtime_db.clone();
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

    pub fn read_recent_briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .evidence()
            .recent_briefs(&self.storage_agent_id()?, limit);
    }

    pub fn read_brief_by_id(&self, brief_id: &str) -> Result<Option<BriefRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .evidence()
            .brief_by_id(&self.storage_agent_id()?, brief_id);
    }

    pub fn read_recent_messages(&self, limit: usize) -> Result<Vec<MessageEnvelope>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .messages()
            .recent(self.current_agent_id()?.as_deref(), limit);
    }

    pub fn read_message_by_id(&self, message_id: &str) -> Result<Option<MessageEnvelope>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .messages()
            .by_id(self.current_agent_id()?.as_deref(), message_id);
    }

    /// Reads messages at or after `offset`, then returns only the most recent
    /// `limit` entries from that range.
    ///
    /// This is not equivalent to returning the first `limit` messages starting
    /// at `offset`; it preserves recent-message window semantics.
    pub fn read_messages_from(&self, offset: usize, limit: usize) -> Result<Vec<MessageEnvelope>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .messages()
            .from(self.current_agent_id()?.as_deref(), offset, limit);
    }

    pub fn read_all_messages(&self) -> Result<Vec<MessageEnvelope>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .messages()
            .all(self.current_agent_id()?.as_deref());
    }

    pub fn read_all_message_values(&self) -> Result<Vec<Value>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .messages()
            .all_values(self.current_agent_id()?.as_deref());
    }

    pub fn read_recent_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return Ok(take_recent(
                runtime_db.tasks().latest_for_agent(&agent_id, limit)?,
                limit,
            ));
        }
        return Ok(take_recent(runtime_db.tasks().latest_all()?, limit));
    }

    pub fn read_recent_work_items(&self, limit: usize) -> Result<Vec<WorkItemRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return Ok(take_recent(
                runtime_db.work_items().latest_for_agent(&agent_id, limit)?,
                limit,
            ));
        }
        return Ok(take_recent(runtime_db.work_items().latest_all()?, limit));
    }

    pub fn read_recent_delivery_summaries(
        &self,
        limit: usize,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .evidence()
            .recent_delivery_summaries(&self.storage_agent_id()?, limit);
    }

    pub fn read_recent_work_item_delegations(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkItemDelegationRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .work_item_delegations()
                .recent_for_agent(&agent_id, limit);
        }
        return runtime_db.work_item_delegations().recent(limit);
    }

    pub fn read_recent_work_item_continuations(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkItemContinuationFrame>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .work_item_continuations()
                .recent_for_agent(&agent_id, limit);
        }
        return runtime_db.work_item_continuations().recent(limit);
    }

    pub fn read_recent_timers(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db.timers().recent_for_agent(&agent_id, limit);
        }
        return runtime_db.timers().recent(limit);
    }

    pub fn read_recent_tool_executions(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .evidence()
            .recent_tool_executions(&self.storage_agent_id()?, limit);
    }

    pub fn read_tool_execution_by_id(&self, tool_id: &str) -> Result<Option<ToolExecutionRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .evidence()
            .tool_execution_by_id(&self.storage_agent_id()?, tool_id);
    }

    pub fn read_recent_turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db.turn_records().recent_for_agent(&agent_id, limit);
        }
        return runtime_db.turn_records().recent(limit);
    }

    pub fn read_recent_transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .transcript_entries()
            .recent(self.current_agent_id()?.as_deref(), limit);
    }

    pub fn read_all_transcript(&self) -> Result<Vec<TranscriptEntry>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .transcript_entries()
            .all(self.current_agent_id()?.as_deref());
    }

    pub fn read_transcript_entry_by_id(&self, entry_id: &str) -> Result<Option<TranscriptEntry>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .transcript_entries()
            .by_id(self.current_agent_id()?.as_deref(), entry_id);
    }

    pub fn read_recent_wait_conditions(&self, limit: usize) -> Result<Vec<WaitConditionRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .wait_conditions()
                .recent_for_agent(&agent_id, limit);
        }
        return runtime_db.wait_conditions().recent(limit);
    }

    pub fn read_recent_queue_entries(&self, limit: usize) -> Result<Vec<QueueEntryRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .queue_entries()
            .recent(self.current_agent_id()?.as_deref(), limit);
    }

    pub fn read_recent_external_triggers(
        &self,
        limit: usize,
    ) -> Result<Vec<ExternalTriggerRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .external_triggers()
                .latest_for_agent_limit(&agent_id, limit);
        }
        Ok(Vec::new())
    }

    pub fn read_recent_operator_notifications(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorNotificationRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .operator_notifications()
                .read_recent_for_agent(&agent_id, limit);
        }
        Ok(Vec::new())
    }

    pub fn read_recent_operator_transport_bindings(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorTransportBinding>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .operator_transport_bindings()
                .read_recent_for_agent(&agent_id, limit);
        }
        Ok(Vec::new())
    }

    pub fn read_recent_operator_delivery_records(
        &self,
        limit: usize,
    ) -> Result<Vec<OperatorDeliveryRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .operator_delivery_records()
                .read_recent_for_agent(&agent_id, limit);
        }
        Ok(Vec::new())
    }

    pub fn read_recent_context_episodes(&self, limit: usize) -> Result<Vec<ContextEpisodeRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .context_episodes()
                .recent_for_agent(&agent_id, limit);
        }
        return runtime_db.context_episodes().recent(limit);
    }

    pub fn read_recent_workspace_entries(&self, limit: usize) -> Result<Vec<WorkspaceEntry>> {
        let runtime_db = self.runtime_db.clone();
        return Ok(take_recent(
            runtime_db.workspace_entries().latest_all()?,
            limit,
        ));
    }

    pub fn read_recent_workspace_occupancies(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkspaceOccupancyRecord>> {
        let runtime_db = self.runtime_db.clone();
        return Ok(take_recent(
            runtime_db.workspace_occupancies().latest_all()?,
            limit,
        ));
    }

    pub fn read_recent_agent_identities(&self, limit: usize) -> Result<Vec<AgentIdentityRecord>> {
        let runtime_db = self.runtime_db.clone();
        return Ok(take_recent(
            runtime_db.agent_identities().latest_all()?,
            limit,
        ));
    }

    pub fn latest_task_records(&self) -> Result<Vec<TaskRecord>> {
        self.latest_task_records_from_recent(usize::MAX)
    }

    pub fn latest_task_records_from_recent(&self, history_limit: usize) -> Result<Vec<TaskRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return Ok(take_recent(
                runtime_db
                    .tasks()
                    .latest_for_agent(&agent_id, history_limit)?,
                history_limit,
            ));
        }
        return Ok(take_recent(runtime_db.tasks().latest_all()?, history_limit));
    }

    pub fn latest_active_task_records(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db.tasks().active_for_agent(&agent_id, limit);
        }
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

    pub fn latest_active_task_records_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.tasks().active_for_agent(agent_id, limit);
    }

    pub fn active_task_count_for_agent(&self, agent_id: &str) -> Result<usize> {
        let runtime_db = self.runtime_db.clone();
        return Ok(runtime_db
            .tasks()
            .active_for_agent(agent_id, usize::MAX)?
            .len());
    }

    pub fn latest_task_record(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.tasks().latest(task_id);
    }

    pub fn latest_work_items(&self) -> Result<Vec<WorkItemRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .work_items()
                .latest_for_agent(&agent_id, usize::MAX);
        }
        return runtime_db.work_items().latest_all();
    }

    pub fn latest_work_items_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.work_items().latest_for_agent(agent_id, limit);
    }

    pub fn work_queue_prompt_projection(&self) -> Result<WorkQueuePromptProjection> {
        let current_work_item_id = self
            .read_agent()?
            .and_then(|agent| agent.current_work_item_id);
        let mut latest = std::collections::HashMap::<String, WorkItemRecord>::new();
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            for record in runtime_db
                .work_items()
                .latest_for_agent(&agent_id, usize::MAX)?
            {
                latest.insert(record.id.clone(), record);
            }
        }

        let current = current_work_item_id
            .as_deref()
            .and_then(|id| latest.get(id))
            .filter(|item| item.state == WorkItemState::Open)
            .cloned();

        let trigger_delivery_by_id = self
            .latest_external_triggers()?
            .into_iter()
            .filter(|trigger| trigger.status == crate::types::ExternalTriggerStatus::Active)
            .filter_map(|trigger| {
                trigger
                    .last_delivered_at
                    .map(|delivered_at| (trigger.external_trigger_id, delivered_at))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let active_wait_conditions = self
            .active_wait_conditions()?
            .into_iter()
            .filter_map(|condition| condition.work_item_id.clone().map(|id| (id, condition)))
            .fold(
                std::collections::BTreeMap::<String, ActiveWaitConditionStates>::new(),
                |mut acc, (id, condition)| {
                    acc.entry(id)
                        .or_default()
                        .record(&condition, &trigger_delivery_by_id);
                    acc
                },
            );
        let active_task_waits = self
            .latest_active_task_records(usize::MAX)?
            .into_iter()
            .filter(|task| task.is_blocking())
            .filter_map(|task| task.effective_work_item_id().map(str::to_string))
            .collect::<std::collections::BTreeSet<_>>();
        let active_continuation_suspended_ids = self
            .latest_active_work_item_continuations_for_agent(
                self.current_agent_id()?.as_deref().unwrap_or_default(),
            )?
            .into_iter()
            .map(|frame| frame.suspended_work_item_id)
            .collect::<std::collections::BTreeSet<_>>();
        let mut readiness = latest
            .values()
            .cloned()
            .map(|item| {
                let is_current = current_work_item_id.as_deref() == Some(item.id.as_str())
                    && item.state == WorkItemState::Open;
                let wait_condition = active_wait_conditions.get(&item.id);
                let wait_condition_state =
                    wait_condition.and_then(ActiveWaitConditionStates::scheduling_state);
                let has_active_waits = wait_condition_state.is_some();
                let has_active_task_waits = active_task_waits.contains(&item.id)
                    || wait_condition.is_some_and(|states| states.task);
                let last_triggered_at = wait_condition.and_then(|states| states.last_triggered_at);
                let has_triggered_waits = last_triggered_at.is_some();
                let yielded = item.state == WorkItemState::Open
                    && active_continuation_suspended_ids.contains(&item.id);
                let scheduling_state = if yielded {
                    WorkItemSchedulingState::YieldedToWorkItem
                } else if has_active_task_waits {
                    item.scheduling_state(Some(WorkItemSchedulingState::WaitingTask))
                } else {
                    item.scheduling_state(wait_condition_state)
                };
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
                    } else if scheduling_state == WorkItemSchedulingState::YieldedToWorkItem {
                        WorkItemCandidateClass::Yielded
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
        let yielded = readiness
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::Yielded)
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        let waiting_for_operator = readiness
            .iter()
            .filter(|item| item.candidate_class == WorkItemCandidateClass::WaitingForOperator)
            .take(3)
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
                    && !active_continuation_suspended_ids.contains(&item.id)
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
            yielded,
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
                task_id: None,
                run_id: None,
            });
        }

        if let Some(run_id) = agent.current_run_id.clone() {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::ActiveTurn,
                reason: "agent has an active turn".into(),
                work_item_id: agent.current_turn_work_item_id.clone(),
                task_id: None,
                run_id: Some(run_id),
            });
        }

        let runtime_db = self.runtime_db.clone();
        let has_queued = runtime_db.queue_entries().has_queued_for_agent(&agent.id)?;
        if has_queued {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::HasQueuedInput,
                reason: "agent has queued input".into(),
                work_item_id: None,
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
                .active_wait_conditions()?
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
                task_id: None,
                run_id: None,
            });
        }

        if let Some(item) = work_queue.waiting_for_operator.first() {
            return Ok(AgentPostureProjection {
                posture: AgentSchedulingPosture::WaitingForOperator,
                reason: item.posture_reason(),
                work_item_id: Some(item.work_item.id.clone()),
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
                task_id: None,
                run_id: None,
            });
        }

        Ok(AgentPostureProjection {
            posture: AgentSchedulingPosture::Idle,
            reason: "no queued input, active turn, runnable work, or active waits".into(),
            work_item_id: None,
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
        let runtime_db = self.runtime_db.clone();
        return runtime_db.work_items().due_blocked_rechecks(agent_id, now);
    }

    pub fn next_blocked_work_item_recheck_at(
        &self,
        agent_id: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.work_items().next_recheck_at(agent_id);
    }

    pub fn latest_work_item(&self, work_item_id: &str) -> Result<Option<WorkItemRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.work_items().latest(work_item_id);
    }

    pub fn latest_delivery_summary(
        &self,
        work_item_id: &str,
    ) -> Result<Option<DeliverySummaryRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .evidence()
            .latest_delivery_summary(&self.storage_agent_id()?, work_item_id);
    }

    pub fn latest_work_item_delegations(&self) -> Result<Vec<WorkItemDelegationRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .work_item_delegations()
                .recent_for_agent(&agent_id, usize::MAX);
        }
        return runtime_db.work_item_delegations().latest_all();
    }

    pub fn latest_work_item_continuations(&self) -> Result<Vec<WorkItemContinuationFrame>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .work_item_continuations()
                .recent_for_agent(&agent_id, usize::MAX);
        }
        return runtime_db.work_item_continuations().latest_all();
    }

    pub fn latest_active_work_item_continuations_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<WorkItemContinuationFrame>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .work_item_continuations()
            .active_for_agent(agent_id);
    }

    pub fn latest_active_work_item_continuation_for_suspended(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<Option<WorkItemContinuationFrame>> {
        Ok(self
            .latest_active_work_item_continuations_for_agent(agent_id)?
            .into_iter()
            .find(|record| record.suspended_work_item_id == work_item_id))
    }

    pub fn latest_active_work_item_continuation_for_active(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<Option<WorkItemContinuationFrame>> {
        Ok(self
            .latest_active_work_item_continuations_for_agent(agent_id)?
            .into_iter()
            .find(|record| record.active_work_item_id == work_item_id))
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
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .work_item_delegations()
            .latest_for_child(child_agent_id);
    }

    pub fn latest_timer_records(&self) -> Result<Vec<TimerRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db.timers().recent_for_agent(&agent_id, usize::MAX);
        }
        return runtime_db.timers().latest_all();
    }

    pub fn latest_timer_record(&self, timer_id: &str) -> Result<Option<TimerRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.timers().latest(timer_id);
    }

    pub fn active_wait_conditions_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<WaitConditionRecord>> {
        let runtime_db = self.runtime_db.clone();
        let records = runtime_db.wait_conditions().active_for_agent(agent_id)?;
        self.filter_active_wait_conditions_for_live_scope(records)
    }

    pub fn active_wait_conditions(&self) -> Result<Vec<WaitConditionRecord>> {
        let runtime_db = self.runtime_db.clone();
        let records = runtime_db.wait_conditions().active_all()?;
        self.filter_active_wait_conditions_for_live_scope(records)
    }

    fn filter_active_wait_conditions_for_live_scope(
        &self,
        records: Vec<WaitConditionRecord>,
    ) -> Result<Vec<WaitConditionRecord>> {
        let mut work_item_is_open = std::collections::BTreeMap::<String, bool>::new();
        let mut live = Vec::new();
        for record in records {
            let Some(work_item_id) = record.work_item_id.as_deref() else {
                live.push(record);
                continue;
            };
            let is_open = match work_item_is_open.get(work_item_id) {
                Some(is_open) => *is_open,
                None => {
                    let is_open = self
                        .latest_work_item(work_item_id)?
                        .is_some_and(|item| item.state == WorkItemState::Open);
                    work_item_is_open.insert(work_item_id.to_string(), is_open);
                    is_open
                }
            };
            if is_open {
                live.push(record);
            }
        }
        Ok(live)
    }

    pub fn active_wait_conditions_for_work_item(
        &self,
        agent_id: &str,
        work_item_id: &str,
    ) -> Result<Vec<WaitConditionRecord>> {
        Ok(self
            .active_wait_conditions_for_agent(agent_id)?
            .into_iter()
            .filter(|record| record.work_item_id.as_deref() == Some(work_item_id))
            .collect())
    }

    /// Returns active wait conditions for an agent without the live-scope filter.
    /// Cancel logic needs the true set of active waits regardless of whether the
    /// associated WorkItem is still Open, because WorkItem completion writes the
    /// Completed state before calling cancel.
    pub fn raw_active_wait_conditions_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<WaitConditionRecord>> {
        let runtime_db = self.runtime_db.clone();
        let records = runtime_db.wait_conditions().active_for_agent(agent_id)?;
        Ok(records)
    }

    pub fn latest_wait_conditions(&self) -> Result<Vec<WaitConditionRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.wait_conditions().latest_all();
    }

    pub fn latest_external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db.external_triggers().latest_for_agent(&agent_id);
        }
        return runtime_db.external_triggers().latest_all();
    }

    pub fn latest_operator_transport_bindings(&self) -> Result<Vec<OperatorTransportBinding>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .operator_transport_bindings()
                .latest_for_agent(&agent_id);
        }
        Ok(Vec::new())
    }

    pub fn latest_operator_delivery_records(&self) -> Result<Vec<OperatorDeliveryRecord>> {
        let runtime_db = self.runtime_db.clone();
        if let Some(agent_id) = self.current_agent_id()? {
            return runtime_db
                .operator_delivery_records()
                .latest_for_agent(&agent_id);
        }
        Ok(Vec::new())
    }

    pub fn latest_workspace_entries(&self) -> Result<Vec<WorkspaceEntry>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.workspace_entries().latest_all();
    }

    pub fn latest_workspace_occupancies(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.workspace_occupancies().latest_all();
    }

    pub fn latest_agent_identities(&self) -> Result<Vec<AgentIdentityRecord>> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db.agent_identities().latest_all();
    }

    pub fn latest_queue_entries(&self) -> Result<Vec<QueueEntryRecord>> {
        let runtime_db = self.runtime_db.clone();
        let records = if let Some(agent_id) = self.current_agent_id()? {
            runtime_db
                .queue_entries()
                .recent(Some(&agent_id), usize::MAX)?
        } else {
            runtime_db.queue_entries().latest_all()?
        };
        // Deduplicate by message_id, keeping the latest entry (last in chronological order)
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

        let runtime_db = self.runtime_db.clone();
        let queued_entries = runtime_db.queue_entries().queued_for_agent(agent_id)?;
        let mut replay_messages = queued_entries
            .into_iter()
            .filter_map(|entry| messages_by_id.get(&entry.message_id).cloned())
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
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .evidence()
            .count_briefs(&self.storage_agent_id()?);
    }

    pub fn count_messages(&self) -> Result<usize> {
        let runtime_db = self.runtime_db.clone();
        return runtime_db
            .messages()
            .count(self.current_agent_id()?.as_deref());
    }
}

fn infer_agent_id_from_data_dir(data_dir: &Path) -> Option<String> {
    let parent_is_agents_dir = data_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("agents");
    if !parent_is_agents_dir {
        return None;
    }
    data_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
}

pub(crate) fn is_active_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    )
}

pub fn to_json_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use chrono::Utc;

    fn truncate_to_millis(dt: DateTime<Utc>) -> DateTime<Utc> {
        let millis = dt.timestamp_millis();
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    use crate::types::{
        AgentState, AgentStatus, AuthorityClass, BriefKind, CallbackDeliveryMode,
        EpisodeBoundaryReason, ExternalTriggerScope, ExternalTriggerStatus,
        ExternalWaitRecoverability, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority, QueueEntryRecord, QueueEntryStatus, TaskKind, TaskRecord, TaskRecoverySpec,
        TaskStatus, TodoItem, TodoItemState, ToolExecutionStatus, TranscriptEntry,
        TranscriptEntryKind, WakeSource, WorkItemPlanStatus, WorkItemState,
    };

    use super::*;

    fn wait_until(mut condition: impl FnMut() -> bool, label: &str) {
        let started_at = std::time::Instant::now();
        loop {
            if condition() {
                return;
            }
            assert!(
                started_at.elapsed() <= std::time::Duration::from_secs(2),
                "{label} did not become true"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn append_event_assigns_monotonic_event_seq() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();

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

        let reopened = AppStorage::new_for_test(dir.path()).unwrap();
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
    fn append_event_persists_before_publishing() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "agent-test").unwrap();
        storage.enable_event_bus(EventBus::new(8)).unwrap();
        let mut receiver = storage.subscribe_events().unwrap().unwrap();

        storage
            .append_event(&AuditEvent::new(
                "durable_before_publish",
                serde_json::json!({}),
            ))
            .unwrap();

        let published = receiver.try_recv().unwrap();
        let reader = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        let persisted = reader
            .audit_events()
            .page_after(Some("agent-test"), 0, 10)
            .unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].id, published.event.id);
        assert_eq!(persisted[0].event_seq, published.event.event_seq);
    }

    #[test]
    fn failed_audit_event_append_is_not_published_or_assigned_a_sequence() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "agent-test").unwrap();
        let runtime_db = storage.runtime_db.clone();
        runtime_db
            .transaction(|tx| {
                tx.execute_batch(
                    "CREATE TRIGGER reject_failed_audit_event
                     BEFORE INSERT ON audit_events
                     WHEN NEW.kind = 'rejected_event'
                     BEGIN
                       SELECT RAISE(FAIL, 'rejected audit event');
                     END;",
                )?;
                Ok(())
            })
            .unwrap();
        storage
            .enable_audit_event_index(runtime_db.clone(), Some("agent-test".to_string()))
            .unwrap();
        storage.enable_event_bus(EventBus::new(8)).unwrap();
        let mut receiver = storage.subscribe_events().unwrap().unwrap();

        let error = storage
            .append_event(&AuditEvent::new("rejected_event", serde_json::json!({})))
            .unwrap_err();
        assert!(error.to_string().contains("rejected audit event"));
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        storage
            .append_event(&AuditEvent::new("accepted_event", serde_json::json!({})))
            .unwrap();

        let published = receiver.try_recv().unwrap();
        assert_eq!(published.event.kind, "accepted_event");
        assert_eq!(published.event.event_seq, 1);
        let persisted = runtime_db
            .audit_events()
            .page_after(Some("agent-test"), 0, 10)
            .unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].kind, "accepted_event");
        assert_eq!(persisted[0].event_seq, 1);
    }

    #[test]
    fn storage_indexes_live_audit_events_when_sink_is_enabled() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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

        wait_until(
            || {
                runtime_db
                    .audit_events()
                    .page_after(Some("agent-test"), 0, 10)
                    .unwrap()
                    .len()
                    == 1
            },
            "live audit event index write",
        );
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "agent-test").unwrap();
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
            .enable_audit_event_index(runtime_db.clone(), Some("agent-test".to_string()))
            .unwrap();

        storage
            .append_event(&AuditEvent::new(
                "db_canonical_event",
                serde_json::json!({ "source": "runtime_db" }),
            ))
            .unwrap();

        wait_until(
            || storage.read_recent_events(10).unwrap().len() == 1,
            "runtime db audit event write",
        );
        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "db_canonical_event");
        assert_eq!(events[0].event_seq, 1);
    }

    #[test]
    fn audit_events_use_runtime_db_after_cutover_before_sink_is_enabled() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "agent-test").unwrap();
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
            .append_event(&AuditEvent::new(
                "db_canonical_bootstrap_event",
                serde_json::json!({ "source": "bootstrap_gap" }),
            ))
            .unwrap();

        wait_until(
            || storage.read_recent_events(10).unwrap().len() == 1,
            "runtime db audit event write before sink",
        );
        let events = storage.read_recent_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "db_canonical_bootstrap_event");
        assert_eq!(events[0].event_seq, 1);
    }

    #[test]
    fn scheduler_control_plane_storage_uses_runtime_db_after_cutover() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let now = truncate_to_millis(Utc::now());
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
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
        let now = truncate_to_millis(Utc::now());
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
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            token: Some("http://localhost/callback".into()),
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
    fn read_recent_turns_scopes_global_runtime_db_to_current_agent() {
        let dir = tempdir().unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )
        .unwrap();
        let agent_a =
            AppStorage::new_for_agent_for_test(dir.path().join("agents/agent-a"), "agent-a")
                .unwrap();
        let agent_b =
            AppStorage::new_for_agent_for_test(dir.path().join("agents/agent-b"), "agent-b")
                .unwrap();
        agent_a
            .append_turn(&TurnRecord::new("agent-a", "turn-a", 1))
            .unwrap();
        agent_b
            .append_turn(&TurnRecord::new("agent-b", "turn-b", 2))
            .unwrap();

        let turns = agent_a.read_recent_turns(10).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].agent_id, "agent-a");
        assert_eq!(turns[0].turn_id, "turn-a");
    }

    #[test]
    fn agent_local_runtime_db_reads_scope_current_state_domains_to_current_agent() {
        let dir = tempdir().unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )
        .unwrap();
        let agent_a =
            AppStorage::new_for_agent_for_test(dir.path().join("agents/agent-a"), "agent-a")
                .unwrap();
        let agent_b =
            AppStorage::new_for_agent_for_test(dir.path().join("agents/agent-b"), "agent-b")
                .unwrap();
        let now = Utc::now();
        let task_for = |agent_id: &str, id: &str| TaskRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: None,
            recovery: None,
        };
        let timer_for = |agent_id: &str, id: &str| TimerRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            created_at: now,
            duration_ms: 1000,
            interval_ms: None,
            repeat: false,
            status: crate::types::TimerStatus::Active,
            summary: Some(id.into()),
            next_fire_at: Some(now),
            last_fired_at: None,
            fire_count: 0,
        };
        let wait_for = |agent_id: &str, id: &str| WaitConditionRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Operator,
            source: None,
            subject_ref: None,
            waiting_for: id.into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
        };
        let episode_for = |agent_id: &str, id: &str| ContextEpisodeRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            workspace_id: "agent_home".into(),
            created_at: now,
            finalized_at: now,
            start_turn_index: 1,
            end_turn_index: 2,
            start_message_count: 1,
            end_message_count: 2,
            boundary_reason: EpisodeBoundaryReason::TaskRejoined,
            current_work_item_id: None,
            objective: None,
            work_summary: None,
            scope_hints: Vec::new(),
            source_turn_ids: Vec::new(),
            source_refs: Vec::new(),
            generated_by: None,
            working_set_files: Vec::new(),
            decisions: Vec::new(),
            carry_forward: Vec::new(),
            waiting_on: Vec::new(),
        };

        agent_a.append_task(&task_for("agent-a", "task-a")).unwrap();
        agent_b.append_task(&task_for("agent-b", "task-b")).unwrap();
        agent_a
            .append_work_item(&WorkItemRecord::new(
                "agent-a",
                "work-a",
                WorkItemState::Open,
            ))
            .unwrap();
        agent_b
            .append_work_item(&WorkItemRecord::new(
                "agent-b",
                "work-b",
                WorkItemState::Open,
            ))
            .unwrap();
        agent_a
            .append_timer(&timer_for("agent-a", "timer-a"))
            .unwrap();
        agent_b
            .append_timer(&timer_for("agent-b", "timer-b"))
            .unwrap();
        agent_a
            .append_wait_condition(&wait_for("agent-a", "wait-a"))
            .unwrap();
        agent_b
            .append_wait_condition(&wait_for("agent-b", "wait-b"))
            .unwrap();
        agent_a
            .append_context_episode(&episode_for("agent-a", "episode-a"))
            .unwrap();
        agent_b
            .append_context_episode(&episode_for("agent-b", "episode-b"))
            .unwrap();

        assert_eq!(agent_a.read_recent_tasks(10).unwrap()[0].id, "task-a");
        assert_eq!(
            agent_a.latest_active_task_records(10).unwrap()[0].id,
            "task-a"
        );
        assert_eq!(
            agent_a.read_recent_work_items(10).unwrap()[0].agent_id,
            "agent-a"
        );
        assert_eq!(agent_a.read_recent_timers(10).unwrap()[0].id, "timer-a");
        assert_eq!(
            agent_a.read_recent_wait_conditions(10).unwrap()[0].id,
            "wait-a"
        );
        assert_eq!(
            agent_a.read_recent_context_episodes(10).unwrap()[0].id,
            "episode-a"
        );
    }

    #[test]
    fn append_message_uses_runtime_db_after_cutover_without_messages_jsonl() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
        wait_until(
            || {
                storage.read_recent_briefs(10).unwrap() == vec![brief.clone()]
                    && storage.read_recent_tool_executions(10).unwrap() == vec![tool.clone()]
                    && storage
                        .latest_delivery_summary("work-db-evidence")
                        .unwrap()
                        .map(|record| record.text)
                        == Some("delivery evidence".into())
            },
            "runtime db evidence writes",
        );
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
    fn runtime_index_outbox_write_does_not_touch_memory_index_db() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        fs::create_dir_all(storage.shared_indexes_dir().join("memory.sqlite3")).unwrap();

        let tool = ToolExecutionRecord {
            id: "tool-index-failure".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 0,
            turn_id: Some("turn-index-failure".into()),
            tool_name: "ExecCommand".into(),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: 1,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "cmd": "echo indexed later" }),
            output: serde_json::json!({ "exit_code": 0 }),
            summary: "command exited".into(),
            invocation_surface: None,
        };

        storage.append_tool_execution(&tool).unwrap();

        assert_eq!(storage.read_recent_tool_executions(10).unwrap(), vec![tool]);
        let runtime_db = storage.runtime_db().unwrap().unwrap();
        let changes = runtime_db
            .runtime_index_outbox()
            .read_after("default", 0, 10)
            .unwrap();
        assert!(changes
            .iter()
            .any(|change| change.source_kind == "tool_command_receipt"));
        assert!(
            !storage
                .shared_indexes_dir()
                .join("memory.default.dirty")
                .exists(),
            "runtime outbox replaces direct memory index writes on the critical path"
        );
    }

    #[test]
    fn runtime_db_memory_episode_and_delegation_writes_skip_live_jsonl_after_cutover() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        let delegation =
            WorkItemDelegationRecord::new("default", "parent-work", "child-agent", "child-work");
        let now = Utc::now();
        let episode = ContextEpisodeRecord {
            id: "ep_db_only".into(),
            agent_id: "default".into(),
            workspace_id: "agent_home".into(),
            created_at: now,
            finalized_at: now + chrono::Duration::seconds(1),
            start_turn_index: 3,
            end_turn_index: 4,
            start_message_count: 6,
            end_message_count: 8,
            boundary_reason: EpisodeBoundaryReason::TaskRejoined,
            current_work_item_id: Some("work-db-only".into()),
            objective: Some("migrate remaining domains".into()),
            work_summary: None,
            scope_hints: Vec::new(),
            source_turn_ids: vec!["turn-1".into()],
            source_refs: Vec::new(),
            generated_by: None,
            working_set_files: Vec::new(),
            decisions: Vec::new(),
            carry_forward: Vec::new(),
            waiting_on: Vec::new(),
        };

        storage.append_work_item_delegation(&delegation).unwrap();
        storage.append_context_episode(&episode).unwrap();

        for file_name in ["work_item_delegations.jsonl", "context_episodes.jsonl"] {
            assert!(
                !storage.ledger_dir().join(file_name).exists(),
                "{file_name} should not be a live compat export after db cutover"
            );
        }

        assert_eq!(
            storage.read_recent_work_item_delegations(10).unwrap(),
            vec![delegation.clone()]
        );
        assert_eq!(
            storage
                .latest_work_item_delegation_for_child("child-agent")
                .unwrap(),
            Some(delegation)
        );
        assert_eq!(
            storage.read_recent_context_episodes(10).unwrap(),
            vec![episode]
        );
    }

    #[test]
    fn runtime_db_evidence_reads_use_directory_agent_id_without_agent_json() {
        let dir = tempdir().unwrap();
        let agent_dir = dir.path().join("agents/default");
        fs::create_dir_all(&agent_dir).unwrap();
        let storage = AppStorage::new_for_test(&agent_dir).unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
        let runtime_db = storage.runtime_db().unwrap().unwrap();
        let changes = runtime_db
            .runtime_index_outbox()
            .read_after("default", 0, 10)
            .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].agent_id, "default");
        assert_eq!(changes[0].source_kind, "brief");
    }

    #[test]
    fn runtime_db_message_and_transcript_import_is_idempotent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
    fn message_seq_counter_resumes_from_existing_ledger() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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

        let reopened = AppStorage::new_for_test(dir.path()).unwrap();
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
    fn storage_round_trip_agent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        storage.write_agent(&agent).unwrap();

        let restored = storage.read_agent().unwrap().unwrap();
        assert_eq!(restored.status, AgentStatus::Asleep);
        assert!(!dir.path().join(".holon/state/agent.json").exists());
    }

    #[test]
    fn write_agent_with_runtime_db_writes_db_only() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        // With DB canonical, agent.json should NOT be created
        assert!(!dir.path().join(".holon/state/agent.json").exists());

        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Stopped;
        agent.turn_index = 7;
        storage.write_agent(&agent).unwrap();

        let restored = storage.read_agent().unwrap().unwrap();
        assert_eq!(restored.status, AgentStatus::Stopped);
        assert_eq!(restored.turn_index, 7);
        // Still no agent.json — DB is canonical
        assert!(!dir.path().join(".holon/state/agent.json").exists());
    }

    #[test]
    fn read_agent_with_runtime_db_does_not_fallback_to_legacy_agent_json() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        // Write a legacy agent.json directly — bypassing storage
        let legacy_agent = AgentState::new("default");
        let agent_path = dir.path().join(".holon/state/agent.json");
        fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        fs::write(
            &agent_path,
            serde_json::to_string_pretty(&legacy_agent).unwrap(),
        )
        .unwrap();
        // DB is empty — read_agent should NOT fall back to JSONL
        assert_eq!(storage.read_agent().unwrap(), None);
    }

    #[test]
    fn read_agent_maps_legacy_paused_status_to_stopped() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Stopped;
        let mut value = serde_json::to_value(&agent).unwrap();
        value["status"] = serde_json::json!("paused");
        std::fs::write(
            dir.path().join(".holon/state/agent.json"),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();

        let restored = storage.read_legacy_agent_for_import().unwrap().unwrap();
        assert_eq!(restored.status, AgentStatus::Stopped);
    }

    #[test]
    fn current_agent_id_is_fixed_at_storage_construction() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "agent-a").unwrap();

        storage.write_agent(&AgentState::new("agent-a")).unwrap();
        let mismatch = storage
            .write_agent(&AgentState::new("agent-b"))
            .unwrap_err();

        assert_eq!(
            storage.current_agent_id().unwrap().as_deref(),
            Some("agent-a")
        );
        assert!(
            mismatch
                .to_string()
                .contains("cannot write agent state for `agent-b`"),
            "{mismatch}"
        );
    }

    #[test]
    fn global_storage_has_no_agent_scope_even_with_agent_json() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_global_for_test(dir.path()).unwrap();

        storage.write_agent(&AgentState::new("default")).unwrap();

        assert_eq!(storage.current_agent_id().unwrap(), None);
    }

    #[test]
    fn latest_active_task_records_reduce_by_id_and_filter_terminal_tasks() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        storage
            .append_task(&task("bounded-extra", "extra task", 4))
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
                ("bounded-repeat", "latest repeat snapshot"),
                ("bounded-extra", "extra task"),
            ]
        );
    }

    #[test]
    fn mark_memory_index_dirty_does_not_rewrite_existing_marker() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
    fn cloned_storage_serializes_concurrent_large_task_appends() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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

        // With DB canonical, verify all concurrent appends landed in DB.
        let tasks = storage
            .read_recent_tasks(thread_count * records_per_thread)
            .unwrap();
        assert_eq!(tasks.len(), thread_count * records_per_thread);
    }

    #[test]
    fn storage_round_trip_transcript_entries() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        assert!(storage.indexes_dir().is_dir());
        assert!(storage.cache_dir().is_dir());
    }

    #[test]
    fn append_message_assigns_monotonic_message_seq() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();

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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();

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
    fn latest_work_item_delegation_for_child_scans_from_tail() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();

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
    fn active_wait_conditions_ignore_completed_work_item_scope() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let now = Utc::now();
        let mut work = WorkItemRecord::new("default", "completed wait", WorkItemState::Open);
        work.blocked_by = Some("task".into());
        storage.append_work_item(&work).unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "stale-work-wait".into(),
                agent_id: "default".into(),
                work_item_id: Some(work.id.clone()),
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
                id: "agent-wait".into(),
                agent_id: "default".into(),
                work_item_id: None,
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("github".into()),
                subject_ref: Some("pr-1".into()),
                waiting_for: "merge".into(),
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

        work.state = WorkItemState::Completed;
        work.blocked_by = None;
        work.revision += 1;
        work.updated_at = now + chrono::Duration::seconds(1);
        storage.append_work_item(&work).unwrap();

        let active_for_agent = storage.active_wait_conditions_for_agent("default").unwrap();
        assert_eq!(
            active_for_agent
                .iter()
                .map(|condition| condition.id.as_str())
                .collect::<Vec<_>>(),
            vec!["agent-wait"]
        );
        assert!(storage
            .active_wait_conditions_for_work_item("default", &work.id)
            .unwrap()
            .is_empty());
        assert_eq!(storage.latest_wait_conditions().unwrap().len(), 2);

        let projection = storage.work_queue_prompt_projection().unwrap();
        let completed = projection
            .completed_recent
            .iter()
            .find(|item| item.work_item.id == work.id)
            .expect("completed work item should still be visible");
        assert_eq!(
            completed.scheduling_state,
            WorkItemSchedulingState::Completed
        );
        assert!(!completed.has_active_waits);
        assert!(!completed.has_active_task_waits);
    }

    #[test]
    fn raw_active_wait_conditions_returns_waits_for_completed_work_item() {
        // Regression test for #1988: cancel must find active waits even after
        // the WorkItem is already marked Completed.
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let now = Utc::now();
        let mut work = WorkItemRecord::new("default", "task with wait", WorkItemState::Open);
        work.blocked_by = Some("task".into());
        storage.append_work_item(&work).unwrap();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "wait-1".into(),
                agent_id: "default".into(),
                work_item_id: Some(work.id.clone()),
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

        // Mark the work item as Completed (as complete_work_item does before cancel).
        work.state = WorkItemState::Completed;
        work.blocked_by = None;
        work.revision += 1;
        storage.append_work_item(&work).unwrap();

        // The filtered path hides the wait, but the raw path must still find it.
        assert!(storage
            .active_wait_conditions_for_work_item("default", &work.id)
            .unwrap()
            .is_empty());
        let raw = storage
            .raw_active_wait_conditions_for_agent("default")
            .unwrap();
        let raw_for_work: Vec<_> = raw
            .into_iter()
            .filter(|r| r.work_item_id.as_deref() == Some(work.id.as_str()))
            .collect();
        assert_eq!(raw_for_work.len(), 1);
        assert_eq!(raw_for_work[0].id, "wait-1");
    }

    #[test]
    fn external_wait_recoverability_is_derived_and_audited() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
    }

    #[test]
    fn work_queue_projection_uses_internal_wait_conditions_for_wait_state() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
            .append_wait_condition(&WaitConditionRecord {
                id: "external-condition".into(),
                agent_id: "default".into(),
                work_item_id: Some(external.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("github".into()),
                subject_ref: Some("pull_request:1".into()),
                waiting_for: "merged".into(),
                wake_sources: vec![WakeSource::ExternalIngress {
                    external_trigger_id: Some("trigger-external".into()),
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
                    WaitConditionKind::External => vec![WakeSource::ExternalIngress {
                        external_trigger_id: Some("trigger-external".into()),
                    }],
                    WaitConditionKind::Timer => vec![WakeSource::Timer {
                        wake_at: Utc::now() + chrono::Duration::minutes(5),
                    }],
                    WaitConditionKind::System => vec![WakeSource::SystemTick],
                    _ => unreachable!("helper is only used for external/timer/system waits"),
                };
                storage
                    .append_wait_condition(&WaitConditionRecord {
                        id: format!("{kind:?}-condition"),
                        agent_id: "default".into(),
                        work_item_id: Some(work_item_id.into()),
                        status: WaitConditionStatus::Active,
                        kind,
                        source: (wait_kind == "External").then(|| "github".into()),
                        subject_ref: (wait_kind == "External").then(|| "pull_request:1".into()),
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        assert_eq!(posture_for(&storage, &agent), AgentSchedulingPosture::Idle);

        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        let mut external = WorkItemRecord::new("default", "external wait", WorkItemState::Open);
        external.blocked_by = Some("github".into());
        storage.append_work_item(&external).unwrap();
        append_wait_condition(&storage, &external.id, WaitConditionKind::External);
        assert_eq!(
            posture_for(&storage, &agent),
            AgentSchedulingPosture::WaitingForExternal
        );

        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
    fn storage_recovery_snapshot_replays_latest_queued_messages() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        assert_eq!(snapshot.replay_messages.len(), 1);
        assert_eq!(snapshot.replay_messages[0].id, queued.id);
    }

    #[test]
    fn recovery_snapshot_orders_message_replay_by_sequence_when_timestamps_tie() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let older = WorkItemRecord::new("default", "older", WorkItemState::Open);
        let mut updated = older.clone();
        updated.objective = "updated".into();
        updated.updated_at = older.created_at + chrono::Duration::milliseconds(100);
        let other_agent = WorkItemRecord::new("other", "other agent", WorkItemState::Open);
        let mut newest = WorkItemRecord::new("default", "newest", WorkItemState::Open);
        newest.updated_at = older.created_at + chrono::Duration::milliseconds(200);

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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
    fn storage_work_queue_prompt_projection_uses_current_and_orders_queue() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();

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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
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
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
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
            .append_wait_condition(&WaitConditionRecord {
                id: "wait-triggered".into(),
                agent_id: "default".into(),
                work_item_id: Some(triggered.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("test".into()),
                subject_ref: None,
                waiting_for: "triggered wait".into(),
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
            .append_external_trigger(&ExternalTriggerRecord {
                external_trigger_id: "trigger-1".into(),
                target_agent_id: "default".into(),
                scope: ExternalTriggerScope::Agent,
                delivery_mode: crate::types::CallbackDeliveryMode::WakeHint,
                token: None,
                token_hash: "token-hash".into(),
                status: ExternalTriggerStatus::Active,
                created_at: now,
                revoked_at: None,
                last_delivered_at: Some(now),
                delivery_count: 1,
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
    fn storage_work_queue_prompt_projection_limits_waiting_for_operator() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let now = Utc::now();

        for index in 0..5 {
            let mut waiting = WorkItemRecord::new(
                "default",
                format!("operator decision {index}"),
                WorkItemState::Open,
            );
            waiting.plan_status = WorkItemPlanStatus::NeedsInput;
            waiting.updated_at = now + chrono::Duration::minutes(index);
            storage.append_work_item(&waiting).unwrap();
        }

        let projection = storage.work_queue_prompt_projection().unwrap();

        assert_eq!(
            projection
                .waiting_for_operator
                .iter()
                .map(|item| item.work_item.objective.as_str())
                .collect::<Vec<_>>(),
            vec![
                "operator decision 4",
                "operator decision 3",
                "operator decision 2"
            ]
        );
    }

    #[test]
    fn storage_waiting_contract_anchor_falls_back_to_latest_waiting_when_no_active_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();

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

    #[test]
    fn write_agent_cleans_up_tmp_file_on_rename_failure() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_test(dir.path()).unwrap();
        let agent = AgentState::new("default");

        // Write agent successfully first
        storage.write_agent(&agent).unwrap();

        // Count tmp files (should be 0 after successful write)
        let state_dir = dir.path().join(".holon/state");
        let tmp_files: Vec<_> = fs::read_dir(&state_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".agent.json."))
            .collect();
        assert_eq!(
            tmp_files.len(),
            0,
            "no tmp files should remain after successful write"
        );
    }

    #[test]
    fn write_agent_rejects_mismatched_agent_id() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "agent-a").unwrap();

        let agent = AgentState::new("agent-b");
        let result = storage.write_agent(&agent);

        assert!(
            result.is_err(),
            "should reject agent state with mismatched id"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cannot write agent state"),
            "error should mention agent id mismatch"
        );
    }
}
