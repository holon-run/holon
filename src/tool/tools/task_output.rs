use anyhow::{anyhow, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::{
        spec::{typed_spec, ToolResultStatus},
        ToolResult,
    },
    types::{TaskOutputResult, TaskOutputRetrievalStatus, ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "TaskOutput";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskOutputArgs {
    pub(crate) task_id: String,
    pub(crate) block: Option<bool>,
    pub(crate) timeout_ms: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<TaskOutputArgs>(
            NAME,
            "Read background task output and optionally wait for task completion.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<ToolResult> {
    let args: TaskOutputArgs = parse_tool_args(NAME, input)?;
    let task_id = validate_non_empty(args.task_id, NAME, "task_id")?;
    let block = args.block.unwrap_or(true);
    let timeout_ms = args.timeout_ms.unwrap_or(30_000);
    let result: TaskOutputResult = runtime.task_output(&task_id, block, timeout_ms).await?;
    serialize_success(NAME, &result)
}

pub(crate) fn render_for_model(result: &ToolResult) -> Result<String> {
    if matches!(result.envelope.status, ToolResultStatus::Error) {
        let error = result
            .tool_error()
            .ok_or_else(|| anyhow!("TaskOutput error result missing tool error"))?;
        return Ok(format!("Task output read failed\n{}\n", error.render()));
    }

    let value = result
        .envelope
        .result
        .clone()
        .ok_or_else(|| anyhow!("TaskOutput result missing payload"))?;
    let result: TaskOutputResult = serde_json::from_value(value)?;
    let status_line = match result.retrieval_status {
        TaskOutputRetrievalStatus::Success => "Task output retrieved".to_string(),
        TaskOutputRetrievalStatus::Timeout => "Task output wait timed out".to_string(),
        TaskOutputRetrievalStatus::NotReady => "Task output is not ready".to_string(),
    };
    let mut lines = vec![
        status_line,
        format!("Task: {}", result.task.task_id),
        format!(
            "Status: {}",
            serde_json::to_string(&result.task.status)?.trim_matches('"')
        ),
    ];
    if let Some(exit_status) = result.task.exit_status {
        lines.push(format!("Exit status: {exit_status}"));
    }
    if let Some(summary) = result.task.summary.as_deref() {
        lines.push(format!("Summary: {summary}"));
    }
    if !result.task.output_preview.trim().is_empty() {
        lines.push(String::new());
        lines.push("Output:".to_string());
        lines.push(result.task.output_preview.clone());
    }
    if !result.task.artifacts.is_empty() {
        lines.push(String::new());
        lines.push("Artifacts:".to_string());
        lines.extend(
            result
                .task
                .artifacts
                .iter()
                .map(|artifact| artifact.path.clone()),
        );
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tool::tools::serialize_success,
        types::{TaskOutputSnapshot, TaskStatus},
    };

    #[test]
    fn task_output_renders_text_receipt() {
        let result = serialize_success(
            NAME,
            &TaskOutputResult {
                retrieval_status: TaskOutputRetrievalStatus::Success,
                task: TaskOutputSnapshot {
                    task_id: "task_123".into(),
                    kind: "command_task".into(),
                    status: TaskStatus::Completed,
                    summary: Some("verification finished".into()),
                    output_preview: "ok\nall good".into(),
                    output_truncated: false,
                    artifacts: Vec::new(),
                    output_artifact: None,
                    result_summary: Some("done".into()),
                    exit_status: Some(0),
                    failure_artifact: None,
                },
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Task output retrieved"));
        assert!(rendered.contains("Task: task_123"));
        assert!(rendered.contains("Output:"));
        assert!(rendered.contains("all good"));
    }
}
