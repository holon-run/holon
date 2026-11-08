//! #3898 reproduction: deterministic real-HTTP Anthropic SSE stream
//! interruption tests.
//!
//! These tests drive the production `reqwest` client of `AnthropicProvider`
//! against a local raw-TCP SSE server so the failure surfaces through the
//! real transport stack (hyper plus the reqwest response decoder) instead of
//! a mock. They pin down which mechanism currently handles each interruption
//! shape:
//!
//! * chunked-body disconnect mid-stream -> a reqwest body/decode error whose
//!   source chain matches the retryable read-interruption allowlist in
//!   `retry.rs`, so the same route retries and recovers;
//! * the same disconnect under a `ProviderDispatchGuard` (a turn where tool
//!   side effects may already have happened) -> the provider defers recovery
//!   to the runtime instead of silently re-sending the request;
//! * a fully-framed chunked body whose gzip stream is truncated -> a genuine
//!   reqwest `error decoding response body` whose EOF source preserves the
//!   existing connection retry policy;
//! * a complete `message_stop` -> protocol finalization without consuming
//!   later HTTP failures, keep-open connections, or trailing SSE frames.
//! The observed error string alone does not prove whether the upstream
//! interruption occurred before or after protocol completion.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::support::*;
use super::*;
use crate::config::ProviderId;
use crate::provider::{
    is_dispatch_guard_rejected, provider_transport_diagnostics, ProviderDispatchGuard,
};
use flate2::{write::GzEncoder, Compression};
use serde_json::json;
use std::io::Write as _;

const SSE_HEADERS: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";

fn chunk(bytes: &[u8]) -> Vec<u8> {
    [format!("{:X}\r\n", bytes.len()).as_bytes(), bytes, b"\r\n"].concat()
}

fn anthropic_terminal_sse_body() -> Vec<u8> {
    concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":",
        "{\"id\":\"msg_retry_test\",\"type\":\"message\",\"role\":\"assistant\",",
        "\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",",
        "\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",",
        "\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"retry ok\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",",
        "\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    )
    .as_bytes()
    .to_vec()
}

/// 200 + SSE headers + the first chunk of a message stream; the server then
/// hard-closes the socket mid-chunked-body (no terminating 0-chunk).
fn sse_response_disconnecting_after_first_event() -> Vec<u8> {
    let message_start = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":",
        "{\"id\":\"msg_retry_test\",\"type\":\"message\",\"role\":\"assistant\",",
        "\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n"
    )
    .as_bytes();
    let mut response = SSE_HEADERS.to_vec();
    response.extend(chunk(message_start));
    response
}

/// 200 + a complete SSE body framed with an exact content-length, so the
/// second attempt of a retry sequence can succeed cleanly.
fn complete_sse_response() -> Vec<u8> {
    let body = anthropic_terminal_sse_body();
    let mut response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(&body);
    response
}

/// A chunked body that is fully framed (terminating 0-chunk included) but
/// carries a gzip stream with its tail cut off. The HTTP framing is valid, so
/// the failure comes from reqwest's response-body decoder, not the socket.
fn sse_response_with_truncated_gzip_body() -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&anthropic_terminal_sse_body())
        .expect("gzip encode should not fail in-memory");
    let compressed = encoder.finish().expect("gzip finish should not fail");
    // Cut the deflate tail plus the CRC32/ISIZE footer to force a decoder EOF.
    let cut = compressed.len().saturating_sub(12).max(1);
    let truncated = &compressed[..cut];

    let mut response = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-encoding: gzip\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".to_vec();
    response.extend(chunk(truncated));
    response.extend_from_slice(b"0\r\n\r\n");
    response
}

