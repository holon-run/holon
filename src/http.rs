use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use anyhow::{anyhow, Result};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, MatchedPath, Path, Query, State},
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        HeaderMap, Request as AxumRequest, Response, StatusCode,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response as AxumResponse,
    },
    routing::{get, patch, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::{
    classify::ServerErrorsFailureClass, compression::CompressionLayer, trace::TraceLayer,
};
use tracing::{error, info, warn, Span};

#[cfg(unix)]
use axum::body::Body;
#[cfg(unix)]
use hyper::{body::Incoming, service::service_fn, Request};
#[cfg(unix)]
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as HyperBuilder,
};
#[cfg(unix)]
use std::convert::Infallible;
#[cfg(unix)]
use tokio::{net::UnixListener, sync::watch};
#[cfg(unix)]
use tower::ServiceExt;

use crate::{
    config::{
        credential_store_path, load_credential_store_at, load_persisted_config_at,
        save_persisted_config_at, set_config_key, unset_config_key, ControlTransportKind,
        HolonConfigFile, ModelRef,
    },
    daemon::{
        graceful_runtime_shutdown, runtime_activity_summary, RuntimeConfigSurface,
        RuntimeServiceHandle,
    },
    host::{PublicAgentError, RuntimeHost},
    ingress::{InboundRequest, WakeDisposition, WakeHint},
    operator_event::{
        is_operator_event_in_display_mode, OperatorDisplayMode, OperatorPresentationContext,
    },
    policy::{default_authority_for_origin, validate_message_kind_for_origin},
    runtime::{CurrentRunAbortError, CurrentRunAbortMode, CurrentRunAbortRequest},
    storage::{EventLogPageOrder, FileActivityMarker},
    system::{ExecutionScopeKind, ExecutionSnapshot, HostLocalBoundary},
    types::{
        ActiveWorkspaceEntry, AdmissionContext, AgentRegistryStatus, AgentSummary, AgentVisibility,
        AuditEvent, AuthorityClass, CallbackDeliveryPayload, CallbackDeliveryResult, ControlAction,
        ExternalTriggerStateSnapshot, MessageBody, MessageDeliverySurface, MessageKind,
        MessageOrigin, OperatorNotificationRecord, OperatorTransportBinding,
        OperatorTransportBindingStatus, OperatorTransportCapabilities,
        OperatorTransportDeliveryAuth, OperatorTransportDeliveryAuthKind, Priority, TaskRecord,
        TaskStatus, TaskStatusSnapshot, TaskStopResult, TimerRecord, TodoItem, TurnTerminalRecord,
        WaitingIntentRecord, WaitingReason, WorkItemPlanStatus, WorkItemRecord, WorkItemState,
        WorkspaceOccupancyRecord, WorktreeSession,
    },
};

const STATE_BOOTSTRAP_TASK_LIMIT: usize = 40;
const STATE_BOOTSTRAP_TASK_DETAIL_STRING_LIMIT: usize = 2048;
#[cfg(test)]
const STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT: usize = 8192;
const STATE_BOOTSTRAP_JSON_ARRAY_LIMIT: usize = 64;
const HTTP_SLOW_RESPONSE_WARN_AFTER: std::time::Duration = std::time::Duration::from_secs(2);
const HTTP_LARGE_RESPONSE_WARN_BYTES: usize = 128 * 1024;

static HTTP_IN_FLIGHT_REQUESTS: AtomicUsize = AtomicUsize::new(0);

fn decrement_http_in_flight_requests() -> usize {
    let mut current = HTTP_IN_FLIGHT_REQUESTS.load(Ordering::Relaxed);
    loop {
        if current == 0 {
            return 0;
        }
        match HTTP_IN_FLIGHT_REQUESTS.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return current - 1,
            Err(next) => current = next,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub host: RuntimeHost,
    pub require_control_token: bool,
    pub runtime_service: Option<RuntimeServiceHandle>,
    pub advertise_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HttpErrorEnvelope {
    ok: bool,
    error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

impl HttpErrorEnvelope {
    fn new(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: error.into(),
            code: None,
            hint: None,
            extensions: Map::new(),
        }
    }

    fn code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn extension(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extensions.insert(key.into(), value.into());
        self
    }
}

const CALLBACK_BODY_LIMIT_BYTES: usize = 256 * 1024;
const DEFAULT_EVENT_STREAM_WINDOW: usize = 128;
const MAX_EVENT_STREAM_WINDOW: usize = 512;
const EVENT_STREAM_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const EVENT_STREAM_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

impl AppState {
    pub fn for_tcp(host: RuntimeHost) -> Self {
        Self::for_tcp_with_runtime_service(host, None)
    }

    pub fn for_tcp_with_runtime_service(
        host: RuntimeHost,
        runtime_service: Option<RuntimeServiceHandle>,
    ) -> Self {
        let require_control_token = host
            .config()
            .control_token_required(ControlTransportKind::Tcp);
        Self {
            host,
            require_control_token,
            runtime_service,
            advertise_url: None,
        }
    }

    pub fn for_unix(host: RuntimeHost) -> Self {
        Self::for_unix_with_runtime_service(host, None)
    }

    pub fn for_unix_with_runtime_service(
        host: RuntimeHost,
        runtime_service: Option<RuntimeServiceHandle>,
    ) -> Self {
        // Unix control is local IPC; filesystem permissions are the access boundary.
        Self {
            host,
            require_control_token: false,
            runtime_service,
            advertise_url: None,
        }
    }

    pub fn with_advertise_url(mut self, advertise_url: Option<String>) -> Self {
        self.advertise_url = advertise_url;
        self
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/handshake", get(handshake))
        .route("/models", get(models_handler))
        .route("/agents/list", get(list_agent_entries))
        .route("/agents/{agent_id}/enqueue", post(enqueue))
        .route("/agents/{agent_id}/status", get(status))
        .route("/control/agents/{agent_id}/status", get(control_status))
        .route("/agents/{agent_id}/briefs", get(briefs))
        .route("/agents/{agent_id}/state", get(agent_state))
        .route("/agents/{agent_id}/events", get(events))
        .route("/agents/{agent_id}/events/stream", get(events_stream))
        .route("/agents/{agent_id}/transcript", get(transcript))
        .route("/agents/{agent_id}/tasks", get(tasks))
        .route("/agents/{agent_id}/tasks/{task_id}", get(task_status))
        .route(
            "/agents/{agent_id}/tasks/{task_id}/output",
            get(task_output),
        )
        .route(
            "/control/agents/{agent_id}/tasks/{task_id}/input",
            post(task_input),
        )
        .route(
            "/control/agents/{agent_id}/tasks/{task_id}/stop",
            post(task_stop),
        )
        .route("/agents/{agent_id}/work-items", get(work_items))
        .route(
            "/agents/{agent_id}/work-items/{work_item_id}",
            get(work_item),
        )
        .route("/agents/{agent_id}/worktree-summary", get(worktree_summary))
        .route("/agents/{agent_id}/timers", get(timers))
        .route("/agents/{agent_id}/timers/{timer_id}", get(timer))
        .route(
            "/control/agents/{agent_id}/tasks",
            post(create_command_task),
        )
        .route(
            "/control/agents/{agent_id}/work-items",
            post(create_work_item),
        )
        .route(
            "/control/agents/{agent_id}/work-items/{work_item_id}/pick",
            post(pick_work_item),
        )
        .route(
            "/control/agents/{agent_id}/work-items/{work_item_id}",
            patch(update_work_item),
        )
        .route(
            "/control/agents/{agent_id}/work-items/{work_item_id}/complete",
            post(complete_work_item),
        )
        .route("/control/agents/{agent_id}/timers", post(create_timer))
        .route(
            "/control/agents/{agent_id}/timers/{timer_id}/cancel",
            post(cancel_timer),
        )
        .route("/control/agents/{agent_id}/create", post(create_agent))
        .route(
            "/control/agents/{agent_id}/workspace/attach",
            post(attach_workspace),
        )
        .route(
            "/control/agents/{agent_id}/workspace/exit",
            post(exit_workspace),
        )
        .route(
            "/control/agents/{agent_id}/workspace/detach",
            post(detach_workspace),
        )
        .route("/control/agents/{agent_id}/model", post(set_agent_model))
        .route(
            "/control/agents/{agent_id}/model/clear",
            post(clear_agent_model),
        )
        .route("/control/agents/{agent_id}/control", post(control))
        .route(
            "/control/agents/{agent_id}/current-run/abort",
            post(abort_current_run),
        )
        .route("/control/agents/{agent_id}/prompt", post(control_prompt))
        .route(
            "/control/agents/{agent_id}/operator-bindings",
            post(create_operator_transport_binding),
        )
        .route(
            "/control/agents/{agent_id}/operator-ingress",
            post(operator_ingress),
        )
        .route("/control/runtime/readiness", get(runtime_readiness))
        .route("/control/runtime/status", get(runtime_status))
        .route("/control/runtime/config", get(runtime_config))
        .route("/control/runtime/config", patch(runtime_config_update))
        .route("/control/runtime/shutdown", post(runtime_shutdown))
        .route(
            "/control/agents/{agent_id}/debug-prompt",
            post(control_debug_prompt),
        )
        .route("/control/agents/{agent_id}/wake", post(control_wake))
        .route(
            "/callbacks/enqueue/{callback_token}",
            post(callback_ingress_enqueue).layer(DefaultBodyLimit::max(CALLBACK_BODY_LIMIT_BYTES)),
        )
        .route(
            "/callbacks/wake/{callback_token}",
            post(callback_ingress_wake).layer(DefaultBodyLimit::max(CALLBACK_BODY_LIMIT_BYTES)),
        )
        .route("/webhooks/generic/{agent_id}", post(generic_webhook))
        .route("/enqueue", post(enqueue_default))
        .route("/agents/{agent_id}/skills", get(list_skills))
        .route(
            "/control/agents/{agent_id}/skills/install",
            post(install_skill),
        )
        .route(
            "/control/agents/{agent_id}/skills/uninstall",
            post(uninstall_skill),
        )
        .route("/status", get(status_default))
        .route("/briefs", get(briefs_default))
        .route("/state", get(state_default))
        .route("/transcript", get(transcript_default))
        .route("/worktree-summary", get(worktree_summary_default))
        .fallback(not_found_handler)
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &AxumRequest<axum::body::Body>| {
                    let matched_path = request
                        .extensions()
                        .get::<MatchedPath>()
                        .map(MatchedPath::as_str)
                        .unwrap_or_else(|| request.uri().path());
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        path = %request.uri().path(),
                        matched_path,
                        status = tracing::field::Empty,
                        response_bytes = tracing::field::Empty,
                        elapsed_ms = tracing::field::Empty,
                        in_flight = tracing::field::Empty,
                    )
                })
                .on_request(|_request: &AxumRequest<axum::body::Body>, span: &Span| {
                    let in_flight = HTTP_IN_FLIGHT_REQUESTS.fetch_add(1, Ordering::Relaxed) + 1;
                    span.record("in_flight", in_flight);
                })
                .on_response(
                    |response: &Response<axum::body::Body>,
                     elapsed: std::time::Duration,
                     span: &Span| {
                        let in_flight = decrement_http_in_flight_requests();
                        let response_bytes = response
                            .headers()
                            .get(axum::http::header::CONTENT_LENGTH)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<usize>().ok());
                        span.record("status", response.status().as_u16());
                        span.record("elapsed_ms", elapsed.as_millis() as u64);
                        span.record("in_flight", in_flight);
                        if let Some(bytes) = response_bytes {
                            span.record("response_bytes", bytes);
                        }

                        let is_slow = elapsed >= HTTP_SLOW_RESPONSE_WARN_AFTER;
                        let is_large = response_bytes
                            .is_some_and(|bytes| bytes >= HTTP_LARGE_RESPONSE_WARN_BYTES);
                        if is_slow || is_large {
                            warn!(
                                status = response.status().as_u16(),
                                elapsed_ms = elapsed.as_millis() as u64,
                                response_bytes,
                                in_flight,
                                "slow or large HTTP response"
                            );
                        } else {
                            info!(
                                status = response.status().as_u16(),
                                elapsed_ms = elapsed.as_millis() as u64,
                                response_bytes,
                                in_flight,
                                "HTTP response"
                            );
                        }
                    },
                )
                .on_failure(
                    |failure: ServerErrorsFailureClass,
                     elapsed: std::time::Duration,
                     span: &Span| {
                        if matches!(failure, ServerErrorsFailureClass::Error(_)) {
                            let in_flight = decrement_http_in_flight_requests();
                            span.record("elapsed_ms", elapsed.as_millis() as u64);
                            span.record("in_flight", in_flight);
                            warn!(
                                %failure,
                                elapsed_ms = elapsed.as_millis() as u64,
                                in_flight,
                                "HTTP request failed before response"
                            );
                        }
                    },
                ),
        )
        .with_state(Arc::new(state))
}

