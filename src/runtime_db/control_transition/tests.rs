use super::*;
use crate::{
    domain::execution_protocol::{
        AdmittedFences, ExecutionAttempt, ExecutionOrigin, ExecutionPriority, ExecutionProvenance,
        ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
    },
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_state_tx},
        repositories::{
            upsert_external_trigger_tx, upsert_queue_entry_tx, upsert_task_tx, upsert_timer_tx,
            upsert_wait_condition_tx,
        },
    },
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        ActiveWorkspaceEntry, AdmissionContext, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentVisibility, CallbackDeliveryMode, ExternalTriggerScope, MessageDeliverySurface,
        Priority, TaskKind, WaitConditionKind, WakeSource, WorkItemExecutionBinding,
    },
};

struct Fixture {
    _home: tempfile::TempDir,
    db: RuntimeDb,
    time: DateTime<Utc>,
    key: ControlKey,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )
        .unwrap();
        db.connection()
            .unwrap()
            .execute_batch(include_str!("schema.sql"))
            .unwrap();
        let time: DateTime<Utc> = "2026-10-13T00:00:00Z".parse().unwrap();
        let identity = AgentIdentityRecord::new(
            "control-agent",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        db.agent_identities().upsert(&identity).unwrap();
        let mut state = AgentState::new("control-agent");
        state.status = AgentStatus::AwakeIdle;
        db.agent_states().upsert_for_fixture(&state).unwrap();
        Self {
            _home: home,
            db,
            time,
            key: ControlKey {
                agent_id: state.id,
                identity_incarnation: identity.incarnation,
                command_id: "intent-1".into(),
            },
        }
    }
    fn input(&self, action: ControlAction) -> ControlPlanInput {
        let snapshot = self.db.load_control_snapshot(&self.key).unwrap();
        let task_results = if action == ControlAction::Stop {
            snapshot
                .tasks
                .iter()
                .map(|before| self.task_result(before))
                .collect()
        } else {
            Vec::new()
        };
        let targets = if action == ControlAction::Stop {
            self.targets(&snapshot)
        } else {
            Vec::new()
        };
        ControlPlanInput {
            key: self.key.clone(),
            invocation: ControlInvocation {
                action,
                source: ControlSource::DirectApi,
                authority_class: AuthorityClass::OperatorInstruction,
            },
            snapshot,
            prepared_at: self.time + chrono::Duration::seconds(10),
            task_results,
            healthy_running: None,
            protected_control_attempt: None,
            protected_control_run_id: None,
            cancellation_targets: targets,
            audit_events: Vec::new(),
            index_changes: Vec::new(),
        }
    }
    fn command(&self, action: ControlAction) -> PreparedControl {
        prepare_control(self.input(action)).unwrap()
    }
    fn task_result(&self, before: &TaskRecord) -> ControlTaskResult {
        let prepared_at = self.time + chrono::Duration::seconds(10);
        let mut task = before.clone();
        task.status = TaskStatus::Interrupted;
        task.updated_at = prepared_at;
        let detail = task
            .detail
            .get_or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        detail.insert(
            "status_before_stop".into(),
            serde_json::to_value(&before.status).unwrap(),
        );
        detail.insert("task_status".into(), json!("interrupted"));
        detail.insert("interrupted_reason".into(), json!("agent_stopped"));
        detail.insert("interrupted_at".into(), json!(prepared_at));
        detail
            .entry("parent_turn_id")
            .or_insert(json!(format!("result-turn:{}", task.id)));
        let mut message = MessageEnvelope::new(
            &task.agent_id,
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: format!("task {} was interrupted because the agent stopped", task.id),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        message.id = format!("result:{}", task.id);
        message.turn_id = Some(format!("result-turn:{}", task.id));
        message.created_at = prepared_at;
        message.task_id = Some(task.id.clone());
        message.work_item_id = task.effective_work_item_id().map(str::to_string);
        message.metadata = Some(
            json!({"task_id": task.id, "task_kind": task.kind, "task_status": "interrupted",
            "task_summary": task.summary, "task_detail": task.detail, "task_recovery": task.recovery,
            "work_item_id": task.effective_work_item_id(), "interrupted_reason": "agent_stopped"}),
        );
        task.parent_message_id = Some(message.id.clone());
        ControlTaskResult { task, message }
    }
    fn targets(&self, snapshot: &ControlSnapshot) -> Vec<ControlCancellationTarget> {
        let mut targets = Vec::new();
        if let Some(run) = &snapshot.state.current_run_id {
            targets.push(ControlCancellationTarget {
                obligation_id: "cancel-run".into(),
                target_kind: ControlCancellationKind::Run,
                target_identity: run.clone(),
                target_generation: json!({"activation_id": null, "source_message_id": null,
                    "owner_epoch": 1, "runtime_lease_id": "original-run-lease"}),
                target: json!({"reason": "agent_stopped"}),
            });
        }
        for task in &snapshot.tasks {
            targets.push(ControlCancellationTarget { obligation_id: format!("cancel-task:{}", task.id), target_kind: ControlCancellationKind::Task,
                target_identity: task.id.clone(), target_generation: json!({"created_at": task.created_at, "rejoin": task.rejoin_fence().ok(),
                    "owner_epoch": 1, "runtime_lease_id": null}), target: json!({"kind": task.kind, "recovery": task.recovery, "work_item_id": task.effective_work_item_id()}) });
        }
        for timer in &snapshot.timers {
            targets.push(ControlCancellationTarget { obligation_id: format!("cancel-timer:{}", timer.id), target_kind: ControlCancellationKind::TimerLoop,
                target_identity: timer.id.clone(), target_generation: json!({"created_at": timer.created_at, "fire_count": timer.fire_count,
                    "next_fire_at": timer.next_fire_at, "owner_epoch": 1, "runtime_lease_id": null}), target: json!({"interval_ms": timer.interval_ms, "repeat": timer.repeat}) });
        }
        targets
    }
    fn seed_all(&self) {
        let task = TaskRecord {
            id: "task-1".into(),
            agent_id: self.key.agent_id.clone(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: self.time,
            updated_at: self.time,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("original task".into()),
            detail: Some(json!({"rejoin_obligation_id":"task-1",
                "rejoin_generation":7,"parent_turn_id":"original-parent"})),
            recovery: None,
        };
        let wait = WaitConditionRecord {
            id: "wait-1".into(),
            agent_id: self.key.agent_id.clone(),
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: Some("task".into()),
            subject_ref: Some("task-1".into()),
            waiting_for: "TaskResult".into(),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: "task-1".into(),
            }],
            continuation: None,
            created_at: self.time,
            updated_at: self.time,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some("original-parent".into()),
            trigger_message_id: None,
            triggered_at: None,
        };
        let timer = TimerRecord {
            id: "timer-1".into(),
            agent_id: self.key.agent_id.clone(),
            created_at: self.time,
            duration_ms: 1000,
            interval_ms: Some(1000),
            repeat: true,
            status: TimerStatus::Active,
            summary: None,
            next_fire_at: Some(self.time + chrono::Duration::hours(1)),
            last_fired_at: Some(self.time),
            fire_count: 3,
        };
        let trigger = ExternalTriggerRecord {
            external_trigger_id: "trigger-1".into(),
            target_agent_id: self.key.agent_id.clone(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            token: Some("opaque-capability".into()),
            token_hash: "hash".into(),
            status: ExternalTriggerStatus::Active,
            created_at: self.time,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        };
        self.db.transaction(|tx| {
            upsert_task_tx(tx, &task)?; upsert_wait_condition_tx(tx, &wait)?; upsert_timer_tx(tx, &timer)?; upsert_external_trigger_tx(tx, &trigger)?;
            for (message_id, status) in [("pending",QueueEntryStatus::Queued),("timer-wake",QueueEntryStatus::Interrupted),("claimed",QueueEntryStatus::Dequeued)] {
                upsert_queue_entry_tx(tx, &QueueEntryRecord { message_id: message_id.into(), agent_id: self.key.agent_id.clone(),
                    priority: Priority::Normal, status, created_at: self.time, updated_at: self.time })?;
            }
            tx.execute("INSERT INTO timer_wakes (timer_id,message_id,fire_count,status,created_at,updated_at) VALUES ('timer-1','timer-wake',3,'pending',?1,?1)", [self.time.to_rfc3339()])?;
            let mut state = self.db_state(tx)?; state.pending = 2; state.status = AgentStatus::AwakeRunning;
            state.current_run_id = Some("original-run".into());
            state.active_workspace_entry = Some(ActiveWorkspaceEntry { workspace_id: "workspace-1".into(), workspace_anchor: "/original".into(),
                execution_root_id: "root-1".into(), execution_root: "/original".into(), projection_kind: WorkspaceProjectionKind::GitWorktreeRoot,
                access_mode: WorkspaceAccessMode::ExclusiveWrite, cwd: "/original".into(), occupancy_id: Some("occupancy-1".into()), projection_metadata: None });
            upsert_workspace_occupancy_tx(tx, &WorkspaceOccupancyRecord { occupancy_id: "occupancy-1".into(), execution_root_id: "root-1".into(),
                workspace_id: "workspace-1".into(), holder_agent_id: state.id.clone(), access_mode: WorkspaceAccessMode::ExclusiveWrite,
                acquired_at: self.time, released_at: None })?;
            upsert_agent_state_tx(tx, &state)?; Ok(())
        }).unwrap();
    }

    fn seed_command_claim(&self, started: bool, activation_in_binding: bool) -> MessageEnvelope {
        self.seed_claim(
            started,
            activation_in_binding,
            Some(ExecutionBinding::Command),
        )
    }

    fn seed_claim(
        &self,
        started: bool,
        activation_in_binding: bool,
        binding: Option<ExecutionBinding>,
    ) -> MessageEnvelope {
        let owner = binding.as_ref().map(|binding| match binding {
            ExecutionBinding::Command => crate::types::TurnOwner::Command,
            ExecutionBinding::AgentLifecycle { agent_id } => {
                crate::types::TurnOwner::AgentLifecycle {
                    agent_id: agent_id.clone(),
                }
            }
            _ => panic!("unsupported fixture binding"),
        });
        let mut message = MessageEnvelope::new(
            &self.key.agent_id,
            MessageKind::Control,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "stop".into(),
            },
        );
        message.id = "consuming-control".into();
        message.created_at = self.time;
        self.db
            .transaction(|tx| {
                let (stored, _) = append_message_tx(tx, &message)?;
                message = stored;
                upsert_queue_entry_tx(
                    tx,
                    &QueueEntryRecord {
                        message_id: message.id.clone(),
                        agent_id: self.key.agent_id.clone(),
                        priority: Priority::Normal,
                        status: QueueEntryStatus::Dequeued,
                        created_at: self.time,
                        updated_at: self.time,
                    },
                )?;
                if let Some(binding) = &binding {
                    let mut protocol = ExecutionProtocolState::empty(&self.key.agent_id);
                    protocol.attempts.insert(
                        "attempt-1".into(),
                        ExecutionAttempt {
                            attempt_id: "attempt-1".into(),
                            agent_id: self.key.agent_id.clone(),
                            source_message_id: Some(message.id.clone()),
                            source: ExecutionSource {
                                identity: ExecutionSourceIdentity::QueueMessage {
                                    message_id: message.id.clone(),
                                },
                                generation: message.message_seq.expect("committed source seq"),
                            },
                            binding: binding.clone(),
                            provenance: ExecutionProvenance {
                                origin: ExecutionOrigin::Operator,
                                trust: ExecutionTrust::OperatorInstruction,
                                priority: ExecutionPriority::Normal,
                                correlation_id: None,
                                causation_id: None,
                            },
                            admitted_fences: AdmittedFences {
                                source_revision: message.message_seq.unwrap(),
                                work_item_source_revision: None,
                                work_item_generation: None,
                                rejoin: None,
                                agent_control_revision: 1,
                                host_registry_revision: 1,
                            },
                            state: ExecutionAttemptState::Open,
                            run_id: None,
                            turn_id: None,
                            recovery_of_attempt_id: None,
                            terminal_outcome_id: None,
                            admitted_at: self.time.to_rfc3339(),
                            terminal_at: None,
                        },
                    );
                    execution_protocol_repository::persist_state_tx(tx, &protocol)?;
                }
                let mut state = self.db_state(tx)?;
                state.status = AgentStatus::AwakeRunning;
                state.current_run_id = Some("original-run".into());
                if started {
                    let mut turn = TurnRecord::new(&state.id, "started-turn", 1);
                    turn.created_at = self.time;
                    turn.run_id = state.current_run_id.clone();
                    turn.owner = Some(owner.clone().unwrap_or_else(|| {
                        crate::types::TurnOwner::AgentLifecycle {
                            agent_id: state.id.clone(),
                        }
                    }));
                    turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&message));
                    turn.input_message_ids = vec![message.id.clone()];
                    upsert_turn_record_tx(tx, &turn)?;
                    state.turn_index = 1;
                    state.current_turn_id = Some(turn.turn_id.clone());
                    state.current_execution_binding = Some(WorkItemExecutionBinding {
                        activation_id: activation_in_binding.then(|| "attempt-1".into()),
                        admission_provenance: None,
                        source_message_id: message.id.clone(),
                        turn_id: turn.turn_id,
                        owner: owner.clone(),
                        work_item_id: None,
                        claimed_work_revision: None,
                    });
                }
                upsert_agent_state_tx(tx, &state)?;
                Ok(())
            })
            .unwrap();
        message
    }
    fn seed_reducer(&self, kind: MessageKind, text: &str) -> MessageEnvelope {
        let mut message = self.seed_claim(true, false, None);
        message.kind = kind;
        message.body = MessageBody::Text { text: text.into() };
        if message.kind == MessageKind::TaskStatus {
            message.origin = MessageOrigin::Task {
                task_id: "status-task".into(),
            };
            message.authority_class = AuthorityClass::RuntimeInstruction;
            message.task_id = Some("status-task".into());
            message.metadata = Some(
                json!({"task_id":"status-task", "task_kind":"command_task", "task_status":"running"}),
            );
        }
        self.db
            .transaction(|tx| {
                crate::runtime_db::evidence::upsert_message_tx(tx, &message)?;
                let mut turn: TurnRecord = payload_optional(
                    tx,
                    "SELECT payload_json FROM turn_records WHERE turn_id=?1",
                    "started-turn",
                )?
                .unwrap();
                turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&message));
                upsert_turn_record_tx(tx, &turn)?;
                Ok(())
            })
            .unwrap();
        message
    }

    fn db_state(&self, connection: &Connection) -> Result<AgentState> {
        payload_optional(
            connection,
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            &self.key.agent_id,
        )?
        .context("fixture State missing")
    }
    fn fingerprint(&self) -> String {
        let connection = self.db.connection().unwrap();
        let mut result = String::new();
        for table in [
            "agent_states",
            "tasks",
            "wait_conditions",
            "timers",
            "timer_wakes",
            "queue_entries",
            "external_triggers",
            "workspace_occupancies",
            "messages",
            "audit_events",
            "runtime_sequences",
            "runtime_index_outbox",
            "runtime_index_outbox_watermarks",
            "control_command_receipts",
            "control_cancellation_obligations",
        ] {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            result.push_str(table);
            while let Some(row) = rows.next().unwrap() {
                for index in 0..columns {
                    result.push_str(&format!("{:?}", row.get_ref(index).unwrap()));
                }
            }
        }
        result
    }
}

