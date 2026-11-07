use super::tests::*;
use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    types::{TaskRecord, TaskStatus},
};

fn load(turns: &[&str]) -> TaskDomainLoadRequest {
    let mut load = request();
    load.record_limit = MAX_RECORDS;
    load.byte_limit = 16 * 1024 * 1024;
    load.query
        .parent_turn_ids
        .extend(turns.iter().map(|turn| (*turn).to_owned()));
    load
}
fn parent_task(id: &str, status: TaskStatus) -> TaskRecord {
    let mut row = task(id, status);
    row.detail = Some(serde_json::json!({"parent_turn_id":"observed-parent"}));
    row
}
fn actual(
    tx: &Transaction<'_>,
    values: &[TaskRecord],
    load: &TaskDomainLoadRequest,
) -> Result<CommittedTaskFacts> {
    let keys = values
        .iter()
        .map(|row| TaskKey::new(TaskFamily::Task, &row.id))
        .collect();
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    for row in values {
        repositories::upsert_task_tx(tx, row)?;
    }
    let state = ContextDurableRead::new(tx, &scope().agent_id)
        .scope_facts()?
        .state
        .context("actual State missing")?;
    register_task_writes_tx(tx, &proof, &keys, &state, load)
}

#[test]
fn complete_parent_turn_headers_exceed_terminal_window_without_hydrating_history() -> Result<()> {
    let (_home, db) = fresh()?;
    let load = load(&["observed-parent", "observed-empty"]);
    let values = (0..130)
        .map(|i| parent_task(&format!("parent-task-{i:03}"), TaskStatus::Completed))
        .collect::<Vec<_>>();
    db.transaction(|tx| {
        actual(tx, &values, &load)?;
        Ok(())
    })?;
    // A post-cutover canonical tail is not an accepted member of the role.
    db.transaction(|tx| {
        repositories::upsert_task_tx(tx, &parent_task("unreceived-tail", TaskStatus::Running))?;
        Ok(())
    })?;
    let captured = db.transaction(|tx| {
        let domain = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?
            .require_current()?
            .resident;
        assert_eq!(
            domain
                .capture(&TaskCaptureRequest::current())?
                .records
                .len(),
            128
        );
        let captured = domain.capture_query(&load.query)?;
        assert!(
            captured.records.is_empty(),
            "metadata role cannot grant any Task body"
        );
        assert_eq!(
            captured.parent_turn_task_headers("observed-parent")?.len(),
            130
        );
        assert!(captured
            .parent_turn_task_headers("observed-empty")?
            .is_empty());
        assert!(captured.parent_turn_task_headers("unobserved").is_err());
        let native: usize = tx.query_row(
            "SELECT COUNT(*)-1 FROM json_tree(?1,'$.parent_turn_tasks')",
            [serde_json::to_string(&captured.coverage.query)?],
            |row| row.get(0),
        )?;
        assert_eq!(
            captured.coverage.query.owned_metadata_record_count()?,
            native
        );
        Ok(captured)
    })?;
    db.transaction(|tx| { tx.execute_batch("ALTER TABLE tasks RENAME TO parent_task_bodies_forbidden; ALTER TABLE agent_states RENAME TO parent_state_bodies_forbidden; ALTER TABLE agent_identities RENAME TO parent_identity_bodies_forbidden;")?; Ok(()) })?;
    assert_eq!(
        captured.parent_turn_task_headers("observed-parent")?.len(),
        130
    );
    assert!(captured
        .parent_turn_task_headers("observed-parent")?
        .values()
        .all(|row| row.status == TaskStatus::Completed && row.task.scope == scope()));
    Ok(())
}

