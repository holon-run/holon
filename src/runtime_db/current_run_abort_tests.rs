use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::{
    current_run_abort::CurrentRunAbortIntent,
    evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
    prepared_input_evidence::PreparedInputEvidence,
    repositories::{upsert_queue_entry_tx, upsert_turn_record_tx},
    tests::test_support::TempRuntimeDb,
    transitions::{
        execution_protocol_repository::{
            persist_execution_commands_tx, validate_execution_commands_tx,
        },
        AgentPostureTransitionCommand, AgentStateMutation, ExecutionProtocolTransition,
        InputMutationExecution, PreparedInputTransition, QueueMutation, QueueOperation,
        QueueTransitionCommand, TransitionCommit, TransitionFaultPoint,
        TurnTerminalTransitionCommand,
    },
    RuntimeDb, RuntimeStateTransitionConflict,
};
use crate::{
    domain::execution_protocol::{
        AdmitExecution, ExecutionAttempt, ExecutionAttemptState, ExecutionProtocolCommand,
        ExecutionProtocolState,
    },
    runtime::resident_input_evidence::InputEvidenceScope,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentStatus, AgentVisibility, AuthorityClass, MessageBody, MessageEnvelope, MessageKind,
        MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus, TurnRecord, TurnTerminalKind,
        TurnTerminalRecord,
    },
};

struct Fixture {
    runtime: TempRuntimeDb,
    scope: InputEvidenceScope,
    at: DateTime<Utc>,
    original: AgentState,
    source: QueueEntryRecord,
    intent: CurrentRunAbortIntent,
}

impl Fixture {
    fn new() -> Result<Self> {
        Self::new_with_turn(false)
    }

