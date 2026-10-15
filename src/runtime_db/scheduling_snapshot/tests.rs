use super::*;
#[path = "cold_query_tests.rs"]
mod cold_query_tests;
#[path = "foreign_owner_tests.rs"]
mod foreign_owner_tests;
#[path = "saved_coverage_budget_tests.rs"]
mod saved_coverage_budget_tests;
#[path = "scope_state_native_tests.rs"]
mod scope_state_native_tests;
#[path = "token_discovery_tests.rs"]
mod token_discovery_tests;
use crate::{
    config::StateMemoryLimits,
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, CallbackDeliveryMode, ExternalTriggerRecord,
        ExternalTriggerScope, ExternalTriggerStatus, MessageBody, MessageEnvelope, MessageKind,
        MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus, TimerRecord, TimerStatus,
    },
};
use chrono::Utc;
pub(super) fn at() -> chrono::DateTime<Utc> {
    "2026-10-01T00:00:00.123456789Z".parse().unwrap()
}
pub(super) fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "scheduling-agent".into(),
        identity_incarnation: 1,
    }
}
pub(super) fn identity() -> AgentIdentityRecord {
    let mut x = AgentIdentityRecord::new(
        "scheduling-agent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    x.created_at = at();
    x.updated_at = at();
    x
}
pub(super) fn timer(id: &str, status: TimerStatus) -> TimerRecord {
    TimerRecord {
        id: id.into(),
        agent_id: scope().agent_id,
        created_at: at(),
        duration_ms: 1000,
        interval_ms: Some(1000),
        repeat: true,
        status,
        summary: Some(id.into()),
        next_fire_at: Some(at() + chrono::Duration::seconds(1)),
        last_fired_at: Some(at()),
        fire_count: 5,
    }
}
pub(super) fn trigger() -> ExternalTriggerRecord {
    ExternalTriggerRecord {
        external_trigger_id: "original-trigger".into(),
        target_agent_id: scope().agent_id,
        scope: ExternalTriggerScope::Agent,
        delivery_mode: CallbackDeliveryMode::WakeHint,
        token: Some("original-token".into()),
        token_hash: "original-hash".into(),
        status: ExternalTriggerStatus::Active,
        created_at: at(),
        revoked_at: None,
        last_delivered_at: None,
        delivery_count: 0,
    }
}
pub(super) fn request() -> SchedulingDomainLoadRequest {
    SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    }
}
pub(super) fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
pub(super) fn legacy(
    sealed: bool,
    seed: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("runtime.sqlite");
    let mut conn = Connection::open(&path)?;
    ensure_migration_table(&conn)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut conn, migration)?;
    }
    {
        let tx = conn.transaction()?;
        upsert_agent_identity_tx(&tx, &identity())?;
        upsert_agent_state_tx(&tx, &AgentState::new(&scope().agent_id))?;
        if sealed {
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[repositories::timestamp(at())])?;
        }
        seed(&tx)?;
        tx.commit()?;
    }
    drop(conn);
    let db = RuntimeDb::open_and_migrate(&path, home.path().join("runtime.lock"))?;
    Ok((home, db))
}
pub(super) fn wake(
    tx: &Transaction<'_>,
    timer_id: &str,
    message_id: &str,
    count: u64,
) -> Result<()> {
    let mut message = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: timer_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "original wake".into(),
        },
    );
    message.id = message_id.into();
    message.created_at = at();
    message.metadata = Some(serde_json::json!({"timer_fire_count":count}));
    let (message, _) = append_message_tx(tx, &message)?;
    repositories::upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: scope().agent_id,
            priority: message.priority,
            status: QueueEntryStatus::Queued,
            created_at: at(),
            updated_at: at(),
        },
    )?;
    let mut state = AgentState::new(&scope().agent_id);
    let n: u64 = tx.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
        [&scope().agent_id],
        |r| r.get(0),
    )?;
    state.pending = usize::try_from(n)?;
    state.total_message_count = usize::try_from(n)?;
    upsert_agent_state_tx(tx, &state)?;
    tx.execute("INSERT INTO timer_wakes(timer_id,message_id,fire_count,status,created_at,updated_at) VALUES(?1,?2,?3,'pending',?4,?4)",params![timer_id,message_id,count,repositories::timestamp(at())])?;
    Ok(())
}
#[test]
fn fixed_actual_pending_wake_keeps_its_count_and_completed_timer_closure() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("repeating", TimerStatus::Active))?;
        repositories::upsert_timer_tx(tx, &timer("completed", TimerStatus::Completed))?;
        repositories::upsert_external_trigger_tx(tx, &trigger())?;
        wake(tx, "repeating", "wake-one", 1)?;
        wake(tx, "completed", "wake-completed", 3)
    })?;
    let budget = ledger()?;
    // A narrow prompt request still installs the complete live body frontier.
    let mut load = request();
    load.capture.include_live = false;
    load.capture.history_limit = 0;
    let snapshot = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget)?.require_current()
    })?;
    let capture = snapshot.resident.capture(&SchedulingCaptureRequest {
        include_live: true,
        history_limit: 0,
        required_keys: BTreeSet::new(),
    })?;
    assert_eq!(capture.active_timer_count()?, 1);
    assert_eq!(capture.pending_wake("repeating")?.unwrap().fire_count, 1);
    assert_eq!(capture.pending_wake("completed")?.unwrap().fire_count, 3);
    let parent = capture
        .lookup(&SchedulingKey::new(SchedulingFamily::Timer, "completed"))?
        .unwrap();
    assert!(
        matches!(&parent.fact,CanonicalSchedulingFact::Timer(t) if t.status==TimerStatus::Completed)
    );
    assert_eq!(
        capture.active_default_trigger()?.unwrap().token.as_deref(),
        Some("original-token")
    );
    assert_eq!(snapshot.input_roots.len(), 2);
    Ok(())
}
#[test]
fn fixed_deleting_is_named_recovery_only_and_retired_never_reads_successor_state() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("admitted", TimerStatus::Active))?;
        let mut id = identity();
        id.status = AgentRegistryStatus::Deleting;
        id.updated_at += chrono::Duration::seconds(1);
        id.revision += 1;
        upsert_agent_identity_tx(tx, &id)
    })?;
    let budget = ledger()?;
    assert!(matches!(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(
            tx,
            &scope(),
            &request(),
            &budget
        ))?,
        SchedulingBootstrapSnapshot::Retired { .. }
    ));
    let original = db.transaction(|tx| {
        prepare_original_scheduling_recovery_snapshot_tx(tx, &scope(), &request(), &budget)?
            .require_current()
    })?;
    assert_eq!(
        original
            .resident
            .capture(&request().capture)?
            .active_timer_count()?,
        1
    );
    db.transaction(|tx|{tx.execute("UPDATE agent_identities SET status='active',payload_json=json_set(payload_json,'$.incarnation',2,'$.status','active') WHERE agent_id=?1",[&scope().agent_id])?;tx.execute("UPDATE agent_states SET payload_json='poison-successor-state' WHERE agent_id=?1",[&scope().agent_id])?;Ok(())})?;
    assert!(matches!(
        db.transaction(|tx| prepare_original_scheduling_recovery_snapshot_tx(
            tx,
            &scope(),
            &request(),
            &budget
        ))?,
        SchedulingBootstrapSnapshot::Retired { .. }
    ));
    assert_eq!(
        original
            .resident
            .capture(&request().capture)?
            .active_timer_count()?,
        1
    );
    Ok(())
}
#[test]
fn raw_late_live_cannot_enter_complete_coverage_or_partially_mint_aliases() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("fixed", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    db.timers().upsert(&timer("late", TimerStatus::Active))?;
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget))
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<SchedulingSnapshotError>(),Some(SchedulingSnapshotError::NeedsPreparation(k)) if k.id=="late")
    );
    let rows: u64 =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM scheduling_domain_records", [], |r| {
                r.get(0)
            })?;
    assert_eq!(rows, 0);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn actual_full_normalized_timer_projection_and_pruned_alias_remain_strict() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("fixed", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    let original = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE timers SET next_fire_at='2026-10-02T00:00:00.000Z' WHERE timer_id='fixed'",
            [],
        )?;
        Ok(())
    })?;
    assert!(db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget))
        .is_err());
    db.transaction(|tx|{repositories::upsert_timer_tx(tx,&timer("fixed",TimerStatus::Active))?;tx.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE record_id='fixed'",[])?;Ok(())})?;
    assert!(db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget))
        .is_err());
    assert_eq!(
        original
            .resident
            .capture(&request().capture)?
            .active_timer_count()?,
        1
    );
    Ok(())
}
#[test]
fn malformed_oversized_normalized_material_fails_budget_before_decode_and_releases_guards(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("fixed", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE timers SET summary=?1,payload_json=?2 WHERE timer_id='fixed'",
            params!["n".repeat(256 * 1024), "{bad".repeat(64 * 1024)],
        )?;
        Ok(())
    })?;
    let mut load = request();
    load.byte_limit = 128 * 1024;
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM scheduling_domain_records", [], |r| {
                r.get::<_, u64>(0)
            })?,
        0
    );
    Ok(())
}
#[test]
fn first_json_seal_uses_original_fixed_and_actual_received_inventory_not_late_tail() -> Result<()> {
    let (_home, db) = legacy(false, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    assert!(matches!(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(
            tx,
            &scope(),
            &request(),
            &budget
        ))
        .unwrap_err()
        .downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::LegacyImportPending)
    ));
    db.timers()
        .upsert(&timer("late-not-imported", TimerStatus::Completed))?;
    let mut updated = timer("original", TimerStatus::Active);
    updated.fire_count = 6;
    db.timers()
        .import_legacy(vec![updated, timer("actual-imported", TimerStatus::Active)])?;
    db.context_episodes().import_legacy(Vec::new())?;
    let snapshot = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    assert_eq!(
        snapshot
            .resident
            .capture(&request().capture)?
            .active_timer_count()?,
        2
    );
    let mut load = request();
    load.capture.required_keys.insert(SchedulingKey::new(
        SchedulingFamily::Timer,
        "late-not-imported",
    ));
    assert!(matches!(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget))
            .unwrap_err()
            .downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::NeedsPreparation(_))
    ));
    assert!(db
        .transaction(|tx| {
            tx.execute("UPDATE scheduling_fixed_manifests SET sealed=0", [])?;
            Ok(())
        })
        .is_err());
    Ok(())
}
#[test]
fn selected_capture_owns_only_selected_arcs_and_shared_weak_ledger_releases() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("selected", TimerStatus::Completed))?;
        repositories::upsert_timer_tx(tx, &timer("unselected", TimerStatus::Completed))
    })?;
    let budget = ledger()?;
    let snapshot = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    let SchedulingCurrentSnapshot {
        facts,
        resident,
        input_roots,
        _read_material,
        ..
    } = snapshot;
    drop(facts);
    drop(input_roots);
    drop(_read_material);
    let all = resident.selected_install_records();
    let selected_weak =
        Arc::downgrade(all.iter().find(|x| x.locator.key.id == "selected").unwrap());
    let other_weak = Arc::downgrade(
        all.iter()
            .find(|x| x.locator.key.id == "unselected")
            .unwrap(),
    );
    drop(all);
    let capture = resident.capture(&SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "selected")]),
    })?;
    assert_eq!(capture.records.len(), 1);
    drop(resident);
    assert!(other_weak.upgrade().is_none());
    assert!(selected_weak.upgrade().is_some());
    assert_eq!(budget.report()?.physical_records.allocations, 1);
    drop(capture);
    assert!(selected_weak.upgrade().is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn historical_missing_table_is_not_a_later_empty_or_adoption_proof() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("fixed", TimerStatus::Completed))?;
        tx.execute("DROP TABLE external_triggers", [])?;
        Ok(())
    })?;
    let budget = ledger()?;
    let schema = crate::runtime_db::migrations::MIGRATIONS
        .iter()
        .find(|m| m.version == 4)
        .unwrap()
        .sql;
    db.transaction(|tx| {
        tx.execute_batch(schema)?;
        Ok(())
    })?;
    let mut load = request();
    load.capture.required_keys.insert(SchedulingKey::new(
        SchedulingFamily::ExternalTrigger,
        "missing-at-cutover",
    ));
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::HistoricalUnavailable(_))
        ),
        "{error:#}"
    );
    db.external_triggers().upsert(&trigger())?;
    assert!(matches!(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(
            tx,
            &scope(),
            &request(),
            &budget
        ))
        .unwrap_err()
        .downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::NeedsPreparation(_))
    ));
    Ok(())
}

