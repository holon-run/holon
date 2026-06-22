use std::{
    collections::VecDeque,
    path::{Component, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, MatchedPath, Path, Query, State},
    http::{
        header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
        HeaderMap, Method, Request as AxumRequest, Response, StatusCode, Uri,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response as AxumResponse,
    },
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use percent_encoding::percent_decode_str;
use rust_embed::RustEmbed;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::{
    classify::ServerErrorsFailureClass, compression::CompressionLayer, trace::TraceLayer,
};
use tracing::{error, info, warn, Span};

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
        credential_store_path, list_credential_profiles_at, load_credential_store_at,
        load_persisted_config_at, remove_credential_profile_at, save_persisted_config_at,
        set_config_key, set_credential_profile_at, unset_config_key, ControlTransportKind,
        CredentialKind, CredentialProfileStatus, HolonConfigFile, ModelRef,
    },
    daemon::{
        graceful_runtime_shutdown, runtime_activity_summary, RuntimeConfigSurface,
        RuntimeServiceHandle,
    },
    diagnostics,
    host::{PublicAgentError, RuntimeHost},
    ingress::{InboundRequest, WakeDisposition, WakeHint},
    operator_event::{
        is_operator_event_in_display_mode, OperatorDisplayMode, OperatorPresentationContext,
    },
    policy::{default_authority_for_origin, validate_message_kind_for_origin},
    runtime::{CurrentRunAbortMode, CurrentRunAbortRequest},
    storage::EventLogPageOrder,
    system::{ExecutionScopeKind, HostLocalBoundary},
    types::{
        ActiveWorkspaceEntry, AdmissionContext, AgentRegistryStatus, AgentSummary, AgentVisibility,
        AuditEvent, AuthorityClass, CallbackDeliveryPayload, CallbackDeliveryResult, ControlAction,
        ExternalTriggerStateSnapshot, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, OperatorTransportBinding, OperatorTransportBindingStatus,
        OperatorTransportCapabilities, OperatorTransportDeliveryAuth,
        OperatorTransportDeliveryAuthKind, Priority, TaskRecord, TaskStatus, TaskStatusSnapshot,
        TaskStopResult, TimerRecord, TodoItem, TranscriptEntry, TurnTerminalRecord,
        WaitingIntentRecord, WaitingReason, WorkItemPlanStatus, WorkItemRecord, WorkItemState,
        WorkspaceOccupancyRecord, WorktreeSession,
    },
};

const STATE_BOOTSTRAP_TASK_LIMIT: usize = 40;
const STATE_BOOTSTRAP_WORK_ITEM_LIMIT: usize = 50;
const STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT: usize = 512;
const STATE_BOOTSTRAP_LAST_TURN_TEXT_LIMIT: usize = 2048;
#[cfg(test)]
const STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT: usize = 8192;
#[cfg(test)]
const STATE_BOOTSTRAP_JSON_ARRAY_LIMIT: usize = 64;
const HTTP_SLOW_RESPONSE_WARN_AFTER: std::time::Duration = std::time::Duration::from_secs(2);
const HTTP_LARGE_RESPONSE_WARN_BYTES: usize = 128 * 1024;

static HTTP_IN_FLIGHT_REQUESTS: AtomicUsize = AtomicUsize::new(0);

#[derive(RustEmbed)]
#[folder = "web-gui/app/dist/"]
struct EmbeddedWebAssets;

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

mod cors;
mod error;
use error::*;
mod agents;
mod control;
mod enqueue;
mod events;
mod runtime;
mod state;
mod tasks;
mod timers;
mod work_items;

// Bring handler functions into scope for router composition.
pub(crate) use agents::{
    brief, briefs, briefs_default, create_agent, handshake, install_skill, list_agent_entries,
    list_skills, models_handler, root, search, status, status_default, transcript,
    transcript_batch_get, transcript_default, transcript_entry, uninstall_skill, worktree_summary,
    worktree_summary_default,
};
pub(crate) use control::{
    abort_current_run, attach_workspace, clear_agent_model, control, control_debug_prompt,
    control_prompt, control_wake, create_operator_transport_binding, detach_workspace,
    exit_workspace, operator_ingress, set_agent_model,
};
pub(crate) use enqueue::{enqueue, enqueue_default};
pub(crate) use events::{events, events_stream, global_events_stream, message, messages_batch_get};
pub(crate) use runtime::{
    delete_credential, list_credentials, runtime_config, runtime_config_update,
    runtime_performance, runtime_readiness, runtime_shutdown, runtime_status, set_credential,
};
pub(crate) use state::{agent_state, sort_state_work_items, state_default};
#[cfg(test)]
use state::{slim_state_task_record, slim_state_work_item_record};

