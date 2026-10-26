use super::*;

fn charge(
    db: &RuntimeDb,
    record: &TaskResultSettlementRecord,
    root: &str,
    now: DateTime<Utc>,
) -> bool {
    db.transaction(|tx| charge_recovery_admission_tx(tx, &record.result_identity, root, now))
        .unwrap()
}

fn latest(db: &RuntimeDb, record: &TaskResultSettlementRecord) -> TaskResultSettlementRecord {
    db.task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap()
}

fn exhaust(db: &RuntimeDb, record: &TaskResultSettlementRecord) -> DateTime<Utc> {
    let mut now = record.next_recheck_at.unwrap();
    for attempt in 1..=TASK_RESULT_RECOVERY_LIMIT {
        assert!(charge(db, record, &format!("root-{attempt}"), now));
        let updated = latest(db, record);
        assert_eq!(updated.recovery_attempts, attempt);
        if attempt < TASK_RESULT_RECOVERY_LIMIT {
            let delay = [60, 120, 240, 480][(attempt - 1) as usize];
            assert_eq!(
                updated.next_recheck_at,
                Some(now + Duration::seconds(delay))
            );
            now = updated.next_recheck_at.unwrap();
        }
    }
    now
}

#[test]
fn finite_recovery_diagnostic_preserves_namespace_epoch_cas_and_idempotence() -> Result<()> {
    let (_dir, db) = runtime_db();
    let mut record = pending_record(0);
    record.recovery_paused_at = Some(record.updated_at);
    insert(&db, &record);
    let now = record.updated_at + Duration::seconds(1);
    let limits = TaskResultWriteLimits::default();
    for (agent, epoch) in [("other-agent", 0), ("agent-a", 1)] {
        let ack = db.transaction(|tx| {
            select_mark_recovery_diagnostic_tx(
                tx, agent, &record.result_identity, epoch, now, limits,
            )?
            .materialize(limits)?
            .execute()
        })?;
        assert_eq!(ack.outcome, TaskResultWriteOutcome::Unit);
        assert!(ack.actual_result_ids.is_empty());
        assert!(latest(&db, &record).recovery_diagnostic_at.is_none());
    }
    db.transaction(|tx| {
        let prepared = select_mark_recovery_diagnostic_tx(
            tx, "agent-a", &record.result_identity, 0, now, limits,
        )?
        .materialize(limits)?;
        let mut changed = by_identity_tx(tx, &record.result_identity)?.unwrap();
        changed.recovery_epoch = 1;
        update_tx(tx, &changed)?;
        assert!(prepared.execute().is_err());
        Ok(())
    })?;
    assert!(latest(&db, &record).recovery_diagnostic_at.is_none());
    for (at, expected) in [(now, true), (now + Duration::seconds(1), false)] {
        let ack = db.transaction(|tx| {
            select_mark_recovery_diagnostic_tx(
                tx, "agent-a", &record.result_identity, 1, at, limits,
            )?
            .materialize(limits)?
            .execute()
        })?;
        assert_eq!(ack.outcome, TaskResultWriteOutcome::Unit);
        assert_eq!(
            ack.actual_result_ids,
            if expected {
                std::collections::BTreeSet::from([record.result_identity.clone()])
            } else {
                Default::default()
            }
        );
        assert_eq!(latest(&db, &record).recovery_diagnostic_at, Some(now));
    }
    let unpaused = pending_record(1);
    insert(&db, &unpaused);
    db.transaction(|tx| {
        let ack = select_mark_recovery_diagnostic_tx(
            tx, "agent-a", &unpaused.result_identity, 0, now, limits,
        )?
        .materialize(limits)?
        .execute()?;
        assert!(ack.actual_result_ids.is_empty());
        assert!(select_mark_recovery_diagnostic_tx(
            tx,
            "agent-a",
            &record.result_identity,
            1,
            now,
            TaskResultWriteLimits {
                record_limit: 1,
                byte_limit: 1,
            },
        )
        .is_err());
        Ok(())
    })?;
    assert!(latest(&db, &unpaused).recovery_diagnostic_at.is_none());
    Ok(())
}

