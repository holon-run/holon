//! Provider routing, fallback, auth, and doctor tests.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::support::*;
use super::*;
use crate::config::{
    CredentialKind, CredentialSource, ModelRef, ProviderAuthConfig, ProviderId,
    ProviderRuntimeConfig, ProviderTransportKind,
};
use crate::provider::retry::{
    classify_provider_error, ProviderFailureKind, RetryDisposition, PROVIDER_MAX_RETRIES,
};
use axum::{extract::State, http::header, response::IntoResponse, routing::post, Json, Router};
use serde_json::{json, Value};

#[test]
fn build_candidate_routes_openai_model_refs() {
    let fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    let config = &fixture.config;
    let model_ref = ModelRef::parse("openai/gpt-5.4").unwrap();
    let candidate = build_candidate(config, &model_ref).unwrap();
    assert_eq!(candidate.model_ref, "openai/gpt-5.4");

    let provider = OpenAiProvider::from_config(config, "gpt-5.4").unwrap();
    let _typed: Arc<dyn super::super::AgentProvider> = Arc::new(provider);
}

#[test]
fn build_candidate_routes_anthropic_model_refs() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[],
        None,
        Some("anthropic-token"),
        false,
    );
    let config = &fixture.config;
    let model_ref = ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap();
    let candidate = build_candidate(config, &model_ref).unwrap();
    assert_eq!(candidate.model_ref, "anthropic/claude-sonnet-4-6");

    let provider = AnthropicProvider::from_config_with_model(config, "claude-sonnet-4-6").unwrap();
    let _typed: Arc<dyn super::super::AgentProvider> = Arc::new(provider);
}

#[test]
fn build_candidate_routes_codex_model_refs() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let config = &fixture.config;
    let model_ref = ModelRef::parse("openai-codex/gpt-5.4").unwrap();
    let candidate = build_candidate(config, &model_ref).unwrap();
    assert_eq!(candidate.model_ref, "openai-codex/gpt-5.4");

    let provider = OpenAiCodexProvider::from_config(config, "gpt-5.4").unwrap();
    let _typed: Arc<dyn super::super::AgentProvider> = Arc::new(provider);
}

#[test]
fn build_candidate_reports_missing_auth_for_each_provider() {
    let openai = test_config("openai/gpt-5.4", &[], None, None, false);
    let openai_err = build_candidate(&openai.config, &ModelRef::parse("openai/gpt-5.4").unwrap())
        .err()
        .expect("missing OPENAI_API_KEY should fail openai candidate");
    assert!(openai_err.to_string().contains("missing OPENAI_API_KEY"));

    let anthropic = test_config("anthropic/claude-sonnet-4-6", &[], None, None, false);
    let anthropic_err = build_candidate(
        &anthropic.config,
        &ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
    )
    .err()
    .expect("missing ANTHROPIC_AUTH_TOKEN should fail anthropic candidate");
    assert!(anthropic_err
        .to_string()
        .contains("missing ANTHROPIC_AUTH_TOKEN"));

    let codex = test_config("openai-codex/gpt-5.4", &[], None, None, false);
    let codex_err = build_candidate(
        &codex.config,
        &ModelRef::parse("openai-codex/gpt-5.4").unwrap(),
    )
    .err()
    .expect("missing Codex auth should fail codex candidate");
    assert!(codex_err
        .to_string()
        .contains("no Codex CLI credentials found"));
}

#[test]
fn build_provider_from_config_uses_effective_fallback_order() {
    let fixture = test_config(
        "openai-codex/gpt-5.4",
        &["openai/gpt-5.4", "anthropic/claude-sonnet-4-6"],
        None,
        Some("anthropic-token"),
        true,
    );
    let config = &fixture.config;
    let _provider = build_provider_from_config(config).unwrap();

    let doctor = provider_doctor(config);
    let providers = doctor["providers"].as_array().unwrap();
    assert_eq!(providers[0]["model"], "openai-codex/gpt-5.4");
    assert_eq!(providers[0]["availability"]["available"], Value::Bool(true));
    assert_eq!(providers[1]["model"], "openai/gpt-5.4");
    assert_eq!(
        providers[1]["availability"]["available"],
        Value::Bool(false)
    );
    assert_eq!(providers[2]["model"], "anthropic/claude-sonnet-4-6");
    assert_eq!(providers[2]["availability"]["available"], Value::Bool(true));
}

#[test]
fn build_provider_from_config_fails_when_no_provider_is_available() {
    let fixture = test_config(
        "openai-codex/gpt-5.4",
        &["openai/gpt-5.4", "anthropic/claude-sonnet-4-6"],
        None,
        None,
        false,
    );
    let err = build_provider_from_config(&fixture.config)
        .err()
        .expect("missing all provider auth should fail provider build");
    assert!(err
        .to_string()
        .contains("no available providers for configured model chain"));
    assert!(err.to_string().contains("openai-codex/gpt-5.4"));
    assert!(err.to_string().contains("openai/gpt-5.4"));
    assert!(err.to_string().contains("anthropic/claude-sonnet-4-6"));
}

#[test]
fn fallback_provider_reports_conservative_prompt_capability_intersection() {
    let fixture = test_config(
        "openai/gpt-5.4",
        &["anthropic/claude-sonnet-4-6"],
        Some("sk-test-key"),
        Some("sk-ant-test-token"),
        false,
    );
    let provider = build_provider_from_config(&fixture.config).unwrap();

    assert_eq!(
        provider.prompt_capabilities(),
        vec![ProviderPromptCapability::FullRequestOnly]
    );
}