#[test]
fn complete_stop_commits_all_segments_and_exact_original_obligations() {
    let f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("not applied")
    };
    assert_eq!(delta.state.status, AgentStatus::Stopped);
    assert_eq!(delta.state.pending, 0);
    assert!(delta.state.current_run_id.is_none() && delta.state.active_workspace_entry.is_none());
    assert_eq!(delta.tasks[0].status, TaskStatus::Interrupted);
    assert_eq!(delta.waits[0].status, WaitConditionStatus::Cancelled);
    assert_eq!(
        delta.waits[0].trigger_message_id.as_deref(),
        Some("result:task-1")
    );
    assert_eq!(delta.timers[0].status, TimerStatus::Cancelled);
    assert_eq!(
        delta.timers[0].next_fire_at,
        Some(f.time + chrono::Duration::hours(1))
    );
    assert_eq!(delta.timer_wakes[0].status, "cancelled");
    assert_eq!(
        delta
            .queue_entries
            .iter()
            .find(|entry| entry.message_id == "timer-wake")
            .unwrap()
            .status,
        QueueEntryStatus::Dropped
    );
    assert!(delta
        .queue_entries
        .iter()
        .filter(|entry| entry.message_id != "timer-wake")
        .all(|entry| entry.status == QueueEntryStatus::Aborted));
    assert_eq!(
        delta.external_triggers[0].status,
        ExternalTriggerStatus::Revoked
    );
    assert_eq!(
        delta.workspace_occupancy.unwrap().released_at,
        Some(command.prepared_at)
    );
    assert_eq!(receipt.obligation_ids.len(), 3);
    assert!(!receipt.audit_bindings.is_empty());
    assert_eq!(receipt.index_bindings.len(), 2);
    let settlement: TaskResultSettlementRecord = payload_optional(
        &f.db.connection().unwrap(),
        "SELECT payload_json FROM task_result_settlements WHERE message_id = ?1",
        "result:task-1",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        settlement.state,
        task_result_settlement::TaskResultSettlementState::PersistedPending
    );
    assert_eq!(delta.result_messages[0].message_seq, Some(1));
}

