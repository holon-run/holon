use std::time::Instant;

use anyhow::{anyhow, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    runtime::RuntimeHandle,
    tool::{
        helpers::{command_preview, invalid_tool_input, parse_tool_args_with_recovery_hint},
        spec::ToolResultStatus,
        ToolError, ToolResult,
    },
    types::{
        AuthorityClass, CommandTaskSpec, ExecCommandBatchItemResult, ExecCommandBatchItemStatus,
        ExecCommandBatchResult, ExecCommandOutcome, ExecCommandResult, ToolCapabilityFamily,
    },
};

use super::{serialize_success, BuiltinToolDefinition};

pub(crate) const NAME: &str = "ExecCommandBatch";
const MAX_ITEMS: usize = 12;
const DEFAULT_BATCH_YIELD_TIME_MS: u64 = 30_000;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecCommandBatchArgs {
    pub(crate) items: Vec<ExecCommandBatchItemArgs>,
    pub(crate) stop_on_error: Option<bool>,
    pub(crate) workdir: Option<String>,
    pub(crate) shell: Option<String>,
    pub(crate) login: Option<bool>,
    pub(crate) yield_time_ms: Option<u64>,
    pub(crate) max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecCommandBatchItemArgs {
    pub(crate) cmd: String,
    pub(crate) workdir: Option<String>,
    pub(crate) shell: Option<String>,
    pub(crate) login: Option<bool>,
    pub(crate) yield_time_ms: Option<u64>,
    pub(crate) max_output_tokens: Option<u64>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: crate::tool::spec::typed_spec::<ExecCommandBatchArgs>(
            NAME,
            include_str!("../tool_descriptions/exec_command_batch.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<ToolResult> {
    let args = parse_batch_args(input)?;
    validate_batch_shape(&args)?;

    let ExecCommandBatchArgs {
        items,
        stop_on_error,
        workdir,
        shell,
        login,
        yield_time_ms,
        max_output_tokens,
    } = args;

    let defaults = ExecCommandBatchDefaults {
        workdir,
        shell,
        login,
        yield_time_ms,
        max_output_tokens,
    };
    let stop_on_error = stop_on_error.unwrap_or(false);
    let mut results = Vec::with_capacity(items.len());
    let mut stop_requested = false;

    for (offset, item) in items.into_iter().enumerate() {
        let index = offset + 1;
        let item = apply_batch_defaults(item, &defaults);
        if stop_requested {
            results.push(ExecCommandBatchItemResult {
                index,
                cmd: item.cmd,
                status: ExecCommandBatchItemStatus::Skipped,
                result: None,
                error_kind: Some("skipped_after_previous_error".into()),
                error_message: Some("skipped because stop_on_error was set".into()),
                duration_ms: None,
            });
            continue;
        }

        let item_result = execute_batch_item(runtime, authority_class, index, item).await;
        if stop_on_error
            && matches!(
                item_result.status,
                ExecCommandBatchItemStatus::Failed | ExecCommandBatchItemStatus::Rejected
            )
        {
            stop_requested = true;
        }
        results.push(item_result);
    }

    let completed_count = results
        .iter()
        .filter(|item| item.status == ExecCommandBatchItemStatus::Completed)
        .count();
    let failed_count = results
        .iter()
        .filter(|item| item.status == ExecCommandBatchItemStatus::Failed)
        .count();
    let rejected_count = results
        .iter()
        .filter(|item| item.status == ExecCommandBatchItemStatus::Rejected)
        .count();
    let skipped_count = results
        .iter()
        .filter(|item| item.status == ExecCommandBatchItemStatus::Skipped)
        .count();
    let item_count = results.len();
    serialize_success(
        NAME,
        &ExecCommandBatchResult {
            item_count,
            completed_count,
            failed_count,
            rejected_count,
            skipped_count,
            stop_on_error,
            items: results,
            summary_text: Some(format!(
                "{NAME} completed {completed_count}/{item_count} items"
            )),
        },
    )
}

fn parse_batch_args(input: &Value) -> Result<ExecCommandBatchArgs> {
    parse_tool_args_with_recovery_hint(NAME, input, || exec_command_batch_recovery_hint(input))
}

fn exec_command_batch_recovery_hint(input: &Value) -> String {
    let Some(object) = input.as_object() else {
        return format!("provide input for {NAME} as {{\"items\":[{{\"cmd\":\"...\"}}]}}");
    };

    let per_item_fields = ["cmd"];
    let top_level_per_item_fields = per_item_fields
        .iter()
        .copied()
        .filter(|field| object.contains_key(*field))
        .collect::<Vec<_>>();

    if object.contains_key("cmd") {
        if top_level_per_item_fields.len() > 1 {
            return format!(
                "Top-level {} are not valid for {NAME}. If this is a single command, use ExecCommand: {{\"cmd\":\"...\"}}. For a batch, move per-command fields into items[]: {{\"items\":[{{\"cmd\":\"...\"}}]}}.",
                top_level_per_item_fields.join("/")
            );
        }
        return format!(
            "{NAME} requires {{\"items\":[{{\"cmd\":\"...\"}}]}}. If you only need to run one command, use ExecCommand instead: {{\"cmd\":\"...\"}}. Use {NAME} only when running multiple sequential commands: {{\"items\":[{{\"cmd\":\"...\"}},{{\"cmd\":\"...\"}}]}}."
        );
    }

    if !top_level_per_item_fields.is_empty() {
        return format!(
            "Top-level {} are not valid for {NAME}; they are per-item fields. If this is a single command, use ExecCommand. For a batch, move them into items[]: {{\"items\":[{{\"cmd\":\"...\"}}]}}.",
            top_level_per_item_fields.join("/")
        );
    }

    let item_has_interactive_field =
        object
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_object().is_some_and(|item| {
                        item.contains_key("tty") || item.contains_key("accepts_input")
                    })
                })
            });
    if item_has_interactive_field {
        return "ExecCommandBatch items do not accept `tty` or `accepts_input`; use ExecCommand for interactive commands, or remove those fields from batch items.".to_string();
    }

    format!("provide input for {NAME} that matches the published tool schema")
}