#[test]
fn provider_doctor_reports_success_and_failure_paths() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &["openai/gpt-5.4", "openai-codex/gpt-5.4"],
        None,
        Some("anthropic-token"),
        false,
    );
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 3);
    assert_eq!(providers[0]["availability"]["available"], Value::Bool(true));
    assert_eq!(
        providers[1]["availability"]["available"],
        Value::Bool(false)
    );
    assert_eq!(
        providers[2]["availability"]["available"],
        Value::Bool(false)
    );
    assert!(providers[1]["availability"]["error"]
        .as_str()
        .unwrap()
        .contains("missing OPENAI_API_KEY"));
    assert!(providers[2]["availability"]["error"]
        .as_str()
        .unwrap()
        .contains("no Codex CLI credentials found"));
}

#[test]
fn provider_doctor_reports_when_provider_fallback_is_disabled() {
    let mut fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &["openai/gpt-5.4", "openai-codex/gpt-5.4"],
        Some("openai-key"),
        Some("anthropic-token"),
        true,
    );
    fixture
        .config
        .stored_config
        .runtime
        .disable_provider_fallback = Some(true);
    fixture.config.disable_provider_fallback = true;

    let doctor = provider_doctor(&fixture.config);
    assert_eq!(doctor["disable_provider_fallback"], Value::Bool(true));
    let providers = doctor["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0]["model"], "anthropic/claude-sonnet-4-6");
}

#[test]
fn provider_doctor_reports_codex_as_available_when_credentials_exist() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let doctor = provider_doctor(&fixture.config);
    assert_eq!(
        doctor["retry_policy"]["max_retries_per_provider"],
        Value::from(PROVIDER_MAX_RETRIES as u64)
    );
    let provider = doctor["providers"].as_array().unwrap().first().unwrap();
    assert_eq!(provider["provider"], "openai-codex");
    assert_eq!(provider["availability"]["available"], Value::Bool(true));
    assert_eq!(
        provider["availability"]["credential"]["account_id"],
        Value::String("acct_test".to_string())
    );
}

#[test]
fn provider_doctor_reports_codex_credential_errors_separately() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, false);
    let doctor = provider_doctor(&fixture.config);
    let provider = doctor["providers"].as_array().unwrap().first().unwrap();
    assert_eq!(provider["availability"]["available"], Value::Bool(false));
    assert!(!provider["availability"]["credential_error"]
        .as_str()
        .unwrap()
        .is_empty());
}

#[derive(Clone, Default)]
struct StreamingRequestCapture {
    request_body: Arc<std::sync::Mutex<Option<Value>>>,
}

#[tokio::test]
async fn openai_codex_provider_sends_streaming_requests_and_parses_terminal_response() {
    async fn handler(
        State(capture): State<StreamingRequestCapture>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        *capture.request_body.lock().unwrap() = Some(body);
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":11,\"output_tokens\":7},\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]},{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"ExecCommand\",\"arguments\":\"{\\\"cmd\\\":\\\"sed -n '1,40p' src/main.rs\\\",\\\"workdir\\\":\\\".\\\"}\"}]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ignored-tail\"}\n\n",
                "data: [DONE]\n\n"
            ),
        )
    }

    let capture = StreamingRequestCapture::default();
    let router = Router::new()
        .route("/codex/responses", post(handler))
        .with_state(capture.clone());
    let server = spawn_test_server(router).await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = server;

    let provider = OpenAiCodexProvider::from_config(&config, "gpt-5.4").unwrap();
    let response = provider
        .complete_turn(provider_turn_request())
        .await
        .unwrap();

    let request_body = capture.request_body.lock().unwrap().clone().unwrap();
    assert_eq!(request_body["stream"], Value::Bool(true));
    assert!(request_body.get("max_output_tokens").is_none());
    assert_eq!(response.input_tokens, 11);
    assert_eq!(response.output_tokens, 7);
    assert_eq!(response.blocks.len(), 2);
    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "done"
    ));
    assert!(matches!(
        &response.blocks[1],
        ModelBlock::ToolUse { id, name, input }
        if id == "call_1" && name == "ExecCommand" && input["cmd"] == "sed -n '1,40p' src/main.rs"
    ));
}

#[derive(Clone, Default)]
struct JsonRequestCapture {
    request_body: Arc<std::sync::Mutex<Option<Value>>>,
}

#[tokio::test]
async fn openai_provider_sends_sleep_schema_in_request_payload() {
    async fn handler(
        State(capture): State<JsonRequestCapture>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        *capture.request_body.lock().unwrap() = Some(body);
        Json(json!({
            "status": "completed",
            "usage": { "input_tokens": 3, "output_tokens": 1 },
            "output": [
                {
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "ok" }]
                }
            ]
        }))
    }

    let capture = JsonRequestCapture::default();
    let router = Router::new()
        .route("/responses", post(handler))
        .with_state(capture.clone());
    let server = spawn_test_server(router).await;

    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = server;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();
    provider
        .complete_turn(provider_turn_request_with_tools(vec![sleep_tool_spec()]))
        .await
        .unwrap();

    let request_body = capture.request_body.lock().unwrap().clone().unwrap();
    let parameters = &request_body["tools"][0]["parameters"];
    assert_eq!(request_body["tools"][0]["name"], "Sleep");
    assert_eq!(request_body["tools"][0]["strict"], Value::Bool(false));
    assert!(parameters["properties"].get("reason").is_some());
    assert_eq!(parameters["required"], json!([]));
    assert_eq!(parameters["additionalProperties"], Value::Bool(false));
    validate_emitted_tool_schema(parameters, ToolSchemaContract::Relaxed).unwrap();
}

