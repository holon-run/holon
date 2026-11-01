use super::*;
use crate::{
    domain::execution_protocol::*,
    types::{AgentStatus, TurnReplayProvenance, TurnTerminalKind, TurnTerminalSummary},
};

fn fixture() -> Result<(tempfile::TempDir, RuntimeDb, MessageEnvelope)> {
    let (directory, db) = fresh()?;
    let mut first = claim_only_attempt(&db)?;
    let original = db.transaction(|tx| {
        let source = ContextDurableRead::new(tx, "agent-a")
            .message_by_id("claim-source")?
            .context("fixture source")?;
        let mut turn = TurnRecord::new("agent-a", "claim-turn", 1);
        turn.owner = Some(TurnOwner::Conversation {
            interaction_id: source.id.clone(),
        });
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
        turn.input_message_ids = vec![source.id.clone()];
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Interrupted,
            reason: None,
            no_brief_reason: None,
            completed_at: time(),
            duration_ms: 1,
        });
        upsert_turn_record_tx(tx, &turn)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn.clone())],
        )?;
        Ok(turn)
    })?;
    interrupt_claim(&db, first.clone())?;
    let incoming = db.transaction(|tx| {
        let mut replay_turn = original.clone();
        replay_turn.turn_id = "claim-replayed-turn".into();
        replay_turn.turn_index = 2;
        replay_turn.terminal.as_mut().unwrap().kind = TurnTerminalKind::DeferredToFallback;
        replay_turn.replay = Some(TurnReplayProvenance {
            source_message_id: "claim-source".into(),
            source_turn_id: original.turn_id.clone(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: original.terminal.clone(),
        });
        upsert_turn_record_tx(tx, &replay_turn)?;
        first.attempt_id = "claim-replayed-attempt".into();
        first.state = ExecutionAttemptState::Interrupted;
        first.recovery_of_attempt_id = Some("claim-attempt".into());
        first.terminal_outcome_id = Some("claim-replayed-outcome".into());
        first.terminal_at = Some(time().to_rfc3339());
        let outcome = ExecutionOutcomeRecord {
            outcome_id: "claim-replayed-outcome".into(),
            attempt_id: first.attempt_id.clone(),
            outcome: ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                reason: "provider failure after replay".into(),
            }),
            created_at: time().to_rfc3339(),
        };
        store_attempt(tx, &first)?;
        tx.execute(
            "INSERT INTO execution_protocol_outcomes(agent_id,outcome_id,attempt_id,payload_json) VALUES(?1,?2,?3,?4)",
            params!["agent-a", outcome.outcome_id, outcome.attempt_id, serde_json::to_string(&outcome)?],
        )?;
        let mut state = AgentState::new("agent-a");
        state.status = AgentStatus::AwakeRunning;
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some(first.attempt_id.clone()),
            admission_provenance: None,
            source_message_id: "claim-source".into(),
            turn_id: replay_turn.turn_id.clone(),
            owner: replay_turn.owner.clone(),
            work_item_id: None,
            claimed_work_revision: None,
        });
        upsert_agent_state_tx(tx, &state)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[
                CanonicalInputEvidence::Turn(replay_turn),
                CanonicalInputEvidence::Attempt(first.clone()),
                CanonicalInputEvidence::Outcome(outcome),
            ],
        )?;
        // The terminal writer retained the observed activation→Turn proof.
        // No live binding is available to reconstruct it on the cold read.
        state.current_execution_binding = None;
        state.status = AgentStatus::Stopped;
        upsert_agent_state_tx(tx, &state)?;
        let incoming =
            provider_lineage_recovery("after-replay", "claim-source", "claim-replayed-turn");
        let (incoming, _) = append_message_tx(tx, &incoming)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(incoming.clone())],
        )?;
        Ok(incoming)
    })?;
    Ok((directory, db, incoming))
}

#[test]
fn provider_lineage_accepts_exact_interrupted_replay_without_reassigning_input() -> Result<()> {
    let (_directory, db, incoming) = fixture()?;
    let wanted = provider_lineage_request(&incoming);
    for prepare in [true, false] {
        let snapshot = current(db.transaction(|tx| {
            if prepare {
                prepare_input_evidence_snapshot_tx(tx, &scope(), &wanted)
            } else {
                load_input_evidence_snapshot_tx(tx, &scope(), &wanted)
            }
        })?);
        let captured = snapshot.resident.capture()?;
        assert_eq!(
            captured.message_turn_keys["claim-source"].as_deref(),
            Some("claim-turn")
        );
        for id in ["claim-attempt", "claim-replayed-attempt"] {
            let record = captured.lookup(&Key::new(Family::Attempt, id))?.unwrap();
            let PreparedInputEvidence::Attempt(attempt) = &record.record else {
                unreachable!()
            };
            assert_eq!(attempt.turn_id.as_deref(), Some("claim-turn"));
        }
        assert!(captured
            .lookup(&Key::new(Family::Attempt, "claim-replayed-attempt"))?
            .unwrap()
            .attempt_turn_keys
            .as_ref()
            .unwrap()
            .contains(&Key::new(Family::Turn, "claim-replayed-turn")));
        assert!(captured.recovery_source_absences.is_empty());
    }
    Ok(())
}

