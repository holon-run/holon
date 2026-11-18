//! OpenAI Chat Completions conversion, streaming, and error classification tests.

use super::support::*;
use super::*;
use crate::config::{ModelRef, ModelRouteRef, ProviderId, ProviderTransportKind};
use crate::model_catalog::{ModelRuntimeOverride, ModelVerbosity};
use crate::provider::OpenAiChatCompletionsProvider;
use axum::{routing::post, Json, Router};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn build_candidate_creates_chat_completions_provider() {
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .transport = ProviderTransportKind::OpenAiChatCompletions;

    let candidate = build_candidate(
        &fixture.config,
        &ModelRouteRef::parse_compatible("openai/gpt-5.4").unwrap(),
    )
    .expect("chat completions provider should build");

    assert_eq!(candidate.model_ref, "openai@default/gpt-5.4");
    assert_eq!(candidate.provider_name, "openai");
}

#[tokio::test]
async fn chat_completions_from_config_uses_resolved_max_output_override_on_the_wire() {
    let request_bodies = Arc::new(Mutex::new(Vec::new()));
    let request_bodies_for_server = request_bodies.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| {
            let captured = request_bodies_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                Json(json!({
                    "id": "chatcmpl-test",
                    "choices": [{
                        "message": { "role": "assistant", "content": "done" },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2
                    }
                }))
            }
        }),
    ))
    .await;
    let mut fixture = test_config("openai/gpt-5.4", &[], Some("openai-key"), None, false);
    let provider_config = fixture
        .config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap();
    provider_config.base_url = base_url;
    provider_config.transport = ProviderTransportKind::OpenAiChatCompletions;
    fixture.config.validated_model_overrides.insert(
        ModelRef::parse("openai/gpt-5.4").unwrap(),
        ModelRuntimeOverride {
            runtime_max_output_tokens: Some(1_234),
            verbosity: Some(ModelVerbosity::Low),
            ..ModelRuntimeOverride::default()
        },
    );
    let provider = OpenAiChatCompletionsProvider::from_config(&fixture.config, "gpt-5.4").unwrap();

    provider
        .complete_turn(ProviderTurnRequest::plain("hello", Vec::new(), Vec::new()))
        .await
        .unwrap();

    let request_bodies = request_bodies.lock().unwrap();
    assert_eq!(request_bodies[0]["max_tokens"], json!(1_234));
}
