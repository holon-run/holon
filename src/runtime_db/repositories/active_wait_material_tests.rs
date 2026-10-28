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

#[path = "active_wait_material_tests/latest_tests.rs"]
mod latest_tests;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "active-wait-material".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

// Independent structural oracle, not the production byte scanner.
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

fn json_cost(value: &serde_json::Value) -> (u64, usize) {
    let payload = serde_json::to_string(value).unwrap();
    let slots = shape(value);
    (
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

fn body(record: &WaitConditionRecord) -> Vec<(u64, usize)> {
    let mut costs = [
        Some(record.id.as_str()),
        Some(record.agent_id.as_str()),
        record.work_item_id.as_deref(),
        record.source.as_deref(),
        record.subject_ref.as_deref(),
        Some(record.waiting_for.as_str()),
        record.turn_id.as_deref(),
        record.trigger_message_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|text| ((text.len() + std::mem::size_of::<String>()) as u64 * 8, 8))
    .collect::<Vec<_>>();
    costs.push(json_cost(
        &serde_json::to_value(&record.wake_sources).unwrap(),
    ));
    if let Some(value) = &record.continuation {
        costs.push(json_cost(value));
    }
    costs
}

fn gates(records: &[WaitConditionRecord]) -> Vec<(u64, usize)> {
    let mut costs = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let additional = match index {
            0 | 4 => 4,
            8 => 8,
            _ => 0,
        };
        if additional > 0 {
            costs.push((
                (additional * std::mem::size_of::<WaitConditionRecord>()) as u64 * 8,
                additional * 8,
            ));
        }
        costs.extend(body(record));
    }
    costs
}

fn fixture(count: usize) -> Result<(tempfile::TempDir, RuntimeDb, Vec<WaitConditionRecord>)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut records = Vec::new();
    for index in 0..count {
        let rich = index % 2 == 0;
        let created_at = "2026-10-01T00:00:00Z".parse::<DateTime<Utc>>()?
            + chrono::Duration::seconds((index % 3) as i64);
        let record = WaitConditionRecord {
            id: format!("wait_{index:03}"),
            agent_id: "web".into(),
            work_item_id: (index != 0).then(|| format!("work_{index:03}")),
            status: WaitConditionStatus::Active,
            kind: [
                WaitConditionKind::Task,
                WaitConditionKind::External,
                WaitConditionKind::Operator,
                WaitConditionKind::Timer,
                WaitConditionKind::System,
            ][index % 5]
                .clone(),
            source: rich.then(|| String::from("")),
            subject_ref: rich.then(|| "subject 汉字\0".into()),
            waiting_for: "{,:} \"quoted\" \\ 🦀".into(),
            wake_sources: if rich {
                vec![
                    WakeSource::TaskResult {
                        task_id: "task".into(),
                    },
                    WakeSource::ExternalIngress {
                        external_trigger_id: Some("trigger".into()),
                    },
                    WakeSource::Timer {
                        wake_at: created_at,
                    },
                    WakeSource::OperatorInput,
                    WakeSource::SystemTick,
                ]
            } else {
                Vec::new()
            },
            continuation: rich.then(|| {
                serde_json::json!({
                    "nested": [{"empty": [], "text": "\\\"[]{}汉字"}], "null": null, "bool": true
                })
            }),
            created_at,
            updated_at: created_at + chrono::Duration::seconds((index % 2) as i64),
            expires_at: rich.then_some(created_at),
            resolved_at: rich.then_some(created_at),
            cancelled_at: rich.then_some(created_at),
            turn_id: rich.then(|| "turn".into()),
            trigger_message_id: rich.then(|| format!("message_{index:03}")),
            triggered_at: rich.then_some(created_at),
        };
        db.wait_conditions().upsert(&record)?;
        records.push(record);
    }
    records.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok((home, db, records))
}

fn read(db: &RuntimeDb, route: usize) -> Result<Vec<WaitConditionRecord>> {
    match route {
        0 => db.wait_conditions().active_for_agent("web"),
        1 => db.wait_conditions().active_all(),
        2 => context_active_waits_on(&db.connection()?, "web"),
        _ => unreachable!(),
    }
}

