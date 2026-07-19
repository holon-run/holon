use super::super::support::*;
use crate::runtime::WaitForWakeKind;
use crate::types::{MessageEnvelope, WaitConditionStatus, WorkItemState};
use chrono::DateTime;

fn task_result_message(task_id: &str) -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "task completed".into(),
        },
    )
}

#[tokio::test]
async fn wait_runtime_path_rolls_back_each_pre_commit_fault() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("wait fault contract".into(), None, None, Vec::new())
            .await
            .unwrap();
        let before = harness.snapshot();
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::External,
                Some("github:holon-run/holon#2258".into()),
                "waiting for lifecycle contract".into(),
                None,
            )
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        let registered = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::External,
                Some("github:holon-run/holon#2258".into()),
                "waiting for lifecycle contract".into(),
                None,
            )
            .await
            .unwrap();
        let after = harness.snapshot();
        let mut expected_condition = registered.condition;
        expected_condition.created_at =
            DateTime::from_timestamp_millis(expected_condition.created_at.timestamp_millis())
                .unwrap();
        expected_condition.updated_at =
            DateTime::from_timestamp_millis(expected_condition.updated_at.timestamp_millis())
                .unwrap();
        assert_eq!(after.wait_conditions, vec![expected_condition]);
        assert_eq!(after.work_items[0].revision, work_item.revision + 1);
        assert_eq!(
            after.index_outbox_high_watermark,
            before.index_outbox_high_watermark + 1
        );
    }
}

#[tokio::test]
async fn wait_post_commit_fault_recovers_active_wait_after_restart() {
    for (fault, expected_effect) in POST_COMMIT_FAULTS {
        let mut harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("post-commit wait".into(), None, None, Vec::new())
            .await
            .unwrap();
        harness.arm_fault(fault);

        let registered = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::External,
                Some("github:holon-run/holon#2258".into()),
                "waiting for lifecycle recovery".into(),
                None,
            )
            .await
            .unwrap();

        harness.assert_post_commit_warning(expected_effect);
        assert_eq!(
            harness
                .runtime()
                .storage()
                .active_wait_conditions_for_work_item("default", &work_item.id)
                .unwrap(),
            vec![registered.condition.clone()]
        );
        let committed = harness.snapshot();

        harness.restart();

        assert_eq!(harness.snapshot(), committed);
        assert_eq!(
            harness
                .runtime()
                .storage()
                .active_wait_conditions_for_work_item("default", &work_item.id)
                .unwrap(),
            vec![registered.condition]
        );
        assert_eq!(
            harness
                .runtime()
                .latest_work_item(&work_item.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            WorkItemState::Open
        );
    }
}

#[tokio::test]
async fn terminal_task_replay_repairs_wait_once_across_restart() {
    let mut harness = LifecycleHarness::new();
    let work = harness
        .runtime()
        .create_work_item("repair task wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    let registration = harness
        .runtime()
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::TaskResult,
            Some("task-replay".into()),
            "waiting for task-replay".into(),
            None,
        )
        .await
        .unwrap();
    let terminal = TaskRecord {
        id: "task-replay".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: harness.now(),
        updated_at: harness.now(),
        parent_message_id: Some("message-replay".into()),
        work_item_id: Some(work.id.clone()),
        summary: Some("task-replay".into()),
        detail: None,
        recovery: None,
    };
    harness.runtime().storage().append_task(&terminal).unwrap();
    let before = harness
        .runtime()
        .inner
        .runtime_db
        .runtime_index_outbox()
        .high_watermark_for_agent("default")
        .unwrap();

    harness
        .runtime()
        .reduce_task_result_message(
            &task_result_message("task-replay"),
            terminal.clone(),
            false,
            None,
        )
        .await
        .unwrap();
    let repaired = harness.snapshot();
    harness.restart();
    harness
        .runtime()
        .reduce_task_result_message(&task_result_message("task-replay"), terminal, false, None)
        .await
        .unwrap();

    let latest = harness
        .runtime()
        .latest_work_item(&work.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.blocked_by, None);
    assert_eq!(
        harness
            .snapshot()
            .wait_conditions
            .iter()
            .find(|condition| condition.id == registration.condition.id)
            .unwrap()
            .status,
        WaitConditionStatus::Resolved
    );
    let changes = harness
        .runtime()
        .inner
        .runtime_db
        .runtime_index_outbox()
        .read_after("default", before, 20)
        .unwrap();
    assert!(changes.iter().all(|change| change.source_kind != "task"));
    assert_eq!(
        harness
            .snapshot()
            .audit_events
            .iter()
            .filter(|event| event.kind == "task_result_received")
            .count(),
        1
    );
    assert_eq!(harness.snapshot(), repaired);
}
