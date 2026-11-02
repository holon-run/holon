use super::*;
use crate::{
    domain::execution_protocol::{
        AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionOrigin,
        ExecutionPriority, ExecutionProtocolState, ExecutionProvenance, ExecutionSource,
        ExecutionSourceIdentity, ExecutionTrust,
    },
    runtime::resident_input_evidence::InputEvidenceScope,
    runtime_db::{
        control_transition::{
            prepare_control, ControlCancellationKind, ControlCancellationTarget, ControlCommit,
            ControlInvocation, ControlKey, ControlPlanInput, ControlSource, ControlStoppedRun,
        },
        dispatch_memory::{
            DispatchMemoryBinding, DispatchMemoryCommandPolicy, DispatchMemoryStart,
        },
        episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey},
        evidence::{append_message_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{
            register_committed_input_evidence_tx, CanonicalInputEvidence as Fact,
        },
        repositories::{upsert_queue_entry_tx, upsert_turn_record_tx},
        transitions::{
            execution_protocol_repository, AgentStateMutation, InputMutationExecution,
            PreparedInputTransition, PreparedQueueTransition, QueueMutation, QueueOperation,
            QueueTransitionCommand,
        },
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentStatus, AgentVisibility, AuditEvent, AuthorityClass, BriefKind, BriefRecord,
        ClosureDecision, ClosureOutcome, ControlAction, MessageBody, MessageEnvelope, MessageKind,
        MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus, RuntimePosture, TurnOwner,
        TurnRecord, TurnTriggerSummary, WorkItemExecutionBinding,
    },
};

struct Fixture {
    _home: tempfile::TempDir,
    db: RuntimeDb,
    source: MessageEnvelope,
    pause: ProviderDispatchPause,
    ledger: crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
}

#[path = "stopped_pause_memory_tests.rs"]
mod memory_tests;

impl Fixture {
    fn new() -> Result<Self> {
        Self::with_dispatch(true)
    }