#[test]
fn fresh_successor_does_not_inherit_old_fixed_live_rows_but_explicit_old_id_is_strict() -> Result<()>
{
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("old-original", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    db.transaction(|tx| {
        let mut next = identity();
        next.incarnation = 2;
        next.updated_at += chrono::Duration::seconds(2);
        upsert_agent_identity_tx(tx, &next)?;
        upsert_agent_state_tx(tx, &AgentState::new(&scope().agent_id))?;
        Ok(())
    })?;
    let next = InputEvidenceScope {
        agent_id: scope().agent_id,
        identity_incarnation: 2,
    };
    let current = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &next, &request(), &budget)?.require_current()
    })?;
    assert_eq!(
        current
            .resident
            .capture(&request().capture)?
            .active_timer_count()?,
        0
    );
    let mut load = request();
    load.capture
        .required_keys
        .insert(SchedulingKey::new(SchedulingFamily::Timer, "old-original"));
    assert!(matches!(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &next, &load, &budget))
            .unwrap_err()
            .downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::NeedsPreparation(_))
    ));
    Ok(())
}
#[test]
fn original_live_normalized_owner_drift_is_not_complete_empty() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    db.transaction(|tx|{tx.execute("UPDATE timers SET agent_id='foreign-owner',payload_json='foreign-body-must-not-decode' WHERE timer_id='original'",[])?;Ok(())})?;
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ForeignRecord(key)) if key == &SchedulingKey::new(SchedulingFamily::Timer, "original")
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn no_state_and_preexisting_deleted_never_construct_ready_or_new_qualification() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("old", TimerStatus::Active))?;
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [&scope().agent_id],
        )?;
        Ok(())
    })?;
    let budget = ledger()?;
    assert!(matches!(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(
            tx,
            &scope(),
            &request(),
            &budget
        ))?,
        SchedulingBootstrapSnapshot::Retired { .. }
    ));
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM scheduling_domain_records", [], |r| {
                r.get::<_, u64>(0)
            })?,
        0
    );
    let (_home, deleted) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("deleted-old", TimerStatus::Active))?;
        // Seed an already canonical v81 tombstone, before the new cutover actor.
        tx.execute("UPDATE agent_identities SET status='deleted',payload_json=json_set(payload_json,'$.status','deleted') WHERE agent_id=?1",[&scope().agent_id])?;
        assert_eq!(
            tx.query_row(
                "SELECT status FROM agent_identities WHERE agent_id=?1",
                [&scope().agent_id],
                |r| r.get::<_, String>(0)
            )?,
            "deleted"
        );
        Ok(())
    })?;
    assert_eq!(
        deleted.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_captures",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    assert!(matches!(
        deleted.transaction(|tx| prepare_original_scheduling_recovery_snapshot_tx(
            tx,
            &scope(),
            &request(),
            &budget
        ))?,
        SchedulingBootstrapSnapshot::Retired { .. }
    ));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn fixed_history128_preserves_order_and_explicit_old_id_without_a_late_tail() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        for n in 0..130 {
            repositories::upsert_timer_tx(
                tx,
                &timer(&format!("closed-{n:03}"), TimerStatus::Completed),
            )?;
        }
        Ok(())
    })?;
    let budget = ledger()?;
    db.timers()
        .upsert(&timer("aaa-late", TimerStatus::Completed))?;
    // The 128 original Timer projections alone retain 25 native nodes each,
    // charged at the actual eight-copy gate: 25_600 exceeds the old 4096 cap.
    let mut history_request = request();
    history_request.record_limit = MAX_RECORDS;
    let loaded = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &history_request, &budget)?
            .require_current()
    })?;
    let history = &loaded.resident.coverage().history[&SchedulingFamily::Timer];
    assert_eq!(history.keys.len(), 128);
    assert!(!history.is_exhaustive);
    assert_eq!(history.keys[0].id, "closed-000");
    assert_eq!(history.keys[127].id, "closed-127");
    let mut explicit = request();
    explicit.capture.history_limit = 0;
    explicit.capture.include_live = false;
    explicit
        .capture
        .required_keys
        .insert(SchedulingKey::new(SchedulingFamily::Timer, "closed-129"));
    // Saved observations and both original history orders coexist with the
    // next exact selection. Even 387 saved entries + 128 row reads, each at
    // the real eight-copy gate, exceed this fixture's original 4096 records.
    let before: (String, u64) = db.connection()?.query_row(
        "SELECT coverage_json,(SELECT COUNT(*) FROM scheduling_domain_records) FROM scheduling_domain_current WHERE agent_id=?1",
        [&scope().agent_id], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let before_bytes = budget.report()?.observed_bytes;
    let error = db
        .transaction(|tx| {
            prepare_scheduling_domain_snapshot_tx(tx, &scope(), &explicit, &budget)?
                .require_current()
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(db.connection()?.query_row(
        "SELECT coverage_json,(SELECT COUNT(*) FROM scheduling_domain_records) FROM scheduling_domain_current WHERE agent_id=?1",
        [&scope().agent_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
    )?, before);
    assert_eq!(budget.report()?.observed_bytes, before_bytes);
    explicit.record_limit = MAX_RECORDS;
    let loaded = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &explicit, &budget)?.require_current()
    })?;
    let captured = loaded.resident.capture(&explicit.capture)?;
    assert_eq!(captured.records.len(), 1);
    assert_eq!(captured.records[0].locator.key.id, "closed-129");
    Ok(())
}
#[test]
fn global_absent_and_pruned_are_distinct_and_old_selected_bytes_survive() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("old-pruned", TimerStatus::Completed))
    })?;
    let budget = ledger()?;
    let original = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    db.transaction(|tx|{
 // Explicit canonical removal fixture plus its original scoped availability.
 tx.execute("DELETE FROM timers WHERE timer_id='old-pruned'",[])?;
 tx.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE record_id='old-pruned'",[])?;Ok(())})?;
    let mut absent = request();
    absent.capture.required_keys.insert(SchedulingKey::new(
        SchedulingFamily::Timer,
        "globally-absent",
    ));
    let original_loaded = original.resident.coverage().history[&SchedulingFamily::Timer]
        .loaded_keys
        .clone();
    let current = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &absent, &budget)?.require_current()
    })?;
    assert_eq!(
        current.resident.coverage().history[&SchedulingFamily::Timer].loaded_keys,
        original_loaded
    );
    assert!(current
        .resident
        .capture(&absent.capture)?
        .lookup(&SchedulingKey::new(
            SchedulingFamily::Timer,
            "globally-absent"
        ))?
        .is_none());
    let mut pruned = request();
    pruned
        .capture
        .required_keys
        .insert(SchedulingKey::new(SchedulingFamily::Timer, "old-pruned"));
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &pruned, &budget))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::Pruned(_))
        ),
        "{error:#}"
    );
    assert!(original
        .resident
        .capture(&request().capture)?
        .lookup(&SchedulingKey::new(SchedulingFamily::Timer, "old-pruned"))?
        .is_some());
    Ok(())
}

