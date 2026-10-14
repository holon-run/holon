use super::prepared_task_stop_tests::{request, rows};
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory},
        resident_task::{TaskFamily, TaskKey},
    },
    runtime_db::{task_snapshot as tasks, task_stop as stops},
    types::{
        AdmissionContext, MessageDeliverySurface, TaskKind, TaskRecord, TaskStatus, WakeSource,
    },
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
fn commit(
    f: &Fixture,
    p: &PreparedInputTransition,
    l: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(p, &work(), l, &request())
}
fn task_command(f: &Fixture, task: TaskRecord) -> TaskTransitionCommand {
    TaskTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        task,
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: Vec::new(),
        expected_wait_conditions: Vec::new(),
        wait_conditions: Vec::new(),
        agent_state: None,
        message_evidence: Vec::new(),
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: true,
        commit_on_idempotent: false,
        fault: None,
    }
}
struct Source {
    record: Arc<stops::TaskStopRecord>,
    _view: Arc<BudgetViewGuard>,
}
fn load(f: &Fixture, seq: u64, l: &InputEvidenceBudgetLedger) -> Result<Source> {
    f.db.transaction(|tx| {
        let source =
            stops::load_task_stop_by_sequence_tx(tx, &f.scope, seq, request().byte_limit, l)?;
        let record = source.records[0].clone();
        let view = l.register_task_stop_view(
            &f.scope,
            InputMemoryCategory::Candidate,
            std::iter::once(&record),
        )?;
        Ok(Source {
            record,
            _view: view,
        })
    })
}
fn source(
    f: &Fixture,
    l: &InputEvidenceBudgetLedger,
    rejoin: bool,
    with_work: bool,
) -> Result<Source> {
    source_with_signal(f, l, rejoin, with_work, stops::TaskStopSignal::NoHandle)
}
fn source_with_signal(
    f: &Fixture,
    l: &InputEvidenceBudgetLedger,
    rejoin: bool,
    with_work: bool,
    signal: stops::TaskStopSignal,
) -> Result<Source> {
    if with_work {
        let p = PreparedInputTransition::work(
            f.scope.clone(),
            "result-owning-work".into(),
            f.at,
            original_work_command(f, None),
            Default::default(),
            false,
        )?;
        commit(f, &p, l)?;
    }
    let task=TaskRecord{id:"frozen-stop-result-task".into(),agent_id:f.scope.agent_id.clone(),kind:TaskKind::AgentMessageWait,status:TaskStatus::Running,
        created_at:f.at,updated_at:f.at,parent_message_id:None,work_item_id:with_work.then(||"original-work".into()),summary:Some("original admitted Task".into()),
        detail:rejoin.then(||serde_json::json!({"rejoin_obligation_id":"frozen-stop-result-task","rejoin_generation":1,"parent_turn_id":"original-task-parent"})),recovery:None};
    let p = PreparedInputTransition::task(
        f.scope.clone(),
        "result-task-first-admission".into(),
        f.at,
        PreparedTaskTransition::new(
            task_command(f, task.clone()),
            Default::default(),
            false,
            None,
            None,
        )?,
    )?;
    commit(f, &p, l)?;
    if with_work {
        let mut wait = namespace_wait(&f.scope.agent_id, f.at);
        wait.id = "result-existing-wait".into();
        wait.work_item_id = Some("original-work".into());
        wait.kind = crate::types::WaitConditionKind::Task;
        wait.wake_sources = vec![WakeSource::TaskResult {
            task_id: task.id.clone(),
        }];
        let mut command = original_wait_command(f, None)?;
        command.wait_conditions = vec![wait];
        let p = PreparedInputTransition::wait(
            f.scope.clone(),
            "result-existing-wait-admission".into(),
            f.at,
            command,
            Default::default(),
            None,
        )?;
        commit(f, &p, l)?;
    }
    let locator = f.db.transaction(|tx| {
        let current =
            tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &f.scope, &request(), l)?
                .require_current()?;
        let capture = current.resident.capture(&request().capture)?;
        Ok(capture
            .lookup(&TaskKey::new(TaskFamily::Task, &task.id))?
            .context("original Task missing")?
            .locator
            .clone())
    })?;
    let mut cancelled = task.clone();
    cancelled.status = TaskStatus::Cancelled;
    cancelled.updated_at = f.at + chrono::Duration::seconds(1);
    let mut message = MessageEnvelope::new(
        &f.scope.agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "task cancelled by operator".into(),
        },
    );
    message.id = "frozen-stop-feedback".into();
    message.created_at = f.at + chrono::Duration::seconds(2);
    message.task_id = Some(task.id.clone());
    message.work_item_id = task.work_item_id.clone();
    message.delivery_surface = Some(MessageDeliverySurface::TaskRejoin);
    message.admission_context = Some(AdmissionContext::RuntimeOwned);
    message.metadata = Some(
        serde_json::json!({"task_id":task.id,"task_kind":task.kind,"task_status":"cancelled","task_summary":cancelled.summary,"task_detail":cancelled.detail,"task_recovery":cancelled.recovery}),
    );
    let intent = stops::PreparedTaskStop {
        task: locator,
        kind: task.kind,
        created_at: task.created_at,
        rejoin: task.rejoin_fence().ok(),
        owner_epoch: 1,
        signal,
        lease_id: (signal != stops::TaskStopSignal::NoHandle)
            .then(|| "original-runtime-lease".into()),
        child: None,
        child_unavailable: None,
        worktree: None,
        worktree_unavailable: None,
        result: Some(stops::TaskStopResultPlan {
            phase_nonce: "original-frozen-result-phase".into(),
            message,
        }),
    };
    let p = PreparedInputTransition::task(
        f.scope.clone(),
        "original-stop-before-feedback".into(),
        cancelled.updated_at,
        PreparedTaskTransition::new(
            task_command(f, cancelled),
            Default::default(),
            false,
            None,
            None,
        )?
        .with_stop_intent(intent)?,
    )?;
    let InputMutationExecution::Committed { receipt, .. } = commit(f, &p, l)? else {
        unreachable!()
    };
    let InputMutationResult::Task {
        stop: Some(binding),
        ..
    } = &receipt.outcome.result
    else {
        unreachable!()
    };
    load(f, binding.effect_seq, l)
}
fn ack(r: &InputMutationReceipt) -> &stops::TaskStopPhaseAck {
    let InputMutationResult::TaskStopPhase { ack } = &r.outcome.result else {
        panic!("not original Result phase")
    };
    ack
}
fn deleting(f: &Fixture, parent: bool) -> Result<()> {
    f.db.transaction(|tx| {
        let raw: String = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
            [&f.scope.agent_id],
            |r| r.get(0),
        )?;
        let mut id = crate::runtime_db::repositories::decode_agent_identity_payload(&raw)?;
        let revision = id.revision;
        id.status = crate::types::AgentRegistryStatus::Deleting;
        id.revision += 1;
        id.updated_at += chrono::Duration::nanoseconds(1);
        upsert_agent_identity_tx(tx, &id)?;
        if parent {
            crate::runtime_db::evidence::insert_agent_deletion_job_tx(
                tx,
                &crate::types::AgentDeletionJob {
                    deletion_id: "parent-cleanup-before-feedback".into(),
                    agent_id: f.scope.agent_id.clone(),
                    mode: crate::types::AgentDeletionMode::ParentCleanup,
                    status: crate::types::AgentDeletionStatus::Pending,
                    phase: crate::types::AgentDeletionPhase::Fence,
                    requested_by: "original-parent".into(),
                    expected_identity_revision: revision,
                    cascade_private_children: false,
                    attempts: 0,
                    last_error: None,
                    next_attempt_at: None,
                    created_at: id.updated_at,
                    updated_at: id.updated_at,
                    completed_at: None,
                },
            )?;
        }
        Ok(())
    })
}

