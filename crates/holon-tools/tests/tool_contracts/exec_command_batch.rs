use super::serialize_success;
use holon_domain::{
    ExecCommandBatchItemResult, ExecCommandBatchItemStatus, ExecCommandBatchResult,
};
use holon_tools::builtins::exec_command_batch::*;
use holon_tools::ToolError;

#[test]
fn rejects_top_level_cmd_with_exec_command_hint() {
    let input = serde_json::json!({
        "cmd": "git status",
        "max_output_tokens": 1000
    });
    let error = parse_batch_args(&input).unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();

    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert!(tool_error
        .details
        .as_ref()
        .and_then(|value| value.get("parse_error"))
        .and_then(|value| value.as_str())
        .is_some_and(|message| message.contains("cmd")));
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .is_some_and(|hint| { hint.contains("ExecCommand") && hint.contains("one command") }));
}

#[test]
fn oversized_items_array_is_rejected_before_per_item_parsing() {
    // Thirteen items whose per-item payloads would fail typed parsing:
    // the raw array length check must reject first, without scanning
    // any item.
    let input = serde_json::json!({
        "items": (0..13).map(|_| serde_json::json!({"cmd": {"nested": "invalid"}})).collect::<Vec<_>>()
    });
    let error = parse_batch_args(&input).unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let details = tool_error.details.as_ref().expect("bounded details");
    assert_eq!(
        details.get("validation_error").and_then(|v| v.as_str()),
        Some("too_many_items")
    );
    assert_eq!(
        details.get("actual_items").and_then(|v| v.as_u64()),
        Some(13)
    );
}

#[test]
fn flood_of_empty_batch_objects_is_rejected_before_per_item_parsing() {
    let input = serde_json::json!({
        "items": (0..18_471).map(|_| serde_json::json!({})).collect::<Vec<_>>()
    });
    let error = parse_batch_args(&input).unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let details = tool_error.details.as_ref().expect("bounded details");
    assert_eq!(
        details.get("validation_error").and_then(|v| v.as_str()),
        Some("too_many_items")
    );
    assert_eq!(
        details.get("actual_items").and_then(|v| v.as_u64()),
        Some(18_471)
    );
}

#[test]
fn twelve_items_parse_normally() {
    let input = serde_json::json!({
        "items": (0..12).map(|i| serde_json::json!({"cmd": format!("echo {i}")})).collect::<Vec<_>>()
    });
    let args = parse_batch_args(&input).expect("twelve items are in contract");
    assert_eq!(args.items.len(), 12);
}

#[test]
fn non_array_items_keeps_typed_parse_failure() {
    let input = serde_json::json!({"items": null});
    let error = parse_batch_args(&input).unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let details = tool_error.details.as_ref().expect("typed parse details");
    assert!(
        details.get("validation_error").is_none(),
        "missing/non-array items keep the existing parse failure shape"
    );
}

#[test]
fn top_level_defaults_apply_to_items_without_overriding_explicit_values() {
    let defaults = ExecCommandBatchDefaults {
        workdir: Some("src".into()),
        shell: Some("/bin/sh".into()),
        login: Some(false),
        yield_time_ms: Some(60_000),
        max_output_tokens: Some(2_000),
    };
    let item = apply_batch_defaults(
        ExecCommandBatchItemArgs {
            cmd: "pwd".into(),
            workdir: None,
            shell: None,
            login: None,
            yield_time_ms: None,
            max_output_tokens: None,
        },
        &defaults,
    );

    assert_eq!(item.workdir.as_deref(), Some("src"));
    assert_eq!(item.shell.as_deref(), Some("/bin/sh"));
    assert_eq!(item.login, Some(false));
    assert_eq!(item.yield_time_ms, Some(60_000));
    assert_eq!(item.max_output_tokens, Some(2_000));

    let item = apply_batch_defaults(
        ExecCommandBatchItemArgs {
            cmd: "pwd".into(),
            workdir: Some("tests".into()),
            shell: Some("/bin/bash".into()),
            login: Some(true),
            yield_time_ms: Some(1_000),
            max_output_tokens: Some(100),
        },
        &defaults,
    );

    assert_eq!(item.workdir.as_deref(), Some("tests"));
    assert_eq!(item.shell.as_deref(), Some("/bin/bash"));
    assert_eq!(item.login, Some(true));
    assert_eq!(item.yield_time_ms, Some(1_000));
    assert_eq!(item.max_output_tokens, Some(100));
}

#[test]
fn parses_supported_top_level_defaults() {
    let input = serde_json::json!({
        "items": [{
            "cmd": "git status"
        }],
        "workdir": ".",
        "login": false,
        "yield_time_ms": 60_000,
        "max_output_tokens": 2_000
    });
    let args = parse_batch_args(&input).expect("top-level defaults should parse");

    assert_eq!(args.workdir.as_deref(), Some("."));
    assert_eq!(args.login, Some(false));
    assert_eq!(args.yield_time_ms, Some(60_000));
    assert_eq!(args.max_output_tokens, Some(2_000));
}

#[test]
fn rejects_interactive_item_fields_with_exec_command_hint() {
    let input = serde_json::json!({
        "items": [{
            "cmd": "python -i",
            "tty": true
        }]
    });
    let error = parse_batch_args(&input).unwrap_err();
    let tool_error = error.downcast_ref::<ToolError>().unwrap();

    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert!(tool_error
        .details
        .as_ref()
        .and_then(|value| value.get("parse_error"))
        .and_then(|value| value.as_str())
        .is_some_and(|message| message.contains("tty")));
    assert!(tool_error.recovery_hint.as_deref().is_some_and(|hint| {
        hint.contains("tty")
            && hint.contains("accepts_input")
            && hint.contains("ExecCommand")
            && hint.contains("interactive")
    }));
}

#[test]
fn render_for_model_uses_command_preview_for_batch_items() {
    let cmd = format!(
        "API_TOKEN=secret_value {}",
        "printf safe_preview ".repeat(40)
    );
    let result = serialize_success(
        NAME,
        &ExecCommandBatchResult {
            item_count: 1,
            completed_count: 0,
            failed_count: 0,
            rejected_count: 0,
            skipped_count: 1,
            stop_on_error: false,
            items: vec![ExecCommandBatchItemResult {
                index: 1,
                cmd,
                status: ExecCommandBatchItemStatus::Skipped,
                result: None,
                error_kind: None,
                error_message: None,
                duration_ms: None,
            }],
            summary_text: Some("batch".into()),
        },
    )
    .unwrap();

    let rendered = render_for_model(&result).unwrap();
    assert!(rendered.contains("API_TOKEN=[redacted]"));
    assert!(rendered.contains("..."));
    assert!(!rendered.contains("secret_value"));
}
