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

#[tokio::test]
async fn work_item_post_commit_fault_recovers_projection_after_restart() {
    for (fault, expected_effect) in POST_COMMIT_FAULTS {
        let mut harness = LifecycleHarness::new();
        harness.arm_fault(fault);

        let created = harness
            .runtime()
            .create_work_item("post-commit work item".into(), None, None, Vec::new())
            .await
            .unwrap();

        harness.assert_post_commit_warning(expected_effect);
        assert_eq!(
            harness
                .runtime()
                .latest_work_item(&created.id)
                .await
                .unwrap(),
            Some(created.clone())
        );
        let cached_before_restart = harness
            .runtime()
            .latest_work_items_for_agent("default", usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            cached_before_restart
                .iter()
                .any(|record| record.id == created.id),
            fault != crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate
        );
        let committed = harness.snapshot();

        harness.restart();

        assert_eq!(harness.snapshot(), committed);
        assert!(harness
            .runtime()
            .latest_work_items_for_agent("default", usize::MAX)
            .await
            .unwrap()
            .iter()
            .any(|record| record.id == created.id));
    }
}

#[tokio::test]
async fn work_item_focus_and_continuation_survive_restart() {
    let mut harness = LifecycleHarness::new();
    let caller = harness
        .runtime()
        .create_work_item("caller".into(), None, None, Vec::new())
        .await
        .unwrap();
    harness
        .runtime()
        .pick_work_item(caller.id.clone())
        .await
        .unwrap();
    let callee = harness
        .runtime()
        .create_work_item("callee".into(), None, None, Vec::new())
        .await
        .unwrap();
    harness
        .runtime()
        .pick_work_item_with_reason(callee.id.clone(), Some("delegate".into()))
        .await
        .unwrap();
    let before_restart = harness.snapshot();

    harness.restart();

    assert_eq!(harness.snapshot(), before_restart);
    assert_eq!(
        harness
            .runtime()
            .agent_state()
            .await
            .unwrap()
            .current_work_item_id
            .as_deref(),
        Some(callee.id.as_str())
    );
    let active = harness
        .runtime()
        .storage()
        .latest_active_work_item_continuations_for_agent("default")
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].suspended_work_item_id, caller.id);
    assert_eq!(active[0].active_work_item_id, callee.id);

    harness
        .runtime()
        .complete_work_item(callee.id.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(
        harness
            .runtime()
            .agent_state()
            .await
            .unwrap()
            .current_work_item_id
            .as_deref(),
        Some(caller.id.as_str())
    );
    let resumed = harness
        .runtime()
        .storage()
        .latest_work_item_continuations()
        .unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(
        resumed[0].state,
        crate::types::WorkItemContinuationState::Resumed
    );
    let completed = harness.snapshot();

    harness.restart();

    assert_eq!(harness.snapshot(), completed);
    assert_eq!(
        harness
            .runtime()
            .agent_state()
            .await
            .unwrap()
            .current_work_item_id
            .as_deref(),
        Some(caller.id.as_str())
    );
}
