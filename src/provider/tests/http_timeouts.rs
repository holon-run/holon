//! Real HTTP phase boundaries; task-local budgets never mutate process env.

use super::support::*;
use super::*;
use crate::{
    config::ProviderId,
    provider::{
        provider_transport_diagnostics,
        retry::{classify_provider_error, ProviderFailureKind},
        transports::{HttpTimeouts, HTTP_TIMEOUTS_OVERRIDE},
    },
};
use std::time::Duration;

fn budgets() -> HttpTimeouts {
    HttpTimeouts {
        connect: Duration::from_millis(100),
        request: Duration::from_millis(500),
        text_headers: Duration::from_millis(100),
        body: Duration::from_millis(300),
        idle: Duration::from_millis(300),
        total: None,
    }
}

#[derive(Clone, Copy, Debug)]
enum Transport {
    Anthropic,
    Codex,
}

impl Transport {
    fn provider(self, url: &str) -> (ProviderTestFixture, Box<dyn AgentProvider>) {
        match self {
            Self::Anthropic => {
                let mut fixture = test_config(
                    "anthropic/claude-sonnet-4-5",
                    &[],
                    None,
                    Some("test"),
                    false,
                );
                let config = fixture
                    .config
                    .providers
                    .get_mut(&ProviderId::anthropic())
                    .unwrap();
                config.base_url = url.to_string();
                let provider = AnthropicProvider::from_runtime_config(
                    config,
                    "claude-sonnet-4-5",
                    1024,
                    &fixture.config.home_dir,
                    true,
                )
                .unwrap();
                (fixture, Box::new(provider))
            }
            Self::Codex => {
                let mut fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
                fixture
                    .config
                    .providers
                    .get_mut(&ProviderId::openai_codex())
                    .unwrap()
                    .base_url = url.to_string();
                let provider =
                    OpenAiCodexProvider::from_config(&fixture.config, "gpt-5.4").unwrap();
                (fixture, Box::new(provider))
            }
        }
    }

    fn terminal(self) -> String {
        match self {
            Self::Anthropic => concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":",
                "{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",",
                "\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",",
                "\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",",
                "\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",",
                "\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            )
            .to_string(),
            Self::Codex => format!(
                "event: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id":"resp_test","status":"completed","output":[{
                        "type":"message","role":"assistant","content":[{
                            "type":"output_text","text":"ok"
                        }]
                    }]}
                })
            ),
        }
    }
}

const SSE_HEADERS: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";

fn chunk(bytes: &[u8]) -> Vec<u8> {
    [format!("{:x}\r\n", bytes.len()).as_bytes(), bytes, b"\r\n"].concat()
}

fn assert_timeout(error: &anyhow::Error, stage: &str) {
    let diagnostics = provider_transport_diagnostics(error).expect("transport diagnostics");
    assert_eq!(diagnostics.stage, stage, "{error:#}");
    assert_eq!(
        classify_provider_error(error).kind,
        ProviderFailureKind::Timeout,
        "{error:#}"
    );
}

#[tokio::test]
async fn phase_timeout_text_senders_bound_missing_headers() {
    for transport in [Transport::Anthropic, Transport::Codex] {
        HTTP_TIMEOUTS_OVERRIDE
            .scope(budgets(), async {
                // TCP is accepted and the request drained: this is NOT a connect timeout.
                let url = spawn_raw_http_server_scripted(vec![vec![(800, vec![])]]).await;
                let (_fixture, provider) = transport.provider(&url);
                let error = tokio::time::timeout(
                    Duration::from_millis(350),
                    provider.complete_turn(provider_turn_request()),
                )
                .await
                .expect("text headers deadline must precede the old request deadline")
                .unwrap_err();
                assert_timeout(
                    &error,
                    match transport {
                        Transport::Anthropic => "request_send",
                        Transport::Codex => "streaming_request_send",
                    },
                );
            })
            .await;
    }
}

#[tokio::test]
async fn phase_timeout_continuous_stream_outlives_default_request_budget() {
    for transport in [Transport::Anthropic, Transport::Codex] {
        HTTP_TIMEOUTS_OVERRIDE
            .scope(budgets(), async {
                let mut script = vec![(0, SSE_HEADERS.to_vec())];
                for _ in 0..7 {
                    script.push((100, chunk(b": ping\n\n")));
                }
                script.push((0, chunk(transport.terminal().as_bytes())));
                script.push((0, b"0\r\n\r\n".to_vec()));
                let url = spawn_raw_http_server_scripted(vec![script]).await;
                let (_fixture, provider) = transport.provider(&url);
                let response = provider
                    .complete_turn(provider_turn_request())
                    .await
                    .expect("progressing SSE must not inherit a default total deadline");
                assert!(
                    matches!(&response.blocks[0], ModelBlock::Text { text } if text == "ok"),
                    "{transport:?}: {:?}",
                    response.blocks
                );
            })
            .await;
    }
}

