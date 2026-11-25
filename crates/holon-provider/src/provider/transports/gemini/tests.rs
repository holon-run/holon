use super::*;

#[test]
fn system_wire_preserves_canonical_shared_prefix_and_plain_fallback() {
    use crate::provider::{PromptContentBlock, PromptStability, ProviderPromptFrame};

    let shared = "## identity\nfixed role\n\n## tool_contract\nfixed tools\n\n";
    for suffix in [
        "## workspace\n/agent-a/task-a",
        "## workspace\n/agent-b/task-b",
    ] {
        let rendered = format!("{shared}{suffix}");
        let mut request = ProviderTurnRequest::plain(&rendered, vec![], vec![]);
        for structured in [false, true] {
            if structured {
                request.prompt_frame = ProviderPromptFrame::structured(
                    &rendered,
                    vec![
                        PromptContentBlock {
                            text: shared.into(),
                            stability: PromptStability::Stable,
                            cache_breakpoint: true,
                        },
                        PromptContentBlock {
                            text: suffix.into(),
                            stability: PromptStability::AgentScoped,
                            cache_breakpoint: true,
                        },
                    ],
                    vec![],
                    None,
                );
            }
            let wire = serde_json::to_value(build_system_instruction(&request)).unwrap();
            assert_eq!(wire["parts"][0]["text"], rendered);
            assert!(wire["parts"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with(shared));
            assert!(!wire.to_string().contains("cache_control"));
        }
    }
}

#[test]
fn gemini_refusals_fail_closed_with_usage() {
    for reason in [
        "SAFETY",
        "RECITATION",
        "BLOCKLIST",
        "PROHIBITED_CONTENT",
        "SPII",
        "IMAGE_SAFETY",
        "IMAGE_PROHIBITED_CONTENT",
        "IMAGE_RECITATION",
    ] {
        for content in [
            Value::Null,
            json!({}),
            json!({"parts": []}),
            json!({"parts": [{"text": ""}]}),
            json!({"parts": [{"text": " \n\t"}]}),
            json!({"parts": [{"text": "partial response"}]}),
            json!({"parts": [{"functionCall": {"name": "ProbeTool", "args": {}}}]}),
            json!({"parts": [{"functionCall": {"args": {}}}]}),
            json!({"parts": "secret-invalid-parts"}),
            json!("secret-invalid-content"),
        ] {
            let parsed = serde_json::from_value(json!({
                "candidates": [{"content": content, "finishReason": reason}],
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 0}
            }))
            .unwrap();
            let error = gemini_response_to_provider_turn_response(parsed, None, None, None)
                .expect_err("rejected content must never become a successful round");
            let transport = error
                .downcast_ref::<crate::provider::ProviderTransportError>()
                .expect("typed refusal");
            assert_eq!(transport.code.as_deref(), Some("gemini_content_blocked"));
            assert_eq!(
                transport.classification.kind,
                ProviderFailureKind::InvalidResponse
            );
            assert_eq!(
                transport.classification.disposition,
                RetryDisposition::FailFast
            );
            assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 11);
            assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 0);
            assert!(error.to_string().contains(reason));
            assert!(!error.to_string().contains("partial response"));
            assert!(!error.to_string().contains("ProbeTool"));
        }
    }
}

#[test]
fn gemini_refusal_keeps_trace_identity_and_sanitized_route_diagnostics() {
    let home = tempfile::tempdir().unwrap();
    let trace = ProviderHttpTrace::failure_only_for_tests(home.path());
    let model_ref = "gemini@alternate/test";
    let url = "https://user:secret-password@example.com/v1?key=secret-key#secret-fragment";
    let request_trace = trace
        .begin_request(
            Some("gemini-refusal-test"),
            "gemini",
            Some(model_ref),
            url,
            "gemini_generate_content",
            &[],
            &json!({}),
        )
        .unwrap();
    let parsed = serde_json::from_value(json!({
        "candidates": [{"finishReason": "PROHIBITED_CONTENT"}]
    }))
    .unwrap();
    let error = gemini_response_to_provider_turn_response(
        parsed,
        Some(model_ref),
        Some(url),
        Some(&request_trace),
    )
    .unwrap_err();
    let diagnostics = crate::provider::provider_transport_diagnostics(&error).unwrap();
    assert_eq!(diagnostics.stage, "response_protocol");
    assert_eq!(diagnostics.model_ref.as_deref(), Some(model_ref));
    assert_eq!(diagnostics.streaming, Some(false));
    let capture = diagnostics
        .http_trace
        .as_ref()
        .expect("failure trace identity");
    assert!(!capture.capture_id.is_empty());
    assert!(std::path::Path::new(&capture.path).is_file());
    assert!(!format!("{error:?} {diagnostics:?}").contains("secret-"));
}

