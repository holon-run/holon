use super::*;
use crate::{
    runtime::resident_work::{WorkCaptureRequest, WorkFamily, WorkKey},
    runtime_db::work_snapshot::{self, WorkDomainLoadRequest},
};

fn work_request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    }
}
fn original_work(tx: &Transaction<'_>) -> Result<WorkItemRecord> {
    let ready: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_domain_current')", [], |row| row.get(0))?;
    if !ready {
        tx.execute_batch(work_snapshot::WORK_DOMAIN_SCHEMA)?;
    }
    work_snapshot::capture_work_domain_upgrade_tx(tx)?;
    work_snapshot::prepare_work_domain_snapshot_tx(tx, &scope(), &work_request())?
        .require_current()?;
    let mut work = WorkItemRecord::new("agent-a", "original work", WorkItemState::Open);
    work.id = "original-work".into();
    let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &work.id)]);
    let proof = work_snapshot::prepare_work_write_tx(tx, &scope(), &keys)?;
    crate::runtime_db::repositories::insert_new_work_item_tx(tx, &work)?;
    let state = ContextDurableRead::new(tx, "agent-a")
        .scope_facts()?
        .state
        .context("fixture has no canonical State")?;
    work_snapshot::register_work_writes_tx(
        tx,
        &proof,
        &keys,
        &BTreeSet::new(),
        &state,
        &work_request(),
    )?;
    let mut deleting = identity();
    deleting.status = AgentRegistryStatus::Deleting;
    upsert_agent_identity_tx(tx, &deleting)?;
    Ok(work)
}
fn head(id: &str, work_id: &str) -> BriefRecord {
    let mut brief = BriefRecord::new(
        "agent-a",
        crate::types::BriefKind::Result,
        "finished",
        None,
        None,
    );
    brief.id = id.into();
    brief.work_item_id = Some(work_id.into());
    brief
}
fn original_proof(tx: &Transaction<'_>) -> Result<work_snapshot::WorkWriteProof> {
    work_snapshot::prepare_original_work_write_tx(
        tx,
        &scope(),
        &BTreeSet::from([WorkKey::new(WorkFamily::Work, "original-work")]),
    )
}
#[test]
fn deleting_no_turn_cleanup_derives_only_original_work_brief_and_tool() {
    let (_dir, db) = fresh().unwrap();
    db.transaction(|tx| {
        original_work(tx)?;
        let work = original_proof(tx)?;
        let brief = head("work-result", "original-work");
        let mut tool = tool();
        tool.output = serde_json::json!({"result":{"exit_status":0}});
        tool.work_item_id = Some("original-work".into());
        let derived = BTreeMap::from([
            (Key::new(Family::Brief, &brief.id), "original-work".into()),
            (
                Key::new(Family::ToolExecution, &tool.id),
                "original-work".into(),
            ),
        ]);
        let proof =
            ExistingAdmissionInputProof::prepare_from_work_tx(tx, &work, BTreeSet::new(), derived)?;
        crate::runtime_db::evidence::insert_brief_evidence_tx(tx, &brief)?;
        insert_tool_evidence_tx(tx, &tool)?;
        let delta = register_committed_cleanup_input_evidence_tx(
            tx,
            &scope(),
            &[
                CanonicalInputEvidence::Brief(brief),
                CanonicalInputEvidence::ToolExecution(tool),
            ],
            &proof,
        )?;
        assert_eq!(delta.records.len(), 2);
        assert!(delta.records.iter().all(|record| matches!(
            record.record,
            PreparedInputEvidence::Brief(_) | PreparedInputEvidence::ToolExecution(_)
        )));
        let turns: u64 = tx.query_row("SELECT COUNT(*) FROM turn_records", [], |row| row.get(0))?;
        assert_eq!(turns, 0, "Work cleanup never fabricates a Turn admission");
        Ok(())
    })
    .unwrap();
}
#[test]
fn derived_actual_work_mismatch_rolls_back_body_and_qualification() {
    let (_dir, db) = fresh().unwrap();
    db.transaction(|tx| {
        original_work(tx)?;
        Ok(())
    })
    .unwrap();
    let result = db.transaction(|tx| {
        let work = original_proof(tx)?;
        let proof = ExistingAdmissionInputProof::prepare_from_work_tx(
            tx,
            &work,
            BTreeSet::new(),
            BTreeMap::from([(
                Key::new(Family::Brief, "wrong-owner"),
                "original-work".into(),
            )]),
        )?;
        let brief = head("wrong-owner", "another-work");
        crate::runtime_db::evidence::insert_brief_evidence_tx(tx, &brief)?;
        register_committed_cleanup_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Brief(brief)],
            &proof,
        )?;
        Ok(())
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("original Work admission"));
    db.transaction(|tx| {
        let body: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM briefs WHERE evidence_id='wrong-owner')",
            [],
            |row| row.get(0),
        )?;
        let alias: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE record_id='wrong-owner')",
            [],
            |row| row.get(0),
        )?;
        assert!(!body && !alias);
        Ok(())
    })
    .unwrap();
}
#[test]
fn original_work_proof_cannot_grant_new_work_or_message_admission() {
    let (_dir, db) = fresh().unwrap();
    db.transaction(|tx| {
        original_work(tx)?;
        let work = original_proof(tx)?;
        for (family, owner) in [
            (Family::Message, "original-work"),
            (Family::Transcript, "original-work"),
            (Family::Brief, "new-work"),
        ] {
            assert!(ExistingAdmissionInputProof::prepare_from_work_tx(
                tx,
                &work,
                BTreeSet::new(),
                BTreeMap::from([(Key::new(family, "fresh-input"), owner.into())])
            )
            .is_err());
        }
        assert!(ExistingAdmissionInputProof::prepare_from_work_tx(
            tx,
            &work,
            BTreeSet::new(),
            BTreeMap::new()
        )
        .is_err());
        assert!(work_snapshot::prepare_original_work_write_tx(
            tx,
            &scope(),
            &BTreeSet::from([WorkKey::new(WorkFamily::Work, "new-work")])
        )
        .is_err());
        Ok(())
    })
    .unwrap();
}
#[test]
fn work_admission_must_be_frozen_before_business_sql_and_existing_brief_cannot_switch_owner() {
    let (_dir, db) = fresh().unwrap();
    db.transaction(|tx| {
        original_work(tx)?;
        let brief=head("prior-result","another-work");
        crate::runtime_db::evidence::insert_brief_evidence_tx(tx,&brief)?;
        let work=original_proof(tx)?;
        assert!(ExistingAdmissionInputProof::prepare_from_work_tx(tx,&work,BTreeSet::new(),BTreeMap::from([(Key::new(Family::Brief,&brief.id),"original-work".into())])).is_err());
        tx.execute("UPDATE work_items SET payload_json=json_set(payload_json,'$.objective','after business SQL'),objective='after business SQL' WHERE work_item_id='original-work'",[])?;
        let stale=ExistingAdmissionInputProof::prepare_from_work_tx(tx,&work,BTreeSet::new(),BTreeMap::from([(Key::new(Family::Brief,"new-result"),"original-work".into())]));
        assert!(stale.err().unwrap().to_string().contains("changed before"));
        Ok(())
    }).unwrap();
}

