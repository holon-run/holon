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

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "work-queue-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn slot(count: usize) -> (u64, usize) {
    (
        (count * std::mem::size_of::<WorkItemRecord>()) as u64 * 8,
        count * 8,
    )
}

// Independent structural oracle: JSON values, not the production byte scanner.
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

fn body(record: &WorkItemRecord) -> Result<(u64, usize)> {
    let payload = serde_json::to_string(record)?;
    let slots = shape(&serde_json::to_value(record)?);
    Ok((
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    ))
}

fn fixture(
    count: usize,
    terminal: bool,
) -> Result<(
    tempfile::TempDir,
    crate::runtime_db::RuntimeDb,
    Vec<WorkItemRecord>,
)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut expected = Vec::new();
    for index in 0..count {
        let mut record = WorkItemRecord::new(
            "web",
            "{,:} \"quoted\" \\ 汉字\0🦀",
            if terminal {
                WorkItemState::Completed
            } else {
                WorkItemState::Open
            },
        );
        record.id = format!("work_{index:03}");
        record.created_at = "2026-10-01T00:00:00Z".parse()?;
        record.updated_at = record.created_at;
        assert!(db.work_items().insert_new(&record)?);
        expected.push(record);
    }
    Ok((home, db, expected))
}

fn read(db: &crate::runtime_db::RuntimeDb, route: usize) -> Result<Vec<WorkItemRecord>> {
    match route {
        0 => db.work_items().open_for_agent("web"),
        1 => db.work_items().latest_non_open_for_agent("web", 20),
        2 => context_open_work_items_on(&db.connection()?, "web"),
        3 => context_non_open_work_items_on(&db.connection()?, "web", 20),
        _ => unreachable!(),
    }
}

fn gates(expected: &[WorkItemRecord]) -> Result<Vec<(u64, usize)>> {
    let mut gates = Vec::new();
    for (index, record) in expected.iter().enumerate() {
        // The first allocation is four slots; growth is at rows five and nine.
        match index {
            0 | 4 => gates.push(slot(4)),
            8 => gates.push(slot(8)),
            _ => {}
        }
        gates.push(body(record)?);
    }
    Ok(gates)
}

fn exhausted(error: &anyhow::Error) -> bool {
    format!("{error:#}")
        .to_lowercase()
        .contains("resourceexhausted")
}

#[test]
fn queue_sql_exact_cost_and_original_holder_cover_empty_and_geometric_lists() -> Result<()> {
    for count in [0, 1, 4, 5, 9] {
        for route in 0..4 {
            let (_home, db, expected) = fixture(count, route % 2 == 1)?;
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let exact = gates(&expected)?.into_iter().fold(header(), add);
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
            let records = {
                let _entry = phase.enter();
                read(&db, route)?
            };
            assert_eq!(records, expected);
            assert_eq!(phase.finish()?, exact);
            let first = phase.guard()?;
            let last = first.clone();
            assert!(Arc::ptr_eq(&first, &last));
            let held = (records, last);
            drop((phase, first));
            assert_eq!(ledger.report()?.observed_bytes, exact.0);
            assert_eq!(held.0, expected);
            drop(held);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn queue_sql_refuses_every_growth_and_decode_gate_and_latches_same_phase() -> Result<()> {
    for route in 0..4 {
        let (_home, db, expected) = fixture(9, route % 2 == 1)?;
        let mut prefix = header();
        for gate in gates(&expected)? {
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
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    assert!(exhausted(&read(&db, route).unwrap_err()));
                    assert!(exhausted(&read(&db, route).unwrap_err()));
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
            prefix = boundary;
        }
    }
    Ok(())
}

#[test]
fn queue_sql_repeated_reads_share_used_allowance_without_phase_reset() -> Result<()> {
    for route in 0..4 {
        let (_home, db, expected) = fixture(5, route % 2 == 1)?;
        let cost = gates(&expected)?.into_iter().fold((0, 0), add);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let exact = add(header(), add(cost, cost));
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            assert_eq!(read(&db, route)?, expected);
            assert_eq!(read(&db, route)?, expected);
            assert_eq!(phase.finish()?, exact);
            assert!(exhausted(&read(&db, route).unwrap_err()));
        }
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn queue_sql_bad_later_json_is_all_or_nothing_and_keeps_original_error_holder() -> Result<()> {
    for route in 0..4 {
        let (_home, db, expected) = fixture(2, route % 2 == 1)?;
        let broken = serde_json::to_string(&expected[1])?.replace("2026-10-01", "x026-10-01");
        db.connection()?.execute(
            "UPDATE work_items SET payload_json=?1 WHERE work_item_id='work_001'",
            [&broken],
        )?;
        let exact = gates(&expected)?.into_iter().fold(header(), add);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            let error = read(&db, route).unwrap_err();
            assert_eq!(
                error.to_string(),
                "decoding work item payload from runtime db"
            );
            assert!(error.downcast_ref::<serde_json::Error>().is_some());
        }
        assert_eq!(phase.finish()?, exact);
        let holder = phase.guard()?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn queue_sql_preserves_column_and_utf8_errors_before_json_decode() -> Result<()> {
    for route in 0..4 {
        let (_home, db, _) = fixture(1, route % 2 == 1)?;
        for expression in ["NULL", "42", "x'ff'", "CAST(x'ff' AS TEXT)"] {
            // A custom statement isolates column conversion from the schema's NOT NULL constraint.
            let connection = db.connection()?;
            let mut statement =
                connection.prepare(&format!("SELECT {expression} AS payload_json"))?;
            let old = statement
                .query_row([], |row| row.get::<_, String>(0))
                .unwrap_err();
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::new(&ledger, &scope())?;
            let error = {
                let _entry = phase.enter();
                work_queue_records(&mut statement, []).unwrap_err()
            };
            let actual = error.downcast_ref::<rusqlite::Error>().unwrap();
            assert_eq!(actual.to_string(), old.to_string());
            assert_eq!(std::mem::discriminant(actual), std::mem::discriminant(&old));
            // Conversion fails before the first successful row needs Vec capacity.
            assert_eq!(phase.finish()?, header());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn queue_sql_keeps_scope_terminal_order_and_zero_limit_short_circuit() -> Result<()> {
    let (_home, db, mut expected) = fixture(3, true)?;
    expected[0].updated_at += chrono::Duration::seconds(1);
    expected[0].revision += 1;
    assert!(db.work_items().update_expected(&expected[0], 1)?);
    let mut other = WorkItemRecord::new("other", "outside scope", WorkItemState::Open);
    other.id = "outside".into();
    assert!(db.work_items().insert_new(&other)?);
    assert!(db.work_items().open_for_agent("web")?.is_empty());
    assert_eq!(
        db.work_items().latest_non_open_for_agent("web", 2)?,
        expected[..2]
    );
    assert_eq!(db.work_items().open_for_agent("other")?, [other]);
    let connection = db.connection()?;
    connection.execute_batch("DROP TABLE work_items")?;
    assert!(context_non_open_work_items_on(&connection, "web", 0)?.is_empty());
    assert!(db
        .work_items()
        .latest_non_open_for_agent("web", 0)?
        .is_empty());
    Ok(())
}
