use super::*;
use crate::storage::AppStorage;
use crate::types::{AgentStatus, TaskRecord, TaskStatus, WorkItemRecord};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SchedulerProjection {
    pub active_tasks: Vec<TaskRecord>,
    pub has_blocking_active_tasks: bool,
    pub current_work_item: Option<WorkItemRecord>,
    pub queued_work_items: usize,
    pub pending_wake_hint: bool,
}

impl SchedulerProjection {
    pub(crate) fn from_state(storage: &AppStorage, state: &AgentState) -> Result<Self> {
        let latest_tasks = storage.latest_task_records()?;
        let active_tasks = state
            .active_task_ids
            .iter()
            .filter_map(|task_id| latest_tasks.iter().find(|task| &task.id == task_id))
            .filter(|task| is_active_task_status(&task.status))
            .cloned()
            .collect::<Vec<_>>();
        let has_blocking_active_tasks = active_tasks.iter().any(TaskRecord::is_blocking);
        let work_queue = storage.work_queue_prompt_projection()?;
        Ok(Self {
            active_tasks,
            has_blocking_active_tasks,
            current_work_item: work_queue.current,
            queued_work_items: work_queue.queued_blocked.len(),
            pending_wake_hint: state.pending_wake_hint.is_some(),
        })
    }
}

pub(crate) fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

pub(crate) fn is_active_task_status(status: &TaskStatus) -> bool {
    !is_terminal_task_status(status)
}

pub(crate) fn projected_status_for_idle(
    state: &AgentState,
    storage: &AppStorage,
) -> Result<AgentStatus> {
    if matches!(
        state.status,
        AgentStatus::Asleep | AgentStatus::Paused | AgentStatus::Stopped
    ) {
        return Ok(state.status.clone());
    }
    if SchedulerProjection::from_state(storage, state)?.has_blocking_active_tasks {
        Ok(AgentStatus::AwaitingTask)
    } else {
        Ok(AgentStatus::AwakeIdle)
    }
}

pub(crate) fn apply_idle_projection(state: &mut AgentState, storage: &AppStorage) -> Result<()> {
    state.status = projected_status_for_idle(state, storage)?;
    state.current_run_id = None;
    Ok(())
}

pub(crate) fn active_task_blocks_work_item_completion(
    task: &TaskRecord,
    work_item_id: &str,
) -> bool {
    if !task.is_blocking() || is_terminal_task_status(&task.status) {
        return false;
    }
    match task.effective_work_item_id() {
        Some(id) => id == work_item_id,
        None => true,
    }
}

pub(crate) fn has_completion_blocking_task_for_work_item(
    storage: &AppStorage,
    active_task_ids: &[String],
    work_item_id: &str,
) -> Result<bool> {
    if active_task_ids.is_empty() {
        return Ok(false);
    }
    let tasks = storage.latest_task_records()?;
    Ok(active_task_ids.iter().any(|task_id| {
        tasks
            .iter()
            .find(|task| &task.id == task_id)
            .is_some_and(|task| active_task_blocks_work_item_completion(task, work_item_id))
    }))
}

pub(crate) fn work_queue_tick_idempotency_key(work_item: &WorkItemRecord, reason: &str) -> String {
    format!(
        "work_queue:{}:{}:{}",
        reason, work_item.id, work_item.revision
    )
}
