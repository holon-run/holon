use super::*;
use crate::types::AgentMessageDeliveryState;

#[tokio::test(start_paused = true)]
async fn closed_reply_owner_disposes_original_without_model_delivery_and_keeps_body() {
    for explicit_owner in [true, false] {
        let provider = Arc::new(FailureProvider::default());
        let harness = LifecycleHarness::with_provider(provider.clone());
        let runtime = harness.runtime();
        let owner = runtime
            .create_work_item(
                "closed reply owner".into(),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        let (task, reply_id) =
            reply_result_with_binding(runtime, &owner, "closed-owner-reply", explicit_owner).await;
        let reply = runtime
            .storage()
            .read_message_by_id(&reply_id)
            .unwrap()
            .unwrap();
        runtime
            .complete_work_item(owner.id.clone(), Vec::new())
            .await
            .unwrap();

        let executor = scheduler_executor::SchedulerDecisionExecutor::new(runtime);
        let mut waiting = scheduler_executor::WaitingReplyQueue::default();
        for fault in crate::runtime::tests::support::PRE_COMMIT_FAULTS {
            let before = harness.snapshot();
            harness.arm_fault(fault);
            let error = match executor.poll_with_waiting_replies(&mut waiting).await {
                Err(error) => error,
                Ok(_) => panic!("non-model disposition must roll back at {fault:?}"),
            };
            crate::runtime::tests::support::assert_injected_transition_fault(&error);
            harness.assert_unchanged(&before);
        }
        assert!(
            matches!(
                executor
                    .poll_with_waiting_replies(&mut waiting)
                    .await
                    .unwrap(),
                scheduler_executor::RunLoopPoll::Idle
            ),
            "closed owner must not activate a model: explicit_owner={explicit_owner}"
        );
        assert!(!waiting.ids.contains(&reply_id));
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(&reply_id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Dropped,
            "original reply must reach its own non-model terminal"
        );
        let delivery = runtime
            .runtime_db()
            .agent_message_deliveries()
            .latest_for_message(&reply_id)
            .unwrap()
            .unwrap();
        assert_eq!(delivery.state, AgentMessageDeliveryState::Failed);
        assert!(!delivery.retryable);
        assert!(runtime
            .storage()
            .read_recent_events(64)
            .unwrap()
            .iter()
            .any(|event| event.kind == "scheduler_authority_input_rejected"
                && event.data["message_id"] == reply_id
                && event.data["reason"] == "canonical_work_item_not_open_same_agent"));
        assert!(provider.requests.lock().await.is_empty());
        assert_eq!(
            runtime
                .storage()
                .read_message_by_id(&reply_id)
                .unwrap()
                .unwrap(),
            reply,
            "non-model disposition must preserve the original body and provenance"
        );
        let source_ref = format!("message:{reply_id}");
        let recovered = runtime
            .get_memory(&source_ref, None)
            .await
            .unwrap()
            .expect("the terminal reply must remain accessible by its original source_ref");
        assert_eq!(recovered.source_ref, source_ref);
        assert!(!recovered.truncated);
        assert!(recovered.content.contains("repeated-reply-evidence"));
        let due = latest(runtime, task.parent_message_id.as_deref().unwrap())
            .next_recheck_at
            .unwrap();
        harness
            .advance((due - harness.now()).to_std().unwrap() + std::time::Duration::from_secs(1))
            .await;
        let runtime = harness.runtime();
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        let settlement = latest(runtime, task.parent_message_id.as_deref().unwrap());
        assert_eq!(settlement.state, TaskResultSettlementState::Settled);
        assert_eq!(
            settlement.disposition,
            Some(TaskResultSettlementDisposition::OwnerClosed)
        );

        let settled = harness.snapshot();
        let executor = scheduler_executor::SchedulerDecisionExecutor::new(runtime);
        assert!(matches!(
            executor
                .poll_with_waiting_replies(&mut waiting)
                .await
                .unwrap(),
            scheduler_executor::RunLoopPoll::Idle
        ));
        harness.assert_unchanged(&settled);
    }
}
