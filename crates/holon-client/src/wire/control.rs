use std::path::PathBuf;

use holon_domain::{AuthorityClass, BriefRecord, ControlAction, MessageEnvelope, TranscriptEntry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::daemon::RuntimeConfigSurface;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RenameAgentRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct DeleteAgentRequest {
    #[serde(default)]
    pub cascade_private_children: bool,
    #[serde(default)]
    pub archive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentDeletionResponse {
    pub ok: bool,
    pub created: bool,
    pub identity: holon_domain::AgentIdentityView,
    pub job: holon_domain::AgentDeletionJob,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentDeletionStatusResponse {
    pub identity: holon_domain::AgentIdentityView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<holon_domain::AgentDeletionJob>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone, PartialEq, Eq)]
pub struct ControlPromptRequest {
    pub text: String,
    /// Reject stale conversation pages after an agent ID is recreated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_incarnation: Option<u64>,
    #[serde(default)]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ControlPromptAttachment>,
    /// Stable caller-generated identity used to safely retry a prompt after
    /// the client loses the HTTP response. Older clients may omit it.
    #[serde(default)]
    pub client_request_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlPromptAttachment {
    Image(ControlPromptImageAttachment),
    File(ControlPromptFileAttachment),
}

impl JsonSchema for ControlPromptAttachment {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ControlPromptAttachment".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "oneOf": [
                generator.subschema_for::<ControlPromptImageAttachment>(),
                generator.subschema_for::<ControlPromptFileAttachment>()
            ],
            "discriminator": {
                "propertyName": "kind",
                "mapping": {
                    "image": "#/components/schemas/ControlPromptImageAttachment",
                    "file": "#/components/schemas/ControlPromptFileAttachment"
                }
            }
        })
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ControlPromptImageAttachment {
    pub name: Option<String>,
    pub media_type: String,
    pub data_base64: String,
}

impl JsonSchema for ControlPromptImageAttachment {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ControlPromptImageAttachment".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        control_prompt_attachment_schema("image")
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ControlPromptFileAttachment {
    pub name: Option<String>,
    pub media_type: String,
    pub data_base64: String,
}

impl JsonSchema for ControlPromptFileAttachment {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ControlPromptFileAttachment".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        control_prompt_attachment_schema("file")
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DebugPromptRequest {
    pub text: String,
    pub authority_class: Option<AuthorityClass>,
    pub manifest: Option<bool>,
    pub budget: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInputRequest {
    pub text: String,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<holon_domain::AgentInvocationContext>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStopRequest {
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<holon_domain::AgentInvocationContext>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BatchGetMessagesRequest {
    #[serde(default)]
    pub message_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchGetMessagesResponse {
    pub messages: Vec<MessageEnvelope>,
    #[serde(default)]
    pub missing_message_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BatchGetBriefsRequest {
    #[serde(default)]
    pub brief_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchGetBriefsResponse {
    pub briefs: Vec<BriefRecord>,
    #[serde(default)]
    pub missing_brief_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BatchGetTranscriptEntriesRequest {
    #[serde(default)]
    pub entry_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchGetTranscriptEntriesResponse {
    pub entries: Vec<TranscriptEntry>,
    #[serde(default)]
    pub missing_entry_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTimerRequest {
    pub duration_ms: Option<u64>,
    pub interval_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Display-only title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Trigger message. Do not combine with the legacy `summary` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// Legacy alias for `instruction`.
    pub summary: Option<String>,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelTimerRequest {
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ControlRequest {
    pub action: ControlAction,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AbortCurrentRunRequest {
    pub run_id: Option<String>,
    pub mode: Option<String>,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttachWorkspaceRequest {
    pub path: String,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExitWorkspaceRequest {
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetachWorkspaceRequest {
    pub workspace_id: String,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SetAgentModelRequest {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<holon_domain::model_values::ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ClearAgentModelRequest {
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetAgentTimezoneRequest {
    pub timezone: String,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ClearAgentTimezoneRequest {
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentRequest {
    pub authority_class: Option<AuthorityClass>,
    pub template: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// One-time hint for the language of the new agent's first default
    /// bootstrap reply. It only shapes that single runtime-owned bootstrap
    /// instruction: it never becomes an agent preference, and it does not
    /// affect later turns, existing agents, or explicit initial messages.
    #[serde(default)]
    pub initial_response_language: Option<String>,
}
fn control_prompt_attachment_schema(kind: &'static str) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "const": kind,
                "enum": [kind]
            },
            "name": {
                "type": ["string", "null"]
            },
            "media_type": {
                "type": "string"
            },
            "data_base64": {
                "type": "string"
            }
        },
        "required": ["kind", "media_type", "data_base64"]
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigReadResponse {
    pub ok: bool,
    pub config_file_path: std::path::PathBuf,
    pub runtime_surface: RuntimeConfigSurface,
    pub reload: RuntimeConfigReloadStatus,
    pub setup: RuntimeSetup,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfigUpdateRequest {
    pub updates: Vec<RuntimeConfigUpdateEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ModelConfigMigrationRequest {
    #[serde(default)]
    pub write: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfigUpdateEntry {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default)]
    pub unset: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigUpdateResponse {
    pub ok: bool,
    pub changed: bool,
    pub config_file_path: std::path::PathBuf,
    pub results: Vec<RuntimeConfigUpdateResult>,
    pub runtime_surface: RuntimeConfigSurface,
    pub reload: RuntimeConfigReloadStatus,
    pub setup: RuntimeSetup,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigReloadStatus {
    pub requested_generation: u64,
    pub completed_generation: u64,
    pub state: RuntimeConfigReloadState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl Default for RuntimeConfigReloadStatus {
    fn default() -> Self {
        Self {
            requested_generation: 0,
            completed_generation: 0,
            state: RuntimeConfigReloadState::Idle,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigReloadState {
    Idle,
    Applying,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigUpdateResult {
    pub key: String,
    pub effect: RuntimeConfigUpdateEffect,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigUpdateEffect {
    AcceptedRequiresRestart,
    AcceptedReloadScheduled,
    Rejected,
}

/// Safe first-run projection; readiness is local construction, not a live request.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct RuntimeSetup {
    pub status: RuntimeSetupStatus,
    pub default_model: Option<String>,
    pub provider: Option<String>,
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSetupStatus {
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelConfigMigrationStatus {
    Canonical,
    Legacy,
    Invalid,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ModelConfigMigrationField {
    pub location: String,
    pub status: ModelConfigMigrationStatus,
    pub current: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct ModelConfigMigrationStoreReport {
    pub changed: bool,
    pub fields: Vec<ModelConfigMigrationField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ModelConfigMigrationReport {
    pub ok: bool,
    pub write: bool,
    pub changed: bool,
    pub config_file_path: PathBuf,
    pub runtime_db_path: PathBuf,
    pub config: ModelConfigMigrationStoreReport,
    pub agent_state: ModelConfigMigrationStoreReport,
}
