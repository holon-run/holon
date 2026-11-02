use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

#[path = "audit_epoch_budget_tests.rs"]
mod epoch_budget_tests;

#[path = "audit_sequence_budget_tests.rs"]
mod sequence_budget_tests;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "audit-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn cost(bytes: usize, slots: usize) -> (u64, usize) {
    (
        (bytes as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}
fn wire_cost(value: &impl Serialize) -> Result<(u64, usize)> {
    // Independent AST formula; never call the production scanner or gate.
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len().saturating_sub(1)
                    + fields.values().map(|value| 2 + slots(value)).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let payload = serde_json::to_string(value)?;
    Ok(cost(payload.len(), slots(&serde_json::from_str(&payload)?)))
}
fn clone_cost(value: &impl Serialize) -> Result<(u64, usize)> {
    // Same independent AST byte bound, but no parser/resolver record allowance.
    let (bytes, decoded_records) = wire_cost(value)?;
    let slots = (decoded_records / 8 - 16) / 4;
    Ok((bytes, (slots + 16) * 8))
}
fn epoch_cost(epoch: &str) -> (u64, usize) {
    // One owned String, not a JSON parser or typed-body clone.
    (
        ((epoch.len() + std::mem::size_of::<String>()) as u64) * 8,
        8,
    )
}
fn sequence_key_cost(agent_id: Option<&str>) -> (u64, usize) {
    agent_id.map_or((0, 0), |id| {
        (
            ((6 + id.len() + std::mem::size_of::<String>()) as u64) * 8,
            8,
        )
    })
}
fn observer_timestamp_cost() -> (u64, usize) {
    (((24 + std::mem::size_of::<String>()) as u64) * 8, 8)
}
fn event() -> AuditEvent {
    let mut event = AuditEvent::legacy(
        "audit_budget_fixture",
        serde_json::json!({"nested": [null, {"text": "\\\"{}[]:, 路"}, []]}),
    );
    event.id = "audit-budget-fixture".into();
    event.created_at = "2026-10-17T17:40:23.123456789Z".parse().unwrap();
    event
}
fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn exhausted(error: &anyhow::Error) {
    assert!(format!("{error:#}")
        .to_lowercase()
        .contains("resourceexhausted"));
}
fn snapshot(db: &crate::runtime_db::RuntimeDb) -> Result<(i64, String, String)> {
    db.transaction(|tx| {
        Ok(tx.query_row(
            "SELECT (SELECT COUNT(*) FROM audit_events),
                    (SELECT COALESCE(group_concat(data_json, '|'), '') FROM
                        (SELECT data_json FROM audit_events ORDER BY audit_event_id)),
                    (SELECT COALESCE(group_concat(value, '|'), '') FROM
                        (SELECT domain || ':' || scope_key || ':' || last_value AS value
                         FROM runtime_sequences ORDER BY domain, scope_key))",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    })
}

#[test]
fn audit_roundtrip_uses_original_remaining_phase_and_last_whole_holder() -> Result<()> {
    for agent_id in [Some("audit-agent"), None] {
        let (_home, db) = fixture()?;
        let budget = ledger()?;
        let mut expected = event();
        expected.event_log_epoch = db.event_log_epoch()?;
        expected.event_seq = 1;
        let write = add(
            add(wire_cost(&expected.created_at)?, wire_cost(&expected)?),
            observer_timestamp_cost(),
        );
        let reads = add(wire_cost(&expected)?, wire_cost(&expected)?);
        let clones = add(clone_cost(&event())?, clone_cost(&expected)?);
        let body = add(
            add(add(write, reads), clones),
            add(
                epoch_cost(&expected.event_log_epoch),
                sequence_key_cost(agent_id),
            ),
        );
        let exact = add(add(header(), header()), body);
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            let (stored, inserted) =
                db.transaction(|tx| append_audit_event_tx(tx, agent_id, &event()))?;
            assert!(inserted);
            assert_eq!(stored, expected);
            assert_eq!(
                db.transaction(|tx| append_audit_event_tx(tx, agent_id, &event()))?,
                (expected.clone(), false)
            );
            db.transaction(|tx| import_audit_event_tx(tx, agent_id, &expected))?;
        }
        assert_eq!(phase.finish()?, exact);
        assert_eq!(transaction.finish()?, add(header(), body));
        let first = Arc::new(vec![phase.guard()?, transaction.guard()?]);
        let last = first.clone();
        assert!(Arc::ptr_eq(&first, &last));
        // TLS has restored the caller: unqualified inspection cannot consume
        // the exhausted phase's remaining allowance or mint a new one.
        db.transaction(|tx| import_audit_event_tx(tx, agent_id, &expected))?;
        assert_eq!(phase.finish()?, exact);
        {
            let _entry = transaction.enter();
            exhausted(
                &db.transaction(|tx| append_audit_event_tx(tx, agent_id, &event()))
                    .unwrap_err(),
            );
        }
        assert!(phase.finish().is_err());
        drop((phase, transaction, expected));
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(first);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn each_audit_output_gate_rolls_back_sequence_before_insert_and_releases() -> Result<()> {
    for import in [false, true] {
        let (_home, db) = fixture()?;
        let before = snapshot(&db)?;
        let mut record = event();
        record.event_seq = 7;
        record.event_log_epoch = db.event_log_epoch()?;
        let mut written = record.clone();
        if !import {
            written.event_seq = 1;
        }
        let mut prefix = add(add(header(), header()), clone_cost(&record)?);
        if !import {
            prefix = add(prefix, epoch_cost(&written.event_log_epoch));
            prefix = add(prefix, sequence_key_cost(Some("audit-agent")));
        }
        for next in [wire_cost(&written.created_at)?, wire_cost(&written)?] {
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
                        if import {
                            import_audit_event_tx(tx, Some("audit-agent"), &record)
                        } else {
                            append_audit_event_tx(tx, Some("audit-agent"), &record).map(|_| ())
                        }
                    });
                    exhausted(&result.unwrap_err());
                }
                assert_eq!(snapshot(&db)?, before);
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert!(transaction.finish().is_err());
                assert!(phase.transaction().is_err());
                drop(transaction);
                assert_eq!(budget.report()?.observed_bytes, header().0);
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, 0);
            }
            prefix = add(prefix, next);
        }
    }
    Ok(())
}

#[test]
fn all_three_borrowed_audit_reads_gate_before_parser_and_release() -> Result<()> {
    for route in 0..3 {
        let (_home, db) = fixture()?;
        let (stored, _) =
            db.transaction(|tx| append_audit_event_tx(tx, Some("audit-agent"), &event()))?;
        let mut incoming = stored.clone();
        if route == 2 {
            incoming.id = "different-id-same-sequence".into();
        }
        db.transaction(|tx| {
            tx.execute("UPDATE audit_events SET data_json='{'", [])?;
            Ok(())
        })?;
        let read = |tx: &Transaction<'_>| {
            if route == 0 {
                append_audit_event_tx(tx, Some("audit-agent"), &incoming).map(|_| ())
            } else {
                import_audit_event_tx(tx, Some("audit-agent"), &incoming)
            }
        };
        let base = add(header(), header());
        let prefix = if route == 0 {
            base
        } else {
            add(base, clone_cost(&incoming)?)
        };
        let next = cost(1, 1);
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
                assert!(db
                    .transaction(|tx| audit_event_by_id_tx(tx, Some("audit-agent"), "missing"))?
                    .is_none());
                assert_eq!(phase.finish()?, base);
                exhausted(&db.transaction(read).unwrap_err());
                assert!(db.transaction(read).is_err());
            }
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            assert!(transaction.finish().is_err());
            drop(transaction);
            assert_eq!(budget.report()?.observed_bytes, header().0);
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
        let budget = ledger()?;
        let phase = ResourceReadMaterial::new(&budget, &scope())?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            let error = db.transaction(read).unwrap_err();
            assert!(error.downcast_ref::<serde_json::Error>().is_some());
        }
        assert_eq!(phase.finish()?, add(prefix, next));
        drop((transaction, phase));
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn audit_identity_conflicts_short_circuit_and_import_contract_is_unchanged() -> Result<()> {
    let (_home, db) = fixture()?;
    let (stored, _) =
        db.transaction(|tx| append_audit_event_tx(tx, Some("audit-agent"), &event()))?;
    let before = snapshot(&db)?;
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    {
        let _entry = phase.enter();
        for agent_id in [Some("foreign-agent"), None] {
            let error = db
                .transaction(|tx| append_audit_event_tx(tx, agent_id, &event()))
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("conflicting audit event agent identity"));
        }
        let mut foreign = stored.clone();
        foreign.event_log_epoch = "foreign-epoch".into();
        let error = db
            .transaction(|tx| import_audit_event_tx(tx, Some("audit-agent"), &foreign))
            .unwrap_err();
        assert!(error.to_string().contains("does not match runtime epoch"));
    }
    assert_eq!(phase.finish()?, header());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    for sequence in [false, true] {
        let mut conflict = stored.clone();
        if sequence {
            conflict.id = "same-sequence-conflict".into();
        } else {
            conflict.event_seq += 1;
        }
        let error = db
            .transaction(|tx| import_audit_event_tx(tx, Some("audit-agent"), &conflict))
            .unwrap_err();
        assert!(error.to_string().contains(if sequence {
            "conflicting audit event identity"
        } else {
            "conflicting audit event sequence"
        }));
        assert_eq!(snapshot(&db)?, before);
    }
    let mut imported = event();
    imported.id = "new-import".into();
    imported.event_seq = 7;
    db.transaction(|tx| import_audit_event_tx(tx, Some("audit-agent"), &imported))?;
    let next = db.transaction(|tx| {
        let mut next = event();
        next.id = "after-import".into();
        append_audit_event_tx(tx, Some("audit-agent"), &next)
    })?;
    assert!(next.1);
    assert_eq!(next.0.event_seq, 8);
    assert_eq!(next.0.event_log_epoch, stored.event_log_epoch);
    Ok(())
}

