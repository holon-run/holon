use super::*;
use crate::{
    runtime::task_reads::{durable_recovery_source_digest, exact_recovery_source_digest},
    runtime_db::{evidence::upsert_agent_state_tx, repositories},
    types::{
        AdmissionContext, AgentState, AuthorityClass, MessageBody, MessageDeliverySurface,
        MessageKind, MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus,
        WaitConditionRecord,
    },
};
use anyhow::Context;

fn fixture() -> (
    tempfile::TempDir,
    RuntimeDb,
    MessageEnvelope,
    TaskResultSettlementRecord,
    WaitConditionRecord,
) {
    let (dir, db) = runtime_db();
    let mut record = pending_record(0);
    record.work_item_id = None;
    let result = reply_result_fixture(&db, &record);
    db.messages().upsert(&result).unwrap();
    let mut source = MessageEnvelope::new(
        "agent-a",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "task_result_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "recover task result".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    source.source_refs.extend([
        ("task_result_message_id".into(), record.message_id.clone()),
        (
            "task_result_identity".into(),
            record.result_identity.clone(),
        ),
        (
            "task_result_parent_turn_id".into(),
            record.parent_turn_id.clone(),
        ),
        (
            "task_result_rejoin_generation".into(),
            record.rejoin_generation.to_string(),
        ),
        ("task_result_owner_key".into(), "agent-a".into()),
    ]);
    let source = db.messages().upsert_committed(&source).unwrap();
    let wait: WaitConditionRecord = serde_json::from_value(serde_json::json!({
        "id": "observed-exact-wait", "agent_id": "agent-a",
        "status": "resolved", "kind": "task", "source": "WaitFor",
        "subject_ref": record.task_id, "waiting_for": "exact task result",
        "wake_sources": [{"kind": "task_result", "task_id": record.task_id}],
        "trigger_message_id": source.id,
        "created_at": record.created_at, "updated_at": record.updated_at,
        "resolved_at": record.updated_at,
    }))
    .unwrap();
    db.transaction(|tx| {
        repositories::upsert_wait_condition_tx(tx, &wait)?;
        upsert_agent_state_tx(tx, &AgentState::new("agent-a"))?;
        Ok(())
    })
    .unwrap();
    (dir, db, source, record, wait)
}

#[test]
fn recovery_observation_preserves_exact_candidate_and_successor_predicates() -> Result<()> {
    let (_dir, db, source, record, mut wait) = fixture();
    db.transaction(|tx| {
        let observed = observe_recovery_source_tx(tx, &source, &[], Utc::now())?;
        let candidate = observed
            .candidate
            .as_ref()
            .context("exact candidate missing")?;
        assert_eq!(candidate.result_identity, record.result_identity);
        assert_eq!(candidate.task_id, record.task_id);
        assert_eq!(observed.exact_wait_id.as_deref(), Some(wait.id.as_str()));
        assert_eq!(
            observed.exact_source_digest.as_deref(),
            Some(exact_recovery_source_digest(&source)?.as_str())
        );
        assert_eq!(
            observed.durable_source_digest.as_deref(),
            Some(durable_recovery_source_digest(&source)?.as_str())
        );
        assert!(!observed.wait_has_successor);
        assert!(!observed.queue_interrupted);
        assert!(!observed.settled);
        repositories::upsert_queue_entry_tx(
            tx,
            &QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: source.agent_id.clone(),
                priority: Priority::Next,
                status: QueueEntryStatus::Interrupted,
                created_at: record.created_at,
                updated_at: record.updated_at,
            },
        )?;
        wait.id = "successor-wait".into();
        repositories::upsert_wait_condition_tx(tx, &wait)?;
        let changed = observe_recovery_source_tx(tx, &source, &[], Utc::now())?;
        assert!(changed.wait_has_successor);
        assert!(changed.queue_interrupted);
        assert!(!observed.wait_has_successor);
        assert!(!observed.queue_interrupted);
        Ok(())
    })
}

#[test]
fn recovery_observation_preserves_settled_durable_source_equality() -> Result<()> {
    let (_dir, db, source, mut record, _wait) = fixture();
    db.transaction(|tx| {
        record.state = TaskResultSettlementState::Settled;
        record.disposition = Some(TaskResultSettlementDisposition::ModelDelivered);
        record.settled_at = Some(Utc::now());
        update_tx(tx, &record)?;
        assert!(observe_recovery_source_tx(tx, &source, &[], Utc::now())?.settled);
        let mut forged = source.clone();
        forged.body = MessageBody::Text {
            text: "different body".into(),
        };
        assert!(!observe_recovery_source_tx(tx, &forged, &[], Utc::now())?.settled);
        Ok(())
    })
}

#[test]
fn paused_recovery_observation_requires_this_source_root_in_the_current_epoch() -> Result<()> {
    let (_dir, db, source, mut record, _wait) = fixture();
    record.recovery_paused_at = Some(Utc::now());
    record.recovery_epoch = 2;
    db.transaction(|tx| {
        update_tx(tx, &record)?;
        let root = format!("activation:message:{}", source.id);
        for (epoch, root) in [(1, root.as_str()), (2, "unrelated-root")] {
            tx.execute(
                "INSERT INTO task_result_recovery_admissions
                 (result_identity,recovery_epoch,root_attempt_id,admitted_at)
                 VALUES(?1,?2,?3,?4)",
                params![record.result_identity, epoch, root, timestamp(Utc::now())],
            )?;
        }
        let observed = observe_recovery_source_tx(tx, &source, &[&record], Utc::now())?;
        let replay = &observed.paused_replays[&record.result_identity];
        assert_eq!(replay.recovery_epoch, 2);
        assert!(!replay.root_is_charged);
        assert!(replay.interrupted_anchor.is_none());
        tx.execute(
            "INSERT INTO task_result_recovery_admissions
             (result_identity,recovery_epoch,root_attempt_id,admitted_at)
             VALUES(?1,?2,?3,?4)",
            params![record.result_identity, 2, root, timestamp(Utc::now())],
        )?;
        let charged = observe_recovery_source_tx(tx, &source, &[&record], Utc::now())?;
        assert!(charged.paused_replays[&record.result_identity].root_is_charged);
        assert!(
            charged.paused_replays[&record.result_identity]
                .interrupted_anchor
                .is_none()
        );
        assert!(!replay.root_is_charged);
        Ok(())
    })
}
