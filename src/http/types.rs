use super::*;

pub(crate) const SEARCH_DEFAULT_LIMIT: usize = 20;
pub(crate) const SEARCH_MAX_LIMIT: usize = 50;

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_all_workspaces: bool,
    #[serde(default)]
    pub agent_ids: Option<Vec<String>>,
    #[serde(default)]
    pub types: Vec<String>,
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentDeletionResponse {
    pub ok: bool,
    pub created: bool,
    pub identity: crate::types::AgentIdentityView,
    pub job: crate::types::AgentDeletionJob,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentDeletionStatusResponse {
    pub identity: crate::types::AgentIdentityView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<crate::types::AgentDeletionJob>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResponse {
    pub query: String,
    pub limit: usize,
    pub results: Vec<crate::memory::MemorySearchResult>,
    /// Aggregate across the queried agents; per-agent detail is available in
    /// `index_status_by_agent` when the request spanned multiple agents.
    pub index_status: crate::memory::MemorySearchIndexStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_status_by_agent:
        Option<std::collections::BTreeMap<String, crate::memory::MemorySearchIndexStatus>>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone)]
pub struct MemoryGetRequest {
    pub source_ref: String,
    pub max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnqueueRequest {
    pub kind: Option<MessageKind>,
    pub priority: Option<Priority>,
    pub authority_class: Option<AuthorityClass>,
    pub body: Option<MessageBody>,
    pub text: Option<String>,
    pub json: Option<Value>,
    pub metadata: Option<Value>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub origin: Option<IncomingOrigin>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IncomingOrigin {
    Operator {
        actor_id: Option<String>,
    },
    Channel {
        channel_id: String,
        sender_id: Option<String>,
    },
    Webhook {
        source: String,
        event_type: Option<String>,
    },
    Timer {
        timer_id: String,
    },
    System {
        subsystem: String,
    },
    Task {
        task_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
pub(crate) struct EnqueueResponse {
    pub(crate) ok: bool,
    pub(crate) agent_id: String,
    pub(crate) message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) disposition: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ControlWakeRequest {
    pub reason: String,
    pub source: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WakeResponse {
    pub(crate) ok: bool,
    pub(crate) agent_id: String,
    pub(crate) disposition: WakeDisposition,
}

#[derive(Debug, Serialize)]
pub(crate) struct CallbackResponse {
    pub(crate) ok: bool,
    #[serde(flatten)]
    pub(crate) result: CallbackDeliveryResult,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone, PartialEq, Eq)]
pub struct ControlPromptRequest {
    pub text: String,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorTransportBindingRequest {
    pub binding_id: Option<String>,
    pub transport: String,
    pub operator_actor_id: String,
    pub target_agent_id: Option<String>,
    pub default_route_id: String,
    pub delivery_callback_url: String,
    pub delivery_auth: OperatorTransportDeliveryAuth,
    pub capabilities: OperatorTransportCapabilities,
    pub provider: Option<String>,
    pub provider_identity_ref: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorIngressRequest {
    pub text: String,
    pub actor_id: String,
    pub binding_id: String,
    pub conversation_ref: Option<String>,
    pub reply_route_id: Option<String>,
    pub provider: Option<String>,
    pub upstream_provider: Option<String>,
    pub provider_message_ref: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DebugPromptRequest {
    pub text: String,
    pub authority_class: Option<AuthorityClass>,
    pub manifest: Option<bool>,
    pub budget: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOutputQuery {
    pub(crate) block: Option<bool>,
    pub(crate) timeout_ms: Option<u64>,
}

pub(crate) const TASK_OUTPUT_DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInputRequest {
    pub text: String,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStopRequest {
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    pub(crate) before_seq: Option<u64>,
    pub(crate) after_seq: Option<u64>,
    pub(crate) limit: Option<usize>,
    pub(crate) order: Option<EventPageOrder>,
    pub(crate) max_level: Option<OperatorDisplayMode>,
    pub(crate) event_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStreamQuery {
    pub(crate) after_seq: Option<u64>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventPageOrder {
    Asc,
    Desc,
}

impl From<EventPageOrder> for EventLogPageOrder {
    fn from(order: EventPageOrder) -> Self {
        match order {
            EventPageOrder::Asc => Self::Asc,
            EventPageOrder::Desc => Self::Desc,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct EventsPageResponse {
    pub(crate) events: Vec<StreamEventEnvelope>,
    pub(crate) event_log_epoch: String,
    /// Stream-level envelope contract version for every event in this page.
    /// Per-event `contract_version` was removed; this is its single source.
    pub(crate) contract_version: u32,
    pub(crate) oldest_seq: Option<u64>,
    pub(crate) newest_seq: Option<u64>,
    pub(crate) cursor_seq: Option<u64>,
    pub(crate) has_older: bool,
    pub(crate) has_newer: bool,
    pub(crate) order: EventPageOrder,
    pub(crate) limit: usize,
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

/// One event on the public event surface. The `payload` object is the only
/// data source: correlation ids such as `message_id`, `task_id`,
/// `work_item_id`, `correlation_id`, and `causation_id` live in the payload
/// itself and are no longer duplicated into an envelope `provenance` object.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct StreamEventEnvelope {
    pub(crate) id: String,
    pub(crate) event_seq: u64,
    pub(crate) event_log_epoch: String,
    pub(crate) ts: chrono::DateTime<Utc>,
    pub(crate) agent_id: String,
    #[serde(rename = "type")]
    pub(crate) event_type: String,
    /// Registry payload schema. Present only on typed events; legacy audit
    /// events are schema-less and carry their data as-is. The envelope
    /// contract version is declared per stream, not per event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) payload_schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) payload_schema_version: Option<u32>,
    pub(crate) payload: Value,
    /// Additive classification derived from the runtime event registry.
    /// Present only while `events.projection-effect.v1` is advertised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) projection_effect: Option<crate::runtime_event::ProjectionEffect>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCommandTaskRequest {
    pub summary: String,
    pub cmd: String,
    pub workdir: Option<String>,
    pub shell: Option<String>,
    pub login: Option<bool>,
    pub tty: Option<bool>,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub accepts_input: Option<bool>,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateWorkItemRequest {
    pub objective: String,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PickWorkItemRequest {
    pub reason: Option<String>,
    #[serde(default)]
    pub clear_blocker: bool,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateWorkItemRequest {
    pub objective: Option<String>,
    pub plan_status: Option<WorkItemPlanStatus>,
    pub todo_list: Option<Vec<TodoItem>>,
    pub blocked_by: Option<Value>,
    #[schemars(range(min = 1))]
    pub recheck_after: Option<u64>,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteWorkItemRequest {
    pub report_text: String,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PickWorkItemResponse {
    pub previous_work_item: Option<WorkItemRecord>,
    pub current_work_item: WorkItemRecord,
    pub current_work_item_id: String,
    pub transition: crate::runtime::WorkItemFocusTransition,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTimerRequest {
    pub duration_ms: u64,
    pub interval_ms: Option<u64>,
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
    pub reasoning_effort: Option<String>,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ClearAgentModelRequest {
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentRequest {
    pub authority_class: Option<AuthorityClass>,
    pub template: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}
