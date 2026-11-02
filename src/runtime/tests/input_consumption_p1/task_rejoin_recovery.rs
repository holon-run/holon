//! Canonical task-result replay across runtime reconstruction (#3669).
use super::*;
use crate::runtime_error::{IdleInputRecoveryPredicate, IdleInputRecoveryRejection};

async fn publish_result_with_ingress(
    runtime: &RuntimeHandle,
    owner_id: &str,
    task_id: &str,
    kind: TaskKind,
    ingress_turn: bool,
) -> String {
    let agent_id = runtime.agent_id().await.unwrap();
    persist_completed_task_parent_fixture(runtime, "p1-mixed-parent", Some(owner_id))
        .await
        .unwrap();
    let detail = serde_json::json!({
        "rejoin_obligation_id": task_id,
        "rejoin_generation": 1,
        "parent_turn_id": "p1-mixed-parent",
        "terminal_reentry": true,
    });
    let mut message = MessageEnvelope::new(
        &agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "replay source result body".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    // Match command_task.rs and the child result producer, not the older
    // turnless batching fixture: real terminal results have an ingress turn.
    message.turn_id = ingress_turn.then(crate::ids::turn_id);
    message.work_item_id = Some(owner_id.into());
    message.metadata = Some(serde_json::json!({
        "task_id": task_id,
        "task_kind": kind.as_str(),
        "task_status": "completed",
        "task_result_id": message.id,
        "task_detail": detail,
        "work_item_id": owner_id,
    }));
    let task = TaskRecord {
        id: task_id.into(),
        agent_id,
        kind,
        status: TaskStatus::Completed,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: Some(message.id.clone()),
        work_item_id: Some(owner_id.into()),
        summary: None,
        detail: Some(detail),
        recovery: None,
    };
    if kind == TaskKind::ChildAgentTask {
        runtime
            .commit_terminal_child_task_result(&task, "task_completed", &message, None)
            .await
            .unwrap();
    } else {
        runtime
            .commit_terminal_task_result(&task, "task_completed", &message)
            .await
            .unwrap();
    }
    message.id
}

async fn interrupted_result(
    kind: TaskKind,
    exact_wait: bool,
    ingress_turn: bool,
    snapshot: bool,
) -> (LifecycleHarness, Arc<BatchProvider>, String, TurnRecord) {
    let provider = Arc::new(BatchProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "reproduce task-result replay source selection".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let task_id = format!("replay-source-{}", kind.as_str());
    let message_id =
        publish_result_with_ingress(runtime, &owner.id, &task_id, kind, ingress_turn).await;
    // Keep the historical parent before the first live dispatch.
    let offset = Utc::now()
        .signed_duration_since(harness.now())
        .to_std()
        .unwrap();
    harness
        .advance(offset + std::time::Duration::from_secs(60))
        .await;
    if exact_wait {
        let outcome = runtime
            .register_wait_for_outcome(
                "default",
                Some(owner.id.clone()),
                WaitForWakeKind::TaskResult,
                Some(task_id),
                "reproduce an interrupted exact task-result wait".into(),
                None,
            )
            .await
            .unwrap();
        let WaitForRegistrationOutcome::TaskResultQueued {
            wait_condition_id, ..
        } = outcome
        else {
            panic!("terminal task must queue its exact waiter");
        };
        let owner = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
        seed_live_waiting_work_execution(runtime, &owner, &wait_condition_id).await;
    }
    let anchor = start_without_provider(runtime).await;
    let activation = runtime
        .agent_state()
        .await
        .unwrap()
        .current_execution_binding
        .unwrap()
        .activation_id
        .unwrap();
    let now = runtime.now();
    runtime
        .apply_task_result_operation((&message_id, &activation, now), || {
            crate::runtime_db::transitions::TaskResultOperation::AdmitMessage {
                message_id: message_id.clone(),
                activation_id: activation.clone(),
                projection: None,
                at: now,
            }
        })
        .await
        .unwrap()
        .record()
        .unwrap()
        .as_ref()
        .unwrap();
    if snapshot {
        runtime
            .admit_idle_task_results(&anchor, 24 * 1024)
            .await
            .unwrap();
        assert!(runtime
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .unwrap());
    }
    let source = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    if snapshot {
        assert_eq!(
            source.idle_input_batch.as_ref().unwrap().activation_id,
            Some(activation.clone())
        );
    }
    let execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime
            .runtime_db()
            .transaction(|tx| {
                crate::runtime_db::task_result_settlement::canonical_live_turn_id_tx(
                    tx,
                    &execution.attempts[&activation],
                )
            })
            .unwrap(),
        snapshot.then(|| source.turn_id.clone())
    );
    assert!(provider.requests.lock().await.is_empty());
    finish_claimed_test_run(runtime).await;
    let (recovered_harness, startup) = Box::pin(harness.recover_daemon_startup()).await;
    harness = recovered_harness;
    assert_eq!(startup.interrupted_execution_attempts, 1);
    assert_eq!(startup.interrupted_turns, 1);
    harness
        .runtime()
        .recover_scheduler_bootstrap_claims()
        .await
        .unwrap();
    (harness, provider, message_id, source)
}

#[tokio::test(start_paused = true)]
async fn context_admission_recovery_charges_frozen_companion_before_rebinding() {
    check_frozen_companion_recovery(false).await;
}

#[tokio::test(start_paused = true)]
async fn context_admission_recovery_charges_frozen_ordinary_batch_before_rebinding() {
    check_frozen_companion_recovery(true).await;
}

async fn check_frozen_companion_recovery(with_follower: bool) {
    let provider = Arc::new(BatchProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "preserve the admitted result representation".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let mut prepared = AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id: "default".into(),
            content: MessageBody::Text {
                text: "small anchor".into(),
            },
            client_idempotency_key: "frozen-small-anchor".into(),
            correlation_id: None,
            causation_id: None,
            requested_priority: Some(Priority::Next),
            forward: false,
            reply_context_id: None,
        },
        AgentMessageCallerContext {
            caller_principal: "agent:peer".into(),
            caller_agent_id: Some("peer".into()),
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            route: "agent_message".into(),
            origin: MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: None,
            current_task_id: None,
            current_work_item_id: None,
            inherited_routing_context: None,
            routing_activation_id: None,
        },
    )
    .unwrap();
    prepared.message.work_item_id = Some(owner.id.clone());
    runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap();
    let follower_id = if with_follower {
        let mut follower = AgentMessageDeliveryService::prepare(
            AgentMessageSendRequest {
                target_agent_id: "default".into(),
                content: MessageBody::Text {
                    text: "frozen ordinary follower 汉字 ".repeat(100),
                },
                client_idempotency_key: "frozen-ordinary-follower".into(),
                correlation_id: None,
                causation_id: None,
                requested_priority: Some(Priority::Next),
                forward: false,
                reply_context_id: None,
            },
            prepared.record.caller.clone(),
        )
        .unwrap();
        follower.message.work_item_id = Some(owner.id.clone());
        runtime
            .agent_message_delivery_service()
            .deliver(&follower)
            .await
            .unwrap();
        Some(follower.message.id)
    } else {
        None
    };
    let companion_id = publish_owner_result_with_priority(
        runtime,
        &owner.id,
        "frozen-large-companion".into(),
        &"frozen evidence ".repeat(450),
        Priority::Next,
    )
    .await;
    // Delivery uses wall time; the fixed cutoff follows both accepted messages.
    let offset = Utc::now()
        .signed_duration_since(harness.now())
        .to_std()
        .unwrap();
    harness.advance(offset).await;
    let anchor = start_without_provider(runtime).await;
    assert_eq!(anchor.id, prepared.message.id);
    let predecessor = runtime
        .agent_state()
        .await
        .unwrap()
        .current_execution_binding
        .unwrap()
        .activation_id
        .unwrap();
    let repository = runtime.runtime_db().task_result_settlements();
    assert!(runtime
        .admit_idle_task_results(&anchor, 24 * 1024)
        .await
        .unwrap());
    runtime
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap();
    let before = repository
        .latest_for_message(&companion_id)
        .unwrap()
        .unwrap();
    assert!(before
        .source_projection
        .as_ref()
        .unwrap()
        .rendered
        .contains("representation: full"));
    let records = repository
        .admitted_for_activation("default", &predecessor)
        .unwrap();
    let pending =
        crate::context::render_pending_task_result_records(runtime.storage(), &anchor, &records)
            .unwrap()
            .unwrap();
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let frozen_batch = turn.idle_input_batch.as_ref().unwrap();
    assert_eq!(frozen_batch.items.len(), 1 + usize::from(with_follower));
    if let Some(id) = &follower_id {
        assert_eq!(&frozen_batch.items[1].message_id, id);
    }
    let current = input_consumption::render_batch(frozen_batch);
    let low_bytes = current.len() + pending.len() - 1;
    let low_tokens = crate::context::estimate_text_tokens(&format!("[current_input]\n{current}"))
        + crate::context::estimate_text_tokens(&format!("[pending_task_results]\n{pending}"))
        - 1;
    assert!(low_bytes < input_consumption::INPUT_BATCH_BYTES);
    if with_follower {
        let anchor_only = format!(
            "{}{}",
            input_consumption::BATCH_HEADER,
            frozen_batch.items[0].rendered
        );
        assert!(anchor_only.len() + pending.len() <= low_bytes);
        assert!(input_consumption::projection::InputBudget {
            tokens: low_tokens,
            bytes: 24 * 1024,
        }
        .fits_idle(&anchor_only, Some(&pending)));
    }
    // Seed the crash window after the notification was drained but before
    // its separately admitted source was delivered to the model.
    let commit = {
        use crate::runtime_db::transitions::{
            AgentStateMutation, PreparedQueueTransition, QueueMutation, QueueOperation,
            QueueTransitionCommand,
        };
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let mut notification = runtime
            .runtime_db()
            .queue_entries()
            .latest(&companion_id)
            .unwrap()
            .unwrap();
        mutation
            .queue
            .pop_next_matching(|queued| queued.id == companion_id);
        mutation.state.pending = mutation.queue.len();
        notification.status = QueueEntryStatus::Processed;
        notification.updated_at = runtime.now().max(notification.updated_at);
        let prepared = PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: notification.agent_id.clone(),
            operation: QueueOperation::Settle,
            mutation: QueueMutation::Upsert(notification),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(mutation.last_persisted_state.clone())),
                record: Box::new(mutation.state.clone()),
            }),
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: Vec::new(),
        });
        let commit = mutation
            .commit_prepared_queue(crate::ids::audit_event_id(), prepared)
            .await
            .unwrap();
        assert!(commit.applied);
        commit
    };
    runtime.apply_transition_commit(commit).await;
    finish_claimed_test_run(runtime).await;
    let (recovered_harness, startup) = Box::pin(harness.recover_daemon_startup()).await;
    harness = recovered_harness;
    assert_eq!(startup.interrupted_execution_attempts, 1);
    assert_eq!(startup.interrupted_turns, 1);
    harness.advance(std::time::Duration::from_secs(60)).await;
    harness.restart();
    let runtime = harness.runtime();
    runtime.recover_scheduler_bootstrap_claims().await.unwrap();
    let recovered = start_without_provider(runtime).await;
    assert_eq!(recovered.id, anchor.id);
    assert_eq!(recovered.work_item_id.as_deref(), Some(owner.id.as_str()));
    let successor = runtime
        .agent_state()
        .await
        .unwrap()
        .current_execution_binding
        .unwrap()
        .activation_id
        .unwrap();
    assert_ne!(successor, predecessor);
    // The small anchor may rebind, but its larger companion must not be
    // reselected as a preview just to pass this reduced admission budget.
    let repository = runtime.runtime_db().task_result_settlements();
    for budget in [
        input_consumption::projection::InputBudget {
            tokens: usize::MAX,
            bytes: low_bytes,
        },
        input_consumption::projection::InputBudget {
            tokens: low_tokens,
            bytes: 24 * 1024,
        },
    ] {
        runtime
            .admit_idle_task_results(&recovered, budget)
            .await
            .unwrap();
        let retained = repository
            .latest_for_message(&companion_id)
            .unwrap()
            .unwrap();
        assert_eq!(retained.activation_id, before.activation_id);
        assert_eq!(retained.admitted_at, before.admitted_at);
        assert_eq!(retained.source_projection, before.source_projection);
        assert!(repository
            .admitted_for_activation("default", &successor)
            .unwrap()
            .iter()
            .all(|record| record.result_identity != before.result_identity));
    }
    runtime
        .admit_idle_task_results(&recovered, 24 * 1024)
        .await
        .unwrap();
    let rebound = repository
        .latest_for_message(&companion_id)
        .unwrap()
        .unwrap();
    assert_eq!(rebound.activation_id.as_deref(), Some(successor.as_str()));
    assert_eq!(rebound.admitted_at, before.admitted_at);
    assert_eq!(rebound.source_projection, before.source_projection);
    runtime
        .attach_idle_sender_inputs(&recovered, 24 * 1024)
        .await
        .unwrap();
    let restored = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let restored_batch = restored.idle_input_batch.unwrap();
    assert_eq!(restored_batch.items, frozen_batch.items);
    assert_eq!(input_consumption::render_batch(&restored_batch), current);
    assert!(current.len() + pending.len() <= 24 * 1024);
    assert!(provider.requests.lock().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn p1_issue_3669_real_task_rejoin_restart_keeps_source_and_delivers_once() {
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        let (harness, provider, message_id, source) =
            interrupted_result(kind, false, true, true).await;
        let runtime = harness.runtime();
        let interrupted = runtime
            .storage()
            .read_turn_by_id(&source.turn_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            interrupted.terminal.unwrap().kind,
            TurnTerminalKind::Interrupted
        );
        let scheduled = claim_model_input(runtime).await;
        assert_eq!(scheduled.message.id, message_id);
        assert_eq!(
            scheduled.message.source_refs.get("replay_source_turn_id"),
            Some(&source.turn_id)
        );
        process_result_batch(runtime, scheduled).await;
        let recovered = runtime.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(recovered.replay.unwrap().source_turn_id, source.turn_id);
        assert_eq!(
            recovered.idle_input_batch.unwrap().items,
            source.idle_input_batch.unwrap().items
        );
        let settlement = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&message_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            settlement.disposition,
            Some(crate::runtime_db::TaskResultSettlementDisposition::ModelDelivered)
        );
        assert_eq!(provider.requests.lock().await.len(), 1);
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        assert_eq!(provider.requests.lock().await.len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn issue_3484_consumed_task_result_keeps_backlog_live_across_restart() {
    use crate::runtime_db::task_result_settlement::TaskResultSettlementState;
    use crate::runtime_db::TaskResultSettlementDisposition;

    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        for restart in [false, true] {
            let (mut harness, provider, result_id, _) =
                interrupted_result(kind, true, true, true).await;
            let runtime = harness.runtime();
            let scheduled = claim_model_input(runtime).await;
            assert_eq!(scheduled.message.id, result_id);
            process_result_batch(runtime, scheduled).await;
            let delivered = runtime.storage().read_recent_turns(1).unwrap().remove(0);
            let consumed = runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&result_id)
                .unwrap()
                .unwrap();
            assert_eq!(consumed.state, TaskResultSettlementState::Settled);
            assert_eq!(
                consumed.disposition,
                Some(TaskResultSettlementDisposition::ModelDelivered)
            );
            assert_eq!(provider.requests.lock().await.len(), 1);
            let task = runtime
                .storage()
                .latest_task_record(&format!("replay-source-{}", kind.as_str()))
                .unwrap()
                .unwrap();

            // The text-only provider leaves its owner runnable. Park that work
            // so autonomous continuations do not obscure backlog delivery.
            runtime
                .update_work_item_fields(
                    task.work_item_id.clone().unwrap(),
                    None,
                    Some(WorkItemPlanStatus::NeedsInput),
                    None,
                    None,
                    None,
                )
                .await
                .unwrap();

            // Exercise today's durable consumption contract before the legacy
            // envelope; this is not a replay of the old database transaction.
            let mut late = MessageEnvelope::new(
                "default",
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: task.id.clone(),
                },
                AuthorityClass::OperatorInstruction,
                Priority::Background,
                MessageBody::Text {
                    text: "already consumed task notification".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            );
            late.turn_id = Some(delivered.turn_id.clone());
            late.metadata = Some(serde_json::json!({
                "task_id": task.id,
                "task_kind": kind.as_str(),
                "task_status": "completed",
                "task_detail": task.detail,
            }));
            let late = runtime.enqueue(late).await.unwrap();
            let follower = runtime
                .enqueue(
                    MessageEnvelope::new(
                        "default",
                        MessageKind::InternalFollowup,
                        MessageOrigin::System {
                            subsystem: "issue-3484-backlog".into(),
                        },
                        AuthorityClass::OperatorInstruction,
                        Priority::Background,
                        MessageBody::Text {
                            text: "handle the already queued internal report".into(),
                        },
                    )
                    .with_admission(
                        MessageDeliverySurface::RuntimeSystem,
                        AdmissionContext::RuntimeOwned,
                    ),
                )
                .await
                .unwrap();
            assert!(late.work_item_id.is_none());
            assert!(follower.work_item_id.is_none());
            if restart {
                harness.restart();
            }
            let runtime = harness.runtime();
            let mut runner = tokio::spawn(runtime.clone().run());
            let drained = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    assert!(
                        !runner.is_finished(),
                        "runtime exited before draining the backlog: {:?}",
                        (&mut runner).await
                    );
                    let entries = runtime.storage().latest_queue_entries().unwrap();
                    if [&late.id, &follower.id].iter().all(|message_id| {
                        entries.iter().any(|entry| {
                            &entry.message_id == *message_id
                                && entry.status == QueueEntryStatus::Processed
                        })
                    }) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await;
            runner.abort();
            let _ = runner.await;
            assert!(
                drained.is_ok(),
                "a consumed task notification must not strand queued input: restart={restart}, \
                 kind={kind:?}, state={:?}, queue={:?}, runtime_errors={:?}",
                runtime.agent_state().await.unwrap(),
                runtime.storage().latest_queue_entries().unwrap(),
                runtime
                    .storage()
                    .read_recent_events(128)
                    .unwrap()
                    .into_iter()
                    .filter(|event| event.kind == "runtime_error")
                    .map(|event| event.data)
                    .collect::<Vec<_>>(),
            );
            assert_eq!(provider.requests.lock().await.len(), 2);
            let late_turn = runtime
                .storage()
                .read_recent_turns(16)
                .unwrap()
                .into_iter()
                .find(|turn| turn.input_message_ids.contains(&late.id))
                .expect("consumed notification must have its own actual reducer Turn");
            assert_ne!(late_turn.turn_id, delivered.turn_id);
            assert!(late_turn.turn_index > delivered.turn_index);
            assert!(late_turn.replay.is_none());
            assert_eq!(
                runtime
                    .storage()
                    .read_turn_by_id(&delivered.turn_id)
                    .unwrap(),
                Some(delivered)
            );
            assert_eq!(
                runtime
                    .runtime_db()
                    .task_result_settlements()
                    .latest_for_message(&result_id)
                    .unwrap(),
                Some(consumed)
            );
            assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
            let execution = runtime
                .runtime_db()
                .transitions()
                .load_execution_protocol_state_if_initialized("default")
                .unwrap()
                .unwrap();
            assert!(!execution
                .attempts
                .contains_key(&scheduler_executor::canonical_activation_id(&late.id)));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn p1_issue_3669_reloading_ingress_loses_claim_selector_but_not_durable_edge() {
    let (harness, provider, message_id, source) =
        interrupted_result(TaskKind::CommandTask, false, true, true).await;
    let runtime = harness.runtime();
    let anchor = start_without_provider(runtime).await;
    let recovered = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(
        recovered.replay.as_ref().unwrap().source_turn_id,
        source.turn_id
    );
    assert_eq!(
        anchor.source_refs.get("replay_source_turn_id"),
        Some(&source.turn_id)
    );
    // Deliberately substitute immutable ingress for the scheduler's claim.
    // This characterizes the predicate, not a proven production call path.
    let ingress = runtime
        .storage()
        .read_message_by_id(&message_id)
        .unwrap()
        .unwrap();
    assert!(!ingress.source_refs.contains_key("replay_source_turn_id"));
    let error = runtime
        .attach_idle_sender_inputs(&ingress, 24 * 1024)
        .await
        .unwrap_err();
    let rejection = error.downcast_ref::<IdleInputRecoveryRejection>().unwrap();
    assert_eq!(
        rejection.predicate,
        IdleInputRecoveryPredicate::ReplaySourceMismatch
    );
    assert_eq!(rejection.field, "replay.source_selector");
    assert!(provider.requests.lock().await.is_empty());
    assert_eq!(
        runtime.storage().read_recent_turns(1).unwrap()[0],
        recovered
    );
    assert!(runtime
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
}

#[tokio::test(start_paused = true)]
async fn p1_issue_3669_exact_wait_restart_replays_canonical_edge() {
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        for ingress_turn in [false, true] {
            for snapshot in [false, true] {
                assert_exact_wait_restart(kind, ingress_turn, snapshot).await;
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn p1_issue_3669_queued_exact_recovery_quarantines_invalid_attachment() {
    for corruption in ["owner", "agent", "terminal"] {
        let (harness, provider, message_id, source) =
            interrupted_result(TaskKind::CommandTask, true, true, true).await;
        let runtime = harness.runtime();
        let mut source = runtime
            .storage()
            .read_turn_by_id(&source.turn_id)
            .unwrap()
            .unwrap();
        let parent = runtime
            .storage()
            .read_turn_by_id("p1-mixed-parent")
            .unwrap()
            .unwrap();
        match corruption {
            "owner" => source.owner = Some(TurnOwner::Command),
            "agent" => source.agent_id = "foreign-agent".into(),
            "terminal" => source.terminal.as_mut().unwrap().kind = TurnTerminalKind::Completed,
            _ => unreachable!(),
        }
        // Corruption injection only: normal writes preserve immutable identity.
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .connection()
                .unwrap()
                .execute(
                    "UPDATE turn_records SET payload_json = ?1 WHERE turn_id = ?2",
                    rusqlite::params![serde_json::to_string(&source).unwrap(), source.turn_id],
                )
                .unwrap(),
            1
        );
        let settlement = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&message_id)
            .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                    .poll()
                    .await
                    .unwrap(),
                scheduler_executor::RunLoopPoll::AuthorityBlocked
            ));
        }
        assert!(matches!(
            scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                .poll()
                .await
                .unwrap(),
            scheduler_executor::RunLoopPoll::Idle
        ));
        assert!(provider.requests.lock().await.is_empty());
        assert_eq!(
            runtime.storage().read_recent_turns(2).unwrap(),
            vec![parent, source]
        );
        assert_eq!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&message_id)
                .unwrap(),
            settlement
        );
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(&message_id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Quarantined
        );
    }
}

