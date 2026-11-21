use super::*;
use crate::{
    domain::execution_protocol::*,
    types::{AgentStatus, WorkItemExecutionBinding},
};

fn successor_binding(id: &str) -> WorkItemExecutionBinding {
    WorkItemExecutionBinding {
        activation_id: Some(id.into()),
        admission_provenance: None,
        source_message_id: "claim-source".into(),
        turn_id: "successor-turn".into(),
        owner: Some(TurnOwner::Conversation {
            interaction_id: "claim-source".into(),
        }),
        work_item_id: None,
        claimed_work_revision: None,
    }
}

fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    ExecutionAttempt,
    ExecutionAttempt,
)> {
    let (dir, db) = fresh()?;
    let mut first = claim_only_attempt_with_hint(&db, None, Some(2))?;
    interrupt_claim(&db, first.clone())?;
    first.state = ExecutionAttemptState::Interrupted;
    first.terminal_outcome_id = Some("claim-interruption".into());
    first.terminal_at = Some(time().to_rfc3339());
    let mut next = first.clone();
    next.attempt_id = "successor-attempt".into();
    next.recovery_of_attempt_id = Some(first.attempt_id.clone());
    next.admitted_fences.work_item_generation = Some(3);
    next.terminal_outcome_id = Some("successor-outcome".into());
    db.transaction(|tx| {
        let source = ContextDurableRead::new(tx, "agent-a")
            .message_by_id("claim-source")?
            .context("fixture source")?;
        let mut turn = TurnRecord::new("agent-a", "successor-turn", 1);
        turn.owner = successor_binding(&next.attempt_id).owner;
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
        turn.input_message_ids = vec![source.id.clone()];
        turn.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::DeferredToFallback,
            reason: None,
            no_brief_reason: None,
            completed_at: time(),
            duration_ms: 1,
        });
        // No replay, run ID, or preallocated Turn hint: only activation→Turn.
        assert!(turn.replay.is_none());
        assert!(next.run_id.is_none() && next.turn_id.is_none());
        upsert_turn_record_tx(tx, &turn)?;
        store_attempt(tx, &next)?;
        let outcome = ExecutionOutcomeRecord {
            outcome_id: "successor-outcome".into(),
            attempt_id: next.attempt_id.clone(),
            outcome: ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                reason: "provider fallback".into(),
            }),
            created_at: time().to_rfc3339(),
        };
        tx.execute(
            "INSERT INTO execution_protocol_outcomes(agent_id,outcome_id,attempt_id,payload_json) VALUES(?1,?2,?3,?4)",
            params!["agent-a", outcome.outcome_id, outcome.attempt_id, serde_json::to_string(&outcome)?],
        )?;
        let mut state = AgentState::new("agent-a");
        state.status = AgentStatus::AwakeRunning;
        state.current_execution_binding = Some(successor_binding(&next.attempt_id));
        upsert_agent_state_tx(tx, &state)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[
                CanonicalInputEvidence::Turn(turn),
                CanonicalInputEvidence::Attempt(next.clone()),
                CanonicalInputEvidence::Outcome(outcome),
            ],
        )?;
        assert!(attempt_turn::for_current(tx, &scope(), &binding_on(tx, &scope())?, &first)?
            .turn_keys.is_empty());
        Ok(())
    })?;
    Ok((dir, db, first, next))
}

fn clear_binding(tx: &Transaction<'_>) -> Result<()> {
    let mut state = AgentState::new("agent-a");
    state.status = AgentStatus::Stopped;
    upsert_agent_state_tx(tx, &state)?;
    Ok(())
}

#[test]
fn claim_only_successor_assignment_stays_exact_after_cold_read() -> Result<()> {
    let (_dir, db, first, next) = fixture()?;
    let incoming = db.transaction(|tx| {
        clear_binding(tx)?;
        let (incoming, _) = append_message_tx(
            tx,
            &provider_lineage_recovery("after-successor", "claim-source", "successor-turn"),
        )?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(incoming.clone())],
        )?;
        Ok(incoming)
    })?;
    for prepare in [true, false, false] {
        let snapshot = current(db.transaction(|tx| {
            let wanted = provider_lineage_request(&incoming);
            if prepare {
                prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted)
            } else {
                load_input_evidence_snapshot_tx(tx, &scope(), &wanted)
            }
        })?);
        let captured = snapshot.resident.capture()?;
        let old = captured
            .lookup(&Key::new(Family::Attempt, &first.attempt_id))?
            .unwrap();
        let successor = captured
            .lookup(&Key::new(Family::Attempt, &next.attempt_id))?
            .unwrap();
        assert_eq!(old.attempt_turn_keys, Some(BTreeSet::new()));
        assert_eq!(
            successor.attempt_turn_keys,
            Some(BTreeSet::from([Key::new(Family::Turn, "successor-turn")]))
        );
        assert!(captured.recovery_source_absences.is_empty());
        let candidates = captured
            .records
            .iter()
            .filter(|r| {
                r.locator.key.family == Family::Attempt
                    && r.attempt_turn_keys.as_ref().is_some_and(|keys| {
                        keys.contains(&Key::new(Family::Turn, "successor-turn"))
                    })
            })
            .count();
        assert_eq!(candidates, 1);
    }
    Ok(())
}

