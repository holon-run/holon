#![allow(dead_code)]

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    http::{self, AppState},
    provider::{
        AgentProvider, ConversationMessage, ProviderTurnRequest, ProviderTurnResponse, StubProvider,
    },
    run_once::{RunFinalStatus, RunOnceResponse},
    runtime::RuntimeHandle,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        OperatorTransportBinding, OperatorTransportBindingStatus, OperatorTransportCapabilities,
        OperatorTransportDeliveryAuth, OperatorTransportDeliveryAuthKind, WorkItemRecord,
        WorkItemState,
    },
};
pub use tempfile::tempdir;
use tokio::sync::watch;
use tokio::{
    net::{TcpListener, UnixListener},
    task::JoinHandle,
    time::{Duration, Instant},
};

pub struct TestConfigBuilder {
    data_dir: Option<PathBuf>,
    workspace_dir: Option<PathBuf>,
    http_addr: String,
    control_auth_mode: ControlAuthMode,
    compaction_trigger_messages: usize,
    compaction_keep_recent_messages: usize,
    prompt_budget_estimated_tokens: usize,
    compaction_trigger_estimated_tokens: usize,
    compaction_keep_recent_estimated_tokens: usize,
}

pub async fn test_work_item(
    runtime: &RuntimeHandle,
    delivery_target: &str,
    state: WorkItemState,
    current: bool,
    blocked_by: Option<&str>,
) -> Result<WorkItemRecord> {
    let (mut record, _) = runtime
        .create_work_item(delivery_target.to_string(), None)
        .await?;
    if let Some(blocked_by) = blocked_by {
        (record, _) = runtime
            .update_work_item_fields(record.id.clone(), Some(Some(blocked_by.to_string())), None)
            .await?;
    }
    if current {
        runtime.pick_work_item(record.id.clone()).await?;
    }
    if state == WorkItemState::Done {
        record = runtime.complete_work_item(record.id.clone(), None).await?;
    }
    Ok(record)
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self {
            data_dir: None,
            workspace_dir: None,
            http_addr: "127.0.0.1:0".into(),
            control_auth_mode: ControlAuthMode::Auto,
            compaction_trigger_messages: 10,
            compaction_keep_recent_messages: 4,
            prompt_budget_estimated_tokens: 16384,
            compaction_trigger_estimated_tokens: 8192,
            compaction_keep_recent_estimated_tokens: 2048,
        }
    }

    pub fn with_data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    pub fn with_workspace_dir(mut self, workspace_dir: PathBuf) -> Self {
        self.workspace_dir = Some(workspace_dir);
        self
    }

    pub fn with_http_addr(mut self, http_addr: impl Into<String>) -> Self {
        self.http_addr = http_addr.into();
        self
    }

    pub fn with_control_auth_mode(mut self, control_auth_mode: ControlAuthMode) -> Self {
        self.control_auth_mode = control_auth_mode;
        self
    }

    pub fn with_compaction(
        mut self,
        trigger_messages: usize,
        keep_recent_messages: usize,
        trigger_tokens: usize,
        keep_recent_tokens: usize,
        prompt_budget: usize,
    ) -> Self {
        self.compaction_trigger_messages = trigger_messages;
        self.compaction_keep_recent_messages = keep_recent_messages;
        self.compaction_trigger_estimated_tokens = trigger_tokens;
        self.compaction_keep_recent_estimated_tokens = keep_recent_tokens;
        self.prompt_budget_estimated_tokens = prompt_budget;
        self
    }

    pub fn build(self) -> AppConfig {
        let data_dir = self
            .data_dir
            .unwrap_or_else(|| tempdir().expect("temp data dir").keep());
        let workspace_dir = self
            .workspace_dir
            .unwrap_or_else(|| tempdir().expect("temp workspace dir").keep());
        AppConfig {
            default_agent_id: "default".into(),
            http_addr: self.http_addr,
            callback_base_url: "http://127.0.0.1:0".into(),
            home_dir: data_dir.clone(),
            data_dir: data_dir.clone(),
            socket_path: data_dir.join("run").join("holon.sock"),
            workspace_dir,
            context_window_messages: 8,
            context_window_briefs: 8,
            compaction_trigger_messages: self.compaction_trigger_messages,
            compaction_keep_recent_messages: self.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: self.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: self.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: self.compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            control_token: Some("secret".into()),
            control_auth_mode: self.control_auth_mode,
            config_file_path: data_dir.join("config.json"),
            stored_config: Default::default(),
            default_model: holon::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            fallback_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            disable_provider_fallback: false,
            tui_alternate_screen: holon::config::AltScreenMode::Auto,
            validated_model_overrides: std::collections::HashMap::new(),
            validated_unknown_model_fallback: None,
            providers: holon::config::provider_registry_for_tests(
                None,
                Some("dummy"),
                data_dir.join(".codex"),
            ),
        }
    }
}

