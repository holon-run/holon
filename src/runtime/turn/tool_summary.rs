//! Tool result summarization and round recap generation.

use serde_json::Value;

use crate::tool::{
    helpers::command_digest,
    spec::{ToolResultEnvelope, ToolResultStatus},
    ToolCall,
};

use super::projection::estimate_text_tokens;
use super::truncate_preview;
use super::{TurnRoundRecord, RECAP_TEXT_PREVIEW_LIMIT};

pub(super) fn artifact_paths(value: &Value) -> Vec<String> {
    value
        .get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|artifact| artifact.get("path").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn summarize_exec_command_result(result: &Value) -> String {
    let disposition = result
        .get("disposition")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match disposition {
        "completed" => {
            let exit_status = result
                .get("exit_status")
                .and_then(Value::as_i64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into());
            let truncated = result
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let artifacts = artifact_paths(result);
            let artifact_note = if artifacts.is_empty() {
                String::new()
            } else {
                format!(" artifacts={}", artifacts.join(", "))
            };
            format!("completed exit_status={exit_status} truncated={truncated}{artifact_note}")
        }
        "promoted_to_task" => {
            let task_id = result
                .get("task_handle")
                .and_then(|handle| handle.get("task_id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let truncated = result
                .get("initial_output_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            format!("promoted_to_task task_id={task_id} initial_output_truncated={truncated}")
        }
        "already_running" => {
            let task_id = result
                .get("task_handle")
                .and_then(|handle| handle.get("task_id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let status = result
                .get("task_handle")
                .and_then(|handle| handle.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("already_running task_id={task_id} status={status}")
        }
        _ => format!("disposition={disposition}"),
    }
}

pub(super) fn summarize_task_output_result(result: &Value) -> String {
    let retrieval_status = result
        .get("retrieval_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let task = result.get("task").unwrap_or(result);
    let task_id = task.get("id").and_then(Value::as_str).unwrap_or("unknown");
    let truncated = task
        .get("output_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_status = task
        .get("exit_status")
        .and_then(Value::as_i64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into());
    let artifacts = artifact_paths(task);
    let artifact_note = if artifacts.is_empty() {
        String::new()
    } else {
        format!(" artifacts={}", artifacts.join(", "))
    };
    format!(
        "retrieval_status={retrieval_status} task_id={task_id} output_truncated={truncated} exit_status={exit_status}{artifact_note}"
    )
}

pub(super) fn summarize_spawn_agent_result(result: &Value) -> Option<String> {
    let agent_id = result.get("agent_id").and_then(Value::as_str)?;
    let task_id = result
        .get("task_handle")
        .and_then(|handle| handle.get("task_id"))
        .and_then(Value::as_str);
    Some(match task_id {
        Some(task_id) => format!("agent_id={agent_id} task_id={task_id}"),
        None => format!("agent_id={agent_id}"),
    })
}

pub(super) fn summarize_view_image_result(result: &Value) -> Option<String> {
    let reference = result.get("visual_reference")?;
    let observation = result.get("observation")?;
    let reference_id = reference
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mime = reference
        .get("mime")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let schema = observation
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let prompt = observation
        .get("prompt")
        .and_then(Value::as_str)
        .map(|prompt| truncate_preview(prompt, RECAP_TEXT_PREVIEW_LIMIT));
    let summary = observation
        .get("summary")
        .and_then(Value::as_str)
        .map(|summary| truncate_preview(summary, RECAP_TEXT_PREVIEW_LIMIT));
    let ocr = observation
        .get("ocr")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("text").and_then(Value::as_str))
                .take(3)
                .map(|text| truncate_preview(text, 80))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut parts = vec![format!(
        "visual_observation schema={schema} ref={reference_id} mime={mime}"
    )];
    if let Some(prompt) = prompt {
        parts.push(format!("prompt=\"{prompt}\""));
    }
    if let Some(summary) = summary {
        parts.push(format!("summary=\"{summary}\""));
    }
    if !ocr.is_empty() {
        parts.push(format!("ocr=[{}]", ocr.join(" | ")));
    }
    Some(parts.join(" "))
}

pub(super) fn summarize_tool_result_envelope(envelope: &ToolResultEnvelope) -> String {
    match envelope.status {
        ToolResultStatus::Error => {
            let error = envelope.error.as_ref();
            let kind = error
                .map(|error| error.kind.as_str())
                .unwrap_or("tool_execution_failed");
            let message = envelope
                .summary_text
                .as_deref()
                .or_else(|| error.map(|error| error.message.as_str()))
                .unwrap_or("tool failed");
            format!("{} error {}: {}", envelope.tool_name, kind, message)
        }
        ToolResultStatus::Success => {
            let detail = envelope
                .result
                .as_ref()
                .and_then(|result| match envelope.tool_name.as_str() {
                    "ExecCommand" => Some(summarize_exec_command_result(result)),
                    "TaskOutput" => Some(summarize_task_output_result(result)),
                    "SpawnAgent" => summarize_spawn_agent_result(result),
                    "ViewImage" => summarize_view_image_result(result),
                    _ => None,
                })
                .or_else(|| envelope.summary_text.clone())
                .unwrap_or_else(|| "completed".into());
            format!("{} {}", envelope.tool_name, detail)
        }
    }
}

pub(super) fn build_round_recap_line(round: &TurnRoundRecord) -> String {
    let assistant_text = round
        .text_blocks
        .iter()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let assistant_preview = if assistant_text.is_empty() {
        None
    } else {
        Some(truncate_preview(&assistant_text, RECAP_TEXT_PREVIEW_LIMIT))
    };
    let tool_calls = round
        .tool_calls
        .iter()
        .map(|call| call.name.as_str())
        .collect::<Vec<_>>();
    let tool_results = round
        .tool_result_envelopes
        .iter()
        .map(summarize_tool_result_envelope)
        .collect::<Vec<_>>();

    let mut parts = Vec::new();
    if let Some(preview) = assistant_preview {
        parts.push(format!("assistant=\"{preview}\""));
    }
    if !tool_calls.is_empty() {
        parts.push(format!("tool_calls=[{}]", tool_calls.join(", ")));
    }
    let command_receipts = round
        .tool_calls
        .iter()
        .filter_map(command_recap_receipt)
        .collect::<Vec<_>>();
    if !command_receipts.is_empty() {
        parts.push(format!(
            "recoverable_command_inputs=[{}]",
            command_receipts.join(", ")
        ));
    }
    if !tool_results.is_empty() {
        parts.push(format!("results=[{}]", tool_results.join(" | ")));
    }
    if !round.follow_up_user_texts.is_empty() {
        parts.push(format!(
            "follow_up_user_texts={}",
            round.follow_up_user_texts.len()
        ));
    }

    let detail = if parts.is_empty() {
        "no compactable detail".into()
    } else {
        parts.join("; ")
    };
    format!("- Round {}: {}", round.round, detail)
}

pub(super) fn command_recap_receipt(call: &ToolCall) -> Option<String> {
    match call.name.as_str() {
        "ExecCommand" => {
            let cmd = call.input.get("cmd").and_then(Value::as_str)?;
            Some(format!(
                "ExecCommand tool_call_id={} cmd_digest={}",
                call.id,
                command_digest(cmd)
            ))
        }
        "ExecCommandBatch" => {
            let items = call.input.get("items").and_then(Value::as_array)?;
            let refs = items
                .iter()
                .enumerate()
                .filter_map(|(offset, item)| {
                    let cmd = item.get("cmd").and_then(Value::as_str)?;
                    Some(format!(
                        "item={} cmd_digest={}",
                        offset + 1,
                        command_digest(cmd)
                    ))
                })
                .collect::<Vec<_>>();
            (!refs.is_empty()).then(|| {
                format!(
                    "ExecCommandBatch tool_call_id={} {}",
                    call.id,
                    refs.join(" | ")
                )
            })
        }
        _ => None,
    }
}

pub(super) fn build_compacted_round_recap(
    rounds: &[TurnRoundRecord],
    recap_budget: usize,
) -> String {
    if rounds.is_empty() {
        return String::new();
    }

    let header =
        "Turn-local recap for older completed rounds (runtime-generated deterministic summary):";
    let fallback = format!(
        "{header}\n- {} older rounds compacted; consult transcript or referenced artifacts if exact details are needed.",
        rounds.len()
    );
    if estimate_text_tokens(&fallback) > recap_budget {
        if estimate_text_tokens(header) <= recap_budget {
            return header.to_string();
        }
        return String::new();
    }

    let mut recap = String::from(header);
    let mut omitted = 0usize;
    for (idx, round) in rounds.iter().enumerate() {
        let line = build_round_recap_line(round);
        let candidate = format!("{recap}\n{line}");
        if estimate_text_tokens(&candidate) > recap_budget {
            omitted = rounds.len().saturating_sub(idx);
            break;
        }
        recap = candidate;
    }

    if omitted > 0 {
        let omission_line =
            format!("- Older compacted rounds omitted from this recap due to budget: {omitted}");
        let candidate = format!("{recap}\n{omission_line}");
        if estimate_text_tokens(&candidate) <= recap_budget {
            recap = candidate;
        }
    }

    recap
}
