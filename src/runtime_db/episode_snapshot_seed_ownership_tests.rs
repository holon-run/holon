use super::*;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    }
}

// Independent from the production gate: two strings, prefix + SHA-256 hex + agent.
pub(super) fn return_cost(agent: &str) -> (u64, usize) {
    ((13 + 64 + agent.len() as u64) * 8, 2 * 8)
}

#[test]
fn returned_seed_alone_retains_original_phase_after_sql_and_tls_exit() -> Result<()> {
    let mut connection = connection("fixed-boundary")?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let before = phase.finish()?;
    let seed = {
        let _entry = phase.enter();
        let tx = connection.transaction()?;
        let seed = insert(&tx, AGENT)?;
        assert!(Arc::ptr_eq(seed._material.as_ref().unwrap(), &holder));
        assert_eq!(seed.id, original_seed_id("fixed-boundary", AGENT)?);
        assert_eq!(seed.agent_id, AGENT);
        tx.commit()?;
        seed
    };
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let json = super::super::seed_candidate_tests::seed_insert_json_cost(&[])?;
    let strings = return_cost(AGENT);
    let expected = (before.0 + json.0 + strings.0, before.1 + json.1 + strings.1);
    assert_eq!(phase.finish()?, expected);
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    let stored: String =
        connection.query_row("SELECT seed_id FROM episode_continuity_seeds", [], |row| {
            row.get(0)
        })?;
    assert_eq!(seed.id, stored);
    drop(seed);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn seed_return_text_is_rejected_before_insert_for_each_budget_dimension() -> Result<()> {
    for byte_failure in [true, false] {
        let mut connection = connection("fixed-boundary")?;
        let scope = scope();
        let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
        let strings = return_cost(AGENT);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            header.0 + strings.0 - u64::from(byte_failure),
            header.1 + strings.1 - usize::from(!byte_failure),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO seed_id_markers VALUES ('rollback')", [])?;
            let error = insert(&tx, AGENT).unwrap_err();
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM episode_continuity_seeds", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(count, 0);
            assert!(ResourceReadMaterial::active_guard().is_err());
            tx.rollback()?;
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(phase.finish().is_err());
        assert_eq!(ledger.report()?.observed_bytes, header.0);
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM seed_id_markers", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn failed_seed_sql_rolls_back_without_releasing_the_retained_original_seed() -> Result<()> {
    let mut connection = connection("fixed-boundary")?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let seed = {
        let _entry = phase.enter();
        let tx = connection.transaction()?;
        let seed = insert(&tx, AGENT)?;
        tx.commit()?;
        seed
    };
    let before = phase.finish()?;
    connection.execute_batch(
        "CREATE TRIGGER reject_returned_seed BEFORE INSERT ON episode_continuity_seeds
         BEGIN SELECT RAISE(ABORT, 'returned seed SQL failure'); END;",
    )?;
    {
        let _entry = phase.enter();
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO seed_id_markers VALUES ('rollback')", [])?;
        let error = insert(&tx, AGENT).unwrap_err();
        assert!(format!("{error:#}").contains("returned seed SQL failure"));
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            seed._material.as_ref().unwrap()
        ));
        tx.rollback()?;
    }
    let strings = return_cost(AGENT);
    let json = super::super::seed_candidate_tests::seed_insert_json_cost(&[])?;
    let expected = (before.0 + strings.0 + json.0, before.1 + strings.1 + json.1);
    assert_eq!(phase.finish()?, expected);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let counts: (i64, i64) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM episode_continuity_seeds),
                (SELECT COUNT(*) FROM seed_id_markers)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(counts, (1, 0));
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    drop(seed);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn loaded_seed_transfers_original_holder_and_no_phase_call_stays_unbudgeted() -> Result<()> {
    let mut connection = connection("fixed-boundary")?;
    let tx = connection.transaction()?;
    let original = insert(&tx, AGENT)?;
    assert!(original._material.is_none());
    tx.commit()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let loaded = {
        let _entry = phase.enter();
        assert!(find_seed(&connection, "missing")?.is_none());
        let seed = find_seed(&connection, AGENT)?.unwrap();
        assert!(Arc::ptr_eq(seed._material.as_ref().unwrap(), &holder));
        seed
    };
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let retained = phase.finish()?.0;
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, retained);
    assert_eq!(loaded.id, original.id);
    assert_eq!(loaded.agent_id, original.agent_id);
    drop(loaded);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(find_seed(&connection, AGENT)?.unwrap()._material.is_none());
    Ok(())
}
