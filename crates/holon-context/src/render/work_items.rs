//! Prompt views over captured WorkItems, plans, and scheduling projections.
use std::collections::BTreeMap;

use holon_domain::{
    work_item_scheduling::{WorkItemCandidateClass, WorkItemSchedulingProjection},
    BriefRecord, TodoItemState, ToolContextProjection, TranscriptEntry, TranscriptEntryKind,
    WaitConditionRecord, WorkItemPlanArtifact, WorkItemRecord, WorkItemRefStatus,
};
use holon_system::utf8::truncate_text;

use super::bounded_inline;
use crate::history::assistant_round_text_preview;

pub trait PlanArtifactLookup {
    fn plan(&self, id: &str) -> Option<&WorkItemPlanArtifact>;
}

impl PlanArtifactLookup for BTreeMap<String, WorkItemPlanArtifact> {
    fn plan(&self, id: &str) -> Option<&WorkItemPlanArtifact> {
        self.get(id)
    }
}

/// Context owns only its existing display windows, not a second queue inventory.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ContextWorkQueue {
    pub current: Option<WorkItemRecord>,
    pub current_runnable: Option<WorkItemSchedulingProjection>,
    pub triggered_blocked: Vec<WorkItemSchedulingProjection>,
    pub queued_runnable: Vec<WorkItemSchedulingProjection>,
    pub yielded: Vec<WorkItemSchedulingProjection>,
    pub waiting_for_operator: Vec<WorkItemSchedulingProjection>,
    pub blocked: Vec<WorkItemSchedulingProjection>,
    pub completing: Vec<WorkItemSchedulingProjection>,
    pub completed_recent: Vec<WorkItemSchedulingProjection>,
}

pub const COMPLETION_SUMMARY_CHAR_LIMIT: usize = 160;

#[derive(Clone, Copy)]
pub struct CompletionReport<'a> {
    pub text: &'a str,
    pub brief_id: Option<&'a str>,
}

pub fn render_current_work_item(
    work_item: &WorkItemRecord,
    scheduling: &WorkItemSchedulingProjection,
    plan_artifact: Option<&WorkItemPlanArtifact>,
    active_wait_conditions: &[WaitConditionRecord],
) -> String {
    let mut lines = vec![
        "Current work item:".to_string(),
        format!("- Id: {}", work_item.id),
        format!("- State: {:?}", work_item.state),
        format!("- Scheduling state: {:?}", scheduling.scheduling_state),
        format!("- Readiness: {:?}", scheduling.readiness),
        format!("- Reason: {:?}", scheduling.reason_code),
        format!("- Objective: {}", work_item.objective),
        format!(
            "- Plan status: {}",
            work_item_plan_status_label(work_item.plan_status)
        ),
    ];
    lines.extend(render_work_item_plan_artifact_lines(plan_artifact, "- "));
    if !work_item.todo_list.is_empty() {
        lines.push("- Todo list:".to_string());
        lines.extend(work_item.todo_list.iter().map(|item| {
            let state = match item.state {
                TodoItemState::Pending => "pending",
                TodoItemState::InProgress => "in_progress",
                TodoItemState::Completed => "completed",
            };
            format!("  - [{state}] {}", item.text)
        }));
    }
    if let Some(blocked_by) = work_item.blocked_by.as_deref() {
        lines.push(format!("- Blocked by: {blocked_by}"));
    }
    let waits = active_wait_conditions
        .iter()
        .filter(|condition| condition.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .collect::<Vec<_>>();
    if !waits.is_empty() {
        lines.push("- Active waits:".to_string());
        lines.extend(waits.into_iter().take(4).map(|condition| {
            let subject = condition
                .subject_ref
                .as_deref()
                .map(|subject| format!(" :: subject_ref={subject}"))
                .unwrap_or_default();
            format!("  - {}{subject}", condition.waiting_for)
        }));
    }
    lines.join("\n")
}

pub fn render_compact_current_work_item(work_item: &WorkItemRecord) -> String {
    format!(
        "Current work item:\n- Id: {}\n- Objective: {}",
        work_item.id,
        bounded_inline(&work_item.objective, 96)
    )
}

pub fn render_empty_current_work_item() -> String {
    "Current work item: none.\nNo focused WorkItem is attached to this turn.".to_string()
}

pub fn render_current_work_refs(
    work_item: &WorkItemRecord,
    max_active_refs: usize,
) -> Option<String> {
    let refs = work_item
        .work_refs
        .iter()
        .filter(|work_ref| work_ref.status == WorkItemRefStatus::Active)
        .take(max_active_refs)
        .collect::<Vec<_>>();
    if refs.is_empty() {
        return None;
    }
    let mut lines = vec!["Current WorkItem refs. Reopen only when needed:".to_string()];
    for work_ref in refs {
        let title = work_ref
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(&work_ref.ref_id);
        let source = work_ref
            .source_ref
            .as_deref()
            .map(|source_ref| format!(" :: source_ref={source_ref}"))
            .unwrap_or_default();
        lines.push(format!(
            "- [{}] {} :: ref={} :: reason={}{}",
            work_ref_kind_label(work_ref.kind),
            truncate_text(title, 120),
            work_ref.ref_id,
            truncate_text(&work_ref.reason, 140),
            source
        ));
    }
    Some(lines.join("\n"))
}

fn work_ref_kind_label(kind: holon_domain::WorkItemRefKind) -> &'static str {
    match kind {
        holon_domain::WorkItemRefKind::File => "file",
        holon_domain::WorkItemRefKind::ToolExecution => "tool_execution",
        holon_domain::WorkItemRefKind::Issue => "issue",
        holon_domain::WorkItemRefKind::Pr => "pr",
        holon_domain::WorkItemRefKind::Url => "url",
        holon_domain::WorkItemRefKind::Memory => "memory",
        holon_domain::WorkItemRefKind::Task => "task",
        holon_domain::WorkItemRefKind::Wait => "wait",
        holon_domain::WorkItemRefKind::Workspace => "workspace",
        holon_domain::WorkItemRefKind::Other => "other",
    }
}