#[test]
fn retrying_original_work_tool_checks_its_owner_with_large_output_and_rejects_a_rebind() {
    let (_dir, db) = fresh().unwrap();
    db.transaction(|tx| {
        original_work(tx)?;
        let work = original_proof(tx)?;
        let mut tool = tool();
        tool.work_item_id = Some("original-work".into());
        let key = Key::new(Family::ToolExecution, &tool.id);
        let derived = BTreeMap::from([(key.clone(), "original-work".into())]);
        let proof = ExistingAdmissionInputProof::prepare_from_work_tx(tx, &work, BTreeSet::new(), derived.clone())?;
        insert_tool_evidence_tx(tx, &tool)?;
        register_committed_cleanup_input_evidence_tx(tx, &scope(), &[CanonicalInputEvidence::ToolExecution(tool)], &proof)?;
        let retry = ExistingAdmissionInputProof::prepare_from_work_tx(tx, &work, BTreeSet::new(), derived.clone())?;
        retry.validate_key_tx(tx, &key)?;
        // D3 normalized ownership remains unchanged; a divergent actual payload
        // association cannot borrow the original Work's cleanup admission.
        tx.execute("UPDATE tool_executions SET payload_json=json_set(payload_json,'$.work_item_id','another-work') WHERE evidence_id=?1", [&key.id])?;
        assert!(ExistingAdmissionInputProof::prepare_from_work_tx(tx, &work, BTreeSet::new(), derived).is_err());
        Ok(())
    }).unwrap();
}
