use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TurnTerminalKind;

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
