use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::RuntimeDb,
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "continuation-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

// Independent structural oracle, not the production JSON byte scanner.
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

fn body(record: &WorkItemContinuationFrame) -> Result<(u64, usize)> {
    let cells = shape(&serde_json::to_value(record)?);
    Ok((
        (serde_json::to_vec(record)?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
        (cells * 4 + 16) * 8,
    ))
}

fn slots(count: usize) -> (u64, usize) {
    (
        (count * std::mem::size_of::<WorkItemContinuationFrame>()) as u64 * 8,
        count * 8,
    )
}

fn gates(records: &[WorkItemContinuationFrame]) -> Result<Vec<(u64, usize)>> {
    let mut gates = Vec::new();
    for (index, record) in records.iter().enumerate() {
        match index {
            0 | 4 => gates.push(slots(4)),
            8 => gates.push(slots(8)),
            _ => {}
        }
        gates.push(body(record)?);
    }
    Ok(gates)
}

fn fixture(count: usize) -> Result<(tempfile::TempDir, RuntimeDb, Vec<WorkItemContinuationFrame>)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut expected = Vec::new();
    for index in 0..count {
        let mut record = WorkItemContinuationFrame::new_on_completed(
            "web",
            format!("suspended_{index}_汉字\0🦀"),
            format!("active_{index}"),
            Some("{,:} \"quoted\" \\ 汉字\0🦀".into()),
        );
        record.id = format!("continuation_{index:03}");
        record.created_at = "2026-10-01T00:00:00Z".parse()?;
        record.updated_at = record.created_at;
        if index == 0 {
            record.updated_at += chrono::Duration::seconds(2);
        } else if index == 1 {
            record.created_at += chrono::Duration::seconds(1);
        }
        record.resolution_reason = Some(String::new());
        db.work_item_continuations().upsert(&record)?;
        expected.push(record);
    }
    // These must be excluded by SQL, not by the decoded payload.
    for (index, (agent, state)) in [
        ("other", WorkItemContinuationState::Active),
        ("web", WorkItemContinuationState::Resumed),
        ("web", WorkItemContinuationState::Cancelled),
    ]
    .into_iter()
    .enumerate()
    {
        let mut record = WorkItemContinuationFrame::new_on_completed(
            agent,
            format!("excluded_{index}"),
            "excluded_active",
            None,
        );
        record.state = state;
        db.work_item_continuations().upsert(&record)?;
    }
    expected.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok((home, db, expected))
}

fn read(db: &RuntimeDb, repository: bool) -> Result<Vec<WorkItemContinuationFrame>> {
    if repository {
        db.work_item_continuations().active_for_agent("web")
    } else {
        context_active_continuations_on(&db.connection()?, "web")
    }
}

fn exhausted(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.to_string().contains("ResourceExhausted"))
}

#[test]
fn continuation_sql_exact_cost_order_filter_and_original_holder() -> Result<()> {
    for count in [0, 1, 4, 5, 9] {
        for repository in [false, true] {
            let (_home, db, expected) = fixture(count)?;
            assert_eq!(read(&db, repository)?, expected);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let exact = gates(&expected)?.into_iter().fold(header(), add);
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
            let first = phase.guard()?;
            let records = {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    ResourceReadMaterial::active_guard()?.as_ref().unwrap(),
                    &first
                ));
                read(&db, repository)?
            };
            assert_eq!(records, expected);
            assert_eq!(phase.finish()?, exact);
            let last = first.clone();
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
fn continuation_sql_refuses_each_growth_and_json_gate_and_latches() -> Result<()> {
    for repository in [false, true] {
        let (_home, db, expected) = fixture(9)?;
        let mut prefix = header();
        let all = gates(&expected)?;
        let total = all.iter().copied().fold(header(), add);
        for gate in all {
            let boundary = add(prefix, gate);
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    if bytes { boundary.0 - 1 } else { total.0 },
                    if bytes { total.1 } else { boundary.1 - 1 },
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    assert!(exhausted(&read(&db, repository).unwrap_err()));
                    assert!(exhausted(&read(&db, repository).unwrap_err()));
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
fn continuation_sql_repeated_reads_spend_original_phase_allowance() -> Result<()> {
    for repository in [false, true] {
        let (_home, db, expected) = fixture(5)?;
        let cost = gates(&expected)?.into_iter().fold((0, 0), add);
        let exact = add(header(), add(cost, cost));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            assert_eq!(read(&db, repository)?, expected);
            assert_eq!(read(&db, repository)?, expected);
            assert_eq!(phase.finish()?, exact);
            assert!(exhausted(&read(&db, repository).unwrap_err()));
        }
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

// The former owning SQL/JSON path, retained only as an error-compatibility oracle.
fn old_read(db: &RuntimeDb) -> Result<Vec<WorkItemContinuationFrame>> {
    let connection = db.connection()?;
    let mut statement = connection.prepare(
        "SELECT payload_json FROM work_item_continuations
         WHERE agent_id = ?1 AND state = 'active'
         ORDER BY updated_at DESC, created_at DESC, continuation_id ASC",
    )?;
    let rows = statement.query_map(["web"], |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_work_item_continuation_payload(&row?))
        .collect()
}

#[test]
fn continuation_sql_partial_read_preserves_sql_utf8_and_json_errors() -> Result<()> {
    for expression in ["X'80'", "CAST(X'80' AS TEXT)", "'{'", "'[]'"] {
        for repository in [false, true] {
            let (_home, db, expected) = fixture(4)?;
            db.connection()?.execute(
                &format!(
                    "UPDATE work_item_continuations SET payload_json={expression}
                     WHERE continuation_id=?1"
                ),
                [&expected[2].id],
            )?;
            let old_error = old_read(&db).unwrap_err();
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::new(&ledger, &scope())?;
            let holder = phase.guard()?;
            let error = {
                let _entry = phase.enter();
                read(&db, repository).unwrap_err()
            };
            assert_eq!(format!("{error:#}"), format!("{old_error:#}"));
            if let Some(old_sql) = old_error.downcast_ref::<rusqlite::Error>() {
                assert_eq!(
                    format!("{:?}", error.downcast_ref::<rusqlite::Error>().unwrap()),
                    format!("{old_sql:?}")
                );
            } else {
                assert!(error.downcast_ref::<serde_json::Error>().is_some());
            }
            let charged = phase.finish()?;
            let prefix = gates(&expected[..2])?.into_iter().fold(header(), add);
            assert!(charged.0 >= prefix.0 && charged.1 >= prefix.1);
            if expression.contains("X'80'") {
                assert_eq!(charged, prefix);
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, charged.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    let (_home, db, _) = fixture(0)?;
    db.connection()?
        .execute_batch("DROP TABLE work_item_continuations")?;
    for repository in [false, true] {
        assert_eq!(
            format!("{:#}", read(&db, repository).unwrap_err()),
            format!("{:#}", old_read(&db).unwrap_err())
        );
    }
    Ok(())
}
