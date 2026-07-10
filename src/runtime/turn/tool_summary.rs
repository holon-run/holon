//! Tool result summarization and round recap generation.

use serde_json::Value;

use crate::tool::{
    helpers::command_digest,
    names as tn,
    spec::{ToolResultEnvelope, ToolResultStatus},
    summary::tool_result_summary,
    ToolCall,
};

use super::projection::estimate_text_tokens;
use super::truncate_preview;
use super::{TurnRoundRecord, RECAP_TEXT_PREVIEW_LIMIT};

pub(super) fn summarize_tool_result_envelope(envelope: &ToolResultEnvelope) -> String {
    match envelope.status {
        ToolResultStatus::Error => {
            let error = envelope.error.as_ref();
            let kind = error
                .map(|error| error.kind.as_str())
                .unwrap_or("tool_execution_failed");
            format!(
                "{} error {}: {}",
                envelope.tool_name,
                kind,
                tool_result_summary(envelope)
            )
        }
        ToolResultStatus::Success => {
            format!("{} {}", envelope.tool_name, tool_result_summary(envelope))
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
        tn::EXEC_COMMAND => {
            let cmd = call.input.get("cmd").and_then(Value::as_str)?;
            Some(format!(
                "ExecCommand tool_call_id={} cmd_digest={}",
                call.id,
                command_digest(cmd)
            ))
        }
        tn::EXEC_COMMAND_BATCH => {
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
