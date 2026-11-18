use holon_tools::builtins::sleep::parse_sleep_args;
use holon_tools::ToolError;
use serde_json::json;

#[test]
fn sleep_rejects_unknown_top_level_fields() {
    let error = parse_sleep_args(&json!({
        "reason": "test",
        "duration_ms": 100,
        "summary": "should be rejected",
    }))
    .unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();

    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert!(tool_error
        .details
        .as_ref()
        .and_then(|value| value.get("parse_error"))
        .and_then(|value| value.as_str())
        .is_some_and(|error| error.contains("unknown field `summary`")));
}

#[test]
fn sleep_rejects_zero_duration_ms() {
    let error = parse_sleep_args(&json!({
        "reason": "pause briefly",
        "duration_ms": 0
    }))
    .unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();

    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert_eq!(
        tool_error.message,
        "Sleep `duration_ms` must be a positive integer when provided"
    );
    assert!(tool_error
        .details
        .as_ref()
        .and_then(|value| value.get("validation_error"))
        .and_then(|value| value.as_str())
        .is_some_and(|error| error == "must be greater than 0"));
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("positive integer")));
}
