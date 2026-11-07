//! Gemini GenerateContent wire request tests.

use std::sync::{Arc, Mutex};

use super::support::*;
use super::*;
use crate::config::ProviderId;
use crate::provider::{
    budget::estimate_context_budget,
    provider_error_code, provider_transport_diagnostics,
    retry::{classify_provider_error, ProviderFailureKind, RetryDisposition},
    ModelToolCallKind, ProviderBlockData, ProviderImageData,
};
use axum::{routing::post, Json, Router};
use base64::Engine;
use serde_json::{json, Value};

fn gemini_image_fixture(format: image::ImageFormat) -> (String, ProviderImageData) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    image::RgbImage::from_pixel(2, 1, image::Rgb([24, 64, 128]))
        .save_with_format(&path, format)
        .unwrap();
    let image = crate::tool::tools::view_image::read_visual_reference(&path).unwrap();
    (
        image.visual_reference.mime,
        base64::engine::general_purpose::STANDARD
            .encode(image.bytes)
            .into(),
    )
}

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
async fn gemini_request_lowers_json_schema_response_format_with_image() {
    let (media_type, data_base64) = gemini_image_fixture(image::ImageFormat::Png);
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["type", "summary", "uncertainties"],
        "properties": {
            "type": { "type": "string", "const": "visual_observation" },
            "summary": { "type": "string" },
            "uncertainties": { "type": "array", "items": { "type": "string" } }
        }
    });
    let mut request = ProviderTurnRequest::plain(
        "Return a visual observation as JSON.",
        vec![ConversationMessage::UserImage {
            prompt: "Describe the image.".into(),
            media_type: media_type.clone(),
            data_base64: data_base64.clone(),
            optional_history: false,
        }],
        Vec::new(),
    );
    request.response_format = Some(crate::provider::ProviderResponseFormatRequest::JsonSchema(
        crate::provider::ProviderJsonSchemaResponseFormat {
            name: "visual_observation_v1".into(),
            schema: schema.clone(),
            strict: true,
        },
    ));

    let (body, response) = capture_gemini_request(request).await;

    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(body["generationConfig"]["responseJsonSchema"], schema);
    assert!(body["generationConfig"].get("responseSchema").is_none());
    assert!(body.get("response_format").is_none());
    assert_eq!(
        body["contents"][0]["parts"][0]["text"],
        "Describe the image."
    );
    assert_eq!(
        body["contents"][0]["parts"][1]["inlineData"],
        json!({ "mimeType": media_type, "data": data_base64 })
    );
    let diagnostics = response
        .request_diagnostics
        .unwrap()
        .response_format
        .expect("Gemini JSON schema response format diagnostics");
    assert!(diagnostics.requested);
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.format_type, "json_schema");
    assert_eq!(
        diagnostics.schema_name.as_deref(),
        Some("visual_observation_v1")
    );
    assert!(diagnostics.fallback_reason.is_none());
}

#[tokio::test]
async fn gemini_request_emits_each_prompt_section_once() {
    let mut request = provider_turn_request_with_prompt_frame();
    request
        .conversation
        .push(ConversationMessage::UserText("current input".into()));

    let (body, response) = capture_gemini_request(request).await;

    assert!(body["generationConfig"].get("responseMimeType").is_none());
    assert!(body["generationConfig"].get("responseJsonSchema").is_none());
    assert!(response
        .request_diagnostics
        .as_ref()
        .unwrap()
        .response_format
        .is_none());
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
async fn gemini_request_lowers_png_jpeg_and_image_only_turns() {
    for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg] {
        for prompt in ["inspect this", ""] {
            for optional_history in [false, true] {
                let (media_type, data_base64) = gemini_image_fixture(format);
                let (body, _) = capture_gemini_request(ProviderTurnRequest::plain(
                    "system",
                    vec![ConversationMessage::UserImage {
                        prompt: prompt.into(),
                        media_type: media_type.clone(),
                        data_base64: data_base64.clone(),
                        optional_history,
                    }],
                    Vec::new(),
                ))
                .await;
                let parts = body["contents"][0]["parts"].as_array().unwrap();
                assert_eq!(parts.len(), if prompt.is_empty() { 1 } else { 2 });
                if !prompt.is_empty() {
                    assert_eq!(parts[0], json!({"text": prompt}));
                }
                assert_eq!(
                    parts.last().unwrap(),
                    &json!({"inlineData": {"mimeType": media_type, "data": data_base64}})
                );
                assert!(!body.to_string().contains("image input omitted"));
            }
        }
    }
}