#[test]
fn each_intermediate_write_rolls_back_the_whole_stop() {
    for fault in [
        ControlFaultPoint::AfterValidation,
        ControlFaultPoint::AfterTaskResultWrites,
        ControlFaultPoint::AfterWaitTimerWrites,
        ControlFaultPoint::AfterQueueTriggerWrites,
        ControlFaultPoint::AfterExecutionWorkspaceWrites,
        ControlFaultPoint::AfterStateWrite,
        ControlFaultPoint::AfterAuditWrites,
        ControlFaultPoint::AfterIndexWrites,
        ControlFaultPoint::AfterReceiptWrite,
        ControlFaultPoint::AfterObligationWrites,
    ] {
        let f = Fixture::new();
        f.seed_all();
        let command = f.command(ControlAction::Stop).with_fault(fault);
        let before = f.fingerprint();
        let error = f.db.commit_control(&command).unwrap_err();
        assert_eq!(
            crate::runtime_db::persistence_certainty(&error),
            Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
        );
        assert_eq!(f.fingerprint(), before, "partial commit at {fault:?}");
    }
}

#[test]
fn lost_ack_and_later_start_replay_keep_the_original_stop_outcome() {
    let mut f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    let error =
        f.db.commit_control(
            &command
                .clone()
                .with_fault(ControlFaultPoint::AfterCommitBeforeAck),
        )
        .unwrap_err();
    assert_eq!(
        crate::runtime_db::persistence_certainty(&error),
        Some(crate::runtime_db::PersistenceCertainty::Indeterminate)
    );
    let receipt =
        f.db.control_receipt(
            command.key(),
            command.invocation(),
            Some(command.command_digest()),
        )
        .unwrap()
        .unwrap();
    f.key.command_id = "new-start".into();
    let start = f.command(ControlAction::Start);
    f.db.commit_control(&start).unwrap();
    let before = f.fingerprint();
    let ControlCommit::Replayed { receipt: replay } = f.db.commit_control(&command).unwrap() else {
        panic!("replayed delta")
    };
    assert_eq!(replay, receipt);
    assert_eq!(f.fingerprint(), before);
    assert_eq!(
        f.db.load_control_snapshot(&f.key).unwrap().state.status,
        AgentStatus::AwakeIdle
    );
}

#[test]
fn every_new_noop_intent_gets_a_receipt_without_a_business_state_write() {
    let mut f = Fixture::new();
    let before_revision = f.db.load_control_snapshot(&f.key).unwrap().control_revision;
    for id in ["new-start-1", "new-start-2"] {
        f.key.command_id = id.into();
        let command = f.command(ControlAction::Start);
        let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap()
        else {
            panic!("new no-op replayed")
        };
        assert!(!receipt.outcome.changed);
        assert_eq!(receipt.agent_control_revision, before_revision);
        assert!(delta.transition.effects.agent_state.is_none());
        assert!(receipt.obligation_ids.is_empty());
    }
}

#[test]
fn changed_intent_or_rederived_payload_cannot_reuse_a_command_key() {
    let f = Fixture::new();
    let command = f.command(ControlAction::Start);
    f.db.commit_control(&command).unwrap();
    let wrong_action = f.command(ControlAction::Stop);
    assert!(f
        .db
        .commit_control(&wrong_action)
        .unwrap_err()
        .downcast_ref::<ControlError>()
        .is_some());
    let mut input = f.input(ControlAction::Start);
    input.prepared_at += chrono::Duration::seconds(1);
    let rederived = prepare_control(input).unwrap();
    assert_eq!(
        f.db.commit_control(&rederived)
            .unwrap_err()
            .downcast_ref::<ControlError>(),
        Some(&ControlError::CommandConflict)
    );
    assert!(f
        .db
        .control_receipt(command.key(), command.invocation(), None)
        .unwrap()
        .is_some());
}

#[test]
fn phantom_active_task_or_timer_rejects_the_entire_prepared_stop() {
    let f = Fixture::new();
    let command = f.command(ControlAction::Stop);
    f.seed_all();
    let before = f.fingerprint();
    assert!(f
        .db
        .commit_control(&command)
        .unwrap_err()
        .downcast_ref::<RuntimeStateTransitionConflict>()
        .is_some());
    assert_eq!(f.fingerprint(), before);
}

#[test]
fn terminal_task_race_keeps_the_winning_result_unchanged() {
    let f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    f.db.transaction(|tx| {
        let mut task = command.expected.tasks[0].clone();
        task.status = TaskStatus::Completed;
        task.updated_at = f.time + chrono::Duration::seconds(5);
        upsert_task_tx(tx, &task)?;
        Ok(())
    })
    .unwrap();
    let before = f.fingerprint();
    assert!(f.db.commit_control(&command).is_err());
    assert_eq!(f.fingerprint(), before);
}

#[test]
fn exact_cancellation_completion_does_not_touch_a_later_start_or_task() {
    let mut f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    f.db.commit_control(&command).unwrap();
    let target = &command.cancellation_targets[0];
    let pending =
        f.db.pending_control_obligations(&f.key.agent_id, f.key.identity_incarnation)
            .unwrap();
    assert_eq!(pending.len(), 3);
    f.key.command_id = "later-start".into();
    let start = f.command(ControlAction::Start);
    f.db.commit_control(&start).unwrap();
    let state = f.db.load_control_snapshot(&f.key).unwrap().state;
    assert!(f
        .db
        .complete_control_obligation(
            command.key(),
            target,
            command.prepared_at + chrono::Duration::seconds(1),
            json!({"joined_original_lease":true})
        )
        .unwrap());
    assert!(f
        .db
        .complete_control_obligation(
            command.key(),
            target,
            command.prepared_at + chrono::Duration::seconds(2),
            json!({"duplicate":true})
        )
        .unwrap());
    assert_eq!(f.db.load_control_snapshot(&f.key).unwrap().state, state);
    let mut wrong = target.clone();
    wrong.target_generation["runtime_lease_id"] = json!("later-lease");
    assert!(f
        .db
        .complete_control_obligation(
            command.key(),
            &wrong,
            command.prepared_at,
            json!({"joined":true})
        )
        .is_err());
    let obligations = load_obligations_in(&f.db.connection().unwrap(), command.key()).unwrap();
    let completed = obligations
        .iter()
        .find(|obligation| obligation.target.obligation_id == target.obligation_id)
        .unwrap();
    assert_eq!(
        completed.completion_evidence,
        Some(json!({"joined_original_lease":true}))
    );
}

#[test]
fn healthy_start_preserves_claim_window_and_started_command_with_unbound_attempt_turn() {
    for (started, activation) in [(false, false), (true, false), (true, true)] {
        let f = Fixture::new();
        let message = f.seed_command_claim(started, activation);
        let mut input = f.input(ControlAction::Start);
        input.healthy_running = Some(ControlRunningProof {
            run_id: "original-run".into(),
            attempt_id: Some("attempt-1".into()),
            source_message_id: message.id.clone(),
            turn_id: started.then(|| "started-turn".into()),
        });
        let original = input.snapshot.clone();
        let command = prepare_control(input).unwrap();
        let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap()
        else {
            panic!("new intent replayed")
        };
        assert!(!receipt.outcome.changed);
        assert_eq!(delta.state, original.state);
        assert!(delta.transition.effects.agent_state.is_none());
        assert_eq!(
            delta.execution_protocol.as_ref().unwrap().attempts["attempt-1"].state,
            ExecutionAttemptState::Open
        );
        assert_eq!(
            f.db.load_control_snapshot(&f.key).unwrap().queue_entries,
            original.queue_entries
        );
        assert!(receipt.obligation_ids.is_empty());
    }
}

