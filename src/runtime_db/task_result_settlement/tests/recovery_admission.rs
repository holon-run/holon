use std::collections::BTreeSet;

use super::*;
use crate::domain::execution_protocol::{ExecutionAttempt, ExecutionAttemptState};
use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
    MessageOrigin, Priority, TurnRecord,
};

#[derive(Clone, Copy, Debug)]
enum Snapshot {
    Unsettled,
    Reply,
}

const SNAPSHOTS: [Snapshot; 2] = [Snapshot::Unsettled, Snapshot::Reply];

fn result_fixture(db: &RuntimeDb, snapshot: Snapshot) -> TaskResultSettlementRecord {
    let record = pending_record(0);
    match snapshot {
        Snapshot::Unsettled => insert(db, &record),
        Snapshot::Reply => {
            let mut message = reply_result_fixture(db, &record);
            queued_original_reply_fixture(db, &record);
            message.metadata.as_mut().unwrap()["task_detail"]["reply_content_source"] =
                serde_json::json!("original_message");
            db.messages().upsert(&message).unwrap();
        }
    }
    record
}

fn recovery_source(db: &RuntimeDb, id: &str) -> MessageEnvelope {
    let mut source = MessageEnvelope::new(
        "agent-a",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "task_result_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "recover deferred task results".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    source.id = id.into();
    source.work_item_id = Some("work-a".into());
    db.messages().upsert(&source).unwrap();
    source
}

fn save_attempt(db: &RuntimeDb, attempt: &ExecutionAttempt) {
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state, source_identity_json,
               source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, attempt_id) DO UPDATE SET
               lifecycle_state = excluded.lifecycle_state,
               payload_json = excluded.payload_json",
            params![
                attempt.agent_id,
                attempt.attempt_id,
                serde_json::to_value(attempt.state)?.as_str().unwrap(),
                serde_json::to_string(&attempt.source.identity)?,
                attempt.source.generation,
                attempt.recovery_of_attempt_id,
                attempt.terminal_outcome_id,
                serde_json::to_string(attempt)?,
            ],
        )?;
        Ok(())
    })
    .unwrap();
}

fn canonical_attempt(
    db: &RuntimeDb,
    source: &MessageEnvelope,
    id: &str,
    predecessor: Option<&str>,
) -> ExecutionAttempt {
    let attempt = serde_json::from_value(serde_json::json!({
        "attempt_id": id, "agent_id": "agent-a", "source_message_id": source.id,
        "source": {
            "identity": {"kind": "queue_message", "message_id": source.id},
            "generation": 1,
        },
        "binding": {"kind": "work_item", "work_item_id": "work-a"},
        "provenance": {
            "origin": "system", "trust": "runtime_instruction", "priority": "next",
        },
        "admitted_fences": {
            "source_revision": 1, "agent_control_revision": 1, "host_registry_revision": 1,
        },
        "state": "open", "turn_id": id,
        "recovery_of_attempt_id": predecessor,
        "admitted_at": pending_record(0).next_recheck_at.unwrap().to_rfc3339(),
    }))
    .unwrap();
    save_attempt(db, &attempt);
    let mut turn = TurnRecord::new("agent-a", id, 1);
    turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(source));
    turn.current_work_item_id = Some("work-a".into());
    turn.input_message_ids.push(source.id.clone());
    db.turn_records().upsert(&turn).unwrap();
    attempt
}

fn admit(
    db: &RuntimeDb,
    record: &TaskResultSettlementRecord,
    snapshot: Snapshot,
    activation: &str,
    now: DateTime<Utc>,
) -> Result<Vec<TaskResultSettlementRecord>> {
    let selected = BTreeSet::from([record.result_identity.clone()]);
    match snapshot {
        Snapshot::Unsettled => db.task_result_settlements().admit_unsettled_snapshot(
            &record.agent_id,
            record.work_item_id.as_deref(),
            activation,
            None,
            Some(&selected),
            now,
        ),
        Snapshot::Reply => db.task_result_settlements().admit_reply_message_snapshot(
            &record.agent_id,
            record.work_item_id.as_deref(),
            "original-reply",
            activation,
            &selected,
            now,
        ),
    }
}

