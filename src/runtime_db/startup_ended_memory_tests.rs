use super::*;
use crate::{
    runtime_db::{
        context_inputs::ContextDurableRead,
        control_transition::{
            prepare_control, ControlCancellationKind, ControlCancellationTarget, ControlInvocation,
            ControlPlanInput, ControlSource,
        },
        dispatch_memory::{DispatchMemoryIntent, DispatchMemoryStart, DISPATCH_MEMORY_SCHEMA},
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
        repositories::{committed_turn_record_tx, upsert_queue_entry_tx, upsert_turn_record_tx},
        transitions::{AgentPostureTransitionCommand, AgentStateMutation, PreparedInputTransition},
        RuntimeDbLock,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentStatus, AgentVisibility, AuthorityClass, ClosureDecision, ClosureOutcome,
        ControlAction, MessageBody, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
        QueueEntryStatus, RuntimePosture, TurnRecord, TurnTriggerSummary, WorkItemExecutionBinding,
    },
};
struct Fixture {
    home: tempfile::TempDir,
    db: RuntimeDb,
    binding: DispatchMemoryBinding,
    source: MessageEnvelope,
    at: DateTime<Utc>,
}
impl Fixture {
    fn new() -> Result<Self> {
        Self::with_replay(false)
    }

    fn with_replay(replay: bool) -> Result<Self> {
        Self::with_source_turn(replay, None)
    }

