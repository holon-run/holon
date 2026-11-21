use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::spec::ToolExecutionContext;
use crate::tool::ToolResult;
use crate::types::AuthorityClass;
use crate::types::CommandTaskSpec;
use crate::types::ExecCommandBatchItemResult;
use crate::types::ExecCommandBatchItemStatus;
use crate::types::ExecCommandBatchResult;
use crate::types::ExecCommandOutcome;
use anyhow::Result;
use serde_json::Value;
use std::time::Instant;

pub(crate) use holon_tools::builtins::exec_command_batch::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
    context: &ToolExecutionContext,
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

        let item_result =
            execute_batch_item(runtime, authority_class, index, item, input, context).await?;
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

async fn execute_batch_item(
    runtime: &RuntimeHandle,
    authority_class: &AuthorityClass,
    index: usize,
    item: ExecCommandBatchItemArgs,
    input: &Value,
    context: &ToolExecutionContext,
) -> Result<ExecCommandBatchItemResult> {
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

    Ok(
        match runtime
            .managed_tasks()
            .execute_exec_command_once(spec, authority_class, context, NAME, input)
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
            Err(error) if error.is::<crate::tool::spec::ExternalCommandRegistrationFailed>() => {
                return Err(error);
            }
            Err(error) => {
                let tool_error = crate::tool::tool_error_from_anyhow(&error);
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
        },
    )
}
