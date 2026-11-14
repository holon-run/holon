use std::sync::{Arc, Mutex};

use axum::{extract::Request, routing::post, Router};
use reqwest::header::HeaderMap;
use serde_json::json;

use super::{support::*, *};
use crate::{
    config::{ProviderEndpointId, ProviderId, ProviderRuntimeConfig, ProviderTransportKind},
    provider::{
        transports::OpenAiChatCompletionsProvider, ProviderGenerateImageRequest, ProviderSessionId,
    },
};

fn provider_for(config: &ProviderRuntimeConfig, home: &std::path::Path) -> Box<dyn AgentProvider> {
    match config.transport {
        ProviderTransportKind::OpenAiChatCompletions => Box::new(
            OpenAiChatCompletionsProvider::from_runtime_config(config, "test-model", 1024, home)
                .unwrap(),
        ),
        ProviderTransportKind::OpenAiResponses => {
            Box::new(OpenAiProvider::from_runtime_config(config, "test-model", 1024, home).unwrap())
        }
        ProviderTransportKind::AnthropicMessages => Box::new(
            AnthropicProvider::from_runtime_config(config, "test-model", 1024, home, false)
                .unwrap(),
        ),
        ProviderTransportKind::GeminiGenerateContent => {
            Box::new(GeminiProvider::from_runtime_config(config, "test-model", 1024, home).unwrap())
        }
        other => panic!("unexpected test transport {other:?}"),
    }
}

