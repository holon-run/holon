//! Original standalone Result phases, including successful empty outcomes.
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
    },
    runtime_db::{
        input_mutation_receipts::InputMutationResult, task_result_settlement as results,
        task_snapshot as tasks,
    },
    types::{TaskKind, TaskStatus},
};
fn request() -> tasks::TaskDomainLoadRequest {
    tasks::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn freeze_result(
    f: &Fixture,
    nonce: &str,
    operation: TaskResultOperation,
) -> Result<PreparedInputTransition> {
    PreparedInputTransition::task_result(
        f.scope.clone(),
        nonce.into(),
        f.at,
        PreparedTaskResultTransition::new(f.scope.agent_id.clone(), operation)?,
    )
}
fn execute(
    f: &Fixture,
    p: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 4 * 1024 * 1024,
            },
            ledger,
            request,
        )
}
fn outcome(
    receipt: &crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
) -> &results::TaskResultWriteOutcome {
    let InputMutationResult::TaskResult { outcome } = &receipt.outcome.result else {
        panic!("wrong result family")
    };
    outcome
}
fn terminal_task(
    f: &Fixture,
    id: &str,
    source: &MessageEnvelope,
    work: Option<String>,
) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: f.scope.agent_id.clone(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: f.at,
        updated_at: f.at,
        parent_message_id: Some(source.id.clone()),
        work_item_id: work,
        summary: Some("terminal original producer".into()),
        detail: Some(
            serde_json::json!({"rejoin_obligation_id":id,"rejoin_generation":1,"parent_turn_id":"original-parent-turn"}),
        ),
        recovery: None,
    }
}
fn qualify_task(f: &Fixture, task: TaskRecord, ledger: &InputEvidenceBudgetLedger) -> Result<()> {
    let p = PreparedInputTransition::task(
        f.scope.clone(),
        format!("{}:original-task", task.id),
        f.at,
        PreparedTaskTransition::new(
            TaskTransitionCommand {
                agent_id: f.scope.agent_id.clone(),
                expected_owner_incarnation: f.scope.identity_incarnation,
                task,
                agent_deletion: None,
                task_result_settlement: None,
                queue_entry: None,
                work_items: vec![],
                expected_wait_conditions: vec![],
                wait_conditions: vec![],
                agent_state: None,
                message_evidence: vec![],
                audit_events: vec![],
                index_changes: vec![],
                notify_scheduler: true,
                commit_on_idempotent: false,
                fault: None,
            },
            Default::default(),
            true,
            None,
            None,
        )?,
    )?;
    drop(execute(f, &p, &request(), ledger)?);
    Ok(())
}
fn original_source(
    f: &Fixture,
    id: &str,
    task_id: &str,
    work: Option<String>,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<MessageEnvelope> {
    let mut q = f.queue()?;
    let message = &mut q.command.message_evidence[0];
    message.id = id.into();
    message.kind = MessageKind::TaskResult;
    message.task_id = Some(task_id.into());
    message.origin = MessageOrigin::Task {
        task_id: task_id.into(),
    };
    message.authority_class = AuthorityClass::RuntimeInstruction;
    message.work_item_id = work;
    let QueueMutation::Upsert(row) = &mut q.command.mutation else {
        unreachable!()
    };
    row.message_id = id.into();
    let p = PreparedInputTransition::queue(f.scope.clone(), format!("{id}:source"), f.at, q)?;
    let InputMutationExecution::Committed { value, .. } = execute(f, &p, &request(), ledger)?
    else {
        panic!("source ACK")
    };
    Ok(value.effects.messages[0].clone())
}
fn pending(
    f: &Fixture,
    ledger: &InputEvidenceBudgetLedger,
    work: bool,
) -> Result<(MessageEnvelope, String)> {
    let owner = if work {
        let command = original_work_command(f, None);
        let p = PreparedInputTransition::work(
            f.scope.clone(),
            "result:original-work".into(),
            f.at,
            command,
            Default::default(),
            false,
        )?;
        drop(execute(f, &p, &request(), ledger)?);
        Some("original-work".into())
    } else {
        None
    };
    let source = original_source(f, "result:source", "result:task", owner.clone(), ledger)?;
    qualify_task(f, terminal_task(f, "result:task", &source, owner), ledger)?;
    let p = freeze_result(
        f,
        "result:ensure",
        TaskResultOperation::EnsurePending {
            task_id: "result:task".into(),
            message: Box::new(source.clone()),
            at: f.at,
        },
    )?;
    let InputMutationExecution::Committed { value, receipt } = execute(f, &p, &request(), ledger)?
    else {
        panic!("first Result ACK")
    };
    let results::TaskResultWriteOutcome::Record(Some(record)) = outcome(&receipt) else {
        panic!("original pending")
    };
    let id = record.result_identity.clone();
    assert_eq!(
        value.effects.task_write_keys,
        BTreeSet::from([TaskKey::new(TaskFamily::ResultSettlement, &id)])
    );
    drop(value);
    Ok((source, id))
}

#[test]
fn result_successful_empty_returns_have_original_receipts_before_later_rows() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let operations = vec![
        TaskResultOperation::EnsurePending {
            task_id: "absent-task".into(),
            message: Box::new(f.queue()?.command.message_evidence.remove(0)),
            at: f.at,
        },
        TaskResultOperation::AdmitMessage {
            projection: None,
            message_id: "result:source".into(),
            activation_id: "empty-activation".into(),
            at: f.at,
        },
        TaskResultOperation::AdmitUnsettled {
            work_item_id: None,
            activation_id: "empty-activation".into(),
            at: f.at,
        },
        TaskResultOperation::AdmitReply {
            work_item_id: None,
            message_id: "result:source".into(),
            activation_id: "empty-activation".into(),
            at: f.at,
        },
        TaskResultOperation::SettleOwnerUnavailable {
            message_id: "result:source".into(),
            disposition: results::TaskResultSettlementDisposition::OwnerMissing,
            at: f.at,
        },
        TaskResultOperation::Defer {
            message_id: "result:source".into(),
            reason: "empty".into(),
            at: f.at,
            deadline: f.at + chrono::Duration::seconds(1),
        },
        TaskResultOperation::Wake {
            work_item_id: "original-work".into(),
            at: f.at,
        },
        TaskResultOperation::MarkRecoveryDiagnostic {
            result_identity: "absent-result".into(),
            epoch: 0,
            now: f.at,
        },
    ];
    let mut originals = vec![];
    for (i, operation) in operations.into_iter().enumerate() {
        let p = freeze_result(&f, &format!("result:empty:{i}"), operation)?;
        let InputMutationExecution::Committed { value, receipt } =
            execute(&f, &p, &request(), &ledger)?
        else {
            panic!("empty ACK")
        };
        assert!(!value.applied);
        assert!(value.effects.task_write_keys.is_empty());
        match (i, outcome(&receipt)) {
            (0 | 1 | 5, results::TaskResultWriteOutcome::Record(None))
            | (2, results::TaskResultWriteOutcome::Records(_))
            | (3 | 7, results::TaskResultWriteOutcome::Unit)
            | (4, results::TaskResultWriteOutcome::Changed(false))
            | (6, results::TaskResultWriteOutcome::Count(0)) => {}
            (_, other) => panic!("wrong original empty {i}: {other:?}"),
        }
        if let results::TaskResultWriteOutcome::Records(records) = outcome(&receipt) {
            assert!(records.is_empty());
        }
        originals.push((p, receipt));
        drop(value);
    }
    let (_, id) = pending(&f, &ledger, true)?;
    let before =
        f.db.task_result_settlements()
            .latest_for_message("result:source")?;
    let before_receipts = f.receipt_count()?;
    for (p, expected) in originals {
        let InputMutationExecution::Replayed {
            receipt,
            _material_budget_views,
        } = execute(&f, &p, &request(), &ledger)?
        else {
            panic!("original empty replays")
        };
        assert_eq!(receipt, expected);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(receipt);
        drop(_material_budget_views);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert_eq!(f.receipt_count()?, before_receipts);
    assert_eq!(
        f.db.task_result_settlements()
            .latest_for_message("result:source")?,
        before
    );
    assert!(f.db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE record_id=?1)",
        [id],
        |r| r.get::<_, bool>(0)
    )?);
    Ok(())
}