#[test]
fn active_wait_sql_exact_cost_values_and_original_holder_cover_geometric_lists() -> Result<()> {
    for count in [0, 1, 4, 5, 9] {
        let (_home, db, expected) = fixture(count)?;
        for route in 0..3 {
            let exact = gates(&expected).into_iter().fold(header(), add);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
            let original = phase.guard()?;
            let records = {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &original
                ));
                read(&db, route)?
            };
            assert_eq!(records, expected);
            assert_eq!(phase.finish()?, exact);
            let last = original.clone();
            let held = (records, last);
            drop((phase, original));
            assert_eq!(ledger.report()?.observed_bytes, exact.0);
            assert_eq!(held.0, expected);
            drop(held);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn active_wait_sql_refuses_every_text_json_and_growth_gate_without_phase_reset() -> Result<()> {
    let (_home, db, expected) = fixture(9)?;
    for route in 0..3 {
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
                {
                    let _entry = phase.enter();
                    for _ in 0..2 {
                        let error = read(&db, route).unwrap_err();
                        assert!(error.downcast_ref::<std::io::Error>().is_some());
                        assert_eq!(
                            crate::runtime_error::describe_runtime_error(&error).code,
                            "io_error"
                        );
                        assert!(format!("{error:#}")
                            .to_lowercase()
                            .contains("resourceexhausted"));
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
    }
    Ok(())
}

#[test]
fn active_wait_sql_scope_status_order_and_unqualified_reads_match_ordinary_decoder() -> Result<()> {
    let (_home, db, expected) = fixture(9)?;
    let mut other = expected[0].clone();
    other.id = "other".into();
    other.agent_id = "other-agent".into();
    db.wait_conditions().upsert(&other)?;
    for (index, status) in [
        WaitConditionStatus::Triggered,
        WaitConditionStatus::Resolved,
        WaitConditionStatus::Cancelled,
        WaitConditionStatus::Expired,
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = other.clone();
        row.id = format!("inactive_{index}");
        row.work_item_id = Some(row.id.clone());
        row.status = status;
        db.wait_conditions().upsert(&row)?;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(read(&db, 0)?, expected);
    assert_eq!(read(&db, 2)?, expected);
    assert!(db.wait_conditions().active_for_agent("")?.is_empty());
    let all = read(&db, 1)?;
    assert_eq!(all.len(), expected.len() + 1);
    assert_eq!(
        all.iter()
            .filter(|r| r.agent_id == "web")
            .cloned()
            .collect::<Vec<_>>(),
        expected
    );
    assert!(all.contains(&other));
    // The untouched ordinary decoder is an independent value oracle.
    let ordinary = db.wait_conditions().latest_for_agent("web")?;
    for row in &expected {
        assert!(ordinary.contains(row));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn active_wait_sql_preserves_column_utf8_enum_json_and_timestamp_errors_after_partial_decode(
) -> Result<()> {
    let (_home, db, expected) = fixture(2)?;
    let columns = [
        "wait_condition_id",
        "agent_id",
        "work_item_id",
        "status",
        "kind",
        "source",
        "subject_ref",
        "waiting_for",
        "created_at",
        "updated_at",
        "expires_at",
        "resolved_at",
        "cancelled_at",
        "last_turn_id",
        "trigger_message_id",
        "triggered_at",
        "wake_sources_json",
        "continuation_json",
    ];
    for (column, replacement) in [
        (0, "NULL"),
        (1, "x'ff'"),
        (2, "x'ff'"),
        (3, "'unknown'"),
        (4, "'unknown'"),
        (5, "CAST(x'ff' AS TEXT)"),
        (6, "x'ff'"),
        (7, "NULL"),
        (8, "'bad'"),
        (9, "'bad'"),
        (10, "'bad'"),
        (11, "'bad'"),
        (12, "'bad'"),
        (13, "x'ff'"),
        (14, "x'ff'"),
        (15, "'bad'"),
        (16, "'[{]'"),
        (16, "'[{\"kind\":\"unknown\"}]'"),
        (17, "'{'"),
    ] {
        // The first row succeeds. Only the last row is damaged, avoiding schema
        // CHECK constraints and testing status errors that Active WHERE excludes.
        let fields = columns.iter().enumerate().map(|(index, name)| {
            if index == column {
                format!("CASE WHEN wait_condition_id='{}' THEN {replacement} ELSE {name} END AS {name}", expected[1].id)
            } else {
                (*name).to_owned()
            }
        }).collect::<Vec<_>>().join(", ");
        let sql = format!("SELECT {fields} FROM wait_conditions ORDER BY updated_at DESC, created_at DESC, wait_condition_id ASC");
        let connection = db.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let ordinary = statement
            .query_map([], |row| {
                decode_wait_condition_row(row).map_err(wait_condition_decode_error)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap_err();
        let expected_error = format!("reading wait conditions: {ordinary}");
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = active_wait_records(&mut statement, []).unwrap_err();
            assert_eq!(format!("{error:#}"), expected_error, "column {column}");
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
        }
        let cost = phase.finish()?;
        assert!(cost.0 >= gates(&expected[..1]).into_iter().fold(header(), add).0);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
