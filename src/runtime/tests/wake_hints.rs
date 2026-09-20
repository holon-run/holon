use super::super::*;
use super::support::*;
use crate::ingress::{WakeDisposition, WakeHint};

struct SelectQueuedAutonomousContinuationHook;

impl scheduler::SemanticCandidateSelectionHook for SelectQueuedAutonomousContinuationHook {
    fn select_autonomous_continuation(
        &self,
        context: &scheduler::AutonomousContinuationSelectionContext,
    ) -> Result<
        scheduler::SemanticCandidateSelectionHookResult,
        scheduler::SemanticCandidateSelectionHookError,
    > {
        let candidate = context
            .candidates
            .iter()
            .find(|candidate| candidate.reactivation_mode == WorkReactivationMode::ActivateQueued)
            .cloned()
            .expect("queued candidate");
        Ok(scheduler::SemanticCandidateSelectionHookResult::Propose(
            scheduler::AutonomousContinuationProposal {
                snapshot_identity: context.snapshot_identity.clone(),
                candidate,
            },
        ))
    }
}

#[tokio::test]
async fn runtime_emits_pending_wake_hint_as_system_tick_on_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    agent.pending_wake_hint = Some(PendingWakeHint {
        reason: "restart wake".into(),
        description: None,
        scope: None,
        external_trigger_id: None,
        source: Some("test".into()),
        resource: None,
        body: None,
        content_type: None,
        correlation_id: Some("corr".into()),
        causation_id: None,
        created_at: Utc::now(),
    });
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_wake_hint.is_none());
    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages
        .iter()
        .any(|message| message.kind == MessageKind::SystemTick
            && message.authority_class == AuthorityClass::RuntimeInstruction));
    runtime_task.abort();
}

#[tokio::test]
async fn recovered_duplicate_wake_hint_clears_pending_without_new_tick() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let pending = PendingWakeHint {
        reason: "restart duplicate wake".into(),
        description: None,
        scope: None,
        external_trigger_id: None,
        source: Some("test".into()),
        resource: None,
        body: None,
        content_type: None,
        correlation_id: Some("dup-corr".into()),
        causation_id: None,
        created_at: Utc::now(),
    };
    let idempotency_key = scheduler::wake_hint_idempotency_key(&pending);
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::AwakeIdle;
    agent.pending_wake_hint = Some(pending);
    storage.write_agent(&agent).unwrap();

    let mut duplicate = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "wake_hint".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "wake hint: restart duplicate wake".into(),
        },
    );
    duplicate.metadata = Some(serde_json::json!({
        "wake_hint": {
            "idempotency_key": idempotency_key,
            "reason": "restart duplicate wake"
        }
    }));
    storage.append_message(&duplicate).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    runtime.emit_recovered_pending_wake_hint().await.unwrap();

    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_wake_hint.is_none());
    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| {
                matches!(
                    (&message.kind, &message.origin),
                    (MessageKind::SystemTick, MessageOrigin::System { subsystem })
                        if subsystem == "wake_hint"
                )
            })
            .count(),
        1,
        "duplicate recovered wake hint must not enqueue another system tick"
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "scheduler_decision"
            && event.data["decision"] == "Noop"
            && event.data["reason"] == "duplicate_wake_hint"
    }));
}

#[tokio::test]
async fn triggered_wake_hint_records_scheduler_decision_before_tick() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let disposition = runtime
        .submit_wake_hint(WakeHint {
            agent_id: "default".into(),
            reason: "immediate wake".into(),
            description: None,
            source: Some("test".into()),
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: Some(MessageBody::Text {
                text: "wake body".into(),
            }),
            content_type: None,
            correlation_id: Some("corr".into()),
            causation_id: None,
        })
        .await
        .unwrap();

    assert_eq!(disposition, WakeDisposition::Triggered);
    let events = runtime.storage().read_recent_events(20).unwrap();
    let decision_index = events
        .iter()
        .position(|event| {
            event.kind == "scheduler_decision"
                && event.data["decision"] == "EmitSystemTick"
                && event.data["reason"] == "wake_hint"
        })
        .expect("wake hint scheduler decision should be recorded");
    let tick_index = events
        .iter()
        .position(|event| event.kind == "system_tick_emitted")
        .expect("wake hint tick should be emitted");
    assert!(
        decision_index < tick_index,
        "scheduler decision should be recorded before tick emission"
    );
}