#[test]
fn gemini_response_preserves_empty_blocks_with_finish_reason_and_usage() {
    let response = gemini_response_to_provider_turn_response(
        GenerateContentResponse {
            candidates: vec![GeminiCandidate {
                content: Some(json!({"role": "model", "parts": []})),
                finish_reason: Some("MAX_TOKENS".to_string()),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: Some(11),
                candidates_token_count: Some(7),
                cached_content_token_count: None,
                thoughts_token_count: None,
            }),
            prompt_feedback: None,
        },
        None,
        None,
        None,
    )
    .expect("empty Gemini content should still be a valid response");

    assert!(response.blocks.is_empty());
    assert_eq!(response.stop_reason.as_deref(), Some("MAX_TOKENS"));
    assert_eq!(response.input_tokens, 11);
    assert_eq!(response.output_tokens, 7);
    assert_eq!(
        response.cache_usage,
        Some(ProviderCacheUsage {
            read_input_tokens: 0,
            creation_input_tokens: 0,
        }),
        "reported usage without cache counters is a measured zero, not unobserved"
    );
}

#[test]
fn gemini_response_accepts_content_without_parts() {
    let parsed: GenerateContentResponse = serde_json::from_value(json!({
        "candidates": [{
            "content": {},
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 11,
            "candidatesTokenCount": 7
        }
    }))
    .expect("Gemini content without parts should still deserialize");

    let error = gemini_response_to_provider_turn_response(parsed, None, None, None)
        .expect_err("decodable STOP content without parts is a retryable empty response");
    let transport = error
        .downcast_ref::<crate::provider::ProviderTransportError>()
        .unwrap();
    assert_eq!(
        transport.classification.kind,
        ProviderFailureKind::EmptyResponse
    );
    assert_eq!(
        transport.classification.disposition,
        RetryDisposition::Retryable
    );
    assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 11);
    assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 7);
}

#[test]
fn gemini_prompt_feedback_blocks_without_candidates_and_redacts_unknown_reasons() {
    for candidates in [
        None,
        Some(json!([])),
        Some(json!([{"content": {"parts": [{"functionCall": {"args": {}}}]}}])),
    ] {
        for reason in ["SAFETY", "OTHER", "secret-unrecognized-reason"] {
            let mut body = json!({
                "promptFeedback": {"blockReason": reason, "blockReasonMessage": "secret-message"},
                "usageMetadata": {"promptTokenCount": 13, "candidatesTokenCount": 2}
            });
            if let Some(candidates) = candidates.as_ref() {
                body["candidates"] = candidates.clone();
            }
            let parsed = serde_json::from_value(body).unwrap();
            let error = gemini_response_to_provider_turn_response(parsed, None, None, None)
                .expect_err("prompt rejection must be typed even without candidates");
            let transport = error
                .downcast_ref::<crate::provider::ProviderTransportError>()
                .unwrap();
            assert_eq!(transport.code.as_deref(), Some("gemini_content_blocked"));
            assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 13);
            assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 2);
            assert!(!format!("{transport:?}").contains("secret-"));
            if reason.starts_with("secret-") {
                assert!(error.to_string().contains("UNKNOWN_BLOCK_REASON"));
            }
        }
    }
}