    fn new_with_turn(qualify_turn: bool) -> Result<Self> {
        let runtime = TempRuntimeDb::new()?;
        let scope = InputEvidenceScope {
            agent_id: "abort-db".into(),
            identity_incarnation: 1,
        };
        let at = "2026-11-15T00:00:00Z".parse()?;
        let mut original = AgentState::new(&scope.agent_id);
        original.status = AgentStatus::AwakeRunning;
        original.current_run_id = Some("original-run".into());
        original.current_turn_id = Some("original-turn".into());
        original.turn_index = 1;
        let source = QueueEntryRecord {
            agent_id: scope.agent_id.clone(),
            message_id: "original-source".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Dequeued,
            created_at: at,
            updated_at: at,
        };
        let intent = CurrentRunAbortIntent {
            agent_id: scope.agent_id.clone(),
            identity_incarnation: 1,
            owner_epoch: 1,
            run_id: "original-run".into(),
            runtime_lease_id: "original-lease".into(),
            activation_id: None,
            source_message_id: source.message_id.clone(),
            turn_id: original.current_turn_id.clone(),
            stop_after_abort: true,
            requested_at: at,
        };
        let f = Self {
            runtime,
            scope,
            at,
            original,
            source,
            intent,
        };
        f.db().transaction(|tx| {
            upsert_agent_identity_tx(
                tx,
                &AgentIdentityRecord::new(
                    &f.scope.agent_id,
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
            )?;
            upsert_agent_state_tx(tx, &f.original)?;
            upsert_queue_entry_tx(tx, &f.source)?;
            append_message_tx(tx, &f.message(&f.source.message_id))?;
            if !qualify_turn {
                upsert_turn_record_tx(tx, &f.turn())?;
            }
            Ok(())
        })?;
        if qualify_turn {
            f.db()
                .evidence()
                .commit_prepared_input_evidence(&PreparedInputEvidence::turn(
                    f.scope.clone(),
                    "qualify-original-turn".into(),
                    f.at,
                    f.turn(),
                )?)?;
        }
        Ok(f)
    }

    fn db(&self) -> &RuntimeDb {
        &self.runtime.db
    }

    fn message(&self, id: &str) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            &self.scope.agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "identical prompt is not source identity".into(),
            },
        );
        message.id = id.into();
        message.created_at = self.at;
        message
    }

    fn turn(&self) -> TurnRecord {
        let mut turn = TurnRecord::new(&self.scope.agent_id, "original-turn", 1);
        turn.run_id = self.original.current_run_id.clone();
        turn.created_at = self.at;
        turn.input_message_ids = vec![self.source.message_id.clone()];
        turn
    }

    fn prepared(
        &self,
        nonce: &str,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<PreparedInputTransition> {
        let mut stopped = self.original.clone();
        stopped.status = AgentStatus::Stopped;
        stopped.current_run_id = None;
        stopped.current_turn_id = None;
        PreparedInputTransition::current_run_abort(
            self.scope.clone(),
            nonce.into(),
            self.at,
            AgentPostureTransitionCommand {
                agent_id: self.scope.agent_id.clone(),
                expected_incarnation: Some(1),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(self.original.clone())),
                    record: Box::new(stopped),
                },
                audit_events: Vec::new(),
                fault,
            },
            self.intent.clone(),
        )
    }

    fn commit(
        &self,
        prepared: &PreparedInputTransition,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        self.db()
            .transitions_in_scope(&self.scope)
            .commit_prepared_transition(prepared)
    }

    fn state(&self) -> Result<AgentState> {
        self.db()
            .agent_states()
            .latest(&self.scope.agent_id)?
            .context("fixture State missing")
    }

    fn count(&self, table: &str) -> Result<i64> {
        Ok(self.db().connection()?.query_row(
            &format!("SELECT COUNT(*) FROM {table}"),
            [],
            |row| row.get(0),
        )?)
    }

    fn queue(&self, source: &str) -> Result<QueueEntryRecord> {
        let payload: String = self.db().connection()?.query_row(
            "SELECT payload_json FROM queue_entries WHERE agent_id=?1 AND message_id=?2",
            params![self.scope.agent_id, source],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&payload)?)
    }

    fn assert_not_committed(&self) -> Result<()> {
        assert_eq!(self.state()?, self.original);
        assert_eq!(self.count("current_run_abort_intents")?, 0);
        assert_eq!(self.count("input_mutation_receipts")?, 0);
        assert!(!self
            .db()
            .operator_aborted_input(&self.scope.agent_id, &self.source.message_id)?);
        Ok(())
    }

    fn attempt(&self, id: &str, source: &str, run: &str) -> Result<ExecutionAttempt> {
        Ok(serde_json::from_value(serde_json::json!({
            "attempt_id": id,
            "agent_id": self.scope.agent_id,
            "source_message_id": source,
            "source": {
                "identity": {"kind": "queue_message", "message_id": source},
                "generation": 1
            },
            "binding": {"kind": "agent_lifecycle", "agent_id": self.scope.agent_id},
            "provenance": {
                "origin": "system", "trust": "runtime_instruction", "priority": "normal"
            },
            "admitted_fences": {
                "source_revision": 1, "agent_control_revision": 1, "host_registry_revision": 1
            },
            "state": "interrupted",
            "run_id": run,
            "turn_id": "original-turn",
            "admitted_at": self.at.to_rfc3339(),
            "terminal_at": self.at.to_rfc3339()
        }))?)
    }

    fn store_attempt(&self, attempt: &ExecutionAttempt) -> Result<()> {
        self.db().transaction(|tx| {
            tx.execute(
                "INSERT INTO execution_protocol_attempts(
                   agent_id,attempt_id,lifecycle_state,source_identity_json,
                   source_generation,recovery_of_attempt_id,terminal_outcome_id,payload_json
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    attempt.agent_id,
                    attempt.attempt_id,
                    serde_json::to_value(attempt.state)?.as_str().unwrap(),
                    serde_json::to_string(&attempt.source.identity)?,
                    attempt.source.generation,
                    attempt.recovery_of_attempt_id,
                    attempt.terminal_outcome_id,
                    serde_json::to_string(attempt)?,
                ],
            )?;
            Ok(())
        })
    }

    fn queue_command(&self, source: &str, status: QueueEntryStatus) -> QueueTransitionCommand {
        let mut record = self.source.clone();
        record.message_id = source.into();
        record.status = status;
        QueueTransitionCommand {
            agent_id: self.scope.agent_id.clone(),
            operation: QueueOperation::Claim,
            mutation: QueueMutation::Consume(record),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: Vec::new(),
        }
    }
}