#[tokio::test]
async fn openai_provider_sends_spawn_agent_schema_without_top_level_composition() {
    async fn handler(
        State(capture): State<JsonRequestCapture>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        *capture.request_body.lock().unwrap() = Some(body);
        Json(json!({
            "status": "completed",
            "usage": { "input_tokens": 3, "output_tokens": 1 },
            "output": [
                {
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "ok" }]
                }
            ]
        }))
    }

    let capture = JsonRequestCapture::default();
    let router = Router::new()
        .route("/responses", post(handler))
        .with_state(capture.clone());
    let server = spawn_test_server(router).await;

    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = server;
    let provider = OpenAiProvider::from_config(&fixture.config, "gpt-5.4").unwrap();
    let spawn_agent = trusted_tool_specs()
        .into_iter()
        .find(|spec| spec.name == "SpawnAgent")
        .expect("SpawnAgent tool should be present");

    provider
        .complete_turn(provider_turn_request_with_tools(vec![spawn_agent]))
        .await
        .unwrap();

    let request_body = capture.request_body.lock().unwrap().clone().unwrap();
    let parameters = &request_body["tools"][0]["parameters"];
    assert_eq!(request_body["tools"][0]["name"], "SpawnAgent");
    assert_eq!(parameters["type"], "object");
    for forbidden in ["allOf", "anyOf", "oneOf", "enum", "not"] {
        assert!(
            parameters.get(forbidden).is_none(),
            "OpenAI SpawnAgent schema should not contain top-level {forbidden}: {parameters}"
        );
    }
    validate_emitted_tool_schema(parameters, ToolSchemaContract::Relaxed).unwrap();
}

#[tokio::test]
async fn openai_codex_provider_sends_sleep_schema_in_request_payload() {
    async fn handler(
        State(capture): State<StreamingRequestCapture>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        *capture.request_body.lock().unwrap() = Some(body);
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1},\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n",
                "data: [DONE]\n\n"
            ),
        )
    }

    let capture = StreamingRequestCapture::default();
    let router = Router::new()
        .route("/codex/responses", post(handler))
        .with_state(capture.clone());
    let server = spawn_test_server(router).await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = server;

    let provider = OpenAiCodexProvider::from_config(&config, "gpt-5.4").unwrap();
    provider
        .complete_turn(provider_turn_request_with_tools(vec![sleep_tool_spec()]))
        .await
        .unwrap();

    let request_body = capture.request_body.lock().unwrap().clone().unwrap();
    let parameters = &request_body["tools"][0]["parameters"];
    assert_eq!(request_body["tools"][0]["name"], "Sleep");
    assert_eq!(request_body["tools"][0]["strict"], Value::Bool(false));
    assert!(parameters["properties"].get("reason").is_some());
    assert_eq!(parameters["required"], json!([]));
    assert_eq!(parameters["additionalProperties"], Value::Bool(false));
    validate_emitted_tool_schema(parameters, ToolSchemaContract::Relaxed).unwrap();
}

#[tokio::test]
async fn openai_codex_provider_reconstructs_output_from_stream_items() {
    async fn handler(Json(_body): Json<Value>) -> impl IntoResponse {
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"stream item ok\"}]}}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
                "data: [DONE]\n\n"
            ),
        )
    }

    let router = Router::new().route("/codex/responses", post(handler));
    let server = spawn_test_server(router).await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = server;

    let provider = OpenAiCodexProvider::from_config(&config, "gpt-5.4").unwrap();
    let response = provider
        .complete_turn(provider_turn_request())
        .await
        .unwrap();

    assert_eq!(response.input_tokens, 3);
    assert_eq!(response.output_tokens, 1);
    assert_eq!(response.blocks.len(), 1);
    assert!(matches!(
        &response.blocks[0],
        ModelBlock::Text { text } if text == "stream item ok"
    ));
}

#[tokio::test]
async fn openai_codex_provider_classifies_response_failed_as_contract_error() {
    async fn handler(Json(_body): Json<Value>) -> impl IntoResponse {
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "event: response.failed\n",
                "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_1\",\"status\":\"failed\",\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"input too long\"}}}\n\n",
                "data: [DONE]\n\n"
            ),
        )
    }

    let router = Router::new().route("/codex/responses", post(handler));
    let server = spawn_test_server(router).await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = server;

    let provider = OpenAiCodexProvider::from_config(&config, "gpt-5.4").unwrap();
    let err = provider
        .complete_turn(provider_turn_request())
        .await
        .err()
        .expect("response.failed should fail");

    let classification = classify_provider_error(&err);
    assert_eq!(classification.kind, ProviderFailureKind::ContractError);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
    assert!(err.to_string().contains("context_length_exceeded"));
    assert!(err.to_string().contains("input too long"));
    assert!(!err.to_string().contains("\"response\""));
}

