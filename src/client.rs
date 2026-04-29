use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{
    config::AppConfig,
    daemon::{RuntimeShutdownResponse, RuntimeStatusResponse},
    http::{
        AttachWorkspaceRequest, ClearAgentModelRequest, ControlPromptRequest, DebugPromptRequest,
        DetachWorkspaceRequest, ExitWorkspaceRequest, SetAgentModelRequest,
    },
    system::ExecutionSnapshot,
    types::{
        ActiveWorkspaceEntry, AgentSummary, BriefRecord, ExternalTriggerStateSnapshot,
        OperatorNotificationRecord, TaskRecord, TimerRecord, TranscriptEntry, TrustLevel,
        TurnTerminalRecord, WaitingIntentRecord, WorkItemRecord, WorkPlanSnapshot,
        WorkspaceOccupancyRecord, WorktreeSession,
    },
};

#[derive(Clone)]
pub struct LocalClient {
    config: AppConfig,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceAttachResponse {
    pub ok: bool,
    pub agent_id: String,
    pub workspace_id: String,
    pub workspace_anchor: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceExitResponse {
    pub ok: bool,
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDetachResponse {
    pub ok: bool,
    pub agent_id: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugPromptResponse {
    pub ok: bool,
    pub agent_id: String,
    pub dump: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSessionSnapshot {
    pub current_run_id: Option<String>,
    pub pending_count: usize,
    pub last_turn: Option<TurnTerminalRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateWorkspaceSnapshot {
    #[serde(default)]
    pub attached_workspaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace_entry: Option<ActiveWorkspaceEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_workspace_occupancy: Option<WorkspaceOccupancyRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_session: Option<WorktreeSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateSnapshot {
    pub agent: AgentSummary,
    pub session: StateSessionSnapshot,
    pub tasks: Vec<TaskRecord>,
    pub transcript_tail: Vec<TranscriptEntry>,
    #[serde(default)]
    pub briefs_tail: Vec<BriefRecord>,
    #[serde(default)]
    pub timers: Vec<TimerRecord>,
    #[serde(default)]
    pub work_items: Vec<WorkItemRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_plan: Option<WorkPlanSnapshot>,
    #[serde(default)]
    pub waiting_intents: Vec<WaitingIntentRecord>,
    #[serde(default)]
    pub external_triggers: Vec<ExternalTriggerStateSnapshot>,
    #[serde(default)]
    pub operator_notifications: Vec<OperatorNotificationRecord>,
    #[serde(default)]
    pub workspace: StateWorkspaceSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<BriefRecord>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EventStreamRequest {
    pub since: Option<String>,
    pub last_event_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamEventEnvelope {
    pub id: String,
    pub seq: u64,
    pub ts: chrono::DateTime<Utc>,
    pub agent_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStreamEvent {
    pub id: String,
    pub event: String,
    pub data: StreamEventEnvelope,
}

pub struct LocalEventStream {
    transport: EventStreamTransport,
    frame_buffer: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct LocalHttpError {
    pub path: String,
    pub status_code: u16,
    pub message: String,
    pub code: Option<String>,
    pub hint: Option<String>,
}

impl LocalHttpError {
    pub fn has_code(&self, expected: &str) -> bool {
        self.code.as_deref() == Some(expected)
    }

    fn code_suffix(&self) -> String {
        self.code
            .as_deref()
            .map(|code| format!(" [{code}]"))
            .unwrap_or_default()
    }

    fn hint_suffix(&self) -> String {
        self.hint
            .as_deref()
            .map(|hint| format!(" Hint: {hint}"))
            .unwrap_or_default()
    }
}

impl std::fmt::Display for LocalHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} returned HTTP {}: {}{}{}",
            self.path,
            self.status_code,
            self.message,
            self.code_suffix(),
            self.hint_suffix()
        )
    }
}

impl std::error::Error for LocalHttpError {}

enum EventStreamTransport {
    Http(reqwest::Response),
    #[cfg(unix)]
    Unix(UnixEventStream),
}

#[cfg(unix)]
struct UnixEventStream {
    stream: tokio::net::UnixStream,
    body_buffer: Vec<u8>,
    chunked: bool,
    current_chunk_size: Option<usize>,
    eof: bool,
}

impl LocalClient {
    pub fn new(config: AppConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()
            .context("failed to build local control client")?;
        Ok(Self { config, http })
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentSummary>> {
        self.get_json("/agents").await
    }

    pub async fn runtime_status(&self) -> Result<RuntimeStatusResponse> {
        self.get_control_json("/control/runtime/status").await
    }

    #[cfg(unix)]
    pub async fn runtime_status_unix_only(&self) -> Result<RuntimeStatusResponse> {
        let body = self
            .send_unix(RequestSpec::get("/control/runtime/status"), true)
            .await?;
        serde_json::from_slice(&body).with_context(|| {
            "failed to decode response body for GET /control/runtime/status over unix socket"
        })
    }

    pub async fn runtime_shutdown(&self) -> Result<RuntimeShutdownResponse> {
        self.post_control_json("/control/runtime/shutdown", &serde_json::json!({}))
            .await
    }

    pub async fn agent_status(&self, agent_id: &str) -> Result<AgentSummary> {
        self.get_json(&format!("/agents/{agent_id}/status")).await
    }

    pub async fn agent_state_snapshot(&self, agent_id: &str) -> Result<AgentStateSnapshot> {
        self.get_json(&format!("/agents/{agent_id}/state")).await
    }

    pub async fn agent_briefs(&self, agent_id: &str, limit: usize) -> Result<Vec<BriefRecord>> {
        self.get_json(&format!("/agents/{agent_id}/briefs?limit={limit}"))
            .await
    }

    pub async fn agent_transcript(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<TranscriptEntry>> {
        self.get_json(&format!("/agents/{agent_id}/transcript?limit={limit}"))
            .await
    }

    pub async fn agent_tasks(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.get_json(&format!("/agents/{agent_id}/tasks?limit={limit}"))
            .await
    }

    pub async fn control_prompt(&self, agent_id: &str, text: impl Into<String>) -> Result<Value> {
        self.post_control_json(
            &format!("/control/agents/{agent_id}/prompt"),
            &ControlPromptRequest { text: text.into() },
        )
        .await
    }

    pub async fn attach_workspace(
        &self,
        agent_id: &str,
        path: impl Into<String>,
    ) -> Result<WorkspaceAttachResponse> {
        self.post_control_json(
            &format!("/control/agents/{agent_id}/workspace/attach"),
            &AttachWorkspaceRequest {
                path: path.into(),
                trust: Some(TrustLevel::TrustedOperator),
            },
        )
        .await
    }

    pub async fn exit_workspace(&self, agent_id: &str) -> Result<WorkspaceExitResponse> {
        self.post_control_json(
            &format!("/control/agents/{agent_id}/workspace/exit"),
            &ExitWorkspaceRequest {
                trust: Some(TrustLevel::TrustedOperator),
            },
        )
        .await
    }

    pub async fn detach_workspace(
        &self,
        agent_id: &str,
        workspace_id: impl Into<String>,
    ) -> Result<WorkspaceDetachResponse> {
        self.post_control_json(
            &format!("/control/agents/{agent_id}/workspace/detach"),
            &DetachWorkspaceRequest {
                workspace_id: workspace_id.into(),
                trust: Some(TrustLevel::TrustedOperator),
            },
        )
        .await
    }

    pub async fn debug_prompt(
        &self,
        agent_id: &str,
        text: impl Into<String>,
        trust: TrustLevel,
    ) -> Result<String> {
        let response: DebugPromptResponse = self
            .post_control_json(
                &format!("/control/agents/{agent_id}/debug-prompt"),
                &DebugPromptRequest {
                    text: text.into(),
                    trust: Some(trust),
                },
            )
            .await?;
        Ok(response.dump)
    }

    pub async fn set_agent_model_override(
        &self,
        agent_id: &str,
        model: impl Into<String>,
    ) -> Result<Value> {
        self.post_control_json(
            &format!("/control/agents/{agent_id}/model"),
            &SetAgentModelRequest {
                model: model.into(),
                trust: Some(TrustLevel::TrustedOperator),
            },
        )
        .await
    }

    pub async fn clear_agent_model_override(&self, agent_id: &str) -> Result<Value> {
        self.post_control_json(
            &format!("/control/agents/{agent_id}/model/clear"),
            &ClearAgentModelRequest {
                trust: Some(TrustLevel::TrustedOperator),
            },
        )
        .await
    }

    pub async fn stream_agent_events(
        &self,
        agent_id: &str,
        request: EventStreamRequest,
    ) -> Result<LocalEventStream> {
        let path = event_stream_path(agent_id, &request)?;

        #[cfg(unix)]
        if self.config.socket_path.exists() {
            let socket_error = match self
                .stream_unix_events(&path, false, request.last_event_id.as_deref())
                .await
            {
                Ok(stream) => {
                    return Ok(LocalEventStream {
                        transport: EventStreamTransport::Unix(stream),
                        frame_buffer: Vec::new(),
                    })
                }
                Err(err) => err,
            };
            return self
                .stream_http_events(&path, false, request.last_event_id.as_deref())
                .await
                .with_context(|| {
                    format!("unix socket event stream failed before HTTP fallback: {socket_error}")
                });
        }

        self.stream_http_events(&path, false, request.last_event_id.as_deref())
            .await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let body = self.send(RequestSpec::get(path), false).await?;
        serde_json::from_slice(&body)
            .with_context(|| format!("failed to decode response body for GET {}", path))
    }

    async fn get_control_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let body = self.send(RequestSpec::get(path), true).await?;
        serde_json::from_slice(&body)
            .with_context(|| format!("failed to decode response body for GET {}", path))
    }

    async fn post_control_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        payload: &B,
    ) -> Result<T> {
        let body = self
            .send(RequestSpec::post_json(path, payload)?, true)
            .await?;
        serde_json::from_slice(&body)
            .with_context(|| format!("failed to decode response body for POST {}", path))
    }

    async fn send(&self, request: RequestSpec, include_control_auth: bool) -> Result<Vec<u8>> {
        #[cfg(unix)]
        if self.config.socket_path.exists() {
            let socket_error = match self.send_unix(request.clone(), include_control_auth).await {
                Ok(body) => return Ok(body),
                Err(err) => err,
            };
            return self
                .send_http(request, include_control_auth)
                .await
                .with_context(|| {
                    format!("unix socket request failed before HTTP fallback: {socket_error}")
                });
        }

        self.send_http(request, include_control_auth).await
    }

    async fn send_http(&self, request: RequestSpec, include_control_auth: bool) -> Result<Vec<u8>> {
        let response = self
            .build_http_request(&request, include_control_auth)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .with_context(|| format!("failed to send {}", request.path))?;
        let status = response.status();
        let bytes = response.bytes().await?.to_vec();
        decode_or_error(status.as_u16(), bytes, &request.path)
    }

    async fn stream_http_events(
        &self,
        path: &str,
        include_control_auth: bool,
        last_event_id: Option<&str>,
    ) -> Result<LocalEventStream> {
        if let Some(last_event_id) = last_event_id {
            validate_header_value("Last-Event-ID", last_event_id)?;
        }
        let request = RequestSpec::get(path);
        let mut builder = self
            .build_http_request(&request, include_control_auth)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(last_event_id) = last_event_id {
            builder = builder.header("Last-Event-ID", last_event_id);
        }
        let response = builder
            .send()
            .await
            .with_context(|| format!("failed to open event stream {}", path))?;
        let status = response.status();
        if !status.is_success() {
            let bytes = response.bytes().await?.to_vec();
            decode_or_error(status.as_u16(), bytes, path)?;
            unreachable!("decode_or_error returns Ok only for successful responses");
        }
        Ok(LocalEventStream {
            transport: EventStreamTransport::Http(response),
            frame_buffer: Vec::new(),
        })
    }

    fn build_http_request(
        &self,
        request: &RequestSpec,
        include_control_auth: bool,
    ) -> reqwest::RequestBuilder {
        let mut builder = match request.method {
            HttpMethod::Get => self
                .http
                .get(format!("http://{}{}", self.config.http_addr, request.path)),
            HttpMethod::Post => self
                .http
                .post(format!("http://{}{}", self.config.http_addr, request.path)),
        };
        if include_control_auth {
            if let Some(token) = &self.config.control_token {
                builder = builder.bearer_auth(token);
            }
        }
        if let Some(body) = request.body.clone() {
            builder = builder
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        builder
    }

    #[cfg(unix)]
    async fn send_unix(&self, request: RequestSpec, include_control_auth: bool) -> Result<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(&self.config.socket_path)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to Holon control socket {}",
                    self.config.socket_path.display()
                )
            })?;

        let method = match request.method {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
        };
        let mut raw = format!(
            "{method} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAccept: application/json\r\n",
            request.path
        );
        if include_control_auth {
            if let Some(token) = &self.config.control_token {
                raw.push_str(&format!(
                    "Authorization: {}\r\n",
                    authorization_header_value(token)?
                ));
            }
        }
        let body_len = request.body.as_ref().map(|body| body.len()).unwrap_or(0);
        raw.push_str(&format!("Content-Length: {body_len}\r\n"));
        if request.body.is_some() {
            raw.push_str("Content-Type: application/json\r\n");
        }
        raw.push_str("\r\n");

        stream.write_all(raw.as_bytes()).await?;
        if let Some(body) = request.body.as_ref() {
            stream.write_all(body).await?;
        }
        stream.flush().await?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        let parsed = parse_http_response(&response).with_context(|| {
            format!("failed to parse unix-socket response for {}", request.path)
        })?;
        decode_or_error(parsed.status_code, parsed.body, &request.path)
    }

    #[cfg(unix)]
    async fn stream_unix_events(
        &self,
        path: &str,
        include_control_auth: bool,
        last_event_id: Option<&str>,
    ) -> Result<UnixEventStream> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        validate_unix_request_target(path)?;
        if let Some(last_event_id) = last_event_id {
            validate_header_value("Last-Event-ID", last_event_id)?;
        }

        let mut stream = UnixStream::connect(&self.config.socket_path)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to Holon control socket {}",
                    self.config.socket_path.display()
                )
            })?;

        let mut raw = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\nAccept: text/event-stream\r\n",
        );
        if include_control_auth {
            if let Some(token) = &self.config.control_token {
                raw.push_str(&format!(
                    "Authorization: {}\r\n",
                    authorization_header_value(token)?
                ));
            }
        }
        if let Some(last_event_id) = last_event_id {
            raw.push_str(&format!("Last-Event-ID: {last_event_id}\r\n"));
        }
        raw.push_str("Content-Length: 0\r\n\r\n");

