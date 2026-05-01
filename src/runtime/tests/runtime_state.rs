use super::super::*;
use super::support::*;

#[tokio::test]
async fn non_model_visible_external_events_do_not_run_interactive_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "should not run",
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::WebhookEvent,
        MessageOrigin::Webhook {
            source: "test".into(),
            event_type: Some("ping".into()),
        },
        TrustLevel::UntrustedExternal,
        Priority::Normal,
        MessageBody::Text { text: "".into() },
    );

    runtime
        .process_message(message, closure_decision(ClosureOutcome::Completed, None))
        .await
        .unwrap();

    assert_eq!(provider.call_count().await, 0);
    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    assert!(transcript
        .iter()
        .all(|entry| entry.kind != TranscriptEntryKind::AssistantRound));
}

#[tokio::test]
async fn model_visible_operator_and_timer_events_run_interactive_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "ran interactive turn",
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "plan the next step".into(),
        },
    );
    runtime
        .process_message(operator, closure_decision(ClosureOutcome::Completed, None))
        .await
        .unwrap();

    let timer = MessageEnvelope::new(
        "default",
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: "timer-1".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Normal,
        MessageBody::Text {
            text: "timer fired".into(),
        },
    );
    runtime
        .process_message(
            timer,
            closure_decision(ClosureOutcome::Waiting, Some(WaitingReason::AwaitingTimer)),
        )
        .await
        .unwrap();

    assert_eq!(provider.call_count().await, 2);
    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    assert!(
        transcript
            .iter()
            .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
            .count()
            >= 2
    );
}

#[tokio::test]
async fn task_status_routes_only_through_task_state_reduction() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "should not run",
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::TaskStatus,
        MessageOrigin::Task {
            task_id: "task-1".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Normal,
        MessageBody::Text {
            text: "task running".into(),
        },
    );
    let mut message = message;
    message.metadata = Some(serde_json::json!({
        "task_id": "task-1",
        "task_kind": "child_agent_task",
        "task_status": "running",
        "task_summary": "task running",
        "task_detail": { "wait_policy": "blocking" },
    }));

    runtime
        .process_message(message, closure_decision(ClosureOutcome::Completed, None))
        .await
        .unwrap();

    assert_eq!(provider.call_count().await, 0);
    let tasks = runtime.latest_task_records().await.unwrap();
    assert!(tasks.iter().any(|task| task.id == "task-1"));
    let events = runtime.storage().read_recent_events(10).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "task_status_updated"));
}

#[tokio::test]
async fn task_result_routes_through_reduction_and_follow_up_behavior() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "task follow-up",
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    runtime
        .storage()
        .append_task(&TaskRecord {
            id: "task-1".into(),
            agent_id: "default".into(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some("task running".into()),
            detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
            recovery: None,
        })
        .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.active_task_ids.push("task-1".into());
        runtime.storage().write_agent(&guard.state).unwrap();
    }

    let message = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task-1".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Normal,
        MessageBody::Text {
            text: "task completed".into(),
        },
    );
    let mut message = message;
    message.metadata = Some(serde_json::json!({
        "task_id": "task-1",
        "task_kind": "child_agent_task",
        "task_status": "completed",
        "task_summary": "task completed",
        "task_detail": { "wait_policy": "blocking" },
    }));

    runtime
        .process_message(
            message,
            closure_decision(
                ClosureOutcome::Waiting,
                Some(WaitingReason::AwaitingTaskResult),
            ),
        )
        .await
        .unwrap();

    assert_eq!(provider.call_count().await, 1);
    let state = runtime.agent_state().await.unwrap();
    assert!(!state.active_task_ids.contains(&"task-1".to_string()));
    let events = runtime.storage().read_recent_events(100).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "task_result_received"));
}