#[test]
fn frozen_stop_result_delivers_without_physical_join_and_owns_work_wait_input_ack() -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, true, true)?;
    assert_eq!(
        source.record.progress.physical,
        stops::TaskStopPhysicalState::MissingProof
    );
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?
        .for_ready_runtime_task_stop_phase()?;
    assert_eq!(
        p.pending_messages()?[0],
        &source.record.intent.result.as_ref().unwrap().message
    );
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    let result = ack(&receipt).result.as_ref().unwrap();
    let stops::TaskStopResultOutcome::Delivered {
        message_id,
        message_seq,
        result_identity: Some(id),
    } = &result.outcome
    else {
        panic!("actual delivery required")
    };
    assert_eq!(message_id, "frozen-stop-feedback");
    assert!(*message_seq > 0 && !id.is_empty());
    assert_eq!(
        ack(&receipt).progress.physical,
        stops::TaskStopPhysicalState::MissingProof
    );
    assert_eq!(
        ack(&receipt).progress.result,
        stops::TaskStopResultState::Completed
    );
    assert_eq!(
        value.effects.messages[0].work_item_id.as_deref(),
        Some("original-work")
    );
    assert_eq!(
        value.effects.wait_condition_writes.wait_ids,
        BTreeSet::from(["result-existing-wait".into()])
    );
    assert!(value.effects.work_delta.is_some() && value.effects.task_delta.is_some());
    assert!(!value.effects.audit_events.is_empty());
    assert_eq!(f.state()?.pending, 1);
    assert_eq!(f.state()?.total_message_count, 1);
    let durable = rows(&f)?;
    let after = load(&f, source.record.effect_seq, &l)?;
    let retry = PreparedInputTransition::task_stop_result(&after.record, &request(), &l)?
        .for_ready_runtime_task_stop_phase()?;
    assert_eq!(p.descriptor(), retry.descriptor());
    let InputMutationExecution::Replayed {
        receipt: original, ..
    } = commit(&f, &retry, &l)?
    else {
        panic!("original receipt")
    };
    assert_eq!(original, receipt);
    assert_eq!(rows(&f)?, durable);
    Ok(())
}

