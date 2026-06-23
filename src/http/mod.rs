pub(crate) use std::{
    collections::VecDeque,
    path::{Component, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

pub(crate) use anyhow::{anyhow, Result};
pub(crate) use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, MatchedPath, Path, Query, State},
    http::{
        header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
        HeaderMap, HeaderName, HeaderValue, Method, Request as AxumRequest, Response, StatusCode,
        Uri,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response as AxumResponse,
    },
    routing::{delete, get, patch, post, put},
    Json, Router,
};
pub(crate) use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
pub(crate) use chrono::Utc;
pub(crate) use percent_encoding::percent_decode_str;
pub(crate) use rust_embed::RustEmbed;
pub(crate) use schemars::JsonSchema;
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use serde_json::{json, Map, Value};
pub(crate) use tokio::time::{sleep, Duration};
pub(crate) use tokio_stream::wrappers::ReceiverStream;
pub(crate) use tower_http::{
    classify::ServerErrorsFailureClass,
    compression::CompressionLayer,
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
pub(crate) use tracing::{error, info, warn, Span};

#[cfg(unix)]
pub(crate) use hyper::{body::Incoming, service::service_fn, Request};
#[cfg(unix)]
pub(crate) use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as HyperBuilder,
};
#[cfg(unix)]
pub(crate) use std::convert::Infallible;
#[cfg(unix)]
pub(crate) use tokio::{net::UnixListener, sync::watch};
#[cfg(unix)]
pub(crate) use tower::ServiceExt;