fn anthropic_provider_at(base_url: String) -> Arc<dyn AgentProvider> {
    let mut fixture = test_config(
        "anthropic/claude-sonnet-4-5",
        &[],
        None,
        Some("anthropic-token"),
        false,
    );
    fixture.config.disable_provider_fallback = true;
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .expect("anthropic provider should be configured by test_config")
        .base_url = base_url;
    // Production turns always wrap the transport in FallbackProvider, which owns
    // same-route retry and dispatch-guard recovery deferral. Calling the raw
    // AnthropicProvider would skip both mechanisms.
    build_provider_from_config(&fixture.config)
        .expect("configured anthropic route should build the production provider")
}

#[tokio::test]
async fn anthropic_stream_disconnect_retries_same_route_and_recovers() {
    let base_url = spawn_raw_http_server_bytes_sequence(vec![
        sse_response_disconnecting_after_first_event(),
        complete_sse_response(),
    ])
    .await;
    let provider = anthropic_provider_at(base_url);

    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(provider_turn_request())
        .await
        .expect("chunked-body disconnect should retry the same route and recover");

    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "retry ok"
    ));
    let timeline = diagnostics.expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), 2);
    let first = &timeline.attempts[0];
    assert_eq!(first.provider, "anthropic");
    assert_eq!(first.failure_kind.as_deref(), Some("connection"));
    assert_eq!(first.disposition.as_deref(), Some("retryable"));
    assert_eq!(first.outcome, ProviderAttemptOutcome::Retrying);
    let transport = first
        .transport_diagnostics
        .as_ref()
        .expect("first attempt should carry transport diagnostics");
    assert_eq!(transport.stage, "streaming_response_body");
    // Baseline: the streaming body path currently omits this metadata.
    assert_eq!(transport.streaming, None);
    assert!(
        transport
            .reqwest
            .as_ref()
            .is_some_and(|reqwest| reqwest.is_body || reqwest.is_decode),
        "disconnect should surface as a reqwest body/decode error, got source chain {:?}",
        transport.source_chain
    );
    assert!(
        transport.source_chain.iter().any(|message| {
            let message = message.to_ascii_lowercase();
            message.contains("eof")
                || message.contains("connection reset")
                || message.contains("connection closed")
                || message.contains("connection aborted")
                || message.contains("broken pipe")
                || message.contains("incomplete message")
                || message.contains("error reading a body from connection")
        }),
        "retryable classification should be justified by the reqwest source chain, got {:?}",
        transport.source_chain
    );
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Succeeded
    );
}

#[derive(Debug)]
struct CountingDispatchGuard {
    dispatches: AtomicUsize,
}

impl ProviderDispatchGuard for CountingDispatchGuard {
    fn before_dispatch(&self) -> anyhow::Result<()> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn anthropic_stream_disconnect_with_dispatch_guard_defers_recovery_to_runtime() {
    let base_url = spawn_raw_http_server_bytes_sequence(vec![
        sse_response_disconnecting_after_first_event(),
        complete_sse_response(),
    ])
    .await;
    let provider = anthropic_provider_at(base_url);
    let guard = Arc::new(CountingDispatchGuard {
        dispatches: AtomicUsize::new(0),
    });
    let mut request = provider_turn_request();
    request.dispatch_guard = Some(guard.clone());

    let error = provider
        .complete_turn_with_diagnostics(request)
        .await
        .expect_err("guarded dispatch must not silently re-send the request");

    assert!(!is_dispatch_guard_rejected(&error));
    assert_eq!(
        guard.dispatches.load(Ordering::SeqCst),
        1,
        "guarded requests must perform exactly one HTTP dispatch"
    );
    let timeline = provider_attempt_timeline(&error).expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), 1);
    let only = &timeline.attempts[0];
    assert_eq!(only.provider, "anthropic");
    assert_eq!(only.failure_kind.as_deref(), Some("connection"));
    assert_eq!(only.disposition.as_deref(), Some("retryable"));
    assert_eq!(
        only.outcome,
        ProviderAttemptOutcome::RetriesExhausted,
        "guarded recovery defers the retry decision to the runtime layer"
    );
    assert!(!only.advanced_to_fallback);
    assert_eq!(
        only.transport_diagnostics
            .as_ref()
            .map(|transport| transport.stage.as_str()),
        Some("streaming_response_body")
    );
    // Whether the interrupted turn re-enters as ProviderFailedNeedsRecovery
    // (side effects already possible) or DeferredToFallback is decided by the
    // runtime turn executor, not the provider retry loop; see
    // `runtime/turn/execution.rs` `maybe_defer_provider_lineage_failure`.
}