        stream.write_all(raw.as_bytes()).await?;
        stream.flush().await?;

        let mut response = read_unix_response_head(&mut stream)
            .await
            .with_context(|| format!("failed to parse unix-socket response for {path}"))?;
        if !(200..300).contains(&response.status_code) {
            let mut tail = Vec::new();
            stream.read_to_end(&mut tail).await?;
            response.body.extend_from_slice(&tail);
            let body = if response.chunked {
                decode_chunked_body(&response.body)?
            } else {
                response.body
            };
            decode_or_error(response.status_code, body, path)?;
            unreachable!("decode_or_error returns Ok only for successful responses");
        }

        Ok(UnixEventStream {
            stream,
            body_buffer: response.body,
            chunked: response.chunked,
            current_chunk_size: None,
            eof: false,
        })
    }
}

#[derive(Clone)]
struct RequestSpec {
    method: HttpMethod,
    path: String,
    body: Option<Vec<u8>>,
}

impl RequestSpec {
    fn get(path: &str) -> Self {
        Self {
            method: HttpMethod::Get,
            path: path.to_string(),
            body: None,
        }
    }

    fn post_json<B: Serialize>(path: &str, payload: &B) -> Result<Self> {
        Ok(Self {
            method: HttpMethod::Post,
            path: path.to_string(),
            body: Some(serde_json::to_vec(payload)?),
        })
    }
}

