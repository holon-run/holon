use std::{cmp::Ordering, collections::BTreeMap};

use chrono::Utc;
use holon_domain::{
    work_item_scheduling::{
        compare_queue_display_order, compare_scheduling_projection_order,
        derive_work_item_scheduling, project_work_item_scheduling, readiness_from_facts,
        WorkItemCandidateClass, WorkItemSchedulingFacts, WorkItemSchedulingProjection,
        WorkItemSchedulingReasonCode,
    },
    WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource, WorkItemPlanStatus,
    WorkItemReadiness, WorkItemRecord, WorkItemSchedulingState, WorkItemState,
};

fn wait(record: &WorkItemRecord, kind: WaitConditionKind) -> WaitConditionRecord {
    let now = Utc::now();
    WaitConditionRecord {
        id: format!("wait-{kind:?}"),
        agent_id: record.agent_id.clone(),
        work_item_id: Some(record.id.clone()),
        status: WaitConditionStatus::Active,
        kind,
        source: Some("test".into()),
        subject_ref: None,
        waiting_for: "test".into(),
        wake_sources: vec![WakeSource::SystemTick],
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}

fn derive(record: &WorkItemRecord, waits: &[WaitConditionRecord]) -> WorkItemSchedulingProjection {
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: record,
        is_current: false,
        is_yielded: false,
        active_wait_conditions: waits,
        trigger_delivery_by_id: &BTreeMap::new(),
    })
}

#[test]
fn golden_precedence_active_wait_outranks_needs_input_and_blocker() {
    let mut record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
    record.plan_status = WorkItemPlanStatus::NeedsInput;
    record.blocked_by = Some("manual".into());
    let waits = [wait(&record, WaitConditionKind::Task)];

    let projection = derive(&record, &waits);

    assert_eq!(
        projection.scheduling_state,
        WorkItemSchedulingState::WaitingTask
    );
    assert_eq!(
        projection.reason_code,
        WorkItemSchedulingReasonCode::ActiveTaskWait
    );
}

#[test]
fn record_only_readiness_is_distinct_from_fact_backed_readiness() {
    let mut record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
    record.blocked_by = Some("manual".into());

    for (kind, expected) in [
        (
            WaitConditionKind::Operator,
            WorkItemReadiness::WaitingForOperator,
        ),
        (WaitConditionKind::Task, WorkItemReadiness::Blocked),
        (WaitConditionKind::Timer, WorkItemReadiness::Blocked),
        (WaitConditionKind::External, WorkItemReadiness::Blocked),
        (WaitConditionKind::System, WorkItemReadiness::Blocked),
    ] {
        let waits = [wait(&record, kind)];
        assert_eq!(record.readiness(), WorkItemReadiness::Runnable);
        assert_eq!(readiness_from_facts(&record, false, &waits), expected);
    }
}

#[test]
fn golden_precedence_completed_is_terminal() {
    let mut record = WorkItemRecord::new("agent", "work", WorkItemState::Completed);
    record.plan_status = WorkItemPlanStatus::Ready;
    let waits = [wait(&record, WaitConditionKind::Operator)];

    let projection = derive(&record, &waits);

    assert_eq!(
        projection.scheduling_state,
        WorkItemSchedulingState::Completed
    );
    assert_eq!(projection.readiness, WorkItemReadiness::Completed);
    assert!(!projection.is_runnable);
    assert!(projection
        .diagnostics
        .contains(&"terminalizing_work_item_has_active_wait".to_string()));
}

#[test]
fn multiple_wait_kinds_are_deterministic_and_diagnostic() {
    let record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
    let waits = [
        wait(&record, WaitConditionKind::External),
        wait(&record, WaitConditionKind::Operator),
        wait(&record, WaitConditionKind::Task),
    ];

    let projection = derive(&record, &waits);

    assert_eq!(
        projection.scheduling_state,
        WorkItemSchedulingState::WaitingTask
    );
    assert!(projection
        .diagnostics
        .contains(&"multiple_active_wait_kinds".to_string()));
}

#[test]
fn yielded_is_never_queued_runnable() {
    let record = WorkItemRecord::new("agent", "work", WorkItemState::Open);
    let projection = derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: &record,
        is_current: false,
        is_yielded: true,
        active_wait_conditions: &[],
        trigger_delivery_by_id: &BTreeMap::new(),
    });

    assert_eq!(
        projection.scheduling_state,
        WorkItemSchedulingState::YieldedToWorkItem
    );
    assert_eq!(projection.candidate_class, WorkItemCandidateClass::Yielded);
    assert!(!projection.is_runnable);
}