#[test]
fn parent_turn_actual_ack_updates_status_and_pruned_member_remains_unavailable() -> Result<()> {
    let (_home, db) = fresh()?;
    let load = load(&["observed-parent"]);
    let original = parent_task("actual-parent-member", TaskStatus::Running);
    db.transaction(|tx| {
        actual(tx, std::slice::from_ref(&original), &load)?;
        Ok(())
    })?;
    let before = db.transaction(|tx| {
        Ok(prepare_task_domain_snapshot_tx(tx, &scope(), &load)?
            .require_current()?
            .resident
            .capture_query(&load.query)?)
    })?;
    let mut ended = original.clone();
    ended.status = TaskStatus::Completed;
    ended.updated_at += chrono::Duration::seconds(1);
    db.transaction(|tx| {
        let facts = actual(tx, std::slice::from_ref(&ended), &load)?;
        assert_eq!(facts.delta.revision, facts.delta.base_revision + 1);
        let current = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?
            .require_current()?
            .resident
            .capture_query(&load.query)?;
        assert_eq!(
            current.parent_turn_task_headers("observed-parent")?[&ended.id].status,
            TaskStatus::Completed
        );
        Ok(())
    })?;
    db.transaction(|tx| {
        let keys=BTreeSet::from([TaskKey::new(TaskFamily::Task,&ended.id)]);
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        tx.execute("DELETE FROM tasks WHERE task_id=?1",[&ended.id])?;
        let state=ContextDurableRead::new(tx,&scope().agent_id).scope_facts()?.state.unwrap();
        register_task_writes_tx(tx,&proof,&keys,&state,&load)?;
        let current=prepare_task_domain_snapshot_tx(tx,&scope(),&load)?.require_current()?.resident.capture_query(&load.query)?;
        assert!(matches!(current.parent_turn_task_headers("observed-parent").unwrap_err().downcast_ref::<TaskReadError>(),Some(TaskReadError::UnavailableQuery(key)) if key.id==ended.id));
        assert_eq!(before.parent_turn_task_headers("observed-parent")?[&ended.id].status,TaskStatus::Running);
        Ok(())
    })
}

#[test]
fn sqlite_upgrade_parent_headers_preserve_status_without_late_adoption() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &parent_task("fixed-parent-member", TaskStatus::Failed))?;
        repositories::upsert_task_tx(
            tx,
            &parent_task("second-fixed-parent-member", TaskStatus::Completed),
        )?;
        Ok(())
    })?;
    let load = load(&["observed-parent"]);
    db.transaction(|tx| {
        repositories::upsert_task_tx(tx, &parent_task("late-parent-member", TaskStatus::Running))?;
        let capture = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?
            .require_current()?
            .resident
            .capture_query(&load.query)?;
        let rows = capture.parent_turn_task_headers("observed-parent")?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows["fixed-parent-member"].status, TaskStatus::Failed);
        assert_eq!(
            rows["second-fixed-parent-member"].status,
            TaskStatus::Completed
        );
        assert!(!rows.contains_key("late-parent-member"));
        assert!(capture.records.is_empty());
        Ok(())
    })
}

#[test]
fn parent_turn_scalar_budget_precedes_header_decode_and_native_coverage_gate() -> Result<()> {
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        tx.execute("INSERT INTO task_query_task_members(agent_id,identity_incarnation,task_id,content_digest,projection_digest,created_at,parent_turn_id,task_status,header_digest) VALUES(?1,1,?2,?3,?3,0,'observed-parent','not-a-status',?3)",params![scope().agent_id,"x".repeat(65536),"a".repeat(64)])?;
        let mut small=load(&["observed-parent"]);small.byte_limit=4096;
        let error=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&small,&ledger).unwrap_err();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted(_))));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (_home, db) = fresh()?;
    let load = load(&["observed-parent"]);
    db.transaction(|tx| {
        actual(
            tx,
            &[parent_task("coverage-member", TaskStatus::Completed)],
            &load,
        )?;
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut small = load.clone();
        small.record_limit = 1;
        let original_scope = scope();
        let mut material =
            read_preparation::TaskReadMaterial::new(&original_scope, &small, Some(&ledger));
        assert!(matches!(
            material
                .read_current_metadata(tx)
                .unwrap_err()
                .downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
