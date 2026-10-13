use super::*;
use crate::runtime_db::prepared_input_evidence::{PreparedEvidenceValue, PreparedInputEvidence};
use crate::runtime_db::transitions::InputMutationExecution;

fn append_result(
    db: &RuntimeDb,
    record: BriefRecord,
    request: &WorkDomainLoadRequest,
) -> Result<crate::runtime_db::input_evidence_writes::InputEvidenceCommit<PreparedEvidenceValue>> {
    let event = crate::types::brief_created_event_for(&record)?;
    let prepared = PreparedInputEvidence::brief_created(
        scope(),
        event.id.clone(),
        record.created_at,
        record,
        event,
        Vec::new(),
    )?;
    let InputMutationExecution::Committed { value, .. } = db
        .evidence()
        .commit_prepared_input_evidence_with_work_request(&prepared, request)?
    else {
        panic!("new result expected");
    };
    Ok(value)
}

#[test]
fn standalone_result_and_source_arrival_keep_history_bodies_and_refresh_one_root() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("source-owner", WorkItemState::Open))?;
        let mut history = work("large-optional-history", WorkItemState::Completed);
        history.objective = "h".repeat(16 * 1024);
        insert_qualified_work(tx, &history)?;
        Ok(())
    })?;
    let before = db
        .transaction(|tx| {
            load_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let mut policy = request();
    policy.byte_limit = 4096;
    let mut result = brief("standalone-head", "source-owner");
    result.content_source = BriefContentSource::TranscriptEntry {
        entry_id: "arriving-transcript".into(),
        relation: BriefContentSourceRelation::Finalizes,
    };
    let ack = append_result(&db, result.clone(), &policy)?;
    let first =
        before.with_committed(ack.work_delta.as_ref().context("metadata delta missing")?)?;
    assert!(first.coverage().latest_results["source-owner"]
        .as_ref()
        .unwrap()
        .transcript_entry_id
        .is_none());
    let mut source = TranscriptEntry::new(
        "agent-a",
        TranscriptEntryKind::AssistantRound,
        None,
        None,
        serde_json::json!({"text":"actual arrived source"}),
    );
    source.id = "arriving-transcript".into();
    let prepared =
        PreparedInputEvidence::transcript(scope(), "source-arrival".into(), time(), source)?;
    let InputMutationExecution::Committed { value, .. } = db
        .evidence()
        .commit_prepared_input_evidence_with_work_request(&prepared, &policy)?
    else {
        panic!("new source expected");
    };
    let second = first.with_committed(value.work_delta.as_ref().unwrap())?;
    assert_eq!(
        second.coverage().latest_results["source-owner"]
            .as_ref()
            .unwrap()
            .transcript_entry_id
            .as_deref(),
        Some("arriving-transcript")
    );
    assert!(first.coverage().latest_results["source-owner"]
        .as_ref()
        .unwrap()
        .transcript_entry_id
        .is_none());
    assert_eq!(second.coverage().nonopen, before.coverage().nonopen);
    let old = before.capture(&WorkCaptureRequest::current())?;
    let new = second.capture(&WorkCaptureRequest::current())?;
    let key = WorkKey::new(WorkFamily::Work, "large-optional-history");
    assert!(Arc::ptr_eq(
        old.lookup(&key)?.unwrap(),
        new.lookup(&key)?.unwrap()
    ));
    assert!(ack.work_delta.unwrap().records.is_empty());
    assert!(value.work_delta.unwrap().records.is_empty());
    Ok(())
}

#[test]
fn standalone_result_outside_coverage_updates_by_key_without_expanding_history() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("outside-owner", WorkItemState::Completed))?;
        for index in 0..129 {
            let mut newer = work(&format!("newer-{index}"), WorkItemState::Completed);
            newer.updated_at += chrono::Duration::seconds(1);
            insert_qualified_work(tx, &newer)?;
        }
        Ok(())
    })?;
    let before = db
        .transaction(|tx| {
            load_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    assert!(!before
        .coverage()
        .latest_results
        .contains_key("outside-owner"));
    let ack = append_result(
        &db,
        brief("outside-standalone-head", "outside-owner"),
        &request(),
    )?;
    let next = before.with_committed(ack.work_delta.as_ref().unwrap())?;
    assert_eq!(next.coverage(), before.coverage());
    let mut selected = request();
    selected
        .capture
        .latest_result_work_ids
        .insert("outside-owner".into());
    let prepared = db.transaction(|tx| {
        prepare_work_domain_snapshot_tx(tx, &scope(), &selected)?.require_current()
    })?;
    assert_eq!(
        prepared.resident.coverage().latest_results["outside-owner"]
            .as_ref()
            .unwrap()
            .brief_id,
        "outside-standalone-head"
    );
    Ok(())
}

#[test]
fn original_retention_keeps_work_history_and_protected_head_while_pruning_actual_inputs(
) -> Result<()> {
    use crate::runtime_db::prepared_input_evidence::{
        PreparedExternalInputEvidence, PreparedExternalInputValue, PreparedInputRetention,
    };
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("retained-owner", WorkItemState::Completed))?;
        let mut history = work("retention-large-history", WorkItemState::Completed);
        history.objective = "h".repeat(16 * 1024);
        insert_qualified_work(tx, &history)?;
        Ok(())
    })?;
    for (id, created_at) in [
        ("retention-protected-source", time()),
        ("retention-old-unrelated", time()),
        (
            "retention-fresh-unrelated",
            time() + chrono::Duration::days(4),
        ),
    ] {
        let mut source = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::AssistantRound,
            None,
            None,
            serde_json::json!({"text":id}),
        );
        source.id = id.into();
        source.created_at = created_at;
        let prepared =
            PreparedInputEvidence::transcript(scope(), format!("arrival:{id}"), time(), source)?;
        db.evidence()
            .commit_prepared_input_evidence_with_work_request(&prepared, &request())?;
    }
    let mut result = brief("retention-head", "retained-owner");
    result.content_source = BriefContentSource::TranscriptEntry {
        entry_id: "retention-protected-source".into(),
        relation: BriefContentSourceRelation::Finalizes,
    };
    append_result(&db, result, &request())?;
    let before = db
        .transaction(|tx| {
            load_work_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let policy = crate::runtime_db::RuntimeDbRetentionPolicy {
        enabled: true,
        transcript_entries_days: 1,
        transcript_entries_min_rows: 1,
        ..Default::default()
    };
    let now = time() + chrono::Duration::days(4);
    let scopes = db.retention_input_scopes(policy.clone(), now)?;
    assert_eq!(scopes, vec![scope()]);
    let prepared = PreparedExternalInputEvidence::Retention(PreparedInputRetention::new(
        scopes,
        "original-work-retention".into(),
        now,
        policy,
        now,
    )?);
    let mut bounded = request();
    bounded.byte_limit = 4096;
    let InputMutationExecution::Committed {
        value: PreparedExternalInputValue::Retention(ack),
        receipt,
    } = db.commit_prepared_external_input_evidence_with_work_request(&prepared, &bounded)?
    else {
        panic!("first actual retention ACK missing");
    };
    assert_eq!(ack.committed.removed_transcript_entries.len(), 1);
    assert_eq!(
        ack.committed.removed_transcript_entries[0].evidence_id,
        "retention-old-unrelated"
    );
    assert_eq!(ack.input_deltas.len(), 1);
    assert_eq!(ack.work_deltas.len(), 1);
    let after = before.with_committed(&ack.work_deltas[0].1)?;
    assert_eq!(before.coverage(), after.coverage());
    assert!(ack.work_deltas[0].1.records.is_empty());
    let old = before.capture(&WorkCaptureRequest::current())?;
    let new = after.capture(&WorkCaptureRequest::current())?;
    let key = WorkKey::new(WorkFamily::Work, "retention-large-history");
    assert!(Arc::ptr_eq(
        old.lookup(&key)?.unwrap(),
        new.lookup(&key)?.unwrap()
    ));
    assert_eq!(
        new.latest_results["retained-owner"]
            .as_ref()
            .unwrap()
            .transcript_entry_id
            .as_deref(),
        Some("retention-protected-source")
    );
    let InputMutationExecution::Replayed { receipt: replay } =
        db.commit_prepared_external_input_evidence_with_work_request(&prepared, &bounded)?
    else {
        panic!("retention must resolve its exact receipt after inventory is empty");
    };
    assert_eq!(receipt, replay);
    Ok(())
}

#[test]
fn standalone_result_without_work_coverage_keeps_the_scope_explicitly_unprepared() -> Result<()> {
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(
            tx,
            &work("fixed-unprepared-owner", WorkItemState::Completed),
        )
    })?;
    assert!(db
        .transaction(|tx| loader::current_coverage(tx, &scope()))?
        .is_none());
    let ack = append_result(
        &db,
        brief("unprepared-head", "fixed-unprepared-owner"),
        &request(),
    )?;
    assert!(ack.work_delta.is_none());
    assert!(db
        .transaction(|tx| loader::current_coverage(tx, &scope()))?
        .is_none());
    let mut selected = request();
    selected
        .capture
        .latest_result_work_ids
        .insert("fixed-unprepared-owner".into());
    let prepared = db.transaction(|tx| {
        prepare_work_domain_snapshot_tx(tx, &scope(), &selected)?.require_current()
    })?;
    assert_eq!(
        prepared.resident.coverage().latest_results["fixed-unprepared-owner"]
            .as_ref()
            .unwrap()
            .brief_id,
        "unprepared-head"
    );
    Ok(())
}

#[test]
fn standalone_result_head_and_qualification_roll_back_with_whole_receipt_failure() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("rollback-owner", WorkItemState::Open)).map(|_| ())
    })?;
    let before = db.transaction(|tx| loader::current_coverage(tx, &scope()))?;
    db.transaction(|tx| {
        tx.execute_batch("CREATE TRIGGER reject_evidence_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'whole receipt failure'); END;")?;
        Ok(())
    })?;
    let error = append_result(
        &db,
        brief("rolled-back-result", "rollback-owner"),
        &request(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("whole receipt failure"));
    assert_eq!(
        db.transaction(|tx| loader::current_coverage(tx, &scope()))?,
        before
    );
    for (table, id_column, id) in [
        ("briefs", "evidence_id", "rolled-back-result"),
        ("input_evidence_current", "record_id", "rolled-back-result"),
    ] {
        let count: i64 = db.connection()?.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE {id_column}=?1"),
            [id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
    }
    Ok(())
}
