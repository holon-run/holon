//! Tool schema contract tests.

use super::support::*;
use super::*;
use crate::provider::retry::{ProviderFailureKind, RetryDisposition};
use crate::provider::transports::OpenAiResponsesTransportContract;
use crate::tool::ToolSpec;
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
fn emitted_spawn_agent_schema_strips_openai_incompatible_top_level_composition() {
    let tools = trusted_tool_specs();
    let spawn_agent = tools
        .into_iter()
        .find(|spec| spec.name == "SpawnAgent")
        .expect("SpawnAgent should be present");

    let relaxed =
        emitted_tool_json_schema(&spawn_agent.input_schema, ToolSchemaContract::Relaxed).unwrap();

    assert_eq!(relaxed["type"], "object");
    for forbidden in ["allOf", "anyOf", "oneOf", "enum", "not"] {
        assert!(
            relaxed.get(forbidden).is_none(),
            "SpawnAgent emitted schema should not contain top-level {forbidden}: {relaxed}"
        );
    }
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
fn openai_input_preserves_apply_patch_custom_tool_results() {
    let input = build_openai_input(&[
        ConversationMessage::UserText("inspect".into()),
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "call_1".into(),
            name: "ApplyPatch".into(),
            input: json!("--- /dev/null\n+++ b/note.txt\n@@ -0,0 +1,1 @@\n+hi\n"),
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
        ModelBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_patch");
            assert_eq!(name, "ApplyPatch");
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

    let classification = super::super::retry::classify_provider_error(&error);
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
    )
    .unwrap();

    assert_eq!(body["stream"], Value::Bool(true));
    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("reasoning").is_some());
    assert_eq!(body["reasoning"], Value::Null);
    assert_eq!(body["include"], json!([]));
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
    )
    .unwrap();

    assert_eq!(body["stream"], Value::Bool(true));
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["reasoning"], json!({ "effort": "low" }));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn openai_request_uses_custom_tool_shape_for_apply_patch() {
    let apply_patch = tool_spec_named("ApplyPatch");
    let request = provider_turn_request_with_tools(vec![apply_patch]);
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Relaxed,
        None,
    )
    .unwrap();

    assert_eq!(body["tools"][0]["type"], "custom");
    assert_eq!(body["tools"][0]["name"], "ApplyPatch");
    assert_eq!(body["tools"][0]["format"]["type"], "grammar");
    assert_eq!(body["tools"][0]["format"]["syntax"], "lark");
    let grammar = body["tools"][0]["format"]["definition"]
        .as_str()
        .expect("grammar definition should be a string");
    assert!(grammar.contains("old_file: \"--- \" file_path LF"));
    assert!(grammar.contains("new_file: \"+++ \" file_path LF"));
    assert!(!grammar.contains("*** Begin Patch"));
}

#[test]
fn built_in_tool_source_schemas_remain_valid() {
    for spec in trusted_tool_specs() {
        crate::tool::schema::validate_source_tool_schema(&spec.input_schema)
            .unwrap_or_else(|error| panic!("{} source schema invalid: {error}", spec.name));
    }
}

#[test]
fn openai_request_payload_validates_full_tool_matrix_in_strict_mode() {
    let tools = trusted_tool_specs();
    let request = provider_turn_request_with_tools(tools.clone());
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Strict,
        None,
    )
    .unwrap();

    assert_eq!(body["max_output_tokens"], Value::from(256));
    let emitted_tools = body["tools"].as_array().expect("tools should be an array");
    assert_eq!(emitted_tools.len(), tools.len());
    for retired in ["Read", "Glob", "Grep"] {
        assert!(emitted_tools.iter().all(|tool| tool["name"] != retired));
    }
    for tool in emitted_tools {
        if tool["type"] == "custom" {
            assert_eq!(tool["name"], "ApplyPatch");
            assert_eq!(tool["format"]["type"], "grammar");
            assert_eq!(tool["format"]["syntax"], "lark");
        } else {
            assert_eq!(tool["strict"], Value::Bool(true));
            validate_emitted_tool_schema(&tool["parameters"], ToolSchemaContract::Strict).unwrap();
        }
    }

    let enqueue = emitted_tools
        .iter()
        .find(|tool| tool["name"] == "Enqueue")
        .expect("Enqueue tool should be present");
    let enqueue_required = enqueue["parameters"]["required"]
        .as_array()
        .expect("required should be an array");
    assert_eq!(enqueue_required.len(), 2);
    assert!(enqueue_required.iter().any(|value| value == "text"));
    assert!(enqueue_required.iter().any(|value| value == "priority"));
    let priority_types = enqueue["parameters"]["properties"]["priority"]["type"]
        .as_array()
        .expect("priority type should be an array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(priority_types.contains(&"string"));
    assert!(priority_types.contains(&"null"));
    assert!(enqueue["parameters"]["properties"]["priority"]["enum"]
        .as_array()
        .expect("priority enum should be an array")
        .iter()
        .any(Value::is_null));

    let sleep = emitted_tools
        .iter()
        .find(|tool| tool["name"] == "Sleep")
        .expect("Sleep tool should be present");
    let duration_types = sleep["parameters"]["properties"]["duration_ms"]["type"]
        .as_array()
        .expect("duration_ms type should be an array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(duration_types.contains(&"integer"));
    assert!(duration_types.contains(&"null"));
}

#[test]
fn openai_codex_request_payload_validates_full_tool_matrix_in_strict_mode() {
    let tools = trusted_tool_specs();
    let request = provider_turn_request_with_tools(tools.clone());
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Strict,
        Some("low"),
    )
    .unwrap();

    assert_eq!(body["stream"], Value::Bool(true));
    assert!(body.get("max_output_tokens").is_none());
    let emitted_tools = body["tools"].as_array().expect("tools should be an array");
    assert_eq!(emitted_tools.len(), tools.len());
    for retired in ["Read", "Glob", "Grep"] {
        assert!(emitted_tools.iter().all(|tool| tool["name"] != retired));
    }
    for tool in emitted_tools {
        if tool["type"] == "custom" {
            assert_eq!(tool["name"], "ApplyPatch");
            assert_eq!(tool["format"]["type"], "grammar");
            assert_eq!(tool["format"]["syntax"], "lark");
        } else {
            assert_eq!(tool["strict"], Value::Bool(true));
            validate_emitted_tool_schema(&tool["parameters"], ToolSchemaContract::Strict).unwrap();
        }
    }

    let external_trigger = emitted_tools
        .iter()
        .find(|tool| tool["name"] == "CreateExternalTrigger")
        .expect("CreateExternalTrigger tool should be present");
    let properties = external_trigger["parameters"]["properties"]
        .as_object()
        .expect("properties should be an object");
    assert!(properties.contains_key("description"));
    assert!(properties.contains_key("source"));
    assert!(properties.contains_key("scope"));
    assert!(properties.contains_key("delivery_mode"));
    assert!(!properties.contains_key("summary"));
    assert!(!properties.contains_key("condition"));
    assert!(!properties.contains_key("resource"));
}
