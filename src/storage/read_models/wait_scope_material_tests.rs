use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::repositories::decode_work_item_payload,
    types::WakeSource,
};
use std::{collections::BTreeSet, sync::Arc};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "wait-scope-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn slots<T>(count: usize) -> (u64, usize) {
    ((count * std::mem::size_of::<T>()) as u64 * 8, count * 8)
}

// Structural oracle independent of the production JSON byte scanner.
fn shape(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            2 + fields.len() * 2
                + fields.len().saturating_sub(1)
                + fields.values().map(shape).sum::<usize>()
        }
        serde_json::Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
        }
        serde_json::Value::String(_) => 1,
        _ => 0,
    }
}

fn lookup_gates(items: &[WorkItemRecord]) -> Result<Vec<(u64, usize)>> {
    let mut gates = Vec::new();
    let mut capacity = 0;
    for (index, item) in items.iter().enumerate() {
        if index == capacity {
            let additional = capacity.max(4);
            gates.push(slots::<WorkItemRecord>(additional));
            capacity += additional;
        }
        let payload = serde_json::to_string(item)?;
        let shape = shape(&serde_json::to_value(item)?);
        gates.push((
            (payload.len() as u64 * 4 + shape as u64 * 128 + 512) * 8,
            (shape * 4 + 16) * 8,
        ));
    }
    Ok(gates)
}

fn gates(waits: &[WaitConditionRecord], items: &[WorkItemRecord]) -> Result<Vec<(u64, usize)>> {
    let mut gates = Vec::new();
    let ids = waits
        .iter()
        .filter(|wait| wait.work_item_id.is_some())
        .count();
    if ids > 0 {
        gates.push(slots::<&str>(ids));
    }
    gates.extend(lookup_gates(items)?);
    if !items.is_empty() {
        gates.push(slots::<(String, usize, bool)>(items.len()));
    }
    Ok(gates)
}

fn wait(id: &str, owner: Option<String>) -> WaitConditionRecord {
    let now = "2026-10-01T00:00:00Z".parse().unwrap();
    WaitConditionRecord {
        id: id.into(),
        agent_id: "web".into(),
        work_item_id: owner,
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Operator,
        source: None,
        subject_ref: None,
        waiting_for: "operator 汉字 \"🦀\"".into(),
        wake_sources: vec![WakeSource::OperatorInput],
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}

fn fixture(count: usize) -> Result<(tempfile::TempDir, RuntimeDb, Vec<WaitConditionRecord>)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut waits = Vec::new();
    for index in 0..count {
        let mut item = WorkItemRecord::new(
            "web",
            "scope {} 汉字 \"🦀\"",
            if index % 2 == 0 {
                WorkItemState::Open
            } else {
                WorkItemState::Completed
            },
        );
        item.id = format!("w{index:03}");
        item.created_at = "2026-10-01T00:00:00Z".parse()?;
        item.updated_at = item.created_at;
        assert!(db.work_items().insert_new(&item)?);
        waits.push(wait(&format!("wait{index}"), Some(item.id)));
    }
    if count > 0 {
        waits.reverse();
        waits.push(wait("duplicate", Some("w000".into())));
        waits.push(wait("missing", Some("absent".into())));
        waits.push(wait("agent", None));
    }
    waits.reserve(11);
    Ok((home, db, waits))
}

