use super::*;
use crate::runtime_db::input_evidence_snapshot::{
    load_or_plan_input_qualification_with_scope_read_tx, prepare_input_evidence_snapshot_tx,
    qualify_planned_input_tx, InputEvidenceBootstrapSnapshot, InputEvidenceLoadRequest,
    InputQualificationPlan,
};

fn input_request(heads: &[&str]) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: heads.iter().map(|id| (*id).into()).collect(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    }
}
fn input_resident(
    tx: &Transaction<'_>,
    request: &InputEvidenceLoadRequest,
) -> Result<crate::runtime::resident_input_evidence::ResidentInputEvidence> {
    match prepare_input_evidence_snapshot_tx(tx, &scope(), request)? {
        InputEvidenceBootstrapSnapshot::Current(snapshot) => Ok(snapshot.resident),
        _ => anyhow::bail!("fixture unexpectedly retired"),
    }
}
fn result_with_source(id: &str, owner: &str, source: &str) -> BriefRecord {
    let mut head = brief(id, owner);
    head.content_source = BriefContentSource::TranscriptEntry {
        entry_id: source.into(),
        relation: BriefContentSourceRelation::DerivedFrom,
    };
    head
}

#[test]
fn completion_existing_source_reuses_scope_without_state_redecode() -> Result<()> {
    use crate::{
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("scope-work", WorkItemState::Completed))?;
        let mut source = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({"text":"qualified source"}),
        );
        source.id = "scope-source".into();
        let (source, _) = append_transcript_entry_tx(tx, &source)?;
        register_committed_input_evidence_tx(
            tx, &scope(), &[CanonicalInputEvidence::Transcript(source.clone())],
        )?;
        let head = result_with_source("scope-head", "scope-work", &source.id);
        write_brief(tx, &head)?;
        register_work_result_writes_tx(
            tx, &scope(), &BTreeSet::from([head.id.clone()]), &request(),
        )?;
        let scope = scope();
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, header, 64)?;
        let selected = BTreeSet::from([head.id.clone()]);
        let key = InputEvidenceKey::new(InputEvidenceFamily::Brief, &head.id);
        {
            let _phase = material.enter();
            assert!(completion_source_observations_with_scope_tx(
                tx, &scope, &selected, &key, Some(&read),
            )?.is_empty());
        }
        assert_eq!(material.finish()?, (header, 64));
        tx.execute(
            "UPDATE transcript_entries SET payload_json=json_set(payload_json,'$.data.text','raw replacement') WHERE evidence_id=?1",
            [&source.id],
        )?;
        assert!(completion_source_observations_with_scope_tx(
            tx, &scope, &selected, &key, Some(&read),
        ).is_err());
        tx.execute(
            "UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',17) WHERE agent_id=?1",
            [&scope.agent_id],
        )?;
        let error = completion_source_observations_with_scope_tx(
            tx, &scope, &selected, &key, Some(&read),
        ).unwrap_err();
        assert!(error.to_string().contains("context scope changed"));
        Ok(())
    })
}

#[test]
fn input_loader_mints_missing_transcript_proof_without_a_fake_unavailable_record() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("completion-work", WorkItemState::Completed))?;
        let head = result_with_source("completion-head", "completion-work", "missing-source");
        write_brief(tx, &head)?;
        register_work_result_writes_tx(
            tx,
            &scope(),
            &BTreeSet::from([head.id.clone()]),
            &request(),
        )?;
        let request = input_request(&[&head.id]);
        let resident = input_resident(tx, &request)?;
        let captured = resident.capture_for_request(&request)?;
        assert_eq!(captured.records.len(), 1);
        assert_eq!(captured.completion_source_observations.len(), 1);
        let target = InputEvidenceKey::new(InputEvidenceFamily::Transcript, "missing-source");
        assert!(!captured.unavailable.contains(&target));
        assert!(captured.lookup(&target).is_err());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn input_loader_keeps_an_independent_mandatory_path_to_the_same_head_strict() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("strict-work", WorkItemState::Completed))?;
        let head = result_with_source("strict-head", "strict-work", "missing-source");
        write_brief(tx, &head)?;
        register_work_result_writes_tx(
            tx,
            &scope(),
            &BTreeSet::from([head.id.clone()]),
            &request(),
        )?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            let mut request = input_request(&["strict-head"]);
            request.mandatory_roots.insert(InputEvidenceKey::new(
                InputEvidenceFamily::Brief,
                "strict-head",
            ));
            input_resident(tx, &request)
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("prepar"));
    Ok(())
}

#[test]
fn input_loader_uses_the_incoming_transcript_edge_for_missing_message_only() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        insert_qualified_work(tx, &work("incoming-work", WorkItemState::Completed))?;
        let mut source = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some("missing-message".into()),
            serde_json::json!({"text":"original source"}),
        );
        source.id = "incoming-source".into();
        let (source, _) = append_transcript_entry_tx(tx, &source)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Transcript(source.clone())],
        )?;
        let head = result_with_source("incoming-head", "incoming-work", &source.id);
        write_brief(tx, &head)?;
        register_work_result_writes_tx(
            tx,
            &scope(),
            &BTreeSet::from([head.id.clone()]),
            &request(),
        )?;
        let request = input_request(&[&head.id]);
        let resident = input_resident(tx, &request)?;
        let captured = resident.capture_for_request(&request)?;
        assert_eq!(captured.records.len(), 2);
        assert_eq!(captured.completion_source_observations.len(), 1);
        assert_eq!(
            captured.completion_source_observations[0].edge,
            crate::runtime::completion_sources::CompletionSourceEdge::IncomingTranscriptMessage
        );
        assert!(captured
            .lookup(&InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                "missing-message"
            ))
            .is_err());
        Ok(())
    })?;
    Ok(())
}