fn work_item_plan_status_label(status: holon_domain::WorkItemPlanStatus) -> &'static str {
    match status {
        holon_domain::WorkItemPlanStatus::Draft => "draft",
        holon_domain::WorkItemPlanStatus::Ready => "ready",
        holon_domain::WorkItemPlanStatus::NeedsInput => "needs_input",
    }
}

pub fn render_current_work_item_process_trace(
    work_item: &WorkItemRecord,
    briefs: &[impl std::borrow::Borrow<BriefRecord>],
    tools: &[impl std::borrow::Borrow<ToolContextProjection>],
    transcript: &[impl std::borrow::Borrow<TranscriptEntry>],
) -> Option<String> {
    let mut lines = vec!["Recent current WorkItem process trace:".to_string()];
    let mut added = 0usize;
    for brief in briefs
        .iter()
        .map(std::borrow::Borrow::<BriefRecord>::borrow)
        .rev()
        .filter(|brief| brief.agent_id == work_item.agent_id)
        .filter(|brief| brief.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .take(3)
    {
        lines.push(format!(
            "- brief:{:?} {}",
            brief.kind,
            truncate_text(&brief.text.replace('\n', " "), 180)
        ));
        added += 1;
    }
    for tool in tools
        .iter()
        .map(std::borrow::Borrow::<ToolContextProjection>::borrow)
        .rev()
        .filter(|tool| tool.agent_id == work_item.agent_id)
        .filter(|tool| tool.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .take(4)
    {
        lines.push(format!(
            "- tool:{} {}",
            tool.tool_name,
            truncate_text(&tool.summary.replace('\n', " "), 180)
        ));
        added += 1;
    }
    for entry in transcript
        .iter()
        .map(std::borrow::Borrow::<TranscriptEntry>::borrow)
        .rev()
        .filter(|entry| entry.agent_id == work_item.agent_id)
        .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
        .filter(|entry| entry.data["work_item_id"].as_str() == Some(work_item.id.as_str()))
        .take(2)
    {
        if let Some(text) = assistant_round_text_preview(entry) {
            lines.push(format!("- assistant_round: {}", truncate_text(&text, 180)));
            added += 1;
        }
    }
    (added > 0).then(|| lines.join("\n"))
}

pub fn render_work_item_candidates<'a>(
    projection: &'a ContextWorkQueue,
    displayed_work_item_id: Option<&str>,
    completion_reports: impl Fn(&'a WorkItemRecord) -> Option<CompletionReport<'a>>,
    plan_observations: &impl PlanArtifactLookup,
) -> Option<String> {
    let mut lines = vec!["Work item candidates by scheduler ranking:".to_string()];
    append_candidate_group(
        &mut lines,
        "Triggered work items:",
        &projection.triggered_blocked,
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    append_candidate_group(
        &mut lines,
        "Focused runnable work items:",
        projection.current_runnable.as_slice(),
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    append_candidate_group(
        &mut lines,
        "Queued runnable work items:",
        &projection.queued_runnable,
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    append_candidate_group(
        &mut lines,
        "Parked/Yielded work items:",
        &projection.yielded,
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    append_candidate_group(
        &mut lines,
        "Waiting for operator:",
        &projection.waiting_for_operator,
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    append_candidate_group(
        &mut lines,
        "Blocked work items:",
        &projection.blocked,
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    append_candidate_group(
        &mut lines,
        "Completing work items:",
        &projection.completing,
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    append_candidate_group(
        &mut lines,
        "Recently completed work items:",
        &projection.completed_recent,
        displayed_work_item_id,
        &completion_reports,
        plan_observations,
    );
    if lines.len() == 1 {
        return None;
    }
    Some(lines.join("\n"))
}

fn append_candidate_group<'a>(
    lines: &mut Vec<String>,
    title: &str,
    items: &'a [WorkItemSchedulingProjection],
    displayed_work_item_id: Option<&str>,
    completion_reports: &impl Fn(&'a WorkItemRecord) -> Option<CompletionReport<'a>>,
    plan_observations: &impl PlanArtifactLookup,
) {
    if items.is_empty() {
        return;
    }
    let items = items
        .iter()
        .filter(|item| Some(item.record().id.as_str()) != displayed_work_item_id)
        .collect::<Vec<_>>();
    if items.is_empty() {
        return;
    }
    let mut title_pushed = false;
    for item in items {
        let record = item.record();
        let completion_report = if record.state == holon_domain::WorkItemState::Completed {
            let report = completion_reports(record);
            if report.is_none() {
                continue;
            }
            report
        } else {
            None
        };
        if !title_pushed {
            lines.push(title.to_string());
            title_pushed = true;
        }
        let view = match item.candidate_class {
            WorkItemCandidateClass::TriggeredBlocked => "triggered_blocked",
            WorkItemCandidateClass::QueuedRunnable => "queued_runnable",
            WorkItemCandidateClass::Yielded => "yielded",
            WorkItemCandidateClass::WaitingForOperator => "waiting_for_operator",
            WorkItemCandidateClass::Blocked => "blocked",
            WorkItemCandidateClass::Completing => "completing",
            WorkItemCandidateClass::CompletedRecent => "completed_recent",
            WorkItemCandidateClass::CurrentRunnable => "current_runnable",
        };
        let mut summary = format!("- [{view}] {} :: {}", record.id, record.objective);
        if let Some(todo) = item.current_todo.as_ref() {
            summary.push_str(&format!(" :: current_todo={}", todo.text));
        }
        if let Some(triggered_at) = item.last_triggered_at {
            summary.push_str(&format!(" :: last_triggered_at={triggered_at}"));
        }
        if let Some(blocked_by) = record.blocked_by.as_deref() {
            summary.push_str(&format!(" :: blocked_by={blocked_by}"));
        }
        lines.push(summary);
        if let Some(report) = completion_report {
            if let Some(brief_id) = report.brief_id {
                lines.push(format!("  - Result ref: brief:{brief_id}"));
            }
            lines.push(format!(
                "  - Completion summary: {}",
                truncate_text(
                    &report.text.replace('\n', " "),
                    COMPLETION_SUMMARY_CHAR_LIMIT
                )
            ));
        }
        if let Some(plan_ref) = render_work_item_plan_ref(record, plan_observations) {
            lines.push(format!("  - {plan_ref}"));
        }
    }
}

fn render_work_item_plan_artifact_lines(
    plan_artifact: Option<&WorkItemPlanArtifact>,
    prefix: &str,
) -> Vec<String> {
    let Some(plan_artifact) = plan_artifact else {
        return Vec::new();
    };
    let preview_indent = " ".repeat(prefix.chars().count());
    let mut lines = vec![
        format!("{prefix}Plan artifact: {}", plan_artifact.path.display()),
        format!(
            "{prefix}Plan preview complete: {}",
            plan_artifact.preview_complete
        ),
    ];
    if !plan_artifact.preview.is_empty() {
        lines.push(format!("{prefix}Plan preview:"));
        lines.extend(
            plan_artifact
                .preview
                .lines()
                .map(|line| format!("{preview_indent}{line}")),
        );
    }
    lines
}

fn render_work_item_plan_ref(
    work_item: &WorkItemRecord,
    plan_observations: &impl PlanArtifactLookup,
) -> Option<String> {
    if let Some(artifact) = work_item.plan_artifact.as_ref() {
        return Some(format!("Plan ref: {}", artifact.path.display()));
    }

    plan_observations
        .plan(&work_item.id)
        .map(|artifact| format!("Plan ref: {}", artifact.path.display()))
}

#[cfg(test)]
mod tests;