#[test]
fn result_admit_defer_wake_and_settle_once_finalizer_preserves_original_return() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let (source, id) = pending(&f, &ledger, true)?;
    let operations = vec![
        TaskResultOperation::AdmitMessage {
            projection: None,
            message_id: source.id.clone(),
            activation_id: "original-activation".into(),
            at: f.at + chrono::Duration::seconds(1),
        },
        TaskResultOperation::Defer {
            message_id: source.id.clone(),
            reason: "original-wait".into(),
            at: f.at + chrono::Duration::seconds(2),
            deadline: f.at + chrono::Duration::seconds(20),
        },
        TaskResultOperation::Wake {
            work_item_id: "original-work".into(),
            at: f.at + chrono::Duration::seconds(3),
        },
        TaskResultOperation::SettleOwnerUnavailable {
            message_id: source.id.clone(),
            disposition: results::TaskResultSettlementDisposition::OwnerMissing,
            at: f.at + chrono::Duration::seconds(4),
        },
    ];
    let mut original_defer = None;
    for (i, operation) in operations.into_iter().enumerate() {
        let p = freeze_result(&f, &format!("result:phase:{i}"), operation)?;
        let InputMutationExecution::Committed { value, receipt } =
            execute(&f, &p, &request(), &ledger)?
        else {
            panic!("phase ACK")
        };
        assert!(value.applied);
        assert_eq!(
            value.effects.task_write_keys,
            BTreeSet::from([TaskKey::new(TaskFamily::ResultSettlement, &id)])
        );
        let delta = value.effects.task_delta.as_ref().unwrap();
        assert_eq!(delta.revision, delta.base_revision + 1);
        assert!(value.effects.work_delta.is_none());
        assert!(value.effects.context_episodes.is_empty());
        match (i, outcome(&receipt)) {
            (0, results::TaskResultWriteOutcome::Record(Some(r))) => {
                assert_eq!(r.activation_id.as_deref(), Some("original-activation"))
            }
            (1, results::TaskResultWriteOutcome::Record(Some(r))) => {
                assert_eq!(r.deferred_reason.as_deref(), Some("original-wait"))
            }
            (2, results::TaskResultWriteOutcome::Count(1))
            | (3, results::TaskResultWriteOutcome::Changed(true)) => {}
            (_, other) => panic!("wrong original phase {i}: {other:?}"),
        }
        if i == 1 {
            original_defer = Some((p, receipt));
        }
        assert!(ledger.report()?.observed_bytes > 0);
        drop(value);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let (p, expected) = original_defer.unwrap();
    let latest =
        f.db.task_result_settlements()
            .latest_for_message(&source.id)?;
    let InputMutationExecution::Replayed {
        receipt,
        _material_budget_views,
    } = execute(&f, &p, &request(), &ledger)?
    else {
        panic!("defer replay")
    };
    assert_eq!(receipt, expected);
    assert_eq!(
        f.db.task_result_settlements()
            .latest_for_message(&source.id)?,
        latest
    );
    drop(receipt);
    drop(_material_budget_views);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn result_finalizer_and_receipt_faults_rollback_actual_row_and_allow_original_retry() -> Result<()>
{
    for receipt_fault in [false, true] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let (source, _) = pending(&f, &ledger, false)?;
        let before =
            f.db.task_result_settlements()
                .latest_for_message(&source.id)?;
        let count = f.receipt_count()?;
        let p = freeze_result(
            &f,
            "result:fault",
            TaskResultOperation::Defer {
                message_id: source.id.clone(),
                reason: "faulted".into(),
                at: f.at + chrono::Duration::seconds(1),
                deadline: f.at + chrono::Duration::seconds(5),
            },
        )?;
        let sql = if receipt_fault {
            "CREATE TRIGGER result_fault BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'result-receipt-fault'); END;"
        } else {
            "CREATE TRIGGER result_fault BEFORE UPDATE ON task_domain_records WHEN OLD.family='result_settlement' BEGIN SELECT RAISE(ABORT,'result-alias-fault'); END;"
        };
        f.db.connection()?.execute_batch(sql)?;
        let error = execute(&f, &p, &request(), &ledger).unwrap_err();
        assert!(
            format!("{error:#}").contains(if receipt_fault {
                "result-receipt-fault"
            } else {
                "result-alias-fault"
            }),
            "{error:#}"
        );
        assert_eq!(
            f.db.task_result_settlements()
                .latest_for_message(&source.id)?,
            before
        );
        assert_eq!(f.receipt_count()?, count);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        f.db.connection()?
            .execute_batch("DROP TRIGGER result_fault")?;
        assert!(matches!(
            execute(&f, &p, &request(), &ledger)?,
            InputMutationExecution::Committed { .. }
        ));
    }
    Ok(())
}

