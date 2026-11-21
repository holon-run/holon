use super::*;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    }
}

fn baseline() -> (u64, usize) {
    ((AGENT.len() as u64 + 256) * 8, 64)
}

#[test]
fn candidate_json_preserves_empty_filtered_duplicate_and_escaped_bytes_at_exact_cost() -> Result<()>
{
    let receipted = rows()?.into_iter().filter(|row| row.receipted).collect();
    for records in [Vec::new(), receipted, rows()?] {
        let expected = original_json(&records)?;
        let candidates = seed_candidate_payloads(&records)?;
        assert_eq!(seed_candidate_payloads_json(&candidates)?, expected);
        let base = baseline();
        let json = candidate_json_cost(&records)?;
        let exact = (base.0 + json.0, base.1 + json.1);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            assert_eq!(seed_candidate_payloads_json(&candidates)?, expected);
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn candidate_json_rejects_both_dimensions_using_the_original_used_remainder() -> Result<()> {
    let records = rows()?;
    let candidates = seed_candidate_payloads(&records)?;
    let base = baseline();
    let preload = (47 * 8, 3 * 8);
    let json = candidate_json_cost(&records)?;
    let exact = (base.0 + preload.0 + json.0, base.1 + preload.1 + json.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(47, 3)?;
            assert_eq!(phase.finish()?, (base.0 + preload.0, base.1 + preload.1));
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            let error = seed_candidate_payloads_json(&candidates).unwrap_err();
            assert!(error.downcast_ref::<std::io::Error>().is_some());
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let last = holder.clone();
        drop((phase, holder));
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn candidate_json_insert_refuses_before_sql_and_releases_reader_for_whole_tx_rollback() -> Result<()>
{
    let records = rows()?;
    let base = baseline();
    let nodes = node_cost(&records)?;
    let json = seed_insert_cost(AGENT, &records)?;
    let exact = (base.0 + nodes.0 + json.0, base.1 + nodes.1 + json.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        let mut connection = connection()?;
        connection.execute_batch(
            "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
             BEGIN SELECT RAISE(ABORT, 'must not reach candidate JSON SQL'); END;",
        )?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO candidate_markers VALUES ('rollback')", [])?;
            let error = insert(&tx, &records).unwrap_err();
            assert!(error.downcast_ref::<std::io::Error>().is_some());
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            assert!(!format!("{error:#}").contains("must not reach candidate JSON SQL"));
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
            tx.execute_batch("DROP TABLE runtime_metadata;")?;
            tx.rollback()?;
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        for table in ["candidate_markers", "episode_continuity_seeds"] {
            assert_eq!(
                connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, u64>(0)
                })?,
                0
            );
        }
        assert_eq!(
            connection.query_row(
                "SELECT value FROM runtime_metadata WHERE key = ?1",
                [UPGRADE_BOUNDARY_KEY],
                |row| row.get::<_, String>(0),
            )?,
            "candidate-boundary"
        );
        let last = holder.clone();
        drop((phase, holder));
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn candidate_json_sql_success_and_error_keep_the_same_phase_and_last_holder() -> Result<()> {
    let records = rows()?;
    let expected = original_json(&records)?;
    let base = baseline();
    let preload = (59 * 8, 2 * 8);
    let nodes = node_cost(&records)?;
    let json = seed_insert_cost(AGENT, &records)?;
    let per_call = (nodes.0 + json.0, nodes.1 + json.1);
    let exact = (
        base.0 + preload.0 + 2 * per_call.0,
        base.1 + preload.1 + 2 * per_call.1,
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    let mut connection = connection()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(59, 2)?;
        for (index, fail) in [false, true].into_iter().enumerate() {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'candidate JSON SQL failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO candidate_markers VALUES ('rollback')", [])?;
            let result = insert(&tx, &records);
            if fail {
                let error = result.unwrap_err();
                assert!(error.downcast_ref::<rusqlite::Error>().is_some());
                assert!(format!("{error:#}").contains("candidate JSON SQL failure"));
            } else {
                result?;
                assert_eq!(stored_json(&tx)?, expected);
            }
            tx.execute_batch("DROP TABLE runtime_metadata;")?;
            tx.rollback()?;
            assert_eq!(
                phase.finish()?,
                (
                    base.0 + preload.0 + (index as u64 + 1) * per_call.0,
                    base.1 + preload.1 + (index + 1) * per_call.1
                )
            );
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            for table in ["candidate_markers", "episode_continuity_seeds"] {
                assert_eq!(
                    connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get::<_, u64>(0)
                    })?,
                    0
                );
            }
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