async fn assert_exact_wait_restart(kind: TaskKind, ingress_turn: bool, snapshot: bool) {
    let (harness, provider, message_id, source) =
        interrupted_result(kind, true, ingress_turn, snapshot).await;
    let runtime = harness.runtime();
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    let expected_source = (snapshot || ingress_turn).then(|| source.turn_id.clone());
    if snapshot {
        assert_eq!(
            runtime
                .storage()
                .read_turn_by_id(&source.turn_id)
                .unwrap()
                .unwrap()
                .terminal
                .unwrap()
                .kind,
            TurnTerminalKind::Interrupted
        );
    }
    let prior_execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    let predecessor = prior_execution
        .attempts
        .values()
        .find(|attempt| attempt.source_message_id.as_deref() == Some(&message_id))
        .unwrap();
    let scheduled = claim_model_input(runtime).await;
    assert_eq!(scheduled.message.id, message_id);
    assert_eq!(
        scheduled.message.turn_id.as_ref(),
        ingress_turn.then_some(&source.turn_id)
    );
    assert_eq!(
        scheduled.message.source_refs.get("replay_source_turn_id"),
        expected_source.as_ref()
    );
    let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = scheduled
        .dispatch_plan
        .execution_admission_provenance
        .as_ref()
    else {
        panic!("recovery must be canonical");
    };
    assert!(activation_id.ends_with(":attempt:2"));
    let execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution.attempts[activation_id].recovery_of_attempt_id,
        Some(predecessor.attempt_id.clone())
    );
    process_result_batch(runtime, scheduled).await;
    let recovered = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    // A real StartedDispatch keeps its actual assignment even without a batch
    // or ingress hint. Host startup has now closed that exact original Turn.
    let original = runtime
        .storage()
        .read_turn_by_id(&source.turn_id)
        .unwrap()
        .unwrap();
    let replay = recovered.replay.as_ref().unwrap();
    assert_eq!(replay.source_turn_id, source.turn_id,);
    assert_eq!(replay.source_message_id, message_id);
    assert_eq!(replay.prior_terminal, original.terminal);
    assert_eq!(replay.reason, "interrupted_queue_claim_reentry",);
    if snapshot {
        assert_eq!(
            recovered.idle_input_batch.unwrap().items,
            source.idle_input_batch.unwrap().items
        );
    }
    assert_eq!(provider.requests.lock().await.len(), 1);
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&message_id)
            .unwrap()
            .unwrap()
            .disposition,
        Some(crate::runtime_db::TaskResultSettlementDisposition::ModelDelivered)
    );
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(provider.requests.lock().await.len(), 1);
}
