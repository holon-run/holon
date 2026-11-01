use super::*;
use crate::{
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    types::{IdleInputBatchItem, IdleInputBatchRecord},
};

fn prior_turn(f: &Fixture, qualified: bool) -> Result<TurnRecord> {
    let mut prior = TurnRecord::new(&f.scope.agent_id, "idle-recovered-turn", 1);
    prior.run_id = Some("idle-recovered-run".into());
    prior.created_at = f.at;
    f.db.transaction(|tx| {
        let mut state = AgentState::new(&f.scope.agent_id);
        state.turn_index = prior.turn_index;
        state.current_turn_id = Some(prior.turn_id.clone());
        state.current_run_id = prior.run_id.clone();
        upsert_agent_state_tx(tx, &state)?;
        if !qualified {
            upsert_turn_record_tx(tx, &prior)?;
        }
        Ok(())
    })?;
    if qualified {
        let prepared = PreparedInputEvidence::turn(
            f.scope.clone(),
            "original-idle-turn".into(),
            f.at,
            prior.clone(),
        )?;
        f.db.evidence().commit_prepared_input_evidence(&prepared)?;
    }
    Ok(prior)
}

fn recovered_turn(prior: &TurnRecord, first: usize, count: usize) -> TurnRecord {
    let items: Vec<_> = (first..first + count)
        .map(|index| IdleInputBatchItem {
            message_id: format!("result-{index:03}"),
            source_ref: format!("task:original-{index:03}"),
            rendered: format!("结果-{index:03}: quoted \\\"{{}}[],:\\n"),
        })
        .collect();
    let mut record = prior.clone();
    record.input_message_ids = items.iter().map(|item| item.message_id.clone()).collect();
    record.idle_input_batch = Some(IdleInputBatchRecord {
        anchor_message_id: items[0].message_id.clone(),
        activation_id: Some("original-idle-activation".into()),
        sender_agent_id: "runtime".into(),
        cutoff: prior.created_at,
        max_items: 8,
        max_bytes: 32 * 1024,
        rendered_bytes: items.iter().map(|item| item.rendered.len()).sum(),
        items,
    });
    record
}

fn prepare(
    f: &Fixture,
    prior: &TurnRecord,
    record: TurnRecord,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputEvidence> {
    PreparedInputEvidence::turn_idle_batch(
        f.scope.clone(),
        "original-idle-recovery".into(),
        f.at,
        prior,
        record,
        ledger,
    )
}

#[test]
fn idle_recovery_digest_keeps_original_shape_and_fits_64_result_phase_remainder() -> Result<()> {
    let f = Fixture::new()?;
    let prior = prior_turn(&f, true)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    // The frozen 64-result recovery failed at 61,336 records / 20,483,336
    // bytes. Keep that original used allowance, including its eightfold gates.
    let used = (20_483_336, 61_336);
    for count in [1usize, 2, 8, 9, 64] {
        let mut seen = std::collections::BTreeSet::new();
        for first in (0..count).step_by(8) {
            let record = recovered_turn(&prior, first, (count - first).min(8));
            seen.extend(record.input_message_ids.iter().cloned());
            let phase = ResourceReadMaterial::after(&ledger, &f.scope, used)?;
            let before = phase.finish()?;
            let prepared = phase.read(|| prepare(&f, &prior, record, &ledger))?;
            let after = phase.finish()?;
            assert!(after.0 > before.0 && after.1 > before.1);
            assert!(used.0 + after.0 <= 64 * 1024 * 1024);
            assert!(used.1 + after.1 <= 65_536);
            assert_eq!(
                prepared.descriptor().command_digest(),
                canonical_command_digest(prepared.command.as_ref())?,
            );
            let EvidenceCommand::TurnIdleBatch { prior_digest, .. } = prepared.command.as_ref()
            else {
                panic!("must retain the original full idle command")
            };
            assert_eq!(*prior_digest, canonical_command_digest(&prior)?);
            let retained = prepared.original_material(&ledger)?.unwrap();
            assert!(prepared
                .original_material(&InputEvidenceBudgetLedger::new(Default::default())?)
                .is_err());
            drop((phase, prepared));
            assert!(ledger.report()?.observed_bytes > 0);
            drop(retained);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        assert_eq!(seen.len(), count);
    }
    let record = recovered_turn(&prior, 0, 8);
    let command = EvidenceCommand::TurnIdleBatch {
        record,
        prior_digest: canonical_command_digest(&prior)?,
    };
    let phase = ResourceReadMaterial::after(&ledger, &f.scope, used)?;
    let error = phase
        .read(|| crate::diagnostics::resource_io::admit_serialized_value(&command))
        .unwrap_err();
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    assert!(phase.finish().is_err());
    Ok(())
}

#[test]
fn idle_recovery_constructor_keeps_exact_byte_record_limits_and_latched_rejection() -> Result<()> {
    let f = Fixture::new()?;
    let prior = prior_turn(&f, true)?;
    let record = recovered_turn(&prior, 0, 8);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &f.scope)?;
    let candidate = record.clone();
    let prepared = phase.read(|| prepare(&f, &prior, candidate, &ledger))?;
    let required = phase.finish()?;
    let descriptor = prepared.descriptor().clone();
    drop((phase, prepared));
    assert_eq!(ledger.report()?.observed_bytes, 0);

    for (bytes, records, accepted) in [
        (required.0, required.1, true),
        (required.0 - 1, required.1, false),
        (required.0, required.1 - 1, false),
    ] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &f.scope, bytes, records)?;
        let candidate = record.clone();
        let result = phase.read(|| prepare(&f, &prior, candidate, &ledger));
        if accepted {
            let prepared = result?;
            assert_eq!(phase.finish()?, required);
            assert!(prepared.original_material(&ledger)?.is_some());
            drop(prepared);
        } else {
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            assert!(phase.finish().is_err());
            let candidate = record.clone();
            {
                let _entry = phase.enter();
                assert!(charge_owned_metadata(0, 0).is_err());
                assert!(prepare(&f, &prior, candidate, &ledger).is_err());
            }
            assert!(f
                .db
                .transaction(|tx| lookup_input_mutation_receipt_tx(tx, &descriptor))?
                .is_none());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn idle_recovery_replays_original_receipt_after_later_turn_write() -> Result<()> {
    let f = Fixture::new()?;
    let prior = prior_turn(&f, true)?;
    let record = recovered_turn(&prior, 0, 8);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let prepared = prepare(&f, &prior, record.clone(), &ledger)?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("idle recovery must commit once")
    };
    assert!(matches!(
        value.record,
        PreparedEvidenceValue::TurnBarrierDone
    ));
    assert!(value.input_delta.query_advance.is_some());
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        record,
    );
    drop(value);
    let mut later = record;
    later.tool_execution_ids.push("later-qualified-tool".into());
    let later_command = PreparedInputEvidence::turn(
        f.scope.clone(),
        "later-idle-turn".into(),
        f.at,
        later.clone(),
    )?;
    f.db.evidence()
        .commit_prepared_input_evidence(&later_command)?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("must replay the original receipt before rechecking its prior")
    };
    assert_eq!(replay, receipt);
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        later,
    );
    Ok(())
}

