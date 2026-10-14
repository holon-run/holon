use super::super::tests::{at, identity, ledger, request, scope, timer, wake};
use super::*;
use crate::runtime_db::{
    evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
    input_evidence_snapshot::legacy_recovery::{
        legacy_seed_recovery_scope_tx, ExclusiveStartupInputRecovery,
    },
    migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
    transitions::{StartupInputRecoveryScope, TransitionFaultPoint},
    RuntimeDb, RuntimeDbLock,
};
use crate::types::{AgentState, AgentStatus, TimerStatus};
fn fixture(bound: bool) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("runtime.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut connection, migration)?;
    }
    let tx = connection.transaction()?;
    if bound {
        upsert_agent_identity_tx(&tx, &identity())?;
    }
    repositories::upsert_timer_tx(&tx, &timer("startup-original", TimerStatus::Active))?;
    wake(&tx, "startup-original", "startup-wake", 2)?;
    let mut state = AgentState::new(&scope().agent_id);
    state.status = AgentStatus::Stopped;
    state.pending = 1;
    state.total_message_count = 1;
    upsert_agent_state_tx(&tx, &state)?;
    tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[repositories::timestamp(at())])?;
    tx.commit()?;
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, home.path().join("runtime.lock"))?;
    Ok((home, db))
}
fn work_request() -> crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
    crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest {
            nonopen_limit: 0,
            ..crate::runtime::resident_work::WorkCaptureRequest::current()
        },
        record_limit: request().record_limit,
        byte_limit: request().byte_limit,
    }
}
fn fingerprint(db: &RuntimeDb) -> Result<Vec<Vec<String>>> {
    let c = db.connection()?;
    ["SELECT payload_json FROM timers ORDER BY timer_id","SELECT json_array(timer_id,message_id,fire_count,status,created_at,updated_at,incorporated_at,cancelled_at) FROM timer_wakes ORDER BY timer_id,message_id","SELECT payload_json FROM queue_entries ORDER BY message_id","SELECT payload_json FROM agent_states ORDER BY agent_id","SELECT json_array(seed_id,family,record_id,original_content_digest,original_projection_digest,content_digest,projection_digest,revision) FROM scheduling_seed_recovery_current ORDER BY seed_id,family,record_id","SELECT coverage_json FROM scheduling_domain_current ORDER BY agent_id,identity_incarnation"].into_iter().map(|sql|{
        let mut q=c.prepare(sql)?;let rows=q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)
    }).collect()
}
#[test]
fn true_v81_unbound_stopped_cleanup_keeps_fixed_anchor_and_adopts_only_acknowledged_versions(
) -> Result<()> {
    let (_home, db) = fixture(false)?;
    let budget = ledger()?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let seed = db.transaction(|tx| legacy_seed_recovery_scope_tx(tx, &scope().agent_id))?;
    let frozen: Vec<(String, String, String)> = {
        let c = db.connection()?;
        let mut q=c.prepare("SELECT record_id,content_digest,projection_digest FROM scheduling_fixed_members ORDER BY inventory_id,family,record_id")?;
        let values = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        values
    };
    let authorities = [StartupInputRecoveryScope::Unbound(seed.clone())];
    let (cancelled, states, ack) = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            None,
        )?;
    assert_eq!(cancelled, 0);
    assert_eq!(states[&scope().agent_id].pending, 0);
    assert!(ack._invalidations.is_empty());
    assert!(budget.report()?.observed_bytes > 0);
    assert_eq!(
        db.timers().latest("startup-original")?.unwrap().status,
        TimerStatus::Cancelled
    );
    let c = db.connection()?;
    assert_eq!(
        c.query_row(
            "SELECT status FROM timer_wakes WHERE timer_id='startup-original'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        "cancelled"
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM scheduling_seed_recovery_current",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        2
    );
    assert_eq!(c.query_row("SELECT (SELECT COUNT(*) FROM agent_identities)+(SELECT COUNT(*) FROM scheduling_domain_current)+(SELECT COUNT(*) FROM scheduling_domain_records)",[],|r|r.get::<_,u64>(0))?,0);
    let mut q=c.prepare("SELECT record_id,content_digest,projection_digest FROM scheduling_fixed_members ORDER BY inventory_id,family,record_id")?;
    assert_eq!(
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?,
        frozen
    );
    drop(q);
    drop(c);
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.transaction(|tx|{
        assert!(crate::runtime_db::episode_snapshot::bind_unbound_episode_continuity_tx(tx,&identity(),at()+chrono::Duration::seconds(2))?);
        upsert_agent_identity_tx(tx,&identity())?;
        let mut load=request();load.capture.required_keys=BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer,"startup-original"),SchedulingKey::wake("startup-original","startup-wake")]);
        let current=prepare_scheduling_domain_snapshot_tx(tx,&scope(),&load,&budget)?.require_current()?;
        let capture=current.resident.capture(&load.capture)?;
        assert!(matches!(&capture.lookup(&SchedulingKey::new(SchedulingFamily::Timer,"startup-original"))?.unwrap().fact,CanonicalSchedulingFact::Timer(t) if t.status==TimerStatus::Cancelled));
        assert!(matches!(&capture.lookup(&SchedulingKey::wake("startup-original","startup-wake"))?.unwrap().fact,CanonicalSchedulingFact::TimerWake(w) if w.status==SchedulingWakeStatus::Cancelled && w.fire_count==2));
        // First prove the original fixed cancellation versions. A later raw
        // active row must then block complete live preparation, never be adopted.
        repositories::upsert_timer_tx(tx,&timer("late-unqualified",TimerStatus::Active))?;
        load.capture.required_keys.insert(SchedulingKey::new(SchedulingFamily::Timer,"late-unqualified"));
        assert!(prepare_scheduling_domain_snapshot_tx(tx,&scope(),&load,&budget).is_err());
        Ok(())
    })?;
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn current_stopped_cancellation_has_actual_sparse_revision_and_whole_transaction_rollback(
) -> Result<()> {
    let (_home, db) = fixture(true)?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?
            .require_current()?;
        Ok(())
    })?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let authorities = [StartupInputRecoveryScope::Current(scope())];
    let before = fingerprint(&db)?;
    assert!(db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            Some(TransitionFaultPoint::BeforeCommit)
        )
        .is_err());
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    let (_, states, ack) = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            None,
        )?;
    assert_eq!(states[&scope().agent_id].pending, 0);
    assert_eq!(ack._invalidations.len(), 1);
    db.transaction(|tx| {
        let input = scope();
        let load = request();
        let mut material = Material::new(&input, &load, &budget);
        let coverage = loader::current(tx, &scope(), &mut material)?.unwrap();
        assert!(
            !coverage.active_timers_complete
                && !coverage.pending_wakes_complete
                && !coverage.active_triggers_complete
        );
        Ok(())
    })?;
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn startup_unbound_late_raw_cleanup_never_grants_membership_and_fixed_drift_rolls_back(
) -> Result<()> {
    let (_home, db) = fixture(false)?;
    let budget = ledger()?;
    db.transaction(|tx| {
        repositories::upsert_timer_tx(tx, &timer("late-raw", TimerStatus::Active))
    })?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let seed = db.transaction(|tx| legacy_seed_recovery_scope_tx(tx, &scope().agent_id))?;
    let authorities = [StartupInputRecoveryScope::Unbound(seed)];
    let (_, _, ack) = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            None,
        )?;
    drop(ack);
    assert_eq!(
        db.timers().latest("late-raw")?.unwrap().status,
        TimerStatus::Cancelled
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_seed_recovery_current WHERE record_id='late-raw'",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    let (_other, drift) = fixture(false)?;
    let lease2 = RuntimeDbLock::lock(drift.lock_path())?;
    let cap2 = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &drift,
        &lease2,
        drift.lock_path(),
        &guard,
    )?;
    let authorities2 = [StartupInputRecoveryScope::Unbound(
        drift.transaction(|tx| legacy_seed_recovery_scope_tx(tx, &scope().agent_id))?,
    )];
    // Corrupt restore updates normalized and payload together but has no writer ACK.
    drift.connection()?.execute("UPDATE timers SET summary='changed',payload_json=json_set(payload_json,'$.summary','changed') WHERE timer_id='startup-original'",[])?;
    let before = fingerprint(&drift)?;
    assert!(drift
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities2,
            &cap2,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            None
        )
        .is_err());
    assert_eq!(fingerprint(&drift)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn startup_material_rejects_large_corrupt_state_before_decode_and_wrong_scope_before_state(
) -> Result<()> {
    let (_home, db) = fixture(true)?;
    let budget = ledger()?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    db.connection()?.execute(
        "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
        params!["{".repeat(65 * 1024), scope().agent_id],
    )?;
    let mut small = request();
    small.byte_limit = 4096;
    let authorities = [StartupInputRecoveryScope::Current(scope())];
    let error = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &small,
            &[scope()],
            &budget,
            None,
        )
        .err()
        .context("expected budget refusal")?;
    assert!(matches!(
        error.downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::ResourceExhausted(_))
    ));
    assert_eq!(budget.report()?.observed_bytes, 0);
    let mut successor = identity();
    successor.incarnation += 1;
    db.connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        params![serde_json::to_string(&successor)?, scope().agent_id],
    )?;
    let error = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            None,
        )
        .err()
        .context("expected stale reservation refusal")?;
    assert!(matches!(
        error.downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::RetiredScope)
    ));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn startup_wrong_database_cap_rejects_before_body_decode() -> Result<()> {
    let (_home, db) = fixture(false)?;
    let (_other, other) = fixture(false)?;
    let budget = ledger()?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let authorities = [StartupInputRecoveryScope::Unbound(
        other.transaction(|tx| legacy_seed_recovery_scope_tx(tx, &scope().agent_id))?,
    )];
    assert!(other
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            None
        )
        .is_err());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn startup_wait_dependency_pool_does_not_decode_unreferenced_historical_task_bodies() -> Result<()>
{
    let (_home, db) = fixture(true)?;
    let budget = ledger()?;
    db.transaction(|tx|{
        for n in 0..1000 {
            // Irrelevant corrupt restore: no live Wait refers to any of these IDs.
            tx.execute("INSERT INTO tasks(task_id,owner_agent_id,kind,status,revision,created_at,updated_at,payload_json) VALUES(?1,?2,'command_task','completed',1,?3,?3,'{')",params![format!("unrelated-{n}"),scope().agent_id,repositories::timestamp(at())])?;
        }
        Ok(())
    })?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let authorities = [StartupInputRecoveryScope::Current(scope())];
    let (_, _, ack) = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &authorities,
            &cap,
            &work_request(),
            &request(),
            &[scope()],
            &budget,
            None,
        )?;
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM tasks WHERE payload_json='{'",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        1000
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM task_domain_records", [], |r| r
                .get::<_, u64>(0))?,
        0
    );
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn startup_large_state_collections_consume_record_allowance_before_bad_typed_field() -> Result<()> {
    let (_home, db) = fixture(true)?;
    let budget = ledger()?;
    db.transaction(|tx| {
        let mut state=AgentState::new(&scope().agent_id);state.status=AgentStatus::Stopped;state.pending=1;state.total_message_count=1;
        state.working_memory.current_working_memory.pending_followups=(0..4096).map(|n|format!("f{n}")).collect();
        upsert_agent_state_tx(tx,&state)?;
        // Corrupt restore retains valid JSON/node count but cannot deserialize.
        tx.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending','bad typed count') WHERE agent_id=?1",[&scope().agent_id])?;
        Ok(())
    })?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let before = fingerprint(&db)?;
    let mut small = request();
    small.record_limit = 1024;
    let error = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &[StartupInputRecoveryScope::Current(scope())],
            &cap,
            &work_request(),
            &small,
            &[scope()],
            &budget,
            None,
        )
        .err()
        .context("large State collections decoded before record admission")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn startup_finite_task_detail_nodes_are_admitted_before_original_task_decoder() -> Result<()> {
    use crate::types::{
        TaskKind, TaskRecord, TaskStatus, WaitConditionKind, WaitConditionRecord,
        WaitConditionStatus, WakeSource,
    };
    let (_home, db) = fixture(true)?;
    let budget = ledger()?;
    db.transaction(|tx| {
        let task = TaskRecord {
            id: "finite-startup-task".into(),
            agent_id: scope().agent_id,
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: at(),
            updated_at: at(),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({"finite_collection":(0..4096).collect::<Vec<_>>()})),
            recovery: None,
        };
        repositories::upsert_task_tx(tx, &task)?;
        let wait = WaitConditionRecord {
            id: "finite-startup-task-wait".into(),
            agent_id: scope().agent_id,
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: Some(task.id.clone()),
            waiting_for: "original task".into(),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: task.id.clone(),
            }],
            continuation: None,
            created_at: at(),
            updated_at: at(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        repositories::upsert_wait_condition_tx(tx, &wait)?;
        // The real Task writer slims unknown detail keys. Install the actual
        // decoder input afterwards so this fixture still exercises native nodes.
        tx.execute(
            "UPDATE tasks SET payload_json=json_set(payload_json,'$.detail.finite_collection',json(?2),'$.summary',42) WHERE task_id=?1",
            params![task.id, serde_json::to_string(&task.detail.as_ref().unwrap()["finite_collection"])?],
        )?;
        let native_nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
            [&task.id], |row| row.get(0),
        )?;
        assert!(native_nodes > 4096);
        Ok(())
    })?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let before = fingerprint(&db)?;
    let mut small = request();
    small.record_limit = 2048;
    let task_before: String = db.connection()?.query_row(
        "SELECT payload_json FROM tasks WHERE task_id='finite-startup-task'",
        [],
        |r| r.get(0),
    )?;
    let error = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &[StartupInputRecoveryScope::Current(scope())],
            &cap,
            &work_request(),
            &small,
            &[scope()],
            &budget,
            None,
        )
        .err()
        .context("finite Task detail decoded before record admission")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT payload_json FROM tasks WHERE task_id='finite-startup-task'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        task_before
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn startup_normalized_wait_continuation_nodes_share_the_original_record_allowance() -> Result<()> {
    use crate::types::{WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource};
    let (_home, db) = fixture(true)?;
    let budget = ledger()?;
    db.transaction(|tx| {
        let wait = WaitConditionRecord {
            id: "finite-startup-continuation".into(),
            agent_id: scope().agent_id,
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Operator,
            source: None,
            subject_ref: None,
            waiting_for: "operator".into(),
            wake_sources: vec![WakeSource::OperatorInput],
            continuation: Some(serde_json::json!({"original":(0..4096).collect::<Vec<_>>()})),
            created_at: at(),
            updated_at: at(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        repositories::upsert_wait_condition_tx(tx, &wait)?;
        Ok(())
    })?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let mutex = tokio::sync::Mutex::new(());
    let guard = mutex.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let before = fingerprint(&db)?;
    let wait_before:String=db.connection()?.query_row("SELECT json_array(status,wake_sources_json,continuation_json,payload_json) FROM wait_conditions WHERE wait_condition_id='finite-startup-continuation'",[],|r|r.get(0))?;
    let mut small = request();
    small.record_limit = 1024;
    let error = db
        .agent_states()
        .reconcile_stale_waits_at_startup_with_scheduling(
            &[StartupInputRecoveryScope::Current(scope())],
            &cap,
            &work_request(),
            &small,
            &[scope()],
            &budget,
            None,
        )
        .err()
        .context("normalized Wait continuation escaped combined node budget")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(db.connection()?.query_row("SELECT json_array(status,wake_sources_json,continuation_json,payload_json) FROM wait_conditions WHERE wait_condition_id='finite-startup-continuation'",[],|r|r.get::<_,String>(0))?,wait_before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
