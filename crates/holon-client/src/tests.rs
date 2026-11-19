use super::{
    api_path, authorization_header_value, decode_chunked_body, decode_or_error, event_stream_path,
    normalize_remote_base_url, parse_http_response, parse_sse_frame, read_http_response_body,
    take_next_sse_frame, validate_header_value, validate_unix_request_target, EventPageResponse,
    EventStreamRequest, EventStreamTransport, LocalEventStream, LocalHttpError,
    StreamEventEnvelope, TuiNetworkPolicy, TUI_LOCAL_NETWORK_POLICY, TUI_REMOTE_NETWORK_POLICY,
};
use holon_domain::RuntimeErrorDomain;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[test]
fn api_paths_match_the_nested_router_root() {
    assert_eq!(api_path("/"), "/api");
    assert_eq!(api_path("/api"), "/api");
    assert_eq!(api_path("/agents/list"), "/api/agents/list");
    assert_eq!(api_path("/api/agents/list"), "/api/agents/list");
}

#[tokio::test]
async fn event_page_resource_error_is_not_automatically_retried() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let count = socket.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..count]).unwrap();
            assert!(request.starts_with("GET /api/agents/default/events?"));
            assert!(request.contains("after_seq=4"));
            observed.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"ok":false,"error":"ResourceExhausted: Event-page canonical input","code":"event_page_resource_exhausted","domain":"runtime","retryable":false}"#;
            let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let client = super::LocalClient::remote(
        super::ConnectionConfig {
            home_dir: Default::default(),
            default_agent_id: "default".into(),
            socket_path: Default::default(),
            control_token: None,
            http_addr: addr.to_string(),
        },
        format!("http://{addr}"),
        "event-page-test-token",
    )
    .unwrap();
    let error = client
        .agent_events_page(
            "default",
            super::EventPageRequest {
                after_seq: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    let error = error.downcast_ref::<LocalHttpError>().unwrap();
    assert_eq!(error.status_code, 500);
    assert!(error.has_code("event_page_resource_exhausted"));
    assert_eq!(error.domain, Some(RuntimeErrorDomain::Runtime));
    assert_eq!(error.retryable, Some(false));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[test]
fn event_page_response_accepts_legacy_pages_without_scan_cursors() {
    let page: EventPageResponse = serde_json::from_value(serde_json::json!({
        "events": [],
        "oldest_seq": null,
        "newest_seq": null,
        "cursor_seq": 100,
        "has_older": false,
        "has_newer": true,
        "order": "asc",
        "limit": 128
    }))
    .unwrap();

    assert_eq!(page.next_before_seq, None);
    assert_eq!(page.next_after_seq, None);
    assert_eq!(page.cursor_seq, Some(100));
    assert!(page.has_newer);
}

#[test]
fn event_page_response_preserves_empty_page_scan_cursors() {
    for (order, next_before_seq, next_after_seq) in
        [("desc", Some(40), None), ("asc", None, Some(60))]
    {
        let page: EventPageResponse = serde_json::from_value(serde_json::json!({
            "events": [],
            "oldest_seq": null,
            "newest_seq": null,
            "next_before_seq": next_before_seq,
            "next_after_seq": next_after_seq,
            "cursor_seq": 100,
            "has_older": true,
            "has_newer": true,
            "order": order,
            "limit": 128
        }))
        .unwrap();

        assert_eq!(page.next_before_seq, next_before_seq);
        assert_eq!(page.next_after_seq, next_after_seq);
        assert_eq!(page.oldest_seq, None);
        assert_eq!(page.newest_seq, None);
        assert!(page.has_older && page.has_newer);
    }
}

#[test]
fn remote_connect_url_rejects_unspecified_hosts() {
    assert!(normalize_remote_base_url("http://0.0.0.0:7878".into()).is_err());
    assert!(normalize_remote_base_url("http://[::]:7878".into()).is_err());
}

#[test]
fn remote_connect_url_normalizes_trailing_slash() {
    assert_eq!(
        normalize_remote_base_url("http://example.test:7878/".into()).unwrap(),
        "http://example.test:7878"
    );
}

#[test]
fn remote_policy_has_longer_timeouts_than_local_policy() {
    assert!(TUI_REMOTE_NETWORK_POLICY.connect_timeout > TUI_LOCAL_NETWORK_POLICY.connect_timeout);
    assert!(
        TUI_REMOTE_NETWORK_POLICY.read_idle_timeout > TUI_LOCAL_NETWORK_POLICY.read_idle_timeout
    );
}

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
            br#"{"ok":false,"error":"agent default is stopped; start first","code":"agent_stopped","hint":"start with `holon agent start default`"}"#.to_vec(),
            "/control/agents/default/prompt",
        )
        .unwrap_err();
    let rendered = err.to_string();
    let typed = err.downcast_ref::<LocalHttpError>().unwrap();
    assert_eq!(typed.status_code, 409);
    assert!(typed.has_code("agent_stopped"));
    assert_eq!(
        typed.hint.as_deref(),
        Some("start with `holon agent start default`")
    );
    assert!(rendered.contains("agent default is stopped; start first"));
    assert!(rendered.contains("[agent_stopped]"));
    assert!(rendered.contains("Hint: start with `holon agent start default`"));
}

