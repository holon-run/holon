use super::*;
use crate::{
    runtime::{
        resident_input_evidence::InputEvidenceScope,
        resident_work::{
            QualifiedWorkRecord, ResidentWorkDomain, WorkAvailability, WorkCoverage,
            WorkHistoryCoverage,
        },
    },
    runtime_db::RuntimeDb,
};
use std::collections::{BTreeMap, BTreeSet};

#[path = "work_reads_tests/query_material_tests.rs"]
mod query_material_tests;

#[path = "work_reads_tests/reader_sort_tests.rs"]
mod reader_sort_tests;

#[path = "work_reads_tests/active_wait_filter_tests.rs"]
mod active_wait_filter_tests;

#[path = "work_reads_tests/lookup_key_tests.rs"]
mod lookup_key_tests;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "work-read-owner".into(),
        identity_incarnation: 3,
    }
}
fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}
fn work(id: &str, state: WorkItemState) -> WorkItemRecord {
    let mut record = WorkItemRecord::new(scope().agent_id, id, state);
    record.id = id.into();
    record.created_at = at(1000);
    record.updated_at = at(1000);
    record
}
fn captured(records: Vec<CanonicalWorkRecord>) -> CapturedWork {
    let records = records
        .into_iter()
        .map(|record| {
            Arc::new(QualifiedWorkRecord {
                scope: scope(),
                key: record.key(),
                revision: 1,
                content_digest: "a".repeat(64),
                resident_bytes: serde_json::to_vec(&record).unwrap().len(),
                record,
            })
        })
        .collect::<Vec<_>>();
    // This is a pure reader parity fixture over repository readbacks, not a
    // production qualification grant or a bootstrap adoption path.
    let coverage = WorkCoverage {
        revision: 1,
        live_complete: true,
        live_work_ids: records
            .iter()
            .filter_map(|record| match &record.record {
                CanonicalWorkRecord::Work(work)
                    if matches!(work.state, WorkItemState::Open | WorkItemState::Completing) =>
                {
                    Some(work.id.clone())
                }
                _ => None,
            })
            .collect(),
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
            keys: Vec::new(),
            is_exhaustive: false,
        },
        latest_results: records
            .iter()
            .filter(|record| record.key.family == WorkFamily::Work)
            .map(|record| (record.key.id.clone(), None))
            .collect(),
    };
    ResidentWorkDomain::bootstrap(scope(), coverage, records, 64, 1024 * 1024)
        .unwrap()
        .capture(&live_work_request())
        .unwrap()
}
fn database(directory: &tempfile::TempDir) -> RuntimeDb {
    RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )
    .unwrap()
}

#[test]
fn resident_work_rechecks_match_sql_normalized_millisecond_predicates() {
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory);
    let mut due = work("due", WorkItemState::Open);
    due.blocked_by = Some("external dependency".into());
    due.recheck_at = Some(at(2000) + chrono::Duration::nanoseconds(900_000));
    let mut future = work("future", WorkItemState::Open);
    future.blocked_by = Some("later dependency".into());
    future.recheck_at = Some(at(4000));
    let mut consumed_same_millis = work("already-consumed", WorkItemState::Open);
    consumed_same_millis.blocked_by = Some("same millisecond".into());
    consumed_same_millis.recheck_at = due.recheck_at;
    consumed_same_millis.recheck_consumed_at =
        Some(at(2000) + chrono::Duration::nanoseconds(100_000));
    let mut completing = work("completing", WorkItemState::Completing);
    completing.blocked_by = Some("terminal candidate".into());
    completing.recheck_at = Some(at(1500));
    let mut unblocked = work("unblocked", WorkItemState::Open);
    unblocked.recheck_at = Some(at(1500));
    for record in [
        &due,
        &future,
        &consumed_same_millis,
        &completing,
        &unblocked,
    ] {
        assert!(db.work_items().insert_new(record).unwrap());
    }
    let records = [
        &due,
        &future,
        &consumed_same_millis,
        &completing,
        &unblocked,
    ]
    .into_iter()
    .map(|record| CanonicalWorkRecord::Work(db.work_items().latest(&record.id).unwrap().unwrap()))
    .collect();
    let observation = captured(records);
    let now = at(2000);
    let expected = db
        .work_items()
        .due_blocked_rechecks(&scope().agent_id, now)
        .unwrap();
    assert_eq!(expected, vec![due]);
    assert_eq!(
        observation
            .due_work_rechecks(now)
            .unwrap()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        observation.next_work_recheck_at().unwrap(),
        db.work_items().next_recheck_at(&scope().agent_id).unwrap()
    );
    assert_eq!(observation.next_work_recheck_at().unwrap(), Some(at(2000)));
    assert_eq!(
        observation
            .due_work_rechecks(at(4000))
            .unwrap()
            .iter()
            .map(|work| work.id.as_str())
            .collect::<Vec<_>>(),
        vec!["due", "future"]
    );
}

