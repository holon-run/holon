use super::WorkQueueReadModel;
pub(crate) use holon_context::render::work_items::ContextWorkQueue;

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
