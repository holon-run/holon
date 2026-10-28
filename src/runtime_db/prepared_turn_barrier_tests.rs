use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;

fn start_turn(f: &Fixture, qualified: bool) -> Result<TurnRecord> {
    let mut record = TurnRecord::new(&f.scope.agent_id, "barrier-turn", 1);
    record.run_id = Some("barrier-run".into());
    record.created_at = f.at;
    f.db.transaction(|tx| {
        let mut state = AgentState::new(&f.scope.agent_id);
        state.turn_index = record.turn_index;
        state.current_turn_id = Some(record.turn_id.clone());
        state.current_run_id = record.run_id.clone();
        upsert_agent_state_tx(tx, &state)?;
        if !qualified {
            upsert_turn_record_tx(tx, &record)?;
        }
        Ok(())
    })?;
    if qualified {
        let prepared = PreparedInputEvidence::turn(
            f.scope.clone(),
            "start-barrier-turn".into(),
            f.at,
            record.clone(),
        )?;
        f.db.evidence().commit_prepared_input_evidence(&prepared)?;
    }
    Ok(record)
}

fn prepare(f: &Fixture, prior: &TurnRecord) -> Result<PreparedInputEvidence> {
    PreparedInputEvidence::turn_barrier(
        f.scope.clone(),
        "turn-barrier-original".into(),
        f.at,
        prior,
        true,
        &InputEvidenceBudgetLedger::new(Default::default())?,
    )
}

#[test]
fn turn_barrier_command_admits_before_digest_and_retains_original_material() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;

    let f = Fixture::new()?;
    let mut prior = start_turn(&f, true)?;
    prior.tool_execution_ids = vec!["quoted \\\"{}[]:,".repeat(256); 8];
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &f.scope)?;
    let before = phase.finish()?;
    let prepared = phase.read(|| {
        PreparedInputEvidence::turn_barrier(
            f.scope.clone(),
            "admitted-barrier".into(),
            f.at,
            &prior,
            true,
            &ledger,
        )
    })?;
    let after = phase.finish()?;
    assert!(after.0 > before.0 && after.1 > before.1);
    let retained = prepared.original_material.as_ref().unwrap().clone();
    drop(phase);
    drop(prepared);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(retained);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    for limits in [(before.0, 65_536), (64 * 1024 * 1024, before.1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &f.scope, limits.0, limits.1)?;
        let error = phase
            .read(|| {
                PreparedInputEvidence::turn_barrier(
                    f.scope.clone(),
                    "refused-barrier".into(),
                    f.at,
                    &prior,
                    true,
                    &ledger,
                )
            })
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("ResourceExhausted"),
            "{error:#}"
        );
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn turn_barrier_updates_only_flag_and_replays_original_after_later_turn_write() -> Result<()> {
    let f = Fixture::new()?;
    let prior = start_turn(&f, true)?;
    let prepared = prepare(&f, &prior)?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("first barrier must commit")
    };
    assert!(matches!(
        value.record,
        PreparedEvidenceValue::TurnBarrierDone
    ));
    let mut expected = prior.clone();
    expected.external_side_effect_possible = true;
    let actual =
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap();
    assert_eq!(actual, expected);
    assert!(value.input_delta.query_advance.is_some());
    drop(value);
    expected.tool_execution_ids.push("later-ack".into());
    let later = PreparedInputEvidence::turn(
        f.scope.clone(),
        "later-turn-write".into(),
        f.at,
        expected.clone(),
    )?;
    f.db.evidence().commit_prepared_input_evidence(&later)?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = f.db.evidence().commit_prepared_input_evidence(&prepared)?
    else {
        panic!("exact original must replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        expected,
    );
    Ok(())
}

#[test]
fn turn_barrier_rejects_changed_qualified_prior_raw_body_and_lost_current_turn() -> Result<()> {
    for mode in ["changed", "raw", "not_current"] {
        let f = Fixture::new()?;
        let prior = start_turn(&f, mode != "raw")?;
        let prepared = prepare(&f, &prior)?;
        if mode == "changed" {
            let mut changed = prior.clone();
            changed.tool_execution_ids.push("concurrent-ack".into());
            let command = PreparedInputEvidence::turn(
                f.scope.clone(),
                "concurrent-turn".into(),
                f.at,
                changed,
            )?;
            f.db.evidence().commit_prepared_input_evidence(&command)?;
        } else if mode == "not_current" {
            f.db.transaction(|tx| {
                let mut state = AgentState::new(&f.scope.agent_id);
                state.turn_index = 2;
                upsert_agent_state_tx(tx, &state)
            })?;
        }
        let error =
            f.db.evidence()
                .commit_prepared_input_evidence(&prepared)
                .expect_err("barrier must not grant or overwrite prior evidence");
        let message = format!("{error:#}");
        assert!(
            if mode == "changed" {
                message.contains("Turn barrier prior changed")
            } else if mode == "not_current" {
                message.contains("Turn barrier is no longer current")
            } else {
                matches!(
                    error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
                    Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(id)
                        | crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(id))
                        if id == &prior.turn_id
                )
            },
            "{mode}: {message}",
        );
        assert!(
            !f.db
                .turn_records()
                .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
                .unwrap()
                .external_side_effect_possible
        );
        assert!(f
            .db
            .transaction(|tx| lookup_input_mutation_receipt_tx(tx, prepared.descriptor()))?
            .is_none());
    }
    Ok(())
}

#[test]
fn turn_barrier_sql_failure_rolls_back_native_qualification_and_original_receipt() -> Result<()> {
    let f = Fixture::new()?;
    let prior = start_turn(&f, true)?;
    let prepared = prepare(&f, &prior)?;
    f.db.transaction(|tx| {
        tx.execute_batch("CREATE TRIGGER reject_turn_barrier BEFORE UPDATE ON turn_records BEGIN SELECT RAISE(ABORT,'barrier write rejected'); END;")?;
        Ok(())
    })?;
    let error =
        f.db.evidence()
            .commit_prepared_input_evidence(&prepared)
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("barrier write rejected"),
        "{error:#}"
    );
    assert_eq!(
        f.db.turn_records()
            .by_id(Some(&f.scope.agent_id), &prior.turn_id)?
            .unwrap(),
        prior
    );
    f.db.transaction(|tx| {
        assert!(lookup_input_mutation_receipt_tx(tx, prepared.descriptor())?.is_none());
        validate_existing_input_evidence_tx(
            tx,
            &f.scope,
            &InputEvidenceKey::new(Family::Turn, &prior.turn_id),
        )?;
        tx.execute_batch("DROP TRIGGER reject_turn_barrier")?;
        Ok(())
    })?;
    assert!(matches!(
        f.db.evidence().commit_prepared_input_evidence(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}