// Frozen old reader: preserves SQLite's IN order and per-chunk duplicate behavior.
fn legacy_lookup(db: &RuntimeDb, ids: &[String]) -> Result<Vec<WorkItemRecord>> {
    let mut records = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(128) {
        let placeholders = (0..chunk.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql =
            format!("SELECT payload_json FROM work_items WHERE work_item_id IN ({placeholders})");
        let connection = db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rows = statement.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
        records.extend(
            rows.map(|row| decode_work_item_payload(&row?))
                .collect::<Result<Vec<_>>>()?,
        );
    }
    Ok(records)
}

fn ids(waits: &[WaitConditionRecord]) -> Vec<String> {
    waits
        .iter()
        .filter_map(|wait| wait.work_item_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[test]
fn wait_scope_exact_cost_retains_original_vec_order_and_holder_across_chunks() -> Result<()> {
    for count in [0, 1, 4, 5, 9, 129] {
        let (_home, db, waits) = fixture(count)?;
        let items = legacy_lookup(&db, &ids(&waits))?;
        let expected = filter_context_active_waits(waits.clone(), items.clone());
        let text = waits
            .iter()
            .map(|wait| (wait.id.clone(), wait.waiting_for.as_ptr()))
            .collect::<HashMap<_, _>>();
        let pointer = waits.as_ptr();
        let capacity = waits.capacity();
        let exact = gates(&waits, &items)?.into_iter().fold(header(), add);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let original = phase.guard()?;
        let actual = {
            let _entry = phase.enter();
            RuntimeReadModels::borrowed_for_agent(&db, "web")?
                .filter_active_wait_conditions_for_live_scope(waits)?
        };
        assert_eq!(actual, expected);
        assert_eq!(actual.as_ptr(), pointer);
        assert_eq!(actual.capacity(), capacity);
        for wait in &actual {
            assert_eq!(wait.waiting_for.as_ptr(), text[&wait.id]);
        }
        assert_eq!(phase.finish()?, exact);
        let last = original.clone();
        drop((phase, original));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(actual);
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn wait_scope_refuses_every_id_lookup_and_index_gate_without_reset_or_partial_result() -> Result<()>
{
    let (_home, db, waits) = fixture(5)?;
    let items = legacy_lookup(&db, &ids(&waits))?;
    let view = RuntimeReadModels::borrowed_for_agent(&db, "web")?;
    let mut prefix = header();
    for gate in gates(&waits, &items)? {
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
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &original
                ));
                for _ in 0..2 {
                    let error = view
                        .filter_active_wait_conditions_for_live_scope(waits.clone())
                        .unwrap_err();
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    crate::diagnostics::resource_io::assert_retryable_resource_error(&error);
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                }
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(phase.finish().is_err());
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
fn wait_scope_lookup_keeps_public_batch_order_and_duplicate_contract() -> Result<()> {
    let (_home, db, _) = fixture(129)?;
    let mut ids = (0..129)
        .rev()
        .map(|i| format!("w{i:03}"))
        .collect::<Vec<_>>();
    ids.extend(["w128".into(), "w000".into(), "absent".into()]);
    let expected = legacy_lookup(&db, &ids)?;
    // w128 appears in both chunks; w000 repeats only within the second chunk.
    assert_eq!(expected.iter().filter(|item| item.id == "w128").count(), 2);
    assert_eq!(expected.iter().filter(|item| item.id == "w000").count(), 1);
    let borrowed = ids.iter().map(String::as_str).collect::<Vec<_>>();
    for owned in [false, true] {
        let exact = lookup_gates(&expected)?.into_iter().fold(header(), add);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            let actual = if owned {
                db.work_items().latest_many(&ids)?
            } else {
                db.work_items().latest_many_borrowed(&borrowed)?
            };
            assert_eq!(actual, expected);
        }
        assert_eq!(phase.finish()?, exact);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    db.connection()?.execute_batch("DROP TABLE work_items")?;
    assert!(db.work_items().latest_many(&[])?.is_empty());
    assert!(db.work_items().latest_many_borrowed(&[])?.is_empty());
    assert_eq!(
        db.work_items().latest_many(&ids).unwrap_err().to_string(),
        legacy_lookup(&db, &ids).unwrap_err().to_string()
    );
    Ok(())
}

#[test]
fn wait_scope_uses_payload_state_and_last_payload_id_not_sql_index() -> Result<()> {
    let (_home, db, waits) = fixture(2)?;
    for duplicate in [false, true] {
        db.connection()?.execute(
            "UPDATE work_items SET state='completed' WHERE work_item_id='w000'",
            [],
        )?;
        if duplicate {
            db.connection()?.execute(
                "UPDATE work_items SET payload_json=json_set(payload_json, '$.id', 'w000')
                 WHERE work_item_id='w001'",
                [],
            )?;
        }
        let items = legacy_lookup(&db, &ids(&waits))?;
        let expected = filter_context_active_waits(waits.clone(), items);
        if duplicate {
            assert_eq!(
                expected
                    .iter()
                    .map(|wait| wait.id.as_str())
                    .collect::<Vec<_>>(),
                ["agent"]
            );
        } else {
            assert!(expected.iter().any(|wait| wait.id == "wait0"));
        }
        assert_eq!(
            RuntimeReadModels::borrowed_for_agent(&db, "web")?
                .filter_active_wait_conditions_for_live_scope(waits.clone())?,
            expected
        );
    }
    Ok(())
}

#[test]
fn wait_scope_lookup_preserves_sql_utf8_json_errors_and_releases_failed_material() -> Result<()> {
    for expression in ["NULL", "42", "x'ff'", "CAST(x'ff' AS TEXT)", "'{}'"] {
        let (_home, db, _) = fixture(1)?;
        db.connection()?.execute_batch(&format!(
            "DROP TABLE work_items;
             CREATE VIEW work_items AS SELECT 'w000' AS work_item_id, {expression} AS payload_json;"
        ))?;
        let ids = vec!["w000".into()];
        let old = legacy_lookup(&db, &ids).unwrap_err();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            let actual = db.work_items().latest_many(&ids).unwrap_err();
            assert_eq!(format!("{actual:#}"), format!("{old:#}"));
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
        }
        let cost = phase.finish()?;
        if expression == "'{}'" {
            assert!(
                cost.0 > header().0,
                "JSON decoding retains its owned material"
            );
        } else {
            assert_eq!(
                cost,
                header(),
                "SQL/UTF-8 rejection precedes any record allocation"
            );
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let (_home, db, waits) = fixture(129)?;
    db.connection()?.execute(
        "UPDATE work_items SET payload_json=replace(payload_json, '2026-10-01', 'x026-10-01')
         WHERE work_item_id='w128'",
        [],
    )?;
    let old = legacy_lookup(&db, &ids(&waits)).unwrap_err();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase =
        ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64 * 1024 * 1024, 1_000_000)?;
    {
        let _entry = phase.enter();
        let actual = RuntimeReadModels::borrowed_for_agent(&db, "web")?
            .filter_active_wait_conditions_for_live_scope(waits)
            .unwrap_err();
        assert_eq!(format!("{actual:#}"), format!("{old:#}"));
        assert!(actual.downcast_ref::<serde_json::Error>().is_some());
    }
    assert!(phase.finish()?.0 > header().0);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