fn latest(db: &RuntimeDb, record: &TaskResultSettlementRecord) -> TaskResultSettlementRecord {
    db.task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap()
}

#[test]
fn recovery_filter_borrows_unchanged_records_but_preserves_sql_readback() {
    for snapshot in SNAPSHOTS {
        let (_dir, db) = runtime_db();
        let record = result_fixture(&db, snapshot);
        let records = [record.clone()];
        let now = record.next_recheck_at.unwrap();
        let check_borrowed = |activation| {
            db.transaction(|tx| {
                let selected = filter_recovery_snapshot_candidates_tx(
                    tx,
                    &record.agent_id,
                    activation,
                    &records,
                    now,
                )?;
                assert_eq!(selected.len(), 1, "{snapshot:?}");
                assert!(matches!(&selected[0], std::borrow::Cow::Borrowed(original)
                    if std::ptr::eq(*original, &records[0])));
                Ok(())
            })
            .unwrap();
        };
        check_borrowed("normal-input");
        let source = recovery_source(&db, "recovery-source");
        canonical_attempt(&db, &source, "root-a", None);
        check_borrowed("root-a");
        let mut stale = record.clone();
        stale.task_status = "forged".into();
        let stale = [stale];
        db.transaction(|tx| {
            let selected = filter_recovery_snapshot_candidates_tx(
                tx,
                &record.agent_id,
                "root-a",
                &stale,
                now,
            )?;
            assert_eq!(selected.len(), 1);
            assert!(matches!(&selected[0], std::borrow::Cow::Owned(current) if current == &record));
            Ok(())
        })
        .unwrap();
        assert_eq!(latest(&db, &record), record, "filter must not write");
    }
}

#[test]
fn recovery_candidate_identity_scan_keeps_order_and_charges_only_identities() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };

    for snapshot in SNAPSHOTS {
        let (_dir, db) = runtime_db();
        let record = result_fixture(&db, snapshot);
        let second = pending_record(1);
        insert(&db, &second);
        let source = recovery_source(&db, "recovery-source");
        let mut prior = canonical_attempt(&db, &source, "root-a", None);
        prior.state = ExecutionAttemptState::Interrupted;
        save_attempt(&db, &prior);
        canonical_attempt(&db, &source, "fallback-a", Some("root-a"));
        let now = second.next_recheck_at.unwrap();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: record.agent_id.clone(),
            identity_incarnation: 1,
        };
        db.transaction(|tx| {
            let read = RecoverySnapshotRead::new(tx, &record.agent_id, "fallback-a")?;
            let material = ResourceReadMaterial::new(&ledger, &scope)?;
            let before = material.finish()?;
            material.read(|| {
                let ids = read.candidate_identities(record.work_item_id.as_deref(), now, 64)?;
                assert_eq!(
                    ids,
                    [
                        record.result_identity.clone(),
                        second.result_identity.clone()
                    ]
                );
                let after = material.finish()?;
                assert_eq!(after.1 - before.1, ids.len() * 8);
                assert_eq!(
                    after.0 - before.0,
                    ids.iter()
                        .map(|id| (id.len() as u64 + 128) * 8)
                        .sum::<u64>()
                );
                assert_eq!(
                    read.candidate_identities(record.work_item_id.as_deref(), now, 1)?,
                    [record.result_identity.clone()]
                );
                assert!(read
                    .candidate_identities(Some("other-owner"), now, 64)?
                    .is_empty());
                assert!(read
                    .candidate_identities(record.work_item_id.as_deref(), now, 0)?
                    .is_empty());
                Ok(())
            })?;
            assert_eq!(
                read.candidates(record.work_item_id.as_deref(), now, 64)?,
                [record.clone(), second.clone()]
            );
            Ok(())
        })?;
        let mut invalid = source;
        invalid.authority_class = AuthorityClass::ExternalEvidence;
        db.messages().upsert(&invalid)?;
        db.transaction(|tx| {
            let read = RecoverySnapshotRead::new(tx, &record.agent_id, "fallback-a")?;
            assert!(read
                .candidate_identities(record.work_item_id.as_deref(), now, 64)?
                .is_empty());
            Ok(())
        })?;
        assert_eq!(
            latest(&db, &record),
            record,
            "identity selection must not write"
        );
        assert_eq!(
            latest(&db, &second),
            second,
            "identity selection must not write"
        );
    }
    Ok(())
}

