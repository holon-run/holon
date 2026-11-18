use super::*;
use crate::tool::{tools::ToolModelRenderContext, ToolResult};

fn envelope(name: &str, result: Value) -> ToolResultEnvelope {
    ToolResult::success(name, result, None).envelope
}

#[test]
fn wait_receipts_keep_continuation_under_budget() {
    for continuation in ["continue_turn", "yield_and_wait", "yield_and_reenter"] {
        let canonical = envelope(
            "WaitFor",
            json!({
                "continuation": continuation,
                "task_id": "task_1",
                "result_message_id": "message_1",
                "work_item": {"objective": "long objective ".repeat(10000)},
            }),
        );
        let rendered = project(&canonical, None, 512).unwrap();
        let value: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["result"]["continuation"], continuation);
        assert_eq!(value["result"]["task_id"], "task_1");
        assert_eq!(value["result"]["result_message_id"], "message_1");
        assert!(estimated_tokens(&rendered) <= 512);
    }
}

#[test]
fn queues_keep_identity_order_and_short_objectives_before_dropping_rows() {
    for count in [0, 1, 15, 22, 100] {
        let items = (0..count).map(|index| json!({
            "id": format!("work_{index:015x}"),
            "objective": format!("Task {index} {}", "long objective ".repeat(500)),
            "state": "open", "scheduling_state": if index % 2 == 0 {"yielded"} else {"runnable"},
            "is_current": index == 0, "is_runnable": index % 2 != 0,
            "plan_artifact": {"preview": "p".repeat(1600)},
            "todo_list": [{"text": "t".repeat(8000), "state": "pending"}],
            "completion_report": "r".repeat(8000),
        })).collect::<Vec<_>>();
        let canonical = envelope(
            "ListWorkItems",
            json!({
                "filter": "open", "limit": 100, "returned": count, "total_matching": count,
                "context": {"current_work_item_id": "work_000000000000000"},
                "work_items": items,
            }),
        );
        let original = serde_json::to_value(&canonical).unwrap();
        let rendered = project(&canonical, Some("tool_execution:test:output"), 2500).unwrap();
        assert!(estimated_tokens(&rendered) <= 2500);
        let projected: Value = serde_json::from_str(&rendered).unwrap();
        let rows = projected["result"]["work_items"].as_array().unwrap();
        if count <= 22 {
            assert_eq!(rows.len(), count);
            for (index, row) in rows.iter().enumerate() {
                assert_eq!(row["id"], items[index]["id"]);
                assert_eq!(row["scheduling_state"], items[index]["scheduling_state"]);
                assert!(row["objective"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("Task {index}")));
            }
        }
        assert_eq!(
            projected["result"]["omitted_count"].as_u64().unwrap() as usize,
            count - rows.len()
        );
        assert_eq!(serde_json::to_value(&canonical).unwrap(), original);
    }
}

#[test]
fn memory_prefix_preserves_unicode_and_source_truncation_separately() {
    let content = "中文🦀\"\\\n".repeat(10_000);
    let canonical = envelope(
        "MemoryGet",
        json!({"memory": {
            "source_ref": "tool_execution:original:output", "kind": "tool_execution",
            "content": content, "truncated": true, "metadata": {"huge": "x".repeat(80_000)},
            "source_artifact": {"path": "/agent/tool-artifacts/source.log", "complete": true}
        }}),
    );
    for budget in [2500, 500] {
        let rendered = project(&canonical, None, budget).unwrap();
        assert!(estimated_tokens(&rendered) <= budget);
        let value: Value = serde_json::from_str(&rendered).unwrap();
        let memory = &value["result"]["memory"];
        let prefix = memory["preview"]["content"].as_str().unwrap();
        assert!(!prefix.is_empty());
        assert!(content.starts_with(prefix));
        assert_eq!(memory["source_ref"], "tool_execution:original:output");
        assert_eq!(memory["truncated"], true);
        assert_eq!(memory["preview"]["projection_truncated"], true);
        assert_eq!(memory["preview"]["shown_chars"], prefix.chars().count());
    }
    assert!(project(&canonical, None, 1).is_none());
}