    fn with_dispatch(started: bool) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let now: DateTime<Utc> = "2026-10-27T12:00:00Z".parse()?;
        let identity = AgentIdentityRecord::new(
            "pause-agent",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        db.agent_identities().upsert(&identity)?;
        let mut state = AgentState::new("pause-agent");
        db.agent_states().upsert_for_fixture(&state)?;
        let mut source = MessageEnvelope::new(
            "pause-agent",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original input".into(),
            },
        );
        source.created_at = now;
        db.transaction(|tx| {
            source = append_message_tx(tx, &source)?.0;
            upsert_queue_entry_tx(
                tx,
                &QueueEntryRecord {
                    agent_id: source.agent_id.clone(),
                    message_id: source.id.clone(),
                    priority: source.priority.clone(),
                    status: QueueEntryStatus::Dequeued,
                    created_at: now,
                    updated_at: now,
                },
            )?;
            let attempt = ExecutionAttempt {
                input_projection: None,
                attempt_id: "pause-attempt".into(),
                agent_id: source.agent_id.clone(),
                source_message_id: Some(source.id.clone()),
                source: ExecutionSource {
                    identity: ExecutionSourceIdentity::QueueMessage {
                        message_id: source.id.clone(),
                    },
                    generation: source.message_seq.unwrap(),
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
                run_id: Some("pause-run".into()),
                turn_id: started.then(|| "pause-turn".into()),
                recovery_of_attempt_id: None,
                terminal_outcome_id: None,
                admitted_at: now.to_rfc3339(),
                terminal_at: None,
            };
            let mut protocol = ExecutionProtocolState::empty(&source.agent_id);
            protocol
                .attempts
                .insert(attempt.attempt_id.clone(), attempt.clone());
            execution_protocol_repository::persist_state_tx(tx, &protocol)?;
            let mut facts = vec![
                Fact::Message(source.clone()),
                Fact::Attempt(attempt.clone()),
            ];
            state.status = AgentStatus::AwakeRunning;
            state.current_run_id = Some("pause-run".into());
            if started {
                let mut turn = TurnRecord::new(&source.agent_id, "pause-turn", 1);
                turn.created_at = now;
                turn.run_id = Some("pause-run".into());
                turn.owner = Some(TurnOwner::Command);
                turn.trigger = Some(TurnTriggerSummary::from_message(&source));
                turn.input_message_ids = vec![source.id.clone()];
                upsert_turn_record_tx(tx, &turn)?;
                state.turn_index = 1;
                state.current_turn_id = Some(turn.turn_id.clone());
                state.current_execution_binding = Some(WorkItemExecutionBinding {
                    activation_id: Some(attempt.attempt_id.clone()),
                    admission_provenance: None,
                    source_message_id: source.id.clone(),
                    turn_id: turn.turn_id.clone(),
                    owner: turn.owner.clone(),
                    work_item_id: None,
                    claimed_work_revision: None,
                });
                facts.push(Fact::Turn(turn));
            }
            upsert_agent_state_tx(tx, &state)?;
            register_committed_input_evidence_tx(
                tx,
                &InputEvidenceScope {
                    agent_id: source.agent_id.clone(),
                    identity_incarnation: 1,
                },
                &facts,
            )?;
            Ok(())
        })?;
        let pause = ProviderDispatchPause {
            parking: ProviderQueueParking {
                agent_id: source.agent_id.clone(),
                message_id: source.id.clone(),
                chain_fingerprint: "original-chain".into(),
                episode_key: "original-episode".into(),
                not_before: now + Duration::seconds(60),
                reason: "chain_cooling".into(),
            },
            attempt_id: "pause-attempt".into(),
            denial: "chain_cooling",
        };
        db.park_provider_queue(&pause.parking)?;
        let fixture = Self {
            _home: home,
            db,
            source,
            pause,
            ledger: crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
                crate::config::StateMemoryLimits::default(),
            )?,
        };
        if started {
            fixture.install_start()?;
        }
        Ok(fixture)
    }

    fn install_start(&self) -> Result<()> {
        use crate::runtime_db::{
            context_inputs::ContextDurableRead, dispatch_memory::DispatchMemoryIntent,
            transitions::AgentPostureTransitionCommand,
        };
        let scope = InputEvidenceScope {
            agent_id: self.source.agent_id.clone(),
            identity_incarnation: 1,
        };
        let prepared = self.db.transaction(|tx| {
            let read = ContextDurableRead::new(tx, &scope.agent_id);
            let state = read
                .scope_facts()?
                .into_owned_state()?
                .context("fixture State")?;
            let turn = read.turn_by_id("pause-turn")?.context("fixture Turn")?;
            PreparedInputTransition::posture(
                scope.clone(),
                "pause-original-start".into(),
                self.source.created_at,
                AgentPostureTransitionCommand {
                    agent_id: scope.agent_id.clone(),
                    expected_incarnation: Some(1),
                    agent_state: AgentStateMutation {
                        expected: Some(Box::new(state.clone())),
                        record: Box::new(state),
                    },
                    audit_events: Vec::new(),
                    fault: None,
                },
                Some(turn),
            )?
            .with_dispatch_memory(DispatchMemoryIntent::Started {
                start: Box::new(self.memory_start()?),
            })
        })?;
        self.db
            .transitions_in_scope(&scope)
            .commit_prepared_transition(&prepared)?;
        Ok(())
    }

    fn frozen_memory(
        &self,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<FrozenStoppedProviderPause> {
        let mut frozen = FrozenStoppedProviderPause::freeze(&self.original(), &self.pause)?;
        let scope = InputEvidenceScope {
            agent_id: self.source.agent_id.clone(),
            identity_incarnation: 1,
        };
        let prepared = PreparedInputTransition::queue(
            scope.clone(),
            "freeze-original-pause-memory".into(),
            self.source.created_at,
            self.ack_transition(&self.stop_committed_queue()?, &self.ack_brief())?,
        )?;
        let ack = prepared.stopped_provider_pause_ack().context("typed ACK")?;
        let memory = self
            .db
            .transaction(|tx| load_stopped_provider_pause_memory_tx(tx, &ack, &scope, ledger))?;
        frozen.with_memory(memory)?;
        Ok(frozen)
    }

    fn frozen_material(&self, frozen: &FrozenStoppedProviderPause) -> Result<(usize, usize)> {
        self.db.transaction(|tx| {
            let proof = stopped_provider_pause_material_tx(tx, &self.original())?;
            let memory = stopped_pause_memory::frozen_material_tx(tx, frozen)?;
            Ok((proof.0 + memory.0, proof.1 + memory.1))
        })
    }

    fn original(&self) -> ControlStoppedRun<'_> {
        ControlStoppedRun {
            agent_id: &self.source.agent_id,
            identity_incarnation: 1,
            owner_epoch: 1,
            run_id: "pause-run",
            runtime_lease_id: "pause-lease",
            activation_id: Some("pause-attempt"),
            source: &self.source,
        }
    }

    fn stop(&self) -> Result<()> {
        let key = ControlKey {
            agent_id: self.source.agent_id.clone(),
            identity_incarnation: 1,
            command_id: "stop-before-pause-settlement".into(),
        };
        let command = prepare_control(ControlPlanInput {
            snapshot: self.db.load_control_snapshot(&key)?,
            key,
            invocation: ControlInvocation {
                action: ControlAction::Stop,
                source: ControlSource::DirectApi,
                authority_class: AuthorityClass::OperatorInstruction,
            },
            prepared_at: self.source.created_at + Duration::seconds(1),
            task_results: Vec::new(),
            healthy_running: None,
            protected_control_attempt: None,
            protected_control_run_id: None,
            cancellation_targets: vec![ControlCancellationTarget {
                obligation_id: "stop-original-run".into(),
                target_kind: ControlCancellationKind::Run,
                target_identity: "pause-run".into(),
                target_generation: serde_json::json!({
                    "owner_epoch": 1, "runtime_lease_id": "pause-lease",
                    "activation_id": "pause-attempt", "source_message_id": self.source.id
                }),
                target: serde_json::json!({"reason": "agent_stopped"}),
            }],
            audit_events: Vec::new(),
            index_changes: Vec::new(),
        })?;
        assert!(matches!(
            self.db.commit_control(&command)?,
            ControlCommit::Applied { .. }
        ));
        Ok(())
    }

    /// The frozen ACK must repeat the Stop-committed queue terminal verbatim;
    /// fabricating a second terminal value is exactly what it rejects.
    fn stop_committed_queue(&self) -> Result<QueueEntryRecord> {
        let payload: String = self.db.connection()?.query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id=?1",
            [&self.source.id],
            |row| row.get(0),
        )?;
        let record: QueueEntryRecord = serde_json::from_str(&payload)?;
        assert_eq!(record.status, QueueEntryStatus::Aborted);
        assert_eq!(record.message_id, self.source.id);
        Ok(record)
    }

    fn evidence_counts(&self) -> Result<(i64, i64)> {
        let briefs: i64 = self.db.connection()?.query_row(
            "SELECT COUNT(*) FROM briefs WHERE agent_id=?1",
            [&self.source.agent_id],
            |row| row.get(0),
        )?;
        let audits: i64 = self.db.connection()?.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1",
            [&self.source.agent_id],
            |row| row.get(0),
        )?;
        Ok((briefs, audits))
    }

    fn ack_brief(&self) -> BriefRecord {
        BriefRecord::new(
            &self.source.agent_id,
            BriefKind::Ack,
            "provider dispatch paused; the Stop owns this settlement",
            Some(self.source.id.clone()),
            None,
        )
    }

    fn ack_transition(
        &self,
        record: &QueueEntryRecord,
        brief: &BriefRecord,
    ) -> Result<PreparedQueueTransition> {
        PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: self.source.agent_id.clone(),
            operation: QueueOperation::Settle,
            mutation: QueueMutation::Upsert(record.clone()),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: vec![
                AuditEvent::legacy(
                    "provider_dispatch_paused",
                    serde_json::json!({"message_id": self.source.id}),
                ),
                AuditEvent::legacy(
                    "queue_entry_settled",
                    serde_json::json!({"message_id": self.source.id}),
                ),
                crate::types::brief_created_event_for(brief)?,
            ],
            notify_scheduler: true,
            fault: None,
            brief_evidence: vec![brief.clone()],
        })
        .with_stopped_provider_pause(&self.original(), &self.pause)
    }

    fn ack_prepared(
        &self,
        nonce: &str,
        queue: PreparedQueueTransition,
    ) -> Result<PreparedInputTransition> {
        let prepared = PreparedInputTransition::queue(
            crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: self.source.agent_id.clone(),
                identity_incarnation: 1,
            },
            nonce.into(),
            self.source.created_at,
            queue,
        )?;
        let Some(ack) = prepared.stopped_provider_pause_ack() else {
            return Ok(prepared);
        };
        let memory = self.db.transaction(|tx| {
            load_stopped_provider_pause_memory_tx(
                tx,
                &ack,
                &InputEvidenceScope {
                    agent_id: self.source.agent_id.clone(),
                    identity_incarnation: 1,
                },
                &self.ledger,
            )
        })?;
        prepared.with_stopped_provider_pause_memory(memory)
    }

    fn commit_ack(
        &self,
        prepared: &PreparedInputTransition,
    ) -> Result<InputMutationExecution<crate::runtime_db::transitions::TransitionCommit>> {
        self.db
            .transitions_in_scope(
                &crate::runtime::resident_input_evidence::InputEvidenceScope {
                    agent_id: self.source.agent_id.clone(),
                    identity_incarnation: 1,
                },
            )
            .commit_prepared_transition_with_work_request_and_memory_budget(
                prepared,
                &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                    capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                    record_limit: 65536,
                    byte_limit: 64 * 1024 * 1024,
                },
                &self.ledger,
                &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                    recovery_at: None,
                    query: Default::default(),
                    capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                    record_limit: 65536,
                    byte_limit: 64 * 1024 * 1024,
                },
            )
    }

    fn memory_binding(&self) -> Result<DispatchMemoryBinding> {
        Ok(DispatchMemoryBinding::from_start(&self.memory_start()?))
    }

    fn memory_start(&self) -> Result<DispatchMemoryStart> {
        DispatchMemoryStart::new(
            EpisodeMemoryKey {
                agent_id: self.source.agent_id.clone(),
                identity_incarnation: 1,
                actual_dispatch_turn_id: "pause-turn".into(),
            },
            EpisodeMemoryInvocation {
                message_id: self.source.id.clone(),
                turn_index: 1,
            },
            &self.source,
            ClosureDecision {
                outcome: ClosureOutcome::Completed,
                waiting_reason: None,
                work_signal: None,
                runtime_posture: RuntimePosture::Awake,
                evidence: Vec::new(),
            },
            AgentState::new(&self.source.agent_id).working_memory,
        )
    }
}

