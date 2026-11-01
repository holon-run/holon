use super::*;
use crate::{
    memory::episode_transition::{plan_episode_memory, EpisodeMemoryPlanInput},
    runtime::resident_work::{WorkCaptureRequest, WorkFamily},
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
        input_mutation_receipts::{
            insert_input_mutation_receipt_tx, InputMutationOutcome, InputMutationResult,
            InputMutationScopeOutcome,
        },
        repositories::{committed_turn_record_tx, upsert_turn_record_tx},
        transitions::{
            AgentPostureTransitionCommand, AgentStateMutation, ExecutionProtocolTransition,
            PreparedInputTransition, PreparedQueueTransition, QueueMutation, QueueOperation,
            QueueTransitionCommand, WorkItemMutation, WorkItemTransitionCommand,
        },
        work_snapshot::{prepare_work_write_tx, register_work_writes_tx, WorkDomainLoadRequest},
        RuntimeDb,
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentStatus, AgentVisibility,
        AuthorityClass, ClosureOutcome, MessageBody, MessageOrigin, Priority, QueueEntryRecord,
        QueueEntryStatus, RuntimePosture, WorkItemExecutionBinding, WorkItemRecord, WorkItemState,
    },
};

struct Fixture {
    _directory: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    start: DispatchMemoryStart,
    source: MessageEnvelope,
    original_start: PreparedInputMutation,
    at: DateTime<Utc>,
}
impl Fixture {
    fn new(install: bool) -> Result<Self> {
        Self::with_trigger(install, true)
    }
    fn with_trigger(install: bool, trigger: bool) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "dispatch-memory".into(),
            identity_incarnation: 1,
        };
        let at: DateTime<Utc> = "2026-10-01T00:00:00Z".parse()?;
        let (source, start) = db.transaction(|tx| {
            tx.execute_batch(DISPATCH_MEMORY_SCHEMA)?;
            let mut identity = AgentIdentityRecord::new(
                &scope.agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            identity.created_at = at;
            identity.updated_at = at;
            upsert_agent_identity_tx(tx, &identity)?;
            let mut source = MessageEnvelope::new(
                &scope.agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "actual dispatch input".into(),
                },
            );
            source.id = "message:dispatch-memory".into();
            source.created_at = at;
            let (source, inserted) = append_message_tx(tx, &source)?;
            assert!(inserted && source.message_seq.is_some());
            let mut turn = TurnRecord::new(&scope.agent_id, "turn:dispatch-memory", 1);
            turn.created_at = at;
            turn.run_id = Some("run:dispatch-memory".into());
            if trigger {
                turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
            }
            turn.input_message_ids.push(source.id.clone());
            upsert_turn_record_tx(tx, &turn)?;
            let actual = committed_turn_record_tx(tx, &scope.agent_id, &turn.turn_id)?;
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
            register_committed_input_evidence_tx(
                tx,
                &scope,
                &[
                    CanonicalInputEvidence::Message(source.clone()),
                    CanonicalInputEvidence::Turn(actual.record),
                ],
            )?;
            let start = DispatchMemoryStart::new(
                EpisodeMemoryKey {
                    agent_id: scope.agent_id.clone(),
                    identity_incarnation: 1,
                    actual_dispatch_turn_id: turn.turn_id,
                },
                EpisodeMemoryInvocation {
                    message_id: source.id.clone(),
                    turn_index: 1,
                },
                &source,
                idle(),
                state.working_memory,
            )?;
            Ok((source, start))
        })?;
        let prepared_start = prepare_start_command(&db, &scope, &start, at)?;
        let original_start = prepared_start.descriptor().clone();
        if install {
            db.transitions_in_scope(&scope)
                .commit_prepared_transition(&prepared_start)?;
        }
        let fixture = Self {
            _directory: directory,
            db,
            scope,
            start,
            source,
            original_start,
            at,
        };
        let mut work = WorkItemRecord::new(
            &fixture.scope.agent_id,
            "original work",
            WorkItemState::Open,
        );
        work.id = "work:dispatch-memory".into();
        work.created_at = at;
        work.updated_at = at;
        let command = WorkItemTransitionCommand {
            agent_id: fixture.scope.agent_id.clone(),
            mutation: WorkItemMutation::Insert { record: work },
            agent_state: None,
            brief_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: true,
            fault: None,
        };
        let prepared = PreparedInputTransition::work(
            fixture.scope.clone(),
            "insert-work".into(),
            at,
            command,
            Default::default(),
            false,
        )?;
        fixture
            .db
            .transitions_in_scope(&fixture.scope)
            .commit_prepared_transition(&prepared)?;
        Ok(fixture)
    }
    fn keys(&self) -> BTreeSet<WorkKey> {
        BTreeSet::from([WorkKey::new(WorkFamily::Work, "work:dispatch-memory")])
    }
    fn frozen(&self, suffix: &str) -> FrozenEpisodeMemoryValues {
        FrozenEpisodeMemoryValues {
            timestamp: self.at,
            initial_builder_id: format!("episode:initial:{suffix}"),
            fallback_builder_id: format!("episode:fallback:{suffix}"),
            next_builder_id: format!("episode:next:{suffix}"),
            audit_event_id: format!("audit:memory:{suffix}"),
        }
    }
    fn descriptor(&self, nonce: &str, ordinal: usize) -> Result<PreparedInputMutation> {
        PreparedInputMutation::new(
            InputMutationFamily::WorkItem,
            nonce.into(),
            vec![self.scope.clone()],
            canonical_command_digest(&("work mutation", ordinal))?,
            self.at,
        )
    }
    fn update(&self, nonce: &str, ordinal: usize, rollback: bool) -> Result<DispatchMemoryRecord> {
        self.db.transaction(|tx| {
            let proof = prepare_work_write_tx(tx, &self.scope, &self.keys())?;
            let mut record: WorkItemRecord = serde_json::from_str(&tx.query_row(
                "SELECT payload_json FROM work_items WHERE work_item_id='work:dispatch-memory'",
                [],
                |row| row.get::<_, String>(0),
            )?)?;
            let revision = record.revision;
            record.revision += 1;
            record.objective = format!("actual mutation {ordinal}");
            record.updated_at += chrono::Duration::milliseconds(1);
            let command = WorkItemTransitionCommand {
                agent_id: self.scope.agent_id.clone(),
                mutation: WorkItemMutation::Update {
                    record,
                    expected_revision: revision,
                },
                agent_state: None,
                brief_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: true,
                fault: None,
            };
            let frozen = self.frozen(nonce);
            let original = PreparedInputMutation::new(
                InputMutationFamily::WorkItem,
                nonce.to_owned(),
                vec![self.scope.clone()],
                canonical_command_digest(&(&command, &self.start, &frozen))?,
                self.at,
            )?;
            let token = prepare_detached_memory_tx(tx, &proof, &original, &frozen)?
                .context("actual dispatch should prepare an obligation")?;
            let value = self
                .db
                .transitions_in_scope(&self.scope)
                .commit_work_item_tx(
                    tx,
                    &command,
                    &ExecutionProtocolTransition::default(),
                    false,
                )?;
            assert!(value.applied);
            let state = ContextDurableRead::new(tx, &self.scope.agent_id)
                .scope_facts()?
                .into_owned_state()?
                .unwrap();
            register_work_writes_tx(
                tx,
                &proof,
                &self.keys(),
                &BTreeSet::new(),
                &state,
                &request(),
            )?;
            let result = record_detached_memory_tx(token, self.keys(), self.start.input_roots())?;
            insert_input_mutation_receipt_tx(
                tx,
                &original,
                &InputMutationOutcome {
                    result: InputMutationResult::Transition,
                    applied: value.applied,
                    delivery_receipt: None,
                    scopes: vec![InputMutationScopeOutcome {
                        scope: self.scope.clone(),
                        install_keys: Default::default(),
                        audit_events: value.effects.audit_events,
                        notify_memory_index: value.effects.notify_memory_index,
                        notify_scheduler: value.effects.notify_scheduler,
                    }],
                },
                self.at,
            )?;
            if rollback {
                anyhow::bail!("original transaction failure after obligation and receipt");
            }
            Ok(result)
        })
    }
    fn read(&self) -> Result<Option<DispatchMemoryRecord>> {
        self.db.transaction(|tx| {
            load_dispatch_memory_tx(tx, self.start.key(), self.start.invocation())
        })
    }
}
fn idle() -> ClosureDecision {
    ClosureDecision {
        outcome: ClosureOutcome::Completed,
        waiting_reason: None,
        work_signal: None,
        runtime_posture: RuntimePosture::Awake,
        evidence: Vec::new(),
    }
}
fn request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    }
}

