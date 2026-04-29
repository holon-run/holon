use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::Value;

use crate::{
    storage::AppStorage,
    types::{
        AgentState, AuditEvent, ClosureDecision, MessageEnvelope, MessageKind, ToolExecutionRecord,
        TurnMemoryDelta, WaitingIntentStatus, WorkItemRecord, WorkItemStatus, WorkPlanSnapshot,
        WorkPlanStepStatus, WorkingMemoryDelta, WorkingMemorySnapshot, WorkingMemoryUpdateReason,
    },
};

const MEMORY_TOOL_LIMIT: usize = 24;
const MEMORY_EVIDENCE_SCAN_MULTIPLIER: usize = 4;
const MEMORY_TOOL_SCAN_LIMIT: usize = MEMORY_TOOL_LIMIT * MEMORY_EVIDENCE_SCAN_MULTIPLIER;
const MEMORY_PLAN_LIMIT: usize = 6;
const MEMORY_FILE_LIMIT: usize = 8;
const MEMORY_FOLLOWUP_LIMIT: usize = 6;
const MEMORY_WAITING_LIMIT: usize = 4;
const MEMORY_COMMAND_LIMIT: usize = 4;
const MEMORY_VERIFICATION_LIMIT: usize = 4;
const MEMORY_SUMMARY_LINE_LIMIT: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingMemoryRefresh {
    pub previous_snapshot: WorkingMemorySnapshot,
    pub current_snapshot: WorkingMemorySnapshot,
    pub turn_memory_delta: TurnMemoryDelta,
    pub working_memory_updated: bool,
}

pub fn mark_working_memory_prompted(agent: &mut AgentState, rendered_revision: u64) {
    if agent
        .working_memory
        .pending_working_memory_delta
        .as_ref()
        .is_some_and(|delta| delta.to_revision <= rendered_revision)
    {
        agent.working_memory.last_prompted_working_memory_revision = Some(rendered_revision);
        agent.working_memory.pending_working_memory_delta = None;
    }
}

pub fn refresh_working_memory(
    storage: &AppStorage,
    agent: &mut AgentState,
    trigger: &MessageEnvelope,
    prior_closure: &ClosureDecision,
    current_closure: &ClosureDecision,
) -> Result<WorkingMemoryRefresh> {
    let next_snapshot = normalize_working_memory_snapshot(derive_working_memory_snapshot(
        storage,
        current_closure,
    )?);
    let persisted_previous_snapshot = agent.working_memory.current_working_memory.clone();
    let previous_snapshot = normalize_working_memory_snapshot(persisted_previous_snapshot.clone());
    let scrubbed_legacy_fields = previous_snapshot != persisted_previous_snapshot;
    if scrubbed_legacy_fields {
        agent.working_memory.current_working_memory = previous_snapshot.clone();
    }
    let cleared_stale_summary =
        !working_memory_snapshot_is_empty(&next_snapshot) && agent.context_summary.take().is_some();
    let turn_delta = derive_turn_memory_delta(
        agent.turn_index,
        &previous_snapshot,
        &next_snapshot,
        storage.read_recent_tool_executions(MEMORY_TOOL_LIMIT)?,
    );
    if previous_snapshot == next_snapshot {
        return Ok(WorkingMemoryRefresh {
            previous_snapshot,
            current_snapshot: next_snapshot,
            turn_memory_delta: turn_delta,
            working_memory_updated: cleared_stale_summary || scrubbed_legacy_fields,
        });
    }

    let next_revision = agent.working_memory.working_memory_revision + 1;
    let reason = derive_update_reason(trigger, prior_closure, &previous_snapshot, &next_snapshot);
    let new_delta = derive_working_memory_delta(
        &previous_snapshot,
        &next_snapshot,
        agent,
        next_revision,
        reason,
    );

    storage.append_working_memory_delta(&new_delta)?;
    storage.append_event(&AuditEvent::new(
        "working_memory_updated",
        serde_json::json!({
            "agent_id": agent.id,
            "revision": next_revision,
            "reason": new_delta.reason,
            "delta": new_delta,
            "turn_memory_delta": turn_delta,
        }),
    ))?;

    agent.working_memory.current_working_memory = next_snapshot.clone();
    agent.working_memory.working_memory_revision = next_revision;
    agent.working_memory.pending_working_memory_delta = Some(merge_pending_delta(
        agent.working_memory.pending_working_memory_delta.as_ref(),
        &new_delta,
    ));

    Ok(WorkingMemoryRefresh {
        previous_snapshot,
        current_snapshot: next_snapshot,
        turn_memory_delta: turn_delta,
        working_memory_updated: true,
    })
}

fn normalize_working_memory_snapshot(mut snapshot: WorkingMemorySnapshot) -> WorkingMemorySnapshot {
    snapshot.scope_hints.clear();
    snapshot.recent_decisions.clear();
    snapshot
}

fn working_memory_snapshot_is_empty(snapshot: &WorkingMemorySnapshot) -> bool {
    snapshot == &WorkingMemorySnapshot::default()
}