#[test]
fn recovery_snapshot_read_reuses_lineage_and_preserves_result_readback() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };

    for snapshot in SNAPSHOTS {
        let (_dir, db) = runtime_db();
        let record = result_fixture(&db, snapshot);
        let source = recovery_source(&db, "recovery-source");
        let mut prior = canonical_attempt(&db, &source, "root-a", None);
        prior.state = ExecutionAttemptState::Interrupted;
        save_attempt(&db, &prior);
        canonical_attempt(&db, &source, "fallback-a", Some("root-a"));
        let now = record.next_recheck_at.unwrap();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: record.agent_id.clone(),
            identity_incarnation: 1,
        };
        let measure = |shared: bool| -> Result<_> {
            let material = ResourceReadMaterial::new(&ledger, &scope)?;
            db.transaction(|tx| {
                material.read(|| {
                    if shared {
                        let read = RecoverySnapshotRead::new(tx, &record.agent_id, "fallback-a")?;
                        let records = read.candidates(record.work_item_id.as_deref(), now, 64)?;
                        let selected = read.filter(&records, now)?;
                        assert_eq!(records, vec![record.clone()]);
                        assert!(
                            matches!(selected.as_slice(), [std::borrow::Cow::Borrowed(value)]
                            if std::ptr::eq(*value, &records[0]))
                        );
                    } else {
                        let records = recovery_snapshot_candidates_for_owner_tx(
                            tx,
                            &record.agent_id,
                            record.work_item_id.as_deref(),
                            "fallback-a",
                            now,
                            64,
                        )?;
                        let selected = filter_recovery_snapshot_candidates_tx(
                            tx,
                            &record.agent_id,
                            "fallback-a",
                            &records,
                            now,
                        )?;
                        assert_eq!(records, vec![record.clone()]);
                        assert!(
                            matches!(selected.as_slice(), [std::borrow::Cow::Borrowed(value)]
                            if std::ptr::eq(*value, &records[0]))
                        );
                    }
                    Ok(())
                })
            })?;
            material.finish()
        };
        let separate = measure(false)?;
        let shared = measure(true)?;
        assert!(
            shared.0 < separate.0 && shared.1 < separate.1,
            "{snapshot:?}"
        );

        let (_other_dir, other_db) = runtime_db();
        db.transaction(|tx| {
            let read = RecoverySnapshotRead::new(tx, &record.agent_id, "fallback-a")?;
            read.validate_cut(tx, &record.agent_id)?;
            assert!(read.validate_cut(tx, "another-agent").is_err());
            other_db.transaction(|other_tx| {
                assert!(read.validate_cut(other_tx, &record.agent_id).is_err());
                Ok(())
            })?;
            let mut stale = record.clone();
            stale.task_status = "forged".into();
            let stale = [stale];
            let selected = read.filter(&stale, now)?;
            assert!(
                matches!(selected.as_slice(), [std::borrow::Cow::Owned(current)]
                if current == &record),
                "canonical SQL readback must not be skipped"
            );
            Ok(())
        })?;
        let mut invalid = source.clone();
        invalid.authority_class = AuthorityClass::ExternalEvidence;
        db.messages().upsert(&invalid)?;
        db.transaction(|tx| {
            let read = RecoverySnapshotRead::new(tx, &record.agent_id, "fallback-a")?;
            assert!(read
                .candidates(record.work_item_id.as_deref(), now, 64)?
                .is_empty());
            assert!(read.filter(&[record.clone()], now)?.is_empty());
            Ok(())
        })?;
        assert_eq!(latest(&db, &record), record, "selection must not write");
    }
    Ok(())
}