#[test]
fn checkpoint_retention_uses_original_turn_and_incarnation() -> Result<()> {
    let f = Fixture::new(true)?;
    let check = |scope: &InputEvidenceScope, turn: &str| -> Result<bool> {
        has_dispatch_memory_checkpoint_on(&f.db.connection()?, scope, turn)
    };
    let turn = &f.start.key().actual_dispatch_turn_id;
    assert!(check(&f.scope, turn)?);
    assert!(!check(&f.scope, "another-turn")?);
    let mut other = f.scope.clone();
    other.identity_incarnation += 1;
    assert!(!check(&other, turn)?);
    other.agent_id = "another-agent".into();
    other.identity_incarnation = f.scope.identity_incarnation;
    assert!(!check(&other, turn)?);
    f.update("retention-pending", 1, false)?;
    assert!(matches!(
        f.read()?,
        Some(DispatchMemoryRecord::Pending { .. })
    ));
    assert!(check(&f.scope, turn)?);
    Ok(())
}

#[test]
fn actual_started_turn_owns_seed_but_payload_hint_cannot_install_it() -> Result<()> {
    let f = Fixture::new(true)?;
    assert_eq!(f.read()?.unwrap().start(), Some(&f.start));
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare_start_command(&f.db, &f.scope, &f.start, f.at)?)?;
    let mut changed = f.start.clone();
    changed.prior_closure.evidence.push("later closure".into());
    changed.digest = changed.compute_digest()?;
    assert!(f
        .db
        .transaction(|tx| install_dispatch_memory_start_tx(tx, &changed, &f.original_start))
        .is_err());
    let mut hint = f.start.clone();
    hint.key.actual_dispatch_turn_id = "turn:payload-hint".into();
    hint.digest = hint.compute_digest()?;
    assert!(f
        .db
        .transaction(|tx| install_dispatch_memory_start_tx(tx, &hint, &f.original_start))
        .is_err());
    assert_eq!(f.read()?.unwrap().start(), Some(&f.start));
    Ok(())
}

