use super::*;
use holon_domain::{
    work_item_scheduling::{WorkItemFocus, WorkItemSchedulingReasonCode},
    WorkItemReadiness, WorkItemSchedulingState, WorkItemState,
};

fn candidate(
    id: &str,
    candidate_class: WorkItemCandidateClass,
    state: WorkItemState,
) -> WorkItemSchedulingProjection {
    let mut work_item = WorkItemRecord::new("agent", format!("{id} objective"), state);
    work_item.id = id.into();
    WorkItemSchedulingProjection {
        work_item,
        scheduling_state: WorkItemSchedulingState::Runnable,
        readiness: WorkItemReadiness::Runnable,
        candidate_class,
        focus: WorkItemFocus::Queued,
        is_current: false,
        is_runnable: true,
        has_active_waits: false,
        has_active_task_waits: false,
        has_triggered_waits: false,
        last_triggered_at: None,
        current_todo: None,
        active_wait_conditions: Vec::new(),
        reason_code: WorkItemSchedulingReasonCode::Runnable,
        diagnostics: Vec::new(),
    }
}

#[test]
fn candidates_follow_prepared_ranking_and_omit_unreported_completion() {
    let queue = ContextWorkQueue {
        triggered_blocked: vec![candidate(
            "triggered",
            WorkItemCandidateClass::TriggeredBlocked,
            WorkItemState::Open,
        )],
        current_runnable: Some(candidate(
            "current",
            WorkItemCandidateClass::CurrentRunnable,
            WorkItemState::Open,
        )),
        queued_runnable: vec![candidate(
            "queued",
            WorkItemCandidateClass::QueuedRunnable,
            WorkItemState::Open,
        )],
        completed_recent: vec![
            candidate(
                "missing",
                WorkItemCandidateClass::CompletedRecent,
                WorkItemState::Completed,
            ),
            candidate(
                "reported",
                WorkItemCandidateClass::CompletedRecent,
                WorkItemState::Completed,
            ),
        ],
        ..Default::default()
    };
    let content = render_work_item_candidates(
        &queue,
        Some("current"),
        |record| {
            (record.id == "reported").then_some(CompletionReport {
                text: "First line\nSecond line",
                brief_id: Some("report"),
            })
        },
        &BTreeMap::new(),
    );
    assert_eq!(
        content.as_deref(),
        Some(concat!(
            "Work item candidates by scheduler ranking:\n",
            "Triggered work items:\n",
            "- [triggered_blocked] triggered :: triggered objective\n",
            "Queued runnable work items:\n",
            "- [queued_runnable] queued :: queued objective\n",
            "Recently completed work items:\n",
            "- [completed_recent] reported :: reported objective\n",
            "  - Result ref: brief:report\n",
            "  - Completion summary: First line Second line"
        ))
    );
}