#[test]
fn result_receipt_budget_precedes_corrupt_prior_outcome_decode() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let p = freeze_result(
        &f,
        "result:oversize-receipt",
        TaskResultOperation::Wake {
            work_item_id: "absent".into(),
            at: f.at,
        },
    )?;
    drop(execute(&f, &p, &request(), &ledger)?);
    f.db.connection()?.execute(
        "UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",
        params![
            "not-json".repeat(128 * 1024),
            p.descriptor().command_nonce()
        ],
    )?;
    let mut tiny = request();
    tiny.byte_limit = 1024 * 1024;
    let error = execute(&f, &p, &tiny, &ledger).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "original Result receipt"
            ))
        ),
        "{error:#}"
    );
    assert_eq!(f.receipt_count()?, 1);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let attempt = crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
        p.descriptor().clone(),
    );
    attempt.finish_original_attempt();
    let mut connection = f.db.connection()?;
    let error = crate::runtime_db::input_mutation_receipts::with_fresh_input_mutation_resolution_and_preflight(
        &mut connection, &attempt,
        |tx| crate::runtime_db::transitions::task_result_receipt_budget_tx(tx,p.descriptor(),&f.scope,&tiny,&ledger),
        |_,_| -> Result<()> { panic!("oversize receipt must fail before lookup/callback") },
    ).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "original Result receipt"
            ))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn ensure_pending_cannot_grant_raw_task_or_raw_source_qualification() -> Result<()> {
    for raw_task in [false, true] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let source = if raw_task {
            original_source(&f, "raw:source", "raw:task", None, &ledger)?
        } else {
            let mut q = f.queue()?;
            let m = &mut q.command.message_evidence[0];
            m.kind = MessageKind::TaskResult;
            m.task_id = Some("raw:task".into());
            m.origin = MessageOrigin::Task {
                task_id: "raw:task".into(),
            };
            m.authority_class = AuthorityClass::RuntimeInstruction;
            let mut state = f.state()?;
            state.total_message_count = 1;
            f.db.transaction(|tx| {
                let (actual, _) = crate::runtime_db::evidence::append_message_tx(tx, m)?;
                upsert_agent_state_tx(tx, &state)?;
                Ok(actual)
            })?
        };
        let task = terminal_task(&f, "raw:task", &source, None);
        if raw_task {
            f.db.transaction(|tx| {
                crate::runtime_db::repositories::upsert_task_tx(tx, &task).map(|_| ())
            })?;
        } else {
            qualify_task(&f, task, &ledger)?;
        }
        let count = f.receipt_count()?;
        let p = freeze_result(
            &f,
            "raw:ensure",
            TaskResultOperation::EnsurePending {
                task_id: "raw:task".into(),
                message: Box::new(source),
                at: f.at,
            },
        )?;
        let error = execute(&f, &p, &request(), &ledger).unwrap_err();
        if raw_task {
            assert!(
                matches!(
                    error.downcast_ref::<tasks::TaskSnapshotError>(),
                    Some(tasks::TaskSnapshotError::NeedsPreparation(_))
                ),
                "{error:#}"
            );
        } else {
            assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(_))),"{error:#}");
        }
        assert_eq!(
            f.db.connection()?.query_row(
                "SELECT COUNT(*) FROM task_result_settlements",
                [],
                |r| r.get::<_, usize>(0)
            )?,
            0
        );
        assert_eq!(f.receipt_count()?, count);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn prepared_result_projection_validates_original_before_writing_a_receipt() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let (source, _) = pending(&f, &ledger, false)?;
    let projection = crate::runtime::input_consumption::projection::project_item(
        &source,
        usize::MAX,
        usize::MAX,
    )?
    .expect("unbounded source fits");
    let mut invalid = projection.clone();
    invalid.rendered.push_str("forged result");
    let before = f
        .db
        .task_result_settlements()
        .latest_for_message(&source.id)?;
    let count = f.receipt_count()?;
    let command = |projection| TaskResultOperation::AdmitMessage {
        message_id: source.id.clone(),
        activation_id: "projection:activation".into(),
        projection: Some(projection),
        at: f.at + chrono::Duration::seconds(1),
    };
    let invalid = freeze_result(&f, "projection:invalid", command(invalid))?;
    let error = execute(&f, &invalid, &request(), &ledger).unwrap_err();
    assert!(format!("{error:#}").contains("projection"), "{error:#}");
    assert_eq!(
        f.db
            .task_result_settlements()
            .latest_for_message(&source.id)?,
        before
    );
    assert_eq!(f.receipt_count()?, count);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let valid = freeze_result(&f, "projection:valid", command(projection.clone()))?;
    let InputMutationExecution::Committed { value, receipt } =
        execute(&f, &valid, &request(), &ledger)?
    else {
        panic!("valid projection must commit")
    };
    let results::TaskResultWriteOutcome::Record(Some(record)) = outcome(&receipt) else {
        panic!("admitted Result outcome")
    };
    assert_eq!(record.source_projection.as_ref(), Some(&projection));
    assert_eq!(
        f.db
            .task_result_settlements()
            .latest_for_message(&source.id)?
            .as_ref(),
        Some(record)
    );
    drop(value);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_admit_token_shares_remaining_material_allowance_and_rolls_back_before_business(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let (source, _) = pending(&f, &ledger, false)?;
    let deferred = freeze_result(
        &f,
        "remaining:defer",
        TaskResultOperation::Defer {
            message_id: source.id.clone(),
            reason: "x".repeat(96 * 1024),
            at: f.at + chrono::Duration::seconds(1),
            deadline: f.at + chrono::Duration::seconds(20),
        },
    )?;
    drop(execute(&f, &deferred, &request(), &ledger)?);
    let original = freeze_result(
        &f,
        "remaining:admit",
        TaskResultOperation::AdmitMessage {
            projection: None,
            message_id: source.id.clone(),
            activation_id: "remaining:activation".into(),
            at: f.at + chrono::Duration::seconds(2),
        },
    )?;
    let mut bounded = request();
    bounded.byte_limit = 3 * 1024 * 1024;
    // The token fits a full request in isolation. It must not receive that full
    // allowance again after original selected raw/typed material has reserved it.
    f.db.transaction(|tx| {
        let selection = results::select_admit_message_tx(
            tx,
            &f.scope.agent_id,
            &source.id,
            "remaining:activation",
            f.at + chrono::Duration::seconds(2),
            results::TaskResultWriteLimits::default(),
        )?;
        let estimate = selection.material_estimate();
        assert!(estimate.payload_bytes + estimate.metadata_estimate_bytes < bounded.byte_limit);
        let guard = ledger.register_owned_bytes(
            &f.scope,
            crate::runtime::input_evidence_budget::InputMemoryCategory::Candidate,
            crate::runtime::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: u64::try_from(estimate.payload_bytes)?,
                metadata_estimate_bytes: u64::try_from(estimate.metadata_estimate_bytes)?,
            },
        )?;
        let operation = selection.materialize(results::TaskResultWriteLimits::default())?;
        let token = tasks::prepare_original_result_predicate_tx(
            tx, &f.scope, &operation, &bounded, &ledger, None,
        )?;
        assert!(token.material_estimate().bytes < bounded.byte_limit);
        assert!(token.material_estimate().bytes + estimate.payload_bytes > bounded.byte_limit);
        drop(token);
        drop(operation);
        drop(guard);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let before =
        f.db.task_result_settlements()
            .latest_for_message(&source.id)?;
    let count = f.receipt_count()?;
    let error = execute(&f, &original, &bounded, &ledger).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        f.db.task_result_settlements()
            .latest_for_message(&source.id)?,
        before
    );
    assert_eq!(f.receipt_count()?, count);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    bounded.byte_limit = 8 * 1024 * 1024;
    let InputMutationExecution::Committed { value, receipt } =
        execute(&f, &original, &bounded, &ledger)?
    else {
        panic!("same original may retry with sufficient material")
    };
    assert!(value.applied);
    assert!(
        matches!(outcome(&receipt),results::TaskResultWriteOutcome::Record(Some(record))
        if record.activation_id.as_deref()==Some("remaining:activation"))
    );
    assert!(ledger.report()?.observed_bytes > 0);
    drop(value);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn result_receipt_all_normalized_columns_are_bounded_before_direct_and_fresh_decode() -> Result<()>
{
    for column in [
        "family",
        "phase_version",
        "command_digest",
        "scope_digest",
        "prepared_at",
        "committed_at",
        "result_digest",
    ] {
        let f = Fixture::new()?;
        let ledger = ledger()?;
        let prepared = freeze_result(
            &f,
            "result:normalized-budget",
            TaskResultOperation::Wake {
                work_item_id: "absent".into(),
                at: f.at,
            },
        )?;
        drop(execute(&f, &prepared, &request(), &ledger)?);
        // Fixed internal column names exercise every normalized String read by
        // the original lookup. Small bad JSON makes ordering observable.
        let connection = f.db.connection()?;
        // Deliberate corruption of checked columns is local to this fixture
        // connection. The production schema and receipt validator stay strict.
        let checked = matches!(
            column,
            "phase_version" | "command_digest" | "scope_digest" | "result_digest"
        );
        if checked {
            connection.execute_batch("PRAGMA ignore_check_constraints=ON")?;
        }
        let update = connection.execute(
            &format!("UPDATE input_mutation_receipts SET {column}=?1,prepared_json='bad-json' WHERE command_nonce=?2"),
            params!["oversized-normalized".repeat(128 * 1024), prepared.descriptor().command_nonce()],
        );
        if checked {
            connection.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
        }
        update?;
        let mut tiny = request();
        tiny.byte_limit = 1024 * 1024;
        let assert_budget = |error: anyhow::Error| {
            assert!(
                matches!(
                    error.downcast_ref::<tasks::TaskSnapshotError>(),
                    Some(tasks::TaskSnapshotError::ResourceExhausted(
                        "original Result receipt"
                    ))
                ),
                "{column}: {error:#}"
            );
        };
        assert_budget(execute(&f, &prepared, &tiny, &ledger).unwrap_err());
        let attempt = crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
            prepared.descriptor().clone(),
        );
        attempt.finish_original_attempt();
        let mut connection = f.db.connection()?;
        let error = crate::runtime_db::input_mutation_receipts::with_fresh_input_mutation_resolution_and_preflight(
            &mut connection, &attempt,
            |tx| crate::runtime_db::transitions::task_result_receipt_budget_tx(tx,prepared.descriptor(),&f.scope,&tiny,&ledger),
            |_,_| -> Result<()> { panic!("normalized bytes must be bounded before lookup/callback") },
        ).unwrap_err();
        assert_budget(error);
        assert_eq!(f.receipt_count()?, 1);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
