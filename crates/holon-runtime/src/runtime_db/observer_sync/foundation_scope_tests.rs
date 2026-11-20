use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    ids::{visibility_scope_id, visibility_scope_id_bytes},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "foundation-scope".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn legacy_scope(runtime: &str, principal: &str, entitlement: &str, generation: u64) -> String {
    // Independent pre-change algorithm, including decimal formatting and hex.
    let mut hash = Sha256::new();
    let decimal = generation.to_string();
    for bytes in [
        b"holon.visibility-scope.v1\x00".as_slice(),
        runtime.as_bytes(),
        b"\x00",
        principal.as_bytes(),
        b"\x00",
        entitlement.as_bytes(),
        b"\x00",
        decimal.as_bytes(),
    ] {
        hash.update(bytes);
    }
    let digest = hash.finalize();
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("vscope1_{hex}")
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}

fn assert_no_probe(connection: &Connection) -> Result<()> {
    assert!(connection.is_autocommit());
    for table in ["agent_identity_reservations", "agent_identities"] {
        let count: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = ?1"),
            ["__observer_sync_reservation_probe__"],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0, "{table}");
    }
    Ok(())
}

#[test]
fn fixed_scope_matches_legacy_bytes_at_every_decimal_width() {
    let mut generations = vec![0, 1, u64::MAX - 1, u64::MAX];
    for exponent in 1..=19 {
        let boundary = 10u64.pow(exponent);
        generations.extend([boundary - 1, boundary, boundary + 1]);
    }
    for (runtime, principal, entitlement) in [
        ("runtime_fixture", "public", "public"),
        ("", "", ""),
        ("runtime_汉字_\"\\\n", "principal\0one", "\0entitlement"),
        ("a\0b", "c", "d"),
        ("a", "b\0c", "d"),
    ] {
        for &generation in &generations {
            let expected = legacy_scope(runtime, principal, entitlement, generation);
            let actual = visibility_scope_id_bytes(runtime, principal, entitlement, generation);
            assert_eq!(actual.len(), 40);
            assert!(actual.is_ascii());
            assert_eq!(actual.as_slice(), expected.as_bytes());
            assert_eq!(
                visibility_scope_id(runtime, principal, entitlement, generation),
                expected,
            );
            assert_eq!(&actual[..16], expected[..16].as_bytes());
        }
    }
}

#[test]
fn fixed_scope_uses_no_phase_remainder_and_preserves_last_holder() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let exact = (header().0 + 37 * 8, header().1 + 3 * 8);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(37, 3)?;
        for generation in [0, 9, 10, 99, 100, u64::MAX] {
            let first = visibility_scope_id_bytes(
                "runtime_汉字",
                PUBLIC_SCOPE_PRINCIPAL,
                PUBLIC_SCOPE_ENTITLEMENT,
                generation,
            );
            let second = visibility_scope_id_bytes(
                "runtime_汉字",
                PUBLIC_SCOPE_PRINCIPAL,
                PUBLIC_SCOPE_ENTITLEMENT,
                generation,
            );
            assert_eq!(first, second);
            assert!(std::str::from_utf8(&first[..16]).is_ok());
        }
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap(),
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(phase.finish()?, exact);
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn real_verifier_preserves_scope_fingerprint_and_empty_identity_verdict() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    for (runtime, generation, verified) in [
        ("runtime_汉字_\"\\\n", 0, true),
        ("runtime_汉字_\"\\\n", u64::MAX, true),
        ("", 10, false),
    ] {
        connection.execute(
            "UPDATE runtime_metadata SET value = ?1 WHERE key = 'runtime_id'",
            [runtime],
        )?;
        connection.execute(
            "UPDATE runtime_metadata SET value = ?1 WHERE key = 'visibility_policy_generation'",
            [generation.to_string()],
        )?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            verify_observer_sync_foundations(&mut connection)?;
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap(),
            ));
        }
        let (actual, detail): (bool, String) = connection.query_row(
            "SELECT verified, detail FROM observer_sync_capability_verifications
             WHERE capability = ?1",
            [RUNTIME_IDENTITY_STABLE],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let detail: serde_json::Value = serde_json::from_str(&detail)?;
        let expected = legacy_scope(
            runtime,
            PUBLIC_SCOPE_PRINCIPAL,
            PUBLIC_SCOPE_ENTITLEMENT,
            generation,
        );
        assert_eq!(actual, verified);
        assert_eq!(detail["public_scope_id_fingerprint"], expected[..16]);
        assert_eq!(detail["runtime_id"], runtime);
        assert_eq!(detail["visibility_policy_generation"], generation);
        assert_no_probe(&connection)?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let retained = phase.finish()?.0;
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, retained);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn real_scope_chain_preserves_next_gate_rejection_and_sql_error_release() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verified_at = 'sentinel'",
        [],
    )?;
    let mut exact = header();
    for key in [
        "runtime_id",
        "event_log_epoch",
        "visibility_policy_generation",
    ] {
        let value: String = connection.query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        exact.0 += ((value.len() + std::mem::size_of::<String>()) as u64) * 8;
        exact.1 += 8;
    }
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
        assert!(ResourceReadMaterial::active_guard().is_err());
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(phase.finish().is_err());
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    assert_no_probe(&connection)?;
    assert_eq!(
        count_rows(
            &connection,
            "SELECT COUNT(*) FROM observer_sync_capability_verifications
             WHERE verified_at != 'sentinel'",
        )?,
        0,
    );
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    connection.execute_batch(
        "CREATE TEMP TRIGGER reject_scope_verification
         BEFORE INSERT ON observer_sync_capability_verifications
         BEGIN SELECT RAISE(ABORT, 'scope-verification-fixture'); END;",
    )?;
    let outer = ResourceReadMaterial::new(&ledger, &scope())?;
    let outer_holder = outer.guard()?;
    {
        let _outer_entry = outer.enter();
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        {
            let _entry = phase.enter();
            let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
            assert!(format!("{error:#}").contains("scope-verification-fixture"));
        }
        assert!(Arc::ptr_eq(
            &outer_holder,
            &ResourceReadMaterial::active_guard()?.unwrap(),
        ));
        assert!(phase.finish()?.0 > header().0);
        assert_no_probe(&connection)?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, outer.finish()?.0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((outer, outer_holder));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