#[derive(Clone, Copy)]
enum HttpMethod {
    Get,
    Post,
}

impl LocalEventStream {
    pub async fn next_event(&mut self) -> Result<AgentStreamEvent> {
        loop {
            while let Some(frame) = take_next_sse_frame(&mut self.frame_buffer)? {
                if let Some(event) = parse_sse_frame(&frame)? {
                    return Ok(event);
                }
            }

            let chunk = match &mut self.transport {
                EventStreamTransport::Http(response) => response
                    .chunk()
                    .await
                    .context("failed to read HTTP event stream chunk")?
                    .map(|bytes| bytes.to_vec()),
                #[cfg(unix)]
                EventStreamTransport::Unix(stream) => stream.read_body_chunk().await?,
            };
            let chunk = chunk.ok_or_else(|| anyhow!("sse stream ended"))?;
            self.frame_buffer.extend_from_slice(&chunk);
        }
    }
}

#[cfg(unix)]
impl UnixEventStream {
    async fn read_body_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        use tokio::io::AsyncReadExt;

        if self.eof {
            return Ok(None);
        }

        if !self.chunked {
            if !self.body_buffer.is_empty() {
                return Ok(Some(std::mem::take(&mut self.body_buffer)));
            }
            let mut buffer = [0u8; 8192];
            let read = self.stream.read(&mut buffer).await?;
            if read == 0 {
                self.eof = true;
                return Ok(None);
            }
            return Ok(Some(buffer[..read].to_vec()));
        }

