//! Live recovery uses the original fence and publishes one whole-input ACK.
use super::*;
use crate::domain::{
    execution_protocol::{
        AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionOrigin,
        ExecutionPriority, ExecutionProtocolState, ExecutionProvenance, ExecutionSource,
        ExecutionSourceIdentity, ExecutionTrust,
    },
    scheduler::SchedulerScenarioClass,
};
use crate::types::{
    AgentStatus, ExecutionAdmissionProvenance, LiveInputBatchRecord, LiveInputSafePoint, TurnOwner,
    TurnReplayProvenance, TurnTerminalKind, TurnTerminalSummary, TurnTriggerSummary,
    WorkItemExecutionBinding,
};

struct RecoveryFixture {
    f: Fixture,
    attempt: ExecutionAttempt,
    turn: TurnRecord,
    predecessor: TurnRecord,
}

impl RecoveryFixture {
    fn new() -> Result<Self> {
        let f = Fixture::new()?;
        let source = f.queue()?.command.message_evidence.remove(0);
        let source =
            f.db.messages()
                .append_with_index_changes_in_scope(&f.scope, &source, &[])?
                .record;
        let mut attempt = ExecutionAttempt {
            attempt_id: "live:successor".into(),
            agent_id: f.scope.agent_id.clone(),
            source_message_id: Some(source.id.clone()),
            input_projection: None,
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: source.id.clone(),
                },
                generation: source.message_seq.context("source sequence missing")?,
            },
            binding: ExecutionBinding::Command,
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::Operator,
                trust: ExecutionTrust::OperatorInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: source.message_seq.unwrap(),
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: Some("live:turn".into()),
            recovery_of_attempt_id: Some("live:interrupted".into()),
            terminal_outcome_id: None,
            admitted_at: f.at.to_rfc3339(),
            terminal_at: None,
        };
        let mut turn = TurnRecord::new(&f.scope.agent_id, "live:turn", 2);
        turn.owner = Some(TurnOwner::Command);
        turn.trigger = Some(TurnTriggerSummary::from_message(&source));
        turn.input_message_ids.push(source.id.clone());
        turn.replay = Some(TurnReplayProvenance {
            source_message_id: source.id.clone(),
            source_turn_id: "live:predecessor".into(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: None,
        });
        let mut predecessor = TurnRecord::new(&f.scope.agent_id, "live:predecessor", 1);
        predecessor.owner = turn.owner.clone();
        predecessor.trigger = turn.trigger.clone();
        predecessor.input_message_ids = turn.input_message_ids.clone();
        predecessor.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Interrupted,
            reason: None,
            no_brief_reason: None,
            completed_at: f.at,
            duration_ms: 0,
        });
        // An empty ledger keeps this fixture focused on the recovery fence and
        // whole transaction ACK, not the separate Result source contracts.
        predecessor.live_input_batches.push(LiveInputBatchRecord {
            activation_id: "live:interrupted".into(),
            safe_point: LiveInputSafePoint::new("live:interrupted", 2),
            cutoff: f.at,
            max_items: 8,
            max_bytes: 24 * 1024,
            rendered_bytes: LiveInputBatchRecord::HEADER.len(),
            result_identities: Vec::new(),
            ordinary_message_ids: Vec::new(),
            items: Vec::new(),
        });
        let mut state = f.state()?;
        f.db.transaction(|tx| {
            attempt.admitted_fences.host_registry_revision =
                authority_fences_tx(tx, &f.scope.agent_id)?.host_registry_revision;
            let mut prior = attempt.clone();
            prior.attempt_id = "live:interrupted".into();
            prior.turn_id = Some(predecessor.turn_id.clone());
            prior.recovery_of_attempt_id = None;
            prior.state = ExecutionAttemptState::Interrupted;
            prior.terminal_at = Some(f.at.to_rfc3339());
            let mut protocol = ExecutionProtocolState::empty(&f.scope.agent_id);
            protocol
                .attempts
                .insert(prior.attempt_id.clone(), prior.clone());
            protocol
                .attempts
                .insert(attempt.attempt_id.clone(), attempt.clone());
            persist_state_tx(tx, &protocol)?;
            upsert_turn_record_tx(tx, &predecessor)?;
            upsert_turn_record_tx(tx, &turn)?;
            state.status = AgentStatus::AwakeRunning;
            state.turn_index = turn.turn_index;
            state.current_turn_id = Some(turn.turn_id.clone());
            state.current_execution_binding = Some(WorkItemExecutionBinding {
                activation_id: Some(attempt.attempt_id.clone()),
                admission_provenance: Some(ExecutionAdmissionProvenance::Canonical {
                    scenario_class: SchedulerScenarioClass::ExplicitlyBoundOperatorInput,
                    activation_id: attempt.attempt_id.clone(),
                }),
                source_message_id: source.id.clone(),
                turn_id: turn.turn_id.clone(),
                owner: turn.owner.clone(),
                work_item_id: None,
                claimed_work_revision: None,
            });
            upsert_agent_state_tx(tx, &state)?;
            use crate::runtime_db::input_evidence_snapshot as input;
            input::register_committed_input_evidence_tx(
                tx,
                &f.scope,
                &[
                    input::CanonicalInputEvidence::Attempt(prior),
                    input::CanonicalInputEvidence::Attempt(attempt.clone()),
                    input::CanonicalInputEvidence::Turn(predecessor.clone()),
                    input::CanonicalInputEvidence::Turn(turn.clone()),
                ],
            )?;
            Ok(())
        })?;
        Ok(Self {
            f,
            attempt,
            turn,
            predecessor,
        })
    }

    fn command(&self, fault: Option<TransitionFaultPoint>) -> LiveInputRecoveryCommand {
        LiveInputRecoveryCommand {
            expected_attempt: self.attempt.clone(),
            expected_turn: self.turn.clone(),
            predecessor: self.predecessor.clone(),
            fault,
        }
    }

    fn freeze(&self, command: LiveInputRecoveryCommand) -> Result<PreparedInputTransition> {
        PreparedInputTransition::live_input_recovery(
            self.f.scope.clone(),
            "live-recovery".into(),
            self.f.at,
            command,
        )
    }
}

