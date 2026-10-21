//! Characterize idle admission separately from exact waits and live safe-points.
use super::*;
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementDisposition, TaskResultSettlementState,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
enum EntryPath {
    Direct,
    BusyDirect,
    Recovery,
}

#[tokio::test(start_paused = true)]
async fn p1_recovery_batching_pre_cutoff_1_2_8_9_64_preserves_delivery_identities() {
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        for count in [1usize, 2, 8, 9, 64] {
            Box::pin(assert_pre_cutoff_batches(kind, count, EntryPath::Recovery)).await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn p1_direct_batching_64_preserves_delivery_identities() {
    // The existing direct/baseline tests cover 1/2/8/9; extend their scan boundary.
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        Box::pin(assert_pre_cutoff_batches(kind, 64, EntryPath::Direct)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn p1_busy_agent_defers_recovery_but_queued_results_keep_direct_batching() {
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        Box::pin(assert_pre_cutoff_batches(kind, 9, EntryPath::BusyDirect)).await;
    }
}

async fn start_unprovided_turn(runtime: &RuntimeHandle) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "defer result admission until the owner is released".into(),
        },
    );
    message.turn_id = Some(crate::ids::turn_id());
    Box::pin(runtime.enqueue(message)).await.unwrap();
    Box::pin(start_without_provider(runtime)).await
}

async fn wait_for_operator(runtime: &RuntimeHandle, owner: &str) {
    let message = Box::pin(start_unprovided_turn(runtime)).await;
    Box::pin(runtime.register_wait_for(
        "default",
        Some(owner.to_owned()),
        WaitForWakeKind::OperatorInput,
        None,
        "hold terminal notifications without model admission".into(),
        None,
    ))
    .await
    .unwrap();
    Box::pin(settle_unprovided_turn(runtime, message)).await;
}

async fn settle_unprovided_turn(runtime: &RuntimeHandle, message: MessageEnvelope) {
    finish_claimed_test_run(runtime).await;
    let terminal = terminal_transition_from_started_turn(runtime, &message);
    assert!(Box::pin(runtime.commit_queue_terminal_settlement(
        QueueEntryRecord {
            message_id: message.id,
            agent_id: message.agent_id,
            priority: message.priority,
            status: QueueEntryStatus::Processed,
            created_at: message.created_at,
            updated_at: Utc::now(),
        },
        Vec::new(),
        true,
        Some(&terminal),
    ))
    .await
    .unwrap());
}

