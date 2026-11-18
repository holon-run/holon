use holon_tools::builtins::timer::{validate_positive, CreateTimerArgs, CREATE_NAME};
use holon_tools::ToolError;
use serde_json::json;

#[test]
fn create_timer_rejects_unknown_fields_and_zero_durations() {
    let error = serde_json::from_value::<CreateTimerArgs>(json!({
        "duration_ms": 1,
        "agent_id": "other",
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field `agent_id`"));

    let error = validate_positive(CREATE_NAME, "duration_ms", 0).unwrap_err();
    assert_eq!(
        error.downcast_ref::<ToolError>().unwrap().kind,
        "invalid_tool_input"
    );
}