pub(crate) use crate::{
    config::{
        credential_store_path, list_credential_profiles_at, load_credential_store_at,
        load_persisted_config_at, remove_credential_profile_at, save_persisted_config_at,
        set_config_key, set_credential_profile_at, unset_config_key, ApiCorsConfigFile,
        ControlTransportKind, CredentialKind, CredentialProfileStatus, HolonConfigFile, ModelRef,
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
    runtime::{CurrentRunAbortError, CurrentRunAbortMode, CurrentRunAbortRequest},
    skills::registry::SkillsRegistry,
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
mod agents;
mod control;
mod events;
mod ingress;
mod skills;
mod state;
mod tasks;
mod types;
mod web;

// Re-export shared helpers used across submodules.
pub(crate) use state::{
    control_admission_context, current_boundary_metadata, enqueue_internal,
    public_admission_context, sort_state_work_items, EnqueueIngress,
};
pub(crate) use web::{accepts_html, web_asset_response};

pub use agents::*;
pub use control::*;
pub use events::{events, events_stream, global_events_stream, message, messages_batch_get};
pub use ingress::{callback_ingress_enqueue, callback_ingress_wake, generic_webhook};
pub use skills::{install_skill, list_skills, skills_catalog, uninstall_skill};
pub use state::{
    agent_state, brief, briefs, briefs_default, enqueue, enqueue_default, state_default, status,
    status_default, transcript, transcript_batch_get, transcript_default, transcript_entry,
    worktree_summary, worktree_summary_default,
};
pub use tasks::{
    cancel_timer, complete_work_item, create_command_task, create_timer, create_work_item,
    pick_work_item, task_input, task_output, task_status, task_stop, tasks, timer, timers,
    tool_execution, update_work_item, work_item, work_items,
};
pub use types::*;
pub use web::web_or_not_found_handler;

pub(crate) const STATE_BOOTSTRAP_TASK_LIMIT: usize = 40;
pub(crate) const STATE_BOOTSTRAP_WORK_ITEM_LIMIT: usize = 50;
pub(crate) const STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT: usize = 512;
pub(crate) const STATE_BOOTSTRAP_LAST_TURN_TEXT_LIMIT: usize = 2048;
#[cfg(test)]
pub(crate) const STATE_BOOTSTRAP_TRANSCRIPT_DATA_STRING_LIMIT: usize = 8192;
#[cfg(test)]
pub(crate) const STATE_BOOTSTRAP_JSON_ARRAY_LIMIT: usize = 64;
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

#[derive(Clone)]
pub struct AppState {
    pub host: RuntimeHost,
    pub require_control_token: bool,
    pub runtime_service: Option<RuntimeServiceHandle>,
    pub advertise_url: Option<String>,
    pub web_dist: Option<Arc<PathBuf>>,
    pub skills_registry: Arc<tokio::sync::RwLock<SkillsRegistry>>,
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

pub(crate) const CALLBACK_BODY_LIMIT_BYTES: usize = 256 * 1024;
pub(crate) const DEFAULT_EVENT_STREAM_WINDOW: usize = 128;
pub(crate) const MAX_EVENT_STREAM_WINDOW: usize = 512;
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
            skills_registry: Arc::new(tokio::sync::RwLock::new(SkillsRegistry::new())),
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
            skills_registry: Arc::new(tokio::sync::RwLock::new(SkillsRegistry::new())),
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
        .route("/", get(agents::root))
        .route("/handshake", get(agents::handshake))
        .route("/models", get(agents::models_handler))
        .route("/agents/list", get(agents::list_agent_entries))
        .route("/agents/{agent_id}/enqueue", post(state::enqueue))
        .route("/agents/{agent_id}/status", get(state::status))
        .route("/agents/{agent_id}/briefs", get(state::briefs))
        .route("/agents/{agent_id}/briefs/{brief_id}", get(state::brief))
        .route("/agents/{agent_id}/state", get(state::agent_state))
        .route("/events/stream", get(events::global_events_stream))
        .route("/agents/{agent_id}/events", get(events::events))
        .route(
            "/agents/{agent_id}/events/stream",
            get(events::events_stream),
        )
        .route(
            "/agents/{agent_id}/messages:batchGet",
            post(events::messages_batch_get),
        )
        .route(
            "/agents/{agent_id}/messages/{message_id}",
            get(events::message),
        )
        .route("/agents/{agent_id}/transcript", get(state::transcript))
        .route(
            "/agents/{agent_id}/transcript:batchGet",
            post(state::transcript_batch_get),
        )
        .route(
            "/agents/{agent_id}/transcript/{entry_id}",
            get(state::transcript_entry),
        )
        .route("/agents/{agent_id}/tasks", get(tasks::tasks))
        .route(
            "/agents/{agent_id}/tasks/{task_id}",
            get(tasks::task_status),
        )
        .route(
            "/agents/{agent_id}/tasks/{task_id}/output",
            get(tasks::task_output),
        )
        .route(
            "/agents/{agent_id}/tool-executions/{tool_execution_id}",
            get(tasks::tool_execution),
        )
        .route(
            "/control/agents/{agent_id}/tasks/{task_id}/input",
            post(tasks::task_input),
        )
        .route(
            "/control/agents/{agent_id}/tasks/{task_id}/stop",
            post(tasks::task_stop),
        )
        .route("/agents/{agent_id}/work-items", get(tasks::work_items))
        .route(
            "/agents/{agent_id}/work-items/{work_item_id}",
            get(tasks::work_item),
        )
        .route(
            "/agents/{agent_id}/worktree-summary",
            get(state::worktree_summary),
        )
        .route("/agents/{agent_id}/timers", get(tasks::timers))
        .route("/agents/{agent_id}/timers/{timer_id}", get(tasks::timer))
        .route(
            "/control/agents/{agent_id}/tasks",
            post(tasks::create_command_task),
        )
        .route(
            "/control/agents/{agent_id}/work-items",
            post(tasks::create_work_item),
        )
        .route(
            "/control/agents/{agent_id}/work-items/{work_item_id}/pick",
            post(tasks::pick_work_item),
        )
        .route(
            "/control/agents/{agent_id}/work-items/{work_item_id}",
            patch(tasks::update_work_item),
        )
        .route(
            "/control/agents/{agent_id}/work-items/{work_item_id}/complete",
            post(tasks::complete_work_item),
        )
        .route(
            "/control/agents/{agent_id}/timers",
            post(tasks::create_timer),
        )
        .route(
            "/control/agents/{agent_id}/timers/{timer_id}/cancel",
            post(tasks::cancel_timer),
        )
        .route(
            "/control/agents/{agent_id}/create",
            post(control::create_agent),
        )
        .route(
            "/control/agents/{agent_id}/workspace/attach",
            post(control::attach_workspace),
        )
        .route(
            "/control/agents/{agent_id}/workspace/exit",
            post(control::exit_workspace),
        )
        .route(
            "/control/agents/{agent_id}/workspace/detach",
            post(control::detach_workspace),
        )
        .route(
            "/control/agents/{agent_id}/model",
            post(control::set_agent_model),
        )
        .route(
            "/control/agents/{agent_id}/model/clear",
            post(control::clear_agent_model),
        )
        .route("/control/agents/{agent_id}/control", post(control::control))
        .route(
            "/control/agents/{agent_id}/current-run/abort",
            post(control::abort_current_run),
        )
        .route(
            "/control/agents/{agent_id}/prompt",
            post(control::control_prompt),
        )
        .route(
            "/control/agents/{agent_id}/operator-bindings",
            post(control::create_operator_transport_binding),
        )
        .route(
            "/control/agents/{agent_id}/operator-ingress",
            post(control::operator_ingress),
        )
        .route(
            "/control/runtime/readiness",
            get(control::runtime_readiness),
        )
        .route("/control/runtime/status", get(control::runtime_status))
        .route(
            "/control/runtime/performance",
            get(control::runtime_performance),
        )
        .route("/control/runtime/config", get(control::runtime_config))
        .route(
            "/control/runtime/config",
            patch(control::runtime_config_update),
        )
        .route("/control/runtime/shutdown", post(control::runtime_shutdown))
        .route(
            "/control/runtime/credentials",
            get(control::list_credentials),
        )
        .route(
            "/control/runtime/credentials/{profile}",
            put(control::set_credential),
        )
        .route(
            "/control/runtime/credentials/{profile}",
            delete(control::delete_credential),
        )
        .route(
            "/control/agents/{agent_id}/debug-prompt",
            post(control::control_debug_prompt),
        )
        .route(
            "/control/agents/{agent_id}/wake",
            post(control::control_wake),
        )
        .route(
            "/callbacks/enqueue/{callback_token}",
            post(ingress::callback_ingress_enqueue)
                .layer(DefaultBodyLimit::max(CALLBACK_BODY_LIMIT_BYTES)),
        )
        .route(
            "/callbacks/wake/{callback_token}",
            post(ingress::callback_ingress_wake)
                .layer(DefaultBodyLimit::max(CALLBACK_BODY_LIMIT_BYTES)),
        )
        .route(
            "/webhooks/generic/{agent_id}",
            post(ingress::generic_webhook),
        )
        .route("/enqueue", post(state::enqueue_default))
        .route("/agents/{agent_id}/skills", get(skills::list_skills))
        .route("/api/skills/catalog", get(skills::skills_catalog))
        .route(
            "/control/agents/{agent_id}/skills/install",
            post(skills::install_skill),
        )
        .route(
            "/control/agents/{agent_id}/skills/uninstall",
            post(skills::uninstall_skill),
        )
        .route("/status", get(state::status_default))
        .route("/briefs", get(state::briefs_default))
        .route("/state", get(state::state_default))
        .route("/transcript", get(state::transcript_default))
        .route("/worktree-summary", get(state::worktree_summary_default));
    let root_routes = api_routes.clone().route(
        "/search",
        get(web::web_or_not_found_handler).post(agents::search),
    );
    let api_routes = api_routes
        .route("/search", post(agents::search))
        .route("/memory/get", post(agents::memory_get));

    Router::new()
        .merge(root_routes)
        .nest("/api", api_routes)
        .fallback(web::web_or_not_found_handler)
        .layer(api_cors_layer(&config.api_cors))
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

fn api_cors_layer(config: &ApiCorsConfigFile) -> CorsLayer {
    if !config.enabled() {
        return CorsLayer::new();
    }

    let methods = config
        .allowed_methods
        .iter()
        .filter_map(|method| method.parse::<Method>().ok())
        .collect::<Vec<_>>();
    let headers = config
        .allowed_headers
        .iter()
        .filter_map(|header| header.parse::<HeaderName>().ok())
        .collect::<Vec<_>>();

    let allow_origin = if config.allowed_origins.iter().any(|origin| origin == "*") {
        AllowOrigin::any()
    } else {
        let configured_origins = config
            .allowed_origins
            .iter()
            .filter_map(|origin| origin.parse::<HeaderValue>().ok())
            .collect::<Vec<_>>();
        AllowOrigin::predicate(move |origin, _| {
            is_default_localhost_cors_origin(origin) || configured_origins.contains(origin)
        })
    };

    let mut layer = CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods(methods)
        .allow_headers(headers)
        .max_age(Duration::from_secs(config.max_age_seconds()));

    if config.allow_credentials() {
        layer = layer.allow_credentials(true);
    }

    layer
}

fn is_default_localhost_cors_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(origin) = url::Url::parse(origin) else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https") {
        return false;
    }
    if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() {
        return false;
    }
    match origin.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(addr)) => addr == std::net::Ipv6Addr::LOCALHOST,
        None => false,
    }
}
pub(crate) fn traced_json<T: Serialize>(
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
pub(crate) fn authorize_control(headers: &HeaderMap, state: &AppState) -> Result<()> {
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

pub(crate) fn authorize_remote_access(headers: &HeaderMap, state: &AppState) -> Result<()> {
    if state.require_control_token {
        authorize_control(headers, state)?;
    }
    Ok(())
}

pub(crate) fn into_origin(origin: IncomingOrigin) -> MessageOrigin {
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

pub(crate) fn require_non_empty(
    value: String,
    field: &str,
) -> Result<String, (StatusCode, Json<Value>)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(bad_request(format!("{field} must not be empty")));
    }
    Ok(value)
}

pub(crate) fn non_empty_or_generated(value: Option<String>, prefix: &str) -> String {
    value
        .and_then(non_empty_opt)
        .unwrap_or_else(|| crate::ids::runtime_id(prefix))
}

pub(crate) fn non_empty_opt(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn non_empty_str(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub(crate) fn validate_operator_transport_delivery_auth(
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

pub(crate) fn forbidden(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::FORBIDDEN, HttpErrorEnvelope::new(reason))
}

pub(crate) fn auth_required(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::FORBIDDEN,
        HttpErrorEnvelope::new(reason)
            .code("auth_required")
            .hint("retry with an Authorization: Bearer <token> header"),
    )
}

pub(crate) fn bad_request(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::BAD_REQUEST, HttpErrorEnvelope::new(reason))
}

pub(crate) fn service_unavailable(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::SERVICE_UNAVAILABLE,
        HttpErrorEnvelope::new(reason),
    )
}