#[test]
fn frozen_stop_result_without_rejoin_delivers_message_and_queue_without_invented_settlement(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, false, false)?;
    assert!(source.record.intent.rejoin.is_none());
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert!(matches!(
        ack(&receipt).result.as_ref().unwrap().outcome,
        stops::TaskStopResultOutcome::Delivered {
            result_identity: None,
            ..
        }
    ));
    assert_eq!(value.effects.queued_messages.len(), 1);
    assert_eq!(f.state()?.pending, 1);
    let (results,attempts,turns):(u64,u64,u64)=f.db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM task_result_settlements),(SELECT COUNT(*) FROM execution_protocol_attempts),(SELECT COUNT(*) FROM turn_records)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!((results, attempts, turns), (0, 0, 0));
    Ok(())
}

#[test]
fn frozen_stop_result_final_receipt_failure_rolls_back_every_business_and_progress_write(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, true, true)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    f.db.connection()?.execute_batch("CREATE TRIGGER fail_frozen_result_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='original-frozen-result-phase' BEGIN SELECT RAISE(ABORT,'frozen-result-receipt-fault'); END;")?;
    let before = rows(&f)?;
    let error = commit(&f, &p, &l).unwrap_err();
    assert!(format!("{error:#}").contains("frozen-result-receipt-fault"));
    assert_eq!(rows(&f)?, before);
    f.db.connection()?
        .execute_batch("DROP TRIGGER fail_frozen_result_receipt;")?;
    let InputMutationExecution::Committed { receipt, .. } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert_eq!(
        ack(&receipt).progress.result,
        stops::TaskStopResultState::Completed
    );
    Ok(())
}

#[test]
fn frozen_stop_result_sparse_delivery_keeps_task_unprepared_but_returns_actual_state_queue_work_input(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, true, true)?;
    f.db.connection()?.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.active_complete',json('false'),'$.unsettled_complete',json('false'),'$.stop.pending_complete',json('false')) WHERE agent_id=?1",[&f.scope.agent_id])?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let restrictive = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?
        .for_ready_runtime_task_stop_phase()?;
    let before = rows(&f)?;
    assert!(commit(&f, &restrictive, &l)
        .unwrap_err()
        .downcast_ref::<stops::TaskStopPhaseError>()
        .is_some());
    assert_eq!(rows(&f)?, before);
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert_eq!(
        ack(&receipt).install_mode,
        stops::TaskStopPhaseInstallMode::Incomplete
    );
    assert!(matches!(
        ack(&receipt).result.as_ref().unwrap().outcome,
        stops::TaskStopResultOutcome::Delivered { .. }
    ));
    assert!(value.effects.task_delta.is_none());
    assert!(ack(&receipt).invalidation.is_some());
    assert!(value.effects.agent_state.is_some() && value.effects.work_delta.is_some());
    assert_eq!(value.effects.messages.len(), 1);
    assert_eq!(f.state()?.pending, 1);
    assert!(commit(&f, &restrictive, &l)
        .unwrap_err()
        .downcast_ref::<stops::TaskStopPhaseError>()
        .is_some());
    Ok(())
}

#[test]
fn frozen_stop_result_parent_cleanup_fence_remains_atomic_with_or_without_rejoin() -> Result<()> {
    for rejoin in [true, false] {
        let f = Fixture::new()?;
        let l = ledger()?;
        let source = source(&f, &l, rejoin, false)?;
        deleting(&f, true)?;
        let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
        let before = rows(&f)?;
        assert!(commit(&f, &p, &l).is_err());
        assert_eq!(rows(&f)?, before);
    }
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, false, false)?;
    deleting(&f, false)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let InputMutationExecution::Committed { receipt, .. } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert!(matches!(
        ack(&receipt).result.as_ref().unwrap().outcome,
        stops::TaskStopResultOutcome::Delivered {
            result_identity: None,
            ..
        }
    ));
    Ok(())
}