#[test]
fn transitions_are_taken_from_payload_not_inferred_from_tool_name() {
    for mode in ["bound", "detached"] {
        let canonical = envelope(
            "CompleteWorkItem",
            json!({
                "work_item": {"id": "work_1", "state": "completing", "scheduling_state": "completing", "plan_artifact": "x".repeat(10_000)},
                "completed_transition": false, "completion_mode": mode,
                "completion_report_promoted": false,
                "continuation_resumed": {"frame_id": "frame_1", "active_work_item_id": "work_2"},
                "warnings": ["prepared, not committed"]
            }),
        );
        let rendered = project(&canonical, None, 1000).unwrap();
        let value: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["result"]["work_item"]["id"], "work_1");
        assert_eq!(value["result"]["completion_mode"], mode);
        assert_eq!(value["result"]["completed_transition"], false);
        assert_eq!(value["result"]["completion_report_promoted"], false);
        assert_eq!(
            value["result"]["continuation_resumed"]["frame_id"],
            "frame_1"
        );
    }
}

#[test]
fn command_ranges_report_the_actual_provider_visible_prefix() {
    let text = " \n汉字🦀\"\\ ".repeat(5000);
    for name in ["ExecCommand", "ExecCommandBatch"] {
        let output = json!({"exit_status": 0, "stdout_preview": text, "truncated": false,
            "artifacts": [{"path": "/agent/tool-artifacts/stdout.log"}]});
        let payload = if name == "ExecCommandBatch" {
            json!({"item_count": 1, "items": [{"index": 1, "status": "completed", "result": output}]})
        } else {
            output
        };
        let canonical = envelope(name, payload);
        let rendered = project(&canonical, None, 800).unwrap();
        let value: Value = serde_json::from_str(&rendered).unwrap();
        let result = if name == "ExecCommandBatch" {
            &value["result"]["items"][0]["result"]
        } else {
            &value["result"]
        };
        let prefix = result["stdout_preview"]["content"].as_str().unwrap();
        assert!(!prefix.is_empty());
        assert!(text.starts_with(prefix));
        assert_eq!(
            result["stdout_preview"]["end_exclusive"],
            prefix.chars().count()
        );
        assert!(estimated_tokens(&rendered) <= 800);
    }
}

#[test]
fn errors_keep_recovery_semantics_even_without_summary() {
    let mut canonical = envelope("FailedTool", Value::Null);
    canonical.status = crate::tool::spec::ToolResultStatus::Error;
    canonical.error = Some(
        crate::tool::ToolError::new("access_denied", "denied ".repeat(5000))
            .with_recovery_hint("ask the operator for authorized access"),
    );
    let rendered = project(&canonical, Some("tool_execution:failed:output"), 500).unwrap();
    let projected: Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(projected["error"]["kind"], "access_denied");
    assert_eq!(
        projected["error"]["recovery_hint"],
        "ask the operator for authorized access"
    );
    assert!(projected["error"]["retryable"].is_boolean());
    assert!(estimated_tokens(&rendered) <= 500);
    assert!(project(&canonical, None, 1).is_none());
}

#[test]
fn unknown_shape_is_explicit_and_tiny_immediate_budget_errors() {
    let result = ToolResult::success(
        "Unknown",
        json!({"unrecognized": "x".repeat(100_000)}),
        None,
    );
    let rendered = project(&result.envelope, None, 400).unwrap();
    let value: Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(value["result"]["body_omitted"], true);
    assert_eq!(value["result"]["result_fields"][0], "unrecognized");
    assert!(super::super::render_tool_result_for_model_with_context(
        &result,
        &ToolModelRenderContext {
            tool_execution_id: "test",
            tool_output_budget_estimated_tokens: 1
        }
    )
    .is_err());
}