    fn with_source_turn(replay: bool, ingress: Option<&str>) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "startup-ended".into(),
            identity_incarnation: 1,
        };
        let at: DateTime<Utc> = "2026-10-13T00:00:00Z".parse()?;
        let (start, source, turn) = db.transaction(|tx| {
            tx.execute_batch(DISPATCH_MEMORY_SCHEMA)?;
            let identity = AgentIdentityRecord::new(
                &scope.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            upsert_agent_identity_tx(tx, &identity)?;
            let mut source = MessageEnvelope::new(
                &scope.agent_id,
                MessageKind::Control,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "start".into(),
                },
            );
            source.id = "startup-ended-source".into();
            source.created_at = at;
            source.turn_id = ingress.map(str::to_owned);
            let (source, inserted) = append_message_tx(tx, &source)?;
            assert!(inserted);
            let mut turn = TurnRecord::new(&scope.agent_id, "startup-ended-turn", 1);
            turn.run_id = Some("startup-ended-run".into());
            turn.created_at = at;
            turn.input_message_ids = vec![source.id.clone()];
            turn.trigger = Some(TurnTriggerSummary::from_message(&source));
            if replay {
                install_source_replay_lineage(tx, &source, &mut turn, at)?;
            }
            upsert_turn_record_tx(tx, &turn)?;
            let turn = committed_turn_record_tx(tx, &scope.agent_id, &turn.turn_id)?.record;
            let mut state = AgentState::new(&scope.agent_id);
            state.status = AgentStatus::AwakeRunning;
            state.turn_index = 1;
            state.total_message_count = 1;
            state.current_turn_id = Some(turn.turn_id.clone());
            state.current_run_id = turn.run_id.clone();
            state.current_execution_binding = Some(WorkItemExecutionBinding {
                activation_id: None,
                admission_provenance: None,
                source_message_id: source.id.clone(),
                turn_id: turn.turn_id.clone(),
                owner: None,
                work_item_id: None,
                claimed_work_revision: None,
            });
            upsert_agent_state_tx(tx, &state)?;
            upsert_queue_entry_tx(
                tx,
                &QueueEntryRecord {
                    message_id: source.id.clone(),
                    agent_id: scope.agent_id.clone(),
                    priority: source.priority.clone(),
                    status: QueueEntryStatus::Dequeued,
                    created_at: at,
                    updated_at: at,
                },
            )?;
            register_committed_input_evidence_tx(
                tx,
                &scope,
                &[
                    CanonicalInputEvidence::Message(source.clone()),
                    CanonicalInputEvidence::Turn(turn.clone()),
                ],
            )?;
            let start = DispatchMemoryStart::new(
                EpisodeMemoryKey {
                    agent_id: scope.agent_id.clone(),
                    identity_incarnation: 1,
                    actual_dispatch_turn_id: turn.turn_id.clone(),
                },
                EpisodeMemoryInvocation {
                    message_id: source.id.clone(),
                    turn_index: 1,
                },
                &source,
                ClosureDecision {
                    outcome: ClosureOutcome::Completed,
                    waiting_reason: None,
                    work_signal: None,
                    runtime_posture: RuntimePosture::Awake,
                    evidence: vec![],
                },
                state.working_memory,
            )?;
            Ok((start, source, turn))
        })?;
        let state = db.agent_states().latest(&scope.agent_id)?.unwrap();
        let prepared = PreparedInputTransition::posture(
            scope.clone(),
            "startup-ended-actual-start".into(),
            at,
            AgentPostureTransitionCommand {
                agent_id: scope.agent_id.clone(),
                expected_incarnation: Some(1),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(state.clone())),
                    record: Box::new(state),
                },
                audit_events: vec![],
                fault: None,
            },
            Some(turn),
        )?
        .with_dispatch_memory(DispatchMemoryIntent::Started {
            start: Box::new(start.clone()),
        })?;
        db.transitions_in_scope(&scope)
            .commit_prepared_transition(&prepared)?;
        if replay {
            db.transaction(|tx| {
                tx.execute(
                    "UPDATE conversation_input_assignments SET turn_id='original-assignment' WHERE message_id=?1",
                    [&source.id],
                )?;
                Ok(())
            })?;
        }
        Ok(Self {
            home,
            db,
            binding: DispatchMemoryBinding::from_start(&start),
            source,
            at,
        })
    }
    fn admit_original_command(&self) -> Result<()> {
        self.admit_original_command_with_turn(Some(
            self.binding.key().actual_dispatch_turn_id.clone(),
        ))
    }

    fn admit_original_command_with_turn(&self, turn_id: Option<String>) -> Result<()> {
        use crate::domain::execution_protocol::{
            AdmitExecution, AdmittedFences, ExecutionOrigin, ExecutionPriority,
            ExecutionProtocolCommand, ExecutionProtocolState, ExecutionProvenance, ExecutionSource,
            ExecutionSourceIdentity, ExecutionTrust,
        };
        use crate::runtime_db::transitions::execution_protocol_repository as protocol;
        self.db.transaction(|tx| {
            let authority = protocol::authority_fences_tx(tx, &self.source.agent_id)?;
            let sequence = self.source.message_seq.unwrap();
            let attempt = ExecutionAttempt {
                attempt_id: format!("activation:message:{}", self.source.id),
                agent_id: self.source.agent_id.clone(),
                source_message_id: Some(self.source.id.clone()),
                source: ExecutionSource {
                    identity: ExecutionSourceIdentity::QueueMessage {
                        message_id: self.source.id.clone(),
                    },
                    generation: sequence,
                },
                binding: ExecutionBinding::AgentLifecycle {
                    agent_id: self.source.agent_id.clone(),
                },
                provenance: ExecutionProvenance {
                    origin: ExecutionOrigin::Operator,
                    trust: ExecutionTrust::OperatorInstruction,
                    priority: ExecutionPriority::Normal,
                    correlation_id: None,
                    causation_id: None,
                },
                admitted_fences: AdmittedFences {
                    source_revision: sequence,
                    work_item_source_revision: None,
                    work_item_generation: None,
                    rejoin: None,
                    agent_control_revision: authority.agent_control_revision,
                    host_registry_revision: authority.host_registry_revision,
                },
                state: ExecutionAttemptState::Open,
                run_id: Some("startup-ended-run".into()),
                turn_id: turn_id.clone(),
                recovery_of_attempt_id: None,
                terminal_outcome_id: None,
                admitted_at: self.at.to_rfc3339(),
                terminal_at: None,
            };
            let commands = [ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
                attempt,
            }))];
            let prepared = protocol::validate_execution_commands_tx(
                tx,
                &self.source.agent_id,
                Some(&ExecutionProtocolState::empty(&self.source.agent_id)),
                &commands,
                &[],
                &[],
                &[],
            )?;
            let (_, actual) =
                protocol::persist_execution_commands_with_touched_facts_tx(tx, prepared)?;
            register_committed_input_evidence_tx(tx, &self.binding.scope(), &actual)?;
            Ok(())
        })
    }
    fn clear_original_posture(&self) -> Result<()> {
        let before = self
            .db
            .agent_states()
            .latest(&self.source.agent_id)?
            .unwrap();
        let mut after = before.clone();
        after.current_run_id = None;
        after.current_turn_id = None;
        after.current_execution_binding = None;
        after.status = AgentStatus::AwakeIdle;
        self.db
            .transitions_in_scope(&self.binding.scope())
            .commit_agent_posture(&AgentPostureTransitionCommand {
                agent_id: self.source.agent_id.clone(),
                expected_incarnation: Some(1),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(before)),
                    record: Box::new(after),
                },
                audit_events: vec![],
                fault: None,
            })?;
        Ok(())
    }
    fn stop(&self) -> Result<ControlKey> {
        let key = ControlKey {
            agent_id: self.source.agent_id.clone(),
            identity_incarnation: 1,
            command_id: "startup-ended-stop".into(),
        };
        let snapshot = self.db.load_control_snapshot(&key)?;
        let command = prepare_control(ControlPlanInput {
            key: key.clone(),
            invocation: ControlInvocation {
                action: ControlAction::Stop,
                source: ControlSource::DirectApi,
                authority_class: AuthorityClass::OperatorInstruction,
            },
            snapshot,
            prepared_at: self.at + chrono::Duration::seconds(1),
            task_results: vec![],
            healthy_running: None,
            protected_control_attempt: None,
            protected_control_run_id: None,
            cancellation_targets: vec![ControlCancellationTarget {
                obligation_id: "startup-ended-run-obligation".into(),
                target_kind: ControlCancellationKind::Run,
                target_identity: "startup-ended-run".into(),
                target_generation: serde_json::json!({"owner_epoch":1,"runtime_lease_id":"original-startup-run-lease","activation_id":null,"source_message_id":self.source.id}),
                target: serde_json::json!({"reason":"agent_stopped"}),
            }],
            audit_events: vec![],
            index_changes: vec![],
        })?;
        self.db.commit_control(&command)?;
        Ok(key)
    }
    fn original_run(&self) -> ControlStoppedRun<'_> {
        ControlStoppedRun {
            agent_id: &self.source.agent_id,
            identity_incarnation: 1,
            owner_epoch: 1,
            run_id: "startup-ended-run",
            runtime_lease_id: "original-startup-run-lease",
            activation_id: None,
            source: &self.source,
        }
    }
}
fn install_source_replay_lineage(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    actual: &mut TurnRecord,
    at: DateTime<Utc>,
) -> Result<()> {
    let mut original = TurnRecord::new(&source.agent_id, "original-assignment", 0);
    original.input_message_ids = vec![source.id.clone()];
    original.terminal = Some(crate::types::TurnTerminalSummary {
        kind: crate::types::TurnTerminalKind::Interrupted,
        reason: Some("daemon_restart".into()),
        no_brief_reason: None,
        completed_at: at,
        duration_ms: 0,
    });
    upsert_turn_record_tx(tx, &original)?;
    let mut middle = TurnRecord::new(&source.agent_id, "intermediate-replay", 0);
    middle.input_message_ids = original.input_message_ids.clone();
    middle.terminal = original.terminal.clone();
    middle.replay = Some(crate::types::TurnReplayProvenance {
        source_message_id: source.id.clone(),
        source_turn_id: original.turn_id.clone(),
        reason: "interrupted_queue_claim_reentry".into(),
        prior_terminal: original.terminal,
    });
    upsert_turn_record_tx(tx, &middle)?;
    actual.replay = Some(crate::types::TurnReplayProvenance {
        source_message_id: source.id.clone(),
        source_turn_id: middle.turn_id,
        reason: "interrupted_queue_claim_reentry".into(),
        prior_terminal: middle.terminal,
    });
    Ok(())
}

