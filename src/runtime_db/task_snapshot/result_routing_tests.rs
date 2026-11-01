use super::tests::{at, identity, legacy, request, scope, task};
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime_db::{evidence, work_snapshot};
use crate::types::{
    AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, TaskStatus,
    TurnRecord,
};
const TASK: &str = "routing-task";
const PARENT: &str = "routing-parent";
const BRIEF: &str = "routing-result-brief";
fn target() -> crate::types::TaskRecord {
    let mut value = task(TASK, TaskStatus::Completed);
    value.detail = Some(serde_json::json!({"parent_turn_id":PARENT}));
    value.parent_message_id = Some("routing-source".into());
    value
}
fn source() -> MessageEnvelope {
    let mut value = MessageEnvelope::new(
        "task-agent",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: TASK.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "accepted result".into(),
        },
    );
    value.id = "routing-source".into();
    value.created_at = at();
    value.task_id = Some(TASK.into());
    value.admission_context = Some(crate::types::AdmissionContext::RuntimeOwned);
    value.delivery_surface = Some(crate::types::MessageDeliverySurface::TaskRejoin);
    value.metadata = Some(
        serde_json::json!({"task_id":TASK,"task_kind":"command_task","task_status":"completed","task_summary":TASK,"task_detail":{"parent_turn_id":PARENT}}),
    );
    value
}
fn fixture(
    parent: bool,
) -> Result<(
    tempfile::TempDir,
    crate::runtime_db::RuntimeDb,
    MessageEnvelope,
)> {
    let (dir, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &target())?;
        if parent {
            let mut parent = TurnRecord::new("task-agent", PARENT, 1);
            parent.created_at = at();
            parent.produced_brief_ids = vec![BRIEF.into()];
            repositories::upsert_turn_record_tx(tx, &parent)?;
            let mut brief = crate::brief::make_task_result("task-agent", TASK, "first result");
            brief.id = BRIEF.into();
            brief.turn_id = Some(PARENT.into());
            brief.created_at = at();
            evidence::insert_brief_evidence_tx(tx, &brief)?;
        }
        Ok(())
    })?;
    // The source is a real post-cutover D3 original write ACK, not a raw hint.
    let source = db
        .messages()
        .append_with_index_changes_in_scope(&scope(), &source(), &[])?
        .record;
    assert!(source.message_seq.is_some_and(|seq| seq > 0));
    Ok((dir, db, source))
}
fn acquire(
    db: &crate::runtime_db::RuntimeDb,
    source: &MessageEnvelope,
    ledger: &InputEvidenceBudgetLedger,
    load: &TaskDomainLoadRequest,
) -> Result<TaskResultParentRead> {
    db.transaction(|tx| {
        let current=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),load,ledger)?.require_current()?;
        let capture=current.resident.capture(&load.capture)?;
        let fact=capture.lookup(&TaskKey::new(TaskFamily::Task,TASK))?.unwrap();
        let work=work_snapshot::prepare_work_domain_snapshot_tx(tx,&scope(),&work_snapshot::WorkDomainLoadRequest {
            capture:crate::runtime::resident_work::WorkCaptureRequest::current(),record_limit:load.record_limit,byte_limit:load.byte_limit,
        })?.require_current()?;
        let before:(u64,u64)=tx.query_row("SELECT (SELECT COUNT(*) FROM input_evidence_current WHERE family IN ('turn','brief')),(SELECT COUNT(*) FROM task_domain_records)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        let candidate=target();
        let (source_proof,source_material)=crate::runtime::AcceptedTaskResultSource::prepare_read_fixture(&scope(),source,&candidate,ledger,load.record_limit,load.byte_limit)?;
        let result=prepare_task_result_parent_read_tx(tx,&scope(),current.facts.state.as_ref().unwrap(),
            capture.coverage.revision,work.resident.coverage().revision,Some(fact),source,&candidate,Some(&source_proof),source_material,load,ledger)?;
        let after:(u64,u64)=tx.query_row("SELECT (SELECT COUNT(*) FROM input_evidence_current WHERE family IN ('turn','brief')),(SELECT COUNT(*) FROM task_domain_records)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(before,after,"finite route creates no Input or Task alias");
        Ok(result)
    })
}
#[test]
fn accepted_result_ack_reads_fixed_parent_then_pure_route_needs_no_task_work_turn_or_brief_sql(
) -> Result<()> {
    let (_dir, db, source) = fixture(true)?;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let read = acquire(&db, &source, &ledger, &request())?;
    db.transaction(|tx| {
        for table in ["tasks", "work_items", "turn_records", "briefs", "messages"] {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} RENAME TO forbidden_routing_{table}"
            ))?;
        }
        Ok(())
    })?;
    assert!(read.parent_delivered()?);
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn qualified_task_real_global_parent_absence_preserves_original_false() -> Result<()> {
    let (_dir, db, source) = fixture(false)?;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let read = acquire(&db, &source, &ledger, &request())?;
    assert!(!read.parent_delivered()?);
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn task_result_route_rejects_raw_late_foreign_pruned_and_missing_fixed_parent() -> Result<()> {
    for mode in ["late", "foreign", "pruned", "missing"] {
        let (_dir, db, source) = fixture(!matches!(mode, "late" | "foreign"))?;
        db.transaction(|tx| {
            if matches!(mode,"late"|"foreign") {
                let mut parent=TurnRecord::new(if mode=="foreign"{"foreign-agent"}else{"task-agent"},PARENT,1);
                parent.created_at=at();repositories::upsert_turn_record_tx(tx,&parent)?;
                if mode=="foreign" {
                    tx.execute("UPDATE turn_records SET payload_json='malformed foreign body' WHERE turn_id=?1",[PARENT])?;
                }
            } else {
                tx.execute("DELETE FROM turn_records WHERE turn_id=?1",[PARENT])?;
                if mode=="pruned" {
                    crate::runtime_db::input_evidence_snapshot::register_pruned_input_evidence_tx(tx,&scope(),&[
                        crate::runtime::resident_input_evidence::InputEvidenceKey::new(crate::runtime::resident_input_evidence::InputEvidenceFamily::Turn,PARENT)])?;
                }
            }
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        assert!(
            acquire(&db, &source, &ledger, &request()).is_err(),
            "{mode}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0, "{mode}");
    }
    Ok(())
}
#[test]
fn route_no_state_and_retired_identity_reject_before_poisoned_successor_state() -> Result<()> {
    for retired in [false, true] {
        let (_dir, db, source) = fixture(true)?;
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        // First capture the legitimate original Task/Work, then corrupt only the
        // negative fixture. The read facet must recheck its actual same-TX scope.
        db.transaction(|tx| {
            let current=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?.require_current()?;
            let capture=current.resident.capture(&request().capture)?;
            let fact=capture.lookup(&TaskKey::new(TaskFamily::Task,TASK))?.unwrap();
            let work=work_snapshot::prepare_work_domain_snapshot_tx(tx,&scope(),&work_snapshot::WorkDomainLoadRequest {
                capture:crate::runtime::resident_work::WorkCaptureRequest::current(),record_limit:4096,byte_limit:4*1024*1024,
            })?.require_current()?;
            if retired {
                let mut next=identity();next.incarnation=2;
                tx.execute("UPDATE agent_identities SET payload_json=?1 WHERE agent_id='task-agent'",[serde_json::to_string(&next)?])?;
                tx.execute("UPDATE agent_states SET payload_json='poisoned successor State' WHERE agent_id='task-agent'",[])?;
            } else {tx.execute("DELETE FROM agent_states WHERE agent_id='task-agent'",[])?;}
            let error=prepare_task_result_parent_read_tx(tx,&scope(),current.facts.state.as_ref().unwrap(),capture.coverage.revision,
                work.resident.coverage().revision,Some(fact),&source,&target(),None,(0,0),&request(),&ledger).err().context("route unexpectedly read missing/successor State")?;
            assert!(!format!("{error:#}").contains("poisoned successor"));Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[test]
fn accepted_parent_material_is_bounded_before_bad_body_and_guards_release() -> Result<()> {
    let (_dir, db, source) = fixture(true)?;
    // Fixed fingerprints remain intact; the canonical body is deliberately
    // damaged and oversized to prove ResourceExhausted precedes decoding it.
    db.transaction(|tx| {
        tx.execute(
            "UPDATE turn_records SET payload_json=?1 WHERE turn_id=?2",
            params!["x".repeat(128 * 1024), PARENT],
        )?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let mut small = request();
    small.byte_limit = 256 * 1024;
    let error = acquire(&db, &source, &ledger, &small)
        .err()
        .context("oversized parent was decoded")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn qualified_large_parent_collections_hit_native_record_budget_before_serde() -> Result<()> {
    let (_dir, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &target())?;
        let mut parent = TurnRecord::new("task-agent", PARENT, 1);
        parent.created_at = at();
        parent.produced_brief_ids = (0..5000)
            .map(|n| format!("original-produced-{n}"))
            .collect();
        repositories::upsert_turn_record_tx(tx, &parent)?;
        Ok(())
    })?;
    let source = db
        .messages()
        .append_with_index_changes_in_scope(&scope(), &source(), &[])?
        .record;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let error = acquire(&db, &source, &ledger, &request())
        .err()
        .context("large qualified parent unexpectedly decoded")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_qualified_task_changed_to_foreign_rejects_before_malformed_body_decode() -> Result<()> {
    let (_dir, db, source) = fixture(true)?;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let load=request();
        let current=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&load,&ledger)?.require_current()?;
        let capture=current.resident.capture(&load.capture)?;
        let fact=capture.lookup(&TaskKey::new(TaskFamily::Task,TASK))?.unwrap();
        let work=work_snapshot::prepare_work_domain_snapshot_tx(tx,&scope(),&work_snapshot::WorkDomainLoadRequest {
            capture:crate::runtime::resident_work::WorkCaptureRequest::current(),record_limit:4096,byte_limit:4*1024*1024,
        })?.require_current()?;
        tx.execute("UPDATE tasks SET owner_agent_id='foreign-agent',payload_json='malformed foreign body' WHERE task_id=?1",[TASK])?;
        let error=prepare_task_result_parent_read_tx(tx,&scope(),current.facts.state.as_ref().unwrap(),capture.coverage.revision,
            work.resident.coverage().revision,Some(fact),&source,&target(),None,(0,0),&load,&ledger)
            .err().context("foreign Task was adopted")?;
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ForeignRecord(_))),"{error:#}");Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn readonly_task_absence_rejects_unavailable_and_fixed_query_only_addresses() -> Result<()> {
    for fixed in [false, true] {
        let (_dir, db) = legacy(true, |tx| {
            if fixed {
                repositories::upsert_task_tx(tx, &target())?;
            }
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        db.transaction(|tx| {
            let load=request();
            let current=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&load,&ledger)?.require_current()?;
            let key=TaskKey::new(TaskFamily::Task,TASK);
            if fixed {
                // Deliberately restored an incomplete canonical/current subset.
                // The immutable first-cutover query inventory still knows it.
                tx.execute("DELETE FROM tasks WHERE task_id=?1",[TASK])?;
                tx.execute("DELETE FROM task_domain_records WHERE family='task' AND record_id=?1",[TASK])?;
                tx.execute("DELETE FROM task_domain_eligibility WHERE family='task' AND record_id=?1",[TASK])?;
                assert!(tx.query_row("SELECT EXISTS(SELECT 1 FROM task_query_fixed_tasks WHERE task_id=?1)",[TASK],|r|r.get::<_,bool>(0))?);
            } else {
                // Explicit restore-negative: an unavailable fixed address is
                // known even though it never granted a Task body.
                tx.execute("INSERT INTO task_domain_eligibility(seed_id,capture_phase,family,record_id,availability,content_digest) SELECT seed_id,capture_phase,'task',?1,'unavailable',NULL FROM task_domain_captures LIMIT 1",[TASK])?;
                assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_domain_eligibility WHERE family='task' AND record_id=?1 AND availability='unavailable'",[TASK],|r|r.get::<_,u64>(0))?,1);
            }
            assert!(qualification::known_anywhere(tx,&key)?);
            let error=super::qualified_input_read::prepare_task_input_read_base_tx(tx,&scope(),current.facts.state.as_ref().unwrap(),
                current.resident.coverage().revision,&key,None,None,&load,&ledger).err().context("known Task address became global absence")?;
            assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::NeedsPreparation(actual)) if actual==&key),"{error:#}");
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn task_result_parent_subtracts_selected_overlap_from_original_hard_cap_first() -> Result<()> {
    let (_dir, db, source) = fixture(true)?;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let mut load = request();
        load.record_limit = 80000;
        load.byte_limit = 80 * 1024 * 1024;
        let current = prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &load, &ledger)?
            .require_current()?;
        let captured = current.resident.capture(&load.capture)?;
        let task = captured
            .lookup(&TaskKey::new(TaskFamily::Task, TASK))?
            .unwrap();
        let work = work_snapshot::prepare_work_domain_snapshot_tx(
            tx,
            &scope(),
            &work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 4 * 1024 * 1024,
            },
        )?
        .require_current()?;
        // The larger configured request never expands the R1 phase hard cap.
        // Under the old subtract-then-clamp path this left nearly16MiB and read.
        let error = prepare_task_result_parent_read_tx(
            tx,
            &scope(),
            current.facts.state.as_ref().unwrap(),
            captured.coverage.revision,
            work.resident.coverage().revision,
            Some(task),
            &source,
            &target(),
            None,
            (64 * 1024 * 1024 + 1, 1),
            &load,
            &ledger,
        )
        .err()
        .context("selected overlap bypassed the original Task hard cap")?;
        assert!(
            matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn accepted_source_incoming_parent_preserves_suppression_when_existing_task_parent_differs(
) -> Result<()> {
    let (_dir, db) = legacy(true, |tx| {
        let mut existing = target();
        existing.detail = Some(serde_json::json!({"parent_turn_id":"older-task-parent"}));
        repositories::upsert_task_tx(tx, &existing)?;
        let mut parent = TurnRecord::new("task-agent", PARENT, 1);
        parent.created_at = at();
        parent.produced_brief_ids = vec![BRIEF.into()];
        repositories::upsert_turn_record_tx(tx, &parent)?;
        let mut brief =
            crate::brief::make_task_result("task-agent", TASK, "original delivered parent");
        brief.id = BRIEF.into();
        brief.turn_id = Some(PARENT.into());
        brief.created_at = at();
        evidence::insert_brief_evidence_tx(tx, &brief)?;
        Ok(())
    })?;
    let message = db
        .messages()
        .append_with_index_changes_in_scope(&scope(), &source(), &[])?
        .record;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let read = acquire(&db, &message, &ledger, &request())?;
    assert!(
        read.parent_delivered()?,
        "original accepted incoming parent must retain suppression semantics"
    );
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
