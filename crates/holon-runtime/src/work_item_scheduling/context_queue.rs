use super::{WorkItemRecord, WorkItemSchedulingProjection, WorkQueueReadModel};

/// Context owns only its existing display windows, not a second queue inventory.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ContextWorkQueue {
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

impl From<WorkQueueReadModel> for ContextWorkQueue {
    fn from(queue: WorkQueueReadModel) -> Self {
        Self {
            current: queue.current,
            current_runnable: queue.current_runnable,
            triggered_blocked: queue.triggered_blocked,
            queued_runnable: queue.queued_runnable,
            yielded: queue.yielded,
            waiting_for_operator: queue.waiting_for_operator,
            blocked: queue.blocked,
            completing: queue.completing,
            completed_recent: queue.completed_recent,
        }
    }
}