#[tokio::test]
async fn openai_codex_provider_retries_rate_limited_stream_failures() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/codex/responses",
        post(move || {
            let attempts = server_attempts.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        concat!(
                            "event: response.failed\n",
                            "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_rate_limited\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"try again in 2.5s\"}}}\n\n",
                            "data: [DONE]\n\n"
                        ),
                    )
                        .into_response()
                } else {
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        concat!(
                            "event: response.output_item.done\n",
                            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"retry ok\"}]}}\n\n",
                            "event: response.completed\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ok\",\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n",
                            "data: [DONE]\n\n"
                        ),
                    )
                        .into_response()
                }
            }
        }),
    ))
    .await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = base_url;

    let provider = build_provider_from_config(&config).unwrap();
    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(provider_turn_request())
        .await
        .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(response.blocks.len(), 1);
    match &response.blocks[0] {
        ModelBlock::Text { text } => assert_eq!(text, "retry ok"),
        other => panic!("expected text block, got {other:?}"),
    }

    let timeline = diagnostics.expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), 2);
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("rate_limited")
    );
    assert_eq!(
        timeline.attempts[0].disposition.as_deref(),
        Some("retryable")
    );
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::Retrying
    );
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Succeeded
    );
}

#[tokio::test]
async fn openai_codex_provider_fails_when_streamed_output_item_count_exceeds_limit() {
    async fn handler(Json(_body): Json<Value>) -> impl IntoResponse {
        let mut body = String::new();
        for idx in 0..129 {
            body.push_str("event: response.output_item.done\n");
            body.push_str(&format!(
                "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"item-{idx}\"}}]}}}}\n\n"
            ));
        }
        body.push_str("event: response.completed\n");
        body.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
        );
        body.push_str("data: [DONE]\n\n");
        ([(header::CONTENT_TYPE, "text/event-stream")], body)
    }

    let router = Router::new().route("/codex/responses", post(handler));
    let server = spawn_test_server(router).await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = server;

    let provider = OpenAiCodexProvider::from_config(&config, "gpt-5.4").unwrap();
    let err = provider
        .complete_turn(provider_turn_request())
        .await
        .err()
        .expect("streamed output item overflow should fail");

    let classification = classify_provider_error(&err);
    assert_eq!(classification.kind, ProviderFailureKind::InvalidResponse);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
    assert!(err
        .to_string()
        .contains("received more than 128 streamed output items"));
}

#[tokio::test]
async fn openai_codex_provider_fails_if_done_arrives_before_terminal_response() {
    async fn handler(Json(_body): Json<Value>) -> impl IntoResponse {
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
                "data: [DONE]\n\n",
            ),
        )
    }

    let router = Router::new().route("/codex/responses", post(handler));
    let server = spawn_test_server(router).await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = server;

    let provider = OpenAiCodexProvider::from_config(&config, "gpt-5.4").unwrap();
    let err = provider
        .complete_turn(provider_turn_request())
        .await
        .err()
        .expect("done before terminal response should fail");

    let classification = classify_provider_error(&err);
    assert_eq!(classification.kind, ProviderFailureKind::InvalidResponse);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
    assert!(err
        .to_string()
        .contains("[DONE] observed before terminal response"));
}

#[tokio::test]
async fn openai_codex_provider_fails_if_done_arrives_at_eof_before_terminal_response() {
    async fn handler(Json(_body): Json<Value>) -> impl IntoResponse {
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
                "data: [DONE]\n",
            ),
        )
    }

    let router = Router::new().route("/codex/responses", post(handler));
    let server = spawn_test_server(router).await;

    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let mut config = fixture.config.clone();
    config
        .providers
        .get_mut(&ProviderId::openai_codex())
        .unwrap()
        .base_url = server;

    let provider = OpenAiCodexProvider::from_config(&config, "gpt-5.4").unwrap();
    let err = provider
        .complete_turn(provider_turn_request())
        .await
        .err()
        .expect("done at EOF before terminal response should fail");

    let classification = classify_provider_error(&err);
    assert_eq!(classification.kind, ProviderFailureKind::InvalidResponse);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
    assert!(err
        .to_string()
        .contains("[DONE] observed before terminal response"));
}

#[tokio::test]
async fn openai_provider_retries_transient_server_errors() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || {
            let attempts = server_attempts.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "temporary failure",
                    )
                        .into_response()
                } else {
                    Json(json!({
                        "status": "completed",
                        "usage": { "input_tokens": 3, "output_tokens": 2 },
                        "output": [
                            {
                                "type": "message",
                                "content": [{ "type": "output_text", "text": "retry ok" }]
                            }
                        ]
                    }))
                    .into_response()
                }
            }
        }),
    ))
    .await;

    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = build_provider_from_config(&fixture.config).unwrap();
    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(provider_turn_request())
        .await
        .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(response.blocks.len(), 1);
    let timeline = diagnostics.expect("missing attempt timeline");
    assert_eq!(
        timeline.winning_model_ref.as_deref(),
        Some("openai/gpt-5.4")
    );
    assert_eq!(
        timeline
            .aggregated_token_usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(5)
    );
    assert_eq!(timeline.attempts.len(), 2);
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::Retrying
    );
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Succeeded
    );
    match &response.blocks[0] {
        ModelBlock::Text { text } => assert_eq!(text, "retry ok"),
        _ => panic!("expected text block"),
    }
}

