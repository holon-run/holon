//! Gemini GenerateContent wire request tests.

use std::sync::{Arc, Mutex};

use super::support::*;
use super::*;
use crate::config::ProviderId;
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};

async fn capture_gemini_request(request: ProviderTurnRequest) -> Value {
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let captured_body_for_server = captured_body.clone();
    let base_url =
        spawn_test_server(Router::new().fallback(post(move |Json(body): Json<Value>| {
            let captured_body = captured_body_for_server.clone();
            async move {
                *captured_body.lock().unwrap() = Some(body);
                Json(json!({
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{ "text": "ok" }]
                        },
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 4,
                        "candidatesTokenCount": 1
                    }
                }))
            }
        })))
        .await;
    let mut fixture = test_config("gemini/gemini-3-pro", &[], None, None, false);
    let provider_config = fixture
        .config
        .providers
        .get_mut(&ProviderId::gemini())
        .unwrap();
    provider_config.base_url = base_url;
    provider_config.credential = Some("gemini-key".into());
    let provider = GeminiProvider::from_runtime_config(
        provider_config,
        "gemini-3-pro",
        fixture.config.runtime_max_output_tokens,
        &fixture.config.home_dir,
    )
    .unwrap();

    provider.complete_turn(request).await.unwrap();

    let body = captured_body.lock().unwrap().clone();
    body.expect("server should capture request body")
}

fn text_occurrences(value: &Value, expected: &str) -> usize {
    match value {
        Value::String(text) => usize::from(text == expected),
        Value::Array(values) => values
            .iter()
            .map(|value| text_occurrences(value, expected))
            .sum(),
        Value::Object(values) => values
            .values()
            .map(|value| text_occurrences(value, expected))
            .sum(),
        _ => 0,
    }
}

#[tokio::test]
async fn gemini_request_emits_each_prompt_section_once() {
    let mut request = provider_turn_request_with_prompt_frame();
    request
        .conversation
        .push(ConversationMessage::UserText("current input".into()));

    let body = capture_gemini_request(request).await;

    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        json!("stable system")
    );
    assert_eq!(
        body["contents"][0]["parts"][0]["text"],
        json!("agent context")
    );
    assert_eq!(
        body["contents"][1]["parts"][0]["text"],
        json!("current input")
    );
    assert_eq!(text_occurrences(&body, "stable system"), 1);
    assert_eq!(text_occurrences(&body, "agent context"), 1);
    assert_eq!(text_occurrences(&body, "current input"), 1);
    assert_eq!(text_occurrences(&body, "rendered system"), 0);
}

#[tokio::test]
async fn gemini_continuation_preserves_history_without_repeating_prompt_sections() {
    let body = capture_gemini_request(provider_continuation_request_with_prompt_frame()).await;

    assert_eq!(text_occurrences(&body, "stable system"), 1);
    assert_eq!(text_occurrences(&body, "agent context"), 1);
    assert_eq!(
        body["contents"][1]["parts"][0]["functionCall"]["name"],
        json!("ExecCommand")
    );
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["response"]["content"],
        json!("ok")
    );
}

#[tokio::test]
async fn gemini_request_falls_back_to_rendered_system_without_structured_blocks() {
    let body = capture_gemini_request(ProviderTurnRequest::plain(
        "fallback system",
        vec![ConversationMessage::UserText("current input".into())],
        Vec::new(),
    ))
    .await;

    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        json!("fallback system")
    );
    assert_eq!(text_occurrences(&body, "fallback system"), 1);
    assert_eq!(text_occurrences(&body, "current input"), 1);
}
