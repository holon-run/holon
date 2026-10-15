use super::*;

#[test]
fn original_external_task_event_loans_debit_actual_memory_once_before_first_input_scope(
) -> Result<()> {
    use crate::runtime::current_event_queries::CurrentEventQueryRegistry;
    use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
    use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;
    use crate::runtime_db::task_snapshot::{
        read_preparation::TaskReadMaterial, TaskDomainLoadRequest,
    };
    for positive in [true, false] {
        let f = Fixture::new(true)?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let registry = CurrentEventQueryRegistry::default();
        let lease = registry.register(&f.scope, 1, 0, &ledger, (64 * 1024 * 1024, 65536))?;
        let roles = registry
            .snapshot(&f.scope, 1, &ledger, (64 * 1024 * 1024, 65536))?
            .context("Event roles missing")?;
        let baseline = ledger.report()?.observed_bytes;
        let memory_request = WorkDomainLoadRequest {
            capture: WorkCaptureRequest::current(),
            byte_limit: 2 * 1024 * 1024,
            record_limit: 65536,
        };
        let task_request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            byte_limit: 64 * 1024 * 1024,
            record_limit: 65536,
        };
        let input_request = InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            byte_limit: 64 * 1024 * 1024,
            record_limit: 65536,
        };
        let result = f.db.transaction(|tx| {
            let intent = DispatchMemoryIntent::Started {
                start: Box::new(f.start.clone()),
            };
            let mut memory =
                DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &memory_request, &ledger)?;
            let remaining = memory.remaining_material_limits()?;
            let keep = if positive { 1024 * 1024 } else { 8192 };
            let allocation = remaining
                .0
                .checked_sub(keep + 256)
                .context("Memory remaining too small")?;
            memory.additional_material(allocation, 1, 1)?;
            let held = vec![0u8; allocation];
            let before = memory.material_usage_for_test();
            if !positive {
                tx.execute_batch(
                    "ALTER TABLE agent_states RENAME TO forbidden_external_task_memory_state",
                )?;
            }
            let mut task = TaskReadMaterial::new(&f.scope, &task_request, Some(&ledger))
                .with_memory_budget(Some(&mut memory));
            for _ in 0..2 {
                task.with_input_material_for_request(
                    &f.scope,
                    &ledger,
                    &input_request,
                    Some(&roles),
                    |input| input.scope_facts_tx(tx).map(|_| ()),
                )?;
            }
            let usage = task.material_estimate();
            drop(task);
            assert_eq!(
                memory.material_usage_for_test(),
                (before.0 + usage.bytes, before.1 + usage.records),
                "the same Event guard and Input allocation cannot debit Memory twice"
            );
            assert_eq!(held.len(), allocation);
            drop((held, memory));
            Ok(())
        });
        if positive {
            result?;
        } else {
            let error = result
                .err()
                .context("Memory prior was skipped before forbidden State")?;
            assert!(
                matches!(
                    error.downcast_ref::<DispatchMemoryError>(),
                    Some(DispatchMemoryError::ResourceExhausted)
                ),
                "{error:#}"
            );
        }
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop((roles, lease, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
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
        Self::with_trigger(install, false)
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
                .state
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
            .state
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
            .state
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
            .state
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
            .state
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
            .state
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
        let state = read.scope_facts()?.state.context("fixture State missing")?;
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
                .state
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

#[test]
fn work_read_material_debits_original_memory_allowance_without_duplicate_owned_guard() -> Result<()>
{
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let policy = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Started {
            start: Box::new(f.start.clone()),
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &policy, &ledger)?;
        let baseline = ledger.report()?.observed_bytes;
        let before = memory.work_request(&policy)?.byte_limit;
        let loaded = crate::runtime_db::work_snapshot::prepare_work_domain_snapshot_with_budget_tx(
            tx,
            &f.scope,
            &policy,
            false,
            &ledger,
            Some(&mut memory),
        )?
        .require_current()?;
        let after = memory.work_request(&policy)?.byte_limit;
        assert!(after < before);
        assert!(ledger.report()?.observed_bytes > baseline);
        drop(loaded);
        assert_eq!(
            ledger.report()?.observed_bytes,
            baseline,
            "Memory debit must not mint another guard for the Work allocation"
        );
        let remaining = memory.work_request(&policy)?.byte_limit;
        let held_bytes = remaining
            .checked_sub(1024)
            .context("fixture has no remaining allowance")?;
        memory.additional_material(held_bytes, 1, 1)?;
        let held = vec![0u8; held_bytes];
        let retained = ledger.report()?.observed_bytes;
        let error = crate::runtime_db::work_snapshot::prepare_work_domain_snapshot_with_budget_tx(
            tx,
            &f.scope,
            &policy,
            false,
            &ledger,
            Some(&mut memory),
        )
        .err()
        .context("Work phase alone fits but shared Memory allowance is exhausted")?;
        assert!(
            matches!(
                error.downcast_ref::<DispatchMemoryError>(),
                Some(DispatchMemoryError::ResourceExhausted)
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, retained);
        drop(held);
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn actual_work_preflight_releases_temporary_memory_debit_but_keeps_original_proof() -> Result<()> {
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let policy = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Started {
            start: Box::new(f.start.clone()),
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &policy, &ledger)?;
        let baseline = ledger.report()?.observed_bytes;
        let proof = crate::runtime_db::work_snapshot::prepare_work_write_with_budget_tx(
            tx,
            &f.scope,
            &f.keys(),
            &policy,
            false,
            &ledger,
            Some(&mut memory),
        )?;
        let held = ledger.report()?.observed_bytes;
        assert!(held > baseline);
        let remaining = memory.work_request(&policy)?;
        let peak =
            crate::runtime_db::work_snapshot::preflight_work_write_snapshot_with_shared_phase_tx(
                tx,
                &proof,
                &remaining,
                Some(&mut memory),
            )?;
        assert!(peak.0 > 0 && peak.1 > 0);
        assert_eq!(ledger.report()?.observed_bytes, held);
        let after = memory.work_request(&policy)?;
        assert_eq!(
            (after.byte_limit, after.record_limit),
            (remaining.byte_limit, remaining.record_limit),
            "only the completed temporary preflight returns its remaining allowance"
        );
        drop(proof);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn actual_work_write_proof_debits_original_memory_without_a_duplicate_guard() -> Result<()> {
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let policy = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Started {
            start: Box::new(f.start.clone()),
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &policy, &ledger)?;
        let baseline = ledger.report()?.observed_bytes;
        let before = memory.work_request(&policy)?.byte_limit;
        let proof = crate::runtime_db::work_snapshot::prepare_work_write_with_budget_tx(
            tx,
            &f.scope,
            &f.keys(),
            &policy,
            false,
            &ledger,
            Some(&mut memory),
        )?;
        assert!(memory.work_request(&policy)?.byte_limit < before);
        assert!(ledger.report()?.observed_bytes > baseline);
        drop(proof);
        assert_eq!(
            ledger.report()?.observed_bytes,
            baseline,
            "Memory only debits the real Work allocation; it does not own a duplicate guard"
        );
        let remaining = memory.work_request(&policy)?.byte_limit;
        let bytes = remaining
            .checked_sub(1024)
            .context("fixture has no remaining Memory allowance")?;
        memory.additional_material(bytes, 1, 1)?;
        let held = vec![0u8; bytes];
        let retained = ledger.report()?.observed_bytes;
        let error = crate::runtime_db::work_snapshot::prepare_work_write_with_budget_tx(
            tx,
            &f.scope,
            &f.keys(),
            &policy,
            false,
            &ledger,
            Some(&mut memory),
        )
        .err()
        .context("Work alone fits but original transaction remaining must reject before decoder")?;
        assert!(
            matches!(
                error.downcast_ref::<DispatchMemoryError>(),
                Some(DispatchMemoryError::ResourceExhausted)
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, retained);
        drop(held);
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

fn input_memory_request(
    f: &Fixture,
) -> crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
    crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            &f.source.id,
        )]),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    }
}
#[test]
fn input_material_debits_original_memory_once_and_keeps_only_its_actual_guard() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial;
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("input-material-once"),
        };
        let policy = WorkDomainLoadRequest {
            capture: WorkCaptureRequest::current(),
            byte_limit: 64 * 1024 * 1024,
            record_limit: 65_536,
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &policy, &ledger)?;
        let before = memory.work_request(&policy)?;
        let observed = ledger.report()?.observed_bytes;
        let request = input_memory_request(&f);
        let mut input = InputReadMaterial::new_with_memory(
            &f.scope,
            &request,
            &ledger,
            (0, 0),
            Some(&mut memory),
        )?;
        input.observe_projected(4096, 8)?;
        let cost = input.usage();
        assert!(ledger.report()?.observed_bytes > observed);
        drop(input);
        assert_eq!(
            ledger.report()?.observed_bytes,
            observed,
            "Memory borrows the actual Input guard; no duplicated observation remains"
        );
        let after = memory.work_request(&policy)?;
        assert_eq!(before.byte_limit - after.byte_limit, cost.0);
        assert_eq!(before.record_limit - after.record_limit, cost.1);
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
#[test]
fn input_material_rejects_foreign_memory_scope_or_ledger_before_decoder() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial;
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("input-material-owner"),
        };
        let policy = WorkDomainLoadRequest {
            capture: WorkCaptureRequest::current(),
            byte_limit: 64 * 1024 * 1024,
            record_limit: 65_536,
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &policy, &ledger)?;
        let before = memory.work_request(&policy)?;
        let observed = ledger.report()?.observed_bytes;
        let request = input_memory_request(&f);
        let foreign = InputEvidenceScope {
            agent_id: "foreign-input-scope".into(),
            identity_incarnation: 1,
        };
        assert!(InputReadMaterial::new_with_memory(
            &foreign,
            &request,
            &ledger,
            (0, 0),
            Some(&mut memory)
        )
        .is_err());
        assert_eq!(ledger.report()?.observed_bytes, observed);
        let other = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
            Default::default(),
        )?;
        assert!(InputReadMaterial::new_with_memory(
            &f.scope,
            &request,
            &other,
            (0, 0),
            Some(&mut memory)
        )
        .is_err());
        assert_eq!(other.report()?.observed_bytes, 0);
        assert_eq!(memory.work_request(&policy)?.byte_limit, before.byte_limit);
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
#[test]
fn input_source_raw_guard_meets_same_memory_remaining_before_malformed_payload_decode() -> Result<()>
{
    use crate::runtime_db::input_evidence_snapshot::{
        prepare_input_evidence_snapshot_with_material_tx, read_material::InputReadMaterial,
    };
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let original: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM messages WHERE evidence_id=?1",
        [&f.source.id],
        |row| row.get(0),
    )?;
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("input-material-remaining"),
        };
        let policy = WorkDomainLoadRequest {
            capture: WorkCaptureRequest::current(),
            byte_limit: 64 * 1024 * 1024,
            record_limit: 65_536,
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &policy, &ledger)?;
        let available = memory.work_request(&policy)?.byte_limit;
        let held_bytes = available
            .checked_sub(64 * 1024)
            .context("fixture Memory allowance too small")?;
        memory.additional_material(held_bytes, 1, 1)?;
        let held = vec![0u8; held_bytes];
        let observed = ledger.report()?.observed_bytes;
        tx.execute(
            "UPDATE messages SET payload_json=?2 WHERE evidence_id=?1",
            params![f.source.id, "!".repeat(256 * 1024)],
        )?;
        let request = input_memory_request(&f);
        let mut input = InputReadMaterial::new_with_memory(
            &f.scope,
            &request,
            &ledger,
            (0, 0),
            Some(&mut memory),
        )?;
        let error = prepare_input_evidence_snapshot_with_material_tx(
            tx, &f.scope, &request, false, &mut input,
        )
        .err()
        .context("Input ignored original Memory remaining")?;
        assert!(
            matches!(
                error.downcast_ref::<DispatchMemoryError>(),
                Some(DispatchMemoryError::ResourceExhausted)
            ),
            "{error:#}"
        );
        drop(input);
        assert_eq!(ledger.report()?.observed_bytes, observed);
        tx.execute(
            "UPDATE messages SET payload_json=?2 WHERE evidence_id=?1",
            params![f.source.id, original],
        )?;
        drop((held, memory));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn original_memory_prior_bounds_actual_current_decoder_before_source_sql() -> Result<()> {
    use crate::runtime::current_read_facts::{CurrentReadError, CurrentReadRequest};
    use crate::runtime_db::{
        current_read_snapshot::{
            prepare_current_read_snapshot_tx, prepare_current_read_using_input_material_tx,
        },
        input_evidence_snapshot::{read_material::InputReadMaterial, InputEvidenceLoadRequest},
    };
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let state =
        f.db.agent_states()
            .latest(&f.scope.agent_id)?
            .context("original State missing")?;
    let work = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        byte_limit: 2 * 1024 * 1024,
        record_limit: 65536,
    };
    let policy = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    for positive in [true, false] {
        let request = CurrentReadRequest::current(work.byte_limit, work.record_limit);
        drop(f.db.transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &f.scope, &state, &request, &ledger)
        })?);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let result = f.db.transaction(|tx| {
            let intent = DispatchMemoryIntent::Started {
                start: Box::new(f.start.clone()),
            };
            let mut memory =
                DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &work, &ledger)?;
            let remaining = memory.remaining_material_limits()?;
            let keep = if positive { 1024 * 1024 } else { 6144 };
            let allocation = remaining
                .0
                .checked_sub(keep + 256)
                .context("original Memory allowance too small")?;
            memory.additional_material(allocation, 1, 1)?;
            let held_memory = vec![0u8; allocation];
            let prior = memory.material_usage_for_test();
            let mut input = InputReadMaterial::new_with_memory(
                &f.scope,
                &policy,
                &ledger,
                (0, 0),
                Some(&mut memory),
            )?;
            let mut current_request =
                CurrentReadRequest::current(policy.byte_limit, policy.record_limit);
            input.narrow_current_request(&mut current_request)?;
            assert!(current_request.byte_limit < keep);
            if !positive {
                // The original full Input/Current policies fit separately. The
                // real Memory prior must reject before touching State SQL.
                tx.execute_batch(
                    "ALTER TABLE agent_states RENAME TO memory_current_state_forbidden",
                )?;
            }
            let current = prepare_current_read_using_input_material_tx(
                tx,
                &f.scope,
                &state,
                &current_request,
                &ledger,
                &mut input,
                false,
            )?;
            let usage = input.usage();
            drop((current, input));
            assert_eq!(
                memory.material_usage_for_test(),
                (prior.0 + usage.0, prior.1 + usage.1)
            );
            drop((held_memory, memory));
            Ok(())
        });
        if positive {
            result?;
        } else {
            let error = result.err().context("original Memory prior was ignored")?;
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::ResourceExhausted)
                ),
                "{error:#}"
            );
            assert!(
                f.db.agent_states().latest(&f.scope.agent_id)?.is_some(),
                "failed phase must restore original schema/State"
            );
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn twelve_actual_qualified_tool_reports_release_source_scratch_from_original_memory_phase(
) -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::{
        read_material::InputReadMaterial, tool_run_report::prepare_qualified_tool_run_report_on,
        InputEvidenceLoadRequest,
    };
    let f = Fixture::new(true)?;
    let mut records = Vec::new();
    for index in 0..12 {
        let original = crate::types::ToolExecutionRecord {
            id: format!("actual-streamed-tool-{index}"),
            agent_id: f.scope.agent_id.clone(),
            work_item_id: None,
            turn_index: 1,
            turn_id: None,
            tool_name: "ExecCommand".into(),
            created_at: f.at,
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: crate::types::ToolExecutionStatus::Success,
            input: serde_json::json!({"cmd":"printf original"}),
            output: serde_json::json!({"stdout":"x".repeat(10 * 1024 * 1024)}),
            summary: "original finite prompt".into(),
            invocation_surface: None,
        };
        let record = f.db.transaction(|tx| {
            crate::runtime_db::evidence::insert_tool_evidence_tx(tx, &original)?;
            let mut delta = register_committed_input_evidence_tx(
                tx,
                &f.scope,
                &[CanonicalInputEvidence::ToolExecution(original.clone())],
            )?;
            assert_eq!(delta.records.len(), 1);
            std::sync::Arc::try_unwrap(delta.records.pop().unwrap())
                .map_err(|_| anyhow::anyhow!("actual fixture writer retained Tool Arc"))
        })?;
        records.push(record);
    }
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let work_request = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    let input_request = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Started {
            start: Box::new(f.start.clone()),
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &work_request, &ledger)?;
        let before = memory.material_usage_for_test();
        let baseline = ledger.report()?.observed_bytes;
        let mut input = InputReadMaterial::new_with_memory(
            &f.scope,
            &input_request,
            &ledger,
            (0, 0),
            Some(&mut memory),
        )?;
        let mut reports = Vec::new();
        for record in &records {
            reports.push(prepare_qualified_tool_run_report_on(
                tx, record, &mut input,
            )?);
            assert!(
                input.usage().0 < 2 * 1024 * 1024,
                "original source scratch must retire after each actual finite report"
            );
        }
        let retained = input.usage();
        let guards = input.into_guards();
        assert_eq!(
            memory.material_usage_for_test(),
            (before.0 + retained.0, before.1 + retained.1)
        );
        assert_eq!(reports.len(), 12);
        for (record, report) in records.iter_mut().zip(reports.drain(..)) {
            record.tool_run_report = Some(report);
            assert_eq!(
                record.tool_run_report()?,
                &crate::runtime::run_report_projection::ToolRunReportProjection::default()
            );
        }
        assert!(ledger.report()?.observed_bytes > baseline);
        assert!(ledger.report()?.observed_bytes < baseline + 2 * 1024 * 1024);
        let weak =
            std::sync::Arc::downgrade(guards.last().context("retained report guard missing")?);
        let last = guards.clone();
        for record in &mut records {
            record.tool_run_report = None;
        }
        drop((guards, reports));
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })?;
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Started {
            start: Box::new(f.start.clone()),
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &work_request, &ledger)?;
        memory.additional_material(50 * 1024 * 1024, 1, 1)?;
        let held = vec![0u8; 50 * 1024 * 1024];
        let memory_before = memory.material_usage_for_test();
        let ledger_before = ledger.report()?.observed_bytes;
        let mut input = InputReadMaterial::new_with_memory(
            &f.scope,
            &input_request,
            &ledger,
            (0, 0),
            Some(&mut memory),
        )?;
        let before = input.usage();
        let error = prepare_qualified_tool_run_report_on(tx, &records[0], &mut input)
            .err()
            .context("Tool reader alone fits but actual Memory prior exhausts the same phase")?;
        assert!(
            matches!(
                error.downcast_ref::<DispatchMemoryError>(),
                Some(DispatchMemoryError::ResourceExhausted)
            ),
            "{error:#}"
        );
        assert_eq!(
            input.usage(),
            before,
            "failed decoder must also retire its original temporary debit"
        );
        drop(input);
        assert_eq!(
            memory.material_usage_for_test(),
            (memory_before.0 + before.0, memory_before.1 + before.1)
        );
        assert_eq!(ledger.report()?.observed_bytes, ledger_before);
        assert!(records
            .iter()
            .all(|record| record.tool_run_report.is_none()));
        drop((held, memory));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn original_memory_and_input_task_work_decoders_share_actual_allocations_once() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::{
        read_material::InputReadMaterial, InputEvidenceLoadRequest,
    };
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let state =
        f.db.agent_states()
            .latest(&f.scope.agent_id)?
            .context("original State missing")?;
    let request = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        byte_limit: 2 * 1024 * 1024,
        record_limit: 65536,
    };
    let input_request = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    for task in [false, true] {
        for positive in [true, false] {
            let result=f.db.transaction(|tx| {
                let intent=DispatchMemoryIntent::Started {start:Box::new(f.start.clone())};
                let mut memory=DispatchMemoryTransactionBudget::prepare_tx(tx,&intent,&request,&ledger)?;
                let remaining=memory.remaining_material_limits()?;
                let keep=if positive {1024*1024} else {8192};
                let allocation=remaining.0.checked_sub(keep+256).context("original Memory allowance too small")?;
                memory.additional_material(allocation,1,1)?;
                let held=vec![0u8;allocation];
                let before=memory.material_usage_for_test();
                let mut input=InputReadMaterial::new_with_memory(&f.scope,&input_request,&ledger,(0,0),Some(&mut memory))?;
                if task {
                    let actual=input.with_task_material(&f.scope,&ledger,|material|
                        crate::runtime_db::task_recovery::prepare_original_task_domains_using_material_tx(
                            tx,&f.scope,&state,&crate::runtime::resident_task::TaskCaptureRequest::current(),
                            &[],input_request.record_limit,input_request.byte_limit,&ledger,None,material,
                        )
                    )?;
                    drop(actual);
                } else {
                    let actual=input.prepare_original_work_tx(tx,&f.scope,&state,
                        &WorkCaptureRequest::current(),None,&[],false,&ledger)?;
                    drop(actual);
                }
                let usage=input.usage();
                drop(input);
                assert_eq!(memory.material_usage_for_test(),(before.0+usage.0,before.1+usage.1),
                    "original Task/Work/Input allocation must debit Memory exactly once");
                assert_eq!(held.len(),allocation);
                drop((held,memory));
                Ok(())
            });
            if positive {
                result?;
            } else {
                let error = result.err().context("actual Memory prior was bypassed")?;
                assert!(
                    matches!(
                        error.downcast_ref::<DispatchMemoryError>(),
                        Some(DispatchMemoryError::ResourceExhausted)
                    ) || matches!(
                        error.downcast_ref::<crate::runtime_db::work_snapshot::WorkSnapshotError>(),
                        Some(
                            crate::runtime_db::work_snapshot::WorkSnapshotError::ResourceExhausted(
                                _
                            )
                        )
                    ),
                    "{error:#}"
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[path = "dispatch_memory_scheduling_joint_tests.rs"]
mod scheduling_joint;

#[test]
fn original_native_input_guards_borrow_started_memory_once_before_the_first_key_copy() -> Result<()> {
    use crate::runtime::{input_queries::{source_roles_for_fixture, InputQuerySpec}, mutation_owner::MutationOwnerRegistry};
    use crate::runtime_db::input_evidence_snapshot::{
        native_birth::NativeInputWriteProof, read_material::InputReadMaterial,
    };
    let f = Fixture::new(true)?;
    let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&f.scope.agent_id, f.scope.identity_incarnation)?;
    let required = [InputEvidenceKey::new(InputEvidenceFamily::Message, "actual-selector-only")];
    let (reservation, lease, roles) = source_roles_for_fixture(&owner, &f.scope,
        InputQuerySpec { required_keys: &required, birth_after: Some(0) }, &ledger,
        (64 * 1024 * 1024, 65536))?;
    drop(roles);
    let source = reservation.input_query_source(&f.scope, &ledger, (64 * 1024 * 1024, 65536))?;
    let source_baseline = ledger.report()?.observed_bytes;
    let request = input_memory_request(&f);
    let measured = f.db.transaction(|tx| {
        let mut input = InputReadMaterial::new(&f.scope, &request, &ledger, (0, 0))?;
        let native = NativeInputWriteProof::prepare_using_input_material_tx(tx, &f.scope,
            |visit| visit(InputEvidenceFamily::Message, "actual-native-memory"), &mut input, Some(&source))?
            .context("original Native source is not installed")?;
        let usage = input.usage();
        drop((native, input));
        Ok(usage)
    })?;
    assert_eq!(ledger.report()?.observed_bytes, source_baseline);
    for positive in [true, false] {
        let before_sql = f.db.connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM input_query_birth_members),COALESCE((SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2),0)",
            rusqlite::params![f.scope.agent_id,f.scope.identity_incarnation],
            |row|Ok((row.get::<_,usize>(0)?,row.get::<_,usize>(1)?,row.get::<_,u64>(2)?)))?;
        let result:Result<()> = f.db.transaction(|tx| {
            let policy = WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:2*1024*1024,record_limit:65536};
            let intent = DispatchMemoryIntent::Deferred {binding:DispatchMemoryBinding::from_start(&f.start),frozen:f.frozen("native-first-copy")};
            let mut memory = DispatchMemoryTransactionBudget::prepare_tx(tx,&intent,&policy,&ledger)?;
            let remaining = memory.remaining_material_limits()?;
            let keep = if positive {measured.0.checked_add(4096)} else {measured.0.checked_sub(1)}
                .context("actual Native measurement overflow")?;
            let allocation = remaining.0.checked_sub(keep.checked_add(256)
                .context("fixture Memory native floor overflow")?)
                .context("fixture Memory prior too small")?;
            assert!(allocation > 256);
            let reservation_before = memory.material_usage_for_test();
            memory.additional_material(allocation,1,1)?;
            assert_eq!(memory.remaining_material_limits()?.0, keep);
            assert_eq!(memory.material_usage_for_test(),
                (reservation_before.0 + allocation + 256, reservation_before.1 + 1));
            let held = vec![0u8;allocation];
            let before = memory.material_usage_for_test();
            let mut input = InputReadMaterial::new_with_memory(&f.scope,&request,&ledger,(0,0),Some(&mut memory))?;
            let native = NativeInputWriteProof::prepare_using_input_material_tx(tx,&f.scope,
                |visit|visit(InputEvidenceFamily::Message,"actual-native-memory"),&mut input,Some(&source))?
                .context("original Native source is not installed")?;
            let usage = input.usage();
            assert_eq!(usage,measured);
            let native = native.retain_input_phase(input)?;
            assert!(ledger.report()?.observed_bytes > source_baseline);
            assert_eq!(memory.material_usage_for_test(),(before.0+usage.0,before.1+usage.1));
            let mut message = f.source.clone();message.id="actual-native-memory".into();message.message_seq=None;
            let (actual, inserted) = append_message_tx(tx,&message)?;assert!(inserted);
            let mut delta = register_committed_input_evidence_tx(tx,&f.scope,&[CanonicalInputEvidence::Message(actual)])?;
            native.finish_delta_tx(tx,&mut delta)?;
            let clone = delta.clone();drop(delta);
            assert!(ledger.report()?.observed_bytes > source_baseline);
            drop((clone,held,memory));
            Ok(())
        });
        if positive {result?;} else {
            let error = result.err().context("actual Memory prior was bypassed")?;
            assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
                Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_)))
                || matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted)),"{error:#}");
            let after_sql = f.db.connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM input_query_birth_members),COALESCE((SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2),0)",
                rusqlite::params![f.scope.agent_id,f.scope.identity_incarnation],
                |row|Ok((row.get::<_,usize>(0)?,row.get::<_,usize>(1)?,row.get::<_,u64>(2)?)))?;
            assert_eq!(after_sql,before_sql,"first Native copy rejection cannot perform business SQL");
        }
        assert_eq!(ledger.report()?.observed_bytes,source_baseline);
    }
    drop((source,lease,reservation));
    assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}


