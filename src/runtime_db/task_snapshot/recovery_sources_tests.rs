use super::*;
use crate::{
    runtime::task_reads::{durable_recovery_source_digest, exact_recovery_source_digest},
    runtime_db::input_evidence_snapshot::{
        register_committed_input_evidence_tx, CanonicalInputEvidence,
    },
    types::{
        AdmissionContext, AgentStatus, MessageDeliverySurface, QueueEntryRecord, QueueEntryStatus,
    },
};

fn source(id: &str) -> MessageEnvelope {
    let mut source = MessageEnvelope::new(
        "task-agent",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "task_result_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "recover task results".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    source.id = id.into();
    source.created_at = at();
    source
}

#[test]
fn recovery_source_coverage_keeps_queue_and_stop_predicates_in_the_captured_view() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let (source, _) = append_message_tx(tx, &source("captured-recovery"))?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(source.clone())],
        )?;
        let mut queue = QueueEntryRecord {
            message_id: source.id.clone(),
            agent_id: scope().agent_id,
            priority: Priority::Next,
            status: QueueEntryStatus::Interrupted,
            created_at: at(),
            updated_at: at(),
        };
        repositories::upsert_queue_entry_tx(tx, &queue)?;
        let mut load = request();
        load.capture.recovery_source_ids.insert(source.id.clone());
        load.recovery_at = Some(at());
        let before = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?.require_current()?;
        assert_eq!(before.resident.coverage().recovery_at, load.recovery_at);
        let observed = &before.resident.coverage().recovery_sources[&source.id];
        assert_eq!(
            observed.exact_source_digest.as_deref(),
            Some(exact_recovery_source_digest(&source)?.as_str())
        );
        assert_eq!(
            observed.durable_source_digest.as_deref(),
            Some(durable_recovery_source_digest(&source)?.as_str())
        );
        assert!(observed.queue_interrupted);
        assert!(observed.agent_not_stopped);
        assert!(observed.candidate.is_none());
        assert!(observed.exact_wait_id.is_none());
        assert!(!observed.wait_has_successor);
        assert!(!observed.settled);
        assert!(observed.paused_replays.is_empty());
        before.resident.capture(&load.capture)?;

        queue.status = QueueEntryStatus::Processed;
        repositories::upsert_queue_entry_tx(tx, &queue)?;
        let mut stopped = AgentState::new("task-agent");
        stopped.status = AgentStatus::Stopped;
        upsert_agent_state_tx(tx, &stopped)?;
        let after = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?.require_current()?;
        let changed = &after.resident.coverage().recovery_sources[&source.id];
        assert!(!changed.queue_interrupted);
        assert!(!changed.agent_not_stopped);
        assert!(observed.queue_interrupted);
        assert!(observed.agent_not_stopped);
        Ok(())
    })
}

#[test]
fn recovery_source_preparation_rejects_unacknowledged_or_missing_messages() -> Result<()> {
    for id in ["raw-recovery", "missing-recovery"] {
        let (_dir, db) = fresh()?;
        db.transaction(|tx| {
            append_message_tx(tx, &source("raw-recovery"))?;
            let mut load = request();
            load.capture.recovery_source_ids.insert(id.into());
            load.recovery_at = Some(at());
            let error = prepare_task_domain_snapshot_tx(tx, &scope(), &load)
                .err()
                .expect("an unavailable recovery source must fail during preparation");
            assert!(matches!(
                error.downcast_ref::<TaskReadError>(),
                Some(TaskReadError::IncompleteRecoverySource(source)) if source == id
            ));
            assert!(error
                .to_string()
                .contains("missing or unavailable from its original Message ACK/header"));
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn recovery_source_coverage_produces_paused_epoch_observations_with_the_result() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let mut value = task("paused-task", TaskStatus::Completed);
        value.parent_message_id = Some("paused-result".into());
        value.detail = Some(serde_json::json!({
            "rejoin_obligation_id": value.id,
            "rejoin_generation": 1,
            "parent_turn_id": "parent-turn",
        }));
        let mut result_source = MessageEnvelope::new(
            "task-agent",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: value.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "paused result".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        result_source.id = "paused-result".into();
        result_source.task_id = Some(value.id.clone());
        let (result_source, _) = append_message_tx(tx, &result_source)?;
        let mut result = TaskResultSettlementRecord::pending(&value, &result_source, at())?
            .context("pending paused result missing")?;
        result.recovery_paused_at = Some(at());
        result.recovery_epoch = 2;
        let mut recovery = source("paused-recovery");
        recovery
            .source_refs
            .insert("task_result_message_id".into(), result.message_id.clone());
        let (recovery, _) = append_message_tx(tx, &recovery)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[
                CanonicalInputEvidence::Message(result_source),
                CanonicalInputEvidence::Message(recovery.clone()),
            ],
        )?;
        let keys = BTreeSet::from([
            TaskKey::new(TaskFamily::Task, &value.id),
            TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
        ]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &value)?;
        upsert_pending_tx(tx, &result)?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request(),
        )?;
        let mut load = request();
        load.capture.recovery_source_ids.insert(recovery.id.clone());
        load.recovery_at = Some(at());
        let before = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?.require_current()?;
        let observed = &before.resident.coverage().recovery_sources[&recovery.id];
        let paused = &observed.paused_replays[&result.result_identity];
        assert_eq!(paused.recovery_epoch, 2);
        assert!(!paused.root_is_charged);
        assert!(paused.interrupted_anchor.is_none());
        tx.execute(
            "INSERT INTO task_result_recovery_admissions
             (result_identity,recovery_epoch,root_attempt_id,admitted_at)
             VALUES(?1,?2,?3,?4)",
            params![
                result.result_identity,
                result.recovery_epoch,
                format!("activation:message:{}", recovery.id),
                repositories::timestamp(at()),
            ],
        )?;
        let after = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?.require_current()?;
        assert!(
            after.resident.coverage().recovery_sources[&recovery.id].paused_replays
                [&result.result_identity]
                .root_is_charged
        );
        assert!(!paused.root_is_charged);
        Ok(())
    })
}
