use super::*;
use crate::{
    config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    }
}

#[test]
fn native_original_material_uses_configured_input_limits_and_keeps_its_own_cap() -> Result<()> {
    use crate::runtime::resident_input_evidence::InputEvidenceFamily;
    use crate::runtime_db::input_evidence_snapshot::native_birth::{
        original_material_limits, NativeInputWriteProof,
    };
    let (_home, db) = fixture()?;
    let scope = scope();
    for (bytes, records, expected) in [
        (1024 * 1024, 1024, (2 * 1024 * 1024, 1024)),
        (128 * 1024 * 1024, 131_072, (64 * 1024 * 1024, 65_536)),
        (1024 * 1024, 1, (2 * 1024 * 1024, 1)),
    ] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits {
            history_per_agent_bytes: bytes,
            pending_per_agent_bytes: bytes,
            record_limit: records,
            ..Default::default()
        })?;
        assert_eq!(original_material_limits(&ledger)?, expected);
        if records == 1 {
            let error = db
                .transaction(|tx| {
                    NativeInputWriteProof::prepare_tx(
                        tx,
                        &scope,
                        &ledger,
                        original_material_limits(&ledger)?,
                        None,
                        |visit| visit(InputEvidenceFamily::Transcript, "native-configured-cap"),
                    )
                    .map(drop)
                })
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("Input read material"),
                "{error:#}"
            );
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn native_birth_borrows_validated_scope_without_another_identity_tree() -> Result<()> {
    use crate::runtime::resident_input_evidence::InputEvidenceFamily;
    use crate::runtime_db::input_evidence_snapshot::native_birth::NativeInputWriteProof;
    let (_home, db) = fixture()?;
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    // Original material header (2), one native scope/address group (257).
    // There is no allowance for another decoded identity tree.
    let limits = (64 * 1024 * 1024, 259);
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        assert!(
            NativeInputWriteProof::prepare_tx(tx, &scope, &ledger, limits, None, |visit| visit(
                InputEvidenceFamily::Transcript,
                "native-scope-read"
            ),)
            .is_err()
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let proof = NativeInputWriteProof::prepare_with_scope_read_tx(
            tx,
            &scope,
            &ledger,
            limits,
            None,
            Some(&read),
            |visit| visit(InputEvidenceFamily::Transcript, "native-scope-read"),
        )?
        .expect("installed native source");
        assert!(ledger.report()?.observed_bytes > 0);
        drop(proof);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        tx.execute(
            "UPDATE agent_states SET control_revision=control_revision+1",
            [],
        )?;
        assert!(NativeInputWriteProof::prepare_with_scope_read_tx(
            tx,
            &scope,
            &ledger,
            limits,
            None,
            Some(&read),
            |visit| visit(InputEvidenceFamily::Transcript, "native-scope-drift"),
        )
        .is_err());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn native_birth_accepts_initial_state_absence_without_qualifying_full_context() -> Result<()> {
    use crate::runtime::resident_input_evidence::InputEvidenceFamily;
    use crate::runtime_db::input_evidence_snapshot::native_birth::NativeInputWriteProof;
    let (_home, db) = fixture()?;
    let (_other_home, other) = fixture()?;
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let limits = (64 * 1024 * 1024, 259);
    db.transaction(|tx| {
        tx.execute(
            "CREATE TEMP TABLE initial_state AS SELECT * FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id],
        )?;
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id],
        )?;
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        assert!(read.facts().state.is_none());
        assert!(read.validate(tx, &scope).is_err());
        let proof = NativeInputWriteProof::prepare_with_scope_read_tx(
            tx,
            &scope,
            &ledger,
            limits,
            None,
            Some(&read),
            |visit| visit(InputEvidenceFamily::Transcript, "native-first-state"),
        )?
        .expect("active identity permits native birth before State");
        assert!(ledger.report()?.observed_bytes > 0);
        drop(proof);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let foreign = InputEvidenceScope {
            identity_incarnation: scope.identity_incarnation + 1,
            ..scope.clone()
        };
        assert!(read.validate_identity_scope(tx, &foreign).is_err());
        other.transaction(|other_tx| {
            assert!(read.validate_identity_scope(other_tx, &scope).is_err());
            Ok(())
        })?;
        tx.execute_batch("INSERT INTO agent_states SELECT * FROM initial_state")?;
        assert!(NativeInputWriteProof::prepare_with_scope_read_tx(
            tx,
            &scope,
            &ledger,
            limits,
            None,
            Some(&read),
            |visit| visit(InputEvidenceFamily::Transcript, "native-first-state-drift"),
        )
        .is_err());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        tx.execute_batch("DROP TABLE initial_state")?;
        Ok(())
    })?;
    Ok(())
}