impl Default for TestConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn init_git_repo(path: &Path) -> Result<()> {
    git(path, &["init"])?;
    git(path, &["config", "user.email", "holon@example.com"])?;
    git(path, &["config", "user.name", "Holon Test"])?;
    std::fs::write(path.join("README.md"), "holon\n")?;
    git(path, &["add", "README.md"])?;
    git(path, &["commit", "-m", "init"])?;
    Ok(())
}

pub struct GitWorkspaceFixture {
    pub root: PathBuf,
}

impl GitWorkspaceFixture {
    pub fn new() -> Result<Self> {
        let root = tempdir()?.keep();
        init_git_repo(&root)?;
        Ok(Self { root })
    }

    pub fn bare() -> Result<Self> {
        let root = tempdir()?.keep();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }
}

pub async fn attach_default_workspace(host: &RuntimeHost) -> Result<()> {
    let runtime = host.default_runtime().await?;
    let workspace = host.ensure_workspace_entry(host.config().workspace_dir.clone())?;
    runtime.attach_workspace(&workspace).await?;
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            Some(host.config().workspace_dir.clone()),
            None,
        )
        .await?;
    Ok(())
}

pub async fn eventually(predicate: impl Fn() -> Result<bool>) -> Result<()> {
    eventually_for(Duration::from_secs(3), predicate).await
}

pub async fn eventually_for(timeout: Duration, predicate: impl Fn() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

pub async fn eventually_async<F, Fut>(predicate: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if predicate().await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

pub struct RuntimeHarness {
    pub host: RuntimeHost,
    pub runtime: RuntimeHandle,
    pub workspace_dir: PathBuf,
}

impl RuntimeHarness {
    pub async fn with_provider(provider: Arc<dyn AgentProvider>) -> Result<Self> {
        Self::with_config_and_provider(TestConfigBuilder::new().build(), provider).await
    }

    pub async fn with_config_and_provider(
        config: AppConfig,
        provider: Arc<dyn AgentProvider>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&config.workspace_dir)?;
        let host = RuntimeHost::new_with_provider(config.clone(), provider)?;
        attach_default_workspace(&host).await?;
        let runtime = host.default_runtime().await?;
        Ok(Self {
            host,
            runtime,
            workspace_dir: config.workspace_dir.clone(),
        })
    }
}

pub struct HttpHarness {
    pub host: RuntimeHost,
    pub base_url: String,
    pub server: JoinHandle<anyhow::Result<()>>,
}

impl HttpHarness {
    pub async fn with_provider(provider: Arc<dyn AgentProvider>) -> Result<Self> {
        let config = TestConfigBuilder::new().build();
        Self::with_config_and_provider(config, provider).await
    }

    pub async fn with_config_and_provider(
        config: AppConfig,
        provider: Arc<dyn AgentProvider>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&config.workspace_dir)?;
        init_git_repo(&config.workspace_dir)?;
        let bind_addr = config.http_addr.clone();
        let host = RuntimeHost::new_with_provider(config, provider)?;
        attach_default_workspace(&host).await?;
        let router: Router = http::router(AppState::for_tcp(host.clone()));
        let listener = TcpListener::bind(&bind_addr).await?;
        let addr = connect_addr(listener.local_addr()?);
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await?;
            Ok(())
        });
        Ok(Self {
            host,
            base_url: format!("http://{}", addr),
            server,
        })
    }

    pub fn abort(&self) {
        self.server.abort();
    }
}