#[test]
fn decode_or_error_reads_runtime_taxonomy_and_correlation() {
    let err = decode_or_error(
            404,
            br#"{"ok":false,"error":"task task_123 not found","code":"task_not_found","domain":"not_found","retryable":false,"context":{"task_id":"task_123"},"correlation":{"message_id":"msg_123","turn_id":"turn_123"}}"#.to_vec(),
            "/agents/default/tasks/task_123",
        )
        .unwrap_err();
    let typed = err.downcast_ref::<LocalHttpError>().unwrap();

    assert_eq!(typed.domain, Some(RuntimeErrorDomain::NotFound));
    assert_eq!(typed.retryable, Some(false));
    assert_eq!(typed.context["task_id"], "task_123");
    assert_eq!(typed.correlation.message_id.as_deref(), Some("msg_123"));
    assert_eq!(typed.correlation.turn_id.as_deref(), Some("turn_123"));
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
            after_seq: Some(123),
            limit: Some(20),
        },
    )
    .unwrap();
    assert_eq!(
        path,
        "/api/agents/default/events/stream?limit=20&after_seq=123"
    );
}

#[test]
fn event_stream_path_includes_seq_cursor() {
    let path = event_stream_path(
        "default",
        &EventStreamRequest {
            after_seq: Some(42),
            limit: None,
        },
    )
    .unwrap();
    assert_eq!(path, "/api/agents/default/events/stream?after_seq=42");
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
    let err = validate_header_value("Authorization", "Bearer token\r\nInjected: yes")
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid Authorization header value"));
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

#[tokio::test]
async fn event_stream_idle_timeout_after_opened_stream_without_chunks() {
    let (response, server) = open_test_event_stream(Vec::new(), true).await;
    let mut stream = LocalEventStream {
        transport: EventStreamTransport::Http(response),
        frame_buffer: Vec::new(),
        idle_timeout: Duration::from_millis(25),
        contract_version: holon_domain::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
    };

    let err = stream.next_event().await.unwrap_err().to_string();
    server.abort();

    assert!(err.contains("event stream idle timeout after 25ms"));
}

#[tokio::test]
async fn http_body_reader_allows_slow_continuous_chunks() {
    let (response, server) = open_test_event_stream(
        vec![
            ("hello ".into(), Duration::ZERO),
            ("world".into(), Duration::from_millis(20)),
        ],
        false,
    )
    .await;
    let network = TuiNetworkPolicy {
        connect_timeout: Duration::from_millis(50),
        read_idle_timeout: Duration::from_millis(50),
        stream_idle_timeout: Duration::from_millis(50),
    };

    let body = read_http_response_body(response, "/slow-json", network)
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(body, b"hello world");
}

#[tokio::test]
async fn http_body_reader_times_out_on_idle_gap() {
    let (response, server) = open_test_event_stream(Vec::new(), true).await;
    let network = TuiNetworkPolicy {
        connect_timeout: Duration::from_millis(50),
        read_idle_timeout: Duration::from_millis(25),
        stream_idle_timeout: Duration::from_millis(50),
    };

    let err = read_http_response_body(response, "/idle-json", network)
        .await
        .unwrap_err()
        .to_string();
    server.abort();

    assert!(err.contains("read idle timeout after 25ms for /idle-json"));
}

#[tokio::test]
async fn sse_comment_heartbeat_does_not_emit_event_and_preserves_liveness() {
    let event = StreamEventEnvelope {
        projection_effect: None,
        event_log_epoch: Some("epoch-test".into()),
        payload_schema: None,
        payload_schema_version: None,
        id: "evt-1".into(),
        event_seq: 1,
        ts: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        agent_id: "agent-1".into(),
        event_type: "agent_message".into(),
        payload: serde_json::json!({"body": "hello"}),
    };
    let event_frame = format!(
        "id: evt-1\nevent: message\ndata: {}\n\n",
        serde_json::to_string(&event).unwrap()
    );
    let (response, server) = open_test_event_stream(
        vec![
            (": heartbeat\n\n".into(), Duration::ZERO),
            (event_frame, Duration::from_millis(25)),
        ],
        false,
    )
    .await;
    let mut stream = LocalEventStream {
        transport: EventStreamTransport::Http(response),
        frame_buffer: Vec::new(),
        idle_timeout: Duration::from_millis(50),
        contract_version: holon_domain::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
    };

    let received = stream.next_event().await.unwrap();
    server.await.unwrap();

    assert_eq!(received.id, "evt-1");
    assert_eq!(received.event, "message");
}

async fn open_test_event_stream(
    chunks: Vec<(String, Duration)>,
    hold_open: bool,
) -> (reqwest::Response, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 1024];
        let _ = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut request)).await;
        socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
        for (chunk, delay) in chunks {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            write_http_chunk(&mut socket, chunk.as_bytes()).await;
        }
        if hold_open {
            tokio::time::sleep(Duration::from_secs(60)).await;
        } else {
            socket.write_all(b"0\r\n\r\n").await.unwrap();
        }
    });
    let response = reqwest::Client::new()
        .get(format!("http://{addr}/events"))
        .send()
        .await
        .unwrap();

    (response, server)
}

async fn write_http_chunk(socket: &mut TcpStream, body: &[u8]) {
    socket
        .write_all(format!("{:X}\r\n", body.len()).as_bytes())
        .await
        .unwrap();
    socket.write_all(body).await.unwrap();
    socket.write_all(b"\r\n").await.unwrap();
}

#[test]
fn parse_sse_frame_ignores_comments_and_multiline_data() {
    let frame = b": heartbeat\nid: 1\nevent: message_admitted\ndata: {\"id\":\"evt_123\",\ndata: \"event_seq\":1,\ndata: \"ts\":\"2026-04-19T08:00:00Z\",\ndata: \"agent_id\":\"default\",\ndata: \"type\":\"message_admitted\",\ndata: \"payload\":{}}\n";
    let parsed = parse_sse_frame(
        frame,
        holon_domain::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
    )
    .unwrap()
    .unwrap();
    assert_eq!(parsed.id, "1");
    assert_eq!(parsed.event, "message_admitted");
    assert_eq!(parsed.data.event_type, "message_admitted");
    assert_eq!(parsed.data.agent_id, "default");
}
