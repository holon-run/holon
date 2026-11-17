//! Original live lifecycle checks for dependency-based Task preservation.
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime_db::task_snapshot as tasks;
use std::{
    sync::{mpsc, Arc},
    time::Duration,
};

#[path = "prepared_live_oracle_tests.rs"]
mod oracle;

fn task_request() -> tasks::TaskDomainLoadRequest {
    tasks::TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        recovery_at: None,
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}

fn resident_with_task(
    f: &RecoveryFixture,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<crate::runtime::resident_task::ResidentTaskDomain>> {
    use crate::runtime::resident_task::{TaskFamily, TaskKey};
    let task = TaskRecord {
        id: "unrelated-command".into(),
        agent_id: f.f.scope.agent_id.clone(),
        kind: crate::types::TaskKind::CommandTask,
        status: crate::types::TaskStatus::Running,
        created_at: f.f.at,
        updated_at: f.f.at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("resident body must not be decoded by live input".into()),
        detail: Some(serde_json::json!({"payload": "x".repeat(64 * 1024)})),
        recovery: None,
    };
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
    f.f.db.transaction_once(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &f.f.scope, &keys)?;
        upsert_task_tx(tx, &task)?;
        tasks::register_task_writes_tx(tx, &proof, &keys, &f.f.state()?, &task_request())?;
        Ok(())
    })?;
    let snapshot = f.f.db.transaction_once(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.f.scope, &task_request(), ledger)?
            .require_current()
    })?;
    Ok(Arc::new(snapshot.resident))
}

fn work_request() -> crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
    crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}

fn coverage_json(f: &RecoveryFixture) -> Result<String> {
    Ok(f.f.db.connection()?.query_row(
        "SELECT coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![f.f.scope.agent_id, f.f.scope.identity_incarnation],
        |row| row.get(0),
    )?)
}

#[test]
fn prepared_live_input_warm_recovery_preserves_nonempty_task_domain_and_matches_full_oracle(
) -> Result<()> {
    let f = RecoveryFixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let resident = resident_with_task(&f, &ledger)?;
    let before = coverage_json(&f)?;
    let prepared = f.freeze(f.command(None))?;
    let mut bases = PreparedTaskBaselines::default();
    bases.defer_roots(vec![resident.clone()]);
    let InputMutationExecution::Committed { value, receipt } =
        f.f.db
            .transitions_in_scope(&f.f.scope)
            .commit_prepared_transition_with_task_preparation(
                &prepared,
                &work_request(),
                &ledger,
                &task_request(),
                &mut bases,
            )?
    else {
        anyhow::bail!("warm recovery did not commit")
    };
    assert!(value.applied && value.effects.current_read_writes.is_some());
    assert!(value.effects.task_delta.is_none());
    assert!(value.effects.task_write_keys.is_empty());
    assert_eq!(coverage_json(&f)?, before);
    assert!(!bases.has_prepared_scope(&f.f.scope));
    assert!(Arc::ptr_eq(bases.resident(&f.f.scope).unwrap(), &resident));
    let oracle = f.f.db.transaction_once(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &f.f.scope,
            &task_request(),
            &ledger,
        )?
        .require_current()
    })?;
    assert_eq!(oracle.resident.coverage(), resident.coverage());
    assert_eq!(
        oracle.resident.selected_install_records(),
        resident.selected_install_records()
    );
    assert_eq!(f.f.receipt_count()?, 1);
    assert!(matches!(
        f.f.db.transitions_in_scope(&f.f.scope).commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { receipt: replay, .. } if replay == receipt
    ));
    Ok(())
}

#[test]
fn prepared_live_input_dependency_probe_never_reads_task_bodies_or_coverage_json() -> Result<()> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let f = RecoveryFixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let resident = resident_with_task(&f, &ledger)?;
    let prepared = f.freeze(f.command(None))?;
    let mut bases = PreparedTaskBaselines::default();
    bases.defer_roots(vec![resident]);
    let mut connection = f.f.db.connection()?;
    connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Read {
            table_name: "tasks" | "task_domain_records" | "task_stop_effects",
            ..
        }
        | AuthAction::Read {
            table_name: "task_domain_current",
            column_name: "coverage_json",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }));
    let tx = connection.transaction()?;
    let proof = live_task::UnchangedLiveTask::prepare_tx(
        &tx,
        &prepared,
        &f.f.scope,
        &task_request(),
        &ledger,
        None,
        Some(&bases),
    )?
    .context("warm recovery unexpectedly required Task preparation")?;
    let mut value = f.f.db.transitions().commit_live_input_recovery_tx(
        &tx,
        Some(&f.f.scope),
        &f.attempt,
        &f.turn,
        &f.predecessor,
        &f.audit_event_id,
        None,
        Some(proof.allowed_keys()),
    )?;
    assert!(value.applied);
    proof.finish(&mut value)?;
    // The same authorizer must reject the conservative metadata read.
    assert!(tx
        .query_row("SELECT coverage_json FROM task_domain_current", [], |r| {
            r.get::<_, String>(0)
        })
        .is_err());
    tx.rollback()?;
    Ok(())
}

