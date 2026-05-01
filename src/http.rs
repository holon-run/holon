use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        HeaderMap, StatusCode,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;
use uuid::Uuid;

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
    config::{ControlTransportKind, ModelRef},
    daemon::{
        graceful_runtime_shutdown, runtime_activity_summary, RuntimeConfigSurface,
        RuntimeServiceHandle,
    },
    host::{PublicAgentError, RuntimeHost},
    ingress::{InboundRequest, WakeDisposition, WakeHint},
    policy::{default_trust_for_origin, validate_message_kind_for_origin},
    storage::FileActivityMarker,
    system::{ExecutionScopeKind, ExecutionSnapshot, HostLocalBoundary},
    types::{
        ActiveWorkspaceEntry, AdmissionContext, AgentRegistryStatus, AgentSummary, AgentVisibility,
        AuditEvent, BriefRecord, CallbackDeliveryPayload, CallbackDeliveryResult, ControlAction,
        ExternalTriggerStateSnapshot, MessageBody, MessageDeliverySurface, MessageKind,
        MessageOrigin, OperatorNotificationRecord, OperatorTransportBinding,
        OperatorTransportBindingStatus, OperatorTransportCapabilities,
        OperatorTransportDeliveryAuth, OperatorTransportDeliveryAuthKind, Priority, TaskRecord,
        TimerRecord, TranscriptEntry, TrustLevel, TurnTerminalRecord, WaitingIntentRecord,
        WorkItemRecord, WorkItemState, WorkPlanSnapshot, WorkspaceOccupancyRecord, WorktreeSession,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub host: RuntimeHost,
    pub require_control_token: bool,
    pub runtime_service: Option<RuntimeServiceHandle>,
}

