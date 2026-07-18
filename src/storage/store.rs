//! Canonical runtime writes and restricted transition entry point.

use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::{
    runtime_db::{
        transitions::{PostCommitEffects, PostCommitWarning},
        RuntimeDb, RuntimeIndexChange,
    },
    types::{
        AgentIdentityRecord, AgentState, BriefRecord, ContextEpisodeRecord, DeliverySummaryRecord,
        ExternalTriggerRecord, MessageEnvelope, OperatorDeliveryRecord, OperatorNotificationRecord,
        OperatorTransportBinding, QueueEntryRecord, TaskRecord, TimerRecord, ToolExecutionRecord,
        TranscriptEntry, TurnRecord, WaitConditionRecord, WorkItemContinuationFrame,
        WorkItemDelegationRecord, WorkItemRecord, WorkspaceEntry, WorkspaceOccupancyRecord,
    },
};

use super::{recovery::external_wait_recoverability_event, RuntimeEventLog, RuntimeIndexOutbox};

#[derive(Debug, Clone)]
pub struct RuntimeStore {
    runtime_db: RuntimeDb,
    agent_id: Option<String>,
    read_only: bool,
    append_mutex: Arc<Mutex<()>>,
    event_log: RuntimeEventLog,
    index_outbox: RuntimeIndexOutbox,
}

impl RuntimeStore {
    pub(crate) fn new(
        runtime_db: RuntimeDb,
        agent_id: Option<String>,
        read_only: bool,
        append_mutex: Arc<Mutex<()>>,
        event_log: RuntimeEventLog,
        index_outbox: RuntimeIndexOutbox,
    ) -> Self {
        Self {
            runtime_db,
            agent_id,
            read_only,
            append_mutex,
            event_log,
            index_outbox,
        }
    }

    pub fn event_log(&self) -> RuntimeEventLog {
        self.event_log.clone()
    }

    pub fn index_outbox(&self) -> RuntimeIndexOutbox {
        self.index_outbox.clone()
    }

    pub(crate) fn publish_transition_events(
        &self,
        effects: &PostCommitEffects,
    ) -> Vec<PostCommitWarning> {
        self.event_log.publish_transition_events(effects)
    }

    pub(crate) fn notify_transition_memory_index(
        &self,
        effects: &PostCommitEffects,
    ) -> Vec<PostCommitWarning> {
        self.index_outbox.notify_transition(effects)
    }

