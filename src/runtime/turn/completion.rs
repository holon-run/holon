//! Work-item completion report promotion during turns.

use crate::tool::names as tn;
use anyhow::Result;
use serde_json::Value;

use crate::provider::{ModelBlock, ToolResultBlock};
use crate::runtime::{
    RuntimeHandle, WorkItemCompletionReportPromotion, WorkItemCompletionReportPromotionOutcome,
};
use crate::tool::{
    helpers::{
        command_cost_diagnostics, command_display, command_preview, effective_tool_output_tokens,
    },
    spec::{ToolResultEnvelope, ToolResultStatus},
    ToolCall,
};
use crate::types::AuditEvent;

use super::{truncate_preview, ROUND_TEXT_PREVIEW_LIMIT};
impl RuntimeHandle {
    pub(super) async fn promote_round_completion_report_if_present(
        &self,
        agent_id: &str,
        round: usize,
        turn_index: u64,
        assistant_blocks: &[ModelBlock],
        tool_results: &mut [ToolResultBlock],
        tool_result_envelopes: &mut [ToolResultEnvelope],
    ) -> Result<Vec<WorkItemCompletionReportPromotion>> {
        let completion_indexes = tool_result_envelopes
            .iter()
            .enumerate()
            .filter(|(_, envelope)| {
                envelope.tool_name == "CompleteWorkItem"
                    && envelope.status == ToolResultStatus::Success
                    && envelope
                        .result
                        .as_ref()
                        .and_then(|result| result.get("completed_transition"))
                        .and_then(Value::as_bool)
                        == Some(true)
            })
            .filter_map(|(index, envelope)| result_work_item_id(envelope).map(|id| (index, id)))
            .collect::<Vec<_>>();
        if completion_indexes.is_empty() {
            return Ok(Vec::new());
        }

        let report_texts_by_tool_id = completion_report_texts_by_tool_id(assistant_blocks);
        let mut promotions = Vec::new();
        for (index, work_item_id) in completion_indexes {
            let tool_use_id = tool_results[index].tool_use_id.as_str();
            let report_text = report_texts_by_tool_id
                .iter()
                .find_map(|(id, text)| (id == tool_use_id).then_some(text.trim()))
                .unwrap_or_default();
            if report_text.is_empty() {
                let warning = completion_report_warning(
                    "missing_completion_report",
                    "CompleteWorkItem succeeded without nearby preceding same-round operator-facing report text; no canonical completion report was promoted.",
                );
                append_completion_warning(&mut tool_result_envelopes[index], warning.clone());
                update_tool_result_block_content(
                    index,
                    tool_results,
                    &tool_result_envelopes[index],
                )?;
                self.record_work_item_completion_warning(
                    work_item_id,
                    "missing_completion_report",
                    "CompleteWorkItem succeeded without nearby preceding same-round operator-facing report text; no canonical completion report was promoted.",
                    Some(turn_index),
                    Some(round),
                )
                .await?;
                continue;
            }

            let warnings = envelope_warnings(&tool_result_envelopes[index]);
            let promotion = match self
                .promote_work_item_completion_report_with_metadata(
                    work_item_id.clone(),
                    report_text.to_string(),
                    Some(turn_index),
                    Some(round),
                    warnings,
                )
                .await?
            {
                WorkItemCompletionReportPromotionOutcome::Promoted(promotion) => promotion,
                WorkItemCompletionReportPromotionOutcome::Unchanged(_) => continue,
            };
            if let Some(result) = tool_result_envelopes[index].result.as_mut() {
                if let Some(object) = result.as_object_mut() {
                    object.insert("completion_report_promoted".into(), serde_json::json!(true));
                    object.insert(
                        "completion_report_source".into(),
                        serde_json::json!("same_assistant_round_preceding_text"),
                    );
                }
            }
            update_tool_result_block_content(index, tool_results, &tool_result_envelopes[index])?;
            self.inner.storage.append_event(&AuditEvent::legacy(
                "work_item_completion_report_candidate_promoted",
                serde_json::json!({
                    "agent_id": agent_id,
                    "work_item_id": work_item_id,
                    "turn_index": turn_index,
                    "round": round,
                    "brief_id": promotion.brief_id.clone(),
                    "text_preview": truncate_preview(report_text, ROUND_TEXT_PREVIEW_LIMIT),
                }),
            ))?;
            promotions.push(promotion);
        }
        Ok(promotions)
    }
}

pub(super) fn completion_report_texts_by_tool_id(
    assistant_blocks: &[ModelBlock],
) -> Vec<(String, String)> {
    let mut pending_text = Vec::<String>::new();
    let mut reports = Vec::<(String, String)>::new();
    for block in assistant_blocks {
        match block {
            ModelBlock::Text { text } => {
                if !text.trim().is_empty() {
                    pending_text.push(text.clone());
                }
            }
            ModelBlock::ToolUse { id, name, .. } => {
                if name == "CompleteWorkItem" {
                    let report_text = pending_text
                        .iter()
                        .map(|text| text.trim())
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    reports.push((id.clone(), report_text));
                }
                pending_text.clear();
            }
            ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => {}
        }
    }
    reports
}

pub(super) fn result_work_item_id(envelope: &ToolResultEnvelope) -> Option<String> {
    envelope
        .result
        .as_ref()?
        .get("work_item")?
        .get("id")?
        .as_str()
        .map(ToString::to_string)
}