#[test]
fn gemini_semantic_empty_responses_are_retryable_but_max_tokens_is_preserved() {
    for reason in [
        Value::Null,
        json!("STOP"),
        json!("UNRECOGNIZED"),
        json!("MAX_TOKENS"),
    ] {
        for content in [
            Value::Null,
            json!({}),
            json!({"parts": []}),
            json!({"parts": [{"text": ""}]}),
            json!({"parts": [{"text": " \n\t"}]}),
        ] {
            let parsed = serde_json::from_value(json!({
                "candidates": [{"content": content, "finishReason": reason}],
                "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 3}
            }))
            .unwrap();
            let result = gemini_response_to_provider_turn_response(parsed, None, None, None);
            if reason == "MAX_TOKENS" {
                let response = result.unwrap();
                assert_eq!(response.stop_reason.as_deref(), Some("MAX_TOKENS"));
                assert_eq!((response.input_tokens, response.output_tokens), (5, 3));
            } else {
                let error = result.expect_err("no semantic output");
                let transport = error
                    .downcast_ref::<crate::provider::ProviderTransportError>()
                    .unwrap();
                assert_eq!(
                    transport.classification.kind,
                    ProviderFailureKind::EmptyResponse
                );
                assert_eq!(
                    transport.classification.disposition,
                    RetryDisposition::Retryable
                );
                assert_eq!(transport.token_usage.as_ref().unwrap().input_tokens, 5);
                assert_eq!(transport.token_usage.as_ref().unwrap().output_tokens, 3);
            }
        }
    }
    for body in [
        json!({}),
        json!({"candidates": []}),
        json!({
            "promptFeedback": {"blockReason": "BLOCK_REASON_UNSPECIFIED"}
        }),
    ] {
        let parsed = serde_json::from_value(body).unwrap();
        let error =
            gemini_response_to_provider_turn_response(parsed, None, None, None).unwrap_err();
        assert_eq!(
            crate::provider::classify_provider_error(&error).kind,
            ProviderFailureKind::EmptyResponse
        );
    }
}

#[test]
fn gemini_non_refusal_text_and_signed_tools_remain_successful() {
    for reason in ["STOP", "MAX_TOKENS", "UNKNOWN_FUTURE_REASON"] {
        let parsed = serde_json::from_value(json!({
                "candidates": [{
                    "finishReason": reason,
                    "content": {"parts": [
                        {"text": " unmodified text "},
                        {"functionCall": {"name": "ProbeTool", "args": {"ok": true}}, "thoughtSignature": "signature"}
                    ]}
                }]
            })).unwrap();
        let response = gemini_response_to_provider_turn_response(parsed, None, None, None).unwrap();
        assert!(
            matches!(&response.blocks[0], ModelBlock::Text { text } if text == " unmodified text ")
        );
        assert!(
            matches!(&response.blocks[1], ModelBlock::ToolUse { provider_data: Some(data), .. }
                if data.payload["thoughtSignature"] == "signature")
        );
        assert_eq!(response.stop_reason.as_deref(), Some(reason));
    }
}

#[test]
fn gemini_tool_use_ids_are_unique_and_responses_restore_function_name() {
    let blocks = gemini_parts_to_model_blocks(vec![
        GeminiPart {
            text: None,
            inline_data: None,
            function_call: Some(GeminiFunctionCall {
                name: "ProbeTool".to_string(),
                args: json!({"first": true}),
            }),
            function_response: None,
            thought_signature: None,
        },
        GeminiPart {
            text: None,
            inline_data: None,
            function_call: Some(GeminiFunctionCall {
                name: "ProbeTool".to_string(),
                args: json!({"second": true}),
            }),
            function_response: None,
            thought_signature: None,
        },
    ]);

    let first_id = match &blocks[0] {
        ModelBlock::ToolUse { id, name, .. } => {
            assert_eq!(name, "ProbeTool");
            id.clone()
        }
        other => panic!("expected first tool use, got {other:?}"),
    };
    let second_id = match &blocks[1] {
        ModelBlock::ToolUse { id, name, .. } => {
            assert_eq!(name, "ProbeTool");
            id.clone()
        }
        other => panic!("expected second tool use, got {other:?}"),
    };

    assert_ne!(first_id, second_id);
    assert_eq!(gemini_function_response_name(&first_id), "ProbeTool");
    assert_eq!(gemini_function_response_name(&second_id), "ProbeTool");
}

#[test]
fn gemini_response_content_role_is_optional() {
    let response: GenerateContentResponse = serde_json::from_value(json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "functionCall": {
                        "name": "ProbeTool",
                        "args": {"token": "probe"}
                    },
                    "thoughtSignature": "opaque-signature"
                }]
            },
            "finishReason": "STOP"
        }]
    }))
    .expect("Gemini response content should not require role");

    let content: GeminiContent = serde_json::from_value(
        response.candidates[0]
            .content
            .clone()
            .expect("candidate should include content"),
    )
    .expect("Gemini response content should not require role");
    assert!(content.role.is_empty());
    assert_eq!(
        content.parts[0].thought_signature.as_deref(),
        Some("opaque-signature")
    );
}

