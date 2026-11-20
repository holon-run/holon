use super::*;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    }
}

fn seed(connection: &Connection, id: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO episode_continuity_seeds (
            seed_id, agent_id, policy_version, boundary_id, source_kind,
            roots_json, candidate_payloads_json, import_required, captured_at
         ) VALUES (?1, ?2, 1, 'boundary', 'upgrade_unbound', '{}', '{}', 0, 'original')",
        params![id, AGENT],
    )?;
    Ok(())
}

fn corrupt_checked_seed(connection: &Connection, update: &str) -> Result<()> {
    // Deliberately corrupt this fixture only; restore schema checks before reading.
    connection.execute_batch("PRAGMA ignore_check_constraints=ON")?;
    let changed = connection.execute(update, []);
    connection.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
    assert_eq!(changed?, 1);
    Ok(())
}

// Independent expected cost: the two returned UTF-8 strings, no row/list slot.
fn cost(id: &str) -> (u64, usize) {
    ((id.len() as u64 + AGENT.len() as u64) * 8, 16)
}

#[test]
fn loaded_seed_exact_cost_accumulates_and_last_returned_holder_retains_it() -> Result<()> {
    let connection = connection("boundary")?;
    let id = "seed\0界\"\\\n".repeat(1024);
    seed(&connection, &id)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let before = phase.finish()?;
    let (first, last) = {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        let before_missing = phase.finish()?;
        assert!(find_seed(&connection, "missing")?.is_none());
        assert_eq!(phase.finish()?, before_missing);
        let first = find_seed(&connection, AGENT)?.unwrap();
        let last = find_seed(&connection, AGENT)?.unwrap();
        for value in [&first, &last] {
            assert_eq!(value.id, id);
            assert_eq!(value.agent_id, AGENT);
            assert_eq!(value.incarnation, None);
            assert!(!value.import_required);
            assert!(Arc::ptr_eq(value._material.as_ref().unwrap(), &holder));
        }
        (first, last)
    };
    let cost = cost(&id);
    let expected = (before.0 + 31 * 8 + cost.0 * 2, before.1 + 24 + cost.1 * 2);
    assert_eq!(phase.finish()?, expected);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((phase, holder, first));
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    assert_eq!(last.id, id);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let unbudgeted = find_seed(&connection, AGENT)?.unwrap();
    assert_eq!(unbudgeted.id, id);
    assert!(unbudgeted._material.is_none());
    Ok(())
}

#[test]
fn loaded_seed_rejects_each_dimension_without_refunding_or_detaching_prior_holder() -> Result<()> {
    for byte_failure in [true, false] {
        let mut connection = connection("boundary")?;
        let id = "seed\0界".repeat(32);
        seed(&connection, &id)?;
        let scope = scope();
        let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
        let cost = cost(&id);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            header.0 + cost.0 * 2 - u64::from(byte_failure),
            header.1 + cost.1 * 2 - usize::from(!byte_failure),
        )?;
        let holder = phase.guard()?;
        let retained = {
            let _entry = phase.enter();
            let retained = find_seed(&connection, AGENT)?.unwrap();
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO seed_id_markers VALUES ('rollback')", [])?;
            let error = find_seed(&tx, AGENT).unwrap_err();
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            assert!(ResourceReadMaterial::active_guard().is_err());
            assert!(Arc::ptr_eq(retained._material.as_ref().unwrap(), &holder));
            tx.rollback()?;
            retained
        };
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM seed_id_markers", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, header.0 + cost.0);
        assert_eq!(retained.id, id);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn loaded_seed_decode_errors_release_sql_and_keep_successful_charges_on_original_phase(
) -> Result<()> {
    let connection = connection("boundary")?;
    seed(&connection, "loaded-seed")?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let before = phase.finish()?;
    let loaded = {
        let _entry = phase.enter();
        connection.execute(
            "UPDATE episode_continuity_seeds SET identity_incarnation = 'invalid'",
            [],
        )?;
        assert!(find_seed(&connection, AGENT).is_err());
        assert_eq!(phase.finish()?, before);
        corrupt_checked_seed(
            &connection,
            "UPDATE episode_continuity_seeds SET identity_incarnation = -1",
        )?;
        assert!(find_seed(&connection, AGENT).is_err());
        let cost = cost("loaded-seed");
        assert_eq!(phase.finish()?, (before.0 + cost.0, before.1 + cost.1));
        connection.execute(
            "UPDATE episode_continuity_seeds SET identity_incarnation = 7, import_required = 1",
            [],
        )?;
        let loaded = find_seed(&connection, AGENT)?.unwrap();
        assert_eq!(loaded.incarnation, Some(7));
        assert!(loaded.import_required);
        assert_eq!(
            phase.finish()?,
            (before.0 + cost.0 * 2, before.1 + cost.1 * 2)
        );
        loaded
    };
    let used = phase.finish()?.0;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, used);
    drop(loaded);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}