#[test]
fn recovery_budget_counts_roots_not_probes_or_fallback_hops() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    let unselected = pending_record(1);
    insert(&db, &record);
    insert(&db, &unselected);
    let now = record.next_recheck_at.unwrap();
    for _ in 0..3 {
        db.task_result_settlements()
            .due_deferred("agent-a", now, 8)
            .unwrap();
        db.task_result_settlements()
            .mark_deferred(&record.message_id, "execution_lane_busy", now, now)
            .unwrap();
    }
    assert_eq!(latest(&db, &record).recovery_attempts, 0);
    assert!(charge(&db, &record, "root-a", now));
    assert!(charge(&db, &record, "root-a", now));
    assert!(!charge(&db, &record, "root-b", now + Duration::seconds(59)));
    let charged = latest(&db, &record);
    assert_eq!(charged.recovery_attempts, 1);
    assert_eq!(latest(&db, &unselected).recovery_attempts, 0);
    db.task_result_settlements()
        .wake_deferred_for_owner("agent-a", "work-a", now)
        .unwrap();
    db.task_result_settlements()
        .mark_deferred(&record.message_id, "outstanding_wake", now, now)
        .unwrap();
    assert_eq!(
        latest(&db, &record).next_recheck_at,
        charged.next_recheck_at
    );
    assert!(charge(
        &db,
        &record,
        "root-b",
        charged.next_recheck_at.unwrap()
    ));
    assert!(charge(&db, &record, "root-a", now));
    assert_eq!(latest(&db, &record).recovery_attempts, 2);
}

#[test]
fn recovery_budget_charge_rolls_back_with_canonical_admission() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    insert(&db, &record);
    let now = record.next_recheck_at.unwrap();
    let failed: Result<()> = db.transaction(|tx| {
        assert!(charge_recovery_admission_tx(
            tx,
            &record.result_identity,
            "root-a",
            now
        )?);
        bail!("admission failed");
    });
    assert!(failed.is_err());
    assert_eq!(latest(&db, &record).recovery_attempts, 0);
    assert!(charge(&db, &record, "root-a", now));
    assert_eq!(latest(&db, &record).recovery_attempts, 1);
}

#[test]
fn exhausted_budget_preserves_obligation_and_does_not_starve_other_results() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    insert(&db, &record);
    let now = exhaust(&db, &record);
    let paused = latest(&db, &record);
    assert_eq!(paused.state, TaskResultSettlementState::PersistedPending);
    assert!(paused.recovery_paused_at.is_some());
    assert!(paused.next_recheck_at.is_none());
    assert!(!charge(&db, &record, "root-six", now + Duration::days(1)));
    assert!(charge(&db, &record, "root-5", now));
    db.task_result_settlements()
        .wake_deferred_for_owner("agent-a", "work-a", now)
        .unwrap();
    db.task_result_settlements()
        .mark_deferred(&record.message_id, "owner_runnable", now, now)
        .unwrap();
    assert_eq!(latest(&db, &record), paused);
    assert!(db
        .task_result_settlements()
        .next_recheck_at("agent-a")
        .unwrap()
        .is_none());
    let other = pending_record(1);
    insert(&db, &other);
    let due = db
        .task_result_settlements()
        .due_deferred("agent-a", now, 1)
        .unwrap();
    assert_eq!(due[0].result_identity, other.result_identity);
    let owner = db
        .task_result_settlements()
        .unsettled_for_owner("agent-a", Some("work-a"), 1)
        .unwrap();
    assert_eq!(owner[0].result_identity, other.result_identity);
    assert!(db
        .task_result_settlements()
        .unsettled_for_agent("agent-a", 8)
        .unwrap()
        .iter()
        .any(|entry| entry.result_identity == record.result_identity));
    let selected = std::collections::BTreeSet::from([record.result_identity.clone()]);
    assert!(db
        .task_result_settlements()
        .admit_unsettled_snapshot(
            "agent-a",
            Some("work-a"),
            "unrelated-input",
            None,
            Some(&selected),
            now,
        )
        .unwrap()
        .is_empty());
    assert_eq!(latest(&db, &record), paused);
    // Explicit admission of this source is not an unrelated input's batch.
    assert!(db
        .task_result_settlements()
        .admit_message("agent-a", &record.message_id, "normal-input", now)
        .unwrap()
        .is_some());
}

