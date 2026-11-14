use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorNotificationBoundary {
    PrimaryOperator,
    ParentSupervisor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorNotificationRecord {
    pub notification_id: String,
    pub agent_id: String,
    pub requested_by_agent_id: String,
    pub target_operator_boundary: OperatorNotificationBoundary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_parent_agent_id: Option<String>,
    pub message: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorTransportBindingStatus {
    Active,
    Revoked,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorTransportDeliveryAuthKind {
    Bearer,
    Hmac,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorTransportDeliveryAuth {
    pub kind: OperatorTransportDeliveryAuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorTransportCapabilities {
    pub text: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperatorTransportBinding {
    pub binding_id: String,
    pub transport: String,
    pub operator_actor_id: String,
    pub target_agent_id: String,
    pub default_route_id: String,
    pub delivery_callback_url: String,
    pub delivery_auth: OperatorTransportDeliveryAuth,
    pub capabilities: OperatorTransportCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_identity_ref: Option<String>,
    pub status: OperatorTransportBindingStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorDeliveryStatus {
    Pending,
    AcceptedByTransport,
    FailedToSubmit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorDeliveryTriggerKind {
    OperatorNotification,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperatorDeliveryRecord {
    pub delivery_intent_id: String,
    pub output_event_id: String,
    pub agent_id: String,
    pub route_id: String,
    pub binding_id: String,
    pub trigger_kind: OperatorDeliveryTriggerKind,
    pub status: OperatorDeliveryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
