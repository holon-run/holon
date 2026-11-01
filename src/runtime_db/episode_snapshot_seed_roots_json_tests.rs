use super::*;

pub(super) fn json_cost(roots: &EpisodeContinuityRoots) -> Result<(u64, usize)> {
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(_) => 1,
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::Object(values) => {
                2 + values.len().saturating_sub(1)
                    + 2 * values.len()
                    + values.values().map(slots).sum::<usize>()
            }
            _ => 0,
        }
    }
    let original = serde_json::to_string(roots)?;
    let slots = slots(&serde_json::from_str(&original)?);
    Ok((
        (original.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    ))
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    }
}

fn baseline() -> (u64, usize) {
    ((AGENT.len() as u64 + 256) * 8, 64)
}

fn roots() -> EpisodeContinuityRoots {
    EpisodeContinuityRoots {
        work_item_ids: ["work-z", "", "\"\\\0\n界\u{2028}"]
            .map(str::to_owned)
            .into(),
        source_refs: ["episode:a", "message:{\"punctuation\":[]}"]
            .map(str::to_owned)
            .into(),
        record_digests: [
            ("z".into(), "digest".into()),
            ("\"\\\0界".into(), "界\0\"\\".repeat(16_384)),
        ]
        .into(),
        unavailable_refs: ["task:missing".into()].into(),
        ..EpisodeContinuityRoots::default()
    }
}

fn insert_roots(
    tx: &Transaction<'_>,
    roots: &EpisodeContinuityRoots,
    records: &[EpisodeRow],
) -> Result<ContinuitySeed> {
    insert_seed_with_candidates_tx(
        tx,
        AGENT,
        None,
        "upgrade_unbound",
        roots,
        false,
        "2026-10-18T00:00:00Z".parse()?,
        records,
    )
}

#[test]
fn roots_json_preserves_default_sorted_and_escaped_bytes_at_independent_exact_cost() -> Result<()> {
    for roots in [EpisodeContinuityRoots::default(), roots()] {
        let expected = serde_json::to_string(&roots)?;
        assert_eq!(continuity_roots_json(&roots)?, expected);
        let base = baseline();
        let json = json_cost(&roots)?;
        let exact = (base.0 + json.0, base.1 + json.1);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            assert_eq!(continuity_roots_json(&roots)?, expected);
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
fn roots_json_rejects_both_dimensions_on_the_original_used_remainder() -> Result<()> {
    let roots = roots();
    let base = baseline();
    let preload = (47 * 8, 3 * 8);
    let json = json_cost(&roots)?;
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
            let error = continuity_roots_json(&roots).unwrap_err();
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
fn roots_json_insert_refuses_before_candidate_json_and_sql_with_whole_tx_rollback() -> Result<()> {
    let roots = roots();
    let records = rows()?;
    let base = baseline();
    let nodes = node_cost(&records)?;
    let json = json_cost(&roots)?;
    let text = seed_text_cost(AGENT);
    let before_roots = (base.0 + text.0 + nodes.0, base.1 + text.1 + nodes.1);
    let exact = (before_roots.0 + json.0, before_roots.1 + json.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let holder = phase.guard()?;
        let mut connection = connection()?;
        connection.execute_batch(
            "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
             BEGIN SELECT RAISE(ABORT, 'must not reach roots JSON SQL'); END;",
        )?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO candidate_markers VALUES ('rollback')", [])?;
            let error = insert_roots(&tx, &roots, &records).unwrap_err();
            assert!(error.downcast_ref::<std::io::Error>().is_some());
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            assert!(!format!("{error:#}").contains("must not reach roots JSON SQL"));
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
            // The boundary reader must have unwound even before the caller rolls back.
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
        assert_eq!(ledger.report()?.observed_bytes, before_roots.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn roots_json_sql_success_and_failure_keep_original_phase_and_last_holder() -> Result<()> {
    let roots = roots();
    let records = rows()?;
    let expected = serde_json::to_string(&roots)?;
    let base = baseline();
    let preload = (59 * 8, 2 * 8);
    let nodes = node_cost(&records)?;
    let json = json_cost(&roots)?;
    let candidates = candidate_json_cost(&records)?;
    let text = seed_text_cost(AGENT);
    let per_call = (
        text.0 + nodes.0 + json.0 + candidates.0,
        text.1 + nodes.1 + json.1 + candidates.1,
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
        charge_owned_metadata(59, 2)?;
        for (index, fail) in [false, true].into_iter().enumerate() {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'roots JSON SQL failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO candidate_markers VALUES ('rollback')", [])?;
            let result = insert_roots(&tx, &roots, &records);
            if fail {
                let error = result.unwrap_err();
                assert!(error.downcast_ref::<rusqlite::Error>().is_some());
                assert!(format!("{error:#}").contains("roots JSON SQL failure"));
            } else {
                result?;
                assert_eq!(
                    tx.query_row(
                        "SELECT roots_json FROM episode_continuity_seeds",
                        [],
                        |row| { row.get::<_, String>(0) }
                    )?,
                    expected
                );
                assert_eq!(stored_json(&tx)?, original_json(&records)?);
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