#[test]
fn original_tool_writer_resumes_the_same_native_started_memory_phase_and_never_registers_twice() -> Result<()> {
    use crate::runtime::{input_queries::{source_roles_for_fixture,InputQuerySpec},mutation_owner::MutationOwner};
    use crate::runtime_db::input_evidence_snapshot::{native_birth::NativeInputWriteProof,
        read_material::InputReadMaterial,tool_writer::{InputToolWriter,OriginalToolInputWriter},
        register_committed_input_evidence_with_tool_material_tx};
    let f=Fixture::new(true)?;
    let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let owner=MutationOwner::standalone(&f.scope.agent_id,1);
    let (reservation,lease,roles)=source_roles_for_fixture(&owner,&f.scope,
        InputQuerySpec{birth_after:Some(0),..Default::default()},&ledger,(64*1024*1024,65536))?;
    drop(roles);let source=reservation.input_query_source(&f.scope,&ledger,(64*1024*1024,65536))?;
    let baseline=ledger.report()?.observed_bytes;
    let tool=crate::types::ToolExecutionRecord {
        id:"actual-native-memory-tool".into(),agent_id:f.scope.agent_id.clone(),work_item_id:None,
        turn_index:1,turn_id:None,tool_name:"ExecCommand".into(),created_at:f.at,completed_at:Some(f.at),duration_ms:0,
        authority_class:AuthorityClass::RuntimeInstruction,status:crate::types::ToolExecutionStatus::Success,
        input:serde_json::json!({"cmd":"printf actual"}),output:serde_json::json!({"stdout":"x".repeat(10*1024*1024)}),
        summary:"actual finite Memory Tool".into(),invocation_surface:None,
    };
    for positive in [false,true] {
        let result:Result<()> = f.db.transaction(|tx| {
            let policy=WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536};
            let intent=DispatchMemoryIntent::Started{start:Box::new(f.start.clone())};
            let mut memory=DispatchMemoryTransactionBudget::prepare_tx(tx,&intent,&policy,&ledger)?;
            let held=if positive{Vec::new()}else{
                memory.additional_material(50*1024*1024,1,1)?;vec![0u8;50*1024*1024]
            };
            let input_request=input_memory_request(&f);
            let mut input=InputReadMaterial::new_with_memory(&f.scope,&input_request,&ledger,(0,0),Some(&mut memory))?;
            let native=NativeInputWriteProof::prepare_using_input_material_tx(tx,&f.scope,
                |visit|visit(InputEvidenceFamily::ToolExecution,&tool.id),&mut input,Some(&source))?
                .context("actual Native schema missing")?;
            let mut native=native.retain_input_phase(input)?;
            let memory_before=memory.material_usage_for_test();
            let foreign=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
            let foreign_error=native.with_input_phase(tx,&foreign,None,|_| -> Result<()> {
                anyhow::bail!("foreign accounting reached the original Tool reader")
            }).err().context("foreign original ledger must be refused before the phase moves")?;
            assert!(format!("{foreign_error:#}").contains("ledger differs"),"{foreign_error:#}");
            assert_eq!(foreign.report()?.observed_bytes,0);
            assert_eq!(memory.material_usage_for_test(),memory_before);
            let (mut delta,increment)=native.with_input_phase(tx,&ledger,Some(&mut memory),|material| {
                let before=material.usage();
                let writer=InputToolWriter::new(tx,material,(0,0));
                writer.reserve_original_tool(&tool)?;
                crate::runtime_db::evidence::insert_tool_evidence_tx(tx,&tool)?;
                writer.before_facts(tx,&f.scope.agent_id,std::slice::from_ref(&tool),1,None)?;
                let delta=register_committed_input_evidence_with_tool_material_tx(tx,&f.scope,
                    &[CanonicalInputEvidence::ToolExecution(tool.clone())],None,&writer)?;
                drop(writer);
                let after=material.usage();
                Ok((delta,(after.0-before.0,after.1-before.1)))
            })?;
            assert_eq!(memory.material_usage_for_test(),
                (memory_before.0+increment.0,memory_before.1+increment.1),
                "resumed Input guards debit actual Memory once with no second owned registration");
            assert_eq!(delta.records.len(),1);
            assert!(delta.records[0].resident_bytes<32*1024);
            assert_eq!(delta.records[0].tool_run_report()?,&crate::runtime::run_report_projection::ToolRunReportProjection::default());
            native.finish_delta_tx(tx,&mut delta)?;
            let last=delta.clone();drop(delta);assert!(ledger.report()?.observed_bytes>baseline);
            drop((last,held,memory));assert_eq!(ledger.report()?.observed_bytes,baseline);
            Ok(())
        });
        if positive{result?;}else{
            let error=result.err().context("real Memory prior must reject before original Tool serialization")?;
            assert!(matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted)),"{error:#}");
            assert_eq!(f.db.connection()?.query_row("SELECT COUNT(*) FROM tool_executions WHERE evidence_id='actual-native-memory-tool'",[],|row|row.get::<_,usize>(0))?,0);
            assert_eq!(f.db.connection()?.query_row("SELECT COUNT(*) FROM input_query_birth_members WHERE family='tool_execution' AND record_id='actual-native-memory-tool'",[],|row|row.get::<_,usize>(0))?,0);
        }
        assert_eq!(ledger.report()?.observed_bytes,baseline);
    }
    drop((source,lease,reservation,owner));assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}