fn validate_batch_shape(args: &ExecCommandBatchArgs) -> Result<()> {
    if args.items.is_empty() {
        return Err(invalid_tool_input(
            NAME,
            format!("{NAME} requires at least one item"),
            json!({
                "field": "items",
                "validation_error": "must not be empty",
            }),
            "provide one or more ExecCommandBatch items",
        ));
    }
    if args.items.len() > MAX_ITEMS {
        return Err(invalid_tool_input(
            NAME,
            format!("{NAME} accepts at most {MAX_ITEMS} items"),
            json!({
                "field": "items",
                "validation_error": "too_many_items",
                "max_items": MAX_ITEMS,
                "actual_items": args.items.len(),
            }),
            "split the command batch into smaller bounded batches",
        ));
    }
    Ok(())
}

struct ExecCommandBatchDefaults {
    workdir: Option<String>,
    shell: Option<String>,
    login: Option<bool>,
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<u64>,
}

fn apply_batch_defaults(
    mut item: ExecCommandBatchItemArgs,
    defaults: &ExecCommandBatchDefaults,
) -> ExecCommandBatchItemArgs {
    if item.workdir.is_none() {
        item.workdir.clone_from(&defaults.workdir);
    }
    if item.shell.is_none() {
        item.shell.clone_from(&defaults.shell);
    }
    if item.login.is_none() {
        item.login = defaults.login;
    }
    if item.yield_time_ms.is_none() {
        item.yield_time_ms = defaults.yield_time_ms;
    }
    if item.max_output_tokens.is_none() {
        item.max_output_tokens = defaults.max_output_tokens;
    }
    item
}