#[test]
fn frozen_stopped_provider_pause_ack_does_not_finish_original_dispatch_memory() -> Result<()> {
    let f = Fixture::new()?;
    let binding = f.memory_binding()?;
    f.stop()?;
    let record = f.stop_committed_queue()?;
    let queue = f.ack_transition(&record, &f.ack_brief())?;
    let prepared = f.ack_prepared("typed-ack-memory-classification", queue)?;
    assert!(
        !prepared.dispatch_memory_candidate(),
        "the typed Brief/Audit ACK must not load producer Memory through stopped State"
    );
    assert_eq!(
        prepared.dispatch_memory_policy(&binding),
        DispatchMemoryCommandPolicy::None,
        "the original producer binding must neither Finish nor become Deferred"
    );
    Ok(())
}

#[test]
fn ordinary_settlement_after_stop_keeps_automatic_dispatch_memory() -> Result<()> {
    let f = Fixture::new()?;
    let binding = f.memory_binding()?;
    f.stop()?;
    let record = f.stop_committed_queue()?;
    let mut queue = f.ack_transition(&record, &f.ack_brief())?;
    queue.stopped_provider_pause = None;
    let prepared = f.ack_prepared("ordinary-settle-memory-classification", queue)?;
    assert!(
        prepared.dispatch_memory_candidate(),
        "Stop alone must not exempt an ordinary Queue settlement"
    );
    assert_eq!(
        prepared.dispatch_memory_policy(&binding),
        DispatchMemoryCommandPolicy::Finish
    );
    Ok(())
}