#[test]
fn first_original_writer_initializes_final_revision_and_noop_keeps_full_install_records(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    let mut changed = timer("original", TimerStatus::Active);
    changed.fire_count = 6;
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "original")]);
    let mut facts = db.transaction(|tx| {
        let proof = prepare_scheduling_write_tx(tx, &scope(), &keys, &request(), &budget)?;
        repositories::upsert_timer_tx(tx, &changed)?;
        let state = ContextDurableRead::new(tx, &scope().agent_id)
            .scope_facts()?
            .state
            .unwrap();
        register_scheduling_writes_tx(tx, &proof, &keys, &state, &request())
    })?;
    assert_eq!((facts.delta.base_revision, facts.delta.revision), (1, 2));
    assert!(facts.requires_prepared_domain);
    assert!(facts
        .delta
        .records
        .iter()
        .all(|r| r.locator.revision <= facts.delta.revision));
    let held = facts.take_budget_views();
    let current = db.transaction(|tx| {
        load_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    let mut noop = db.transaction(|tx| {
        let proof = prepare_scheduling_write_tx(tx, &scope(), &keys, &request(), &budget)?;
        repositories::upsert_timer_tx(tx, &changed)?;
        let state = ContextDurableRead::new(tx, &scope().agent_id)
            .scope_facts()?
            .state
            .unwrap();
        register_scheduling_writes_tx(tx, &proof, &keys, &state, &request())
    })?;
    assert_eq!((noop.delta.base_revision, noop.delta.revision), (2, 2));
    assert!(!noop.delta.records.is_empty());
    let views = noop.take_budget_views();
    let installed = current.resident.with_committed(&noop.delta)?;
    assert_eq!(
        installed
            .capture(&request().capture)?
            .active_timer_count()?,
        1
    );
    drop(views);
    drop(held);
    Ok(())
}
#[test]
fn finalizer_fault_rolls_back_business_aliases_and_scope_and_original_cleanup_is_existing_only(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    let before = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    let observed = budget.report()?.observed_bytes;
    db.transaction(|tx|{tx.execute_batch("CREATE TRIGGER reject_scheduling_finalizer BEFORE UPDATE ON scheduling_domain_current WHEN NEW.revision>OLD.revision BEGIN SELECT RAISE(ABORT,'fixture Scheduling epilogue'); END;")?;Ok(())})?;
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "new")]);
    let error = db
        .transaction(|tx| {
            let proof = prepare_scheduling_write_tx(tx, &scope(), &keys, &request(), &budget)?;
            repositories::upsert_timer_tx(tx, &timer("new", TimerStatus::Active))?;
            let state = ContextDurableRead::new(tx, &scope().agent_id)
                .scope_facts()?
                .state
                .unwrap();
            register_scheduling_writes_tx(tx, &proof, &keys, &state, &request())
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("fixture Scheduling epilogue"));
    assert!(db.timers().latest("new")?.is_none());
    assert_eq!(budget.report()?.observed_bytes, observed);
    assert_eq!(
        before
            .resident
            .capture(&request().capture)?
            .active_timer_count()?,
        1
    );
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_scheduling_finalizer;")?;
        let mut id = identity();
        id.status = AgentRegistryStatus::Deleting;
        id.revision += 1;
        id.updated_at += chrono::Duration::seconds(1);
        upsert_agent_identity_tx(tx, &id)?;
        Ok(())
    })?;
    assert!(db
        .transaction(|tx| {
            prepare_scheduling_write_tx(tx, &scope(), &keys, &request(), &budget)?;
            Ok(())
        })
        .is_err());
    assert!(db
        .transaction(|tx| {
            prepare_original_scheduling_cleanup_tx(tx, &scope(), &keys, &request(), &budget)?;
            Ok(())
        })
        .is_err());
    let existing = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "original")]);
    db.transaction(|tx| {
        let proof =
            prepare_original_scheduling_cleanup_tx(tx, &scope(), &existing, &request(), &budget)?;
        let mut value = timer("original", TimerStatus::Cancelled);
        value.next_fire_at = None;
        repositories::upsert_timer_tx(tx, &value)?;
        let state = ContextDurableRead::new(tx, &scope().agent_id)
            .scope_facts()?
            .state
            .unwrap();
        let facts = register_scheduling_writes_tx(tx, &proof, &existing, &state, &request())?;
        assert_eq!(facts.delta.coverage.active_timers.len(), 0);
        Ok(())
    })?;
    Ok(())
}

