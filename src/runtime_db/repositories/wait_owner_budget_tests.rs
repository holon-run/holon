use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

type Cost = (u64, usize);
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "wait-owner-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> Cost {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: Cost, b: Cost) -> Cost {
    (a.0 + b.0, a.1 + b.1)
}
fn cost(bytes: usize, slots: usize) -> Cost {
    (
        (bytes as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}
fn wire(value: &impl serde::Serialize) -> Result<Cost> {
    // Independent AST formula, not the production punctuation scanner.
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len().saturating_sub(1)
                    + fields.values().map(|v| 2 + slots(v)).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let text = serde_json::to_string(value)?;
    Ok(cost(text.len(), slots(&serde_json::from_str(&text)?)))
}
fn metadata(bytes: usize) -> Cost {
    (bytes as u64 * 8, 8)
}
fn record(id: &str, seconds: i64) -> WaitConditionRecord {
    let at = "2026-10-17T13:42:30.123456789Z"
        .parse::<DateTime<Utc>>()
        .unwrap()
        + chrono::Duration::seconds(seconds);
    serde_json::from_value(serde_json::json!({
        "id": id, "agent_id": scope().agent_id, "status": "active", "kind": "external",
        "waiting_for": "{,:} \"x\" \\ 路",
        "wake_sources": [{"kind": "external_ingress", "external_trigger_id": "owner-trigger"}],
        "continuation": {"nested": [null, 7, "{,:} \"\\ 路"]},
        "turn_id": "owner-turn", "created_at": at, "updated_at": at,
    }))
    .unwrap()
}
fn cancelled(old: &WaitConditionRecord, replacement: &WaitConditionRecord) -> WaitConditionRecord {
    let mut value = old.clone();
    value.status = WaitConditionStatus::Cancelled;
    value.updated_at = replacement.updated_at;
    value.resolved_at = None;
    value.cancelled_at = Some(replacement.updated_at);
    value
}
fn read_costs(sql_id: &str, old: &WaitConditionRecord) -> Result<Vec<Cost>> {
    Ok(vec![
        wire(old)?,
        metadata(sql_id.len() + std::mem::size_of::<(String, WaitConditionRecord)>()),
    ])
}
fn write_costs(old: &WaitConditionRecord, new: &WaitConditionRecord) -> Result<Vec<Cost>> {
    Ok(vec![
        wire(&cancelled(old, new))?,
        wire(&new.updated_at)?,
        metadata(old.id.len() + std::mem::size_of::<String>()),
        metadata(old.turn_id.as_ref().unwrap().len() + std::mem::size_of::<String>()),
        // Source and initially absent turn revisions each persist a timestamp.
        wire(&new.updated_at)?,
        wire(&new.updated_at)?,
        metadata(std::mem::size_of::<String>()),
    ])
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
fn seed(
    db: &crate::runtime_db::RuntimeDb,
    sql_id: &str,
    value: &WaitConditionRecord,
) -> Result<()> {
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO wait_conditions (
                wait_condition_id,agent_id,work_item_id,status,kind,waiting_for,
                created_at,updated_at,payload_json
             ) VALUES (?1,?2,?3,?4,'external',?5,?6,?7,?8)",
            params![
                sql_id,
                value.agent_id,
                value.work_item_id,
                enum_string(&value.status)?,
                value.waiting_for,
                timestamp(value.created_at),
                timestamp(value.updated_at),
                serde_json::to_string(value)?,
            ],
        )?;
        Ok(())
    })
}
fn payload(db: &crate::runtime_db::RuntimeDb, id: &str) -> Result<WaitConditionRecord> {
    db.transaction(|tx| {
        let json: String = tx.query_row(
            "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
            [id],
            |row| row.get(0),
        )?;
        decode_wait_condition_payload(&json)
    })
}
fn revisions(db: &crate::runtime_db::RuntimeDb) -> Result<i64> {
    db.transaction(|tx| {
        Ok(tx.query_row(
            "SELECT count(*) FROM conversation_source_revisions",
            [],
            |r| r.get(0),
        )?)
    })
}
fn exhausted(error: anyhow::Error) {
    assert!(format!("{error:#}")
        .to_lowercase()
        .contains("resourceexhausted"));
}

