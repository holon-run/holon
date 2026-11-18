//! Tool schema contract tests.

use super::support::*;
use super::*;
use crate::provider::retry::{ProviderFailureKind, RetryDisposition};
use crate::provider::transports::OpenAiResponsesTransportContract;
use holon_domain::model_values::ModelVerbosity;
use serde_json::{json, Value};

#[test]
fn relaxed_emitted_schema_disables_additional_properties_recursively() {
    let schema = json!({
        "type": "object",
        "properties": {
            "payload": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                }
            }
        }
    });
    let relaxed = emitted_tool_json_schema(&schema, ToolSchemaContract::Relaxed).unwrap();
    assert_eq!(relaxed["additionalProperties"], Value::Bool(false));
    assert_eq!(
        relaxed["properties"]["payload"]["additionalProperties"],
        Value::Bool(false)
    );
    validate_emitted_tool_schema(&relaxed, ToolSchemaContract::Relaxed).unwrap();
}

#[test]
fn openai_input_preserves_tool_results() {
    let input = build_openai_input(&[
        ConversationMessage::UserText("inspect".into()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "call_1".into(),
            name: "ExecCommand".into(),
            input: json!({"cmd": "sed -n '1,40p' src/main.rs", "workdir": "."}),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "call_1".into(),
            content: "file contents".into(),
            is_error: false,
            error: None,
        }]),
    ])
    .unwrap();
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[2]["type"], "function_call_output");
}

#[test]
fn openai_input_preserves_apply_patch_function_tool_results() {
    let input = build_openai_input(&[
        ConversationMessage::UserText("inspect".into()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "call_1".into(),
            name: "ApplyPatch".into(),
            input: json!("--- /dev/null\n+++ b/note.txt\n@@ -0,0 +1,1 @@\n+hi\n"),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "call_1".into(),
            content: "Success. Updated the following files:\nA note.txt".into(),
            is_error: false,
            error: None,
        }]),
    ])
    .unwrap();
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[2]["type"], "function_call_output");
}

#[test]
fn openai_input_preserves_apply_patch_custom_tool_results() {
    let input = build_openai_input(&[
        ConversationMessage::UserText("inspect".into()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "call_1".into(),
            name: "ApplyPatch".into(),
            input: json!("--- /dev/null\n+++ b/note.txt\n@@ -0,0 +1,1 @@\n+hi\n"),
            kind: crate::provider::ModelToolCallKind::Custom,
            provider_data: None,
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "call_1".into(),
            content: "Success. Updated the following files:\nA note.txt".into(),
            is_error: false,
            error: None,
        }]),
    ])
    .unwrap();
    assert_eq!(input[1]["type"], "custom_tool_call");
    assert_eq!(input[2]["type"], "custom_tool_call_output");
}

#[test]
fn parse_openai_response_handles_text_and_function_calls() {
    let response = json!({
        "status": "completed",
        "usage": {
            "input_tokens": 11,
            "output_tokens": 7,
            "input_tokens_details": {
                "cached_tokens": 5
            }
        },
        "output": [
            {
                "type": "message",
                "content": [
                    { "type": "output_text", "text": "done" }
                ]
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "ExecCommand",
                "arguments": "{\"cmd\":\"sed -n '1,40p' src/main.rs\",\"workdir\":\".\"}"
            }
        ]
    });
    let parsed = parse_openai_response(response).unwrap();
    assert_eq!(parsed.blocks.len(), 2);
    assert_eq!(parsed.input_tokens, 11);
    assert_eq!(parsed.output_tokens, 7);
    assert_eq!(
        parsed
            .cache_usage
            .as_ref()
            .map(|usage| usage.read_input_tokens),
        Some(5)
    );
    assert!(matches!(
        &parsed.blocks[1],
        ModelBlock::ToolUse {
            kind: crate::provider::ModelToolCallKind::Function,
            ..
        }
    ));
}

#[test]
fn model_tool_call_kind_defaults_to_function_for_legacy_blocks() {
    let block: ModelBlock = serde_json::from_value(json!({
        "type": "tool_use",
        "id": "call_legacy",
        "name": "ApplyPatch",
        "input": {
            "patch": "legacy function probe"
        }
    }))
    .unwrap();
    assert!(matches!(
        block,
        ModelBlock::ToolUse {
            kind: crate::provider::ModelToolCallKind::Function,
            ..
        }
    ));
}

#[test]
fn model_tool_call_kind_preserves_custom_across_serialization() {
    let block = ModelBlock::ToolUse {
        id: "call_custom".into(),
        name: "ApplyPatch".into(),
        input: json!("custom patch probe"),
        kind: crate::provider::ModelToolCallKind::Custom,
        provider_data: None,
    };
    let serialized = serde_json::to_value(&block).unwrap();
    assert_eq!(serialized["kind"], json!("custom"));
    let restored: ModelBlock = serde_json::from_value(serialized).unwrap();
    assert!(matches!(
        restored,
        ModelBlock::ToolUse {
            id,
            name,
            input,
            kind: crate::provider::ModelToolCallKind::Custom,
            provider_data: None,
        } if id == "call_custom" && name == "ApplyPatch" && input == json!("custom patch probe")
    ));
}