#[test]
fn stopped_pause_acknowledgement_is_read_only_and_replayable() -> Result<()> {
    let f = Fixture::new()?;
    f.db.transaction(|tx| {
        assert!(!acknowledge_stopped_provider_pause_tx(
            tx,
            &f.original(),
            &f.pause
        )?);
        Ok(())
    })?;
    f.stop()?;
    f.db.transaction(|tx| {
        let before: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let (bytes, records) = stopped_provider_pause_material_tx(tx, &f.original())?;
        assert!(bytes > 0 && records > 1);
        for _ in 0..2 {
            assert!(acknowledge_stopped_provider_pause_tx(
                tx,
                &f.original(),
                &f.pause
            )?);
        }
        let after: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        assert_eq!(
            before, after,
            "pause acknowledgement must not rewrite Stop facts"
        );
        Ok(())
    })
}

#[test]
fn stopped_pause_acknowledgement_rejects_changed_original_or_parking() -> Result<()> {
    let f = Fixture::new()?;
    f.stop()?;
    f.db.transaction(|tx| {
        for original in [
            ControlStoppedRun {
                owner_epoch: 2,
                ..f.original()
            },
            ControlStoppedRun {
                runtime_lease_id: "another-lease",
                ..f.original()
            },
            ControlStoppedRun {
                identity_incarnation: 2,
                ..f.original()
            },
        ] {
            assert!(!acknowledge_stopped_provider_pause_tx(
                tx, &original, &f.pause
            )?);
        }
        for field in [
            "attempt", "source", "owner", "chain", "episode", "deadline", "reason",
        ] {
            let mut pause = f.pause.clone();
            match field {
                "attempt" => pause.attempt_id = "another-attempt".into(),
                "source" => pause.parking.message_id = "another-source".into(),
                "owner" => pause.parking.agent_id = "another-agent".into(),
                "chain" => pause.parking.chain_fingerprint = "another-chain".into(),
                "episode" => pause.parking.episode_key = "another-episode".into(),
                "deadline" => pause.parking.not_before += Duration::seconds(1),
                "reason" => pause.parking.reason = "recovery_budget_exhausted".into(),
                _ => unreachable!(),
            }
            assert!(acknowledge_stopped_provider_pause_tx(tx, &f.original(), &pause).is_err());
        }
        Ok(())
    })?;
    f.db.clear_provider_queue_parking(&f.source.agent_id, &f.source.id)?;
    f.db.transaction(|tx| {
        assert!(acknowledge_stopped_provider_pause_tx(tx, &f.original(), &f.pause).is_err());
        Ok(())
    })
}

