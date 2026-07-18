//! Shared WorkItem scheduling projection and queue read model.

use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{
    TodoItem, TodoItemState, WaitConditionKind, WaitConditionRecord, WaitConditionSummary,
    WakeSource, WorkItemPlanStatus, WorkItemReadiness, WorkItemRecord, WorkItemSchedulingState,
    WorkItemState,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemCandidateClass {
    CurrentRunnable,
    TriggeredBlocked,
    QueuedRunnable,
    WaitingForOperator,
    Yielded,
    Blocked,
    CompletedRecent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemFocus {
    Current,
    Queued,
    Yielded,
    Blocked,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemSchedulingReasonCode {
    Completed,
    ContinuationYielded,
    ActiveTaskWait,
    ActiveOperatorWait,
    ActiveTimerWait,
    ActiveExternalWait,
    ActiveSystemWait,
    ManualBlocker,
    PlanNeedsInput,
    Runnable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemSchedulingProjection {
    #[serde(flatten)]
    pub work_item: WorkItemRecord,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub focus: WorkItemFocus,
    pub is_current: bool,
    pub is_runnable: bool,
    pub has_active_waits: bool,
    pub has_active_task_waits: bool,
    pub has_triggered_waits: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_triggered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_todo: Option<TodoItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_wait_conditions: Vec<WaitConditionSummary>,
    pub reason_code: WorkItemSchedulingReasonCode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

impl WorkItemSchedulingProjection {
    pub fn record(&self) -> &WorkItemRecord {
        &self.work_item
    }

    pub fn posture_reason(&self) -> String {
        let label = if self.is_current {
            "current WorkItem"
        } else {
            "queued WorkItem"
        };
        format!(
            "{label} {} is {:?}",
            self.work_item.id, self.scheduling_state
        )
    }
}

impl Deref for WorkItemSchedulingProjection {
    type Target = WorkItemRecord;

    fn deref(&self) -> &Self::Target {
        &self.work_item
    }
}

impl DerefMut for WorkItemSchedulingProjection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.work_item
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkQueueReadModel {
    pub current: Option<WorkItemRecord>,
    pub queued_blocked: Vec<WorkItemRecord>,
    pub items: Vec<WorkItemSchedulingProjection>,
    pub current_runnable: Option<WorkItemSchedulingProjection>,
    pub triggered_blocked: Vec<WorkItemSchedulingProjection>,
    pub queued_runnable: Vec<WorkItemSchedulingProjection>,
    pub yielded: Vec<WorkItemSchedulingProjection>,
    pub waiting_for_operator: Vec<WorkItemSchedulingProjection>,
    pub blocked: Vec<WorkItemSchedulingProjection>,
    pub completed_recent: Vec<WorkItemSchedulingProjection>,
}

impl WorkQueueReadModel {
    pub fn has_non_current_candidates(&self) -> bool {
        self.triggered_blocked.iter().any(|item| !item.is_current)
            || self.queued_runnable.iter().any(|item| !item.is_current)
            || self.yielded.iter().any(|item| !item.is_current)
            || self
                .waiting_for_operator
                .iter()
                .any(|item| !item.is_current)
            || self.blocked.iter().any(|item| !item.is_current)
            || self.completed_recent.iter().any(|item| !item.is_current)
    }
}

impl WorkItemRecord {
    pub fn readiness(&self) -> WorkItemReadiness {
        record_only_readiness(self)
    }

    pub fn is_runnable(&self) -> bool {
        self.readiness() == WorkItemReadiness::Runnable
    }

    pub fn is_waiting_for_operator(&self) -> bool {
        self.readiness() == WorkItemReadiness::WaitingForOperator
    }
}

pub struct WorkItemSchedulingFacts<'a> {
    pub work_item: &'a WorkItemRecord,
    pub is_current: bool,
    pub is_yielded: bool,
    pub active_wait_conditions: &'a [WaitConditionRecord],
    pub trigger_delivery_by_id: &'a BTreeMap<String, DateTime<Utc>>,
}

#[derive(Debug, Default)]
struct ActiveWaitConditionStates {
    task: bool,
    external: bool,
    operator: bool,
    timer: bool,
    system: bool,
    last_triggered_at: Option<DateTime<Utc>>,
}

impl ActiveWaitConditionStates {
    fn record(
        &mut self,
        condition: &WaitConditionRecord,
        trigger_delivery_by_id: &BTreeMap<String, DateTime<Utc>>,
    ) {
        match condition.kind {
            WaitConditionKind::Task => self.task = true,
            WaitConditionKind::External => self.external = true,
            WaitConditionKind::Operator => self.operator = true,
            WaitConditionKind::Timer => self.timer = true,
            WaitConditionKind::System => self.system = true,
        }
        for wake_source in &condition.wake_sources {
            let WakeSource::ExternalIngress {
                external_trigger_id: Some(external_trigger_id),
            } = wake_source
            else {
                continue;
            };
            if let Some(delivered_at) = trigger_delivery_by_id.get(external_trigger_id) {
                self.last_triggered_at = Some(
                    self.last_triggered_at
                        .map_or(*delivered_at, |current| current.max(*delivered_at)),
                );
            }
        }
    }

    fn scheduling_state(&self) -> Option<WorkItemSchedulingState> {
        if self.task {
            Some(WorkItemSchedulingState::WaitingTask)
        } else if self.operator {
            Some(WorkItemSchedulingState::WaitingOperator)
        } else if self.timer {
            Some(WorkItemSchedulingState::WaitingTimer)
        } else if self.external {
            Some(WorkItemSchedulingState::WaitingExternal)
        } else if self.system {
            Some(WorkItemSchedulingState::WaitingSystem)
        } else {
            None
        }
    }

    fn kind_count(&self) -> usize {
        [
            self.task,
            self.operator,
            self.timer,
            self.external,
            self.system,
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
    }
}

pub fn derive_work_item_scheduling(
    facts: WorkItemSchedulingFacts<'_>,
) -> WorkItemSchedulingProjection {
    let mut waits = facts
        .active_wait_conditions
        .iter()
        .filter(|condition| condition.work_item_id.as_deref() == Some(facts.work_item.id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    waits.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut wait_states = ActiveWaitConditionStates::default();
    for wait in &waits {
        wait_states.record(wait, facts.trigger_delivery_by_id);
    }

    let mut diagnostics = Vec::new();
    if facts.work_item.state == WorkItemState::Completed && facts.is_current {
        diagnostics.push("completed_work_item_marked_current".into());
    }
    if facts.is_yielded && facts.is_current {
        diagnostics.push("yielded_work_item_marked_current".into());
    }
    if facts.work_item.state == WorkItemState::Completed && !waits.is_empty() {
        diagnostics.push("completed_work_item_has_active_wait".into());
    }
    if wait_states.kind_count() > 1 {
        diagnostics.push("multiple_active_wait_kinds".into());
    }

    let (scheduling_state, reason_code) = if facts.work_item.state == WorkItemState::Completed {
        (
            WorkItemSchedulingState::Completed,
            WorkItemSchedulingReasonCode::Completed,
        )
    } else if facts.is_yielded {
        (
            WorkItemSchedulingState::YieldedToWorkItem,
            WorkItemSchedulingReasonCode::ContinuationYielded,
        )
    } else if let Some(wait_state) = wait_states.scheduling_state() {
        let reason = match wait_state {
            WorkItemSchedulingState::WaitingTask => WorkItemSchedulingReasonCode::ActiveTaskWait,
            WorkItemSchedulingState::WaitingOperator => {
                WorkItemSchedulingReasonCode::ActiveOperatorWait
            }
            WorkItemSchedulingState::WaitingTimer => WorkItemSchedulingReasonCode::ActiveTimerWait,
            WorkItemSchedulingState::WaitingExternal => {
                WorkItemSchedulingReasonCode::ActiveExternalWait
            }
            WorkItemSchedulingState::WaitingSystem => {
                WorkItemSchedulingReasonCode::ActiveSystemWait
            }
            _ => unreachable!("active waits only project waiting states"),
        };
        (wait_state, reason)
    } else if facts.work_item.blocked_by.is_some() {
        (
            WorkItemSchedulingState::Blocked,
            WorkItemSchedulingReasonCode::ManualBlocker,
        )
    } else if facts.work_item.plan_status == WorkItemPlanStatus::NeedsInput {
        (
            WorkItemSchedulingState::WaitingOperator,
            WorkItemSchedulingReasonCode::PlanNeedsInput,
        )
    } else {
        (
            WorkItemSchedulingState::Runnable,
            WorkItemSchedulingReasonCode::Runnable,
        )
    };
    let readiness = readiness_for_scheduling_state(scheduling_state);
    let is_current = facts.is_current && facts.work_item.state == WorkItemState::Open;
    let focus = if facts.work_item.state == WorkItemState::Completed {
        WorkItemFocus::Completed
    } else if is_current {
        WorkItemFocus::Current
    } else if facts.is_yielded {
        WorkItemFocus::Yielded
    } else if facts.work_item.blocked_by.is_some() {
        WorkItemFocus::Blocked
    } else {
        WorkItemFocus::Queued
    };
    let has_triggered_waits = wait_states.last_triggered_at.is_some();
    let candidate_class = if is_current && scheduling_state == WorkItemSchedulingState::Runnable {
        WorkItemCandidateClass::CurrentRunnable
    } else if facts.work_item.state == WorkItemState::Completed {
        WorkItemCandidateClass::CompletedRecent
    } else if has_triggered_waits && facts.work_item.blocked_by.is_some() {
        WorkItemCandidateClass::TriggeredBlocked
    } else if scheduling_state == WorkItemSchedulingState::Runnable {
        WorkItemCandidateClass::QueuedRunnable
    } else if scheduling_state == WorkItemSchedulingState::YieldedToWorkItem {
        WorkItemCandidateClass::Yielded
    } else if scheduling_state == WorkItemSchedulingState::WaitingOperator {
        WorkItemCandidateClass::WaitingForOperator
    } else {
        WorkItemCandidateClass::Blocked
    };

    WorkItemSchedulingProjection {
        current_todo: current_todo(facts.work_item),
        work_item: facts.work_item.clone(),
        scheduling_state,
        readiness,
        candidate_class,
        focus,
        is_current,
        is_runnable: readiness == WorkItemReadiness::Runnable,
        has_active_waits: !waits.is_empty(),
        has_active_task_waits: wait_states.task,
        has_triggered_waits,
        last_triggered_at: wait_states.last_triggered_at,
        active_wait_conditions: waits.into_iter().map(WaitConditionSummary::from).collect(),
        reason_code,
        diagnostics,
    }
}

pub fn record_only_readiness(record: &WorkItemRecord) -> WorkItemReadiness {
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: record,
        is_current: false,
        is_yielded: false,
        active_wait_conditions: &[],
        trigger_delivery_by_id: &BTreeMap::new(),
    })
    .readiness
}

pub fn readiness_for_scheduling_state(state: WorkItemSchedulingState) -> WorkItemReadiness {
    match state {
        WorkItemSchedulingState::Runnable => WorkItemReadiness::Runnable,
        WorkItemSchedulingState::YieldedToWorkItem => WorkItemReadiness::Yielded,
        WorkItemSchedulingState::WaitingOperator => WorkItemReadiness::WaitingForOperator,
        WorkItemSchedulingState::WaitingTask
        | WorkItemSchedulingState::WaitingExternal
        | WorkItemSchedulingState::WaitingTimer
        | WorkItemSchedulingState::WaitingSystem
        | WorkItemSchedulingState::Blocked => WorkItemReadiness::Blocked,
        WorkItemSchedulingState::Completed => WorkItemReadiness::Completed,
    }
}

pub fn compare_scheduling_projection_order(
    left: &WorkItemSchedulingProjection,
    right: &WorkItemSchedulingProjection,
) -> std::cmp::Ordering {
    candidate_class_rank(left.candidate_class)
        .cmp(&candidate_class_rank(right.candidate_class))
        .then_with(|| match left.candidate_class {
            WorkItemCandidateClass::TriggeredBlocked => {
                compare_timestamp_desc_option(left.last_triggered_at, right.last_triggered_at)
                    .then_with(|| {
                        compare_timestamp_desc(
                            left.work_item.updated_at,
                            right.work_item.updated_at,
                        )
                    })
            }
            WorkItemCandidateClass::QueuedRunnable => left
                .work_item
                .updated_at
                .cmp(&right.work_item.updated_at)
                .then_with(|| left.work_item.created_at.cmp(&right.work_item.created_at)),
            WorkItemCandidateClass::Yielded
            | WorkItemCandidateClass::WaitingForOperator
            | WorkItemCandidateClass::Blocked
            | WorkItemCandidateClass::CompletedRecent => {
                compare_timestamp_desc(left.work_item.updated_at, right.work_item.updated_at)
            }
            WorkItemCandidateClass::CurrentRunnable => std::cmp::Ordering::Equal,
        })
        .then_with(|| left.work_item.id.cmp(&right.work_item.id))
}

pub fn compare_queue_display_order(
    left: &WorkItemRecord,
    right: &WorkItemRecord,
) -> std::cmp::Ordering {
    u8::from(left.blocked_by.is_some())
        .cmp(&u8::from(right.blocked_by.is_some()))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.updated_at.cmp(&right.updated_at))
        .then_with(|| left.id.cmp(&right.id))
}

pub fn candidate_class_rank(class: WorkItemCandidateClass) -> u8 {
    match class {
        WorkItemCandidateClass::CurrentRunnable => 0,
        WorkItemCandidateClass::TriggeredBlocked => 1,
        WorkItemCandidateClass::QueuedRunnable => 2,
        WorkItemCandidateClass::Yielded => 3,
        WorkItemCandidateClass::WaitingForOperator => 4,
        WorkItemCandidateClass::Blocked => 5,
        WorkItemCandidateClass::CompletedRecent => 6,
    }
}

fn compare_timestamp_desc(left: DateTime<Utc>, right: DateTime<Utc>) -> std::cmp::Ordering {
    right.cmp(&left)
}

fn compare_timestamp_desc_option(
    left: Option<DateTime<Utc>>,
    right: Option<DateTime<Utc>>,
) -> std::cmp::Ordering {
    right.cmp(&left)
}

pub fn current_todo(record: &WorkItemRecord) -> Option<TodoItem> {
    record
        .todo_list
        .iter()
        .find(|item| item.state == TodoItemState::InProgress)
        .or_else(|| {
            record
                .todo_list
                .iter()
                .find(|item| item.state == TodoItemState::Pending)
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WaitConditionStatus, WakeSource};

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
        }
    }

    fn derive(
        record: &WorkItemRecord,
        waits: &[WaitConditionRecord],
    ) -> WorkItemSchedulingProjection {
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
            .contains(&"completed_work_item_has_active_wait".to_string()));
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
                                assert_eq!(
                                    projection.candidate_class,
                                    WorkItemCandidateClass::Yielded
                                );
                            } else if let Some(kind) = wait_kind.as_ref() {
                                let expected = match kind {
                                    WaitConditionKind::Task => WorkItemSchedulingState::WaitingTask,
                                    WaitConditionKind::Operator => {
                                        WorkItemSchedulingState::WaitingOperator
                                    }
                                    WaitConditionKind::Timer => {
                                        WorkItemSchedulingState::WaitingTimer
                                    }
                                    WaitConditionKind::External => {
                                        WorkItemSchedulingState::WaitingExternal
                                    }
                                    WaitConditionKind::System => {
                                        WorkItemSchedulingState::WaitingSystem
                                    }
                                };
                                assert_eq!(projection.scheduling_state, expected);
                            }
                        }
                    }
                }
            }
        }
    }
}