fn seed_preallocated_claim(f: &Fixture) -> MessageEnvelope {
    let mut message = f.seed_command_claim(false, false);
    message.turn_id = Some("preallocated-unstarted-turn".into());
    f.db.transaction(|tx| {
        crate::runtime_db::evidence::upsert_message_tx(tx, &message)?;
        let mut protocol =
            execution_protocol_repository::context_execution_protocol_on(tx, &f.key.agent_id)?
                .unwrap();
        protocol.attempts.get_mut("attempt-1").unwrap().turn_id = message.turn_id.clone();
        execution_protocol_repository::persist_state_tx(tx, &protocol)?;
        Ok(())
    })
    .unwrap();
    message
}

#[test]
fn preallocated_claim_without_started_turn_allows_healthy_start_and_stop() {
    for action in [ControlAction::Start, ControlAction::Stop] {
        let f = Fixture::new();
        let message = seed_preallocated_claim(&f);
        let mut input = f.input(action.clone());
        assert!(input.snapshot.unstarted_attempt_ids.contains("attempt-1"));
        assert!(input.snapshot.attempt_turns.is_empty());
        if action == ControlAction::Start {
            input.healthy_running = Some(ControlRunningProof {
                run_id: "original-run".into(),
                attempt_id: Some("attempt-1".into()),
                source_message_id: message.id,
                turn_id: None,
            });
        }
        let command = prepare_control(input).unwrap();
        let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap()
        else {
            panic!("new claim-window intent replayed")
        };
        assert!(receipt.outcome.interrupted_turn_ids.is_empty());
        if action == ControlAction::Start {
            assert!(!receipt.outcome.changed);
            assert_eq!(delta.state.current_run_id.as_deref(), Some("original-run"));
            assert_eq!(
                delta.execution_protocol.unwrap().attempts["attempt-1"].state,
                ExecutionAttemptState::Open
            );
        } else {
            assert_eq!(delta.state.status, AgentStatus::Stopped);
            assert_eq!(receipt.outcome.interrupted_attempt_ids, vec!["attempt-1"]);
        }
    }
}

#[test]
fn healthy_start_replay_claim_preserves_the_existing_terminal_input_assignment() {
    let f = Fixture::new();
    let mut message = f.seed_claim(
        true,
        true,
        Some(ExecutionBinding::AgentLifecycle {
            agent_id: f.key.agent_id.clone(),
        }),
    );
    message.turn_id = Some("started-turn".into());
    f.db.transaction(|tx| {
        crate::runtime_db::evidence::upsert_message_tx(tx, &message)?;
        let protocol =
            execution_protocol_repository::context_execution_protocol_on(tx, &f.key.agent_id)?
                .unwrap();
        let mut replay = protocol.attempts["attempt-1"].clone();
        let protocol = execution_protocol::interrupt_execution(
            &protocol,
            &InterruptExecution {
                attempt_id: "attempt-1".into(),
                outcome_id: "original-interruption".into(),
                reason: "original runtime ended".into(),
                interrupted_at: f.time.to_rfc3339(),
            },
        )
        .unwrap()
        .state;
        replay.attempt_id = "replay-attempt".into();
        replay.turn_id = message.turn_id.clone();
        replay.recovery_of_attempt_id = Some("attempt-1".into());
        let protocol = execution_protocol::admit_execution(
            &protocol,
            &execution_protocol::AdmitExecution { attempt: replay },
        )
        .unwrap()
        .state;
        execution_protocol_repository::persist_state_tx(tx, &protocol)?;
        let mut turn: TurnRecord = payload_optional(
            tx,
            "SELECT payload_json FROM turn_records WHERE turn_id=?1",
            "started-turn",
        )?
        .unwrap();
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Interrupted,
            reason: Some("original runtime ended".into()),
            no_brief_reason: Some(TurnNoBriefReason::Interrupted),
            completed_at: f.time,
            duration_ms: 0,
        });
        upsert_turn_record_tx(tx, &turn)?;
        Ok(())
    })
    .unwrap();
    let mut input = f.input(ControlAction::Start);
    assert!(!input
        .snapshot
        .unstarted_attempt_ids
        .contains("replay-attempt"));
    input.healthy_running = Some(ControlRunningProof {
        run_id: "original-run".into(),
        attempt_id: Some("replay-attempt".into()),
        source_message_id: message.id.clone(),
        turn_id: None,
    });
    let original = input.snapshot.clone();
    let command = prepare_control(input).unwrap();
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("new replay-claim Start intent replayed")
    };
    assert!(!receipt.outcome.changed);
    assert_eq!(delta.state, original.state);
    assert_eq!(
        delta.execution_protocol.as_ref().unwrap().attempts["replay-attempt"].state,
        ExecutionAttemptState::Open
    );
    assert_eq!(
        f.db.connection()
            .unwrap()
            .query_row(
                "SELECT turn_id FROM conversation_input_assignments WHERE message_id=?1",
                [&message.id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "started-turn"
    );
}

#[test]
fn missing_preallocated_turn_requires_exact_unstarted_message_queue_and_assignment() {
    for damage in [
        "binding",
        "assignment",
        "queue",
        "message",
        "hint",
        "sequence",
    ] {
        let f = Fixture::new();
        let message = seed_preallocated_claim(&f);
        f.db.transaction(|tx| {
            match damage {
                "binding" => {
                    let mut state = f.db_state(tx)?;
                    state.current_execution_binding = Some(WorkItemExecutionBinding {
                        activation_id: Some("attempt-1".into()),
                        admission_provenance: None,
                        source_message_id: message.id.clone(),
                        turn_id: message.turn_id.clone().unwrap(),
                        owner: Some(crate::types::TurnOwner::Command),
                        work_item_id: None,
                        claimed_work_revision: None,
                    });
                    upsert_agent_state_tx(tx, &state)?;
                }
                "assignment" => {
                    tx.execute("INSERT INTO conversation_input_assignments(message_id, agent_id, turn_id, revision, assigned_at) VALUES(?1,?2,?3,1,?4)", params![message.id, f.key.agent_id, message.turn_id, f.time.to_rfc3339()])?;
                }
                "queue" => { tx.execute("UPDATE queue_entries SET payload_json=json_set(payload_json,'$.status','queued'),status='queued' WHERE message_id=?1", [&message.id])?; }
                "message" => { tx.execute("DELETE FROM messages WHERE message_id=?1", [&message.id])?; }
                "hint" => { tx.execute("UPDATE messages SET payload_json=json_set(payload_json,'$.turn_id','other-turn'),turn_id='other-turn' WHERE message_id=?1", [&message.id])?; }
                "sequence" => { tx.execute("UPDATE messages SET message_seq=message_seq+1 WHERE message_id=?1", [&message.id])?; }
                _ => unreachable!(),
            }
            Ok(())
        }).unwrap();
        assert!(
            f.db.load_control_snapshot(&f.key).is_err(),
            "accepted damaged {damage} as a pre-Turn claim"
        );
    }
}

#[test]
fn healthy_start_rejects_a_stale_claim_or_omitted_started_turn() {
    let f = Fixture::new();
    let message = f.seed_command_claim(true, false);
    for (source, turn) in [
        ("other-source", Some("started-turn")),
        (message.id.as_str(), None),
    ] {
        let mut input = f.input(ControlAction::Start);
        input.healthy_running = Some(ControlRunningProof {
            run_id: "original-run".into(),
            attempt_id: Some("attempt-1".into()),
            source_message_id: source.into(),
            turn_id: turn.map(String::from),
        });
        assert!(prepare_control(input).is_err());
    }
}

#[test]
fn direct_stop_interrupts_open_command_and_its_started_turn_without_attempt_turn_alias() {
    let f = Fixture::new();
    f.seed_command_claim(true, false);
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("not applied")
    };
    assert_eq!(receipt.outcome.interrupted_attempt_ids, vec!["attempt-1"]);
    assert_eq!(receipt.outcome.interrupted_turn_ids, vec!["started-turn"]);
    assert_eq!(
        delta.execution_protocol.as_ref().unwrap().attempts["attempt-1"].state,
        ExecutionAttemptState::Interrupted
    );
    assert_eq!(
        delta.turns[0].terminal.as_ref().unwrap().kind,
        TurnTerminalKind::Interrupted
    );
    assert_eq!(delta.transition.effects.turns, delta.turns);
    assert_eq!(
        delta.transition.effects.execution_protocol,
        delta.execution_protocol
    );
}

