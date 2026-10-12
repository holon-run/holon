//! Typed read capability for explicit context acquisition. No renderer receives it.

use anyhow::Result;

use crate::{
    domain::execution_protocol::ExecutionProtocolState,
    runtime_db::{
        context_inputs::ContextDurableRead, task_result_settlement::TaskResultSettlementRecord,
    },
    storage::{AppStorage, WorkQueueReadModel},
    types::{
        BriefRecord, DeliverySummaryRecord, ExternalTriggerRecord, MessageEnvelope, TaskRecord,
        ToolExecutionRecord, TranscriptEntry, TurnOwner, TurnRecord, WaitConditionRecord,
        WorkItemRecord,
    },
};

/// Acquisition-only typed reads. Implementations cannot provide a database,
/// connection factory, write operation, resource access or renderer fallback.
pub(crate) trait ContextInputs {
    fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>>;
    fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>>;
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>>;
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>>;
    fn tools(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>>;
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolExecutionRecord>>;
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>>;
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>>;
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>>;
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>>;
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>>;
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>>;
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>>;
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>>;
    fn work_queue(&self) -> Result<WorkQueueReadModel>;
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>>;
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>>;
    /// Existing active_for_agent ordering, unbounded so count and display share a view.
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>>;
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>>;
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>>;
}

/// Compatibility adapter only. The owner-coherent path uses ContextDurableRead;
/// this legacy adapter intentionally retains its current multi-connection reads.
impl ContextInputs for AppStorage {
    fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>> {
        self.read_all_messages()
    }
    fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>> {
        self.read_message_by_id(id)
    }
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        self.read_recent_briefs(limit)
    }
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>> {
        self.read_brief_by_id(id)
    }
    fn tools(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>> {
        self.read_recent_tool_executions(limit)
    }
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolExecutionRecord>> {
        self.read_tool_execution_by_id(id)
    }
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        self.read_recent_turns(limit)
    }
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>> {
        self.read_recent_turns_for_owner(owner, limit)
    }
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>> {
        self.read_turn_by_id(id)
    }
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        self.read_recent_transcript(limit)
    }
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>> {
        self.read_transcript_for_turns(ids)
    }
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>> {
        self.read_transcript_entry_by_id(id)
    }
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>> {
        let Some(db) = self.runtime_db()? else {
            return Ok(None);
        };
        db.transitions()
            .load_execution_protocol_state_if_initialized(agent_id)
    }
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        self.active_wait_conditions_for_agent(agent_id)
    }
    fn work_queue(&self) -> Result<WorkQueueReadModel> {
        self.work_queue_prompt_projection()
    }
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>> {
        self.latest_work_item(id)
    }
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        self.latest_external_triggers()
    }
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
        self.latest_active_task_records_for_agent(agent_id, usize::MAX)
    }
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
        self.read_recent_delivery_summaries(limit)
    }
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        let Some(db) = self.runtime_db()? else {
            return Ok(Vec::new());
        };
        db.task_result_settlements()
            .admitted_for_activation(agent_id, activation_id)
    }
}

// All delegated methods are inherent, same-connection reads supplied by the
// narrow runtime_db facade. No AppStorage fallback is reachable from this impl.
impl ContextInputs for ContextDurableRead<'_> {
    fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>> {
        ContextDurableRead::anchor_messages(self)
    }
    fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>> {
        ContextDurableRead::message_by_id(self, id)
    }
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        ContextDurableRead::briefs(self, limit)
    }
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>> {
        ContextDurableRead::brief_by_id(self, id)
    }
    fn tools(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>> {
        ContextDurableRead::tools(self, limit)
    }
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolExecutionRecord>> {
        ContextDurableRead::tool_by_id(self, id)
    }
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        ContextDurableRead::turns(self, limit)
    }
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>> {
        ContextDurableRead::turns_for_owner(self, owner, limit)
    }
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>> {
        ContextDurableRead::turn_by_id(self, id)
    }
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        ContextDurableRead::transcript(self, limit)
    }
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>> {
        ContextDurableRead::transcript_for_turns(self, ids)
    }
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>> {
        ContextDurableRead::transcript_by_id(self, id)
    }
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>> {
        ContextDurableRead::execution_protocol_state(self, agent_id)
    }
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        ContextDurableRead::active_waits(self, agent_id)
    }
    fn work_queue(&self) -> Result<WorkQueueReadModel> {
        ContextDurableRead::work_queue(self)
    }
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>> {
        ContextDurableRead::work_item(self, id)
    }
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        ContextDurableRead::external_triggers(self)
    }
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
        ContextDurableRead::active_tasks(self, agent_id)
    }
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
        ContextDurableRead::delivery_summaries(self, limit)
    }
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        ContextDurableRead::admitted_settlements(self, agent_id, activation_id)
    }
}
