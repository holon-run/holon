//! Project captured history without storage or publication capabilities.
use std::cmp::Ordering;
use std::collections::BTreeSet;

use holon_domain::{
    command_output_source_ref, command_receipt_source_ref, provider_recovery_source_turn_id,
    AuthorityClass, BriefRecord, ContinuationTriggerKind, MessageEnvelope, MessageKind,
    MessageOrigin, ToolContextProjection, ToolExecutionStatus, TranscriptEntry,
    TranscriptEntryKind, TurnRecord, WorkItemRecord,
};
use holon_system::utf8::truncate_text;
use serde_json::Value;

use crate::budget::{estimate_text_tokens, truncate_section_content};
use crate::render::{
    body_preview, bounded_inline, indent_block, message_body_text, sanitize_inline,
};

/// Facts that have already been qualified and captured by the runtime.
pub trait TurnHistoryObservations {
    fn message_belongs_to_turn(&self, turn: &TurnRecord, id: &str) -> bool;
    fn brief_text<'a>(&'a self, brief: &'a BriefRecord) -> &'a str;
    fn trigger_summary_valid(&self, turn: &TurnRecord) -> bool;
}

pub fn history_turns_without_current_input(
    mut records: Vec<TurnRecord>,
    message_id: &str,
    limit: usize,
) -> Vec<TurnRecord> {
    // Active turns are now durable. Current input and in-flight rounds already
    // have dedicated prompt sections; including them as history duplicates them.
    records.retain(|record| {
        record.terminal.is_some() || !record.input_message_ids.iter().any(|id| id == message_id)
    });
    records.drain(..records.len().saturating_sub(limit));
    records
}

fn render_recent_turn_input_line(
    message: &MessageEnvelope,
    mode: RecentTurnProjectionMode,
) -> String {
    let message_ref = format!("message:{}", sanitize_inline(&message.id));
    if is_trusted_operator_input(message) {
        let body = message_body_text(&message.body);
        let preview_limit = match mode {
            RecentTurnProjectionMode::Continuity => usize::MAX,
            RecentTurnProjectionMode::Nearby => 360,
            RecentTurnProjectionMode::Older => 180,
        };
        let sanitized = sanitize_inline(&body);
        if matches!(mode, RecentTurnProjectionMode::Continuity)
            || sanitized.chars().count() <= preview_limit
        {
            format!("  - operator input full: {sanitized} message_ref={message_ref}")
        } else {
            let preview = truncate_text(&sanitized, preview_limit);
            format!(
                "  - operator input preview: {preview} [truncated; full via message_ref={message_ref}]"
            )
        }
    } else if matches!(mode, RecentTurnProjectionMode::Continuity)
        && provider_recovery_source_turn_id(message).is_some()
    {
        format!(
            "  - input full: {} message_ref={message_ref}",
            sanitize_inline(&message_body_text(&message.body))
        )
    } else {
        render_recent_turn_runtime_input_line(message, &message_ref).unwrap_or_else(|| {
            format!(
                "  - input: {} message_ref={}",
                sanitize_inline(&body_preview(&message.body)),
                message_ref
            )
        })
    }
}