#[test]
fn canonical_state_direct_decode_admits_raw_and_nodes_at_both_original_bounds() -> Result<()> {
    let (_home, db) = fixture()?;
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let (raw, nodes): (u64, usize) = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB))+256,(SELECT COUNT(*) FROM json_tree(payload_json)) FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let header = ResourceReadMaterial::new(&ledger, &scope)?;
        let (header_bytes, header_records) = header.finish()?;
        drop(header);
        // Independent scalar oracle: one raw JSON plus one owning State tree.
        let bytes = usize::try_from(header_bytes)?
            + 8 * (usize::try_from(raw)? + nodes * std::mem::size_of::<serde_json::Value>());
        let records = header_records + 8 * (1 + nodes);
        for (byte_limit, record_limit, succeeds) in [
            (bytes, records, true),
            (bytes - 1, records, false),
            (bytes, records - 1, false),
        ] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger, &scope, u64::try_from(byte_limit)?, record_limit,
            )?;
            {
                let _entry = phase.enter();
                let read = ContextDurableRead::new(tx, &scope.agent_id);
                let result = read.state_facts();
                if succeeds {
                    assert_eq!(result?.0.as_ref().unwrap().id, scope.agent_id);
                    assert_eq!(phase.finish()?, (u64::try_from(bytes)?, records));
                } else {
                    let error = result.unwrap_err();
                    assert!(format!("{error:#}").contains("ResourceExhausted"), "{error:#}");
                    assert!(phase.finish().is_err());
                    assert!(read.state_facts().is_err(), "failure remains latched");
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    })?;
    Ok(())
}

#[test]
fn context_scope_read_reuses_original_decode_without_more_material() -> Result<()> {
    let (_home, db) = fixture()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        let _entry = phase.enter();
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        let original = read.facts().state.as_ref().unwrap().id.as_ptr();
        let used = phase.finish()?;
        for _ in 0..3 {
            read.validate(tx, &scope)?;
            assert_eq!(read.facts().state.as_ref().unwrap().id.as_ptr(), original);
            assert_eq!(phase.finish()?, used);
        }
        let facts = read.into_facts(tx, &scope)?;
        assert_eq!(facts.state.as_ref().unwrap().id.as_ptr(), original);
        assert_eq!(phase.finish()?, used);
        Ok(())
    })?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn context_scope_read_rejects_foreign_transaction_and_incarnation() -> Result<()> {
    let (_home, db) = fixture()?;
    let (_other_home, other) = fixture()?;
    let scope = scope();
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        other.transaction(|other_tx| {
            assert_eq!(
                read.validate(other_tx, &scope).unwrap_err().to_string(),
                "context scope read belongs to another transaction or scope"
            );
            Ok(())
        })?;
        let mut foreign = scope.clone();
        foreign.identity_incarnation += 1;
        assert_eq!(
            read.validate(tx, &foreign).unwrap_err().to_string(),
            "context scope read belongs to another transaction or scope"
        );
        Ok(())
    })
}

#[test]
fn context_scope_read_rejects_next_transaction_on_same_connection() -> Result<()> {
    let (_home, db) = fixture()?;
    let connection = db.connection()?;
    let scope = scope();
    let original = connection.unchecked_transaction()?;
    let read = ContextDurableRead::new(&original, &scope.agent_id).scope_read()?;
    original.execute_batch("COMMIT")?;
    let next = connection.unchecked_transaction()?;
    assert_eq!(
        read.validate(&next, &scope).unwrap_err().to_string(),
        "context scope read belongs to another transaction or scope"
    );
    next.rollback()?;
    Ok(())
}

