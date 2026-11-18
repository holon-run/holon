use super::*;
use holon_domain::execution_protocol::{
    ConversationOutcome, ExecutionOutcome, WaitReference, WorkItemOutcome,
};
use holon_domain::execution_protocol::{WorkItemExecutionRecord, WorkItemExecutionState};

fn wait_outcome(wait_id: &str) -> ExecutionOutcomeRecord {
    ExecutionOutcomeRecord {
        outcome_id: "outcome-1".into(),
        attempt_id: "attempt-1".into(),
        outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Wait {
            wait: WaitReference {
                wait_id: wait_id.into(),
            },
        }),
        created_at: "2026-08-05T00:00:00Z".into(),
    }
}

#[test]
fn stored_outcome_identity_ignores_removed_wait_generation_field() {
    let existing = serde_json::json!({
        "outcome_id": "outcome-1",
        "attempt_id": "attempt-1",
        "outcome": {
            "owner": "work_item",
            "outcome": {
                "kind": "wait",
                "wait": {
                    "wait_id": "wait-1",
                    "generation": 7
                }
            }
        },
        "created_at": "2026-08-05T00:00:00Z"
    });
    assert!(stored_outcome_matches(&existing.to_string(), &wait_outcome("wait-1")).unwrap());
    assert!(!stored_outcome_matches(&existing.to_string(), &wait_outcome("wait-2")).unwrap());
}

#[test]
fn legacy_needs_repair_record_requires_the_v113_projection_and_stays_paused() {
    let legacy = serde_json::json!({
        "source_revision": 4,
        "state": {
            "state": "needs_repair",
            "generation": 3,
            "repair_id": "work_item_waits_ambiguous:work-a"
        }
    })
    .to_string();
    assert_eq!(
        decode_work_item_record_with_migration(&legacy, "4:3:paused").unwrap(),
        WorkItemExecutionRecord {
            source_revision: 4,
            state: WorkItemExecutionState::Paused {
                generation: 3,
                reason: "work_item_waits_ambiguous:work-a".into(),
            }
        }
    );
    for wrong_marker in [
        "5:3:paused",
        "4:4:paused",
        "4:3:runnable",
        "4:3:needs_repair",
    ] {
        assert!(decode_work_item_record_with_migration(&legacy, wrong_marker).is_err());
    }
    // Current records pass through unchanged; the migration is idempotent.
    let current = WorkItemExecutionRecord {
        source_revision: 2,
        state: WorkItemExecutionState::Runnable {
            generation: 1,
            recovery_ref: Some("provider_dispatch_rejected:turn-1".into()),
        },
    };
    let payload = serde_json::to_string(&current).unwrap();
    assert_eq!(
        decode_work_item_record_with_migration(&payload, "2:1:runnable").unwrap(),
        current
    );
}

#[test]
fn legacy_needs_repair_outcome_migrates_to_interrupted() {
    let legacy = serde_json::json!({
        "outcome_id": "outcome-1",
        "attempt_id": "attempt-1",
        "outcome": {
            "owner": "work_item",
            "outcome": {
                "kind": "needs_repair",
                "repair_id": "provider_dispatch_rejected:turn-1"
            }
        },
        "created_at": "2026-08-05T00:00:00Z"
    });
    let migrated = decode_outcome_record_with_migration(&legacy.to_string()).unwrap();
    assert_eq!(
        migrated.outcome,
        ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted {
            reason: "provider_dispatch_rejected:turn-1".into()
        })
    );
    let wait = wait_outcome("wait-9");
    let payload = serde_json::to_string(&wait).unwrap();
    assert_eq!(
        decode_outcome_record_with_migration(&payload).unwrap(),
        wait
    );
}

fn legacy_needs_repair_payload() -> serde_json::Value {
    serde_json::json!({
        "outcome_id": "outcome:message:msg-1",
        "attempt_id": "attempt-1",
        "outcome": {
            "owner": "work_item",
            "outcome": {
                "kind": "needs_repair",
                "repair_id": "provider_dispatch_rejected:turn-1"
            }
        },
        "created_at": "2026-08-05T00:00:00Z"
    })
}

#[test]
fn retired_needs_repair_rewrite_is_field_equivalent_to_the_decoder() {
    let legacy = legacy_needs_repair_payload().to_string();
    let decoded = decode_outcome_record_with_migration(&legacy).unwrap();
    let rewritten = rewrite_outcome_needs_repair(&legacy)
        .unwrap()
        .expect("legacy needs_repair payload is rewritten");
    assert_eq!(
        serde_json::from_str::<ExecutionOutcomeRecord>(&rewritten).unwrap(),
        decoded,
        "migration rewrite and load-time decode must agree field by field"
    );
    let value: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
    assert_eq!(value["outcome"]["owner"], "work_item");
    assert_eq!(value["outcome"]["outcome"]["kind"], "interrupted");
    assert_eq!(
        value["outcome"]["outcome"]["reason"],
        "provider_dispatch_rejected:turn-1"
    );
    assert_eq!(value["outcome_id"], "outcome:message:msg-1");
    assert_eq!(value["attempt_id"], "attempt-1");
    assert_eq!(value["created_at"], "2026-08-05T00:00:00Z");
    // The rewrite is idempotent.
    assert_eq!(rewrite_outcome_needs_repair(&rewritten).unwrap(), None);
}

#[test]
fn retired_needs_repair_rewrite_preserves_other_outcomes() {
    let wait = wait_outcome("wait-9");
    assert_eq!(
        rewrite_outcome_needs_repair(&serde_json::to_string(&wait).unwrap()).unwrap(),
        None
    );
    let conversation = ExecutionOutcomeRecord {
        outcome_id: "outcome-2".into(),
        attempt_id: "attempt-2".into(),
        outcome: ExecutionOutcome::Conversation(ConversationOutcome::Replied),
        created_at: "2026-08-05T00:00:00Z".into(),
    };
    assert_eq!(
        rewrite_outcome_needs_repair(&serde_json::to_string(&conversation).unwrap()).unwrap(),
        None
    );
}

#[test]
fn damaged_retired_needs_repair_fails_closed() {
    let mut missing = legacy_needs_repair_payload();
    missing["outcome"]["outcome"]
        .as_object_mut()
        .unwrap()
        .remove("repair_id");
    let error = rewrite_outcome_needs_repair(&missing.to_string()).unwrap_err();
    assert!(error.to_string().contains("lacks repair_id"));

    let mut foreign_owner = legacy_needs_repair_payload();
    foreign_owner["outcome"]["owner"] = serde_json::json!("conversation");
    let error = rewrite_outcome_needs_repair(&foreign_owner.to_string()).unwrap_err();
    assert!(error.to_string().contains("unexpected owner"));
}