#[tokio::test]
async fn runtime_emits_work_queue_system_tick_for_active_item_on_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    storage.append_work_item(&active).unwrap();
    seed_runnable_work_execution(&storage, &active);
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let messages = runtime.storage().read_recent_messages(20).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("continue_active")
    }));
    runtime_task.abort();
}

#[tokio::test]
async fn idle_tick_prefers_current_work_item_over_queued_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    let queued = WorkItemRecord::new("default", "queued runtime cleanup", WorkItemState::Open);
    storage.append_work_item(&active).unwrap();
    storage.append_work_item(&queued).unwrap();
    seed_runnable_work_execution(&storage, &active);
    seed_runnable_work_execution(&storage, &queued);
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    assert!(runtime.maybe_emit_pending_system_tick(None).await.unwrap());

    let queued_latest = runtime
        .latest_work_item(&queued.id)
        .await
        .unwrap()
        .expect("queued item should still exist");
    assert_eq!(queued_latest.state, WorkItemState::Open);

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("continue_active")
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("work_item_id"))
                .and_then(serde_json::Value::as_str)
                == Some(active.id.as_str())
    }));
}

#[tokio::test]
async fn idle_tick_uses_runtime_autonomous_continuation_hook() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    let queued = WorkItemRecord::new("default", "queued runtime cleanup", WorkItemState::Open);
    storage.append_work_item(&active).unwrap();
    storage.append_work_item(&queued).unwrap();
    seed_runnable_work_execution(&storage, &active);
    seed_runnable_work_execution(&storage, &queued);
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    storage.write_agent(&agent).unwrap();

    let mut runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    runtime.set_autonomous_continuation_hook_for_test(Arc::new(
        SelectQueuedAutonomousContinuationHook,
    ));

    assert!(runtime.maybe_emit_pending_system_tick(None).await.unwrap());

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("queued_available")
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("work_item_id"))
                .and_then(serde_json::Value::as_str)
                == Some(queued.id.as_str())
    }));
}

#[tokio::test]
async fn idle_tick_suppresses_continue_active_after_model_reentry_task_result() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    storage.append_work_item(&active).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let task_result_rejoin = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::TaskResult,
        class: ContinuationClass::ResumeExpectedWait,
        model_reentry: true,
        prior_closure_outcome: ClosureOutcome::Waiting,
        prior_waiting_reason: Some(WaitingReason::AwaitingTaskResult),
        matched_waiting_reason: true,
        evidence: vec!["matches_waiting_reason".into()],
    };

    assert!(!runtime
        .maybe_emit_pending_system_tick(Some(&task_result_rejoin))
        .await
        .unwrap());

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(!messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("continue_active")
    }));
}

#[tokio::test]
async fn idle_tick_prefers_pending_wake_hint_over_work_queue_tick() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    storage.append_work_item(&active).unwrap();

    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    agent.pending_wake_hint = Some(PendingWakeHint {
        reason: "resume from callback".into(),
        description: None,
        scope: None,
        external_trigger_id: None,
        source: Some("test".into()),
        resource: None,
        body: None,
        content_type: None,
        correlation_id: Some("wake-priority".into()),
        causation_id: None,
        created_at: Utc::now(),
    });
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    assert!(runtime.maybe_emit_pending_system_tick(None).await.unwrap());
    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_wake_hint.is_none());

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && matches!(
                message.origin,
                MessageOrigin::System { ref subsystem } if subsystem == "wake_hint"
            )
    }));
    assert!(!messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .is_some()
    }));
}