impl Drop for HttpHarness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub fn assert_run_once_completed_text(response: &RunOnceResponse, expected_text: &str) {
    assert_eq!(response.final_status, RunFinalStatus::Completed);
    assert_eq!(response.waiting_reason, None);
    assert_eq!(response.final_text, expected_text);
    assert!(response.failure_artifact.is_none());
    assert_eq!(response.token_usage.input_tokens, response.input_tokens);
    assert_eq!(response.token_usage.output_tokens, response.output_tokens);
    assert_eq!(
        response.token_usage.total_tokens,
        response.input_tokens + response.output_tokens
    );
}

fn connect_addr(addr: SocketAddr) -> SocketAddr {
    if addr.ip().is_unspecified() {
        SocketAddr::new(
            match addr {
                SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            },
            addr.port(),
        )
    } else {
        addr
    }
}

// Shared HTTP route helpers

pub struct RuntimeFailureProvider;

#[async_trait]
impl AgentProvider for RuntimeFailureProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        anyhow::bail!("provider transport broke")
    }
}

#[derive(Clone, Debug)]
pub struct DeliveryCallbackRecord {
    pub authorization: Option<String>,
    pub idempotency_key: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct UnixResponse {
    pub status: u16,
}

#[cfg(unix)]
pub async fn unix_request(
    socket_path: &Path,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<UnixResponse> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(socket_path).await?;
    let body = body.unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let text = String::from_utf8(response)?;
    let head = if let Some((head, _)) = text.split_once("\r\n\r\n") {
        head
    } else if let Some((head, _)) = text.split_once("\n\n") {
        head
    } else {
        anyhow::bail!("invalid unix http response: {text:?}");
    };
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| anyhow::anyhow!("missing status line"))?
        .parse::<u16>()?;
    Ok(UnixResponse { status })
}

#[derive(Clone, Debug)]
pub struct ParsedSseEvent {
    pub _id: String,
    pub event: String,
    pub data: serde_json::Value,
}

fn parse_sse_frame(raw: &str) -> Option<ParsedSseEvent> {
    let mut id = String::new();
    let mut event = String::new();
    let mut data = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim_end();
        if let Some(value) = trimmed.strip_prefix("id:") {
            id = value.trim().to_string();
        } else if let Some(value) = trimmed.strip_prefix("event:") {
            event = value.trim().to_string();
        } else if let Some(value) = trimmed.strip_prefix("data:") {
            data.push(value.trim().to_string());
        }
    }
    if data.is_empty() {
        return None;
    }
    let data_json = if data.len() == 1 {
        serde_json::from_str(&data[0]).ok()?
    } else {
        serde_json::Value::String(data.join("\n"))
    };
    Some(ParsedSseEvent {
        _id: id,
        event,
        data: data_json,
    })
}

pub async fn read_next_sse_event(response: &mut reqwest::Response) -> Result<ParsedSseEvent> {
    let mut buffer = String::new();
    loop {
        let chunk = response
            .chunk()
            .await?
            .ok_or_else(|| anyhow::anyhow!("sse stream ended"))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(split) = buffer.find("\n\n") {
            let frame = buffer[..split].to_string();
            buffer.drain(0..split + 2);
            if frame.trim().is_empty() {
                continue;
            }
            if let Some(event) = parse_sse_frame(&frame) {
                return Ok(event);
            }
        }
    }
}

pub async fn spawn_unix_server(
    config: AppConfig,
) -> Result<(
    RuntimeHost,
    PathBuf,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let host = RuntimeHost::new_with_provider(
        config.clone(),
        Arc::new(StubProvider::new("route result")),
    )?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_unix(host.clone()));
    let listener = tokio::net::UnixListener::bind(&config.socket_path)?;
    let socket_path = config.socket_path.clone();
    let server = tokio::spawn(async move {
        let (_tx, rx) = watch::channel(false);
        http::serve_unix(listener, router, rx).await?;
        Ok(())
    });
    Ok((host, socket_path, server))
}

