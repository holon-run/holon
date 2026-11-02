use super::*;
use crate::runtime::completion_sources::{
    CompletionOptionalAbsence, CompletionOptionalSourceObservation, CompletionSourceEdge,
};
use crate::types::{
    BriefContentSource, BriefContentSourceRelation, BriefKind, TranscriptEntryKind,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "completion-owner".into(),
        identity_incarnation: 4,
    }
}
fn qualify(record: PreparedInputEvidence) -> Arc<QualifiedInputEvidence> {
    Arc::new(QualifiedInputEvidence {
        locator: EvidenceLocator {
            scope: scope(),
            key: record.key(),
            revision: 3,
            content_digest: "a".repeat(64),
        },
        resident_bytes: serde_json::to_vec(&record).unwrap().len(),
        record,
        created_at: "2026-10-12T00:00:00Z".into(),
        order_sequence: None,
        turn_index: None,
        owner: None,
        attempt_turn_keys: None,
    })
}
fn head(id: &str) -> Arc<QualifiedInputEvidence> {
    let mut record = BriefRecord::new(scope().agent_id, BriefKind::Result, "done", None, None);
    record.id = id.into();
    record.content_source = BriefContentSource::TranscriptEntry {
        entry_id: "source".into(),
        relation: BriefContentSourceRelation::DerivedFrom,
    };
    qualify(PreparedInputEvidence::Brief(record))
}
fn source() -> Arc<QualifiedInputEvidence> {
    let mut record = TranscriptEntry::new(
        scope().agent_id,
        TranscriptEntryKind::IncomingMessage,
        None,
        Some("missing-message".into()),
        serde_json::json!({"text":"legacy"}),
    );
    record.id = "source".into();
    qualify(PreparedInputEvidence::Transcript(record))
}
fn coverage(
    head: &Arc<QualifiedInputEvidence>,
    source: &Arc<QualifiedInputEvidence>,
) -> InputEvidenceCoverage {
    InputEvidenceCoverage {
        selected_completion_brief_ids: BTreeSet::from([head.locator.key.id.clone()]),
        completion_source_observations: vec![CompletionOptionalSourceObservation {
            head: head.locator.clone(),
            source: source.locator.clone(),
            edge: CompletionSourceEdge::IncomingTranscriptMessage,
            declared_target: InputEvidenceKey::new(InputEvidenceFamily::Message, "missing-message"),
            availability: CompletionOptionalAbsence::GlobalAbsent,
        }],
        recovery_source_absences: Default::default(),
        replay_source_absences: Default::default(),
        recovery_lineage_messages: Default::default(),
        execution: Default::default(),
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        turn_transcript_keys: Default::default(),
        message_turn_keys: Default::default(),
        unavailable: Default::default(),
        record_limit: 32,
        byte_limit: 64 * 1024,
    }
}
fn request(head_id: &str) -> crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
    crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: BTreeSet::from([head_id.into()]),
        record_limit: 32,
        byte_limit: 64 * 1024,
    }
}
#[test]
fn completion_absence_is_private_to_its_exact_path_and_lookup_stays_unprepared() {
    let head = head("head");
    let source = source();
    let resident = ResidentInputEvidence::bootstrap(
        scope(),
        vec![head.clone(), source.clone()],
        coverage(&head, &source),
    )
    .unwrap();
    let captured = resident.capture_for_request(&request("head")).unwrap();
    assert_eq!(captured.records.len(), 2);
    assert_eq!(captured.completion_source_observations.len(), 1);
    assert!(captured.selected_completion_brief_ids.contains("head"));
    let missing = InputEvidenceKey::new(InputEvidenceFamily::Message, "missing-message");
    assert!(captured.lookup(&missing).is_err());
    assert!(!captured.unavailable.contains(&missing));
    assert!(resident
        .capture_for_request(&request("other-head"))
        .is_err());
    let mut strict = request("head");
    strict.mandatory_roots.insert(missing);
    assert!(resident.capture_for_request(&strict).is_err());
}
#[test]
fn the_selected_head_itself_stays_strict_when_independently_mandatory() {
    let head = head("head");
    let source = source();
    let mut prepared = coverage(&head, &source);
    prepared.mandatory_roots.insert(head.locator.key.clone());
    assert!(ResidentInputEvidence::bootstrap(scope(), vec![head, source], prepared).is_err());
}

