use super::*;
use crate::runtime::resident_input_evidence::ResidentInputEvidence;
use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;
use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::{LegacyDeliverySourceAvailability, PreparedInputEvidence},
        resident_work::WorkDeliveryHead,
    },
    runtime_db::{
        evidence::insert_delivery_summary_evidence_tx,
        input_evidence_snapshot::register_pruned_input_evidence_tx,
    },
    types::DeliverySummaryRecord,
};

fn delivery(id: &str, owner: &str, text: &str, offset: i64) -> DeliverySummaryRecord {
    let mut record = DeliverySummaryRecord::new("agent-a", owner, text, Some(7), None);
    record.id = id.into();
    record.created_at = time() + chrono::Duration::milliseconds(offset);
    record
}

#[test]
fn selected_work_owns_two_original_delivery_heads_and_default_body_capture_is_pure() -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("two-heads", WorkItemState::Completed))?;
        raw_legacy_work(tx, &work("one-head", WorkItemState::Completed))?;
        for value in [
            delivery("old", "two-heads", "accepted", 0),
            delivery("new-empty", "two-heads", "", 1),
            delivery("same", "one-head", "one body", 2),
            delivery("unselected", "outside", "outside history", 3),
        ] {
            insert_delivery_summary_evidence_tx(tx, &value)?;
        }
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (work, inputs) = db.transaction_once(|tx| {
        prepare_work_with_owning_inputs_tx(tx, &scope(), &request(), false, &ledger, None, (0, 0))
    })?;
    let inputs = inputs.context("default selected source bodies were not prepared")?;
    let captured = work.resident.capture(&WorkCaptureRequest::current())?;
    let heads = captured.delivery_heads("two-heads")?;
    assert_eq!(heads.latest_any.body_id(), Some("new-empty"));
    assert_eq!(heads.latest_nonempty.body_id(), Some("old"));
    let same = captured.delivery_heads("one-head")?;
    assert_eq!(same.latest_any.body_id(), same.latest_nonempty.body_id());
    assert_eq!(work.owning_input_roots.len(), 3);
    let inputs = inputs.with_budget(ledger.clone())?;
    let selected = inputs.capture()?;
    assert_eq!(
        selected
            .records
            .iter()
            .filter(|record| matches!(record.record, PreparedInputEvidence::LegacyDelivery(_)))
            .count(),
        3
    );
    assert!(!selected
        .records
        .iter()
        .any(|record| record.locator.key.id == "unselected"));
    let weak = selected
        .records
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    db.transaction(|tx| {
        tx.execute_batch(
            "DROP TABLE delivery_summaries; DROP TABLE work_items; DROP TABLE agent_states;",
        )?;
        Ok(())
    })?;
    // Both views below use only held immutable allocations after all relevant
    // body/state tables have become unreadable.
    assert_eq!(captured.delivery_heads("two-heads")?, heads);
    let key = InputEvidenceKey::new(InputEvidenceFamily::LegacyDelivery, "old");
    let PreparedInputEvidence::LegacyDelivery(body) = &selected.lookup(&key)?.unwrap().record
    else {
        unreachable!()
    };
    assert_eq!(body.text, "accepted");
    let repeated = inputs.capture()?;
    assert!(Arc::ptr_eq(
        selected.lookup(&key)?.unwrap(),
        repeated.lookup(&key)?.unwrap()
    ));
    drop(repeated);
    drop(inputs);
    assert!(weak.iter().all(|record| record.upgrade().is_some()));
    drop(selected);
    drop(captured);
    drop(work);
    assert!(weak.iter().all(|record| record.upgrade().is_none()));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_delivery_removal_updates_selected_work_metadata_once_without_body_hydration(
) -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("selected", WorkItemState::Completed))?;
        insert_delivery_summary_evidence_tx(tx, &delivery("old", "selected", "old", 0))?;
        insert_delivery_summary_evidence_tx(tx, &delivery("latest", "selected", "latest", 1))?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (prepared, inputs) = db.transaction_once(|tx| {
        prepare_work_with_owning_inputs_tx(tx, &scope(), &request(), false, &ledger, None, (0, 0))
    })?;
    let inputs = inputs.context("original source body was not prepared")?;
    let before = prepared.resident.capture(&WorkCaptureRequest::current())?;
    let original = InputEvidenceKey::new(InputEvidenceFamily::LegacyDelivery, "latest");
    let (delta, changed) = db.transaction_once(|tx| {
        tx.execute(
            "DELETE FROM delivery_summaries WHERE evidence_id='latest'",
            [],
        )?;
        let delta =
            register_pruned_input_evidence_tx(tx, &scope(), std::slice::from_ref(&original))?;
        let changed =
            refresh_work_delivery_sources_tx(tx, &scope(), &delta, &request(), &ledger, None)?
                .context("actual source removal did not update existing Work head")?;
        assert!(
            refresh_work_delivery_sources_tx(tx, &scope(), &delta, &request(), &ledger, None)?
                .is_none()
        );
        Ok((delta, changed))
    })?;
    assert_eq!(delta.removals.len(), 1);
    assert_eq!(changed.base_revision, before.revision);
    assert_eq!(changed.revision, before.revision + 1);
    assert!(changed.records.is_empty() && changed.removals.is_empty());
    let next = prepared.resident.with_committed(&changed)?;
    let captured = next.capture(&WorkCaptureRequest::current())?;
    match &captured.delivery_heads("selected")?.latest_any {
        WorkDeliveryHead::Present {
            record_id,
            availability,
            ..
        } => {
            assert_eq!(record_id, "latest");
            assert_eq!(*availability, LegacyDeliverySourceAvailability::Pruned);
        }
        _ => anyhow::bail!("pruned head was replaced by absence or an older body"),
    }
    assert_eq!(
        before.delivery_heads("selected")?.latest_any.body_id(),
        Some("latest")
    );
    assert!(captured
        .delivery_heads("selected")?
        .latest_any
        .body_id()
        .is_none());
    // An old held Input body survives as evidence; it cannot reinstall SQL.
    assert!(inputs.capture()?.lookup(&original)?.is_some());
    let rows: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM delivery_summaries WHERE evidence_id='latest'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(rows, 0);
    Ok(())
}