#[test]
fn audit_body_clone_gates_reject_each_copy_and_roll_back() -> Result<()> {
    for route in 0..3 {
        let (_home, db) = fixture()?;
        let before = snapshot(&db)?;
        let mut incoming = event();
        if route == 1 {
            incoming.event_seq = 7;
        }
        let mut normalized = incoming.clone();
        normalized.event_log_epoch = db.event_log_epoch()?;
        let mut copies = vec![clone_cost(&incoming)?];
        if route == 2 {
            copies.push(clone_cost(&normalized)?);
        }
        let mut prefix = add(header(), header());
        if route != 0 {
            prefix = add(prefix, epoch_cost(&normalized.event_log_epoch));
        }
        for next in copies {
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
                            append_audit_event_tx(tx, Some("audit-agent"), &incoming).map(|_| ())
                        } else {
                            import_audit_event_tx(tx, Some("audit-agent"), &incoming)
                        }
                    });
                    exhausted(&result.unwrap_err());
                }
                assert_eq!(snapshot(&db)?, before);
                assert_eq!(incoming.event_log_epoch, "");
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert!(transaction.finish().is_err());
                assert!(phase.transaction().is_err());
                drop(transaction);
                assert_eq!(budget.report()?.observed_bytes, header().0);
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, 0);
            }
            prefix = add(prefix, next);
        }
    }
    Ok(())
}