#[test]
fn queued_stop_preserves_only_its_own_canonical_command_attempt_and_input() {
    let f = Fixture::new();
    let message = f.seed_command_claim(true, false);
    let mut input = f.input(ControlAction::Stop);
    input.invocation.source = ControlSource::ControlMessage {
        message_id: message.id.clone(),
    };
    input.protected_control_attempt = Some("attempt-1".into());
    input.protected_control_run_id = Some("original-run".into());
    input
        .cancellation_targets
        .retain(|target| target.target_kind != ControlCancellationKind::Run);
    let command = prepare_control(input).unwrap();
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("not applied")
    };
    assert!(
        receipt.outcome.interrupted_attempt_ids.is_empty()
            && receipt.outcome.interrupted_turn_ids.is_empty()
    );
    assert!(receipt.obligation_ids.is_empty());
    assert_eq!(delta.state.status, AgentStatus::Stopped);
    let current = f.db.load_control_snapshot(&f.key).unwrap();
    assert_eq!(current.queue_entries[0].status, QueueEntryStatus::Dequeued);
    assert_eq!(
        current.execution_protocol.unwrap().attempts["attempt-1"].state,
        ExecutionAttemptState::Open
    );
    assert!(current.attempt_turns[0].terminal.is_none());
    let mut wrong = f.input(ControlAction::Stop);
    wrong.invocation.source = ControlSource::ControlMessage {
        message_id: "other-message".into(),
    };
    wrong.protected_control_attempt = Some("attempt-1".into());
    assert!(prepare_control(wrong).is_err());
}

#[test]
fn queued_reducer_control_preserves_only_its_exact_started_turn_and_run() {
    let f = Fixture::new();
    let message = f.seed_claim(true, false, None);
    let mut input = f.input(ControlAction::Stop);
    assert_eq!(
        input.snapshot.reducer_control_source_message_id.as_deref(),
        Some(message.id.as_str())
    );
    input.invocation.source = ControlSource::ControlMessage {
        message_id: message.id.clone(),
    };
    input.protected_control_run_id = Some("original-run".into());
    input
        .cancellation_targets
        .retain(|target| target.target_kind != ControlCancellationKind::Run);
    let command = prepare_control(input).unwrap();
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("not applied")
    };
    assert_eq!(delta.state.status, AgentStatus::Stopped);
    assert!(receipt.outcome.aborted_run_id.is_none());
    assert!(receipt.outcome.interrupted_attempt_ids.is_empty());
    assert!(receipt.outcome.interrupted_turn_ids.is_empty());
    assert!(receipt.obligation_ids.is_empty());
    let current = f.db.load_control_snapshot(&f.key).unwrap();
    assert_eq!(current.queue_entries[0].status, QueueEntryStatus::Dequeued);
    assert!(current.attempt_turns[0].terminal.is_none());
    assert!(current.execution_protocol.is_none());
}

#[test]
fn external_stop_interrupts_the_exact_reducer_control_turn_without_inventing_an_attempt() {
    for kind in [MessageKind::Control, MessageKind::TaskStatus] {
        let f = Fixture::new();
        let message = f.seed_reducer(kind, "stop");
        let command = f.command(ControlAction::Stop);
        let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap()
        else {
            panic!("not applied")
        };
        assert_eq!(
            receipt.outcome.aborted_run_id.as_deref(),
            Some("original-run")
        );
        assert_eq!(receipt.outcome.interrupted_turn_ids, vec!["started-turn"]);
        assert!(receipt.outcome.interrupted_attempt_ids.is_empty());
        assert_eq!(delta.turns.len(), 1);
        let turn = &delta.turns[0];
        assert_eq!(turn.run_id.as_deref(), Some("original-run"));
        assert_eq!(
            turn.trigger.as_ref().unwrap().message_id.as_deref(),
            Some(message.id.as_str())
        );
        assert_eq!(
            turn.terminal.as_ref().unwrap().kind,
            TurnTerminalKind::Interrupted
        );
        assert_eq!(delta.transition.effects.turns, delta.turns);
        let current = f.db.load_control_snapshot(&f.key).unwrap();
        assert_eq!(current.queue_entries.len(), 0);
        assert!(current.execution_protocol.is_none());
        assert_eq!(current.attempt_turns[0], *turn);
    }
}

#[test]
fn healthy_start_preserves_a_qualified_reducer_turn_without_an_attempt() {
    for queued in [false, true] {
        let f = Fixture::new();
        let message = f.seed_reducer(MessageKind::Control, "start");
        let mut input = f.input(ControlAction::Start);
        if queued {
            input.invocation.source = ControlSource::ControlMessage {
                message_id: message.id.clone(),
            };
            input.protected_control_run_id = Some("original-run".into());
        }
        input.healthy_running = Some(ControlRunningProof {
            run_id: "original-run".into(),
            attempt_id: None,
            source_message_id: message.id,
            turn_id: Some("started-turn".into()),
        });
        let before = input.snapshot.clone();
        let command = prepare_control(input).unwrap();
        let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap()
        else {
            panic!("not applied")
        };
        assert!(!receipt.outcome.changed);
        assert_eq!(delta.state, before.state);
        assert!(delta.execution_protocol.is_none());
        assert!(receipt.obligation_ids.is_empty());
        let current = f.db.load_control_snapshot(&f.key).unwrap();
        assert_eq!(current.queue_entries, before.queue_entries);
        assert_eq!(current.attempt_turns, before.attempt_turns);
    }
}

#[test]
fn healthy_reducer_start_cannot_omit_or_replace_its_started_source_turn() {
    let f = Fixture::new();
    let message = f.seed_reducer(MessageKind::Control, "start");
    for damage in ["run", "source", "turn", "no_turn", "attempt", "assignment"] {
        let mut input = f.input(ControlAction::Start);
        let mut proof = ControlRunningProof {
            run_id: "original-run".into(),
            attempt_id: None,
            source_message_id: message.id.clone(),
            turn_id: Some("started-turn".into()),
        };
        match damage {
            "run" => proof.run_id = "other-run".into(),
            "source" => proof.source_message_id = "other-source".into(),
            "turn" => proof.turn_id = Some("other-turn".into()),
            "no_turn" => proof.turn_id = None,
            "attempt" => proof.attempt_id = Some("synthetic-attempt".into()),
            "assignment" => input.snapshot.reducer_source_message_id = None,
            _ => unreachable!(),
        }
        input.healthy_running = Some(proof);
        assert!(
            prepare_control(input).is_err(),
            "accepted invalid reducer proof {damage}"
        );
    }
}