#[test]
fn abort_accepts_the_open_admission_before_settlement_attaches_its_run() -> Result<()> {
    let mut f = Fixture::new()?;
    let mut attempt = f.attempt("original-activation", "original-source", "original-run")?;
    attempt.state = ExecutionAttemptState::Open;
    attempt.run_id = None;
    attempt.turn_id = None;
    attempt.terminal_at = None;
    f.store_attempt(&attempt)?;
    f.intent.activation_id = Some(attempt.attempt_id);

    f.commit(&f.prepared("abort-open-admission", None)?)?;
    assert_eq!(f.count("current_run_abort_intents")?, 1);
    assert_eq!(f.state()?.status, AgentStatus::Stopped);
    Ok(())
}

#[test]
fn abort_intent_and_posture_sql_failures_roll_back_without_ack_or_fence() -> Result<()> {
    for trigger in [
        "CREATE TRIGGER reject_abort BEFORE INSERT ON current_run_abort_intents
         BEGIN SELECT RAISE(ABORT,'abort intent rejected'); END;",
        "CREATE TRIGGER reject_abort BEFORE UPDATE ON agent_states
         BEGIN SELECT RAISE(ABORT,'abort posture rejected'); END;",
        "CREATE TRIGGER reject_abort BEFORE INSERT ON input_mutation_receipts
         BEGIN SELECT RAISE(ABORT,'abort receipt rejected'); END;",
    ] {
        let f = Fixture::new()?;
        f.db().transaction(|tx| {
            tx.execute_batch(trigger)?;
            Ok(())
        })?;
        let prepared = f.prepared("atomic-abort", None)?;
        let error = f.commit(&prepared).unwrap_err();
        assert!(format!("{error:#}").contains("rejected"), "{error:#}");
        f.assert_not_committed()?;
        assert_eq!(f.queue(&f.source.message_id)?, f.source);
        f.db().transaction(|tx| {
            tx.execute_batch("DROP TRIGGER reject_abort")?;
            Ok(())
        })?;
        assert!(matches!(
            f.commit(&prepared)?,
            InputMutationExecution::Committed { .. }
        ));
        assert_eq!(f.count("current_run_abort_intents")?, 1);
        assert_eq!(f.state()?.status, AgentStatus::Stopped);
    }
    Ok(())
}

#[test]
fn abort_transition_faults_roll_back_intent_and_posture_together() -> Result<()> {
    for fault in [
        TransitionFaultPoint::AfterValidation,
        TransitionFaultPoint::AfterCanonicalWrites,
        TransitionFaultPoint::AfterAuditWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        let f = Fixture::new()?;
        let error = f
            .commit(&f.prepared("faulted-abort", Some(fault))?)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("injected runtime transition fault"),
            "{error:#}"
        );
        f.assert_not_committed()?;
    }
    Ok(())
}

