use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger, resident_input_evidence::InputEvidenceScope,
};

const BODY: &str = r#"{"external_trigger_id":"sql-trigger","target_agent_id":"scheduling-agent","scope":"agent","delivery_mode":"wake_hint","token":"sql-token","token_hash":"sql-hash","status":"active","created_at":"2026-10-01T00:00:00Z","revoked_at":null,"last_delivered_at":null,"delivery_count":1}"#;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "scheduling-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn body_cost() -> (u64, usize) {
    // Eleven keys/colons, ten commas, two braces, eight string values.
    (
        (BODY.len() as u64 * 4 + 42 * 128 + 512) * 8,
        (42 * 4 + 16) * 8,
    )
}
fn slots(count: usize) -> (u64, usize) {
    (
        (count * std::mem::size_of::<ExternalTriggerRecord>()) as u64 * 8,
        count * 8,
    )
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.external_triggers()
        .upsert(&serde_json::from_str(BODY)?)?;
    Ok((home, db))
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

enum Read {
    One(Option<ExternalTriggerRecord>),
    List(Vec<ExternalTriggerRecord>),
}
impl Read {
    fn first(&self) -> Option<&ExternalTriggerRecord> {
        match self {
            Self::One(record) => record.as_ref(),
            Self::List(records) => records.first(),
        }
    }
}
fn read(db: &crate::runtime_db::RuntimeDb, route: usize) -> Result<Read> {
    let repository = db.external_triggers();
    Ok(match route {
        0 => Read::One(repository.latest("sql-trigger")?),
        1 => Read::One(repository.active_default_for_agent("scheduling-agent")?),
        2 => Read::One(repository.active_by_token_hash("sql-hash")?),
        3 => Read::List(repository.latest_for_agent("scheduling-agent")?),
        4 => Read::List(repository.latest_for_agent_limit("scheduling-agent", 2)?),
        5 => Read::List(repository.latest_all()?),
        6 => {
            let connection = db.connection()?;
            Read::List(context_external_triggers_on(
                &connection,
                "scheduling-agent",
            )?)
        }
        _ => unreachable!(),
    })
}
fn unit(route: usize) -> (u64, usize) {
    add(body_cost(), if route < 3 { (0, 0) } else { slots(4) })
}
fn invalidate_date(db: &crate::runtime_db::RuntimeDb) -> Result<()> {
    let broken = BODY.replace("2026-10-01", "x026-10-01");
    assert_eq!(broken.len(), BODY.len());
    db.transaction(|tx| {
        tx.execute(
            "UPDATE external_triggers SET payload_json=?1 WHERE external_trigger_id='sql-trigger'",
            [&broken],
        )?;
        Ok(())
    })
}

#[test]
fn every_read_borrows_sql_and_uses_exact_caller_phase_through_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    for route in 0..7 {
        let budget = ledger()?;
        let exact = add(header(), unit(route));
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let record = {
            let _entry = phase.enter();
            read(&db, route)?
        };
        assert_eq!(record.first(), Some(&serde_json::from_str(BODY)?));
        assert_eq!(phase.finish()?, exact, "route {route}");
        // The existing plain-record API relies on its caller's explicit holder.
        let first = phase.guard()?;
        let last = first.clone();
        assert!(std::sync::Arc::ptr_eq(&first, &last));
        let holder = (record, last);
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(first);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        assert_eq!(holder.0.first(), Some(&serde_json::from_str(BODY)?));
        drop(holder);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn every_read_refuses_bytes_and_records_before_bad_body_and_latches_failure() -> Result<()> {
    let (_home, db) = fixture()?;
    invalidate_date(&db)?;
    for route in 0..7 {
        for bytes in [true, false] {
            let budget = ledger()?;
            let exact = add(header(), unit(route));
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes { exact.0 - 1 } else { 64 * 1024 * 1024 },
                if bytes { 65536 } else { exact.1 - 1 },
            )?;
            {
                let _entry = phase.enter();
                let error = match read(&db, route) {
                    Err(error) => error,
                    Ok(_) => panic!("route {route} unexpectedly admitted"),
                };
                assert!(format!("{error:#}")
                    .to_lowercase()
                    .contains("resourceexhausted"));
                assert!(read(&db, route).is_err());
            }
            let prefix = add(header(), if route < 3 { (0, 0) } else { slots(4) });
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn lists_admit_capacity_and_each_body_without_partial_results_or_fresh_allowance() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut first: ExternalTriggerRecord = serde_json::from_str(BODY)?;
    first.external_trigger_id = "aaa-trigger".into();
    first.status = ExternalTriggerStatus::Revoked;
    // The active-default unique index requires this historical row to be revoked.
    // "revoked" is one JSON byte longer than "active"; no extra structural slot.
    let historical_extra = (4 * 8, 0);
    assert_eq!(serde_json::to_string(&first)?.len(), BODY.len() + 1);
    db.external_triggers().upsert(&first)?;
    invalidate_date(&db)?;
    for route in 3..7 {
        let prefix = add(add(header(), unit(route)), historical_extra);
        let exact = add(prefix, body_cost());
        for bytes in [true, false] {
            let budget = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes { exact.0 - 1 } else { 64 * 1024 * 1024 },
                if bytes { 65536 } else { exact.1 - 1 },
            )?;
            {
                let _entry = phase.enter();
                let error = match read(&db, route) {
                    Err(error) => error,
                    Ok(_) => panic!("route {route} returned a partial list"),
                };
                assert!(format!("{error:#}")
                    .to_lowercase()
                    .contains("resourceexhausted"));
            }
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
        // Refuse the first capacity growth before constructing even the first record.
        for bytes in [true, false] {
            let budget = ledger()?;
            let exact = add(header(), slots(4));
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes { exact.0 - 1 } else { 64 * 1024 * 1024 },
                if bytes { 65536 } else { exact.1 - 1 },
            )?;
            {
                let _entry = phase.enter();
                assert!(read(&db, route).is_err());
            }
            assert_eq!(budget.report()?.observed_bytes, header().0);
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn legacy_reads_preserve_filter_order_limit_missing_and_decode_errors() -> Result<()> {
    let (_home, db) = fixture()?;
    let repository = db.external_triggers();
    let original: ExternalTriggerRecord = serde_json::from_str(BODY)?;
    let mut newer = original.clone();
    newer.external_trigger_id = "new-trigger".into();
    newer.token_hash = "new-hash".into();
    newer.created_at += chrono::Duration::seconds(1);
    newer.status = ExternalTriggerStatus::Revoked;
    repository.upsert(&newer)?;
    let mut foreign = newer.clone();
    foreign.external_trigger_id = "foreign-trigger".into();
    foreign.target_agent_id = "foreign-agent".into();
    foreign.token_hash = "foreign-hash".into();
    foreign.status = ExternalTriggerStatus::Revoked;
    repository.upsert(&foreign)?;
    assert_eq!(
        repository.latest_for_agent("scheduling-agent")?,
        vec![newer.clone(), original.clone()]
    );
    assert_eq!(
        repository.latest_for_agent_limit("scheduling-agent", 1)?,
        vec![newer.clone()]
    );
    assert!(repository
        .latest_for_agent_limit("scheduling-agent", 0)?
        .is_empty());
    assert_eq!(
        repository.active_default_for_agent("scheduling-agent")?,
        Some(original.clone())
    );
    assert!(repository
        .active_default_for_agent("foreign-agent")?
        .is_none());
    assert!(repository.active_by_token_hash("foreign-hash")?.is_none());
    assert_eq!(repository.latest_all()?, vec![original, newer, foreign]);
    assert!(repository.latest("missing")?.is_none());
    assert!(repository.latest_for_agent("missing")?.is_empty());
    assert!(repository.active_by_token_hash("missing")?.is_none());
    invalidate_date(&db)?;
    for route in 0..7 {
        let error = match read(&db, route) {
            Err(error) => error,
            Ok(_) => panic!("route {route} ignored bad payload"),
        };
        assert!(format!("{error:#}").contains("decoding external trigger payload"));
    }
    Ok(())
}

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

fn many_fixture(
    count: usize,
) -> Result<(
    tempfile::TempDir,
    crate::runtime_db::RuntimeDb,
    Vec<ExternalTriggerRecord>,
)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut expected = Vec::new();
    for index in 0..count {
        let mut record: ExternalTriggerRecord = serde_json::from_str(BODY)?;
        record.external_trigger_id = format!("trigger_{index:03}");
        record.token_hash = format!("hash_{index}");
        record.token = Some("{,:} \"quoted\" \\ 汉字\0🦀".into());
        record.status = ExternalTriggerStatus::Revoked;
        record.created_at += chrono::Duration::seconds((index % 3) as i64);
        db.external_triggers().upsert(&record)?;
        expected.push(record);
    }
    expected.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
    });
    Ok((home, db, expected))
}

fn read_many(
    db: &crate::runtime_db::RuntimeDb,
    route: usize,
) -> Result<Vec<ExternalTriggerRecord>> {
    let repository = db.external_triggers();
    match route {
        0 => repository.latest_for_agent("scheduling-agent"),
        1 => repository.latest_for_agent_limit("scheduling-agent", usize::MAX),
        2 => repository.latest_all(),
        3 => context_external_triggers_on(&db.connection()?, "scheduling-agent"),
        _ => unreachable!(),
    }
}

// Independent JSON structure and geometric-capacity oracle.
fn gates(records: &[ExternalTriggerRecord]) -> Result<Vec<(u64, usize)>> {
    let mut gates = Vec::new();
    let mut capacity = 0;
    for (index, record) in records.iter().enumerate() {
        if index == capacity {
            let growth = capacity.max(4);
            gates.push(slots(growth));
            capacity += growth;
        }
        let cells = shape(&serde_json::to_value(record)?);
        gates.push((
            (serde_json::to_vec(record)?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
            (cells * 4 + 16) * 8,
        ));
    }
    Ok(gates)
}

fn exhausted(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.to_string().contains("ResourceExhausted"))
}

#[test]
fn trigger_list_capacity_exact_cost_order_and_last_original_holder() -> Result<()> {
    for count in [0, 1, 4, 5, 8, 9, 17] {
        let (_home, db, expected) = many_fixture(count)?;
        for route in 0..4 {
            let budget = ledger()?;
            let exact = gates(&expected)?.into_iter().fold(header(), add);
            let phase =
                ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
            let original = phase.guard()?;
            let records = {
                let _entry = phase.enter();
                assert!(std::sync::Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &original
                ));
                read_many(&db, route)?
            };
            let expected = if route == 2 {
                expected.iter().rev().cloned().collect::<Vec<_>>()
            } else {
                expected.clone()
            };
            assert_eq!(records, expected);
            assert_eq!(phase.finish()?, exact);
            let capacity = if count == 0 {
                0
            } else {
                count.max(4).next_power_of_two()
            };
            assert_eq!(records.capacity(), capacity);
            let last = original.clone();
            let held = (records, last);
            drop((phase, original));
            assert_eq!(budget.report()?.observed_bytes, exact.0);
            assert_eq!(held.0, expected);
            drop(held);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn trigger_list_refuses_each_capacity_and_body_gate_and_latches() -> Result<()> {
    let (_home, db, expected) = many_fixture(9)?;
    let all = gates(&expected)?;
    let total = all.iter().copied().fold(header(), add);
    for route in 0..4 {
        let mut prefix = header();
        for gate in &all {
            let boundary = add(prefix, *gate);
            for bytes in [true, false] {
                let budget = ledger()?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &budget,
                    &scope(),
                    if bytes { boundary.0 - 1 } else { total.0 },
                    if bytes { total.1 } else { boundary.1 - 1 },
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    assert!(exhausted(&read_many(&db, route).unwrap_err()));
                    assert!(exhausted(&read_many(&db, route).unwrap_err()));
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                drop(holder);
                assert_eq!(budget.report()?.observed_bytes, 0);
            }
            prefix = boundary;
        }
    }
    Ok(())
}

#[test]
fn trigger_list_repeated_reads_spend_original_phase_allowance() -> Result<()> {
    let (_home, db, expected) = many_fixture(5)?;
    let cost = gates(&expected)?.into_iter().fold((0, 0), add);
    let exact = add(header(), add(cost, cost));
    for route in 0..4 {
        let budget = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        {
            let _entry = phase.enter();
            assert_eq!(read_many(&db, route)?.len(), expected.len());
            assert_eq!(read_many(&db, route)?.len(), expected.len());
            assert_eq!(phase.finish()?, exact);
            assert!(exhausted(&read_many(&db, route).unwrap_err()));
        }
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