#[test]
fn queued_reducer_control_requires_actual_assignment_and_exact_active_source_run() {
    for damage in [
        "assignment",
        "foreign_assignment",
        "queue",
        "binding_source",
        "binding_activation",
        "binding_owner",
        "current_turn",
        "run",
        "trigger",
        "terminal",
        "message_kind",
        "sequence",
    ] {
        let f = Fixture::new();
        let message = f.seed_claim(true, false, None);
        f.db.transaction(|tx| {
            match damage {
                "assignment" => { tx.execute("DELETE FROM conversation_input_assignments WHERE message_id=?1", [&message.id])?; }
                "foreign_assignment" => { tx.execute("UPDATE conversation_input_assignments SET agent_id='other-agent' WHERE message_id=?1", [&message.id])?; }
                "queue" => { tx.execute("UPDATE queue_entries SET status='queued',payload_json=json_set(payload_json,'$.status','queued') WHERE message_id=?1", [&message.id])?; }
                "binding_source" => { tx.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.current_execution_binding.source_message_id','other-source') WHERE agent_id=?1", [&f.key.agent_id])?; }
                "binding_activation" => { tx.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.current_execution_binding.activation_id','other-attempt') WHERE agent_id=?1", [&f.key.agent_id])?; }
                "binding_owner" => { tx.execute(r#"UPDATE agent_states SET payload_json=json_set(payload_json,'$.current_execution_binding.owner',json('{"kind":"command"}')) WHERE agent_id=?1"#, [&f.key.agent_id])?; }
                "current_turn" => { tx.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.current_turn_id','other-turn') WHERE agent_id=?1", [&f.key.agent_id])?; }
                "run" => { tx.execute("UPDATE turn_records SET payload_json=json_set(payload_json,'$.run_id','other-run') WHERE turn_id='started-turn'", [])?; }
                "trigger" => { tx.execute("UPDATE turn_records SET payload_json=json_set(payload_json,'$.trigger.message_id','other-source') WHERE turn_id='started-turn'", [])?; }
                "terminal" => {
                    let mut turn: TurnRecord = payload_optional(tx, "SELECT payload_json FROM turn_records WHERE turn_id=?1", "started-turn")?.unwrap();
                    turn.terminal = Some(TurnTerminalSummary { kind: TurnTerminalKind::Completed, reason: None, no_brief_reason: Some(TurnNoBriefReason::ReducerOnly { reason: "fixture completed".into() }), completed_at: f.time, duration_ms: 0 });
                    tx.execute("UPDATE turn_records SET payload_json=?1 WHERE turn_id='started-turn'", [serde_json::to_string(&turn)?])?;
                }
                "message_kind" => { tx.execute("UPDATE messages SET payload_json=json_set(payload_json,'$.kind','operator_prompt') WHERE message_id=?1", [&message.id])?; }
                "sequence" => { tx.execute("UPDATE messages SET message_seq=message_seq+1 WHERE message_id=?1", [&message.id])?; }
                _ => unreachable!(),
            }
            Ok(())
        }).unwrap();
        let Ok(snapshot) = f.db.load_control_snapshot(&f.key) else {
            continue;
        };
        assert!(
            snapshot.reducer_control_source_message_id.is_none(),
            "qualified damaged {damage}"
        );
        let mut input = f.input(ControlAction::Stop);
        input.invocation.source = ControlSource::ControlMessage {
            message_id: message.id,
        };
        input.protected_control_run_id = Some("original-run".into());
        input
            .cancellation_targets
            .retain(|target| target.target_kind != ControlCancellationKind::Run);
        assert!(prepare_control(input).is_err(), "accepted damaged {damage}");
    }
}

#[test]
fn reducer_control_assignment_is_revalidated_inside_the_commit_transaction() {
    let f = Fixture::new();
    let message = f.seed_claim(true, false, None);
    let mut input = f.input(ControlAction::Stop);
    input.invocation.source = ControlSource::ControlMessage {
        message_id: message.id.clone(),
    };
    input.protected_control_run_id = Some("original-run".into());
    input
        .cancellation_targets
        .retain(|target| target.target_kind != ControlCancellationKind::Run);
    let command = prepare_control(input).unwrap();
    f.db.connection()
        .unwrap()
        .execute(
            "DELETE FROM conversation_input_assignments WHERE message_id=?1",
            [&message.id],
        )
        .unwrap();
    assert!(f.db.commit_control(&command).is_err());
    assert_eq!(
        f.db.load_control_snapshot(&f.key).unwrap().state.status,
        AgentStatus::AwakeRunning
    );
    assert!(f
        .db
        .control_receipt(command.key(), command.invocation(), None)
        .unwrap()
        .is_none());
}

#[test]
fn reducer_control_does_not_hide_an_open_execution_attempt_for_the_same_source() {
    let f = Fixture::new();
    let message = f.seed_command_claim(true, false);
    f.db.transaction(|tx| {
        let mut state = f.db_state(tx)?;
        state.current_execution_binding.as_mut().unwrap().owner = None;
        upsert_agent_state_tx(tx, &state)?;
        Ok(())
    })
    .unwrap();
    let mut input = f.input(ControlAction::Stop);
    assert!(input.snapshot.reducer_control_source_message_id.is_none());
    input.invocation.source = ControlSource::ControlMessage {
        message_id: message.id,
    };
    input.protected_control_run_id = Some("original-run".into());
    input
        .cancellation_targets
        .retain(|target| target.target_kind != ControlCancellationKind::Run);
    assert!(prepare_control(input).is_err());
}

#[test]
fn stop_wait_cancellation_uses_the_existing_millisecond_storage_precision() {
    let mut f = Fixture::new();
    f.time += chrono::Duration::nanoseconds(123_456_789);
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("not applied")
    };
    assert_eq!(receipt.outcome.cancelled_wait_ids, vec!["wait-1"]);
    assert_eq!(delta.waits.len(), 1);
    let wait = &delta.waits[0];
    assert_eq!(wait.status, WaitConditionStatus::Cancelled);
    assert_eq!(
        wait.cancelled_at.unwrap().timestamp_millis(),
        command.prepared_at.timestamp_millis()
    );
    assert_eq!(
        wait.updated_at.timestamp_millis(),
        command.prepared_at.timestamp_millis()
    );
    assert_ne!(wait.cancelled_at, Some(command.prepared_at));
}

#[test]
fn recovery_reads_current_terminal_targets_and_episodes_in_one_scope() {
    let f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    f.db.commit_control(&command).unwrap();
    let ControlRecoveryObservation::Current {
        snapshot,
        episodes,
        receipt,
        facts,
        obligations,
    } =
        f.db.load_control_recovery(
            command.key(),
            command.invocation(),
            command.command_digest(),
            12,
        )
        .unwrap()
    else {
        panic!("not current")
    };
    assert_eq!(snapshot.state.status, AgentStatus::Stopped);
    assert!(snapshot.tasks.is_empty() && snapshot.timers.is_empty());
    assert_eq!(episodes.state.as_ref(), Some(&snapshot.state));
    assert_eq!(
        facts
            .transition
            .effects
            .agent_state
            .as_ref()
            .unwrap()
            .record
            .as_ref(),
        &snapshot.state
    );
    assert_eq!(
        facts.transition.effects.tasks[0].status,
        TaskStatus::Interrupted
    );
    assert_eq!(
        facts.transition.effects.timers[0].status,
        TimerStatus::Cancelled
    );
    assert_eq!(
        facts.transition.effects.external_triggers[0].status,
        ExternalTriggerStatus::Revoked
    );
    assert_eq!(facts.transition.effects.messages[0].id, "result:task-1");
    assert_eq!(facts.transition.effects.messages[0].message_seq, Some(1));
    assert_eq!(facts.waits[0].status, WaitConditionStatus::Cancelled);
    assert_eq!(facts.timer_wakes[0].status, "cancelled");
    assert!(facts.current_queue_messages.is_empty());
    assert_eq!(obligations.len(), 3);
    assert!(receipt.is_some());
}

#[test]
fn historical_receipt_is_visible_after_retirement_without_installable_state() {
    let f = Fixture::new();
    let command = f.command(ControlAction::Stop);
    f.db.commit_control(&command).unwrap();
    f.db.connection().unwrap().execute("UPDATE agent_identities SET status='deleted',payload_json=json_set(payload_json,'$.status','deleted') WHERE agent_id=?1",[&f.key.agent_id]).unwrap();
    let ControlRecoveryObservation::Retired {
        current_identity,
        receipt,
        obligations,
    } =
        f.db.load_control_recovery(
            command.key(),
            command.invocation(),
            command.command_digest(),
            12,
        )
        .unwrap()
    else {
        panic!("retired scope returned State")
    };
    assert_eq!(
        current_identity.unwrap().status,
        AgentRegistryStatus::Deleted
    );
    assert!(receipt.is_some());
    assert!(obligations.is_empty());
}

#[test]
fn receipt_duplicate_columns_and_missing_touched_records_fail_closed() {
    let f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    f.db.commit_control(&command).unwrap();
    f.db.connection()
        .unwrap()
        .execute(
            "UPDATE control_command_receipts SET committed_at='2030-01-01T00:00:00+00:00'",
            [],
        )
        .unwrap();
    assert!(f
        .db
        .control_receipt(
            command.key(),
            command.invocation(),
            Some(command.command_digest())
        )
        .is_err());
    f.db.connection()
        .unwrap()
        .execute(
            "UPDATE control_command_receipts SET committed_at=?1",
            [command.prepared_at.to_rfc3339()],
        )
        .unwrap();
    f.db.connection()
        .unwrap()
        .execute("DELETE FROM timer_wakes WHERE timer_id='timer-1'", [])
        .unwrap();
    f.db.connection()
        .unwrap()
        .execute("DELETE FROM timers WHERE timer_id='timer-1'", [])
        .unwrap();
    assert!(f
        .db
        .load_control_recovery(
            command.key(),
            command.invocation(),
            command.command_digest(),
            12
        )
        .is_err());
}

#[test]
fn unfinished_obligations_preserve_exact_sources_but_completed_receipts_are_not_roots() {
    let f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    f.db.commit_control(&command).unwrap();
    let connection = f.db.connection().unwrap();
    connection.execute("UPDATE control_cancellation_obligations SET state='needs_attention' WHERE obligation_id='cancel-task:task-1'",[]).unwrap();
    let roots = control_cancellation_source_refs_in(&connection).unwrap();
    assert!(roots.contains(&"task:task-1".into()));
    assert!(roots.contains(&"turn:original-parent".into()));
    assert!(roots.contains(&"message:result:task-1".into()));
    drop(connection);
    for target in command.cancellation_targets() {
        f.db.complete_control_obligation(
            command.key(),
            target,
            command.prepared_at + chrono::Duration::seconds(1),
            json!({"settled_original":true}),
        )
        .unwrap();
    }
    assert!(
        control_cancellation_source_refs_in(&f.db.connection().unwrap())
            .unwrap()
            .is_empty()
    );
    assert!(f
        .db
        .control_receipt(command.key(), command.invocation(), None)
        .unwrap()
        .is_some());
}

#[test]
fn stop_cancels_a_real_completed_one_shot_pending_wake_without_loop_obligation() {
    let f = Fixture::new();
    let timer = TimerRecord {
        id: "one-shot".into(),
        agent_id: f.key.agent_id.clone(),
        created_at: f.time,
        duration_ms: 1,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: Some(f.time),
        last_fired_at: None,
        fire_count: 0,
    };
    f.db.transaction(|tx| {
        upsert_timer_tx(tx, &timer)?;
        Ok(())
    })
    .unwrap();
    let mut fired = timer.clone();
    fired.status = TimerStatus::Completed;
    fired.fire_count = 1;
    fired.last_fired_at = Some(f.time);
    fired.next_fire_at = None;
    let mut message = MessageEnvelope::new(
        &f.key.agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: timer.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "timer fired".into(),
        },
    );
    message.id = "one-shot-wake".into();
    message.created_at = f.time;
    message.metadata = Some(json!({"timer_id":timer.id,"timer_fire_count":1}));
    let before = f.db.load_control_snapshot(&f.key).unwrap().state;
    let mut after = before.clone();
    after.pending = 1;
    after.total_message_count = 1;
    let fire =
        f.db.timers()
            .fire(&crate::runtime_db::TimerFire {
                expected: timer,
                record: fired.clone(),
                message: message.clone(),
                queue_entry: QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: f.key.agent_id.clone(),
                    priority: Priority::Next,
                    status: QueueEntryStatus::Queued,
                    created_at: f.time,
                    updated_at: f.time,
                },
                agent_state: (before, after),
            })
            .unwrap();
    assert!(fire.advanced && fire.wake_created);
    let command = f.command(ControlAction::Stop);
    assert!(command.expected.timers.is_empty());
    assert_eq!(command.expected.pending_timer_wakes.len(), 1);
    let fingerprint = f.fingerprint();
    assert!(f
        .db
        .commit_control(
            &command
                .clone()
                .with_fault(ControlFaultPoint::AfterWaitTimerWrites)
        )
        .is_err());
    assert_eq!(f.fingerprint(), fingerprint);
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("not applied")
    };
    assert_eq!(
        receipt.outcome.cancelled_timer_wake_message_ids,
        vec!["one-shot-wake"]
    );
    assert_eq!(
        receipt.outcome.dropped_timer_queue_message_ids,
        vec!["one-shot-wake"]
    );
    assert!(receipt.outcome.cancelled_timer_ids.is_empty() && receipt.obligation_ids.is_empty());
    assert_eq!(delta.timer_wakes[0].status, "cancelled");
    assert_eq!(delta.queue_entries[0].status, QueueEntryStatus::Dropped);
    let stored: TimerRecord = payload_optional(
        &f.db.connection().unwrap(),
        "SELECT payload_json FROM timers WHERE timer_id=?1",
        "one-shot",
    )
    .unwrap()
    .unwrap();
    assert_eq!(stored, fired);
}

#[test]
fn stop_reports_changed_for_only_a_remaining_completed_timer_pending_wake() {
    let f = Fixture::new();
    f.db.transaction(|tx| {
        let mut state = f.db_state(tx)?; state.status = AgentStatus::Stopped;
        upsert_agent_state_tx(tx, &state)?;
        let timer = TimerRecord { id:"legacy-one-shot".into(), agent_id:f.key.agent_id.clone(),
            created_at:f.time, duration_ms:1, interval_ms:None, repeat:false, status:TimerStatus::Completed,
            summary:None, next_fire_at:None, last_fired_at:Some(f.time), fire_count:1 };
        upsert_timer_tx(tx,&timer)?;
        tx.execute("INSERT INTO timer_wakes(timer_id,message_id,fire_count,status,created_at,updated_at) VALUES('legacy-one-shot','old-wake',1,'pending',?1,?1)", [f.time.to_rfc3339()])?;
        Ok(())
    }).unwrap();
    let command = f.command(ControlAction::Stop);
    assert_eq!(command.candidate.as_ref(), &command.expected.state);
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("not applied")
    };
    assert!(receipt.outcome.changed);
    assert!(
        receipt.outcome.aborted_queue_message_ids.is_empty()
            && receipt.outcome.dropped_timer_queue_message_ids.is_empty()
    );
    assert_eq!(
        receipt.outcome.cancelled_timer_wake_message_ids,
        vec!["old-wake"]
    );
    assert!(delta.transition.effects.agent_state.is_none());
    assert_eq!(delta.timer_wakes[0].status, "cancelled");
}