#[tokio::test]
async fn runtime_surfaces_queued_work_item_with_work_queue_system_tick_on_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();
    let queued = WorkItemRecord::new(
        "default",
        "surface queued runtime cleanup",
        WorkItemState::Open,
    );
    let queued_id = queued.id.clone();
    storage.append_work_item(&queued).unwrap();
    seed_runnable_work_execution(&storage, &queued);

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let active = runtime
        .latest_work_item(&queued_id)
        .await
        .unwrap()
        .expect("queued item should still exist");
    assert_eq!(active.state, WorkItemState::Open);

    let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "system_tick_emitted"
            && event.data["work_queue"]["work_item_id"].as_str() == Some(queued_id.as_str())
    }));
    runtime_task.abort();
}

#[tokio::test]
async fn queued_work_item_update_wakes_sleeping_runtime_and_surfaces_it() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let queued = runtime
        .create_work_item(
            "wake from direct queued work item update".into(),
            None,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let active = runtime
        .latest_work_item(&queued.id)
        .await
        .unwrap()
        .expect("queued item should still exist");
    assert_eq!(active.state, WorkItemState::Open);

    let messages = runtime.storage().read_recent_messages(20).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("work_item_id"))
                .and_then(serde_json::Value::as_str)
                == Some(queued.id.as_str())
    }));
    runtime_task.abort();
}

// ── submit_wake_hint state machine coverage (issue #1858 P1) ───────
//
// These tests pin down the per-status disposition of `submit_wake_hint`
// without spinning up the runtime loop. They complement the existing
// recovery + restart tests, which only exercise the
// `Booting|AwakeIdle|Asleep + empty-queue → Triggered` path.

fn wake_hint_smoke() -> WakeHint {
    WakeHint {
        agent_id: "default".into(),
        reason: "state-machine probe".into(),
        description: None,
        source: Some("test".into()),
        scope: None,
        external_trigger_id: None,
        resource: None,
        body: Some(MessageBody::Text {
            text: "wake body".into(),
        }),
        content_type: None,
        correlation_id: Some("corr-sm".into()),
        causation_id: None,
    }
}

#[tokio::test]
async fn wake_hint_when_awake_running_is_coalesced() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::AwakeRunning;
        guard.state.current_run_id = Some("run-live".into());
        guard.current_run_abort = Some(CurrentRunAbortHandle {
            run_id: "run-live".into(),
            token: CancellationToken::new(),
            reason: Arc::new(StdMutex::new("operator_aborted".into())),
        });
        guard.persist_state(&runtime.inner.storage).unwrap();
    }

    let disposition = runtime.submit_wake_hint(wake_hint_smoke()).await.unwrap();
    assert_eq!(disposition, WakeDisposition::Coalesced);

    let state = runtime.agent_state().await.unwrap();
    let pending = state
        .pending_wake_hint
        .expect("pending_wake_hint must be set after Coalesced");
    assert_eq!(pending.reason, "state-machine probe");
    assert_eq!(pending.correlation_id.as_deref(), Some("corr-sm"));

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "wake_hint_coalesced"
            && event.data["agent_id"] == "default"
            && event.data["correlation_id"] == "corr-sm"
    }));
}

#[tokio::test]
async fn wake_hint_recovers_stale_awake_running_projection_and_triggers() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::AwakeRunning;
        guard.state.current_run_id = Some("run-stale".into());
        guard.persist_state(&runtime.inner.storage).unwrap();
    }

    let disposition = runtime.submit_wake_hint(wake_hint_smoke()).await.unwrap();
    assert_eq!(disposition, WakeDisposition::Triggered);

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.status, AgentStatus::AwakeIdle);
    assert_eq!(state.current_run_id, None);
    assert_eq!(state.pending_wake_hint, None);

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "scheduler_stale_run_projection_recovered"
            && event.data["boundary"] == "wake_hint_submission"
            && event.data["previous_run_id"] == "run-stale"
            && event.data["handle_disposition"] == "missing"
    }));
    assert!(events
        .iter()
        .any(|event| event.kind == "wake_hint_triggered"));
}