async fn assert_pre_cutoff_batches(kind: TaskKind, count: usize, path: EntryPath) {
    let provider = Arc::new(BatchProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let recovery = matches!(path, EntryPath::Recovery);
    let owner = Box::pin(runtime.create_work_item(
        format!("characterize {path:?} {} {count}", kind.as_str()),
        Some(if recovery {
            WorkItemPlanStatus::NeedsInput
        } else {
            WorkItemPlanStatus::Ready
        }),
        None,
        Vec::new(),
    ))
    .await
    .unwrap();
    if recovery {
        Box::pin(wait_for_operator(runtime, &owner.id)).await;
    } else {
        Box::pin(runtime.pick_work_item(owner.id.clone()))
            .await
            .unwrap();
    }
    let busy_message = if matches!(path, EntryPath::BusyDirect) {
        Some(Box::pin(start_unprovided_turn(runtime)).await)
    } else {
        None
    };
    let mut sources = BTreeMap::new();
    for index in 0..count {
        let body = format!("batch-characterization-body-{index:03}-end");
        let source = Box::pin(publish_owner_result_with_reentry(
            runtime,
            &owner.id,
            format!("characterize-{}-{index:03}", kind.as_str()),
            &body,
            Priority::Normal,
            kind,
            !recovery,
        ))
        .await;
        let record = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&source)
            .unwrap()
            .unwrap();
        assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
        assert!(sources
            .insert(record.result_identity, (source, body))
            .is_none());
    }
    assert!(provider.requests.lock().await.is_empty());
    if let Some(message) = busy_message {
        // Fresh queued notifications have no recovery deadline. Seed due
        // obligations so this scan actually exercises the occupied lane.
        for (source, _) in sources.values() {
            runtime
                .runtime_db()
                .task_result_settlements()
                .mark_deferred(
                    source,
                    "awaiting_canonical_admission",
                    runtime.now(),
                    runtime.now() + chrono::Duration::seconds(30),
                )
                .unwrap();
        }
        harness.advance(std::time::Duration::from_secs(31)).await;
        let due_sources: BTreeSet<_> = runtime
            .runtime_db()
            .task_result_settlements()
            .due_deferred("default", runtime.now(), 8)
            .unwrap()
            .into_iter()
            .map(|record| record.message_id)
            .collect();
        assert_eq!(due_sources.len(), count.min(8));
        assert!(!Box::pin(runtime.emit_due_task_result_recovery())
            .await
            .unwrap());
        let queued = runtime.inner.agent.lock().await.queue.input_snapshot(64);
        assert_eq!(
            queued
                .iter()
                .map(|message| &message.id)
                .collect::<BTreeSet<_>>(),
            sources.values().map(|(source, _)| source).collect()
        );
        for (source, _) in sources.values() {
            let record = runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(source)
                .unwrap()
                .unwrap();
            assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
            assert!(record.activation_id.is_none());
            assert!(record.deferred_at.is_some());
            let next_recheck = record.next_recheck_at.unwrap();
            if due_sources.contains(source) {
                assert_eq!(
                    record.deferred_reason.as_deref(),
                    Some("execution_lane_busy")
                );
                assert!(next_recheck > runtime.now());
            } else {
                // The ninth result is beyond this scan's eight-row limit.
                assert!(next_recheck <= runtime.now());
            }
            assert_eq!(record.recovery_attempts, 0);
        }
        // No provider runs while the lane is occupied. Once it closes, use the
        // original task notifications, not recovery wakes or live attachments.
        Box::pin(settle_unprovided_turn(runtime, message)).await;
        assert!(provider.requests.lock().await.is_empty());
    }
    if recovery {
        // Consume real notifications while ineligible, retaining only obligations.
        for _ in 0..count * 4 + 4 {
            if runtime.inner.agent.lock().await.queue.is_empty() {
                break;
            }
            let executor = scheduler_executor::SchedulerDecisionExecutor::new(runtime);
            let poll = Box::pin(executor.poll()).await.unwrap();
            if let scheduler_executor::RunLoopPoll::Message(scheduled) = poll {
                assert!(!scheduled.scheduler_decision.model_reentry);
                assert!(Box::pin(process_result_batch(runtime, scheduled))
                    .await
                    .is_empty());
            }
        }
        assert!(runtime.inner.agent.lock().await.queue.is_empty());
        assert!(provider.requests.lock().await.is_empty());
        Box::pin(runtime.update_work_item_fields(
            owner.id.clone(),
            None,
            Some(WorkItemPlanStatus::Ready),
            None,
            None,
            None,
        ))
        .await
        .unwrap();
        Box::pin(runtime.pick_work_item_with_reason_and_clear_blocker(
            owner.id.clone(),
            Some("release the characterization owner".into()),
            true,
        ))
        .await
        .unwrap();
        harness.restart();
        Box::pin(harness.runtime().recover_scheduler_bootstrap_claims())
            .await
            .unwrap();
    }
    let runtime = harness.runtime();
    let attempts_before = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap()
        .attempts
        .len();
    // Every result is persisted strictly before the first admission cutoff.
    harness.advance(std::time::Duration::from_secs(31)).await;
    let mut delivered = BTreeSet::new();
    let mut activations = BTreeSet::new();
    let mut batches = Vec::new();
    for _ in 0..count * 4 + 4 {
        if delivered.len() == count {
            break;
        }
        if recovery && runtime.inner.agent.lock().await.queue.is_empty() {
            assert!(Box::pin(runtime.emit_due_task_result_recovery())
                .await
                .unwrap());
            assert!(!Box::pin(runtime.emit_due_task_result_recovery())
                .await
                .unwrap());
        }
        let executor = scheduler_executor::SchedulerDecisionExecutor::new(runtime);
        let poll = Box::pin(executor.poll()).await.unwrap();
        let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            continue;
        };
        if !scheduled.scheduler_decision.model_reentry {
            assert!(Box::pin(process_result_batch(runtime, scheduled))
                .await
                .is_empty());
            continue;
        }
        if recovery {
            assert!(matches!(
                &scheduled.message.origin,
                MessageOrigin::System { subsystem } if subsystem == "task_result_recovery"
            ));
            assert_eq!(scheduled.message.priority, Priority::Background);
        } else {
            assert!(matches!(
                &scheduled.message.origin,
                MessageOrigin::Task { .. }
            ));
        }
        let (activation, records) = Box::pin(deliver_batch(
            runtime, &provider, scheduled, &owner.id, &owner.id, &sources,
        ))
        .await;
        assert!(activations.insert(activation));
        assert_eq!(records.len(), (count - delivered.len()).min(8), "{path:?}");
        batches.push(records.len());
        for record in records {
            assert!(
                delivered.insert(record.result_identity),
                "duplicate admission"
            );
        }
        // Each activation settles exactly its identities; overflow stays pending.
        for (identity, (source, _)) in &sources {
            let record = runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(source)
                .unwrap()
                .unwrap();
            assert_eq!(&record.result_identity, identity);
            if !delivered.contains(identity) {
                assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
                assert!(record.activation_id.is_none());
            }
        }
        if recovery && delivered.len() < count {
            harness.advance(std::time::Duration::from_secs(31)).await;
        }
    }
    assert_eq!(
        delivered,
        sources.keys().cloned().collect(),
        "{path:?}/{batches:?}"
    );
    // A follower's result can settle in an earlier batch while its original
    // notification remains queued. Drain those reducers without re-admission.
    for _ in 0..count * 2 + 8 {
        if runtime.inner.agent.lock().await.queue.is_empty() {
            break;
        }
        let executor = scheduler_executor::SchedulerDecisionExecutor::new(runtime);
        if let scheduler_executor::RunLoopPoll::Message(scheduled) =
            Box::pin(executor.poll()).await.unwrap()
        {
            assert!(!scheduled.scheduler_decision.model_reentry);
            assert!(Box::pin(process_result_batch(runtime, scheduled))
                .await
                .is_empty());
        }
    }
    assert!(runtime.inner.agent.lock().await.queue.is_empty());
    assert_eq!(batches.len(), count.div_ceil(8));
    assert_eq!(provider.requests.lock().await.len(), batches.len());
    let execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(execution.attempts.len() - attempts_before, batches.len());
    for activation in activations {
        assert_eq!(
            execution.attempts[&activation].state,
            crate::domain::execution_protocol::ExecutionAttemptState::Settled
        );
    }
    for (source, _) in sources.values() {
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(source)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Processed
        );
    }
    harness.advance(std::time::Duration::from_secs(31)).await;
    assert!(!Box::pin(runtime.emit_due_task_result_recovery())
        .await
        .unwrap());
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .unsettled_for_agent("default", 64)
        .unwrap()
        .is_empty());
}