#[test]
fn provider_lineage_replay_rejects_incomplete_canonical_proofs() -> Result<()> {
    let (_directory, db, incoming) = fixture()?;
    let snapshot = current(db.transaction(|tx| {
        prepare_input_evidence_snapshot_tx(tx, &scope(), &provider_lineage_request(&incoming))
    })?);
    let captured = snapshot.resident.capture()?;
    let baseline = captured
        .records
        .iter()
        .map(|record| (record.locator.key.clone(), record.clone()))
        .collect::<BTreeMap<_, _>>();
    for change in [
        "assignment",
        "agent",
        "owner",
        "trigger",
        "membership",
        "prior_terminal",
        "reason",
        "source_message",
        "predecessor",
        "source",
        "binding",
        "provenance",
        "activation_turn",
        "second_replay",
        "outcome",
        "missing_body",
    ] {
        let mut records = baseline.clone();
        let assigned = if change == "assignment" {
            "claim-replayed-turn"
        } else {
            "claim-turn"
        };
        if change == "second_replay" {
            let mut ancestor = baseline[&Key::new(Family::Attempt, "claim-attempt")]
                .as_ref()
                .clone();
            ancestor.locator.key.id = "claim-ancestor".into();
            let PreparedInputEvidence::Attempt(attempt) = &mut ancestor.record else {
                unreachable!()
            };
            attempt.attempt_id = "claim-ancestor".into();
            records.insert(ancestor.locator.key.clone(), Arc::new(ancestor));
        }
        let turn_key = Key::new(Family::Turn, "claim-turn");
        if ["agent", "owner", "trigger", "membership", "prior_terminal"].contains(&change) {
            let PreparedInputEvidence::Turn(turn) =
                &mut Arc::make_mut(records.get_mut(&turn_key).unwrap()).record
            else {
                unreachable!()
            };
            match change {
                "agent" => turn.agent_id = "agent-b".into(),
                "owner" => turn.owner = Some(TurnOwner::Command),
                "trigger" => turn.trigger = None,
                "membership" => turn.input_message_ids.clear(),
                "prior_terminal" => {
                    turn.terminal.as_mut().unwrap().kind = TurnTerminalKind::Completed
                }
                _ => unreachable!(),
            }
        }
        if ["reason", "source_message"].contains(&change) {
            let PreparedInputEvidence::Turn(turn) = &mut Arc::make_mut(
                records
                    .get_mut(&Key::new(Family::Turn, "claim-replayed-turn"))
                    .unwrap(),
            )
            .record
            else {
                unreachable!()
            };
            let replay = turn.replay.as_mut().unwrap();
            if change == "reason" {
                replay.reason = "unproven_retry".into();
            } else {
                replay.source_message_id = "another-input".into();
            }
        }
        if [
            "predecessor",
            "source",
            "binding",
            "provenance",
            "second_replay",
        ]
        .contains(&change)
        {
            let PreparedInputEvidence::Attempt(attempt) = &mut Arc::make_mut(
                records
                    .get_mut(&Key::new(Family::Attempt, "claim-attempt"))
                    .unwrap(),
            )
            .record
            else {
                unreachable!()
            };
            match change {
                "predecessor" => attempt.state = ExecutionAttemptState::Settled,
                "source" => attempt.source.generation += 1,
                "binding" => attempt.binding = ExecutionBinding::Command,
                "provenance" => attempt.provenance.correlation_id = Some("changed".into()),
                "second_replay" => attempt.recovery_of_attempt_id = Some("claim-ancestor".into()),
                _ => unreachable!(),
            }
        }
        if change == "activation_turn" {
            Arc::make_mut(
                records
                    .get_mut(&Key::new(Family::Attempt, "claim-replayed-attempt"))
                    .unwrap(),
            )
            .attempt_turn_keys
            .as_mut()
            .unwrap()
            .remove(&Key::new(Family::Turn, "claim-replayed-turn"));
        }
        if change == "outcome" {
            let PreparedInputEvidence::Outcome(outcome) = &mut Arc::make_mut(
                records
                    .get_mut(&Key::new(Family::Outcome, "claim-replayed-outcome"))
                    .unwrap(),
            )
            .record
            else {
                unreachable!()
            };
            outcome.attempt_id = "another-attempt".into();
        }
        if change == "missing_body" {
            records.remove(&Key::new(Family::Attempt, "claim-attempt"));
        }
        assert!(
            provider_replay::validate(
                &records,
                &captured.execution,
                "claim-source",
                "claim-replayed-turn",
                assigned,
            )
            .is_err(),
            "{change}"
        );
    }
    Ok(())
}
