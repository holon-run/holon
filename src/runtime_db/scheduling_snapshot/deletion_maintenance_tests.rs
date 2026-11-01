use super::super::tests::{at, ledger, legacy, request, scope, timer, trigger, wake};
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
    },
    types::*,
};
fn prepared(db: &RuntimeDb, budget: &InputEvidenceBudgetLedger) -> Result<()> {
    db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), budget)?
            .require_current()?;
        Ok(())
    })
}
fn job(db: &RuntimeDb, phase: AgentDeletionPhase) -> Result<AgentDeletionJob> {
    let identity = db.agent_identities().latest(&scope().agent_id)?.unwrap();
    let (_, mut job, _) =
        db.agent_deletions()
            .begin(&scope().agent_id, identity.revision, "fixture", false)?;
    job.phase = phase;
    job.status = AgentDeletionStatus::Running;
    db.agent_deletions().update(&job)?;
    Ok(job)
}
fn seed(tx: &Transaction<'_>) -> Result<()> {
    repositories::upsert_timer_tx(tx, &timer("deletion-timer", TimerStatus::Active))?;
    repositories::upsert_external_trigger_tx(tx, &trigger())?;
    wake(tx, "deletion-timer", "old-pending-wake", 2)
}
fn fingerprint(db: &RuntimeDb) -> Result<Vec<Vec<String>>> {
    let c = db.connection()?;
    ["SELECT json_array(timer_id,agent_id,status,payload_json) FROM timers ORDER BY timer_id","SELECT json_array(timer_id,message_id,fire_count,status,created_at,updated_at,incorporated_at,cancelled_at) FROM timer_wakes ORDER BY timer_id,message_id","SELECT json_array(external_trigger_id,status,payload_json) FROM external_triggers ORDER BY external_trigger_id","SELECT json_array(message_id,status,payload_json) FROM queue_entries ORDER BY message_id","SELECT json_array(agent_id,identity_incarnation,revision,coverage_json) FROM scheduling_domain_current ORDER BY agent_id,identity_incarnation","SELECT json_array(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest) FROM scheduling_domain_records ORDER BY agent_id,identity_incarnation,family,record_id","SELECT payload_json FROM agent_deletion_jobs ORDER BY deletion_id"].into_iter().map(|sql|{
        let mut q=c.prepare(sql)?;let values=q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(values)
    }).collect()
}
#[test]
fn operator_deleting_no_state_preserves_original_timer_then_ingress_footprint() -> Result<()> {
    let (_home, db) = legacy(true, seed)?;
    let budget = ledger()?;
    prepared(&db, &budget)?;
    let mut original = job(&db, AgentDeletionPhase::Quiesce)?;
    db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&scope().agent_id],
    )?;
    let (ack, id) = db.cancel_timer_for_deletion(&scope(), &original, "deletion-timer", &budget)?;
    assert_eq!(id.status, AgentRegistryStatus::Deleting);
    assert_eq!(ack.changed, 1);
    assert_eq!(
        ack.qualified_actual_keys,
        BTreeSet::from([SchedulingKey::new(
            SchedulingFamily::Timer,
            "deletion-timer"
        )])
    );
    assert!(ack.invalidation.is_some());
    assert_eq!(
        db.connection()?.query_row(
            "SELECT status FROM timer_wakes WHERE timer_id='deletion-timer'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        "pending"
    );
    assert_eq!(
        db.queue_entries()
            .latest("old-pending-wake")?
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    let coverage = db
        .transaction(|tx| {
            let input_scope = scope();
            let load = request();
            let mut m = Material::new(&input_scope, &load, &budget);
            loader::current(tx, &scope(), &mut m)
        })?
        .unwrap();
    assert!(
        !coverage.active_timers_complete
            && !coverage.pending_wakes_complete
            && !coverage.active_triggers_complete
    );
    drop(ack);
    original.phase = AgentDeletionPhase::Ingress;
    db.agent_deletions().update(&original)?;
    let (ack, _) =
        db.revoke_trigger_for_deletion(&scope(), &original, "original-trigger", at(), &budget)?;
    assert_eq!(ack.changed, 1);
    assert_eq!(ack.qualified_actual_keys.len(), 1);
    drop(ack);
    assert!(db.agent_states().latest(&scope().agent_id)?.is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn actual_bulk_timer_sql_ack_contains_only_written_wakes_and_queue_entries() -> Result<()> {
    let (_home, db) = legacy(true, seed)?;
    db.transaction(|tx| {
        let ack = repositories::cancel_active_timers_with_writes_tx(tx, &scope().agent_id, at())?;
        assert_eq!(ack.cancelled_timers, 1);
        assert_eq!(ack.timer_ids, BTreeSet::from(["deletion-timer".into()]));
        assert_eq!(
            ack.wake_pairs,
            BTreeSet::from([("deletion-timer".into(), "old-pending-wake".into())])
        );
        assert_eq!(ack.queue_entries.len(), 1);
        assert_eq!(ack.queue_entries[0].status, QueueEntryStatus::Dropped);
        let repeat =
            repositories::cancel_active_timers_with_writes_tx(tx, &scope().agent_id, at())?;
        assert_eq!(repeat.cancelled_timers, 0);
        assert!(
            repeat.timer_ids.is_empty()
                && repeat.wake_pairs.is_empty()
                && repeat.queue_entries.is_empty()
        );
        Ok(())
    })
}
fn supervised() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("runtime.sqlite");
    let mut c = Connection::open(&path)?;
    ensure_migration_table(&c)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut c, migration)?;
    }
    let parent = AgentIdentityRecord::new(
        "cleanup-parent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let child = AgentIdentityRecord::new(
        scope().agent_id,
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(parent.agent_id.clone()),
        Some("creation-task".into()),
    );
    let tx = c.transaction()?;
    for (id, records) in [
        (
            &parent,
            crate::runtime_db::agent_relations::independent_creation_records(
                &parent,
                None,
                AgentCanonicalDurability::Persistent,
            ),
        ),
        (
            &child,
            crate::runtime_db::agent_relations::supervised_creation_records(
                &child,
                &parent.agent_id,
                "creation-task",
                None,
            ),
        ),
    ] {
        upsert_agent_identity_tx(&tx, id)?;
        crate::runtime_db::agent_relations::upsert_canonical_record_set_tx(&tx, &records)?;
    }
    seed(&tx)?;
    let mut queue = QueueEntryRecord {
        message_id: "old-pending-wake".into(),
        agent_id: child.agent_id.clone(),
        priority: Priority::Next,
        status: QueueEntryStatus::Dropped,
        created_at: at(),
        updated_at: at(),
    };
    // Genuine pre-cutover backlog: wake pending but the source queue is already
    // terminal, so ParentCleanup's original execution blocker can allow it.
    queue.updated_at += chrono::Duration::seconds(1);
    repositories::upsert_queue_entry_tx(&tx, &queue)?;
    let mut state = AgentState::new(&child.agent_id);
    state.status = AgentStatus::Stopped;
    state.total_message_count = 1;
    upsert_agent_state_tx(&tx, &state)?;
    tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[repositories::timestamp(at())])?;
    tx.commit()?;
    drop(c);
    let db = RuntimeDb::open_and_migrate(&path, home.path().join("runtime.lock"))?;
    Ok((home, db))
}
#[test]
fn original_parent_cleanup_gate_and_bulk_ack_are_atomic_and_once() -> Result<()> {
    let (_home, db) = supervised()?;
    let budget = ledger()?;
    prepared(&db, &budget)?;
    let (_, mut original, _) =
        db.agent_deletions()
            .begin_parent_cleanup(&scope().agent_id, "cleanup-parent", 1, 1)?;
    original.phase = AgentDeletionPhase::Quiesce;
    original.status = AgentDeletionStatus::Running;
    db.agent_deletions().update(&original)?;
    let (ack, _) = db.cancel_parent_cleanup_timers(&scope(), &original, at(), &budget)?;
    assert_eq!(ack.changed, 1);
    assert_eq!(ack.qualified_actual_keys.len(), 2);
    assert!(ack.invalidation.is_some());
    drop(ack);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT status FROM timer_wakes WHERE timer_id='deletion-timer'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        "cancelled"
    );
    let before = fingerprint(&db)?;
    let (repeat, _) = db.cancel_parent_cleanup_timers(&scope(), &original, at(), &budget)?;
    assert_eq!(repeat.changed, 0);
    assert!(repeat.invalidation.is_none());
    drop(repeat);
    assert_eq!(fingerprint(&db)?, before);
    // Corrupt restored accepted work must retain the original blocker, rather
    // than exploiting Scheduling qualification as new cancellation authority.
    db.connection()?.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',1) WHERE agent_id=?1",[&scope().agent_id])?;
    let before = fingerprint(&db)?;
    assert!(format!(
        "{:#}",
        db.cancel_parent_cleanup_timers(&scope(), &original, at(), &budget)
            .err()
            .unwrap()
    )
    .contains("agent_cleanup_blocked"));
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn raw_late_deletion_timer_is_cleaned_without_new_alias_and_invalidates_old_coverage() -> Result<()>
{
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    prepared(&db, &budget)?;
    db.timers()
        .upsert(&timer("raw-late", TimerStatus::Active))?;
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    let (ack, _) = db.cancel_timer_for_deletion(&scope(), &original, "raw-late", &budget)?;
    assert_eq!(ack.changed, 1);
    assert!(ack.qualified_actual_keys.is_empty());
    assert!(ack.invalidation.is_some());
    drop(ack);
    assert!(!db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM scheduling_domain_records WHERE record_id='raw-late')",
        [],
        |r| r.get::<_, bool>(0)
    )?);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn final_scheduling_failure_rolls_back_original_timer_alias_coverage_and_job() -> Result<()> {
    let (_home, db) = legacy(true, seed)?;
    let budget = ledger()?;
    prepared(&db, &budget)?;
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    db.connection()?.execute_batch("CREATE TRIGGER deny_deletion_scheduling BEFORE UPDATE ON scheduling_domain_current BEGIN SELECT RAISE(ABORT,'deletion Scheduling rollback'); END;")?;
    let before = fingerprint(&db)?;
    let err = db
        .cancel_timer_for_deletion(&scope(), &original, "deletion-timer", &budget)
        .err()
        .unwrap();
    assert!(format!("{err:#}").contains("deletion Scheduling rollback"));
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn stale_job_or_successor_scope_is_rejected_without_state_decode_or_effects() -> Result<()> {
    let (_home, db) = legacy(true, seed)?;
    let budget = ledger()?;
    prepared(&db, &budget)?;
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    let mut changed = original.clone();
    changed.phase = AgentDeletionPhase::Ingress;
    db.agent_deletions().update(&changed)?;
    let before = fingerprint(&db)?;
    assert!(db
        .cancel_timer_for_deletion(&scope(), &original, "deletion-timer", &budget)
        .is_err());
    assert_eq!(fingerprint(&db)?, before);
    db.agent_deletions().update(&original)?;
    db.connection()?.execute("UPDATE agent_identities SET status='active',payload_json=json_set(payload_json,'$.status','active','$.incarnation',2) WHERE agent_id=?1",[&scope().agent_id])?;
    db.connection()?.execute(
        "UPDATE agent_states SET payload_json='poison-successor-state' WHERE agent_id=?1",
        [&scope().agent_id],
    )?;
    let before = fingerprint(&db)?;
    let err = db
        .cancel_timer_for_deletion(&scope(), &original, "deletion-timer", &budget)
        .err()
        .unwrap();
    assert!(format!("{err:#}").contains("incarnation changed"));
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn deletion_body_budget_precedes_malformed_timer_decode_and_releases() -> Result<()> {
    let (_home, db) = legacy(true, seed)?;
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    db.connection()?.execute(
        "UPDATE timers SET payload_json=?1 WHERE timer_id='deletion-timer'",
        ["{".repeat(65 * 1024)],
    )?;
    let mut limits = StateMemoryLimits::default();
    limits.history_per_agent_bytes = 64 * 1024;
    limits.pending_per_agent_bytes = 64 * 1024;
    let budget = InputEvidenceBudgetLedger::new(limits)?;
    let before = fingerprint(&db)?;
    let err = db
        .cancel_timer_for_deletion(&scope(), &original, "deletion-timer", &budget)
        .err()
        .unwrap();
    assert!(
        matches!(
            err.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{err:#}"
    );
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn fixed_unsealed_existing_deletion_cleanup_preserves_fixed_rights_without_ready() -> Result<()> {
    let (_home, db) = legacy(false, seed)?;
    // SQLite upgrades are complete; model a persisted, unfinished file import.
    assert_eq!(
        db.connection()?.execute(
            "UPDATE episode_continuity_seeds
             SET source_kind = 'legacy_identity_import', import_required = 1
             WHERE agent_id = ?1",
            [&scope().agent_id],
        )?,
        1
    );
    assert_eq!(
        db.pending_legacy_episode_import_agent_ids()?,
        [scope().agent_id]
    );
    let budget = ledger()?;
    assert!(matches!(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(
            tx,
            &scope(),
            &request(),
            &budget
        ))
        .err()
        .unwrap()
        .downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::LegacyImportPending)
    ));
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    let (ack, _) = db.cancel_timer_for_deletion(&scope(), &original, "deletion-timer", &budget)?;
    assert_eq!(ack.qualified_actual_keys.len(), 1);
    drop(ack);
    let error = db
        .transaction(|tx| {
            prepare_original_scheduling_recovery_snapshot_tx(tx, &scope(), &request(), &budget)
        })
        .err()
        .unwrap();
    assert!(matches!(
        error.downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::LegacyImportPending)
    ));
    assert_eq!(
        db.pending_legacy_episode_import_agent_ids()?,
        [scope().agent_id]
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn deleted_cleanup_repair_preserves_retired_aliases_without_reading_poisoned_state() -> Result<()> {
    let (_home, db) = legacy(true, seed)?;
    let budget = ledger()?;
    prepared(&db, &budget)?;
    db.agent_identities()
        .tombstone_with_closed_supervision(&scope().agent_id)?;
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    assert_eq!(original.mode, AgentDeletionMode::CleanupRepair);
    db.connection()?.execute(
        "UPDATE agent_states SET payload_json='poison-retired-state' WHERE agent_id=?1",
        [&scope().agent_id],
    )?;
    let before = fingerprint(&db)?;
    let (ack, id) = db.cancel_timer_for_deletion(&scope(), &original, "deletion-timer", &budget)?;
    assert_eq!(id.status, AgentRegistryStatus::Deleted);
    assert_eq!(ack.changed, 1);
    assert!(ack.qualified_actual_keys.is_empty() && ack.invalidation.is_none());
    drop(ack);
    let after = fingerprint(&db)?;
    assert_eq!(before[4], after[4]);
    assert_eq!(before[5], after[5]);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn deletion_bounded_id_batch_preserves_original_timer_chronological_iteration() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        for id in ["same-a", "same-z", "late-b"] {
            let mut record = timer(id, TimerStatus::Active);
            if id == "late-b" {
                record.created_at += chrono::Duration::seconds(2);
                record.next_fire_at = Some(record.created_at + chrono::Duration::seconds(1));
            }
            repositories::upsert_timer_tx(tx, &record)?;
        }
        Ok(())
    })?;
    let budget = ledger()?;
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    let expected = db
        .timers()
        .recent_for_agent(&scope().agent_id, usize::MAX)?
        .into_iter()
        .filter(|t| t.status == TimerStatus::Active)
        .map(|t| t.id)
        .collect::<Vec<_>>();
    let batch = db.deletion_scheduling_batch(
        &scope(),
        &original,
        DeletionSchedulingSelection::Timers,
        &budget,
    )?;
    assert_eq!(batch.ids, expected);
    assert!(budget.report()?.observed_bytes > 0);
    drop(batch);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_per_timer_quiesce_cancels_fixed_legacy_scheduled_alias() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("legacy-scheduled", TimerStatus::Active))?;
        // Authentic old on-disk spelling, frozen before the v82/D6 cutover.
        tx.execute("UPDATE timers SET status='scheduled',payload_json=json_set(payload_json,'$.status','scheduled') WHERE timer_id='legacy-scheduled'",[])?;
        Ok(())
    })?;
    let budget = ledger()?;
    prepared(&db, &budget)?;
    assert_eq!(
        db.timers().latest("legacy-scheduled")?.unwrap().status,
        TimerStatus::Active
    );
    let original = job(&db, AgentDeletionPhase::Quiesce)?;
    let batch = db.deletion_scheduling_batch(
        &scope(),
        &original,
        DeletionSchedulingSelection::Timers,
        &budget,
    )?;
    assert_eq!(batch.ids, vec!["legacy-scheduled".to_owned()]);
    drop(batch);
    let (ack, _) =
        db.cancel_timer_for_deletion(&scope(), &original, "legacy-scheduled", &budget)?;
    assert_eq!(ack.changed, 1);
    assert_eq!(
        ack.qualified_actual_keys,
        BTreeSet::from([SchedulingKey::new(
            SchedulingFamily::Timer,
            "legacy-scheduled"
        )])
    );
    assert!(ack.invalidation.is_some());
    drop(ack);
    assert_eq!(
        db.timers().latest("legacy-scheduled")?.unwrap().status,
        TimerStatus::Cancelled
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
