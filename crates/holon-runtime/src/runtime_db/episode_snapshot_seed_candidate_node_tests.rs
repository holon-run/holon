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
fn candidate_nodes_charge_only_unique_unreceipted_keys_and_preserve_original_json() -> Result<()> {
    let records = rows()?;
    let expected = original_json(&records)?;
    let original: BTreeMap<String, String> = serde_json::from_str(&expected)?;
    let nodes = original.len();
    let base = baseline();
    let exact = (base.0 + nodes as u64 * 256 * 8, base.1 + nodes * 8);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let candidates = seed_candidate_payloads(&records)?;
        assert_eq!(serde_json::to_string(&candidates)?, expected);
        assert_eq!(phase.finish()?, exact);
        for (key, _) in candidates {
            assert!(records.iter().any(|row| {
                !row.receipted
                    && key.len() == row.record.id.len()
                    && std::ptr::eq(key.as_ptr(), row.record.id.as_ptr())
            }));
        }
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn candidate_nodes_empty_receipted_and_duplicate_replacement_need_no_new_allowance() -> Result<()> {
    let records = rows()?;
    let base = baseline();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), base.0, base.1)?;
    {
        let _entry = phase.enter();
        assert!(seed_candidate_payloads(&[])?.is_empty());
        let receipted: Vec<_> = records.into_iter().filter(|row| row.receipted).collect();
        assert!(seed_candidate_payloads(&receipted)?.is_empty());
        assert_eq!(phase.finish()?, base);
    }
    drop(phase);
    let mut records = rows()?;
    records.retain(|row| row.record.id == "a");
    let exact = (base.0 + 256 * 8, base.1 + 8);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        let candidates = seed_candidate_payloads(&records)?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            serde_json::to_string(&candidates)?,
            original_json(&records)?
        );
        assert_eq!(phase.finish()?, exact);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn candidate_nodes_reject_each_new_key_on_both_dimensions_and_rollback_before_sql() -> Result<()> {
    let records = rows()?;
    let original: BTreeMap<String, String> = serde_json::from_str(&original_json(&records)?)?;
    let header = baseline();
    let text = seed_text_cost(AGENT);
    let base = (header.0 + text.0, header.1 + text.1);
    for admitted in 0..original.len() {
        for bytes_limited in [true, false] {
            let exact = (base.0 + admitted as u64 * 256 * 8, base.1 + admitted * 8);
            let limits = if bytes_limited {
                (exact.0 + 256 * 8 - 1, base.1 + original.len() * 8)
            } else {
                (base.0 + original.len() as u64 * 256 * 8, exact.1 + 8 - 1)
            };
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
            let holder = phase.guard()?;
            let mut connection = connection()?;
            connection.execute_batch(
                "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                 BEGIN SELECT RAISE(ABORT, 'must not reach SQL'); END;",
            )?;
            {
                let _entry = phase.enter();
                let tx = connection.transaction()?;
                tx.execute("INSERT INTO candidate_markers VALUES ('rollback')", [])?;
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                let error = insert(&tx, &records).unwrap_err();
                assert!(
                    error.downcast_ref::<std::io::Error>().is_some(),
                    "{error:#}"
                );
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(phase.finish().is_err());
                // The failed collector and boundary reader have both unwound.
                tx.execute_batch("DROP TABLE runtime_metadata;")?;
                tx.rollback()?;
                for table in ["candidate_markers", "episode_continuity_seeds"] {
                    assert_eq!(
                        connection.query_row(
                            &format!("SELECT COUNT(*) FROM {table}"),
                            [],
                            |row| row.get::<_, u64>(0)
                        )?,
                        0
                    );
                }
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let last = holder.clone();
            drop((phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, exact.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn candidate_nodes_sql_success_and_failure_keep_original_phase_until_last_holder() -> Result<()> {
    let records = rows()?;
    let expected = original_json(&records)?;
    let original: BTreeMap<String, String> = serde_json::from_str(&expected)?;
    let base = baseline();
    let preload = (37 * 8, 2 * 8);
    let json = seed_insert_cost(AGENT, &records)?;
    let per_call = (
        original.len() as u64 * 256 * 8 + json.0,
        original.len() * 8 + json.1,
    );
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
        charge_owned_metadata(37, 2)?;
        for (index, fail) in [false, true].into_iter().enumerate() {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'candidate node SQL failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO candidate_markers VALUES ('rollback')", [])?;
            let result = insert(&tx, &records);
            if fail {
                let error = result.unwrap_err();
                assert!(error.downcast_ref::<rusqlite::Error>().is_some());
                assert!(format!("{error:#}").contains("candidate node SQL failure"));
            } else {
                result?;
                assert_eq!(stored_json(&tx)?, expected);
            }
            tx.execute_batch("DROP TABLE runtime_metadata;")?;
            tx.rollback()?;
            for table in ["candidate_markers", "episode_continuity_seeds"] {
                assert_eq!(
                    connection
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                            .get::<_, u64>(0))?,
                    0
                );
            }
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
