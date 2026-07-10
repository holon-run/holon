//! Canonical human-readable summaries of tool execution results.

use serde_json::Value;

use super::{
    helpers::truncate_text,
    names as tn,
    spec::{ToolResultEnvelope, ToolResultStatus},
};

const TOOL_RESULT_SUMMARY_LIMIT: usize = 200;

pub(crate) fn tool_result_summary(envelope: &ToolResultEnvelope) -> String {
    let summary = match envelope.status {
        ToolResultStatus::Error => envelope
            .summary_text
            .clone()
            .or_else(|| envelope.error.as_ref().map(|error| error.message.clone()))
            .unwrap_or_else(|| "tool failed".into()),
        ToolResultStatus::Success => envelope
            .result
            .as_ref()
            .and_then(|result| match envelope.tool_name.as_str() {
                tn::EXEC_COMMAND => Some(summarize_exec_command_result(result)),
                tn::TASK_OUTPUT => Some(summarize_task_output_result(result)),
                tn::SPAWN_AGENT => summarize_spawn_agent_result(result),
                tn::VIEW_IMAGE => summarize_view_image_result(result),
                _ => None,
            })
            .or_else(|| envelope.summary_text.clone())
            .or_else(|| summary_from_result_payload(envelope.result.as_ref()))
            .unwrap_or_else(|| "completed".into()),
    };
    truncate_text(&summary, TOOL_RESULT_SUMMARY_LIMIT)
}

fn artifact_paths(value: &Value) -> Vec<String> {
    value
        .get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|artifact| artifact.get("path").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn summarize_exec_command_result(result: &Value) -> String {
    let disposition = result
        .get("disposition")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match disposition {
        "completed" => {
            let exit_status = result
                .get("exit_status")
                .and_then(Value::as_i64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into());
            let truncated = result
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let artifacts = artifact_paths(result);
            let artifact_note = if artifacts.is_empty() {
                String::new()
            } else {
                format!(" artifacts={}", artifacts.join(", "))
            };
            format!("completed exit_status={exit_status} truncated={truncated}{artifact_note}")
        }
        "promoted_to_task" => {
            let task_id = result
                .get("task_handle")
                .and_then(|handle| handle.get("task_id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let truncated = result
                .get("initial_output_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            format!("promoted_to_task task_id={task_id} initial_output_truncated={truncated}")
        }
        "already_running" => {
            let task_id = result
                .get("task_handle")
                .and_then(|handle| handle.get("task_id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let status = result
                .get("task_handle")
                .and_then(|handle| handle.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("already_running task_id={task_id} status={status}")
        }
        _ => format!("disposition={disposition}"),
    }
}

fn summarize_task_output_result(result: &Value) -> String {
    let retrieval_status = result
        .get("retrieval_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let task = result.get("task").unwrap_or(result);
    let task_id = task.get("id").and_then(Value::as_str).unwrap_or("unknown");
    let truncated = task
        .get("output_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_status = task
        .get("exit_status")
        .and_then(Value::as_i64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into());
    let artifacts = artifact_paths(task);
    let artifact_note = if artifacts.is_empty() {
        String::new()
    } else {
        format!(" artifacts={}", artifacts.join(", "))
    };
    format!(
        "retrieval_status={retrieval_status} task_id={task_id} output_truncated={truncated} exit_status={exit_status}{artifact_note}"
    )
}

fn summarize_spawn_agent_result(result: &Value) -> Option<String> {
    let agent_id = result.get("agent_id").and_then(Value::as_str)?;
    let task_id = result
        .get("task_handle")
        .and_then(|handle| handle.get("task_id"))
        .and_then(Value::as_str);
    Some(match task_id {
        Some(task_id) => format!("agent_id={agent_id} task_id={task_id}"),
        None => format!("agent_id={agent_id}"),
    })
}

fn summarize_view_image_result(result: &Value) -> Option<String> {
    let reference = result.get("visual_reference")?;
    let observation = result.get("observation")?;
    let reference_id = reference
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mime = reference
        .get("mime")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let schema = observation
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let prompt = observation
        .get("prompt")
        .and_then(Value::as_str)
        .map(|prompt| truncate_text(prompt, 160));
    let summary = observation
        .get("summary")
        .and_then(Value::as_str)
        .map(|summary| truncate_text(summary, 160));
    let ocr = observation
        .get("ocr")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("text").and_then(Value::as_str))
                .take(3)
                .map(|text| truncate_text(text, 80))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut parts = vec![format!(
        "visual_observation schema={schema} ref={reference_id} mime={mime}"
    )];
    if let Some(prompt) = prompt {
        parts.push(format!("prompt=\"{prompt}\""));
    }
    if let Some(summary) = summary {
        parts.push(format!("summary=\"{summary}\""));
    }
    if !ocr.is_empty() {
        parts.push(format!("ocr=[{}]", ocr.join(" | ")));
    }
    Some(parts.join(" "))
}

fn summary_from_result_payload(value: Option<&Value>) -> Option<String> {
    let object = value?.as_object()?;
    for field in [
        "disposition",
        "retrieval_status",
        "status",
        "task_id",
        "agent_id",
        "id",
    ] {
        if let Some(summary) = object.get(field).and_then(Value::as_str) {
            return Some(summary.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::tool_result_summary;
    use crate::tool::{ToolError, ToolResult};

    #[test]
    fn result_summary_falls_back_to_completed_instead_of_the_tool_name() {
        let result = ToolResult::success(
            "AgentGet",
            json!({
                "profile": {"name": "default"},
                "active_tasks": [{"id": "task-1"}]
            }),
            None,
        );
        assert_eq!(tool_result_summary(&result.envelope), "completed");

        let error = ToolResult::error("ExecCommand", ToolError::new("failure", "command exploded"));
        assert_eq!(tool_result_summary(&error.envelope), "command exploded");
    }

    #[test]
    fn result_summary_uses_tool_specific_result_fields() {
        let result = ToolResult::success(
            "ExecCommand",
            json!({
                "disposition": "completed",
                "exit_status": 2,
                "truncated": false
            }),
            None,
        );
        assert_eq!(
            tool_result_summary(&result.envelope),
            "completed exit_status=2 truncated=false"
        );
    }
}