#[test]
fn abort_exact_retry_replays_but_duplicate_nonce_cannot_overwrite_intent() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared("original-abort", None)?;
    assert!(matches!(
        f.commit(&prepared)?,
        InputMutationExecution::Committed { .. }
    ));
    let stopped = f.state()?;
    assert!(matches!(
        f.commit(&prepared)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert!(f
        .commit(&f.prepared("different-abort-command", None)?)
        .is_err());
    assert_eq!(f.count("current_run_abort_intents")?, 1);
    assert_eq!(f.count("input_mutation_receipts")?, 1);
    assert_eq!(f.state()?, stopped);
    Ok(())
}

#[test]
fn stale_abort_rolls_back_intent_and_preserves_replacement_run() -> Result<()> {
    let f = Fixture::new()?;
    let prepared = f.prepared("stale-abort", None)?;
    let mut replacement = f.original.clone();
    replacement.current_run_id = Some("replacement-run".into());
    f.db()
        .transaction(|tx| upsert_agent_state_tx(tx, &replacement))?;
    assert!(f.commit(&prepared).is_err());
    assert_eq!(f.state()?, replacement);
    assert_eq!(f.count("current_run_abort_intents")?, 0);
    assert_eq!(f.count("input_mutation_receipts")?, 0);
    Ok(())
}

#[test]
fn abort_cannot_overwrite_a_previously_completed_source_or_attempt() -> Result<()> {
    for completed_attempt in [false, true] {
        let mut f = Fixture::new()?;
        if completed_attempt {
            let mut attempt =
                f.attempt("completed-attempt", &f.source.message_id, "original-run")?;
            attempt.state = ExecutionAttemptState::Settled;
            attempt.terminal_outcome_id = Some("completed-outcome".into());
            f.store_attempt(&attempt)?;
            f.intent.activation_id = Some(attempt.attempt_id);
        } else {
            let mut completed = f.source.clone();
            completed.status = QueueEntryStatus::Processed;
            f.db()
                .transaction(|tx| upsert_queue_entry_tx(tx, &completed))?;
        }
        let queue_before = f.queue(&f.source.message_id)?;
        let attempt_before = f
            .db()
            .transitions()
            .execution_attempt_by_id(&f.scope.agent_id, "completed-attempt")?;
        assert!(f.commit(&f.prepared("too-late-abort", None)?).is_err());
        f.assert_not_committed()?;
        assert_eq!(f.queue(&f.source.message_id)?, queue_before);
        assert_eq!(
            f.db()
                .transitions()
                .execution_attempt_by_id(&f.scope.agent_id, "completed-attempt")?,
            attempt_before
        );
    }
    Ok(())
}

#[test]
fn committed_abort_rejects_original_source_claim_but_not_independent_crash_input() -> Result<()> {
    let f = Fixture::new()?;
    f.commit(&f.prepared("committed-abort", None)?)?;
    for (source, aborted) in [("original-source", true), ("independent-source", false)] {
        let mut interrupted = f.source.clone();
        interrupted.message_id = source.into();
        interrupted.status = QueueEntryStatus::Interrupted;
        f.db().transaction(|tx| {
            upsert_queue_entry_tx(tx, &interrupted)?;
            append_message_tx(tx, &f.message(source))
        })?;
        let result = f
            .db()
            .transitions()
            .commit_queue(&f.queue_command(source, QueueEntryStatus::Dequeued));
        if aborted {
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains("operator-aborted input cannot be claimed"),
                "{error:#}"
            );
            assert_eq!(f.queue(source)?, interrupted);
        } else {
            assert!(result?.applied);
            assert_eq!(f.queue(source)?.status, QueueEntryStatus::Dequeued);
        }
    }
    Ok(())
}

#[test]
fn abort_source_fence_covers_attached_run_not_same_prompt_or_another_run() -> Result<()> {
    let f = Fixture::new()?;
    for (id, source, run) in [
        ("live-original", "live-source", "original-run"),
        ("independent", "same-text-other-source", "another-run"),
    ] {
        f.store_attempt(&f.attempt(id, source, run)?)?;
    }
    f.commit(&f.prepared("committed-abort", None)?)?;
    for (source, expected) in [
        ("original-source", true),
        ("live-source", true),
        ("same-text-other-source", false),
        ("fresh-source", false),
    ] {
        assert_eq!(
            f.db().operator_aborted_input(&f.scope.agent_id, source)?,
            expected,
            "{source}"
        );
    }
    for source in ["original-source", "live-source"] {
        let mut attempt = f.attempt("new-replay-attempt", source, "replay-run")?;
        attempt.state = ExecutionAttemptState::Open;
        attempt.run_id = None;
        attempt.turn_id = None;
        attempt.terminal_at = None;
        let protocol = ExecutionProtocolTransition {
            bootstrap: Some(ExecutionProtocolState::empty(&f.scope.agent_id)),
            commands: vec![ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
                attempt,
            }))],
        };
        let error = f
            .db()
            .transaction(|tx| {
                let prepared = validate_execution_commands_tx(
                    tx,
                    &f.scope.agent_id,
                    protocol.bootstrap.as_ref(),
                    &protocol.commands,
                    &[],
                    &[],
                    &[],
                )?;
                persist_execution_commands_tx(tx, prepared)?;
                Ok(())
            })
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("operator-aborted input cannot be admitted"),
            "{error:#}"
        );
        assert!(f
            .db()
            .transitions()
            .execution_attempt_by_id(&f.scope.agent_id, "new-replay-attempt")?
            .is_none());
    }
    Ok(())
}

