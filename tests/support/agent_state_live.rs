use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, ensure, Result};
use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, StatusCode},
    response::Response,
    Router,
};
use futures_util::StreamExt;
use holon::{
    config::{
        AppConfig, ModelRouteCapability, ModelRouteRef, ProviderRuntimeConfig,
        ProviderTransportKind, RuntimeModelCatalog,
    },
    context::ContextConfig,
    host::RuntimeHost,
    provider::{
        AgentProvider, AnthropicProvider, ConversationMessage, ModelBlock, ProviderTurnRequest,
        ProviderTurnResponse,
    },
    runtime::RuntimeHandle,
    types::{
        AdmissionContext, AuthorityClass, BriefKind, EpisodeBoundaryReason, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        QueueEntryStatus, TurnRecord, WorkItemPlanStatus,
    },
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use url::Url;

const SEND_LIMIT: usize = 12;
const BODY_LIMIT: usize = 128 * 1024;
const OUTPUT_LIMIT: u64 = 512;
const ESTIMATED_INPUT_LIMIT: u64 = 196_608;
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const AGENT: &str = "acceptance";
const SUITE_VERSION: &str = "agent-state-live/v1";
const PROJECT: &str = "ORCHID_3478";

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_label(value: &str) -> Result<String> {
    ensure!(
        !value.is_empty()
            && value.len() <= 200
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._/@:+-".contains(&c)),
        "route metadata contains unsupported characters"
    );
    Ok(value.to_owned())
}

fn safe_url(url: &Url) -> String {
    // The selected route is also validated to have no userinfo/query/fragment.
    // Keep the origin only: even a path can contain credential material.
    url.origin().ascii_serialization()
}

struct SelectedRoute {
    route: ModelRouteRef,
    upstream: ProviderRuntimeConfig,
    template: AppConfig,
    supports_reasoning: bool,
}

impl SelectedRoute {
    fn from_config(source: AppConfig, route: ModelRouteRef) -> Result<Self> {
        safe_label(&route.as_string())?;
        let resolved = RuntimeModelCatalog::from_config(&source)
            .resolve_explicit_model_route(
                &ContextConfig::default(),
                &route,
                ModelRouteCapability::Turn,
            )
            .ok_or_else(|| anyhow!("configured acceptance route cannot serve turns"))?;
        ensure!(
            resolved.provider_config().transport == ProviderTransportKind::AnthropicMessages,
            "acceptance v1 supports a configured anthropic_messages route"
        );
        ensure!(
            resolved.provider_config().has_configured_credential(),
            "acceptance route has no local credential material"
        );
        let upstream = resolved.provider_config().clone();
        let mut template = crate::support::TestConfigBuilder::new()
            .build()
            .config()
            .clone();
        template.default_agent_id = AGENT.into();
        template.default_model = resolved.route_ref.clone();
        template.fallback_models.clear();
        template.disable_provider_fallback = true;
        template.runtime_max_output_tokens = OUTPUT_LIMIT as u32;
        template.validated_model_overrides = source.validated_model_overrides.clone();
        template.validated_unknown_model_fallback = source.validated_unknown_model_fallback.clone();
        template.model_discovery_cache = source.model_discovery_cache.clone();
        template.stored_config.decision.enabled = Some(false);
        template.stored_config.model.default = Some(resolved.route_ref.as_string());
        template.stored_config.runtime.disable_provider_fallback = Some(true);
        template.stored_config.runtime.max_output_tokens = Some(OUTPUT_LIMIT as u32);
        template.prompt_budget_estimated_tokens = 8192;
        template.compaction_trigger_messages = 100;
        template.compaction_trigger_estimated_tokens = 100_000;
        template.context_window_messages = 12;
        template.context_window_briefs = 12;
        Ok(Self {
            route: resolved.route_ref,
            upstream,
            template,
            supports_reasoning: resolved.policy.capabilities.supports_reasoning,
        })
    }
}

#[derive(Clone, Serialize)]
struct WireObservation {
    send: usize,
    case: String,
    request_bytes: usize,
    estimated_input_tokens: u64,
    output_limit: u64,
    status: Option<u16>,
    elapsed_ms: Option<u128>,
    failure: Option<&'static str>,
}

#[derive(Default)]
struct Budget {
    wires: Vec<WireObservation>,
    estimated_input_tokens: u64,
    reported_input_tokens: u64,
    reported_output_tokens: u64,
    stop_reason: Option<&'static str>,
    case: String,
}

struct GatewayState {
    upstream: Url,
    expected_path: String,
    credential: String,
    client: reqwest::Client,
    budget: Mutex<Budget>,
}

struct Gateway {
    local_base: String,
    state: Arc<GatewayState>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl Gateway {
    async fn start(base_url: &str, credential: String, allow_loopback: bool) -> Result<Self> {
        let mut upstream =
            Url::parse(base_url).map_err(|_| anyhow!("invalid acceptance endpoint"))?;
        ensure!(
            upstream.username().is_empty()
                && upstream.password().is_none()
                && upstream.query().is_none()
                && upstream.fragment().is_none(),
            "acceptance endpoint must not embed credentials or query fields"
        );
        let loopback = upstream.host_str().is_some_and(|host| {
            host == "127.0.0.1" || host == "localhost" || host == "[::1]" || host == "::1"
        });
        ensure!(
            upstream.scheme() == "https"
                || (allow_loopback && loopback && upstream.scheme() == "http"),
            "live acceptance requires an HTTPS upstream"
        );
        let prefix = upstream.path().trim_end_matches('/').to_owned();
        upstream.set_path(&format!("{prefix}/v1/messages"));
        let expected_path = "/v1/messages".to_owned();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| anyhow!("cannot create acceptance forwarding client"))?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_base = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(GatewayState {
            upstream,
            expected_path,
            credential,
            client,
            budget: Mutex::new(Budget::default()),
        });
        let app = Router::new().fallback(forward).with_state(state.clone());
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
        });
        Ok(Self {
            local_base,
            state,
            shutdown: Some(tx),
            task: Some(task),
        })
    }

    fn set_case(&self, case: &str) {
        self.state.budget.lock().unwrap().case = case.into();
    }

    fn close(&self) {
        self.state
            .budget
            .lock()
            .unwrap()
            .stop_reason
            .get_or_insert("suite_closed");
    }

    async fn shutdown(&mut self) {
        self.close();
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(Duration::from_secs(65), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn reject(status: StatusCode, code: &'static str) -> Response {
    let mut response = Response::new(Body::from(
        json!({"error":{"type":"acceptance_guard","message":code}}).to_string(),
    ));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response
}

async fn forward(State(state): State<Arc<GatewayState>>, request: Request) -> Response {
    if request.method() != axum::http::Method::POST
        || request.uri().path() != state.expected_path
        || request.uri().query().is_some()
    {
        return reject(StatusCode::BAD_REQUEST, "unexpected_endpoint");
    }
    let headers = request.headers().clone();
    let body = match to_bytes(request.into_body(), BODY_LIMIT).await {
        Ok(body) => body,
        Err(_) => return reject(StatusCode::PAYLOAD_TOO_LARGE, "wire_body_limit"),
    };
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return reject(StatusCode::BAD_REQUEST, "invalid_wire_json"),
    };
    let output = value.get("max_tokens").and_then(Value::as_u64).unwrap_or(0);
    if output == 0 || output > OUTPUT_LIMIT {
        return reject(StatusCode::BAD_REQUEST, "output_limit");
    }
    // Operational estimate only; tokenizers and Unicode can change this ratio.
    let estimated = body.len().div_ceil(4) as u64;
    let index = {
        let mut budget = state.budget.lock().unwrap();
        if budget.stop_reason.is_some()
            || budget.wires.len() >= SEND_LIMIT
            || budget.estimated_input_tokens.saturating_add(estimated) > ESTIMATED_INPUT_LIMIT
            || budget.reported_input_tokens >= ESTIMATED_INPUT_LIMIT
            || budget.reported_output_tokens >= OUTPUT_LIMIT * SEND_LIMIT as u64
        {
            budget.stop_reason.get_or_insert("send_or_token_budget");
            return reject(StatusCode::TOO_MANY_REQUESTS, "send_or_token_budget");
        }
        let index = budget.wires.len();
        let case = budget.case.clone();
        budget.estimated_input_tokens += estimated;
        budget.wires.push(WireObservation {
            send: index + 1,
            case,
            request_bytes: body.len(),
            estimated_input_tokens: estimated,
            output_limit: output,
            status: None,
            elapsed_ms: None,
            failure: None,
        });
        index
    };
    let started = Instant::now();
    // The actual credential is introduced only here. It never enters the local
    // provider config, request body, trace, diagnostics, or report.
    let mut outgoing = state
        .client
        .post(state.upstream.clone())
        .bearer_auth(&state.credential)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body);
    for name in ["anthropic-version", "anthropic-beta"] {
        if let Some(value) = headers.get(name) {
            outgoing = outgoing.header(name, value);
        }
    }
    let upstream = match outgoing.send().await {
        Ok(response) => response,
        Err(_) => {
            finish_wire(&state, index, started, None, Some("upstream_send_unknown"));
            state.budget.lock().unwrap().stop_reason = Some("upstream_send_unknown");
            return reject(StatusCode::BAD_GATEWAY, "upstream_send_unknown");
        }
    };
    let status = upstream.status();
    if !status.is_success() {
        finish_wire(
            &state,
            index,
            started,
            Some(status.as_u16()),
            Some("upstream_status"),
        );
        state.budget.lock().unwrap().stop_reason = Some("upstream_status");
        // Never return upstream error text/headers, which can echo credentials.
        return reject(StatusCode::BAD_GATEWAY, "upstream_status");
    }
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let mut bytes = Vec::new();
    let mut stream = upstream.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                finish_wire(
                    &state,
                    index,
                    started,
                    Some(status.as_u16()),
                    Some("upstream_body_unknown"),
                );
                state.budget.lock().unwrap().stop_reason = Some("upstream_body_unknown");
                return reject(StatusCode::BAD_GATEWAY, "upstream_body_unknown");
            }
        };
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            finish_wire(
                &state,
                index,
                started,
                Some(status.as_u16()),
                Some("response_limit"),
            );
            state.budget.lock().unwrap().stop_reason = Some("response_limit");
            return reject(StatusCode::BAD_GATEWAY, "response_limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    finish_wire(&state, index, started, Some(status.as_u16()), None);
    let mut response = Response::new(Body::from(bytes));
    if let Some(value) = content_type {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

fn finish_wire(
    state: &GatewayState,
    index: usize,
    started: Instant,
    status: Option<u16>,
    failure: Option<&'static str>,
) {
    let mut budget = state.budget.lock().unwrap();
    let wire = &mut budget.wires[index];
    wire.status = status;
    wire.elapsed_ms = Some(started.elapsed().as_millis());
    wire.failure = failure;
}

#[derive(Clone, Serialize)]
struct RoundObservation {
    round: usize,
    request_fingerprint: String,
    has_project_anchor: bool,
    has_episode_fact: bool,
    tool_result_ids: Vec<String>,
    output_text: Option<String>,
    tool_names: Vec<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_usage: Option<Value>,
    stable_prefix_fingerprint: Option<String>,
    elapsed_ms: u128,
    failure: Option<&'static str>,
}

struct CheckedProvider {
    inner: Arc<dyn AgentProvider>,
    gateway: Arc<GatewayState>,
    allow_tool: bool,
    tool_calls: AtomicUsize,
    failed: AtomicBool,
    rounds: Mutex<Vec<RoundObservation>>,
}

#[async_trait]
impl AgentProvider for CheckedProvider {
    async fn complete_turn(
        &self,
        mut request: ProviderTurnRequest,
    ) -> Result<ProviderTurnResponse> {
        ensure!(
            !self.failed.load(Ordering::Acquire),
            "acceptance case has already failed"
        );
        request
            .tools
            .retain(|tool| self.allow_tool && tool.name == "GetAgent");
        request.native_web_search = None;
        let conversation = request.conversation.iter().map(|message| match message {
            ConversationMessage::UserText(text) => json!({"user_text":text}),
            ConversationMessage::UserBlocks(blocks) => json!({"user_blocks":blocks}),
            ConversationMessage::UserImage { prompt, media_type, data_base64, optional_history } =>
                json!({"user_image":{"prompt":prompt,"media_type":media_type,"data_base64":data_base64,"optional_history":optional_history}}),
            ConversationMessage::AssistantBlocks(blocks) => json!({"assistant_blocks":blocks}),
            ConversationMessage::UserToolResults(results) => json!({"tool_results":results}),
        }).collect::<Vec<_>>();
        let serialized = serde_json::to_vec(&json!({
            "prompt_frame":request.prompt_frame,
            "conversation":conversation,
            "tools":request.tools,
        }))?;
        let text = String::from_utf8_lossy(&serialized);
        let tool_result_ids = request
            .conversation
            .iter()
            .filter_map(|message| match message {
                ConversationMessage::UserToolResults(results) => {
                    Some(results.iter().map(|r| r.tool_use_id.clone()))
                }
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        let started = Instant::now();
        let result = self.inner.complete_turn(request).await;
        let mut observation = RoundObservation {
            round: 0,
            request_fingerprint: digest(&serialized),
            has_project_anchor: text.contains(PROJECT),
            has_episode_fact: text.contains("MAPLE"),
            tool_result_ids,
            output_text: None,
            tool_names: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            cache_usage: None,
            stable_prefix_fingerprint: None,
            elapsed_ms: started.elapsed().as_millis(),
            failure: None,
        };
        let result = match result {
            Ok(response) => {
                let mut unsafe_tool = false;
                for block in &response.blocks {
                    match block {
                        ModelBlock::ToolUse { name, input, .. } => {
                            observation.tool_names.push(name.clone());
                            if !self.allow_tool
                                || name != "GetAgent"
                                || input
                                    .get("agent_id")
                                    .and_then(Value::as_str)
                                    .is_some_and(|id| id != AGENT)
                                || self.tool_calls.fetch_add(1, Ordering::AcqRel) != 0
                            {
                                unsafe_tool = true;
                            }
                        }
                        ModelBlock::ProviderToolUse { .. }
                        | ModelBlock::ProviderToolResult { .. } => unsafe_tool = true,
                        _ => {}
                    }
                }
                observation.output_text = Some(
                    response
                        .blocks
                        .iter()
                        .filter_map(|block| match block {
                            ModelBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                if response.input_tokens > 0 || response.output_tokens > 0 {
                    observation.input_tokens = Some(response.input_tokens);
                    observation.output_tokens = Some(response.output_tokens);
                    let mut budget = self.gateway.budget.lock().unwrap();
                    budget.reported_input_tokens = budget
                        .reported_input_tokens
                        .saturating_add(response.input_tokens);
                    budget.reported_output_tokens = budget
                        .reported_output_tokens
                        .saturating_add(response.output_tokens);
                } else {
                    self.gateway.budget.lock().unwrap().stop_reason = Some("usage_not_reported");
                }
                observation.cache_usage = response
                    .cache_usage
                    .as_ref()
                    .and_then(|usage| serde_json::to_value(usage).ok());
                observation.stable_prefix_fingerprint = response
                    .request_diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.stable_prefix.as_ref())
                    .map(|prefix| prefix.stable_prefix_fingerprint.clone());
                if unsafe_tool || response.output_tokens > OUTPUT_LIMIT {
                    observation.failure = Some("tool_or_output_policy");
                    Err(anyhow!("acceptance provider violated tool/output policy"))
                } else {
                    Ok(response)
                }
            }
            Err(_) => {
                observation.failure = Some("provider_failed");
                Err(anyhow!(
                    "acceptance provider failed; safe details are in the report"
                ))
            }
        };
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
            self.gateway
                .budget
                .lock()
                .unwrap()
                .stop_reason
                .get_or_insert("provider_failed");
        }
        let mut rounds = self.rounds.lock().unwrap();
        observation.round = rounds.len() + 1;
        rounds.push(observation);
        result
    }

    fn prompt_capabilities(&self) -> Vec<holon::provider::ProviderPromptCapability> {
        self.inner.prompt_capabilities()
    }

    fn prompt_tool_specs(&self, tools: &[holon::tool::ToolSpec]) -> Vec<holon::tool::ToolSpec> {
        self.inner
            .prompt_tool_specs(tools)
            .into_iter()
            .filter(|tool| self.allow_tool && tool.name == "GetAgent")
            .collect()
    }
}

struct Fixture {
    root: TempDir,
    config: AppConfig,
    host: Option<RuntimeHost>,
    runtime: RuntimeHandle,
    provider: Arc<CheckedProvider>,
}

impl Fixture {
    async fn new(selection: &SelectedRoute, gateway: &Gateway, case: &str) -> Result<Self> {
        gateway.set_case(case);
        let root = tempfile::tempdir()?;
        let mut config = selection.template.clone();
        config.home_dir = root.path().join("home");
        config.data_dir = config.home_dir.clone();
        config.user_home_dir = Some(root.path().join("user"));
        config.config_file_path = config.home_dir.join("config.json");
        config.socket_path = config.home_dir.join("run/holon.sock");
        config.workspace_dir = root.path().join("workspace");
        config.control_token = None;
        config.http_addr = "127.0.0.1:0".into();
        config.callback_base_url = "http://127.0.0.1:0".into();
        std::fs::create_dir_all(&config.workspace_dir)?;
        let agent_home = config.agent_root_dir().join(AGENT);
        std::fs::create_dir_all(&agent_home)?;
        std::fs::write(agent_home.join("AGENTS.md"), "This is an isolated acceptance fixture. Follow the latest operator's requested output format. Do not use tools unless explicitly requested. Never create background work or change work items. The harness supplies work scope.\n")?;
        let mut provider_config = selection.upstream.clone();
        provider_config.base_url = gateway.local_base.clone();
        provider_config.credential = Some("acceptance-local-placeholder".into());
        provider_config.credential_store_path = None;
        provider_config.codex_home = None;
        provider_config.reasoning_effort = None;
        provider_config.builtin_web_search = None;
        provider_config.context_management.enabled = false;
        config.providers.clear();
        config
            .providers
            .insert(provider_config.id.clone(), provider_config.clone());
        let raw = AnthropicProvider::from_runtime_config(
            &provider_config,
            &selection.route.model,
            OUTPUT_LIMIT as u32,
            root.path(),
            selection.supports_reasoning,
        )
        .map_err(|_| anyhow!("cannot instantiate acceptance transport"))?;
        let provider = Arc::new(CheckedProvider {
            inner: Arc::new(raw),
            gateway: gateway.state.clone(),
            allow_tool: case == "L3",
            tool_calls: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            rounds: Mutex::new(Vec::new()),
        });
        let host = RuntimeHost::new_with_provider(config.clone(), provider.clone())?;
        let runtime = host.default_runtime().await?;
        Ok(Self {
            root,
            config,
            host: Some(host),
            runtime,
            provider,
        })
    }

    async fn send(
        &self,
        step: &str,
        prompt: &str,
        work_item: Option<&str>,
    ) -> Result<(TurnRecord, String)> {
        let mut message = MessageEnvelope::new(
            AGENT,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("acceptance-fixture".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: format!("[HOLON_LIVE/{step}] {prompt}"),
            },
        )
        .with_admission(
            MessageDeliverySurface::CliPrompt,
            AdmissionContext::LocalProcess,
        );
        message.id = format!("acceptance-{step}");
        message.work_item_id = work_item.map(str::to_owned);
        let admitted = self.runtime.enqueue(message).await?;
        let message_id = admitted.id.clone();
        tokio::time::timeout(Duration::from_secs(150), async {
            loop {
                ensure!(
                    !self.provider.failed.load(Ordering::Acquire),
                    "acceptance provider failed"
                );
                let storage = self.runtime.storage();
                let queue = storage.latest_queue_entries()?;
                if queue.iter().any(|entry| {
                    entry.message_id == message_id && entry.status == QueueEntryStatus::Processed
                }) {
                    let matches = storage
                        .read_recent_turns(100)?
                        .into_iter()
                        .filter(|turn| {
                            turn.input_message_ids.contains(&message_id) && turn.terminal.is_some()
                        })
                        .collect::<Vec<_>>();
                    ensure!(
                        matches.len() == 1,
                        "accepted input must have exactly one terminal actual turn"
                    );
                    let turn = matches.into_iter().next().unwrap();
                    let briefs = storage.read_briefs_for_turn(&turn.turn_id)?;
                    let text = briefs
                        .iter()
                        .filter(|brief| brief.kind == BriefKind::Result)
                        .map(|brief| brief.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    ensure!(
                        !text.trim().is_empty(),
                        "terminal turn must publish a durable result brief"
                    );
                    let persisted = storage
                        .read_message_by_id(&message_id)?
                        .ok_or_else(|| anyhow!("admitted message disappeared"))?;
                    ensure!(
                        persisted.authority_class == AuthorityClass::OperatorInstruction
                            && matches!(persisted.origin, MessageOrigin::Operator { .. }),
                        "operator source authority changed"
                    );
                    return Ok((turn, text));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("acceptance turn deadline exceeded"))?
    }

    async fn restart(&mut self) -> Result<()> {
        if let Some(host) = self.host.as_ref() {
            host.shutdown().await?;
        }
        self.host = Some(RuntimeHost::new_with_provider(
            self.config.clone(),
            self.provider.clone(),
        )?);
        self.runtime = self.host.as_ref().unwrap().default_runtime().await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(host) = self.host.as_ref() {
            host.shutdown().await?;
        }
        self.host = None;
        // root stays alive until after the Host/runtime handles have drained.
        ensure!(
            self.root.path().exists(),
            "fixture root released before shutdown"
        );
        Ok(())
    }
}

#[derive(Serialize)]
struct CaseReport {
    case: String,
    passed: bool,
    failure: Option<&'static str>,
    checks: BTreeMap<String, Value>,
    rounds: Vec<RoundObservation>,
}

fn check(report: &mut CaseReport, name: &str, passed: bool) -> Result<()> {
    report.checks.insert(name.into(), json!(passed));
    ensure!(passed, "acceptance functional assertion failed");
    Ok(())
}

fn code_reply(text: &str, expected: &str) -> bool {
    let text = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value.get("code").and_then(Value::as_str).map(str::to_owned))
        .as_deref()
        == Some(expected)
}

async fn run_case(fixture: &mut Fixture, report: &mut CaseReport) -> Result<()> {
    match report.case.as_str() {
        "L1" => {
            let (first, text) = fixture.send("l1-initial", "Remember PROJECT_CODE=ORCHID_3478. Return only JSON {\"code\":\"ORCHID_3478\"}. Do not call tools.", None).await?;
            check(report, "initial_fact_answer", code_reply(&text, PROJECT))?;
            let (second, text) = fixture.send("l1-followup", "What PROJECT_CODE did I give you? Return only a JSON object with the code field. Do not call tools.", None).await?;
            check(report, "remembered_fact_answer", code_reply(&text, PROJECT))?;
            check(
                report,
                "distinct_actual_turns",
                first.turn_id != second.turn_id,
            )?;
            let anchor = fixture
                .provider
                .rounds
                .lock()
                .unwrap()
                .last()
                .is_some_and(|round| round.has_project_anchor);
            check(report, "followup_retains_operator_anchor", anchor)?;
        }
        "L2" => {
            let stable = "This synthetic ledger is read-only. The required reply is CACHE_OK. Preserve the ledger facts and do not invoke any tools. ".repeat(48);
            for step in ["l2-cache-first", "l2-cache-second"] {
                let (_, text) = fixture
                    .send(
                        step,
                        &format!("{stable}\nReply with exactly CACHE_OK."),
                        None,
                    )
                    .await?;
                check(report, step, text.trim() == "CACHE_OK")?;
            }
            report
                .checks
                .insert("cache_hit_required".into(), json!(false));
        }
        "L3" => {
            let (turn, text) = fixture.send("l3-tool", "Call GetAgent exactly once for this agent. After its successful result, reply with exactly TOOL_OK. No other tools.", None).await?;
            check(report, "tool_answer", text.trim() == "TOOL_OK")?;
            let tools = fixture
                .runtime
                .storage()
                .read_tool_executions_for_turn(&turn.turn_id)?;
            check(
                report,
                "one_durable_get_agent",
                tools.len() == 1
                    && tools[0].tool_name == "GetAgent"
                    && tools[0].status == holon::types::ToolExecutionStatus::Success,
            )?;
            let continuity = fixture
                .provider
                .rounds
                .lock()
                .unwrap()
                .last()
                .is_some_and(|round| !round.tool_result_ids.is_empty());
            check(report, "continuation_received_tool_result", continuity)?;
        }
        "L4" => {
            // NeedsInput prevents autonomous work ticks while the fixture sets
            // the scope. The two operator messages still use canonical admission.
            let a = fixture
                .runtime
                .create_work_item(
                    "Episode A: EPISODE_FACT=MAPLE".into(),
                    Some(WorkItemPlanStatus::NeedsInput),
                    Some("Wait for operator input. The fixed episode fact is MAPLE.".into()),
                    Vec::new(),
                )
                .await?;
            let b = fixture
                .runtime
                .create_work_item(
                    "Episode B: verify previous fact".into(),
                    Some(WorkItemPlanStatus::NeedsInput),
                    Some("Wait for operator input; preserve the previous episode.".into()),
                    Vec::new(),
                )
                .await?;
            fixture.runtime.pick_work_item(a.id.clone()).await?;
            let (first, text) = fixture.send("l4-work-a", "Remember EPISODE_FACT=MAPLE. Reply with exactly MAPLE, without tools or work changes.", Some(&a.id)).await?;
            check(report, "work_a_answer", text.trim() == "MAPLE")?;
            fixture.runtime.pick_work_item(b.id.clone()).await?;
            let (second, text) = fixture.send("l4-work-b", "What EPISODE_FACT did I give in the previous work scope? Reply only with that word. Do not call tools or change work.", Some(&b.id)).await?;
            check(report, "work_b_remembers_fact", text.trim() == "MAPLE")?;
            check(
                report,
                "actual_work_bindings",
                first.current_work_item_id.as_deref() == Some(a.id.as_str())
                    && second.current_work_item_id.as_deref() == Some(b.id.as_str()),
            )?;
            let episodes = fixture
                .runtime
                .storage()
                .read_recent_context_episodes(100)?;
            let archived = episodes.iter().find(|episode| {
                episode.current_work_item_id.as_deref() == Some(a.id.as_str())
                    && episode.boundary_reason == EpisodeBoundaryReason::ActiveWorkSwitched
            });
            check(
                report,
                "active_work_switch_archived_episode_a",
                archived.is_some(),
            )?;
            let archived = archived.unwrap();
            let turns = fixture.runtime.storage().read_recent_turns(100)?;
            // The existing fallback builder permits an empty source set. Keep
            // that distinction visible instead of inventing provenance.
            report.checks.insert(
                "episode_sources_present".into(),
                json!(!archived.source_turn_ids.is_empty()),
            );
            check(
                report,
                "episode_sources_resolve_to_actual_turns",
                archived
                    .source_turn_ids
                    .iter()
                    .all(|source| turns.iter().any(|turn| &turn.turn_id == source)),
            )?;
            let state = fixture.runtime.agent_state().await?;
            check(
                report,
                "builder_epoch_advanced",
                state.working_memory.archived_episode_count >= 1
                    && state.working_memory.compression_epoch >= 1,
            )?;
            report
                .checks
                .insert("archived_source_refs".into(), json!(archived.source_refs));
        }
        "L5" => {
            let (first, text) = fixture
                .send(
                    "l5-before-restart",
                    "Remember RESTART_CODE=RIVER. Reply with exactly RIVER. No tools.",
                    None,
                )
                .await?;
            check(report, "before_restart_answer", text.trim() == "RIVER")?;
            let before = fixture
                .runtime
                .storage()
                .latest_agent_identities()?
                .into_iter()
                .find(|identity| identity.agent_id == AGENT)
                .ok_or_else(|| anyhow!("fixture identity missing"))?;
            fixture.restart().await?;
            let (second, text) = fixture.send("l5-after-restart", "What RESTART_CODE did I give before restart? Reply only with that word. No tools.", None).await?;
            check(
                report,
                "after_restart_remembers_fact",
                text.trim() == "RIVER",
            )?;
            let after = fixture
                .runtime
                .storage()
                .latest_agent_identities()?
                .into_iter()
                .find(|identity| identity.agent_id == AGENT)
                .ok_or_else(|| anyhow!("fixture identity missing after restart"))?;
            check(
                report,
                "identity_incarnation_unchanged",
                before.incarnation == after.incarnation,
            )?;
            let turns = fixture.runtime.storage().read_recent_turns(100)?;
            check(
                report,
                "old_turn_not_reexecuted",
                first.turn_id != second.turn_id
                    && turns
                        .iter()
                        .filter(|turn| {
                            turn.input_message_ids
                                .contains(&"acceptance-l5-before-restart".into())
                        })
                        .count()
                        == 1,
            )?;
            report.checks.insert(
                "restart_kind".into(),
                json!("new_host_same_test_process_same_durable_home"),
            );
        }
        _ => bail!("unknown acceptance case"),
    }
    check(
        report,
        "exactly_two_provider_rounds",
        fixture.provider.rounds.lock().unwrap().len() == 2,
    )?;
    Ok(())
}

fn manifest(route: &ModelRouteRef) -> Value {
    json!({"version":SUITE_VERSION,"seed":20261012,"route":route.as_string(),"cases":["L1_operator_fact","L2_cache_observation","L3_get_agent_tool","L4_episode_scope","L5_host_restart"],"normal_wire_sends":10,"max_wire_sends":SEND_LIMIT,"max_output_tokens":OUTPUT_LIMIT,"max_final_wire_body_bytes":BODY_LIMIT,"max_estimated_input_tokens":ESTIMATED_INPUT_LIMIT,"estimated_token_algorithm":"utf8_wire_bytes_div4_ceil/v1","reported_input_stop_line":ESTIMATED_INPUT_LIMIT,"input_stop_line_is_post_response_not_prebilling_guarantee":true,"allowed_tools":["GetAgent"],"timeout_per_wire_seconds":60,"timeout_per_arm_seconds":900,"restart_kind":"host_restart_not_process_crash","cache_hit_required":false})
}

async fn run_suite(selection: &SelectedRoute, gateway: &Gateway) -> Vec<CaseReport> {
    let mut reports = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(900);
    for case in ["L1", "L2", "L3", "L4", "L5"] {
        if gateway.state.budget.lock().unwrap().stop_reason.is_some() {
            break;
        }
        let mut report = CaseReport {
            case: case.into(),
            passed: false,
            failure: None,
            checks: BTreeMap::new(),
            rounds: Vec::new(),
        };
        match Fixture::new(selection, gateway, case).await {
            Ok(mut fixture) => {
                // Keep the fixture and its root outside the cancelled future.
                // Cleanup owns and joins the runtime after the case stops.
                match tokio::time::timeout_at(deadline, run_case(&mut fixture, &mut report)).await {
                    Ok(Ok(())) => (),
                    Ok(Err(_)) => report.failure = Some("functional_case_failed"),
                    Err(_) => report.failure = Some("suite_deadline"),
                }
                if report.failure.is_some() {
                    gateway.close();
                }
                if fixture.close().await.is_err() {
                    report.failure = Some("fixture_shutdown_failed");
                }
                report.rounds = fixture.provider.rounds.lock().unwrap().clone();
                report.passed = report.failure.is_none();
            }
            Err(_) => report.failure = Some("fixture_setup_failed"),
        }
        let passed = report.passed;
        reports.push(report);
        if !passed {
            break;
        }
    }
    reports
}

fn write_report(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub async fn run_live() -> Result<()> {
    let arm = std::env::var("HOLON_STATE_ACCEPTANCE_ARM").unwrap_or_default();
    ensure!(
        arm == "baseline" || arm == "new",
        "select baseline or new acceptance arm"
    );
    let revision = std::env::var("HOLON_STATE_ACCEPTANCE_REVISION").unwrap_or_default();
    ensure!(
        revision.len() == 40 && revision.bytes().all(|c| c.is_ascii_hexdigit()),
        "supply the exact implementation SHA"
    );
    let report_path = PathBuf::from(
        std::env::var_os("HOLON_STATE_ACCEPTANCE_REPORT")
            .ok_or_else(|| anyhow!("set an acceptance report path"))?,
    );
    ensure!(
        report_path.is_absolute() && !report_path.exists(),
        "report must be a new absolute path"
    );
    let source = AppConfig::load_for_config_inspection()
        .map_err(|_| anyhow!("cannot resolve configured acceptance route"))?;
    let route = match std::env::var("HOLON_STATE_ACCEPTANCE_ROUTE") {
        Ok(value) => ModelRouteRef::parse_compatible(&value)
            .map_err(|_| anyhow!("invalid acceptance route"))?,
        Err(_) => source.default_model.clone(),
    };
    let selection = SelectedRoute::from_config(source, route)?;
    let credential = selection
        .upstream
        .credential
        .clone()
        .ok_or_else(|| anyhow!("acceptance credential unavailable"))?;
    let mut gateway = Gateway::start(&selection.upstream.base_url, credential, false).await?;
    let started = Instant::now();
    let reports = run_suite(&selection, &gateway).await;
    gateway.shutdown().await;
    let timeout = reports
        .iter()
        .any(|case| case.failure == Some("suite_deadline"));
    let passed = !timeout && reports.len() == 5 && reports.iter().all(|case| case.passed);
    let budget = gateway.state.budget.lock().unwrap();
    let script = manifest(&selection.route);
    let rounds = reports
        .iter()
        .flat_map(|case| &case.rounds)
        .collect::<Vec<_>>();
    let usage_complete = rounds.len() == budget.wires.len()
        && rounds
            .iter()
            .all(|round| round.input_tokens.is_some() && round.output_tokens.is_some())
        && budget
            .wires
            .iter()
            .all(|wire| wire.status == Some(200) && wire.failure.is_none());
    let usage_measurement = json!({
        "status":if usage_complete { "complete_for_successful_script" } else { "partial_or_unavailable" },
        "reported_totals_complete":usage_complete,
        "scope":"provider-reported successful round usage; failed or unknown wire attempts are not assigned zero usage",
        "functional_pass_does_not_establish_cost_reduction":true,
    });
    let report = json!({"schema_version":1,"arm":arm,"implementation_sha":revision,"manifest":script,"manifest_sha256":digest(&serde_json::to_vec(&script)?),"runner_source_sha256":digest(include_bytes!("agent_state_live.rs")),"test_entry_source_sha256":digest(include_bytes!("../live_agent_state_acceptance.rs")),"provider":selection.route.provider.as_str(),"endpoint":selection.route.endpoint.as_str(),"model":selection.route.model,"upstream_origin":safe_url(&gateway.state.upstream),"local_credential_available":true,"remote_auth_verified_only_if_successful_wire":budget.wires.iter().any(|wire| wire.status==Some(200)),"passed":passed,"timed_out":timeout,"elapsed_ms":started.elapsed().as_millis(),"cases":reports,"wire_sends":budget.wires,"wire_count":budget.wires.len(),"estimated_input_tokens":budget.estimated_input_tokens,"reported_input_tokens":budget.reported_input_tokens,"reported_output_tokens":budget.reported_output_tokens,"usage_measurement":usage_measurement,"stop_reason":budget.stop_reason,"sql_statement_count":null,"file_read_count":null,"resident_or_pinned_bytes":null,"not_measured":["sql_statement_count","file_read_count","resident_or_pinned_bytes","precise_pre_send_billing_input_tokens","dollar_cost"],"live_latency_is_not_capacity_p95":true});
    write_report(&report_path, &report)?;
    ensure!(passed, "live agent-state acceptance did not complete all functional cases; inspect the safe JSON report");
    Ok(())
}

async fn mock_server(
    redirect: bool,
) -> Result<(String, Arc<AtomicUsize>, JoinHandle<std::io::Result<()>>)> {
    let calls = Arc::new(AtomicUsize::new(0));
    let capture = calls.clone();
    let app = Router::new().fallback(move |request: Request| {
        let capture = capture.clone();
        async move {
            capture.fetch_add(1, Ordering::AcqRel);
            if redirect {
                let mut response = reject(StatusCode::TEMPORARY_REDIRECT, "test_redirect");
                response.headers_mut().insert(header::LOCATION, axum::http::HeaderValue::from_static("http://127.0.0.1:1/must-not-send"));
                return response;
            }
            let body = to_bytes(request.into_body(), BODY_LIMIT).await.unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            let messages = value["messages"].as_array();
            let last = messages.and_then(|messages| messages.last());
            let last_json = last.map(Value::to_string).unwrap_or_default();
            let tool_result = last_json.contains("tool_result");
            let text = if last_json.contains("l1-") { "{\"code\":\"ORCHID_3478\"}" } else if last_json.contains("l2-") { "CACHE_OK" } else if last_json.contains("l4-") { "MAPLE" } else if last_json.contains("l5-") { "RIVER" } else { "TOOL_OK" };
            let tool = last_json.contains("l3-tool") && !tool_result;
            let content = if tool { json!([{"type":"tool_use","id":"fixture-get-agent","name":"GetAgent","input":{}}]) } else { json!([{"type":"text","text":text}]) };
            let result = json!({"id":"fixture-message","type":"message","role":"assistant","model":"fixture-model","content":content,"stop_reason":if tool { "tool_use" } else { "end_turn" },"usage":{"input_tokens":10,"output_tokens":5}});
            let mut response = Response::new(Body::from(result.to_string()));
            response.headers_mut().insert(header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json"));
            response
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok((origin, calls, task))
}

pub async fn verify_gateway_limits() -> Result<()> {
    let (upstream, calls, server) = mock_server(false).await?;
    let mut gateway = Gateway::start(&upstream, "fixture-only-placeholder".into(), true).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let endpoint = format!("{}/v1/messages", gateway.local_base);
    let too_large = client
        .post(&endpoint)
        .body("x".repeat(BODY_LIMIT + 1))
        .send()
        .await?;
    ensure!(
        too_large.status() == StatusCode::PAYLOAD_TOO_LARGE,
        "oversized request passed the gateway"
    );
    let bad_output = client
        .post(&endpoint)
        .json(&json!({"max_tokens":513}))
        .send()
        .await?;
    ensure!(
        bad_output.status() == StatusCode::BAD_REQUEST && calls.load(Ordering::Acquire) == 0,
        "output limit caused upstream I/O"
    );
    for _ in 0..SEND_LIMIT {
        ensure!(
            client
                .post(&endpoint)
                .json(&json!({"max_tokens":1}))
                .send()
                .await?
                .status()
                .is_success(),
            "bounded fixture send failed"
        );
    }
    ensure!(
        client
            .post(&endpoint)
            .json(&json!({"max_tokens":1}))
            .send()
            .await?
            .status()
            == StatusCode::TOO_MANY_REQUESTS,
        "13th upstream send was allowed"
    );
    ensure!(
        calls.load(Ordering::Acquire) == SEND_LIMIT,
        "wire count does not match real upstream sends"
    );
    gateway.shutdown().await;
    server.abort();
    let (upstream, calls, server) = mock_server(true).await?;
    let mut gateway = Gateway::start(&upstream, "fixture-only-placeholder".into(), true).await?;
    let result = client
        .post(format!("{}/v1/messages", gateway.local_base))
        .json(&json!({"max_tokens":1}))
        .send()
        .await?;
    ensure!(
        result.status() == StatusCode::BAD_GATEWAY && calls.load(Ordering::Acquire) == 1,
        "upstream redirect was followed"
    );
    gateway.shutdown().await;
    server.abort();
    Ok(())
}

pub async fn verify_offline_script() -> Result<()> {
    let (upstream, calls, server) = mock_server(false).await?;
    let source = crate::support::TestConfigBuilder::new()
        .build()
        .config()
        .clone();
    let route = source.default_model.clone();
    let mut selection = SelectedRoute::from_config(source, route)?;
    selection.upstream.base_url = upstream.clone();
    let mut gateway = Gateway::start(&upstream, "fixture-only-placeholder".into(), true).await?;
    let reports = run_suite(&selection, &gateway).await;
    gateway.shutdown().await;
    server.abort();
    ensure!(
        reports.len() == 5 && reports.iter().all(|report| report.passed),
        "offline acceptance script failed: {}",
        serde_json::to_string(&reports)?
    );
    ensure!(
        calls.load(Ordering::Acquire) == 10,
        "fixed script did not use exactly ten real local wire sends"
    );
    Ok(())
}
