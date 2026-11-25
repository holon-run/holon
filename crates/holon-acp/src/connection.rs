//! NDJSON transport and JSON-RPC multiplexer over one bidirectional stream.
//!
//! Exactly one task owns reading. Outbound requests are matched to responses by
//! id; peer-initiated requests are dispatched to the [`AcpClientHandler`];
//! `session/update` notifications are forwarded to the consumer channel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

use crate::error::{AcpError, RpcError, METHOD_NOT_FOUND, REQUEST_CANCELLED};
use crate::handler::{
    AcpClientHandler, ElicitationRequest, PermissionOutcome, PermissionRequest, PermissionResponse,
};
use crate::jsonrpc::{Frame, RpcId};
use crate::protocol::SessionNotification;

/// Failure delivered to a waiting outbound request.
enum RequestFailure {
    Rpc(RpcError),
    Client(AcpError),
}

type PendingMap = HashMap<u64, oneshot::Sender<Result<Value, RequestFailure>>>;

/// A single ACP connection. Cheap to share behind `Arc`.
pub struct AcpConnection {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Mutex<PendingMap>,
    cancel_tx: watch::Sender<u64>,
    notifications_tx: mpsc::UnboundedSender<SessionNotification>,
    handler: Arc<dyn AcpClientHandler>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl AcpConnection {
    /// Spawn the reader task for `stream` and return the connection plus the
    /// `session/update` receiver. Only protocol frames may travel on `stream`.
    pub fn spawn<S>(
        stream: S,
        handler: Arc<dyn AcpClientHandler>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SessionNotification>)
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, write_half) = tokio::io::split(stream);
        let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = watch::channel(0u64);
        let connection = Arc::new(Self {
            writer: Mutex::new(Box::new(write_half)),
            pending: Mutex::new(HashMap::new()),
            cancel_tx,
            notifications_tx,
            handler,
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });

        let reader_connection = Arc::clone(&connection);
        tokio::spawn(async move {
            reader_connection.read_loop(read_half).await;
        });

        (connection, notifications_rx)
    }

    /// Spawn the reader task for a separate reader/writer pair.
    ///
    /// Managed child processes expose their stdin and stdout as two distinct
    /// handles, so the ACP client cannot always be built from one duplex
    /// stream. This entry point keeps the same framing and multiplexing rules
    /// while letting the caller own the two halves.
    pub fn spawn_split<R, W>(
        reader: R,
        writer: W,
        handler: Arc<dyn AcpClientHandler>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SessionNotification>)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = watch::channel(0u64);
        let connection = Arc::new(Self {
            writer: Mutex::new(Box::new(writer)),
            pending: Mutex::new(HashMap::new()),
            cancel_tx,
            notifications_tx,
            handler,
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });

        let reader_connection = Arc::clone(&connection);
        tokio::spawn(async move {
            reader_connection.read_loop(reader).await;
        });

        (connection, notifications_rx)
    }

    /// Send a request and await its result without session scoping.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.request_scoped(method, params, None).await
    }

    /// Send a request and classify RPC failures with the addressed session.
    pub async fn request_scoped(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, AcpError> {
        self.request_with_timeout(method, params, session_id, None)
            .await
    }

    /// Send a request with an optional per-call deadline.
    ///
    /// The deadline is caller-scoped rather than connection-wide so that a
    /// fail-fast handshake bound never truncates a long prompt turn. On expiry
    /// the pending entry is removed and [`AcpError::Timeout`] is returned.
    pub async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<Value, AcpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write_frame(Frame::Request {
            id: RpcId::Number(id),
            method: method.to_string(),
            params: Some(params),
        })
        .await?;
        let received = match timeout {
            Some(limit) => match tokio::time::timeout(limit, rx).await {
                Ok(received) => received,
                Err(_) => {
                    self.pending.lock().await.remove(&id);
                    return Err(AcpError::Timeout {
                        method: method.to_string(),
                    });
                }
            },
            None => rx.await,
        };
        match received {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(RequestFailure::Rpc(error))) => Err(AcpError::from_rpc(error, session_id)),
            Ok(Err(RequestFailure::Client(error))) => Err(error),
            Err(_) => Err(AcpError::Closed),
        }
    }

    /// Send a notification. Returns only after the frame is flushed.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.write_frame(Frame::Notification {
            method: method.to_string(),
            params: Some(params),
        })
        .await
    }

    /// Subscribe to turn-cancellation signals.
    pub fn subscribe_cancel(&self) -> watch::Receiver<u64> {
        self.cancel_tx.subscribe()
    }

    /// Resolve every pending reverse request as cancelled.
    ///
    /// Reverse handlers observe the watch value change and reply without
    /// waiting for the harness, so a cancel can never deadlock on a permission
    /// request the harness is blocked on.
    pub fn cancel_pending_reverse_requests(&self) {
        let next = self.cancel_tx.borrow().wrapping_add(1);
        let _ = self.cancel_tx.send(next);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Close the connection and fail every pending outbound request.
    ///
    /// ACP has no in-band abort frame. Once a turn is abandoned without the peer
    /// settling it (for example after a cancel-settle timeout), the connection
    /// can no longer guarantee one in-flight prompt per session, so the only
    /// safe recovery is to tear it down and rebuild the harness process.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.fail_pending(RequestFailure::Client(AcpError::Closed))
            .await;
    }

    async fn write_frame(&self, frame: Frame) -> Result<(), AcpError> {
        if self.is_closed() {
            return Err(AcpError::Closed);
        }
        let mut line = frame.to_line()?;
        line.push('\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|error| AcpError::transport(error.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|error| AcpError::transport(error.to_string()))
    }

    async fn read_loop<R>(self: Arc<Self>, read_half: R)
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let mut lines = BufReader::new(read_half).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match Frame::parse(&line) {
                        Ok(Frame::Response { id, result }) => self.resolve(id, Ok(result)).await,
                        Ok(Frame::Error { id, error }) => {
                            self.resolve(id, Err(RequestFailure::Rpc(error))).await
                        }
                        Ok(Frame::Request { id, method, params }) => {
                            self.spawn_reverse_request(id, method, params);
                        }
                        Ok(Frame::Notification { method, params }) => {
                            self.forward_notification(&method, params.as_ref());
                        }
                        Err(error) => {
                            self.fail_pending(RequestFailure::Client(error)).await;
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    self.fail_pending(RequestFailure::Client(AcpError::transport(
                        error.to_string(),
                    )))
                    .await;
                    break;
                }
            }
        }
        self.closed.store(true, Ordering::Relaxed);
        self.fail_pending(RequestFailure::Client(AcpError::Closed))
            .await;
    }

    async fn resolve(&self, id: RpcId, result: Result<Value, RequestFailure>) {
        let RpcId::Number(number) = id else {
            return;
        };
        if let Some(sender) = self.pending.lock().await.remove(&number) {
            let _ = sender.send(result);
        }
    }

    fn forward_notification(&self, method: &str, params: Option<&Value>) {
        if method != "session/update" {
            return;
        }
        match SessionNotification::from_params(params) {
            Ok(Some(notification)) => {
                let _ = self.notifications_tx.send(notification);
            }
            Ok(None) => {}
            Err(_) => {
                // A malformed update is transport-level noise. Dropping it keeps
                // the prompt loop alive rather than failing an unrelated turn.
            }
        }
    }

    fn spawn_reverse_request(self: &Arc<Self>, id: RpcId, method: String, params: Option<Value>) {
        let connection = Arc::clone(self);
        tokio::spawn(async move {
            let mut cancel_rx = connection.subscribe_cancel();
            let epoch = *cancel_rx.borrow();
            let reply = tokio::select! {
                reply = dispatch_reverse_request(&*connection.handler, &method, params.as_ref()) => reply,
                _ = wait_for_cancel(&mut cancel_rx, epoch) => cancelled_reply(&method),
            };
            let frame = match reply {
                ReverseReply::Result(result) => Frame::Response { id, result },
                ReverseReply::Error(error) => Frame::Error { id, error },
            };
            let _ = connection.write_frame(frame).await;
        });
    }

    async fn fail_pending(&self, failure: RequestFailure) {
        let pending = std::mem::take(&mut *self.pending.lock().await);
        for (_, sender) in pending {
            let _ = sender.send(Err(failure.clone()));
        }
    }
}

