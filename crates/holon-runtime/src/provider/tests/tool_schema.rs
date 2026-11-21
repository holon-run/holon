//! Tool schema contract tests.

use super::support::*;
use super::*;
use crate::provider::transports::OpenAiResponsesTransportContract;
use crate::tool::{apply_patch::ApplyPatchSurface, tools::apply_patch_tool};
use serde_json::{json, Value};

#[test]
fn emitted_invoke_agent_schema_keeps_union_nested_under_target() {
    let tools = trusted_tool_specs();
    let invoke_agent = tools
        .into_iter()
        .find(|spec| spec.name == "InvokeAgent")
        .expect("InvokeAgent should be present");

    let relaxed =
        emitted_tool_json_schema(&invoke_agent.input_schema, ToolSchemaContract::Relaxed).unwrap();

    assert_eq!(relaxed["type"], "object");
    for forbidden in ["allOf", "anyOf", "oneOf", "enum", "not"] {
        assert!(
            relaxed.get(forbidden).is_none(),
            "InvokeAgent emitted schema should not contain top-level {forbidden}: {relaxed}"
        );
    }
    let target = &relaxed["properties"]["target"];
    assert!(target.to_string().contains("existing_agent"));
    assert!(target.to_string().contains("new_subagent"));
    validate_emitted_tool_schema(&relaxed, ToolSchemaContract::Relaxed).unwrap();
}

#[test]
fn openai_request_uses_function_tool_shape_for_generic_apply_patch() {
    let apply_patch = tool_spec_named("ApplyPatch");
    let request = provider_turn_request_with_tools(vec![apply_patch]);
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

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "ApplyPatch");
    assert_eq!(
        body["tools"][0]["parameters"]["properties"]["patch"]["type"],
        "string"
    );
    assert!(body["tools"][0].get("format").is_none());
    let description = body["tools"][0]["description"]
        .as_str()
        .expect("ApplyPatch description should be a string");
    assert!(!description.contains("*** Begin Patch"));
    assert!(!description.contains("*** End Patch"));
    let parameters = body["tools"][0]["parameters"].to_string();
    assert!(!parameters.contains("*** Begin Patch"));
    assert!(!parameters.contains("*** End Patch"));
}

#[test]
fn openai_codex_request_uses_custom_codex_dsl_tool_shape_for_apply_patch() {
    let apply_patch = apply_patch_tool::definition_for_surface(ApplyPatchSurface::CodexDslFreeform)
        .unwrap()
        .spec;
    let request = provider_turn_request_with_tools(vec![apply_patch]);
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

    assert_eq!(body["tools"][0]["type"], "custom");
    assert_eq!(body["tools"][0]["name"], "ApplyPatch");
    assert_eq!(body["tools"][0]["format"]["type"], "grammar");
    assert_eq!(body["tools"][0]["format"]["syntax"], "lark");
    let grammar = body["tools"][0]["format"]["definition"]
        .as_str()
        .expect("grammar definition should be a string");
    assert!(grammar.contains("*** Begin Patch"));
    assert!(!grammar.contains("old_file: \"--- \" file_path LF"));
}

#[test]
fn deepseek_responses_request_uses_custom_apply_patch_tool_shape() {
    let apply_patch = apply_patch_tool::definition_for_surface(ApplyPatchSurface::CodexDslFreeform)
        .unwrap()
        .spec;
    assert_eq!(apply_patch.name, "ApplyPatch");
    let request = provider_turn_request_with_tools(vec![apply_patch]);
    let body = build_openai_responses_request(
        "deepseek-v4-pro",
        256,
        &request,
        OpenAiResponsesTransportContract::DeepSeekStreaming,
        ToolSchemaContract::Relaxed,
        Some("high"),
        None,
    )
    .unwrap();

    assert_eq!(body["tools"][0]["type"], "custom");
    assert_eq!(body["tools"][0]["name"], "apply_patch");
    assert_eq!(body["tools"][0]["format"]["type"], "grammar");
    assert_eq!(body["reasoning"], json!({ "effort": "high" }));
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
        None,
    )
    .unwrap();

    assert_eq!(body["max_output_tokens"], Value::from(256));
    let emitted_tools = body["tools"].as_array().expect("tools should be an array");
    assert_eq!(emitted_tools.len(), tools.len());
    for retired in ["Read", "Glob", "Grep"] {
        assert!(emitted_tools.iter().all(|tool| tool["name"] != retired));
    }
    assert!(emitted_tools.iter().all(|tool| tool["name"] != "Sleep"));
    assert!(emitted_tools.iter().any(|tool| tool["name"] == "WaitFor"));
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

    let memory_get = emitted_tools
        .iter()
        .find(|tool| tool["name"] == "MemoryGet")
        .expect("MemoryGet tool should be present");
    assert_eq!(
        memory_get["parameters"]["properties"]["max_chars"]["minimum"],
        Value::from(1.0)
    );
    assert_eq!(
        memory_get["parameters"]["properties"]["max_chars"]["maximum"],
        Value::from(50000.0)
    );
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
        None,
    )
    .unwrap();

    assert_eq!(body["stream"], Value::Bool(true));
    assert!(body.get("max_output_tokens").is_none());
    let emitted_tools = body["tools"].as_array().expect("tools should be an array");
    assert_eq!(emitted_tools.len(), tools.len());
    for retired in ["Read", "Glob", "Grep"] {
        assert!(emitted_tools.iter().all(|tool| tool["name"] != retired));
    }
    assert!(emitted_tools.iter().all(|tool| tool["name"] != "Sleep"));
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

    assert!(emitted_tools
        .iter()
        .all(|tool| tool["name"] != "CreateExternalTrigger"));
    assert!(emitted_tools
        .iter()
        .all(|tool| tool["name"] != "CancelExternalTrigger"));
}
