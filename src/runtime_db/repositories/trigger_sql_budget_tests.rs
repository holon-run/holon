use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger, resident_input_evidence::InputEvidenceScope,
};

const BODY: &str = r#"{"external_trigger_id":"sql-trigger","target_agent_id":"scheduling-agent","scope":"agent","delivery_mode":"wake_hint","token":"sql-token","token_hash":"sql-hash","status":"revoked","created_at":"2026-10-01T00:00:00.123456789Z","revoked_at":"2026-10-01T00:00:00.123456789Z","last_delivered_at":"2026-10-01T00:00:00.123456789Z","delivery_count":1}"#;
const DATE: &str = r#""2026-10-01T00:00:00.123456789Z""#;

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
fn costs() -> [(u64, usize); 4] {
    // Eleven keys/colons, ten commas, two braces, ten string values.
    [
        cost(BODY.len(), 44),
        cost(DATE.len(), 1),
        cost(DATE.len(), 1),
        cost(DATE.len(), 1),
    ]
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
fn seed_active(db: &crate::runtime_db::RuntimeDb, payload: &str) -> Result<()> {
    db.transaction(|tx| {
        upsert_external_trigger_tx(tx, &serde_json::from_str(BODY)?)?;
        tx.execute(
            "UPDATE external_triggers SET status='active',payload_json=?1",
            [payload],
        )?;
        Ok(())
    })
}
fn row_count(db: &crate::runtime_db::RuntimeDb) -> Result<i64> {
    db.transaction(|tx| {
        Ok(tx.query_row("SELECT count(*) FROM external_triggers", [], |r| r.get(0))?)
    })
}

#[test]
fn sql_serialization_and_each_timestamp_share_exact_transaction_and_ack_holders() -> Result<()> {
    let (_home, db) = fixture()?;
    let record: ExternalTriggerRecord = serde_json::from_str(BODY)?;
    assert_eq!(serde_json::to_string(&record)?, BODY);
    assert_eq!(serde_json::to_string(&record.created_at)?, DATE);
    let budget = ledger()?;
    let body = costs().into_iter().fold((0, 0), add);
    let exact = add(add(header(), header()), body);
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        db.transaction(|tx| {
            assert!(upsert_external_trigger_with_write_tx(tx, &record)?);
            Ok(())
        })?;
    }
    assert_eq!(phase.finish()?, exact);
    assert_eq!(transaction.finish()?, add(header(), body));
    db.transaction(|tx| {
        let row: (String, String, String, String, String) = tx.query_row(
            "SELECT payload_json,status,created_at,revoked_at,last_delivered_at FROM external_triggers",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        assert_eq!(row.0, BODY);
        assert_eq!(row.1, "revoked");
        for timestamp in [row.2, row.3, row.4] {
            assert_eq!(timestamp, "2026-10-01T00:00:00.123Z");
        }
        Ok(())
    })?;
    let first = transaction.guard()?;
    let last = first.clone();
    assert!(std::sync::Arc::ptr_eq(&first, &last));
    drop((transaction, phase, record));
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(first);
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn every_sql_output_gate_refuses_before_write_and_rollback_releases_only_transaction() -> Result<()>
{
    let (_home, db) = fixture()?;
    let record: ExternalTriggerRecord = serde_json::from_str(BODY)?;
    let mut prefix = add(header(), header());
    for next in costs() {
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
                let error = db
                    .transaction(|tx| upsert_external_trigger_tx(tx, &record))
                    .unwrap_err();
                assert!(format!("{error:#}")
                    .to_lowercase()
                    .contains("resourceexhausted"));
                assert!(db
                    .transaction(|tx| upsert_external_trigger_tx(tx, &record))
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
    Ok(())
}

#[test]
fn borrowed_sql_payload_decodes_with_one_envelope_and_keeps_original_remainder() -> Result<()> {
    let (_home, db) = fixture()?;
    let budget = ledger()?;
    seed_active(&db, BODY)?;
    let exact = add(header(), cost(BODY.len(), 44));
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let record = {
        let _entry = phase.enter();
        db.transaction(|tx| active_default_for_agent_tx(tx, &scope().agent_id))?
    };
    assert_eq!(record, Some(serde_json::from_str(BODY)?));
    assert_eq!(phase.finish()?, exact);
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        assert!(db
            .transaction(|tx| active_default_for_agent_tx(tx, &scope().agent_id))
            .is_err());
    }
    assert!(phase.finish().is_err());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop((record, holder));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn missing_sql_row_is_free_and_decode_rejection_precedes_parser_with_error_release() -> Result<()> {
    let (_home, db) = fixture()?;
    let budget = ledger()?;
    let phase = ResourceReadMaterial::new(&budget, &scope())?;
    {
        let _entry = phase.enter();
        assert!(db
            .transaction(|tx| active_default_for_agent_tx(tx, &scope().agent_id))?
            .is_none());
    }
    assert_eq!(phase.finish()?, header());
    drop(phase);
    seed_active(&db, "{")?;
    for bytes in [true, false] {
        let next = cost(1, 1);
        let prefix = add(header(), header());
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
            let error = db
                .transaction(|tx| active_default_for_agent_tx(tx, &scope().agent_id))
                .unwrap_err();
            assert!(format!("{error:#}")
                .to_lowercase()
                .contains("resourceexhausted"));
        }
        assert_eq!(budget.report()?.observed_bytes, prefix.0);
        assert!(phase.finish().is_err());
        drop(transaction);
        assert_eq!(budget.report()?.observed_bytes, header().0);
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    let phase = ResourceReadMaterial::new(&budget, &scope())?;
    let transaction = phase.transaction()?;
    {
        let _entry = transaction.enter();
        let error = db
            .transaction(|tx| active_default_for_agent_tx(tx, &scope().agent_id))
            .unwrap_err();
        assert!(format!("{error:#}").contains("decoding external trigger payload"));
    }
    assert_eq!(transaction.finish()?, add(header(), cost(1, 1)));
    drop(transaction);
    assert_eq!(budget.report()?.observed_bytes, header().0);
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