impl Clone for RequestFailure {
    fn clone(&self) -> Self {
        match self {
            RequestFailure::Rpc(error) => RequestFailure::Rpc(error.clone()),
            RequestFailure::Client(error) => RequestFailure::Client(error.clone()),
        }
    }
}

async fn wait_for_cancel(cancel_rx: &mut watch::Receiver<u64>, epoch: u64) {
    loop {
        if *cancel_rx.borrow() != epoch {
            return;
        }
        if cancel_rx.changed().await.is_err() {
            // Sender dropped: the connection is gone; reply to avoid a hang.
            return;
        }
    }
}

enum ReverseReply {
    Result(Value),
    Error(RpcError),
}

fn cancelled_reply(method: &str) -> ReverseReply {
    match method {
        "session/request_permission" => {
            match serde_json::to_value(PermissionResponse::new(PermissionOutcome::Cancelled)) {
                Ok(value) => ReverseReply::Result(value),
                Err(_) => ReverseReply::Error(RpcError::new(
                    REQUEST_CANCELLED,
                    "request cancelled by client",
                )),
            }
        }
        _ => ReverseReply::Error(RpcError::new(
            REQUEST_CANCELLED,
            "request cancelled by client",
        )),
    }
}

async fn dispatch_reverse_request(
    handler: &dyn AcpClientHandler,
    method: &str,
    params: Option<&Value>,
) -> ReverseReply {
    match method {
        "session/request_permission" => match PermissionRequest::from_params(params) {
            Ok(request) => {
                let outcome: PermissionOutcome = handler.request_permission(request).await;
                match serde_json::to_value(PermissionResponse::new(outcome)) {
                    Ok(value) => ReverseReply::Result(value),
                    Err(error) => ReverseReply::Error(RpcError::new(
                        -32603,
                        format!("failed to serialize permission outcome: {error}"),
                    )),
                }
            }
            Err(error) => ReverseReply::Error(RpcError::new(-32602, error.to_string())),
        },
        "elicitation/create" => match ElicitationRequest::from_params(params) {
            Ok(request) => match handler.create_elicitation(request).await {
                Ok(value) => ReverseReply::Result(value),
                Err(error) => {
                    ReverseReply::Error(RpcError::new(METHOD_NOT_FOUND, error.to_string()))
                }
            },
            Err(error) => ReverseReply::Error(RpcError::new(-32602, error.to_string())),
        },
        other => ReverseReply::Error(RpcError::new(
            METHOD_NOT_FOUND,
            format!("unsupported reverse request `{other}`"),
        )),
    }
}