#[test]
fn canonical_snapshot_admission_charges_only_selected_results_and_preserves_retry_fields() {
    for snapshot in SNAPSHOTS {
        let (_dir, db) = runtime_db();
        let record = result_fixture(&db, snapshot);
        let unselected = pending_record(1);
        insert(&db, &unselected);
        let source = recovery_source(&db, "recovery-source");
        canonical_attempt(&db, &source, "root-a", None);
        let now = record.next_recheck_at.unwrap();

        let admitted = admit(&db, &record, snapshot, "root-a", now).unwrap();
        assert_eq!(admitted.len(), 1, "{snapshot:?}");
        assert_eq!(admitted[0].state, TaskResultSettlementState::CallerAdmitted);
        assert_eq!(admitted[0].recovery_attempts, 1);
        assert_eq!(
            admitted[0].last_recovery_root_attempt_id.as_deref(),
            Some("root-a")
        );
        assert_eq!(admitted[0].last_recovery_admitted_at, Some(now));
        assert_eq!(
            admitted[0].next_recheck_at,
            Some(now + Duration::seconds(60))
        );
        assert_eq!(latest(&db, &record), admitted[0]);
        assert_eq!(latest(&db, &unselected).recovery_attempts, 0);
        assert!(admit(&db, &record, snapshot, "root-a", now)
            .unwrap()
            .is_empty());
        assert_eq!(latest(&db, &record).recovery_attempts, 1);
    }
}

#[test]
fn canonical_snapshot_admission_rolls_back_the_budget_when_snapshot_write_fails() {
    for snapshot in SNAPSHOTS {
        let (_dir, db) = runtime_db();
        let record = result_fixture(&db, snapshot);
        let source = recovery_source(&db, "recovery-source");
        canonical_attempt(&db, &source, "root-a", None);
        let now = record.next_recheck_at.unwrap();
        db.transaction(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER reject_snapshot_admission
                 BEFORE UPDATE ON task_result_settlements
                 WHEN NEW.state = 'caller_admitted'
                 BEGIN SELECT RAISE(ABORT, 'snapshot write failed'); END;",
            )?;
            Ok(())
        })
        .unwrap();

        assert!(admit(&db, &record, snapshot, "root-a", now).is_err());
        assert_eq!(latest(&db, &record), record);
        db.transaction(|tx| {
            let charged: i64 = tx.query_row(
                "SELECT COUNT(*) FROM task_result_recovery_admissions",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(charged, 0);
            tx.execute_batch("DROP TRIGGER reject_snapshot_admission")?;
            Ok(())
        })
        .unwrap();
        let admitted = admit(&db, &record, snapshot, "root-a", now).unwrap();
        assert_eq!(admitted.len(), 1, "{snapshot:?}");
        assert_eq!(admitted[0].recovery_attempts, 1);
    }
}

