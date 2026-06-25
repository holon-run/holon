//! WorkItem readiness projection, scheduling display ordering, and work queue prompt projection.

use chrono::{DateTime, Utc};

use crate::types::{
    TodoItem, TodoItemState, WaitConditionKind, WaitConditionRecord, WakeSource, WorkItemReadiness,
    WorkItemRecord, WorkItemSchedulingState,
};

#[derive(Debug, Clone, Default)]
pub struct WorkQueuePromptProjection {
    pub current: Option<WorkItemRecord>,
    pub queued_blocked: Vec<WorkItemRecord>,
    pub readiness: Vec<WorkItemReadinessProjection>,
    pub current_runnable: Option<WorkItemReadinessProjection>,
    pub triggered_blocked: Vec<WorkItemReadinessProjection>,
    pub queued_runnable: Vec<WorkItemReadinessProjection>,
    pub yielded: Vec<WorkItemReadinessProjection>,
    pub waiting_for_operator: Vec<WorkItemReadinessProjection>,
    pub blocked: Vec<WorkItemReadinessProjection>,
    pub completed_recent: Vec<WorkItemReadinessProjection>,
}

impl WorkQueuePromptProjection {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItemReadinessProjection {
    pub work_item: WorkItemRecord,
    pub scheduling_state: WorkItemSchedulingState,
    pub readiness: WorkItemReadiness,
    pub candidate_class: WorkItemCandidateClass,
    pub is_current: bool,
    pub has_active_waits: bool,
    pub has_active_task_waits: bool,
    pub has_triggered_waits: bool,
    pub last_triggered_at: Option<DateTime<Utc>>,
    pub current_todo: Option<TodoItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItemCandidateClass {
    CurrentRunnable,
    TriggeredBlocked,
    QueuedRunnable,
    WaitingForOperator,
    Yielded,
    Blocked,
    CompletedRecent,
}

#[derive(Debug, Default)]
pub(crate) struct ActiveWaitConditionStates {
    pub(crate) task: bool,
    pub(crate) external: bool,
    pub(crate) operator: bool,
    pub(crate) timer: bool,
    pub(crate) system: bool,
    pub(crate) last_triggered_at: Option<DateTime<Utc>>,
}

impl ActiveWaitConditionStates {
    pub(crate) fn record(
        &mut self,
        condition: &WaitConditionRecord,
        trigger_delivery_by_id: &std::collections::BTreeMap<String, DateTime<Utc>>,
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

    pub(crate) fn scheduling_state(&self) -> Option<WorkItemSchedulingState> {
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
}

impl WorkItemReadinessProjection {
    pub fn record(&self) -> &WorkItemRecord {
        &self.work_item
    }

    pub(super) fn posture_reason(&self) -> String {
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

pub(crate) fn compare_readiness_projection_order(
    left: &WorkItemReadinessProjection,
    right: &WorkItemReadinessProjection,
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
            WorkItemCandidateClass::QueuedRunnable => {
                compare_timestamp_asc(left.work_item.updated_at, right.work_item.updated_at)
                    .then_with(|| {
                        compare_timestamp_asc(left.work_item.created_at, right.work_item.created_at)
                    })
            }
            WorkItemCandidateClass::Yielded => {
                compare_timestamp_desc(left.work_item.updated_at, right.work_item.updated_at)
            }
            WorkItemCandidateClass::WaitingForOperator
            | WorkItemCandidateClass::Blocked
            | WorkItemCandidateClass::CompletedRecent => {
                compare_timestamp_desc(left.work_item.updated_at, right.work_item.updated_at)
            }
            WorkItemCandidateClass::CurrentRunnable => std::cmp::Ordering::Equal,
        })
        .then_with(|| left.work_item.id.cmp(&right.work_item.id))
}

pub(crate) fn compare_queue_display_order(
    left: &WorkItemRecord,
    right: &WorkItemRecord,
) -> std::cmp::Ordering {
    blocked_rank(left)
        .cmp(&blocked_rank(right))
        .then_with(|| compare_timestamp_asc(left.created_at, right.created_at))
        .then_with(|| compare_timestamp_asc(left.updated_at, right.updated_at))
        .then_with(|| left.id.cmp(&right.id))
}

pub(crate) fn blocked_rank(record: &WorkItemRecord) -> u8 {
    u8::from(record.blocked_by.is_some())
}

pub(crate) fn readiness_for_scheduling_state(state: WorkItemSchedulingState) -> WorkItemReadiness {
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

pub(crate) fn candidate_class_rank(class: WorkItemCandidateClass) -> u8 {
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

pub(crate) fn compare_timestamp_asc(
    left: DateTime<Utc>,
    right: DateTime<Utc>,
) -> std::cmp::Ordering {
    left.cmp(&right)
}

pub(crate) fn compare_timestamp_desc(
    left: DateTime<Utc>,
    right: DateTime<Utc>,
) -> std::cmp::Ordering {
    right.cmp(&left)
}

pub(crate) fn compare_timestamp_desc_option(
    left: Option<DateTime<Utc>>,
    right: Option<DateTime<Utc>>,
) -> std::cmp::Ordering {
    right.cmp(&left)
}

pub(crate) fn current_todo(record: &WorkItemRecord) -> Option<TodoItem> {
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