#[test]
fn original_input_prefix_is_borrowed_before_memory_and_only_its_exact_phase_can_resume() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial;
    let f=Fixture::new(false)?;
    let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let policy=WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536};
    let request=input_memory_request(&f);
    f.db.transaction(|tx| {
        let mut input=InputReadMaterial::new(&f.scope,&request,&ledger,(0,0))?;
        input.scope_facts_tx(tx)?;
        input.original_receipt_material_tx(tx,&f.original_start)?;
        let prefix=input.usage();
        let intent=DispatchMemoryIntent::Started {start:Box::new(f.start.clone())};
        let mut memory=DispatchMemoryTransactionBudget::prepare_with_original_input_tx(tx,&intent,&policy,&ledger,None,Some(&input))?;
        let actual=memory.material_usage_for_test();
        assert!(actual.0>prefix.0 && actual.1>prefix.1);
        let other=InputReadMaterial::new(&f.scope,&request,&ledger,(0,0))?;
        let error=memory.original_input_phase_usage(&other).err().context("another decoder phase was accepted")?;
        assert!(format!("{error:#}").contains("another Input decoder phase"));drop(other);
        let baseline=ledger.report()?.observed_bytes;
        let mut input=input.resume_after_original_memory(&mut memory)?;
        assert_eq!(input.usage(),actual,"the original cumulative Memory cost must enter Input exactly once");
        assert_eq!(ledger.report()?.observed_bytes,baseline,"resuming cannot register another allocation");
        input.observe_projected(16*1024,64)?;
        let held=vec![0u8;16*1024];
        let slots=std::mem::size_of::<crate::runtime::input_evidence_budget::BudgetViewGuard>()
            + 2*std::mem::size_of::<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>();
        let next=input.usage();assert_eq!(next,(actual.0+16*1024+slots,actual.1+64+1));
        assert_eq!(ledger.report()?.observed_bytes,baseline+u64::try_from(16*1024+slots)?,
            "the additional actual guard/slots must be registered only once");
        drop(input);assert_eq!(memory.material_usage_for_test(),next);
        drop((memory,held));assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })?;
    f.db.transaction(|tx| {
        let intent=DispatchMemoryIntent::Started {start:Box::new(f.start.clone())};
        let plain=DispatchMemoryTransactionBudget::prepare_tx(tx,&intent,&policy,&ledger)?;
        let cost=plain.material_usage_for_test();drop(plain);
        let mut input=InputReadMaterial::new(&f.scope,&request,&ledger,(0,0))?;
        let facts=input.scope_facts_tx(tx)?;
        input.original_receipt_material_tx(tx,&f.original_start)?;
        let prefix=input.usage();
        let bytes=prefix.0.checked_add(cost.0).and_then(|n|n.checked_sub(1)).context("joint first phase fixture overflow")?;
        assert!(cost.0<bytes && prefix.0<bytes,"both true phases fit separately");
        let joint=WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:bytes,record_limit:65536};
        let error=DispatchMemoryTransactionBudget::prepare_with_original_input_tx(tx,&intent,&joint,&ledger,None,Some(&input))
            .err().context("one-byte-short joint original prefix was ignored")?;
        assert!(matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted)),"{error:#}");
        assert!(facts.state.is_some());drop((facts,input));assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn original_started_writer_uses_the_earliest_input_memory_phase_and_replays_before_checkpoint_decode() -> Result<()> {
    use crate::runtime_db::transitions::InputMutationExecution;
    use crate::runtime::{input_queries::{source_roles_for_fixture,InputQuerySpec},mutation_owner::MutationOwnerRegistry};
    let f=Fixture::new(false)?;
    let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let owners=MutationOwnerRegistry::default();let owner=owners.claim(&f.scope.agent_id,f.scope.identity_incarnation)?;
    let (reservation,lease,roles)=source_roles_for_fixture(&owner,&f.scope,InputQuerySpec {birth_after:Some(0),..Default::default()},&ledger,(64*1024*1024,65536))?;drop(roles);
    let source=reservation.input_query_source(&f.scope,&ledger,(64*1024*1024,65536))?;
    let baseline=ledger.report()?.observed_bytes;
    let p=prepare_start_command(&f.db,&f.scope,&f.start,f.at)?;
    let work=WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536};
    let tasks=crate::runtime_db::task_snapshot::TaskDomainLoadRequest {capture:crate::runtime::resident_task::TaskCaptureRequest::current(),query:Default::default(),byte_limit:64*1024*1024,record_limit:65536};
    let InputMutationExecution::Committed {value,receipt}=f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_input_query_source(&p,&work,&ledger,&tasks,&source)? else {anyhow::bail!("original Started whole ACK missing")};
    assert_eq!(receipt.prepared,f.original_start);
    assert_eq!(f.db.connection()?.query_row("SELECT COUNT(*) FROM dispatch_memory_checkpoints",[],|r|r.get::<_,usize>(0))?,1);
    assert!(value.effects.input_evidence_delta.query_advance.is_some());
    let last=value.effects.input_evidence_delta.clone();drop(value);
    assert!(ledger.report()?.observed_bytes>baseline);drop(last);assert_eq!(ledger.report()?.observed_bytes,baseline);
    let raw:String=f.db.connection()?.query_row("SELECT payload_json FROM dispatch_memory_checkpoints",[],|r|r.get(0))?;
    f.db.connection()?.execute("UPDATE dispatch_memory_checkpoints SET payload_json='not-json'",[])?;
    let InputMutationExecution::Replayed {receipt:actual,..}=f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_input_query_source(&p,&work,&ledger,&tasks,&source)? else {anyhow::bail!("original receipt replay entered checkpoint decoder")};
    assert_eq!(actual,receipt);
    f.db.connection()?.execute("UPDATE dispatch_memory_checkpoints SET payload_json=?1",[raw])?;
    assert_eq!(ledger.report()?.observed_bytes,baseline);
    drop((source,lease,reservation));assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}