        loop {
            if let Some(size) = self.current_chunk_size {
                let required = size
                    .checked_add(2)
                    .ok_or_else(|| anyhow!("malformed chunked response: chunk size overflow"))?;
                if self.body_buffer.len() < required {
                    if !self.read_more().await? {
                        return Err(anyhow!("malformed chunked response: truncated chunk"));
                    }
                    continue;
                }
                let data = self.body_buffer.drain(..size).collect::<Vec<_>>();
                let terminator = self.body_buffer.drain(..2).collect::<Vec<_>>();
                if terminator.as_slice() != b"\r\n" {
                    return Err(anyhow!(
                        "malformed chunked response: missing chunk terminator"
                    ));
                }
                self.current_chunk_size = None;
                return Ok(Some(data));
            }

            if let Some(size_line_end) = find_bytes(&self.body_buffer, b"\r\n") {
                let size_line = std::str::from_utf8(&self.body_buffer[..size_line_end])
                    .context("malformed chunked response: non-utf8 size line")?;
                let size_hex = size_line.split(';').next().unwrap_or_default().trim();
                let size = usize::from_str_radix(size_hex, 16).with_context(|| {
                    format!("malformed chunked response: invalid size {size_hex}")
                })?;
                self.body_buffer.drain(..size_line_end + 2);
                if size == 0 {
                    while self.body_buffer.len() < 2 {
                        if !self.read_more().await? {
                            break;
                        }
                    }
                    if self.body_buffer.starts_with(b"\r\n") {
                        self.body_buffer.drain(..2);
                    }
                    self.eof = true;
                    return Ok(None);
                }
                self.current_chunk_size = Some(size);
                continue;
            }

            if !self.read_more().await? {
                return Err(anyhow!(
                    "malformed chunked response: missing size line terminator"
                ));
            }
        }
    }

    async fn read_more(&mut self) -> Result<bool> {
        use tokio::io::AsyncReadExt;

        let mut buffer = [0u8; 8192];
        let read = self.stream.read(&mut buffer).await?;
        if read == 0 {
            return Ok(false);
        }
        self.body_buffer.extend_from_slice(&buffer[..read]);
        Ok(true)
    }
}

