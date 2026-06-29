//! Turn execution module: drives the provider conversation loop, context
//! projection, checkpointing, tool execution, and completion handling.
//!
//! Public entrypoints are [`RuntimeHandle::run_agent_loop`],
//! [`RuntimeHandle::persist_turn_record`], and
//! [`RuntimeHandle::persist_turn_aborted_record`].

mod checkpoint;
mod completion;
mod context_management;
mod execution;
mod projection;
mod reminders;
mod tool_summary;

#[cfg(test)]
mod tests;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::provider::{ModelBlock, ToolResultBlock};
use crate::tool::{spec::ToolResultEnvelope, ToolCall};
use crate::types::{
    AuditEvent, BriefKind, MessageEnvelope, TurnRecord, TurnTerminalKind, TurnTerminalRecord,
    TurnTerminalSummary, TurnTriggerSummary,
};

use super::{message_dispatch::message_text, RuntimeHandle};

use checkpoint::turn_optional_id_matches;
pub(crate) use projection::build_round_estimated_tokens;
#[allow(unused_imports)]
pub(crate) use projection::estimate_tool_specs_tokens;

pub(crate) struct AgentLoopOutcome {
    pub(super) final_text: String,
    pub(super) final_text_source_assistant_round_id: Option<String>,
    pub(super) turn_index: u64,
    pub(super) terminal: TurnTerminalRecord,
    pub(super) should_sleep: bool,
    pub(super) sleep_duration_ms: Option<u64>,
    pub(super) allow_sleep_runnable_work_override: bool,
    pub(super) terminal_kind: TurnTerminalKind,
}


pub(crate) struct LoopControlOptions {
    pub(super) max_tool_rounds: Option<usize>,
}

#[derive(Debug, Clone)]
struct TurnRoundRecord {
    round: usize,
    assistant_blocks: Vec<ModelBlock>,
    text_blocks: Vec<String>,
    tool_calls: Vec<ToolCall>,
    tool_results: Vec<ToolResultBlock>,
    tool_result_envelopes: Vec<ToolResultEnvelope>,
    follow_up_user_texts: Vec<String>,
    estimated_tokens: usize,
}

const MAX_OUTPUT_RECOVERY_ATTEMPTS: usize = 2;
const ROUND_TEXT_PREVIEW_LIMIT: usize = 600;
const RECAP_TEXT_PREVIEW_LIMIT: usize = 160;
const MIN_EXACT_TAIL_ROUNDS: usize = 2;
pub(super) const CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS: usize = 256;
const DEGRADED_ROUND_PROVENANCE_MARKER: &str = "[runtime: last turn content trimmed to fit prompt budget — truncated sections are marked inline]";
const DEGRADED_ROUND_MINIMUM_CONTENT_CHARS: usize = 200;
const WORK_ITEM_STALE_REMINDER_ROUNDS: usize = 10;
const WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS: usize = 10;
const WORK_ITEM_STALE_REMINDER_MAX_TOKENS: usize = 512;
const WORK_ITEM_STALE_REMINDER_PLAN_LINE_LIMIT: usize = 8;
const WORK_ITEM_STALE_REMINDER_PLAN_CHAR_LIMIT: usize = 1_200;
const WORK_ITEM_STALE_REMINDER_TODO_LIMIT: usize = 8;
const TURN_RECORD_SCAN_LIMIT: usize = 4096;
const OPERATOR_INTERJECTION_HEADER: &str =
    "[Operator message received while this turn was in progress]";
const COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT: &str = "\
[Runtime-generated full progress checkpoint request]
You are crossing a context compaction boundary. Before continuing, include a concise progress checkpoint for continuation in your next assistant message.

Include:
- current user goal
- current work item objective, plan_status, plan, and todo_list state
- files, commands, or sources already inspected
- key findings and ruled-out paths
- what remains unknown
- the next goal-aligned action

If continuing exploration, name the specific missing information and the next bounded command/query. If enough evidence already exists to act, make the next action the concrete mutation, verification, or delivery step instead of another read.
If the current todo item became complete through material progress, update the work item after that progress is recorded.
This is not a request to finish the task; after the checkpoint, continue with the next goal-aligned action when useful.
Do not assume the task requires code changes unless the user goal does.";

const DELTA_CHECKPOINT_PREVIEW_LIMIT: usize = 1_200;
const CHECKPOINT_RESUME_PROMPT: &str = "\
[Runtime-generated checkpoint continuation]
Continue from the checkpoint's next goal-aligned action now. Do not restate the checkpoint. If the checkpoint says enough evidence exists to act, call the concrete mutation, verification, or delivery tool next; otherwise run only the named bounded command/query.";

fn truncate_preview(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let mut preview = trimmed.chars().take(limit).collect::<String>();
    if trimmed.chars().count() > limit {
        preview.push_str("...");
    }
    preview
}

fn append_follow_up_user_texts(round: &mut TurnRoundRecord, texts: Vec<String>) {
    if texts.is_empty() {
        return;
    }
    round.follow_up_user_texts.extend(texts);
    round.estimated_tokens = build_round_estimated_tokens(
        &round.assistant_blocks,
        &round.tool_results,
        &round.follow_up_user_texts,
    );
}

fn render_metadata_value<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(label)) => label,
        Ok(Value::Null) => "none".into(),
        Ok(value) => value.to_string(),
        Err(_) => "unavailable".into(),
    }
}