fn wait(id: &str, owner: Option<&str>, millis: i64) -> WaitConditionRecord {
    serde_json::from_value(serde_json::json!({
        "id":id,"agent_id":scope().agent_id,"work_item_id":owner,
        "status":"active","kind":"operator","waiting_for":"operator",
        "created_at":at(millis),"updated_at":at(millis),
        "continuation":{"recheck_at":at(3000).to_rfc3339()}
    }))
    .unwrap()
}
#[test]
fn resident_agent_wait_recheck_preserves_active_triggered_resolved_sql_parity() {
    let directory = tempfile::tempdir().unwrap();
    let db = database(&directory);
    let owner = work("owned-work", WorkItemState::Open);
    db.work_items().insert_new(&owner).unwrap();
    let mut agent_wait = wait("agent-wait", None, 1500);
    let owned_wait = wait("owned-wait", Some(&owner.id), 1000);
    db.wait_conditions().upsert(&agent_wait).unwrap();
    db.wait_conditions().upsert(&owned_wait).unwrap();
    for status in [
        WaitConditionStatus::Active,
        WaitConditionStatus::Triggered,
        WaitConditionStatus::Resolved,
    ] {
        if status == WaitConditionStatus::Triggered {
            agent_wait.mark_triggered("original-trigger-message", at(4000));
            db.wait_conditions().upsert(&agent_wait).unwrap();
        } else if status == WaitConditionStatus::Resolved {
            agent_wait.status = status.clone();
            agent_wait.updated_at = at(5000);
            agent_wait.resolved_at = Some(at(5000));
            db.wait_conditions().upsert(&agent_wait).unwrap();
        }
        let durable = db
            .wait_conditions()
            .unresolved_for_agent(&scope().agent_id)
            .unwrap();
        let mut records = vec![CanonicalWorkRecord::Work(owner.clone())];
        records.extend(durable.iter().cloned().map(CanonicalWorkRecord::Wait));
        let observation = captured(records);
        assert_eq!(
            observation
                .unresolved_wait_conditions()
                .unwrap()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            durable
        );
        let expected_next = durable
            .iter()
            .filter(|wait| {
                wait.work_item_id.is_none() && wait.status == WaitConditionStatus::Active
            })
            .filter_map(WaitConditionRecord::recheck_at)
            .min();
        let expected_due = durable
            .iter()
            .filter(|wait| {
                wait.work_item_id.is_none()
                    && wait.status == WaitConditionStatus::Active
                    && wait
                        .recheck_at()
                        .is_some_and(|deadline| deadline <= at(6000))
            })
            .min_by_key(|wait| wait.recheck_at());
        assert_eq!(
            observation.next_agent_wait_recheck_at().unwrap(),
            expected_next
        );
        assert_eq!(
            observation.due_agent_wait_recheck(at(6000)).unwrap(),
            expected_due
        );
        assert_eq!(
            expected_next.is_some(),
            status == WaitConditionStatus::Active,
            "Triggered stays resident but cannot emit another agent-wait deadline"
        );
    }
}

#[test]
fn an_unprepared_named_work_is_never_an_absent_or_empty_live_observation() {
    let observation = captured(vec![CanonicalWorkRecord::Work(work(
        "present",
        WorkItemState::Open,
    ))]);
    assert!(matches!(
        observation.work_item("not-prepared").unwrap_err().downcast_ref::<WorkReadError>(),
        Some(WorkReadError::NeedsPreparation(key)) if key.id == "not-prepared"
    ));
    let mut partial = observation;
    partial.live_complete = false;
    assert!(matches!(
        partial
            .next_work_recheck_at()
            .unwrap_err()
            .downcast_ref::<WorkReadError>(),
        Some(WorkReadError::IncompleteLive)
    ));
    assert!(matches!(
        partial
            .next_agent_wait_recheck_at()
            .unwrap_err()
            .downcast_ref::<WorkReadError>(),
        Some(WorkReadError::IncompleteLive)
    ));
}