#[test]
fn cancellation_cost_and_sql_identity_survive_last_transaction_holder() -> Result<()> {
    for triggered in [false, true] {
        let (_home, db) = fixture()?;
        let mut old = record("payload-identity", 0);
        if triggered {
            old.mark_triggered("wake-identity", old.updated_at);
        }
        let new = record("replacement", 1);
        seed(&db, "sql-identity", &old)?;
        let body = read_costs("sql-identity", &old)?
            .into_iter()
            .chain(write_costs(&old, &new)?)
            .fold((0, 0), add);
        let exact = add(add(header(), header()), body);
        let budget = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            let writes =
                db.transaction(|tx| cancel_stale_unresolved_wait_owner_rows_tx(tx, &new))?;
            assert!(!writes.primary_applied);
            assert_eq!(writes.wait_ids, BTreeSet::from([old.id.clone()]));
            assert_eq!(writes.turn_ids, BTreeSet::from(["owner-turn".into()]));
        }
        assert_eq!(phase.finish()?, exact);
        assert_eq!(transaction.finish()?, add(header(), body));
        assert_eq!(payload(&db, "sql-identity")?, cancelled(&old, &new));
        db.transaction(|tx| {
            let row: (String, String, Option<String>, u64) = tx.query_row(
                "SELECT w.updated_at,w.cancelled_at,w.resolved_at,r.revision
                 FROM wait_conditions w JOIN conversation_source_revisions r
                 ON r.source_id=?1 AND r.source_kind=?2 WHERE w.wait_condition_id=?3",
                params![old.id, SOURCE_WAIT, "sql-identity"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            assert_eq!(
                row,
                (
                    "2026-10-17T13:42:31.123Z".into(),
                    "2026-10-17T13:42:31.123Z".into(),
                    None,
                    1
                )
            );
            Ok(())
        })?;
        let first = transaction.guard()?;
        let last = first.clone();
        assert!(std::sync::Arc::ptr_eq(&first, &last));
        drop((old, new, phase, transaction));
        assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
        drop(first);
        assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn every_cancellation_gate_rejects_and_rolls_back_in_original_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    let old = record("old", 0);
    let new = record("replacement", 1);
    seed(&db, &old.id, &old)?;
    let mut prefix = add(header(), header());
    for next in read_costs(&old.id, &old)?
        .into_iter()
        .chain(write_costs(&old, &new)?)
    {
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
                exhausted(
                    db.transaction(|tx| cancel_stale_unresolved_wait_owner_rows_tx(tx, &new))
                        .unwrap_err(),
                );
                exhausted(
                    db.transaction(|tx| cancel_stale_unresolved_wait_owner_rows_tx(tx, &new))
                        .unwrap_err(),
                );
            }
            assert_eq!(payload(&db, &old.id)?, old);
            assert_eq!(revisions(&db)?, 0);
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
    Ok(())
}

#[test]
fn malformed_later_candidate_is_seen_before_any_cancellation() -> Result<()> {
    let (_home, db) = fixture()?;
    // Exercise a legacy duplicate-owner pool without changing production indexes.
    db.transaction(|tx| {
        tx.execute_batch("DROP INDEX wait_conditions_unresolved_agent_owner")?;
        Ok(())
    })?;
    for id in ["a", "b"] {
        seed(&db, id, &record(id, 0))?;
    }
    let order: Vec<String> = db.transaction(|tx| {
        let mut s = tx.prepare(
            "SELECT wait_condition_id FROM wait_conditions
            WHERE agent_id=?1 AND work_item_id IS NULL AND wait_condition_id!='replacement'
            AND status IN ('active','triggered')",
        )?;
        let ids = s
            .query_map([&scope().agent_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    })?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE wait_conditions SET payload_json='{' WHERE wait_condition_id=?1",
            [&order[1]],
        )?;
        Ok(())
    })?;
    let budget = ledger()?;
    let exact = add(
        read_costs(&order[0], &record(&order[0], 0))?
            .into_iter()
            .fold(add(header(), header()), add),
        cost(1, 1),
    );
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        db.transaction(|tx| {
            let error = cancel_stale_unresolved_wait_owner_rows_tx(tx, &record("replacement", 1))
                .unwrap_err();
            assert!(format!("{error:#}").contains("decoding wait condition"));
            let changed: i64 = tx.query_row(
                "SELECT count(*) FROM wait_conditions WHERE status='cancelled'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(changed, 0, "the complete pool must be frozen before writes");
            Ok(())
        })?;
    }
    assert_eq!(phase.finish()?, exact);
    assert_eq!(revisions(&db)?, 0);
    drop(transaction);
    assert_eq!(budget.report()?.observed_bytes, header().0);
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn cancellation_preserves_owner_status_time_filters_without_detached_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        tx.execute_batch("DROP INDEX wait_conditions_unresolved_agent_owner")?;
        Ok(())
    })?;
    let incoming = record("replacement", 1);
    let old = record("old", 0);
    let mut equal = record("equal-triggered", 1);
    equal.mark_triggered("equal-wake", equal.updated_at);
    let newer = record("newer", 2);
    let mut foreign_agent = record("foreign-agent", 0);
    foreign_agent.agent_id = "other-agent".into();
    let mut foreign_work = record("foreign-work", 0);
    foreign_work.work_item_id = Some("other-work".into());
    let mut resolved = record("resolved", 0);
    resolved.status = WaitConditionStatus::Resolved;
    let records = [
        &old,
        &equal,
        &newer,
        &incoming,
        &foreign_agent,
        &foreign_work,
        &resolved,
    ];
    for value in records {
        seed(&db, &value.id, value)?;
    }
    let budget = ledger()?;
    assert_eq!(budget.report()?.observed_bytes, 0);
    let writes = db.transaction(|tx| cancel_stale_unresolved_wait_owner_rows_tx(tx, &incoming))?;
    assert_eq!(
        writes.wait_ids,
        BTreeSet::from([old.id.clone(), equal.id.clone()])
    );
    assert_eq!(payload(&db, &old.id)?, cancelled(&old, &incoming));
    assert_eq!(payload(&db, &equal.id)?, cancelled(&equal, &incoming));
    for unchanged in [&newer, &incoming, &foreign_agent, &foreign_work, &resolved] {
        assert_eq!(payload(&db, &unchanged.id)?, *unchanged);
    }
    assert_eq!(revisions(&db)?, 2);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_upsert_rolls_back_cancellation_when_primary_envelope_is_refused() -> Result<()> {
    let (_home, db) = fixture()?;
    let old = record("old", 0);
    let new = record("replacement", 1);
    seed(&db, &old.id, &old)?;
    // Cancellation IDs move into the primary write-set before its envelope.
    let merge = add(
        metadata(std::mem::size_of::<String>()),
        metadata(std::mem::size_of::<String>()),
    );
    let exact = read_costs(&old.id, &old)?
        .into_iter()
        .chain(write_costs(&old, &new)?)
        .fold(add(add(header(), header()), merge), add);
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        exhausted(
            db.transaction(|tx| upsert_wait_condition_with_writes_tx(tx, &new))
                .unwrap_err(),
        );
    }
    assert_eq!(payload(&db, &old.id)?, old);
    assert_eq!(revisions(&db)?, 0);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    assert!(transaction.finish().is_err());
    drop(transaction);
    assert_eq!(budget.report()?.observed_bytes, header().0);
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
