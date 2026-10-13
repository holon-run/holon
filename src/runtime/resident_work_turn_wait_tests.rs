use super::*;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "turn-wait-owner".into(),
        identity_incarnation: 7,
    }
}
fn qualified(record: CanonicalWorkRecord, revision: u64) -> Arc<QualifiedWorkRecord> {
    Arc::new(QualifiedWorkRecord {
        scope: scope(),
        key: record.key(),
        revision,
        content_digest: "a".repeat(64),
        resident_bytes: serde_json::to_vec(&record).unwrap().len(),
        record,
    })
}
fn wait(id: &str, turn_id: &str, status: &str, created_millis: i64) -> Arc<QualifiedWorkRecord> {
    let at = chrono::DateTime::from_timestamp_millis(created_millis).unwrap();
    let record = serde_json::from_value(serde_json::json!({
        "id": id, "agent_id": scope().agent_id, "work_item_id": "closed-owner",
        "status": status, "kind": "operator", "waiting_for": "operator",
        "created_at": at, "updated_at": at, "turn_id": turn_id,
        "resolved_at": (status == "resolved").then_some(at),
        "cancelled_at": (status == "cancelled").then_some(at),
    }))
    .unwrap();
    qualified(CanonicalWorkRecord::Wait(record), 1)
}
fn owner() -> Arc<QualifiedWorkRecord> {
    let mut work = WorkItemRecord::new(&scope().agent_id, "closed owner", WorkItemState::Completed);
    work.id = "closed-owner".into();
    qualified(CanonicalWorkRecord::Work(work), 1)
}
fn coverage(records: &[Arc<QualifiedWorkRecord>], revision: u64) -> WorkCoverage {
    WorkCoverage {
        revision,
        live_complete: true,
        live_work_ids: BTreeSet::new(),
        unresolved_wait_ids: BTreeSet::new(),
        active_frame_ids: BTreeSet::new(),
        turn_wait_keys: BTreeMap::new(),
        task_wait_keys: BTreeMap::new(),
        trigger_wait_keys: BTreeMap::new(),
        mandatory_roots: BTreeSet::new(),
        observations: records
            .iter()
            .map(|record| (record.key.clone(), WorkAvailability::Present))
            .collect(),
        nonopen: WorkHistoryCoverage {
            limit: 0,
            keys: Vec::new(),
            is_exhaustive: false,
        },
        latest_results: records
            .iter()
            .filter_map(|record| {
                (record.key.family == WorkFamily::Work).then(|| (record.key.id.clone(), None))
            })
            .collect(),
    }
}
fn domain(
    records: Vec<Arc<QualifiedWorkRecord>>,
    coverage: WorkCoverage,
) -> Result<ResidentWorkDomain> {
    ResidentWorkDomain::bootstrap(scope(), coverage, records, 32, 1024 * 1024)
}
fn request(turn_id: &str) -> WorkCaptureRequest {
    WorkCaptureRequest {
        include_live: false,
        nonopen_limit: 0,
        mandatory_roots: BTreeSet::new(),
        latest_result_work_ids: BTreeSet::new(),
        required_turn_ids: BTreeSet::from([turn_id.into()]),
        required_task_ids: BTreeSet::new(),
        required_trigger_message_ids: BTreeSet::new(),
    }
}

#[test]
fn complete_turn_wait_relation_captures_resolved_cancelled_and_owner_without_pinning_other_turns() {
    let resolved = wait("resolved", "current", "resolved", 2000);
    let cancelled = wait("cancelled", "current", "cancelled", 1000);
    let unrelated = wait("unrelated", "other", "resolved", 3000);
    let weak_resolved = Arc::downgrade(&resolved);
    let weak_unrelated = Arc::downgrade(&unrelated);
    let records = vec![owner(), resolved, cancelled, unrelated];
    let mut prepared = coverage(&records, 1);
    prepared.turn_wait_keys.insert(
        "current".into(),
        BTreeSet::from(["resolved".into(), "cancelled".into()]),
    );
    prepared
        .turn_wait_keys
        .insert("other".into(), BTreeSet::from(["unrelated".into()]));
    let resident = domain(records, prepared).unwrap();
    let captured = resident.capture(&request("current")).unwrap();
    assert!(!captured.live_complete);
    assert_eq!(captured.turn_wait_keys.len(), 1);
    assert_eq!(
        captured
            .waits_for_turn("current")
            .unwrap()
            .iter()
            .map(|wait| wait.id.as_str())
            .collect::<Vec<_>>(),
        vec!["cancelled", "resolved"]
    );
    assert!(captured
        .lookup(&WorkKey::new(WorkFamily::Work, "closed-owner"))
        .unwrap()
        .is_some());
    assert!(captured
        .waits_for_turn("other")
        .unwrap_err()
        .downcast_ref::<WorkReadError>()
        .is_some());
    assert!(!captured
        .observations
        .contains_key(&WorkKey::new(WorkFamily::Wait, "unrelated")));
    drop(resident);
    assert!(weak_unrelated.upgrade().is_none());
    assert!(weak_resolved.upgrade().is_some());
    drop(captured);
    assert!(weak_resolved.upgrade().is_none());
}