#[test]
fn canonical_snapshot_recovery_reuses_the_interrupted_root_across_restart() {
    for snapshot in SNAPSHOTS {
        let (dir, db) = runtime_db();
        let record = result_fixture(&db, snapshot);
        let source = recovery_source(&db, "recovery-source");
        let mut root = canonical_attempt(&db, &source, "root-a", None);
        let now = record.next_recheck_at.unwrap();
        let charged = admit(&db, &record, snapshot, "root-a", now).unwrap();
        assert_eq!(charged.len(), 1);
        root.state = ExecutionAttemptState::Interrupted;
        save_attempt(&db, &root);
        drop(db);
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )
        .unwrap();
        canonical_attempt(&db, &source, "restart-a", Some("root-a"));
        assert_eq!(
            db.task_result_settlements()
                .filter_recovery_snapshot_candidates(
                    &record.agent_id,
                    "restart-a",
                    &charged,
                    now + Duration::seconds(1),
                )
                .unwrap(),
            charged,
            "an already charged interrupted root remains eligible during backoff",
        );
        assert_eq!(latest(&db, &record), charged[0]);

        let recovered = admit(
            &db,
            &record,
            snapshot,
            "restart-a",
            now + Duration::seconds(1),
        )
        .unwrap();
        assert_eq!(recovered.len(), 1, "{snapshot:?}");
        assert_eq!(recovered[0].activation_id.as_deref(), Some("restart-a"));
        assert_eq!(recovered[0].recovery_attempts, 1);
        assert_eq!(
            recovered[0].last_recovery_root_attempt_id.as_deref(),
            Some("root-a")
        );
        assert_eq!(recovered[0].next_recheck_at, charged[0].next_recheck_at);
        assert_eq!(recovered[0].last_recovery_admitted_at, Some(now));
    }
}

#[test]
fn new_recovery_root_cannot_bypass_backoff_in_snapshot_admission() {
    let (_dir, db) = runtime_db();
    let record = result_fixture(&db, Snapshot::Unsettled);
    let source = recovery_source(&db, "first-source");
    let mut root = canonical_attempt(&db, &source, "root-a", None);
    let now = record.next_recheck_at.unwrap();
    let first = admit(&db, &record, Snapshot::Unsettled, "root-a", now).unwrap();
    root.state = ExecutionAttemptState::Interrupted;
    save_attempt(&db, &root);
    let source = recovery_source(&db, "second-source");
    canonical_attempt(&db, &source, "root-b", Some("root-a"));
    assert!(db
        .task_result_settlements()
        .filter_recovery_snapshot_candidates(
            &record.agent_id,
            "root-b",
            &first,
            now + Duration::seconds(59),
        )
        .unwrap()
        .is_empty());
    assert_eq!(latest(&db, &record), first[0], "selection never charges");

    assert!(admit(
        &db,
        &record,
        Snapshot::Unsettled,
        "root-b",
        now + Duration::seconds(59),
    )
    .unwrap()
    .is_empty());
    assert_eq!(latest(&db, &record), first[0]);
    let admitted = admit(
        &db,
        &record,
        Snapshot::Unsettled,
        "root-b",
        now + Duration::seconds(60),
    )
    .unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].recovery_attempts, 2);
    assert_eq!(
        admitted[0].last_recovery_root_attempt_id.as_deref(),
        Some("root-b")
    );
    assert_eq!(
        admitted[0].next_recheck_at,
        Some(now + Duration::seconds(180))
    );
}