async fn capture_server() -> (String, Arc<Mutex<Vec<HeaderMap>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let base_url = spawn_test_server(Router::new().fallback(post(move |request: Request| {
        let sink = sink.clone();
        async move {
            let path = request.uri().path().to_owned();
            assert!(!path.contains("/v1/v1/"));
            sink.lock().unwrap().push(request.headers().clone());
            let (content_type, body) = if path.ends_with("/messages") {
                (
                    "text/event-stream",
                    concat!(
                        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"test-model\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\n",
                        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                    ).to_owned(),
                )
            } else if path.ends_with("/images/generations") {
                ("application/json", json!({
                    "data":[{"b64_json":"YWJj","mime_type":"image/png"}]
                }).to_string())
            } else if path.ends_with("/chat/completions") {
                ("application/json", json!({
                    "choices":[{"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":1,"completion_tokens":1}
                }).to_string())
            } else if path.ends_with("/responses") {
                ("application/json", json!({
                    "id":"response-1","status":"completed",
                    "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"OK"}]}],
                    "usage":{"input_tokens":1,"output_tokens":1}
                }).to_string())
            } else {
                ("application/json", json!({
                    "candidates":[{"content":{"role":"model","parts":[{"text":"OK"}]},"finishReason":"STOP"}],
                    "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1}
                }).to_string())
            };
            ([("content-type", content_type)], body)
        }
    }))).await;
    (base_url, captured)
}

#[tokio::test]
async fn inference_headers_reach_all_four_transports_without_shared_client_leaks() {
    let fixture = test_config("openai/test-model", &[], Some("test-key"), None, false);
    let (base_url, captured) = capture_server().await;
    let mut config = fixture.config.providers[&ProviderId::openai()].clone();
    config.base_url = format!("{base_url}/v1");
    config.headers = serde_json::from_value(json!({
        "x-holon-routing-test":"not-for-logs",
        "user-agent":"operator-harness"
    }))
    .unwrap();
    for transport in [
        ProviderTransportKind::OpenAiChatCompletions,
        ProviderTransportKind::OpenAiResponses,
        ProviderTransportKind::AnthropicMessages,
        ProviderTransportKind::GeminiGenerateContent,
    ] {
        config.transport = transport;
        config.base_url = if transport == ProviderTransportKind::AnthropicMessages {
            base_url.clone()
        } else {
            format!("{base_url}/v1")
        };
        let provider = provider_for(&config, &fixture.config.home_dir);
        provider
            .complete_turn(provider_turn_request())
            .await
            .unwrap();
    }
    {
        let headers = captured.lock().unwrap();
        assert_eq!(headers.len(), 4);
        for headers in headers.iter() {
            assert_eq!(headers["x-holon-routing-test"], "not-for-logs");
            assert_eq!(headers["user-agent"], "operator-harness");
            assert!(!headers.contains_key("x-opencode-session"));
        }
    }
    config.headers = Default::default();
    let provider = provider_for(&config, &fixture.config.home_dir);
    provider
        .complete_turn(provider_turn_request())
        .await
        .unwrap();
    assert!(!captured
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .contains_key("x-holon-routing-test"));
}

#[tokio::test]
async fn go_messages_accepts_origin_and_persisted_version_prefix_bases() {
    let fixture = test_config("openai/test-model", &[], Some("test-key"), None, false);
    let (base_url, captured) = capture_server().await;
    let mut config = fixture.config.providers[&ProviderId::openai()].clone();
    config.route_provider = ProviderId::parse("opencode-go").unwrap();
    config.transport = ProviderTransportKind::AnthropicMessages;
    for base in [
        base_url.clone(),
        format!("{base_url}/v1"),
        format!("{base_url}/v1/"),
    ] {
        config.base_url = base;
        let mut request = provider_turn_request();
        request.provider_session_id = Some(ProviderSessionId::from_runtime_identity(
            "installation",
            "agent",
            1,
        ));
        provider_for(&config, &fixture.config.home_dir)
            .complete_turn(request)
            .await
            .unwrap();
    }
    assert_eq!(captured.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn configured_headers_reach_existing_image_generation_paths() {
    let fixture = test_config("openai/test-model", &[], Some("test-key"), None, false);
    let (base_url, captured) = capture_server().await;
    let mut config = fixture.config.providers[&ProviderId::openai()].clone();
    config.base_url = format!("{base_url}/v1");
    config.headers = serde_json::from_value(json!({"x-image-route":"image-route-value"})).unwrap();
    for transport in [
        ProviderTransportKind::OpenAiChatCompletions,
        ProviderTransportKind::OpenAiResponses,
    ] {
        config.transport = transport;
        let response = provider_for(&config, &fixture.config.home_dir)
            .generate_image(ProviderGenerateImageRequest {
                prompt: "synthetic image".into(),
                size: None,
                aspect_ratio: None,
                resolution: None,
                background: None,
                output_format: None,
                material: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(response.images[0].bytes.as_ref(), b"abc");
    }
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    for headers in captured.iter() {
        assert_eq!(headers["x-image-route"], "image-route-value");
    }
}

#[tokio::test]
async fn configured_headers_are_not_forwarded_through_redirects() {
    let fixture = test_config("openai/test-model", &[], Some("test-key"), None, false);
    let redirected = Arc::new(Mutex::new(Vec::<HeaderMap>::new()));
    let sink = redirected.clone();
    let base_url = spawn_test_server(
        Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async {
                    (
                        axum::http::StatusCode::TEMPORARY_REDIRECT,
                        [("location", "/redirect-target")],
                    )
                }),
            )
            .route(
                "/redirect-target",
                post(move |request: Request| {
                    let sink = sink.clone();
                    async move {
                        sink.lock().unwrap().push(request.headers().clone());
                        "must not be reached"
                    }
                }),
            ),
    )
    .await;
    let mut config = fixture.config.providers[&ProviderId::openai()].clone();
    config.base_url = format!("{base_url}/v1");
    config.transport = ProviderTransportKind::OpenAiChatCompletions;
    config.headers = serde_json::from_value(json!({"x-route-only":"not-forwarded"})).unwrap();
    assert!(provider_for(&config, &fixture.config.home_dir)
        .complete_turn(provider_turn_request())
        .await
        .is_err());
    assert!(redirected.lock().unwrap().is_empty());
}

#[tokio::test]
async fn go_session_is_required_before_http_and_stable_across_protocols_and_cache_states() {
    let fixture = test_config("openai/test-model", &[], Some("test-key"), None, false);
    let (base_url, captured) = capture_server().await;
    let mut config = fixture.config.providers[&ProviderId::openai()].clone();
    config.base_url = format!("{base_url}/v1");
    config.route_provider = ProviderId::parse("opencode-go").unwrap();
    let session = ProviderSessionId::from_runtime_identity("installation", "coding-agent", 7);
    for (endpoint, transport) in [
        ("default", ProviderTransportKind::OpenAiChatCompletions),
        ("responses", ProviderTransportKind::OpenAiResponses),
        ("messages", ProviderTransportKind::AnthropicMessages),
    ] {
        config.route_endpoint = ProviderEndpointId::parse(endpoint).unwrap();
        config.transport = transport;
        config.base_url = if transport == ProviderTransportKind::AnthropicMessages {
            base_url.clone()
        } else {
            format!("{base_url}/v1")
        };
        let provider = provider_for(&config, &fixture.config.home_dir);
        let before = captured.lock().unwrap().len();
        let error = provider
            .complete_turn(provider_turn_request())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("trusted provider session"));
        assert_eq!(captured.lock().unwrap().len(), before);
        for mut request in [
            provider_turn_request(),
            provider_turn_request_with_prompt_frame(),
        ] {
            request.provider_session_id = Some(session.clone());
            provider.complete_turn(request.clone()).await.unwrap();
            provider.complete_turn(request).await.unwrap();
        }
    }
    {
        let headers = captured.lock().unwrap();
        assert_eq!(headers.len(), 12);
        for headers in headers.iter() {
            assert_eq!(headers["x-opencode-session"], session.as_str());
            assert_eq!(
                headers["user-agent"],
                format!("holon/{}", env!("CARGO_PKG_VERSION"))
            );
        }
    }
    config.headers = serde_json::from_value(json!({"user-agent":"custom-go-harness"})).unwrap();
    let provider = provider_for(&config, &fixture.config.home_dir);
    let other = ProviderSessionId::from_runtime_identity("installation", "coding-agent", 8);
    let mut request = provider_turn_request();
    request.provider_session_id = Some(other.clone());
    provider.complete_turn(request).await.unwrap();
    let headers = captured.lock().unwrap();
    assert_eq!(headers.last().unwrap()["user-agent"], "custom-go-harness");
    assert_eq!(
        headers.last().unwrap()["x-opencode-session"],
        other.as_str()
    );
}

/// Explicitly opted-in, synthetic coding traffic; at most four paid requests.
/// Credentials use AppConfig's normal resolution and are never printed or copied.
#[tokio::test]
#[ignore = "requires explicit HOLON_LIVE_OPENCODE_GO=1 and a configured Go subscription"]
async fn live_go_messages_responses_tool_round_trip() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var("HOLON_LIVE_OPENCODE_GO").as_deref() == Ok("1"),
        "set HOLON_LIVE_OPENCODE_GO=1 to authorize up to four paid requests"
    );
    let mut config = crate::config::AppConfig::load()?;
    config.runtime_max_output_tokens = 1024;
    super::live_go_comparison::enable_responses_comparison(&mut config)?;
    let model = "glm-5.3-flash";
    let session = ProviderSessionId::from_runtime_identity(
        "synthetic-protocol-test-installation",
        "synthetic-coding-conversation",
        1,
    );
    let mut failures = Vec::new();
    for endpoint in ["messages", "responses"] {
        let route = crate::config::ModelRouteRef::parse_compatible(&format!(
            "opencode-go@{endpoint}/{model}"
        ))?;
        let candidate = super::super::catalog::build_candidate(&config, &route)?;
        let mut request = ProviderTurnRequest::plain(
            "You are reviewing a tiny synthetic Rust function. First call InspectCode exactly once with path src/add.rs. After its result, answer with the corrected function only. Do not use other tools.",
            vec![ConversationMessage::UserText(
                "Review this synthetic source: fn add(a: i32, b: i32) -> i32 { a - b }. It should add. InspectCode returns the test failure; use it before answering.".into(),
            )],
            vec![crate::tool::ToolSpec {
                name: "InspectCode".into(),
                description: "Read a synthetic test result; no actual filesystem access.".into(),
                input_schema: json!({
                    "type":"object",
                    "properties":{"path":{"type":"string"}},
                    "required":["path"],
                    "additionalProperties":false
                }),
                freeform_grammar: None,
            }],
        );
        request.provider_session_id = Some(session.clone());
        for round in 0..2 {
            let started = std::time::Instant::now();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                candidate.provider.complete_turn(request.clone()),
            )
            .await;
            let response = match result {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let status = error
                        .downcast_ref::<crate::provider::retry::ProviderTransportError>()
                        .and_then(|error| error.status);
                    eprintln!("go_live endpoint={endpoint} model={model} round={round} error_class={:?} status={status:?} elapsed_ms={}", crate::provider::retry::classify_provider_error(&error).kind, started.elapsed().as_millis());
                    failures.push(format!("{endpoint} round {round}: provider error"));
                    break;
                }
                Err(_) => {
                    failures.push(format!("{endpoint} round {round}: 120s timeout"));
                    break;
                }
            };
            let calls = response
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ModelBlock::ToolUse {
                        id, name, input, ..
                    } => Some((id.clone(), name.clone(), input.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>();
            eprintln!("go_live endpoint={endpoint} model={model} round={round} elapsed_ms={} input_tokens={} output_tokens={} tool_calls={} stop_reason={:?} diagnostics={}", started.elapsed().as_millis(), response.input_tokens, response.output_tokens, calls.len(), response.stop_reason, serde_json::to_string(&response.request_diagnostics)?);
            if round == 0 {
                if calls.len() != 1
                    || calls[0].1 != "InspectCode"
                    || calls[0].2["path"] != "src/add.rs"
                {
                    failures.push(format!(
                        "{endpoint}: expected one valid synthetic tool call"
                    ));
                    break;
                }
                request
                    .conversation
                    .push(ConversationMessage::AssistantBlocks(response.blocks));
                request.conversation.push(ConversationMessage::UserToolResults(vec![crate::provider::ToolResultBlock {
                    tool_use_id: calls[0].0.clone(),
                    content: "Synthetic test: add(2, 3) returned -1; expected 5. Replace subtraction with addition.".into(),
                    is_error: false,
                    error: None,
                }]));
            } else if !calls.is_empty()
                || !response.blocks.iter().any(
                    |block| matches!(block, ModelBlock::Text { text } if text.contains("a + b")),
                )
            {
                failures.push(format!(
                    "{endpoint}: expected corrected function after tool result"
                ));
            }
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "bounded Go comparison failed: {}",
        failures.join("; ")
    );
    Ok(())
}