#[test]
fn idle_recovery_rejects_scope_prior_and_current_turn_drift_without_receipts() -> Result<()> {
    for mode in ["scope", "prior", "not_current", "raw"] {
        let f = Fixture::new()?;
        let prior = prior_turn(&f, mode != "raw")?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let prepared = prepare(&f, &prior, recovered_turn(&prior, 0, 8), &ledger)?;
        let mut expected = prior.clone();
        match mode {
            "scope" => f.db.transaction(|tx| {
                let mut identity: AgentIdentityRecord = serde_json::from_str(&tx.query_row(
                    "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                    [&f.scope.agent_id],
                    |row| row.get::<_, String>(0),
                )?)?;
                identity.incarnation += 1;
                upsert_agent_identity_tx(tx, &identity)
            })?,
            "prior" => {
                expected
                    .tool_execution_ids
                    .push("concurrent-qualified-tool".into());
                let changed = PreparedInputEvidence::turn(
                    f.scope.clone(),
                    "changed-idle-prior".into(),
                    f.at,
                    expected.clone(),
                )?;
                f.db.evidence().commit_prepared_input_evidence(&changed)?;
            }
            "not_current" => f.db.transaction(|tx| {
                let mut state = AgentState::new(&f.scope.agent_id);
                state.turn_index = prior.turn_index + 1;
                upsert_agent_state_tx(tx, &state)
            })?,
            _ => {}
        }
        let error =
            f.db.evidence()
                .commit_prepared_input_evidence(&prepared)
                .unwrap_err();
        let message = format!("{error:#}");
        match mode {
            "scope" => assert!(message.contains("identity incarnation is no longer current")),
            "prior" => assert!(message.contains("Turn barrier prior changed")),
            "not_current" => assert!(message.contains("Turn barrier is no longer current")),
            _ => assert!(matches!(
                error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
                Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(id)
                    | crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(id))
                    if id == &prior.turn_id
            )),
        }
        assert_eq!(
            f.db.turn_records()
                .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
                .unwrap(),
            expected,
        );
        assert!(f
            .db
            .transaction(|tx| lookup_input_mutation_receipt_tx(tx, prepared.descriptor()))?
            .is_none());
    }
    Ok(())
}

#[test]
fn idle_recovery_receipt_failure_rolls_back_turn_and_native_qualification() -> Result<()> {
    let f = Fixture::new()?;
    let prior = prior_turn(&f, true)?;
    let record = recovered_turn(&prior, 0, 8);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let prepared = prepare(&f, &prior, record.clone(), &ledger)?;
    f.db.connection()?.execute_batch(
        "CREATE TRIGGER reject_idle_receipt BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'idle receipt rejected'); END;",
    )?;
    let error =
        f.db.evidence()
            .commit_prepared_input_evidence(&prepared)
            .unwrap_err();
    assert!(format!("{error:#}").contains("idle receipt rejected"));
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        prior,
    );
    f.db.transaction(|tx| {
        assert!(lookup_input_mutation_receipt_tx(tx, prepared.descriptor())?.is_none());
        validate_existing_input_evidence_tx(
            tx,
            &f.scope,
            &InputEvidenceKey::new(Family::Turn, &prior.turn_id),
        )?;
        tx.execute_batch("DROP TRIGGER reject_idle_receipt")?;
        Ok(())
    })?;
    assert!(matches!(
        f.db.evidence().commit_prepared_input_evidence(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        record,
    );
    Ok(())
}