#[test]
fn attention_handoff_preserves_first_evidence_and_cannot_replace_completion() {
    let f = Fixture::new();
    f.seed_all();
    let command = f.command(ControlAction::Stop);
    f.db.commit_control(&command).unwrap();
    let target = &command.cancellation_targets[0];
    let attention_at = command.prepared_at + chrono::Duration::seconds(1);
    let evidence = json!({"reason":"original_process_lease_unavailable"});
    assert!(f
        .db
        .mark_control_obligation_needs_attention(
            command.key(),
            target,
            attention_at,
            evidence.clone()
        )
        .unwrap());
    let attention =
        f.db.control_obligation(command.key(), target)
            .unwrap()
            .unwrap();
    assert_eq!(attention.state, ControlObligationState::NeedsAttention);
    assert_eq!(attention.completion_evidence, Some(evidence));
    assert!(f
        .db
        .mark_control_obligation_needs_attention(
            command.key(),
            target,
            attention_at + chrono::Duration::seconds(1),
            json!({"later":true})
        )
        .unwrap());
    assert_eq!(
        f.db.control_obligation(command.key(), target).unwrap(),
        Some(attention)
    );
    let mut wrong = target.clone();
    wrong.target_generation["runtime_lease_id"] = json!("replacement-lease");
    assert!(f
        .db
        .mark_control_obligation_needs_attention(
            command.key(),
            &wrong,
            attention_at,
            json!({"wrong":true})
        )
        .is_err());
    assert!(f.db.control_obligation(command.key(), &wrong).is_err());
    assert!(f
        .db
        .complete_control_obligation(
            command.key(),
            target,
            attention_at + chrono::Duration::seconds(2),
            json!({"exact_original_joined":true})
        )
        .unwrap());
    let completed =
        f.db.control_obligation(command.key(), target)
            .unwrap()
            .unwrap();
    assert_eq!(completed.state, ControlObligationState::Completed);
    assert!(!f
        .db
        .mark_control_obligation_needs_attention(
            command.key(),
            target,
            attention_at + chrono::Duration::seconds(3),
            json!({"late_attention":true})
        )
        .unwrap());
    assert_eq!(
        f.db.control_obligation(command.key(), target).unwrap(),
        Some(completed)
    );
    let mut missing = target.clone();
    missing.obligation_id = "missing-obligation".into();
    assert!(f
        .db
        .control_obligation(command.key(), &missing)
        .unwrap()
        .is_none());
}

