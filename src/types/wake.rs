use super::message::MessageBody;
use crate::calendar::CalendarSchedule;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{de, Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallbackDeliveryMode {
    EnqueueMessage,
    #[serde(alias = "wake_only")]
    WakeHint,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTriggerScope {
    Agent,
}

impl<'de> Deserialize<'de> for ExternalTriggerScope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "agent" | "work_item" => Ok(Self::Agent),
            other => Err(de::Error::unknown_variant(other, &["agent"])),
        }
    }
}

fn default_external_trigger_scope() -> ExternalTriggerScope {
    ExternalTriggerScope::Agent
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitConditionStatus {
    Active,
    Triggered,
    Resolved,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitConditionKind {
    Task,
    External,
    Operator,
    Timer,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeSource {
    TaskResult {
        task_id: String,
    },
    ExternalIngress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_trigger_id: Option<String>,
    },
    Timer {
        wake_at: DateTime<Utc>,
    },
    OperatorInput,
    SystemTick,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWaitRecoverability {
    Recoverable,
    Weak,
    ExplicitNoFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WaitConditionRecord {
    pub id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub status: WaitConditionStatus,
    pub kind: WaitConditionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<String>,
    pub waiting_for: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_sources: Vec<WakeSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggered_at: Option<DateTime<Utc>>,
}

impl WaitConditionRecord {
    pub fn recheck_at(&self) -> Option<DateTime<Utc>> {
        self.continuation
            .as_ref()?
            .get("recheck_at")?
            .as_str()?
            .parse()
            .ok()
    }

    pub fn trigger_message_id(&self) -> Option<&str> {
        self.trigger_message_id.as_deref()
    }

    pub fn triggered_at(&self) -> Option<DateTime<Utc>> {
        self.triggered_at
    }

    pub fn mark_triggered(&mut self, message_id: &str, triggered_at: DateTime<Utc>) {
        self.trigger_message_id = Some(message_id.to_string());
        self.triggered_at = Some(triggered_at);
        self.status = WaitConditionStatus::Triggered;
        self.updated_at = triggered_at;
    }

    pub fn external_recoverability(&self) -> Option<ExternalWaitRecoverability> {
        if self.kind != WaitConditionKind::External || self.status != WaitConditionStatus::Active {
            return None;
        }
        if explicit_no_fallback_reason(self.continuation.as_ref()).is_some() {
            return Some(ExternalWaitRecoverability::ExplicitNoFallback);
        }
        if self.wake_sources.iter().any(recoverable_wake_source)
            || continuation_declares_recovery(self.continuation.as_ref())
        {
            return Some(ExternalWaitRecoverability::Recoverable);
        }
        Some(ExternalWaitRecoverability::Weak)
    }

    pub fn no_fallback_reason(&self) -> Option<String> {
        self.no_fallback_reason_ref().map(ToString::to_string)
    }

    pub(crate) fn no_fallback_reason_ref(&self) -> Option<&str> {
        explicit_no_fallback_reason(self.continuation.as_ref())
    }
}

fn recoverable_wake_source(source: &WakeSource) -> bool {
    matches!(source, WakeSource::Timer { .. } | WakeSource::SystemTick)
}

fn continuation_declares_recovery(continuation: Option<&Value>) -> bool {
    let Some(value) = continuation else {
        return false;
    };
    [
        "durable_queue",
        "durable_queue_ref",
        "recheck_source",
        "recheck_after",
        "recheck_at",
        "fallback",
        "fallback_source",
    ]
    .into_iter()
    .any(|key| value.get(key).is_some_and(json_truthy))
        || value
            .get("recoverability")
            .or_else(|| value.get("recovery"))
            .is_some_and(|value| {
                value
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind == "recoverable")
                    || value
                        .get("durable_queue")
                        .or_else(|| value.get("recheck_source"))
                        .or_else(|| value.get("fallback_source"))
                        .is_some_and(json_truthy)
            })
}

fn explicit_no_fallback_reason(continuation: Option<&Value>) -> Option<&str> {
    let value = continuation?;
    value
        .get("no_fallback_reason")
        .or_else(|| value.get("explicit_no_fallback_reason"))
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty())
        .or_else(|| {
            value
                .get("recoverability")
                .or_else(|| value.get("recovery"))
                .and_then(|value| {
                    let kind = value.get("kind").and_then(Value::as_str)?;
                    (kind == "explicit_no_fallback" || kind == "no_fallback")
                        .then(|| value.get("reason").and_then(Value::as_str))
                        .flatten()
                })
                .filter(|reason| !reason.trim().is_empty())
        })
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Number(_) => true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTriggerStatus {
    Active,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTriggerRecord {
    pub external_trigger_id: String,
    pub target_agent_id: String,
    #[serde(default = "default_external_trigger_scope")]
    pub scope: ExternalTriggerScope,
    pub delivery_mode: CallbackDeliveryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub token_hash: String,
    pub status: ExternalTriggerStatus,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_delivered_at: Option<DateTime<Utc>>,
    pub delivery_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ExternalTriggerStateSnapshot {
    pub external_trigger_id: String,
    pub target_agent_id: String,
    pub scope: ExternalTriggerScope,
    pub delivery_mode: CallbackDeliveryMode,
    pub status: ExternalTriggerStatus,
    pub delivery_count: u64,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_delivered_at: Option<DateTime<Utc>>,
}

impl From<ExternalTriggerRecord> for ExternalTriggerStateSnapshot {
    fn from(record: ExternalTriggerRecord) -> Self {
        Self {
            external_trigger_id: record.external_trigger_id,
            target_agent_id: record.target_agent_id,
            scope: record.scope,
            delivery_mode: record.delivery_mode,
            status: record.status,
            delivery_count: record.delivery_count,
            created_at: record.created_at,
            revoked_at: record.revoked_at,
            last_delivered_at: record.last_delivered_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WaitConditionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub status: WaitConditionStatus,
    pub kind: WaitConditionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<String>,
    pub waiting_for: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_sources: Vec<WakeSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_recoverability: Option<ExternalWaitRecoverability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<WaitConditionRecord> for WaitConditionSummary {
    fn from(record: WaitConditionRecord) -> Self {
        let external_recoverability = record.external_recoverability();
        let no_fallback_reason = record.no_fallback_reason();
        Self {
            id: record.id,
            work_item_id: record.work_item_id,
            status: record.status,
            kind: record.kind,
            source: record.source,
            subject_ref: record.subject_ref,
            waiting_for: record.waiting_for,
            wake_sources: record.wake_sources,
            external_recoverability,
            no_fallback_reason,
            continuation: record.continuation,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

impl From<&WaitConditionRecord> for WaitConditionSummary {
    fn from(record: &WaitConditionRecord) -> Self {
        Self {
            id: record.id.clone(),
            work_item_id: record.work_item_id.clone(),
            status: record.status.clone(),
            kind: record.kind.clone(),
            source: record.source.clone(),
            subject_ref: record.subject_ref.clone(),
            waiting_for: record.waiting_for.clone(),
            wake_sources: record.wake_sources.clone(),
            external_recoverability: record.external_recoverability(),
            no_fallback_reason: record.no_fallback_reason(),
            continuation: record.continuation.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTriggerSummary {
    pub external_trigger_id: String,
    pub target_agent_id: String,
    pub scope: ExternalTriggerScope,
    pub delivery_mode: CallbackDeliveryMode,
    pub status: ExternalTriggerStatus,
    pub delivery_count: u64,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTriggerCapability {
    pub external_trigger_id: String,
    pub trigger_url: String,
    pub target_agent_id: String,
    pub delivery_mode: CallbackDeliveryMode,
    pub status: ExternalTriggerStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallbackIngressDisposition {
    Enqueued,
    Triggered,
    Coalesced,
    Ignored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallbackDeliveryResult {
    pub agent_id: String,
    pub external_trigger_id: String,
    pub scope: ExternalTriggerScope,
    pub delivery_mode: CallbackDeliveryMode,
    pub disposition: CallbackIngressDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallbackDeliveryPayload {
    pub body: Option<MessageBody>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct TimerRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub interval_ms: Option<u64>,
    pub repeat: bool,
    #[serde(default)]
    pub status: TimerStatus,
    pub summary: Option<String>,
    #[serde(default)]
    pub next_fire_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_fired_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub fire_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarSchedule>,
}

/// Selection for explicit active/history list reads, applied before the limit.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordListStatus {
    #[default]
    All,
    Active,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatus {
    #[serde(alias = "scheduled")]
    Active,
    Completed,
    Cancelled,
}

impl Default for TimerStatus {
    fn default() -> Self {
        Self::Active
    }
}