#[tokio::test]
async fn wake_hint_fails_closed_on_run_id_mismatch() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::AwakeRunning;
        guard.state.current_run_id = Some("run-projected".into());
        guard.current_run_abort = Some(CurrentRunAbortHandle {
            run_id: "run-owned".into(),
            token: CancellationToken::new(),
            reason: Arc::new(StdMutex::new("operator_aborted".into())),
        });
        guard.persist_state(&runtime.inner.storage).unwrap();
    }

    let error = runtime
        .submit_wake_hint(wake_hint_smoke())
        .await
        .expect_err("mismatched run ownership must fail closed");
    assert!(error
        .to_string()
        .contains("scheduler run projection mismatch"));

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.status, AgentStatus::AwakeRunning);
    assert_eq!(state.current_run_id.as_deref(), Some("run-projected"));
    assert!(state.pending_wake_hint.is_none());

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "scheduler_run_projection_invariant_violation"
            && event.data["boundary"] == "wake_hint_submission"
            && event.data["projected_run_id"] == "run-projected"
            && event.data["handle_run_id"] == "run-owned"
    }));
}

#[tokio::test]
async fn wake_hint_when_awaiting_task_is_coalesced() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::AwaitingTask;
        runtime.storage().write_agent(&guard.state).unwrap();
    }

    let disposition = runtime.submit_wake_hint(wake_hint_smoke()).await.unwrap();
    assert_eq!(disposition, WakeDisposition::Coalesced);

    let state = runtime.agent_state().await.unwrap();
    assert!(
        state.pending_wake_hint.is_some(),
        "AwaitingTask should still set pending_wake_hint"
    );
}

#[tokio::test]
async fn wake_hint_when_stopped_is_ignored() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::Stopped;
        runtime.storage().write_agent(&guard.state).unwrap();
    }

    let disposition = runtime.submit_wake_hint(wake_hint_smoke()).await.unwrap();
    assert_eq!(disposition, WakeDisposition::Ignored);

    let state = runtime.agent_state().await.unwrap();
    assert!(
        state.pending_wake_hint.is_none(),
        "Stopped must NOT populate pending_wake_hint"
    );

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(
        events.iter().any(|event| event.kind == "wake_hint_ignored"),
        "Stopped disposition must be recorded as wake_hint_ignored"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "wake_hint_coalesced" || event.kind == "wake_hint_triggered"),
        "Stopped must not record Triggered or Coalesced"
    );
}

#[tokio::test]
async fn wake_hint_when_awake_idle_with_nonempty_queue_is_coalesced() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::AwakeIdle;
        guard.queue.push(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "queued while idle".into(),
            },
        ));
        guard.state.pending = guard.queue.len();
        runtime.storage().write_agent(&guard.state).unwrap();
    }

    let disposition = runtime.submit_wake_hint(wake_hint_smoke()).await.unwrap();
    assert_eq!(
        disposition,
        WakeDisposition::Coalesced,
        "AwakeIdle + non-empty queue must coalesce instead of triggering immediately"
    );

    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_wake_hint.is_some());
}

#[tokio::test]
async fn wake_hint_when_asleep_with_nonempty_queue_is_coalesced() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::Asleep;
        guard.queue.push(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "queued while asleep".into(),
            },
        ));
        guard.state.pending = guard.queue.len();
        runtime.storage().write_agent(&guard.state).unwrap();
    }

    let disposition = runtime.submit_wake_hint(wake_hint_smoke()).await.unwrap();
    assert_eq!(
        disposition,
        WakeDisposition::Coalesced,
        "Asleep + non-empty queue must coalesce instead of triggering immediately"
    );

    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_wake_hint.is_some());
}
