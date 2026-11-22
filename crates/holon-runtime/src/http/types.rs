pub use holon_client::wire::{
    AbortCurrentRunRequest, AgentDeletionResponse, AgentDeletionStatusResponse,
    AttachWorkspaceRequest, BatchGetBriefsRequest, BatchGetBriefsResponse, BatchGetMessagesRequest,
    BatchGetMessagesResponse, BatchGetTranscriptEntriesRequest, BatchGetTranscriptEntriesResponse,
    CancelTimerRequest, ClearAgentModelRequest, ClearAgentTimezoneRequest, ControlPromptAttachment,
    ControlPromptFileAttachment, ControlPromptImageAttachment, ControlPromptRequest,
    ControlRequest, CreateAgentRequest, CreateTimerRequest, DebugPromptRequest, DeleteAgentRequest,
    DetachWorkspaceRequest, ExitWorkspaceRequest, RenameAgentRequest, SetAgentModelRequest,
    SetAgentTimezoneRequest, TaskInputRequest, TaskStopRequest,
};

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
pub struct EnqueueResponse {
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

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ActiveHistoryQuery {
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) status: crate::types::RecordListStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOutputQuery {
    pub(crate) block: Option<bool>,
    pub(crate) timeout_ms: Option<u64>,
}

pub(crate) const TASK_OUTPUT_DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    pub(crate) before_seq: Option<u64>,
    pub(crate) after_seq: Option<u64>,
    pub(crate) limit: Option<usize>,
    pub(crate) order: Option<EventPageOrder>,
    pub(crate) event_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStreamQuery {
    pub(crate) after_seq: Option<u64>,
    pub(crate) limit: Option<usize>,
}

pub(crate) use holon_server::events::EventPageOrder;

impl From<EventPageOrder> for EventLogPageOrder {
    fn from(order: EventPageOrder) -> Self {
        match order {
            EventPageOrder::Asc => Self::Asc,
            EventPageOrder::Desc => Self::Desc,
        }
    }
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
    #[serde(
        default,
        deserialize_with = "deserialize_recheck_after",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<u64>", range(min = 1))]
    pub recheck_after: Option<Option<u64>>,
    pub authority_class: Option<AuthorityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context: Option<crate::types::AgentInvocationContext>,
}

// Keep explicit null distinct from omission so the handler can reject it too.
fn deserialize_recheck_after<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<u64>>, D::Error> {
    Option::deserialize(deserializer).map(Some)
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
