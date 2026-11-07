use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{WaitConditionKind, WakeSource},
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "scheduling-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn cost(bytes: usize, slots: usize) -> (u64, usize) {
    (
        (bytes as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn wire_cost(value: &impl serde::Serialize) -> Result<(u64, usize)> {
    // Independent AST formula; do not reuse the production wire scanner.
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
fn record(cancelled: bool) -> WaitConditionRecord {
    let at = "2026-10-17T11:22:23.123456789Z".parse().unwrap();
    WaitConditionRecord {
        id: "wait-sql".into(),
        agent_id: "scheduling-agent".into(),
        work_item_id: None,
        status: if cancelled {
            WaitConditionStatus::Cancelled
        } else {
            WaitConditionStatus::Resolved
        },
        kind: WaitConditionKind::External,
        source: Some("external".into()),
        subject_ref: Some("topic".into()),
        waiting_for: "{,:} \"x\" \\ 路".into(),
        wake_sources: vec![WakeSource::ExternalIngress {
            external_trigger_id: Some("external-id".into()),
        }],
        continuation: Some(serde_json::json!({"nested": ["{,:} \"\\ 路", 7, null]})),
        created_at: at,
        updated_at: at,
        expires_at: Some(at),
        resolved_at: (!cancelled).then_some(at),
        cancelled_at: cancelled.then_some(at),
        turn_id: None,
        trigger_message_id: Some("wake-message".into()),
        triggered_at: Some(at),
    }
}
fn costs(record: &WaitConditionRecord) -> Result<Vec<(u64, usize)>> {
    let mut costs = vec![wire_cost(record)?, wire_cost(&record.wake_sources)?];
    if let Some(continuation) = &record.continuation {
        costs.push(wire_cost(continuation)?);
    }
    for at in [
        Some(record.created_at),
        Some(record.updated_at),
        record.expires_at,
        record.resolved_at,
        record.cancelled_at,
        record.triggered_at,
    ]
    .into_iter()
    .flatten()
    {
        costs.push(wire_cost(&at)?);
    }
    // Applied row ID owns one String and one logical set slot.
    costs.push((
        (record.id.len() + std::mem::size_of::<String>()) as u64 * 8,
        8,
    ));
    // The applied wait also persists its source revision timestamp.
    costs.push(wire_cost(&record.updated_at)?);
    Ok(costs)
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
fn row_count(db: &crate::runtime_db::RuntimeDb) -> Result<i64> {
    db.transaction(|tx| {
        Ok(tx.query_row("SELECT count(*) FROM wait_conditions", [], |row| row.get(0))?)
    })
}
fn replace_payload(db: &crate::runtime_db::RuntimeDb, payload: &str) -> Result<()> {
    db.transaction(|tx| {
        tx.execute("UPDATE wait_conditions SET payload_json=?1", [payload])?;
        Ok(())
    })
}
fn assert_exhausted(error: anyhow::Error) {
    assert!(format!("{error:#}")
        .to_lowercase()
        .contains("resourceexhausted"));
}

#[test]
fn primary_wait_envelope_matches_independent_cost_and_last_transaction_holder() -> Result<()> {
    for cancelled in [false, true] {
        let (_home, db) = fixture()?;
        let record = record(cancelled);
        let body = costs(&record)?.into_iter().fold((0, 0), add);
        let exact = add(add(header(), header()), body);
        let budget = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            let writes = db.transaction(|tx| upsert_wait_condition_with_writes_tx(tx, &record))?;
            assert!(writes.primary_applied);
            assert_eq!(writes.wait_ids, BTreeSet::from([record.id.clone()]));
            assert!(writes.turn_ids.is_empty());
        }
        assert_eq!(phase.finish()?, exact);
        assert_eq!(transaction.finish()?, add(header(), body));
        db.transaction(|tx| {
            let payload: String =
                tx.query_row("SELECT payload_json FROM wait_conditions", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(payload, serde_json::to_string(&record)?);
            let mut statement = tx.prepare(
                "SELECT created_at,updated_at,expires_at,resolved_at,cancelled_at,triggered_at
                 FROM wait_conditions",
            )?;
            let mut rows = statement.query([])?;
            let row = rows.next()?.unwrap();
            for (index, present) in [true, true, true, !cancelled, cancelled, true]
                .into_iter()
                .enumerate()
            {
                let actual: Option<String> = row.get(index)?;
                assert_eq!(
                    actual.as_deref(),
                    present.then_some("2026-10-17T11:22:23.123Z")
                );
            }
            Ok(())
        })?;
        let first = transaction.guard()?;
        let last = first.clone();
        assert!(std::sync::Arc::ptr_eq(&first, &last));
        drop((record, transaction, phase));
        assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
        drop(first);
        assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn each_primary_wait_envelope_gate_rejects_before_write_and_latches_original_phase() -> Result<()> {
    for cancelled in [false, true] {
        let (_home, db) = fixture()?;
        let record = record(cancelled);
        let mut prefix = add(header(), header());
        for next in costs(&record)? {
            for bytes in [true, false] {
                let budget = ledger()?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &budget,
                    &scope(),
                    if bytes {
                        prefix.0 + next.0 - 1
                    } else {
                        64 * 1024 * 1024
                    },
                    if bytes { 65536 } else { prefix.1 + next.1 - 1 },
                )?;
                let transaction = phase.transaction()?;
                {
                    let _entry = transaction.enter();
                    assert_exhausted(
                        db.transaction(|tx| upsert_wait_condition_tx(tx, &record))
                            .unwrap_err(),
                    );
                    assert!(db
                        .transaction(|tx| upsert_wait_condition_tx(tx, &record))
                        .is_err());
                }
                assert_eq!(row_count(&db)?, 0);
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert!(transaction.finish().is_err());
                drop(transaction);
                assert_eq!(budget.report()?.observed_bytes, header().0);
                assert!(phase.finish().is_err());
                assert!(phase.transaction().is_err());
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, 0);
            }
            prefix = add(prefix, next);
        }
    }
    Ok(())
}

#[test]
fn existing_wait_decode_is_one_borrowed_envelope_and_uses_original_remainder() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = record(false);
    db.transaction(|tx| upsert_wait_condition_tx(tx, &record))?;
    let budget = ledger()?;
    let exact = add(header(), wire_cost(&record)?);
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        let writes = db.transaction(|tx| upsert_wait_condition_with_writes_tx(tx, &record))?;
        assert!(!writes.primary_applied);
        assert!(writes.wait_ids.is_empty());
        assert!(writes.turn_ids.is_empty());
    }
    assert_eq!(phase.finish()?, exact);
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        assert_exhausted(
            db.transaction(|tx| upsert_wait_condition_tx(tx, &record))
                .unwrap_err(),
        );
    }
    assert!(phase.finish().is_err());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(holder);
    assert_eq!(budget.report()?.observed_bytes, 0);

    replace_payload(&db, "{")?;
    for refuse in [true, false] {
        let prefix = add(header(), header());
        let next = cost(1, 1);
        let exact = add(prefix, next);
        let phase = ResourceReadMaterial::with_test_limits(
            &budget,
            &scope(),
            exact.0 - u64::from(refuse),
            exact.1,
        )?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            let error = db
                .transaction(|tx| upsert_wait_condition_tx(tx, &record))
                .unwrap_err();
            if refuse {
                assert_exhausted(error);
                assert!(transaction.finish().is_err());
            } else {
                assert!(format!("{error:#}").contains("decoding wait condition payload"));
                assert_eq!(transaction.finish()?, add(header(), next));
            }
        }
        assert_eq!(
            budget.report()?.observed_bytes,
            if refuse { prefix.0 } else { exact.0 }
        );
        drop(transaction);
        assert_eq!(budget.report()?.observed_bytes, header().0);
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn primary_wait_sql_keeps_enum_null_idempotence_and_stale_write_semantics() -> Result<()> {
    for status in [
        WaitConditionStatus::Resolved,
        WaitConditionStatus::Cancelled,
        WaitConditionStatus::Expired,
    ] {
        for kind in [
            WaitConditionKind::Task,
            WaitConditionKind::External,
            WaitConditionKind::Operator,
            WaitConditionKind::Timer,
            WaitConditionKind::System,
        ] {
            let (_home, db) = fixture()?;
            let mut record = record(false);
            record.status = status.clone();
            record.kind = kind.clone();
            record.continuation = None;
            record.wake_sources.clear();
            record.expires_at = None;
            record.resolved_at = None;
            record.trigger_message_id = None;
            record.triggered_at = None;
            assert!(db.transaction(|tx| upsert_wait_condition_tx(tx, &record))?);
            assert!(!db.transaction(|tx| upsert_wait_condition_tx(tx, &record))?);
            db.transaction(|tx| {
                let columns: (String, String, String, Option<String>) = tx.query_row(
                    "SELECT status,kind,wake_sources_json,continuation_json FROM wait_conditions",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
                assert_eq!(
                    columns,
                    (
                        enum_string(&status)?,
                        enum_string(&kind)?,
                        "[]".into(),
                        None
                    )
                );
                Ok(())
            })?;
            let mut active = record.clone();
            active.status = WaitConditionStatus::Active;
            replace_payload(&db, &serde_json::to_string(&active)?)?;
            let budget = ledger()?;
            let exact = add(header(), wire_cost(&active)?);
            let phase =
                ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
            record.updated_at -= chrono::Duration::seconds(1);
            {
                let _entry = phase.enter();
                assert!(!db.transaction(|tx| upsert_wait_condition_tx(tx, &record))?);
            }
            assert_eq!(phase.finish()?, exact);
            let payload = db.transaction(|tx| {
                Ok(
                    tx.query_row("SELECT payload_json FROM wait_conditions", [], |row| {
                        row.get::<_, String>(0)
                    })?,
                )
            })?;
            assert_eq!(payload, serde_json::to_string(&active)?);
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}