struct ParsedHttpResponse {
    status_code: u16,
    body: Vec<u8>,
}

struct ParsedHttpResponseHead {
    status_code: u16,
    chunked: bool,
    body: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct ErrorPayload {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    hint: Option<String>,
}

fn event_stream_path(agent_id: &str, request: &EventStreamRequest) -> Result<String> {
    let mut url = reqwest::Url::parse("http://localhost")
        .context("failed to initialize event stream URL builder")?;
    url.path_segments_mut()
        .map_err(|_| anyhow!("failed to build event stream path"))?
        .extend(["agents", agent_id, "events"]);
    {
        let mut query = url.query_pairs_mut();
        if let Some(limit) = request.limit {
            query.append_pair("limit", &limit.to_string());
        }
        if let Some(since) = request.since.as_deref() {
            query.append_pair("since", since);
        }
    }

    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    Ok(path)
}

fn validate_unix_request_target(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        return Err(anyhow!(
            "invalid unix event stream request target: expected origin-form path"
        ));
    }
    if !path.is_ascii() {
        return Err(anyhow!(
            "invalid unix event stream request target: non-ASCII bytes are not allowed"
        ));
    }
    if path
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(anyhow!(
            "invalid unix event stream request target: control or space characters are not allowed"
        ));
    }
    Ok(())
}

