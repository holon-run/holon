use chrono::Utc;
use holon_domain::{
    wake_matching::{
        authoritative_wait_correlation, matching_wake_source, message_matches_wait_condition,
        scheduler_wait_correlation, select_wait_to_trigger, WaitTriggerSelection,
    },
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
    MessageKind, MessageOrigin, Priority, WaitConditionKind, WaitConditionRecord,
    WaitConditionStatus, WakeSource,
};

fn task_wait(id: &str, work_item_id: Option<&str>, task_id: &str) -> WaitConditionRecord {
    let now = Utc::now();
    WaitConditionRecord {
        id: id.into(),
        agent_id: "agent-a".into(),
        work_item_id: work_item_id.map(ToString::to_string),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Task,
        source: None,
        subject_ref: Some(task_id.into()),
        waiting_for: format!("waiting for {task_id}"),
        wake_sources: vec![WakeSource::TaskResult {
            task_id: task_id.into(),
        }],
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: Some("turn-wait".into()),
        trigger_message_id: None,
        triggered_at: None,
    }
}

fn task_result(task_id: &str, work_item_id: Option<&str>) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "agent-a",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "completed".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.work_item_id = work_item_id.map(ToString::to_string);
    message.turn_id = Some("turn-trigger".into());
    message
}

#[test]
fn selection_matches_cross_owner_task_wait() {
    // #3124: a task result message is addressed to the task's owner,
    // but any WorkItem in the same agent may hold the dependency wait.
    let message = task_result("task-a", Some("work-b"));
    let wait = task_wait("wait-a", Some("work-a"), "task-a");
    match select_wait_to_trigger(&message, &[wait]) {
        WaitTriggerSelection::Match { condition, wake } => {
            assert_eq!(condition.id, "wait-a");
            assert_eq!(wake.source, "task_result");
        }
        other => panic!("expected cross-owner task match, got {other:?}"),
    }
}

#[test]
fn selection_picks_owner_wait_deterministically_among_task_waiters() {
    // One result message admits one consumed trigger marker per agent;
    // the task owner's own wait wins over older foreign waiters.
    let mut message = task_result("task-a", Some("work-b"));
    message.task_id = Some("task-a".into());
    let owner_wait = task_wait("wait-owner", Some("work-b"), "task-a");
    let mut other_wait = task_wait("wait-other", Some("work-a"), "task-a");
    other_wait.created_at -= chrono::Duration::milliseconds(5);
    match select_wait_to_trigger(&message, &[other_wait, owner_wait]) {
        WaitTriggerSelection::Match { condition, .. } => {
            assert_eq!(condition.id, "wait-owner");
        }
        other => panic!("expected deterministic task wait pick, got {other:?}"),
    }
}

#[test]
fn selection_picks_one_wait_deterministically_instead_of_quarantining() {
    // #3914: a wake matching several waits is not ambiguous. The runtime
    // picks one waiter: an exact task waiter, then WorkItem scope over
    // agent lifecycle, then registration order, then the stable id.
    let message = task_result("task-a", Some("work-a"));
    let mut lifecycle = task_wait("wait-0-lifecycle", None, "task-a");
    lifecycle.created_at -= chrono::Duration::days(1);
    let same_time = task_wait("wait-a", Some("work-a"), "task-a");
    let mut later = task_wait("wait-b", Some("work-a"), "task-a");
    later.created_at = same_time.created_at + chrono::Duration::seconds(1);
    let selected = |waits: &[WaitConditionRecord]| match select_wait_to_trigger(&message, waits) {
        WaitTriggerSelection::Match { condition, .. } => condition.id.clone(),
        other => panic!("expected a single deterministic match, got {other:?}"),
    };
    // WorkItem scope wins over an older agent-lifecycle wait.
    assert_eq!(
        selected(&[lifecycle, same_time.clone(), later.clone()]),
        "wait-a"
    );
    // Equal registration time falls back to the stable id.
    let mut same_time_late_id = task_wait("wait-c", Some("work-a"), "task-a");
    same_time_late_id.created_at = same_time.created_at;
    assert_eq!(selected(&[same_time.clone(), same_time_late_id]), "wait-a");
    // Registration order decides otherwise.
    assert_eq!(selected(&[later, same_time]), "wait-a");
}

#[test]
fn exact_correlation_does_not_fall_back_to_another_wait() {
    let mut message = task_result("task-a", Some("work-a"));
    message
        .source_refs
        .insert("wait_id".into(), "wait-old".into());
    assert_eq!(
        select_wait_to_trigger(
            &message,
            &[task_wait("wait-current", Some("work-a"), "task-a")]
        ),
        WaitTriggerSelection::StaleExact {
            wait_id: "wait-old",
        }
    );
}

#[test]
fn same_turn_message_cannot_trigger_a_new_wait() {
    let mut message = task_result("task-a", Some("work-a"));
    message.turn_id = Some("turn-wait".into());
    assert_eq!(
        select_wait_to_trigger(&message, &[task_wait("wait-a", Some("work-a"), "task-a")]),
        WaitTriggerSelection::NoMatch
    );
}

#[test]
fn timer_tick_only_matches_the_wait_for_the_same_timer() {
    let now = Utc::now();
    let mut matching = WaitConditionRecord {
        id: "wait-matching".into(),
        agent_id: "default".into(),
        work_item_id: Some("work-matching".into()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Timer,
        source: Some("test".into()),
        subject_ref: Some("timer-1".into()),
        waiting_for: "timer-1".into(),
        wake_sources: vec![WakeSource::Timer { wake_at: now }],
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    };
    let mut other = matching.clone();
    other.id = "wait-other".into();
    other.subject_ref = Some("timer-2".into());
    let message = MessageEnvelope::new(
        "default",
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: "timer-1".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "timer fired".into(),
        },
    );

    assert!(message_matches_wait_condition(&message, &matching));
    assert!(!message_matches_wait_condition(&message, &other));
    assert!(matching_wake_source(&message, &matching).is_some());
    assert!(matching_wake_source(&message, &other).is_none());

    matching.subject_ref = None;
    assert!(message_matches_wait_condition(&message, &matching));
    assert!(matching_wake_source(&message, &matching).is_none());
}

#[test]
fn timer_delivery_uses_writer_correlation() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: "timer-1".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "timer fired".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TimerScheduler,
        AdmissionContext::RuntimeOwned,
    );
    message
        .source_refs
        .insert("wait_id".into(), "wait-timer".into());

    assert_eq!(authoritative_wait_correlation(&message), Some("wait-timer"));
    assert_eq!(scheduler_wait_correlation(&message), None);
}
