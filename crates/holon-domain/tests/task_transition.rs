use chrono::Utc;
use holon_domain::{
    task_transition::{should_ignore_task_update, stable_terminal_task_event_id},
    TaskKind, TaskRecord, TaskStatus,
};

fn task(status: TaskStatus) -> TaskRecord {
    let now = Utc::now();
    TaskRecord {
        id: "task-1".into(),
        agent_id: "default".into(),
        kind: TaskKind::ChildAgentTask,
        status,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: None,
        recovery: None,
    }
}

#[test]
fn stale_non_terminal_updates_are_ignored_after_terminal_status_exists() {
    let latest = task(TaskStatus::Completed);
    let stale = task(TaskStatus::Running);
    assert!(should_ignore_task_update(Some(&latest), &stale));
}

#[test]
fn conflicting_terminal_updates_are_ignored_after_terminal_status_exists() {
    let latest = task(TaskStatus::Completed);
    let late_terminal = task(TaskStatus::Failed);
    assert!(should_ignore_task_update(Some(&latest), &late_terminal));
}

#[test]
fn repeated_same_terminal_updates_are_preserved_for_result_events() {
    let latest = task(TaskStatus::Failed);
    let repeated_terminal = task(TaskStatus::Failed);
    assert!(!should_ignore_task_update(
        Some(&latest),
        &repeated_terminal
    ));
}

#[test]
fn stale_running_update_is_ignored_after_cancelling() {
    let latest = task(TaskStatus::Cancelling);
    let stale = task(TaskStatus::Running);
    assert!(should_ignore_task_update(Some(&latest), &stale));
}

#[test]
fn task_record_work_item_id_falls_back_to_detail_for_old_records() {
    let mut record = task(TaskStatus::Running);
    record.detail = Some(serde_json::json!({
        "wait_policy": "blocking",
        "work_item_id": "work-old",
    }));

    assert_eq!(record.effective_work_item_id(), Some("work-old"));

    record.work_item_id = Some("work-new".into());
    assert_eq!(record.effective_work_item_id(), Some("work-new"));
}

#[test]
fn terminal_task_event_id_matches_persisted_format() {
    let mut terminal = task(TaskStatus::Completed);
    terminal.parent_message_id = Some("result-1".into());

    assert_eq!(
        stable_terminal_task_event_id("task_completed", &terminal),
        "event_c16b4594063cb2c",
    );
}