fn render_operator_interjection_text(message: &MessageEnvelope) -> String {
    format!(
        "{OPERATOR_INTERJECTION_HEADER}\nmessage_id={}\norigin={}\nauthority_class={}\ndelivery_surface={}\nadmission_context={}\n\n{}",
        message.id,
        render_metadata_value(&message.origin),
        render_metadata_value(&message.authority_class),
        render_metadata_value(&message.delivery_surface),
        render_metadata_value(&message.admission_context),
        message_text(&message.body).trim(),
    )
}

impl RuntimeHandle {
    pub(super) async fn persist_turn_record(&self, terminal: &TurnTerminalRecord) -> Result<()> {
        let (agent_id, run_id, current_work_item_id) = {
            let guard = self.inner.agent.lock().await;
            (
                guard.state.id.clone(),
                guard.state.current_run_id.clone(),
                guard
                    .state
                    .current_turn_work_item_id
                    .clone()
                    .or_else(|| guard.state.current_work_item_id.clone()),
            )
        };
        let turn_id = terminal.turn_id.trim();
        if turn_id.is_empty() {
            return Ok(());
        }

        let messages = self.inner.storage.read_all_messages()?;
        let briefs = self
            .inner
            .storage
            .read_recent_briefs(TURN_RECORD_SCAN_LIMIT)?;
        let tools = self
            .inner
            .storage
            .read_recent_tool_executions(TURN_RECORD_SCAN_LIMIT)?;
        let wait_conditions = self
            .inner
            .storage
            .read_recent_wait_conditions(TURN_RECORD_SCAN_LIMIT)?;

        let input_messages = messages
            .iter()
            .filter(|message| {
                turn_optional_id_matches(message.turn_id.as_deref(), turn_id)
                    || message.message_seq == Some(terminal.turn_index)
            })
            .collect::<Vec<_>>();

        let mut record = TurnRecord::new(agent_id, turn_id, terminal.turn_index);
        record.run_id = run_id;
        record.current_work_item_id = current_work_item_id;
        record.trigger = input_messages
            .first()
            .map(|message| TurnTriggerSummary::from_message(message));
        record.input_message_ids = input_messages
            .iter()
            .map(|message| message.id.clone())
            .collect();
        record.tool_execution_ids = tools
            .iter()
            .filter(|tool| {
                turn_optional_id_matches(tool.turn_id.as_deref(), turn_id)
                    || tool.turn_index == terminal.turn_index
            })
            .map(|tool| tool.id.clone())
            .collect();
        record.produced_brief_ids = briefs
            .iter()
            .filter(|brief| {
                turn_optional_id_matches(brief.turn_id.as_deref(), turn_id)
                    || brief.turn_index == Some(terminal.turn_index)
            })
            .map(|brief| brief.id.clone())
            .collect();
        record.completed_work_item_ids = briefs
            .iter()
            .filter(|brief| {
                brief.kind == BriefKind::Result
                    && (turn_optional_id_matches(brief.turn_id.as_deref(), turn_id)
                        || brief.turn_index == Some(terminal.turn_index))
            })
            .filter_map(|brief| brief.work_item_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        record.waiting_condition_ids = wait_conditions
            .iter()
            .filter(|condition| turn_optional_id_matches(condition.turn_id.as_deref(), turn_id))
            .map(|condition| condition.id.clone())
            .collect();
        record.terminal = Some(TurnTerminalSummary::from_terminal(terminal));

        self.inner.storage.append_turn(&record)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_record",
            serde_json::json!({
                "turn_id": record.turn_id,
                "turn_index": record.turn_index,
                "agent_id": record.agent_id,
                "run_id": record.run_id,
                "current_work_item_id": record.current_work_item_id,
                "tool_execution_ids": record.tool_execution_ids,
                "produced_brief_ids": record.produced_brief_ids,
                "completed_work_item_ids": record.completed_work_item_ids,
                "waiting_condition_ids": record.waiting_condition_ids,
                "terminal": record.terminal,
                "created_at": record.created_at,
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn persist_turn_aborted_record(
        &self,
        run_id: &str,
        reason: &str,
        last_assistant_message: Option<String>,
        duration_ms: u64,
    ) -> Result<TurnTerminalRecord> {
        let record = {
            let mut guard = self.inner.agent.lock().await;
            let turn_id = guard
                .state
                .current_turn_id
                .clone()
                .filter(|turn_id| !turn_id.trim().is_empty())
                .unwrap_or_else(crate::ids::turn_id);
            guard.state.current_turn_id = Some(turn_id.clone());
            let record = TurnTerminalRecord {
                turn_id,
                turn_index: guard.state.turn_index,
                kind: TurnTerminalKind::Aborted,
                reason: Some(reason.to_string()),
                last_assistant_message,
                checkpoint: None,
                completed_at: chrono::Utc::now(),
                duration_ms,
            };
            guard.state.last_turn_terminal = Some(record.clone());
            guard.persist_state(&self.inner.storage)?;
            record
        };
        self.persist_turn_record(&record).await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal",
            serde_json::to_value(&record)?,
        ))?;
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal_aborted",
            serde_json::json!({
                "run_id": run_id,
                "reason": reason,
                "turn_id": record.turn_id.clone(),
                "turn_index": record.turn_index,
                "kind": record.kind,
                "completed_at": record.completed_at,
                "duration_ms": record.duration_ms,
            }),
        ))?;
        Ok(record)
    }
}