fn traced_json<T: Serialize>(
    route: &'static str,
    started_at: std::time::Instant,
    value: T,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    let bytes = serde_json::to_vec(&value).map_err(|err| error_response(err.into()))?;
    let build_elapsed = started_at.elapsed();
    if build_elapsed >= HTTP_SLOW_RESPONSE_WARN_AFTER
        || bytes.len() >= HTTP_LARGE_RESPONSE_WARN_BYTES
    {
        warn!(
            route,
            handler_build_ms = build_elapsed.as_millis() as u64,
            response_bytes = bytes.len(),
            "large or slow HTTP JSON payload built"
        );
    }
    Ok(([(CONTENT_TYPE, "application/json")], bytes).into_response())
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
struct EnqueueResponse {
    ok: bool,
    agent_id: String,
    message_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ControlWakeRequest {
    pub reason: String,
    pub source: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct WakeResponse {
    ok: bool,
    agent_id: String,
    disposition: WakeDisposition,
}

#[derive(Debug, Serialize)]
struct CallbackResponse {
    ok: bool,
    #[serde(flatten)]
    result: CallbackDeliveryResult,
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
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOutputQuery {
    block: Option<bool>,
    timeout_ms: Option<u64>,
}

const TASK_OUTPUT_DEFAULT_TIMEOUT_MS: u64 = 30_000;

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
    before_seq: Option<u64>,
    after_seq: Option<u64>,
    limit: Option<usize>,
    order: Option<EventPageOrder>,
    max_level: Option<OperatorDisplayMode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStreamQuery {
    after_seq: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EventPageOrder {
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
struct EventsPageResponse {
    events: Vec<StreamEventEnvelope>,
    oldest_seq: Option<u64>,
    newest_seq: Option<u64>,
    cursor_seq: Option<u64>,
    has_older: bool,
    has_newer: bool,
    order: EventPageOrder,
    limit: usize,
}

#[derive(Debug, Default, Serialize)]
struct EventReplayProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authority_class: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_surface: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    admission_context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_route: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_item_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    causation_id: Option<Value>,
}

#[derive(Debug, Serialize)]
struct StateSessionSnapshot {
    current_run_id: Option<String>,
    pending_count: usize,
    last_turn: Option<TurnTerminalRecord>,
}

#[derive(Debug, Serialize)]
struct StateWorkspaceSnapshot {
    attached_workspaces: Vec<String>,
    active_workspace_entry: Option<ActiveWorkspaceEntry>,
    active_workspace_occupancy: Option<WorkspaceOccupancyRecord>,
    worktree_session: Option<WorktreeSession>,
}

#[derive(Debug, Serialize)]
struct AgentStateSnapshot {
    agent: AgentSummary,
    session: StateSessionSnapshot,
    tasks: Vec<TaskRecord>,
    timers: Vec<TimerRecord>,
    work_items: Vec<WorkItemRecord>,
    waiting_intents: Vec<WaitingIntentRecord>,
    external_triggers: Vec<ExternalTriggerStateSnapshot>,
    operator_notifications: Vec<OperatorNotificationRecord>,
    workspace: StateWorkspaceSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<ExecutionSnapshot>,
}

#[derive(Debug, Serialize)]
struct StreamEventEnvelope {
    id: String,
    event_seq: u64,
    ts: chrono::DateTime<Utc>,
    agent_id: String,
    #[serde(rename = "type")]
    event_type: String,
    provenance: EventReplayProvenance,
    payload: Value,
}

struct EventStreamState {
    runtime: crate::runtime::RuntimeHandle,
    runtime_id: String,
    event_window_limit: usize,
    event_marker: FileActivityMarker,
    last_seen_seq: u64,
    buffered: VecDeque<AuditEvent>,
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

pub async fn root(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "default_agent": state.host.config().default_agent_id,
    })))
}

pub async fn models_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state.host.default_runtime().await.map_err(error_response)?;
    let available_models = runtime.available_models().await.map_err(error_response)?;
    let model_availability = runtime.model_availability().await.map_err(error_response)?;
    traced_json(
        "/models",
        started_at,
        json!({
            "available_models": available_models,
            "model_availability": model_availability,
        }),
    )
}

pub async fn handshake(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let config = state.host.config();
    Ok(Json(json!({
        "ok": true,
        "protocol": {
            "name": "holon-control",
            "version": 1,
        },
        "auth": {
            "mode": if state.require_control_token { "bearer" } else { "local" },
            "required": state.require_control_token,
        },
        "capabilities": [
            "agents.list",
            "agents.state",
            "agents.events",
            "agents.control",
            "tui.remote"
        ],
        "runtime": {
            "default_agent": config.default_agent_id,
            "workspace_dir": config.workspace_dir,
            "home_dir": config.home_dir,
            "listen": config.http_addr,
            "advertise_url": state.advertise_url,
        }
    })))
}

pub async fn list_agent_entries(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let agents = state
        .host
        .list_agent_entries()
        .await
        .map_err(error_response)?;
    traced_json("/agents/list", started_at, agents)
}

pub async fn runtime_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime_service = state
        .runtime_service
        .as_ref()
        .ok_or_else(|| service_unavailable("runtime service metadata is unavailable"))?;
    let activity = runtime_activity_summary(&state.host)
        .await
        .map_err(error_response)?;
    let last_failure = state
        .host
        .public_agent_activity_snapshots()
        .await
        .map_err(error_response)?
        .into_iter()
        .filter_map(|agent| agent.last_runtime_failure)
        .max_by(|left, right| left.occurred_at.cmp(&right.occurred_at));
    let (startup_surface, runtime_surface) = runtime_surfaces(&state);
    Ok(Json(runtime_service.status_response(
        activity,
        last_failure,
        startup_surface,
        runtime_surface,
    )))
}