pub async fn spawn_delivery_callback(
    status: axum::http::StatusCode,
) -> Result<(
    String,
    Arc<Mutex<Vec<DeliveryCallbackRecord>>>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let records = Arc::new(Mutex::new(Vec::new()));
    let route_records = Arc::clone(&records);
    let router = Router::new().route(
        "/delivery",
        axum::routing::post(
            move |headers: axum::http::HeaderMap,
                  axum::Json(payload): axum::Json<serde_json::Value>| {
                let route_records = Arc::clone(&route_records);
                async move {
                    route_records
                        .lock()
                        .expect("delivery callback records lock")
                        .push(DeliveryCallbackRecord {
                            authorization: headers
                                .get(reqwest::header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(ToString::to_string),
                            idempotency_key: headers
                                .get("Idempotency-Key")
                                .and_then(|value| value.to_str().ok())
                                .map(ToString::to_string),
                            payload,
                        });
                    (
                        status,
                        axum::Json(serde_json::json!({
                            "status": "accepted",
                            "transport_delivery_id": "ain_del_test"
                        })),
                    )
                }
            },
        ),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok(())
    });
    Ok((format!("http://{addr}/delivery"), records, server))
}

pub async fn wait_until<F>(mut predicate: F) -> Result<()>
where
    F: FnMut() -> Result<bool>,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if predicate()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for condition");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// Shared HTTP route helpers

/// Create default test configuration.
pub fn test_config() -> AppConfig {
    TestConfigBuilder::new()
        .with_control_auth_mode(ControlAuthMode::Auto)
        .build()
}

pub fn test_config_with_paths(
    data_dir: PathBuf,
    workspace_dir: PathBuf,
    http_addr: String,
    control_auth_mode: ControlAuthMode,
) -> AppConfig {
    TestConfigBuilder::new()
        .with_data_dir(data_dir)
        .with_workspace_dir(workspace_dir)
        .with_http_addr(http_addr)
        .with_control_auth_mode(control_auth_mode)
        .build()
}

pub async fn spawn_server() -> Result<(
    RuntimeHost,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let config = test_config();
    let bind_addr = config.http_addr.clone();
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("route result")))?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_tcp(host.clone()));
    let listener = TcpListener::bind(&bind_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok(())
    });
    Ok((host, format!("http://{}", addr), server))
}

pub async fn spawn_server_for_host(
    host: RuntimeHost,
) -> Result<(String, tokio::task::JoinHandle<anyhow::Result<()>>)> {
    let router: Router = http::router(AppState::for_tcp(host));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok(())
    });
    Ok((format!("http://{}", addr), server))
}

pub async fn spawn_server_with_config(
    config: AppConfig,
) -> Result<(
    RuntimeHost,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let bind_addr = config.http_addr.clone();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("route result")))?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_tcp(host.clone()));
    let listener = TcpListener::bind(&bind_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok(())
    });
    Ok((host, format!("http://{}", addr), server))
}

pub async fn spawn_server_with_runtime_config(
    config: AppConfig,
) -> Result<(
    RuntimeHost,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    std::fs::create_dir_all(&config.workspace_dir)?;
    init_git_repo(&config.workspace_dir)?;
    let bind_addr = config.http_addr.clone();
    let host = RuntimeHost::new(config)?;
    attach_default_workspace(&host).await?;
    let router: Router = http::router(AppState::for_tcp(host.clone()));
    let listener = TcpListener::bind(&bind_addr).await?;
    let addr = connect_addr(listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok(())
    });
    Ok((host, format!("http://{}", addr), server))
}

// HTTP route support modules
pub mod http_callback;
pub mod http_client;
pub mod http_control;
pub mod http_events;
pub mod http_ingress;
pub mod http_operator_ingress;
pub mod http_tasks;
pub mod http_workspace;

// Runtime flow support modules
pub mod runtime_compaction;
pub mod runtime_helpers;
pub mod runtime_providers;
pub mod runtime_subagents;
pub mod runtime_tasks;
pub mod runtime_waiting;
pub mod runtime_workspace_worktree;

