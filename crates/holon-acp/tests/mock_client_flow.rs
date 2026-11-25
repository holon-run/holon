//! End-to-end client behaviour against an in-process mock ACP server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holon_acp::handler::{PermissionOutcome, PermissionRequest};
use holon_acp::jsonrpc::{Frame, RpcId};
use holon_acp::protocol::{SessionUpdate, StopReason};
use holon_acp::{
    AcpClient, AcpClientHandler, AcpError, ClientCapabilities, DenyAllHandler, InitializeParams,
    PromptParams, SessionLoadParams, SessionNewParams, SessionResumeParams,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Normal,
    AuthenticationRequired,
    SessionGone,
    PermissionBlocked,
    PermissionSelected,
    PermissionDenied,
    InitializeHangs,
}

#[derive(Clone, Debug)]
enum ServerEvent {
    Initialized(Value),
    PermissionRequested,
    PermissionReply(Value),
    CancelReceived,
}

/// Handler whose permission decision never resolves on its own; only
/// cancellation can unblock it.
#[derive(Debug, Default)]
struct HangingHandler;

#[async_trait]
impl AcpClientHandler for HangingHandler {
    async fn request_permission(&self, _request: PermissionRequest) -> PermissionOutcome {
        std::future::pending().await
    }
}

/// Handler that approves the first offered option.
#[derive(Debug, Default)]
struct AllowOnceHandler;

#[async_trait]
impl AcpClientHandler for AllowOnceHandler {
    async fn request_permission(&self, _request: PermissionRequest) -> PermissionOutcome {
        PermissionOutcome::Selected {
            option_id: "allow_once".into(),
        }
    }
}

fn start(scenario: Scenario) -> MockServer {
    start_with_timeout(scenario, None)
}

type MockServer = (
    Arc<AcpClient>,
    mpsc::UnboundedReceiver<ServerEvent>,
    Arc<Mutex<Vec<String>>>,
);

fn start_with_timeout(scenario: Scenario, request_timeout: Option<Duration>) -> MockServer {
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let methods = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(run_mock(
        server_io,
        scenario,
        events_tx,
        Arc::clone(&methods),
    ));

    let handler: Arc<dyn AcpClientHandler> = match scenario {
        Scenario::PermissionBlocked => Arc::new(HangingHandler),
        Scenario::PermissionSelected => Arc::new(AllowOnceHandler),
        _ => Arc::new(DenyAllHandler),
    };
    let mut client =
        AcpClient::new(client_io, handler).with_cancel_settle_timeout(Duration::from_secs(2));
    if let Some(timeout) = request_timeout {
        client = client.with_request_timeout(timeout);
    }
    (Arc::new(client), events_rx, methods)
}

async fn write_frame(writer: &mut (impl AsyncWriteExt + Unpin), frame: Frame) {
    let mut line = frame.to_line().unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
}