#[test]
fn loaded_seed_repository_binding_failure_rolls_back_and_retries_in_same_phase() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        seed(tx, "loaded-seed")?;
        tx.execute_batch(
            "CREATE TRIGGER reject_loaded_identity BEFORE INSERT ON agent_identities
             BEGIN SELECT RAISE(ABORT, 'after loaded seed binding'); END;",
        )?;
        Ok(())
    })?;
    let identity = identity();
    // The original registry writer charges three timestamp strings per attempt.
    let registry = (3 * (24 + std::mem::size_of::<String>() as u64) * 8, 24);
    // Independently count JSON punctuation for this flat identity fixture.
    let value = serde_json::to_value(&identity)?;
    let fields = value.as_object().unwrap();
    assert!(fields
        .values()
        .all(|value| !value.is_array() && !value.is_object()));
    let slots = fields.len() * 3 + 1 + fields.values().filter(|value| value.is_string()).count();
    let decoded = (
        (serde_json::to_vec(&identity)?.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let before = phase.finish()?;
    {
        let _entry = phase.enter();
        let error = db
            .agent_identities()
            .create_legacy_materialized_with_relations(&identity, &Default::default())
            .unwrap_err();
        assert!(format!("{error:#}").contains("after loaded seed binding"));
        let cost = cost("loaded-seed");
        assert_eq!(
            phase.finish()?,
            (
                before.0 + cost.0 + registry.0,
                before.1 + cost.1 + registry.1
            )
        );
        db.transaction(|tx| {
            assert!(!has_any_binding(tx, AGENT)?);
            assert!(load_identity(tx, AGENT)?.is_none());
            tx.execute_batch("DROP TRIGGER reject_loaded_identity;")?;
            Ok(())
        })?;
        db.agent_identities()
            .create_legacy_materialized_with_relations(&identity, &Default::default())?;
        assert_eq!(
            phase.finish()?,
            (
                before.0 + (cost.0 + registry.0) * 2 + decoded.0,
                before.1 + (cost.1 + registry.1) * 2 + decoded.1
            )
        );
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    db.transaction(|tx| {
        let bound: (String, u64) = tx.query_row(
            "SELECT seed_id, identity_incarnation FROM agent_episode_continuity WHERE agent_id = ?1",
            [AGENT],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(bound, ("loaded-seed".into(), identity.incarnation));
        assert_eq!(load_identity(tx, AGENT)?.unwrap(), identity);
        Ok(())
    })?;
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn loaded_seed_policy_pending_and_incarnation_checks_remain_fail_closed() -> Result<()> {
    let mut connection = connection("boundary")?;
    connection.execute_batch(
        "CREATE TABLE agent_identities (agent_id TEXT PRIMARY KEY, payload_json TEXT NOT NULL);",
    )?;
    seed(&connection, "loaded-seed")?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let before = phase.finish()?;
    {
        let _entry = phase.enter();
        let identity = identity();
        let tx = connection.transaction()?;
        corrupt_checked_seed(
            &tx,
            "UPDATE episode_continuity_seeds SET policy_version = 2",
        )?;
        assert!(!bind_unbound_episode_continuity_tx(
            &tx,
            &identity,
            Utc::now()
        )?);
        assert_eq!(phase.finish()?, before);
        tx.execute(
            "UPDATE episode_continuity_seeds SET policy_version = 1, import_required = 1",
            [],
        )?;
        let error = bind_unbound_episode_continuity_tx(&tx, &identity, Utc::now()).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<EpisodeSnapshotError>(),
            Some(EpisodeSnapshotError::LegacyImportPending)
        ));
        tx.execute(
            "UPDATE episode_continuity_seeds SET import_required = 0, identity_incarnation = 99",
            [],
        )?;
        let error = bind_unbound_episode_continuity_tx(&tx, &identity, Utc::now()).unwrap_err();
        assert!(format!("{error:#}").contains("cannot adopt a different identity scope"));
        assert!(!has_any_binding(&tx, AGENT)?);
        tx.rollback()?;
    }
    let cost = cost("loaded-seed");
    assert_eq!(
        phase.finish()?,
        (before.0 + cost.0 * 2, before.1 + cost.1 * 2)
    );
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
