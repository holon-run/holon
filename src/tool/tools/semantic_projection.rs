//! Budgeted receipts shared by immediate delivery and history compaction.
use serde_json::{json, Map, Value};

use crate::tool::spec::ToolResultEnvelope;

use super::{estimated_tokens, truncate_chars};

fn fields(value: &Value, keys: &[&str]) -> Value {
    let mut result = Map::new();
    for key in keys {
        if let Some(value) = value.get(*key) {
            result.insert((*key).into(), value.clone());
        }
    }
    Value::Object(result)
}

fn work_item(value: &Value, objective_chars: usize) -> Value {
    let mut result = fields(
        value,
        &[
            "id",
            "state",
            "scheduling_state",
            "is_current",
            "is_runnable",
            "reason_code",
        ],
    );
    if objective_chars > 0 {
        if let Some(objective) = value.get("objective").and_then(Value::as_str) {
            result["objective"] = json!(truncate_chars(objective, objective_chars));
        }
        if let Some(blocker) = value.get("blocked_by").and_then(Value::as_str) {
            result["blocked_by"] = json!(truncate_chars(blocker, objective_chars));
        }
    }
    result
}

fn copy_warnings(value: &Value, result: &mut Value, chars: usize) {
    if let Some(warnings) = value["warnings"].as_array() {
        result["warnings"] = json!(warnings
            .iter()
            .take(8)
            .map(|warning| {
                if let Some(text) = warning.as_str() {
                    json!(truncate_chars(text, chars.clamp(32, 256)))
                } else {
                    let mut item = fields(warning, &["code"]);
                    if let Some(message) = warning["message"].as_str() {
                        item["message"] = json!(truncate_chars(message, chars.clamp(32, 256)));
                    }
                    item
                }
            })
            .collect::<Vec<_>>());
        result["warnings_omitted_count"] = json!(warnings.len().saturating_sub(8));
    }
}

fn transition(value: &Value, chars: usize) -> Value {
    let mut result = fields(
        value,
        &[
            "previous_work_item_id",
            "current_work_item_id",
            "frame_id",
            "suspended_work_item_id",
            "active_work_item_id",
            "return_policy",
            "previous_readiness",
            "current_readiness",
            "switch_kind",
            "terminal_transition",
            "current_focus_mode",
            "blocker_cleared",
        ],
    );
    if let Some(reason) = value["reason"].as_str() {
        result["reason"] = json!(truncate_chars(reason, chars.min(128)));
    }
    copy_warnings(value, &mut result, chars);
    result
}

fn text_preview(value: &str, chars: usize) -> Value {
    let content = value.chars().take(chars).collect::<String>();
    json!({
        "content": content,
        "shown_chars": content.chars().count(),
        "range_unit": "unicode_scalar",
        "start": 0,
        "end_exclusive": content.chars().count(),
        "projection_truncated": content.len() < value.len(),
    })
}

fn command(value: &Value, chars: usize) -> Value {
    let mut result = fields(
        value,
        &[
            "outcome",
            "exit_status",
            "truncated",
            "artifacts",
            "stdout_artifact",
            "stderr_artifact",
            "stdout_ref",
            "stderr_ref",
            "task_handle",
            "initial_output_truncated",
        ],
    );
    for key in ["stdout_preview", "stderr_preview", "initial_output_preview"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            result[key] = text_preview(text, chars);
        }
    }
    result
}

fn generic_facts(value: &Value, depth: usize, nodes: &mut usize) -> Option<Value> {
    if depth > 3 || *nodes == 0 {
        return None;
    }
    *nodes -= 1;
    match value {
        Value::Object(map) => {
            let mut facts = Map::new();
            for (key, value) in map.iter().take(32) {
                if *nodes == 0 {
                    break;
                }
                if !value.is_object()
                    && !value.is_array()
                    && (matches!(
                        key.as_str(),
                        "id" | "state" | "status" | "count" | "total" | "returned"
                    ) || key.ends_with("_id")
                        || key.ends_with("_ref")
                        || key.ends_with("_count"))
                {
                    *nodes -= 1;
                    let value = if matches!(key.as_str(), "state" | "status") {
                        value
                            .as_str()
                            .map(|text| json!(truncate_chars(text, 96)))
                            .unwrap_or_else(|| value.clone())
                    } else {
                        value.clone()
                    };
                    facts.insert(key.clone(), value);
                } else if let Some(nested) = generic_facts(value, depth + 1, nodes) {
                    facts.insert(key.clone(), nested);
                }
            }
            (!facts.is_empty()).then_some(Value::Object(facts))
        }
        Value::Array(items) => {
            let items = items
                .iter()
                .take(4)
                .filter_map(|item| generic_facts(item, depth + 1, nodes))
                .collect::<Vec<_>>();
            (!items.is_empty()).then_some(Value::Array(items))
        }
        _ => None,
    }
}