#[tokio::test]
async fn task_result_persists_reduced_state_when_agent_status_is_not_mutable() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "should not run",
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.status = AgentStatus::Paused;
        guard.state.active_task_ids.push("task-1".into());
        runtime.storage().write_agent(&guard.state).unwrap();
    }

    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task-1".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Normal,
        MessageBody::Text {
            text: "task completed".into(),
        },
    );
    message.metadata = Some(serde_json::json!({
        "task_id": "task-1",
        "task_kind": "child_agent_task",
        "task_status": "completed",
        "task_summary": "task completed",
        "task_detail": { "wait_policy": "blocking" },
    }));

    runtime
        .process_message(message, closure_decision(ClosureOutcome::Completed, None))
        .await
        .unwrap();

    assert_eq!(provider.call_count().await, 0);
    let persisted = runtime
        .storage()
        .read_agent()
        .unwrap()
        .expect("agent state should be persisted");
    assert_eq!(persisted.status, AgentStatus::Paused);
    assert!(!persisted.active_task_ids.contains(&"task-1".to_string()));
}

#[tokio::test]
async fn unknown_control_action_fails_without_mutating_runtime_state() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let before = runtime.agent_state().await.unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::Control,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Next,
        MessageBody::Text {
            text: "bogus".into(),
        },
    );
    let error = runtime
        .process_message(message, closure_decision(ClosureOutcome::Completed, None))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown control action"));
    let after = runtime.agent_state().await.unwrap();
    assert_eq!(after.status, before.status);
    assert_eq!(after.current_run_id, before.current_run_id);
}

#[tokio::test]
async fn final_status_rewrite_preserves_paused_stopped_and_asleep_states() {
    for status in [
        AgentStatus::Paused,
        AgentStatus::Stopped,
        AgentStatus::Asleep,
    ] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = status.clone();
            runtime.storage().write_agent(&guard.state).unwrap();
        }

        let message = MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: Some("ping".into()),
            },
            TrustLevel::UntrustedExternal,
            Priority::Normal,
            MessageBody::Text { text: "".into() },
        );

        runtime
            .process_message(message, closure_decision(ClosureOutcome::Completed, None))
            .await
            .unwrap();
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, status);
    }
}

#[test]
fn incoming_transcript_entries_preserve_delivery_surface_and_correlation_metadata() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::WebhookEvent,
        MessageOrigin::Webhook {
            source: "github".into(),
            event_type: Some("issue_comment".into()),
        },
        TrustLevel::TrustedIntegration,
        Priority::Normal,
        MessageBody::Text {
            text: "payload".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpWebhook,
        AdmissionContext::PublicUnauthenticated,
    );
    message.correlation_id = Some("corr-1".into());
    message.causation_id = Some("cause-1".into());

    runtime.record_incoming_transcript_entry(&message).unwrap();

    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let entry = transcript.last().expect("incoming transcript entry");
    assert_eq!(
        entry.data["delivery_surface"].as_str(),
        Some("http_webhook")
    );
    assert_eq!(
        entry.data["admission_context"].as_str(),
        Some("public_unauthenticated")
    );
    assert_eq!(
        entry.data["authority_class"].as_str(),
        Some("integration_signal")
    );
    assert_eq!(entry.data["correlation_id"].as_str(), Some("corr-1"));
    assert_eq!(entry.data["causation_id"].as_str(), Some("cause-1"));
}

#[tokio::test]
async fn runtime_does_not_force_completion_after_post_verification_stagnation() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    std::fs::write(workspace.path().join("app.txt"), "before").unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StagnatingAfterVerificationProvider {
            calls: Mutex::new(0),
        }),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            TrustLevel::TrustedOperator,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: Some(3),
            },
        )
        .await
        .unwrap();

    assert!(
        !outcome.should_sleep,
        "runtime should not force terminal delivery after exploratory rounds"
    );
    assert!(
        outcome
            .final_text
            .contains("Stopped after reaching the maximum tool loop depth (3)."),
        "unexpected final_text: {}",
        outcome.final_text
    );
}

