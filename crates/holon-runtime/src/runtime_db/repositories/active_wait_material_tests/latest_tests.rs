use super::*;

fn latest_fixture(
    count: usize,
) -> Result<(tempfile::TempDir, RuntimeDb, Vec<WaitConditionRecord>)> {
    let (home, db, mut expected) = fixture(count)?;
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    for (index, row) in expected.iter_mut().enumerate() {
        row.status = [
            WaitConditionStatus::Active,
            WaitConditionStatus::Triggered,
            WaitConditionStatus::Resolved,
            WaitConditionStatus::Cancelled,
            WaitConditionStatus::Expired,
        ][index % 5]
            .clone();
        db.wait_conditions().upsert(row)?;
    }
    Ok((home, db, expected))
}

#[test]
fn latest_wait_explicit_material_preserves_all_statuses_order_cost_and_last_holder() -> Result<()> {
    for count in [0, 1, 4, 5, 9] {
        let (_home, db, expected) = latest_fixture(count)?;
        if let Some(first) = expected.first() {
            let mut other = first.clone();
            other.id = "foreign".into();
            other.agent_id = "foreign".into();
            db.wait_conditions().upsert(&other)?;
        }
        assert_eq!(db.wait_conditions().latest_for_agent("web")?, expected);
        let exact = gates(&expected).into_iter().fold(header(), add);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let original = phase.guard()?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let records = db
            .wait_conditions()
            .latest_for_agent_with_material("web", &phase)?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(records, expected);
        assert_eq!(phase.finish()?, exact);
        assert!(Arc::ptr_eq(&original, &phase.guard()?));
        let held = Arc::new((records, original));
        let last = held.clone();
        drop((held, phase));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        assert_eq!(last.0, expected);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn latest_wait_explicit_material_refuses_each_all_status_gate_and_latches_empty_retry() -> Result<()>
{
    let (_home, db, expected) = latest_fixture(9)?;
    let mut prefix = header();
    for gate in gates(&expected) {
        let boundary = add(prefix, gate);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes {
                    boundary.0 - 1
                } else {
                    64 * 1024 * 1024
                },
                if bytes { 65536 } else { boundary.1 - 1 },
            )?;
            let original = phase.guard()?;
            let error = db
                .wait_conditions()
                .latest_for_agent_with_material("web", &phase)
                .unwrap_err();
            assert!(error.downcast_ref::<std::io::Error>().is_some());
            assert!(format!("{error:#}")
                .to_lowercase()
                .contains("resourceexhausted"));
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            let latched = phase.finish().unwrap_err().to_string();
            for agent in ["web", "absent"] {
                let retry = db
                    .wait_conditions()
                    .latest_for_agent_with_material(agent, &phase)
                    .unwrap_err();
                assert_eq!(retry.to_string(), latched);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(ResourceReadMaterial::active_guard()?.is_none());
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(original);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        prefix = boundary;
    }
    Ok(())
}

#[test]
fn latest_wait_explicit_material_decodes_filtered_rows_and_preserves_legacy_errors() -> Result<()> {
    let (_home, db, expected) = latest_fixture(5)?;
    let last = expected.last().unwrap();
    assert_eq!(last.status, WaitConditionStatus::Expired);
    for (column, replacement) in [
        ("created_at", "'bad'"),
        ("wake_sources_json", "'[{]'"),
        ("continuation_json", "'{'"),
    ] {
        db.wait_conditions().upsert(last)?;
        let connection = db.connection()?;
        connection.execute(
            &format!(
                "UPDATE wait_conditions SET {column}={replacement} WHERE wait_condition_id=?1"
            ),
            [&last.id],
        )?;
        let ordinary = db.wait_conditions().latest_for_agent("web").unwrap_err();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let error = db
            .wait_conditions()
            .latest_for_agent_with_material("web", &phase)
            .unwrap_err();
        assert_eq!(format!("{error:#}"), format!("{ordinary:#}"), "{column}");
        let charged = phase.finish()?;
        let prefix = gates(&expected[..4]).into_iter().fold(header(), add);
        assert!(charged.0 > prefix.0);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, charged.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