fn two_fixed_heads() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let (dir, db) = legacy(|tx| {
        for (id, owner, source) in [
            ("a-head", "a-work", "a-source"),
            ("z-head", "z-work", "z-source"),
        ] {
            raw_legacy_work(tx, &work(owner, WorkItemState::Completed))?;
            insert_brief_evidence_tx(tx, &result_with_source(id, owner, source))?;
        }
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut policy = request();
        policy
            .capture
            .latest_result_work_ids
            .extend(["a-work".into(), "z-work".into()]);
        prepare_work_domain_snapshot_tx(tx, &scope(), &policy)?.require_current()?;
        Ok(())
    })?;
    Ok((dir, db))
}

#[test]
fn two_fixed_heads_can_materialize_aliases_in_order_without_adopting_new_sources() -> Result<()> {
    let (_dir, db) = two_fixed_heads()?;
    db.transaction(|tx| {
        let request = input_request(&["a-head", "z-head"]);
        let resident = input_resident(tx, &request)?;
        let captured = resident.capture_for_request(&request)?;
        assert_eq!(captured.records.len(), 2);
        assert_eq!(captured.completion_source_observations.len(), 2);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn read_only_qualification_batches_fixed_completion_heads() -> Result<()> {
    let (_dir, db) = two_fixed_heads()?;
    let scope = scope();
    let request = input_request(&["a-head", "z-head"]);
    let read = || {
        db.read_for_preparation(|tx| {
            let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
            load_or_plan_input_qualification_with_scope_read_tx(
                tx,
                &scope,
                request.borrowed(),
                false,
                read,
            )
        })
    };
    let error = read().err().context("fixed heads need qualification")?;
    let plan = error
        .downcast_ref::<InputQualificationPlan>()
        .with_context(|| format!("missing qualification plan: {error:#}"))?;
    let expected = ["a-head", "z-head"]
        .into_iter()
        .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Brief, id))
        .collect::<BTreeSet<_>>();
    assert_eq!(plan.keys(), &expected);
    db.transaction(|tx| qualify_planned_input_tx(tx, &scope, plan))?;
    let InputEvidenceBootstrapSnapshot::Current(snapshot) = read()? else {
        anyhow::bail!("fixture unexpectedly retired");
    };
    let captured = snapshot.resident.capture_for_request(&request)?;
    assert_eq!(captured.records.len(), 2);
    assert_eq!(captured.completion_source_observations.len(), 2);
    Ok(())
}

#[test]
fn fixed_linkage_completion_keeps_hash_and_qualification_unchanged() -> Result<()> {
    use crate::runtime_db::evidence::append_audit_event_tx;
    let mut head = result_with_source("fixed-link-head", "fixed-link-work", "absent-source");
    let (_dir, db) = legacy(|tx| {
        raw_legacy_work(tx, &work("fixed-link-work", WorkItemState::Completed))?;
        insert_brief_evidence_tx(tx, &head)?;
        tx.execute("INSERT OR IGNORE INTO runtime_metadata(key,value,created_at,updated_at) VALUES('event_log_epoch','fixture-epoch',?1,?1)", [time().to_rfc3339()])?;
        let event = crate::types::brief_created_event_for(&head)?;
        let (event, _) = append_audit_event_tx(tx, Some("agent-a"), &event)?;
        head.created_event_seq = Some(event.event_seq);
        tx.execute(
            "UPDATE briefs SET created_event_seq=?1,payload_json=?2 WHERE evidence_id=?3",
            params![
                i64::try_from(event.event_seq)?,
                serde_json::to_string(&head)?,
                head.id
            ],
        )?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut policy=request();
        policy.capture.latest_result_work_ids.insert("fixed-link-work".into());
        prepare_work_domain_snapshot_tx(tx, &scope(), &policy)?.require_current()?;
        let before:(String,String)=tx.query_row("SELECT content_hash,payload_json FROM briefs WHERE evidence_id=?1",[&head.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_ne!(before.0, crate::runtime_db::evidence::content_hash(&before.1));
        let captured=input_resident(tx,&input_request(&[&head.id]))?.capture()?;
        assert_eq!(captured.completion_source_observations.len(),1);
        let before_alias:(i64,String,String)=tx.query_row("SELECT revision,content_digest,fingerprint_json FROM input_evidence_current WHERE family='brief' AND record_id=?1",[&head.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let selected=BTreeSet::from([head.id.clone()]);
        let key=InputEvidenceKey::new(InputEvidenceFamily::Brief,&head.id);
        for _ in 0..2 {
            assert_eq!(completion_source_observations_tx(tx,&scope(),&selected,&key)?.len(),1);
        }
        let after:(String,String)=tx.query_row("SELECT content_hash,payload_json FROM briefs WHERE evidence_id=?1",[&head.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let after_alias:(i64,String,String)=tx.query_row("SELECT revision,content_digest,fingerprint_json FROM input_evidence_current WHERE family='brief' AND record_id=?1",[&head.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(before,after); assert_eq!(before_alias,after_alias);
        tx.execute("DELETE FROM audit_events WHERE agent_id='agent-a' AND event_seq=?1", [i64::try_from(head.created_event_seq.unwrap())?])?;
        assert!(completion_source_observations_tx(tx,&scope(),&selected,&key).is_err());
        Ok(())
    })
}