#[test]
fn observed_empty_turn_wait_relation_is_distinct_from_unprepared() {
    let mut prepared = coverage(&[], 1);
    prepared.nonopen.is_exhaustive = true;
    prepared
        .turn_wait_keys
        .insert("empty".into(), BTreeSet::new());
    let resident = domain(vec![], prepared).unwrap();
    assert!(resident
        .capture(&request("empty"))
        .unwrap()
        .waits_for_turn("empty")
        .unwrap()
        .is_empty());
    assert!(
        matches!(resident.capture(&request("missing")).unwrap_err().downcast_ref::<WorkReadError>(), Some(WorkReadError::TurnWaitsNotPrepared(id)) if id == "missing")
    );
    let mut default_request = WorkCaptureRequest::current();
    default_request.nonopen_limit = 0;
    let default_capture = resident.capture(&default_request).unwrap();
    assert!(
        default_capture.turn_wait_keys.is_empty(),
        "only named Turn relations are copied"
    );
    assert!(default_capture.waits_for_turn("empty").is_err());
}

#[test]
fn turn_wait_relation_requires_every_exact_body_and_rejects_omission_or_wrong_turn() {
    let records = vec![owner(), wait("wait", "current", "resolved", 1000)];
    let mut omitted = coverage(&records, 1);
    omitted
        .turn_wait_keys
        .insert("current".into(), BTreeSet::new());
    assert!(
        format!("{:#}", domain(records.clone(), omitted).unwrap_err())
            .contains("omits an observed Wait")
    );
    let mut wrong = coverage(&records, 1);
    wrong
        .turn_wait_keys
        .insert("different".into(), BTreeSet::from(["wait".into()]));
    assert!(format!("{:#}", domain(records.clone(), wrong).unwrap_err())
        .contains("disagrees with its Wait"));
    let mut missing = coverage(&records, 1);
    missing
        .turn_wait_keys
        .insert("current".into(), BTreeSet::from(["never-prepared".into()]));
    assert!(domain(records, missing)
        .unwrap_err()
        .downcast_ref::<WorkReadError>()
        .is_some());
}

#[test]
fn original_transaction_delta_preserves_final_turn_wait_relation_after_terminal_changes() {
    let initial = wait("wait", "current", "active", 1000);
    let initial_records = vec![owner(), initial];
    let mut initial_coverage = coverage(&initial_records, 1);
    initial_coverage.unresolved_wait_ids.insert("wait".into());
    initial_coverage
        .turn_wait_keys
        .insert("current".into(), BTreeSet::from(["wait".into()]));
    let original = domain(initial_records, initial_coverage).unwrap();
    let mut terminal = wait("wait", "current", "resolved", 1000).as_ref().clone();
    terminal.revision = 2;
    let terminal = Arc::new(terminal);
    let mut added = wait("extra", "current", "cancelled", 2000).as_ref().clone();
    added.revision = 2;
    let added = Arc::new(added);
    let final_records = vec![owner(), terminal.clone(), added.clone()];
    let mut final_coverage = coverage(&final_records, 2);
    final_coverage.turn_wait_keys.insert(
        "current".into(),
        BTreeSet::from(["wait".into(), "extra".into()]),
    );
    let delta = CommittedWorkDelta {
        scope: scope(),
        base_revision: 1,
        revision: 2,
        records: vec![terminal, added],
        removals: BTreeMap::new(),
        coverage: final_coverage,
    };
    let next = original.with_committed(&delta).unwrap();
    let captured = next.capture(&request("current")).unwrap();
    assert!(captured.unresolved_wait_ids.is_empty());
    assert_eq!(
        captured
            .waits_for_turn("current")
            .unwrap()
            .iter()
            .map(|wait| wait.status.clone())
            .collect::<Vec<_>>(),
        vec![
            WaitConditionStatus::Resolved,
            WaitConditionStatus::Cancelled
        ]
    );
    assert!(original
        .capture(&request("current"))
        .unwrap()
        .waits_for_turn("current")
        .unwrap()
        .iter()
        .all(|wait| wait.status == WaitConditionStatus::Active));
}