#[tokio::test]
async fn anthropic_truncated_gzip_stream_body_preserves_connection_retry() {
    let response = sse_response_with_truncated_gzip_body();
    let base_url =
        spawn_raw_http_server_bytes_sequence(vec![response.clone(), response.clone(), response])
            .await;
    let provider = anthropic_provider_at(base_url);

    let error = provider
        .complete_turn(provider_turn_request())
        .await
        .expect_err("truncated gzip stream body must fail the turn");

    let diagnostics =
        provider_transport_diagnostics(&error).expect("error should include transport diagnostics");
    assert!(
        error
            .to_string()
            .contains("Anthropic streaming response body failed"),
        "unexpected error summary: {error}"
    );
    assert!(
        error.to_string().contains("error decoding response body"),
        "expected the production reqwest decoder wording, got: {error}"
    );
    assert_eq!(diagnostics.stage, "streaming_response_body");
    assert!(
        diagnostics
            .reqwest
            .as_ref()
            .is_some_and(|reqwest| reqwest.is_decode),
        "expected a genuine reqwest decode failure, got source chain {:?}",
        diagnostics.source_chain
    );
    // An interrupted compressed body before message_stop still follows the
    // existing connection retry policy. Inspect the actual attempts rather
    // than reclassifying the aggregated all-providers-failed wrapper.
    let timeline = provider_attempt_timeline(&error).expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), 3);
    assert!(timeline.attempts.iter().all(|attempt| {
        attempt.failure_kind.as_deref() == Some("connection")
            && attempt.disposition.as_deref() == Some("retryable")
            && !attempt.advanced_to_fallback
    }));
}

#[tokio::test]
async fn anthropic_message_stop_completes_before_http_body_disconnect() {
    let mut response = SSE_HEADERS.to_vec();
    response.extend(chunk(&anthropic_terminal_sse_body()));
    // The semantic message is complete, but HTTP chunk framing is interrupted.
    // Serve every retry so a later connection refusal cannot hide the body error.
    let base_url =
        spawn_raw_http_server_bytes_sequence(vec![response.clone(), response.clone(), response])
            .await;
    let provider = anthropic_provider_at(base_url);

    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(provider_turn_request())
        .await
        .expect("message_stop must complete the response without waiting for HTTP EOF");

    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "retry ok"
    ));
    assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(response.input_tokens, 1);
    assert_eq!(response.output_tokens, 1);
    assert_eq!(
        diagnostics
            .expect("missing attempt timeline")
            .attempts
            .len(),
        1
    );
}

#[tokio::test]
async fn anthropic_message_stop_completes_while_http_connection_stays_open() {
    let body = anthropic_terminal_sse_body();
    // Split inside the terminal event, not just between SSE events.
    let split = body.len() - 4;
    let mut first = SSE_HEADERS.to_vec();
    first.extend(chunk(&body[..split]));
    let base_url = spawn_raw_http_server_scripted(vec![vec![
        (0, first),
        (10, chunk(&body[split..])),
        // The test must finish before the server supplies HTTP EOF.
        (60_000, b"0\r\n\r\n".to_vec()),
    ]])
    .await;
    let provider = anthropic_provider_at(base_url);
    let (response, diagnostics) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.complete_turn_with_diagnostics(provider_turn_request()),
    )
    .await
    .expect("message_stop must not wait for HTTP EOF")
    .expect("complete SSE response should succeed");

    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "retry ok"
    ));
    assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!((response.input_tokens, response.output_tokens), (1, 1));
    assert_eq!(
        diagnostics
            .expect("missing attempt timeline")
            .attempts
            .len(),
        1
    );
}