pub(super) fn envelope_completes_work_item(envelope: &ToolResultEnvelope) -> bool {
    envelope.tool_name == "CompleteWorkItem"
        && envelope.status == ToolResultStatus::Success
        && envelope
            .result
            .as_ref()
            .and_then(|result| result.get("completed_transition"))
            .and_then(Value::as_bool)
            == Some(true)
}

pub(super) fn envelope_warnings(envelope: &ToolResultEnvelope) -> Vec<Value> {
    envelope
        .result
        .as_ref()
        .and_then(|result| result.get("warnings"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn completion_report_warning(kind: &str, message: &str) -> Value {
    serde_json::json!({
        "kind": kind,
        "message": message,
    })
}

pub(super) fn append_completion_warning(envelope: &mut ToolResultEnvelope, warning: Value) {
    let Some(result) = envelope.result.as_mut() else {
        return;
    };
    let Some(object) = result.as_object_mut() else {
        return;
    };
    let warnings = object
        .entry("warnings")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(array) = warnings.as_array_mut() {
        array.push(warning);
    }
}

pub(super) fn update_tool_result_block_content(
    index: usize,
    tool_results: &mut [ToolResultBlock],
    envelope: &ToolResultEnvelope,
) -> Result<()> {
    if let Some(block) = tool_results.get_mut(index) {
        block.content = serde_json::to_string(envelope)?;
    }
    Ok(())
}

pub(super) fn command_preview_field(call: &ToolCall) -> Option<String> {
    (call.name == tn::EXEC_COMMAND)
        .then(|| call.input.get("cmd").and_then(Value::as_str))
        .flatten()
        .map(command_preview)
}

pub(super) fn command_display_field(call: &ToolCall) -> Option<String> {
    (call.name == tn::EXEC_COMMAND)
        .then(|| call.input.get("cmd").and_then(Value::as_str))
        .flatten()
        .map(command_display)
}

pub(super) fn command_batch_preview_field(call: &ToolCall) -> Option<Value> {
    if call.name != tn::EXEC_COMMAND_BATCH {
        return None;
    }
    let items = call.input.get("items").and_then(Value::as_array)?;
    let previews = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            item.get("cmd")
                .and_then(Value::as_str)
                .map(command_preview)
                .map(|cmd| {
                    serde_json::json!({
                        "index": index,
                        "cmd": cmd,
                        "cmd_display": item
                            .get("cmd")
                            .and_then(Value::as_str)
                            .map(command_display),
                    })
                })
        })
        .collect::<Vec<_>>();
    (!previews.is_empty()).then(|| Value::Array(previews))
}

pub(super) fn exec_command_disposition_field(
    call: &ToolCall,
    envelope: &ToolResultEnvelope,
) -> Option<String> {
    matches!(call.name.as_str(), name if name == tn::EXEC_COMMAND || name == tn::EXEC_COMMAND_BATCH)
        .then(|| envelope.result.as_ref())
        .flatten()
        .and_then(|result| result.get("disposition"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn exec_command_exit_status_field(
    call: &ToolCall,
    envelope: &ToolResultEnvelope,
) -> Option<i32> {
    matches!(call.name.as_str(), name if name == tn::EXEC_COMMAND || name == tn::EXEC_COMMAND_BATCH)
        .then(|| envelope.result.as_ref())
        .flatten()
        .and_then(|result| result.get("exit_status"))
        .and_then(Value::as_i64)
        .map(|status| status as i32)
}

pub(super) fn exec_command_task_handle_field(
    call: &ToolCall,
    envelope: &ToolResultEnvelope,
) -> Option<Value> {
    matches!(call.name.as_str(), name if name == tn::EXEC_COMMAND || name == tn::EXEC_COMMAND_BATCH)
        .then(|| envelope.result.as_ref())
        .flatten()
        .and_then(|result| result.get("task_handle"))
        .cloned()
}

pub(super) fn command_cost_field(
    call: &ToolCall,
    default_tool_output_tokens: u64,
    max_tool_output_tokens: u64,
) -> Option<serde_json::Value> {
    if call.name != tn::EXEC_COMMAND {
        return None;
    }
    let cmd = call.input.get("cmd").and_then(Value::as_str)?;
    let requested = call.input.get("max_output_tokens").and_then(Value::as_u64);
    let effective = effective_tool_output_tokens(
        requested,
        default_tool_output_tokens,
        max_tool_output_tokens,
    );
    match serde_json::to_value(command_cost_diagnostics(cmd, effective)) {
        Ok(value) => Some(value),
        Err(error) => {
            eprintln!(
                "failed to serialize command cost diagnostics for ExecCommand audit event: {error}"
            );
            None
        }
    }
}

pub(super) fn rejects_truncated_mutation_tool_call(tool_name: &str) -> bool {
    matches!(
        tool_name,
        tn::APPLY_PATCH
            | tn::CREATE_WORK_ITEM
            | tn::PICK_WORK_ITEM
            | tn::UPDATE_WORK_ITEM
            | tn::COMPLETE_WORK_ITEM
    )
}

pub(super) fn truncated_mutation_recovery_hint(tool_name: &str) -> &'static str {
    if tool_name == tn::APPLY_PATCH {
        "the previous ApplyPatch mutation was not executed because the provider stopped at the output limit; do not resend the same huge patch unchanged. Retry as a complete smaller patch, a sequence of smaller patches, or a bounded ExecCommand/scripted rewrite when cheaper to verify. Inspect only the necessary context, not broad surrounding files"
    } else {
        "the previous mutation was not executed because the provider stopped at the output limit; do not resend the same oversized tool call unchanged. Retry with a complete smaller tool call, or split the state update into a short sequence of complete tool calls after inspecting only the necessary context"
    }
}