#[test]
fn canonical_snapshot_admission_pauses_after_five_distinct_recovery_roots() {
    let (_dir, db) = runtime_db();
    let record = result_fixture(&db, Snapshot::Unsettled);
    let mut now = record.next_recheck_at.unwrap();
    for index in 1..=TASK_RESULT_RECOVERY_LIMIT {
        let source = recovery_source(&db, &format!("recovery-source-{index}"));
        let id = format!("root-{index}");
        let mut root = canonical_attempt(&db, &source, &id, None);
        let admitted = admit(&db, &record, Snapshot::Unsettled, &id, now).unwrap();
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].recovery_attempts, index);
        assert_eq!(
            db.task_result_settlements()
                .filter_recovery_snapshot_candidates(&record.agent_id, &id, &admitted, now,)
                .unwrap(),
            admitted,
            "the final charged root can still finish after its budget pauses",
        );
        root.state = ExecutionAttemptState::Interrupted;
        save_attempt(&db, &root);
        if index < TASK_RESULT_RECOVERY_LIMIT {
            now = admitted[0].next_recheck_at.unwrap();
        }
    }
    let paused = latest(&db, &record);
    assert_eq!(paused.state, TaskResultSettlementState::CallerAdmitted);
    assert!(paused.recovery_paused_at.is_some());
    assert_eq!(paused.next_recheck_at, None);
    let source = recovery_source(&db, "sixth-source");
    let mut denied = canonical_attempt(&db, &source, "root-six", None);
    assert!(db
        .task_result_settlements()
        .filter_recovery_snapshot_candidates(
            &record.agent_id,
            "root-six",
            &[paused.clone()],
            now + Duration::days(1),
        )
        .unwrap()
        .is_empty());
    assert!(admit(
        &db,
        &record,
        Snapshot::Unsettled,
        "root-six",
        now + Duration::days(1),
    )
    .unwrap()
    .is_empty());
    assert_eq!(latest(&db, &record), paused);
    denied.state = ExecutionAttemptState::Interrupted;
    save_attempt(&db, &denied);

    let mut normal = recovery_source(&db, "normal-source");
    normal.origin = MessageOrigin::System {
        subsystem: "operator_dispatch".into(),
    };
    db.messages().upsert(&normal).unwrap();
    canonical_attempt(&db, &normal, "normal-input", None);
    assert_eq!(
        db.task_result_settlements()
            .filter_recovery_snapshot_candidates(
                &record.agent_id,
                "normal-input",
                &[paused.clone()],
                now + Duration::days(1),
            )
            .unwrap(),
        Vec::<TaskResultSettlementRecord>::new(),
        "an unrelated input does not rearm another result's paused obligation",
    );
    let admitted = admit(
        &db,
        &record,
        Snapshot::Unsettled,
        "normal-input",
        now + Duration::days(1),
    )
    .unwrap();
    assert!(admitted.is_empty());
    assert_eq!(latest(&db, &record), paused);
}

#[test]
fn recovery_snapshot_requires_runtime_provenance_but_normal_input_is_not_charged() {
    for snapshot in SNAPSHOTS {
        for invalid_field in ["authority", "surface", "context", "agent"] {
            let (_dir, db) = runtime_db();
            let record = result_fixture(&db, snapshot);
            let mut source = recovery_source(&db, "recovery-source");
            match invalid_field {
                "authority" => source.authority_class = AuthorityClass::OperatorInstruction,
                "surface" => source.delivery_surface = Some(MessageDeliverySurface::TaskRejoin),
                "context" => source.admission_context = None,
                "agent" => source.agent_id = "agent-b".into(),
                _ => unreachable!(),
            }
            let persisted = db.messages().upsert(&source);
            if invalid_field == "agent" {
                assert!(persisted.unwrap_err().to_string().contains("changed agent"));
                assert_eq!(latest(&db, &record), record);
                continue;
            }
            persisted.unwrap();
            canonical_attempt(&db, &source, "root-a", None);
            let now = record.next_recheck_at.unwrap();
            assert!(admit(&db, &record, snapshot, "root-a", now)
                .unwrap()
                .is_empty());
            assert_eq!(latest(&db, &record), record);
        }
        let (_dir, db) = runtime_db();
        let record = result_fixture(&db, snapshot);
        let mut source = recovery_source(&db, "normal-source");
        source.origin = MessageOrigin::System {
            subsystem: "operator_dispatch".into(),
        };
        db.messages().upsert(&source).unwrap();
        canonical_attempt(&db, &source, "normal-input", None);
        let admitted = admit(
            &db,
            &record,
            snapshot,
            "normal-input",
            record.next_recheck_at.unwrap(),
        )
        .unwrap();
        assert_eq!(admitted.len(), 1, "{snapshot:?}");
        assert_eq!(admitted[0].recovery_attempts, 0);
        assert_eq!(admitted[0].last_recovery_root_attempt_id, None);
    }
}