#[tokio::test]
async fn openai_provider_fails_fast_on_contract_errors() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || {
            let attempts = server_attempts.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                (axum::http::StatusCode::BAD_REQUEST, "bad request").into_response()
            }
        }),
    ))
    .await;

    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    let provider = build_provider_from_config(&fixture.config).unwrap();
    let error = provider
        .complete_turn(provider_turn_request())
        .await
        .err()
        .expect("400 should fail fast without retry");

    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(error
        .to_string()
        .contains("fail_fast (contract_error, status=400)"));
    let timeline = provider_attempt_timeline(&error).expect("missing attempt timeline");
    assert!(timeline.aggregated_token_usage.is_none());
    assert_eq!(timeline.winning_model_ref, None);
    assert_eq!(timeline.attempts.len(), 1);
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::FailFastAborted
    );
    assert!(!timeline.attempts[0].advanced_to_fallback);
}

#[tokio::test]
async fn openai_provider_preserves_structured_unknown_transport_diagnostics() {
    let response = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{\"id\":\"resp_partial\"";
    let base_url = spawn_raw_http_server(response).await;

    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = base_url;
    fixture
        .config
        .stored_config
        .runtime
        .disable_provider_fallback = Some(true);
    fixture.config.disable_provider_fallback = true;

    let provider = build_provider_from_config(&fixture.config).unwrap();
    let error = provider
        .complete_turn(provider_turn_request())
        .await
        .err()
        .expect("truncated response body should fail");

    assert!(error.to_string().contains("fail_fast (unknown)"));
    let diagnostics =
        provider_transport_diagnostics(&error).expect("missing structured transport diagnostics");
    assert_eq!(diagnostics.stage, "response_body");
    assert_eq!(diagnostics.provider.as_deref(), Some("openai"));
    assert_eq!(diagnostics.model_ref.as_deref(), Some("openai/gpt-5.4"));
    assert!(diagnostics
        .url
        .as_deref()
        .unwrap_or_default()
        .ends_with("/responses"));
    assert!(diagnostics.reqwest.is_some());
    assert!(!diagnostics.source_chain.is_empty());

    let timeline = provider_attempt_timeline(&error).expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), 1);
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("unknown")
    );
    assert_eq!(
        timeline.attempts[0]
            .transport_diagnostics
            .as_ref()
            .map(|diag| diag.stage.as_str()),
        Some("response_body")
    );
}

#[tokio::test]
async fn provider_fallback_continues_after_retry_exhaustion() {
    let openai_attempts = Arc::new(AtomicUsize::new(0));
    let openai_server_attempts = openai_attempts.clone();
    let openai_base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || {
            let attempts = openai_server_attempts.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "server unavailable",
                )
                    .into_response()
            }
        }),
    ))
    .await;

    let anthropic_attempts = Arc::new(AtomicUsize::new(0));
    let anthropic_server_attempts = anthropic_attempts.clone();
    let anthropic_base_url = spawn_test_server(
        Router::new()
            .route(
                "/v1/messages",
                post(move |State(attempts): State<Arc<AtomicUsize>>| async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "content": [{ "type": "text", "text": "anthropic fallback" }],
                        "stop_reason": "end_turn",
                        "usage": { "input_tokens": 4, "output_tokens": 2 }
                    }))
                }),
            )
            .with_state(anthropic_server_attempts),
    )
    .await;

    let mut fixture = test_config(
        "openai/gpt-5.4",
        &["anthropic/claude-sonnet-4-6"],
        Some("openai-key"),
        Some("anthropic-token"),
        false,
    );
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = openai_base_url;
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .base_url = anthropic_base_url;

    let provider = build_provider_from_config(&fixture.config).unwrap();
    let (response, diagnostics) = provider
        .complete_turn_with_diagnostics(provider_turn_request())
        .await
        .unwrap();

    assert_eq!(
        openai_attempts.load(Ordering::SeqCst),
        provider_max_attempts()
    );
    assert_eq!(anthropic_attempts.load(Ordering::SeqCst), 1);
    let timeline = diagnostics.expect("missing attempt timeline");
    assert_eq!(
        timeline.winning_model_ref.as_deref(),
        Some("anthropic/claude-sonnet-4-6")
    );
    assert_eq!(
        timeline
            .aggregated_token_usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(6)
    );
    assert_eq!(timeline.attempts.len(), provider_max_attempts() + 1);
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::Retrying
    );
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Retrying
    );
    assert_eq!(
        timeline.attempts[provider_max_attempts() - 1].outcome,
        ProviderAttemptOutcome::RetriesExhausted
    );
    assert!(timeline.attempts[provider_max_attempts() - 1].advanced_to_fallback);
    assert_eq!(
        timeline.attempts.last().unwrap().outcome,
        ProviderAttemptOutcome::Succeeded
    );
    match &response.blocks[0] {
        ModelBlock::Text { text } => assert_eq!(text, "anthropic fallback"),
        _ => panic!("expected text block"),
    }
}

