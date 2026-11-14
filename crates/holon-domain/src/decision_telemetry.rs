use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionAdvisoryCompletedEvent {
    pub decision_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub request_fingerprint: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub abstain: bool,
    pub fallback: bool,
    pub timeout: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub evidence: Vec<Value>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionOutcomeRecordedEvent {
    pub decision_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub task_id: String,
    pub task_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_choice: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

pub fn stable_decision_id(
    agent_id: &str,
    turn_id: Option<&str>,
    message_id: Option<&str>,
    work_item_id: Option<&str>,
    tool_call_id: Option<&str>,
    request_fingerprint: &str,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        agent_id,
        turn_id.unwrap_or_default(),
        message_id.unwrap_or_default(),
        work_item_id.unwrap_or_default(),
        tool_call_id.unwrap_or_default(),
        request_fingerprint,
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    format!("decision_{:x}", hasher.finalize())
}

pub fn safe_value(value: Option<&Value>, max_chars: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|text| text.chars().take(max_chars).collect())
        .filter(|text: &String| !text.is_empty())
}
