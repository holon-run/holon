use super::*;
use crate::runtime_db::{
    evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
    migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
    repositories::{committed_turn_record_tx, upsert_turn_record_tx},
};
use crate::types::{
    AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility, TurnNoBriefReason,
    TurnTerminalKind, TurnTerminalSummary,
};
use rusqlite::Connection;
fn time() -> DateTime<Utc> {
    "2026-10-12T00:00:00Z".parse().unwrap()
}
fn legacy() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let mut conn = Connection::open(&path)?;
    ensure_migration_table(&conn)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut conn, migration)?;
    }
    {
        let tx = conn.transaction()?;
        let mut state = AgentState::new("legacy-a");
        state.turn_index = 1;
        state.current_turn_id = Some("turn-a".into());
        upsert_agent_state_tx(&tx, &state)?;
        let mut turn = TurnRecord::new("legacy-a", "turn-a", 1);
        turn.created_at = time();
        upsert_turn_record_tx(&tx, &turn)?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[time().to_rfc3339()])?;
        tx.commit()?;
    }
    drop(conn);
    let db = RuntimeDb::open_and_migrate(&path, dir.path().join("runtime.lock"))?;
    db.transaction(|tx| {
        tx.execute_batch(INPUT_EVIDENCE_SCHEMA)?;
        capture_input_evidence_upgrade_tx(tx, time())
    })?;
    Ok((dir, db))
}
fn interrupt_turn(tx: &Transaction<'_>) -> Result<CanonicalInputEvidence> {
    let mut turn = committed_turn_record_tx(tx, "legacy-a", "turn-a")?.record;
    turn.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Interrupted,
        reason: Some("startup_recovery/daemon_restart".into()),
        no_brief_reason: Some(TurnNoBriefReason::Interrupted),
        completed_at: time() + chrono::Duration::seconds(1),
        duration_ms: 1000,
    });
    upsert_turn_record_tx(tx, &turn)?;
    Ok(CanonicalInputEvidence::Turn(
        committed_turn_record_tx(tx, "legacy-a", "turn-a")?.record,
    ))
}
#[test]
fn fixed_unbound_member_recovery_survives_later_explicit_identity_binding() -> Result<()> {
    let (_dir, db) = legacy()?;
    let frozen:String=db.connection()?.query_row("SELECT fingerprint_json FROM input_evidence_eligibility WHERE family='turn' AND record_id='turn-a' LIMIT 1",[],|r|r.get(0))?;
    db.transaction(|tx| {
        let scope = legacy_seed_recovery_scope_tx(tx, "legacy-a")?;
        let prepared =
            prepare_unbound_seed_recovery_tx(tx, &scope, &[Key::new(Family::Turn, "turn-a")])?;
        let actual = interrupt_turn(tx)?;
        register_unbound_seed_recovery_tx(tx, &prepared, &[actual], &[])
    })?;
    let after:String=db.connection()?.query_row("SELECT fingerprint_json FROM input_evidence_eligibility WHERE family='turn' AND record_id='turn-a' LIMIT 1",[],|r|r.get(0))?;
    assert_eq!(frozen, after);
    db.transaction(|tx| {
        let identity = crate::types::AgentIdentityRecord::new(
            "legacy-a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        assert!(
            crate::runtime_db::episode_snapshot::bind_unbound_episode_continuity_tx(
                tx,
                &identity,
                time()
            )?
        );
        upsert_agent_identity_tx(tx, &identity)?;
        let scope = InputEvidenceScope {
            agent_id: "legacy-a".into(),
            identity_incarnation: identity.incarnation,
        };
        let request = InputEvidenceLoadRequest {
            interrupted_sources: Default::default(),
            windows: vec![InputWindowRequest {
                selector: InputWindowSelector::GlobalTurns,
                limit: 1,
            }],
            mandatory_roots: BTreeSet::new(),
            record_limit: 512,
            byte_limit: 1024 * 1024,
        };
        assert!(matches!(
            prepare_input_evidence_snapshot_tx(tx, &scope, &request)?,
            InputEvidenceBootstrapSnapshot::Current(_)
        ));
        Ok(())
    })?;
    Ok(())
}
#[test]
fn unbound_startup_never_adopts_late_unrelated_turn() -> Result<()> {
    let (_dir, db) = legacy()?;
    db.turn_records()
        .upsert(&TurnRecord::new("legacy-a", "late-unqualified", 2))?;
    db.transaction(|tx| {
        let scope = legacy_seed_recovery_scope_tx(tx, "legacy-a")?;
        assert!(prepare_unbound_seed_recovery_tx(
            tx,
            &scope,
            &[Key::new(Family::Turn, "late-unqualified")]
        )
        .is_err());
        Ok(())
    })?;
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_seed_recovery_current",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    Ok(())
}
#[test]
fn unbound_startup_wrong_checkpoint_or_new_identity_rejects() -> Result<()> {
    let (_dir, db) = legacy()?;
    db.transaction(|tx| {
        let mut scope = legacy_seed_recovery_scope_tx(tx, "legacy-a")?;
        scope.capture_checkpoint_digest = "wrong".into();
        assert!(prepare_unbound_seed_recovery_tx(tx, &scope, &[]).is_err());
        Ok(())
    })?;
    db.transaction(|tx| {
        let identity = crate::types::AgentIdentityRecord::new(
            "legacy-a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        upsert_agent_identity_tx(tx, &identity)?;
        assert!(legacy_seed_recovery_scope_tx(tx, "legacy-a").is_err());
        Ok(())
    })?;
    Ok(())
}
#[test]
fn canonical_recovery_and_seed_revision_rollback_together() -> Result<()> {
    let (_dir, db) = legacy()?;
    let result: Result<()> = db.transaction(|tx| {
        let scope = legacy_seed_recovery_scope_tx(tx, "legacy-a")?;
        let prepared =
            prepare_unbound_seed_recovery_tx(tx, &scope, &[Key::new(Family::Turn, "turn-a")])?;
        let actual = interrupt_turn(tx)?;
        register_unbound_seed_recovery_tx(tx, &prepared, &[actual], &[])?;
        anyhow::bail!("fixture after qualification")
    });
    assert!(result.is_err());
    assert!(db
        .turn_records()
        .by_id(Some("legacy-a"), "turn-a")?
        .unwrap()
        .terminal
        .is_none());
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_seed_recovery_current",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    Ok(())
}