#[test]
fn transition_matrix_preserves_terminal_and_wait_invariants() {
    let plan_statuses = [
        WorkItemPlanStatus::Draft,
        WorkItemPlanStatus::Ready,
        WorkItemPlanStatus::NeedsInput,
    ];
    let wait_kinds = [
        None,
        Some(WaitConditionKind::Task),
        Some(WaitConditionKind::Operator),
        Some(WaitConditionKind::Timer),
        Some(WaitConditionKind::External),
        Some(WaitConditionKind::System),
    ];

    for state in [WorkItemState::Open, WorkItemState::Completed] {
        for plan_status in plan_statuses {
            for blocked in [false, true] {
                for yielded in [false, true] {
                    for wait_kind in &wait_kinds {
                        let mut record = WorkItemRecord::new("agent", "matrix", state.clone());
                        record.plan_status = plan_status;
                        record.blocked_by = blocked.then(|| "blocked".into());
                        let waits = wait_kind
                            .clone()
                            .map(|kind| vec![wait(&record, kind)])
                            .unwrap_or_default();
                        let projection = derive_work_item_scheduling(WorkItemSchedulingFacts {
                            work_item: &record,
                            is_current: false,
                            is_yielded: yielded,
                            active_wait_conditions: &waits,
                            trigger_delivery_by_id: &BTreeMap::new(),
                        });

                        if state == WorkItemState::Completed {
                            assert_eq!(
                                projection.scheduling_state,
                                WorkItemSchedulingState::Completed
                            );
                            assert_eq!(projection.readiness, WorkItemReadiness::Completed);
                            assert!(!projection.is_runnable);
                        } else if yielded {
                            assert_eq!(
                                projection.scheduling_state,
                                WorkItemSchedulingState::YieldedToWorkItem
                            );
                            assert_eq!(projection.candidate_class, WorkItemCandidateClass::Yielded);
                        } else if let Some(kind) = wait_kind.as_ref() {
                            let expected = match kind {
                                WaitConditionKind::Task => WorkItemSchedulingState::WaitingTask,
                                WaitConditionKind::Operator => {
                                    WorkItemSchedulingState::WaitingOperator
                                }
                                WaitConditionKind::Timer => WorkItemSchedulingState::WaitingTimer,
                                WaitConditionKind::External => {
                                    WorkItemSchedulingState::WaitingExternal
                                }
                                WaitConditionKind::System => WorkItemSchedulingState::WaitingSystem,
                            };
                            assert_eq!(projection.scheduling_state, expected);
                        }
                    }
                }
            }
        }
    }
}

fn sort_item(id: String, state: WorkItemState) -> WorkItemRecord {
    let mut item = WorkItemRecord::new("web", "sort 汉字\0🦀", state);
    item.id = id;
    item.plan_status = WorkItemPlanStatus::Ready;
    item.created_at = "2026-10-01T00:00:00Z".parse().unwrap();
    item.updated_at = item.created_at;
    item
}

#[test]
fn queue_sort_comparators_have_unique_id_ties_and_match_stable_sort() {
    let classes = [
        WorkItemCandidateClass::CurrentRunnable,
        WorkItemCandidateClass::TriggeredBlocked,
        WorkItemCandidateClass::QueuedRunnable,
        WorkItemCandidateClass::Yielded,
        WorkItemCandidateClass::WaitingForOperator,
        WorkItemCandidateClass::Blocked,
        WorkItemCandidateClass::Completing,
        WorkItemCandidateClass::CompletedRecent,
    ];
    let mut rows = Vec::new();
    for (class_index, class) in classes.into_iter().enumerate() {
        for index in (0..33).rev() {
            let mut record = sort_item(
                format!("{class_index}-{index:03}-汉字\0🦀"),
                WorkItemState::Open,
            );
            record.created_at += chrono::Duration::seconds(index % 3);
            record.updated_at += chrono::Duration::seconds(index % 5);
            if index % 2 == 0 {
                record.blocked_by = Some("manual".into());
            }
            let mut row = project_work_item_scheduling(record, false, false, &[], &|_| None);
            row.candidate_class = class;
            row.last_triggered_at = (index % 2 == 0).then_some(row.work_item.updated_at);
            rows.push(row);
        }
    }
    for (index, left) in rows.iter().enumerate() {
        for right in &rows[index + 1..] {
            assert_ne!(left.id, right.id);
            for order in [
                compare_queue_display_order(left, right),
                compare_scheduling_projection_order(left, right),
            ] {
                assert_ne!(order, Ordering::Equal);
            }
        }
    }
    let mut stable = rows.clone();
    stable.sort_by(compare_scheduling_projection_order);
    rows.sort_unstable_by(compare_scheduling_projection_order);
    assert_eq!(rows, stable);
    let mut records = rows
        .into_iter()
        .map(|row| row.work_item)
        .collect::<Vec<_>>();
    let mut stable = records.clone();
    stable.sort_by(compare_queue_display_order);
    records.sort_unstable_by(compare_queue_display_order);
    assert_eq!(records, stable);
}