pub async fn runtime_readiness(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime_service = state
        .runtime_service
        .as_ref()
        .ok_or_else(|| service_unavailable("runtime service metadata is unavailable"))?;
    let (startup_surface, runtime_surface) = runtime_surfaces(&state);
    Ok(Json(
        runtime_service.readiness_response(startup_surface, runtime_surface),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigReadResponse {
    pub ok: bool,
    pub config_file_path: std::path::PathBuf,
    pub runtime_surface: RuntimeConfigSurface,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfigUpdateRequest {
    pub updates: Vec<RuntimeConfigUpdateEntry>,
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
    Rejected,
}

pub async fn runtime_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let config = state.host.config();
    Ok(Json(RuntimeConfigReadResponse {
        ok: true,
        config_file_path: config.config_file_path.clone(),
        runtime_surface: RuntimeConfigSurface::new(config),
    }))
}

pub async fn runtime_config_update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<RuntimeConfigUpdateRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let config = state.host.config();
    let stored = load_persisted_config_at(&config.config_file_path).map_err(error_response)?;
    let mut candidate = stored.clone();
    let mut results = Vec::new();

    for update in request.updates {
        if !is_runtime_mutable_config_key(&update.key) {
            results.push(RuntimeConfigUpdateResult {
                key: update.key,
                effect: RuntimeConfigUpdateEffect::Rejected,
                reason: "unsupported or startup-only config key".into(),
            });
            continue;
        }

        let result = if update.unset {
            unset_config_key(&mut candidate, &update.key)
        } else {
            match update.value {
                Some(value) => {
                    set_config_key(&mut candidate, &update.key, &config_value_as_raw(value))
                }
                None => Err(anyhow!(
                    "runtime config update for {} requires value or unset=true",
                    update.key
                )),
            }
        };

        match result {
            Ok(()) => {
                results.push(RuntimeConfigUpdateResult {
                    key: update.key,
                    effect: RuntimeConfigUpdateEffect::AcceptedRequiresRestart,
                    reason: "persisted in config.json; the running host keeps its current effective config until restart/reload support is added".into(),
                });
            }
            Err(error) => results.push(RuntimeConfigUpdateResult {
                key: update.key,
                effect: RuntimeConfigUpdateEffect::Rejected,
                reason: error.to_string(),
            }),
        }
    }

    if results
        .iter()
        .any(|result| result.effect == RuntimeConfigUpdateEffect::Rejected)
    {
        reject_accepted_runtime_config_results(
            &mut results,
            "batch rejected; no runtime config updates were persisted",
        );
    } else if let Err(error) = validate_runtime_config_candidate(config, &candidate) {
        reject_accepted_runtime_config_results(
            &mut results,
            &format!("updated config is invalid: {error}"),
        );
    }

    let changed = results
        .iter()
        .any(|result| result.effect == RuntimeConfigUpdateEffect::AcceptedRequiresRestart);

    if changed {
        save_persisted_config_at(&config.config_file_path, &candidate).map_err(error_response)?;
    }

    Ok(Json(RuntimeConfigUpdateResponse {
        ok: true,
        changed,
        config_file_path: config.config_file_path.clone(),
        results,
        runtime_surface: RuntimeConfigSurface::new(config),
    }))
}

fn reject_accepted_runtime_config_results(results: &mut [RuntimeConfigUpdateResult], reason: &str) {
    for result in results {
        if result.effect == RuntimeConfigUpdateEffect::AcceptedRequiresRestart {
            result.effect = RuntimeConfigUpdateEffect::Rejected;
            if result.reason.is_empty() {
                result.reason = reason.into();
            } else {
                result.reason = format!("{reason}: {}", result.reason);
            }
        }
    }
}

fn validate_runtime_config_candidate(
    config: &crate::config::AppConfig,
    candidate: &HolonConfigFile,
) -> Result<()> {
    let credentials = load_credential_store_at(&credential_store_path(&config.home_dir))?;
    crate::web::materialize_web_config(&candidate.web, &credentials)?;
    Ok(())
}

fn is_runtime_mutable_config_key(key: &str) -> bool {
    matches!(
        key,
        "model.default"
            | "model.fallbacks"
            | "models.catalog"
            | "model.unknown_fallback"
            | "model.unknown_fallback.context_window_tokens"
            | "model.unknown_fallback.effective_context_window_percent"
            | "model.unknown_fallback.prompt_budget_estimated_tokens"
            | "model.unknown_fallback.compaction_trigger_estimated_tokens"
            | "model.unknown_fallback.compaction_keep_recent_estimated_tokens"
            | "model.unknown_fallback.runtime_max_output_tokens"
            | "runtime.max_output_tokens"
            | "runtime.default_tool_output_tokens"
            | "runtime.max_tool_output_tokens"
            | "runtime.disable_provider_fallback"
    ) || key.starts_with("web.")
}

fn config_value_as_raw(value: Value) -> String {
    match value {
        Value::String(value) => value,
        other => other.to_string(),
    }
}

fn runtime_surfaces(
    state: &AppState,
) -> (crate::daemon::RuntimeStartupSurface, RuntimeConfigSurface) {
    let config = state.host.config();
    let startup_surface = crate::daemon::RuntimeStartupSurface {
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        workspace_dir: config.workspace_dir.clone(),
        default_agent_id: config.default_agent_id.clone(),
        callback_base_url: config.callback_base_url.clone(),
        control_token_configured: config.control_token.is_some(),
        control_auth_mode: config.control_auth_mode.into(),
    };
    (startup_surface, RuntimeConfigSurface::new(config))
}

pub async fn runtime_shutdown(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime_service = state
        .runtime_service
        .as_ref()
        .ok_or_else(|| service_unavailable("runtime service metadata is unavailable"))?;
    graceful_runtime_shutdown(&state.host, runtime_service)
        .await
        .map_err(error_response)?;
    Ok(Json(runtime_service.shutdown_response()))
}

pub async fn enqueue_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<EnqueueRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let agent_id = state.host.config().default_agent_id.clone();
    enqueue_internal(state, agent_id, request, EnqueueIngress::Public).await
}

pub async fn enqueue(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<EnqueueRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    enqueue_internal(state, agent_id, request, EnqueueIngress::Public).await
}

#[derive(Debug, Clone, Copy)]
enum EnqueueIngress {
    Public,
    Trusted {
        delivery_surface: MessageDeliverySurface,
        admission_context: AdmissionContext,
    },
}

fn public_admission_context() -> AdmissionContext {
    AdmissionContext::PublicUnauthenticated
}

fn control_admission_context(state: &AppState) -> AdmissionContext {
    if state.require_control_token {
        AdmissionContext::ControlAuthenticated
    } else {
        AdmissionContext::LocalProcess
    }
}

async fn current_boundary_metadata(runtime: &crate::runtime::RuntimeHandle) -> Result<Value> {
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    Ok(HostLocalBoundary::from_snapshot(&execution.snapshot()).audit_metadata())
}

async fn enqueue_internal(
    state: Arc<AppState>,
    agent_id: String,
    request: EnqueueRequest,
    ingress: EnqueueIngress,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let kind = request.kind.unwrap_or(MessageKind::WebhookEvent);
    if matches!(kind, MessageKind::SystemTick | MessageKind::CallbackEvent) {
        return Err(forbidden(
            "runtime-owned message kinds may not be enqueued externally",
        ));
    }
    let priority = request.priority.unwrap_or(Priority::Normal);
    if matches!(ingress, EnqueueIngress::Public) && priority == Priority::Interject {
        return Err(forbidden("public enqueue may not use interject priority"));
    }
    let origin = match ingress {
        EnqueueIngress::Public => match request.origin {
            Some(IncomingOrigin::Channel {
                channel_id,
                sender_id,
            }) => MessageOrigin::Channel {
                channel_id,
                sender_id,
            },
            Some(IncomingOrigin::Webhook { source, event_type }) => {
                MessageOrigin::Webhook { source, event_type }
            }
            Some(_) => {
                return Err(forbidden(
                    "public enqueue only accepts channel or webhook origins",
                ));
            }
            None => MessageOrigin::Webhook {
                source: "http".into(),
                event_type: None,
            },
        },
        EnqueueIngress::Trusted { .. } => {
            request
                .origin
                .map(into_origin)
                .unwrap_or(MessageOrigin::Webhook {
                    source: "http".into(),
                    event_type: None,
                })
        }
    };
    let authority_class = match ingress {
        EnqueueIngress::Public => {
            if request.authority_class.is_some() {
                return Err(forbidden("public enqueue may not override authority_class"));
            }
            default_authority_for_origin(&origin)
        }
        EnqueueIngress::Trusted { .. } => request
            .authority_class
            .unwrap_or_else(|| default_authority_for_origin(&origin)),
    };
    let (delivery_surface, admission_context) = match ingress {
        EnqueueIngress::Public => (
            MessageDeliverySurface::HttpPublicEnqueue,
            public_admission_context(),
        ),
        EnqueueIngress::Trusted {
            delivery_surface,
            admission_context,
        } => (delivery_surface, admission_context),
    };
    let kind_decision = validate_message_kind_for_origin(&kind, &origin);
    if !kind_decision.allowed {
        return Err(forbidden(kind_decision.reason));
    }

    let body = request
        .body
        .unwrap_or_else(|| match (request.text, request.json) {
            (Some(text), _) => MessageBody::Text { text },
            (_, Some(value)) => MessageBody::Json { value },
            _ => MessageBody::Text {
                text: String::new(),
            },
        });

    let message = InboundRequest {
        agent_id: agent_id.clone(),
        kind,
        priority,
        origin,
        authority_class,
        body,
        delivery_surface,
        admission_context,
        metadata: request.metadata,
        correlation_id: request.correlation_id,
        causation_id: request.causation_id,
    }
    .into_message();

    let runtime = state
        .host
        .get_public_agent_for_external_ingress(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let queued = runtime.enqueue(message).await.map_err(error_response)?;

    Ok(Json(EnqueueResponse {
        ok: true,
        agent_id,
        message_id: queued.id,
    }))
}

pub async fn status_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    status(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
    )
    .await
}

pub async fn status(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent = runtime.agent_summary().await.map_err(error_response)?;
    Ok(Json(agent))
}

pub async fn control_status(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_or_create_agent(&agent_id)
        .await
        .map_err(error_response)?;
    let agent = runtime.agent_summary().await.map_err(error_response)?;
    Ok(Json(agent))
}

pub async fn state_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    agent_state(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
    )
    .await
}

