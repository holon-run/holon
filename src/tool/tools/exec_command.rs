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
    types::{
        CommandTaskSpec, ExecCommandOutcome, ExecCommandResult, ToolCapabilityFamily, TrustLevel,
    },
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "ExecCommand";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecCommandArgs {
    pub(crate) cmd: String,
    pub(crate) workdir: Option<String>,
    pub(crate) shell: Option<String>,
    pub(crate) login: Option<bool>,
    pub(crate) tty: Option<bool>,
    pub(crate) accepts_input: Option<bool>,
    pub(crate) continue_on_result: Option<bool>,
    pub(crate) yield_time_ms: Option<u64>,
    pub(crate) max_output_tokens: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<ExecCommandArgs>(
            NAME,
            "Start a shell command inside the workspace. Valid startup input uses `cmd` plus optional `workdir`, `shell`, `login`, `tty`, `accepts_input`, `continue_on_result`, `yield_time_ms`, and `max_output_tokens`; do not pass result or task metadata such as `status` or `task_handle`. Short commands return immediately; long non-interactive commands become command_task automatically.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    trust: &TrustLevel,
    input: &Value,
) -> Result<ToolResult> {
    let args: ExecCommandArgs = parse_tool_args(NAME, input)?;
    let tty = args.tty.unwrap_or(false);
    let spec = CommandTaskSpec {
        cmd: args.cmd,
        workdir: args.workdir,
        shell: args.shell,
        login: args.login.unwrap_or(true),
        tty,
        yield_time_ms: args.yield_time_ms.unwrap_or(10_000),
        max_output_tokens: args.max_output_tokens,
        accepts_input: args.accepts_input.unwrap_or(tty),
        continue_on_result: args.continue_on_result.unwrap_or(false),
    };
    let result: ExecCommandResult = runtime.execute_exec_command(spec, trust).await?;
    serialize_success(NAME, &result)
}

pub(crate) fn render_for_model(result: &ToolResult) -> Result<String> {
    if matches!(result.envelope.status, ToolResultStatus::Error) {
        let error = result
            .tool_error()
            .ok_or_else(|| anyhow!("ExecCommand error result missing tool error"))?;
        return Ok(format!("Command failed\n{}\n", error.render()));
    }

    let value = result
        .envelope
        .result
        .clone()
        .ok_or_else(|| anyhow!("ExecCommand result missing payload"))?;
    let result: ExecCommandResult = serde_json::from_value(value)?;
    match result.outcome {
        ExecCommandOutcome::Completed {
            exit_status,
            stdout_preview,
            stderr_preview,
            artifacts,
            ..
        } => {
            let mut lines = vec![match exit_status {
                Some(code) => format!("Process exited with code {code}"),
                None => "Process exited".to_string(),
            }];
            if let Some(stdout) = stdout_preview.filter(|value| !value.trim().is_empty()) {
                lines.push(String::new());
                lines.push("stdout:".to_string());
                lines.push(stdout);
            }
            if let Some(stderr) = stderr_preview.filter(|value| !value.trim().is_empty()) {
                lines.push(String::new());
                lines.push("stderr:".to_string());
                lines.push(stderr);
            }
            if !artifacts.is_empty() {
                lines.push(String::new());
                lines.push("Artifacts:".to_string());
                lines.extend(artifacts.into_iter().map(|artifact| artifact.path));
            }
            Ok(lines.join("\n"))
        }
        ExecCommandOutcome::PromotedToTask {
            task_handle,
            initial_output_preview,
            ..
        } => {
            let mut lines = vec![
                "Command promoted to background task".to_string(),
                format!("Task: {}", task_handle.task_id),
            ];
            lines.push(String::new());
            lines.push("Initial output:".to_string());
            lines.push(
                initial_output_preview
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "(none captured before promotion)".to_string()),
            );
            Ok(lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::tools::serialize_success;

    #[test]
    fn exec_command_completed_renders_text_receipt() {
        let result = serialize_success(
            NAME,
            &ExecCommandResult {
                outcome: ExecCommandOutcome::Completed {
                    exit_status: Some(0),
                    stdout_preview: Some("line one\nline two".into()),
                    stderr_preview: None,
                    truncated: false,
                    artifacts: Vec::new(),
                    stdout_artifact: None,
                    stderr_artifact: None,
                },
                summary_text: Some("command exited with status 0".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Process exited with code 0"));
        assert!(rendered.contains("stdout:"));
        assert!(rendered.contains("line one"));
    }

    #[test]
    fn exec_command_promoted_renders_task_receipt() {
        let result = serialize_success(
            NAME,
            &ExecCommandResult {
                outcome: ExecCommandOutcome::PromotedToTask {
                    task_handle: crate::types::TaskHandle {
                        task_id: "task_123".into(),
                        task_kind: "command_task".into(),
                        status: crate::types::TaskStatus::Running,
                        initial_output: None,
                    },
                    initial_output_preview: Some("booting".into()),
                    initial_output_truncated: false,
                },
                summary_text: Some("command promoted to a managed task".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Command promoted to background task"));
        assert!(rendered.contains("Task: task_123"));
        assert!(rendered.contains("Initial output:"));
    }

    #[test]
    fn exec_command_promoted_renders_empty_initial_output_placeholder() {
        let result = serialize_success(
            NAME,
            &ExecCommandResult {
                outcome: ExecCommandOutcome::PromotedToTask {
                    task_handle: crate::types::TaskHandle {
                        task_id: "task_123".into(),
                        task_kind: "command_task".into(),
                        status: crate::types::TaskStatus::Running,
                        initial_output: None,
                    },
                    initial_output_preview: None,
                    initial_output_truncated: false,
                },
                summary_text: Some("command promoted to a managed task".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Initial output:"));
        assert!(rendered.contains("(none captured before promotion)"));
    }
}
