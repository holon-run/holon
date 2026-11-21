use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "observer-agent".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn wire_cost(payload: &str) -> Result<(u64, usize)> {
    // Independent AST formula, not the production scanner or gate.
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
    let slots = slots(&serde_json::from_str(payload)?);
    Ok((
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    ))
}

fn timestamp_cost() -> (u64, usize) {
    // Real current UTC timestamps have four year digits and three decimals.
    (
        (("2026-10-17T18:50:39.123Z".len() + std::mem::size_of::<String>()) as u64) * 8,
        8,
    )
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb, AuditEvent)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut event = AuditEvent::legacy("observer_fixture", serde_json::json!({}));
    event.event_log_epoch = db.event_log_epoch()?;
    db.connection()?.execute(
        "UPDATE observer_sync_capability_verifications
         SET verified = 1, verification_version = ?1, event_log_epoch = ?2,
             event_generation = 0, verified_event_generation = 0,
             verified_at = 'sentinel', detail = 'sentinel'
         WHERE capability = ?3",
        rusqlite::params![
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            event.event_log_epoch,
            EVENT_PROJECTION_EFFECT_COMPLETE,
        ],
    )?;
    Ok((home, db, event))
}

fn unsupported(event: &AuditEvent, reason: usize) -> AuditEvent {
    let mut event = event.clone();
    event.contract_version = crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION;
    event.kind = "future_\\\"{}[]:, 路".into();
    event.payload_schema = "holon.runtime_event.future".into();
    match reason {
        0 => {}
        1 => {
            event.kind = "brief_created".into();
            event.payload_schema = "holon.runtime_event.wrong".into();
        }
        2 => {
            let descriptor =
                crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic.descriptor();
            event.kind = descriptor.wire_name.into();
            event.payload_schema = descriptor.payload_schema.into();
            event.payload_schema_version = descriptor.payload_schema_version + 1;
        }
        _ => unreachable!(),
    }
    event
}

fn detail(event: &AuditEvent) -> String {
    let crate::runtime_event::ProjectionEffectClassification::Unsupported(reason) =
        crate::runtime_event::classify_projection_effect(
            &event.kind,
            event.contract_version,
            &event.payload_schema,
            event.payload_schema_version,
        )
    else {
        panic!("fixture must classify as unsupported");
    };
    serde_json::json!({
        "source": "trusted_append", "complete": false,
        "kind": event.kind, "reason": format!("{reason:?}"),
    })
    .to_string()
}

fn snapshot(db: &crate::runtime_db::RuntimeDb) -> Result<(i64, i64, i64, String, String)> {
    Ok(db.connection()?.query_row(
        "SELECT verified, event_generation, verified_event_generation, verified_at, detail
         FROM observer_sync_capability_verifications WHERE capability = ?1",
        [EVENT_PROJECTION_EFFECT_COMPLETE],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?)
}

fn advance(connection: &Connection) -> Result<()> {
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET event_generation = event_generation + 1
         WHERE capability = ?1",
        [EVENT_PROJECTION_EFFECT_COMPLETE],
    )?;
    Ok(())
}

