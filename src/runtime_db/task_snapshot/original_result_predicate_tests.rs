use super::*;
use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
use crate::runtime_db::task_result_settlement::{
    select_admit_message_tx, select_admit_reply_message_tx, select_admit_unsettled_tx,
    PreparedTaskResultMutation, TaskResultWriteLimits, TaskResultWriteOutcome,
};

fn result_for_message_on(
    tx: &Transaction<'_>,
    message_id: &str,
) -> Result<Option<crate::runtime_db::TaskResultSettlementRecord>> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE message_id=?1",
            [message_id],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
        .transpose()
}

fn selected<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    kind: &str,
    message: &str,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(
    PreparedTaskResultMutation<'tx, 'connection>,
    Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
)> {
    let limits = TaskResultWriteLimits::default();
    let selection = match kind {
        "reply" => select_admit_reply_message_tx(
            tx,
            "task-agent",
            None,
            "original-reply",
            "new-activation",
            at() + Duration::seconds(2),
            limits,
        )?,
        "unsettled" => select_admit_unsettled_tx(
            tx,
            "task-agent",
            None,
            "new-activation",
            at() + Duration::seconds(2),
            limits,
        )?,
        _ => select_admit_message_tx(
            tx,
            "task-agent",
            message,
            "new-activation",
            at() + Duration::seconds(2),
            limits,
        )?,
    };
    let estimate = selection.material_estimate();
    let guard = ledger.register_owned_bytes(
        &scope(),
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(estimate.payload_bytes)?,
            metadata_estimate_bytes: u64::try_from(estimate.metadata_estimate_bytes)?,
        },
    )?;
    Ok((selection.materialize(limits)?, guard))
}

#[test]
fn opaque_predicate_keeps_invalid_reply_skip_and_message_unsettled_actual_ids_without_foreign_decode(
) -> Result<()> {
    for relation in ["missing", "foreign_bad_body", "invalid_fence"] {
        for operation in ["reply", "message", "unsettled"] {
            let (_dir, db) = fresh()?;
            let expected = result_task("predicate-source-task");
            let result = db.transaction(|tx| {
                if relation != "missing" {
                    let mut canonical = expected.clone();
                    if relation == "foreign_bad_body" { canonical.agent_id = "foreign-agent".into(); }
                    if relation == "invalid_fence" { canonical.detail = None; }
                    repositories::upsert_task_tx(tx, &canonical)?;
                    if relation == "foreign_bad_body" {
                        // Valid JSON but deliberately not a decodable TaskRecord.
                        // Its unrelated malformed field is never foreign-body authority.
                        tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.summary',json_object('bad','TaskSummary')) WHERE task_id=?1", [&canonical.id])?;
                        let raw: String = tx.query_row("SELECT payload_json FROM tasks WHERE task_id=?1",[&canonical.id],|r|r.get(0))?;
                        assert!(serde_json::from_str::<TaskRecord>(&raw).is_err());
                    }
                }
                let mut source = result_source("predicate-message", &expected.id);
                if operation == "unsettled" {
                    // The original owner selector excludes original-message
                    // reply rows; this operation needs an ordinary TaskResult.
                    source.metadata = None;
                }
                qualify_result(tx, &source, &expected)
            })?;
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            db.transaction(|tx| {
                let (prepared, selection_guard) =
                    selected(tx, operation, &result.message_id, &ledger)?;
                assert_eq!(
                    prepared.candidate_result_ids(),
                    &BTreeSet::from([result.result_identity.clone()])
                );
                let proof = prepare_original_result_predicate_tx(
                    tx,
                    &scope(),
                    &prepared,
                    &request(),
                    &ledger,
                    None,
                )?;
                let retained = ledger.report()?.observed_bytes;
                let (ack, retention) = prepared.execute_with_original_predicate(proof)?;
                assert_eq!(ledger.report()?.observed_bytes, retained);
                if operation == "reply" {
                    assert!(matches!(ack.outcome, TaskResultWriteOutcome::Unit));
                    assert!(ack.actual_result_ids.is_empty());
                    assert_eq!(
                        stored_on(
                            tx,
                            "task-agent",
                            &TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity)
                        )?
                        .unwrap()
                        .fact,
                        CanonicalTaskFact::ResultSettlement(result.clone())
                    );
                } else {
                    assert_eq!(
                        ack.actual_result_ids,
                        BTreeSet::from([result.result_identity.clone()])
                    );
                    match ack.outcome {
                        TaskResultWriteOutcome::Record(value) => assert!(value.is_none()),
                        TaskResultWriteOutcome::Records(value) => assert!(value.is_empty()),
                        _ => anyhow::bail!("wrong original admit outcome"),
                    }
                    let CanonicalTaskFact::ResultSettlement(current) = stored_on(
                        tx,
                        "task-agent",
                        &TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
                    )?
                    .unwrap()
                    .fact
                    else {
                        unreachable!()
                    };
                    assert_eq!(current.state, TaskResultSettlementState::Settled);
                    assert_eq!(
                        current.disposition,
                        Some(TaskResultSettlementDisposition::InvalidOrStale)
                    );
                    assert_eq!(current.next_recheck_at, None);
                }
                assert!(
                    alias(tx, &scope(), &TaskKey::new(TaskFamily::Task, &expected.id))?.is_none()
                );
                drop(retention);
                drop(selection_guard);
                assert_eq!(ledger.report()?.observed_bytes, 0);
                Ok(())
            })?;
        }
    }
    Ok(())
}

