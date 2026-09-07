use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