#[test]
fn proof_reads_return_scalars_without_epoch_material_and_preserve_reuse_predicates() -> Result<()> {
    let (_home, db, event) = fixture()?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    for (epoch, version, generation, verified_generation, verified, expected) in [
        (
            Some(event.event_log_epoch.as_str()),
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            0,
            0,
            1,
            Some(true),
        ),
        (
            Some(event.event_log_epoch.as_str()),
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            4,
            4,
            0,
            Some(false),
        ),
        (
            None,
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            0,
            0,
            1,
            None,
        ),
        (
            Some("foreign epoch"),
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            0,
            0,
            1,
            None,
        ),
        (Some(event.event_log_epoch.as_str()), -1, 0, 0, 1, None),
        (
            Some(event.event_log_epoch.as_str()),
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            -1,
            -1,
            1,
            None,
        ),
        (
            Some(event.event_log_epoch.as_str()),
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            4,
            3,
            1,
            None,
        ),
    ] {
        db.connection()?.execute(
            "UPDATE observer_sync_capability_verifications SET event_log_epoch = ?1,
             verification_version = ?2, event_generation = ?3, verified_event_generation = ?4,
             verified = ?5 WHERE capability = ?6",
            rusqlite::params![
                epoch,
                version,
                generation,
                verified_generation,
                verified,
                EVENT_PROJECTION_EFFECT_COMPLETE
            ],
        )?;
        let _entry = phase.enter();
        let proof =
            event_projection_effect_proof(&db.connection()?, &event.event_log_epoch)?.unwrap();
        assert_eq!(
            proof.event_log_epoch_matches,
            epoch == Some(event.event_log_epoch.as_str())
        );
        assert_eq!(proof.verification_version, version);
        assert_eq!(proof.event_generation, generation);
        assert_eq!(proof.verified_event_generation, verified_generation);
        assert_eq!(proof.verified, verified != 0);
        assert_eq!(
            reusable_event_projection_effect_verification(
                &db.connection()?,
                &event.event_log_epoch
            )?,
            expected
        );
    }
    assert_eq!(phase.finish()?, header());
    db.connection()?.execute(
        "UPDATE observer_sync_capability_verifications SET event_log_epoch = x'ff'
         WHERE capability = ?1",
        [EVENT_PROJECTION_EFFECT_COMPLETE],
    )?;
    {
        let _entry = phase.enter();
        assert!(event_projection_effect_proof(&db.connection()?, &event.event_log_epoch).is_err());
        assert!(reusable_event_projection_effect_verification(
            &db.connection()?,
            &event.event_log_epoch
        )
        .is_err());
        assert!(record_appended_event_projection_effect(&db.connection()?, &event).is_err());
    }
    assert_eq!(phase.finish()?, header());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn timestamp_and_detail_gates_use_original_remaining_phase_and_roll_back() -> Result<()> {
    let (_home, db, event) = fixture()?;
    let time = timestamp_cost();
    let prior = (17 * 8, 8);
    let base = add(add(header(), header()), prior);
    for unsupported_detail in [false, true] {
        let event = if unsupported_detail {
            unsupported(&event, 0)
        } else {
            event.clone()
        };
        let next = if unsupported_detail {
            wire_cost(&detail(&event))?
        } else {
            time
        };
        let prefix = if unsupported_detail {
            add(base, time)
        } else {
            base
        };
        for bytes in [true, false] {
            let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes {
                    prefix.0 + next.0 - 1
                } else {
                    64 << 20
                },
                if bytes { 65536 } else { prefix.1 + next.1 - 1 },
            )?;
            let transaction = phase.transaction()?;
            let holders = Arc::new(vec![phase.guard()?, transaction.guard()?]);
            let before = snapshot(&db)?;
            {
                let _entry = transaction.enter();
                charge_owned_metadata(17, 1)?;
                let error = db
                    .transaction(|tx| {
                        advance(tx)?;
                        record_appended_event_projection_effect(tx, &event)
                    })
                    .unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
            }
            assert_eq!(snapshot(&db)?, before);
            assert!(phase.finish().is_err());
            assert!(transaction.finish().is_err());
            let observed = budget.report()?.observed_bytes;
            if unsupported_detail {
                assert_eq!(observed, base.0 + time.0);
            } else {
                assert_eq!(observed, base.0);
            }
            // Outside TLS, an inspection does not consume the failed phase.
            charge_owned_metadata(1, 1)?;
            assert_eq!(budget.report()?.observed_bytes, observed);
            drop((phase, transaction));
            assert_eq!(budget.report()?.observed_bytes, observed);
            drop(holders);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn trusted_append_outputs_preserve_wire_and_last_whole_holder() -> Result<()> {
    for reason in 0..6 {
        let (_home, db, event) = fixture()?;
        let mut event = if reason < 3 {
            unsupported(&event, reason)
        } else {
            event
        };
        if reason == 4 {
            let descriptor =
                crate::runtime_event::RuntimeEventKind::SchedulerDiagnostic.descriptor();
            event.kind = descriptor.wire_name.into();
            event.contract_version = crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION;
            event.payload_schema = descriptor.payload_schema.into();
            event.payload_schema_version = descriptor.payload_schema_version;
        }
        if reason == 5 {
            db.connection()?.execute(
                "UPDATE observer_sync_capability_verifications SET verified = 0
                 WHERE capability = ?1",
                [EVENT_PROJECTION_EFFECT_COMPLETE],
            )?;
        }
        let expected_detail = if reason < 3 {
            detail(&event)
        } else {
            "sentinel".into()
        };
        let detail_cost = if reason < 3 {
            wire_cost(&expected_detail)?
        } else {
            (0, 0)
        };
        let time = timestamp_cost();
        let base = add(add(header(), header()), (17 * 8, 8));
        let maximum = add(add(base, time), detail_cost);
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase =
            ResourceReadMaterial::with_test_limits(&budget, &scope(), maximum.0, maximum.1)?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            charge_owned_metadata(17, 1)?;
            db.transaction(|tx| {
                advance(tx)?;
                record_appended_event_projection_effect(tx, &event)
            })?;
        }
        let used = phase.finish()?;
        assert_eq!(used, add(add(base, time), detail_cost));
        assert_eq!(
            transaction.finish()?,
            (used.0 - header().0, used.1 - header().1)
        );
        let stored = snapshot(&db)?;
        assert_eq!(
            (stored.0, stored.1, stored.2),
            (i64::from((3..5).contains(&reason)), 1, 1)
        );
        assert_eq!(stored.4, expected_detail);
        assert!(stored.3.ends_with('Z'));
        assert_eq!(stored.3.len(), 24);
        chrono::DateTime::parse_from_rfc3339(&stored.3)?;
        charge_owned_metadata(1, 1)?;
        assert_eq!(phase.finish()?, used);
        let first = Arc::new(vec![phase.guard()?, transaction.guard()?]);
        let last = first.clone();
        drop((phase, transaction));
        assert_eq!(budget.report()?.observed_bytes, used.0);
        drop(first);
        assert_eq!(budget.report()?.observed_bytes, used.0);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn verification_timestamp_admits_exact_string_metadata_before_formatting() -> Result<()> {
    for text in [
        "2026-10-17T18:50:39Z",
        "2026-10-17T18:50:39.123456789Z",
        "0000-01-01T00:00:00Z",
        "-0001-01-01T00:00:00Z",
        "+10000-01-01T00:00:00Z",
        "-10000-01-01T00:00:00Z",
    ] {
        let now: chrono::DateTime<chrono::Utc> = text.parse()?;
        let expected = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let cost = (
            ((expected.len() + std::mem::size_of::<String>()) as u64) * 8,
            8,
        );
        for succeeds in [false, true] {
            let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let exact = add(header(), cost);
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                exact.0 - u64::from(!succeeds),
                exact.1,
            )?;
            {
                let _entry = phase.enter();
                let result = observer_verification_timestamp(now);
                if succeeds {
                    assert_eq!(result?, expected);
                } else {
                    assert!(format!("{:#}", result.unwrap_err()).contains("ResourceExhausted"));
                }
            }
            if succeeds {
                assert_eq!(phase.finish()?, exact);
            } else {
                assert!(phase.finish().is_err());
            }
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn ineligible_append_proofs_and_absent_rows_do_not_admit_outputs() -> Result<()> {
    let (_home, db, event) = fixture()?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), header().0, header().1)?;
    for (version, epoch, generation, verified_generation) in [
        (-1, Some(event.event_log_epoch.as_str()), 1, 0),
        (EVENT_PROJECTION_EFFECT_VERIFIER_VERSION, None, 1, 0),
        (
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            Some("foreign"),
            1,
            0,
        ),
        (
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            Some(event.event_log_epoch.as_str()),
            0,
            -1,
        ),
        (
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            Some(event.event_log_epoch.as_str()),
            4,
            2,
        ),
    ] {
        db.connection()?.execute(
            "UPDATE observer_sync_capability_verifications SET verification_version = ?1,
             event_log_epoch = ?2, event_generation = ?3, verified_event_generation = ?4
             WHERE capability = ?5",
            rusqlite::params![
                version,
                epoch,
                generation,
                verified_generation,
                EVENT_PROJECTION_EFFECT_COMPLETE
            ],
        )?;
        let before = snapshot(&db)?;
        {
            let _entry = phase.enter();
            record_appended_event_projection_effect(&db.connection()?, &event)?;
        }
        assert_eq!(snapshot(&db)?, before);
        assert_eq!(phase.finish()?, header());
    }
    db.connection()?
        .execute("DELETE FROM observer_sync_capability_verifications", [])?;
    {
        let _entry = phase.enter();
        assert!(
            event_projection_effect_proof(&db.connection()?, &event.event_log_epoch)?.is_none()
        );
        record_appended_event_projection_effect(&db.connection()?, &event)?;
    }
    db.connection()?
        .execute("DROP TABLE observer_sync_capability_verifications", [])?;
    {
        let _entry = phase.enter();
        record_appended_event_projection_effect(&db.connection()?, &event)?;
    }
    assert_eq!(phase.finish()?, header());
    drop(phase);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
