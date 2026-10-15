use super::*;
use crate::runtime_db::RuntimeDb;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "task-wait-reader".into(),
        identity_incarnation: 7,
    }
}
fn at(millis: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(millis).unwrap()
}
fn qualified(record: CanonicalWorkRecord) -> Arc<QualifiedWorkRecord> {
    Arc::new(QualifiedWorkRecord {
        scope: scope(),
        key: record.key(),
        revision: 1,
        content_digest: "a".repeat(64),
        resident_bytes: serde_json::to_vec(&record).unwrap().len(),
        record,
    })
}
fn wait(id: &str, task: &str) -> WaitConditionRecord {
    serde_json::from_value(serde_json::json!({
        "id":id,"agent_id":scope().agent_id,"status":"active","kind":"task",
        "waiting_for":"result","wake_sources":[{"kind":"task_result","task_id":task}],
        "created_at":at(1000),"updated_at":at(1000)
    }))
    .unwrap()
}
fn coverage(records: &[Arc<QualifiedWorkRecord>]) -> WorkCoverage {
    WorkCoverage {
        legacy_delivery_heads: std::collections::BTreeMap::new(),
        revision: 1,
        live_complete: true,
        live_work_ids: BTreeSet::new(),
        unresolved_wait_ids: records
            .iter()
            .filter_map(|record| match &record.record {
                CanonicalWorkRecord::Wait(wait)
                    if matches!(
                        wait.status,
                        WaitConditionStatus::Active | WaitConditionStatus::Triggered
                    ) =>
                {
                    Some(wait.id.clone())
                }
                _ => None,
            })
            .collect(),
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
            keys: vec![],
            is_exhaustive: true,
        },
        latest_results: records
            .iter()
            .filter(|record| record.key.family == WorkFamily::Work)
            .map(|record| (record.key.id.clone(), None))
            .collect(),
    }
}
fn request() -> WorkCaptureRequest {
    let mut request = WorkCaptureRequest::current();
    request.nonopen_limit = 0;
    request.required_task_ids.insert("task-original".into());
    request
        .required_trigger_message_ids
        .insert("result-original".into());
    request
}
#[test]
fn focused_task_and_trigger_frontiers_preserve_all_status_sql_membership_and_id_order() {
    let directory = tempfile::tempdir().unwrap();
    let db = RuntimeDb::open_and_migrate(
        directory.path().join("db.sqlite"),
        directory.path().join("db.lock"),
    )
    .unwrap();
    let mut first = wait("a-resolved", "task-original");
    let mut cancelled = wait("z-cancelled", "task-original");
    db.wait_conditions().upsert(&first).unwrap();
    first.mark_triggered("result-original", at(2000));
    db.wait_conditions().upsert(&first).unwrap();
    first.status = WaitConditionStatus::Resolved;
    first.updated_at = at(3000);
    first.resolved_at = Some(at(3000));
    db.wait_conditions().upsert(&first).unwrap();
    db.wait_conditions().upsert(&cancelled).unwrap();
    cancelled.status = WaitConditionStatus::Cancelled;
    cancelled.updated_at = at(4000);
    cancelled.cancelled_at = Some(at(4000));
    db.wait_conditions().upsert(&cancelled).unwrap();
    db.wait_conditions()
        .upsert(&wait("unrelated", "task-other"))
        .unwrap();
    let durable = db
        .wait_conditions()
        .latest_for_agent(&scope().agent_id)
        .unwrap();
    let records = durable
        .iter()
        .cloned()
        .map(CanonicalWorkRecord::Wait)
        .map(qualified)
        .collect::<Vec<_>>();
    let mut prepared = coverage(&records);
    prepared.task_wait_keys.insert(
        "task-original".into(),
        durable
            .iter()
            .filter(|wait| task_wait_matches(wait, "task-original"))
            .map(|wait| wait.id.clone())
            .collect(),
    );
    prepared.trigger_wait_keys.insert(
        "result-original".into(),
        durable
            .iter()
            .filter(|wait| wait.trigger_message_id() == Some("result-original"))
            .map(|wait| wait.id.clone())
            .collect(),
    );
    let resident =
        ResidentWorkDomain::bootstrap(scope(), prepared, records, 32, 1024 * 1024).unwrap();
    let captured = resident.capture(&request()).unwrap();
    assert_eq!(
        captured
            .waits_for_task("task-original")
            .unwrap()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        durable
            .iter()
            .filter(|wait| task_wait_matches(wait, "task-original"))
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        captured
            .waits_triggered_by("result-original")
            .unwrap()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![first]
    );
    assert_eq!(
        captured
            .waits_for_task("task-original")
            .unwrap()
            .iter()
            .map(|wait| wait.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-resolved", "z-cancelled"]
    );
    assert!(
        captured.waits_for_task("task-other").is_err(),
        "resident body is not a complete task relation"
    );
}
#[test]
fn focused_empty_relation_is_distinct_from_unprepared_and_omitted_present_wait() {
    let mut prepared = coverage(&[]);
    prepared
        .task_wait_keys
        .insert("task-original".into(), BTreeSet::new());
    prepared
        .trigger_wait_keys
        .insert("result-original".into(), BTreeSet::new());
    let resident =
        ResidentWorkDomain::bootstrap(scope(), prepared, vec![], 32, 1024 * 1024).unwrap();
    let captured = resident.capture(&request()).unwrap();
    assert!(captured.waits_for_task("task-original").unwrap().is_empty());
    assert!(captured
        .waits_triggered_by("result-original")
        .unwrap()
        .is_empty());
    let mut missing = request();
    missing.required_task_ids.insert("not-observed".into());
    assert!(
        matches!(resident.capture(&missing).unwrap_err().downcast_ref::<WorkReadError>(),
        Some(WorkReadError::TaskWaitsNotPrepared(id)) if id=="not-observed")
    );
    let record = qualified(CanonicalWorkRecord::Wait(wait("present", "task-original")));
    let mut prepared = coverage(&[record.clone()]);
    prepared
        .task_wait_keys
        .insert("task-original".into(), BTreeSet::new());
    assert!(
        ResidentWorkDomain::bootstrap(scope(), prepared, vec![record], 32, 1024 * 1024).is_err(),
        "complete empty cannot omit an already observed matching Wait"
    );
}
#[test]
fn selected_resolved_task_wait_owns_closed_work_and_releases_unselected_relation_arcs() {
    let mut work = WorkItemRecord::new(scope().agent_id, "closed", WorkItemState::Completed);
    work.id = "closed".into();
    let mut selected = wait("selected", "task-original");
    selected.work_item_id = Some(work.id.clone());
    selected.status = WaitConditionStatus::Resolved;
    selected.resolved_at = Some(at(2000));
    selected.updated_at = at(2000);
    selected.trigger_message_id = Some("result-original".into());
    selected.triggered_at = Some(at(1500));
    let mut other = wait("other", "task-other");
    other.status = WaitConditionStatus::Resolved;
    other.resolved_at = Some(at(2000));
    let selected = qualified(CanonicalWorkRecord::Wait(selected));
    let weak_selected = Arc::downgrade(&selected);
    let other = qualified(CanonicalWorkRecord::Wait(other));
    let weak_other = Arc::downgrade(&other);
    let records = vec![qualified(CanonicalWorkRecord::Work(work)), selected, other];
    let mut prepared = coverage(&records);
    prepared
        .task_wait_keys
        .insert("task-original".into(), BTreeSet::from(["selected".into()]));
    prepared
        .task_wait_keys
        .insert("task-other".into(), BTreeSet::from(["other".into()]));
    prepared.trigger_wait_keys.insert(
        "result-original".into(),
        BTreeSet::from(["selected".into()]),
    );
    let resident =
        ResidentWorkDomain::bootstrap(scope(), prepared, records, 32, 1024 * 1024).unwrap();
    let mut request = request();
    request.include_live = false;
    let captured = resident.capture(&request).unwrap();
    assert_eq!(captured.task_wait_keys.len(), 1);
    assert!(captured
        .lookup(&WorkKey::new(WorkFamily::Work, "closed"))
        .unwrap()
        .is_some());
    drop(resident);
    assert!(weak_other.upgrade().is_none());
    assert!(weak_selected.upgrade().is_some());
    drop(captured);
    assert!(weak_selected.upgrade().is_none());
}
