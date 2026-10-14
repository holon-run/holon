use super::*;
use crate::runtime::resident_task::{ResidentTaskDomain, TaskReadError};
use crate::runtime_db::task_snapshot::{
    prepare_task_domain_snapshot_with_budget_tx, register_task_writes_with_budget_tx,
    register_task_writes_with_stop_observation_tx, TaskSnapshotError,
};

fn selected(id: &str) -> TaskCaptureRequest {
    TaskCaptureRequest {
        include_active: false,
        include_unsettled: false,
        terminal_limit: 0,
        required_keys: BTreeSet::from([TaskKey::new(TaskFamily::Task, id)]),
    }
}
fn current(tx: &Transaction<'_>, ledger: &InputEvidenceBudgetLedger) -> Result<ResidentTaskDomain> {
    Ok(
        prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), ledger)?
            .require_current()?
            .resident,
    )
}

#[test]
fn pending_stop_survives_terminal_window_and_capture_releases_weak_body() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let effect =
        db.transaction(|tx| stop(tx, &ledger, "outside-history", TaskStopSignal::Cancel, true))?;
    let mut window = request();
    window.capture.terminal_limit = 0;
    let domain = db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "original-task")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        let mut terminal = proof.task_before("original-task")?.clone();
        terminal.status = TaskStatus::Cancelled;
        terminal.updated_at = at() + chrono::Duration::seconds(1);
        repositories::upsert_task_tx(tx, &terminal)?;
        register_task_writes_with_budget_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("stop-agent"),
            &window,
            &ledger,
        )?;
        Ok(
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &window, &ledger)?
                .require_current()?
                .resident,
        )
    })?;
    let ordinary = domain.capture(&window.capture)?;
    assert!(ordinary.records.is_empty());
    assert!(ordinary.stop_records.is_empty());
    assert!(matches!(
        ordinary
            .pending_stops()
            .err()
            .and_then(|e| e.downcast::<TaskReadError>().ok()),
        Some(TaskReadError::IncompleteStops)
    ));
    drop(ordinary);
    let pending = domain.capture_pending_stops()?;
    assert!(
        pending.records.is_empty(),
        "original ACK must not become a current Task body"
    );
    assert_eq!(
        pending
            .pending_stops()?
            .map(|s| s.effect_seq)
            .collect::<Vec<_>>(),
        vec![effect.effect_seq]
    );
    assert!(pending.stop_head("original-task").is_err());
    let weak = Arc::downgrade(&pending.stop_records[0]);
    let observed = ledger.report()?.observed_bytes;
    assert!(observed > 0);
    drop(domain);
    assert!(
        weak.upgrade().is_some(),
        "selected pin keeps only its Stop Arc alive"
    );
    drop(pending);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn selected_task_head_is_latest_sequence_and_none_is_explicit() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let cancel =
        db.transaction(|tx| stop(tx, &ledger, "head-cancel", TaskStopSignal::Cancel, true))?;
    let force =
        db.transaction(|tx| stop(tx, &ledger, "head-force", TaskStopSignal::Force, true))?;
    let domain = db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "without-stop")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        let mut other = task();
        other.id = "without-stop".into();
        repositories::upsert_task_tx(tx, &other)?;
        register_task_writes_with_budget_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("stop-agent"),
            &request(),
            &ledger,
        )?;
        current(tx, &ledger)
    })?;
    let capture = domain.capture(&selected("original-task"))?;
    assert_eq!(capture.stop_records.len(), 1);
    assert_eq!(
        capture.stop_head("original-task")?.unwrap().effect_seq,
        force.effect_seq
    );
    assert!(capture.stop_head("without-stop").is_err());
    assert!(!capture.coverage.stop.pending_complete);
    let all = domain.capture_pending_stops()?;
    assert_eq!(
        all.pending_stops()?
            .map(|r| r.effect_seq)
            .collect::<Vec<_>>(),
        vec![cancel.effect_seq, force.effect_seq]
    );
    let none = domain.capture(&selected("without-stop"))?;
    assert!(none.stop_head("without-stop")?.is_none());
    assert!(none.stop_records.is_empty());
    assert_eq!(none.coverage.stop.heads.len(), 1);
    // Candidate, Current and Pins share actual allocation identity, not copies.
    let bodies = domain.selected_install_stop_records();
    let actual = bodies
        .iter()
        .find(|r| r.effect_seq == force.effect_seq)
        .unwrap();
    assert!(Arc::ptr_eq(actual, &capture.stop_records[0]));
    let candidate = domain.candidate_stop_budget_view()?;
    let weak = Arc::downgrade(actual);
    drop(bodies);
    drop(domain);
    drop(all);
    drop(none);
    assert!(weak.upgrade().is_some());
    drop(capture);
    assert!(
        weak.upgrade().is_none(),
        "budget guard must not own Stop body"
    );
    drop(candidate);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn stop_progress_alone_advances_task_revision_once_and_retains_head() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let original =
        db.transaction(|tx| stop(tx, &ledger, "progress-only", TaskStopSignal::Cancel, true))?;
    let joined = crate::runtime::task_stop_join::join_fixture(&original, false, &ledger).await?;
    let old = db.transaction(|tx| current(tx, &ledger))?;
    let old_capture = old.capture(&selected("original-task"))?;
    let delta = db.transaction(|tx| {
        let proof = prepare_task_write_tx(tx, &scope(), &BTreeSet::new())?;
        let changed =
            mark_task_stop_monitor_joined_tx(tx, &original, &joined, at(), MAX_BYTES, &ledger)?;
        assert!(changed.changed);
        let facts = register_task_writes_with_budget_tx(
            tx,
            &proof,
            &BTreeSet::new(),
            &AgentState::new("stop-agent"),
            &request(),
            &ledger,
        )?;
        assert_eq!(facts.delta.revision, old.coverage().revision + 1);
        Ok(facts.delta)
    })?;
    let new = old.with_committed(&delta)?;
    assert!(new
        .capture_pending_stops()?
        .pending_stops()?
        .next()
        .is_none());
    let new_capture = new.capture(&selected("original-task"))?;
    assert_eq!(
        old_capture
            .stop_head("original-task")?
            .unwrap()
            .progress
            .physical,
        TaskStopPhysicalState::Pending
    );
    assert_eq!(
        new_capture
            .stop_head("original-task")?
            .unwrap()
            .progress
            .physical,
        TaskStopPhysicalState::MonitorJoined
    );
    db.transaction(|tx| {
        let proof = prepare_task_write_tx(tx, &scope(), &BTreeSet::new())?;
        assert!(
            !mark_task_stop_monitor_joined_tx(tx, &original, &joined, at(), MAX_BYTES, &ledger)?
                .changed
        );
        let replay = register_task_writes_with_budget_tx(
            tx,
            &proof,
            &BTreeSet::new(),
            &AgentState::new("stop-agent"),
            &request(),
            &ledger,
        )?;
        assert_eq!(replay.delta.revision, new.coverage().revision);
        assert!(replay.delta.stop_records.is_empty());
        Ok(())
    })?;
    drop((delta, new_capture, old_capture, new, old, joined));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pending_stop_scalar_budget_precedes_raw_normalized_and_whole_receipt_decode() -> Result<()> {
    for corrupted in ["progress_json", "updated_at", "outcome_json"] {
        let (_dir, db, ledger) = fixture()?;
        db.transaction(|tx| stop(tx, &ledger, "oversized-stop", TaskStopSignal::Cancel, true))?;
        db.transaction(|tx| {
            let (table, predicate) = if corrupted == "outcome_json" {
                ("input_mutation_receipts", "command_nonce='oversized-stop'")
            } else {
                ("task_stop_effects", "command_nonce='oversized-stop'")
            };
            tx.execute(
                &format!("UPDATE {table} SET {corrupted}=?1 WHERE {predicate}"),
                ["{".repeat(32 * 1024)],
            )?;
            Ok(())
        })?;
        let mut limited = request();
        limited.byte_limit = 32 * 1024;
        let error = db
            .transaction(|tx| {
                prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &limited, &ledger)
                    .map(|_| ())
            })
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

// Callback runs inside the actual INSERT transaction while descriptor/proof live.
fn with_inserted(
    tx: &Transaction<'_>,
    ledger: &InputEvidenceBudgetLedger,
    body: impl FnOnce(
        &PreparedInputMutation,
        &crate::runtime_db::task_snapshot::TaskWriteProof,
        &TaskStopRecord,
        &TaskStopInsertedObservation<'_, '_>,
    ) -> Result<()>,
) -> Result<()> {
    with_inserted_at(tx, ledger, at(), body)
}

fn with_inserted_at(
    tx: &Transaction<'_>,
    ledger: &InputEvidenceBudgetLedger,
    prepared_at: DateTime<Utc>,
    body: impl FnOnce(
        &PreparedInputMutation,
        &crate::runtime_db::task_snapshot::TaskWriteProof,
        &TaskStopRecord,
        &TaskStopInsertedObservation<'_, '_>,
    ) -> Result<()>,
) -> Result<()> {
    let scope = scope();
    let request = request();
    let key = TaskKey::new(TaskFamily::Task, "original-task");
    let snapshot = current(tx, ledger)?;
    let locator = snapshot
        .capture(&selected("original-task"))?
        .lookup(&key)?
        .unwrap()
        .locator
        .clone();
    let original = prepare_task_write_tx(tx, &scope, &BTreeSet::from([key]))?;
    let intent = PreparedTaskStop {
        task: locator,
        kind: TaskKind::CommandTask,
        created_at: at(),
        rejoin: None,
        owner_epoch: 1,
        signal: TaskStopSignal::Cancel,
        lease_id: Some("original-lease".into()),
        child: None,
        child_unavailable: None,
        worktree: None,
        worktree_unavailable: None,
        result: None,
    };
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::Task,
        "insert-cap".into(),
        vec![scope],
        canonical_command_digest(&intent)?,
        prepared_at,
    )?;
    let proof = prepare_task_stop_tx(tx, &prepared, &intent, &original, &request, ledger)?;
    let mut expected = original.task_before("original-task")?.clone();
    expected.status = TaskStatus::Cancelling;
    expected.detail.as_mut().unwrap()["cancel_requested"] = serde_json::json!(true);
    repositories::upsert_task_tx(tx, &expected)?;
    tx.execute_batch("SAVEPOINT first_stop")?;
    let (record, inserted) = insert_task_stop_with_observation_ack_tx(&proof, &expected)?;
    body(&prepared, &original, &record, &inserted)
}