#[test]
fn original_memory_checkpoint_raw_and_complete_native_keys_follow_the_same_input_remaining() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial;
    let f=Fixture::new(true)?;
    let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let request=input_memory_request(&f);
    for malformed in [true,false] {
        let result:Result<()>=f.db.transaction(|tx| {
            let mut input=InputReadMaterial::new(&f.scope,&request,&ledger,(0,0))?;
            input.scope_facts_tx(tx)?;
            let raw=if malformed {"!".repeat(9*1024*1024)} else {
                let object=(0..4096).map(|i|(format!("key-{i}"),serde_json::Value::Null)).collect::<serde_json::Map<_,_>>();
                serde_json::to_string(&serde_json::Value::Object(object))?
            };
            let policy=WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:if malformed {4*1024*1024}else{64*1024*1024},record_limit:if malformed {65536}else{60000}};
            if !malformed {
                let values:usize=tx.query_row("SELECT COUNT(*) FROM json_tree(?1)",[&raw],|r|r.get(0))?;
                let complete:usize=tx.query_row("SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(?1)",[&raw],|r|r.get(0))?;
                assert!(values*12+input.usage().1+12<=policy.record_limit);
                assert!(complete*12+input.usage().1+12>policy.record_limit);
            }
            assert_eq!(tx.execute("UPDATE dispatch_memory_checkpoints SET payload_json=?1",[raw])?,1);
            let intent=DispatchMemoryIntent::Deferred {binding:DispatchMemoryBinding::from_start(&f.start),frozen:f.frozen("first-prefix-checkpoint")};
            DispatchMemoryTransactionBudget::prepare_with_original_input_tx(tx,&intent,&policy,&ledger,None,Some(&input))?;
            anyhow::bail!("first raw/native checkpoint gate was bypassed")
        });
        let error=result.err().context("actual checkpoint must reject")?;
        assert!(matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted)),"{error:#}");
        assert_eq!(ledger.report()?.observed_bytes,0);
        let raw:String=f.db.connection()?.query_row("SELECT payload_json FROM dispatch_memory_checkpoints",[],|r|r.get(0))?;
        assert!(serde_json::from_str::<serde_json::Value>(&raw)?.is_object(),"original whole transaction must roll back checkpoint corruption");
    }
    Ok(())
}

