use super::super::support::*;

#[tokio::test]
async fn work_item_runtime_path_rolls_back_each_pre_commit_fault() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let before = harness.snapshot();
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .create_work_item("fault contract".into(), None, None, Vec::new())
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        let created = harness
            .runtime()
            .create_work_item("fault contract retry".into(), None, None, Vec::new())
            .await
            .unwrap();
        let after = harness.snapshot();
        assert_eq!(after.work_items, vec![created]);
        assert_eq!(
            after.index_outbox_high_watermark,
            before.index_outbox_high_watermark + 1
        );
    }
}