pub async fn agent_state(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent = runtime.agent_summary().await.map_err(error_response)?;
    let tasks = runtime
        .active_tasks(STATE_BOOTSTRAP_TASK_LIMIT)
        .await
        .map_err(error_response)?
        .into_iter()
        .map(slim_state_task_record)
        .collect();
    let timers = runtime.recent_timers(50).await.map_err(error_response)?;
    let mut work_items = runtime.latest_work_items().await.map_err(error_response)?;
    sort_state_work_items(&mut work_items);
    let waiting_intents = runtime
        .latest_waiting_intents()
        .await
        .map_err(error_response)?;
    let external_triggers = runtime
        .latest_external_triggers()
        .await
        .map_err(error_response)?
        .into_iter()
        .map(ExternalTriggerStateSnapshot::from)
        .collect();
    let operator_notifications = runtime
        .recent_operator_notifications(50)
        .await
        .map_err(error_response)?;
    let workspace = state_workspace_snapshot(&agent);
    let execution = runtime.execution_snapshot().await.map_err(error_response)?;
    let session = StateSessionSnapshot {
        current_run_id: agent.agent.current_run_id.clone(),
        pending_count: agent.agent.pending,
        last_turn: agent.agent.last_turn_terminal.clone(),
    };
    traced_json(
        "/agents/{agent_id}/state",
        started_at,
        AgentStateSnapshot {
            agent,
            session,
            tasks,
            timers,
            work_items,
            waiting_intents,
            external_triggers,
            operator_notifications,
            execution: Some(execution),
            workspace,
        },
    )
}

fn sort_state_work_items(work_items: &mut [WorkItemRecord]) {
    work_items.sort_by(|left, right| {
        state_work_item_rank(left)
            .cmp(&state_work_item_rank(right))
            .then_with(|| {
                if left.state == WorkItemState::Open && right.state == WorkItemState::Open {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.updated_at.cmp(&right.updated_at))
                } else {
                    right
                        .updated_at
                        .cmp(&left.updated_at)
                        .then_with(|| right.created_at.cmp(&left.created_at))
                }
            })
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn slim_state_task_record(mut task: TaskRecord) -> TaskRecord {
    if let Some(detail) = task.detail.take() {
        task.detail = Some(slim_state_json_value(
            detail,
            STATE_BOOTSTRAP_TASK_DETAIL_STRING_LIMIT,
        ));
    }
    task
}

#[cfg(test)]
fn slim_state_transcript_entry(
    mut entry: crate::types::TranscriptEntry,
) -> crate::types::TranscriptEntry {
    entry.data = slim_state_json_value(entry.data, STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT);
    entry
}

fn slim_state_json_value(value: Value, string_limit: usize) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_state_bootstrap_string(&text, string_limit)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(STATE_BOOTSTRAP_JSON_ARRAY_LIMIT)
                .map(|item| slim_state_json_value(item, string_limit))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, slim_state_json_value(value, string_limit)))
                .collect(),
        ),
        other => other,
    }
}

fn truncate_state_bootstrap_string(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }

    let truncated_char_limit = limit.saturating_sub(3);
    let mut truncate_at = None;
    for (index, (byte_index, _)) in text.char_indices().enumerate() {
        if limit <= 3 {
            if index == limit {
                return text[..byte_index].to_string();
            }
        } else {
            if index == truncated_char_limit {
                truncate_at = Some(byte_index);
            }
            if index == limit {
                let byte_index = truncate_at.unwrap_or(byte_index);
                return format!("{}...", &text[..byte_index]);
            }
        }
    }
    text.to_string()
}

fn state_work_item_rank(item: &WorkItemRecord) -> u8 {
    match item.state {
        WorkItemState::Open if item.blocked_by.is_none() => 0,
        WorkItemState::Open => 1,
        WorkItemState::Completed => 2,
    }
}

fn state_workspace_snapshot(agent: &AgentSummary) -> StateWorkspaceSnapshot {
    StateWorkspaceSnapshot {
        attached_workspaces: agent.agent.attached_workspaces.clone(),
        active_workspace_entry: agent.agent.active_workspace_entry.clone(),
        active_workspace_occupancy: agent.active_workspace_occupancy.clone(),
        worktree_session: agent.agent.worktree_session.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        sort_state_work_items, STATE_BOOTSTRAP_JSON_ARRAY_LIMIT,
        STATE_BOOTSTRAP_TASK_DETAIL_STRING_LIMIT, STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT,
    };
    use crate::types::{
        TaskKind, TaskRecord, TaskStatus, TranscriptEntry, TranscriptEntryKind, WorkItemRecord,
        WorkItemState,
    };
    use chrono::{Duration, Utc};
    use serde_json::json;

    #[test]
    fn state_sort_preserves_queue_display_order() {
        let mut active = WorkItemRecord::new("default", "active", WorkItemState::Open);
        active.updated_at = Utc::now() + Duration::minutes(5);

        let mut queued_early = WorkItemRecord::new("default", "queued first", WorkItemState::Open);
        queued_early.created_at = Utc::now();
        queued_early.updated_at = queued_early.created_at;

        let mut queued_late = WorkItemRecord::new("default", "queued second", WorkItemState::Open);
        queued_late.created_at = queued_early.created_at + Duration::minutes(1);
        queued_late.updated_at = queued_late.created_at;

        let mut waiting = WorkItemRecord::new("default", "waiting", WorkItemState::Open);
        waiting.created_at = queued_late.created_at + Duration::minutes(1);
        waiting.updated_at = waiting.created_at;

        let completed = WorkItemRecord::new("default", "completed", WorkItemState::Completed);
        let mut work_items = vec![
            waiting.clone(),
            completed,
            queued_late.clone(),
            active.clone(),
            queued_early.clone(),
        ];

        sort_state_work_items(&mut work_items);

        let ordered = work_items
            .iter()
            .map(|item| item.objective.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                active.objective.as_str(),
                queued_early.objective.as_str(),
                queued_late.objective.as_str(),
                waiting.objective.as_str(),
                "completed",
            ]
        );
    }

    #[test]
    fn state_bootstrap_slims_large_task_detail_and_transcript_data() {
        let now = chrono::Utc::now();
        let task = TaskRecord {
            id: "task-1".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("large task".into()),
            detail: Some(json!({
                "cmd": "printf test",
                "output_path": "/tmp/output.log",
                "output_summary": "x".repeat(STATE_BOOTSTRAP_TASK_DETAIL_STRING_LIMIT + 64),
                "lines": (0..(STATE_BOOTSTRAP_JSON_ARRAY_LIMIT + 10)).collect::<Vec<_>>()
            })),
            recovery: None,
        };
        let slimmed = super::slim_state_task_record(task);
        let detail = slimmed.detail.expect("detail");
        assert_eq!(detail["cmd"], "printf test");
        assert_eq!(detail["output_path"], "/tmp/output.log");
        assert!(
            detail["output_summary"]
                .as_str()
                .expect("summary")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TASK_DETAIL_STRING_LIMIT
        );
        assert_eq!(
            detail["lines"].as_array().expect("lines").len(),
            STATE_BOOTSTRAP_JSON_ARRAY_LIMIT
        );

        let entry = TranscriptEntry {
            id: "entry-1".into(),
            transcript_seq: None,
            agent_id: "default".into(),
            created_at: now,
            kind: TranscriptEntryKind::ToolResults,
            round: Some(1),
            related_message_id: None,
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            data: json!({"content": "y".repeat(STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT + 64)}),
        };
        let slimmed_entry = super::slim_state_transcript_entry(entry);
        assert!(
            slimmed_entry.data["content"]
                .as_str()
                .expect("content")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT
        );
    }

    #[test]
    fn state_bootstrap_string_truncation_preserves_total_budget() {
        assert_eq!(super::truncate_state_bootstrap_string("abcdef", 0), "");
        assert_eq!(super::truncate_state_bootstrap_string("abcdef", 2), "ab");
        assert_eq!(
            super::truncate_state_bootstrap_string("abcdef", 6),
            "abcdef"
        );
        assert_eq!(
            super::truncate_state_bootstrap_string("abcdefg", 6),
            "abc..."
        );
        assert_eq!(
            super::truncate_state_bootstrap_string("你好世界", 3),
            "你好世"
        );
        assert_eq!(
            super::truncate_state_bootstrap_string("你好世界a", 4),
            "你..."
        );
    }
}

pub async fn briefs_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    briefs(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
        Query(query),
    )
    .await
}

pub async fn briefs(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let briefs = runtime
        .recent_briefs(query.limit.unwrap_or(20))
        .await
        .map_err(error_response)?;
    Ok(Json(briefs))
}

pub async fn transcript_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    transcript(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
        Query(query),
    )
    .await
}

pub async fn worktree_summary_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    worktree_summary(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
    )
    .await
}

pub async fn events(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_EVENT_STREAM_WINDOW)
        .clamp(1, MAX_EVENT_STREAM_WINDOW);
    let order = query.order.unwrap_or(EventPageOrder::Desc);
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    }
    let cursor_seq = runtime
        .storage()
        .latest_event_seq()
        .map_err(error_response)?;
    let max_level = query.max_level;
    let filter_context = match max_level {
        Some(_) => Some(
            event_filter_context(&runtime)
                .await
                .map_err(error_response)?,
        ),
        None => None,
    };
    let page = runtime
        .storage()
        .read_event_page_matching(
            query.before_seq,
            query.after_seq,
            limit,
            order.into(),
            |event| match (max_level, filter_context.as_ref()) {
                (Some(level), Some(filter_context)) => is_operator_event_in_display_mode(
                    &event.kind,
                    &event.data,
                    &event_fallback_summary(event),
                    filter_context,
                    level,
                ),
                _ => true,
            },
        )
        .map_err(error_response)?;
    let oldest_seq = oldest_seq(&page.events, order);
    let newest_seq = newest_seq(&page.events, order);
    let events = page
        .events
        .iter()
        .map(|event| stream_event_envelope(&agent_id, event))
        .collect();
    Ok(Json(EventsPageResponse {
        events,
        oldest_seq,
        newest_seq,
        cursor_seq,
        has_older: page.has_older,
        has_newer: page.has_newer,
        order,
        limit,
    }))
}