#[test]
fn gemini_tool_use_thought_signature_survives_persistence_and_replay() {
    let signature = "opaque-thought-signature";
    let blocks = gemini_parts_to_model_blocks(vec![GeminiPart {
        text: None,
        inline_data: None,
        function_call: Some(GeminiFunctionCall {
            name: "ProbeTool".to_string(),
            args: json!({"probe": true}),
        }),
        function_response: None,
        thought_signature: Some(signature.to_string()),
    }]);

    let persisted = serde_json::to_vec(&blocks).expect("tool use should serialize");
    let restored: Vec<ModelBlock> =
        serde_json::from_slice(&persisted).expect("tool use should deserialize");
    let content =
        conversation_message_to_gemini_content(&ConversationMessage::AssistantBlocks(restored))
            .expect("assistant tool use should produce Gemini content");
    let payload = serde_json::to_value(content).expect("Gemini content should serialize");

    assert_eq!(payload["parts"][0]["functionCall"]["name"], "ProbeTool");
    assert_eq!(
        payload["parts"][0]["functionCall"]["args"],
        json!({"probe": true})
    );
    assert_eq!(payload["parts"][0]["thoughtSignature"], signature);
}

#[test]
fn gemini_tool_use_without_provider_data_omits_thought_signature() {
    let content =
        conversation_message_to_gemini_content(&ConversationMessage::AssistantBlocks(vec![
            ModelBlock::ToolUse {
                id: "call-1".to_string(),
                name: "ProbeTool".to_string(),
                input: json!({"probe": true}),
                kind: ModelToolCallKind::Function,
                provider_data: None,
            },
        ]))
        .expect("assistant tool use should produce Gemini content");
    let payload = serde_json::to_value(content).expect("Gemini content should serialize");

    assert!(payload["parts"][0].get("thoughtSignature").is_none());
}

#[test]
fn gemini_tool_schemas_strip_fields_gemini_rejects() {
    let request_tools = json!({
        "type": "object",
        "properties": {
            "cmd": {"type": "string", "description": "command"},
            "labels": {
                "type": "object",
                "additionalProperties": true,
                "title": "Labels",
            },
            "mode": {
                "oneOf": [
                    {"type": "string", "enum": ["a"]},
                    {"type": "string", "enum": ["b"]},
                ],
            },
            "maybe": {"type": ["string", "null"]},
        },
        "required": ["cmd"],
        "additionalProperties": false,
        "$schema": "http://json-schema.org/draft-07/schema#",
    });

    let lowered = gemini_safe_schema(&request_tools);
    let lowered_text = lowered.to_string();
    assert!(!lowered_text.contains("additionalProperties"));
    assert!(!lowered_text.contains("$schema"));
    assert!(!lowered_text.contains("title"));
    assert_eq!(lowered["type"], "object");
    assert_eq!(lowered["properties"]["labels"]["type"], "object");
    let mode_variants = lowered["properties"]["mode"]["anyOf"]
        .as_array()
        .expect("oneOf should lower to anyOf");
    assert_eq!(mode_variants.len(), 2);
    assert_eq!(lowered["properties"]["maybe"]["type"], "string");
    assert_eq!(lowered["properties"]["maybe"]["nullable"], true);
}

#[test]
fn gemini_tool_schemas_lower_boolean_and_tuple_schemas() {
    assert_eq!(
        gemini_safe_schema(&Value::Bool(true)),
        json!({"type": "object"})
    );
    let tuple = gemini_safe_schema(&json!({
        "type": "array",
        "items": [{"type": "string"}, {"type": "number"}],
    }));
    assert_eq!(tuple["items"], json!({"type": "string"}));
}

fn gemini_native_search_request(kind: ProviderNativeWebSearchKind) -> ProviderTurnRequest {
    let mut request = ProviderTurnRequest::plain("search", vec![], vec![]);
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind,
        provider_id: "gemini".into(),
        provider_model_ref: "gemini/gemini-test".into(),
        advertised_tool_type: "google_search".into(),
        backend_kind: "gemini_web_search".into(),
        max_results: Some(5),
    });
    request
}