async fn run_mock(
    stream: DuplexStream,
    scenario: Scenario,
    events: mpsc::UnboundedSender<ServerEvent>,
    methods: Arc<Mutex<Vec<String>>>,
) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();
    let mut pending_prompt: Option<RpcId> = None;

    while let Some(line) = lines.next_line().await.unwrap() {
        if line.trim().is_empty() {
            continue;
        }
        let frame = Frame::parse(&line).unwrap();
        match frame {
            Frame::Request { id, method, params } => {
                methods.lock().unwrap().push(method.clone());
                match method.as_str() {
                    "initialize" => {
                        let capabilities = params
                            .as_ref()
                            .and_then(|params| params.get("clientCapabilities"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        events.send(ServerEvent::Initialized(capabilities)).unwrap();
                        if matches!(scenario, Scenario::InitializeHangs) {
                            // Never answer, so the client's handshake deadline fires.
                            continue;
                        }
                        let reply = match scenario {
                            Scenario::AuthenticationRequired => Frame::Error {
                                id,
                                error: holon_acp::RpcError {
                                    code: -32000,
                                    message: "Authentication required".into(),
                                    data: Some(json!({
                                        "authMethods": ["oauth-personal", "gemini-api-key"]
                                    })),
                                },
                            },
                            _ => Frame::Response {
                                id,
                                result: json!({
                                    "protocolVersion": 1,
                                    "agentCapabilities": {
                                        "loadSession": matches!(scenario, Scenario::SessionGone),
                                        // ACP v1 declares resume by presence in
                                        // `agentCapabilities.sessionCapabilities`.
                                        "sessionCapabilities": if matches!(
                                            scenario,
                                            Scenario::SessionGone
                                        ) {
                                            json!({"resume": {}})
                                        } else {
                                            json!({})
                                        },
                                    },
                                    "authMethods": [
                                        {"id": "oauth-personal", "name": "Personal OAuth"},
                                        {"id": "gemini-api-key", "name": "Gemini API key"},
                                    ],
                                }),
                            },
                        };
                        write_frame(&mut write_half, reply).await;
                    }
                    "session/new" => {
                        write_frame(
                            &mut write_half,
                            Frame::Response {
                                id,
                                result: json!({"sessionId": "s1"}),
                            },
                        )
                        .await;
                    }
                    "session/resume" | "session/load" => {
                        let reply = if matches!(scenario, Scenario::SessionGone) {
                            Frame::Error {
                                id,
                                error: holon_acp::RpcError {
                                    code: -32002,
                                    message: "session not found".into(),
                                    data: None,
                                },
                            }
                        } else {
                            Frame::Response {
                                id,
                                result: json!({"sessionId": "s1"}),
                            }
                        };
                        write_frame(&mut write_half, reply).await;
                    }
                    "session/prompt" => match scenario {
                        Scenario::PermissionBlocked
                        | Scenario::PermissionSelected
                        | Scenario::PermissionDenied => {
                            pending_prompt = Some(id);
                            write_frame(
                                &mut write_half,
                                Frame::Request {
                                    id: RpcId::Number(900),
                                    method: "session/request_permission".into(),
                                    params: Some(json!({
                                        "sessionId": "s1",
                                        "toolCall": {"toolCallId": "t1", "title": "write file"},
                                        "options": [
                                            {"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
                                            {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"}
                                        ],
                                    })),
                                },
                            )
                            .await;
                            events.send(ServerEvent::PermissionRequested).unwrap();
                        }
                        _ => {
                            for update in [
                                json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "thinking"}}),
                                json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hello"}}),
                                json!({"sessionUpdate": "tool_call", "toolCall": {"toolCallId": "t1", "status": "completed"}}),
                                json!({"sessionUpdate": "usage_update", "used": 12, "size": 200}),
                            ] {
                                write_frame(
                                    &mut write_half,
                                    Frame::Notification {
                                        method: "session/update".into(),
                                        params: Some(json!({"sessionId": "s1", "update": update})),
                                    },
                                )
                                .await;
                            }
                            write_frame(
                                &mut write_half,
                                Frame::Response {
                                    id,
                                    result: json!({"stopReason": "end_turn"}),
                                },
                            )
                            .await;
                        }
                    },
                    other => {
                        write_frame(
                            &mut write_half,
                            Frame::Error {
                                id,
                                error: holon_acp::RpcError {
                                    code: -32601,
                                    message: format!("unsupported method {other}"),
                                    data: None,
                                },
                            },
                        )
                        .await;
                    }
                }
            }
            Frame::Response { id, result } => {
                if id == RpcId::Number(900) {
                    events.send(ServerEvent::PermissionReply(result)).unwrap();
                    if let Some(prompt_id) = pending_prompt.take() {
                        let stop_reason = if matches!(scenario, Scenario::PermissionSelected) {
                            "end_turn"
                        } else {
                            "cancelled"
                        };
                        write_frame(
                            &mut write_half,
                            Frame::Response {
                                id: prompt_id,
                                result: json!({"stopReason": stop_reason}),
                            },
                        )
                        .await;
                    }
                }
            }
            Frame::Notification { method, .. } => {
                if method == "session/cancel" {
                    events.send(ServerEvent::CancelReceived).unwrap();
                }
            }
            Frame::Error { .. } => {}
        }
    }
}

