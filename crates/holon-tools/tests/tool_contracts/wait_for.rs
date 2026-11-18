use holon_tools::builtins::wait_for::*;
use holon_tools::ToolError;
use serde_json::json;

#[test]
fn wait_for_rejects_unknown_top_level_fields() {
    let error = parse_wait_for_args(&json!({
        "reason": "wait",
        "wake": "operator_input",
        "summary": "not allowed",
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
fn wait_for_requires_resource_for_task_and_timer_waits() {
    for wake in [WaitForWakeArg::TaskResult, WaitForWakeArg::Timer] {
        let error = validate_resource_for_wake(wake, None).unwrap_err();
        let tool_error = error.downcast_ref::<ToolError>().unwrap();
        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert_eq!(
            tool_error
                .details
                .as_ref()
                .and_then(|value| value.get("field"))
                .and_then(|value| value.as_str()),
            Some("resource")
        );
    }
}

#[test]
fn wait_for_validates_final_wait_before_deferring() {
    let args = WaitForArgs {
        reason: "wait for timer".into(),
        wake: WaitForWakeArg::Timer,
        delivery: WaitForDeliveryArg::Final,
        work_item_id: None,
        resource: None,
        recheck_after_ms: None,
    };

    let error = validate_wait_for_args(&args).unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();

    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert_eq!(
        tool_error
            .details
            .as_ref()
            .and_then(|value| value.get("field"))
            .and_then(|value| value.as_str()),
        Some("resource")
    );
}

#[test]
fn wait_for_allows_operator_and_external_without_resource() {
    validate_resource_for_wake(WaitForWakeArg::OperatorInput, None).unwrap();
    validate_resource_for_wake(WaitForWakeArg::External, None).unwrap();
    validate_resource_for_wake(WaitForWakeArg::System, None).unwrap();
}

#[test]
fn wait_for_treats_empty_resource_as_absent() {
    assert_eq!(optional_resource(Some("  ".into())), None);
    assert_eq!(
        optional_resource(Some("  github:repo#1  ".into())),
        Some("github:repo#1".into())
    );
}

#[test]
fn wait_owner_prefers_execution_binding_over_explicit_request() {
    assert_eq!(
        resolve_wait_work_item_id(
            None,
            Some("execution-work".into()),
            Some("turn-work".into()),
            Some("current-work".into()),
        ),
        (
            Some("execution-work".into()),
            WaitForOwnerSelection::ExecutionBinding,
        )
    );
    // A conflicting explicit request is ignored, not fatal (#3124):
    // the execution binding keeps deciding who waits.
    assert_eq!(
        resolve_wait_work_item_id(
            Some("explicit-work".into()),
            Some("execution-work".into()),
            Some("turn-work".into()),
            Some("current-work".into()),
        ),
        (
            Some("execution-work".into()),
            WaitForOwnerSelection::ExecutionBinding,
        )
    );
}

#[test]
fn wait_owner_falls_back_to_explicit_turn_current_and_lifecycle() {
    assert_eq!(
        resolve_wait_work_item_id(
            Some("explicit-work".into()),
            Some("execution-work".into()),
            Some("turn-work".into()),
            Some("current-work".into()),
        ),
        (
            Some("execution-work".into()),
            WaitForOwnerSelection::ExecutionBinding,
        )
    );
    assert_eq!(
        resolve_wait_work_item_id(
            None,
            None,
            Some("turn-work".into()),
            Some("current-work".into()),
        ),
        (Some("turn-work".into()), WaitForOwnerSelection::TurnBound)
    );
    assert_eq!(
        resolve_wait_work_item_id(None, None, None, Some("current-work".into())),
        (
            Some("current-work".into()),
            WaitForOwnerSelection::CurrentFocus,
        )
    );
    assert_eq!(
        resolve_wait_work_item_id(None, None, None, None),
        (None, WaitForOwnerSelection::AgentLifecycle)
    );
}

#[test]
fn wait_for_parses_recheck_after_ms() {
    let args = parse_wait_for_args(&json!({
        "reason": "wait",
        "wake": "external",
        "delivery": "silent",
        "recheck_after_ms": 300000,
    }))
    .unwrap();

    assert_eq!(args.recheck_after_ms, Some(300000));
}

#[test]
fn wait_for_parses_integral_decimal_string_recheck_after_ms() {
    let args = parse_wait_for_args(&json!({
        "reason": "wait",
        "wake": "external",
        "delivery": "silent",
        "recheck_after_ms": "900000.0",
    }))
    .unwrap();

    assert_eq!(args.recheck_after_ms, Some(900000));
}