#[test]
fn prepared_live_input_dependency_probe_rejects_uncovered_selectors_and_recovery() -> Result<()> {
    use crate::runtime::resident_task::{TaskFamily, TaskKey};
    let f = RecoveryFixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let resident = resident_with_task(&f, &ledger)?;
    let key = TaskKey::new(TaskFamily::Task, "not-observed");
    for selector in 0..7 {
        let mut request = task_request();
        match selector {
            0 => {
                request.query.required_keys.insert(key.clone());
            }
            1 => {
                request.capture.required_keys.insert(key.clone());
            }
            2 => {
                request.query.output_task_ids.insert(key.id.clone());
            }
            3 => {
                request
                    .query
                    .reply_message_ids
                    .insert("not-observed".into());
            }
            4 => {
                request.query.parent_turn_ids.insert(f.turn.turn_id.clone());
            }
            5 => {
                request
                    .capture
                    .recovery_source_ids
                    .insert("not-observed".into());
            }
            _ => request.recovery_at = Some(f.f.at),
        }
        f.f.db.transaction_once(|tx| {
            assert!(
                !tasks::unchanged::resident_is_current_tx(tx, &f.f.scope, &resident, &request)?,
                "uncovered selector {selector} was mistaken for an empty observation"
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn prepared_live_input_dependency_probe_rechecks_old_source_and_result_relations() -> Result<()> {
    use crate::domain::execution_protocol::TaskRejoinSource;
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementRecord, TaskResultSettlementState,
    };
    let f = RecoveryFixture::new()?;
    let mut source = f.f.queue()?.command.message_evidence.remove(0);
    source.id = "message:old-task-source".into();
    source.origin = MessageOrigin::Task {
        task_id: "old-task".into(),
    };
    source.metadata = Some(serde_json::json!({"task_id": "old-task"}));
    let source =
        f.f.db
            .messages()
            .append_with_index_changes_in_scope(&f.f.scope, &source, &[])?
            .record;
    // The proposed envelope no longer names a Task. Its old accepted Source
    // must still prevent bypassing the original Task writer.
    let mut proposed = source.clone();
    proposed.origin = MessageOrigin::System {
        subsystem: "agent_message".into(),
    };
    proposed.metadata = None;
    f.f.db.transaction_once(|tx| {
        assert!(!tasks::unchanged::message_is_unrelated_tx(
            tx, &f.f.scope, &proposed
        )?);
        let message =
            crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &f.f.scope.agent_id)
                .message_by_id(&f.turn.input_message_ids[0])?
                .context("original Message missing")?;
        assert!(tasks::unchanged::message_is_unrelated_tx(
            tx, &f.f.scope, &message
        )?);
        let settlement = TaskResultSettlementRecord {
            result_identity: "late-result-relation".into(),
            agent_id: f.f.scope.agent_id.clone(),
            task_id: "old-task".into(),
            message_id: message.id.clone(),
            work_item_id: None,
            rejoin_generation: 1,
            source: TaskRejoinSource::ParentTurn {
                parent_turn_id: f.predecessor.turn_id.clone(),
            },
            task_status: "completed".into(),
            state: TaskResultSettlementState::CallerAdmitted,
            activation_id: f.attempt.recovery_of_attempt_id.clone(),
            disposition: None,
            created_at: f.f.at,
            updated_at: f.f.at,
            admitted_at: Some(f.f.at),
            settled_at: None,
            deferred_reason: None,
            deferred_at: None,
            next_recheck_at: None,
            recovery_epoch: 0,
            recovery_attempts: 0,
            last_recovery_root_attempt_id: None,
            last_recovery_admitted_at: None,
            recovery_paused_at: None,
            recovery_diagnostic_at: None,
            source_projection: None,
            reply_projection: None,
        };
        assert!(crate::runtime_db::task_result_settlement::upsert_pending_tx(tx, &settlement)?);
        assert!(!tasks::unchanged::message_is_unrelated_tx(
            tx, &f.f.scope, &message
        )?);
        assert!(live_input::has_result_writes_tx(
            tx,
            &f.freeze(f.command(None))?
        )?);
        tx.execute(
            "DELETE FROM task_result_settlements WHERE result_identity=?1",
            [&settlement.result_identity],
        )?;
        assert!(tasks::unchanged::message_is_unrelated_tx(
            tx, &f.f.scope, &message
        )?);
        assert!(!live_input::has_result_writes_tx(
            tx,
            &f.freeze(f.command(None))?
        )?);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn prepared_live_input_dependency_probe_rejects_source_order_and_queue_membership_changes(
) -> Result<()> {
    let f = RecoveryFixture::new()?;
    f.f.db.transaction_once(|tx| {
        let message =
            crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &f.f.scope.agent_id)
                .message_by_id(&f.turn.input_message_ids[0])?
                .context("original Message missing")?;
        assert!(tasks::unchanged::message_is_unrelated_tx(
            tx, &f.f.scope, &message
        )?);
        let mut source = message.clone();
        source.origin = MessageOrigin::Task {
            task_id: "new-source".into(),
        };
        source.metadata = Some(serde_json::json!({"task_id": "new-source"}));
        assert!(!tasks::unchanged::message_is_unrelated_tx(
            tx, &f.f.scope, &source
        )?);
        // A negative scalar observation must be checked again in the writer,
        // not inferred from the envelope or a previously borrowed Task root.
        assert_eq!(
            tx.execute(
                "UPDATE task_message_order_headers SET message_seq=COALESCE(message_seq,0)+1
                 WHERE agent_id=?1 AND identity_incarnation=?2 AND evidence_id=?3",
                params![
                    f.f.scope.agent_id,
                    f.f.scope.identity_incarnation,
                    message.id
                ],
            )?,
            1
        );
        assert!(!tasks::unchanged::message_is_unrelated_tx(
            tx, &f.f.scope, &message
        )?);
        let mut queue = QueueEntryRecord {
            agent_id: f.f.scope.agent_id.clone(),
            message_id: message.id.clone(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Interjected,
            created_at: f.f.at,
            updated_at: f.f.at,
        };
        for status in [
            QueueEntryStatus::Queued,
            QueueEntryStatus::Interrupted,
            QueueEntryStatus::Dequeued,
            QueueEntryStatus::Interjected,
        ] {
            let before = QueueEntryRecord {
                status,
                ..queue.clone()
            };
            crate::runtime_db::repositories::upsert_queue_entry_tx(tx, &before)?;
            assert!(tasks::unchanged::queue_stays_live_tx(
                tx, &f.f.scope, &queue
            )?);
        }
        assert_eq!(
            tx.execute(
                "DELETE FROM queue_entries WHERE message_id=?1",
                [&message.id]
            )?,
            1
        );
        assert!(!tasks::unchanged::queue_stays_live_tx(
            tx, &f.f.scope, &queue
        )?);
        crate::runtime_db::repositories::upsert_queue_entry_tx(tx, &queue)?;
        queue.status = QueueEntryStatus::Queued;
        assert!(!tasks::unchanged::queue_stays_live_tx(
            tx, &f.f.scope, &queue
        )?);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn prepared_live_input_new_selector_and_stale_revision_use_original_task_path() -> Result<()> {
    for stale in [false, true] {
        let f = RecoveryFixture::new()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let resident = resident_with_task(&f, &ledger)?;
        let prepared = f.freeze(f.command(None))?;
        let mut request = task_request();
        if stale {
            // Model an accepted peer writer advancing the Task lane.
            f.f.db.transaction_once(|tx| {
                tx.execute("UPDATE task_domain_current SET revision=revision+1,coverage_json=json_set(coverage_json,'$.revision',revision+1) WHERE agent_id=?1", [&f.f.scope.agent_id])?;
                Ok(())
            })?;
        } else {
            request.query.parent_turn_ids.insert(f.turn.turn_id.clone());
        }
        let mut bases = PreparedTaskBaselines::default();
        bases.defer_roots(vec![resident]);
        let InputMutationExecution::Committed { value, .. } =
            f.f.db
                .transitions_in_scope(&f.f.scope)
                .commit_prepared_transition_with_task_preparation(
                    &prepared,
                    &work_request(),
                    &ledger,
                    &request,
                    &mut bases,
                )?
        else {
            anyhow::bail!("dependent recovery did not commit")
        };
        assert!(value.applied);
        let delta = value
            .effects
            .task_delta
            .context("dependent input omitted Task ACK")?;
        assert!(
            delta.prepared_base.is_some(),
            "fallback must install its unpublished reader baseline"
        );
        assert!(
            bases.resident(&f.f.scope).is_none(),
            "a reader-prepared baseline is not an installed resident"
        );
        if !stale {
            assert!(delta
                .coverage
                .query
                .parent_turn_tasks
                .contains_key(&f.turn.turn_id));
        }
        assert_eq!(f.f.receipt_count()?, 1);
    }
    Ok(())
}

#[test]
fn prepared_live_input_independent_rollback_keeps_task_domain_and_receipt_absent() -> Result<()> {
    let f = RecoveryFixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let resident = resident_with_task(&f, &ledger)?;
    let before = coverage_json(&f)?;
    let mut bases = PreparedTaskBaselines::default();
    bases.defer_roots(vec![resident]);
    let prepared = f.freeze(f.command(Some(TransitionFaultPoint::AfterCanonicalWrites)))?;
    assert!(f
        .f
        .db
        .transitions_in_scope(&f.f.scope)
        .commit_prepared_transition_with_task_preparation(
            &prepared,
            &work_request(),
            &ledger,
            &task_request(),
            &mut bases,
        )
        .is_err());
    assert_eq!(f.f.receipt_count()?, 0);
    assert_eq!(coverage_json(&f)?, before);
    assert_eq!(
        f.f.db
            .turn_records()
            .by_id(Some(&f.f.scope.agent_id), &f.turn.turn_id)?,
        Some(f.turn.clone())
    );
    Ok(())
}

#[test]
fn prepared_live_input_reprepare_reader_barrier_allows_peer_writer_and_full_ack() -> Result<()> {
    let f = RecoveryFixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let resident = resident_with_task(&f, &ledger)?;
    let prepared = f.freeze(f.command(None))?;
    let directory = tempfile::tempdir()?;
    let full_db = oracle::fork_db(&f.f.db, directory.path())?;
    let request = task_request();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (written_tx, written_rx) = mpsc::channel();
    let execution = std::thread::scope(|threads| -> Result<_> {
        let reader_db = f.f.db.clone();
        let reader_scope = f.f.scope.clone();
        let reader_ledger = ledger.clone();
        let reader_request = request.clone();
        let prepared = &prepared;
        let writer_at = f.f.at + chrono::Duration::seconds(1);
        let reader = threads.spawn(move || {
            task_baselines::with_reader_barrier(
                move || {
                    entered_tx.send(())?;
                    release_rx.recv_timeout(Duration::from_secs(10))?;
                    Ok(())
                },
                || {
                    // No installed root: the real live writer must unwind its
                    // preflight and perform the original reader preparation.
                    crate::runtime_db::transitions::with_live_input_oracle(writer_at, || {
                        reader_db
                            .transitions_in_scope(&reader_scope)
                            .commit_prepared_transition_with_task_preparation(
                                prepared,
                                &work_request(),
                                &reader_ledger,
                                &reader_request,
                                &mut PreparedTaskBaselines::default(),
                            )
                    })
                },
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(10))?;
        let writer_db = f.f.db.clone();
        let writer = threads.spawn(move || {
            let written = writer_db.transaction_once(|tx| {
                upsert_agent_state_tx(tx, &AgentState::new("live-barrier-peer"))?;
                Ok(())
            });
            let _ = written_tx.send(written);
        });
        // Release even on writer failure/timeout: an assertion must not strand
        // the reader or writer. The channels, not sleeps, define the barrier.
        let written = written_rx.recv_timeout(Duration::from_secs(10));
        let _ = release_tx.send(());
        let execution = reader.join().expect("live reader panicked")?;
        writer.join().expect("peer writer panicked");
        written.context("live preparation held the writer barrier")??;
        Ok(execution)
    })?;
    let InputMutationExecution::Committed { value, receipt } = execution else {
        anyhow::bail!("live original ACK missing")
    };
    assert!(value.applied);
    let delta = value
        .effects
        .task_delta
        .as_ref()
        .context("cold Task ACK missing")?;
    assert!(delta.prepared_base.is_some());
    let applied = resident.with_committed(delta)?;
    let truth = f.f.db.transaction_once(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.f.scope, &request, &ledger)?
            .require_current()
    })?;
    assert_eq!(applied.coverage(), truth.resident.coverage());
    assert_eq!(
        applied.selected_install_records(),
        truth.resident.selected_install_records()
    );
    let (full, ..) = oracle::commit_recovery(&full_db, &f, &prepared, &resident, true)?;
    oracle::compare_acks(&value, &full)?;
    assert!(f.f.db.agent_states().latest("live-barrier-peer")?.is_some());
    assert_eq!(f.f.receipt_count()?, 1);
    assert!(matches!(
        f.f.db
            .transitions_in_scope(&f.f.scope)
            .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { receipt: replay, .. } if replay == receipt
    ));
    Ok(())
}