#[path = "timer_writer_tests.rs"]
mod timer_writer_tests;

#[path = "timer_kernel_tests.rs"]
mod timer_kernel_tests;

#[path = "history_order_tests.rs"]
mod history_order_tests;

#[test]
fn pruned_history_without_original_loaded_order_keeps_metadata_and_rejects_time_fabrication(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("pruned-original", TimerStatus::Completed))
    })?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()?;
        tx.execute("DELETE FROM timers WHERE timer_id='pruned-original'", [])?;
        tx.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE record_id='pruned-original'", [])?;
        tx.execute("DELETE FROM scheduling_domain_current", [])?;
        // A same-ID raw successor cannot revive scoped Pruned membership.
        let mut foreign = timer("pruned-original", TimerStatus::Completed);
        foreign.agent_id = "foreign-successor".into();
        repositories::upsert_timer_tx(tx, &foreign)?;
        tx.execute("UPDATE timers SET payload_json='must-not-read-successor-body' WHERE timer_id='pruned-original'", [])?;
        Ok(())
    })?;
    let current = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    let history = &current.resident.coverage().history[&SchedulingFamily::Timer];
    assert_eq!(
        history.keys,
        vec![SchedulingKey::new(
            SchedulingFamily::Timer,
            "pruned-original"
        )]
    );
    assert!(history.loaded_keys.is_empty());
    assert!(matches!(
        current
            .resident
            .capture(&request().capture)
            .unwrap_err()
            .downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::NeedsHistory)
    ));
    let live = SchedulingCaptureRequest {
        include_live: true,
        history_limit: 0,
        required_keys: Default::default(),
    };
    assert_eq!(current.resident.capture(&live)?.active_timer_count()?, 0);
    let mut exact = live;
    exact.required_keys.insert(SchedulingKey::new(
        SchedulingFamily::Timer,
        "pruned-original",
    ));
    assert!(matches!(
        current
            .resident
            .capture(&exact)
            .unwrap_err()
            .downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::Pruned(_))
    ));
    drop(current);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pruned_same_millisecond_head_keeps_observed_nanosecond_order_and_never_rolls_to_raw_tail(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        for (id, nanos) in [("clock-a", 300), ("clock-b", 100), ("clock-c", 200)] {
            let mut record = timer(id, TimerStatus::Completed);
            record.created_at = at() + chrono::Duration::nanoseconds(nanos);
            repositories::upsert_timer_tx(tx, &record)?;
        }
        Ok(())
    })?;
    let budget = ledger()?;
    let mut load = request();
    load.capture.history_limit = 3;
    let original = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget)?.require_current()
    })?;
    let old = original.resident.coverage().history[&SchedulingFamily::Timer].clone();
    assert_eq!(
        old.keys
            .iter()
            .map(|key| key.id.as_str())
            .collect::<Vec<_>>(),
        vec!["clock-a", "clock-b", "clock-c"]
    );
    assert_eq!(
        old.loaded_keys
            .iter()
            .map(|key| key.id.as_str())
            .collect::<Vec<_>>(),
        vec!["clock-a", "clock-c", "clock-b"]
    );
    db.transaction(|tx| {
        tx.execute("DELETE FROM timers WHERE timer_id='clock-a'", [])?;
        tx.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE record_id='clock-a'", [])?;
        repositories::upsert_timer_tx(tx, &timer("aaa-unqualified-tail", TimerStatus::Completed))?;
        Ok(())
    })?;
    let current = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget)?.require_current()
    })?;
    assert_eq!(
        current.resident.coverage().history[&SchedulingFamily::Timer],
        old
    );
    let captured = current.resident.capture(&load.capture)?;
    assert_eq!(captured.records.len(), 2);
    assert!(captured
        .lookup(&SchedulingKey::new(SchedulingFamily::Timer, "clock-c"))?
        .is_some());
    assert!(matches!(
        captured
            .lookup(&SchedulingKey::new(SchedulingFamily::Timer, "clock-a"))
            .unwrap_err()
            .downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::Pruned(_))
    ));
    drop((captured, current, original));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[path = "stored_raw_first_tests.rs"]
mod stored_raw_first_tests;