pub(crate) fn not_found(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(StatusCode::NOT_FOUND, HttpErrorEnvelope::new(reason))
}

pub(crate) fn task_lifecycle_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    let message = error.to_string();
    if message.starts_with("task ") && message.ends_with(" not found") {
        not_found(message)
    } else {
        error_response(error)
    }
}

pub(crate) fn work_item_lifecycle_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
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

pub(crate) fn normalize_optional_non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|inner| {
        let trimmed = inner.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(crate) fn parse_blocked_by_mutation(
    value: Value,
) -> Result<Option<String>, (StatusCode, Json<Value>)> {
    match value {
        Value::Null => Ok(None),
        Value::String(inner) => {
            let trimmed = inner.trim().to_string();
            Ok((!trimmed.is_empty()).then_some(trimmed))
        }
        _ => Err(bad_request("blocked_by must be a string or null")),
    }
}

pub(crate) fn stopped_agent_conflict(
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

pub(crate) fn agent_access_error(error: PublicAgentError) -> (StatusCode, Json<Value>) {
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

pub(crate) fn abort_error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
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

pub(crate) fn skill_install_error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
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

pub(crate) fn error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        HttpErrorEnvelope::new(error.to_string()),
    )
}

pub(crate) fn http_error(
    status: StatusCode,
    envelope: HttpErrorEnvelope,
) -> (StatusCode, Json<Value>) {
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

#[cfg(test)]
mod tests {
    use super::{router, AppState};
    use crate::{config::AppConfig, host::RuntimeHost, provider::StubProvider};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
    };
    use std::{fs, sync::Arc};
    use tempfile::tempdir;
    use tower::ServiceExt;

    fn test_host() -> (tempfile::TempDir, RuntimeHost) {
        let home = tempdir().unwrap();
        fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        (home, host)
    }

    #[tokio::test]
    async fn get_search_serves_web_app_instead_of_api_method_not_allowed() {
        let (_home, host) = test_host();
        let web_dist = tempdir().unwrap();
        fs::write(web_dist.path().join("index.html"), "<html>holon ui</html>").unwrap();
        let app =
            router(AppState::for_tcp(host).with_web_dist(Some(web_dist.path().to_path_buf())));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/search")
                    .header(header::ACCEPT, "text/html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"<html>holon ui</html>");
    }
}