async fn deliver_batch(
    runtime: &RuntimeHandle,
    provider: &BatchProvider,
    scheduled: scheduler_executor::ScheduledMessage,
    result_owner: &str,
    consuming_owner: &str,
    sources: &BTreeMap<String, (String, String)>,
) -> (String, Vec<crate::runtime_db::TaskResultSettlementRecord>) {
    let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = scheduled
        .dispatch_plan
        .execution_admission_provenance
        .as_ref()
    else {
        panic!("a result batch must have a canonical activation");
    };
    let activation = activation_id.clone();
    let message = scheduled.message;
    let requests_before = provider.requests.lock().await.len();
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        message.clone(),
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await
    .unwrap();
    let records = runtime
        .runtime_db()
        .task_result_settlements()
        .admitted_for_activation("default", &activation)
        .unwrap();
    let request_text = {
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), requests_before + 1);
        let request = requests.last().unwrap();
        format!(
            "{}\n{:?}",
            serde_json::to_string(&request.prompt_frame).unwrap(),
            request.conversation
        )
    };
    assert!(!records.is_empty());
    for record in &records {
        assert_eq!(record.state, TaskResultSettlementState::CallerAdmitted);
        assert_eq!(record.activation_id.as_deref(), Some(activation.as_str()));
        assert_eq!(record.work_item_id.as_deref(), Some(result_owner));
        let (source, body) = &sources[&record.result_identity];
        assert_eq!(&record.message_id, source);
        assert!(
            request_text.contains(body),
            "admitted body absent from provider"
        );
        // The triggering notification is current input, not a pending-result row.
        if record.message_id != message.id {
            assert!(
                request_text.contains(&record.result_identity),
                "admitted follower identity absent from provider"
            );
        }
    }
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(
        turn.effective_owner(),
        TurnOwner::WorkItem {
            work_item_id: consuming_owner.to_owned(),
        }
    );
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state.current_turn_id.as_deref(),
        Some(turn.turn_id.as_str())
    );
    // Direct notifications need not carry a turn ID at admission. The started
    // turn's execution binding still identifies the canonical activation.
    assert_eq!(
        state
            .current_execution_binding
            .as_ref()
            .and_then(|binding| binding.activation_id.as_deref()),
        Some(activation.as_str())
    );
    assert_eq!(terminal.turn_record.turn_id, turn.turn_id);
    assert!(Box::pin(runtime.commit_queue_terminal_settlement(
        QueueEntryRecord {
            message_id: message.id,
            agent_id: message.agent_id,
            priority: message.priority,
            status: QueueEntryStatus::Processed,
            created_at: message.created_at,
            updated_at: Utc::now(),
        },
        Vec::new(),
        true,
        Some(&terminal),
    ))
    .await
    .unwrap());
    for record in &records {
        let settled = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(settled.result_identity, record.result_identity);
        assert_eq!(settled.activation_id, record.activation_id);
        assert_eq!(settled.state, TaskResultSettlementState::Settled);
        assert_eq!(
            settled.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
    }
    (activation, records)
}

