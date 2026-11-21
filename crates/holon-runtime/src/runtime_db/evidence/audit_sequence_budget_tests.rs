use super::*;

#[test]
fn audit_write_key_uses_original_remainder_static_host_and_last_holder() -> Result<()> {
    for agent_id in ["", "audit-agent", "路径:é"] {
        let budget = ledger()?;
        let body = sequence_key_cost(Some(agent_id));
        let prior = (17 * 8, 3 * 8);
        let exact = add(add(add(header(), header()), prior), body);
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let transaction = phase.transaction()?;
        let key = {
            let _entry = transaction.enter();
            crate::diagnostics::resource_io::charge_owned_metadata(17, 3)?;
            let key = audit_write_sequence_scope(Some(agent_id))?;
            assert!(matches!(key, Cow::Owned(_)));
            assert_eq!(key, format!("agent:{agent_id}"));
            let host = audit_write_sequence_scope(None)?;
            assert!(matches!(host, Cow::Borrowed("host")));
            assert_eq!(phase.finish()?, exact);
            assert_eq!(transaction.finish()?, add(add(header(), prior), body));
            key
        };
        // Exiting TLS must not charge this exhausted phase for inspection.
        assert_eq!(audit_write_sequence_scope(Some(agent_id))?, key);
        assert_eq!(phase.finish()?, exact);
        let holder = Arc::new((key, phase.guard()?, transaction.guard()?));
        let last = holder.clone();
        drop((phase, transaction));
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(holder);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        assert_eq!(last.0, format!("agent:{agent_id}"));
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    {
        let _entry = phase.enter();
        assert!(matches!(
            audit_write_sequence_scope(None)?,
            Cow::Borrowed("host")
        ));
        assert_eq!(phase.finish()?, header());
    }
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

fn write_prefix(
    route: usize,
    incoming: &AuditEvent,
    normalized: &AuditEvent,
) -> Result<(u64, usize)> {
    let epoch = epoch_cost(&normalized.event_log_epoch);
    let body = match route {
        0 => add(clone_cost(incoming)?, epoch),
        1 => add(
            add(epoch, clone_cost(incoming)?),
            add(
                add(wire_cost(&normalized.created_at)?, wire_cost(normalized)?),
                observer_timestamp_cost(),
            ),
        ),
        _ => add(
            add(epoch, clone_cost(incoming)?),
            add(clone_cost(normalized)?, epoch),
        ),
    };
    Ok(add(add(header(), header()), body))
}

#[test]
fn audit_write_key_rejects_each_route_before_allocation_and_rolls_back() -> Result<()> {
    for route in 0..3 {
        for agent_id in ["audit-agent", "路径:é"] {
            let (_home, db) = fixture()?;
            let before = snapshot(&db)?;
            let mut incoming = event();
            incoming.event_seq = if route == 1 { 7 } else { 0 };
            let mut normalized = incoming.clone();
            normalized.event_log_epoch = db.event_log_epoch()?;
            let prefix = write_prefix(route, &incoming, &normalized)?;
            let next = sequence_key_cost(Some(agent_id));
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
                        let result = if route == 0 {
                            append_audit_event_tx(tx, Some(agent_id), &incoming).map(|_| ())
                        } else {
                            import_audit_event_tx(tx, Some(agent_id), &incoming)
                        };
                        exhausted(result.as_ref().unwrap_err());
                        let count: i64 =
                            tx.query_row("SELECT COUNT(*) FROM audit_events", [], |row| {
                                row.get(0)
                            })?;
                        // Nonzero import has already inserted; the original
                        // transaction, not a compensating delete, rolls it back.
                        assert_eq!(count, i64::from(route == 1));
                        result
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
    }
    Ok(())
}

#[test]
fn audit_write_key_exact_routes_preserve_sql_scope_sequence_and_whole_holder() -> Result<()> {
    for route in 0..3 {
        for agent_id in [None, Some(""), Some("路径:é")] {
            let (_home, db) = fixture()?;
            let mut incoming = event();
            incoming.event_seq = if route == 1 { 7 } else { 0 };
            let mut normalized = incoming.clone();
            normalized.event_log_epoch = db.event_log_epoch()?;
            let prefix = write_prefix(route, &incoming, &normalized)?;
            let mut expected = normalized.clone();
            if route != 1 {
                expected.event_seq = 1;
            }
            let mut exact = add(prefix, sequence_key_cost(agent_id));
            if route != 1 {
                exact = add(
                    exact,
                    add(
                        add(wire_cost(&expected.created_at)?, wire_cost(&expected)?),
                        observer_timestamp_cost(),
                    ),
                );
            }
            let budget = ledger()?;
            let phase =
                ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
            let transaction = phase.transaction()?;
            {
                let _entry = transaction.enter();
                db.transaction(|tx| {
                    if route == 0 {
                        assert_eq!(
                            append_audit_event_tx(tx, agent_id, &incoming)?,
                            (expected.clone(), true)
                        );
                        Ok(())
                    } else {
                        import_audit_event_tx(tx, agent_id, &incoming)
                    }
                })?;
                assert_eq!(phase.finish()?, exact);
            }
            let first = Arc::new(vec![phase.guard()?, transaction.guard()?]);
            let last = first.clone();
            drop((phase, transaction));
            assert_eq!(budget.report()?.observed_bytes, exact.0);
            db.transaction(|tx| {
                let (key, head): (String, u64) = tx.query_row(
                    "SELECT scope_key, last_value FROM runtime_sequences WHERE domain='audit_event'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(key, agent_id.map_or_else(|| "host".into(), |id| format!("agent:{id}")));
                assert_eq!(head, expected.event_seq);
                assert_eq!(audit_event_by_id_tx(tx, agent_id, &incoming.id)?, Some(expected.clone()));
                Ok(())
            })?;
            let mut next = incoming.clone();
            next.id = "next-write".into();
            let (next, inserted) =
                db.transaction(|tx| append_audit_event_tx(tx, agent_id, &next))?;
            assert!(inserted);
            assert_eq!(next.event_seq, expected.event_seq + 1);
            assert_eq!(budget.report()?.observed_bytes, exact.0);
            drop(first);
            assert_eq!(budget.report()?.observed_bytes, exact.0);
            drop(last);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn audit_write_key_duplicate_and_conflict_short_circuits_do_not_allocate_key() -> Result<()> {
    for route in 0..3 {
        let (_home, db) = fixture()?;
        let agent_id = Some("路径:é");
        let (stored, _) = db.transaction(|tx| append_audit_event_tx(tx, agent_id, &event()))?;
        let before = snapshot(&db)?;
        let mut incoming = stored.clone();
        if route == 2 {
            incoming.id = "same-sequence-other-id".into();
        }
        let mut exact = add(add(header(), header()), wire_cost(&stored)?);
        if route != 0 {
            exact = add(exact, clone_cost(&incoming)?);
        }
        let budget = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            let result = db.transaction(|tx| {
                if route == 0 {
                    assert_eq!(
                        append_audit_event_tx(tx, agent_id, &incoming)?,
                        (stored.clone(), false)
                    );
                    Ok(())
                } else {
                    import_audit_event_tx(tx, agent_id, &incoming)
                }
            });
            if route == 2 {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("conflicting audit event identity"));
            } else {
                result?;
            }
            assert_eq!(phase.finish()?, exact);
        }
        assert_eq!(snapshot(&db)?, before);
        drop((phase, transaction));
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