#[test]
fn stop_persists_an_explicit_absent_run_obligation_and_replays_the_original_target() {
    let f = Fixture::new();
    f.seed_all();
    let mut input = f.input(ControlAction::Stop);
    let run = input
        .cancellation_targets
        .iter_mut()
        .find(|target| target.target_kind == ControlCancellationKind::Run)
        .unwrap();
    run.target_generation["runtime_lease_id"] = Value::Null;
    let expected = run.clone();
    let command = prepare_control(input).unwrap();
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("new Stop replayed");
    };
    assert!(receipt.obligation_ids.contains(&expected.obligation_id));
    assert_eq!(delta.state.current_run_id, None);
    let obligation =
        f.db.control_obligation(command.key(), &expected)
            .unwrap()
            .unwrap();
    assert_eq!(obligation.state, ControlObligationState::Pending);
    assert!(obligation.completion_evidence.is_none());
    let ControlCommit::Replayed { receipt: replayed } = f.db.commit_control(&command).unwrap()
    else {
        panic!("original Stop did not replay");
    };
    assert_eq!(replayed, receipt);
    assert_eq!(
        f.db.control_obligation(command.key(), &expected).unwrap(),
        Some(obligation)
    );
}

#[test]
fn stop_rejects_missing_run_or_implicit_optional_generation_facts() {
    let f = Fixture::new();
    f.seed_all();
    let mut omitted = f.input(ControlAction::Stop);
    omitted
        .cancellation_targets
        .retain(|target| target.target_kind != ControlCancellationKind::Run);
    assert!(prepare_control(omitted).is_err());
    for field in ["runtime_lease_id", "activation_id", "source_message_id"] {
        let mut input = f.input(ControlAction::Stop);
        let run = input
            .cancellation_targets
            .iter_mut()
            .find(|target| target.target_kind == ControlCancellationKind::Run)
            .unwrap();
        run.target_generation.as_object_mut().unwrap().remove(field);
        assert!(prepare_control(input).is_err(), "missing {field}");
    }
    let mut wrong = f.input(ControlAction::Stop);
    wrong
        .cancellation_targets
        .iter_mut()
        .find(|target| target.target_kind == ControlCancellationKind::Run)
        .unwrap()
        .target_identity = "replacement-run".into();
    assert!(prepare_control(wrong).is_err());
    let mut unprotected = f.input(ControlAction::Stop);
    unprotected.protected_control_run_id = Some("original-run".into());
    assert!(prepare_control(unprotected).is_err());
}

#[test]
fn absent_run_claim_requires_its_actual_started_turn_and_open_source() {
    for started in [false, true] {
        let f = Fixture::new();
        let message = f.seed_command_claim(started, false);
        let mut input = f.input(ControlAction::Stop);
        let run = input
            .cancellation_targets
            .iter_mut()
            .find(|target| target.target_kind == ControlCancellationKind::Run)
            .unwrap();
        run.target_generation = json!({"owner_epoch":1,"runtime_lease_id":null,
            "activation_id":"attempt-1","source_message_id":message.id});
        if started {
            let command = prepare_control(input).unwrap();
            assert!(matches!(
                f.db.commit_control(&command).unwrap(),
                ControlCommit::Applied { .. }
            ));
        } else {
            assert!(prepare_control(input).is_err());
        }
    }
    let f = Fixture::new();
    f.seed_command_claim(true, false);
    let mut input = f.input(ControlAction::Stop);
    let run = input
        .cancellation_targets
        .iter_mut()
        .find(|target| target.target_kind == ControlCancellationKind::Run)
        .unwrap();
    run.target_generation = json!({"owner_epoch":1,"runtime_lease_id":null,
        "activation_id":"attempt-1","source_message_id":"other-source"});
    assert!(prepare_control(input).is_err());
}

#[test]
fn protected_command_without_original_self_run_proof_still_requires_absent_run() {
    let f = Fixture::new();
    let message = f.seed_command_claim(true, false);
    let mut input = f.input(ControlAction::Stop);
    input.invocation.source = ControlSource::ControlMessage {
        message_id: message.id,
    };
    input.protected_control_attempt = Some("attempt-1".into());
    input.cancellation_targets[0].target_generation["runtime_lease_id"] = Value::Null;
    let mut omitted = f.input(ControlAction::Stop);
    omitted.invocation = input.invocation.clone();
    omitted.protected_control_attempt = input.protected_control_attempt.clone();
    omitted.cancellation_targets.clear();
    assert!(prepare_control(omitted).is_err());
    let mut wrong = f.input(ControlAction::Stop);
    wrong.invocation = input.invocation.clone();
    wrong.protected_control_attempt = input.protected_control_attempt.clone();
    wrong.protected_control_run_id = Some("replacement-run".into());
    wrong.cancellation_targets.clear();
    assert!(prepare_control(wrong).is_err());
    let command = prepare_control(input).unwrap();
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control(&command).unwrap() else {
        panic!("new Stop replayed");
    };
    assert_eq!(receipt.obligation_ids.len(), 1);
    assert!(receipt.outcome.interrupted_attempt_ids.is_empty());
    assert_eq!(
        delta.execution_protocol.unwrap().attempts["attempt-1"].state,
        ExecutionAttemptState::Open
    );
}

#[test]
fn pending_scope_inventory_keeps_retired_attention_and_excludes_completed() {
    let mut f = Fixture::new();
    let mut state =
        f.db.agent_states()
            .latest(&f.key.agent_id)
            .unwrap()
            .unwrap();
    state.status = AgentStatus::AwakeRunning;
    state.current_run_id = Some("old-run".into());
    f.db.agent_states().upsert_for_fixture(&state).unwrap();
    let old = f.command(ControlAction::Stop);
    f.db.commit_control(&old).unwrap();
    let old_target = &old.cancellation_targets[0];
    f.db.mark_control_obligation_needs_attention(
        old.key(),
        old_target,
        f.time + chrono::Duration::seconds(11),
        json!({"original_lease_absent":true}),
    )
    .unwrap();
    let mut successor = old.expected.identity.clone();
    successor.incarnation = 2;
    successor.revision += 1;
    f.db.agent_identities().upsert(&successor).unwrap();
    state.current_run_id = Some("new-run".into());
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id = ?1",
            [&f.key.agent_id],
        )?;
        upsert_agent_state_tx(tx, &state)
    })
    .unwrap();
    f.key.identity_incarnation = 2;
    f.key.command_id = "successor-stop".into();
    let current = f.command(ControlAction::Stop);
    f.db.commit_control(&current).unwrap();
    assert_eq!(
        f.db.pending_control_obligation_scopes().unwrap(),
        vec![(f.key.agent_id.clone(), 1), (f.key.agent_id.clone(), 2)]
    );
    f.db.complete_control_obligation(
        current.key(),
        &current.cancellation_targets[0],
        f.time + chrono::Duration::seconds(12),
        json!({"exact_original_joined":true}),
    )
    .unwrap();
    assert_eq!(
        f.db.pending_control_obligation_scopes().unwrap(),
        vec![(f.key.agent_id.clone(), 1)]
    );
    f.db.complete_control_obligation(
        old.key(),
        old_target,
        f.time + chrono::Duration::seconds(12),
        json!({"exact_original_joined":true}),
    )
    .unwrap();
    assert!(f.db.pending_control_obligation_scopes().unwrap().is_empty());
}

#[test]
fn key_only_recovery_reads_verified_original_receipt_after_retirement() {
    let f = Fixture::new();
    assert!(f.db.control_receipt_for_recovery(&f.key).unwrap().is_none());
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, .. } = f.db.commit_control(&command).unwrap() else {
        panic!("new command did not apply")
    };
    f.db.connection().unwrap().execute(
        "UPDATE agent_identities SET status='deleted',payload_json=json_set(payload_json,'$.status','deleted') WHERE agent_id=?1",
        [&f.key.agent_id],
    ).unwrap();
    assert_eq!(
        f.db.control_receipt_for_recovery(command.key()).unwrap(),
        Some(receipt)
    );
    let mut other = f.key.clone();
    other.identity_incarnation += 1;
    assert!(f.db.control_receipt_for_recovery(&other).unwrap().is_none());
}

#[test]
fn key_only_recovery_rejects_receipt_column_and_embedded_key_corruption() {
    let f = Fixture::new();
    f.db.commit_control(&f.command(ControlAction::Stop))
        .unwrap();
    f.db.connection()
        .unwrap()
        .execute("UPDATE control_command_receipts SET action='start'", [])
        .unwrap();
    assert!(f.db.control_receipt_for_recovery(&f.key).is_err());
    f.db.connection().unwrap().execute(
        "UPDATE control_command_receipts SET action='stop',receipt_json=json_set(receipt_json,'$.key.identity_incarnation',2)",
        [],
    ).unwrap();
    assert!(f.db.control_receipt_for_recovery(&f.key).is_err());
}
