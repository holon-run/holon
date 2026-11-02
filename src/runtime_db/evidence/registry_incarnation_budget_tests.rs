use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use std::sync::Arc;

pub(super) fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "registry-incarnation".into(),
        identity_incarnation: 1,
    }
}

pub(super) fn name_key_cost(record: &AgentIdentityRecord) -> (u64, usize) {
    record.name.as_deref().map_or((0, 0), |name| {
        let original_key = crate::types::agent_name_key(name);
        (
            ((original_key.len() + std::mem::size_of::<String>()) as u64) * 8,
            8,
        )
    })
}

pub(super) fn timestamp_cost(record: &AgentIdentityRecord) -> (u64, usize) {
    [
        Some(record.created_at),
        Some(record.updated_at),
        record.deleted_at,
    ]
    .into_iter()
    .flatten()
    .fold((0, 0), |total, value| {
        let original = value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        add(
            total,
            (
                ((original.len() + std::mem::size_of::<String>()) as u64) * 8,
                8,
            ),
        )
    })
}

pub(super) fn clock_timestamp_cost() -> (u64, usize) {
    let original = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    (
        ((original.len() + std::mem::size_of::<String>()) as u64) * 8,
        8,
    )
}

pub(super) fn upsert_timestamp_cost(record: &AgentIdentityRecord) -> (u64, usize) {
    add(clock_timestamp_cost(), timestamp_cost(record))
}

pub(super) fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

pub(super) fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

pub(super) fn decode_cost(payload: &str) -> (u64, usize) {
    // The semantic shape counts opening quotes, not closing quotes; each
    // object key contributes its opening quote and colon.
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len() * 2
                    + fields.len().saturating_sub(1)
                    + fields.values().map(slots).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let cells = slots(&serde_json::from_str(payload).unwrap());
    (
        (payload.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
        (cells * 4 + 16) * 8,
    )
}

pub(super) fn identity() -> AgentIdentityRecord {
    let mut record = AgentIdentityRecord::new(
        "registry-identity",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        Some("parent-引号\"{}[]:,".into()),
        Some("delegated-\\\"task".into()),
    );
    record.name = Some("registry-名字".into());
    record.lineage_parent_agent_id = Some("lineage-parent".into());
    record.created_at = "2026-10-17T21:40:00Z".parse().unwrap();
    record.updated_at = record.created_at;
    record
}

pub(super) fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}

pub(super) fn snapshot(
    db: &crate::runtime_db::RuntimeDb,
) -> Result<Vec<Vec<Vec<rusqlite::types::Value>>>> {
    let connection = db.connection()?;
    [
        "agent_identities",
        "agent_identity_reservations",
        "agent_episode_continuity",
        "runtime_metadata",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = connection.prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))?;
        let rows = statement
            .query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|index| row.get(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .collect()
}