#[test]
fn default_work_delivery_native_nodes_reject_before_alias_or_coverage_install_and_release(
) -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("selected", WorkItemState::Completed))?;
        let mut value = delivery("large", "selected", "accepted", 0);
        value.evidence = Some(serde_json::json!({"nodes":vec![0;5000]}));
        insert_delivery_summary_evidence_tx(tx, &value)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let before:Option<String>=db.connection()?.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1",[],|r|r.get(0)).optional()?;
    let mut limited = request();
    limited.record_limit = 8192;
    let probe = db
        .transaction_once(|tx| -> Result<()> {
            drop(
                prepare_work_domain_snapshot_with_budget_tx(
                    tx,
                    &scope(),
                    &limited,
                    false,
                    &ledger,
                    None,
                )?
                .require_current()?,
            );
            anyhow::bail!("Work-only phase accepted; roll back the probe")
        })
        .unwrap_err();
    assert_eq!(
        probe.to_string(),
        "Work-only phase accepted; roll back the probe"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let error = db
        .transaction_once(|tx| {
            prepare_work_with_owning_inputs_tx(tx, &scope(), &limited, false, &ledger, None, (0, 0))
        })
        .unwrap_err();
    assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))),"{error:#}");
    let (after, aliases):(Option<String>,u64)=db.connection()?.query_row("SELECT (SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1),(SELECT COUNT(*) FROM input_evidence_current WHERE family='legacy_delivery')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!(before, after);
    assert_eq!(aliases, 0);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn work_and_delivery_each_fit_but_combined_original_phase_rejects_without_reobserving_prior(
) -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::{
        prepare_input_evidence_snapshot_with_material_tx, read_material::InputReadMaterial,
        InputEvidenceLoadRequest,
    };
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("selected", WorkItemState::Completed))?;
        let mut value = delivery("head", "selected", "accepted", 0);
        value.evidence = Some(serde_json::json!({"nodes":vec![0;1000]}));
        insert_delivery_summary_evidence_tx(tx, &value)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    // Establish the actual sidecars, so both cost probes and the compound read
    // follow identical prepared-alias predicates rather than first hydration.
    drop(db.transaction_once(|tx| {
        prepare_work_with_owning_inputs_tx(tx, &scope(), &request(), false, &ledger, None, (0, 0))
    })?);
    let original = scope();
    let full = request();
    let work_cost = db.transaction_once(|tx| {
        let current = prepare_work_domain_snapshot_with_budget_tx(
            tx, &original, &full, false, &ledger, None,
        )?
        .require_current()?;
        Ok(current.material_usage)
    })?;
    let mut input_request = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
            InputEvidenceFamily::LegacyDelivery,
            "head",
        )]),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: MAX_METADATA_RECORDS,
        byte_limit: MAX_METADATA_BYTES,
    };
    let input_cost = db.transaction_once(|tx| {
        let mut material = InputReadMaterial::new(&original, &input_request, &ledger, (0, 0))?;
        drop(prepare_input_evidence_snapshot_with_material_tx(
            tx,
            &original,
            &input_request,
            false,
            &mut material,
        )?);
        Ok(material.usage())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(work_cost.0 > 1024 && input_cost.0 > 1024);
    let mut limited = full;
    limited.byte_limit = work_cost
        .0
        .max(input_cost.0)
        .checked_add(1024)
        .context("fixture combined cap overflow")?;
    assert!(work_cost.0 < limited.byte_limit && input_cost.0 < limited.byte_limit);
    assert!(work_cost.0.checked_add(input_cost.0).unwrap() > limited.byte_limit);
    drop(db.transaction_once(|tx| {
        prepare_work_domain_snapshot_with_budget_tx(tx, &original, &limited, false, &ledger, None)
    })?);
    input_request.byte_limit = limited.byte_limit;
    db.transaction_once(|tx| {
        let mut material = InputReadMaterial::new(&original, &input_request, &ledger, (0, 0))?;
        drop(prepare_input_evidence_snapshot_with_material_tx(
            tx,
            &original,
            &input_request,
            false,
            &mut material,
        )?);
        Ok(())
    })?;
    let before:String=db.connection()?.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1",[],|r|r.get(0))?;
    let error = db
        .transaction_once(|tx| {
            prepare_work_with_owning_inputs_tx(
                tx,
                &original,
                &limited,
                false,
                &ledger,
                None,
                (0, 0),
            )
        })
        .unwrap_err();
    assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))),"{error:#}");
    let after:String=db.connection()?.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1",[],|r|r.get(0))?;
    assert_eq!(before, after);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn restored_delivery_head_association_or_time_drift_rejects_before_owning_alias_install(
) -> Result<()> {
    for wrong_work in [true, false] {
        let (_dir, db) = legacy(|tx| {
            raw_legacy_work(tx, &work("source-work", WorkItemState::Completed))?;
            raw_legacy_work(tx, &work("other-work", WorkItemState::Completed))?;
            insert_delivery_summary_evidence_tx(
                tx,
                &delivery("source", "source-work", "actual source", 0),
            )?;
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        drop(db.transaction_once(|tx| {
            prepare_work_domain_snapshot_with_budget_tx(
                tx,
                &scope(),
                &request(),
                false,
                &ledger,
                None,
            )
        })?);
        db.transaction(|tx| {
            if wrong_work {
                tx.execute("UPDATE input_legacy_delivery_current_sources SET work_item_id='other-work' WHERE agent_id='agent-a' AND identity_incarnation=1 AND record_id='source'",[])?;
            } else {
                tx.execute("UPDATE input_legacy_delivery_current_sources SET created_at='2099-01-01T00:00:00Z' WHERE agent_id='agent-a' AND identity_incarnation=1 AND record_id='source'",[])?;
            }
            Ok(())
        })?;
        let before:String=db.connection()?.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1",[],|r|r.get(0))?;
        let error = db
            .transaction_once(|tx| {
                prepare_work_with_owning_inputs_tx(
                    tx,
                    &scope(),
                    &request(),
                    false,
                    &ledger,
                    None,
                    (0, 0),
                )
            })
            .unwrap_err();
        assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
            Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(_))),"{error:#}");
        let (after,aliases):(String,u64)=db.connection()?.query_row("SELECT (SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1),(SELECT COUNT(*) FROM input_evidence_current WHERE family='legacy_delivery')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(before, after);
        assert_eq!(aliases, 0);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn source_ack_metadata_phase_debits_original_still_live_input_body_and_preserves_its_guard(
) -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("selected", WorkItemState::Completed))?;
        let mut value = delivery("head", "selected", "accepted", 0);
        value.evidence = Some(serde_json::json!({"nodes":vec![0;1000]}));
        insert_delivery_summary_evidence_tx(tx, &value)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (work, inputs) = db.transaction_once(|tx| {
        prepare_work_with_owning_inputs_tx(tx, &scope(), &request(), false, &ledger, None, (0, 0))
    })?;
    let inputs = inputs
        .context("actual source Input absent")?
        .with_budget(ledger.clone())?;
    let captured = inputs.capture()?;
    let key = InputEvidenceKey::new(InputEvidenceFamily::LegacyDelivery, "head");
    let record = captured.lookup(&key)?.unwrap();
    let original_cost = record.resident_material_estimate()?;
    let delta = crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta {
        records: vec![record.clone()],
        ..Default::default()
    };
    let mut limited = request();
    limited.byte_limit = original_cost
        .0
        .checked_add(1024)
        .context("Source phase fixture cost overflow")?;
    assert!(original_cost.0 < limited.byte_limit);
    let before_usage = ledger.report()?.observed_bytes;
    let before:String=db.connection()?.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1",[],|r|r.get(0))?;
    let error = db
        .transaction_once(|tx| {
            refresh_work_delivery_sources_tx(tx, &scope(), &delta, &limited, &ledger, None)
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    let after:String=db.connection()?.query_row("SELECT coverage_json FROM work_domain_current WHERE agent_id='agent-a' AND identity_incarnation=1",[],|r|r.get(0))?;
    assert_eq!(before, after);
    assert_eq!(ledger.report()?.observed_bytes, before_usage);
    assert!(Arc::ptr_eq(record, captured.lookup(&key)?.unwrap()));
    drop(delta);
    drop(captured);
    drop(inputs);
    drop(work);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn original_work_input_ack(
    db: &crate::runtime_db::RuntimeDb,
    ledger: &InputEvidenceBudgetLedger,
    previous: &ResidentInputEvidence,
    fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
) -> Result<
    crate::runtime_db::transitions::InputMutationExecution<
        crate::runtime_db::transitions::TransitionCommit,
    >,
> {
    use crate::runtime_db::transitions::{
        PreparedInputTransition, WorkItemMutation, WorkItemTransitionCommand,
    };
    let mut record = db
        .work_items()
        .latest("selected")?
        .context("original selected Work absent")?;
    let revision = record.revision;
    record.revision += 1;
    record.updated_at += chrono::Duration::seconds(1);
    record.objective = "actual original Work source ACK".into();
    let mut brief = brief("actual-new-result", "selected");
    brief.created_at = record.updated_at;
    let command = WorkItemTransitionCommand {
        agent_id: scope().agent_id,
        mutation: WorkItemMutation::Update {
            record,
            expected_revision: revision,
        },
        agent_state: None,
        brief_evidence: vec![brief.clone()],
        audit_events: vec![crate::types::brief_created_event_for(&brief)?],
        index_changes: Vec::new(),
        notify_scheduler: false,
        fault,
    };
    let prepared = PreparedInputTransition::work(
        scope(),
        "original-work-input-ack".into(),
        time(),
        command,
        Default::default(),
        false,
    )?
    .with_work_input_selection(
        previous,
        ledger,
        (MAX_METADATA_BYTES, MAX_METADATA_RECORDS),
    )?;
    db.transitions_in_scope(&scope())
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &prepared,
            &request(),
            ledger,
            &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: MAX_METADATA_RECORDS,
                byte_limit: MAX_METADATA_BYTES,
            },
        )
}

#[test]
fn original_work_ack_prepares_complete_input_closure_without_minting_hydrated_births() -> Result<()>
{
    use crate::runtime_db::transitions::InputMutationExecution;
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("selected", WorkItemState::Open))?;
        insert_delivery_summary_evidence_tx(
            tx,
            &delivery("original-head", "selected", "old original", 0),
        )?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (previous_work, previous) = db.transaction_once(|tx| {
        prepare_work_with_owning_inputs_tx(tx, &scope(), &request(), false, &ledger, None, (0, 0))
    })?;
    let previous = previous
        .context("original Input closure absent")?
        .with_budget(ledger.clone())?;
    let old = InputEvidenceKey::new(InputEvidenceFamily::LegacyDelivery, "original-head");
    let old_revision = previous.capture()?.lookup(&old)?.unwrap().locator.revision;
    let InputMutationExecution::Committed { value, receipt } =
        original_work_input_ack(&db, &ledger, &previous, None)?
    else {
        anyhow::bail!("first original Work source phase replayed")
    };
    let prepared = value
        .effects
        .prepared_work_inputs
        .as_ref()
        .context("original ACK omitted prepared Input closure")?;
    let work = value
        .effects
        .work_delta
        .as_ref()
        .context("original ACK omitted actual Work delta")?;
    let roots = &value
        .effects
        .work_input_preparation
        .as_ref()
        .unwrap()
        .owning_roots;
    assert!(prepared.covers_work_input_sources(&work.coverage, roots));
    assert!(prepared
        .capture()?
        .lookup(&InputEvidenceKey::new(
            InputEvidenceFamily::Brief,
            "actual-new-result"
        ))?
        .is_some());
    assert_eq!(
        prepared.capture()?.lookup(&old)?.unwrap().locator.revision,
        old_revision
    );
    assert!(
        value
            .effects
            .input_evidence_delta
            .records
            .iter()
            .all(|record| record.locator.key != old),
        "hydrated original Legacy is not a native D3 write"
    );
    assert_eq!(
        receipt.outcome.scopes[0].install_keys.completion_brief_ids,
        BTreeSet::from(["actual-new-result".to_owned()])
    );
    let next = previous
        .with_committed_work_sources(
            &value.effects.input_evidence_delta,
            roots.clone(),
            Some(prepared),
        )?
        .context("same-TX Work input closure failed to install into original domain")?;
    assert!(next.covers_work_input_sources(&work.coverage, roots));
    let captured = next.capture()?;
    let source = captured.lookup(&old)?.unwrap();
    let weak = Arc::downgrade(source);
    db.connection()?.execute_batch("DROP TABLE briefs;DROP TABLE delivery_summaries;DROP TABLE messages;DROP TABLE work_items;")?;
    assert!(
        matches!(&captured.lookup(&old)?.unwrap().record,PreparedInputEvidence::LegacyDelivery(body) if body.text=="old original")
    );
    assert!(next
        .capture()?
        .lookup(&InputEvidenceKey::new(
            InputEvidenceFamily::Brief,
            "actual-new-result"
        ))?
        .is_some());
    drop(captured);
    drop(next);
    drop(value);
    drop(previous);
    drop(previous_work);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_work_input_phase_before_commit_fault_rolls_back_all_source_aliases_and_receipt(
) -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("selected", WorkItemState::Open))?;
        insert_delivery_summary_evidence_tx(
            tx,
            &delivery("original-head", "selected", "old original", 0),
        )?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (work, previous) = db.transaction_once(|tx| {
        prepare_work_with_owning_inputs_tx(tx, &scope(), &request(), false, &ledger, None, (0, 0))
    })?;
    let previous = previous.unwrap().with_budget(ledger.clone())?;
    let original = db.work_items().latest("selected")?.unwrap();
    let coverage = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    let baseline = ledger.report()?.observed_bytes;
    let error = original_work_input_ack(
        &db,
        &ledger,
        &previous,
        Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BeforeCommit"), "{error:#}");
    assert_eq!(
        db.work_items().latest("selected")?.as_ref(),
        Some(&original)
    );
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        coverage
    );
    let (body,alias,receipt):(u64,u64,u64)=db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM briefs WHERE evidence_id='actual-new-result'),(SELECT COUNT(*) FROM input_evidence_current WHERE family='brief' AND record_id='actual-new-result'),(SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='original-work-input-ack')",
        [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    assert_eq!((body, alias, receipt), (0, 0, 0));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert!(matches!(
        original_work_input_ack(&db, &ledger, &previous, None)?,
        crate::runtime_db::transitions::InputMutationExecution::Committed { .. }
    ));
    drop(previous);
    drop(work);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn committed_work_input_readback_debits_original_work_phase_before_its_first_scope_decode(
) -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("selected", WorkItemState::Open))?;
        insert_delivery_summary_evidence_tx(
            tx,
            &delivery("original-head", "selected", "old original", 0),
        )?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let error = db
        .transaction_once(|tx| -> Result<()> {
            let selected = scope();
            let policy = request();
            let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, "selected")]);
            let proof = prepare_work_write_with_budget_tx(
                tx, &selected, &keys, &policy, false, &ledger, None,
            )?;
            let mut material =
                writes::WorkWriteMaterial::for_readback(&proof, &policy, None)?.unwrap();
            let state = material.final_state_tx(tx, &proof)?;
            let (work, _) = material.register_tx(
                tx,
                &proof,
                &keys,
                &BTreeSet::new(),
                &state,
                &policy,
                false,
            )?;
            let usage = work
                .material_usage
                .context("actual Work material omitted")?;
            let input = InputEvidenceLoadRequest {
                windows: Vec::new(),
                mandatory_roots: BTreeSet::new(),
                interrupted_sources: BTreeSet::new(),
                selected_completion_brief_ids: BTreeSet::new(),
                record_limit: MAX_METADATA_RECORDS,
                byte_limit: MAX_METADATA_BYTES,
            };
            let mut small = policy;
            small.byte_limit = usage.0 + 1;
            assert!(usage.0 < small.byte_limit);
            prepare_committed_work_inputs_tx(
                tx,
                &selected,
                &input,
                &work.delta,
                &work.owning_input_roots,
                usage,
                &small,
                &ledger,
                None,
                None,
            )?;
            anyhow::bail!("combined original material unexpectedly admitted")
        })
        .unwrap_err();
    assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))),"{error:#}");
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