#[tokio::test]
async fn reading_discovered_skill_marks_it_active_and_promotes_on_success() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let skill_dir = workspace.path().join(".agents/skills/demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nFollow the demo workflow.",
    )
    .unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(SkillReadProvider {
            calls: Mutex::new(0),
        }),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens: 65536,
            compaction_keep_recent_estimated_tokens: 4096,
            ..context_config()
        },
    )
    .unwrap();

    runtime.begin_interactive_turn(None, None).await.unwrap();
    let prompt = runtime
        .preview_prompt(
            "use the demo skill".to_string(),
            TrustLevel::TrustedOperator,
        )
        .await
        .unwrap();
    let outcome = runtime
        .run_agent_loop(
            "default",
            TrustLevel::TrustedOperator,
            prompt,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();
    runtime.promote_turn_active_skills().await.unwrap();

    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.active_skills.len(), 1);
    let skill = &state.active_skills[0];
    assert_eq!(skill.skill_id, "workspace:demo");
    assert_eq!(
        skill.activation_source,
        SkillActivationSource::ImplicitFromCatalog
    );
    assert_eq!(skill.activation_state, SkillActivationState::SessionActive);
    assert_eq!(skill.activated_at_turn, state.turn_index);

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "skill_activated" && event.data["skill_id"] == "workspace:demo"
    }));
}

#[test]
fn sanitize_subagent_result_removes_think_and_tool_markup() {
    let input = r#"I'll inspect the workspace first.
<think>
hidden planning
</think>
**[SYSTEM] Updating plan...**
<list_files>
<path>.</path>
</list_files>
Final concise answer."#;

    let cleaned = sanitize_subagent_result(input);
    assert!(!cleaned.contains("<think>"));
    assert!(!cleaned.contains("<list_files>"));
    assert!(!cleaned.contains("[SYSTEM]"));
    assert!(cleaned.contains("I'll inspect the workspace first."));
    assert!(cleaned.contains("Final concise answer."));
}

#[test]
fn sanitize_subagent_result_removes_single_line_tool_markup_and_system_lines() {
    let input = r#"[SYSTEM] Updating plan
Let me start by checking the workspace.
<read_file path="src/runtime.rs"></read_file>
Final answer with grounded content."#;

    let cleaned = sanitize_subagent_result(input);
    assert!(!cleaned.contains("[SYSTEM]"));
    assert!(!cleaned.contains("<read_file"));
    assert!(cleaned.contains("Let me start by checking the workspace."));
    assert!(cleaned.contains("Final answer with grounded content."));
}

#[test]
fn sanitize_subagent_result_drops_unclosed_think_block() {
    let input = "I'll inspect this first.\n<think>\nhidden\nstill hidden";
    let cleaned = sanitize_subagent_result(input);
    assert_eq!(cleaned, "I'll inspect this first.");
}

#[test]
fn sanitize_subagent_result_preserves_english_result_prefixes() {
    let cleaned = sanitize_subagent_result(
        "I will update src/runtime/subagent.rs and verify with cargo test.",
    );
    assert_eq!(
        cleaned,
        "I will update src/runtime/subagent.rs and verify with cargo test."
    );
}

#[test]
fn sanitize_subagent_result_preserves_chinese_final_report() {
    let input = "结论：已经定位到问题。\n相关文件：src/runtime/subagent.rs\n验证：cargo test -q";
    let cleaned = sanitize_subagent_result(input);
    assert_eq!(cleaned, input);
}

#[test]
fn runtime_failure_summary_preserves_exact_limit_without_ellipsis() {
    let message = "x".repeat(200);
    let error = anyhow!(message.clone());

    let summary = RuntimeHandle::summarize_runtime_failure_error(&error);

    assert_eq!(summary, message);
    assert_eq!(summary.chars().count(), 200);
    assert!(!summary.ends_with('…'));
}