fn validate_header_value(header_name: &str, value: &str) -> Result<()> {
    reqwest::header::HeaderValue::from_str(value)
        .with_context(|| format!("invalid {header_name} header value"))?;
    Ok(())
}

fn authorization_header_value(token: &str) -> Result<String> {
    let value = format!("Bearer {token}");
    validate_header_value("Authorization", &value)?;
    Ok(value)
}

fn find_bytes(buffer: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    buffer
        .windows(needle.len())
        .position(|window| window == needle)
}

fn take_next_sse_frame(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    let split = [b"\r\n\r\n".as_slice(), b"\n\n".as_slice()]
        .into_iter()
        .filter_map(|delimiter| find_bytes(buffer, delimiter).map(|index| (index, delimiter.len())))
        .min_by_key(|(index, _)| *index);
    let Some((index, delimiter_len)) = split else {
        return Ok(None);
    };
    let frame = buffer[..index].to_vec();
    buffer.drain(..index + delimiter_len);
    Ok(Some(frame))
}

fn parse_sse_frame(frame: &[u8]) -> Result<Option<AgentStreamEvent>> {
    let text = std::str::from_utf8(frame).context("malformed sse frame: non-utf8 payload")?;
    let mut id = String::new();
    let mut event = String::new();
    let mut data_lines = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with(':') {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("id:") {
            id = value.trim().to_string();
        } else if let Some(value) = trimmed.strip_prefix("event:") {
            event = value.trim().to_string();
        } else if let Some(value) = trimmed.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }

    if data_lines.is_empty() {
        return Ok(None);
    }

    let data: StreamEventEnvelope = serde_json::from_str(&data_lines.join("\n"))
        .context("failed to decode SSE event payload as JSON")?;
    Ok(Some(AgentStreamEvent { id, event, data }))
}

