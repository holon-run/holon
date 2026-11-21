use super::*;

#[test]
fn audit_epoch_checks_borrow_caller_identity_without_allocating() -> Result<()> {
    let (_home, db) = fixture()?;
    let expected = db.event_log_epoch()?;
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    {
        let _entry = phase.enter();
        db.transaction(|tx| {
            let epoch = event_log_epoch_tx(tx, Some(&expected))?;
            assert!(matches!(epoch, Cow::Borrowed(_)));
            assert!(std::ptr::eq(epoch.as_ref().as_ptr(), expected.as_ptr()));
            let error = event_log_epoch_tx(tx, Some("foreign-epoch")).unwrap_err();
            assert!(error.to_string().contains("does not match runtime epoch"));
            Ok(())
        })?;
        assert_eq!(phase.finish()?, header());
    }
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn audit_epoch_gates_reject_each_actual_copy_and_roll_back() -> Result<()> {
    for route in 0..3 {
        for agent_id in [Some("audit-agent"), None] {
            let (_home, db) = fixture()?;
            let before = snapshot(&db)?;
            let mut incoming = event();
            incoming.event_seq = if route == 1 { 7 } else { 0 };
            let mut normalized = incoming.clone();
            normalized.event_log_epoch = db.event_log_epoch()?;
            let epoch = epoch_cost(&normalized.event_log_epoch);
            let steps = match route {
                0 => vec![(false, clone_cost(&incoming)?), (true, epoch)],
                1 => vec![(true, epoch)],
                _ => vec![
                    (true, epoch),
                    (false, clone_cost(&incoming)?),
                    (false, clone_cost(&normalized)?),
                    (true, epoch),
                ],
            };
            let mut prefix = add(header(), header());
            for (is_epoch, next) in steps {
                if is_epoch {
                    for bytes in [true, false] {
                        let budget = ledger()?;
                        let phase = ResourceReadMaterial::with_test_limits(
                            &budget,
                            &scope(),
                            if bytes {
                                prefix.0 + next.0 - 1
                            } else {
                                64 << 20
                            },
                            if bytes { 65536 } else { prefix.1 + next.1 - 1 },
                        )?;
                        let transaction = phase.transaction()?;
                        {
                            let _entry = transaction.enter();
                            let result = db.transaction(|tx| {
                                if route == 0 {
                                    append_audit_event_tx(tx, agent_id, &incoming).map(|_| ())
                                } else {
                                    import_audit_event_tx(tx, agent_id, &incoming)
                                }
                            });
                            exhausted(&result.unwrap_err());
                        }
                        assert_eq!(snapshot(&db)?, before);
                        assert!(incoming.event_log_epoch.is_empty());
                        assert_eq!(budget.report()?.observed_bytes, prefix.0);
                        assert!(transaction.finish().is_err());
                        assert!(phase.transaction().is_err());
                        drop(transaction);
                        assert_eq!(budget.report()?.observed_bytes, header().0);
                        drop(phase);
                        assert_eq!(budget.report()?.observed_bytes, 0);
                    }
                }
                prefix = add(prefix, next);
            }
        }
    }
    Ok(())
}

#[test]
fn audit_epoch_copy_uses_original_remainder_and_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let expected = db.event_log_epoch()?;
    let body = epoch_cost(&expected);
    let prior = (13 * 8, 2 * 8);
    let exact = add(add(add(header(), header()), prior), body);
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let epoch = {
        let _entry = transaction.enter();
        crate::diagnostics::resource_io::charge_owned_metadata(13, 2)?;
        let epoch = db.transaction(|tx| event_log_epoch_tx(tx, None))?;
        assert!(matches!(epoch, Cow::Owned(_)));
        assert_eq!(epoch, expected);
        // Validation still succeeds after the original remainder is exhausted.
        db.transaction(|tx| event_log_epoch_tx(tx, Some(&expected)).map(|_| ()))?;
        assert_eq!(phase.finish()?, exact);
        assert_eq!(transaction.finish()?, add(add(header(), prior), body));
        epoch
    };
    // No TLS material is inherited by persistence inspection.
    db.transaction(|tx| event_log_epoch_tx(tx, None))?;
    assert_eq!(phase.finish()?, exact);
    let holder = Arc::new((epoch, phase.guard()?, transaction.guard()?));
    let last = holder.clone();
    drop((phase, transaction));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(holder);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    assert_eq!(last.0, expected);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn audit_epoch_missing_or_invalid_sql_value_precedes_copy() -> Result<()> {
    for missing in [true, false] {
        let (_home, db) = fixture()?;
        db.transaction(|tx| {
            if missing {
                tx.execute(
                    "DELETE FROM runtime_metadata WHERE key='event_log_epoch'",
                    [],
                )?;
            } else {
                tx.execute(
                    "UPDATE runtime_metadata SET value=x'00ff' WHERE key='event_log_epoch'",
                    [],
                )?;
            }
            Ok(())
        })?;
        let before = snapshot(&db)?;
        let budget = ledger()?;
        let phase =
            ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
        {
            let _entry = phase.enter();
            let error = db
                .transaction(|tx| event_log_epoch_tx(tx, None))
                .unwrap_err();
            if missing {
                assert!(error
                    .to_string()
                    .contains("runtime event-log epoch is missing"));
            } else {
                assert!(error
                    .downcast_ref::<rusqlite::types::FromSqlError>()
                    .is_some());
            }
            let error = db
                .transaction(|tx| import_audit_event_tx(tx, None, &event()))
                .unwrap_err();
            assert!(!format!("{error:#}").contains("ResourceExhausted"));
            assert_eq!(phase.finish()?, header());
        }
        assert_eq!(snapshot(&db)?, before);
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