#[test]
fn opaque_predicate_admits_qualified_task_but_never_adopts_a_matching_raw_task() -> Result<()> {
    for qualified in [true, false] {
        let (_dir, db) = fresh()?;
        let value = result_task("own-predicate-task");
        let result = db.transaction(|tx| {
            if qualified {
                write_task(tx, &value)?;
            } else {
                repositories::upsert_task_tx(tx, &value)?;
            }
            let source = result_source("own-predicate-message", &value.id);
            if qualified {
                return qualify_result(tx, &source, &value);
            }
            // Existing raw business rows are kept without late membership.
            let (message, _) = append_message_tx(tx, &source)?;
            let result = TaskResultSettlementRecord::pending(&value, &message, at())?.unwrap();
            upsert_pending_tx(tx, &result)?;
            Ok(result)
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let (prepared,guard)=selected(tx,"message",&result.message_id,&ledger)?;
            if qualified {
                let proof=prepare_original_result_predicate_tx(tx,&scope(),&prepared,&request(),&ledger,None)?;
                let (ack,retention)=prepared.execute_with_original_predicate(proof)?;
                assert_eq!(ack.actual_result_ids,BTreeSet::from([result.result_identity.clone()]));
                let admitted=ack.into_record()?.unwrap();
                assert_eq!(admitted.state,TaskResultSettlementState::CallerAdmitted);
                assert_eq!(admitted.activation_id.as_deref(),Some("new-activation"));
                drop(retention);
            } else {
                let error=prepare_original_result_predicate_tx(tx,&scope(),&prepared,&request(),&ledger,None).err().context("matching raw Task must fail")?;
                assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::NeedsPreparation(key)) if key.id==value.id));
                assert!(alias(tx,&scope(),&TaskKey::new(TaskFamily::Task,&value.id))?.is_none());
                assert!(alias(tx,&scope(),&TaskKey::new(TaskFamily::ResultSettlement,&result.result_identity))?.is_none());
                assert_eq!(stored_on(tx,"task-agent",&TaskKey::new(TaskFamily::ResultSettlement,&result.result_identity))?.unwrap().fact,
                    CanonicalTaskFact::ResultSettlement(result.clone()));
            }
            drop(guard);
            assert_eq!(ledger.report()?.observed_bytes,0);
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn opaque_predicate_rejects_original_transaction_drift_and_wrong_selection_without_writes(
) -> Result<()> {
    for corruption in ["task_tuple", "task_full", "result", "selection"] {
        let (_dir, db) = fresh()?;
        let value = result_task("drift-task");
        let result = db.transaction(|tx| {
            write_task(tx, &value)?;
            qualify_result(tx, &result_source("drift-message", &value.id), &value)
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let error=db.transaction(|tx| {
            let (prepared,_guard)=selected(tx,"message",&result.message_id,&ledger)?;
            let proof=prepare_original_result_predicate_tx(tx,&scope(),&prepared,&request(),&ledger,None)?;
            match corruption {
                "task_tuple" => { tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.detail.rejoin_generation',2) WHERE task_id=?1",[&value.id])?; }
                "task_full" => { tx.execute("UPDATE tasks SET summary='new',payload_json=json_set(payload_json,'$.summary','new') WHERE task_id=?1",[&value.id])?; }
                "result" => { tx.execute("UPDATE task_result_settlements SET payload_json=json_set(payload_json,'$.deferred_reason','changed') WHERE result_identity=?1",[&result.result_identity])?; }
                "selection" => {
                    let (other,_guard2)=selected(tx,"message","absent-other-message",&ledger)?;
                    return other.execute_with_original_predicate(proof).map(|_|());
                }
                _=>unreachable!(),
            }
            prepared.execute_with_original_predicate(proof).map(|_|())
        }).err().context("drift must roll back")?;
        assert!(
            format!("{error:#}").contains("changed") || format!("{error:#}").contains("incomplete")
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        db.transaction(|tx| {
            let stored =
                stored_on(tx, "task-agent", &TaskKey::new(TaskFamily::Task, &value.id))?.unwrap();
            assert_eq!(
                stored.fact,
                CanonicalTaskFact::Task(repositories::slim_task_record_for_payload(&value))
            );
            assert_eq!(
                stored_on(
                    tx,
                    "task-agent",
                    &TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity)
                )?
                .unwrap()
                .fact,
                CanonicalTaskFact::ResultSettlement(result.clone())
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn opaque_predicate_reserves_finite_tuple_before_decode_and_releases_failed_guards() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = result_task("oversized-foreign-task");
    let result = db.transaction(|tx| {
        let mut foreign = value.clone();
        foreign.agent_id = "foreign-agent".into();
        repositories::upsert_task_tx(tx, &foreign)?;
        qualify_result(
            tx,
            &result_source("oversized-predicate-message", &value.id),
            &value,
        )
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        // Explicit damaged predicate metadata, with the payload ID matching the
        // normalized ID. It must fail the integer-length reservation first.
        tx.execute("UPDATE tasks SET owner_agent_id=?2,payload_json=json_set(payload_json,'$.agent_id',?2) WHERE task_id=?1",params![value.id,"x".repeat(32*1024)])?;
        let (prepared,guard)=selected(tx,"message",&result.message_id,&ledger)?;
        let before=ledger.report()?.observed_bytes;
        let mut tiny=request();tiny.byte_limit=16*1024;
        let error=prepare_original_result_predicate_tx(tx,&scope(),&prepared,&tiny,&ledger,None).err().context("predicate budget must reject")?;
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted(_))));
        assert_eq!(ledger.report()?.observed_bytes,before);
        assert_eq!(stored_on(tx,"task-agent",&TaskKey::new(TaskFamily::ResultSettlement,&result.result_identity))?.unwrap().fact,
            CanonicalTaskFact::ResultSettlement(result.clone()));
        drop(guard);
        assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })
}

#[test]
fn invalid_only_requires_original_predicate_and_does_not_decode_foreign_task() -> Result<()> {
    for relation in ["missing", "foreign_bad_body", "invalid_fence", "qualified"] {
        let (_dir, db) = fresh()?;
        let expected = result_task("invalid-only-task");
        let result = db.transaction(|tx| {
            if relation == "qualified" { write_task(tx, &expected)?; }
            else if relation != "missing" {
                let mut actual = expected.clone();
                if relation == "foreign_bad_body" { actual.agent_id="foreign-agent".into(); }
                else { actual.detail=None; }
                repositories::upsert_task_tx(tx, &actual)?;
                if relation == "foreign_bad_body" {
                    tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.summary',json_object('bad','not-text')) WHERE task_id=?1", [&actual.id])?;
                }
            }
            qualify_result(tx, &result_source("invalid-only-source", &expected.id), &expected)
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let estimate=crate::runtime_db::task_result_settlement::settle_invalid_only_material_estimate_tx(tx,"task-agent",&result.message_id)?;
            let guard=ledger.register_owned_bytes(&scope(),InputMemoryCategory::Candidate,
                OwnedBytesEstimate{payload_bytes:u64::try_from(estimate.payload_bytes)?,metadata_estimate_bytes:u64::try_from(estimate.metadata_estimate_bytes)?})?;
            let select=|| crate::runtime_db::task_result_settlement::select_settle_invalid_only_tx(tx,"task-agent",&result.message_id,at()+Duration::seconds(2),TaskResultWriteLimits::default())?.materialize(TaskResultWriteLimits::default());
            let prepared=select()?;
            assert!(prepared.dependency_task_ids().is_empty());
            let error=select()?.execute().unwrap_err();
            assert!(error.to_string().contains("original transaction predicate proof"));
            let proof=prepare_original_result_predicate_tx(tx,&scope(),&prepared,&request(),&ledger,None)?;
            let (ack,retained)=prepared.execute_with_original_predicate(proof)?;
            assert_eq!(ack.actual_result_ids.is_empty(),relation=="qualified");
            assert!(matches!(ack.outcome,TaskResultWriteOutcome::Changed(changed) if changed==(relation!="qualified")));
            let current=result_for_message_on(tx,&result.message_id)?.unwrap();
            if relation=="qualified" { assert_eq!(current,result); }
            else {
                assert_eq!(current.disposition,Some(TaskResultSettlementDisposition::InvalidOrStale));
                assert_eq!(current.activation_id,None);
                assert_eq!(current.next_recheck_at,None);
                assert!(alias(tx,&scope(),&TaskKey::new(TaskFamily::Task,&expected.id))?.is_none());
            }
            drop(retained);drop(guard);
            assert_eq!(ledger.report()?.observed_bytes,0);
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn invalid_only_budget_precedes_normalized_address_and_malformed_result_decode() -> Result<()> {
    let (_dir, db) = fresh()?;
    let expected = result_task("invalid-only-budget-task");
    let result = db.transaction(|tx| {
        qualify_result(
            tx,
            &result_source("invalid-only-budget-source", &expected.id),
            &expected,
        )
    })?;
    db.transaction(|tx| {
        tx.execute("UPDATE task_result_settlements SET task_id=?1,payload_json='{' WHERE result_identity=?2",params!["t".repeat(128*1024),result.result_identity])?;
        let error=crate::runtime_db::task_result_settlement::select_settle_invalid_only_tx(tx,"task-agent",&result.message_id,at(),TaskResultWriteLimits{record_limit:4096,byte_limit:4096}).err().context("budget must precede malformed Result")?;
        assert!(error.downcast_ref::<crate::runtime_db::task_result_settlement::TaskResultFootprintExhausted>().is_some());
        Ok(())
    })
}

#[test]
fn invalid_only_does_not_mask_qualified_task_corruption() -> Result<()> {
    let (_dir, db) = fresh()?;
    let expected = result_task("invalid-only-corrupt-qualified");
    let result = db.transaction(|tx| {
        write_task(tx, &expected)?;
        qualify_result(
            tx,
            &result_source("invalid-only-corrupt-source", &expected.id),
            &expected,
        )
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        tx.execute("UPDATE tasks SET summary='changed outside qualified writer',payload_json=json_set(payload_json,'$.summary','changed outside qualified writer') WHERE task_id=?1",[&expected.id])?;
        let estimate=crate::runtime_db::task_result_settlement::settle_invalid_only_material_estimate_tx(tx,"task-agent",&result.message_id)?;
        let guard=ledger.register_owned_bytes(&scope(),InputMemoryCategory::Candidate,OwnedBytesEstimate{payload_bytes:u64::try_from(estimate.payload_bytes)?,metadata_estimate_bytes:u64::try_from(estimate.metadata_estimate_bytes)?})?;
        let prepared=crate::runtime_db::task_result_settlement::select_settle_invalid_only_tx(tx,"task-agent",&result.message_id,at(),TaskResultWriteLimits::default())?.materialize(TaskResultWriteLimits::default())?;
        let error=prepare_original_result_predicate_tx(tx,&scope(),&prepared,&request(),&ledger,None).err().context("qualified corruption must remain strict")?;
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::CanonicalChanged(key)) if key.id==expected.id), "{error:#}");
        assert_eq!(result_for_message_on(tx,&result.message_id)?,Some(result.clone()));
        drop(prepared);drop(guard);assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })
}