pub async fn events_stream(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EventStreamQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let event_window_limit = query
        .limit
        .unwrap_or(DEFAULT_EVENT_STREAM_WINDOW)
        .clamp(1, MAX_EVENT_STREAM_WINDOW);
    let after_seq = query.after_seq;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if state.require_control_token {
        authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    }
    let initial_event_marker = runtime
        .storage()
        .poll_activity_marker()
        .map_err(error_response)?
        .events;
    let events = runtime
        .storage()
        .read_recent_events(event_window_limit.saturating_add(1))
        .map_err(error_response)?;
    let buffered = initial_buffered_events(&events, after_seq)?;
    let last_seen_seq =
        after_seq.unwrap_or_else(|| events.last().map_or(0, |event| event.event_seq));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
    tokio::spawn(async move {
        let mut state = EventStreamState {
            runtime,
            runtime_id: agent_id,
            event_window_limit,
            event_marker: initial_event_marker,
            last_seen_seq,
            buffered,
        };
        loop {
            if let Some(event) = state.buffered.pop_front() {
                let envelope = stream_event_envelope(&state.runtime_id, &event);
                state.last_seen_seq = event.event_seq;
                let payload = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
                if tx
                    .send(Ok(Event::default()
                        .id(envelope.event_seq.to_string())
                        .event(envelope.event_type)
                        .data(payload)))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            let event_marker = match state.runtime.storage().poll_activity_marker() {
                Ok(marker) => marker.events,
                Err(err) => {
                    error!("failed to poll event marker for stream: {err}");
                    sleep(EVENT_STREAM_POLL_INTERVAL).await;
                    continue;
                }
            };
            if event_marker == state.event_marker {
                sleep(EVENT_STREAM_POLL_INTERVAL).await;
                continue;
            }
            let latest_events: Vec<AuditEvent> = match state
                .runtime
                .storage()
                .read_recent_events(state.event_window_limit.saturating_add(1))
            {
                Ok(latest_events) => latest_events,
                Err(err) => {
                    error!("failed to load events for stream: {err}");
                    sleep(EVENT_STREAM_POLL_INTERVAL).await;
                    continue;
                }
            };
            match refresh_buffered_events(&mut state, latest_events) {
                Ok(()) => {
                    state.event_marker = event_marker;
                    if !state.buffered.is_empty() {
                        continue;
                    }
                }
                Err(seq) => {
                    error!("event stream cursor fell out of replay window: {seq}");
                    break;
                }
            }
            sleep(EVENT_STREAM_POLL_INTERVAL).await;
        }
    });
    let stream = ReceiverStream::new(rx);
    let keep_alive = KeepAlive::new()
        .interval(EVENT_STREAM_HEARTBEAT_INTERVAL)
        .text("heartbeat");
    Ok(Sse::new(stream).keep_alive(keep_alive))
}

fn initial_buffered_events(
    events: &[AuditEvent],
    after_seq: Option<u64>,
) -> std::result::Result<VecDeque<AuditEvent>, (StatusCode, Json<Value>)> {
    let start_index = if let Some(after_seq) = after_seq {
        if after_seq == 0 {
            0
        } else {
            match events.iter().position(|event| event.event_seq == after_seq) {
                Some(position) => position + 1,
                None => return Err(event_seq_not_found(after_seq)),
            }
        }
    } else {
        events.len()
    };
    Ok(events.iter().skip(start_index).cloned().collect())
}

fn refresh_buffered_events(
    state: &mut EventStreamState,
    latest_events: Vec<AuditEvent>,
) -> std::result::Result<(), u64> {
    let start_index = if state.last_seen_seq == 0 {
        0
    } else {
        latest_events
            .iter()
            .position(|event| event.event_seq == state.last_seen_seq)
            .map(|position| position + 1)
            .ok_or(state.last_seen_seq)?
    };
    state
        .buffered
        .extend(latest_events.into_iter().skip(start_index));
    Ok(())
}

fn oldest_seq(events: &[AuditEvent], order: EventPageOrder) -> Option<u64> {
    match order {
        EventPageOrder::Asc => events.first(),
        EventPageOrder::Desc => events.last(),
    }
    .map(|event| event.event_seq)
}

fn newest_seq(events: &[AuditEvent], order: EventPageOrder) -> Option<u64> {
    match order {
        EventPageOrder::Asc => events.last(),
        EventPageOrder::Desc => events.first(),
    }
    .map(|event| event.event_seq)
}

fn stream_event_envelope(agent_id: &str, event: &AuditEvent) -> StreamEventEnvelope {
    StreamEventEnvelope {
        id: event.id.clone(),
        event_seq: event.event_seq,
        ts: event.created_at,
        agent_id: agent_id.to_string(),
        event_type: event.kind.clone(),
        provenance: event_replay_provenance(&event.data),
        payload: event.data.clone(),
    }
}

async fn event_filter_context(
    runtime: &crate::runtime::RuntimeHandle,
) -> Result<OperatorPresentationContext> {
    let agent = runtime.agent_summary().await?;
    let completed_work_item_ids = runtime
        .latest_work_items()
        .await?
        .into_iter()
        .filter(|item| item.state == WorkItemState::Completed)
        .map(|item| item.id)
        .collect();
    Ok(OperatorPresentationContext {
        awaiting_operator_input: agent.closure.waiting_reason
            == Some(WaitingReason::AwaitingOperatorInput),
        completed_work_item_ids,
    })
}

fn event_fallback_summary(event: &AuditEvent) -> String {
    event
        .data
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or(event.kind.as_str())
        .to_string()
}

fn event_replay_provenance(payload: &Value) -> EventReplayProvenance {
    EventReplayProvenance {
        origin: clone_payload_field(payload, "origin"),
        authority_class: clone_payload_field(payload, "authority_class"),
        delivery_surface: clone_payload_field(payload, "delivery_surface"),
        admission_context: clone_payload_field(payload, "admission_context"),
        transport: clone_payload_field(payload, "transport"),
        source: clone_payload_field(payload, "source"),
        reply_route: clone_payload_field(payload, "reply_route"),
        message_id: clone_payload_field(payload, "message_id"),
        task_id: clone_payload_field(payload, "task_id"),
        work_item_id: clone_payload_field(payload, "work_item_id"),
        correlation_id: clone_payload_field(payload, "correlation_id"),
        causation_id: clone_payload_field(payload, "causation_id"),
    }
}

fn clone_payload_field(payload: &Value, field: &str) -> Option<Value> {
    payload.get(field).filter(|value| !value.is_null()).cloned()
}

fn event_seq_not_found(after_seq: u64) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::NOT_FOUND,
        HttpErrorEnvelope::new(format!(
            "after_seq {after_seq} was not found in the replay window"
        ))
        .code("cursor_not_found")
        .extension("after_seq", after_seq)
        .extension("event_seq", after_seq),
    )
}

async fn not_found_handler() -> (StatusCode, Json<Value>) {
    not_found("Not Found")
}

pub async fn transcript(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let transcript = runtime
        .recent_transcript(query.limit.unwrap_or(50))
        .await
        .map_err(error_response)?;
    Ok(Json(transcript))
}

pub async fn worktree_summary(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let summary = runtime
        .summarize_worktree_tasks()
        .await
        .map_err(error_response)?;
    Ok(Json(json!({
        "agent_id": agent_id,
        "summary": summary,
    })))
}

pub async fn tasks(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    Ok(Json(
        runtime
            .active_tasks(query.limit.unwrap_or(50))
            .await
            .map_err(error_response)?,
    ))
}

pub async fn task_status(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(not_found(format!("task {task_id} not found")));
    }
    let snapshot = runtime
        .managed_tasks()
        .task_status_snapshot(&task_id)
        .await
        .map_err(task_lifecycle_error)?;
    Ok(Json(snapshot))
}

pub async fn task_output(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TaskOutputQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(not_found(format!("task {task_id} not found")));
    }
    let output = runtime
        .managed_tasks()
        .task_output(
            &task_id,
            query.block.unwrap_or(false),
            query.timeout_ms.unwrap_or(TASK_OUTPUT_DEFAULT_TIMEOUT_MS),
        )
        .await
        .map_err(task_lifecycle_error)?;
    Ok(Json(output))
}

pub async fn task_input(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<TaskInputRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(not_found(format!("task {task_id} not found")));
    }
    let authority_class = request
        .authority_class
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let result = runtime
        .managed_tasks()
        .task_input_with_trust(&task_id, &request.text, &authority_class)
        .await
        .map_err(task_lifecycle_error)?;
    Ok(Json(result))
}

