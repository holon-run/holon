use super::BuiltinToolDefinition;
use crate::tool::helpers::command_preview;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::parse_tool_args_with_recovery_hint;
use crate::tool::spec::ToolOutcome;
use crate::tool::spec::ToolResultStatus;
use crate::types::ExecCommandBatchItemStatus;
use crate::types::ExecCommandBatchResult;
use crate::types::ExecCommandOutcome;
use crate::types::ExecCommandResult;
use crate::types::ToolCapabilityFamily;
use anyhow::anyhow;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

pub const NAME: &str = crate::tool::names::EXEC_COMMAND_BATCH;

pub const MAX_ITEMS: usize = 12;

pub const DEFAULT_BATCH_YIELD_TIME_MS: u64 = 30_000;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandBatchArgs {
    /// One to twelve sequential commands; every array entry (including an
    /// empty object) counts toward the batch limit.
    #[schemars(length(min = 1, max = 12))]
    pub items: Vec<ExecCommandBatchItemArgs>,
    pub stop_on_error: Option<bool>,
    pub workdir: Option<String>,
    pub shell: Option<String>,
    pub login: Option<bool>,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandBatchItemArgs {
    pub cmd: String,
    pub workdir: Option<String>,
    pub shell: Option<String>,
    pub login: Option<bool>,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: crate::tool::spec::typed_spec::<ExecCommandBatchArgs>(
            NAME,
            include_str!("../tool_descriptions/exec_command_batch.md"),
        )?,
    })
}

pub fn parse_batch_args(input: &Value) -> Result<ExecCommandBatchArgs> {
    // Reject oversized raw item arrays before per-item typed parsing,
    // recovery-hint scanning, or any execution work; empty arrays keep the
    // existing validate_batch_shape error, and missing or non-array `items`
    // keeps the existing typed parse failure.
    if let Some(items) = input.get("items").and_then(Value::as_array) {
        if items.len() > MAX_ITEMS {
            return Err(invalid_tool_input(
                NAME,
                format!("{NAME} accepts at most {MAX_ITEMS} items"),
                json!({
                    "field": "items",
                    "validation_error": "too_many_items",
                    "max_items": MAX_ITEMS,
                    "actual_items": items.len(),
                }),
                "split the command batch into smaller bounded batches",
            ));
        }
    }
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

pub fn validate_batch_shape(args: &ExecCommandBatchArgs) -> Result<()> {
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

pub struct ExecCommandBatchDefaults {
    pub workdir: Option<String>,
    pub shell: Option<String>,
    pub login: Option<bool>,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

pub fn apply_batch_defaults(
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

pub fn render_for_model(result: &ToolOutcome<impl Default>) -> Result<String> {
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
            truncated,
            ..
        } => {
            match exit_status {
                Some(code) => lines.push(format!("exit={code}")),
                None => lines.push("exit=unknown".to_string()),
            }
            if *truncated {
                lines.push("Output truncated; stream previews are contiguous prefixes.".into());
            }
            if let Some(stdout) = stdout_preview.as_ref().filter(|value| !value.is_empty()) {
                lines.push(format!(
                    "stdout: (Unicode scalar range [0, {}))",
                    stdout.chars().count()
                ));
                lines.push(stdout.clone());
            }
            if let Some(stderr) = stderr_preview.as_ref().filter(|value| !value.is_empty()) {
                lines.push(format!(
                    "stderr: (Unicode scalar range [0, {}))",
                    stderr.chars().count()
                ));
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
