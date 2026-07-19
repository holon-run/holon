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