#[tokio::test]
async fn anthropic_message_stop_ignores_trailing_events_and_partial_frames() {
    for tail in [
        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"late\"}}\n\n".as_slice(),
        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"wrong\"}}\n\n",
        b"data: {not valid json}\n\n",
        b"data: {\"type\":",
    ] {
        for separate_http_chunk in [false, true] {
            let mut body = anthropic_terminal_sse_body();
            let mut response = SSE_HEADERS.to_vec();
            if separate_http_chunk {
                response.extend(chunk(&body));
                response.extend(chunk(tail));
            } else {
                body.extend_from_slice(tail);
                response.extend(chunk(&body));
            }
            response.extend_from_slice(b"0\r\n\r\n");
            let base_url = spawn_raw_http_server_bytes_sequence(vec![
                response.clone(),
                response.clone(),
                response,
            ])
            .await;
            let provider = anthropic_provider_at(base_url);
            let (response, diagnostics) = provider
                .complete_turn_with_diagnostics(provider_turn_request())
                .await
                .expect("bytes following message_stop cannot change the completed response");

            assert_eq!(response.blocks.len(), 1);
            assert!(matches!(
                &response.blocks[0],
                ModelBlock::Text { text } if text == "retry ok"
            ));
            assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
            assert_eq!((response.input_tokens, response.output_tokens), (1, 1));
            assert_eq!(diagnostics.expect("missing attempt timeline").attempts.len(), 1);
        }
    }
}

#[tokio::test]
async fn anthropic_message_stop_finalizes_tool_json_without_guarded_redispatch() {
    let events = [
        json!({"type":"message_start","message":{"id":"msg_tool","content":[],"usage":{"input_tokens":3}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-1","name":"ProbeTool","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"value\":"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"42}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ];
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    let mut raw_response = SSE_HEADERS.to_vec();
    raw_response.extend(chunk(body.as_bytes()));
    let base_url = spawn_raw_http_server_bytes_sequence(vec![raw_response]).await;
    let provider = anthropic_provider_at(base_url);
    let guard = Arc::new(CountingDispatchGuard {
        dispatches: AtomicUsize::new(0),
    });
    let mut request = provider_turn_request();
    request.dispatch_guard = Some(guard.clone());
    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(request)
        .await
        .expect("completed tool response should not read a later body failure");

    assert_eq!(guard.dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(
        diagnostics
            .expect("missing attempt timeline")
            .attempts
            .len(),
        1
    );
    assert_eq!(response.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!((response.input_tokens, response.output_tokens), (3, 7));
    assert!(matches!(
        &response.blocks[0],
        ModelBlock::ToolUse { id, name, input, .. }
            if id == "tool-1" && name == "ProbeTool" && input == &json!({"value":42})
    ));
}

#[tokio::test]
async fn anthropic_message_stop_still_requires_protocol_finalization() {
    let complete = anthropic_terminal_sse_body();
    let missing_start = b"data: {\"type\":\"message_stop\"}\n\n".as_slice();
    let stop = b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    for (body, expected) in [
        (missing_start, "without message_start"),
        (
            &complete[..complete.len() - stop.len()],
            "without message_stop",
        ),
    ] {
        let response = chunked_http_response(&[body]);
        let base_url = spawn_raw_http_server_bytes_sequence(vec![
            response.clone(),
            response.clone(),
            response,
        ])
        .await;
        let provider = anthropic_provider_at(base_url);
        let error = provider
            .complete_turn(provider_turn_request())
            .await
            .expect_err("message finalization must reject an incomplete protocol");
        assert!(
            format!("{error:#}").contains(expected),
            "unexpected finalization error: {error:#}"
        );
    }
}