fn semantic_result(name: &str, value: &Value, chars: usize, rows: usize) -> Value {
    match name {
        "ListWorkItems" => {
            let mut result = fields(
                value,
                &["filter", "limit", "returned", "total_matching", "context"],
            );
            let items = value.get("work_items").and_then(Value::as_array);
            let shown = items.map_or(0, |items| items.len().min(rows));
            result["work_items"] = json!(items
                .into_iter()
                .flatten()
                .take(shown)
                .map(|item| {
                    let mut row = work_item(item, chars.min(96));
                    if chars > 1024 {
                        if let Some(todo) = item.get("todo_list") {
                            row["todo_list"] = todo.clone();
                        }
                    }
                    row
                })
                .collect::<Vec<_>>());
            result["shown"] = json!(shown);
            result["omitted_count"] = json!(items.map_or(0, Vec::len) - shown);
            let mut omitted = vec!["plan_artifact", "completion_report", "work_refs"];
            if chars <= 1024 {
                omitted.push("todo_list");
            }
            result["details_omitted"] = json!(omitted);
            result
        }
        "PickWorkItem" | "CompleteWorkItem" => {
            let mut result = fields(
                value,
                &[
                    "current_work_item_id",
                    "transition",
                    "terminal_transition",
                    "binding_note",
                    "continuation_created",
                    "continuation_resolved",
                    "continuation_resumed",
                    "completed_transition",
                    "completion_mode",
                    "completion_phase",
                    "work_item_id",
                    "request_id",
                    "status",
                    "completion_report_promoted",
                    "warnings",
                ],
            );
            for key in [
                "transition",
                "continuation_created",
                "continuation_resolved",
                "continuation_resumed",
            ] {
                if let Some(value) = value.get(key).filter(|value| value.is_object()) {
                    result[key] = transition(value, chars);
                }
            }
            for key in ["previous_work_item", "current_work_item", "work_item"] {
                if let Some(item) = value.get(key) {
                    result[key] = work_item(item, chars.min(96));
                }
            }
            copy_warnings(value, &mut result, chars);
            result
        }
        "MemoryGet" => {
            let memory = &value["memory"];
            let mut result = fields(
                memory,
                &["source_ref", "kind", "truncated", "source_artifact"],
            );
            if chars >= 256 {
                if let Some(title) = memory["title"].as_str() {
                    result["title"] = json!(truncate_chars(title, 96));
                }
            }
            if let Some(content) = memory["content"].as_str() {
                result["preview"] = text_preview(content, chars);
            }
            json!({"memory": result})
        }
        "ExecCommand" => command(value, chars),
        "ExecCommandBatch" => {
            let mut result = fields(
                value,
                &[
                    "item_count",
                    "completed_count",
                    "failed_count",
                    "rejected_count",
                    "skipped_count",
                ],
            );
            let items = value["items"].as_array();
            result["items"] = json!(items
                .into_iter()
                .flatten()
                .take(rows)
                .map(|item| {
                    let mut result =
                        fields(item, &["index", "status", "error_kind", "error_message"]);
                    if let Some(output) = item.get("result") {
                        result["result"] = command(output, chars);
                    }
                    result
                })
                .collect::<Vec<_>>());
            result["omitted_count"] = json!(items.map_or(0, Vec::len).saturating_sub(rows));
            result
        }
        _ => {
            let mut result = match generic_facts(value, 0, &mut 64) {
                Some(Value::Object(map)) => Value::Object(map),
                Some(facts) => json!({"facts": facts}),
                None => json!({}),
            };
            if let Some(item) = value.get("work_item") {
                result["work_item"] = work_item(item, chars.min(96));
            }
            result["body_omitted"] = json!(true);
            result["result_type"] = json!(match value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            });
            result["result_fields"] = json!(value
                .as_object()
                .map(|map| map.keys().take(32).collect::<Vec<_>>()));
            result
        }
    }
}

/// None means that even a useful receipt cannot fit; never emit an empty success.
pub(crate) fn project(
    envelope: &ToolResultEnvelope,
    output_ref: Option<&str>,
    budget: usize,
) -> Option<String> {
    let value = envelope.result.as_ref().unwrap_or(&Value::Null);
    let mut receipt = json!({
        "tool_name": envelope.tool_name,
        "status": envelope.status,
        "provider_projection_truncated": true,
    });
    if let Some(reference) = output_ref.or_else(|| value["output_ref"].as_str()) {
        receipt["output_ref"] = json!(reference);
    }
    if let Some(artifact) = value.get("recovery_artifact") {
        receipt["recovery_artifact"] = artifact.clone();
    }
    if let Some(summary) = &envelope.summary_text {
        receipt["summary_text"] = json!(truncate_chars(summary, 256));
    }
    if let Some(error) = &envelope.error {
        receipt["error"] = json!({
            "kind": error.kind,
            "message": truncate_chars(&error.message, 256),
            "recovery_hint": error.recovery_hint.as_deref().map(|text| truncate_chars(text, 256)),
            "retryable": error.retryable,
        });
    }
    // Remove optional detail and shorten text before ever dropping identities.
    for rows in [usize::MAX, 64, 32, 16, 8, 4, 1, 0] {
        if rows == 0 && envelope.tool_name == "ExecCommandBatch" {
            continue;
        }
        for chars in [budget.saturating_mul(4), 1024, 256, 96, 32, 0] {
            if let Some(summary) = &envelope.summary_text {
                receipt["summary_text"] = json!(truncate_chars(summary, chars.clamp(32, 256)));
            }
            if chars == 0
                && matches!(
                    envelope.tool_name.as_str(),
                    "MemoryGet" | "ExecCommand" | "ExecCommandBatch"
                )
            {
                continue;
            }
            receipt["result"] = semantic_result(&envelope.tool_name, value, chars, rows);
            let rendered = serde_json::to_string(&receipt).ok()?;
            if estimated_tokens(&rendered) <= budget {
                return Some(rendered);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{tools::ToolModelRenderContext, ToolResult};

    fn envelope(name: &str, result: Value) -> ToolResultEnvelope {
        ToolResult::success(name, result, None).envelope
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
}
