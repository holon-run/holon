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
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResponse {
    pub query: String,
    pub limit: usize,
    pub results: Vec<crate::memory::MemorySearchResult>,
    pub index_status: crate::memory::MemorySearchIndexStatus,
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

#[derive(Debug, Serialize)]
pub(crate) struct EnqueueResponse {
    pub(crate) ok: bool,
    pub(crate) agent_id: String,
    pub(crate) message_id: String,
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
pub struct ControlPromptRequest {
    pub text: String,
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
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStopRequest {
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    pub(crate) before_seq: Option<u64>,
    pub(crate) after_seq: Option<u64>,
    pub(crate) limit: Option<usize>,
    pub(crate) order: Option<EventPageOrder>,
    pub(crate) max_level: Option<OperatorDisplayMode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStreamQuery {
    pub(crate) after_seq: Option<u64>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Serialize)]
pub(crate) struct EventsPageResponse {
    pub(crate) events: Vec<StreamEventEnvelope>,
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

#[derive(Debug, Default, Serialize)]
pub(crate) struct EventReplayProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) authority_class: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) delivery_surface: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) admission_context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transport: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reply_route: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) work_item_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) causation_id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct StateSessionSnapshot {
    pub(crate) current_run_id: Option<String>,
    pub(crate) pending_count: usize,
    pub(crate) last_turn: Option<TurnTerminalRecord>,
}

#[derive(Debug, Serialize)]
pub(crate) struct StateWorkspaceSnapshot {
    pub(crate) attached_workspaces: Vec<String>,
    pub(crate) active_workspace_entry: Option<ActiveWorkspaceEntry>,
    pub(crate) active_workspace_occupancy: Option<WorkspaceOccupancyRecord>,
    pub(crate) worktree_session: Option<WorktreeSession>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentStateSnapshot {
    pub(crate) agent: AgentSummary,
    pub(crate) session: StateSessionSnapshot,
    pub(crate) tasks: Vec<TaskRecord>,
    pub(crate) timers: Vec<TimerRecord>,
    pub(crate) work_items: Vec<WorkItemRecord>,
    pub(crate) waiting_intents: Vec<WaitingIntentRecord>,
    pub(crate) external_triggers: Vec<ExternalTriggerStateSnapshot>,
    pub(crate) workspace: StateWorkspaceSnapshot,
}

#[derive(Debug, Serialize)]
pub(crate) struct StreamEventEnvelope {
    pub(crate) id: String,
    pub(crate) event_seq: u64,
    pub(crate) ts: chrono::DateTime<Utc>,
    pub(crate) agent_id: String,
    #[serde(rename = "type")]
    pub(crate) event_type: String,
    pub(crate) provenance: EventReplayProvenance,
    pub(crate) payload: Value,
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
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateWorkItemRequest {
    pub objective: String,
    pub authority_class: Option<AuthorityClass>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PickWorkItemRequest {
    pub reason: Option<String>,
    #[serde(default)]
    pub clear_blocker: bool,
    pub authority_class: Option<AuthorityClass>,
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
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteWorkItemRequest {
    pub authority_class: Option<AuthorityClass>,
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
pub struct CreateAgentRequest {
    pub authority_class: Option<AuthorityClass>,
    pub template: Option<String>,
}