#[test]
fn stopped_pause_acknowledgement_requires_original_stop_terminal_facts() -> Result<()> {
    for corrupt in [
        "UPDATE queue_entries SET status='interrupted', payload_json=json_set(payload_json,'$.status','interrupted')",
        "UPDATE turn_records SET payload_json=json_set(payload_json,'$.terminal.reason','provider_dispatch_parked')",
        "UPDATE turn_records SET payload_json=json_set(payload_json,'$.terminal.completed_at','2026-10-27T12:00:02Z')",
        "UPDATE execution_protocol_attempts SET lifecycle_state='open', payload_json=json_set(payload_json,'$.state','open')",
    ] {
        let f = Fixture::new()?;
        f.stop()?;
        f.db.transaction(|tx| {
            tx.execute(corrupt, [])?;
            assert!(acknowledge_stopped_provider_pause_tx(tx, &f.original(), &f.pause).is_err());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn frozen_stopped_provider_pause_freezes_only_its_original_dispatch() -> Result<()> {
    let f = Fixture::new()?;
    for field in ["attempt", "source", "owner", "reason"] {
        let mut pause = f.pause.clone();
        match field {
            "attempt" => pause.attempt_id = "another-attempt".into(),
            "source" => pause.parking.message_id = "another-source".into(),
            "owner" => pause.parking.agent_id = "another-agent".into(),
            "reason" => pause.parking.reason = "recovery_budget_exhausted".into(),
            _ => unreachable!(),
        }
        assert!(FrozenStoppedProviderPause::freeze(&f.original(), &pause).is_err());
    }
    let frozen = FrozenStoppedProviderPause::freeze(&f.original(), &f.pause)?;
    let stopped = frozen.stopped_run();
    assert!(std::ptr::eq(stopped.source, &frozen.source));
    assert_eq!(
        (
            stopped.agent_id,
            stopped.run_id,
            stopped.runtime_lease_id,
            stopped.activation_id
        ),
        (
            f.original().agent_id,
            f.original().run_id,
            f.original().runtime_lease_id,
            f.original().activation_id
        )
    );
    assert_eq!(
        (stopped.identity_incarnation, stopped.owner_epoch),
        (f.original().identity_incarnation, f.original().owner_epoch)
    );
    Ok(())
}

#[test]
fn frozen_stopped_provider_pause_final_queue_tx_charges_then_reverifies() -> Result<()> {
    let f = Fixture::new()?;
    f.stop()?;
    let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        crate::config::StateMemoryLimits::default(),
    )?;
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: f.source.agent_id.clone(),
        identity_incarnation: 1,
    };
    let frozen = f.frozen_memory(&ledger)?;
    let (bytes, records) = f.frozen_material(&frozen)?;
    assert!(
        bytes > 4096 && bytes <= 16 * 1024 && records <= 16,
        "the finite Stop fixture exceeds 4096 but must stay small"
    );
    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        // Admit exactly the independently measured proof, not a larger budget.
        byte_limit: bytes,
        record_limit: records,
    };
    let observed = ledger.report()?.observed_bytes;
    f.db.transaction(|tx| {
        let before: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let state_before: String = tx.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
            [&f.source.agent_id],
            |row| row.get(0),
        )?;
        let _guard =
            charge_frozen_stopped_provider_pause_tx(tx, &frozen, &scope, &request, &ledger, None)?;
        assert_eq!(
            ledger.report()?.observed_bytes,
            observed + u64::try_from(bytes)?
        );
        for _ in 0..2 {
            acknowledge_frozen_stopped_provider_pause_tx(tx, &frozen)?;
        }
        let after: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        assert_eq!(before, after, "frozen pause acknowledgement writes nothing");
        let state_after: String = tx.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
            [&f.source.agent_id],
            |row| row.get(0),
        )?;
        assert_eq!(state_before, state_after, "AgentState stays byte-identical");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, observed);
    Ok(())
}

