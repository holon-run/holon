//! Memory documents projected from already acquired source records.
//!
//! The caller resolves source content and supplies its command preview formatter
//! so tool receipts and indexed previews use the same redaction policy.

use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Utc};
use holon_domain::{
    command_digest, command_output_source_ref, command_receipt_source_ref, BriefRecord,
    CommandTaskStatusSnapshot, ContextEpisodeRecord, MessageBody, MessageEnvelope, TaskRecord,
    TaskStatus, TodoItemState, ToolExecutionRecord, TurnRecord, WorkItemPlanStatus, WorkItemRecord,
    WorkspaceEntry,
};
use serde_json::{json, Value};

use crate::{excerpt, truncate_chars, MemoryDocument, MemoryGetResult};

pub fn memory_get_result(document: MemoryDocument, max_chars: usize) -> MemoryGetResult {
    let (content, truncated) = truncate_chars(&document.body, max_chars);
    MemoryGetResult {
        kind: document.source_kind,
        source_ref: document.source_ref,
        scope_kind: document.scope_kind,
        workspace_id: document.workspace_id,
        agent_id: document.agent_id,
        source_path: document.source_path.map(|path| path.display().to_string()),
        title: document.title,
        content,
        truncated,
        updated_at: document.updated_at,
        metadata: document.metadata,
    }
}

pub fn agent_memory_markdown_document(
    agent_id: String,
    name: &str,
    title: &str,
    path: &Path,
    body: String,
    updated_at: DateTime<Utc>,
) -> MemoryDocument {
    MemoryDocument {
        source_ref: format!("agent_memory:{name}"),
        source_kind: "agent_memory_markdown".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id,
        source_path: Some(path.to_path_buf()),
        title: title.into(),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "memory_name": name,
            "governance_surface": "curated_durable_memory",
            "provenance_class": "agent_home_memory_markdown",
            "trust_class": "agent_curated",
        }),
        updated_at,
    }
}

pub fn workspace_profile_document(entry: WorkspaceEntry, agent_id: &str) -> MemoryDocument {
    let title = entry
        .repo_name
        .clone()
        .unwrap_or_else(|| format!("Workspace {}", entry.workspace_id));
    let body = format!(
        "{}\nworkspace_id: {}\nworkspace_anchor: {}",
        title,
        entry.workspace_id,
        entry.workspace_anchor.display()
    );
    MemoryDocument {
        source_ref: format!("workspace_profile:{}", entry.workspace_id),
        source_kind: "workspace_profile".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(entry.workspace_id.clone()),
        agent_id: agent_id.to_string(),
        source_path: Some(entry.workspace_anchor.clone()),
        title,
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "workspace_anchor": entry.workspace_anchor,
            "governance_surface": "workspace_profile_projection",
            "provenance_class": "workspace_registry",
            "trust_class": "runtime_projection",
        }),
        updated_at: entry.updated_at,
    }
}

pub fn brief_document(brief: BriefRecord, body: String, agent_home: &Path) -> MemoryDocument {
    MemoryDocument {
        source_ref: format!("brief:{}", brief.id),
        source_kind: "brief".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(brief.workspace_id),
        agent_id: brief.agent_id,
        source_path: None,
        title: format!("Brief {:?}", brief.kind),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "work_item_id": brief.work_item_id,
            "related_message_id": brief.related_message_id,
            "related_task_id": brief.related_task_id,
            "agent_home": agent_home,
            "governance_surface": "runtime_evidence",
            "provenance_class": "brief_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: brief.created_at,
    }
}

pub fn message_document(message: MessageEnvelope) -> MemoryDocument {
    let body = message_document_body(&message);
    let excerpt_body = message_body_text_for_memory(&message.body);
    let title = format!("Message {}", message.id);
    MemoryDocument {
        source_ref: format!("message:{}", message.id),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: message.agent_id.clone(),
        source_path: None,
        title,
        sanitized_excerpt: excerpt(&excerpt_body),
        body,
        metadata: json!({
            "message_id": message.id,
            "turn_id": message.turn_id,
            "message_seq": message.message_seq,
            "work_item_id": message.work_item_id,
            "task_id": message.task_id,
            "governance_surface": "runtime_evidence",
            "provenance_class": "message_envelope",
            "trust_class": "runtime_evidence",
        }),
        updated_at: message.created_at,
    }
}

