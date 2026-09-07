use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageOrigin, Priority,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentIdentityLifecycle {
    Active,
    Deleting,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleFenceState {
    Open,
    DeletionFenced,
    Tombstoned,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentLineageCreationCause {
    LegacySpawn,
    CreateAgent,
    Migration,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentLineageRecord {
    pub child_agent_id: String,
    pub parent_agent_id: String,
    pub creation_cause: AgentLineageCreationCause,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSupervisionState {
    Active,
    CleanupRequired,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentSupervisionRecord {
    pub supervision_id: String,
    pub supervisor_agent_id: String,
    pub child_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from_task_id: Option<String>,
    pub state: AgentSupervisionState,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCanonicalDurability {
    Persistent,
    Ephemeral,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentDurabilityRecord {
    pub agent_id: String,
    pub durability: AgentCanonicalDurability,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleAttachment {
    Independent,
    SupervisionAttached,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentLifecycleAttachmentRecord {
    pub agent_id: String,
    pub attachment: AgentLifecycleAttachment,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapabilityFamily {
    CoreAgent,
    LocalEnvironment,
    Web,
    AgentCreation,
    AuthorityExpanding,
    ExternalTrigger,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPolicyEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentCapabilityPolicyRule {
    pub family: AgentCapabilityFamily,
    pub effect: AgentPolicyEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentCapabilityPolicyRecord {
    pub agent_id: String,
    pub revision: u64,
    pub rules: Vec<AgentCapabilityPolicyRule>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessagePrincipalKind {
    Operator,
    SupervisingParent,
    PeerAgent,
    ExternalIngress,
    RuntimeCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentMessagePolicyRule {
    pub principal_kind: AgentMessagePrincipalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    pub effect: AgentPolicyEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentMessagePolicyRecord {
    pub agent_id: String,
    pub revision: u64,
    pub default_effect: AgentPolicyEffect,
    pub rules: Vec<AgentMessagePolicyRule>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageDeliveryOutcome {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageDeliveryState {
    Queued,
    Dispatched,
    Consumed,
    Failed,
    CancelledByDeletion,
    Rejected,
}

impl AgentMessageDeliveryState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Consumed | Self::Failed | Self::CancelledByDeletion | Self::Rejected
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageDeliveryRejectionCode {
    TargetNotFound,
    MessageNotAuthorized,
    InvalidMessage,
    InvalidCorrelation,
    InvalidPriority,
    IdempotencyConflict,
    AgentStopped,
    AgentDeleting,
    AgentDeleted,
    QueueUnavailable,
}

impl AgentMessageDeliveryRejectionCode {
    pub fn retryable(self) -> bool {
        matches!(self, Self::AgentStopped | Self::QueueUnavailable)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentMessageSendRequest {
    pub target_agent_id: String,
    pub content: MessageBody,
    pub client_idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_priority: Option<Priority>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentMessageCallerContext {
    pub caller_principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_agent_id: Option<String>,
    pub principal_kind: AgentMessagePrincipalKind,
    pub route: String,
    pub origin: MessageOrigin,
    pub authority_class: AuthorityClass,
    pub delivery_surface: MessageDeliverySurface,
    pub admission_context: AdmissionContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work_item_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentMessageAdmissionEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_status: Option<AgentIdentityLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_policy_revision: Option<u64>,
    pub principal_kind: AgentMessagePrincipalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    pub route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentMessageDeliveryRecord {
    pub delivery_id: String,
    pub target_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    pub idempotency_scope: String,
    pub idempotency_key_digest: String,
    pub request_digest: String,
    pub caller: AgentMessageCallerContext,
    pub outcome: AgentMessageDeliveryOutcome,
    pub state: AgentMessageDeliveryState,
    pub state_version: u64,
    pub admission_evidence: AgentMessageAdmissionEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_code: Option<AgentMessageDeliveryRejectionCode>,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentMessageDeliveryReceipt {
    pub delivery_id: String,
    pub target_agent_id: String,
    pub outcome: AgentMessageDeliveryOutcome,
    pub state: AgentMessageDeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<DateTime<Utc>>,
    pub idempotent_replay: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_code: Option<AgentMessageDeliveryRejectionCode>,
    pub retryable: bool,
    pub lifecycle_snapshot: AgentMessageAdmissionEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl AgentMessageDeliveryRecord {
    pub fn receipt(&self, idempotent_replay: bool) -> AgentMessageDeliveryReceipt {
        AgentMessageDeliveryReceipt {
            delivery_id: self.delivery_id.clone(),
            target_agent_id: self.target_agent_id.clone(),
            outcome: self.outcome,
            state: self.state,
            accepted_at: self.accepted_at,
            terminal_at: self.terminal_at,
            idempotent_replay,
            rejection_code: self.rejection_code,
            retryable: self.retryable,
            lifecycle_snapshot: self.admission_evidence.clone(),
            correlation_id: self.correlation_id.clone(),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum AgentMessageDeliveryError {
    #[error("agent message idempotency key was reused with different request semantics")]
    IdempotencyConflict {
        idempotency_scope: String,
        idempotency_key_digest: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCanonicalRelationAxis {
    Lineage,
    Supervision,
    Durability,
    LifecycleAttachment,
    CapabilityPolicy,
    MessagePolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCanonicalValueSource {
    Canonical,
    Legacy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCanonicalResolution {
    Resolved,
    Ambiguous,
    Contradictory,
    MissingEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentCanonicalProjectionIssue {
    pub axis: AgentCanonicalRelationAxis,
    pub resolution: AgentCanonicalResolution,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct AgentCanonicalProjectionSources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<AgentCanonicalValueSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervision: Option<AgentCanonicalValueSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durability: Option<AgentCanonicalValueSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_attachment: Option<AgentCanonicalValueSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_policy: Option<AgentCanonicalValueSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_policy: Option<AgentCanonicalValueSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentCanonicalRelationsProjection {
    pub agent_id: String,
    pub identity_lifecycle: AgentIdentityLifecycle,
    pub lifecycle_fence: AgentLifecycleFenceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<AgentLineageRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervision: Option<AgentSupervisionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durability: Option<AgentDurabilityRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_attachment: Option<AgentLifecycleAttachmentRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_policy: Option<AgentCapabilityPolicyRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_policy: Option<AgentMessagePolicyRecord>,
    pub sources: AgentCanonicalProjectionSources,
    pub resolution: AgentCanonicalResolution,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<AgentCanonicalProjectionIssue>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentCanonicalRecordSet {
    pub lineage: Option<AgentLineageRecord>,
    pub supervision: Option<AgentSupervisionRecord>,
    pub durability: Option<AgentDurabilityRecord>,
    pub lifecycle_attachment: Option<AgentLifecycleAttachmentRecord>,
    pub capability_policy: Option<AgentCapabilityPolicyRecord>,
    pub message_policy: Option<AgentMessagePolicyRecord>,
}
