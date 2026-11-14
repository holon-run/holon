use super::message::{
    admission_trigger_kind_for_message_kind, AuthorityClass, MessageEnvelope, MessageOrigin,
    Priority,
};
use super::serde_helpers::is_false;
use chrono::{DateTime, Utc};
use holon_domain::{ContinuationTriggerKind, MessageKind, TurnTerminalKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TurnTerminalRecord {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub turn_id: String,
    pub turn_index: u64,
    pub kind: TurnTerminalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_brief_reason: Option<TurnNoBriefReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<TurnTerminalCheckpointRecord>,
    pub completed_at: DateTime<Utc>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnTriggerSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub kind: MessageKind,
    pub origin: MessageOrigin,
    pub authority_class: AuthorityClass,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_kind: Option<ContinuationTriggerKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl TurnTriggerSummary {
    pub fn from_message(message: &MessageEnvelope) -> Self {
        Self {
            message_id: Some(message.id.clone()),
            kind: message.kind.clone(),
            origin: message.origin.clone(),
            authority_class: message.authority_class,
            priority: message.priority.clone(),
            // Dispatch fills this default without rewriting the persisted source.
            trigger_kind: Some(
                message
                    .trigger_kind
                    .unwrap_or_else(|| admission_trigger_kind_for_message_kind(&message.kind)),
            ),
            task_id: message.task_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnTerminalSummary {
    pub kind: TurnTerminalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_brief_reason: Option<TurnNoBriefReason>,
    pub completed_at: DateTime<Utc>,
    pub duration_ms: u64,
}

impl TurnTerminalSummary {
    pub fn from_terminal(record: &TurnTerminalRecord) -> Self {
        Self {
            kind: record.kind,
            reason: record.reason.clone(),
            no_brief_reason: record.no_brief_reason.clone(),
            completed_at: record.completed_at,
            duration_ms: record.duration_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnNoBriefReason {
    ReducerOnly { reason: String },
    Aborted,
    Interrupted,
    ToolOnlyWait,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnReplayProvenance {
    pub source_message_id: String,
    pub source_turn_id: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_terminal: Option<TurnTerminalSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnOwner {
    WorkItem { work_item_id: String },
    Conversation { interaction_id: String },
    AgentLifecycle { agent_id: String },
    Command,
}

impl TurnOwner {
    pub fn work_item_id(&self) -> Option<&str> {
        match self {
            Self::WorkItem { work_item_id } => Some(work_item_id),
            Self::Conversation { .. } | Self::AgentLifecycle { .. } | Self::Command => None,
        }
    }

    pub fn index_parts(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::WorkItem { work_item_id } => ("work_item", Some(work_item_id)),
            Self::Conversation { interaction_id } => ("conversation", Some(interaction_id)),
            Self::AgentLifecycle { agent_id } => ("agent_lifecycle", Some(agent_id)),
            Self::Command => ("command", None),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnRecord {
    pub turn_id: String,
    pub turn_index: u64,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TurnOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TurnTriggerSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_message_ids: Vec<String>,
    /// Source-neutral, durable evidence for the idle-start input snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_input_batch: Option<IdleInputBatchRecord>,
    /// Source-specific safe-point admissions, separate from the idle snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live_input_batches: Vec<LiveInputBatchRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_execution_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub produced_brief_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivery_summary_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_work_item_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_condition_ids: Vec<String>,
    /// Typed local failure evidence, committed with the aborted turn and attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_failure: Option<crate::runtime_error::RuntimeErrorDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TurnTerminalSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<TurnReplayProvenance>,
    /// Durable fence for a destructive lifecycle operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_operation_id: Option<String>,
    /// An external process may have produced a side effect before this turn
    /// reached a durable tool-result checkpoint. Such turns are never replayed.
    #[serde(default)]
    pub external_side_effect_possible: bool,
    /// The original tool call protected by the external side effect fence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) external_side_effect_tool_call_id: Option<String>,
    /// This operator dispatch must commit its provider entry receipt before execution.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) operator_dispatch_fenced: bool,
    pub created_at: DateTime<Utc>,
}

impl TurnRecord {
    pub fn new(agent_id: impl Into<String>, turn_id: impl Into<String>, turn_index: u64) -> Self {
        Self {
            turn_id: turn_id.into().trim().to_string(),
            turn_index,
            agent_id: agent_id.into(),
            run_id: None,
            current_work_item_id: None,
            owner: None,
            trigger: None,
            input_message_ids: Vec::new(),
            idle_input_batch: None,
            live_input_batches: Vec::new(),
            tool_execution_ids: Vec::new(),
            produced_brief_ids: Vec::new(),
            delivery_summary_ids: Vec::new(),
            completed_work_item_ids: Vec::new(),
            waiting_condition_ids: Vec::new(),
            runtime_failure: None,
            terminal: None,
            replay: None,
            destructive_operation_id: None,
            external_side_effect_possible: false,
            external_side_effect_tool_call_id: None,
            operator_dispatch_fenced: false,
            created_at: Utc::now(),
        }
    }

    pub fn effective_owner(&self) -> TurnOwner {
        self.owner.clone().unwrap_or_else(|| {
            self.current_work_item_id.as_ref().map_or_else(
                || TurnOwner::AgentLifecycle {
                    agent_id: self.agent_id.clone(),
                },
                |work_item_id| TurnOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                },
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct IdleInputBatchRecord {
    pub anchor_message_id: String,
    /// Canonical admission that persisted this turn's snapshot, not new authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_id: Option<String>,
    pub sender_agent_id: String,
    pub cutoff: DateTime<Utc>,
    pub max_items: usize,
    pub max_bytes: usize,
    pub rendered_bytes: usize,
    pub items: Vec<IdleInputBatchItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct IdleInputBatchItem {
    pub message_id: String,
    pub source_ref: String,
    pub rendered: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct LiveInputBatchRecord {
    /// Current ledger binding; recovery may rebind admission without changing its safe point.
    pub activation_id: String,
    pub safe_point: LiveInputSafePoint,
    pub cutoff: DateTime<Utc>,
    pub max_items: usize,
    pub max_bytes: usize,
    pub rendered_bytes: usize,
    pub result_identities: Vec<String>,
    pub ordinary_message_ids: Vec<String>,
    pub items: Vec<IdleInputBatchItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
pub struct LiveInputSafePoint {
    /// Original attachment attempt, retained across recovery.
    pub activation_id: String,
    pub provider_round: usize,
}

impl LiveInputSafePoint {
    pub(crate) fn new(activation_id: impl Into<String>, provider_round: usize) -> Self {
        Self {
            activation_id: activation_id.into(),
            provider_round,
        }
    }
}

impl LiveInputBatchRecord {
    pub(crate) const HEADER: &'static str =
        "Current live input batch (receive order; original authority and reply routes preserved):\n";

    pub(crate) fn rendered_text(&self) -> String {
        let mut text = String::from(Self::HEADER);
        for item in &self.items {
            text.push_str(&item.rendered);
        }
        text
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TurnTerminalCheckpointRecord {
    pub request_id: String,
    pub requested_at_round: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_round: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn_index: Option<u64>,
    pub text: String,
    pub checkpoint_anchor_generation: u64,
    pub current_anchor_generation: u64,
}