#[test]
fn detached_commands_append_original_members_without_resetting_first_baseline_or_ids() -> Result<()>
{
    let f = Fixture::new(true)?;
    let first = f.update("work-one", 1, false)?;
    let second = f.update("work-two", 2, false)?;
    assert_eq!(second.start(), first.start());
    assert_eq!(second.obligation().unwrap().frozen(), &f.frozen("work-one"));
    assert_eq!(second.obligation().unwrap().members.len(), 2);
    f.db.transaction(|tx| {
        validate_dispatch_memory_members_tx(tx, &second)?;
        assert!(validate_new_dispatch_tx(tx, &f.scope).is_err());
        let proof = prepare_work_write_tx(tx, &f.scope, &f.keys())?;
        assert!(
            prepare_detached_memory_tx(
                tx,
                &proof,
                &f.descriptor("same-turn-tool", 3)?,
                &f.frozen("three")
            )?
            .is_some(),
            "the same actual Turn still admits tools"
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn whole_business_failure_rolls_back_work_obligation_and_receipt() -> Result<()> {
    let f = Fixture::new(true)?;
    let before = f.db.work_items().latest("work:dispatch-memory")?;
    let seed = f.read()?;
    assert!(
        format!("{:#}", f.update("failed-work", 1, true).unwrap_err())
            .contains("original transaction failure")
    );
    assert_eq!(f.db.work_items().latest("work:dispatch-memory")?, before);
    assert_eq!(f.read()?, seed);
    let missing: bool = f.db.connection()?.query_row("SELECT NOT EXISTS(SELECT 1 FROM input_mutation_receipts WHERE command_nonce='failed-work')", [], |row| row.get(0))?;
    assert!(missing);
    Ok(())
}

#[test]
fn actual_open_dispatch_requires_original_start_and_original_memory_baseline() -> Result<()> {
    let f = Fixture::new(false)?;
    let error = f.update("no-start", 1, false).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::MissingStart)
    ));
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare_start_command(&f.db, &f.scope, &f.start, f.at)?)?;
    f.db.transaction(|tx| {
        let mut state = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        state.working_memory.compression_epoch += 1;
        upsert_agent_state_tx(tx, &state)
    })?;
    let error = f.update("late-baseline", 2, false).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::BaselineAdvanced)
    ));
    Ok(())
}

