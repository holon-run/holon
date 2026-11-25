//! Task transition predicates over the facts supplied by the caller.

use crate::execution_protocol::{
    eligible_work_item_at_generation, ExecutionAttempt, ExecutionAttemptState,
    ExecutionSourceIdentity, WorkItemExecutionRecord,
};
use crate::{
    is_terminal_task_status, MessageEnvelope, MessageKind, MessageOrigin, TaskRecord, TaskStatus,
    WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
};
use sha2::{Digest, Sha256};

/// Decide whether an incoming update regresses the current task status.
pub fn should_ignore_task_update(latest: Option<&TaskRecord>, task: &TaskRecord) -> bool {
    let Some(latest) = latest else {
        return false;
    };

    if is_terminal_task_status(&latest.status)
        && is_terminal_task_status(&task.status)
        && latest.status != task.status
    {
        return true;
    }

    task_status_phase(&latest.status) > task_status_phase(&task.status)
}

fn task_status_phase(status: &TaskStatus) -> u8 {
    match status {
        TaskStatus::Queued => 0,
        TaskStatus::Running => 1,
        TaskStatus::Cancelling => 2,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 3,
    }
}

pub fn stable_terminal_task_event_id(event_kind: &str, task: &TaskRecord) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event_kind.as_bytes());
    hasher.update([0]);
    hasher.update(task.id.as_bytes());
    hasher.update([0]);
    if let Some(message_id) = task.parent_message_id.as_deref() {
        hasher.update(message_id.as_bytes());
    }
    hasher.update([0]);
    let status = match task.status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    };
    hasher.update(status.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("event_{}", &digest[..15])
}

/// Match the result against the supplied canonical attempt, owner and wait records.
pub fn canonical_exact_task_wait_matches<'a>(
    message: &MessageEnvelope,
    task: &TaskRecord,
    activation_id: &str,
    attempt: &ExecutionAttempt,
    owner_work: Option<&WorkItemExecutionRecord>,
    waits: impl Iterator<Item = &'a WaitConditionRecord> + Clone,
) -> bool {
    if attempt.state != ExecutionAttemptState::Open
        || attempt.attempt_id != activation_id
        || attempt.agent_id != message.agent_id
        || !attempt.binding.matches_agent_partition(&message.agent_id)
        || attempt.source_message_id.as_deref() != Some(message.id.as_str())
        || task.agent_id != message.agent_id
        || message.kind != MessageKind::TaskResult
        || message.task_id.as_deref() != Some(task.id.as_str())
        || !matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &task.id)
    {
        return false;
    }
    let owner = match attempt.binding.work_item_id() {
        Some(work_item_id) => {
            if !owner_work.is_some_and(|work| {
                eligible_work_item_at_generation(work, attempt.admitted_fences.work_item_generation)
            }) {
                return false;
            }
            Some(work_item_id)
        }
        _ => None,
    };
    let wait_id = match &attempt.source.identity {
        ExecutionSourceIdentity::TriggeredWait {
            wait_id,
            trigger_message_id,
            ..
        } if trigger_message_id == &message.id => Some(wait_id.as_str()),
        ExecutionSourceIdentity::TaskResult {
            task_id,
            result_message_id,
        } if task_id == &task.id
            && result_message_id == &message.id
            && owner.is_some()
            && task.effective_work_item_id() == owner
            && message.work_item_id.as_deref() == owner =>
        {
            None
        }
        _ => return false,
    };
    // A replacement wait must not lend its predecessor's resolved trigger.
    if waits.clone().any(|wait| {
        wait.work_item_id.as_deref() == owner
            && matches!(
                wait.status,
                WaitConditionStatus::Active | WaitConditionStatus::Triggered
            )
    }) {
        return false;
    }
    let mut matching = waits.filter(|wait| {
        wait_id.is_none_or(|wait_id| wait.id == wait_id)
                && wait.work_item_id.as_deref() == owner
                && wait.status == WaitConditionStatus::Resolved
                && wait.kind == WaitConditionKind::Task
                && wait.trigger_message_id() == Some(message.id.as_str())
                && wait.wake_sources.iter().any(|source| {
                    matches!(source, WakeSource::TaskResult { task_id } if task_id == &task.id)
                })
    });
    matching.next().is_some() && matching.next().is_none()
}