#[test]
fn first_insert_cap_observes_before_whole_receipt() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| {
        with_inserted(tx, &ledger, |prepared, proof, record, inserted| {
            assert!(
                load_pending_task_stops_tx(tx, &scope(), MAX_RECORDS, MAX_BYTES, &ledger).is_err(),
                "ordinary lookup cannot borrow first-INSERT authority"
            );
            let facts = register_task_writes_with_stop_observation_tx(
                tx,
                proof,
                &BTreeSet::from([TaskKey::new(TaskFamily::Task, "original-task")]),
                &AgentState::new("stop-agent"),
                &request(),
                &ledger,
                None,
                inserted,
            )?;
            assert_eq!(
                facts.delta.coverage.stop.pending_effect_seqs,
                BTreeSet::from([record.effect_seq])
            );
            let outcome = InputMutationOutcome {
                result: InputMutationResult::Task {
                    task_id: "original-task".into(),
                    deletion: None,
                    stop: Some(record.receipt_binding()),
                },
                applied: true,
                delivery_receipt: None,
                scopes: vec![InputMutationScopeOutcome {
                    scope: scope(),
                    install_keys: Default::default(),
                    audit_events: vec![],
                    notify_memory_index: false,
                    notify_scheduler: false,
                }],
            };
            insert_input_mutation_receipt_tx(tx, prepared, &outcome, at())?;
            Ok(())
        })
    })?;
    db.transaction(|tx| {
        let domain = current(tx, &ledger)?;
        assert_eq!(domain.capture_pending_stops()?.pending_stops()?.count(), 1);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn first_insert_cap_rejects_rollback_tampering_and_another_transaction() -> Result<()> {
    for failure in ["rollback", "tamper", "other-tx"] {
        let (_dir, db, ledger) = fixture()?;
        let (_other_dir, other_db, _other_ledger) = fixture()?;
        let failure_result = db.transaction(|tx| with_inserted(tx, &ledger, |_, proof, _, inserted| {
            if failure == "other-tx" {
                other_db.transaction(|other| {
                    assert!(format!("{:#}", inserted.validate_presence_tx(other).unwrap_err()).contains("crossed transaction"));
                    Ok(())
                })?;
            } else {
                if failure == "rollback" {
                    tx.execute_batch("ROLLBACK TO first_stop")?;
                } else {
                    tx.execute("UPDATE task_stop_effects SET updated_at='2026-10-01T00:00:01.123Z' WHERE command_nonce='insert-cap'", [])?;
                }
                let error = register_task_writes_with_stop_observation_tx(tx, proof,
                    &BTreeSet::from([TaskKey::new(TaskFamily::Task, "original-task")]),
                    &AgentState::new("stop-agent"), &request(), &ledger, None, inserted).unwrap_err();
                assert!(format!("{error:#}").contains(if failure == "rollback" { "lost its actual row" } else { "actual row changed" }), "{error:#}");
            }
            anyhow::bail!("rollback test transaction")
        }));
        assert!(failure_result.is_err());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        db.transaction(|tx| {
            assert_eq!(
                tx.query_row("SELECT COUNT(*) FROM task_stop_effects", [], |r| r
                    .get::<_, usize>(0))?,
                0
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn canonical_task_noop_with_new_stop_advances_domain_without_actual_task_keys() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let first =
        db.transaction(|tx| stop(tx, &ledger, "first-cancel", TaskStopSignal::Cancel, true))?;
    let old = db.transaction(|tx| current(tx, &ledger))?;
    let facts = db.transaction(|tx| {
        let mut final_facts = None;
        with_inserted(tx, &ledger, |prepared, proof, record, inserted| {
            assert_eq!(record.ack_task, first.ack_task);
            assert!(record.effect_seq > first.effect_seq);
            let facts = register_task_writes_with_stop_observation_tx(
                tx,
                proof,
                &BTreeSet::new(),
                &AgentState::new("stop-agent"),
                &request(),
                &ledger,
                None,
                inserted,
            )?;
            assert_eq!(facts.delta.revision, old.coverage().revision + 1);
            let outcome = InputMutationOutcome {
                result: InputMutationResult::Task {
                    task_id: "original-task".into(),
                    deletion: None,
                    stop: Some(record.receipt_binding()),
                },
                applied: true,
                delivery_receipt: None,
                scopes: vec![InputMutationScopeOutcome {
                    scope: scope(),
                    install_keys: Default::default(),
                    audit_events: vec![],
                    notify_memory_index: false,
                    notify_scheduler: false,
                }],
            };
            insert_input_mutation_receipt_tx(tx, prepared, &outcome, at())?;
            final_facts = Some(facts);
            Ok(())
        })?;
        final_facts.context("missing actual Stop finalizer")
    })?;
    let next = old.with_committed(&facts.delta)?;
    let capture = next.capture(&selected("original-task"))?;
    assert!(capture.stop_head("original-task")?.unwrap().effect_seq > first.effect_seq);
    assert_eq!(next.capture_pending_stops()?.pending_stops()?.count(), 2);
    drop((capture, next, old, facts));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pending_stop_does_not_make_no_state_or_retired_task_scope_ready() -> Result<()> {
    for retired in ["no-state", "deleted", "successor"] {
        let (_dir, db, ledger) = fixture()?;
        db.transaction(|tx| stop(tx, &ledger, "retired-pending", TaskStopSignal::Cancel, true))?;
        db.transaction(|tx| {
            if retired == "no-state" {
                tx.execute("DELETE FROM agent_states WHERE agent_id='stop-agent'", [])?;
            } else {
                tx.execute("UPDATE agent_identities SET status=?1,payload_json=json_set(payload_json,'$.status',?1,'$.incarnation',?2) WHERE agent_id='stop-agent'",
                    params![if retired == "deleted" { "deleted" } else { "active" }, if retired == "deleted" {1} else {2}])?;
                tx.execute_batch("ALTER TABLE agent_states RENAME TO hidden_successor_state")?;
            }
            assert!(matches!(prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?,
                crate::runtime_db::task_snapshot::TaskBootstrapSnapshot::Retired { .. }));
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_stop_effects WHERE physical_state='pending'", [], |r| r.get::<_,usize>(0))?, 1);
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn ordinary_head_observation_does_not_decode_completed_nonhead_history() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let first = db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "old-terminal-head",
            TaskStopSignal::Cancel,
            true,
        )
    })?;
    let joined = crate::runtime::task_stop_join::join_fixture(&first, false, &ledger).await?;
    db.transaction(|tx| {
        mark_task_stop_monitor_joined_tx(tx, &first, &joined, at(), MAX_BYTES, &ledger)?;
        Ok(())
    })?;
    drop(joined);
    let latest = db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "latest-terminal-head",
            TaskStopSignal::Force,
            true,
        )
    })?;
    let joined = crate::runtime::task_stop_join::join_fixture(&latest, false, &ledger).await?;
    db.transaction(|tx| {
        mark_task_stop_monitor_joined_tx(tx, &latest, &joined, at(), MAX_BYTES, &ledger)?;
        // Deliberate cold-history corruption must not turn selected-head loading
        // into a scan or decode of all completed Stop effects.
        tx.execute(
            "UPDATE task_stop_effects SET progress_json=?1 WHERE effect_seq=?2",
            params!["{".repeat(256 * 1024), first.effect_seq],
        )?;
        let domain = current(tx, &ledger)?;
        assert_eq!(domain.selected_install_stop_records().len(), 1);
        let capture = domain.capture(&selected("original-task"))?;
        assert_eq!(
            capture.stop_head("original-task")?.unwrap().effect_seq,
            latest.effect_seq
        );
        assert!(domain
            .capture_pending_stops()?
            .pending_stops()?
            .next()
            .is_none());
        Ok(())
    })?;
    drop(joined);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn first_insert_cap_preserves_nanosecond_descriptor_with_actual_millisecond_progress() -> Result<()>
{
    let (_dir, db, ledger) = fixture()?;
    let precise: DateTime<Utc> = "2026-10-01T00:00:00.123456789Z".parse()?;
    db.transaction(|tx| {
        with_inserted_at(tx, &ledger, precise, |prepared, proof, record, inserted| {
            assert_eq!(prepared.prepared_at(), precise);
            assert_eq!(record.prepared.prepared_at(), precise);
            assert_eq!(record.updated_at, at());
            register_task_writes_with_stop_observation_tx(
                tx,
                proof,
                &BTreeSet::from([TaskKey::new(TaskFamily::Task, "original-task")]),
                &AgentState::new("stop-agent"),
                &request(),
                &ledger,
                None,
                inserted,
            )?;
            let outcome = InputMutationOutcome {
                result: InputMutationResult::Task {
                    task_id: "original-task".into(),
                    deletion: None,
                    stop: Some(record.receipt_binding()),
                },
                applied: true,
                delivery_receipt: None,
                scopes: vec![InputMutationScopeOutcome {
                    scope: scope(),
                    install_keys: Default::default(),
                    audit_events: vec![],
                    notify_memory_index: false,
                    notify_scheduler: false,
                }],
            };
            insert_input_mutation_receipt_tx(tx, prepared, &outcome, precise)?;
            Ok(())
        })
    })?;
    db.transaction(|tx| {
        let observed = load_pending_task_stops_tx(tx, &scope(), MAX_RECORDS, MAX_BYTES, &ledger)?;
        assert_eq!(observed.records.len(), 1);
        assert_eq!(observed.records[0].prepared.prepared_at(), precise);
        assert_eq!(observed.records[0].updated_at, at());
        assert_eq!(
            current(tx, &ledger)?
                .capture_pending_stops()?
                .pending_stops()?
                .count(),
            1
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
