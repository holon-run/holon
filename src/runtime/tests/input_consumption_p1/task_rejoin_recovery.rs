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
    // Turn creation still uses wall time. Keep test-clock terminals after it.
    let offset = Utc::now()
        .signed_duration_since(harness.now())
        .to_std()
        .unwrap();
    harness
        .advance(offset + std::time::Duration::from_secs(60))
        .await;
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
        seed_waiting_work_execution(runtime.storage(), &owner, &wait_condition_id);
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
    runtime
        .runtime_db()
        .task_result_settlements()
        .admit_message("default", &message_id, &activation, runtime.now())
        .unwrap()
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
    harness.restart();
    harness
        .runtime()
        .recover_scheduler_bootstrap_claims()
        .await
        .unwrap();
    (harness, provider, message_id, source)
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
            vec![source]
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
    assert_eq!(
        recovered.replay.map(|edge| edge.source_turn_id),
        expected_source
    );
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