#[test]
fn audit_append_body_uses_original_remaining_phase_and_last_holder() -> Result<()> {
    for agent_id in [Some("audit-agent"), None] {
        let (_home, db) = fixture()?;
        let incoming = event();
        let mut expected = incoming.clone();
        expected.event_log_epoch = db.event_log_epoch()?;
        expected.event_seq = 1;
        let body = add(
            add(
                clone_cost(&incoming)?,
                add(
                    epoch_cost(&expected.event_log_epoch),
                    sequence_key_cost(agent_id),
                ),
            ),
            add(
                add(wire_cost(&expected.created_at)?, wire_cost(&expected)?),
                observer_timestamp_cost(),
            ),
        );
        let prior = (13 * 8, 2 * 8);
        let exact = add(add(add(header(), header()), prior), body);
        let budget = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let transaction = phase.transaction()?;
        let stored = {
            let _entry = transaction.enter();
            crate::diagnostics::resource_io::charge_owned_metadata(13, 2)?;
            let (stored, inserted) =
                db.transaction(|tx| append_audit_event_tx(tx, agent_id, &incoming))?;
            assert!(inserted);
            assert_eq!(stored, expected);
            assert_eq!(transaction.finish()?, add(add(header(), prior), body));
            stored
        };
        assert_eq!(phase.finish()?, exact);
        let holder = Arc::new((stored, phase.guard()?, transaction.guard()?));
        let last = holder.clone();
        drop((phase, transaction, incoming, expected));
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(holder);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        assert_eq!(last.0.event_seq, 1);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn audit_import_body_counts_both_zero_sequence_copies_on_original_phase() -> Result<()> {
    for sequence in [0, 7] {
        let (_home, db) = fixture()?;
        let mut incoming = event();
        incoming.event_seq = sequence;
        let mut normalized = incoming.clone();
        normalized.event_log_epoch = db.event_log_epoch()?;
        let copies = if sequence == 0 {
            add(clone_cost(&incoming)?, clone_cost(&normalized)?)
        } else {
            clone_cost(&incoming)?
        };
        let mut expected = normalized.clone();
        expected.event_seq = sequence.max(1);
        let body = add(
            add(
                copies,
                if sequence == 0 {
                    add(
                        epoch_cost(&expected.event_log_epoch),
                        epoch_cost(&expected.event_log_epoch),
                    )
                } else {
                    epoch_cost(&expected.event_log_epoch)
                },
            ),
            add(
                add(wire_cost(&expected.created_at)?, wire_cost(&expected)?),
                observer_timestamp_cost(),
            ),
        );
        let exact = add(add(header(), header()), body);
        let budget = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            db.transaction(|tx| import_audit_event_tx(tx, None, &incoming))?;
            assert_eq!(phase.finish()?, exact);
            assert_eq!(transaction.finish()?, add(header(), body));
        }
        let stored = db
            .transaction(|tx| audit_event_by_id_tx(tx, None, &incoming.id))?
            .unwrap();
        assert_eq!(stored, expected);
        assert_eq!(incoming.event_log_epoch, "");
        // Persistence import returns no runtime carrier; retain both original
        // guards explicitly, without claiming ownership for a bare SQL read.
        let holder = Arc::new((phase.guard()?, transaction.guard()?));
        let last = holder.clone();
        drop((phase, transaction));
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(holder);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn audit_duplicate_and_invalid_epoch_short_circuit_before_typed_clone() -> Result<()> {
    let (_home, db) = fixture()?;
    let incoming = event();
    let (stored, _) =
        db.transaction(|tx| append_audit_event_tx(tx, Some("audit-agent"), &incoming))?;
    let before = snapshot(&db)?;
    let exact = add(header(), wire_cost(&stored)?);
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let mut foreign = stored.clone();
    foreign.event_log_epoch = "foreign-epoch".into();
    foreign.data = serde_json::json!({"unrelated": "body".repeat(4096)});
    {
        let _entry = phase.enter();
        assert_eq!(
            db.transaction(|tx| append_audit_event_tx(tx, Some("audit-agent"), &incoming))?,
            (stored, false)
        );
        let error = db
            .transaction(|tx| import_audit_event_tx(tx, Some("audit-agent"), &foreign))
            .unwrap_err();
        assert!(error.to_string().contains("does not match runtime epoch"));
        assert_eq!(phase.finish()?, exact);
    }
    assert_eq!(snapshot(&db)?, before);
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