#[tokio::test]
async fn phase_timeout_first_body_and_stalled_stream_use_idle_deadline() {
    for transport in [Transport::Anthropic, Transport::Codex] {
        for initial_chunk in [false, true] {
            HTTP_TIMEOUTS_OVERRIDE
                .scope(budgets(), async {
                    let mut script = vec![(0, SSE_HEADERS.to_vec())];
                    if initial_chunk {
                        script.push((0, chunk(b": ping\n\n")));
                    }
                    script.push((800, vec![]));
                    let url = spawn_raw_http_server_scripted(vec![script]).await;
                    let (_fixture, provider) = transport.provider(&url);
                    let error = provider
                        .complete_turn(provider_turn_request())
                        .await
                        .unwrap_err();
                    assert_timeout(&error, "streaming_response_body");
                })
                .await;
        }
    }
}

#[tokio::test]
async fn phase_timeout_json_and_error_bodies_keep_independent_budget() {
    for transport in [Transport::Anthropic, Transport::Codex] {
        for status in ["200 OK", "503 Service Unavailable"] {
            HTTP_TIMEOUTS_OVERRIDE
                .scope(budgets(), async {
                    let headers = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: 1024\r\nconnection: close\r\n\r\n"
                    );
                    let url = spawn_raw_http_server_scripted(vec![vec![
                        (0, headers.into_bytes()),
                        (800, vec![]),
                    ]])
                    .await;
                    let (_fixture, provider) = transport.provider(&url);
                    let error = tokio::time::timeout(
                        Duration::from_millis(650),
                        provider.complete_turn(provider_turn_request()),
                    )
                    .await
                    .expect("body reader must stop at its own budget")
                    .unwrap_err();
                    if status.starts_with("503") {
                        // Preserve status-based retries even if its body cannot be read.
                        let diagnostics = provider_transport_diagnostics(&error).unwrap();
                        assert_eq!(diagnostics.stage, "response_status");
                        assert_eq!(diagnostics.status, Some(503));
                    } else {
                        assert_timeout(
                            &error,
                            match transport {
                                Transport::Anthropic => "response_body",
                                Transport::Codex => "streaming_response_body",
                            },
                        );
                    }
                })
                .await;
        }
    }
}

#[tokio::test]
async fn phase_timeout_cancelling_request_drops_connection_without_retry() {
    use tokio::{io::AsyncReadExt, net::TcpListener, sync::oneshot};
    for transport in [Transport::Anthropic, Transport::Codex] {
        HTTP_TIMEOUTS_OVERRIDE.scope(budgets(), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let (accepted_tx, accepted_rx) = oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                assert!(stream.read(&mut bytes).await.unwrap() > 0);
                accepted_tx.send(()).unwrap();
                while stream.read(&mut bytes).await.unwrap() > 0 {}
                assert!(
                    tokio::time::timeout(Duration::from_millis(150), listener.accept())
                        .await.is_err(),
                    "cancellation must not spawn another attempt"
                );
            });
            let (fixture, _direct_provider) = transport.provider(&url);
            let provider = build_provider_from_config(&fixture.config).unwrap();
            let mut call = Box::pin(provider.complete_turn(provider_turn_request()));
            tokio::select! {
                accepted = accepted_rx => accepted.unwrap(),
                response = &mut call => panic!("unexpected response before cancellation: {response:?}"),
            }
            drop(call);
            tokio::time::timeout(Duration::from_secs(1), server)
                .await.expect("cancelled request must close its connection")
                .unwrap();
        }).await;
    }
}

#[tokio::test]
async fn phase_timeout_explicit_legacy_total_still_caps_progressing_stream() {
    let mut timeouts = budgets();
    timeouts.total = Some(Duration::from_millis(200));
    for transport in [Transport::Anthropic, Transport::Codex] {
        HTTP_TIMEOUTS_OVERRIDE
            .scope(timeouts, async {
                let mut script = vec![(0, SSE_HEADERS.to_vec())];
                for _ in 0..7 {
                    script.push((50, chunk(b": ping\n\n")));
                }
                let url = spawn_raw_http_server_scripted(vec![script]).await;
                let (_fixture, provider) = transport.provider(&url);
                let error = provider
                    .complete_turn(provider_turn_request())
                    .await
                    .unwrap_err();
                assert_timeout(&error, "streaming_response_body");
            })
            .await;
    }
}