#[test]
fn another_owning_brief_to_the_same_transcript_cannot_borrow_completion_absence() {
    let head = head("a-head");
    let source = source();
    let strict_owner = self::head("z-owner");
    let mut prepared = coverage(&head, &source);
    prepared
        .mandatory_roots
        .insert(strict_owner.locator.key.clone());
    assert!(
        ResidentInputEvidence::bootstrap(scope(), vec![head, source, strict_owner], prepared)
            .is_err()
    );
}
#[test]
fn ordinary_history_window_keeps_the_same_transcript_strict() {
    let head = head("head");
    let source = source();
    let mut prepared = coverage(&head, &source);
    prepared.windows.push(InputWindowCoverage {
        selector: InputWindowSelector::RecentTranscript,
        limit: 1,
        is_exhaustive: true,
        keys: vec![source.locator.key.clone()],
    });
    assert!(ResidentInputEvidence::bootstrap(scope(), vec![head, source], prepared).is_err());
}
#[test]
fn pruned_or_rewritten_source_cannot_satisfy_an_absence_observation() {
    let head = head("head");
    let source = source();
    let mut prepared = coverage(&head, &source);
    prepared.unavailable.insert(InputEvidenceKey::new(
        InputEvidenceFamily::Message,
        "missing-message",
    ));
    assert!(ResidentInputEvidence::bootstrap(
        scope(),
        vec![head.clone(), source.clone()],
        prepared
    )
    .is_err());
    let prepared = coverage(&head, &source);
    let mut rewritten = source.as_ref().clone();
    rewritten.locator.revision += 1;
    assert!(
        ResidentInputEvidence::bootstrap(scope(), vec![head, Arc::new(rewritten)], prepared)
            .is_err()
    );
}
#[test]
fn source_commit_invalidates_completion_preparation_instead_of_reusing_old_absence() {
    let head = head("head");
    let source = source();
    let resident = ResidentInputEvidence::bootstrap(
        scope(),
        vec![head.clone(), source.clone()],
        coverage(&head, &source),
    )
    .unwrap();
    let mut revised = source.as_ref().clone();
    revised.locator.revision += 1;
    revised.locator.content_digest = "b".repeat(64);
    let delta = CommittedInputEvidenceDelta {
        records: vec![Arc::new(revised)],
        ..Default::default()
    };
    assert!(resident
        .with_committed(&delta, BTreeSet::from([head.locator.key.clone()]))
        .unwrap()
        .is_none());
    assert!(
        resident.capture_for_request(&request("head")).is_ok(),
        "old capture remains coherent and immutable"
    );
}