#[test]
fn startup_source_assignment_replay_lineage_preserves_original_and_sizes() -> Result<()> {
    let f = Fixture::new()?;
    let original_sizes =
        f.db.transaction(|tx| lineage::source_assignment_material_tx(tx, &f.binding))?;
    let f = Fixture::with_replay(true)?;
    let sizes =
        f.db.transaction(|tx| lineage::source_assignment_material_tx(tx, &f.binding))?;
    assert!(sizes.0 > original_sizes.0);
    assert_eq!(sizes.1, original_sizes.1 + 2);
    let proof =
        f.db.transaction(|tx| read_original_address_tx(tx, &f.binding))?;
    assert_eq!(
        proof,
        f.db.transaction(|tx| read_original_address_tx(tx, &f.binding))?
    );
    assert_eq!(
        f.db.transaction(|tx| tx
            .query_row(
                "SELECT turn_id FROM conversation_input_assignments WHERE message_id=?1",
                [&f.source.id],
                |r| Ok(r.get::<_, String>(0)?),
            )
            .map_err(Into::into))?,
        "original-assignment"
    );
    Ok(())
}

#[test]
fn startup_source_assignment_replay_lineage_rejects_broken_original_proof() -> Result<()> {
    for mutation in [
        "DELETE FROM turn_records WHERE turn_id='intermediate-replay'",
        "UPDATE turn_records SET agent_id='foreign' WHERE turn_id='intermediate-replay'",
        "UPDATE turn_records SET payload_json=json_set(payload_json,'$.replay.source_message_id','foreign') WHERE turn_id='intermediate-replay'",
        "UPDATE turn_records SET payload_json=json_set(payload_json,'$.input_message_ids',json('[]')) WHERE turn_id='original-assignment'",
        "UPDATE turn_records SET payload_json=json_set(payload_json,'$.terminal.reason','changed') WHERE turn_id='original-assignment'",
        "UPDATE turn_records SET payload_json=json_set(payload_json,'$.replay.source_turn_id','intermediate-replay') WHERE turn_id='intermediate-replay'",
    ] {
        let f = Fixture::with_replay(true)?;
        f.db.transaction(|tx| {
            tx.execute_batch(mutation)?;
            // SQL-only preflight must also terminate for cyclic raw pointers.
            lineage::source_assignment_material_tx(tx, &f.binding)?;
            Ok(())
        })?;
        assert!(f.db.transaction(|tx| read_original_address_tx(tx, &f.binding)).is_err(), "{mutation}");
    }
    Ok(())
}