pub async fn task_stop(
    Path((agent_id, task_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<TaskStopRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    if !runtime
        .task_record(&task_id)
        .await
        .map_err(error_response)?
        .is_some_and(|task| task.agent_id == agent_id)
    {
        return Err(not_found(format!("task {task_id} not found")));
    }
    let authority_class = request
        .authority_class
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let task = runtime
        .managed_tasks()
        .stop_task(&task_id, &authority_class)
        .await
        .map_err(task_lifecycle_error)?;
    let force_stop_requested = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("force_stop_requested"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let snapshot = TaskStatusSnapshot::from_task_record(&task);
    let result = TaskStopResult {
        summary_text: Some(match task.status {
            TaskStatus::Cancelling => format!("stop requested for task {}", task.id),
            TaskStatus::Cancelled => format!("cancelled task {}", task.id),
            _ => format!("updated task {}", task.id),
        }),
        task: snapshot,
        stop_requested: true,
        force_stop_requested,
    };
    Ok(Json(result))
}

pub async fn create_command_task(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateCommandTaskRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let effective_trust = provided_trust
        .clone()
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let task = runtime
        .schedule_command_task(
            request.summary,
            crate::types::CommandTaskSpec {
                cmd: request.cmd,
                workdir: request.workdir,
                shell: request.shell,
                login: request.login.unwrap_or(true),
                tty: request.tty.unwrap_or(false),
                yield_time_ms: request.yield_time_ms.unwrap_or(10_000),
                max_output_tokens: request.max_output_tokens,
                accepts_input: request.accepts_input.unwrap_or(false),
                terminal_reentry: false,
            },
            effective_trust.clone(),
        )
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "task_create_requested",
            json!({
                "task_id": task.id,
                "kind": task.kind,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "effective_trust": effective_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(task))
}

pub async fn create_work_item(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateWorkItemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let objective = request.objective.trim().to_string();
    if objective.is_empty() {
        return Err(bad_request("objective must not be empty"));
    }
    let (runtime, record) = state
        .host
        .enqueue_public_work_item(&agent_id, objective)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "work_item_enqueue_requested",
            json!({
                "work_item_id": record.id,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(record))
}

pub async fn pick_work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<PickWorkItemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let reason = normalize_optional_non_empty(request.reason);
    if request.clear_blocker && reason.is_none() {
        return Err(bad_request(
            "clear_blocker requires a non-empty reason explaining why the blocker is resolved",
        ));
    }
    let picked = runtime
        .pick_work_item_with_reason_and_clear_blocker(work_item_id, reason, request.clear_blocker)
        .await
        .map_err(work_item_lifecycle_error)?;
    runtime
        .append_audit_event(
            "work_item_pick_requested",
            json!({
                "work_item_id": picked.current_work_item.id.clone(),
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    let current_work_item_id = picked.current_work_item.id.clone();
    Ok(Json(PickWorkItemResponse {
        previous_work_item: picked.previous_work_item,
        current_work_item: picked.current_work_item,
        current_work_item_id,
        transition: picked.transition,
    }))
}

pub async fn update_work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<UpdateWorkItemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let objective = request
        .objective
        .map(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                Err(bad_request("objective must not be empty"))
            } else {
                Ok(trimmed)
            }
        })
        .transpose()?;
    let blocked_by = request
        .blocked_by
        .map(parse_blocked_by_mutation)
        .transpose()?;
    if request.recheck_after == Some(0) {
        return Err(bad_request("recheck_after must be greater than 0"));
    }
    if request.recheck_after.is_some() && blocked_by.as_ref().is_none_or(Option::is_none) {
        return Err(bad_request(
            "recheck_after requires a non-empty blocked_by value",
        ));
    }
    if objective.is_none()
        && request.plan_status.is_none()
        && request.todo_list.is_none()
        && blocked_by.is_none()
    {
        return Err(bad_request(
            "request must include at least one mutation field",
        ));
    }
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let record = runtime
        .update_work_item_fields_with_recheck(
            work_item_id,
            objective,
            request.plan_status,
            None,
            request.todo_list,
            blocked_by,
            request.recheck_after,
        )
        .await
        .map_err(work_item_lifecycle_error)?;
    runtime
        .append_audit_event(
            "work_item_update_requested",
            json!({
                "work_item_id": record.id.clone(),
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(record))
}

pub async fn complete_work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CompleteWorkItemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let record = runtime
        .complete_work_item(work_item_id, Vec::new())
        .await
        .map_err(work_item_lifecycle_error)?;
    runtime
        .append_audit_event(
            "work_item_complete_requested",
            json!({
                "work_item_id": record.id.clone(),
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(record))
}

pub async fn work_items(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let mut work_items = runtime
        .latest_work_items_for_agent(&agent_id, query.limit.unwrap_or(50))
        .await
        .map_err(error_response)?;
    sort_state_work_items(&mut work_items);
    Ok(Json(work_items))
}

pub async fn work_item(
    Path((agent_id, work_item_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(work_item) = runtime
        .latest_work_item(&work_item_id)
        .await
        .map_err(error_response)?
        .filter(|item| item.agent_id == agent_id)
    else {
        return Err(not_found(format!("work item {work_item_id} not found")));
    };
    Ok(Json(work_item))
}

pub async fn timers(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    Ok(Json(
        runtime
            .recent_timers(query.limit.unwrap_or(50))
            .await
            .map_err(error_response)?,
    ))
}

pub async fn timer(
    Path((agent_id, timer_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(timer) = runtime
        .latest_timer(&timer_id)
        .await
        .map_err(error_response)?
        .filter(|timer| timer.agent_id == agent_id)
    else {
        return Err(not_found(format!("timer {timer_id} not found")));
    };
    Ok(Json(timer))
}

pub async fn create_timer(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateTimerRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let timer = runtime
        .schedule_timer(request.duration_ms, request.interval_ms, request.summary)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "timer_create_requested",
            json!({
                "timer_id": timer.id,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(timer))
}

pub async fn cancel_timer(
    Path((agent_id, timer_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CancelTimerRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let timer = runtime
        .cancel_timer(&timer_id)
        .await
        .map_err(timer_lifecycle_error)?;
    runtime
        .append_audit_event(
            "timer_cancel_requested",
            json!({
                "timer_id": timer.id,
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(timer))
}

fn timer_lifecycle_error(err: anyhow::Error) -> (StatusCode, Json<Value>) {
    let message = err.to_string();
    if message.starts_with("timer ") && message.ends_with(" not found") {
        not_found(message)
    } else if message.starts_with("cannot ") {
        bad_request(message)
    } else {
        error_response(err)
    }
}

pub async fn list_skills(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    let skills = crate::skills::list_installed_skills(&agent_home).map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skills": skills,
    })))
}

pub async fn install_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::InstallSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let skill_name =
        crate::skills::install_skill_with_user_home(&agent_home, Some(&user_home), &request.kind)
            .map_err(skill_install_error_response)?;
    runtime
        .append_audit_event(
            "skill_installed",
            json!({
                "target_agent_id": agent_id,
                "skill_name": skill_name,
                "kind": request.kind,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": skill_name,
    })))
}

pub async fn uninstall_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::UninstallSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    crate::skills::uninstall_skill(&agent_home, &request.name).map_err(error_response)?;
    runtime
        .append_audit_event(
            "skill_uninstalled",
            json!({
                "target_agent_id": agent_id,
                "skill_name": request.name,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": request.name,
    })))
}

pub async fn create_agent(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let agent = state
        .host
        .create_named_agent(&agent_id, request.template.as_deref())
        .await
        .map_err(error_response)?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "agent_created",
            json!({
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(agent))
}

pub async fn control(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ControlRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let action = request.action.clone();
    let runtime = state
        .host
        .control_public_agent(&agent_id, action.clone())
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "control_request_admitted",
            json!({
                "target_agent_id": agent_id,
                "action": action,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn abort_current_run(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AbortCurrentRunRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let mode = match request.mode.as_deref().unwrap_or("stop_after_abort") {
        "stop_after_abort" => CurrentRunAbortMode::StopAfterAbort,
        "pause_after_abort" => CurrentRunAbortMode::StopAfterAbort,
        other => {
            return Err(bad_request(format!(
                "unsupported abort mode {other}; expected stop_after_abort or deprecated alias pause_after_abort"
            )))
        }
    };
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class.clone();
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let outcome = runtime
        .abort_current_run(CurrentRunAbortRequest {
            run_id: request.run_id.clone(),
            mode,
        })
        .await
        .map_err(abort_error_response)?;
    Ok(Json(json!({
        "ok": true,
        "aborted": true,
        "agent_id": outcome.agent_id,
        "run_id": outcome.run_id,
        "mode": outcome.mode.as_str(),
        "admission_context": admission_context,
        "provided_trust": provided_trust,
    })))
}

pub async fn attach_workspace(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AttachWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let workspace = state
        .host
        .ensure_workspace_entry(std::path::PathBuf::from(&request.path))
        .map_err(error_response)?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .attach_workspace(&workspace)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "workspace_attach_requested",
            json!({
                "target_agent_id": agent_id,
                "workspace_id": workspace.workspace_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "workspace_id": workspace.workspace_id,
        "workspace_anchor": workspace.workspace_anchor,
    })))
}

pub async fn exit_workspace(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ExitWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime.exit_workspace().await.map_err(error_response)?;
    runtime
        .append_audit_event(
            "workspace_exit_requested",
            json!({
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
    })))
}

pub async fn detach_workspace(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<DetachWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let workspace_id = request.workspace_id.trim().to_string();
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    runtime
        .detach_workspace(&workspace_id)
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "workspace_detach_requested",
            json!({
                "target_agent_id": agent_id,
                "workspace_id": workspace_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "workspace_id": workspace_id,
    })))
}

pub async fn set_agent_model(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SetAgentModelRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    if let Some(reasoning_effort) = request.reasoning_effort.as_deref() {
        validate_reasoning_effort(reasoning_effort)?;
    }
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let model = ModelRef::parse(&request.model).map_err(error_response)?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let model_state = runtime
        .set_model_override(model.clone(), request.reasoning_effort.clone())
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "agent_model_override_requested",
            json!({
                "target_agent_id": agent_id,
                "override_model": model,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
                "model": model_state,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "model": model_state,
    })))
}

fn validate_reasoning_effort(value: &str) -> Result<(), (StatusCode, Json<Value>)> {
    match value {
        "low" | "medium" | "high" | "xhigh" => Ok(()),
        _ => Err(bad_request(format!(
            "invalid reasoning_effort '{value}'; must be one of low, medium, high, xhigh"
        ))),
    }
}

pub async fn clear_agent_model(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ClearAgentModelRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.authority_class;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let model_state = runtime
        .clear_model_override()
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "agent_model_override_clear_requested",
            json!({
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "provided_trust": provided_trust,
                "boundary": boundary,
                "model": model_state,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "model": model_state,
    })))
}

pub async fn control_prompt(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ControlPromptRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    enqueue_internal(
        state,
        agent_id,
        EnqueueRequest {
            kind: Some(MessageKind::OperatorPrompt),
            priority: Some(Priority::Interject),
            authority_class: Some(AuthorityClass::OperatorInstruction),
            body: Some(MessageBody::Text { text: request.text }),
            text: None,
            json: None,
            metadata: Some(json!({ "control": true })),
            correlation_id: None,
            causation_id: None,
            origin: Some(IncomingOrigin::Operator {
                actor_id: Some("control".into()),
            }),
        },
        EnqueueIngress::Trusted {
            delivery_surface: MessageDeliverySurface::HttpControlPrompt,
            admission_context,
        },
    )
    .await
}

pub async fn create_operator_transport_binding(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<OperatorTransportBindingRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let target_agent_id = request.target_agent_id.unwrap_or_else(|| agent_id.clone());
    if target_agent_id != agent_id {
        return Err(bad_request(
            "operator transport binding target_agent_id must match route agent_id",
        ));
    }
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let delivery_auth = validate_operator_transport_delivery_auth(request.delivery_auth)?;
    let binding = OperatorTransportBinding {
        binding_id: non_empty_or_generated(request.binding_id, "opbind"),
        transport: require_non_empty(request.transport, "transport")?,
        operator_actor_id: require_non_empty(request.operator_actor_id, "operator_actor_id")?,
        target_agent_id,
        default_route_id: require_non_empty(request.default_route_id, "default_route_id")?,
        delivery_callback_url: require_non_empty(
            request.delivery_callback_url,
            "delivery_callback_url",
        )?,
        delivery_auth,
        capabilities: request.capabilities,
        provider: request.provider.and_then(non_empty_opt),
        provider_identity_ref: request.provider_identity_ref.and_then(non_empty_opt),
        status: OperatorTransportBindingStatus::Active,
        created_at: Utc::now(),
        last_seen_at: None,
        metadata: request.metadata,
    };
    let binding = runtime
        .upsert_operator_transport_binding(binding)
        .await
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "binding": binding,
    })))
}