pub fn derive_working_memory_snapshot(
    storage: &AppStorage,
    current_closure: &ClosureDecision,
) -> Result<WorkingMemorySnapshot> {
    let projection = storage.work_queue_prompt_projection()?;
    let waiting_anchor = projection.active.as_ref().or_else(|| {
        projection
            .queued_waiting
            .iter()
            .filter(|item| item.status == WorkItemStatus::Waiting)
            .max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.created_at.cmp(&right.created_at))
                    .then_with(|| left.id.cmp(&right.id))
            })
    });
    let active_work_item = waiting_anchor;
    let active_work_plan = active_work_item
        .map(|item| storage.latest_work_plan(&item.id))
        .transpose()?
        .flatten();
    let recent_tools = storage.read_recent_tool_executions(MEMORY_TOOL_SCAN_LIMIT)?;
    let active_waiting = storage
        .latest_waiting_intents()?
        .into_iter()
        .filter(|record| record.status == WaitingIntentStatus::Active)
        .collect::<Vec<_>>();
    let active_work_item_id = active_work_item.map(|item| item.id.as_str());
    let memory_tools = collect_memory_tools(&recent_tools, active_work_item_id);

    let work_summary = active_work_item
        .and_then(|item| item.summary.clone().or_else(|| item.progress_note.clone()));
    let current_plan = active_work_plan
        .as_ref()
        .map(render_current_plan)
        .unwrap_or_default();
    let queued_waiting_followups = projection
        .queued_waiting
        .iter()
        .filter(|item| Some(item.id.as_str()) != active_work_item.map(|anchor| anchor.id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let pending_followups = collect_pending_followups(
        active_work_item,
        active_work_plan.as_ref(),
        &queued_waiting_followups,
    );
    let waiting_on = collect_waiting_on(&active_waiting, active_work_item_id, current_closure);
    let working_set_files = collect_working_set_files(&memory_tools);

    Ok(WorkingMemorySnapshot {
        active_work_item_id: active_work_item.map(|item| item.id.clone()),
        delivery_target: active_work_item.map(|item| item.delivery_target.clone()),
        work_summary,
        current_plan,
        working_set_files,
        pending_followups,
        waiting_on,
        ..WorkingMemorySnapshot::default()
    })
}

fn derive_update_reason(
    trigger: &MessageEnvelope,
    prior_closure: &ClosureDecision,
    previous: &WorkingMemorySnapshot,
    next: &WorkingMemorySnapshot,
) -> WorkingMemoryUpdateReason {
    if previous.active_work_item_id != next.active_work_item_id
        || previous.delivery_target != next.delivery_target
        || previous.work_summary != next.work_summary
    {
        return WorkingMemoryUpdateReason::ActiveWorkChanged;
    }
    if matches!(
        trigger.kind,
        MessageKind::TaskResult | MessageKind::TaskStatus
    ) {
        return WorkingMemoryUpdateReason::TaskRejoined;
    }
    if prior_closure.waiting_reason.is_some()
        && matches!(
            trigger.kind,
            MessageKind::SystemTick
                | MessageKind::CallbackEvent
                | MessageKind::TimerTick
                | MessageKind::WebhookEvent
                | MessageKind::ChannelEvent
        )
    {
        return WorkingMemoryUpdateReason::WakeResumed;
    }
    WorkingMemoryUpdateReason::TerminalTurnCompleted
}

fn derive_turn_memory_delta(
    turn_index: u64,
    previous: &WorkingMemorySnapshot,
    next: &WorkingMemorySnapshot,
    recent_tools: Vec<ToolExecutionRecord>,
) -> TurnMemoryDelta {
    let current_turn_tools = recent_tools
        .into_iter()
        .filter(|record| record.turn_index == turn_index.max(1))
        .collect::<Vec<_>>();
    let commands = current_turn_tools
        .iter()
        .rev()
        .filter(|record| record.tool_name == "ExecCommand")
        .filter_map(extract_exec_command)
        .take(MEMORY_COMMAND_LIMIT)
        .collect::<Vec<_>>();
    let verification = current_turn_tools
        .iter()
        .rev()
        .filter(|record| looks_like_verification(&record.summary))
        .map(|record| truncate_line(&record.summary, 120))
        .take(MEMORY_VERIFICATION_LIMIT)
        .collect::<Vec<_>>();

    TurnMemoryDelta {
        turn_index: turn_index.max(1),
        active_work_changed: previous.active_work_item_id != next.active_work_item_id
            || previous.delivery_target != next.delivery_target
            || previous.work_summary != next.work_summary,
        work_plan_changed: previous.current_plan != next.current_plan,
        scope_hints_changed: false,
        touched_files: diff_list(&previous.working_set_files, &next.working_set_files),
        commands,
        verification,
        decisions: Vec::new(),
        pending_followups: next.pending_followups.clone(),
        waiting_on: next.waiting_on.clone(),
    }
}

fn derive_working_memory_delta(
    previous: &WorkingMemorySnapshot,
    next: &WorkingMemorySnapshot,
    agent: &AgentState,
    next_revision: u64,
    reason: WorkingMemoryUpdateReason,
) -> WorkingMemoryDelta {
    let mut changed_fields = Vec::new();
    let mut summary_lines = Vec::new();

    push_changed_field(
        &mut changed_fields,
        &mut summary_lines,
        "active_work_item_id",
        previous.active_work_item_id.as_deref(),
        next.active_work_item_id.as_deref(),
        "active work item",
    );
    push_changed_field(
        &mut changed_fields,
        &mut summary_lines,
        "delivery_target",
        previous.delivery_target.as_deref(),
        next.delivery_target.as_deref(),
        "delivery target",
    );
    push_changed_field(
        &mut changed_fields,
        &mut summary_lines,
        "work_summary",
        previous.work_summary.as_deref(),
        next.work_summary.as_deref(),
        "work summary",
    );
    push_changed_vec(
        &mut changed_fields,
        &mut summary_lines,
        "current_plan",
        &previous.current_plan,
        &next.current_plan,
        "current plan",
    );
    push_changed_vec(
        &mut changed_fields,
        &mut summary_lines,
        "working_set_files",
        &previous.working_set_files,
        &next.working_set_files,
        "working set files",
    );
    push_changed_vec(
        &mut changed_fields,
        &mut summary_lines,
        "pending_followups",
        &previous.pending_followups,
        &next.pending_followups,
        "pending follow-ups",
    );
    push_changed_vec(
        &mut changed_fields,
        &mut summary_lines,
        "waiting_on",
        &previous.waiting_on,
        &next.waiting_on,
        "waiting state",
    );

    WorkingMemoryDelta {
        from_revision: agent
            .working_memory
            .last_prompted_working_memory_revision
            .unwrap_or(agent.working_memory.working_memory_revision),
        to_revision: next_revision,
        created_at_turn: agent.turn_index,
        reason,
        changed_fields,
        summary_lines: limit_vec(summary_lines, MEMORY_SUMMARY_LINE_LIMIT),
    }
}

fn merge_pending_delta(
    pending: Option<&WorkingMemoryDelta>,
    new_delta: &WorkingMemoryDelta,
) -> WorkingMemoryDelta {
    let Some(pending) = pending else {
        return new_delta.clone();
    };

    WorkingMemoryDelta {
        from_revision: pending.from_revision,
        to_revision: new_delta.to_revision,
        created_at_turn: new_delta.created_at_turn,
        reason: new_delta.reason,
        changed_fields: dedup_owned(
            pending
                .changed_fields
                .iter()
                .cloned()
                .chain(new_delta.changed_fields.iter().cloned())
                .collect(),
            MEMORY_SUMMARY_LINE_LIMIT * 2,
        ),
        summary_lines: dedup_owned(
            pending
                .summary_lines
                .iter()
                .cloned()
                .chain(new_delta.summary_lines.iter().cloned())
                .collect(),
            MEMORY_SUMMARY_LINE_LIMIT,
        ),
    }
}

fn render_current_plan(plan: &WorkPlanSnapshot) -> Vec<String> {
    plan.items
        .iter()
        .filter(|item| item.status != WorkPlanStepStatus::Completed)
        .map(|item| format!("[{:?}] {}", item.status, item.step))
        .take(MEMORY_PLAN_LIMIT)
        .collect()
}

fn collect_pending_followups(
    active_work_item: Option<&WorkItemRecord>,
    active_work_plan: Option<&WorkPlanSnapshot>,
    queued_waiting: &[WorkItemRecord],
) -> Vec<String> {
    let mut items = Vec::new();

    if let Some(active) = active_work_item {
        if let Some(progress) = active.progress_note.as_deref() {
            items.push(format!("active: {}", truncate_line(progress, 120)));
        }
    }
    if let Some(plan) = active_work_plan {
        items.extend(
            plan.items
                .iter()
                .filter(|item| item.status != WorkPlanStepStatus::Completed)
                .map(|item| format!("plan: {}", truncate_line(&item.step, 120))),
        );
    }
    items.extend(queued_waiting.iter().map(|item| {
        format!(
            "queued: {}",
            truncate_line(
                item.summary.as_deref().unwrap_or(&item.delivery_target),
                120,
            )
        )
    }));

    dedup_owned(items, MEMORY_FOLLOWUP_LIMIT)
}

fn collect_memory_tools<'a>(
    recent_tools: &'a [ToolExecutionRecord],
    active_work_item_id: Option<&str>,
) -> Vec<&'a ToolExecutionRecord> {
    collect_work_item_bound_or_legacy(
        recent_tools,
        active_work_item_id,
        MEMORY_TOOL_LIMIT,
        |_| true,
        |record| record.work_item_id.as_deref(),
    )
}