#[tokio::test]
async fn gemini_required_unsupported_images_fail_fast_instead_of_becoming_text_only() {
    for prompt in ["inspect this", ""] {
        let (body, response) = gemini_request_with_budget(
            ProviderTurnRequest::plain(
                "system",
                vec![ConversationMessage::UserImage {
                    prompt: prompt.into(),
                    media_type: "application/pdf".into(),
                    data_base64: String::from("sensitive-image-data").into(),
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
        assert_eq!(provider_error_code(&error), Some("image_input_invalid"));
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
async fn gemini_request_preserves_image_text_and_tool_history_order() {
    let (png_type, png_data) = gemini_image_fixture(image::ImageFormat::Png);
    let (jpeg_type, jpeg_data) = gemini_image_fixture(image::ImageFormat::Jpeg);
    let (body, _) = capture_gemini_request(ProviderTurnRequest::plain(
        "system",
        vec![
            ConversationMessage::UserText("before".into()),
            ConversationMessage::UserImage {
                prompt: "first image".into(),
                media_type: png_type.clone(),
                data_base64: png_data.clone(),
                optional_history: false,
            },
            ConversationMessage::UserText("between".into()),
            ConversationMessage::UserImage {
                prompt: String::new(),
                media_type: jpeg_type.clone(),
                data_base64: jpeg_data.clone(),
                optional_history: false,
            },
            ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
                id: "ProbeTool__holon_gemini_call_0".into(),
                name: "ProbeTool".into(),
                input: json!({"inspect": true}),
                kind: ModelToolCallKind::Function,
                provider_data: Some(ProviderBlockData {
                    format: "gemini.function_call".into(),
                    payload: json!({"thoughtSignature": "opaque-signature"}),
                }),
            }]),
            ConversationMessage::UserToolResults(vec![ToolResultBlock {
                tool_use_id: "ProbeTool__holon_gemini_call_0".into(),
                content: "tool result".into(),
                is_error: false,
                error: None,
            }]),
            ConversationMessage::UserText("after".into()),
        ],
        vec![crate::tool::ToolSpec {
            name: "ProbeTool".into(),
            description: "inspect an image".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            freeform_grammar: None,
        }],
    ))
    .await;
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 7);
    assert_eq!(contents[0]["parts"], json!([{"text": "before"}]));
    assert_eq!(
        contents[1]["parts"],
        json!([
            {"text": "first image"},
            {"inlineData": {"mimeType": png_type, "data": png_data}}
        ])
    );
    assert_eq!(contents[2]["parts"], json!([{"text": "between"}]));
    assert_eq!(
        contents[3]["parts"],
        json!([{"inlineData": {"mimeType": jpeg_type, "data": jpeg_data}}])
    );
    assert_eq!(contents[4]["role"], "model");
    assert_eq!(
        contents[4]["parts"],
        json!([{
            "functionCall": {"name": "ProbeTool", "args": {"inspect": true}},
            "thoughtSignature": "opaque-signature"
        }])
    );
    assert_eq!(
        contents[5]["parts"],
        json!([{"functionResponse": {
            "name": "ProbeTool",
            "response": {"content": "tool result", "is_error": false}
        }}])
    );
    assert_eq!(contents[6]["parts"], json!([{"text": "after"}]));
    assert_eq!(
        body["tools"][0]["function_declarations"][0]["name"],
        "ProbeTool"
    );
}

#[tokio::test]
async fn gemini_required_invalid_images_fail_fast_without_leaking_payloads() {
    for (media_type, data_base64) in [
        ("image/png", ""),
        ("application/pdf", "sensitive-image-data"),
    ] {
        for prompt in ["inspect this", ""] {
            let (body, response) = gemini_request_with_budget(
                ProviderTurnRequest::plain(
                    "system",
                    vec![ConversationMessage::UserImage {
                        prompt: prompt.into(),
                        media_type: media_type.into(),
                        data_base64: String::from(data_base64).into(),
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
            assert_eq!(provider_error_code(&error), Some("image_input_invalid"));
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
}

#[tokio::test]
async fn gemini_required_oversized_image_is_rejected_before_http() {
    let max_bytes = crate::tool::tools::view_image::MAX_IMAGE_BYTES as usize;
    let (body, response) = gemini_request_with_budget(
        ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserImage {
                prompt: "inspect this".into(),
                media_type: "image/png".into(),
                data_base64: "A".repeat(max_bytes.div_ceil(3) * 4 + 4).into(),
                optional_history: false,
            }],
            Vec::new(),
        ),
        None,
        8192,
    )
    .await;
    assert!(body.is_none());
    let error = response.expect_err("oversized image must not be sent");
    assert_eq!(provider_error_code(&error), Some("image_input_invalid"));
    assert_eq!(
        classify_provider_error(&error).disposition,
        RetryDisposition::FailFast
    );
}

#[tokio::test]
async fn gemini_optional_invalid_history_is_explicitly_omitted() {
    let (body, _) = capture_gemini_request(ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserImage {
            prompt: "historical image".into(),
            media_type: "application/pdf".into(),
            data_base64: String::from("sensitive-image-data").into(),
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