#[test]
fn same_nonce_changed_command_is_not_a_second_obligation_member() -> Result<()> {
    let f = Fixture::new(true)?;
    let first = f.update("original-work", 1, false)?;
    let error =
        f.db.transaction(|tx| {
            let proof = prepare_work_write_tx(tx, &f.scope, &f.keys())?;
            let token = prepare_detached_memory_tx(
                tx,
                &proof,
                &f.descriptor("original-work", 99)?,
                &f.frozen("different"),
            )?
            .unwrap();
            record_detached_memory_tx(token, f.keys(), f.start.input_roots())
        })
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::MemberConflict)
    ));
    assert_eq!(f.read()?, Some(first));
    Ok(())
}

#[test]
fn no_actual_dispatch_does_not_create_work_memory_responsibility() -> Result<()> {
    let f = Fixture::new(false)?;
    f.db.transaction(|tx| {
        let mut state = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        state.current_execution_binding = None;
        state.current_turn_id = None;
        state.current_run_id = None;
        upsert_agent_state_tx(tx, &state)?;
        let proof = prepare_work_write_tx(tx, &f.scope, &f.keys())?;
        assert!(prepare_detached_memory_tx(
            tx,
            &proof,
            &f.descriptor("direct-work", 1)?,
            &f.frozen("direct")
        )?
        .is_none());
        assert!(unfinished_dispatch_memory_tx(tx, &f.scope)?.is_empty());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn done_requires_real_original_d1_receipt_and_compacts_the_saved_baseline() -> Result<()> {
    let f = Fixture::new(true)?;
    f.update("work-before-finish", 1, false)?;
    let error = f
        .db
        .transaction(|tx| mark_dispatch_memory_done_tx(tx, f.start.key(), f.start.invocation()))
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::MissingReceipt)
    ));
    let done = f.db.transaction(|tx| {
        let state = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        let previous = state.working_memory.current_working_memory.clone();
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &state,
                actual_dispatch_turn_id: &f.start.key.actual_dispatch_turn_id,
                trigger: &f.source,
                prior_closure: f.start.prior_closure(),
                current_closure: &idle(),
                previous_snapshot: &previous,
                current_snapshot: &previous,
            },
            &f.frozen("work-before-finish"),
        );
        let command = episode_memory::EpisodeMemoryTransitionCommand::new(
            f.start.key.clone(),
            f.start.invocation.clone(),
            state,
            plan,
            f.at,
        )?;
        episode_memory::commit_episode_memory_tx(tx, &command)?;
        let done = mark_dispatch_memory_done_tx(tx, f.start.key(), f.start.invocation())?;
        validate_new_dispatch_tx(tx, &f.scope)?;
        assert!(unfinished_dispatch_memory_tx(tx, &f.scope)?.is_empty());
        Ok(done)
    })?;
    assert!(matches!(done, DispatchMemoryRecord::Done { .. }));
    assert!(done.start().is_none() && done.obligation().is_none());
    assert!(
        has_dispatch_memory_checkpoint_on(
            &f.db.connection()?,
            &f.scope,
            &f.start.key().actual_dispatch_turn_id,
        )?,
        "a completed memory checkpoint must retain its execution binding until dispatch state advances"
    );
    assert!(!has_dispatch_memory_checkpoint_on(
        &f.db.connection()?,
        &f.scope,
        "another-turn",
    )?);
    let mut other = f.scope.clone();
    other.identity_incarnation += 1;
    assert!(!has_dispatch_memory_checkpoint_on(
        &f.db.connection()?,
        &other,
        &f.start.key().actual_dispatch_turn_id,
    )?);
    other.agent_id = "another-agent".into();
    other.identity_incarnation = f.scope.identity_incarnation;
    assert!(!has_dispatch_memory_checkpoint_on(
        &f.db.connection()?,
        &other,
        &f.start.key().actual_dispatch_turn_id,
    )?);
    assert_eq!(
        f.db.transaction(|tx| mark_dispatch_memory_done_tx(
            tx,
            f.start.key(),
            f.start.invocation()
        ))?,
        done
    );
    Ok(())
}