#[tokio::test]
async fn provider_fallback_can_be_disabled_for_retry_exhaustion() {
    let openai_attempts = Arc::new(AtomicUsize::new(0));
    let openai_server_attempts = openai_attempts.clone();
    let openai_base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || {
            let attempts = openai_server_attempts.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "server unavailable",
                )
                    .into_response()
            }
        }),
    ))
    .await;

    let anthropic_attempts = Arc::new(AtomicUsize::new(0));
    let anthropic_server_attempts = anthropic_attempts.clone();
    let anthropic_base_url = spawn_test_server(
        Router::new()
            .route(
                "/v1/messages",
                post(move |State(attempts): State<Arc<AtomicUsize>>| async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "content": [{ "type": "text", "text": "anthropic fallback" }],
                        "stop_reason": "end_turn",
                        "usage": { "input_tokens": 4, "output_tokens": 2 }
                    }))
                }),
            )
            .with_state(anthropic_server_attempts),
    )
    .await;

    let mut fixture = test_config(
        "openai/gpt-5.4",
        &["anthropic/claude-sonnet-4-6"],
        Some("openai-key"),
        Some("anthropic-token"),
        false,
    );
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .base_url = openai_base_url;
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .base_url = anthropic_base_url;
    fixture
        .config
        .stored_config
        .runtime
        .disable_provider_fallback = Some(true);
    fixture.config.disable_provider_fallback = true;

    let provider = build_provider_from_config(&fixture.config).unwrap();
    let error = provider
        .complete_turn(provider_turn_request())
        .await
        .err()
        .expect("retry exhaustion should fail without advancing to fallback");

    assert_eq!(
        openai_attempts.load(Ordering::SeqCst),
        provider_max_attempts()
    );
    assert_eq!(anthropic_attempts.load(Ordering::SeqCst), 0);
    let timeline = provider_attempt_timeline(&error).expect("missing attempt timeline");
    assert_eq!(timeline.attempts.len(), provider_max_attempts());
    assert_eq!(timeline.winning_model_ref, None);
    assert!(timeline
        .attempts
        .iter()
        .all(|attempt| !attempt.advanced_to_fallback));
}

#[test]
fn build_provider_from_config_preserves_order_of_unique_models() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[
            "openai/gpt-5.4",
            "anthropic/claude-sonnet-4-6",
            "openai-codex/gpt-5.4",
        ],
        Some("openai-key"),
        Some("anthropic-token"),
        true,
    );
    let provider = build_provider_from_config(&fixture.config).unwrap();
    let refs = provider.configured_model_refs();
    assert_eq!(
        refs.as_slice(),
        &[
            "anthropic/claude-sonnet-4-6",
            "openai/gpt-5.4",
            "openai-codex/gpt-5.4"
        ]
    );
}

#[test]
fn build_provider_from_config_skips_unavailable_models_and_continues() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &["openai/gpt-5.4", "anthropic/claude-sonnet-4-6"],
        None,
        Some("anthropic-token"),
        false,
    );
    let provider = build_provider_from_config(&fixture.config).unwrap();
    let refs = provider.configured_model_refs();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0], "anthropic/claude-sonnet-4-6");
}

#[test]
fn build_provider_from_config_fails_on_unavailable_primary_when_fallback_disabled() {
    let mut fixture = test_config(
        "openai/gpt-5.4",
        &["anthropic/claude-sonnet-4-6"],
        None,
        Some("anthropic-token"),
        false,
    );
    fixture
        .config
        .stored_config
        .runtime
        .disable_provider_fallback = Some(true);
    fixture.config.disable_provider_fallback = true;

    let err = build_provider_from_config(&fixture.config)
        .err()
        .expect("deterministic mode should fail on unavailable primary provider");
    assert!(err
        .to_string()
        .contains("no available providers for configured model chain"));
    assert!(err.to_string().contains("openai/gpt-5.4"));
    assert!(!err.to_string().contains("anthropic/claude-sonnet-4-6"));
}

#[test]
fn build_provider_from_config_fails_when_all_models_unavailable() {
    let fixture = test_config(
        "openai/gpt-5.4",
        &["anthropic/claude-sonnet-4-6", "openai-codex/gpt-5.4"],
        None,
        None,
        false,
    );
    let err = build_provider_from_config(&fixture.config)
        .err()
        .expect("should fail when all providers unavailable");
    assert!(err.to_string().contains("no available providers"));
    assert!(err.to_string().contains("openai/gpt-5.4"));
    assert!(err.to_string().contains("anthropic/claude-sonnet-4-6"));
    assert!(err.to_string().contains("openai-codex/gpt-5.4"));
}

#[test]
fn build_provider_from_config_uses_custom_openai_responses_provider() {
    let mut fixture = test_config("openrouter/custom-model", &[], None, None, false);
    let provider_id = ProviderId::parse("openrouter").unwrap();
    fixture.config.providers.insert(
        provider_id.clone(),
        ProviderRuntimeConfig {
            id: provider_id,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://openrouter.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENROUTER_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: Some("openrouter-key".into()),
            codex_home: None,
            originator: None,
            context_management: Default::default(),
        },
    );

    let provider = build_provider_from_config(&fixture.config).unwrap();
    assert_eq!(
        provider.configured_model_refs(),
        vec!["openrouter/custom-model"]
    );

    let doctor = provider_doctor(&fixture.config);
    let provider_status = &doctor["providers"].as_array().unwrap()[0];
    assert_eq!(provider_status["provider"], "openrouter");
    assert_eq!(provider_status["settings"]["transport"], "openai_responses");
    assert_eq!(provider_status["settings"]["auth"]["source"], "env");
    assert_eq!(provider_status["settings"]["auth"]["kind"], "api_key");
    assert_eq!(
        provider_status["settings"]["auth"]["credential_configured"],
        true
    );
}

#[test]
fn build_candidate_accepts_openai_responses_without_auth() {
    let mut fixture = test_config("local-openai/custom-model", &[], None, None, false);
    let provider_id = ProviderId::parse("local-openai").unwrap();
    fixture.config.providers.insert(
        provider_id.clone(),
        ProviderRuntimeConfig {
            id: provider_id,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "http://127.0.0.1:8080/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::None,
                kind: CredentialKind::None,
                env: None,
                profile: None,
                external: None,
            },
            credential: None,
            codex_home: None,
            originator: None,
            context_management: Default::default(),
        },
    );

    let candidate = build_candidate(
        &fixture.config,
        &ModelRef::parse("local-openai/custom-model").unwrap(),
    )
    .unwrap();

    assert_eq!(candidate.model_ref, "local-openai/custom-model");
    assert_eq!(candidate.provider_name, "local-openai");
}

