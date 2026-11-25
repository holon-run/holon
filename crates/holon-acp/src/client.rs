//! ACP wire v1 client: handshake, session lifecycle, prompt streaming, cancel.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::connection::AcpConnection;
use crate::error::AcpError;
use crate::handler::{AcpClientHandler, DenyAllHandler};
use crate::protocol::{
    AgentCapabilities, AuthMethod, InitializeParams, InitializeResult, PromptParams, PromptResult,
    SessionInfo, SessionLoadParams, SessionNewParams, SessionNotification, SessionResumeParams,
    SessionUpdate, ACP_PROTOCOL_VERSION,
};

/// How long a cancelled turn may take to settle in-flight peer updates.
pub const DEFAULT_CANCEL_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cached handshake result.
#[derive(Clone, Debug, Default)]
pub struct Handshake {
    pub protocol_version: u32,
    pub agent_capabilities: AgentCapabilities,
    pub auth_methods: Vec<AuthMethod>,
}

impl Handshake {
    /// Whether the peer declared `session/resume` support.
    pub fn supports_resume(&self) -> bool {
        self.agent_capabilities
            .session_capabilities
            .supports_resume()
    }

    /// Whether the peer declared `session/load` support.
    pub fn supports_load(&self) -> bool {
        self.agent_capabilities.load_session
    }

    pub fn auth_method_ids(&self) -> Vec<String> {
        self.auth_methods
            .iter()
            .map(|method| method.id.clone())
            .collect()
    }
}

impl From<InitializeResult> for Handshake {
    fn from(result: InitializeResult) -> Self {
        Self {
            protocol_version: result.protocol_version,
            agent_capabilities: result.agent_capabilities,
            auth_methods: result.auth_methods,
        }
    }
}

/// ACP client bound to one harness process connection.
///
/// At most one `prompt` runs at a time per connection; that matches the ACP
/// model of one session and one in-flight prompt per harness process.
pub struct AcpClient {
    connection: Arc<AcpConnection>,
    handshake: RwLock<Option<Handshake>>,
    updates: Mutex<mpsc::UnboundedReceiver<SessionNotification>>,
    cancel_settle_timeout: Duration,
    request_timeout: Option<Duration>,
}