#[test]
fn deleting_observes_original_dispatch_but_successor_scope_cannot_inherit_it() -> Result<()> {
    let f = Fixture::new(true)?;
    f.db.transaction(|tx| {
        let mut identity = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .identity
            .unwrap();
        identity.status = AgentRegistryStatus::Deleting;
        identity.revision += 1;
        upsert_agent_identity_tx(tx, &identity)
    })?;
    // Only this existing seed/admission is observed. Ordinary Work writes remain
    // Active-only; M2 will use their separately validated cleanup reducer.
    f.db.transaction(|tx| {
        let proof =
            super::super::work_snapshot::prepare_original_work_write_tx(tx, &f.scope, &f.keys())?;
        assert!(prepare_detached_memory_tx(
            tx,
            &proof,
            &f.descriptor("cleanup-observation", 1)?,
            &f.frozen("cleanup")
        )?
        .is_some());
        assert!(install_dispatch_memory_start_tx(tx, &f.start, &f.original_start).is_err());
        Ok(())
    })?;
    let successor_scope = InputEvidenceScope {
        agent_id: f.scope.agent_id.clone(),
        identity_incarnation: 2,
    };
    f.db.transaction(|tx| {
        validate_new_dispatch_tx(tx, &successor_scope)?;
        let successor_key = EpisodeMemoryKey {
            identity_incarnation: 2,
            ..f.start.key.clone()
        };
        assert!(load_dispatch_memory_tx(tx, &successor_key, &f.start.invocation)?.is_none());
        assert!(prepare_work_write_tx(tx, &successor_scope, &f.keys()).is_err());
        Ok(())
    })?;
    f.db.transaction(|tx| {
        let mut identity = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .identity
            .unwrap();
        identity.status = AgentRegistryStatus::Deleted;
        identity.revision += 1;
        identity.updated_at += chrono::Duration::milliseconds(1);
        upsert_agent_identity_tx(tx, &identity)?;
        assert!(super::super::work_snapshot::prepare_original_work_write_tx(
            tx,
            &f.scope,
            &f.keys()
        )
        .is_err());
        assert!(install_dispatch_memory_start_tx(tx, &f.start, &f.original_start).is_err());
        Ok(())
    })?;
    // Historical reading still returns original metadata. It confers no current
    // authority and does not install an old State into a successor owner.
    assert!(f.read()?.is_some());
    Ok(())
}