#[test]
fn historical_abort_fence_requires_exact_source_terminal_kind_and_reason() -> Result<()> {
    for (kind, reason, expected) in [
        (
            TurnTerminalKind::Interrupted,
            Some("operator_aborted"),
            true,
        ),
        (
            TurnTerminalKind::Interrupted,
            Some("startup_recovery"),
            false,
        ),
        (
            TurnTerminalKind::Interrupted,
            Some("operator_aborted_by_external"),
            false,
        ),
        (TurnTerminalKind::Interrupted, None, false),
        (TurnTerminalKind::Completed, Some("operator_aborted"), false),
    ] {
        let f = Fixture::new()?;
        let mut historical = f.turn();
        historical.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(
            &TurnTerminalRecord {
                turn_id: historical.turn_id.clone(),
                turn_index: historical.turn_index,
                kind,
                reason: reason.map(str::to_owned),
                last_assistant_message: None,
                no_brief_reason: None,
                checkpoint: None,
                completed_at: f.at,
                duration_ms: 0,
            },
        ));
        f.db().transaction(|tx| {
            upsert_turn_record_tx(tx, &historical)?;
            append_message_tx(tx, &f.message("same-prompt-new-identity"))?;
            Ok(())
        })?;
        assert_eq!(
            f.db()
                .operator_aborted_input(&f.scope.agent_id, &f.source.message_id)?,
            expected,
            "{kind:?} {reason:?}"
        );
        assert!(!f
            .db()
            .operator_aborted_input(&f.scope.agent_id, "same-prompt-new-identity")?);
        assert_eq!(f.state()?, f.original);
        assert_eq!(f.queue(&f.source.message_id)?, f.source);
        assert_eq!(f.count("current_run_abort_intents")?, 0);
    }
    Ok(())
}

#[test]
fn abort_fences_late_terminal_even_when_producer_rebases_its_state() -> Result<()> {
    let f = Fixture::new_with_turn(true)?;
    let mut turn = f.turn();
    turn.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(
        &TurnTerminalRecord {
            turn_id: turn.turn_id.clone(),
            turn_index: turn.turn_index,
            kind: TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some("losing response".into()),
            no_brief_reason: None,
            checkpoint: None,
            completed_at: f.at,
            duration_ms: 0,
        },
    ));
    f.commit(&f.prepared("abort-before-rebase", None)?)?;
    let stopped = f.state()?;
    let late = PreparedInputTransition::terminal(
        f.scope.clone(),
        "rebased-late-completion".into(),
        f.at,
        TurnTerminalTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(stopped.clone())),
                record: Box::new(stopped.clone()),
            },
            turn_record: turn,
            terminal_tool_executions: Vec::new(),
            audit_events: Vec::new(),
            fault: None,
        },
    )?;
    let error = f.commit(&late).unwrap_err();
    assert!(
        format!("{error:#}").contains("operator-aborted input cannot commit"),
        "{error:#}"
    );
    assert_eq!(f.state()?, stopped);
    assert_eq!(
        f.queue("original-source")?.status,
        QueueEntryStatus::Dequeued
    );
    assert_eq!(f.count("input_mutation_receipts")?, 2);
    Ok(())
}

