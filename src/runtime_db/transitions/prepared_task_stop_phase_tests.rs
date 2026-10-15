use super::prepared_task_stop_tests::{bundle, request, rows, seed};
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory},
        task_stop_join,
    },
    runtime_db::{task_snapshot as tasks, task_stop as stops},
    types::TaskKind,
};
use std::sync::Arc;

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn work() -> crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
    crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
struct StopFixture {
    record: Arc<stops::TaskStopRecord>,
    _view: Arc<BudgetViewGuard>,
}
fn original_stop(f: &Fixture, ledger: &InputEvidenceBudgetLedger) -> Result<StopFixture> {
    let task = seed(f, TaskKind::CommandTask)?;
    let prepared = bundle(
        f,
        &task,
        "phase-original-stop",
        stops::TaskStopSignal::Cancel,
        None,
    )?;
    let InputMutationExecution::Committed { receipt, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &prepared,
                &work(),
                ledger,
                &request(),
            )?
    else {
        unreachable!()
    };
    let InputMutationResult::Task {
        stop: Some(binding),
        ..
    } = &receipt.outcome.result
    else {
        unreachable!()
    };
    f.db.transaction(|tx| {
        let loaded = stops::load_task_stop_by_sequence_tx(
            tx,
            &f.scope,
            binding.effect_seq,
            request().byte_limit,
            ledger,
        )?;
        let record = loaded.records[0].clone();
        let view = ledger.register_task_stop_view(
            &f.scope,
            InputMemoryCategory::Candidate,
            std::iter::once(&record),
        )?;
        Ok(StopFixture {
            record,
            _view: view,
        })
    })
}
fn commit(
    f: &Fixture,
    prepared: &PreparedInputTransition,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            prepared,
            &work(),
            ledger,
            &request(),
        )
}
fn ack(receipt: &InputMutationReceipt) -> &stops::TaskStopPhaseAck {
    let InputMutationResult::TaskStopPhase { ack } = &receipt.outcome.result else {
        panic!("wrong original phase ACK")
    };
    ack
}