#[test]
fn gemini_tools_mount_google_search_for_gemini_native_search() {
    let tools = build_gemini_tools(&gemini_native_search_request(
        ProviderNativeWebSearchKind::Gemini,
    ));
    assert_eq!(tools, vec![json!({ "google_search": {} })]);
}

#[test]
fn gemini_tools_skip_native_search_of_other_kinds() {
    let tools = build_gemini_tools(&gemini_native_search_request(
        ProviderNativeWebSearchKind::OpenAi,
    ));
    assert!(tools.is_empty());
}

#[test]
fn gemini_native_search_diagnostics_mark_only_gemini_lowered() {
    let lowered = gemini_native_web_search_diagnostics(&gemini_native_search_request(
        ProviderNativeWebSearchKind::Gemini,
    ))
    .expect("gemini kind should produce diagnostics");
    assert!(lowered.lowered);
    assert_eq!(lowered.advertised_tool_type, "google_search");
    assert!(lowered.fallback_reason.is_none());

    let not_lowered = gemini_native_web_search_diagnostics(&gemini_native_search_request(
        ProviderNativeWebSearchKind::OpenAi,
    ))
    .expect("non-gemini kind still reports diagnostics");
    assert!(!not_lowered.lowered);
    assert!(not_lowered.fallback_reason.is_some());
}

#[test]
fn gemini_native_search_output_parses_grounding_citations() {
    let response = json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "grounded answer" }]
            },
            "finishReason": "STOP",
            "groundingMetadata": {
                "webSearchQueries": ["fixture query"],
                "groundingChunks": [
                    { "web": { "uri": "https://example.test/first", "title": "First" } },
                    { "web": { "uri": "https://example.test/second" } },
                    { "web": { "uri": "", "title": "Dropped empty uri" } },
                    { "web": { "title": "Missing uri" } }
                ]
            }
        }],
        "usageMetadata": { "promptTokenCount": 4, "candidatesTokenCount": 6 }
    });
    let outcome = parse_gemini_native_web_search_output(&response);
    assert!(outcome.search_executed);
    assert_eq!(outcome.citations.len(), 2);
    assert_eq!(outcome.citations[0].url, "https://example.test/first");
    assert_eq!(outcome.citations[0].title.as_deref(), Some("First"));
    assert_eq!(outcome.citations[1].url, "https://example.test/second");
    assert_eq!(outcome.citations[1].title, None);
}

#[test]
fn gemini_native_search_output_requires_grounding_evidence() {
    let response = json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "ungrounded answer" }]
            },
            "finishReason": "STOP"
        }]
    });
    let outcome = parse_gemini_native_web_search_output(&response);
    assert!(!outcome.search_executed);
    assert!(outcome.citations.is_empty());
}

#[test]
fn gemini_usage_reports_cached_and_thinking_tokens() {
    let parsed: GenerateContentResponse = serde_json::from_value(json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "ok" }] },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 1200,
            "cachedContentTokenCount": 800,
            "candidatesTokenCount": 40,
            "thoughtsTokenCount": 60
        }
    }))
    .expect("Gemini usage metadata should deserialize");

    let response = gemini_response_to_provider_turn_response(parsed, None, None, None)
        .expect("text response with usage should be valid");

    // `promptTokenCount` already includes the cached content tokens.
    assert_eq!(response.input_tokens, 1200);
    // `thoughtsTokenCount` is a sibling addend of `candidatesTokenCount`.
    assert_eq!(response.output_tokens, 100);
    assert_eq!(
        response.cache_usage,
        Some(ProviderCacheUsage {
            read_input_tokens: 800,
            creation_input_tokens: 0,
        })
    );
}

#[test]
fn gemini_usage_without_metadata_stays_unobserved() {
    let parsed: GenerateContentResponse = serde_json::from_value(json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "ok" }] },
            "finishReason": "STOP"
        }]
    }))
    .expect("response without usage metadata should deserialize");

    let response = gemini_response_to_provider_turn_response(parsed, None, None, None)
        .expect("text response without usage should be valid");

    assert_eq!(response.input_tokens, 0);
    assert_eq!(response.output_tokens, 0);
    assert_eq!(
        response.cache_usage, None,
        "missing usageMetadata must stay unobserved instead of measured zero"
    );
}