#[test]
fn abort_fences_prepared_queue_completion_and_provider_recovery() -> Result<()> {
    for (with_recovery, cancel_first) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let f = Fixture::new()?;
        let mut command = f.queue_command("original-source", QueueEntryStatus::Processed);
        command.operation = QueueOperation::Settle;
        let QueueMutation::Consume(record) = command.mutation else {
            unreachable!()
        };
        command.mutation = if with_recovery {
            let mut recovery = f.message("losing-provider-recovery").with_admission(
                crate::types::MessageDeliverySurface::RuntimeSystem,
                crate::types::AdmissionContext::RuntimeOwned,
            );
            recovery.kind = MessageKind::InternalFollowup;
            recovery.origin = MessageOrigin::System {
                subsystem: "model_lineage_recovery".into(),
            };
            recovery.authority_class = AuthorityClass::RuntimeInstruction;
            recovery.priority = Priority::Next;
            recovery.causation_id = Some(f.source.message_id.clone());
            recovery
                .source_refs
                .insert("source_message_id".into(), f.source.message_id.clone());
            recovery
                .source_refs
                .insert("source_turn_id".into(), "original-turn".into());
            recovery.metadata = Some(serde_json::json!({"provider_recovery": {
                "fallback_model_ref": "openai/gpt-5",
                "source_message_id": f.source.message_id,
                "source_turn_id": "original-turn",
                "source_terminal_kind": "deferred_to_fallback",
                "source_round": 1
            }}));
            let mut turn = f.turn();
            turn.terminal = Some(crate::types::TurnTerminalSummary {
                kind: TurnTerminalKind::DeferredToFallback,
                reason: None,
                no_brief_reason: None,
                completed_at: f.at,
                duration_ms: 0,
            });
            command.turn_record = Some(turn);
            command.message_evidence.push(recovery.clone());
            QueueMutation::SettleWithProviderRecovery {
                record,
                recovery: Box::new(recovery),
            }
        } else {
            QueueMutation::Upsert(record)
        };
        if cancel_first {
            f.commit(&f.prepared("abort-before-queue-completion", None)?)?;
            let error = f.db().transitions().commit_queue(&command).unwrap_err();
            assert!(
                format!("{error:#}").contains("operator-aborted input cannot commit"),
                "{error:#}"
            );
            assert_eq!(
                f.queue("original-source")?.status,
                QueueEntryStatus::Dequeued
            );
            assert_eq!(f.count("messages")?, 1);
            assert_eq!(f.count("current_run_abort_intents")?, 1);
        } else {
            assert!(f.db().transitions().commit_queue(&command)?.applied);
            assert_eq!(
                f.queue("original-source")?.status,
                QueueEntryStatus::Processed
            );
            assert_eq!(f.count("messages")?, if with_recovery { 2 } else { 1 });
        }
    }
    Ok(())
}

#[test]
fn abort_ack_fences_a_prepared_late_completion_from_restoring_running_state() -> Result<()> {
    let f = Fixture::new_with_turn(true)?;
    let mut completed_turn = f.turn();
    completed_turn.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(
        &TurnTerminalRecord {
            turn_id: completed_turn.turn_id.clone(),
            turn_index: completed_turn.turn_index,
            kind: TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some("late provider response".into()),
            no_brief_reason: None,
            checkpoint: None,
            completed_at: f.at,
            duration_ms: 0,
        },
    ));
    let mut completed_state = f.original.clone();
    completed_state.status = AgentStatus::AwakeIdle;
    completed_state.current_run_id = None;
    completed_state.current_turn_id = None;
    let late = PreparedInputTransition::terminal(
        f.scope.clone(),
        "late-producer-completion".into(),
        f.at,
        TurnTerminalTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(f.original.clone())),
                record: Box::new(completed_state),
            },
            turn_record: completed_turn,
            terminal_tool_executions: Vec::new(),
            audit_events: Vec::new(),
            fault: None,
        },
    )?;
    f.commit(&f.prepared("abort-before-completion", None)?)?;
    let stopped = f.state()?;
    let error = f.commit(&late).unwrap_err();
    assert!(
        error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some(),
        "{error:#}"
    );
    assert_eq!(f.state()?, stopped);
    assert_eq!(f.count("input_mutation_receipts")?, 2);
    assert_eq!(f.count("current_run_abort_intents")?, 1);
    assert!(f
        .db()
        .operator_aborted_input(&f.scope.agent_id, &f.source.message_id)?);
    Ok(())
}
