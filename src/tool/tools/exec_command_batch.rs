use std::time::Instant;

use anyhow::{anyhow, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    runtime::RuntimeHandle,
    tool::{
        helpers::{invalid_tool_input, parse_tool_args},
        spec::ToolResultStatus,
        ToolError, ToolResult,
    },
    types::{
        CommandTaskSpec, ExecCommandBatchItemResult, ExecCommandBatchItemStatus,
        ExecCommandBatchResult, ExecCommandOutcome, ExecCommandResult, ToolCapabilityFamily,
        TrustLevel,
    },
};

use super::{serialize_success, BuiltinToolDefinition};

pub(crate) const NAME: &str = "ExecCommandBatch";
const MAX_ITEMS: usize = 12;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecCommandBatchArgs {
    pub(crate) items: Vec<ExecCommandBatchItemArgs>,
    pub(crate) stop_on_error: Option<bool>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecCommandBatchItemArgs {
    pub(crate) cmd: String,
    pub(crate) workdir: Option<String>,
    pub(crate) shell: Option<String>,
    pub(crate) login: Option<bool>,
    pub(crate) yield_time_ms: Option<u64>,
    pub(crate) max_output_tokens: Option<u64>,
    pub(crate) tty: Option<bool>,
    pub(crate) accepts_input: Option<bool>,
    pub(crate) continue_on_result: Option<bool>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: crate::tool::spec::typed_spec::<ExecCommandBatchArgs>(
            NAME,
            "Run a bounded sequential batch of ExecCommand-like startup requests and return one grouped receipt. Each item supports cmd plus optional workdir, shell, login, yield_time_ms, and max_output_tokens. Do not use tty, accepts_input, continue_on_result, or non-command tools inside the batch.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    trust: &TrustLevel,
    input: &Value,
) -> Result<ToolResult> {
    let args: ExecCommandBatchArgs = parse_tool_args(NAME, input)?;
    validate_batch_shape(&args)?;

    let stop_on_error = args.stop_on_error.unwrap_or(false);
    let mut results = Vec::with_capacity(args.items.len());
    let mut stop_requested = false;

    for (offset, item) in args.items.into_iter().enumerate() {
        let index = offset + 1;
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

        let item_result = execute_batch_item(runtime, trust, index, item).await;
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

async fn execute_batch_item(
    runtime: &RuntimeHandle,
    trust: &TrustLevel,
    index: usize,
    item: ExecCommandBatchItemArgs,
) -> ExecCommandBatchItemResult {
    let started = Instant::now();
    let cmd = item.cmd.clone();
    if let Some(error) = rejected_item_error(&item) {
        return ExecCommandBatchItemResult {
            index,
            cmd,
            status: ExecCommandBatchItemStatus::Rejected,
            result: None,
            error_kind: Some(error.kind),
            error_message: Some(error.message),
            duration_ms: Some(started.elapsed().as_millis() as u64),
        };
    }

    let spec = CommandTaskSpec {
        cmd: item.cmd,
        workdir: item.workdir,
        shell: item.shell,
        login: item.login.unwrap_or(true),
        tty: false,
        yield_time_ms: item.yield_time_ms.unwrap_or(10_000),
        max_output_tokens: item.max_output_tokens,
        accepts_input: false,
        continue_on_result: false,
    };

    match runtime.execute_exec_command_once(spec, trust).await {
        Ok(result) => {
            let status = match result.outcome {
                ExecCommandOutcome::Completed { exit_status, .. } if exit_status == Some(0) => {
                    ExecCommandBatchItemStatus::Completed
                }
                ExecCommandOutcome::Completed { .. } => ExecCommandBatchItemStatus::Failed,
                ExecCommandOutcome::PromotedToTask { .. } => ExecCommandBatchItemStatus::Failed,
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

fn rejected_item_error(item: &ExecCommandBatchItemArgs) -> Option<ToolError> {
    let field = if item.tty.is_some() {
        Some("tty")
    } else if item.accepts_input.is_some() {
        Some("accepts_input")
    } else if item.continue_on_result.is_some() {
        Some("continue_on_result")
    } else {
        None
    }?;

    Some(
        ToolError::new(
            "unsupported_batch_command_field",
            format!("{NAME} items do not support `{field}`"),
        )
        .with_details(json!({
            "field": field,
            "reason": "batch command items cannot request interactive or command-task continuation semantics",
        }))
        .with_recovery_hint(format!(
            "call ExecCommand directly when you need `{field}`"
        ))
        .with_retryable(false),
    )
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
        lines.push(format!("[{}] {}", item.index, item.cmd));
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_command_task_fields_in_items() {
        let item = ExecCommandBatchItemArgs {
            cmd: "python -i".into(),
            workdir: None,
            shell: None,
            login: None,
            yield_time_ms: None,
            max_output_tokens: None,
            tty: Some(true),
            accepts_input: None,
            continue_on_result: None,
        };

        let error = rejected_item_error(&item).expect("tty should be rejected");
        assert_eq!(error.kind, "unsupported_batch_command_field");
        assert!(error.message.contains("tty"));
    }
}