async fn next_event(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("mock event timed out")
        .expect("mock event channel closed")
}

#[tokio::test]
async fn initialize_new_session_prompt_streams_updates_to_stop_reason() {
    let (client, mut events, _methods) = start(Scenario::Normal);

    let handshake = client
        .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
        .await
        .unwrap();
    assert!(!handshake.supports_resume());
    assert!(!handshake.supports_load());
    assert_eq!(
        handshake.auth_method_ids(),
        vec!["oauth-personal", "gemini-api-key"]
    );

    match next_event(&mut events).await {
        ServerEvent::Initialized(capabilities) => {
            // Trimmed defaults: no `fs`, no `terminal`.
            assert_eq!(capabilities, json!({}));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let session = client
        .new_session(SessionNewParams::new("/work/repo"))
        .await
        .unwrap();
    assert_eq!(session.session_id, "s1");

    let mut updates = Vec::new();
    let result = client
        .prompt(PromptParams::text("s1", "do the work"), |update| {
            updates.push(update.clone())
        })
        .await
        .unwrap();

    assert_eq!(result.stop_reason, StopReason::EndTurn);
    let kinds: Vec<&str> = updates.iter().map(SessionUpdate::kind).collect();
    assert_eq!(
        kinds,
        vec![
            "agent_thought_chunk",
            "agent_message_chunk",
            "tool_call",
            "usage_update"
        ]
    );
}

#[tokio::test]
async fn resume_and_load_are_blocked_before_the_wire_when_undeclared() {
    let (client, mut events, methods) = start(Scenario::Normal);
    client
        .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
        .await
        .unwrap();
    assert!(matches!(
        next_event(&mut events).await,
        ServerEvent::Initialized(_)
    ));

    let resume = client
        .resume_session(SessionResumeParams::new("s1"))
        .await
        .unwrap_err();
    assert!(matches!(
        resume,
        AcpError::UnsupportedByPeer {
            operation: "session/resume"
        }
    ));

    let load = client
        .load_session(SessionLoadParams::new("s1"))
        .await
        .unwrap_err();
    assert!(matches!(
        load,
        AcpError::UnsupportedByPeer {
            operation: "session/load"
        }
    ));

    let methods = methods.lock().unwrap().clone();
    assert!(!methods.iter().any(|method| method == "session/resume"));
    assert!(!methods.iter().any(|method| method == "session/load"));
}

#[tokio::test]
async fn cancel_unblocks_pending_permission_request_without_deadlock() {
    let (client, mut events, _methods) = start(Scenario::PermissionBlocked);
    client
        .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
        .await
        .unwrap();
    assert!(matches!(
        next_event(&mut events).await,
        ServerEvent::Initialized(_)
    ));
    client
        .new_session(SessionNewParams::new("/work/repo"))
        .await
        .unwrap();

    let prompt_client = Arc::clone(&client);
    let prompt = tokio::spawn(async move {
        prompt_client
            .prompt(PromptParams::text("s1", "run"), |_| {})
            .await
    });

    // The mock now waits on our permission decision; cancel must resolve it.
    let mut saw_request = false;
    for _ in 0..10 {
        if matches!(
            next_event(&mut events).await,
            ServerEvent::PermissionRequested
        ) {
            saw_request = true;
            break;
        }
    }
    assert!(saw_request, "permission request never reached the client");

    let handle = client.cancel_handle("s1");
    handle.cancel().await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), prompt)
        .await
        .expect("prompt deadlocked after cancel")
        .expect("prompt task panicked")
        .expect("prompt failed");
    assert_eq!(result.stop_reason, StopReason::Cancelled);

    let mut saw_cancel = false;
    let mut saw_reply = false;
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(300), events.recv()).await {
            Ok(Some(ServerEvent::CancelReceived)) => saw_cancel = true,
            Ok(Some(ServerEvent::PermissionReply(outcome))) => {
                assert_eq!(outcome["outcome"]["outcome"], json!("cancelled"));
                saw_reply = true;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
        if saw_cancel && saw_reply {
            break;
        }
    }
    assert!(saw_cancel, "mock never observed session/cancel");
    assert!(saw_reply, "mock never observed a permission reply");
}