#[test]
fn newly_committed_completion_head_reuses_its_resident_strict_closure() {
    let head = head("new-head");
    let mut entry = TranscriptEntry::new(
        scope().agent_id,
        TranscriptEntryKind::AssistantRound,
        None,
        None,
        serde_json::json!({"text": "done"}),
    );
    entry.id = "source".into();
    let source = qualify(PreparedInputEvidence::Transcript(entry));
    let mut prepared = coverage(&head, &source);
    prepared.selected_completion_brief_ids.clear();
    prepared.completion_source_observations.clear();
    prepared.mandatory_roots.insert(source.locator.key.clone());
    let original =
        ResidentInputEvidence::bootstrap(scope(), vec![source.clone()], prepared).unwrap();
    let delta = CommittedInputEvidenceDelta {
        records: vec![head.clone()],
        ..Default::default()
    };
    let mut resident = original
        .with_committed(&delta, BTreeSet::from([head.locator.key.clone()]))
        .unwrap()
        .unwrap();
    assert!(!resident
        .coverage
        .selected_completion_brief_ids
        .contains("new-head"));
    let ledger =
        InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
    let header_bytes = (scope().agent_id.len() as u64 + 256) * 8;
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope(),
        header_bytes,
        64,
    )
    .unwrap();
    let baseline = material.finish().unwrap();
    assert!(resident
        .preparation_request_is_covered(&request("new-head"), &material)
        .unwrap());
    assert_eq!(material.finish().unwrap(), baseline);
    assert!(!original
        .preparation_request_is_covered(&request("new-head"), &material)
        .unwrap());
    let mut window_coverage = resident.coverage.clone();
    window_coverage.mandatory_roots.clear();
    window_coverage.windows.push(InputWindowCoverage {
        selector: InputWindowSelector::RecentBriefs,
        limit: 1,
        is_exhaustive: true,
        keys: vec![head.locator.key.clone()],
    });
    let window_resident = ResidentInputEvidence::bootstrap(
        scope(),
        vec![head.clone(), source.clone()],
        window_coverage,
    )
    .unwrap();
    assert!(window_resident
        .preparation_request_is_covered(&request("new-head"), &material)
        .unwrap());
    assert_eq!(material.finish().unwrap(), baseline);
    // The borrowed proof does not create space for the actual owning capture.
    assert!(window_resident
        .capture_for_request_with_material(&request("new-head"), &material)
        .is_err());
    assert!(window_resident
        .preparation_request_is_covered(&request("new-head"), &material)
        .is_err());
    let captured = resident.capture_for_request(&request("new-head")).unwrap();
    assert!(captured
        .records
        .iter()
        .any(|record| Arc::ptr_eq(record, &head)));
    assert!(captured
        .records
        .iter()
        .any(|record| Arc::ptr_eq(record, &source)));
    assert!(captured.completion_source_observations.is_empty());
    assert!(original.capture_for_request(&request("new-head")).is_err());

    // Even a resident head cannot hide a missing strict descendant.
    resident.records.remove(&source.locator.key);
    let error = resident
        .capture_for_request(&request("new-head"))
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<InputEvidenceReadError>(),
        Some(&InputEvidenceReadError::NeedsPreparation(
            source.locator.key.clone()
        ))
    );
}

#[test]
fn newly_selected_head_cannot_borrow_another_heads_source_absence() {
    let head = head("old-head");
    let source = source();
    let mut resident = ResidentInputEvidence::bootstrap(
        scope(),
        vec![head.clone(), source.clone()],
        coverage(&head, &source),
    )
    .unwrap();
    let other = self::head("new-head");
    // An extra candidate is not an owning path or an absence observation.
    resident.records.insert(other.locator.key.clone(), other);
    let ledger =
        InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
    let material =
        crate::diagnostics::resource_io::ResourceReadMaterial::new(&ledger, &scope()).unwrap();
    assert!(!resident
        .preparation_request_is_covered(&request("new-head"), &material)
        .unwrap());
    let error = resident
        .capture_for_request(&request("new-head"))
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<InputEvidenceReadError>(),
        Some(&InputEvidenceReadError::NeedsPreparation(
            InputEvidenceKey::new(InputEvidenceFamily::Message, "missing-message")
        ))
    );
    assert!(resident.capture_for_request(&request("old-head")).is_ok());
}

#[test]
fn current_budget_counts_completion_bodies_after_request_pins_are_released() {
    let head = head("head");
    let source = source();
    let bytes = (head.resident_bytes + source.resident_bytes) as u64;
    let weak_head = Arc::downgrade(&head);
    let weak_source = Arc::downgrade(&source);
    let prepared = coverage(&head, &source);
    let budget =
        InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
    let resident = ResidentInputEvidence::bootstrap(scope(), vec![head, source], prepared)
        .unwrap()
        .with_budget(budget.clone())
        .unwrap();
    let before = budget.report().unwrap();
    assert_eq!(before.physical_records.allocations, 2);
    assert_eq!(before.physical_records.bytes, bytes);
    assert!(before.categories[&InputMemoryCategory::Current].bytes >= bytes);
    let captured = resident.capture_for_request(&request("head")).unwrap();
    assert_eq!(
        budget.report().unwrap().physical_records.allocations,
        2,
        "shared current and Pins are deduplicated"
    );
    drop(captured);
    let after = budget.report().unwrap();
    assert_eq!(after.physical_records.allocations, 2);
    assert_eq!(after.physical_records.bytes, bytes);
    assert!(!after.categories.contains_key(&InputMemoryCategory::Pins));
    assert!(weak_head.upgrade().is_some() && weak_source.upgrade().is_some());
    drop(resident);
    assert!(weak_head.upgrade().is_none() && weak_source.upgrade().is_none());
    assert_eq!(budget.report().unwrap().observed_bytes, 0);
}