#[test]
fn parse_openai_response_handles_custom_tool_calls() {
    let response = json!({
        "status": "completed",
        "output": [
            {
                "type": "custom_tool_call",
                "call_id": "call_patch",
                "name": "ApplyPatch",
                "input": "--- /dev/null\n+++ b/note.txt\n@@ -0,0 +1,1 @@\n+hi\n"
            }
        ]
    });
    let parsed = parse_openai_response(response).unwrap();
    assert_eq!(parsed.blocks.len(), 1);
    match &parsed.blocks[0] {
        ModelBlock::ToolUse {
            id,
            name,
            input,
            kind,
            ..
        } => {
            assert_eq!(id, "call_patch");
            assert_eq!(name, "ApplyPatch");
            assert_eq!(*kind, crate::provider::ModelToolCallKind::Custom);
            assert_eq!(
                input.as_str(),
                Some("--- /dev/null\n+++ b/note.txt\n@@ -0,0 +1,1 @@\n+hi\n")
            );
        }
        _ => panic!("expected tool use block"),
    }
}

#[test]
fn parse_openai_response_falls_back_to_prompt_token_details_for_cached_tokens() {
    let response = json!({
        "status": "completed",
        "usage": {
            "input_tokens": 11,
            "output_tokens": 7,
            "prompt_tokens_details": {
                "cached_tokens": 3
            }
        },
        "output": [
            {
                "type": "message",
                "content": [
                    { "type": "output_text", "text": "done" }
                ]
            }
        ]
    });
    let parsed = parse_openai_response(response).unwrap();
    assert_eq!(
        parsed
            .cache_usage
            .as_ref()
            .map(|usage| usage.read_input_tokens),
        Some(3)
    );
}

#[test]
fn parse_openai_response_classifies_shape_errors_as_invalid_response() {
    let error = parse_openai_response(json!({
        "status": "completed",
        "output": [
            {
                "type": "function_call",
                "name": "ExecCommand",
                "arguments": "{}"
            }
        ]
    }))
    .err()
    .expect("missing call_id should fail");

    let classification = retry::classify_provider_error(&error);
    assert_eq!(classification.kind, ProviderFailureKind::InvalidResponse);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
}

#[test]
fn openai_responses_request_sets_store_false() {
    let request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("hello".into())],
        vec![ToolSpec {
            name: "ExecCommand".into(),
            description: "Run a shell command".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string" }
                },
                "required": ["cmd"]
            }),
            freeform_grammar: None,
        }],
    );

    let body = build_openai_responses_request(
        "gpt-5.3-codex-spark",
        2048,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Relaxed,
        None,
        None,
    )
    .unwrap();
    assert_eq!(body["store"], Value::Bool(false));
}

#[test]
fn openai_responses_request_lowers_prompt_frame_to_full_request_with_cache_key() {
    let request = provider_turn_request_with_prompt_frame();

    let body = build_openai_responses_request(
        "gpt-5.4",
        2048,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Relaxed,
        None,
        None,
    )
    .unwrap();

    assert_eq!(body["instructions"], json!("rendered system"));
    assert_eq!(body["prompt_cache_key"], json!("cache-key"));
    assert_eq!(
        body["input"][0]["content"][0]["text"],
        json!("agent context")
    );
}

#[test]
fn build_openai_standard_request_includes_max_output_tokens() {
    let request = provider_turn_request();
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Relaxed,
        None,
        None,
    )
    .unwrap();

    assert_eq!(body["max_output_tokens"], Value::from(256));
    assert!(body.get("stream").is_none());
}

#[test]
fn build_openai_codex_streaming_request_omits_max_output_tokens() {
    let request = provider_turn_request();
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        None,
        None,
    )
    .unwrap();

    assert_eq!(body["stream"], Value::Bool(true));
    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("reasoning").is_some());
    assert_eq!(body["reasoning"], Value::Null);
    assert_eq!(body["include"], json!([]));
}

#[test]
fn build_openai_codex_streaming_request_sends_verbosity() {
    let request = provider_turn_request();
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        None,
        Some(ModelVerbosity::Low),
    )
    .unwrap();

    assert_eq!(body["stream"], Value::Bool(true));
    assert_eq!(body["text"], json!({ "verbosity": "low" }));
}

#[test]
fn build_openai_codex_streaming_request_sends_supported_reasoning_effort() {
    let request = provider_turn_request();
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        Some("low"),
        None,
    )
    .unwrap();

    assert_eq!(body["stream"], Value::Bool(true));
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["reasoning"], json!({ "effort": "low" }));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}
