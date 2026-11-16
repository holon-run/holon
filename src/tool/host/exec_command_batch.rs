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

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_top_level_cmd_with_exec_command_hint() {
        let input = serde_json::json!({
            "cmd": "git status",
            "max_output_tokens": 1000
        });
        let error = parse_batch_args(&input).unwrap_err();
        let tool_error = crate::tool::tool_error_from_anyhow(&error);

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
        let tool_error = crate::tool::tool_error_from_anyhow(&error);

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