#[test]
fn runtime_failure_summary_keeps_prefix_for_long_single_segment() {
    let message = "x".repeat(260);
    let error = anyhow!(message);

    let summary = RuntimeHandle::summarize_runtime_failure_error(&error);

    assert_eq!(summary.chars().count(), 200);
    assert!(summary.ends_with('…'));
    assert!(summary.starts_with(&"x".repeat(16)));
    assert_ne!(summary, "…");
}

#[test]
fn runtime_failure_summary_truncates_exact_budget_before_ellipsis() {
    let message = format!("{} {}", "x".repeat(200), "tail");
    let error = anyhow!(message);

    let summary = RuntimeHandle::summarize_runtime_failure_error(&error);
    let expected = format!("{}…", "x".repeat(199));

    assert_eq!(summary.chars().count(), 200);
    assert!(summary.ends_with('…'));
    assert_eq!(summary, expected);
}

#[test]
fn wake_hint_preserved_when_replaced_during_critical_window() {
    use tokio::runtime::Runtime;

    // Enable checkpoint mechanism for this test
    crate::runtime::test_util::enable_checkpoint();

    // RAII guard to ensure checkpoint is disabled even on panic
    struct CheckpointGuard;
    impl Drop for CheckpointGuard {
        fn drop(&mut self) {
            crate::runtime::test_util::disable_checkpoint();
        }
    }
    let _guard = CheckpointGuard;

    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let rt = Runtime::new().unwrap();

    // Create agent with idle status and an initial wake hint
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::AwakeIdle;
    agent.pending_wake_hint = Some(PendingWakeHint {
        reason: "original-hint".into(),
        source: Some("test".into()),
        resource: None,
        body: None,
        content_type: None,
        correlation_id: Some("corr-original".into()),
        causation_id: None,
        created_at: Utc::now(),
    });
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    // Verify the hint is set
    rt.block_on(async {
        let state = runtime.agent_state().await.unwrap();
        assert!(state.pending_wake_hint.is_some());
        assert_eq!(
            state.pending_wake_hint.as_ref().unwrap().reason,
            "original-hint"
        );
    });

    // Spawn emit task in background - it will:
    // 1. Read "original-hint"
    // 2. Complete emit
    // 3. Block at checkpoint waiting for our signal
    let runtime_clone = runtime.clone();
    let emit_handle = std::thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            // This will block at the checkpoint after emit completes
            runtime_clone
                .maybe_emit_pending_system_tick(None)
                .await
                .unwrap()
        })
    });

    // Wait for the emit thread to reach the checkpoint
    // At this point:
    // - "original-hint" has been emitted as SystemTick
    // - The checkpoint notify is waiting
    // - The lock has NOT been reacquired yet
    rt.block_on(async {
        crate::runtime::test_util::wait_for_emit_at_checkpoint().await;
    });

    // NOW we're in the critical window: emit done, lock not held yet
    // Replace the hint while emit thread is blocked at checkpoint
    rt.block_on(async {
        // Acquire the lock and update the hint
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.pending_wake_hint = Some(PendingWakeHint {
            reason: "new-hint".into(),
            source: Some("test".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: Some("corr-new".into()),
            causation_id: None,
            created_at: Utc::now(),
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
        drop(guard);
    });

    // Release the checkpoint - let emit thread continue
    crate::runtime::test_util::release_checkpoint();

    // Wait for emit thread to finish
    emit_handle.join().unwrap();

    // Verify the NEW hint is preserved (not cleared by the old hint's comparison)
    rt.block_on(async {
        let state = runtime.agent_state().await.unwrap();
        assert!(state.pending_wake_hint.is_some());
        assert_eq!(state.pending_wake_hint.as_ref().unwrap().reason, "new-hint");
    });

    // Verify the SystemTick event was emitted
    let events = runtime.storage().read_recent_events(10).unwrap();
    assert!(events.iter().any(|e| e.kind == "system_tick_emitted"));
}
