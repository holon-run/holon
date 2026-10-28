//! Gemini GenerateContent wire request tests.

use std::sync::{Arc, Mutex};

use super::support::*;
use super::*;
use crate::config::ProviderId;
use crate::provider::{
    budget::estimate_context_budget,
    provider_error_code, provider_transport_diagnostics,
    retry::{classify_provider_error, ProviderFailureKind, RetryDisposition},
};
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};

async fn capture_gemini_request(request: ProviderTurnRequest) -> (Value, ProviderTurnResponse) {
    let (body, response) = gemini_request_with_budget(request, None, 8192).await;
    (
        body.expect("server should capture request body"),
        response.unwrap(),
    )
}

async fn gemini_request_with_budget(
    request: ProviderTurnRequest,
    context_window_tokens: Option<usize>,
    max_output_tokens: u32,
) -> (Option<Value>, anyhow::Result<ProviderTurnResponse>) {
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
    let provider = GeminiProvider::from_runtime_config_with_context_window(
        provider_config,
        "gemini-3-pro",
        max_output_tokens,
        context_window_tokens,
        &fixture.config.home_dir,
    )
    .unwrap();

    let response = provider.complete_turn(request).await;

    let body = captured_body.lock().unwrap().clone();
    (body, response)
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

    let (body, response) = capture_gemini_request(request).await;

    let timeline = response
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.transport_timeline.as_ref())
        .expect("Gemini transport timeline");
    assert!(!timeline.streaming);
    assert!(timeline.request_started_at <= timeline.response_headers_at);
    assert!(
        timeline.response_headers_at
            <= timeline
                .response_body_completed_at
                .expect("Gemini response body completion")
    );
    assert!(
        timeline
            .response_body_completed_at
            .expect("Gemini response body completion")
            <= timeline.parse_completed_at
    );

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
    let (body, _) = capture_gemini_request(provider_continuation_request_with_prompt_frame()).await;

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
    let (body, _) = capture_gemini_request(ProviderTurnRequest::plain(
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

#[tokio::test]
async fn gemini_output_is_clamped_on_the_wire_to_the_remaining_context() {
    let request = ProviderTurnRequest::plain(
        &"x".repeat(24_000),
        vec![ConversationMessage::UserText("continue".into())],
        Vec::new(),
    );
    let (body, response) = gemini_request_with_budget(request, Some(16_384), 8192).await;
    response.unwrap();
    let body = body.unwrap();
    let effective = body["generationConfig"]["maxOutputTokens"]
        .as_u64()
        .unwrap() as u32;
    assert!(effective > 1);
    assert!(effective < 8192);
    let budget = estimate_context_budget(
        Some(16_384),
        effective,
        &body,
        &["generationConfig.maxOutputTokens"],
    )
    .unwrap();
    assert_eq!(budget.overflow_tokens(), 0);
}

#[tokio::test]
async fn gemini_unknown_context_preserves_the_configured_output_limit() {
    let (body, response) = gemini_request_with_budget(
        ProviderTurnRequest::plain("short", Vec::new(), Vec::new()),
        None,
        65_536,
    )
    .await;
    response.unwrap();
    assert_eq!(body.unwrap()["generationConfig"]["maxOutputTokens"], 65_536);
}

#[tokio::test]
async fn gemini_context_preflight_counts_system_text_and_tool_json_before_http() {
    for large_tool in [false, true] {
        let system = if large_tool {
            "short".to_string()
        } else {
            "x".repeat(40_000)
        };
        let request = ProviderTurnRequest::plain(
            &system,
            vec![ConversationMessage::UserText("continue".into())],
            if large_tool {
                vec![crate::tool::ToolSpec {
                    name: "Example".into(),
                    description: "tool".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {"text": {"type": "string", "description": "x".repeat(40_000)}}
                    }),
                    freeform_grammar: None,
                }]
            } else {
                Vec::new()
            },
        );
        let (body, response) = gemini_request_with_budget(request, Some(8192), 8192).await;
        let error = response.expect_err("oversized final payload must not be sent");
        assert!(body.is_none());
        assert_eq!(provider_error_code(&error), Some("context_length_exceeded"));
        let classification = classify_provider_error(&error);
        assert_eq!(classification.kind, ProviderFailureKind::ContractError);
        assert_eq!(classification.disposition, RetryDisposition::FailFast);
        let diagnostics = provider_transport_diagnostics(&error).unwrap();
        assert_eq!(diagnostics.stage, "request_preflight");
        assert_eq!(diagnostics.provider.as_deref(), Some("gemini"));
        let budget = diagnostics.context_budget.as_ref().unwrap();
        assert!(budget.estimated_input_tokens > 10_000);
        assert_eq!(budget.estimated_text_tokens, budget.estimated_input_tokens);
        assert_eq!(budget.image_reserve_tokens, 0);
        assert_eq!(budget.image_count, 0);
        assert_eq!(budget.inline_image_base64_bytes, 0);
        assert_eq!(budget.estimated_inline_image_decoded_bytes, 0);
        assert_eq!(budget.requested_output_tokens, 1);
        assert_eq!(budget.context_window_tokens, 8192);
        assert!(budget.overflow_tokens > 0);
        assert!(!serde_json::to_string(diagnostics)
            .unwrap()
            .contains("gemini-key"));
    }
}

#[tokio::test]
async fn gemini_required_images_fail_fast_instead_of_becoming_text_only() {
    for prompt in ["inspect this", ""] {
        let (body, response) = gemini_request_with_budget(
            ProviderTurnRequest::plain(
                "system",
                vec![ConversationMessage::UserImage {
                    prompt: prompt.into(),
                    media_type: "image/png".into(),
                    data_base64: "sensitive-image-data".into(),
                    optional_history: false,
                }],
                Vec::new(),
            ),
            None,
            8192,
        )
        .await;
        let error = response.expect_err("required images cannot be silently discarded");
        assert!(body.is_none());
        assert_eq!(provider_error_code(&error), Some("image_input_unsupported"));
        assert_eq!(
            classify_provider_error(&error).disposition,
            RetryDisposition::FailFast
        );
        assert_eq!(
            provider_transport_diagnostics(&error).unwrap().stage,
            "request_preflight"
        );
        assert!(!error.to_string().contains("sensitive-image-data"));
    }
}

#[tokio::test]
async fn gemini_optional_history_remains_explicitly_text_only() {
    let (body, _) = capture_gemini_request(ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserImage {
            prompt: "historical image".into(),
            media_type: "image/png".into(),
            data_base64: "sensitive-image-data".into(),
            optional_history: true,
        }],
        Vec::new(),
    ))
    .await;
    let serialized = body.to_string();
    assert!(serialized.contains("image input omitted"));
    assert!(!serialized.contains("sensitive-image-data"));
    assert!(!serialized.contains("inlineData"));
}