const CALLBACK_BODY_LIMIT_BYTES: usize = 256 * 1024;
const DEFAULT_EVENT_STREAM_WINDOW: usize = 128;
const MAX_EVENT_STREAM_WINDOW: usize = 512;
const EVENT_STREAM_POLL_INTERVAL: Duration = Duration::from_millis(250);

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
        }
    }

    pub fn for_unix(host: RuntimeHost) -> Self {
        Self::for_unix_with_runtime_service(host, None)
    }

    pub fn for_unix_with_runtime_service(
        host: RuntimeHost,
        runtime_service: Option<RuntimeServiceHandle>,
    ) -> Self {
        let require_control_token = host
            .config()
            .control_token_required(ControlTransportKind::Unix);
        Self {
            host,
            require_control_token,
            runtime_service,
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/agents", get(list_agents))
        .route("/agents/:agent_id/enqueue", post(enqueue))
        .route("/agents/:agent_id/status", get(status))
        .route("/agents/:agent_id/briefs", get(briefs))
        .route("/agents/:agent_id/state", get(agent_state))
        .route("/agents/:agent_id/events", get(events))
        .route("/agents/:agent_id/transcript", get(transcript))
        .route("/agents/:agent_id/tasks", get(tasks))
        .route("/agents/:agent_id/worktree-summary", get(worktree_summary))
        .route("/agents/:agent_id/timers", get(timers))
        .route("/control/agents/:agent_id/tasks", post(create_command_task))
        .route(
            "/control/agents/:agent_id/work-items",
            post(create_work_item),
        )
        .route("/control/agents/:agent_id/timers", post(create_timer))
        .route("/control/agents/:agent_id/create", post(create_agent))
        .route(
            "/control/agents/:agent_id/workspace/attach",
            post(attach_workspace),
        )
        .route(
            "/control/agents/:agent_id/workspace/exit",
            post(exit_workspace),
        )
        .route(
            "/control/agents/:agent_id/workspace/detach",
            post(detach_workspace),
        )
        .route("/control/agents/:agent_id/model", post(set_agent_model))
        .route(
            "/control/agents/:agent_id/model/clear",
            post(clear_agent_model),
        )
        .route("/control/agents/:agent_id/control", post(control))
        .route("/control/agents/:agent_id/prompt", post(control_prompt))
        .route(
            "/control/agents/:agent_id/operator-bindings",
            post(create_operator_transport_binding),
        )
        .route(
            "/control/agents/:agent_id/operator-ingress",
            post(operator_ingress),
        )
        .route("/control/runtime/status", get(runtime_status))
        .route("/control/runtime/shutdown", post(runtime_shutdown))
        .route(
            "/control/agents/:agent_id/debug-prompt",
            post(control_debug_prompt),
        )
        .route("/control/agents/:agent_id/wake", post(control_wake))
        .route(
            "/callbacks/enqueue/:callback_token",
            post(callback_ingress_enqueue).layer(DefaultBodyLimit::max(CALLBACK_BODY_LIMIT_BYTES)),
        )
        .route(
            "/callbacks/wake/:callback_token",
            post(callback_ingress_wake).layer(DefaultBodyLimit::max(CALLBACK_BODY_LIMIT_BYTES)),
        )
        .route("/webhooks/generic/:agent_id", post(generic_webhook))
        .route("/enqueue", post(enqueue_default))
        .route("/status", get(status_default))
        .route("/briefs", get(briefs_default))
        .route("/state", get(state_default))
        .route("/events", get(events_default))
        .route("/transcript", get(transcript_default))
        .route("/worktree-summary", get(worktree_summary_default))
        .with_state(Arc::new(state))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnqueueRequest {
    pub kind: Option<MessageKind>,
    pub priority: Option<Priority>,
    pub trust: Option<TrustLevel>,
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
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    limit: Option<usize>,
    since: Option<String>,
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
    transcript_tail: Vec<TranscriptEntry>,
    briefs_tail: Vec<BriefRecord>,
    timers: Vec<TimerRecord>,
    work_items: Vec<WorkItemRecord>,
    work_plan: Option<WorkPlanSnapshot>,
    waiting_intents: Vec<WaitingIntentRecord>,
    external_triggers: Vec<ExternalTriggerStateSnapshot>,
    operator_notifications: Vec<OperatorNotificationRecord>,
    workspace: StateWorkspaceSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<ExecutionSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brief: Option<BriefRecord>,
    cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct StreamEventEnvelope {
    id: String,
    seq: u64,
    ts: chrono::DateTime<Utc>,
    agent_id: String,
    #[serde(rename = "type")]
    event_type: String,
    payload: Value,
}

struct EventStreamState {
    runtime: crate::runtime::RuntimeHandle,
    runtime_id: String,
    event_window_limit: usize,
    event_marker: FileActivityMarker,
    last_seen_cursor: Option<String>,
    next_seq: u64,
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
    pub continue_on_result: Option<bool>,
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateWorkItemRequest {
    pub delivery_target: String,
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateTimerRequest {
    pub duration_ms: u64,
    pub interval_ms: Option<u64>,
    pub summary: Option<String>,
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ControlRequest {
    pub action: ControlAction,
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttachWorkspaceRequest {
    pub path: String,
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExitWorkspaceRequest {
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetachWorkspaceRequest {
    pub workspace_id: String,
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SetAgentModelRequest {
    pub model: String,
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ClearAgentModelRequest {
    pub trust: Option<TrustLevel>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateAgentRequest {
    pub trust: Option<TrustLevel>,
    pub template: Option<String>,
}

pub async fn root(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    Ok(Json(json!({
        "ok": true,
        "default_agent": state.host.config().default_agent_id,
    })))
}

pub async fn list_agents(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let agents = state.host.list_agents().await.map_err(error_response)?;
    Ok(Json(agents))
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
        .map_err(error_response)?
        .into_iter()
        .filter_map(|agent| agent.last_runtime_failure)
        .max_by(|left, right| left.occurred_at.cmp(&right.occurred_at));
    let agent_summaries = state.host.list_agents().await.map_err(error_response)?;
    let config = state.host.config();
    let startup_surface = crate::daemon::RuntimeStartupSurface {
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        workspace_dir: config.workspace_dir.clone(),
        default_agent_id: config.default_agent_id.clone(),
        control_token_configured: config.control_token.is_some(),
        control_auth_mode: config.control_auth_mode.into(),
    };
    let runtime_surface = RuntimeConfigSurface::new(config);
    let agent_model_overrides = agent_summaries
        .into_iter()
        .map(|summary| crate::daemon::RuntimeAgentOverrideSummary {
            agent_id: summary.identity.agent_id,
            override_model: summary.model.override_model.map(|model| model.as_string()),
        })
        .collect::<Vec<_>>();
    Ok(Json(runtime_service.status_response(
        activity,
        last_failure,
        startup_surface,
        runtime_surface,
        agent_model_overrides,
    )))
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
    Json(request): Json<EnqueueRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let agent_id = state.host.config().default_agent_id.clone();
    enqueue_internal(state, agent_id, request, EnqueueIngress::Public).await
}

pub async fn enqueue(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<EnqueueRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
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
    let trust = match ingress {
        EnqueueIngress::Public => {
            if request.trust.is_some() {
                return Err(forbidden("public enqueue may not override trust"));
            }
            default_trust_for_origin(&origin)
        }
        EnqueueIngress::Trusted { .. } => request
            .trust
            .unwrap_or_else(|| default_trust_for_origin(&origin)),
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
        trust,
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
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    status(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
    )
    .await
}

pub async fn status(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent = runtime.agent_summary().await.map_err(error_response)?;
    Ok(Json(agent))
}

pub async fn state_default(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    agent_state(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
    )
    .await
}

pub async fn agent_state(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let cursor = runtime
        .recent_events(1)
        .await
        .map_err(error_response)?
        .into_iter()
        .next()
        .map(|event| event.id);
    let agent = runtime.agent_summary().await.map_err(error_response)?;
    let tasks = runtime.recent_tasks(50).await.map_err(error_response)?;
    let transcript_tail = runtime
        .recent_transcript(100)
        .await
        .map_err(error_response)?;
    let briefs_tail = runtime.recent_briefs(24).await.map_err(error_response)?;
    let brief = briefs_tail.last().cloned();
    let timers = runtime.recent_timers(50).await.map_err(error_response)?;
    let mut work_items = runtime.latest_work_items().await.map_err(error_response)?;
    sort_state_work_items(&mut work_items);
    let work_plan = match select_state_work_plan_target(
        agent.agent.current_turn_work_item_id.as_deref(),
        &work_items,
    ) {
        Some(work_item_id) => runtime
            .latest_work_plan(&work_item_id)
            .await
            .map_err(error_response)?,
        None => None,
    };
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
    Ok(Json(AgentStateSnapshot {
        agent,
        session,
        tasks,
        transcript_tail,
        briefs_tail,
        timers,
        work_items,
        work_plan,
        waiting_intents,
        external_triggers,
        operator_notifications,
        execution: Some(execution),
        workspace,
        brief,
        cursor,
    }))
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

fn state_work_item_rank(item: &WorkItemRecord) -> u8 {
    match item.state {
        WorkItemState::Open if item.blocked_by.is_none() => 0,
        WorkItemState::Open => 1,
        WorkItemState::Done => 2,
    }
}

fn select_state_work_plan_target(
    current_turn_work_item_id: Option<&str>,
    work_items: &[WorkItemRecord],
) -> Option<String> {
    let selected = current_turn_work_item_id
        .and_then(|id| {
            work_items
                .iter()
                .find(|item| item.id == id && item.state != WorkItemState::Done)
        })
        .or_else(|| {
            work_items
                .iter()
                .find(|item| item.state == WorkItemState::Open)
        })?;
    Some(selected.id.clone())
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
    use super::{select_state_work_plan_target, sort_state_work_items};
    use crate::types::{WorkItemRecord, WorkItemState};
    use chrono::{Duration, Utc};

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

        let completed = WorkItemRecord::new("default", "completed", WorkItemState::Done);
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
            .map(|item| item.delivery_target.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                active.delivery_target.as_str(),
                queued_early.delivery_target.as_str(),
                queued_late.delivery_target.as_str(),
                waiting.delivery_target.as_str(),
                "completed",
            ]
        );
    }

    #[test]
    fn state_work_plan_target_skips_completed_current_turn_binding() {
        let completed_id = "completed-bound".to_string();
        let queued_id = "queued-next".to_string();

        let mut completed =
            WorkItemRecord::new("default", "completed bound item", WorkItemState::Done);
        completed.id = completed_id.clone();

        let mut queued = WorkItemRecord::new("default", "queued next item", WorkItemState::Open);
        queued.id = queued_id.clone();

        let work_items = vec![completed, queued];

        assert_eq!(
            select_state_work_plan_target(Some(completed_id.as_str()), &work_items),
            Some(queued_id)
        );
    }
}

pub async fn briefs_default(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    briefs(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        Query(query),
    )
    .await
}

pub async fn briefs(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
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

pub async fn events_default(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    events(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        headers,
        Query(query),
    )
    .await
}

pub async fn transcript_default(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    transcript(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
        Query(query),
    )
    .await
}

pub async fn worktree_summary_default(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    worktree_summary(
        Path(state.host.config().default_agent_id.clone()),
        State(state),
    )
    .await
}

pub async fn events(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let event_window_limit = query
        .limit
        .unwrap_or(DEFAULT_EVENT_STREAM_WINDOW)
        .clamp(1, MAX_EVENT_STREAM_WINDOW);
    let cursor = query
        .since
        .or_else(|| {
            headers
                .get("last-event-id")
                .and_then(|value| value.to_str().ok())
                .map(|value| value.to_string())
        })
        .filter(|value| !value.is_empty());
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let initial_event_marker = runtime
        .storage()
        .poll_activity_marker()
        .map_err(error_response)?
        .events;
    let events = runtime
        .recent_events(event_window_limit)
        .await
        .map_err(error_response)?;
    let buffered = initial_buffered_events(&events, cursor.as_deref())?;
    let last_seen_cursor = cursor.or_else(|| events.last().map(|event| event.id.clone()));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
    tokio::spawn(async move {
        let mut state = EventStreamState {
            runtime,
            runtime_id: agent_id,
            event_window_limit,
            event_marker: initial_event_marker,
            last_seen_cursor,
            next_seq: 0,
            buffered,
        };
        loop {
            if let Some(event) = state.buffered.pop_front() {
                let envelope = stream_event_envelope(state.next_seq, &state.runtime_id, &event);
                state.next_seq += 1;
                state.last_seen_cursor = Some(event.id.clone());
                let payload = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
                if tx
                    .send(Ok(Event::default()
                        .id(envelope.id)
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
            let latest_events: Vec<AuditEvent> =
                match state.runtime.recent_events(state.event_window_limit).await {
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
                Err(cursor) => {
                    error!("event stream cursor fell out of replay window: {cursor}");
                    break;
                }
            }
            sleep(EVENT_STREAM_POLL_INTERVAL).await;
        }
    });
    let stream = ReceiverStream::new(rx);
    let keep_alive = KeepAlive::new()
        .interval(Duration::from_secs(15))
        .text("heartbeat");
    Ok(Sse::new(stream).keep_alive(keep_alive))
}

fn initial_buffered_events(
    events: &[AuditEvent],
    cursor: Option<&str>,
) -> std::result::Result<VecDeque<AuditEvent>, (StatusCode, Json<Value>)> {
    let start_index = if let Some(cursor) = cursor {
        match events.iter().position(|event| event.id == cursor) {
            Some(position) => position + 1,
            None => return Err(cursor_too_old(cursor.to_string())),
        }
    } else {
        events.len()
    };
    Ok(events.iter().skip(start_index).cloned().collect())
}

fn refresh_buffered_events(
    state: &mut EventStreamState,
    latest_events: Vec<AuditEvent>,
) -> std::result::Result<(), String> {
    let start_index = if let Some(cursor) = state.last_seen_cursor.as_deref() {
        latest_events
            .iter()
            .position(|event| event.id == cursor)
            .map(|position| position + 1)
            .ok_or_else(|| cursor.to_string())?
    } else {
        0
    };
    for event in latest_events.into_iter().skip(start_index) {
        state.buffered.push_back(event);
    }
    Ok(())
}

fn stream_event_envelope(seq: u64, agent_id: &str, event: &AuditEvent) -> StreamEventEnvelope {
    StreamEventEnvelope {
        id: event.id.clone(),
        seq,
        ts: event.created_at,
        agent_id: agent_id.to_string(),
        event_type: event.kind.clone(),
        payload: event.data.clone(),
    }
}

fn cursor_too_old(cursor: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(json!({
            "ok": false,
            "error": format!("cursor {cursor} is too old or not found"),
            "code": "cursor_too_old",
            "cursor": cursor,
        })),
    )
}

pub async fn transcript(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
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
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
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
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    Ok(Json(
        runtime
            .recent_tasks(query.limit.unwrap_or(50))
            .await
            .map_err(error_response)?,
    ))
}

pub async fn create_command_task(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateCommandTaskRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.trust;
    let effective_trust = provided_trust
        .clone()
        .unwrap_or(TrustLevel::TrustedOperator);
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
                continue_on_result: request.continue_on_result.unwrap_or(false),
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
    let provided_trust = request.trust;
    let delivery_target = request.delivery_target.trim().to_string();
    if delivery_target.is_empty() {
        return Err(bad_request("delivery_target must not be empty"));
    }
    let (runtime, record) = state
        .host
        .enqueue_public_work_item(&agent_id, delivery_target)
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

pub async fn timers(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
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

pub async fn create_timer(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateTimerRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.trust;
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

pub async fn create_agent(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.trust;
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
    let provided_trust = request.trust;
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

pub async fn attach_workspace(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AttachWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.trust;
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
    let provided_trust = request.trust;
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
    let provided_trust = request.trust;
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
    let admission_context = control_admission_context(&state);
    let provided_trust = request.trust;
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
        .set_model_override(model.clone())
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

pub async fn clear_agent_model(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ClearAgentModelRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| forbidden(err.to_string()))?;
    let admission_context = control_admission_context(&state);
    let provided_trust = request.trust;
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
            priority: Some(Priority::Normal),
            trust: Some(TrustLevel::TrustedOperator),
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
        priority: Priority::Normal,
        origin: MessageOrigin::Operator {
            actor_id: Some(actor_id),
        },
        trust: TrustLevel::TrustedOperator,
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
    let effective_trust = request.trust.clone().unwrap_or(TrustLevel::TrustedOperator);
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let boundary = current_boundary_metadata(&runtime)
        .await
        .map_err(error_response)?;
    let dump = runtime
        .preview_prompt(request.text.clone(), effective_trust.clone())
        .await
        .map_err(error_response)?
        .render_dump();
    runtime
        .append_audit_event(
            "debug_prompt_requested",
            json!({
                "target_agent_id": agent_id,
                "admission_context": admission_context,
                "effective_trust": effective_trust,
                "boundary": boundary,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
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
                    "agent {} is stopped; wake does not override stopped; resume first",
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
            source: request.source,
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
    let payload = build_callback_delivery_payload(&headers, body).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
    })?;
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
            Self::Wake => crate::types::CallbackDeliveryMode::WakeOnly,
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
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    enqueue_internal(
        state,
        agent_id,
        EnqueueRequest {
            kind: Some(MessageKind::WebhookEvent),
            priority: Some(Priority::Normal),
            trust: Some(TrustLevel::TrustedIntegration),
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
        .unwrap_or_else(|| format!("{prefix}_{}", Uuid::new_v4().simple()))
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
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "ok": false,
            "error": reason.into(),
        })),
    )
}

fn bad_request(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "ok": false,
            "error": reason.into(),
        })),
    )
}

fn service_unavailable(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "ok": false,
            "error": reason.into(),
        })),
    )
}

fn not_found(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "ok": false,
            "error": reason.into(),
        })),
    )
}

fn stopped_agent_conflict(
    reason: impl Into<String>,
    agent_id: impl Into<String>,
) -> (StatusCode, Json<Value>) {
    let agent_id = agent_id.into();
    let hint = format!(
        "resume with `holon control resume --agent {}` or POST /control/agents/{}/control with JSON body {{\"action\":\"resume\"}}",
        agent_id, agent_id
    );
    (
        StatusCode::CONFLICT,
        Json(json!({
            "ok": false,
            "error": reason.into(),
            "code": "agent_stopped",
            "agent_id": agent_id,
            "hint": hint,
        })),
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
            format!("agent {} is stopped; resume first", agent_id),
            agent_id,
        ),
        PublicAgentError::Runtime(error) => error_response(error),
    }
}

fn error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "ok": false,
            "error": error.to_string(),
        })),
    )
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
            accepted = listener.accept() => accepted?,
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
