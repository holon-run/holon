use super::*;
use crate::runtime::resident_work::{CanonicalWorkRecord, WorkFamily, WorkKey};
use crate::runtime::tests::support::{LifecycleHarness, PRE_COMMIT_FAULTS};

async fn assert_live_record(runtime: &RuntimeHandle, expected: &WorkItemRecord) {
    let observed = runtime
        .capture_work_read(&crate::runtime::work_reads::live_work_request())
        .await
        .unwrap();
    assert_eq!(
        observed
            .work
            .lookup(&WorkKey::new(WorkFamily::Work, &expected.id))
            .unwrap()
            .unwrap()
            .record,
        CanonicalWorkRecord::Work(expected.clone())
    );
    assert_eq!(
        runtime.storage().latest_work_item(&expected.id).unwrap(),
        Some(expected.clone())
    );
}

#[tokio::test]
async fn live_record_writer_acknowledges_cas_and_preserves_execution_generation() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = runtime
        .create_work_item("original record".into(), None, None, Vec::new())
        .await
        .unwrap();
    let before = harness.snapshot();
    let execution = &before.execution_protocol.as_ref().unwrap().work_items[&original.id];
    let mut updated = original.clone();
    updated.objective = "updated live record".into();
    updated.revision += 1;
    runtime
        .update_work_item_record_expected(&updated, original.revision)
        .await
        .unwrap();
    assert_live_record(runtime, &updated).await;
    let after = harness.snapshot();
    let next_execution = &after.execution_protocol.as_ref().unwrap().work_items[&original.id];
    assert_eq!(next_execution.source_revision, updated.revision);
    assert_eq!(next_execution.generation(), execution.generation());

    runtime
        .update_work_item_record_expected(&updated, original.revision)
        .await
        .unwrap();
    assert_live_record(runtime, &updated).await;

    let mut conflicting = updated.clone();
    conflicting.objective = "same revision, conflicting body".into();
    let error = runtime
        .update_work_item_record_expected(&conflicting, original.revision)
        .await
        .unwrap_err();
    assert!(error
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .is_some());
    assert_live_record(runtime, &updated).await;
}

#[tokio::test]
async fn live_record_writer_rejects_another_owner_without_writing() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = runtime
        .create_work_item("owned record".into(), None, None, Vec::new())
        .await
        .unwrap();
    let mut foreign = original.clone();
    foreign.agent_id = "another-agent".into();
    foreign.revision += 1;
    assert!(runtime
        .update_work_item_record_expected(&foreign, original.revision)
        .await
        .unwrap_err()
        .to_string()
        .contains("requires its owning runtime"));
    assert_live_record(runtime, &original).await;
}

#[tokio::test]
async fn live_record_writer_rolls_back_work_and_execution_before_commit() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let original = runtime
            .create_work_item("rollback record".into(), None, None, Vec::new())
            .await
            .unwrap();
        let before = harness.snapshot();
        let mut updated = original.clone();
        updated.objective = "must roll back".into();
        updated.revision += 1;
        harness.arm_fault(fault);
        assert!(runtime
            .update_work_item_record_expected(&updated, original.revision)
            .await
            .is_err());
        assert_live_record(runtime, &original).await;
        assert_eq!(
            harness.snapshot().execution_protocol,
            before.execution_protocol
        );
    }
}

#[tokio::test]
async fn live_record_writer_is_required_for_qualified_storage_updates() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = runtime
        .create_work_item("qualified record".into(), None, None, Vec::new())
        .await
        .unwrap();
    let mut updated = original.clone();
    updated.objective = "must use the owner".into();
    updated.revision += 1;
    let error = runtime
        .storage()
        .update_work_item_expected(&updated, original.revision)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("requires its owning runtime writer"));
    assert_live_record(runtime, &original).await;
    runtime
        .update_work_item_record_expected(&updated, original.revision)
        .await
        .unwrap();
    assert_live_record(runtime, &updated).await;
}

#[test]
fn offline_unqualified_record_writer_keeps_cas_semantics() {
    let home = tempfile::tempdir().unwrap();
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )
    .unwrap();
    let original = WorkItemRecord::new("offline-agent", "legacy record", WorkItemState::Open);
    assert!(db.work_items().insert_new(&original).unwrap());
    let mut updated = original.clone();
    updated.objective = "offline update".into();
    updated.revision += 1;
    assert!(db
        .work_items()
        .update_expected(&updated, original.revision)
        .unwrap());
    assert!(!db
        .work_items()
        .update_expected(&updated, original.revision)
        .unwrap());
    let mut conflicting = updated.clone();
    conflicting.objective = "conflicting offline update".into();
    assert!(db
        .work_items()
        .update_expected(&conflicting, original.revision)
        .unwrap_err()
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .is_some());
    assert_eq!(db.work_items().latest(&original.id).unwrap(), Some(updated));
}