#[test]
fn frozen_stopped_provider_pause_final_queue_tx_rejects_small_material_budgets() -> Result<()> {
    let f = Fixture::new()?;
    f.stop()?;
    let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        crate::config::StateMemoryLimits::default(),
    )?;
    let scope = InputEvidenceScope {
        agent_id: f.source.agent_id.clone(),
        identity_incarnation: 1,
    };
    let frozen = f.frozen_memory(&ledger)?;
    let (bytes, records) = f.frozen_material(&frozen)?;
    assert!(
        bytes > 4096 && bytes <= 16 * 1024 && records > 0 && records <= 16,
        "the finite Stop fixture must exercise the original small-budget refusal"
    );
    let observed = ledger.report()?.observed_bytes;
    let evidence_before = f.evidence_counts()?;
    for (label, byte_limit, record_limit) in [
        ("original 4096-byte budget", 4096, records),
        ("one byte below exact material", bytes - 1, records),
        ("one record below exact material", bytes, records - 1),
    ] {
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            byte_limit,
            record_limit,
        };
        f.db.transaction(|tx| {
            let before: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
            let error = charge_frozen_stopped_provider_pause_tx(
                tx, &frozen, &scope, &request, &ledger, None,
            )
            .err()
            .expect("insufficient proof material budget must fail");
            assert!(
                matches!(
                    error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
                    Some(
                        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                            "original Command material"
                        )
                    )
                ),
                "{label}: {error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, observed, "{label}");
            let after: u64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
            assert_eq!(before, after, "{label}: refusal must write nothing");
            Ok(())
        })?;
    }
    assert_eq!(f.evidence_counts()?, evidence_before);
    Ok(())
}

#[test]
fn frozen_stopped_provider_pause_final_queue_tx_rejects_changed_parking_and_missing_stop(
) -> Result<()> {
    let f = Fixture::new()?;
    f.db.transaction(|tx| {
        assert!(acknowledge_frozen_stopped_provider_pause_tx(
            tx,
            &FrozenStoppedProviderPause::freeze(&f.original(), &f.pause)?
        )
        .is_err());
        Ok(())
    })?;
    f.stop()?;
    let frozen = f.frozen_memory(&f.ledger)?;
    f.db.transaction(|tx| {
        let mut changed = f.pause.clone();
        changed.parking.episode_key = "another-episode".into();
        let frozen_changed = FrozenStoppedProviderPause::freeze(&f.original(), &changed)?;
        assert!(acknowledge_frozen_stopped_provider_pause_tx(tx, &frozen_changed).is_err());
        acknowledge_frozen_stopped_provider_pause_tx(tx, &frozen)?;
        Ok(())
    })
}

