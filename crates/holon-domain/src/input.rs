use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