#[test]
fn claim_only_successor_exclusion_requires_exact_canonical_evidence() -> Result<()> {
    for change in [
        "parent",
        "source",
        "agent",
        "binding",
        "provenance",
        "proof",
        "activation",
        "namespace",
        "retired-namespace",
        "missing-turn",
        "turn-agent",
        "fork",
        "prior-source",
        "prior-fence",
    ] {
        let (_dir, db, mut first, mut next) = fixture()?;
        db.transaction(|tx| {
            if change != "activation" {
                clear_binding(tx)?;
            }
            match change {
                "parent" => next.recovery_of_attempt_id = Some("unrelated".into()),
                "source" => next.source.generation += 1,
                "agent" => next.agent_id = "other-agent".into(),
                "binding" => next.binding = ExecutionBinding::AgentLifecycle { agent_id: "agent-a".into() },
                "provenance" => next.provenance.correlation_id = Some("unrelated".into()),
                "proof" => {
                    let mut proof: serde_json::Value = serde_json::from_str(&tx.query_row(
                        "SELECT attempt_turn_proof_json FROM input_evidence_current WHERE record_id=?1",
                        [&next.attempt_id], |r| r.get::<_, String>(0),
                    )?)?;
                    proof["fences"]["host_registry_revision"] = serde_json::json!(900);
                    tx.execute("UPDATE input_evidence_current SET attempt_turn_proof_json=?1 WHERE record_id=?2",
                        params![serde_json::to_string(&proof)?, next.attempt_id])?;
                }
                "activation" => {
                    let mut state = AgentState::new("agent-a");
                    let mut binding = successor_binding("unrelated");
                    binding.activation_id = Some("unrelated".into());
                    state.current_execution_binding = Some(binding);
                    upsert_agent_state_tx(tx, &state)?;
                }
                "namespace" => {
                    tx.execute("UPDATE input_evidence_current SET identity_incarnation=2 WHERE record_id=?1",
                        [&next.attempt_id])?;
                }
                "retired-namespace" => {
                    let mut identity = ContextDurableRead::new(tx, "agent-a").identity_fact()?.unwrap();
                    identity.incarnation = 2;
                    upsert_agent_identity_tx(tx, &identity)?;
                }
                "missing-turn" => { tx.execute("DELETE FROM turn_records WHERE turn_id='successor-turn'", [])?; }
                "turn-agent" => {
                    let payload: String = tx.query_row("SELECT payload_json FROM turn_records WHERE turn_id='successor-turn'", [], |r| r.get(0))?;
                    let mut turn: TurnRecord = serde_json::from_str(&payload)?;
                    turn.agent_id = "other-agent".into();
                    tx.execute("UPDATE turn_records SET payload_json=?1 WHERE turn_id='successor-turn'", [serde_json::to_string(&turn)?])?;
                }
                "fork" => {
                    let mut fork = next.clone();
                    fork.attempt_id = "fork".into();
                    fork.terminal_outcome_id = Some("fork-outcome".into());
                    store_attempt(tx, &fork)?;
                    let outcome = ExecutionOutcomeRecord {
                        outcome_id: "fork-outcome".into(),
                        attempt_id: fork.attempt_id.clone(),
                        outcome: ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                            reason: "provider fallback".into(),
                        }),
                        created_at: time().to_rfc3339(),
                    };
                    tx.execute(
                        "INSERT INTO execution_protocol_outcomes(agent_id,outcome_id,attempt_id,payload_json) VALUES(?1,?2,?3,?4)",
                        params![fork.agent_id, outcome.outcome_id, outcome.attempt_id, serde_json::to_string(&outcome)?],
                    )?;
                }
                "prior-source" => first.source.generation += 1,
                "prior-fence" => first.admitted_fences.host_registry_revision += 1,
                _ => unreachable!(),
            }
            if matches!(change, "parent" | "source" | "agent" | "binding" | "provenance") {
                tx.execute("UPDATE execution_protocol_attempts SET payload_json=?1 WHERE attempt_id=?2",
                    params![serde_json::to_string(&next)?, next.attempt_id])?;
            }
            let aliases: i64 = tx.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r.get(0))?;
            let result = attempt_turn::for_current(tx, &scope(), &binding_on(tx, &scope())?, &first);
            match result {
                Ok(proof) => {
                    assert!(!matches!(change, "proof" | "retired-namespace" | "missing-turn" | "turn-agent" | "prior-source" | "prior-fence"));
                    assert!(proof.turn_keys.contains(&Key::new(Family::Turn, "successor-turn")), "{change}");
                }
                Err(error) => assert!(
                    matches!(change, "agent" | "proof" | "retired-namespace" | "missing-turn" | "turn-agent" | "prior-source" | "prior-fence"),
                    "{change}: {error:#}"
                ),
            }
            assert_eq!(aliases, tx.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r.get::<_, i64>(0))?);
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn claim_only_successor_never_erases_started_or_first_capture_evidence() -> Result<()> {
    for change in ["run", "hint", "binding", "nonempty-prior", "first-capture"] {
        let (_dir, db, mut first, _next) = fixture()?;
        db.transaction(|tx| {
            let mut prior = alias_on(tx, &scope(), &Key::new(Family::Attempt, &first.attempt_id))?
                .unwrap()
                .attempt_turn_proof
                .unwrap();
            match change {
                "run" => first.run_id = Some("started-run".into()),
                "hint" => first.turn_id = Some("successor-turn".into()),
                "binding" => {
                    let mut state = AgentState::new("agent-a");
                    state.current_execution_binding = Some(successor_binding(&first.attempt_id));
                    upsert_agent_state_tx(tx, &state)?;
                }
                "nonempty-prior" => {
                    prior
                        .turn_keys
                        .insert(Key::new(Family::Turn, "successor-turn"));
                }
                "first-capture" => {}
                _ => unreachable!(),
            }
            let proof = attempt_turn::observe(
                tx,
                Some(&scope()),
                &first,
                (change != "first-capture").then_some(&prior),
            )?;
            assert!(
                proof
                    .turn_keys
                    .contains(&Key::new(Family::Turn, "successor-turn")),
                "{change}"
            );
            Ok(())
        })?;
    }
    Ok(())
}