/// Drive one prompt turn that triggers a permission request and return the raw
/// permission reply the mock observed, plus the stop reason the turn settled
/// with.
async fn permission_reply(scenario: Scenario) -> (Value, StopReason) {
    let (client, mut events, _methods) = start(scenario);
    client
        .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
        .await
        .unwrap();
    assert!(matches!(
        next_event(&mut events).await,
        ServerEvent::Initialized(_)
    ));
    client
        .new_session(SessionNewParams::new("/work/repo"))
        .await
        .unwrap();

    let prompt_client = Arc::clone(&client);
    let prompt = tokio::spawn(async move {
        prompt_client
            .prompt(PromptParams::text("s1", "run"), |_| {})
            .await
    });

    let mut reply = None;
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(300), events.recv()).await {
            Ok(Some(ServerEvent::PermissionReply(value))) => {
                reply = Some(value);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    let reply = reply.expect("mock never observed a permission reply");

    let result = tokio::time::timeout(Duration::from_secs(5), prompt)
        .await
        .expect("prompt did not settle")
        .expect("prompt task panicked")
        .expect("prompt failed");
    (reply, result.stop_reason)
}

#[tokio::test]
async fn handler_permission_reply_wraps_selected_outcome_for_wire_v1() {
    let (reply, stop_reason) = permission_reply(Scenario::PermissionSelected).await;
    assert_eq!(
        reply,
        json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}})
    );
    assert_eq!(stop_reason, StopReason::EndTurn);
}

#[tokio::test]
async fn default_handler_permission_reply_wraps_cancelled_outcome() {
    let (reply, stop_reason) = permission_reply(Scenario::PermissionDenied).await;
    assert_eq!(reply, json!({"outcome": {"outcome": "cancelled"}}));
    assert_eq!(stop_reason, StopReason::Cancelled);
}

#[tokio::test]
async fn handshake_timeout_is_typed_and_scoped_to_lifecycle_calls() {
    let (client, _events, _methods) =
        start_with_timeout(Scenario::InitializeHangs, Some(Duration::from_millis(50)));
    let error = client
        .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
        .await
        .expect_err("initialize must time out against a silent peer");
    match error {
        AcpError::Timeout { method } => assert_eq!(method, "initialize"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn authentication_error_is_typed_with_auth_methods() {
    let (client, _events, _methods) = start(Scenario::AuthenticationRequired);
    let error = client
        .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
        .await
        .unwrap_err();
    match error {
        AcpError::AuthenticationRequired {
            auth_methods,
            message,
        } => {
            assert!(message.contains("Authentication"));
            assert_eq!(auth_methods, vec!["oauth-personal", "gemini-api-key"]);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn missing_session_is_classified_as_session_not_found() {
    let (client, _events, _methods) = start(Scenario::SessionGone);
    let handshake = client
        .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
        .await
        .unwrap();
    assert!(handshake.supports_resume());
    assert!(handshake.supports_load());

    client
        .new_session(SessionNewParams::new("/work/repo"))
        .await
        .unwrap();

    let error = client
        .resume_session(SessionResumeParams::new("gone"))
        .await
        .unwrap_err();
    assert!(error.is_session_not_found());
    assert!(error.recovery_hint().is_some());

    let error = client
        .load_session(SessionLoadParams::new("gone"))
        .await
        .unwrap_err();
    assert!(error.is_session_not_found());
}

#[test]
fn harness_spec_serializes_declaratively() {
    let spec = holon_acp::antigravity::profile();
    let value = serde_json::to_value(&spec).unwrap();
    assert_eq!(value["name"], json!("antigravity"));
    assert_eq!(value["program"], json!("agy"));
    assert_eq!(spec.build_launch(&Default::default()).program, "agy");
    let env: BTreeMap<String, String> = BTreeMap::new();
    assert!(spec.filter_env(&env).is_empty());
}
