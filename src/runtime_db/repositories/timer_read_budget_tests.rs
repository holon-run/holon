use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger, resident_input_evidence::InputEvidenceScope,
};

const BODY: &str = r#"{"id":"sql-timer-1","agent_id":"scheduling-agent","created_at":"2026-10-01T00:00:00Z","duration_ms":1000,"interval_ms":1000,"repeat":true,"status":"completed","summary":"{,:} \"x\" \\ 路","next_fire_at":"2026-10-01T00:00:01Z","last_fired_at":"2026-10-01T00:00:00Z","fire_count":5}"#;

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
    // Eleven keys/colons, ten commas, two braces, seven string values.
    // Escaped quotes, punctuation and UTF-8 within summary add no shape slots.
    (
        (BODY.len() as u64 * 4 + 41 * 128 + 512) * 8,
        (41 * 4 + 16) * 8,
    )
}
fn cell() -> (u64, usize) {
    (std::mem::size_of::<TimerRecord>() as u64 * 8, 8)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn unit(route: usize) -> (u64, usize) {
    add(body_cost(), if route == 0 { (0, 0) } else { cell() })
}
fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let record: TimerRecord = serde_json::from_str(BODY)?;
    assert_eq!(serde_json::to_string(&record)?, BODY);
    db.timers().upsert(&record)?;
    Ok((home, db))
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
enum Read {
    One(Option<TimerRecord>),
    List(Vec<TimerRecord>),
}
impl Read {
    fn first(&self) -> Option<&TimerRecord> {
        match self {
            Self::One(record) => record.as_ref(),
            Self::List(records) => records.first(),
        }
    }
}
fn read(db: &crate::runtime_db::RuntimeDb, route: usize) -> Result<Read> {
    let repository = db.timers();
    Ok(match route {
        0 => Read::One(repository.latest("sql-timer-1")?),
        1 => Read::List(repository.latest_all()?),
        2 => Read::List(repository.recent(usize::MAX)?),
        3 => Read::List(repository.recent_for_agent("scheduling-agent", usize::MAX)?),
        _ => unreachable!(),
    })
}
fn invalidate_date(db: &crate::runtime_db::RuntimeDb) -> Result<()> {
    let broken = BODY.replace("2026-10-01", "x026-10-01");
    assert_eq!(broken.len(), BODY.len());
    db.transaction(|tx| {
        tx.execute(
            "UPDATE timers SET payload_json=?1 WHERE timer_id='sql-timer-1'",
            [&broken],
        )?;
        Ok(())
    })
}
fn error_of(db: &crate::runtime_db::RuntimeDb, route: usize) -> anyhow::Error {
    match read(db, route) {
        Err(error) => error,
        Ok(_) => panic!("route {route} unexpectedly returned a record or partial list"),
    }
}

#[test]
fn public_reads_borrow_sql_and_charge_exact_original_phase_through_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    for route in 0..4 {
        let budget = ledger()?;
        let exact = add(header(), unit(route));
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let record = {
            let _entry = phase.enter();
            read(&db, route)?
        };
        assert_eq!(record.first(), Some(&serde_json::from_str(BODY)?));
        assert_eq!(phase.finish()?, exact, "route {route}");
        // These plain-record APIs require their caller to retain its material.
        let first = phase.guard()?;
        let last = first.clone();
        assert!(std::sync::Arc::ptr_eq(&first, &last));
        let holder = (record, last);
        drop(phase);
        drop(first);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        assert_eq!(holder.0.first(), Some(&serde_json::from_str(BODY)?));
        drop(holder);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn public_reads_refuse_each_allocation_before_bad_decode_and_latch_original_phase() -> Result<()> {
    let (_home, db) = fixture()?;
    invalidate_date(&db)?;
    for route in 0..4 {
        let boundaries = if route == 0 {
            vec![unit(route)]
        } else {
            vec![cell(), unit(route)]
        };
        for boundary in boundaries {
            for bytes in [true, false] {
                let budget = ledger()?;
                let exact = add(header(), boundary);
                let phase = ResourceReadMaterial::with_test_limits(
                    &budget,
                    &scope(),
                    if bytes { exact.0 - 1 } else { 64 * 1024 * 1024 },
                    if bytes { 65536 } else { exact.1 - 1 },
                )?;
                {
                    let _entry = phase.enter();
                    for _ in 0..2 {
                        assert!(format!("{:#}", error_of(&db, route))
                            .to_lowercase()
                            .contains("resourceexhausted"));
                    }
                }
                let prefix = add(
                    header(),
                    if route > 0 && boundary == unit(route) {
                        cell()
                    } else {
                        (0, 0)
                    },
                );
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                drop(phase);
                assert_eq!(budget.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn lists_share_used_allowance_and_error_releases_prefix_with_original_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut first: TimerRecord = serde_json::from_str(BODY)?;
    first.id = "aaa-timer-1".into();
    assert_eq!(serde_json::to_string(&first)?.len(), BODY.len());
    db.timers().upsert(&first)?;
    invalidate_date(&db)?;
    for route in 1..4 {
        for bytes in [true, false] {
            let budget = ledger()?;
            let prefix = add(header(), unit(route));
            let exact = add(prefix, unit(route));
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes { exact.0 - 1 } else { 64 * 1024 * 1024 },
                if bytes { 65536 } else { exact.1 - 1 },
            )?;
            {
                let _entry = phase.enter();
                assert!(format!("{:#}", error_of(&db, route))
                    .to_lowercase()
                    .contains("resourceexhausted"));
                assert!(format!("{:#}", error_of(&db, route))
                    .to_lowercase()
                    .contains("resourceexhausted"));
            }
            assert_eq!(budget.report()?.observed_bytes, add(prefix, cell()).0);
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn inspection_preserves_order_limits_scope_missing_and_decode_error_without_detached_phase(
) -> Result<()> {
    let (_home, db) = fixture()?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let repository = db.timers();
    let original: TimerRecord = serde_json::from_str(BODY)?;
    let mut newer = original.clone();
    newer.id = "sql-timer-2".into();
    newer.created_at += chrono::Duration::seconds(2);
    newer.next_fire_at = Some(newer.created_at);
    newer.last_fired_at = Some(newer.created_at);
    repository.upsert(&newer)?;
    let mut foreign = newer.clone();
    foreign.id = "sql-timer-3".into();
    foreign.agent_id = "foreign-agent".into();
    foreign.created_at += chrono::Duration::seconds(2);
    foreign.next_fire_at = Some(foreign.created_at);
    foreign.last_fired_at = Some(foreign.created_at);
    repository.upsert(&foreign)?;
    assert_eq!(repository.latest("sql-timer-3")?, Some(foreign.clone()));
    assert_eq!(
        repository.latest_all()?,
        vec![original.clone(), newer.clone(), foreign.clone()]
    );
    assert_eq!(repository.recent(1)?, vec![foreign]);
    assert_eq!(
        repository.recent_for_agent("scheduling-agent", usize::MAX)?,
        vec![original, newer.clone()]
    );
    assert_eq!(
        repository.recent_for_agent("scheduling-agent", 1)?,
        vec![newer]
    );
    invalidate_date(&db)?;
    let budget = ledger()?;
    let exact = header();
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        assert!(repository.latest("missing")?.is_none());
        assert!(repository.recent(0)?.is_empty());
        assert!(repository
            .recent_for_agent("scheduling-agent", 0)?
            .is_empty());
        assert!(repository
            .recent_for_agent("missing", usize::MAX)?
            .is_empty());
    }
    assert_eq!(phase.finish()?, exact);
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    for route in 0..4 {
        assert!(format!("{:#}", error_of(&db, route)).contains("decoding timer payload"));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