#[test]
fn frozen_stopped_provider_pause_ack_commits_only_original_brief_and_audit() -> Result<()> {
    let f = Fixture::new()?;
    f.stop()?;
    let record = f.stop_committed_queue()?;
    let (briefs_before, audits_before) = f.evidence_counts()?;
    let brief = f.ack_brief();
    let prepared = f.ack_prepared("stopped-pause-ack", f.ack_transition(&record, &brief)?)?;
    let queue_before: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM queue_entries WHERE message_id=?1",
        [&f.source.id],
        |row| row.get(0),
    )?;
    let state_before: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    let attempt_before: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM execution_protocol_attempts WHERE attempt_id=?1",
        ["pause-attempt"],
        |row| row.get(0),
    )?;
    let execution = f.commit_ack(&prepared)?;
    let InputMutationExecution::Committed { value, .. } = &execution else {
        anyhow::bail!("frozen stopped pause ACK did not commit");
    };
    assert!(value.applied);
    assert_eq!(
        value.effects.audit_events.len(),
        3,
        "only the original pause events commit"
    );
    assert!(
        !value.effects.task_material_budget_views.is_empty(),
        "frozen ACK keeps its charged material guard in the same transaction"
    );
    let briefs: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM briefs WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(briefs, briefs_before + 1);
    let audits: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(audits, audits_before + 3);
    let queue_after: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM queue_entries WHERE message_id=?1",
        [&f.source.id],
        |row| row.get(0),
    )?;
    assert_eq!(
        queue_after, queue_before,
        "Stop-owned queue value untouched"
    );
    let state_after: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(state_after, state_before, "AgentState stays byte-identical");
    let attempt_after: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM execution_protocol_attempts WHERE attempt_id=?1",
        ["pause-attempt"],
        |row| row.get(0),
    )?;
    assert_eq!(attempt_after, attempt_before, "attempt stays untouched");
    // Exact receipt replay writes nothing new.
    let replay = f.commit_ack(&prepared)?;
    assert!(matches!(replay, InputMutationExecution::Replayed { .. }));
    let briefs_again: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM briefs WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(briefs_again, briefs_before + 1);
    let audits_again: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(audits_again, audits_before + 3);
    Ok(())
}