#[test]
fn charged_backoff_survives_restart_without_advancing_on_owner_wake() {
    let (dir, db) = runtime_db();
    let record = pending_record(0);
    insert(&db, &record);
    let now = record.next_recheck_at.unwrap();
    assert!(charge(&db, &record, "root-a", now));
    drop(db);
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    assert_eq!(
        latest(&db, &record).next_recheck_at,
        Some(now + Duration::seconds(60))
    );
    assert_eq!(
        db.task_result_settlements()
            .wake_deferred_for_owner("agent-a", "work-a", now)
            .unwrap(),
        0
    );
    assert!(!charge(&db, &record, "root-b", now + Duration::seconds(59)));
    assert!(charge(&db, &record, "root-a", now));
    assert!(charge(&db, &record, "root-b", now + Duration::seconds(60)));
    assert_eq!(latest(&db, &record).recovery_attempts, 2);
}

#[test]
fn recovery_budget_and_pause_survive_restart_and_only_targeted_rearm_resets_epoch() {
    let (dir, db) = runtime_db();
    let record = pending_record(0);
    let other = pending_record(1);
    insert(&db, &record);
    insert(&db, &other);
    let now = exhaust(&db, &record);
    drop(db);
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    assert_eq!(latest(&db, &record).recovery_attempts, 5);
    assert!(latest(&db, &record).next_recheck_at.is_none());
    assert!(!charge(
        &db,
        &record,
        "after-restart",
        now + Duration::days(1)
    ));
    let repo = db.task_result_settlements();
    assert!(repo
        .rearm_recovery("other-agent", &record.result_identity, now)
        .is_err());
    assert!(repo
        .rearm_recovery("agent-a", &record.result_identity, now)
        .unwrap());
    assert!(!repo
        .rearm_recovery("agent-a", &record.result_identity, now)
        .unwrap());
    let rearmed = latest(&db, &record);
    assert_eq!(rearmed.recovery_epoch, 1);
    assert_eq!(rearmed.recovery_attempts, 0);
    assert_eq!(rearmed.next_recheck_at, Some(now + Duration::seconds(30)));
    assert_eq!(latest(&db, &other).recovery_epoch, 0);
    assert!(charge(
        &db,
        &record,
        "root-1",
        rearmed.next_recheck_at.unwrap()
    ));
    assert_eq!(latest(&db, &record).recovery_attempts, 1);
}

#[test]
fn recovery_budget_migration_preserves_legacy_payload_and_defaults() {
    let mut legacy = pending_record(0);
    legacy.state = TaskResultSettlementState::CallerAdmitted;
    legacy.activation_id = Some("legacy-root".into());
    legacy.admitted_at = Some(legacy.created_at);
    let mut payload = serde_json::to_value(&legacy).unwrap();
    for field in [
        "recovery_epoch",
        "recovery_attempts",
        "last_recovery_root_attempt_id",
        "last_recovery_admitted_at",
        "recovery_paused_at",
        "recovery_diagnostic_at",
    ] {
        payload.as_object_mut().unwrap().remove(field);
    }
    let payload = serde_json::to_string(&payload).unwrap();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE task_result_settlements (
           result_identity TEXT PRIMARY KEY, agent_id TEXT, state TEXT, payload_json TEXT
         );",
        )
        .unwrap();
    connection.execute(
        "INSERT INTO task_result_settlements VALUES ('result-00', 'agent-a', 'caller_admitted', ?1)",
        [&payload],
    ).unwrap();
    let migration = crate::runtime_db::migrations::MIGRATIONS
        .iter()
        .find(|migration| migration.version == 83)
        .unwrap();
    connection.execute_batch(migration.sql).unwrap();
    let (stored, attempts, paused): (String, u32, Option<String>) = connection.query_row(
        "SELECT payload_json, recovery_attempts, recovery_paused_at FROM task_result_settlements",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(stored, payload);
    assert_eq!(attempts, 0);
    assert!(paused.is_none());
    let restored: TaskResultSettlementRecord = serde_json::from_str(&stored).unwrap();
    assert_eq!(restored.recovery_attempts, 0);
    assert_eq!(restored.recovery_epoch, 0);
    assert!(restored.recovery_paused_at.is_none());
    assert_eq!(restored.result_identity, "result-00");
    assert_eq!(restored.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(restored.activation_id.as_deref(), Some("legacy-root"));
}