#[cfg(unix)]
async fn read_unix_response_head(
    stream: &mut tokio::net::UnixStream,
) -> Result<ParsedHttpResponseHead> {
    use tokio::io::AsyncReadExt;

    let mut buffer = Vec::new();
    let (header_end, delimiter_len) = loop {
        if let Some(index) = find_bytes(&buffer, b"\r\n\r\n") {
            break (index, 4);
        }
        if let Some(index) = find_bytes(&buffer, b"\n\n") {
            break (index, 2);
        }

        let mut chunk = [0u8; 8192];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(anyhow!(
                "malformed HTTP response: missing header terminator"
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let header_bytes = &buffer[..header_end];
    let body = buffer[header_end + delimiter_len..].to_vec();
    let header_text = std::str::from_utf8(header_bytes)
        .context("malformed HTTP response: headers are not valid UTF-8")?;
    let mut lines = header_text.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| anyhow!("malformed HTTP response: missing status line"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed HTTP response: missing status code"))?
        .parse::<u16>()
        .context("malformed HTTP response: invalid status code")?;
    let chunked = lines.any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });

    Ok(ParsedHttpResponseHead {
        status_code,
        chunked,
        body,
    })
}

fn decode_or_error(status_code: u16, body: Vec<u8>, path: &str) -> Result<Vec<u8>> {
    if (200..300).contains(&status_code) {
        return Ok(body);
    }

    let payload = serde_json::from_slice::<ErrorPayload>(&body).ok();
    let message = payload
        .as_ref()
        .and_then(|value| value.error.as_deref())
        .map(ToString::to_string)
        .unwrap_or_else(|| String::from_utf8_lossy(&body).trim().to_string());
    let code = payload.as_ref().and_then(|value| value.code.clone());
    let hint = payload.as_ref().and_then(|value| value.hint.clone());
    Err(LocalHttpError {
        path: path.to_string(),
        status_code,
        message,
        code,
        hint,
    }
    .into())
}

fn parse_http_response(buffer: &[u8]) -> Result<ParsedHttpResponse> {
    let header_end = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed HTTP response: missing header terminator"))?;
    let header_bytes = &buffer[..header_end];
    let body = &buffer[header_end + 4..];
    let header_text = std::str::from_utf8(header_bytes)
        .context("malformed HTTP response: headers are not valid UTF-8")?;
    let mut lines = header_text.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| anyhow!("malformed HTTP response: missing status line"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed HTTP response: missing status code"))?
        .parse::<u16>()
        .context("malformed HTTP response: invalid status code")?;
    let chunked = lines.any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });

    Ok(ParsedHttpResponse {
        status_code,
        body: if chunked {
            decode_chunked_body(body)?
        } else {
            body.to_vec()
        },
    })
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = 0usize;
    let mut decoded = Vec::new();

    loop {
        let size_line_end = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|offset| cursor + offset)
            .ok_or_else(|| anyhow!("malformed chunked response: missing size line terminator"))?;
        let size_line = std::str::from_utf8(&body[cursor..size_line_end])
            .context("malformed chunked response: non-utf8 size line")?;
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .with_context(|| format!("malformed chunked response: invalid size {}", size_hex))?;
        cursor = size_line_end + 2;
        if size == 0 {
            break;
        }
        let chunk_end = cursor
            .checked_add(size)
            .ok_or_else(|| anyhow!("malformed chunked response: chunk size overflow"))?;
        if chunk_end + 2 > body.len() {
            return Err(anyhow!("malformed chunked response: truncated chunk"));
        }
        decoded.extend_from_slice(&body[cursor..chunk_end]);
        if &body[chunk_end..chunk_end + 2] != b"\r\n" {
            return Err(anyhow!(
                "malformed chunked response: missing chunk terminator"
            ));
        }
        cursor = chunk_end + 2;
    }

    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::{
        authorization_header_value, decode_chunked_body, decode_or_error, event_stream_path,
        parse_http_response, parse_sse_frame, take_next_sse_frame, validate_header_value,
        validate_unix_request_target, EventStreamRequest, LocalHttpError,
    };

    #[test]
    fn parse_http_response_decodes_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n";
        let parsed = parse_http_response(raw).unwrap();
        assert_eq!(parsed.status_code, 200);
        assert_eq!(parsed.body, b"test");
    }

    #[test]
    fn decode_chunked_body_rejects_truncated_payload() {
        let err = decode_chunked_body(b"4\r\nte").unwrap_err().to_string();
        assert!(err.contains("truncated chunk"));
    }

    #[test]
    fn decode_or_error_includes_code_and_hint_for_stopped_agent() {
        let err = decode_or_error(
            409,
            br#"{"ok":false,"error":"agent default is stopped; resume first","code":"agent_stopped","hint":"resume with `holon control resume --agent default`"}"#.to_vec(),
            "/control/agents/default/prompt",
        )
        .unwrap_err();
        let rendered = err.to_string();
        let typed = err.downcast_ref::<LocalHttpError>().unwrap();
        assert_eq!(typed.status_code, 409);
        assert!(typed.has_code("agent_stopped"));
        assert_eq!(
            typed.hint.as_deref(),
            Some("resume with `holon control resume --agent default`")
        );
        assert!(rendered.contains("agent default is stopped; resume first"));
        assert!(rendered.contains("[agent_stopped]"));
        assert!(rendered.contains("Hint: resume with `holon control resume --agent default`"));
    }

    #[test]
    fn decode_or_error_falls_back_to_plain_error_message() {
        let err = decode_or_error(
            500,
            br#"{"ok":false,"error":"internal exploded"}"#.to_vec(),
            "/control/runtime/status",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("internal exploded"));
        assert!(!err.contains("Hint:"));
    }

    #[test]
    fn event_stream_path_includes_query_parameters() {
        let path = event_stream_path(
            "default",
            &EventStreamRequest {
                since: Some("evt_123".into()),
                last_event_id: Some("evt_122".into()),
                limit: Some(20),
            },
        )
        .unwrap();
        assert_eq!(path, "/agents/default/events?limit=20&since=evt_123");
    }

    #[test]
    fn event_stream_path_encodes_reserved_query_characters() {
        let path = event_stream_path(
            "default",
            &EventStreamRequest {
                since: Some("evt?x=1&y=2".into()),
                last_event_id: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(path, "/agents/default/events?since=evt%3Fx%3D1%26y%3D2");
    }

    #[test]
    fn validate_unix_request_target_rejects_crlf_injection() {
        let err = validate_unix_request_target("/agents/default/events\r\nInjected: yes")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid unix event stream request target"));
    }

    #[test]
    fn validate_header_value_rejects_crlf_injection() {
        let err = validate_header_value("Last-Event-ID", "evt_123\r\nInjected: yes")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid Last-Event-ID header value"));
    }

    #[test]
    fn authorization_header_value_rejects_crlf_injection() {
        let err = authorization_header_value("secret\r\nInjected: yes")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid Authorization header value"));
    }

    #[test]
    fn take_next_sse_frame_supports_lf_and_crlf_delimiters() {
        let mut lf = b"id: evt_1\nevent: ping\ndata: {}\n\nid: evt_2".to_vec();
        let first = take_next_sse_frame(&mut lf).unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(&first).unwrap(),
            "id: evt_1\nevent: ping\ndata: {}"
        );
        assert_eq!(std::str::from_utf8(&lf).unwrap(), "id: evt_2");

        let mut crlf = b"id: evt_1\r\nevent: ping\r\ndata: {}\r\n\r\n".to_vec();
        let first = take_next_sse_frame(&mut crlf).unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(&first).unwrap(),
            "id: evt_1\r\nevent: ping\r\ndata: {}"
        );
        assert!(crlf.is_empty());
    }

    #[test]
    fn parse_sse_frame_ignores_comments_and_multiline_data() {
        let frame = b": heartbeat\nid: evt_123\nevent: message_admitted\ndata: {\"id\":\"evt_123\",\ndata: \"seq\":1,\ndata: \"ts\":\"2026-04-19T08:00:00Z\",\ndata: \"agent_id\":\"default\",\ndata: \"type\":\"message_admitted\",\ndata: \"payload\":{}}\n";
        let parsed = parse_sse_frame(frame).unwrap().unwrap();
        assert_eq!(parsed.id, "evt_123");
        assert_eq!(parsed.event, "message_admitted");
        assert_eq!(parsed.data.event_type, "message_admitted");
        assert_eq!(parsed.data.agent_id, "default");
    }
}