pub async fn operator_ingress(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<OperatorIngressRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let text = require_non_empty(request.text, "text")?;
    let actor_id = require_non_empty(request.actor_id, "actor_id")?;
    let binding_id = require_non_empty(request.binding_id, "binding_id")?;
    let runtime = state
        .host
        .get_public_agent_for_external_ingress(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let Some(mut binding) = runtime
        .active_operator_transport_binding(&binding_id)
        .await
        .map_err(error_response)?
    else {
        return Err(forbidden("operator transport binding is not active"));
    };
    if binding.target_agent_id != agent_id {
        return Err(forbidden(
            "operator transport binding does not target this agent",
        ));
    }
    if binding.operator_actor_id != actor_id {
        return Err(forbidden("operator transport actor does not match binding"));
    }
    let expected_provider = binding
        .provider
        .as_deref()
        .unwrap_or(&binding.transport)
        .to_string();
    if let Some(provider) = request.provider.as_deref().and_then(non_empty_str) {
        if provider != expected_provider {
            return Err(forbidden(
                "operator transport provider does not match binding",
            ));
        }
    }

    binding.last_seen_at = Some(Utc::now());
    runtime
        .upsert_operator_transport_binding(binding.clone())
        .await
        .map_err(error_response)?;

    let reply_route_id = request.reply_route_id.and_then(non_empty_opt);
    let metadata = json!({
        "operator_transport": {
            "binding_id": binding.binding_id,
            "transport": binding.transport,
            "reply_route_id": reply_route_id,
            "provider": request.provider.and_then(non_empty_opt).unwrap_or(expected_provider),
            "provider_identity_ref": binding.provider_identity_ref,
            "upstream_provider": request.upstream_provider,
            "provider_message_ref": request.provider_message_ref,
            "metadata": request.metadata,
        }
    });
    let message = InboundRequest {
        agent_id: agent_id.clone(),
        kind: MessageKind::OperatorPrompt,
        priority: Priority::Interject,
        origin: MessageOrigin::Operator {
            actor_id: Some(actor_id),
        },
        authority_class: AuthorityClass::OperatorInstruction,
        body: MessageBody::Text { text },
        delivery_surface: MessageDeliverySurface::RemoteOperatorTransport,
        admission_context: AdmissionContext::OperatorTransportAuthenticated,
        metadata: Some(metadata),
        correlation_id: request.correlation_id,
        causation_id: request.causation_id,
    }
    .into_message();
    let queued = runtime.enqueue(message).await.map_err(error_response)?;
    Ok(Json(EnqueueResponse {
        ok: true,
        agent_id,
        message_id: queued.id,
    }))
}

pub async fn control_debug_prompt(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<DebugPromptRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let effective_trust = request
        .authority_class
        .clone()
        .unwrap_or(AuthorityClass::OperatorInstruction);
    let boundary = state
        .host
        .public_agent_boundary_metadata(&agent_id)
        .map_err(agent_access_error)?;
    let dump = state
        .host
        .preview_public_agent_prompt(&agent_id, request.text.clone(), effective_trust.clone())
        .map_err(agent_access_error)?
        .render_dump();
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "admission_context": admission_context,
        "effective_trust": effective_trust,
        "boundary": boundary,
        "dump": dump,
    })))
}

pub async fn control_wake(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ControlWakeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    if request.reason.trim().is_empty() {
        return Err(forbidden("wake reason may not be empty"));
    }
    let admission_context = control_admission_context(&state);
    let runtime = state
        .host
        .get_public_agent_for_external_ingress(&agent_id)
        .await
        .map_err(|error| match error {
            PublicAgentError::Stopped { agent_id } => stopped_agent_conflict(
                format!(
                    "agent {} is stopped; wake does not override stopped; start first",
                    agent_id
                ),
                agent_id,
            ),
            other => agent_access_error(other),
        })?;
    let reason = request.reason.clone();
    let disposition = runtime
        .submit_wake_hint(WakeHint {
            agent_id: agent_id.clone(),
            reason: reason.clone(),
            description: None,
            source: request.source,
            scope: None,
            waiting_intent_id: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: request.correlation_id,
            causation_id: request.causation_id,
        })
        .await
        .map_err(error_response)?;
    runtime
        .append_audit_event(
            "wake_requested",
            json!({
                "target_agent_id": agent_id,
                "reason": reason,
                "admission_context": admission_context,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(WakeResponse {
        ok: true,
        agent_id,
        disposition,
    }))
}

async fn callback_ingress(
    mode: CallbackIngressMode,
    callback_token: String,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let payload = build_callback_delivery_payload(&headers, body)
        .map_err(|err| bad_request(err.to_string()))?;
    let (agent_id, descriptor) = state
        .host
        .resolve_external_trigger_record(&callback_token)
        .await
        .map_err(error_response)?
        .ok_or_else(|| forbidden("invalid callback token"))?;
    let identity = state
        .host
        .agent_identity_record(&agent_id)
        .map_err(error_response)?
        .ok_or_else(|| forbidden("invalid callback token"))?;
    if identity.status != AgentRegistryStatus::Active {
        return Err(forbidden("invalid callback token"));
    }
    let runtime = if identity.visibility == AgentVisibility::Public {
        state
            .host
            .get_public_agent_for_external_ingress(&agent_id)
            .await
            .map_err(agent_access_error)?
    } else {
        state
            .host
            .get_or_create_agent(&agent_id)
            .await
            .map_err(error_response)?
    };
    if descriptor.delivery_mode != mode.delivery_mode() {
        return Err(forbidden("callback delivery mode mismatch"));
    }
    let result = runtime
        .deliver_callback(&descriptor.external_trigger_id, payload)
        .await
        .map_err(|err| {
            error!(
                external_trigger_id = %descriptor.external_trigger_id,
                error = %err,
                "callback ingress rejected delivery"
            );
            forbidden("forbidden")
        })?;
    Ok(Json(CallbackResponse { ok: true, result }))
}

pub async fn callback_ingress_enqueue(
    Path(callback_token): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    callback_ingress(
        CallbackIngressMode::Enqueue,
        callback_token,
        headers,
        State(state),
        body,
    )
    .await
}

pub async fn callback_ingress_wake(
    Path(callback_token): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    callback_ingress(
        CallbackIngressMode::Wake,
        callback_token,
        headers,
        State(state),
        body,
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallbackIngressMode {
    Enqueue,
    Wake,
}

impl CallbackIngressMode {
    fn delivery_mode(self) -> crate::types::CallbackDeliveryMode {
        match self {
            Self::Enqueue => crate::types::CallbackDeliveryMode::EnqueueMessage,
            Self::Wake => crate::types::CallbackDeliveryMode::WakeHint,
        }
    }
}

fn build_callback_delivery_payload(
    headers: &HeaderMap,
    body: Bytes,
) -> Result<CallbackDeliveryPayload> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let body = if body.is_empty() {
        None
    } else {
        Some(parse_callback_body(content_type.as_deref(), &body)?)
    };
    Ok(CallbackDeliveryPayload {
        body,
        content_type,
        correlation_id: None,
        causation_id: None,
    })
}

fn parse_callback_body(content_type: Option<&str>, body: &[u8]) -> Result<MessageBody> {
    match content_type {
        Some(content_type) if is_json_content_type(content_type) => {
            let value = serde_json::from_slice(body)
                .map_err(|err| anyhow!("invalid JSON callback body: {err}"))?;
            Ok(MessageBody::Json { value })
        }
        Some(content_type) if is_text_content_type(content_type) => {
            let text = std::str::from_utf8(body)
                .map_err(|err| anyhow!("invalid UTF-8 callback body: {err}"))?;
            Ok(MessageBody::Text {
                text: text.to_string(),
            })
        }
        Some(content_type) => Ok(MessageBody::Json {
            value: json!({
                "content_type": content_type,
                "body_base64": BASE64_STANDARD.encode(body),
            }),
        }),
        None => match std::str::from_utf8(body) {
            Ok(text) => Ok(MessageBody::Text {
                text: text.to_string(),
            }),
            Err(_) => Ok(MessageBody::Json {
                value: json!({
                    "body_base64": BASE64_STANDARD.encode(body),
                }),
            }),
        },
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    let normalized = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    normalized == "application/json" || normalized.ends_with("+json")
}

fn is_text_content_type(content_type: &str) -> bool {
    let normalized = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    normalized.starts_with("text/")
}

pub async fn generic_webhook(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    enqueue_internal(
        state,
        agent_id,
        EnqueueRequest {
            kind: Some(MessageKind::WebhookEvent),
            priority: Some(Priority::Normal),
            authority_class: Some(AuthorityClass::IntegrationSignal),
            body: Some(MessageBody::Json { value: payload }),
            text: None,
            json: None,
            metadata: None,
            correlation_id: None,
            causation_id: None,
            origin: Some(IncomingOrigin::Webhook {
                source: "generic_webhook".into(),
                event_type: None,
            }),
        },
        EnqueueIngress::Trusted {
            delivery_surface: MessageDeliverySurface::HttpWebhook,
            admission_context: public_admission_context(),
        },
    )
    .await
}

fn authorize_control(headers: &HeaderMap, state: &AppState) -> Result<()> {
    if !state.require_control_token {
        return Ok(());
    }
    let expected_token = state
        .host
        .config()
        .control_token
        .as_deref()
        .ok_or_else(|| anyhow!("control token required but not configured"))?;
    let provided = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow!("missing Authorization header"))?;
    let prefix = "Bearer ";
    if !provided.starts_with(prefix) {
        return Err(anyhow!("invalid Authorization scheme"));
    }
    let token = &provided[prefix.len()..];
    if token != expected_token {
        return Err(anyhow!("invalid control token"));
    }
    Ok(())
}

fn authorize_remote_access(headers: &HeaderMap, state: &AppState) -> Result<()> {
    if state.require_control_token {
        authorize_control(headers, state)?;
    }
    Ok(())
}

fn into_origin(origin: IncomingOrigin) -> MessageOrigin {
    match origin {
        IncomingOrigin::Operator { actor_id } => MessageOrigin::Operator { actor_id },
        IncomingOrigin::Channel {
            channel_id,
            sender_id,
        } => MessageOrigin::Channel {
            channel_id,
            sender_id,
        },
        IncomingOrigin::Webhook { source, event_type } => {
            MessageOrigin::Webhook { source, event_type }
        }
        IncomingOrigin::Timer { timer_id } => MessageOrigin::Timer { timer_id },
        IncomingOrigin::System { subsystem } => MessageOrigin::System { subsystem },
        IncomingOrigin::Task { task_id } => MessageOrigin::Task { task_id },
    }
}

fn require_non_empty(value: String, field: &str) -> Result<String, (StatusCode, Json<Value>)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(bad_request(format!("{field} must not be empty")));
    }
    Ok(value)
}

fn non_empty_or_generated(value: Option<String>, prefix: &str) -> String {
    value
        .and_then(non_empty_opt)
        .unwrap_or_else(|| crate::ids::runtime_id(prefix))
}

fn non_empty_opt(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn non_empty_str(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn validate_operator_transport_delivery_auth(
    mut auth: OperatorTransportDeliveryAuth,
) -> Result<OperatorTransportDeliveryAuth, (StatusCode, Json<Value>)> {
    match auth.kind {
        OperatorTransportDeliveryAuthKind::Bearer => {
            let Some(token) = auth.bearer_token.take().and_then(non_empty_opt) else {
                return Err(bad_request(
                    "delivery_auth.bearer_token must be non-empty when kind is bearer",
                ));
            };
            auth.bearer_token = Some(token);
            Ok(auth)
        }
        OperatorTransportDeliveryAuthKind::Hmac => Err(bad_request(
            "delivery_auth.kind=hmac is not supported until HMAC signing is implemented",
        )),
    }
}

fn forbidden(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::FORBIDDEN, HttpErrorEnvelope::new(reason))
}

fn bad_request(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::BAD_REQUEST, HttpErrorEnvelope::new(reason))
}

fn service_unavailable(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::SERVICE_UNAVAILABLE,
        HttpErrorEnvelope::new(reason),
    )
}