#[tokio::test(start_paused = true)]
async fn p1_exact_waiter_admits_only_its_identity_from_nine_same_owner_results() {
    let provider = Arc::new(BatchProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "nine results belong to this producer".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let waiter = runtime
        .create_work_item(
            "only one exact dependency belongs to this execution".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut sources = BTreeMap::new();
    let mut exact_source = String::new();
    for index in 0..9 {
        let body = format!("exact-waiter-body-{index:03}-end");
        let source = Box::pin(publish_owner_result(
            runtime,
            &owner.id,
            format!("exact-batch-{index}"),
            &body,
        ))
        .await;
        // Scope, not cross-owner queue priority, is under test: make the exact
        // notification first, with all nine identities pending before admission.
        if index == 0 {
            exact_source = source.clone();
        }
        let record = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&source)
            .unwrap()
            .unwrap();
        sources.insert(record.result_identity, (source, body));
    }
    let outcome = Box::pin(runtime.register_wait_for_outcome(
        "default",
        Some(waiter.id.clone()),
        WaitForWakeKind::TaskResult,
        Some("exact-batch-0".into()),
        "one exact result must not absorb producer siblings".into(),
        None,
    ))
    .await
    .unwrap();
    let WaitForRegistrationOutcome::TaskResultQueued {
        result_message_id,
        wait_condition_id,
        ..
    } = outcome
    else {
        panic!("a completed dependency must queue the exact result");
    };
    assert_eq!(result_message_id, exact_source);
    let waiter = runtime.latest_work_item(&waiter.id).await.unwrap().unwrap();
    seed_waiting_work_execution(runtime.storage(), &waiter, &wait_condition_id);
    let scheduled = Box::pin(claim_model_input(runtime)).await;
    assert_eq!(scheduled.message.id, exact_source);
    let (_, records) = Box::pin(deliver_batch(
        runtime, &provider, scheduled, &owner.id, &waiter.id, &sources,
    ))
    .await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].message_id, exact_source);
    let request_text = {
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), 1);
        format!(
            "{}\n{:?}",
            serde_json::to_string(&requests[0].prompt_frame).unwrap(),
            requests[0].conversation
        )
    };
    for (source, body) in sources
        .values()
        .filter(|(source, _)| *source != exact_source)
    {
        assert!(
            !request_text.contains(body),
            "producer sibling body must not reach the exact waiter"
        );
        let record = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(source)
            .unwrap()
            .unwrap();
        assert_eq!(record.work_item_id.as_deref(), Some(owner.id.as_str()));
        assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
        assert!(record.activation_id.is_none());
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(source)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
    }
    assert_eq!(provider.requests.lock().await.len(), 1);
}
