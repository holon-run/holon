//! Whole Result phase consumes the opaque predicate; it does not decode foreign Tasks.
use super::*;
use crate::runtime_db::task_result_settlement::TaskResultWriteOutcome;
use crate::runtime_db::transitions::{
    InputMutationExecution, PreparedInputTransition, PreparedTaskResultTransition,
    TaskResultOperation,
};

#[test]
fn original_whole_result_admit_uses_invalid_observation_without_foreign_body_or_adoption(
) -> Result<()> {
    for kind in ["message", "reply", "unsettled", "invalid_only"] {
        let (_dir, db) = fresh()?;
        let expected = result_task("whole-foreign-task");
        let mut source = result_source("whole-result-message", &expected.id);
        if kind == "unsettled" {
            source.metadata = None;
        }
        let source = db
            .messages()
            .append_with_index_changes_in_scope(&scope(), &source, &[])?
            .record;
        let result=db.transaction(|tx| {
            let mut foreign=expected.clone();foreign.agent_id="foreign-agent".into();
            repositories::upsert_task_tx(tx,&foreign)?;
            tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.summary',json_object('bad','not-text')) WHERE task_id=?1",[&foreign.id])?;
            qualify_result(tx,&source,&expected)
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let operation = match kind {
            "invalid_only" => TaskResultOperation::SettleInvalidOnly {
                message_id: source.id.clone(),
                at: at() + Duration::seconds(1),
            },
            "reply" => TaskResultOperation::AdmitReply {
                work_item_id: None,
                message_id: "original-reply".into(),
                activation_id: "whole-activation".into(),
                at: at() + Duration::seconds(1),
            },
            "unsettled" => TaskResultOperation::AdmitUnsettled {
                work_item_id: None,
                activation_id: "whole-activation".into(),
                at: at() + Duration::seconds(1),
            },
            _ => TaskResultOperation::AdmitMessage {
                message_id: source.id.clone(),
                activation_id: "whole-activation".into(),
                at: at() + Duration::seconds(1),
            },
        };
        let original = PreparedInputTransition::task_result(
            scope(),
            format!("whole:{kind}"),
            at(),
            PreparedTaskResultTransition::new("task-agent".into(), operation)?,
        )?;
        let InputMutationExecution::Committed { value, receipt } = db
            .transitions_in_scope(&scope())
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &original,
                &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                    capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                    record_limit: 4096,
                    byte_limit: 4 * 1024 * 1024,
                },
                &ledger,
                &request(),
            )?
        else {
            panic!("whole original ACK")
        };
        assert_eq!(value.applied, kind != "reply");
        if kind == "invalid_only" {
            assert!(matches!(
                &receipt.outcome.result,
                crate::runtime_db::input_mutation_receipts::InputMutationResult::TaskResult { outcome }
                    if matches!(outcome.as_ref(), TaskResultWriteOutcome::Changed(true))
            ));
        }
        assert!(value
            .effects
            .task_observation_keys
            .iter()
            .all(|key| key.family != TaskFamily::Task));
        db.transaction(|tx| {
            assert!(alias(tx, &scope(), &TaskKey::new(TaskFamily::Task, &expected.id))?.is_none());
            let current = stored_on(
                tx,
                "task-agent",
                &TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
            )?
            .unwrap();
            let CanonicalTaskFact::ResultSettlement(actual) = current.fact else {
                unreachable!()
            };
            if kind == "reply" {
                assert_eq!(actual, result);
                assert!(value.effects.task_write_keys.is_empty());
            } else {
                assert_eq!(actual.state, TaskResultSettlementState::Settled);
                assert_eq!(
                    actual.disposition,
                    Some(TaskResultSettlementDisposition::InvalidOrStale)
                );
                assert_eq!(
                    value.effects.task_write_keys,
                    BTreeSet::from([TaskKey::new(
                        TaskFamily::ResultSettlement,
                        &result.result_identity
                    )])
                );
            }
            Ok(())
        })?;
        assert!(ledger.report()?.observed_bytes > 0);
        drop(value);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let InputMutationExecution::Replayed {
            receipt: replayed,
            _material_budget_views,
        } = db
            .transitions_in_scope(&scope())
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &original,
                &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                    capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                    record_limit: 4096,
                    byte_limit: 4 * 1024 * 1024,
                },
                &ledger,
                &request(),
            )?
        else {
            panic!("exact receipt replay")
        };
        assert_eq!(replayed, receipt);
        drop(replayed);
        drop(_material_budget_views);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn invalid_only_whole_receipt_rechecks_legal_task_completion_and_replays_original_noop(
) -> Result<()> {
    fn write_actual_task(tx: &Transaction<'_>, value: &TaskRecord) -> Result<()> {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, value)?;
        let actual = crate::runtime_db::context_inputs::ContextDurableRead::new(tx, "task-agent")
            .scope_facts()?
            .state
            .context("actual canonical fixture State")?;
        register_task_writes_tx(tx, &proof, &keys, &actual, &request()).map(|_| ())
    }
    let (_dir, db) = fresh()?;
    let terminal = result_task("invalid-only-later-completion");
    let mut running = terminal.clone();
    running.status = TaskStatus::Running;
    let source = db
        .messages()
        .append_with_index_changes_in_scope(
            &scope(),
            &result_source("invalid-only-later-source", &terminal.id),
            &[],
        )?
        .record;
    let result = db.transaction(|tx| {
        write_actual_task(tx, &running)?;
        qualify_result(tx, &source, &terminal)
    })?;
    let captured = db.transaction(|tx| {
        Ok(prepare_task_domain_snapshot_tx(tx, &scope(), &request())?
            .require_current()?
            .resident
            .capture(&request().capture)?)
    })?;
    assert!(matches!(
        captured
            .result_dependency(&result.result_identity)?
            .relation,
        TaskResultDependencyRelation::Invalid { .. }
    ));
    let original = PreparedInputTransition::task_result(
        scope(),
        "invalid-only:before-completion".into(),
        at(),
        PreparedTaskResultTransition::new(
            "task-agent".into(),
            TaskResultOperation::SettleInvalidOnly {
                message_id: source.id.clone(),
                at: at() + Duration::seconds(2),
            },
        )?,
    )?;
    // The snapshot was Invalid, but the original transaction observes the legal
    // Running -> Completed transition. It must keep this now-valid Result.
    db.transaction(|tx| {
        running.status = TaskStatus::Completed;
        running.updated_at = at() + Duration::seconds(1);
        write_actual_task(tx, &running)
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    let execute = || {
        db.transitions_in_scope(&scope())
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &original,
                &work,
                &ledger,
                &request(),
            )
    };
    let InputMutationExecution::Committed { value, receipt } = execute()? else {
        panic!("original invalid-only ACK")
    };
    assert!(!value.applied);
    assert!(value.effects.task_write_keys.is_empty());
    assert!(matches!(
        &receipt.outcome.result,
        crate::runtime_db::input_mutation_receipts::InputMutationResult::TaskResult { outcome }
                    if matches!(outcome.as_ref(), TaskResultWriteOutcome::Changed(false))
    ));
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&source.id)?,
        Some(result.clone())
    );
    drop(value);
    // A different original phase can defer later; this command still returns
    // its own historical no-op rather than recomputing against the later row.
    let later = PreparedInputTransition::task_result(
        scope(),
        "invalid-only:later-defer".into(),
        at(),
        PreparedTaskResultTransition::new(
            "task-agent".into(),
            TaskResultOperation::Defer {
                message_id: source.id.clone(),
                reason: "later-original-phase".into(),
                at: at() + Duration::seconds(3),
                deadline: at() + Duration::seconds(30),
            },
        )?,
    )?;
    drop(
        db.transitions_in_scope(&scope())
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &later,
                &work,
                &ledger,
                &request(),
            )?,
    );
    let InputMutationExecution::Replayed {
        receipt: replayed,
        _material_budget_views,
    } = execute()?
    else {
        panic!("original invalid-only replay")
    };
    assert_eq!(replayed, receipt);
    drop(_material_budget_views);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
