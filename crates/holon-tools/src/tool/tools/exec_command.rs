use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::tool::spec::ToolOutcome;
use crate::tool::spec::ToolResultStatus;
use crate::types::ExecCommandDuplicatePolicy;
use crate::types::ExecCommandOutcome;
use crate::types::ExecCommandResult;
use crate::types::ToolCapabilityFamily;
use anyhow::anyhow;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::EXEC_COMMAND;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandArgs {
    pub cmd: String,
    pub workdir: Option<String>,
    pub shell: Option<String>,
    pub login: Option<bool>,
    pub tty: Option<bool>,
    #[serde(default)]
    pub duplicate_policy: ExecCommandDuplicatePolicy,
    pub accepts_input: Option<bool>,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<ExecCommandArgs>(
            NAME,
            include_str!("../tool_descriptions/exec_command.md"),
        )?,
    })
}

pub fn render_for_model(result: &ToolOutcome<impl Default>) -> Result<String> {
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
            truncated,
            ..
        } => {
            let mut lines = vec![match exit_status {
                Some(code) => format!("Process exited with code {code}"),
                None => "Process exited".to_string(),
            }];
            if truncated {
                lines.push("Output truncated; stream previews are contiguous prefixes.".into());
            }
            if let Some(stdout) = stdout_preview.filter(|value| !value.is_empty()) {
                lines.push(String::new());
                lines.push(format!(
                    "stdout: (Unicode scalar range [0, {}))",
                    stdout.chars().count()
                ));
                lines.push(stdout);
            }
            if let Some(stderr) = stderr_preview.filter(|value| !value.is_empty()) {
                lines.push(String::new());
                lines.push(format!(
                    "stderr: (Unicode scalar range [0, {}))",
                    stderr.chars().count()
                ));
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
        ExecCommandOutcome::AlreadyRunning {
            task_handle,
            command,
            summary,
            instructions,
            ..
        } => {
            let mut lines = vec![
                "Command is already running".to_string(),
                format!("Task: {}", task_handle.task_id),
                format!("Status: {:?}", task_handle.status),
            ];
            if let Some(summary) = summary.filter(|value| !value.trim().is_empty()) {
                lines.push(format!("Task summary: {summary}"));
            }
            if let Some(command) = command {
                if let Some(cmd) = command
                    .cmd
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                {
                    lines.push(format!("Command: {cmd}"));
                }
                if let Some(workdir) = command.workdir.as_deref() {
                    lines.push(format!("workdir={workdir}"));
                }
                if let Some(shell) = command.shell.as_deref() {
                    lines.push(format!("shell={shell}"));
                }
            }
            if let Some(instructions) = instructions.filter(|value| !value.trim().is_empty()) {
                lines.push(String::new());
                lines.push(instructions);
            }
            Ok(lines.join("\n"))
        }
    }
}