impl AcpClient {
    /// Build a client from any bidirectional stream.
    pub fn new<S>(stream: S, handler: Arc<dyn AcpClientHandler>) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (connection, updates) = AcpConnection::spawn(stream, handler);
        Self {
            connection,
            handshake: RwLock::new(None),
            updates: Mutex::new(updates),
            cancel_settle_timeout: DEFAULT_CANCEL_SETTLE_TIMEOUT,
            request_timeout: None,
        }
    }

    /// Build a client from a separate reader and writer.
    ///
    /// Managed harness processes expose stdin and stdout as two distinct
    /// handles, so callers that supervise a child process cannot always
    /// provide one duplex stream.
    pub fn from_split<R, W>(reader: R, writer: W, handler: Arc<dyn AcpClientHandler>) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (connection, updates) = AcpConnection::spawn_split(reader, writer, handler);
        Self {
            connection,
            handshake: RwLock::new(None),
            updates: Mutex::new(updates),
            cancel_settle_timeout: DEFAULT_CANCEL_SETTLE_TIMEOUT,
            request_timeout: None,
        }
    }

    /// Build a client that cancels every permission request.
    pub fn with_deny_all<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        Self::new(stream, Arc::new(DenyAllHandler))
    }

    pub fn with_cancel_settle_timeout(mut self, timeout: Duration) -> Self {
        self.cancel_settle_timeout = timeout;
        self
    }

    /// Apply a deadline to handshake and session lifecycle calls.
    ///
    /// The bound covers `initialize`, `session/new`, `session/resume`, and
    /// `session/load`. It deliberately does not cover `prompt`, because a turn
    /// may legitimately outlive any fixed bound; cancelled turns are bounded by
    /// [`AcpClient::with_cancel_settle_timeout`] instead.
    ///
    /// Defaults to no deadline, so callers that need fail-fast handshakes opt in.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    pub fn connection(&self) -> &Arc<AcpConnection> {
        &self.connection
    }

    /// Cached handshake, available after a successful `initialize`.
    pub async fn handshake(&self) -> Option<Handshake> {
        self.handshake.read().await.clone()
    }

    /// Negotiate wire version 1 and cache the peer capability declarations.
    pub async fn initialize(&self, params: InitializeParams) -> Result<Handshake, AcpError> {
        if params.protocol_version != ACP_PROTOCOL_VERSION {
            return Err(AcpError::protocol(format!(
                "unsupported ACP protocol version {} (client implements {ACP_PROTOCOL_VERSION})",
                params.protocol_version
            )));
        }
        let value =
            serde_json::to_value(&params).map_err(|error| AcpError::protocol(error.to_string()))?;
        let result = self
            .connection
            .request_with_timeout("initialize", value, None, self.request_timeout)
            .await?;
        let result: InitializeResult = serde_json::from_value(result)
            .map_err(|error| AcpError::protocol(format!("invalid initialize result: {error}")))?;
        if result.protocol_version != ACP_PROTOCOL_VERSION {
            return Err(AcpError::protocol(format!(
                "peer selected unsupported ACP protocol version {}",
                result.protocol_version
            )));
        }
        let handshake = Handshake::from(result);
        *self.handshake.write().await = Some(handshake.clone());
        Ok(handshake)
    }

    /// Create a new logical session on the harness.
    pub async fn new_session(&self, params: SessionNewParams) -> Result<SessionInfo, AcpError> {
        self.require_handshake().await?;
        let value =
            serde_json::to_value(&params).map_err(|error| AcpError::protocol(error.to_string()))?;
        let result = self
            .connection
            .request_with_timeout("session/new", value, None, self.request_timeout)
            .await?;
        parse_session_info(result)
    }

    /// Resume an existing session. Preflights the peer's declared capability.
    pub async fn resume_session(
        &self,
        params: SessionResumeParams,
    ) -> Result<SessionInfo, AcpError> {
        let handshake = self.require_handshake().await?;
        if !handshake.supports_resume() {
            return Err(AcpError::UnsupportedByPeer {
                operation: "session/resume",
            });
        }
        let value =
            serde_json::to_value(&params).map_err(|error| AcpError::protocol(error.to_string()))?;
        let result = self
            .connection
            .request_with_timeout(
                "session/resume",
                value,
                Some(&params.session_id),
                self.request_timeout,
            )
            .await?;
        parse_session_info(result)
    }

    /// Load an existing session. Preflights the peer's declared capability.
    pub async fn load_session(&self, params: SessionLoadParams) -> Result<SessionInfo, AcpError> {
        let handshake = self.require_handshake().await?;
        if !handshake.supports_load() {
            return Err(AcpError::UnsupportedByPeer {
                operation: "session/load",
            });
        }
        let value =
            serde_json::to_value(&params).map_err(|error| AcpError::protocol(error.to_string()))?;
        let result = self
            .connection
            .request_with_timeout(
                "session/load",
                value,
                Some(&params.session_id),
                self.request_timeout,
            )
            .await?;
        parse_session_info(result)
    }

    /// Send one prompt and stream `session/update` notifications until the peer
    /// answers with a `stopReason`.
    ///
    /// A cancelled turn that the peer does not settle within the cancel-settle
    /// timeout fails with [`AcpError::Cancelled`] and closes the connection,
    /// because ACP allows only one in-flight prompt per session; rebuild the
    /// client before prompting again.
    pub async fn prompt<F>(
        &self,
        params: PromptParams,
        mut on_update: F,
    ) -> Result<PromptResult, AcpError>
    where
        F: FnMut(&SessionUpdate),
    {
        self.require_handshake().await?;
        let session_id = params.session_id.clone();
        let value =
            serde_json::to_value(&params).map_err(|error| AcpError::protocol(error.to_string()))?;
        let mut cancel_rx = self.connection.subscribe_cancel();
        let mut updates = self.updates.lock().await;

        let request = self
            .connection
            .request_scoped("session/prompt", value, Some(&session_id));
        tokio::pin!(request);

        let mut cancelled = false;
        let mut deadline: Option<Instant> = None;
        loop {
            let deadline_fut = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                result = &mut request => {
                    while let Ok(notification) = updates.try_recv() {
                        if notification.session_id == session_id {
                            on_update(&notification.update);
                        }
                    }
                    let value = result?;
                    return parse_prompt_result(value);
                }
                maybe = updates.recv() => {
                    if let Some(notification) = maybe {
                        if notification.session_id == session_id {
                            on_update(&notification.update);
                        }
                    }
                }
                changed = cancel_rx.changed(), if !cancelled => {
                    if changed.is_ok() {
                        cancelled = true;
                        deadline = Some(Instant::now() + self.cancel_settle_timeout);
                    }
                }
                _ = deadline_fut, if cancelled => {
                    // The peer never settled the cancelled turn. Leaving the
                    // request in flight would allow a second prompt on the same
                    // session and let the old turn's updates leak into the next
                    // callback, so tear the connection down instead.
                    self.connection.close().await;
                    return Err(AcpError::Cancelled);
                }
            }
        }
    }

    /// Cancellation handle for a session; safe to call while `prompt` is running.
    pub fn cancel_handle(&self, session_id: impl Into<String>) -> CancelHandle {
        CancelHandle {
            connection: Arc::clone(&self.connection),
            session_id: session_id.into(),
        }
    }

    async fn require_handshake(&self) -> Result<Handshake, AcpError> {
        self.handshake
            .read()
            .await
            .clone()
            .ok_or_else(|| AcpError::protocol("initialize must complete before session methods"))
    }
}

/// Handle that requests `session/cancel` without borrowing the client mutably.
#[derive(Clone)]
pub struct CancelHandle {
    connection: Arc<AcpConnection>,
    session_id: String,
}

impl CancelHandle {
    /// Send `session/cancel` and resolve pending reverse requests as cancelled.
    ///
    /// Reverse permission/elicitation requests are answered immediately so the
    /// harness is not left waiting on this client while this client waits for
    /// the harness.
    pub async fn cancel(&self) -> Result<(), AcpError> {
        let result = self
            .connection
            .notify(
                "session/cancel",
                serde_json::json!({ "sessionId": self.session_id }),
            )
            .await;
        self.connection.cancel_pending_reverse_requests();
        result
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

fn parse_session_info(value: serde_json::Value) -> Result<SessionInfo, AcpError> {
    let session: SessionInfo = serde_json::from_value(value)
        .map_err(|error| AcpError::protocol(format!("invalid session result: {error}")))?;
    if session.session_id.is_empty() {
        return Err(AcpError::protocol("session result is missing sessionId"));
    }
    Ok(session)
}

fn parse_prompt_result(value: serde_json::Value) -> Result<PromptResult, AcpError> {
    serde_json::from_value(value)
        .map_err(|error| AcpError::protocol(format!("invalid prompt result: {error}")))
}
