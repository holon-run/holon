use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::{atomic::AtomicUsize, Arc};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "verification-success-detail".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn cases(
    verified: bool,
    missing: &'static [&'static str],
) -> Vec<(VerificationSuccessDetail<'static>, String, usize)> {
    use VerificationSuccessDetail::*;
    let epoch = "epoch 路\\\"\n{}[]:,";
    let runtime = "runtime\t🙂";
    let fingerprint = "0123456789abcdef";
    vec![
        (
            RuntimeIdentity {
                event_log_epoch: epoch,
                public_scope_id_fingerprint: fingerprint,
                runtime_id: runtime,
                visibility_policy_generation: u64::MAX,
            },
            serde_json::json!({
                "runtime_id": runtime, "event_log_epoch": epoch,
                "visibility_policy_generation": u64::MAX,
                "public_scope_id_fingerprint": fingerprint,
            })
            .to_string(),
            16,
        ),
        (
            IdentityReservations {
                active_reservations_without_registry: 1,
                agent_state_identity_mismatch: 2,
                completed_deletion_with_available_registry: u64::MAX,
                enforcement_probe: if verified { "passed" } else { "failed" },
                missing_reservations: 3,
                missing_source_tables: missing,
                tombstone_state_drift: 4,
            },
            serde_json::json!({
                "missing_source_tables": missing, "missing_reservations": 3,
                "active_reservations_without_registry": 1,
                "tombstone_state_drift": 4, "agent_state_identity_mismatch": 2,
                "completed_deletion_with_available_registry": u64::MAX,
                "enforcement_probe": if verified { "passed" } else { "failed" },
            })
            .to_string(),
            23 + 2 + missing.len() + missing.len().saturating_sub(1),
        ),
        (
            Inventory {
                complete: verified,
                registry_kinds: usize::MAX,
                source: "full_inventory",
            },
            serde_json::json!({
                "source": "full_inventory", "registry_kinds": usize::MAX, "complete": verified,
            })
            .to_string(),
            11,
        ),
        (
            BriefLinkage {
                shared_sequences: 0,
                unmatched_linkages: 0,
                verified,
            },
            serde_json::json!({
                "unmatched_linkages": 0, "shared_sequences": 0, "verified": verified,
            })
            .to_string(),
            10,
        ),
        (
            Roster { verified },
            serde_json::json!({ "verified": verified }).to_string(),
            4,
        ),
        (
            Projection {
                boundary_rule: "event_head_seq",
                verified,
            },
            serde_json::json!({
                "verified": verified, "boundary_rule": "event_head_seq",
            })
            .to_string(),
            8,
        ),
        (
            Conversation {
                boundary_rule: "deferred_read_transaction_event_head",
                cursor_key: "durable_runtime_metadata",
                legacy_unattributed_briefs: "existing_briefs_surface",
                verified,
            },
            serde_json::json!({
                "verified": verified, "boundary_rule": "deferred_read_transaction_event_head",
                "cursor_key": "durable_runtime_metadata",
                "legacy_unattributed_briefs": "existing_briefs_surface",
            })
            .to_string(),
            16,
        ),
    ]
}

fn cost(wire: &str, slots: usize) -> (u64, usize) {
    // Independently counted shape: braces, keys, colons, separators and
    // string values; the reservation variant also includes its borrowed array.
    (
        (wire.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

struct Counted<'a> {
    detail: &'a VerificationSuccessDetail<'a>,
    passes: &'a AtomicUsize,
}

impl serde::Serialize for Counted<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.passes.fetch_add(1, Ordering::Relaxed);
        self.detail.serialize(serializer)
    }
}

fn fixture() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE observer_sync_capability_verifications (
           capability TEXT PRIMARY KEY CHECK (capability != 'reject'),
           verified INTEGER, verified_at TEXT, detail TEXT
         )",
    )?;
    Ok(connection)
}

#[test]
fn all_success_shapes_preserve_legacy_bytes_without_a_phase() -> Result<()> {
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    for verified in [false, true] {
        for missing in [&[][..], &["table 路\\\"\n", "{}[]:,"][..]] {
            for (detail, wire, _) in cases(verified, missing) {
                let passes = AtomicUsize::new(0);
                assert_eq!(
                    verification_success_detail(&Counted {
                        detail: &detail,
                        passes: &passes
                    })?,
                    wire,
                );
                assert_eq!(passes.load(Ordering::Relaxed), 1);
            }
        }
    }
    Ok(())
}

#[test]
fn all_success_shapes_share_original_remainder_and_whole_last_holder() -> Result<()> {
    let cases = cases(false, &["table 路\\\"\n", "{}[]:,"]);
    let details = cases
        .iter()
        .map(|(_, wire, slots)| cost(wire, *slots))
        .fold((0, 0), add);
    let prior = (23 * 8, 2 * 8);
    let exact = add(add(add(header(), header()), prior), details);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _active = phase.enter();
        charge_owned_metadata(23, 2)?;
    }
    let material = phase.transaction()?;
    let connection = fixture()?;
    {
        let _outer = phase.enter();
        {
            let _active = material.enter();
            for (index, (detail, wire, _)) in cases.iter().enumerate() {
                let passes = AtomicUsize::new(0);
                let result = verification_success_detail(&Counted {
                    detail,
                    passes: &passes,
                })?;
                assert_eq!(&result, wire);
                assert_eq!(passes.load(Ordering::Relaxed), 2);
                persist_verification(&connection, &index.to_string(), false, "now", &result)?;
            }
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &material.guard()?,
            ));
        }
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &phase.guard()?,
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(phase.finish()?, exact);
    assert_eq!(material.finish()?, add(header(), details));
    let first = Arc::new(vec![phase.guard()?, material.guard()?]);
    let last = first.clone();
    drop((phase, material));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(first);
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for (index, (_, wire, _)) in cases.iter().enumerate() {
        let (verified, stored): (bool, String) = connection.query_row(
            "SELECT verified, detail FROM observer_sync_capability_verifications WHERE capability = ?1",
            [index.to_string()], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert!(!verified);
        assert_eq!(&stored, wire);
    }
    Ok(())
}