#[test]
fn context_scope_read_rejects_canonical_drift_and_normalized_mismatch() -> Result<()> {
    let scope = scope();
    for sql in [
        "UPDATE agent_states SET control_revision=control_revision+1",
        "UPDATE agent_states SET payload_json='not JSON'",
        "UPDATE agent_identities SET payload_json='not JSON'",
        "UPDATE agent_identities SET status='deleting'",
    ] {
        let (_home, db) = fixture()?;
        db.transaction(|tx| {
            let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
            tx.execute(sql, [])?;
            assert_eq!(
                read.validate(tx, &scope).unwrap_err().to_string(),
                "context scope changed after its validated read"
            );
            Ok(())
        })?;
    }
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        tx.execute("UPDATE agent_identities SET status='deleting'", [])?;
        let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
        assert_eq!(
            read.validate(tx, &scope).unwrap_err().to_string(),
            "context identity normalized/payload projection mismatch"
        );
        Ok(())
    })
}

#[test]
fn canonical_state_decode_reuses_body_at_identity_only_limits_and_keeps_original_guard(
) -> Result<()> {
    let (_home, db) = fixture()?;
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let source = ResourceReadMaterial::new(&ledger, &scope)?;
    let proof = db.transaction(|tx| {
        source.read(|| {
            ContextDurableRead::new(tx, &scope.agent_id)
                .scope_read()?
                .decoded_state()
                .ok_or_else(|| anyhow::anyhow!("canonical decode missing"))
        })
    })?;
    drop(source);
    assert!(ledger.report()?.observed_bytes > 0);
    let identity = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        identity.read(|| ContextDurableRead::new(tx, &scope.agent_id).identity_fact())
    })?;
    let (bytes, records) = identity.finish()?;
    drop(identity);
    let exact = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
    let retained = db.transaction(|tx| {
        exact.read(|| {
            let read = ContextDurableRead::new(tx, &scope.agent_id)
                .scope_read_using_decode(Some(&proof))?;
            read.validate(tx, &scope)?;
            assert!(Arc::ptr_eq(
                read.facts().state.as_ref().unwrap(),
                &proof.state
            ));
            assert_eq!(exact.finish()?, (bytes, records));
            let retained = read.decoded_state().unwrap();
            tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1",
                [],
            )?;
            assert!(read.validate(tx, &scope).is_err());
            Ok(retained)
        })
    })?;
    drop((exact, proof));
    assert!(ledger.report()?.observed_bytes > 0);
    drop(retained);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn canonical_state_decode_never_substitutes_for_changed_rows_or_scope() -> Result<()> {
    for sql in [
        "UPDATE agent_states SET control_revision=control_revision+1",
        "UPDATE agent_states SET payload_json=' ' || payload_json",
        "UPDATE agent_states SET payload_json='not JSON'",
        "UPDATE agent_states SET payload_json=json_set(payload_json,'$.id','foreign')",
        "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2)",
    ] {
        let (_home, db) = fixture()?;
        let scope = scope();
        let proof = db.transaction(|tx| {
            Ok(ContextDurableRead::new(tx, &scope.agent_id)
                .scope_read()?
                .decoded_state()
                .unwrap())
        })?;
        db.transaction(|tx| {
            tx.execute(sql, [])?;
            let durable = ContextDurableRead::new(tx, &scope.agent_id);
            let baseline = durable.scope_read();
            let reused = durable.scope_read_using_decode(Some(&proof));
            match (baseline, reused) {
                (Ok(expected), Ok(actual)) => {
                    assert_eq!(actual.facts().state, expected.facts().state, "{sql}");
                    assert_eq!(actual.facts().identity, expected.facts().identity, "{sql}");
                    assert_eq!(
                        actual.facts().control_revision,
                        expected.facts().control_revision
                    );
                    assert!(
                        !Arc::ptr_eq(actual.facts().state.as_ref().unwrap(), &proof.state),
                        "{sql}"
                    );
                }
                (Err(expected), Err(actual)) => {
                    assert_eq!(format!("{actual:#}"), format!("{expected:#}"), "{sql}")
                }
                _ => panic!("canonical decode behavior changed: {sql}"),
            }
            Ok(())
        })?;
    }
    Ok(())
}
