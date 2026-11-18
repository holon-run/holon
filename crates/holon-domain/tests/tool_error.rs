use holon_domain::ToolError;
use serde_json::{json, Value};

const MODEL_VISIBLE_DETAILS_MAX_CHARS: usize = 1_000;

#[test]
fn tool_error_renders_as_pretty_json() {
    let rendered = ToolError::new("invalid_tool_input", "missing required string field")
        .with_details(json!({ "field": "cmd" }))
        .with_recovery_hint("provide a string value for `cmd`")
        .render();

    assert!(rendered.contains("\"kind\": \"invalid_tool_input\""));
    assert!(rendered.contains("\"field\": \"cmd\""));
    assert!(rendered.contains("provide a string value"));
}

#[test]
fn tool_error_model_rendering_uses_shared_corrective_receipt() {
    let rendered = ToolError::new("invalid_tool_input", "missing required string field")
        .with_details(json!({ "field": "cmd", "parse_error": "missing field `cmd`" }))
        .with_recovery_hint("provide a string value for `cmd`")
        .render_for_model(Some("ExecCommand"));
    let receipt: Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["tool_name"], "ExecCommand");
    assert_eq!(receipt["kind"], "invalid_tool_input");
    assert_eq!(receipt["message"], "missing required string field");
    assert_eq!(receipt["hint"], "provide a string value for `cmd`");
    assert_eq!(receipt["field"], "cmd");
    assert_eq!(receipt["retryable"], false);
    assert_eq!(receipt["details"]["parse_error"], "missing field `cmd`");
}

#[test]
fn tool_error_model_rendering_supports_runtime_failures() {
    let rendered = ToolError::new("command_spawn_failed", "failed to start command process")
        .with_recovery_hint("check the command path and arguments")
        .with_retryable(true)
        .render_for_model(Some("ExecCommand"));
    let receipt: Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["kind"], "command_spawn_failed");
    assert_eq!(receipt["hint"], "check the command path and arguments");
    assert_eq!(receipt["retryable"], true);
}

#[test]
fn tool_error_model_rendering_bounds_long_details_without_changing_canonical_error() {
    let raw_payload = "x".repeat(MODEL_VISIBLE_DETAILS_MAX_CHARS + 500);
    let error = ToolError::new("invalid_tool_input", "malformed tool input")
        .with_details(json!({ "raw_input": raw_payload.clone() }));
    let canonical = error.render();
    let rendered = error.render_for_model(Some("ApplyPatch"));
    let receipt: Value = serde_json::from_str(&rendered).unwrap();

    assert!(canonical.contains(&raw_payload));
    assert_eq!(receipt["details"]["omitted"], true);
    assert_eq!(
        receipt["details"]["reason"],
        "details exceeded model-visible budget"
    );
    assert!(receipt["details"]["sha256"].as_str().unwrap().len() >= 64);
    assert!(
        receipt["details"]["preview"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= MODEL_VISIBLE_DETAILS_MAX_CHARS
    );
    assert!(!rendered.contains(&raw_payload));
}

#[test]
fn tool_error_deserializes_legacy_shape_with_taxonomy_defaults() {
    let tool_error: ToolError = serde_json::from_value(serde_json::json!({
        "kind": "tool_execution_failed",
        "message": "legacy failure",
        "retryable": false
    }))
    .unwrap();

    assert_eq!(tool_error.domain, None);
    assert!(tool_error.source_chain.is_empty());
}