#[test]
fn prepared_live_input_recovery_installs_turn_input_task_and_current_acks_once() -> Result<()> {
    let f = RecoveryFixture::new()?;
    let prepared = f.freeze(f.command(None))?;
    let InputMutationExecution::Committed { value, receipt } =
        f.f.db
            .transitions_in_scope(&f.f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("original recovery ACK missing");
    };
    assert!(value.applied);
    let mut expected = f.turn.clone();
    expected.live_input_batches = f.predecessor.live_input_batches.clone();
    expected.live_input_batches[0].activation_id = f.attempt.attempt_id.clone();
    assert_eq!(value.effects.turns, vec![expected.clone()]);
    assert_eq!(
        f.f.db
            .turn_records()
            .by_id(Some(&f.f.scope.agent_id), &expected.turn_id)?,
        Some(expected)
    );
    assert!(value
        .effects
        .input_evidence_delta
        .records
        .iter()
        .any(|record| record.locator.key
            == crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                crate::runtime::resident_input_evidence::InputEvidenceFamily::Turn,
                &f.turn.turn_id
            )));
    assert_eq!(value.effects.task_delta.as_ref().unwrap().scope, f.f.scope);
    assert!(value.effects.current_read_writes.is_some());
    assert_eq!(f.f.receipt_count()?, 1);
    assert!(matches!(f.f.db.transitions_in_scope(&f.f.scope)
        .commit_prepared_transition(&prepared)?,
        InputMutationExecution::Replayed { receipt: replay, .. } if replay == receipt));
    Ok(())
}

#[test]
fn prepared_live_input_recovery_rolls_back_all_acks_with_canonical_failure() -> Result<()> {
    let f = RecoveryFixture::new()?;
    let prepared = f.freeze(f.command(Some(TransitionFaultPoint::AfterCanonicalWrites)))?;
    assert!(f
        .f
        .db
        .transitions_in_scope(&f.f.scope)
        .commit_prepared_transition(&prepared)
        .is_err());
    assert_eq!(
        f.f.db
            .turn_records()
            .by_id(Some(&f.f.scope.agent_id), &f.turn.turn_id)?,
        Some(f.turn.clone())
    );
    assert_eq!(f.f.receipt_count()?, 0);
    let original = f.f.db.transitions().commit_live_input_recovery(
        &f.attempt,
        &f.turn,
        &f.predecessor,
        None,
    )?;
    assert!(
        original.applied,
        "standalone test adapter uses the same original fence"
    );
    assert_eq!(original.effects.turns.len(), 1);
    Ok(())
}

#[test]
fn prepared_live_input_recovery_freezes_predecessor_and_rejects_foreign_namespaces() -> Result<()> {
    let f = RecoveryFixture::new()?;
    let original = f.freeze(f.command(None))?;
    let fault = f.freeze(f.command(Some(TransitionFaultPoint::BeforeCommit)))?;
    assert_eq!(original.descriptor(), fault.descriptor());
    let mut changed = f.command(None);
    changed.predecessor.live_input_batches[0].max_items -= 1;
    assert_ne!(
        original.descriptor().command_digest(),
        f.freeze(changed)?.descriptor().command_digest()
    );
    for field in 0..4 {
        let mut command = f.command(None);
        match field {
            0 => command.expected_attempt.agent_id = "foreign".into(),
            1 => command.expected_turn.agent_id = "foreign".into(),
            2 => command.predecessor.agent_id = "foreign".into(),
            _ => command.predecessor.turn_id.clear(),
        }
        assert!(f.freeze(command).is_err());
    }
    Ok(())
}
