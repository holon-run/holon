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
        CommandTaskSpec, ExecCommandDuplicatePolicy, ExecCommandOutcome, ExecCommandResult,
        TaskStatus, ToolCapabilityFamily, TrustLevel,
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
    pub(crate) duplicate_policy: Option<ExecCommandDuplicatePolicy>,
    pub(crate) accepts_input: Option<bool>,
    pub(crate) yield_time_ms: Option<u64>,
    pub(crate) max_output_tokens: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<ExecCommandArgs>(
            NAME,
            "Start a shell command inside the workspace. Valid startup input uses `cmd` plus optional `workdir`, `shell`, `login`, `tty`, `accepts_input`, `yield_time_ms`, `max_output_tokens`, and `duplicate_policy`; do not pass result or task metadata such as `status` or `task_handle`. `yield_time_ms` defaults to 10_000 ms when omitted; set it only when intentionally changing the foreground wait window. `duplicate_policy` controls active task reuse (`reuse_running` default, `start_new` to force a second process). Short commands return immediately; long non-interactive commands become command_task automatically.",
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
    let duplicate_policy = args.duplicate_policy.unwrap_or_default();
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
        .execute_exec_command(spec, &duplicate_policy, trust)
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
        ExecCommandOutcome::AlreadyRunning { task } => {
            let status = match task.status {
                TaskStatus::Queued => "queued",
                TaskStatus::Running => "running",
                TaskStatus::Cancelling => "cancelling",
                TaskStatus::Completed => "completed",
                TaskStatus::Failed => "failed",
                TaskStatus::Cancelled => "cancelled",
                TaskStatus::Interrupted => "interrupted",
            };
            let mut lines = vec![
                "Command is already running".to_string(),
                format!("Existing task: {}", task.task_id),
                format!("Status: {status}"),
            ];
            if let Some(command) = &task.command {
                if let Some(cmd) = &command.cmd {
                    lines.push(format!("Command: {cmd}"));
                }
                if let Some(workdir) = &command.workdir {
                    lines.push(format!("Workdir: {workdir}"));
                }
            }
            lines.push("Inspect with TaskStatus and TaskOutput using this task id.".to_string());
            lines.push(
                "To start a second instance, set duplicate_policy to \"start_new\".".to_string(),
            );
            Ok(lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::tools::serialize_success;
    use crate::tool::ToolError;
    use crate::types::TaskStatusSnapshot;
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
    fn exec_command_renders_already_running_receipt() {
        let result = serialize_success(
            NAME,
            &ExecCommandResult {
                outcome: ExecCommandOutcome::AlreadyRunning {
                    task: TaskStatusSnapshot {
                        task_id: "task_123".into(),
                        kind: "command_task".into(),
                        status: TaskStatus::Running,
                        summary: Some("long command".into()),
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                        wait_policy: crate::types::TaskWaitPolicy::Background,
                        parent_message_id: None,
                        command: Some(crate::types::CommandTaskStatusSnapshot {
                            cmd: Some("printf hi".into()),
                            cmd_digest: Some("digest".into()),
                            workdir: Some("/workspace".into()),
                            shell: Some("bash".into()),
                            login: Some(true),
                            tty: Some(false),
                            output_path: None,
                            result_summary: None,
                            exit_status: None,
                            terminal_reentry: None,
                            promoted_from_exec_command: None,
                            accepts_input: None,
                            input_target: None,
                        }),
                        child_agent_id: None,
                        child_observability: None,
                        child_supervision: None,
                    },
                },
                command_diagnostics: None,
                summary_text: Some("command already running".into()),
            },
        )
        .unwrap();

        let rendered = render_for_model(&result).unwrap();
        assert!(rendered.contains("Command is already running"));
        assert!(rendered.contains("Existing task: task_123"));
        assert!(rendered.contains("To start a second instance"));
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
}