pub(crate) use tasks::{
    create_command_task, task_input, task_output, task_status, task_stop, tasks, tool_execution,
};
pub(crate) use timers::{cancel_timer, create_timer, timer, timers};
pub(crate) use work_items::{
    complete_work_item, create_work_item, pick_work_item, update_work_item, work_item, work_items,
};

#[derive(Clone)]
pub struct AppState {
    pub host: RuntimeHost,
    pub require_control_token: bool,
    pub runtime_service: Option<RuntimeServiceHandle>,
    pub advertise_url: Option<String>,
    pub web_dist: Option<Arc<PathBuf>>,
}

const CALLBACK_BODY_LIMIT_BYTES: usize = 256 * 1024;
const DEFAULT_EVENT_STREAM_WINDOW: usize = 128;
const MAX_EVENT_STREAM_WINDOW: usize = 512;
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
            web_dist: None,
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
            web_dist: None,
        }
    }

    pub fn with_advertise_url(mut self, advertise_url: Option<String>) -> Self {
        self.advertise_url = advertise_url;
        self
    }

    pub fn with_web_dist(mut self, web_dist: Option<PathBuf>) -> Self {
        self.web_dist = web_dist.map(Arc::new);
        self
    }
}

pub fn router(state: AppState) -> Router {
    let config = state.host.config();
    let api_routes = Router::new()
        .route("/", get(root))
        .route("/handshake", get(handshake))
        .route("/models", get(models_handler))
        .route("/search", post(search))
        .route("/agents/list", get(list_agent_entries))
        .route("/agents/{agent_id}/enqueue", post(enqueue))
        .route("/agents/{agent_id}/status", get(status))
        .route("/agents/{agent_id}/briefs", get(briefs))
        .route("/agents/{agent_id}/briefs/{brief_id}", get(brief))
        .route("/agents/{agent_id}/state", get(agent_state))
        .route("/events/stream", get(global_events_stream))
        .route("/agents/{agent_id}/events", get(events))
        .route("/agents/{agent_id}/events/stream", get(events_stream))
        .route(
            "/agents/{agent_id}/messages:batchGet",
            post(messages_batch_get),
        )
        .route("/agents/{agent_id}/messages/{message_id}", get(message))
        .route("/agents/{agent_id}/transcript", get(transcript))
        .route(
            "/agents/{agent_id}/transcript:batchGet",
            post(transcript_batch_get),
        )
        .route(
            "/agents/{agent_id}/transcript/{entry_id}",
            get(transcript_entry),
        )
        .route("/agents/{agent_id}/tasks", get(tasks))
        .route("/agents/{agent_id}/tasks/{task_id}", get(task_status))
        .route(
            "/agents/{agent_id}/tasks/{task_id}/output",
            get(task_output),
        )
        .route(
            "/agents/{agent_id}/tool-executions/{tool_execution_id}",
            get(tool_execution),
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
        .route("/control/runtime/performance", get(runtime_performance))
        .route("/control/runtime/config", get(runtime_config))
        .route("/control/runtime/config", patch(runtime_config_update))
        .route("/control/runtime/shutdown", post(runtime_shutdown))
        .route("/control/runtime/credentials", get(list_credentials))
        .route(
            "/control/runtime/credentials/{profile}",
            put(set_credential),
        )
        .route(
            "/control/runtime/credentials/{profile}",
            delete(delete_credential),
        )
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
        .route("/worktree-summary", get(worktree_summary_default));

    Router::new()
        .merge(api_routes.clone())
        .nest("/api", api_routes)
        .fallback(web_or_not_found_handler)
        .layer(cors::api_cors_layer(&config.api_cors))
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
    diagnostics::record_http_json_response(route, build_elapsed, bytes.len());
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

const SEARCH_DEFAULT_LIMIT: usize = 20;
const SEARCH_MAX_LIMIT: usize = 50;

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
    workspace: StateWorkspaceSnapshot,
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
    AcceptedReloaded,
    Rejected,
}

#[derive(Debug, Serialize)]
struct CredentialListResponse {
    ok: bool,
    profiles: Vec<CredentialProfileStatus>,
}

#[derive(Debug, Deserialize)]
pub struct SetCredentialRequest {
    kind: String,
    material: String,
}

#[derive(Debug, Serialize)]
struct SetCredentialResponse {
    ok: bool,
    profile: CredentialProfileStatus,
}

#[derive(Debug, Serialize)]
struct DeleteCredentialResponse {
    ok: bool,
    profile: CredentialProfileStatus,
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

#[cfg(test)]
fn slim_state_transcript_entry(
    mut entry: crate::types::TranscriptEntry,
) -> crate::types::TranscriptEntry {
    entry.data = slim_state_json_value(entry.data, STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT);
    entry
}

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::{
        sort_state_work_items, STATE_BOOTSTRAP_JSON_ARRAY_LIMIT,
        STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT, STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT,
    };
    use crate::types::{
        TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState, TranscriptEntry,
        TranscriptEntryKind, WorkItemRecord, WorkItemState,
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
    fn state_bootstrap_omits_task_detail_and_slims_transcript_data() {
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
                "output_summary": "x".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64),
                "lines": (0..(STATE_BOOTSTRAP_JSON_ARRAY_LIMIT + 10)).collect::<Vec<_>>()
            })),
            recovery: None,
        };
        let slimmed = super::slim_state_task_record(task);
        assert!(slimmed.detail.is_none());
        assert!(slimmed.recovery.is_none());

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
    fn state_bootstrap_slims_work_item_records() {
        let mut item = WorkItemRecord::new(
            "default",
            "x".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64),
            WorkItemState::Open,
        );
        item.todo_list = vec![TodoItem {
            text: "large todo".into(),
            state: TodoItemState::InProgress,
        }];
        item.blocked_by = Some("b".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64));
        item.result_summary = Some("r".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT + 64));

        let slimmed = super::slim_state_work_item_record(item);

        assert!(slimmed.objective.chars().count() <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
        assert!(slimmed.todo_list.is_empty());
        assert!(slimmed.work_refs.is_empty());
        assert!(slimmed.plan_artifact.is_none());
        assert!(
            slimmed
                .blocked_by
                .as_deref()
                .expect("blocker")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT
        );
        assert!(
            slimmed
                .result_summary
                .as_deref()
                .expect("result")
                .chars()
                .count()
                <= STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT
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

async fn web_or_not_found_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> AxumResponse {
    if matches!(method, Method::GET | Method::HEAD) {
        let head_only = method == Method::HEAD;
        let request_path = uri.path().trim_start_matches('/');
        if !request_path.is_empty() {
            if let Some(response) = web_asset_response(&state, request_path, head_only).await {
                return response;
            }
        }
        if accepts_html(&headers) {
            if let Some(response) = web_asset_response(&state, "index.html", head_only).await {
                return response;
            }
        }
    }
    not_found("Not Found").into_response()
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| {
            accept
                .split(',')
                .any(|part| part.trim_start().starts_with("text/html"))
        })
}

async fn web_asset_response(
    state: &AppState,
    request_path: &str,
    head_only: bool,
) -> Option<AxumResponse> {
    let path = normalize_web_asset_path(request_path)?;
    let bytes = if let Some(web_dist) = &state.web_dist {
        tokio::fs::read(web_dist.join(&path)).await.ok()?
    } else {
        EmbeddedWebAssets::get(&path)?.data.into_owned()
    };
    let content_type = mime_guess::from_path(&path).first_or_octet_stream();
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type.as_ref())
        .body(body)
        .ok()
}

fn normalize_web_asset_path(request_path: &str) -> Option<String> {
    let decoded = percent_decode_str(request_path).decode_utf8().ok()?;
    let decoded = decoded.trim_start_matches('/');
    if decoded.is_empty() || decoded.contains('\\') {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in std::path::Path::new(decoded).components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            _ => return None,
        }
    }
    normalized.to_str().map(|path| path.replace('\\', "/"))
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
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
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
    let config = state.host.config();
    let expected_token = config
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
