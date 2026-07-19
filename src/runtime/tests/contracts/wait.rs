use super::super::support::*;
use crate::runtime::WaitForWakeKind;
use chrono::DateTime;

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