#[test]
fn each_success_shape_rejects_before_owning_serialization_and_rolls_back() -> Result<()> {
    let cases = cases(true, &[]);
    let prior = (31 * 8, 3 * 8);
    for gate in 0..cases.len() {
        let before = cases[..gate]
            .iter()
            .map(|(_, wire, slots)| cost(wire, *slots))
            .fold(add(header(), prior), add);
        let through = add(before, cost(&cases[gate].1, cases[gate].2));
        for records in [false, true] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                through.0 - u64::from(!records),
                through.1 - usize::from(records),
            )?;
            let holder = phase.guard()?;
            let mut connection = fixture()?;
            let passes = AtomicUsize::new(0);
            {
                let _active = phase.enter();
                charge_owned_metadata(31, 3)?;
                let transaction = connection.transaction()?;
                for (index, (detail, wire, _)) in cases[..=gate].iter().enumerate() {
                    let result = verification_success_detail(&Counted {
                        detail,
                        passes: &passes,
                    });
                    if index == gate {
                        assert!(result
                            .unwrap_err()
                            .to_string()
                            .contains("ResourceExhausted"));
                    } else {
                        let detail = result?;
                        assert_eq!(&detail, wire);
                        persist_verification(
                            &transaction,
                            &index.to_string(),
                            true,
                            "now",
                            &detail,
                        )?;
                    }
                }
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert_eq!(passes.load(Ordering::Relaxed), gate * 2 + 1);
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(connection.is_autocommit());
            assert_eq!(
                count_rows(
                    &connection,
                    "SELECT COUNT(*) FROM observer_sync_capability_verifications"
                )?,
                0
            );
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn success_detail_sql_errors_preserve_phase_and_release_only_at_last_holder() -> Result<()> {
    for (detail, wire, slots) in cases(false, &["missing"]) {
        let exact = add(header(), cost(&wire, slots));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        let mut connection = fixture()?;
        {
            let _active = phase.enter();
            let transaction = connection.transaction()?;
            let detail = verification_success_detail(&detail)?;
            persist_verification(&transaction, "earlier", false, "now", &detail)?;
            assert!(persist_verification(&transaction, "reject", false, "now", &detail).is_err());
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        assert!(connection.is_autocommit());
        assert_eq!(
            count_rows(
                &connection,
                "SELECT COUNT(*) FROM observer_sync_capability_verifications"
            )?,
            0
        );
        assert_eq!(phase.finish()?, exact);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn actual_verifier_persists_success_shapes_and_normal_false_on_original_phase() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut connection = db.connection()?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verification_version = 0 WHERE capability = ?1",
        [EVENT_PROJECTION_EFFECT_COMPLETE],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    {
        let _active = phase.enter();
        verify_observer_sync_foundations(&mut connection)?;
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    assert!(connection.is_autocommit());
    let mut details = Vec::new();
    for (capability, fields) in [
        (RUNTIME_IDENTITY_STABLE, 4),
        (AGENT_IDENTITY_RESERVED, 7),
        (EVENT_PROJECTION_EFFECT_COMPLETE, 3),
        (BRIEF_ATOMIC_LINKAGE_VERIFIED, 3),
        (ROSTER_SNAPSHOT_VERIFIED, 1),
        (PROJECTION_SNAPSHOT_VERIFIED, 2),
        (CONVERSATION_READ_VERIFIED, 4),
    ] {
        let (verified, wire): (bool, String) = connection.query_row(
            "SELECT verified, detail FROM observer_sync_capability_verifications WHERE capability = ?1",
            [capability], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert!(verified, "{capability}: {wire}");
        let value: serde_json::Value = serde_json::from_str(&wire)?;
        assert_eq!(value.as_object().unwrap().len(), fields);
        assert_eq!(value.to_string(), wire);
        assert!(value.get("error").is_none());
        details.push(wire);
    }
    // Proof reuse leaves the previous inventory detail untouched.
    verify_observer_sync_foundations(&mut connection)?;
    let inventory: String = connection.query_row(
        "SELECT detail FROM observer_sync_capability_verifications WHERE capability = ?1",
        [EVENT_PROJECTION_EFFECT_COMPLETE],
        |row| row.get(0),
    )?;
    assert_eq!(inventory, details[2]);
    connection.execute(
        "INSERT INTO agent_identity_reservations
         (agent_id, reservation_state, reserved_at, retired_at, source)
         VALUES ('orphan-success-test', 'active', 'now', NULL, 'verification_test')",
        [],
    )?;
    {
        let _active = phase.enter();
        verify_observer_sync_foundations(&mut connection)?;
    }
    let (verified, wire): (bool, String) = connection.query_row(
        "SELECT verified, detail FROM observer_sync_capability_verifications WHERE capability = ?1",
        [AGENT_IDENTITY_RESERVED],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert!(!verified);
    let value: serde_json::Value = serde_json::from_str(&wire)?;
    assert_eq!(value["active_reservations_without_registry"], 1);
    assert_eq!(value["enforcement_probe"], "passed");
    assert!(value.get("error").is_none());
    let used = phase.finish()?.0;
    assert!(used > header().0);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, used);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
