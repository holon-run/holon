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
        AuthorityClass, CommandTaskSpec, ExecCommandDuplicatePolicy, ExecCommandOutcome,
        ExecCommandResult, ToolCapabilityFamily,
    },
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "ExecCommand";

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecCommandArgs {
    pub(crate) cmd: String,
    pub(crate) workdir: Option<String>,
    pub(crate) shell: Option<String>,
    pub(crate) login: Option<bool>,
    pub(crate) tty: Option<bool>,
    #[serde(default)]
    pub(crate) duplicate_policy: ExecCommandDuplicatePolicy,
    pub(crate) accepts_input: Option<bool>,
    pub(crate) yield_time_ms: Option<u64>,
    pub(crate) max_output_tokens: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<ExecCommandArgs>(
            NAME,
            include_str!("../tool_descriptions/exec_command.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<ToolResult> {
    let args: ExecCommandArgs = parse_tool_args(NAME, input)?;
    let tty = args.tty.unwrap_or(false);
    let duplicate_policy = args.duplicate_policy;
    let spec = CommandTaskSpec {
        cmd: args.cmd,
        workdir: args.workdir,
        shell: args.shell,
        login: args.login.unwrap_or(true),
        tty,
        yield_time_ms: args.yield_time_ms.unwrap_or(10_000),
        max_output_tokens: args.max_output_tokens,
        accepts_input: args.accepts_input.unwrap_or(tty),
        terminal_reentry: false,
    };
    let result: ExecCommandResult = runtime
        .managed_tasks()
        .execute_exec_command(spec, duplicate_policy, authority_class)
        .await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::tools::serialize_success;
    use crate::tool::ToolError;
    use serde_json::json;

    #[test]
    fn exec_command_args_reject_continue_on_result() {
        let err = match serde_json::from_value::<ExecCommandArgs>(serde_json::json!({
            "cmd": "echo ok",
            "continue_on_result": true
        })) {
            Ok(_) => panic!("continue_on_result is no longer an ExecCommand startup field"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("continue_on_result"));
    }

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
                command_diagnostics: None,
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
    fn exec_command_rejects_command_field_instead_of_cmd() {
        let error = parse_tool_args::<ExecCommandArgs>(
            NAME,
            &json!({
                "command": "git status",
            }),
        )
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert!(tool_error.recovery_hint.as_deref().is_some_and(|hint| hint
            .contains("provide input for ExecCommand that matches the published tool schema")));
        assert!(tool_error
            .details
            .as_ref()
            .and_then(|value| value.get("parse_error"))
            .and_then(|value| value.as_str())
            .is_some_and(|message| {
                message.contains("command")
                    || message.contains("cmd")
                    || message.contains("missing field")
            }));
    }

    #[test]
    fn exec_command_rejects_task_metadata_fields() {
        let error = parse_tool_args::<ExecCommandArgs>(
            NAME,
            &json!({
                "cmd": "git status",
                "status": "running",
            }),
        )
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "invalid_tool_input");
        assert!(tool_error
            .details
            .as_ref()
            .and_then(|value| value.get("parse_error"))
            .and_then(|value| value.as_str())
            .is_some_and(|message| message.contains("status")));
        assert!(tool_error.recovery_hint.as_deref().is_some_and(|hint| hint
            .contains("provide input for ExecCommand that matches the published tool schema")));
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
                command_diagnostics: None,
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
                command_diagnostics: None,
                summary_text: Some("command promoted to a managed task".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Initial output:"));
        assert!(rendered.contains("(none captured before promotion)"));
    }

    #[test]
    fn exec_command_default_duplicate_policy_is_reuse_running() {
        let args = serde_json::from_value::<ExecCommandArgs>(serde_json::json!({
            "cmd": "printf ok",
        }))
        .expect("default duplicate policy should parse");
        assert_eq!(
            args.duplicate_policy,
            ExecCommandDuplicatePolicy::ReuseRunning
        );
    }

    #[test]
    fn exec_command_duplicate_policy_start_new_parses() {
        let args = serde_json::from_value::<ExecCommandArgs>(serde_json::json!({
            "cmd": "printf ok",
            "duplicate_policy": "start_new",
        }))
        .expect("start_new duplicate policy should parse");
        assert_eq!(args.duplicate_policy, ExecCommandDuplicatePolicy::StartNew);
    }
}
