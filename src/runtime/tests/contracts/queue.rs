use super::super::support::*;
use crate::types::QueueEntryRecord;

#[tokio::test]
async fn queue_runtime_path_rolls_back_each_pre_commit_fault() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-fault-contract".into(),
            agent_id: "default".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        harness
            .runtime()
            .storage()
            .append_queue_entry(&queued)
            .unwrap();
        let before = harness.snapshot();
        let mut processed = queued.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at = now + chrono::Duration::seconds(1);
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .commit_queue_settlement(
                processed.clone(),
                vec![AuditEvent::legacy(
                    "queue_fault_contract",
                    serde_json::json!({}),
                )],
                true,
            )
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        assert!(harness
            .runtime()
            .commit_queue_settlement(
                processed.clone(),
                vec![AuditEvent::legacy(
                    "queue_fault_contract",
                    serde_json::json!({}),
                )],
                true,
            )
            .await
            .unwrap());
        let after = harness.snapshot();
        assert_eq!(after.queue_entries, vec![processed]);
        assert_eq!(after.audit_events.len(), before.audit_events.len() + 1);
    }
}
