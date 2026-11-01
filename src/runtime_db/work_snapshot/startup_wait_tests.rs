use super::*;
use crate::runtime_db::{
    input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery,
    transitions::{StartupInputRecoveryScope, TransitionFaultPoint},
    RuntimeDbLock,
};

#[test]
fn startup_filtered_write_ids_keep_empty_shortcut_and_candidate_fence() -> Result<()> {
    use crate::{
        config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
        runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let (_dir, db) = fresh()?;
    let late = active_wait("unqualified-filtered-wait")?;
    db.transaction(|tx| {
        repositories::upsert_wait_condition_with_writes_tx(tx, &late)?;
        let candidates = BTreeSet::from([late.id.clone()]);
        let proof =
            prepare_existing_work_wait_cleanup_tx(tx, &scope(), &candidates, &request(), false)?;
        let mut writes = repositories::WaitConditionWrites {
            primary_applied: true,
            wait_ids: candidates,
            turn_ids: BTreeSet::from(["unused-turn-路".to_owned()]),
        };
        let turn_ptr = writes.turn_ids.first().unwrap().as_ptr();
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let header = ((scope().agent_id.len() as u64 + 256) * 8, 64);
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header.0, header.1)?;
        {
            let _entry = phase.enter();
            // Even an invalid epilogue is not examined for an empty intersection.
            assert!(
                register_existing_work_wait_cleanup_tx(tx, &proof, &writes, None, &request(),)?
                    .is_none()
            );
            writes.wait_ids.insert("not-a-prepared-candidate".into());
            let error =
                register_existing_work_wait_cleanup_tx(tx, &proof, &writes, None, &request())
                    .unwrap_err();
            assert!(error.to_string().contains("unprepared actual Wait writes"));
            assert!(writes.primary_applied);
            assert_eq!(writes.turn_ids.first().unwrap().as_ptr(), turn_ptr);
        }
        assert_eq!(phase.finish()?, header);
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn startup_filtered_write_ids_publish_only_qualified_cancelled_waits() -> Result<()> {
    let (_dir, db) = fresh()?;
    let qualified = active_wait("qualified-filtered-wait")?;
    let late_owner = work("late-filtered-owner", WorkItemState::Open);
    let mut late = active_wait("late-filtered-wait")?;
    late.work_item_id = Some(late_owner.id.clone());
    db.transaction(|tx| {
        write_qualified_wait(tx, &qualified, &request())?;
        repositories::insert_new_work_item_tx(tx, &late_owner)?;
        assert_eq!(
            repositories::upsert_wait_condition_with_writes_tx(tx, &late)?.wait_ids,
            BTreeSet::from([late.id.clone()])
        );
        let proof = prepare_existing_work_wait_cleanup_tx(
            tx,
            &scope(),
            &BTreeSet::from([qualified.id.clone(), late.id.clone()]),
            &request(),
            false,
        )?;
        let mut writes = repositories::WaitConditionWrites::default();
        for source in [&qualified, &late] {
            let mut cancelled = source.clone();
            cancelled.status = WaitConditionStatus::Cancelled;
            cancelled.updated_at += chrono::Duration::seconds(1);
            cancelled.cancelled_at = Some(cancelled.updated_at);
            writes.extend(repositories::upsert_wait_condition_with_writes_tx(
                tx, &cancelled,
            )?)?;
        }
        let state = ContextDurableRead::new(tx, "agent-a")
            .scope_facts()?
            .state
            .unwrap();
        let facts =
            register_existing_work_wait_cleanup_tx(tx, &proof, &writes, Some(&state), &request())?
                .context("qualified cancellation lost its finalizer")?;
        let expected_key = WorkKey::new(WorkFamily::Wait, &qualified.id);
        assert_eq!(facts.delta.records.len(), 1);
        assert_eq!(facts.delta.records[0].key, expected_key);
        assert!(!facts
            .delta
            .coverage
            .unresolved_wait_ids
            .contains(&qualified.id));
        assert!(!facts
            .delta
            .coverage
            .observations
            .contains_key(&WorkKey::new(WorkFamily::Wait, &late.id),));
        assert_eq!(
            writes.wait_ids,
            BTreeSet::from([qualified.id.clone(), late.id.clone()])
        );
        Ok(())
    })
}

fn active_wait(id: &str) -> Result<WaitConditionRecord> {
    let mut wait = task_wait(id, "missing-startup-task", "unused-trigger")?;
    wait.status = WaitConditionStatus::Active;
    wait.resolved_at = None;
    wait.trigger_message_id = None;
    wait.triggered_at = None;
    Ok(wait)
}
async fn cleanup(
    db: &RuntimeDb,
    dir: &std::path::Path,
    scopes: &[StartupInputRecoveryScope],
    fault: Option<TransitionFaultPoint>,
) -> Result<(usize, BTreeMap<String, AgentState>)> {
    let path = dir.join("startup-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&path)?;
    let handoff = tokio::sync::Mutex::new(());
    let held = handoff.lock().await;
    let capability =
        ExclusiveStartupInputRecovery::for_pre_activation_host(db, &lease, &path, &held)?;
    db.agent_states()
        .reconcile_stale_waits_at_startup_in_scopes(scopes, &capability, &request(), fault)
}
fn unbound() -> Result<(tempfile::TempDir, RuntimeDb, String)> {
    unbound_with_import_status(true)
}
fn unbound_with_import_status(imported: bool) -> Result<(tempfile::TempDir, RuntimeDb, String)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut connection, migration)?;
    }
    let mut wait = active_wait("fixed-unbound-wait")?;
    if !imported {
        wait.work_item_id = Some("unbound-fixed-work".into());
    }
    {
        let tx = connection.transaction()?;
        let mut state = AgentState::new("agent-a");
        state.status = crate::types::AgentStatus::Stopped;
        upsert_agent_state_tx(&tx, &state)?;
        if !imported {
            let mut work = work("unbound-fixed-work", WorkItemState::Open);
            work.blocked_by_wait_id = Some(wait.id.clone());
            raw_legacy_work(&tx, &work)?;
        }
        raw_legacy_wait(&tx, &wait)?;
        if imported {
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[time().to_rfc3339()])?;
        }
        tx.commit()?;
    }
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, dir.path().join("runtime.lock"))?;
    db.transaction(ensure_schema)?;
    let seed: String = db.connection()?.query_row(
        "SELECT seed_id FROM episode_continuity_seeds WHERE agent_id='agent-a'",
        [],
        |r| r.get(0),
    )?;
    Ok((dir, db, seed))
}
fn fixed_digest(db: &RuntimeDb, seed: &str) -> Result<String> {
    Ok(db.connection()?.query_row("SELECT content_digest FROM work_domain_eligibility WHERE seed_id=?1 AND family='wait' AND record_id='fixed-unbound-wait' ORDER BY capture_phase LIMIT 1",[seed],|r|r.get(0))?)
}
#[tokio::test]
async fn startup_unbound_wait_cleanup_preserves_fixed_members_and_binds_actual_cancelled_version(
) -> Result<()> {
    let (dir, db, seed) = unbound()?;
    let before = fixed_digest(&db, &seed)?;
    let scopes = db.startup_input_recovery_scopes(&["agent-a".into()])?;
    assert!(matches!(
        scopes.as_slice(),
        [StartupInputRecoveryScope::Unbound(_)]
    ));
    assert_eq!(cleanup(&db, dir.path(), &scopes, None).await?.0, 1);
    assert_eq!(fixed_digest(&db, &seed)?, before);
    let count: i64 = db.connection()?.query_row(
        "SELECT count(*) FROM work_domain_seed_recovery_current WHERE seed_id=?1",
        [&seed],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1);
    // A later raw row may retain the original cleanup behavior, but cannot
    // acquire membership through that maintenance operation.
    let late = active_wait("late-unreceived-wait")?;
    db.transaction(|tx| repositories::upsert_wait_condition_with_writes_tx(tx, &late).map(|_| ()))?;
    assert_eq!(cleanup(&db, dir.path(), &scopes, None).await?.0, 1);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT count(*) FROM work_domain_seed_recovery_current WHERE seed_id=?1",
            [&seed],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    db.transaction(|tx| {
        assert!(crate::runtime_db::episode_snapshot::bind_unbound_episode_continuity_tx(tx,&identity(),time())?);
        upsert_agent_identity_tx(tx,&identity())?;
        let mut selected=request();
        selected.capture.mandatory_roots.insert(WorkKey::new(WorkFamily::Wait,"fixed-unbound-wait"));
        let current=prepare_work_domain_snapshot_tx(tx,&scope(),&selected)?.require_current()?;
        let captured=current.resident.capture(&selected.capture)?;
        assert!(captured.records.iter().any(|row|matches!(&row.record,CanonicalWorkRecord::Wait(wait) if wait.id=="fixed-unbound-wait" && wait.status==WaitConditionStatus::Cancelled)));
        let mut invalid=selected;
        invalid.capture.mandatory_roots.insert(WorkKey::new(WorkFamily::Wait,&late.id));
        let error=prepare_work_domain_snapshot_tx(tx,&scope(),&invalid).unwrap_err();
        assert!(matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::NeedsPreparation(_))));
        Ok(())
    })?;
    Ok(())
}
#[tokio::test]
async fn startup_unbound_wait_fault_and_changed_seed_roll_back_canonical_and_overlay() -> Result<()>
{
    let (dir, db, seed) = unbound()?;
    let before = fixed_digest(&db, &seed)?;
    let scopes = db.startup_input_recovery_scopes(&["agent-a".into()])?;
    for fault in [
        TransitionFaultPoint::AfterCanonicalWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        assert!(cleanup(&db, dir.path(), &scopes, Some(fault))
            .await
            .is_err());
        let wait = db
            .wait_conditions()
            .latest_for_agent("agent-a")?
            .into_iter()
            .find(|w| w.id == "fixed-unbound-wait")
            .unwrap();
        assert_eq!(wait.status, WaitConditionStatus::Active);
        assert_eq!(fixed_digest(&db, &seed)?, before);
        assert_eq!(
            db.connection()?.query_row(
                "SELECT count(*) FROM work_domain_seed_recovery_current",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
    }
    let mut invalid = scopes.clone();
    let StartupInputRecoveryScope::Unbound(seed) = &mut invalid[0] else {
        unreachable!()
    };
    seed.boundary_id.push_str("-wrong");
    assert!(cleanup(&db, dir.path(), &invalid, None).await.is_err());
    assert_eq!(
        db.wait_conditions().latest_for_agent("agent-a")?[0].status,
        WaitConditionStatus::Active
    );
    Ok(())
}
#[tokio::test]
async fn startup_current_identity_without_state_updates_only_existing_wait_qualification(
) -> Result<()> {
    let wait = active_wait("state-less-fixed-wait")?;
    let (dir, db) = legacy(|tx| {
        tx.execute("DELETE FROM agent_states WHERE agent_id='agent-a'", [])?;
        raw_legacy_wait(tx, &wait)
    })?;
    let scopes = db.startup_input_recovery_scopes(&["agent-a".into()])?;
    assert!(matches!(
        scopes.as_slice(),
        [StartupInputRecoveryScope::Current(_)]
    ));
    let (count, states) = cleanup(&db, dir.path(), &scopes, None).await?;
    assert_eq!(count, 1);
    assert!(!states.contains_key("agent-a"));
    let connection = db.connection()?;
    assert_eq!(
        connection.query_row(
            "SELECT count(*) FROM work_domain_current WHERE agent_id='agent-a'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(connection.query_row("SELECT availability FROM work_domain_records WHERE agent_id='agent-a' AND family='wait' AND record_id=?1",[&wait.id],|r|r.get::<_,String>(0))?,"present");
    drop(connection);
    // First later preparation can build an exact domain; no fake State was
    // needed to preserve the original maintenance responsibility.
    db.transaction(|tx| {
        let mut selected=request();
        selected.capture.mandatory_roots.insert(WorkKey::new(WorkFamily::Wait,&wait.id));
        let current=prepare_work_domain_snapshot_tx(tx,&scope(),&selected)?.require_current()?;
        assert!(current.facts.state.is_none());
        let captured=current.resident.capture(&selected.capture)?;
        assert!(captured.records.iter().any(|r|matches!(&r.record,CanonicalWorkRecord::Wait(wait) if wait.status==WaitConditionStatus::Cancelled)));
        Ok(())
    })?;
    Ok(())
}