#[test]
fn original_deferred_work_writer_keeps_one_early_input_memory_allowance_and_whole_ack() -> Result<()> {
    use crate::runtime::{input_queries::{source_roles_for_fixture,InputQuerySpec},mutation_owner::MutationOwnerRegistry};
    use crate::runtime_db::transitions::{InputMutationExecution,TransitionFaultPoint};
    for rollback in [false,true] {
        let f=Fixture::new(true)?;
        let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
        let owners=MutationOwnerRegistry::default();let owner=owners.claim(&f.scope.agent_id,f.scope.identity_incarnation)?;
        let (reservation,lease,roles)=source_roles_for_fixture(&owner,&f.scope,InputQuerySpec {birth_after:Some(0),..Default::default()},&ledger,(64*1024*1024,65536))?;drop(roles);
        let source=reservation.input_query_source(&f.scope,&ledger,(64*1024*1024,65536))?;
        let baseline=ledger.report()?.observed_bytes;
        let before=f.db.work_items().latest("work:dispatch-memory")?.context("actual Work missing")?;
        let memory_before=f.read()?;
        let mut record=before.clone();record.revision+=1;record.objective="actual early phase Work mutation".into();record.updated_at+=chrono::Duration::milliseconds(1);
        let p=PreparedInputTransition::work(f.scope.clone(),"early-input-deferred-work".into(),f.at,
            WorkItemTransitionCommand {agent_id:f.scope.agent_id.clone(),mutation:WorkItemMutation::Update {record:record.clone(),expected_revision:before.revision},agent_state:None,
                brief_evidence:Vec::new(),audit_events:Vec::new(),index_changes:Vec::new(),notify_scheduler:true,
                fault:rollback.then_some(TransitionFaultPoint::AfterCanonicalWrites)},Default::default(),false)?
            .with_dispatch_memory(DispatchMemoryIntent::Deferred {binding:DispatchMemoryBinding::from_start(&f.start),frozen:f.frozen("early-input-deferred-work")})?;
        let tasks=crate::runtime_db::task_snapshot::TaskDomainLoadRequest {capture:crate::runtime::resident_task::TaskCaptureRequest::current(),query:Default::default(),byte_limit:64*1024*1024,record_limit:65536};
        let result=f.db.transitions_in_scope(&f.scope).commit_prepared_transition_with_input_query_source(&p,&request(),&ledger,&tasks,&source);
        if rollback {
            assert!(result.is_err());assert_eq!(f.db.work_items().latest(&before.id)?,Some(before));assert_eq!(f.read()?,memory_before);
            assert_eq!(f.db.connection()?.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",[p.descriptor().command_nonce()],|r|r.get::<_,usize>(0))?,0);
        } else {
            let InputMutationExecution::Committed {value,receipt}=result? else {anyhow::bail!("actual Deferred Work whole ACK missing")};
            assert!(value.applied&&receipt.outcome.applied);assert_eq!(f.db.work_items().latest(&record.id)?,Some(record));
            let memory=f.read()?.context("actual Memory checkpoint missing")?;
            assert_eq!(memory.start(),Some(&f.start));assert_eq!(memory.obligation().context("actual Deferred member missing")?.members.len(),1);
            assert!(value.effects.input_evidence_delta.query_advance.is_some());
            let last=value.effects.input_evidence_delta.clone();drop(value);assert!(ledger.report()?.observed_bytes>baseline);drop(last);
        }
        assert_eq!(ledger.report()?.observed_bytes,baseline);
        drop((source,lease,reservation));assert_eq!(ledger.report()?.observed_bytes,0);
    }
    Ok(())
}

#[path="dispatch_memory_task_preflight_tests.rs"]
mod task_preflight_material_tests;

#[path="dispatch_memory_scheduling_readback_tests.rs"]
mod scheduling_readback_material_tests;

#[path = "dispatch_memory_task_source_tests.rs"]
mod task_source_material;

#[path = "dispatch_memory_member_receipt_tests.rs"]
mod member_receipt_material;

#[path = "dispatch_memory_task_readback_lifetime_tests.rs"]
mod task_readback_lifetime;