pub fn callback_token(trigger_url: &str) -> &str {
    trigger_url
        .rsplit('/')
        .next()
        .expect("callback url should end with a token")
}

pub fn callback_path(trigger_url: &str) -> String {
    trigger_url
        .split_once("/callbacks/")
        .map(|(_, path)| path)
        .map(|path| format!("/callbacks/{path}"))
        .expect("callback url should contain /callbacks/")
}

pub async fn create_operator_transport_binding(
    client: &reqwest::Client,
    base: &str,
    binding_id: &str,
    delivery_callback_url: &str,
) -> Result<serde_json::Value> {
    let response = client
        .post(format!("{base}/control/agents/default/operator-bindings"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "binding_id": binding_id,
            "transport": "agentinbox",
            "operator_actor_id": "operator:jolestar",
            "default_route_id": "route-default",
            "delivery_callback_url": delivery_callback_url,
            "delivery_auth": {
                "kind": "bearer",
                "bearer_token": "delivery-secret"
            },
            "capabilities": {
                "text": true
            },
            "provider": "agentinbox",
            "provider_identity_ref": "agentinbox:operator:jolestar",
            "metadata": {
                "conversation_ref": "agentinbox:dm:jolestar"
            }
        }))
        .send()
        .await?;
    assert!(
        response.status().is_success(),
        "{:?}",
        response.text().await?
    );
    Ok(response.json().await?)
}

// Domain-specific runtime test support utilities

/// Parse tool result content as JSON value.
pub fn parse_tool_result_value(result: &holon::tool::ToolResult) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(&result.content_text()?)?)
}

/// Parse the "result" field from tool result payload.
pub fn parse_tool_result_payload(result: &holon::tool::ToolResult) -> Result<serde_json::Value> {
    Ok(parse_tool_result_value(result)?["result"].clone())
}

/// Create a test operator transport binding.
pub fn operator_transport_binding(binding_id: &str, route_id: &str) -> OperatorTransportBinding {
    OperatorTransportBinding {
        binding_id: binding_id.to_string(),
        transport: "agentinbox".into(),
        operator_actor_id: "operator:jolestar".into(),
        target_agent_id: "default".into(),
        default_route_id: route_id.to_string(),
        delivery_callback_url: "http://127.0.0.1:1/delivery".into(),
        delivery_auth: OperatorTransportDeliveryAuth {
            kind: OperatorTransportDeliveryAuthKind::Bearer,
            key_id: None,
            bearer_token: Some("delivery-secret".into()),
        },
        capabilities: OperatorTransportCapabilities {
            text: true,
            markdown: None,
            attachments: None,
        },
        provider: Some("agentinbox".into()),
        provider_identity_ref: Some("agentinbox:operator:jolestar".into()),
        status: OperatorTransportBindingStatus::Active,
        created_at: chrono::Utc::now(),
        last_seen_at: None,
        metadata: None,
    }
}

/// Check if request preserves prior tool context.
pub fn preserves_prior_tool_context(request: &ProviderTurnRequest) -> bool {
    let has_exact_tool_results = request
        .conversation
        .iter()
        .any(|message| matches!(message, ConversationMessage::UserToolResults(_)));
    let has_turn_local_recap = request.conversation.iter().any(|message| {
        matches!(
            message,
            ConversationMessage::UserText(text)
                if text.contains("Turn-local recap for older completed rounds")
        )
    });
    has_exact_tool_results || has_turn_local_recap
}

/// Get delegated prompt text from request.
pub fn delegated_prompt_text(request: &ProviderTurnRequest) -> String {
    request
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserText(text) => Some(text.clone()),
            ConversationMessage::UserBlocks(blocks) => Some(
                blocks
                    .iter()
                    .map(|block| block.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

/// Wait until an async predicate is true with default timeout.
pub async fn wait_until_async<F, Fut>(predicate: F) -> Result<()>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<bool>> + Send,
{
    eventually_async(predicate).await
}