#[test]
fn claim_ack_before_started_turn_keeps_work_command_as_no_dispatch() -> Result<()> {
    let f = Fixture::new(false)?;
    let before = f.db.agent_states().latest(&f.scope.agent_id)?.unwrap();
    let mut idle = before.clone();
    idle.current_run_id = None;
    idle.current_turn_id = None;
    idle.current_execution_binding = None;
    let posture = PreparedInputTransition::posture(
        f.scope.clone(),
        "claim-window-idle".into(),
        f.at,
        AgentPostureTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            expected_incarnation: Some(1),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(before)),
                record: Box::new(idle),
            },
            audit_events: Vec::new(),
            fault: None,
        },
        None,
    )?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&posture)?;
    let mut source = f.source.clone();
    source.id = "message:claim-only".into();
    source.message_seq = None;
    source.turn_id = None;
    let before = f.db.agent_states().latest(&f.scope.agent_id)?.unwrap();
    let mut queued_state = before.clone();
    queued_state.pending = 1;
    let queued = QueueEntryRecord {
        message_id: source.id.clone(),
        agent_id: f.scope.agent_id.clone(),
        priority: source.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: f.at,
        updated_at: f.at,
    };
    let command = QueueTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        operation: QueueOperation::Admit,
        mutation: QueueMutation::Upsert(queued.clone()),
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(before)),
            record: Box::new(queued_state),
        }),
        message_evidence: vec![source],
        transcript_entries: Vec::new(),
        turn_record: None,
        audit_events: Vec::new(),
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    };
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&PreparedInputTransition::queue(
            f.scope.clone(),
            "claim-window-admit".into(),
            f.at,
            PreparedQueueTransition::new(command),
        )?)?;
    let before = f.db.agent_states().latest(&f.scope.agent_id)?.unwrap();
    let mut claimed_state = before.clone();
    claimed_state.current_run_id = Some("run:claim-only".into());
    claimed_state.status = AgentStatus::AwakeRunning;
    claimed_state.pending = 0;
    let mut claimed = queued.clone();
    claimed.status = QueueEntryStatus::Dequeued;
    let command = QueueTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        operation: QueueOperation::Claim,
        mutation: QueueMutation::Consume(claimed),
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(before)),
            record: Box::new(claimed_state),
        }),
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: None,
        audit_events: Vec::new(),
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    };
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&PreparedInputTransition::queue(
            f.scope.clone(),
            "claim-window-claim".into(),
            f.at,
            PreparedQueueTransition::new(command),
        )?)?;
    f.db.transaction(|tx| {
        let actual = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        assert_eq!(actual.current_run_id.as_deref(), Some("run:claim-only"));
        assert!(actual.current_turn_id.is_none() && actual.current_execution_binding.is_none());
        let proof = prepare_work_write_tx(tx, &f.scope, &f.keys())?;
        assert!(prepare_detached_memory_tx(
            tx,
            &proof,
            &f.descriptor("claim-window-work", 1)?,
            &f.frozen("claim-window")
        )?
        .is_none());
        assert!(unfinished_dispatch_memory_tx(tx, &f.scope)?.is_empty());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn seed_cannot_freeze_a_source_different_from_the_actual_binding() -> Result<()> {
    let f = Fixture::new(false)?;
    f.db.transaction(|tx| {
        let mut actual = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        actual
            .current_execution_binding
            .as_mut()
            .unwrap()
            .source_message_id = "message:different-source".into();
        upsert_agent_state_tx(tx, &actual)?;
        assert!(install_dispatch_memory_start_tx(tx, &f.start, &f.original_start).is_err());
        assert!(load_dispatch_memory_tx(tx, f.start.key(), f.start.invocation())?.is_none());
        Ok(())
    })?;
    Ok(())
}

fn prepare_start_command(
    db: &RuntimeDb,
    scope: &InputEvidenceScope,
    start: &DispatchMemoryStart,
    at: DateTime<Utc>,
) -> Result<PreparedInputTransition> {
    db.transaction(|tx| {
        let read = ContextDurableRead::new(tx, &scope.agent_id);
        let state = read
            .scope_facts()?
            .into_owned_state()?
            .context("fixture State missing")?;
        let turn = read
            .turn_by_id(&start.key.actual_dispatch_turn_id)?
            .context("fixture Turn missing")?;
        PreparedInputTransition::posture(
            scope.clone(),
            "actual-start-posture".into(),
            at,
            AgentPostureTransitionCommand {
                agent_id: scope.agent_id.clone(),
                expected_incarnation: Some(scope.identity_incarnation),
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
            start: Box::new(start.clone()),
        })
    })
}
#[path = "dispatch_memory_binding_tests.rs"]
mod binding_tests;

#[path = "dispatch_memory_retired_tests.rs"]
mod retired_tests;

#[path = "dispatch_memory_auto_tests.rs"]
mod automatic_tests;

#[path = "dispatch_memory_task_budget_tests.rs"]
mod task_budget_tests;

impl RuntimeDb {
    /// The original canonical assignment + typed Started posture, with no
    /// runtime construction or guessed physical producer completion.
    pub(crate) fn original_cold_started_stop_host_fixture(
        &self,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        let at = Utc::now() - chrono::Duration::seconds(5);
        let start = self.transaction(|tx| {
            let mut source = MessageEnvelope::new(
                &scope.agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "original cold actual dispatch".into(),
                },
            );
            source.id = "stop-host-original-dispatch-source".into();
            source.created_at = at;
            let (source, inserted) = append_message_tx(tx, &source)?;
            ensure!(inserted, "original source duplicated");
            let mut turn = TurnRecord::new(&scope.agent_id, "stop-host-original-dispatch-turn", 1);
            turn.created_at = at;
            turn.run_id = Some("stop-host-original-dispatch-run".into());
            turn.input_message_ids.push(source.id.clone());
            turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
            upsert_turn_record_tx(tx, &turn)?;
            let actual = committed_turn_record_tx(tx, &scope.agent_id, &turn.turn_id)?;
            let mut state = ContextDurableRead::new(tx, &scope.agent_id)
                .scope_facts()?
                .into_owned_state()?
                .context("cold original State missing")?;
            state.status = AgentStatus::AwakeRunning;
            state.turn_index = 1;
            state.total_message_count += 1;
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
            register_committed_input_evidence_tx(
                tx,
                scope,
                &[
                    CanonicalInputEvidence::Message(source.clone()),
                    CanonicalInputEvidence::Turn(actual.record),
                ],
            )?;
            DispatchMemoryStart::new(
                EpisodeMemoryKey {
                    agent_id: scope.agent_id.clone(),
                    identity_incarnation: scope.identity_incarnation,
                    actual_dispatch_turn_id: turn.turn_id,
                },
                EpisodeMemoryInvocation {
                    message_id: source.id.clone(),
                    turn_index: 1,
                },
                &source,
                idle(),
                state.working_memory,
            )
        })?;
        let prepared = prepare_start_command(self, scope, &start, at)?;
        ensure!(
            matches!(
                self.transitions_in_scope(scope)
                    .commit_prepared_transition(&prepared)?,
                crate::runtime_db::transitions::InputMutationExecution::Committed { .. }
            ),
            "original Start missing"
        );
        Ok(())
    }
}