#[test]
fn build_candidate_handles_multiple_openai_models() {
    let fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    let config = &fixture.config;

    let gpt54 = build_candidate(config, &ModelRef::parse("openai/gpt-5.4").unwrap()).unwrap();
    let gpt53 = build_candidate(config, &ModelRef::parse("openai/gpt-5.3").unwrap()).unwrap();

    assert_eq!(gpt54.model_ref, "openai/gpt-5.4");
    assert_eq!(gpt53.model_ref, "openai/gpt-5.3");
    assert_eq!(gpt54.provider_name, "openai");
    assert_eq!(gpt53.provider_name, "openai");
}

#[test]
fn build_candidate_handles_multiple_anthropic_models() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[],
        None,
        Some("anthropic-token"),
        false,
    );
    let config = &fixture.config;

    let sonnet = build_candidate(
        config,
        &ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
    )
    .unwrap();
    let haiku = build_candidate(
        config,
        &ModelRef::parse("anthropic/claude-haiku-4-5").unwrap(),
    )
    .unwrap();

    assert_eq!(sonnet.model_ref, "anthropic/claude-sonnet-4-6");
    assert_eq!(haiku.model_ref, "anthropic/claude-haiku-4-5");
    assert_eq!(sonnet.provider_name, "anthropic");
    assert_eq!(haiku.provider_name, "anthropic");
}

#[test]
fn provider_doctor_includes_partial_availability_in_chain() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &["openai/gpt-5.4", "openai-codex/gpt-5.4"],
        None,
        Some("anthropic-token"),
        false,
    );
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 3);

    // Default model should be available
    assert_eq!(providers[0]["model"], "anthropic/claude-sonnet-4-6");
    assert_eq!(providers[0]["availability"]["available"], Value::Bool(true));

    // Fallbacks should be unavailable
    assert_eq!(providers[1]["model"], "openai/gpt-5.4");
    assert_eq!(
        providers[1]["availability"]["available"],
        Value::Bool(false)
    );

    assert_eq!(providers[2]["model"], "openai-codex/gpt-5.4");
    assert_eq!(
        providers[2]["availability"]["available"],
        Value::Bool(false)
    );
}

#[test]
fn build_candidate_reports_empty_api_key_as_missing() {
    let mut fixture = test_config("openai/gpt-5.4", &[], Some(""), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .credential = Some("".to_string());
    let err = build_candidate(&fixture.config, &ModelRef::parse("openai/gpt-5.4").unwrap())
        .err()
        .expect("empty API key should fail");
    assert!(err.to_string().contains("missing OPENAI_API_KEY"));
}

#[test]
fn build_candidate_reports_whitespace_api_key_as_missing() {
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("   "), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .credential = Some("   ".to_string());
    let err = build_candidate(&fixture.config, &ModelRef::parse("openai/gpt-5.4").unwrap())
        .err()
        .expect("whitespace API key should fail");
    assert!(err.to_string().contains("missing OPENAI_API_KEY"));
}

#[test]
fn build_candidate_requires_at_least_one_available_provider_in_chain() {
    let fixture = test_config(
        "openai/gpt-5.4",
        &["anthropic/claude-sonnet-4-6", "openai-codex/gpt-5.4"],
        None,
        None,
        false,
    );
    let err = build_provider_from_config(&fixture.config)
        .err()
        .expect("should fail with no available providers");
    assert!(err.to_string().contains("no available providers"));
}

#[test]
fn build_candidate_succeeds_with_valid_codex_auth() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let candidate = build_candidate(
        &fixture.config,
        &ModelRef::parse("openai-codex/gpt-5.4").unwrap(),
    )
    .unwrap();
    assert_eq!(candidate.model_ref, "openai-codex/gpt-5.4");
    assert_eq!(candidate.provider_name, "openai-codex");
}

#[test]
fn provider_doctor_distinguishes_auth_from_other_errors() {
    let fixture = test_config("openai/gpt-5.4", &[], None, None, false);
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let provider = &providers[0];

    // Missing auth should be reported distinctly
    assert_eq!(provider["availability"]["available"], Value::Bool(false));
    assert!(provider["availability"]["error"]
        .as_str()
        .unwrap()
        .contains("missing OPENAI_API_KEY"));
    // Note: Config-level auth errors are classified as "unknown" since they
    // don't go through the transport error classification layer
    assert_eq!(
        provider["availability"]["failure_kind"],
        Value::String("unknown".to_string())
    );
    assert_eq!(
        provider["availability"]["disposition"],
        Value::String("fail_fast".to_string())
    );
}

#[test]
fn build_candidate_fails_when_openai_env_auth_missing() {
    let fixture = test_config("openai/gpt-5.4", &[], None, None, false);
    let err = build_candidate(&fixture.config, &ModelRef::parse("openai/gpt-5.4").unwrap())
        .err()
        .expect("missing env auth should fail");
    assert!(err.to_string().contains("missing OPENAI_API_KEY"));
}