fn not_found(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::NOT_FOUND, HttpErrorEnvelope::new(reason))
}

fn task_lifecycle_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    let message = error.to_string();
    if message.starts_with("task ") && message.ends_with(" not found") {
        not_found(message)
    } else {
        error_response(error)
    }
}

fn work_item_lifecycle_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if (lower.contains("work item") && lower.ends_with("not found"))
        || lower.starts_with("unknown work item ")
    {
        not_found(message)
    } else if message.starts_with("cannot ") {
        bad_request(message)
    } else {
        error_response(error)
    }
}

fn normalize_optional_non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|inner| {
        let trimmed = inner.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn parse_blocked_by_mutation(value: Value) -> Result<Option<String>, (StatusCode, Json<Value>)> {
    match value {
        Value::Null => Ok(None),
        Value::String(inner) => {
            let trimmed = inner.trim().to_string();
            Ok((!trimmed.is_empty()).then_some(trimmed))
        }
        _ => Err(bad_request("blocked_by must be a string or null")),
    }
}

fn stopped_agent_conflict(
    reason: impl Into<String>,
    agent_id: impl Into<String>,
) -> (StatusCode, Json<Value>) {
    let agent_id = agent_id.into();
    let hint = format!(
        "start with `holon agent start {}` or POST /control/agents/{}/control with JSON body {{\"action\":\"start\"}}",
        agent_id, agent_id
    );
    http_error(
        StatusCode::CONFLICT,
        HttpErrorEnvelope::new(reason)
            .code("agent_stopped")
            .hint(hint)
            .extension("agent_id", agent_id),
    )
}

fn agent_access_error(error: PublicAgentError) -> (StatusCode, Json<Value>) {
    match error {
        PublicAgentError::Private { agent_id } => {
            forbidden(format!("agent {} is private", agent_id))
        }
        PublicAgentError::NotFound { agent_id } => {
            not_found(format!("agent {} not found", agent_id))
        }
        PublicAgentError::Archived { agent_id } => {
            not_found(format!("agent {} is archived", agent_id))
        }
        PublicAgentError::Stopped { agent_id } => stopped_agent_conflict(
            format!("agent {} is stopped; start first", agent_id),
            agent_id,
        ),
        PublicAgentError::Runtime(error) => error_response(error),
    }
}

fn abort_error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    match error.downcast::<CurrentRunAbortError>() {
        Ok(CurrentRunAbortError::StaleRunId {
            requested_run_id,
            current_run_id,
        }) => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new(format!(
                "stale run_id {requested_run_id}; current run is {current_run_id}"
            ))
            .code("stale_run_id")
            .extension("requested_run_id", requested_run_id)
            .extension("current_run_id", current_run_id),
        ),
        Ok(CurrentRunAbortError::NoCurrentRun { agent_id }) => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new(format!("agent {agent_id} has no current run to abort"))
                .code("no_current_run")
                .extension("agent_id", agent_id),
        ),
        Err(error) => error_response(error),
    }
}

fn skill_install_error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    match error.downcast::<crate::skills::SkillInstallConflict>() {
        Ok(conflict) => http_error(
            StatusCode::CONFLICT,
            HttpErrorEnvelope::new(conflict.to_string())
                .code("skill_already_installed")
                .hint("uninstall the existing skill first or choose a different skill name")
                .extension("skill_name", conflict.skill_name)
                .extension("destination", conflict.destination.to_string_lossy().to_string()),
        ),
        Err(error) => match error.downcast::<crate::skills::SkillManagerUnavailable>() {
            Ok(unavailable) => http_error(
                StatusCode::FAILED_DEPENDENCY,
                HttpErrorEnvelope::new(unavailable.to_string())
                    .code("skill_manager_unavailable")
                    .hint("Install Node.js/npm so `npx skills` is available, or install the skill manually into ~/.agents/skills and link it by name.")
                    .extension("manager", unavailable.manager),
            ),
            Err(error) => match error.downcast::<crate::skills::RemoteSkillInstallFailed>() {
                Ok(failed) => http_error(
                    StatusCode::BAD_GATEWAY,
                    HttpErrorEnvelope::new(failed.to_string())
                        .code("remote_skill_install_failed")
                        .extension("package", failed.package)
                        .extension("exit_status", failed.status)
                        .extension("stdout", failed.stdout)
                        .extension("stderr", failed.stderr),
                ),
                Err(error) => match error.downcast::<crate::skills::RemoteSkillInstallTimedOut>() {
                    Ok(timeout) => http_error(
                        StatusCode::GATEWAY_TIMEOUT,
                        HttpErrorEnvelope::new(timeout.to_string())
                            .code("remote_skill_install_timeout")
                            .extension("package", timeout.package)
                            .extension("timeout_seconds", timeout.timeout.as_secs()),
                    ),
                    Err(error) => error_response(error),
                },
            },
        },
    }
}

fn error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        HttpErrorEnvelope::new(error.to_string()),
    )
}

fn http_error(status: StatusCode, envelope: HttpErrorEnvelope) -> (StatusCode, Json<Value>) {
    let value = serde_json::to_value(envelope).expect("HTTP error envelope serializes");
    (status, Json(value))
}

#[cfg(unix)]
pub async fn serve_unix(
    listener: UnixListener,
    router: Router,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    loop {
        let (stream, _) = tokio::select! {
            changed = shutdown.changed() => {
                match changed {
                    Ok(()) if *shutdown.borrow() => return Ok(()),
                    Ok(()) => continue,
                    Err(_) => return Ok(()),
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok(accepted) => accepted,
                    Err(err) if crate::fd_limit::is_fd_exhaustion_error(&err) => {
                        warn!(
                            error = %err,
                            backoff_ms = 100,
                            "unix control server accept hit file descriptor limit; backing off"
                        );
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            }
        };
        let router = router.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request: Request<Incoming>| {
                let router = router.clone();
                async move {
                    let response = router
                        .oneshot(request.map(Body::new))
                        .await
                        .unwrap_or_else(|err| match err {});
                    Ok::<_, Infallible>(response)
                }
            });
            if let Err(err) = HyperBuilder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                .await
            {
                error!(error = %err, "unix control server connection failed");
            }
        });
    }
}