#[test]
fn registry_incarnation_reuses_original_remainder_and_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    db.agent_identities().upsert(&record)?;
    let before_sql = snapshot(&db)?;
    let one = add(
        decode_cost(&serde_json::to_string(&record)?),
        add(name_key_cost(&record), upsert_timestamp_cost(&record)),
    );
    let used = (31 * 8, 3 * 8);
    let before = add(header(), used);
    let exact = add(add(before, one), one);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        for attempt in 1..=2 {
            db.agent_identities().upsert(&record)?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder,
            ));
            assert_eq!(
                phase.finish()?,
                (
                    before.0 + one.0 * attempt,
                    before.1 + one.1 * attempt as usize
                )
            );
            assert_eq!(snapshot(&db)?, before_sql);
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn registry_incarnation_denial_precedes_reservation_and_rolls_back_transaction() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    db.agent_identities().upsert(&record)?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_incarnation_reservation BEFORE INSERT ON agent_identity_reservations
         BEGIN SELECT RAISE(ABORT, 'unexpected_reservation_write'); END;",
    )?;
    let before_sql = snapshot(&db)?;
    let before = add(header(), (29 * 8, 2 * 8));
    let exact = add(before, decode_cost(&serde_json::to_string(&record)?));
    for dimension in 0..2 {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            exact.0 - u64::from(dimension == 0),
            exact.1 - usize::from(dimension == 1),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(29, 2)?;
            let error = db
                .transaction(|tx| {
                    tx.execute(
                        "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                     VALUES ('registry_budget_marker', 'rollback',
                             '2026-10-17T21:40:00Z', '2026-10-17T21:40:00Z')",
                        [],
                    )?;
                    upsert_agent_identity_tx(tx, &record)
                })
                .unwrap_err();
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            assert!(ResourceReadMaterial::active_guard().is_err());
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(phase.finish().is_err());
        assert_eq!(snapshot(&db)?, before_sql);
        assert_eq!(ledger.report()?.observed_bytes, before.0);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, before.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn registry_incarnation_admits_before_validating_complete_identity() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    db.agent_identities().upsert(&record)?;
    let mut invalid = serde_json::to_value(&record)?;
    invalid["status"] = "unknown_registry_status".into();
    let invalid = serde_json::to_string(&invalid)?;
    for (payload, cost) in [
        (invalid.as_str(), decode_cost(&invalid)),
        ("{}", decode_cost("{}")),
        // One opening object cell, with no valid complete JSON value.
        ("{", ((4 + 128 + 512) * 8, (4 + 16) * 8)),
    ] {
        db.connection()?.execute(
            "UPDATE agent_identities SET payload_json = ?1 WHERE agent_id = ?2",
            params![payload, record.agent_id],
        )?;
        let before_sql = snapshot(&db)?;
        let exact = add(header(), cost);
        for denied in [None, Some(0), Some(1)] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(denied == Some(0)),
                exact.1 - usize::from(denied == Some(1)),
            )?;
            {
                let _entry = phase.enter();
                let error = db.agent_identities().upsert(&record).unwrap_err();
                if denied.is_some() {
                    assert!(format!("{error:#}").contains("ResourceExhausted"));
                    assert!(phase.finish().is_err());
                    assert_eq!(ledger.report()?.observed_bytes, header().0);
                } else {
                    assert!(error.downcast_ref::<serde_json::Error>().is_some());
                    assert_eq!(phase.finish()?, exact);
                }
                assert_eq!(snapshot(&db)?, before_sql);
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        let error = db.agent_identities().upsert(&record).unwrap_err();
        assert!(error.downcast_ref::<serde_json::Error>().is_some());
        assert_eq!(snapshot(&db)?, before_sql);
    }
    Ok(())
}

#[test]
fn registry_incarnation_missing_row_and_sql_errors_do_not_charge_decode() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let exact = add(
        add(header(), decode_cost(&serde_json::to_string(&record)?)),
        add(name_key_cost(&record), upsert_timestamp_cost(&record)),
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        // The missing pre-read is free; the actual post-write decode is charged.
        db.agent_identities().upsert(&record)?;
        assert_eq!(phase.finish()?, exact);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.connection()?
        .execute_batch("DROP TABLE agent_identities")?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        let error = db.agent_identities().upsert(&record).unwrap_err();
        assert!(format!("{error:#}").contains("no such table"));
        assert_eq!(phase.finish()?, header());
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder,
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, header().0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn registry_incarnation_preserves_stale_continuity_and_retired_guard() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    db.agent_identities().upsert(&record)?;
    let before_sql = snapshot(&db)?;
    let mut replacement = record.clone();
    replacement.incarnation = 2;
    replacement.updated_at -= chrono::Duration::seconds(1);
    db.agent_identities().upsert(&replacement)?;
    assert_eq!(snapshot(&db)?, before_sql);
    replacement.updated_at = record.updated_at + chrono::Duration::seconds(1);
    db.agent_identities().upsert(&replacement)?;
    let count: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM agent_episode_continuity WHERE agent_id = ?1 AND identity_incarnation = 2",
        [&record.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    let mut deleted = replacement.clone();
    deleted.status = AgentRegistryStatus::Deleted;
    db.agent_identities().upsert(&deleted)?;
    let before_sql = snapshot(&db)?;
    let exact = add(header(), decode_cost(&serde_json::to_string(&deleted)?));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        let error = db.agent_identities().upsert(&replacement).unwrap_err();
        assert!(format!("{error:#}").contains("retired"));
        assert_eq!(phase.finish()?, exact);
        assert_eq!(snapshot(&db)?, before_sql);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