#[test]
fn build_candidate_fails_when_anthropic_env_auth_missing() {
    let fixture = test_config("anthropic/claude-sonnet-4-6", &[], None, None, false);
    let err = build_candidate(
        &fixture.config,
        &ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
    )
    .err()
    .expect("missing env auth should fail");
    assert!(err.to_string().contains("missing ANTHROPIC_AUTH_TOKEN"));
}

#[test]
fn build_candidate_fails_when_codex_external_cli_auth_missing() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, false);
    let err = build_candidate(
        &fixture.config,
        &ModelRef::parse("openai-codex/gpt-5.4").unwrap(),
    )
    .err()
    .expect("missing external CLI auth should fail");
    assert!(err.to_string().contains("no Codex CLI credentials found"));
}

#[test]
fn build_candidate_succeeds_with_openai_env_auth() {
    let fixture = test_config("openai/gpt-5.4", &[], Some("sk-test-key"), None, false);
    let candidate = build_candidate(&fixture.config, &ModelRef::parse("openai/gpt-5.4").unwrap())
        .expect("valid env auth should succeed");
    assert_eq!(candidate.model_ref, "openai/gpt-5.4");
    assert_eq!(candidate.provider_name, "openai");
}

#[test]
fn build_candidate_succeeds_with_anthropic_env_auth() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[],
        None,
        Some("sk-ant-test-token"),
        false,
    );
    let candidate = build_candidate(
        &fixture.config,
        &ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
    )
    .expect("valid env auth should succeed");
    assert_eq!(candidate.model_ref, "anthropic/claude-sonnet-4-6");
    assert_eq!(candidate.provider_name, "anthropic");
}

#[test]
fn provider_doctor_reports_missing_openai_env_auth() {
    let fixture = test_config("openai/gpt-5.4", &[], None, None, false);
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let openai_provider = providers
        .iter()
        .find(|p| p["provider"] == "openai")
        .expect("should have openai provider");

    assert_eq!(
        openai_provider["availability"]["available"],
        Value::Bool(false)
    );
    assert!(openai_provider["availability"]["error"]
        .as_str()
        .unwrap()
        .contains("missing OPENAI_API_KEY"));
}

#[test]
fn provider_doctor_reports_missing_anthropic_env_auth() {
    let fixture = test_config("anthropic/claude-sonnet-4-6", &[], None, None, false);
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let anthropic_provider = providers
        .iter()
        .find(|p| p["provider"] == "anthropic")
        .expect("should have anthropic provider");

    assert_eq!(
        anthropic_provider["availability"]["available"],
        Value::Bool(false)
    );
    assert!(anthropic_provider["availability"]["error"]
        .as_str()
        .unwrap()
        .contains("missing ANTHROPIC_AUTH_TOKEN"));
}

#[test]
fn provider_doctor_reports_missing_codex_external_cli_auth() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, false);
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let codex_provider = providers
        .iter()
        .find(|p| p["provider"] == "openai-codex")
        .expect("should have codex provider");

    assert_eq!(
        codex_provider["availability"]["available"],
        Value::Bool(false)
    );
    assert!(codex_provider["availability"]["error"]
        .as_str()
        .unwrap()
        .contains("no Codex CLI credentials found"));
}

#[test]
fn provider_doctor_reports_valid_openai_env_auth() {
    let fixture = test_config("openai/gpt-5.4", &[], Some("sk-test-key"), None, false);
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let openai_provider = providers
        .iter()
        .find(|p| p["provider"] == "openai")
        .expect("should have openai provider");

    assert_eq!(
        openai_provider["availability"]["available"],
        Value::Bool(true)
    );
    assert_eq!(
        openai_provider["availability"]["prompt_capabilities"],
        json!([
            "full_request_only",
            "prompt_cache_key",
            "incremental_responses"
        ])
    );
}

#[test]
fn provider_doctor_reports_valid_anthropic_env_auth() {
    let fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[],
        None,
        Some("sk-ant-test-token"),
        false,
    );
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let anthropic_provider = providers
        .iter()
        .find(|p| p["provider"] == "anthropic")
        .expect("should have anthropic provider");

    assert_eq!(
        anthropic_provider["availability"]["available"],
        Value::Bool(true)
    );
    assert_eq!(
        anthropic_provider["availability"]["prompt_capabilities"],
        json!(["full_request_only", "prompt_cache_blocks"])
    );
}

#[test]
fn provider_doctor_reports_anthropic_context_management_when_enabled() {
    let mut fixture = test_config(
        "anthropic/claude-sonnet-4-6",
        &[],
        None,
        Some("sk-ant-test-token"),
        false,
    );
    fixture
        .config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .context_management
        .enabled = true;
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let anthropic_provider = providers
        .iter()
        .find(|p| p["provider"] == "anthropic")
        .expect("should have anthropic provider");

    assert_eq!(
        anthropic_provider["availability"]["prompt_capabilities"],
        json!([
            "full_request_only",
            "prompt_cache_blocks",
            "context_management"
        ])
    );
}

#[test]
fn provider_doctor_reports_valid_codex_external_cli_auth() {
    let fixture = test_config("openai-codex/gpt-5.4", &[], None, None, true);
    let doctor = provider_doctor(&fixture.config);
    let providers = doctor["providers"].as_array().unwrap();
    let codex_provider = providers
        .iter()
        .find(|p| p["provider"] == "openai-codex")
        .expect("should have codex provider");

    assert_eq!(
        codex_provider["availability"]["available"],
        Value::Bool(true)
    );
}