#[test]
fn exclusive_startup_attempt_preserves_canonical_ingress_without_accepting_foreign_turn(
) -> Result<()> {
    for ingress in [None, Some("immutable-ingress-turn")] {
        for attempt_turn in [
            None,
            ingress,
            Some("startup-ended-turn"),
            Some("foreign-turn"),
        ] {
            let f = Fixture::with_source_turn(false, ingress)?;
            f.admit_original_command_with_turn(attempt_turn.map(str::to_owned))?;
            let lease_path = f.home.path().join("maintenance.lock");
            let lease = RuntimeDbLock::try_lock(&lease_path)?;
            let handoff = tokio::sync::Mutex::new(());
            let handoff_guard = handoff.try_lock().unwrap();
            let capability = ExclusiveStartupInputRecovery::for_pre_activation_host(
                &f.db,
                &lease,
                &lease_path,
                &handoff_guard,
            )?;
            let scopes =
                f.db.startup_input_recovery_scopes(&[f.source.agent_id.clone()])?;
            f.db.recover_interrupted_runtime_state_at_startup_in_scopes(&scopes, &capability)?;
            let session = StartupEndedMemorySession::new(&f.db, &capability)?;
            let authorized =
                f.db.transaction(|tx| session.authorize_interrupted_tx(tx, &f.binding).map(|_| ()));
            assert_eq!(
                authorized.is_ok(),
                attempt_turn != Some("foreign-turn"),
                "{authorized:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn exclusive_startup_interrupt_authorization_survives_posture_tx_but_not_session() -> Result<()> {
    let f = Fixture::new()?;
    f.admit_original_command()?;
    let lease_path = f.home.path().join("maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lease_path)?;
    let handoff = tokio::sync::Mutex::new(());
    let handoff_guard = handoff.try_lock().unwrap();
    let capability = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &f.db,
        &lease,
        &lease_path,
        &handoff_guard,
    )?;
    let session = StartupEndedMemorySession::new(&f.db, &capability)?;
    assert!(f
        .db
        .transaction(|tx| session.authorize_interrupted_tx(tx, &f.binding))
        .is_err());
    let scopes =
        f.db.startup_input_recovery_scopes(&[f.source.agent_id.clone()])?;
    f.db.recover_interrupted_runtime_state_at_startup_in_scopes(&scopes, &capability)?;
    let sizes =
        f.db.transaction(|tx| session.interrupted_proof_material_tx(tx, &f.binding))?;
    assert!(sizes.0 > 0 && sizes.1 >= 7);
    let token =
        f.db.transaction(|tx| session.authorize_interrupted_tx(tx, &f.binding))?;
    assert_eq!(f.db.transaction(|tx| token.proof_material_tx(tx))?, sizes);
    assert!(f
        .db
        .transaction(|tx| token.validate_tx(tx, f.binding.key(), f.binding.invocation()))
        .is_err());
    f.clear_original_posture()?;
    f.db.transaction(|tx| token.validate_tx(tx, f.binding.key(), f.binding.invocation()))?;
    let serialized = serde_json::to_value(&token)?;
    assert!(serialized.get("activity").is_none());
    let other = Fixture::new()?;
    assert!(other
        .db
        .transaction(|tx| token.validate_tx(tx, f.binding.key(), f.binding.invocation()))
        .is_err());
    let mut wrong = f.binding.invocation().clone();
    wrong.turn_index += 1;
    assert!(f
        .db
        .transaction(|tx| token.validate_tx(tx, f.binding.key(), &wrong))
        .is_err());
    drop(session);
    assert!(f
        .db
        .transaction(|tx| token.validate_tx(tx, f.binding.key(), f.binding.invocation()))
        .is_err());
    Ok(())
}
#[test]
fn committed_stop_then_crash_uses_real_exclusive_startup_without_completing_os_obligation(
) -> Result<()> {
    let f = Fixture::new()?;
    let key = f.stop()?;
    let lease_path = f.home.path().join("maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lease_path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock().unwrap();
    let cap =
        ExclusiveStartupInputRecovery::for_pre_activation_host(&f.db, &lease, &lease_path, &guard)?;
    let session = StartupEndedMemorySession::new(&f.db, &cap)?;
    let scopes =
        f.db.startup_input_recovery_scopes(&[f.source.agent_id.clone()])?;
    f.db.recover_interrupted_runtime_state_at_startup_in_scopes(&scopes, &cap)?;
    assert!(f
        .db
        .transaction(|tx| session.authorize_interrupted_tx(tx, &f.binding))
        .is_err());
    let sizes = f
        .db
        .transaction(|tx| session.stopped_proof_material_tx(tx, &f.binding, &f.original_run()))?;
    assert!(sizes.0 > 0 && sizes.1 >= 5);
    let token = f
        .db
        .transaction(|tx| session.authorize_stopped_tx(tx, &f.binding, &key, &f.original_run()))?;
    assert_eq!(f.db.transaction(|tx| token.proof_material_tx(tx))?, sizes);
    f.db.transaction(|tx| token.validate_tx(tx, f.binding.key(), f.binding.invocation()))?;
    let states=f.db.connection()?.query_row("SELECT COUNT(*) FROM control_cancellation_obligations WHERE agent_id=?1 AND state='pending'",[&f.source.agent_id],|r|r.get::<_,usize>(0))?;
    assert_eq!(states, 1);
    drop(session);
    assert!(f
        .db
        .transaction(|tx| token.validate_tx(tx, f.binding.key(), f.binding.invocation()))
        .is_err());
    assert!(f
        .db
        .transaction(
            |tx| super::super::dispatch_memory::unfinished_dispatch_memory_tx(
                tx,
                &f.binding.scope()
            )
        )
        .map(|v| !v.is_empty())?);
    Ok(())
}
#[test]
fn terminal_rows_without_original_startup_audit_never_mint_startup_authorization() -> Result<()> {
    let f = Fixture::new()?;
    let lease_path = f.home.path().join("maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lease_path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock().unwrap();
    let cap =
        ExclusiveStartupInputRecovery::for_pre_activation_host(&f.db, &lease, &lease_path, &guard)?;
    let session = StartupEndedMemorySession::new(&f.db, &cap)?;
    let scopes =
        f.db.startup_input_recovery_scopes(&[f.source.agent_id.clone()])?;
    f.db.recover_interrupted_runtime_state_at_startup_in_scopes(&scopes, &cap)?;
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM audit_events WHERE audit_event_id=?1",
            [format!(
                "audit:startup-interrupted-turn:{}",
                f.binding.key().actual_dispatch_turn_id
            )],
        )?;
        Ok(())
    })?;
    assert!(f
        .db
        .transaction(|tx| session.authorize_interrupted_tx(tx, &f.binding))
        .is_err());
    assert!(f
        .db
        .transaction(|tx| ContextDurableRead::new(tx, &f.source.agent_id)
            .turn_by_id(&f.binding.key().actual_dispatch_turn_id))
        .map(|v| v.unwrap().terminal.is_some())?);
    Ok(())
}