    fn ensure_writable(&self) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "cannot write through read-only runtime storage"
        );
        Ok(())
    }

    fn current_agent_id(&self) -> Result<Option<String>> {
        Ok(self.agent_id.clone())
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        let changes = self.index_outbox.changes_for_brief(brief);
        self.runtime_db
            .evidence()
            .append_brief_with_index_changes(brief, &changes)?;
        self.index_outbox.enqueue_brief_best_effort(brief)
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        self.ensure_writable()?;
        let changes = self.index_outbox.changes_for_message(message);
        let message = self
            .runtime_db
            .messages()
            .append_with_index_changes(message, &changes)?;
        self.index_outbox.enqueue_message_best_effort(&message)
    }

    pub fn append_task(&self, task: &TaskRecord) -> Result<()> {
        let changes = self.index_outbox.changes_for_task(task);
        self.runtime_db
            .tasks()
            .upsert_with_index_changes(task, &changes)?;
        self.index_outbox.enqueue_task_best_effort(task)
    }

    pub fn append_work_item(&self, record: &WorkItemRecord) -> Result<()> {
        self.ensure_writable()?;
        if self.runtime_db.work_items().latest(&record.id)?.is_some() {
            let expected_revision = record.revision.checked_sub(1).ok_or_else(|| {
                anyhow::anyhow!(
                    "work item {} update has invalid revision {}",
                    record.id,
                    record.revision
                )
            })?;
            self.update_work_item_expected(record, expected_revision)?;
        } else {
            self.insert_work_item(record)?;
        }
        Ok(())
    }

    pub fn insert_work_item(&self, record: &WorkItemRecord) -> Result<()> {
        self.ensure_writable()?;
        let changes = self.index_outbox.changes_for_work_item(record);
        self.runtime_db
            .work_items()
            .insert_new_with_index_changes(record, &changes)?;
        self.index_outbox.enqueue_work_item_best_effort(record)
    }

    pub fn update_work_item_expected(
        &self,
        record: &WorkItemRecord,
        expected_revision: u64,
    ) -> Result<()> {
        self.ensure_writable()?;
        let changes = self.index_outbox.changes_for_work_item(record);
        self.runtime_db
            .work_items()
            .update_expected_with_index_changes(record, expected_revision, &changes)?;
        self.index_outbox.enqueue_work_item_best_effort(record)
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        self.runtime_db.evidence().append_delivery_summary(record)
    }

    pub fn append_work_item_delegation(&self, record: &WorkItemDelegationRecord) -> Result<()> {
        self.runtime_db.work_item_delegations().upsert(record)
    }

    pub fn append_work_item_continuation(&self, record: &WorkItemContinuationFrame) -> Result<()> {
        self.runtime_db.work_item_continuations().upsert(record)
    }

    pub fn append_timer(&self, timer: &TimerRecord) -> Result<()> {
        self.runtime_db.timers().upsert(timer)
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        let changes = self.index_outbox.changes_for_tool_execution(record);
        self.runtime_db
            .evidence()
            .append_tool_execution_with_index_changes(record, &changes)?;
        self.index_outbox.enqueue_tool_execution_best_effort(record)
    }

    pub fn append_turn(&self, record: &TurnRecord) -> Result<()> {
        self.runtime_db.turn_records().upsert(record)
    }

    pub fn append_transcript_entry(&self, entry: &TranscriptEntry) -> Result<()> {
        self.ensure_writable()?;
        self.runtime_db.transcript_entries().append(entry)?;
        Ok(())
    }

    pub fn append_queue_entry(&self, record: &QueueEntryRecord) -> Result<()> {
        self.runtime_db.queue_entries().upsert(record)
    }

    pub fn try_claim_queued_message(&self, record: &QueueEntryRecord) -> Result<bool> {
        self.runtime_db
            .queue_entries()
            .try_claim_queued_message(record)
    }

    pub fn append_wait_condition(&self, record: &WaitConditionRecord) -> Result<()> {
        let event = external_wait_recoverability_event(record);
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        self.runtime_db.wait_conditions().upsert(record)?;
        if let Some(event) = event.as_ref() {
            self.event_log.append_with_append_mutex_held(event)?;
        }
        Ok(())
    }

    pub fn append_external_trigger(&self, record: &ExternalTriggerRecord) -> Result<()> {
        self.runtime_db.external_triggers().upsert(record)
    }

    pub fn append_operator_notification(&self, record: &OperatorNotificationRecord) -> Result<()> {
        let agent_id = self.current_agent_id()?.ok_or_else(|| {
            anyhow::anyhow!("agent_id is required for operator_notification persistence")
        })?;
        self.runtime_db
            .operator_notifications()
            .insert(&agent_id, record)
    }

    pub fn append_operator_transport_binding(
        &self,
        record: &OperatorTransportBinding,
    ) -> Result<()> {
        let agent_id = self.current_agent_id()?.ok_or_else(|| {
            anyhow::anyhow!("agent_id is required for operator_transport_binding persistence")
        })?;
        self.runtime_db
            .operator_transport_bindings()
            .upsert(&agent_id, record)
    }

    pub fn append_operator_delivery_record(&self, record: &OperatorDeliveryRecord) -> Result<()> {
        let agent_id = self.current_agent_id()?.ok_or_else(|| {
            anyhow::anyhow!("agent_id is required for operator_delivery_record persistence")
        })?;
        self.runtime_db
            .operator_delivery_records()
            .upsert(&agent_id, record)
    }

    pub fn append_context_episode(&self, record: &ContextEpisodeRecord) -> Result<()> {
        let changes = self.index_outbox.changes_for_context_episode(record);
        self.runtime_db
            .context_episodes()
            .upsert_with_index_changes(record, &changes)?;
        self.index_outbox
            .enqueue_context_episode_best_effort(record)
    }

    pub fn append_workspace_entry(&self, entry: &WorkspaceEntry) -> Result<()> {
        let changes = self.index_outbox.changes_for_workspace_entry(entry);
        self.runtime_db
            .workspace_entries()
            .upsert_with_index_changes(entry, &changes)?;
        self.index_outbox.enqueue_workspace_entry_best_effort(entry)
    }

    pub fn append_workspace_occupancy(&self, entry: &WorkspaceOccupancyRecord) -> Result<()> {
        self.runtime_db.workspace_occupancies().upsert(entry)
    }

    pub fn append_agent_identity(&self, entry: &AgentIdentityRecord) -> Result<()> {
        self.runtime_db.agent_identities().upsert(entry)
    }

    pub(crate) fn index_changes_for_task(&self, task: &TaskRecord) -> Vec<RuntimeIndexChange> {
        self.index_outbox.changes_for_task(task)
    }

    pub(crate) fn index_changes_for_work_item(
        &self,
        record: &WorkItemRecord,
    ) -> Vec<RuntimeIndexChange> {
        self.index_outbox.changes_for_work_item(record)
    }

    pub(crate) fn wait_condition_auxiliary_events(
        &self,
        record: &WaitConditionRecord,
    ) -> Vec<crate::types::AuditEvent> {
        external_wait_recoverability_event(record)
            .into_iter()
            .collect()
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
        self.runtime_db.agent_states().upsert(agent)
    }
}