#[test]
fn frozen_stopped_provider_pause_ack_rejects_every_non_original_surface() -> Result<()> {
    let f = Fixture::new()?;
    f.stop()?;
    let record = f.stop_committed_queue()?;
    let (briefs_before, audits_before) = f.evidence_counts()?;
    let receipt_snapshot = || -> Result<Vec<String>> {
        let connection = f.db.connection()?;
        let mut query = connection.prepare(
            "SELECT json_array(command_nonce, family, phase_version, command_digest,
                scope_digest, scopes_json, prepared_at, prepared_json, outcome_json,
                committed_at, result_digest)
             FROM input_mutation_receipts ORDER BY command_nonce",
        )?;
        let rows = query.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    };
    let receipts_before = receipt_snapshot()?;
    let brief = f.ack_brief();
    let stopped_turn = TurnRecord::new(&f.source.agent_id, "stop-owned-turn", 7);
    let state_rewrite = AgentState::new(&f.source.agent_id);
    let consume_record = record.clone();
    let mut requeued = record.clone();
    requeued.status = QueueEntryStatus::Queued;
    let mut changed_value = record.clone();
    changed_value.updated_at += Duration::seconds(1);
    let interrupt = crate::domain::execution_protocol::ExecutionProtocolCommand::Interrupt(
        crate::domain::execution_protocol::InterruptExecution {
            attempt_id: "pause-attempt".into(),
            outcome_id: "outcome:interrupted:pause-attempt".into(),
            reason: "agent_stopped".into(),
            interrupted_at: f.source.created_at.to_rfc3339(),
        },
    );
    let timer_message_id = f.source.id.clone();
    let extra_message = f.source.clone();
    let second_brief = f.ack_brief();
    let violations: Vec<(&str, Box<dyn FnOnce(&mut PreparedQueueTransition)>)> = vec![
        (
            "stop-owned turn",
            Box::new(move |queue| queue.command.turn_record = Some(stopped_turn)),
        ),
        (
            "agent state rewrite",
            Box::new(move |queue| {
                queue.command.agent_state = Some(AgentStateMutation {
                    expected: None,
                    record: Box::new(state_rewrite),
                })
            }),
        ),
        (
            "queue consume",
            Box::new(move |queue| queue.command.mutation = QueueMutation::Consume(consume_record)),
        ),
        (
            "requeued terminal",
            Box::new(move |queue| queue.command.mutation = QueueMutation::Upsert(requeued)),
        ),
        (
            "changed queue value",
            Box::new(move |queue| queue.command.mutation = QueueMutation::Upsert(changed_value)),
        ),
        (
            "execution command",
            Box::new(move |queue| queue.execution_protocol.commands.push(interrupt)),
        ),
        (
            "timer wake claim",
            Box::new(move |queue| {
                queue.timer_wake_claim = Some(crate::runtime_db::transitions::TimerWakeClaim {
                    timer_id: "pause-timer".into(),
                    message_id: timer_message_id,
                    fire_count: 1,
                })
            }),
        ),
        (
            "task result settlement",
            Box::new(|queue| {
                queue.task_result_settlement = Some(
                    crate::runtime_db::task_result_settlement::TaskResultActivationSettlement {
                        activation_id: "pause-attempt".into(),
                        disposition:
                            crate::runtime_db::TaskResultSettlementDisposition::ModelDelivered,
                        settled_at: Utc::now(),
                    },
                )
            }),
        ),
        (
            "extra message evidence",
            Box::new(move |queue| queue.command.message_evidence.push(extra_message)),
        ),
        (
            "second brief",
            Box::new(move |queue| queue.command.brief_evidence.push(second_brief)),
        ),
    ];
    for (index, (label, violate)) in violations.into_iter().enumerate() {
        let mut queue = f.ack_transition(&record, &brief)?;
        violate(&mut queue);
        let nonce = format!("stopped-pause-ack-reject-{index}");
        let prepared = if label == "changed queue value" {
            // Freeze memory against the real ACK before testing the final TX's
            // independent full-value check on another in-memory Queue value.
            let memory = f
                .frozen_memory(&f.ledger)?
                .memory
                .context("original memory")?;
            PreparedInputTransition::queue(
                InputEvidenceScope {
                    agent_id: f.source.agent_id.clone(),
                    identity_incarnation: 1,
                },
                nonce,
                f.source.created_at,
                queue,
            )?
            .with_stopped_provider_pause_memory(memory)?
        } else {
            f.ack_prepared(&nonce, queue)?
        };
        if label != "changed queue value" {
            assert!(prepared.stopped_provider_pause_ack().is_none(), "{label}");
            assert!(
                prepared.dispatch_memory_candidate(),
                "{label} must not gain the typed ACK Memory exemption"
            );
            assert_eq!(
                prepared.dispatch_memory_policy(&f.memory_binding()?),
                DispatchMemoryCommandPolicy::Finish,
                "{label} must retain ordinary settlement classification"
            );
        }
        assert!(
            f.commit_ack(&prepared).is_err(),
            "{label} must not ride the frozen ACK"
        );
        assert_eq!(
            receipt_snapshot()?,
            receipts_before,
            "{label} must leave every original receipt unchanged"
        );
    }
    let receipts: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts
         WHERE command_nonce GLOB 'stopped-pause-ack-reject-*'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(receipts, 0, "every rejected ACK rolled back completely");
    let briefs: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM briefs WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(briefs, briefs_before, "every rejected ACK stays brief-free");
    let audits: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1",
        [&f.source.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        audits, audits_before,
        "every rejected ACK rolls back its audits"
    );
    let queue_payload: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM queue_entries WHERE message_id=?1",
        [&f.source.id],
        |row| row.get(0),
    )?;
    assert_eq!(queue_payload, serde_json::to_string(&record)?);
    Ok(())
}