fn render_recent_turn_runtime_input_line(
    message: &MessageEnvelope,
    message_ref: &str,
) -> Option<String> {
    if message.kind != MessageKind::SystemTick {
        return None;
    }

    let metadata = message.metadata.as_ref();
    if let Some(wake_hint) = metadata.and_then(|metadata| metadata.get("wake_hint")) {
        let reason = wake_hint
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let source = wake_hint
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let resource = wake_hint
            .get("resource")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("none");
        let work_item = wake_hint
            .get("work_item_id")
            .and_then(serde_json::Value::as_str)
            .map(|id| format!(" work_item={}", bounded_inline(id, 80)))
            .unwrap_or_default();
        return Some(format!(
            "  - input summary: wake hint source={} reason={} resource={}{} message_ref={}",
            bounded_inline(source, 80),
            bounded_inline(reason, 120),
            bounded_inline(resource, 160),
            work_item,
            message_ref
        ));
    }

    if let Some(work_queue) = metadata.and_then(|metadata| metadata.get("work_queue")) {
        let reason = work_queue
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let work_item_id = work_queue
            .get("work_item_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let objective = work_queue
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        return Some(format!(
            "  - input summary: work queue tick reason={} work_item={} objective={} message_ref={}",
            bounded_inline(reason, 120),
            bounded_inline(work_item_id, 80),
            bounded_inline(objective, 160),
            message_ref
        ));
    }

    if let Some(recheck) = metadata.and_then(|metadata| metadata.get("work_item_recheck")) {
        let count = recheck
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let targets = recheck
            .get("items")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let work_item_id = item
                            .get("work_item_id")
                            .and_then(serde_json::Value::as_str)?;
                        let blocked_by = item
                            .get("blocked_by")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unset");
                        let recheck_at = item
                            .get("recheck_at")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unscheduled");
                        Some(format!(
                            "work_item={} objective={} blocked_by={} recheck_at={}",
                            bounded_inline(work_item_id, 80),
                            bounded_inline(
                                item.get("objective")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("unknown"),
                                160,
                            ),
                            bounded_inline(blocked_by, 120),
                            bounded_inline(recheck_at, 40),
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        return Some(format!(
            "  - input summary: work item recheck count={} targets=[{}] message_ref={}",
            count,
            bounded_inline(&targets, 800),
            message_ref
        ));
    }

    let subsystem = match &message.origin {
        MessageOrigin::System { subsystem } => bounded_inline(subsystem, 80),
        _ => "unknown".to_string(),
    };
    Some(format!(
        "  - input summary: {} subsystem={} message_ref={}",
        runtime_continuation_label(message),
        subsystem,
        message_ref
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecentTurnProjectionMode {
    Continuity,
    Nearby,
    Older,
}

fn render_recent_turn_brief_line_from_snapshot(
    observations: &dyn TurnHistoryObservations,
    brief: &BriefRecord,
    mode: RecentTurnProjectionMode,
    budget: usize,
) -> Option<String> {
    if brief.kind == holon_domain::BriefKind::Ack {
        return None;
    }

    let brief_ref = format!("brief:{}", sanitize_inline(&brief.id));
    let brief_text = observations.brief_text(brief);
    match mode {
        RecentTurnProjectionMode::Continuity => {
            render_continuity_brief_line(brief, &brief_text, &brief_ref, budget)
        }
        RecentTurnProjectionMode::Nearby => Some(format!(
            "    - {:?}: {} brief_ref={}",
            brief.kind,
            sanitize_inline(&truncate_text(&brief_text.replace('\n', " "), 1200)),
            brief_ref
        )),
        RecentTurnProjectionMode::Older => Some(format!(
            "    - {:?}: {} brief_ref={}",
            brief.kind,
            sanitize_inline(&truncate_text(&brief_text.replace('\n', " "), 240)),
            brief_ref
        )),
    }
}

fn render_continuity_brief_line(
    brief: &BriefRecord,
    brief_text: &str,
    brief_ref: &str,
    budget: usize,
) -> Option<String> {
    let full = format!(
        "    - {:?} full:\n{}\n      brief_ref={}",
        brief.kind,
        indent_block(brief_text, 6),
        brief_ref
    );
    if estimate_text_tokens(&full) <= budget {
        return Some(full);
    }

    let prefix = format!("    - {:?} excerpt:\n", brief.kind);
    let notice = format!("\n      [truncated; full via brief_ref={}]", brief_ref);
    let rendered = truncate_section_content(
        &prefix,
        &indent_block(brief_text, 6),
        budget.max(64),
        Some(&notice),
    );
    if rendered.contains("brief_ref=") {
        Some(rendered)
    } else {
        Some(format!(
            "    - {:?} excerpt: [truncated; full via brief_ref={}]",
            brief.kind, brief_ref
        ))
    }
}

pub fn latest_trusted_operator_input<'a>(
    messages: &'a [impl std::borrow::Borrow<MessageEnvelope>],
    current_message: &'a MessageEnvelope,
) -> Option<&'a MessageEnvelope> {
    if is_trusted_operator_input(current_message) {
        return Some(current_message);
    }

    messages
        .iter()
        .map(std::borrow::Borrow::<MessageEnvelope>::borrow)
        .filter(|message| is_trusted_operator_input(message) && message.message_seq.is_some())
        .max_by(|left, right| compare_message_recency(left, right))
}

pub fn is_trusted_operator_input(message: &MessageEnvelope) -> bool {
    message.authority_class == AuthorityClass::OperatorInstruction
        && matches!(message.origin, MessageOrigin::Operator { .. })
}

pub fn compare_message_recency(left: &MessageEnvelope, right: &MessageEnvelope) -> Ordering {
    left.message_seq
        .cmp(&right.message_seq)
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

pub fn runtime_continuation_label(message: &MessageEnvelope) -> &'static str {
    match message.trigger_kind {
        Some(ContinuationTriggerKind::TaskResult) => "a task-result continuation",
        Some(ContinuationTriggerKind::ExternalEvent) => "an external-event continuation",
        Some(ContinuationTriggerKind::TimerFire) => "a timer continuation",
        Some(ContinuationTriggerKind::InternalFollowup) => "an internal-followup continuation",
        Some(ContinuationTriggerKind::SystemTick) => "a runtime system-tick continuation",
        Some(ContinuationTriggerKind::OperatorInput) => "operator-triggered input",
        None => match message.kind {
            MessageKind::TaskResult | MessageKind::TaskStatus => "a task-result continuation",
            MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent => {
                "an external-event continuation"
            }
            MessageKind::TimerTick => "a timer continuation",
            MessageKind::InternalFollowup => "an internal-followup continuation",
            MessageKind::SystemTick => "a runtime system-tick continuation",
            _ => "runtime-originated input",
        },
    }
}

pub fn render_recent_turns_with_budget_from_snapshot(
    observations: &dyn TurnHistoryObservations,
    turn_records: &[impl std::borrow::Borrow<TurnRecord>],
    messages: &[impl std::borrow::Borrow<MessageEnvelope>],
    briefs: &[impl std::borrow::Borrow<BriefRecord>],
    tools: &[impl std::borrow::Borrow<ToolContextProjection>],
    transcript: &[impl std::borrow::Borrow<TranscriptEntry>],
    current_message: &MessageEnvelope,
    current_work_item: Option<&WorkItemRecord>,
    budget: usize,
) -> Option<String> {
    render_turn_records_with_budget_from_snapshot(
        observations,
        turn_records,
        messages,
        briefs,
        tools,
        transcript,
        current_message,
        current_work_item,
        budget,
    )
}

pub fn render_turn_records_with_budget_from_snapshot(
    observations: &dyn TurnHistoryObservations,
    turn_records: &[impl std::borrow::Borrow<TurnRecord>],
    messages: &[impl std::borrow::Borrow<MessageEnvelope>],
    briefs: &[impl std::borrow::Borrow<BriefRecord>],
    tools: &[impl std::borrow::Borrow<ToolContextProjection>],
    transcript: &[impl std::borrow::Borrow<TranscriptEntry>],
    current_message: &MessageEnvelope,
    current_work_item: Option<&WorkItemRecord>,
    budget: usize,
) -> Option<String> {
    if turn_records.is_empty() {
        return None;
    }

    let recovery_turn_id = provider_recovery_source_turn_id(current_message);
    let latest_operator_for_continuation = (recovery_turn_id.is_none()
        && !is_trusted_operator_input(current_message))
    .then(|| latest_trusted_operator_input(messages, current_message))
    .flatten();
    let continuation_turn_id = latest_operator_for_continuation
        .and_then(|operator| {
            turn_records
                .iter()
                .map(std::borrow::Borrow::<TurnRecord>::borrow)
                .find(|record| observations.message_belongs_to_turn(record, &operator.id))
        })
        .map(|record| record.turn_id.clone());
    let latest_previous_turn_id = turn_records
        .iter()
        .map(std::borrow::Borrow::<TurnRecord>::borrow)
        .rev()
        .map(|record| record.turn_id.clone())
        .next();
    let continuity_turn_id = recovery_turn_id.map(str::to_owned).or_else(|| {
        continuation_turn_id
            .clone()
            .or_else(|| latest_previous_turn_id.clone())
    });
    let nearby_turn_ids = turn_records
        .iter()
        .map(std::borrow::Borrow::<TurnRecord>::borrow)
        .rev()
        .filter(|record| continuity_turn_id.as_deref() != Some(record.turn_id.as_str()))
        .take(2)
        .map(|record| record.turn_id.as_str())
        .collect::<Vec<_>>();

    let mut rendered_turns = turn_records
        .iter()
        .map(std::borrow::Borrow::<TurnRecord>::borrow)
        .filter(|record| continuation_turn_id.as_deref() != Some(record.turn_id.as_str()))
        .filter_map(|record| {
            let mode = recent_turn_projection_mode(
                record,
                continuity_turn_id.as_deref(),
                &nearby_turn_ids,
            );
            render_turn_record_projection_with_transcript_from_snapshot(
                observations,
                record,
                messages,
                briefs,
                tools,
                transcript,
                None,
                mode,
                budget,
            )
        })
        .collect::<Vec<_>>();

    if let Some(operator) = latest_operator_for_continuation {
        if let Some(record) = turn_records
            .iter()
            .map(std::borrow::Borrow::<TurnRecord>::borrow)
            .find(|record| observations.message_belongs_to_turn(record, &operator.id))
        {
            if let Some(rendered) = render_current_continuation_turn_record_projection_from_snapshot(
                observations,
                record,
                current_message,
                operator,
                messages,
                briefs,
                tools,
                transcript,
                current_work_item,
                budget,
            ) {
                rendered_turns.push(rendered);
            }
        }
    }

    if rendered_turns.is_empty() {
        return None;
    }

    render_budgeted_recent_turns("Recent turns:", rendered_turns, budget)
}

fn recent_turn_projection_mode(
    record: &TurnRecord,
    continuity_turn_id: Option<&str>,
    nearby_turn_ids: &[&str],
) -> RecentTurnProjectionMode {
    if continuity_turn_id == Some(record.turn_id.as_str()) {
        RecentTurnProjectionMode::Continuity
    } else if nearby_turn_ids.contains(&record.turn_id.as_str()) {
        RecentTurnProjectionMode::Nearby
    } else {
        RecentTurnProjectionMode::Older
    }
}

pub fn render_turn_record_projection_with_transcript_from_snapshot(
    observations: &dyn TurnHistoryObservations,
    record: &TurnRecord,
    messages: &[impl std::borrow::Borrow<MessageEnvelope>],
    briefs: &[impl std::borrow::Borrow<BriefRecord>],
    tools: &[impl std::borrow::Borrow<ToolContextProjection>],
    transcript: &[impl std::borrow::Borrow<TranscriptEntry>],
    continuation: Option<&MessageEnvelope>,
    mode: RecentTurnProjectionMode,
    budget: usize,
) -> Option<String> {
    let trigger_message_id = record
        .trigger
        .as_ref()
        .and_then(|trigger| trigger.message_id.as_ref());
    let find_owned_message = |id: &String| {
        messages
            .iter()
            .map(std::borrow::Borrow::<MessageEnvelope>::borrow)
            .find(|message| {
                message.id == *id && observations.message_belongs_to_turn(record, &message.id)
            })
    };
    let trigger_message = match trigger_message_id {
        Some(id) => find_owned_message(id),
        // Legacy records lack an explicit anchor; live followers are never triggers.
        None => record
            .input_message_ids
            .iter()
            .filter(|id| {
                !record
                    .live_input_batches
                    .iter()
                    .flat_map(|batch| &batch.items)
                    .any(|item| item.message_id == **id)
            })
            .find_map(find_owned_message),
    };
    let mut lines = vec![format!(
        "- Turn {}:",
        if record.turn_index != 0 {
            format!("turn_index {}", record.turn_index)
        } else {
            record.turn_id.clone()
        }
    )];
    lines.push(format!("  - turn_id: {}", sanitize_inline(&record.turn_id)));
    if let Some(replay) = &record.replay {
        lines.push(format!(
            "  - replay: source_message_id={} source_turn_id={} reason={}",
            sanitize_inline(&replay.source_message_id),
            sanitize_inline(&replay.source_turn_id),
            sanitize_inline(&replay.reason)
        ));
        lines.push(
            "  - projection advisory: replay provenance is historical evidence, not continuation identity"
                .to_string(),
        );
    }
    if let Some(trigger_message) = trigger_message {
        lines.push(format!(
            "  - trigger: {}",
            turn_trigger_label(trigger_message)
        ));
    } else if observations.trigger_summary_valid(record) {
        if let Some(trigger) = &record.trigger {
            lines.push(format!(
                "  - trigger: {}",
                turn_trigger_summary_label(trigger)
            ));
        } else {
            lines.push("  - trigger: unavailable".to_string());
        }
    } else {
        lines.push("  - trigger: unavailable".to_string());
    }
    if let Some(continuation) = continuation {
        lines.push(format!(
            "  - continues input: {}",
            trigger_message
                .and_then(|message| message.message_seq.map(|seq| format!("message_seq {seq}")))
                .or_else(|| {
                    trigger_message
                        .map(|message| message.id.clone())
                        .or_else(|| {
                            record
                                .trigger
                                .as_ref()
                                .and_then(|trigger| trigger.message_id.clone())
                        })
                })
                .unwrap_or_else(|| record.turn_id.clone())
        ));
        lines.push(format!(
            "  - continuation trigger: {}",
            turn_trigger_label(continuation)
        ));
    }
    if let Some(trigger_message) = trigger_message {
        lines.push(render_recent_turn_input_line(trigger_message, mode));
    }

    let (execution_sequence, has_execution_sequence) =
        render_turn_execution_sequence(record, briefs, tools, transcript, mode);
    if has_execution_sequence {
        lines.push("  - execution sequence:".to_string());
        lines.extend(execution_sequence);
    }

    let mut related_briefs = Vec::new();
    let mut brief_budget = budget.saturating_sub(estimate_text_tokens(&lines.join("\n")));
    for brief in record.produced_brief_ids.iter().filter_map(|id| {
        briefs
            .iter()
            .map(std::borrow::Borrow::<BriefRecord>::borrow)
            .find(|brief| {
                brief.id == *id && turn_record_owns_object(record, brief.turn_id.as_deref())
            })
    }) {
        if let Some(rendered) =
            render_recent_turn_brief_line_from_snapshot(observations, brief, mode, brief_budget)
        {
            brief_budget = brief_budget.saturating_sub(estimate_text_tokens(&rendered));
            related_briefs.push(rendered);
        }
    }
    if !related_briefs.is_empty() {
        if !has_execution_sequence {
            lines.push("  - produced briefs:".to_string());
            lines.extend(related_briefs);
        } else {
            let sequence_brief_ids = transcript
                .iter()
                .map(std::borrow::Borrow::<TranscriptEntry>::borrow)
                .filter(|entry| {
                    entry.agent_id == record.agent_id
                        && entry
                            .data
                            .get("turn_id")
                            .and_then(Value::as_str)
                            .is_some_and(|turn_id| turn_id == record.turn_id)
                        && is_projectable_assistant_round(entry)
                })
                .filter_map(|entry| {
                    briefs
                        .iter()
                        .map(std::borrow::Borrow::<BriefRecord>::borrow)
                        .find(|brief| {
                            brief.finalizes_assistant_round_id.as_deref() == Some(entry.id.as_str())
                                && turn_record_owns_object(record, brief.turn_id.as_deref())
                        })
                        .map(|brief| brief.id.as_str())
                })
                .collect::<BTreeSet<_>>();
            let fallback_refs = record
                .produced_brief_ids
                .iter()
                .filter_map(|id| {
                    briefs
                        .iter()
                        .map(std::borrow::Borrow::<BriefRecord>::borrow)
                        .find(|brief| &brief.id == id)
                })
                .filter(|brief| {
                    turn_record_owns_object(record, brief.turn_id.as_deref())
                        && !sequence_brief_ids.contains(brief.id.as_str())
                })
                .map(|brief| {
                    format!(
                        "    - result fallback: brief_ref=brief:{}",
                        sanitize_inline(&brief.id)
                    )
                })
                .collect::<Vec<_>>();
            if !fallback_refs.is_empty() {
                lines.push("  - retrieval fallbacks:".to_string());
                lines.extend(fallback_refs);
            }
        }
    }

    let related_tools = record
        .tool_execution_ids
        .iter()
        .filter_map(|id| {
            tools
                .iter()
                .map(std::borrow::Borrow::<ToolContextProjection>::borrow)
                .find(|tool| {
                    tool.id == *id && turn_record_owns_object(record, tool.turn_id.as_deref())
                })
        })
        .collect::<Vec<_>>();
    if !related_tools.is_empty() && !has_execution_sequence {
        lines.push("  - tool executions:".to_string());
        lines.extend(
            render_recent_tool_execution_rollup(&related_tools)
                .into_iter()
                .map(|tool| format!("    {tool}")),
        );
    }

    Some(lines.join("\n"))
}

pub fn render_turn_execution_sequence(
    record: &TurnRecord,
    briefs: &[impl std::borrow::Borrow<BriefRecord>],
    tools: &[impl std::borrow::Borrow<ToolContextProjection>],
    transcript: &[impl std::borrow::Borrow<TranscriptEntry>],
    mode: RecentTurnProjectionMode,
) -> (Vec<String>, bool) {
    let mut rendered = Vec::new();
    let mut rendered_tool_ids = BTreeSet::new();
    for entry in transcript
        .iter()
        .map(std::borrow::Borrow::<TranscriptEntry>::borrow)
        .filter(|entry| {
            entry.agent_id == record.agent_id
                && entry
                    .data
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .is_some_and(|turn_id| turn_id == record.turn_id)
        })
    {
        match entry.kind {
            TranscriptEntryKind::AssistantRound if is_projectable_assistant_round(&entry) => {
                if let Some(text) = assistant_round_text_preview(&entry) {
                    let limit = match mode {
                        RecentTurnProjectionMode::Continuity => 1200,
                        RecentTurnProjectionMode::Nearby => 480,
                        RecentTurnProjectionMode::Older => 240,
                    };
                    let brief_ref = briefs
                        .iter()
                        .map(std::borrow::Borrow::<BriefRecord>::borrow)
                        .find(|brief| {
                            brief.finalizes_assistant_round_id.as_deref() == Some(entry.id.as_str())
                                && turn_record_owns_object(record, brief.turn_id.as_deref())
                        })
                        .map(|brief| format!(" brief_ref=brief:{}", sanitize_inline(&brief.id)))
                        .unwrap_or_default();
                    rendered.push(format!(
                        "    - assistant: {}{}",
                        sanitize_inline(&truncate_text(&text.replace('\n', " "), limit)),
                        brief_ref
                    ));
                }
            }
            TranscriptEntryKind::ToolResults => {
                let limit = match mode {
                    RecentTurnProjectionMode::Continuity => 480,
                    RecentTurnProjectionMode::Nearby => 240,
                    RecentTurnProjectionMode::Older => 120,
                };
                let recovered = entry.data["recovered_exec_command_result"] == Value::Bool(true);
                let refs = entry.data.get("refs").and_then(Value::as_array);
                for reference in refs.into_iter().flatten() {
                    let Some(tool_id) = reference["tool_execution_id"]
                        .as_str()
                        .filter(|id| !id.trim().is_empty())
                    else {
                        continue;
                    };
                    let Some(tool) = tools
                        .iter()
                        .map(std::borrow::Borrow::<ToolContextProjection>::borrow)
                        .find(|tool| {
                            tool.id == tool_id
                                && turn_record_owns_object(record, tool.turn_id.as_deref())
                        })
                    else {
                        continue;
                    };
                    if !rendered_tool_ids.insert(tool.id.as_str()) {
                        continue;
                    }
                    if recovered {
                        if let Some(content) = reference["provider_visible_text"].as_str() {
                            rendered.push(format!(
                                "    - tool: {} result={} tool_execution_id={}{}",
                                sanitize_inline(&tool.tool_name),
                                sanitize_inline(&truncate_text(content, limit)),
                                sanitize_inline(&tool.id),
                                command_output_refs(tool),
                            ));
                            continue;
                        }
                    }
                    rendered.push(format!(
                        "    - tool: {} summary={} tool_execution_id={}",
                        sanitize_inline(&tool.tool_name),
                        sanitize_inline(&truncate_text(&tool.summary.replace('\n', " "), limit)),
                        sanitize_inline(&tool.id)
                    ));
                }
            }
            _ => {}
        }
    }
    // This flag describes transcript-backed entries only; unsequenced tools
    // appended below are deliberately rendered as a fallback.
    let has_execution_sequence = !rendered.is_empty();
    if has_execution_sequence {
        let unsequenced_tools = record
            .tool_execution_ids
            .iter()
            .filter_map(|id| {
                tools
                    .iter()
                    .map(std::borrow::Borrow::<ToolContextProjection>::borrow)
                    .find(|tool| {
                        tool.id == *id
                            && turn_record_owns_object(record, tool.turn_id.as_deref())
                            && !rendered_tool_ids.contains(tool.id.as_str())
                    })
            })
            .collect::<Vec<_>>();
        rendered.extend(unsequenced_tools.into_iter().map(|tool| {
            let limit = match mode {
                RecentTurnProjectionMode::Continuity => 480,
                RecentTurnProjectionMode::Nearby => 240,
                RecentTurnProjectionMode::Older => 120,
            };
            format!(
                "    - tool (unsequenced): {} summary={} tool_execution_id={}",
                sanitize_inline(&tool.tool_name),
                sanitize_inline(&truncate_text(&tool.summary.replace('\n', " "), limit)),
                sanitize_inline(&tool.id)
            )
        }));
    }
    (rendered, has_execution_sequence)
}

fn render_current_continuation_turn_record_projection_from_snapshot(
    observations: &dyn TurnHistoryObservations,
    record: &TurnRecord,
    current_message: &MessageEnvelope,
    operator: &MessageEnvelope,
    messages: &[impl std::borrow::Borrow<MessageEnvelope>],
    briefs: &[impl std::borrow::Borrow<BriefRecord>],
    tools: &[impl std::borrow::Borrow<ToolContextProjection>],
    transcript: &[impl std::borrow::Borrow<TranscriptEntry>],
    current_work_item: Option<&WorkItemRecord>,
    budget: usize,
) -> Option<String> {
    let mut rendered = render_turn_record_projection_with_transcript_from_snapshot(
        observations,
        record,
        messages,
        briefs,
        tools,
        transcript,
        Some(current_message),
        RecentTurnProjectionMode::Continuity,
        budget,
    )?;
    let mut lines = vec![
        format!(
            "  - current relation: {}",
            runtime_continuation_label(current_message)
        ),
        format!(
            "  - current input: {}",
            sanitize_inline(&body_preview(&current_message.body))
        ),
    ];
    if let Some(work_item) = current_work_item {
        lines.push(format!(
            "  - current work item: {} :: {}",
            sanitize_inline(&work_item.id),
            sanitize_inline(&truncate_text(&work_item.objective, 160))
        ));
    }
    if !observations.message_belongs_to_turn(record, &operator.id) {
        lines.push(format!(
            "  - continues input id: {}",
            sanitize_inline(&operator.id)
        ));
    }
    rendered.push('\n');
    rendered.push_str(&lines.join("\n"));
    Some(rendered)
}

pub fn turn_record_owns_object(record: &TurnRecord, object_turn_id: Option<&str>) -> bool {
    let record_turn_id = record.turn_id.trim();
    object_turn_id.is_some_and(|object_turn_id| {
        let object_turn_id = object_turn_id.trim();
        !record_turn_id.is_empty() && !object_turn_id.is_empty() && object_turn_id == record_turn_id
    })
}

fn turn_trigger_label(message: &MessageEnvelope) -> &'static str {
    if is_trusted_operator_input(message) {
        return "trusted operator input";
    }
    runtime_continuation_label(message)
}

fn turn_trigger_summary_label(trigger: &holon_domain::TurnTriggerSummary) -> &'static str {
    if matches!(trigger.authority_class, AuthorityClass::OperatorInstruction) {
        return "trusted operator input";
    }
    match trigger.trigger_kind {
        Some(ContinuationTriggerKind::TaskResult) => "a task-result continuation",
        Some(ContinuationTriggerKind::ExternalEvent) => "an external-event continuation",
        Some(ContinuationTriggerKind::TimerFire) => "a timer continuation",
        Some(ContinuationTriggerKind::InternalFollowup) => "an internal-followup continuation",
        Some(ContinuationTriggerKind::SystemTick) => "a runtime system-tick continuation",
        Some(ContinuationTriggerKind::OperatorInput) => "operator-triggered input",
        None => match trigger.kind {
            MessageKind::TaskResult | MessageKind::TaskStatus => "a task-result continuation",
            MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent => {
                "an external-event continuation"
            }
            MessageKind::TimerTick => "a timer continuation",
            MessageKind::InternalFollowup => "an internal-followup continuation",
            MessageKind::SystemTick => "a runtime system-tick continuation",
            _ => "runtime-originated input",
        },
    }
}

fn command_output_refs(record: &ToolContextProjection) -> String {
    if record.tool_name != "ExecCommand" || !record.command.has_output_evidence {
        return String::new();
    }
    format!(
        " stdout_ref={} stderr_ref={} output_ref={}",
        command_output_source_ref(&record.id, None, "stdout"),
        command_output_source_ref(&record.id, None, "stderr"),
        command_output_source_ref(&record.id, None, "output")
    )
}

fn tool_execution_rollup_ref(record: &ToolContextProjection) -> String {
    command_output_source_ref(&record.id, None, "output")
}

pub fn status_label(status: &ToolExecutionStatus) -> &'static str {
    match status {
        ToolExecutionStatus::Deferred => "deferred",
        ToolExecutionStatus::Success => "success",
        ToolExecutionStatus::Error => "error",
        ToolExecutionStatus::Interrupted => "interrupted",
        ToolExecutionStatus::Pending => "pending",
    }
}

fn command_disposition(record: &ToolContextProjection) -> Option<&str> {
    record.command.disposition.as_deref()
}

pub fn command_task_id(record: &ToolContextProjection) -> Option<&str> {
    record.command.task_id.as_deref()
}

fn command_exit_status(record: &ToolContextProjection) -> Option<i64> {
    record.command.exit_status
}

fn command_has_nonzero_exit_status(record: &ToolContextProjection) -> bool {
    command_exit_status(record).is_some_and(|status| status != 0)
}

fn tool_execution_needs_old_turn_alert(record: &ToolContextProjection) -> bool {
    matches!(record.status, ToolExecutionStatus::Error)
        || command_has_nonzero_exit_status(record)
        || (record.tool_name == "ExecCommandBatch" && record.batch_failed > 0)
        || matches!(
            command_disposition(record),
            Some("promoted_to_task" | "already_running")
        )
        || record.command.truncated
        || record.command.has_artifact
}

fn tool_execution_needs_old_turn_failure_count(record: &ToolContextProjection) -> bool {
    matches!(record.status, ToolExecutionStatus::Error)
        || command_has_nonzero_exit_status(record)
        || (record.tool_name == "ExecCommandBatch" && record.batch_failed > 0)
}

pub fn render_recent_tool_execution_rollup(records: &[&ToolContextProjection]) -> Vec<String> {
    let total = records.len();
    let success = records
        .iter()
        .filter(|record| !tool_execution_needs_old_turn_failure_count(record))
        .count();
    let error = records
        .iter()
        .filter(|record| tool_execution_needs_old_turn_failure_count(record))
        .count();
    let promoted = records
        .iter()
        .filter(|record| {
            matches!(
                command_disposition(record),
                Some("promoted_to_task" | "already_running")
            )
        })
        .count();
    let mut refs = records
        .iter()
        .take(6)
        .map(|record| tool_execution_rollup_ref(record))
        .collect::<Vec<_>>();
    if records.len() > refs.len() {
        refs.push(format!("+{} more", records.len() - refs.len()));
    }
    let mut lines = vec![format!(
        "- summary: total={total} success={success} error={error} promoted={promoted} refs=[{}]",
        refs.join(", ")
    )];
    lines.extend(
        records
            .iter()
            .filter(|record| tool_execution_needs_old_turn_alert(record))
            .map(|record| render_recent_tool_execution_alert(record)),
    );
    lines
}

fn render_recent_tool_execution_alert(record: &ToolContextProjection) -> String {
    let mut parts = vec![
        format!(
            "- alert: {} status={} tool_execution_id={}",
            record.tool_name,
            status_label(&record.status),
            sanitize_inline(&record.id)
        ),
        format!(
            "summary={}",
            sanitize_inline(&truncate_text(&record.summary, 160))
        ),
    ];
    if let Some(disposition) = command_disposition(record) {
        parts.push(format!("disposition={disposition}"));
    }
    if let Some(task_id) = command_task_id(record) {
        parts.push(format!("task_id={}", sanitize_inline(task_id)));
    }
    if let Some(exit_status) = command_exit_status(record) {
        parts.push(format!("exit_status={exit_status}"));
    }
    if record.tool_name == "ExecCommand" {
        parts.push(format!(
            "cmd_ref={}",
            command_receipt_source_ref(&record.id, None)
        ));
        let output_refs = command_output_refs(record);
        if !output_refs.is_empty() {
            parts.push(output_refs.trim().to_string());
        }
        if let Some(preview) = record.command.command_preview.as_deref() {
            parts.push(format!("cmd_preview={preview}"));
        }
    }
    parts.join(" ")
}

pub fn render_budgeted_recent_turns(
    heading: &str,
    turns: Vec<String>,
    budget: usize,
) -> Option<String> {
    if turns.is_empty() {
        return None;
    }

    let mut selected = Vec::new();
    let mut used = estimate_text_tokens(heading) + 1;
    for turn in turns.into_iter().rev() {
        let cost = estimate_text_tokens(&turn);
        if used + cost > budget {
            if selected.is_empty() {
                let remaining = budget.saturating_sub(used);
                let truncated = truncate_section_content(
                    "",
                    &turn,
                    remaining,
                    Some("\n[truncated recent turn; use visible refs for full evidence]"),
                );
                if !truncated.trim().is_empty() {
                    selected.push(truncated);
                }
            }
            break;
        }
        used += cost;
        selected.push(turn);
    }

    if selected.is_empty() {
        return None;
    }

    selected.reverse();
    Some(format!("{heading}\n{}", selected.join("\n")))
}

pub fn assistant_round_text_preview(entry: &TranscriptEntry) -> Option<String> {
    let text = entry
        .data
        .get("blocks")?
        .as_array()?
        .iter()
        .filter_map(|block| {
            if let Some(kind) = block.get("type").and_then(serde_json::Value::as_str) {
                if kind != "text" {
                    return None;
                }
            }
            block
                .get("Text")
                .and_then(|value| value.get("text"))
                .or_else(|| block.get("text"))
                .and_then(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

fn is_projectable_assistant_round(entry: &TranscriptEntry) -> bool {
    entry.kind == TranscriptEntryKind::AssistantRound
        && entry.data.get("visibility").and_then(Value::as_str) != Some("runtime_private")
        && entry.data.get("round_purpose").and_then(Value::as_str) != Some("runtime_checkpoint")
        && assistant_round_text_preview(entry).is_some()
}