#[test]
fn frozen_stop_result_no_state_and_retired_poisoned_successor_only_record_unavailable() -> Result<()>
{
    for retired in [false, true] {
        let f = Fixture::new()?;
        let l = ledger()?;
        let source = source(&f, &l, true, false)?;
        if retired {
            // Explicit conflicting successor restore: the old continuation must
            // not decode, install or overwrite this unrelated State body.
            f.db.connection()?.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&f.scope.agent_id])?;
            f.db.connection()?.execute(
                "UPDATE agent_states SET payload_json='poisoned-successor-state' WHERE agent_id=?1",
                [&f.scope.agent_id],
            )?;
        } else {
            f.db.connection()?.execute(
                "DELETE FROM agent_states WHERE agent_id=?1",
                [&f.scope.agent_id],
            )?;
        }
        let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
        let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
            unreachable!()
        };
        let expected = if retired {
            stops::TaskStopResultUnavailable::ScopeRetired
        } else {
            stops::TaskStopResultUnavailable::OwnerNoState
        };
        assert_eq!(
            ack(&receipt).result.as_ref().unwrap().outcome,
            stops::TaskStopResultOutcome::Unavailable { reason: expected }
        );
        assert!(
            value.effects.messages.is_empty()
                && value.effects.agent_state.is_none()
                && value.effects.task_delta.is_none()
        );
        let after = rows(&f)?;
        assert!(matches!(
            commit(&f, &p, &l)?,
            InputMutationExecution::Replayed { .. }
        ));
        assert_eq!(rows(&f)?, after);
        let count: u64 = f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE evidence_id='frozen-stop-feedback'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 0);
    }
    Ok(())
}

#[test]
fn frozen_stop_result_address_survives_independent_physical_attention() -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source_with_signal(&f, &l, true, false, stops::TaskStopSignal::Abort)?;
    let original = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let phase = PreparedInputTransition::task_stop_safe_progress(
        &source.record,
        "original-physical-attention".into(),
        f.at + chrono::Duration::seconds(5),
        stops::TaskStopProgressAction::PhysicalAttention {
            reason: "original handle unavailable; no exit proof".into(),
        },
        &request(),
        &l,
    )?;
    commit(&f, &phase, &l)?;
    let current = load(&f, source.record.effect_seq, &l)?;
    let delivered = PreparedInputTransition::task_stop_result(&current.record, &request(), &l)?;
    assert_eq!(original.descriptor(), delivered.descriptor());
    let InputMutationExecution::Committed { receipt, .. } = commit(&f, &delivered, &l)? else {
        unreachable!()
    };
    assert_eq!(
        ack(&receipt).progress.physical,
        stops::TaskStopPhysicalState::NeedsAttention
    );
    assert_eq!(
        ack(&receipt).progress.result,
        stops::TaskStopResultState::Completed
    );
    assert_eq!(ack(&receipt).updated_at, current.record.updated_at);
    assert!(matches!(
        ack(&receipt).result.as_ref().unwrap().outcome,
        stops::TaskStopResultOutcome::Delivered { .. }
    ));
    Ok(())
}

#[path = "task_stop_partial_recovery_tests.rs"]
mod partial_recovery;

// Reuse the real typed original Task/Wait/Stop fixture in Host actor tests.
impl RuntimeDb {
    pub(crate) fn original_stop_result_host_fixture(
        &self,
        scope: &InputEvidenceScope,
        l: &InputEvidenceBudgetLedger,
        with_work: bool,
    ) -> Result<(Arc<stops::TaskStopRecord>, Arc<BudgetViewGuard>)> {
        let f = Fixture {
            _directory: tempfile::tempdir()?,
            db: self.clone(),
            scope: scope.clone(),
            at: Utc::now() - chrono::Duration::seconds(10),
        };
        let source = source(&f, l, false, with_work)?;
        Ok((source.record, source._view))
    }
}

impl RuntimeDb {
    pub(crate) fn original_stop_abort_host_fixture(
        &self,
        scope: &InputEvidenceScope,
        l: &InputEvidenceBudgetLedger,
    ) -> Result<(Arc<stops::TaskStopRecord>, Arc<BudgetViewGuard>)> {
        let f = Fixture {
            _directory: tempfile::tempdir()?,
            db: self.clone(),
            scope: scope.clone(),
            at: Utc::now() - chrono::Duration::seconds(10),
        };
        let source = source_with_signal(&f, l, false, false, stops::TaskStopSignal::Abort)?;
        Ok((source.record, source._view))
    }
}