fn collect_work_item_bound_or_legacy<'a, T, P, W>(
    records: &'a [T],
    active_work_item_id: Option<&str>,
    limit: usize,
    mut predicate: P,
    mut work_item_id: W,
) -> Vec<&'a T>
where
    P: FnMut(&T) -> bool,
    W: FnMut(&T) -> Option<&str>,
{
    let mut selected = Vec::new();
    match active_work_item_id {
        Some(active_work_item_id) => {
            for record in records.iter().rev() {
                if predicate(record) && work_item_id(record) == Some(active_work_item_id) {
                    selected.push(record);
                    if selected.len() == limit {
                        return selected;
                    }
                }
            }
            for record in records.iter().rev() {
                if predicate(record) && work_item_id(record).is_none() {
                    selected.push(record);
                    if selected.len() == limit {
                        return selected;
                    }
                }
            }
        }
        None => {
            for record in records.iter().rev() {
                if predicate(record) {
                    selected.push(record);
                    if selected.len() == limit {
                        return selected;
                    }
                }
            }
        }
    }
    selected
}

fn collect_waiting_on(
    active_waiting: &[crate::types::WaitingIntentRecord],
    active_work_item_id: Option<&str>,
    current_closure: &ClosureDecision,
) -> Vec<String> {
    let mut waiting_records = active_waiting.iter().collect::<Vec<_>>();
    waiting_records.sort_by(|left, right| {
        waiting_relevance_rank(left.work_item_id.as_deref(), active_work_item_id)
            .cmp(&waiting_relevance_rank(
                right.work_item_id.as_deref(),
                active_work_item_id,
            ))
            .then_with(|| {
                compare_option_timestamp_desc(left.last_triggered_at, right.last_triggered_at)
            })
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut waiting = waiting_records
        .into_iter()
        .map(|record| {
            if let Some(resource) = record.resource.as_deref() {
                format!(
                    "{} on {}",
                    truncate_line(&record.summary, 100),
                    truncate_line(resource, 80)
                )
            } else {
                truncate_line(&record.summary, 120)
            }
        })
        .take(MEMORY_WAITING_LIMIT)
        .collect::<Vec<_>>();

    if let Some(reason) = current_closure.waiting_reason {
        waiting.push(format!("runtime: {}", format_waiting_reason(reason)));
    }

    dedup_owned(waiting, MEMORY_WAITING_LIMIT)
}

fn collect_working_set_files(recent_tools: &[&ToolExecutionRecord]) -> Vec<String> {
    let mut files = Vec::new();
    for record in recent_tools {
        match record.tool_name.as_str() {
            "ApplyPatch" => {
                if let Some(input) = record
                    .input
                    .as_str()
                    .or_else(|| record.input.get("patch").and_then(Value::as_str))
                {
                    files.extend(extract_patch_files(input));
                }
            }
            _ => {}
        }
    }
    dedup_owned(files, MEMORY_FILE_LIMIT)
}

fn waiting_relevance_rank(
    waiting_work_item_id: Option<&str>,
    active_work_item_id: Option<&str>,
) -> u8 {
    match (active_work_item_id, waiting_work_item_id) {
        (Some(active_work_item_id), Some(waiting_work_item_id))
            if waiting_work_item_id == active_work_item_id =>
        {
            0
        }
        (Some(_), None) => 1,
        (Some(_), Some(_)) => 2,
        (None, _) => 0,
    }
}

fn compare_option_timestamp_desc(
    left: Option<chrono::DateTime<chrono::Utc>>,
    right: Option<chrono::DateTime<chrono::Utc>>,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn extract_patch_files(input: &str) -> Vec<String> {
    let lines = input.lines().collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut pending_rename_from: Option<String> = None;
    let mut index = 0usize;
    while index < lines.len() {
        if let Some(path) = lines[index].strip_prefix("rename from ") {
            pending_rename_from = Some(strip_diff_prefix(path).to_string());
            index += 1;
            continue;
        }
        if let Some(path) = lines[index].strip_prefix("rename to ") {
            if let Some(from) = pending_rename_from.take() {
                push_unique_patch_file(&mut files, from);
                push_unique_patch_file(&mut files, strip_diff_prefix(path).to_string());
            }
            index += 1;
            continue;
        }
        if let Some(old_path) = lines[index].strip_prefix("--- ") {
            if index + 1 < lines.len() {
                if let Some(new_path) = lines[index + 1].strip_prefix("+++ ") {
                    for path in [old_path, new_path] {
                        let path = strip_diff_prefix(path);
                        if path != "/dev/null" {
                            push_unique_patch_file(&mut files, path.to_string());
                        }
                    }
                    index += 2;
                    continue;
                }
            }
        }
        index += 1;
    }
    files
}

fn push_unique_patch_file(files: &mut Vec<String>, path: String) {
    if !files.iter().any(|existing| existing == &path) {
        files.push(path);
    }
}

fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

fn extract_exec_command(record: &ToolExecutionRecord) -> Option<String> {
    record
        .input
        .get("cmd")
        .and_then(Value::as_str)
        .map(|cmd| truncate_line(cmd, 120))
}

fn looks_like_verification(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "verified",
        "verification",
        "cargo test",
        "passed",
        "pass",
        "build",
        "check",
        "fmt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn diff_list(previous: &[String], next: &[String]) -> Vec<String> {
    let previous = previous.iter().collect::<BTreeSet<_>>();
    next.iter()
        .filter(|item| !previous.contains(item))
        .cloned()
        .collect()
}

fn push_changed_field(
    changed_fields: &mut Vec<String>,
    summary_lines: &mut Vec<String>,
    field: &str,
    previous: Option<&str>,
    next: Option<&str>,
    label: &str,
) {
    if previous == next {
        return;
    }
    changed_fields.push(field.to_string());
    let line = match next {
        Some(next) => format!("updated {label}: {}", truncate_line(next, 120)),
        None => format!("cleared {label}"),
    };
    summary_lines.push(line);
}

fn push_changed_vec(
    changed_fields: &mut Vec<String>,
    summary_lines: &mut Vec<String>,
    field: &str,
    previous: &[String],
    next: &[String],
    label: &str,
) {
    if previous == next {
        return;
    }
    changed_fields.push(field.to_string());
    if next.is_empty() {
        summary_lines.push(format!("cleared {label}"));
    } else {
        summary_lines.push(format!(
            "updated {label}: {}",
            truncate_line(&next.join("; "), 120)
        ));
    }
}

fn format_waiting_reason(reason: crate::types::WaitingReason) -> &'static str {
    match reason {
        crate::types::WaitingReason::AwaitingOperatorInput => "awaiting operator input",
        crate::types::WaitingReason::AwaitingExternalChange => "awaiting external change",
        crate::types::WaitingReason::AwaitingTaskResult => "awaiting task result",
        crate::types::WaitingReason::AwaitingTimer => "awaiting timer",
    }
}

fn truncate_line(input: &str, limit: usize) -> String {
    let trimmed = input.trim();
    let mut output = trimmed.chars().take(limit).collect::<String>();
    if trimmed.chars().count() > limit {
        output.push_str("...");
    }
    output
}

fn dedup_owned(values: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for value in values {
        let normalized = value.trim();
        if normalized.is_empty() || !seen.insert(normalized.to_string()) {
            continue;
        }
        output.push(normalized.to_string());
        if output.len() >= limit {
            break;
        }
    }
    output
}

fn limit_vec(values: Vec<String>, limit: usize) -> Vec<String> {
    values.into_iter().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{
        storage::AppStorage,
        types::{
            AgentState, BriefKind, BriefRecord, CallbackDeliveryMode, ClosureDecision, MessageBody,
            MessageEnvelope, MessageKind, MessageOrigin, Priority, RuntimePosture,
            ToolExecutionRecord, ToolExecutionStatus, TrustLevel, WaitingIntentRecord,
            WaitingIntentStatus, WaitingReason, WorkItemRecord, WorkItemStatus, WorkPlanItem,
            WorkPlanSnapshot, WorkPlanStepStatus,
        },
    };

    use super::*;

    fn closure(waiting_reason: Option<WaitingReason>) -> ClosureDecision {
        ClosureDecision {
            outcome: if waiting_reason.is_some() {
                crate::types::ClosureOutcome::Waiting
            } else {
                crate::types::ClosureOutcome::Completed
            },
            waiting_reason,
            work_signal: None,
            runtime_posture: RuntimePosture::Awake,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn derive_working_memory_snapshot_projects_work_state_and_tool_evidence() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut active =
            WorkItemRecord::new("default", "fix benchmark export", WorkItemStatus::Active);
        active.summary = Some("repair benchmark export output".into());
        active.progress_note = Some("keep export format stable".into());
        storage.append_work_item(&active).unwrap();
        storage
            .append_work_plan(&WorkPlanSnapshot::new(
                "default",
                &active.id,
                vec![
                    WorkPlanItem {
                        step: "patch exporter".into(),
                        status: WorkPlanStepStatus::InProgress,
                    },
                    WorkPlanItem {
                        step: "run focused test".into(),
                        status: WorkPlanStepStatus::Pending,
                    },
                ],
            ))
            .unwrap();
        storage
            .append_brief(&BriefRecord::new(
                "default",
                BriefKind::Result,
                "Updated exporter path and verified cargo test --test metrics_export.",
                None,
                None,
            ))
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-1".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                turn_index: 1,
                tool_name: "ApplyPatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({"patch": "--- a/src/export.rs\n+++ b/src/export.rs\n@@ -1,1 +1,1 @@\n-...\n+...\n" }),
                output: json!({}),
                summary: "updated export file".into(),
                invocation_surface: None,
            })
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-2".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                turn_index: 1,
                tool_name: "ExecCommand".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({"cmd": "cargo test --test metrics_export"}),
                output: json!({}),
                summary: "Verified with cargo test --test metrics_export".into(),
                invocation_surface: None,
            })
            .unwrap();
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait_1".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                summary: "wait for CI webhook".into(),
                source: "github".into(),
                resource: Some("pull/1".into()),
                condition: "ci completed".into(),
                delivery_mode: CallbackDeliveryMode::EnqueueMessage,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb_1".into(),
                created_at: Utc::now(),
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();

        let snapshot = derive_working_memory_snapshot(
            &storage,
            &closure(Some(WaitingReason::AwaitingExternalChange)),
        )
        .unwrap();
        assert_eq!(
            snapshot.active_work_item_id.as_deref(),
            Some(active.id.as_str())
        );
        assert!(snapshot
            .current_plan
            .iter()
            .any(|item| item.contains("patch exporter")));
        assert!(snapshot
            .working_set_files
            .contains(&"src/export.rs".to_string()));
        assert!(snapshot
            .waiting_on
            .iter()
            .any(|item| item.contains("wait for CI webhook")));
    }

    #[test]
    fn derive_working_memory_snapshot_uses_waiting_anchor_when_no_active_work_exists() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let queued = WorkItemRecord::new(
            "default",
            "queue follow-up verification",
            WorkItemStatus::Queued,
        );
        let mut waiting = WorkItemRecord::new(
            "default",
            "wait for operator approval",
            WorkItemStatus::Waiting,
        );
        waiting.summary = Some("hold completion until operator confirms".into());
        storage.append_work_item(&queued).unwrap();
        storage.append_work_item(&waiting).unwrap();

        let snapshot = derive_working_memory_snapshot(
            &storage,
            &closure(Some(WaitingReason::AwaitingExternalChange)),
        )
        .unwrap();

        assert_eq!(
            snapshot.active_work_item_id.as_deref(),
            Some(waiting.id.as_str())
        );
        assert_eq!(
            snapshot.delivery_target.as_deref(),
            Some("wait for operator approval")
        );
        assert!(!snapshot
            .pending_followups
            .iter()
            .any(|item| item.contains("hold completion until operator confirms")));
        assert!(snapshot
            .pending_followups
            .iter()
            .any(|item| item.contains("queue follow-up verification")));
    }

    #[test]
    fn derive_working_memory_snapshot_prefers_active_work_bound_briefs_and_tools() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut active =
            WorkItemRecord::new("default", "fix active target", WorkItemStatus::Active);
        active.summary = Some("current active summary".into());
        storage.append_work_item(&active).unwrap();
        let other = WorkItemRecord::new("default", "other target", WorkItemStatus::Queued);
        storage.append_work_item(&other).unwrap();

        storage
            .append_brief(&BriefRecord {
                work_item_id: Some(active.id.clone()),
                ..BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    "current work verification passed",
                    None,
                    None,
                )
            })
            .unwrap();
        storage
            .append_brief(&BriefRecord {
                work_item_id: None,
                ..BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    "legacy unbound benchmark note",
                    None,
                    None,
                )
            })
            .unwrap();
        storage
            .append_brief(&BriefRecord {
                work_item_id: Some(other.id.clone()),
                ..BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    "other work should not leak",
                    None,
                    None,
                )
            })
            .unwrap();

        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-active".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                turn_index: 1,
                tool_name: "ApplyPatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({"patch": "--- a/src/active.rs\n+++ b/src/active.rs\n@@ -1,1 +1,1 @@\n-...\n+...\n" }),
                output: json!({}),
                summary: "updated active file".into(),
                invocation_surface: None,
            })
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-legacy".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 1,
                tool_name: "ApplyPatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({"patch": "--- a/src/legacy.rs\n+++ b/src/legacy.rs\n@@ -1,1 +1,1 @@\n-...\n+...\n" }),
                output: json!({}),
                summary: "updated legacy file".into(),
                invocation_surface: None,
            })
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-other".into(),
                agent_id: "default".into(),
                work_item_id: Some(other.id.clone()),
                turn_index: 1,
                tool_name: "ApplyPatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({"patch": "--- a/src/other.rs\n+++ b/src/other.rs\n@@ -1,1 +1,1 @@\n-...\n+...\n" }),
                output: json!({}),
                summary: "updated other file".into(),
                invocation_surface: None,
            })
            .unwrap();

        let snapshot = derive_working_memory_snapshot(&storage, &closure(None)).unwrap();

        assert!(
            snapshot.recent_decisions.is_empty(),
            "terminal brief prose must not be copied into working memory decisions"
        );
        assert!(snapshot
            .working_set_files
            .contains(&"src/active.rs".to_string()));
        assert!(snapshot
            .working_set_files
            .contains(&"src/legacy.rs".to_string()));
        assert!(!snapshot
            .working_set_files
            .contains(&"src/other.rs".to_string()));
    }

    #[test]
    fn extract_patch_files_includes_unified_diff_rename_only_paths() {
        let files = extract_patch_files(
            "diff --git a/src/old.rs b/src/new.rs\nsimilarity index 100%\nrename from src/old.rs\nrename to src/new.rs\n",
        );
        assert_eq!(files, vec!["src/old.rs", "src/new.rs"]);
    }

    #[test]
    fn derive_working_memory_snapshot_scans_past_trimmed_recent_noise_for_bound_evidence() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let active = WorkItemRecord::new("default", "current target", WorkItemStatus::Active);
        let other = WorkItemRecord::new("default", "other target", WorkItemStatus::Queued);
        storage.append_work_item(&active).unwrap();
        storage.append_work_item(&other).unwrap();

        storage
            .append_brief(&BriefRecord {
                work_item_id: Some(active.id.clone()),
                ..BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    "active evidence survives older scan window",
                    None,
                    None,
                )
            })
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-active-earlier".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                turn_index: 1,
                tool_name: "ApplyPatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({"patch": "--- a/src/active_earlier.rs\n+++ b/src/active_earlier.rs\n@@ -1,1 +1,1 @@\n-...\n+...\n" }),
                output: json!({}),
                summary: "updated active earlier file".into(),
                invocation_surface: None,
            })
            .unwrap();

        for idx in 0..MEMORY_TOOL_LIMIT {
            storage
                .append_brief(&BriefRecord {
                    work_item_id: Some(other.id.clone()),
                    ..BriefRecord::new(
                        "default",
                        BriefKind::Result,
                        format!("other brief noise {idx}"),
                        None,
                        None,
                    )
                })
                .unwrap();
        }
        for idx in 0..MEMORY_TOOL_LIMIT {
            storage
                .append_tool_execution(&ToolExecutionRecord {
                    id: format!("tool-other-noise-{idx}"),
                    agent_id: "default".into(),
                    work_item_id: Some(other.id.clone()),
                    turn_index: 1,
                    tool_name: "ApplyPatch".into(),
                    created_at: Utc::now(),
                    completed_at: Some(Utc::now()),
                    duration_ms: 10,
                    trust: TrustLevel::TrustedOperator,
                    status: ToolExecutionStatus::Success,
                    input: json!({"patch": format!("--- a/src/noise_{idx}.rs\n+++ b/src/noise_{idx}.rs\n@@ -1,1 +1,1 @@\n-...\n+...\n") }),
                    output: json!({}),
                    summary: format!("updated noise file {idx}"),
                    invocation_surface: None,
                })
                .unwrap();
        }

        let snapshot = derive_working_memory_snapshot(&storage, &closure(None)).unwrap();

        assert!(snapshot.recent_decisions.is_empty());
        assert!(snapshot
            .working_set_files
            .contains(&"src/active_earlier.rs".to_string()));
    }

    #[test]
    fn derive_working_memory_snapshot_sorts_waiting_on_by_work_relevance_and_recency() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let base = Utc::now();

        let active =
            WorkItemRecord::new("default", "current waiting target", WorkItemStatus::Active);
        let other = WorkItemRecord::new("default", "other waiting target", WorkItemStatus::Queued);
        storage.append_work_item(&active).unwrap();
        storage.append_work_item(&other).unwrap();

        for waiting in [
            WaitingIntentRecord {
                id: "wait-current-old".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                summary: "current old".into(),
                source: "github".into(),
                resource: None,
                condition: "old".into(),
                delivery_mode: CallbackDeliveryMode::WakeOnly,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-current-old".into(),
                created_at: base,
                cancelled_at: None,
                last_triggered_at: Some(base + chrono::Duration::seconds(1)),
                trigger_count: 1,
                correlation_id: None,
                causation_id: None,
            },
            WaitingIntentRecord {
                id: "wait-current-new".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                summary: "current new".into(),
                source: "github".into(),
                resource: None,
                condition: "new".into(),
                delivery_mode: CallbackDeliveryMode::WakeOnly,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-current-new".into(),
                created_at: base + chrono::Duration::seconds(2),
                cancelled_at: None,
                last_triggered_at: Some(base + chrono::Duration::seconds(6)),
                trigger_count: 2,
                correlation_id: None,
                causation_id: None,
            },
            WaitingIntentRecord {
                id: "wait-legacy-new".into(),
                agent_id: "default".into(),
                work_item_id: None,
                summary: "legacy new".into(),
                source: "github".into(),
                resource: None,
                condition: "legacy new".into(),
                delivery_mode: CallbackDeliveryMode::WakeOnly,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-legacy-new".into(),
                created_at: base + chrono::Duration::seconds(3),
                cancelled_at: None,
                last_triggered_at: Some(base + chrono::Duration::seconds(5)),
                trigger_count: 1,
                correlation_id: None,
                causation_id: None,
            },
            WaitingIntentRecord {
                id: "wait-legacy-old".into(),
                agent_id: "default".into(),
                work_item_id: None,
                summary: "legacy old".into(),
                source: "github".into(),
                resource: None,
                condition: "legacy old".into(),
                delivery_mode: CallbackDeliveryMode::WakeOnly,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-legacy-old".into(),
                created_at: base + chrono::Duration::seconds(4),
                cancelled_at: None,
                last_triggered_at: Some(base + chrono::Duration::seconds(4)),
                trigger_count: 1,
                correlation_id: None,
                causation_id: None,
            },
            WaitingIntentRecord {
                id: "wait-other-newest".into(),
                agent_id: "default".into(),
                work_item_id: Some(other.id.clone()),
                summary: "other newest".into(),
                source: "github".into(),
                resource: None,
                condition: "other".into(),
                delivery_mode: CallbackDeliveryMode::WakeOnly,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-other-newest".into(),
                created_at: base + chrono::Duration::seconds(5),
                cancelled_at: None,
                last_triggered_at: Some(base + chrono::Duration::seconds(7)),
                trigger_count: 3,
                correlation_id: None,
                causation_id: None,
            },
        ] {
            storage.append_waiting_intent(&waiting).unwrap();
        }

        let snapshot = derive_working_memory_snapshot(&storage, &closure(None)).unwrap();

        assert_eq!(
            snapshot.waiting_on,
            vec![
                "current new".to_string(),
                "current old".to_string(),
                "legacy new".to_string(),
                "legacy old".to_string(),
            ]
        );
    }

    #[test]
    fn refresh_working_memory_merges_unprompted_updates_and_resets_after_prompt() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.turn_index = 1;

        let mut active = WorkItemRecord::new("default", "ship docs", WorkItemStatus::Active);
        active.summary = Some("publish working memory docs".into());
        storage.append_work_item(&active).unwrap();
        let trigger = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "document the change".into(),
            },
        );

        refresh_working_memory(
            &storage,
            &mut agent,
            &trigger,
            &closure(None),
            &closure(None),
        )
        .unwrap();
        let pending = agent
            .working_memory
            .pending_working_memory_delta
            .clone()
            .unwrap();
        assert_eq!(pending.from_revision, 0);
        assert_eq!(pending.to_revision, 1);

        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait_2".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                summary: "wait for reviewer".into(),
                source: "github".into(),
                resource: None,
                condition: "review requested".into(),
                delivery_mode: CallbackDeliveryMode::EnqueueMessage,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb_2".into(),
                created_at: Utc::now(),
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();

        refresh_working_memory(
            &storage,
            &mut agent,
            &MessageEnvelope::new(
                "default",
                MessageKind::SystemTick,
                MessageOrigin::System {
                    subsystem: "work_queue".into(),
                },
                TrustLevel::TrustedSystem,
                Priority::Normal,
                MessageBody::Text {
                    text: "continue".into(),
                },
            ),
            &closure(Some(WaitingReason::AwaitingExternalChange)),
            &closure(Some(WaitingReason::AwaitingExternalChange)),
        )
        .unwrap();
        let pending = agent
            .working_memory
            .pending_working_memory_delta
            .clone()
            .unwrap();
        assert_eq!(pending.from_revision, 0);
        assert_eq!(pending.to_revision, 2);

        mark_working_memory_prompted(&mut agent, 2);
        assert!(agent.working_memory.pending_working_memory_delta.is_none());
        assert_eq!(
            agent.working_memory.last_prompted_working_memory_revision,
            Some(2)
        );

        active.progress_note = Some("review arrived".into());
        storage.append_work_item(&active).unwrap();
        refresh_working_memory(
            &storage,
            &mut agent,
            &MessageEnvelope::new(
                "default",
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: "task_1".into(),
                },
                TrustLevel::TrustedSystem,
                Priority::Normal,
                MessageBody::Text {
                    text: "task result".into(),
                },
            ),
            &closure(Some(WaitingReason::AwaitingTaskResult)),
            &closure(None),
        )
        .unwrap();
        let pending = agent
            .working_memory
            .pending_working_memory_delta
            .clone()
            .unwrap();
        assert_eq!(pending.from_revision, 2);
        assert_eq!(pending.to_revision, 3);
    }

    #[test]
    fn refresh_working_memory_scrubs_legacy_prose_fields_without_empty_delta() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.turn_index = 1;

        let mut active = WorkItemRecord::new("default", "ship docs", WorkItemStatus::Active);
        active.summary = Some("publish working memory docs".into());
        storage.append_work_item(&active).unwrap();

        agent.working_memory.current_working_memory = WorkingMemorySnapshot {
            active_work_item_id: Some(active.id.clone()),
            delivery_target: Some(active.delivery_target.clone()),
            work_summary: active.summary.clone(),
            scope_hints: vec!["legacy brief text".into()],
            recent_decisions: vec!["legacy final answer prose".into()],
            ..WorkingMemorySnapshot::default()
        };
        let trigger = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        );

        let refresh = refresh_working_memory(
            &storage,
            &mut agent,
            &trigger,
            &closure(None),
            &closure(None),
        )
        .unwrap();

        assert!(refresh.working_memory_updated);
        assert_eq!(agent.working_memory.working_memory_revision, 0);
        assert!(agent
            .working_memory
            .current_working_memory
            .scope_hints
            .is_empty());
        assert!(agent
            .working_memory
            .current_working_memory
            .recent_decisions
            .is_empty());
        assert!(agent.working_memory.pending_working_memory_delta.is_none());
        assert!(storage
            .read_recent_working_memory_deltas(10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn refresh_working_memory_clears_stale_context_summary_when_memory_is_active() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.context_summary = Some("stale compacted summary".into());

        let active = WorkItemRecord::new("default", "ship memory cleanup", WorkItemStatus::Active);
        storage.append_work_item(&active).unwrap();
        let trigger = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        );

        let refresh = refresh_working_memory(
            &storage,
            &mut agent,
            &trigger,
            &closure(None),
            &closure(None),
        )
        .unwrap();

        assert!(refresh.working_memory_updated);
        assert_eq!(agent.context_summary, None);
        assert_eq!(
            agent.working_memory.current_working_memory.delivery_target,
            Some("ship memory cleanup".into())
        );
    }

    #[test]
    fn derive_turn_memory_delta_only_uses_current_turn_tool_evidence() {
        let previous = WorkingMemorySnapshot::default();
        let next = WorkingMemorySnapshot::default();
        let delta = derive_turn_memory_delta(
            4,
            &previous,
            &next,
            vec![
                ToolExecutionRecord {
                    id: "tool-old".into(),
                    agent_id: "default".into(),
                    work_item_id: None,
                    turn_index: 3,
                    tool_name: "ExecCommand".into(),
                    created_at: Utc::now(),
                    completed_at: Some(Utc::now()),
                    duration_ms: 10,
                    trust: TrustLevel::TrustedOperator,
                    status: crate::types::ToolExecutionStatus::Success,
                    input: json!({"cmd": "cargo test old"}),
                    output: json!({}),
                    summary: "Verified with cargo test old".into(),
                    invocation_surface: None,
                },
                ToolExecutionRecord {
                    id: "tool-new".into(),
                    agent_id: "default".into(),
                    work_item_id: None,
                    turn_index: 4,
                    tool_name: "ExecCommand".into(),
                    created_at: Utc::now(),
                    completed_at: Some(Utc::now()),
                    duration_ms: 10,
                    trust: TrustLevel::TrustedOperator,
                    status: crate::types::ToolExecutionStatus::Success,
                    input: json!({"cmd": "cargo test --test metrics_export"}),
                    output: json!({}),
                    summary: "Verified with cargo test --test metrics_export".into(),
                    invocation_surface: None,
                },
            ],
        );

        assert_eq!(delta.commands, vec!["cargo test --test metrics_export"]);
        assert_eq!(
            delta.verification,
            vec!["Verified with cargo test --test metrics_export"]
        );
    }
}