#[tokio::test]
async fn stop_progress_original_join_is_whole_receipt_and_one_current_task_revision() -> Result<()>
{
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let source = original_stop(&f, &ledger)?;
    let old = f.db.transaction(|tx| {
        Ok(
            tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &request(), &ledger)?
                .require_current()?
                .resident,
        )
    })?;
    let joined = Arc::new(task_stop_join::join_fixture(&source.record, false, &ledger).await?);
    let at = source.record.updated_at
        + chrono::Duration::seconds(1)
        + chrono::Duration::nanoseconds(456789);
    let phase = PreparedInputTransition::task_stop_monitor_joined(
        &source.record,
        "original-join-phase".into(),
        at,
        joined,
        &request(),
        &ledger,
    )?;
    assert!(phase.pending_messages()?.is_empty());
    assert!(!phase.dispatch_memory_candidate());
    assert_eq!(phase.descriptor().scopes(), std::slice::from_ref(&f.scope));
    let before = rows(&f)?;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &phase, &ledger)? else {
        unreachable!()
    };
    assert!(value.applied);
    assert_eq!(receipt.prepared.prepared_at(), at);
    assert_ne!(at.timestamp_subsec_nanos() % 1_000_000, 0);
    f.db.transaction(|tx| {
        let actual: String = tx.query_row(
            "SELECT updated_at FROM task_stop_effects WHERE effect_seq=?1",
            [source.record.effect_seq],
            |r| r.get(0),
        )?;
        assert_eq!(actual, crate::runtime_db::migrations::timestamp(at));
        Ok(())
    })?;
    assert_eq!(
        ack(&receipt).progress.physical,
        stops::TaskStopPhysicalState::MonitorJoined
    );
    assert_eq!(
        ack(&receipt).install_mode,
        stops::TaskStopPhaseInstallMode::Current
    );
    assert_eq!(ack(&receipt).revision, source.record.revision + 1);
    let delta = value
        .effects
        .task_delta
        .as_ref()
        .context("current progress omitted Task delta")?;
    assert_eq!(delta.revision, old.coverage().revision + 1);
    assert!(value.effects.task_write_keys.is_empty());
    let current = old.with_committed(delta)?;
    assert_eq!(
        current.coverage().stop.records[&source.record.effect_seq]
            .locator
            .revision,
        ack(&receipt).revision
    );
    assert_eq!(
        old.coverage().stop.records[&source.record.effect_seq]
            .locator
            .revision,
        source.record.revision
    );
    let after = rows(&f)?;
    for table in [
        "tasks",
        "messages",
        "queue_entries",
        "task_result_settlements",
        "agent_states",
        "agent_identities",
        "execution_protocol_attempts",
        "turn_records",
    ] {
        assert_eq!(
            before.get(table),
            after.get(table),
            "progress cannot change {table}"
        );
    }
    let InputMutationExecution::Replayed {
        receipt: original, ..
    } = commit(&f, &phase, &ledger)?
    else {
        unreachable!()
    };
    assert_eq!(original, receipt);
    assert_eq!(rows(&f)?, after);
    drop((value, current, old, phase, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn stop_progress_receipt_last_and_finalizer_fault_roll_back_actual_progress() -> Result<()> {
    for table in ["task_domain_current", "input_mutation_receipts"] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let source = original_stop(&f, &ledger)?;
        let phase = PreparedInputTransition::task_stop_safe_progress(
            &source.record,
            "rollback-progress".into(),
            source.record.updated_at + chrono::Duration::seconds(1),
            stops::TaskStopProgressAction::PhysicalAttention {
                reason: "original monitor failed to join".into(),
            },
            &request(),
            &ledger,
        )?;
        let sql = if table == "task_domain_current" {
            "CREATE TRIGGER stop_phase_fault BEFORE UPDATE ON task_domain_current BEGIN SELECT RAISE(ABORT,'Stop finalizer fault'); END;"
        } else {
            "CREATE TRIGGER stop_phase_fault BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='rollback-progress' BEGIN SELECT RAISE(ABORT,'Stop receipt fault'); END;"
        };
        f.db.connection()?.execute_batch(sql)?;
        let before = rows(&f)?;
        let error = commit(&f, &phase, &ledger)
            .err()
            .context("phase fault must roll back")?;
        assert!(format!("{error:#}").contains("fault"));
        assert_eq!(rows(&f)?, before);
        f.db.connection()?
            .execute_batch("DROP TRIGGER stop_phase_fault")?;
        let InputMutationExecution::Committed { receipt, .. } = commit(&f, &phase, &ledger)? else {
            unreachable!()
        };
        assert_eq!(ack(&receipt).revision, source.record.revision + 1);
        drop((phase, source));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn stop_progress_no_state_and_successor_only_invalidate_original_metadata_without_readiness(
) -> Result<()> {
    for mode in ["no_state", "successor", "incomplete"] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let source = original_stop(&f, &ledger)?;
        let phase = PreparedInputTransition::task_stop_safe_progress(
            &source.record,
            "old-scope-progress".into(),
            source.record.updated_at + chrono::Duration::seconds(1),
            stops::TaskStopProgressAction::PhysicalAttention {
                reason: "old monitor unavailable".into(),
            },
            &request(),
            &ledger,
        )?;
        f.db.transaction(|tx| {
            if mode=="successor" {
                // Explicit conflicting-restore fixture; ordinary identity APIs
                // would require original coordination before reincarnation.
                tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&f.scope.agent_id])?;
                tx.execute("UPDATE agent_states SET payload_json='poison-successor-State' WHERE agent_id=?1",[&f.scope.agent_id])?;
            } else if mode=="incomplete" {
                // State exists but is intentionally unreadable: only complete
                // scalar coverage may permit the full current-domain path.
                tx.execute("UPDATE agent_states SET payload_json='poison-unprepared-State' WHERE agent_id=?1",[&f.scope.agent_id])?;
                tx.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.active_complete',json('false'),'$.unsettled_complete',json('false'),'$.stop.pending_complete',json('false')) WHERE agent_id=?1 AND identity_incarnation=1",[&f.scope.agent_id])?;
            } else {tx.execute("DELETE FROM agent_states WHERE agent_id=?1",[&f.scope.agent_id])?;}
            Ok(())
        })?;
        let before = rows(&f)?;
        let InputMutationExecution::Committed { value, receipt } = commit(&f, &phase, &ledger)?
        else {
            unreachable!()
        };
        assert!(value.effects.task_delta.is_none());
        assert!(value.effects.tasks.is_empty());
        assert!(value.effects.agent_state.is_none());
        assert!(value.effects.agent_identities.is_empty());
        assert_eq!(
            ack(&receipt).install_mode,
            match mode {
                "successor" => stops::TaskStopPhaseInstallMode::Retired,
                "incomplete" => stops::TaskStopPhaseInstallMode::Incomplete,
                _ => stops::TaskStopPhaseInstallMode::NoState,
            }
        );
        assert!(ack(&receipt).invalidation.is_some());
        let after = rows(&f)?;
        for table in [
            "tasks",
            "messages",
            "queue_entries",
            "task_result_settlements",
            "agent_states",
            "agent_identities",
            "task_domain_records",
        ] {
            assert_eq!(
                before.get(table),
                after.get(table),
                "old progress cannot write {table}"
            );
        }
        f.db.transaction(|tx| {
            let coverage:String=tx.query_row("SELECT coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=1",[&f.scope.agent_id],|r|r.get(0))?;
            let coverage:crate::runtime::resident_task::TaskCoverage=serde_json::from_str(&coverage)?;
            assert!(!coverage.active_complete && !coverage.unsettled_complete && !coverage.stop.pending_complete);
            Ok(())
        })?;
        let InputMutationExecution::Replayed {
            receipt: original, ..
        } = commit(&f, &phase, &ledger)?
        else {
            unreachable!()
        };
        assert_eq!(original, receipt);
        assert_eq!(rows(&f)?, after);
        // The actual ACK owns Candidate guards through publication.
        drop((value, original, receipt, phase, source));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn stop_progress_cannot_turn_safe_observation_into_physical_proof_or_replace_original_nonce(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let source = original_stop(&f, &ledger)?;
    let at = source.record.updated_at + chrono::Duration::seconds(1);
    let before = rows(&f)?;
    assert!(PreparedInputTransition::task_stop_safe_progress(
        &source.record,
        "no-join".into(),
        at,
        stops::TaskStopProgressAction::MonitorJoined,
        &request(),
        &ledger
    )
    .is_err());
    let phase = PreparedInputTransition::task_stop_safe_progress(
        &source.record,
        "fixed-progress-nonce".into(),
        at,
        stops::TaskStopProgressAction::PhysicalAttention {
            reason: "original error".into(),
        },
        &request(),
        &ledger,
    )?;
    assert_eq!(rows(&f)?, before);
    let InputMutationExecution::Committed { .. } = commit(&f, &phase, &ledger)? else {
        unreachable!()
    };
    let changed = PreparedInputTransition::task_stop_safe_progress(
        &source.record,
        "fixed-progress-nonce".into(),
        at,
        stops::TaskStopProgressAction::PhysicalAttention {
            reason: "different intent".into(),
        },
        &request(),
        &ledger,
    )?;
    let after = rows(&f)?;
    let error = commit(&f, &changed, &ledger)
        .err()
        .context("changed sameNonce must reject")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::input_mutation_receipts::ReceiptError>(),
        Some(crate::runtime_db::input_mutation_receipts::ReceiptError::CommandConflict)
    ));
    assert_eq!(rows(&f)?, after);
    drop((changed, phase, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn attention_phase(
    source: &StopFixture,
    nonce: &str,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputTransition> {
    PreparedInputTransition::task_stop_safe_progress(
        &source.record,
        nonce.into(),
        source.record.updated_at + chrono::Duration::seconds(1),
        stops::TaskStopProgressAction::PhysicalAttention {
            reason: "original monitor unavailable".into(),
        },
        &request(),
        ledger,
    )
}

#[test]
fn sparse_stop_recovery_requires_finished_original_arc_and_positive_whole_ack() -> Result<()> {
    use crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt;
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let source = original_stop(&f, &ledger)?;
    let phase = attention_phase(&source, "sparse-original-attempt", &ledger)?;
    let attempt = OriginalInputMutationAttempt::new(phase.descriptor().clone());
    let saved = attempt.clone();
    let other = OriginalInputMutationAttempt::new(phase.descriptor().clone());
    f.db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&f.scope.agent_id],
    )?;
    let InputMutationExecution::Committed { receipt, .. } = commit(&f, &phase, &ledger)? else {
        unreachable!()
    };
    let before = rows(&f)?;
    let error =
        f.db.observe_task_stop_sparse_recovery(&attempt, &request(), &ledger)
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::OriginalAttemptNotFinished)
    ));
    attempt.finish_original_attempt();
    let baseline = ledger.report()?.observed_bytes;
    let evidence =
        f.db.observe_task_stop_sparse_recovery(&saved, &request(), &ledger)?
            .context("actual sparse ACK missing")?;
    assert!(evidence.matches_original(&attempt) && evidence.matches_original(&saved));
    assert!(!evidence.matches_original(&other));
    assert_eq!(evidence.receipt(), &receipt);
    assert_eq!(evidence.mode(), stops::TaskStopPhaseInstallMode::NoState);
    assert_eq!(
        evidence.state_presence(),
        stops::TaskStopSparseStatePresence::Absent
    );
    let identity = evidence
        .identity()
        .context("same original identity missing")?;
    assert_eq!(identity.agent_id, f.scope.agent_id);
    assert_eq!(identity.incarnation, f.scope.identity_incarnation);
    assert!(ledger.report()?.observed_bytes > baseline);
    assert_eq!(rows(&f)?, before);
    drop(evidence);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let absent = attention_phase(&source, "no-original-phase-ack", &ledger)?;
    let absent_attempt = OriginalInputMutationAttempt::new(absent.descriptor().clone());
    absent_attempt.finish_original_attempt();
    assert!(f
        .db
        .observe_task_stop_sparse_recovery(&absent_attempt, &request(), &ledger)?
        .is_none());
    assert_eq!(rows(&f)?, before, "absence must not mint sparse success");
    drop((absent, phase, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn sparse_stop_recovery_uses_fresh_retired_mode_and_revalidates_initial_whole_receipt() -> Result<()>
{
    use crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt;
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let source = original_stop(&f, &ledger)?;
    let phase = attention_phase(&source, "current-ack-then-retired", &ledger)?;
    let attempt = OriginalInputMutationAttempt::new(phase.descriptor().clone());
    let InputMutationExecution::Committed { receipt, .. } = commit(&f, &phase, &ledger)? else {
        unreachable!()
    };
    attempt.finish_original_attempt();
    assert_eq!(
        ack(&receipt).install_mode,
        stops::TaskStopPhaseInstallMode::Current
    );
    assert!(f
        .db
        .observe_task_stop_sparse_recovery(&attempt, &request(), &ledger)?
        .is_none());
    f.db.transaction(|tx| {
        // Deliberate conflicting restore: recovery must classify the actual
        // successor identity, without decoding its poisoned State.
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1", [&f.scope.agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poison-successor-State' WHERE agent_id=?1", [&f.scope.agent_id])?;
        Ok(())
    })?;
    let before = rows(&f)?;
    let evidence =
        f.db.observe_task_stop_sparse_recovery(&attempt, &request(), &ledger)?
            .context("retired original ACK missing")?;
    assert_eq!(evidence.mode(), stops::TaskStopPhaseInstallMode::Retired);
    assert_eq!(
        evidence.state_presence(),
        stops::TaskStopSparseStatePresence::NotReadRetired
    );
    assert_eq!(evidence.identity().unwrap().incarnation, 2);
    assert_eq!(
        evidence.receipt(),
        &receipt,
        "return original outcome, not current progress"
    );
    assert_eq!(rows(&f)?, before);
    drop(evidence);
    f.db.connection()?.execute(
        "UPDATE input_mutation_receipts SET result_digest=?1 WHERE command_nonce=?2",
        rusqlite::params!["0".repeat(64), source.record.prepared.command_nonce()],
    )?;
    let corrupt = rows(&f)?;
    assert!(
        f.db.observe_task_stop_sparse_recovery(&attempt, &request(), &ledger)
            .is_err(),
        "phase checksum alone cannot authenticate its initial Stop"
    );
    assert_eq!(rows(&f)?, corrupt);
    drop((phase, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn ready_runtime_stop_tag_cannot_install_sparse_first_ack_or_sparse_replay_as_ready() -> Result<()>
{
    for mode in ["no_state", "incomplete", "retired"] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let source = original_stop(&f, &ledger)?;
        let saved_state = f.state()?;
        let ready = attention_phase(&source, "runtime-restricted-phase", &ledger)?;
        let descriptor = ready.descriptor().clone();
        let ready = ready.for_ready_runtime_task_stop_phase()?;
        assert_eq!(
            ready.descriptor(),
            &descriptor,
            "routing restriction is not business intent"
        );
        f.db.transaction(|tx| {
            match mode {
                "no_state" => { tx.execute("DELETE FROM agent_states WHERE agent_id=?1", [&f.scope.agent_id])?; },
                "incomplete" => { tx.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.active_complete',json('false'),'$.unsettled_complete',json('false'),'$.stop.pending_complete',json('false')) WHERE agent_id=?1 AND identity_incarnation=1", [&f.scope.agent_id])?; },
                _ => {
                    tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1", [&f.scope.agent_id])?;
                    tx.execute("UPDATE agent_states SET payload_json='poison-successor-State' WHERE agent_id=?1", [&f.scope.agent_id])?;
                }
            }
            Ok(())
        })?;
        let before = rows(&f)?;
        let error = commit(&f, &ready, &ledger).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<stops::TaskStopPhaseError>(),
            Some(stops::TaskStopPhaseError::RequiresMetadataActor)
        ));
        assert_eq!(
            rows(&f)?,
            before,
            "restricted first ACK must have no writes"
        );
        let flexible = attention_phase(&source, "runtime-restricted-phase", &ledger)?;
        assert_eq!(flexible.descriptor(), &descriptor);
        let InputMutationExecution::Committed { receipt, .. } = commit(&f, &flexible, &ledger)?
        else {
            unreachable!()
        };
        assert_ne!(
            ack(&receipt).install_mode,
            stops::TaskStopPhaseInstallMode::Current
        );
        if mode != "retired" {
            f.db.transaction(|tx| {
                upsert_agent_state_tx(tx, &saved_state)?;
                tasks::prepare_task_domain_snapshot_with_budget_tx(
                    tx,
                    &f.scope,
                    &request(),
                    &ledger,
                )?
                .require_current()?;
                assert_eq!(
                    stops::observe_task_stop_phase_mode_tx(tx, &f.scope)?,
                    stops::TaskStopPhaseInstallMode::Current
                );
                Ok(())
            })?;
        }
        let after = rows(&f)?;
        let error = commit(&f, &ready, &ledger).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<stops::TaskStopPhaseError>(),
                Some(stops::TaskStopPhaseError::RequiresMetadataActor)
            ),
            "original sparse outcome cannot be republished as Ready"
        );
        assert_eq!(rows(&f)?, after);
        let InputMutationExecution::Replayed {
            receipt: original, ..
        } = commit(&f, &flexible, &ledger)?
        else {
            unreachable!()
        };
        assert_eq!(original, receipt);
        drop((ready, flexible, source));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn sparse_stop_identity_material_is_admitted_before_malformed_decode_and_releases_guards(
) -> Result<()> {
    use crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt;
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let source = original_stop(&f, &ledger)?;
    let phase = attention_phase(&source, "identity-budget-phase", &ledger)?;
    let attempt = OriginalInputMutationAttempt::new(phase.descriptor().clone());
    f.db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&f.scope.agent_id],
    )?;
    let InputMutationExecution::Committed { .. } = commit(&f, &phase, &ledger)? else {
        unreachable!()
    };
    attempt.finish_original_attempt();
    f.db.connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params!["malformed".repeat(256 * 1024), &f.scope.agent_id],
    )?;
    let mut limited = request();
    limited.byte_limit = 512 * 1024;
    let before = rows(&f)?;
    let baseline = ledger.report()?.observed_bytes;
    let error =
        f.db.observe_task_stop_sparse_recovery(&attempt, &limited, &ledger)
            .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "Stop sparse identity material"
            ))
        ),
        "budget must precede malformed JSON: {error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(rows(&f)?, before);
    let next = attention_phase(&source, "identity-budget-new-phase", &ledger)?;
    let before_next = ledger.report()?.observed_bytes;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &next,
                &work(),
                &ledger,
                &limited,
            )
            .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "Stop phase material"
            ))
        ),
        "scalar classifier must run after length admission: {error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before_next);
    assert_eq!(rows(&f)?, before);
    drop((next, phase, source));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[path = "task_stop_current_source_tests.rs"]
mod current_source;
