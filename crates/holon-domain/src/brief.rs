use super::message::BriefAttachment;
use super::serde_helpers::default_agent_home_workspace_id;
use crate::ids;
use crate::workspace_identity::agent_home_workspace_id;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliverySummaryRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub work_item_id: String,
    pub created_at: DateTime<Utc>,
    pub text: String,
    #[serde(
        default,
        alias = "source_turn_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
}

impl DeliverySummaryRecord {
    pub fn new(
        agent_id: impl Into<String>,
        work_item_id: impl Into<String>,
        text: impl Into<String>,
        source_turn_index: Option<u64>,
        evidence: Option<Value>,
    ) -> Self {
        Self {
            id: ids::delivery_summary_id(),
            agent_id: agent_id.into(),
            work_item_id: work_item_id.into(),
            created_at: Utc::now(),
            text: text.into(),
            turn_id: None,
            source_turn_index,
            evidence,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BriefContentSourceRelation {
    DerivedFrom,
    Finalizes,
    Excerpt,
}

impl Default for BriefContentSourceRelation {
    fn default() -> Self {
        Self::DerivedFrom
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BriefContentSource {
    TranscriptEntry {
        entry_id: String,
        #[serde(default)]
        relation: BriefContentSourceRelation,
    },
    Inline,
}

impl Default for BriefContentSource {
    fn default() -> Self {
        Self::Inline
    }
}

impl BriefContentSource {
    pub fn transcript_entry_id(&self) -> Option<&str> {
        match self {
            Self::TranscriptEntry { entry_id, .. } => Some(entry_id),
            Self::Inline => None,
        }
    }

    pub fn relation(&self) -> Option<BriefContentSourceRelation> {
        match self {
            Self::TranscriptEntry { relation, .. } => Some(*relation),
            Self::Inline => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BriefKind {
    Ack,
    Result,
    Failure,
}

impl BriefKind {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Result | Self::Failure)
    }

    pub fn is_success(self) -> bool {
        matches!(self, Self::Result)
    }

    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failure)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Citation {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct BriefRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    #[serde(default = "default_agent_home_workspace_id")]
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub kind: BriefKind,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub content_source: BriefContentSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalizes_assistant_round_id: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<Citation>>,
    /// Immutable linkage to the unique `brief_created` audit event committed
    /// in the same runtime DB transition as this record. `None` for records
    /// whose creating event cannot be identified (pre-linkage history or
    /// ambiguous backfill candidates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_event_seq: Option<u64>,
    pub attachments: Option<Vec<BriefAttachment>>,
    pub related_message_id: Option<String>,
    pub related_task_id: Option<String>,
}

impl BriefRecord {
    pub fn new(
        agent_id: impl Into<String>,
        kind: BriefKind,
        text: impl Into<String>,
        related_message_id: Option<String>,
        related_task_id: Option<String>,
    ) -> Self {
        let agent_id = agent_id.into();
        Self {
            id: ids::brief_id(),
            workspace_id: agent_home_workspace_id(&agent_id),
            agent_id,
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: text.into(),
            citations: None,
            attachments: None,
            created_event_seq: None,
            related_message_id,
            related_task_id,
        }
    }
}

use crate::{AuditEvent, MessageEnvelope};

pub const QUEUED_WORK_ACK_PREFIX: &str = "Queued work: ";

pub fn make_acknowledgement_event(message: &MessageEnvelope) -> AuditEvent {
    AuditEvent::legacy(
        "message_acknowledged",
        serde_json::json!({
            "agent_id": &message.agent_id,
            "message_id": &message.id,
            "summary": format!("{QUEUED_WORK_ACK_PREFIX}{}", preview_message(message)),
        }),
    )
}

pub fn make_ack(agent_id: &str, message: &MessageEnvelope) -> BriefRecord {
    let preview = preview_message(message);
    BriefRecord::new(
        agent_id,
        BriefKind::Ack,
        format!("{QUEUED_WORK_ACK_PREFIX}{preview}"),
        Some(message.id.clone()),
        None,
    )
}

pub fn make_result(
    agent_id: &str,
    message: &MessageEnvelope,
    text: impl Into<String>,
) -> BriefRecord {
    BriefRecord::new(
        agent_id,
        BriefKind::Result,
        text,
        Some(message.id.clone()),
        None,
    )
}

pub fn make_failure(
    agent_id: &str,
    message: &MessageEnvelope,
    text: impl Into<String>,
) -> BriefRecord {
    BriefRecord::new(
        agent_id,
        BriefKind::Failure,
        text,
        Some(message.id.clone()),
        None,
    )
}

pub fn make_task_result(agent_id: &str, task_id: &str, text: impl Into<String>) -> BriefRecord {
    BriefRecord::new(
        agent_id,
        BriefKind::Result,
        text,
        None,
        Some(task_id.to_string()),
    )
}

fn preview_message(message: &MessageEnvelope) -> String {
    match &message.body {
        crate::MessageBody::Text { text } => truncate(text, 80),
        crate::MessageBody::Json { value } => truncate(&value.to_string(), 80),
        crate::MessageBody::Brief { text, .. } => truncate(text, 80),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max).collect();
    format!("{truncated}...")
}