async fn execute_batch_item(
    runtime: &RuntimeHandle,
    authority_class: &AuthorityClass,
    index: usize,
    item: ExecCommandBatchItemArgs,
) -> ExecCommandBatchItemResult {
    let started = Instant::now();
    let cmd = item.cmd.clone();
    let spec = CommandTaskSpec {
        cmd: item.cmd,
        workdir: item.workdir,
        shell: item.shell,
        login: item.login.unwrap_or(true),
        tty: false,
        yield_time_ms: item.yield_time_ms.unwrap_or(DEFAULT_BATCH_YIELD_TIME_MS),
        max_output_tokens: item.max_output_tokens,
        accepts_input: false,
        terminal_reentry: false,
    };

    match runtime
        .managed_tasks()
        .execute_exec_command_once(spec, authority_class)
        .await
    {
        Ok(result) => {
            let status = match result.outcome {
                ExecCommandOutcome::Completed { exit_status, .. } if exit_status == Some(0) => {
                    ExecCommandBatchItemStatus::Completed
                }
                ExecCommandOutcome::Completed { .. } => ExecCommandBatchItemStatus::Failed,
                ExecCommandOutcome::PromotedToTask { .. } => ExecCommandBatchItemStatus::Failed,
                ExecCommandOutcome::AlreadyRunning { .. } => ExecCommandBatchItemStatus::Failed,
            };
            ExecCommandBatchItemResult {
                index,
                cmd,
                status,
                result: Some(result),
                error_kind: None,
                error_message: None,
                duration_ms: Some(started.elapsed().as_millis() as u64),
            }
        }
        Err(error) => {
            let tool_error = ToolError::from_anyhow(&error);
            ExecCommandBatchItemResult {
                index,
                cmd,
                status: ExecCommandBatchItemStatus::Failed,
                result: None,
                error_kind: Some(tool_error.kind),
                error_message: Some(tool_error.message),
                duration_ms: Some(started.elapsed().as_millis() as u64),
            }
        }
    }
}

pub(crate) fn render_for_model(result: &ToolResult) -> Result<String> {
    if matches!(result.envelope.status, ToolResultStatus::Error) {
        let error = result
            .tool_error()
            .ok_or_else(|| anyhow!("{NAME} error result missing tool error"))?;
        return Ok(format!("{NAME} failed\n{}\n", error.render()));
    }

    let value = result
        .envelope
        .result
        .clone()
        .ok_or_else(|| anyhow!("{NAME} result missing payload"))?;
    let result: ExecCommandBatchResult = serde_json::from_value(value)?;
    let mut lines = vec![format!(
        "{NAME} completed {}/{} items",
        result.completed_count, result.item_count
    )];
    for item in result.items {
        lines.push(String::new());
        lines.push(format!("[{}] {}", item.index, command_preview(&item.cmd)));
        match item.status {
            ExecCommandBatchItemStatus::Completed | ExecCommandBatchItemStatus::Failed => {
                render_exec_item(&mut lines, item.result.as_ref());
                if let Some(message) = item.error_message.as_deref() {
                    lines.push(format!("error: {message}"));
                }
            }
            ExecCommandBatchItemStatus::Rejected => {
                lines.push(format!(
                    "rejected={}",
                    item.error_kind.as_deref().unwrap_or("rejected")
                ));
                if let Some(message) = item.error_message.as_deref() {
                    lines.push(message.to_string());
                }
            }
            ExecCommandBatchItemStatus::Skipped => {
                lines.push("skipped=stop_on_error".to_string());
            }
        }
    }
    Ok(lines.join("\n"))
}

fn render_exec_item(lines: &mut Vec<String>, result: Option<&ExecCommandResult>) {
    let Some(result) = result else {
        lines.push("failed=no_result".to_string());
        return;
    };
    match &result.outcome {
        ExecCommandOutcome::Completed {
            exit_status,
            stdout_preview,
            stderr_preview,
            artifacts,
            ..
        } => {
            match exit_status {
                Some(code) => lines.push(format!("exit={code}")),
                None => lines.push("exit=unknown".to_string()),
            }
            if let Some(stdout) = stdout_preview
                .as_ref()
                .filter(|value| !value.trim().is_empty())
            {
                lines.push("stdout:".to_string());
                lines.push(stdout.clone());
            }
            if let Some(stderr) = stderr_preview
                .as_ref()
                .filter(|value| !value.trim().is_empty())
            {
                lines.push("stderr:".to_string());
                lines.push(stderr.clone());
            }
            if !artifacts.is_empty() {
                lines.push("Artifacts:".to_string());
                lines.extend(artifacts.iter().map(|artifact| artifact.path.clone()));
            }
        }
        ExecCommandOutcome::PromotedToTask { task_handle, .. } => {
            lines.push("failed=promoted_to_task".to_string());
            lines.push(format!("task={}", task_handle.task_id));
        }
        ExecCommandOutcome::AlreadyRunning { task_handle, .. } => {
            lines.push("failed=already_running".to_string());
            lines.push(format!("task={}", task_handle.task_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolError;

    #[test]
    fn rejects_top_level_cmd_with_exec_command_hint() {
        let input = serde_json::json!({
            "cmd": "git status",
            "max_output_tokens": 1000
        });
        let error = parse_batch_args(&input).unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);

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
        let tool_error = ToolError::from_anyhow(&error);

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
}