fn message_document_body(message: &MessageEnvelope) -> String {
    message_document_body_with_limit(message, 8_000)
}

pub fn message_document_body_with_limit(message: &MessageEnvelope, limit: usize) -> String {
    let mut lines = vec![
        format!("message_ref: message:{}", message.id),
        format!("message_id: {}", message.id),
    ];
    if let Some(turn_id) = message.turn_id.as_deref() {
        lines.push(format!("turn_ref: turn:{turn_id}"));
    }
    if let Some(message_seq) = message.message_seq {
        lines.push(format!("message_seq: {message_seq}"));
    }
    lines.push(format!("kind: {:?}", message.kind));
    lines.push(format!("origin: {:?}", message.origin));
    lines.push(format!("authority_class: {:?}", message.authority_class));
    lines.push(format!("priority: {:?}", message.priority));
    if let Some(trigger_kind) = message.trigger_kind {
        lines.push(format!("trigger_kind: {:?}", trigger_kind));
    }
    if let Some(work_item_id) = message.work_item_id.as_deref() {
        lines.push(format!("work_item_ref: work_item:{work_item_id}"));
    }
    if let Some(task_id) = message.task_id.as_deref() {
        lines.push(format!("task_ref: task:{task_id}"));
    }
    if let Some(delivery_surface) = message.delivery_surface {
        lines.push(format!("delivery_surface: {:?}", delivery_surface));
    }
    if let Some(admission_context) = message.admission_context {
        lines.push(format!("admission_context: {:?}", admission_context));
    }
    if !message.source_refs.is_empty() {
        lines.push(format!(
            "source_refs: {}",
            message
                .source_refs
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let body = message_body_text_for_memory(&message.body);
    lines.push("body:".to_string());
    lines.push(truncate_multiline(&body, limit));
    lines.join("\n")
}

fn message_body_text_for_memory(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

fn truncate_multiline(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n[truncated]")
    } else {
        truncated
    }
}

pub fn episode_document(episode: ContextEpisodeRecord) -> MemoryDocument {
    let body = episode_document_body(&episode);
    let title = episode
        .work_summary
        .clone()
        .or_else(|| episode.objective.clone())
        .unwrap_or_else(|| format!("Episode {}", episode.id));
    MemoryDocument {
        source_ref: format!("episode:{}", episode.id),
        source_kind: "context_episode".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(episode.workspace_id),
        agent_id: episode.agent_id,
        source_path: None,
        title,
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "episode_id": episode.id,
            "current_work_item_id": episode.current_work_item_id,
            "boundary_reason": episode.boundary_reason,
            "working_set_files": episode.working_set_files,
            "source_refs": episode.source_refs,
            "governance_surface": "runtime_evidence",
            "provenance_class": "context_episode_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: episode.finalized_at,
    }
}

fn episode_document_body(episode: &ContextEpisodeRecord) -> String {
    let mut lines = vec![
        format!("episode_ref: episode:{}", episode.id),
        format!(
            "turns: {}-{}",
            episode.start_turn_index, episode.end_turn_index
        ),
        format!("boundary: {:?}", episode.boundary_reason),
    ];
    if let Some(work_item_id) = episode.current_work_item_id.as_deref() {
        lines.push(format!("work_item_ref: work_item:{work_item_id}"));
    }
    if let Some(objective) = episode.objective.as_deref() {
        lines.push(format!(
            "objective_preview: {}",
            truncate_inline(objective, 180)
        ));
    }
    if let Some(work_summary) = episode.work_summary.as_deref() {
        lines.push(format!(
            "work_summary_preview: {}",
            truncate_inline(work_summary, 180)
        ));
    }
    if !episode.source_refs.is_empty() {
        lines.push(format!(
            "retrieval_refs: {}",
            episode
                .source_refs
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.source_turn_ids.is_empty() {
        lines.push(format!(
            "provenance_turn_ids: {}",
            episode
                .source_turn_ids
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.working_set_files.is_empty() {
        lines.push(format!(
            "files: {}",
            episode
                .working_set_files
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.carry_forward.is_empty() {
        lines.push(format!(
            "followups: {}",
            episode
                .carry_forward
                .iter()
                .take(8)
                .map(|item| truncate_inline(item, 160))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.waiting_on.is_empty() {
        lines.push(format!(
            "waiting_on: {}",
            episode
                .waiting_on
                .iter()
                .take(8)
                .map(|item| truncate_inline(item, 160))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.join("\n")
}

pub fn turn_document(turn: TurnRecord, tool_executions: &[ToolExecutionRecord]) -> MemoryDocument {
    let body = turn_document_body(&turn, tool_executions);
    MemoryDocument {
        source_ref: format!("turn:{}", turn.turn_id),
        source_kind: "turn".into(),
        scope_kind: "workspace".into(),
        workspace_id: None,
        agent_id: turn.agent_id,
        source_path: None,
        title: format!("Turn {}", turn.turn_index),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "turn_id": turn.turn_id,
            "turn_index": turn.turn_index,
            "current_work_item_id": turn.current_work_item_id,
            "governance_surface": "runtime_evidence",
            "provenance_class": "turn_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: turn.created_at,
    }
}

fn turn_document_body(turn: &TurnRecord, tool_executions: &[ToolExecutionRecord]) -> String {
    let mut lines = vec![
        format!("turn_ref: turn:{}", turn.turn_id),
        format!("turn_index: {}", turn.turn_index),
    ];
    if let Some(trigger) = turn.trigger.as_ref() {
        lines.push(format!("trigger_kind: {:?}", trigger.kind));
        if let Some(message_id) = trigger.message_id.as_deref() {
            lines.push(format!("trigger_message_ref: message:{message_id}"));
        }
    }
    if let Some(work_item_id) = turn.current_work_item_id.as_deref() {
        lines.push(format!("current_work_item_ref: work_item:{work_item_id}"));
    }
    if !turn.input_message_ids.is_empty() {
        lines.push(format!(
            "input_message_refs: {}",
            turn.input_message_ids
                .iter()
                .take(12)
                .map(|id| format!("message:{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !turn.tool_execution_ids.is_empty() {
        append_id_list(&mut lines, "tool_execution_ids", &turn.tool_execution_ids);
        lines.push(format!(
            "tool_execution_refs_when_available: {}",
            tool_execution_refs_when_available(&turn.tool_execution_ids, tool_executions)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !turn.produced_brief_ids.is_empty() {
        lines.push(format!(
            "brief_refs_when_semantic: {}",
            turn.produced_brief_ids
                .iter()
                .take(12)
                .map(|id| format!("brief:{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !turn.completed_work_item_ids.is_empty() {
        lines.push(format!(
            "completed_work_item_refs: {}",
            turn.completed_work_item_ids
                .iter()
                .take(12)
                .map(|id| format!("work_item:{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    append_id_list(
        &mut lines,
        "delivery_summary_ids",
        &turn.delivery_summary_ids,
    );
    append_id_list(
        &mut lines,
        "waiting_condition_ids",
        &turn.waiting_condition_ids,
    );
    if let Some(terminal) = turn.terminal.as_ref() {
        lines.push(format!("terminal_kind: {:?}", terminal.kind));
        if let Some(reason) = terminal.reason.as_deref() {
            lines.push(format!("terminal_reason: {}", truncate_inline(reason, 180)));
        }
    }
    lines.join("\n")
}

fn tool_execution_refs_when_available<'a>(
    tool_execution_ids: &'a [String],
    tool_executions: &'a [ToolExecutionRecord],
) -> impl Iterator<Item = String> + 'a {
    let records_by_id = tool_executions
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    tool_execution_ids
        .iter()
        .take(12)
        .filter_map(move |id| records_by_id.get(id.as_str()).copied())
        .flat_map(|record| {
            let mut refs = Vec::new();
            match record.tool_name.as_str() {
                "ExecCommand" => {
                    if record.input.get("cmd").and_then(Value::as_str).is_some() {
                        refs.push(command_receipt_source_ref(&record.id, None));
                    }
                    refs.push(command_output_source_ref(&record.id, None, "output"));
                }
                "ExecCommandBatch" => {
                    if let Some(items) = record.input.get("items").and_then(Value::as_array) {
                        refs.extend(items.iter().enumerate().filter_map(|(index, item)| {
                            item.get("cmd")
                                .and_then(Value::as_str)
                                .map(|_| command_receipt_source_ref(&record.id, Some(index + 1)))
                        }));
                    }
                    refs.push(command_output_source_ref(&record.id, None, "output"));
                }
                _ => refs.push(command_output_source_ref(&record.id, None, "output")),
            }
            refs
        })
}

fn append_id_list(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!(
        "{label}: {}",
        values
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    ));
}

fn truncate_inline(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

pub fn work_item_document(item: WorkItemRecord) -> MemoryDocument {
    let body = work_item_document_body(&item);
    MemoryDocument {
        source_ref: format!("work_item:{}", item.id),
        source_kind: "work_item".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(item.workspace_id),
        agent_id: item.agent_id,
        source_path: None,
        title: item.objective.clone(),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "work_item_id": item.id,
            "state": item.state,
            "blocked_by": item.blocked_by,
            "governance_surface": "runtime_evidence",
            "provenance_class": "work_item_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: item.updated_at,
    }
}

fn work_item_document_body(item: &WorkItemRecord) -> String {
    let mut lines = vec![
        format!("Objective: {}", item.objective),
        format!(
            "Plan status: {}",
            work_item_plan_status_label(item.plan_status)
        ),
    ];
    if let Some(plan_artifact) = item.plan_artifact.as_ref() {
        if !plan_artifact.preview.trim().is_empty() {
            lines.push("Plan preview:".into());
            lines.push(plan_artifact.preview.clone());
        }
    }
    if let Some(result_brief_id) = item
        .result_brief_id
        .as_ref()
        .filter(|result_brief_id| !result_brief_id.trim().is_empty())
    {
        lines.push(format!("Result ref: brief:{result_brief_id}"));
    }
    if let Some(result_summary) = item
        .result_summary
        .as_ref()
        .filter(|result_summary| !result_summary.trim().is_empty())
    {
        lines.push("Legacy result summary:".into());
        lines.push(result_summary.clone());
    }
    if !item.todo_list.is_empty() {
        lines.push("Todo list:".into());
        for todo in &item.todo_list {
            lines.push(format!(
                "- [{}] {}",
                todo_item_state_label(todo.state),
                todo.text
            ));
        }
    }
    lines.join("\n")
}

pub fn task_document(task: TaskRecord, command_preview: fn(&str) -> String) -> MemoryDocument {
    let agent_id = task.agent_id.clone();
    let task_kind = task.kind.as_str();
    let command = CommandTaskStatusSnapshot::identity_from_task_record(&task);
    let command = command.as_ref();
    let command_text = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("cmd"))
        .and_then(Value::as_str)
        .or_else(|| command.and_then(|entry| entry.cmd.as_deref()));
    let child_agent_id = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("child_agent_id"))
        .and_then(Value::as_str);
    let exit_status = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("exit_status"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let mut body = task_document_body(
        &task,
        task_kind,
        command,
        command_text,
        child_agent_id,
        exit_status,
        command_preview,
    );
    if let Some(cmd) = command_text {
        body.push('\n');
        if cmd.contains('\n') {
            body.push_str("cmd:\n");
        } else {
            body.push_str("cmd: ");
        }
        body.push_str(cmd);
    }
    MemoryDocument {
        source_ref: format!("task:{}", task.id),
        source_kind: "task".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: agent_id.clone(),
        source_path: None,
        title: task
            .summary
            .clone()
            .unwrap_or_else(|| format!("Task {}", task.id)),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "task_id": task.id,
            "kind": task_kind,
            "status": task_status_label(&task.status),
            "summary": task.summary,
            "work_item_id": task.work_item_id,
            "cmd_digest": task_command_digest(command, command_text),
            "cmd_preview": command
                .and_then(|entry| entry.cmd.as_deref())
                .or(command_text)
                .map(command_preview),
            "exit_status": exit_status,
            "agent_id": agent_id,
            "created_at": task.created_at.to_rfc3339(),
            "governance_surface": "runtime_evidence",
            "provenance_class": "task_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: task.updated_at,
    }
}

fn task_document_body(
    task: &TaskRecord,
    task_kind: &str,
    command: Option<&CommandTaskStatusSnapshot>,
    command_text: Option<&str>,
    child_agent_id: Option<&str>,
    exit_status: Option<i32>,
    command_preview: fn(&str) -> String,
) -> String {
    let mut lines = vec![
        format!("task_id: {}", task.id),
        format!("kind: {task_kind}"),
        format!("status: {}", task_status_label(&task.status)),
        format!("summary: {}", task.summary.clone().unwrap_or_default()),
        format!("created_at: {}", task.created_at.to_rfc3339()),
        format!("updated_at: {}", task.updated_at.to_rfc3339()),
    ];
    if let Some(work_item_id) = task.work_item_id.as_deref() {
        lines.push(format!("work_item_id: {work_item_id}"));
    }
    if let Some(cmd_digest) = task_command_digest(command, command_text) {
        lines.push(format!("cmd_digest: {cmd_digest}"));
    }
    if let Some(cmd_preview) = command
        .and_then(|entry| entry.cmd.as_deref())
        .or(command_text)
        .map(command_preview)
    {
        lines.push(format!("cmd_preview: {cmd_preview}"));
    }
    if let Some(exit_status) = exit_status {
        lines.push(format!("exit_status: {exit_status}"));
    }
    if let Some(child_agent_id) = child_agent_id {
        lines.push(format!("child_agent_id: {child_agent_id}"));
    }
    lines.join("\n")
}

fn task_command_digest(
    command: Option<&CommandTaskStatusSnapshot>,
    command_text: Option<&str>,
) -> Option<String> {
    command
        .and_then(|entry| {
            entry
                .cmd_digest
                .clone()
                .or_else(|| entry.cmd.as_ref().map(|cmd| command_digest(cmd)))
        })
        .or_else(|| command_text.map(command_digest))
}

fn task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

pub fn command_execution_record_document_count(record: &ToolExecutionRecord) -> usize {
    match record.tool_name.as_str() {
        "ExecCommand" => usize::from(record.input.get("cmd").and_then(Value::as_str).is_some()),
        "ExecCommandBatch" => record
            .input
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item.get("cmd").and_then(Value::as_str).is_some())
            .count(),
        _ => 0,
    }
}

pub fn command_execution_record_documents_slice(
    record: &ToolExecutionRecord,
    offset: usize,
    limit: usize,
    command_preview: fn(&str) -> String,
) -> Vec<MemoryDocument> {
    match record.tool_name.as_str() {
        "ExecCommand" => record
            .input
            .get("cmd")
            .and_then(Value::as_str)
            .map(|cmd| command_receipt_document(record, None, None, cmd, command_preview))
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect(),
        "ExecCommandBatch" => record
            .input
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(offset, item)| {
                item.get("cmd").and_then(Value::as_str).map(|cmd| {
                    command_receipt_document(
                        record,
                        Some(offset + 1),
                        Some(item),
                        cmd,
                        command_preview,
                    )
                })
            })
            .skip(offset)
            .take(limit)
            .collect(),
        _ => Vec::new(),
    }
}

pub fn generic_tool_execution_output_document_from_record(
    record: &ToolExecutionRecord,
) -> MemoryDocument {
    let source_ref = command_output_source_ref(&record.id, None, "output");
    let output = json!({
        "source_type": "tool_execution_output",
        "tool_execution_id": record.id,
        "tool_name": record.tool_name,
        "selector": "output",
        "status": record.status,
        "summary": record.summary,
        "output": record.output,
        "message": "Output evidence recovered from the tool execution record."
    });
    let body = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
    MemoryDocument {
        source_ref,
        source_kind: "tool_execution_output".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: record.agent_id.clone(),
        source_path: None,
        title: format!("{} tool output", record.tool_name),
        sanitized_excerpt: format!(
            "tool_execution_id={} tool_name={} selector=output summary={}",
            record.id,
            record.tool_name,
            truncate_chars(&record.summary, 240).0
        ),
        body,
        metadata: json!({
            "tool_execution_id": record.id,
            "tool_name": record.tool_name,
            "turn_index": record.turn_index,
            "work_item_id": record.work_item_id,
            "selector": "output",
            "governance_surface": "runtime_evidence",
            "provenance_class": "tool_execution_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: record.completed_at.unwrap_or(record.created_at),
    }
}

/// Render command output for an optional one-based batch item index.
pub fn command_output_document(
    record: &ToolExecutionRecord,
    batch_item_index: Option<usize>,
    stream: &'static str,
) -> Option<MemoryDocument> {
    let output = command_output_envelope(record, batch_item_index, stream)?;
    let source_ref = command_output_source_ref(&record.id, batch_item_index, stream);
    let title = match batch_item_index {
        Some(index) => format!("{} {stream} output item {}", record.tool_name, index),
        None => format!("{} {stream} output", record.tool_name),
    };
    let body = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
    let preview = output
        .get("preview")
        .and_then(Value::as_str)
        .or_else(|| output.get("content").and_then(Value::as_str))
        .unwrap_or("");
    Some(MemoryDocument {
        source_ref,
        source_kind: "tool_command_output".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: record.agent_id.clone(),
        source_path: None,
        title,
        sanitized_excerpt: format!(
            "tool_execution_id={} tool_name={} selector={} batch_item_index={:?} preview={}",
            record.id,
            record.tool_name,
            stream,
            batch_item_index,
            truncate_chars(preview, 240).0
        ),
        body,
        metadata: json!({
            "tool_execution_id": record.id,
            "tool_name": record.tool_name,
            "turn_index": record.turn_index,
            "work_item_id": record.work_item_id,
            "batch_item_index": batch_item_index,
            "selector": stream,
            "governance_surface": "runtime_evidence",
            "provenance_class": "tool_execution_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: record.completed_at.unwrap_or(record.created_at),
    })
}

fn command_output_envelope(
    record: &ToolExecutionRecord,
    batch_item_index: Option<usize>,
    stream: &'static str,
) -> Option<Value> {
    let output = match (record.tool_name.as_str(), batch_item_index) {
        ("ExecCommand", None) => record
            .output
            .get("result")
            .or_else(|| {
                record
                    .output
                    .get("envelope")
                    .and_then(|value| value.get("result"))
            })
            .unwrap_or(&record.output),
        ("ExecCommandBatch", Some(index)) => record
            .output
            .get("result")
            .or_else(|| {
                record
                    .output
                    .get("envelope")
                    .and_then(|value| value.get("result"))
            })
            .unwrap_or(&record.output)
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| index.checked_sub(1).and_then(|index| items.get(index)))
            .map(|item| item.get("result").unwrap_or(item))?,
        _ => return None,
    };
    let content = match stream {
        "stdout" => output.get("stdout_preview").and_then(Value::as_str),
        "stderr" => output.get("stderr_preview").and_then(Value::as_str),
        "output" => output
            .get("stdout_preview")
            .and_then(Value::as_str)
            .or_else(|| output.get("stderr_preview").and_then(Value::as_str))
            .or_else(|| output.get("initial_output_preview").and_then(Value::as_str)),
        _ => None,
    };
    let artifact_key = match stream {
        "stdout" => Some("stdout_artifact"),
        "stderr" => Some("stderr_artifact"),
        _ => None,
    };
    let artifact = artifact_key
        .and_then(|key| output.get(key).and_then(Value::as_u64))
        .and_then(|index| {
            output
                .get("artifacts")
                .and_then(Value::as_array)
                .and_then(|artifacts| artifacts.get(index as usize))
        })
        .cloned();
    let available = content.is_some() || artifact.is_some();
    Some(json!({
        "source_type": "tool_command_output",
        "tool_execution_id": record.id,
        "tool_name": record.tool_name,
        "batch_item_index": batch_item_index,
        "selector": stream,
        "status": record.status,
        "disposition": output.get("disposition").and_then(Value::as_str).unwrap_or("unknown"),
        "content": content,
        "preview": content.unwrap_or(""),
        "output_available": available,
        "availability": if available { "available" } else { "output_unavailable" },
        "truncated": output.get("truncated").and_then(Value::as_bool).unwrap_or(false)
            || output.get("initial_output_truncated").and_then(Value::as_bool).unwrap_or(false),
        "artifact": artifact,
        "message": if available {
            "Output evidence recovered from the tool execution record. If artifact is present, use that pointer for full bytes rather than expecting MemoryGet to stream them."
        } else {
            "No persisted output evidence is available for this tool execution stream; older records may contain command input only."
        },
    }))
}

pub fn command_receipt_document(
    record: &ToolExecutionRecord,
    batch_item_index: Option<usize>,
    batch_item_input: Option<&Value>,
    cmd: &str,
    command_preview: fn(&str) -> String,
) -> MemoryDocument {
    let cmd_digest = command_digest(cmd);
    let source_ref = command_receipt_source_ref(&record.id, batch_item_index);
    let title = match batch_item_index {
        Some(index) => format!("{} command receipt item {}", record.tool_name, index),
        None => format!("{} command receipt", record.tool_name),
    };
    let preview = command_preview(cmd);
    let input_json = batch_item_input.unwrap_or(&record.input).to_owned();
    let input_json =
        serde_json::to_string_pretty(&input_json).unwrap_or_else(|_| input_json.to_string());
    let batch_item_input = batch_item_input
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()));
    let body = format!(
        "tool_execution_id: {}\ntool_name: {}\ncmd_digest: {}\ninput_json:\n{}\n{}cmd:\n{}",
        record.id,
        record.tool_name,
        cmd_digest,
        input_json,
        batch_item_input
            .as_deref()
            .map(|value| format!("batch_item_input_json:\n{value}\n"))
            .unwrap_or_default(),
        cmd
    );
    MemoryDocument {
        source_ref,
        source_kind: "tool_command_receipt".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: record.agent_id.clone(),
        source_path: None,
        title,
        sanitized_excerpt: format!(
            "tool_execution_id={} tool_name={} cmd_digest={} cmd_preview={}",
            record.id, record.tool_name, cmd_digest, preview
        ),
        body,
        metadata: json!({
            "tool_execution_id": record.id,
            "tool_name": record.tool_name,
            "turn_index": record.turn_index,
            "work_item_id": record.work_item_id,
            "batch_item_index": batch_item_index,
            "cmd_digest": cmd_digest,
            "cmd_preview": preview,
            "governance_surface": "runtime_evidence",
            "provenance_class": "tool_execution_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: record.completed_at.unwrap_or(record.created_at),
    }
}

fn work_item_plan_status_label(status: WorkItemPlanStatus) -> &'static str {
    match status {
        WorkItemPlanStatus::Draft => "draft",
        WorkItemPlanStatus::Ready => "ready",
        WorkItemPlanStatus::NeedsInput => "needs_input",
    }
}

fn todo_item_state_label(state: TodoItemState) -> &'static str {
    match state {
        TodoItemState::Pending => "pending",
        TodoItemState::InProgress => "in_progress",
        TodoItemState::Completed => "completed",
    }
}
