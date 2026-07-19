use super::super::support::*;

#[tokio::test]
async fn task_runtime_path_rolls_back_each_pre_commit_fault() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let now = Utc::now();
        let task = TaskRecord {
            id: "task-fault-contract".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("task fault contract".into()),
            detail: None,
            recovery: None,
        };
        let before = harness.snapshot();
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .persist_task_transition(&task, "task_fault_contract")
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        harness
            .runtime()
            .persist_task_transition(&task, "task_fault_contract")
            .await
            .unwrap();
        let after = harness.snapshot();
        assert_eq!(after.tasks, vec![task]);
        assert_eq!(after.audit_events.len(), before.audit_events.len() + 1);
        assert_eq!(
            after.index_outbox_high_watermark,
            before.index_outbox_high_watermark + 1
        );
    }
}

#[tokio::test]
async fn task_post_commit_fault_recovers_active_projection_after_restart() {
    for (fault, expected_effect) in [
        (
            crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
            "projection_cache_update",
        ),
        (
            crate::runtime_db::transitions::TransitionFaultPoint::BeforeEventPublication,
            "event_publication",
        ),
    ] {
        let mut harness = LifecycleHarness::new();
        let now = harness.now();
        let task = TaskRecord {
            id: format!("task-post-commit-{expected_effect}"),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("task post-commit contract".into()),
            detail: None,
            recovery: None,
        };
        harness.arm_fault(fault);

        harness
            .runtime()
            .persist_task_transition(&task, "task_post_commit_contract")
            .await
            .unwrap();

        harness.assert_post_commit_warning(expected_effect);
        assert_eq!(
            harness.runtime().task_record(&task.id).await.unwrap(),
            Some(task.clone())
        );
        let cached_before_restart = harness.runtime().active_tasks(usize::MAX).await.unwrap();
        assert_eq!(
            cached_before_restart
                .iter()
                .any(|record| record.id == task.id),
            fault != crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate
        );
        let committed = harness.snapshot();

        harness.restart();

        assert_eq!(harness.snapshot(), committed);
        assert!(harness
            .runtime()
            .active_tasks(usize::MAX)
            .await
            .unwrap()
            .iter()
            .any(|record| record.id == task.id));
    }
}

#[tokio::test]
async fn task_restart_interrupts_inflight_task_once() {
    let mut harness = LifecycleHarness::new();
    let now = harness.now();
    harness
        .runtime()
        .storage()
        .append_task(&TaskRecord {
            id: "task-restart-contract".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now - chrono::Duration::seconds(1),
            updated_at: now - chrono::Duration::seconds(1),
            parent_message_id: None,
            work_item_id: None,
            summary: Some("recoverable command".into()),
            detail: None,
            recovery: Some(TaskRecoverySpec::CommandTask {
                summary: "recoverable command".into(),
                spec: crate::types::CommandTaskSpec {
                    cmd: "sleep 5".into(),
                    workdir: None,
                    shell: None,
                    login: true,
                    tty: false,
                    yield_time_ms: 10,
                    max_output_tokens: None,
                    accepts_input: false,
                    terminal_reentry: false,
                },
                authority_class: AuthorityClass::OperatorInstruction,
                promoted_from_exec_command: false,
            }),
        })
        .unwrap();

    harness.restart();
    let runtime = harness.runtime().clone();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        100,
        |events| {
            events
                .iter()
                .any(|event| event.kind == "task_interrupted_on_restart")
        },
        "task interruption on restart",
    )
    .await;
    runtime_task.abort();

    let interrupted = harness
        .runtime()
        .task_record("task-restart-contract")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(interrupted.status, TaskStatus::Interrupted);
    assert_eq!(
        interrupted
            .detail
            .as_ref()
            .and_then(|detail| detail.get("status_before_restart"))
            .and_then(serde_json::Value::as_str),
        Some("running")
    );
    let output = harness
        .runtime()
        .task_output("task-restart-contract", false, 0)
        .await
        .unwrap();
    assert_eq!(output.retrieval_status, TaskOutputRetrievalStatus::NotReady);
    assert_eq!(output.task.status, TaskStatus::Interrupted);
    let messages = harness.snapshot().messages;
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && matches!(
                message.origin,
                MessageOrigin::System { ref subsystem } if subsystem == "task_restart"
            )
    }));
    assert!(messages.iter().any(|message| {
        message
            .metadata
            .as_ref()
            .and_then(|value| value.get("interrupted_tasks"))
            .and_then(|value| value.get("count"))
            .and_then(serde_json::Value::as_u64)
            == Some(1)
    }));
    assert!(messages.iter().any(|message| {
        message
            .metadata
            .as_ref()
            .and_then(|value| value.get("interrupted_tasks"))
            .and_then(|value| value.get("items"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("status_before_restart")
                        .and_then(serde_json::Value::as_str)
                        == Some("running")
                })
            })
    }));
    assert_eq!(
        harness
            .snapshot()
            .audit_events
            .iter()
            .filter(|event| event.kind == "task_interrupted_on_restart")
            .count(),
        1
    );
    let interrupted_events = harness
        .snapshot()
        .audit_events
        .iter()
        .filter(|event| event.kind == "task_interrupted_on_restart")
        .count();

    harness.restart();
    let runtime = harness.runtime().clone();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::task::yield_now().await;
    runtime_task.abort();

    assert_eq!(
        harness
            .runtime()
            .task_record("task-restart-contract")
            .await
            .unwrap(),
        Some(interrupted)
    );
    assert_eq!(
        harness
            .snapshot()
            .audit_events
            .iter()
            .filter(|event| event.kind == "task_interrupted_on_restart")
            .count(),
        interrupted_events
    );
}
