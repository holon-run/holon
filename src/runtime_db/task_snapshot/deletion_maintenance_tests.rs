use super::*;
use crate::types::{AgentDeletionJob, AgentDeletionPhase, AgentDeletionStatus};

fn job(db: &RuntimeDb) -> Result<AgentDeletionJob> {
    let identity = db.agent_identities().latest("task-agent")?.unwrap();
    let (_, mut job, _) =
        db.agent_deletions()
            .begin("task-agent", identity.revision, "test", false)?;
    job.status = AgentDeletionStatus::Running;
    job.phase = AgentDeletionPhase::Quiesce;
    db.agent_deletions().update(&job)?;
    Ok(job)
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn status(db: &RuntimeDb, id: &str) -> Result<TaskStatus> {
    Ok(db.tasks().latest(id)?.unwrap().status)
}
fn aliases(db: &RuntimeDb, id: &str) -> Result<usize> {
    Ok(db.connection()?.query_row(
        "SELECT COUNT(*) FROM task_domain_records WHERE agent_id='task-agent' AND record_id=?1",
        [id],
        |r| r.get(0),
    )?)
}

#[test]
fn deletion_task_no_state_cancels_qualified_and_raw_without_ready_or_new_qualification(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = ledger()?;
    let qualified = task("qualified-delete", TaskStatus::Running);
    db.transaction(|tx| write_task(tx, &qualified).map(|_| ()))?;
    let raw = task("raw-delete", TaskStatus::Running);
    db.tasks().upsert(&raw)?;
    let job = job(&db)?;
    db.connection()?
        .execute("DELETE FROM agent_states WHERE agent_id='task-agent'", [])?;
    for id in [&qualified.id, &raw.id] {
        let (ack, identity) =
            db.cancel_task_for_deletion(&scope(), &job, id, at() + Duration::seconds(1), &ledger)?;
        assert!(ack.changed);
        assert_eq!(identity.status, AgentRegistryStatus::Deleting);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(ack);
        assert_eq!(status(&db, id)?, TaskStatus::Cancelled);
    }
    assert_eq!(aliases(&db, &qualified.id)?, 1);
    assert_eq!(aliases(&db, &raw.id)?, 0);
    db.transaction(|tx| {
        let coverage = current_coverage(tx, &scope())?.unwrap();
        assert!(
            !coverage.active_complete
                && !coverage.unsettled_complete
                && !coverage.stop.pending_complete
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM agent_states", [], |r| r
                .get::<_, usize>(0))?,
            0
        );
        let key = TaskKey::new(TaskFamily::Task, &qualified.id);
        let proof = prepare_existing_task_maintenance_tx(tx, &scope(), &BTreeSet::from([key]))?;
        assert!(proof.before.values().all(|x| x.qualified));
        Ok(())
    })?;
    assert!(db
        .deletion_task_batch(&scope(), &job, &ledger)?
        .ids
        .is_empty());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn deletion_task_alias_fault_rolls_back_original_cancellation_and_scope_metadata() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = ledger()?;
    let value = task("delete-fault", TaskStatus::Running);
    db.transaction(|tx| write_task(tx, &value).map(|_| ()))?;
    let job = job(&db)?;
    let before = db.transaction(|tx| current_coverage(tx, &scope()))?;
    db.connection()?.execute_batch("CREATE TRIGGER fail_delete_task_alias BEFORE INSERT ON task_domain_records BEGIN SELECT RAISE(ABORT,'delete-task-alias-fault'); END;")?;
    let error = db
        .cancel_task_for_deletion(
            &scope(),
            &job,
            &value.id,
            at() + Duration::seconds(1),
            &ledger,
        )
        .err()
        .context("alias fault missing")?;
    assert!(
        format!("{error:#}").contains("delete-task-alias-fault"),
        "{error:#}"
    );
    assert_eq!(status(&db, &value.id)?, TaskStatus::Running);
    assert_eq!(db.transaction(|tx| current_coverage(tx, &scope()))?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.connection()?
        .execute_batch("DROP TRIGGER fail_delete_task_alias")?;
    assert!(
        db.cancel_task_for_deletion(
            &scope(),
            &job,
            &value.id,
            at() + Duration::seconds(1),
            &ledger
        )?
        .0
        .changed
    );
    Ok(())
}

#[test]
fn deletion_task_job_incarnation_and_pruned_fences_precede_raw_business() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = ledger()?;
    let value = task("delete-fenced", TaskStatus::Running);
    db.transaction(|tx| write_task(tx, &value).map(|_| ()))?;
    let job = job(&db)?;
    let wrong = InputEvidenceScope {
        agent_id: "task-agent".into(),
        identity_incarnation: 2,
    };
    assert!(db
        .cancel_task_for_deletion(
            &wrong,
            &job,
            &value.id,
            at() + Duration::seconds(1),
            &ledger
        )
        .is_err());
    let mut stale = job.clone();
    stale.attempts += 1;
    assert!(db
        .cancel_task_for_deletion(
            &scope(),
            &stale,
            &value.id,
            at() + Duration::seconds(1),
            &ledger
        )
        .is_err());
    let mut parent = job.clone();
    parent.mode = crate::types::AgentDeletionMode::ParentCleanup;
    db.agent_deletions().update(&parent)?;
    assert!(db
        .cancel_task_for_deletion(
            &scope(),
            &parent,
            &value.id,
            at() + Duration::seconds(1),
            &ledger
        )
        .is_err());
    db.agent_deletions().update(&job)?;
    db.connection()?.execute("UPDATE task_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE record_id=?1",[&value.id])?;
    assert!(db
        .cancel_task_for_deletion(
            &scope(),
            &job,
            &value.id,
            at() + Duration::seconds(1),
            &ledger
        )
        .is_err());
    assert_eq!(status(&db, &value.id)?, TaskStatus::Running);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn deleted_cleanup_repair_keeps_original_raw_cancellation_without_touching_retired_aliases(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = ledger()?;
    let value = task("deleted-repair", TaskStatus::Running);
    db.transaction(|tx| write_task(tx, &value).map(|_| ()))?;
    let before: String = db.connection()?.query_row(
        "SELECT content_digest||projection_digest FROM task_domain_records WHERE record_id=?1",
        [&value.id],
        |r| r.get(0),
    )?;
    let mut identity = db.agent_identities().latest("task-agent")?.unwrap();
    identity.status = AgentRegistryStatus::Deleted;
    identity.deleted_at = Some(at());
    db.agent_identities().upsert(&identity)?;
    let job = job(&db)?;
    assert_eq!(job.mode, crate::types::AgentDeletionMode::CleanupRepair);
    let (ack, actual) = db.cancel_task_for_deletion(
        &scope(),
        &job,
        &value.id,
        at() + Duration::seconds(1),
        &ledger,
    )?;
    assert!(ack.changed);
    assert_eq!(actual.status, AgentRegistryStatus::Deleted);
    drop(ack);
    assert_eq!(status(&db, &value.id)?, TaskStatus::Cancelled);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT content_digest||projection_digest FROM task_domain_records WHERE record_id=?1",
            [&value.id],
            |r| r.get::<_, String>(0)
        )?,
        before
    );
    // Corrupted/successor metadata must never inherit this retired namespace.
    db.connection()?.execute("UPDATE agent_identities SET status='deleting',payload_json=json_set(payload_json,'$.status','deleting','$.incarnation',2) WHERE agent_id='task-agent'",[])?;
    assert!(db
        .cancel_task_for_deletion(
            &scope(),
            &job,
            &value.id,
            at() + Duration::seconds(1),
            &ledger
        )
        .is_err());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn deletion_task_predecode_material_rejects_large_damaged_body_and_id_batch_is_bounded(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = ledger()?;
    for n in 0..129 {
        db.tasks()
            .upsert(&task(&format!("delete-batch-{n:03}"), TaskStatus::Running))?;
    }
    let job = job(&db)?;
    let batch = db.deletion_task_batch(&scope(), &job, &ledger)?;
    assert_eq!(batch.ids.len(), 128);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(batch);
    db.connection()?.execute(
        "UPDATE tasks SET payload_json=?1 WHERE task_id='delete-batch-000'",
        ["x".repeat(9 * 1024 * 1024)],
    )?;
    let error = db
        .cancel_task_for_deletion(
            &scope(),
            &job,
            "delete-batch-000",
            at() + Duration::seconds(1),
            &ledger,
        )
        .err()
        .context("large damaged body accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT status FROM tasks WHERE task_id='delete-batch-000'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        "running"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
