use super::*;
use crate::runtime_db::input_evidence_snapshot::{
    prepare_input_evidence_snapshot_tx, InputEvidenceBootstrapSnapshot, InputEvidenceLoadRequest,
};

fn input_request(heads: &[&str]) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
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

#[test]
fn two_fixed_heads_can_materialize_aliases_in_order_without_adopting_new_sources() -> Result<()> {
    let (_dir, db) = legacy(|tx| {
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
        let request = input_request(&["a-head", "z-head"]);
        let resident = input_resident(tx, &request)?;
        let captured = resident.capture_for_request(&request)?;
        assert_eq!(captured.records.len(), 2);
        assert_eq!(captured.completion_source_observations.len(), 2);
        Ok(())
    })?;
    Ok(())
}
