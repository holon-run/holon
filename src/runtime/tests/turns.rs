use super::super::*;
use super::support::*;

use crate::tool::ApplyPatchSurface;
use crate::types::{ToolExecutionStatus, WaitConditionKind};

struct PickThenExecProvider {
    calls: Mutex<usize>,
    target_work_item_id: String,
}

#[tokio::test]
async fn turn_record_uses_exact_turn_evidence_beyond_recent_window() {
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
    let turn_id = "turn-exact-evidence";
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "collect exact turn evidence".into(),
        },
    );
    message.turn_id = Some(turn_id.into());
    runtime.storage().append_message(&message).unwrap();

    let mut target_brief =
        BriefRecord::new("default", BriefKind::Result, "target brief", None, None);
    target_brief.id = "brief-exact-target".into();
    target_brief.turn_id = Some(turn_id.into());
    runtime.storage().append_brief(&target_brief).unwrap();

    let target_tool = ToolExecutionRecord {
        id: "tool-exact-target".into(),
        agent_id: "default".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: Some(turn_id.into()),
        tool_name: "WaitFor".into(),
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        duration_ms: 1,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({ "wake": "external" }),
        output: serde_json::json!({ "waiting": true }),
        summary: "wait registered".into(),
        invocation_surface: None,
    };
    runtime
        .storage()
        .append_tool_execution(&target_tool)
        .unwrap();

    let now = Utc::now();
    let target_wait = WaitConditionRecord {
        id: "wait-exact-target".into(),
        agent_id: "default".into(),
        work_item_id: None,
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::External,
        source: Some("WaitFor".into()),
        subject_ref: Some("github:holon-run/holon#3055".into()),
        waiting_for: "exact evidence".into(),
        wake_sources: Vec::new(),
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: Some(turn_id.into()),
        trigger_message_id: None,
        triggered_at: None,
    };
    runtime
        .storage()
        .append_wait_condition(&target_wait)
        .unwrap();

    runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            for index in 0..=4096 {
                let noise_turn_id = format!("turn-noise-{index}");
                let mut noise = BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    format!("noise {index}"),
                    None,
                    None,
                );
                noise.id = format!("brief-noise-{index}");
                noise.turn_id = Some(noise_turn_id.clone());
                crate::runtime_db::evidence::insert_brief_evidence_tx(tx, &noise)?;

                let noise_tool = ToolExecutionRecord {
                    id: format!("tool-noise-{index}"),
                    agent_id: "default".into(),
                    work_item_id: None,
                    turn_index: index + 2,
                    turn_id: Some(noise_turn_id.clone()),
                    tool_name: "WaitFor".into(),
                    created_at: noise.created_at,
                    completed_at: Some(noise.created_at),
                    duration_ms: 1,
                    authority_class: AuthorityClass::RuntimeInstruction,
                    status: ToolExecutionStatus::Success,
                    input: serde_json::json!({ "wake": "external" }),
                    output: serde_json::json!({ "waiting": true }),
                    summary: "noise wait".into(),
                    invocation_surface: None,
                };
                crate::runtime_db::evidence::insert_tool_evidence_tx(tx, &noise_tool)?;

                let noise_wait = WaitConditionRecord {
                    id: format!("wait-noise-{index}"),
                    agent_id: "default".into(),
                    work_item_id: None,
                    status: WaitConditionStatus::Active,
                    kind: WaitConditionKind::External,
                    source: Some("WaitFor".into()),
                    subject_ref: None,
                    waiting_for: "noise".into(),
                    wake_sources: Vec::new(),
                    continuation: None,
                    created_at: noise.created_at,
                    updated_at: noise.created_at,
                    expires_at: None,
                    resolved_at: None,
                    cancelled_at: None,
                    turn_id: Some(noise_turn_id),
                    trigger_message_id: None,
                    triggered_at: None,
                };
                crate::runtime_db::repositories::upsert_wait_condition_tx(tx, &noise_wait)?;
            }
            Ok(())
        })
        .unwrap();

    let terminal = TurnTerminalRecord {
        turn_id: turn_id.into(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    };
    let record = runtime.build_turn_record(&terminal).await.unwrap();

    assert_eq!(record.input_message_ids, vec![message.id]);
    assert_eq!(record.produced_brief_ids, vec![target_brief.id]);
    assert_eq!(record.tool_execution_ids, vec![target_tool.id]);
    assert_eq!(record.waiting_condition_ids, vec![target_wait.id]);
}

#[tokio::test]
async fn wait_for_publication_scope_preserves_existing_briefs_and_rejects_extra_publication() {
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
    let prepared = runtime
        .prepare_wait_for_outcome(
            "default",
            None,
            WaitForWakeKind::External,
            Some("github:holon-run/holon#3055".into()),
            "verify Brief publication scope".into(),
            None,
        )
        .await
        .unwrap();
    let PrepareWaitForOutcome::Prepared(mut prepared) = prepared else {
        panic!("external wait should prepare a settlement");
    };
    prepared.brief_publication_scope = Some(WaitForBriefPublicationScope {
        existing_brief_ids: vec!["brief-existing".into()],
    });

    let mut silent = terminal_settlement_transition(
        TurnTerminalKind::Completed,
        vec!["brief-existing".into()],
        None,
    );
    silent.prepared_wait_for = Some(prepared.clone());
    runtime
        .validate_wait_for_terminal_publication(&silent, &prepared, &[])
        .unwrap();

    prepared.delivery = crate::tool::tools::wait_for::WaitForDeliveryArg::Final;
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "publish final report".into(),
        },
    );
    let mut brief = brief::make_result("default", &message, "final report");
    brief.turn_id = Some("turn-settlement".into());
    brief.finalizes_assistant_round_id = Some("assistant-round-final".into());
    prepared.brief = Some(brief.clone());
    let brief_created = brief_created_event_for(&brief).unwrap();
    let mut final_transition = terminal_settlement_transition(
        TurnTerminalKind::Completed,
        vec!["brief-existing".into(), brief.id.clone()],
        None,
    );
    final_transition.prepared_wait_for = Some(prepared.clone());
    runtime
        .validate_wait_for_terminal_publication(
            &final_transition,
            &prepared,
            std::slice::from_ref(&brief_created),
        )
        .unwrap();

    final_transition
        .turn_record
        .produced_brief_ids
        .push("brief-unexplained".into());
    let error = runtime
        .validate_wait_for_terminal_publication(
            &final_transition,
            &prepared,
            std::slice::from_ref(&brief_created),
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("must publish exactly its prepared Brief"));
}

struct StablePrefixDiagnosticsProvider;

fn terminal_settlement_transition(
    kind: TurnTerminalKind,
    produced_brief_ids: Vec<String>,
    no_brief_reason: Option<TurnNoBriefReason>,
) -> turn::TurnTerminalTransition {
    let terminal = TurnTerminalRecord {
        turn_id: "turn-settlement".into(),
        turn_index: 7,
        kind,
        reason: None,
        last_assistant_message: None,
        no_brief_reason,
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    };
    let mut turn_record = TurnRecord::new("default", terminal.turn_id.clone(), terminal.turn_index);
    turn_record.produced_brief_ids = produced_brief_ids;
    turn_record.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(&terminal));
    turn::TurnTerminalTransition {
        terminal,
        turn_record,
        prepared_work_item_completion: None,
        prepared_wait_for: None,
        terminal_tool_executions: Vec::new(),
    }
}

#[test]
fn terminal_settlement_accepts_exactly_one_brief_or_typed_no_brief_reason() {
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

    for transition in [
        terminal_settlement_transition(
            TurnTerminalKind::Completed,
            vec!["brief-result".into()],
            None,
        ),
        terminal_settlement_transition(
            TurnTerminalKind::Completed,
            Vec::new(),
            Some(TurnNoBriefReason::ReducerOnly {
                reason: "task_status".into(),
            }),
        ),
        terminal_settlement_transition(
            TurnTerminalKind::Completed,
            Vec::new(),
            Some(TurnNoBriefReason::ToolOnlyWait),
        ),
        terminal_settlement_transition(
            TurnTerminalKind::Aborted,
            Vec::new(),
            Some(TurnNoBriefReason::Aborted),
        ),
    ] {
        runtime
            .validate_terminal_brief_settlement(&transition)
            .unwrap();
    }
}

#[test]
fn terminal_settlement_rejects_missing_or_ambiguous_settlement_with_diagnostic() {
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

    for transition in [
        terminal_settlement_transition(TurnTerminalKind::Completed, Vec::new(), None),
        terminal_settlement_transition(
            TurnTerminalKind::Completed,
            vec!["brief-result".into()],
            Some(TurnNoBriefReason::ToolOnlyWait),
        ),
        terminal_settlement_transition(
            TurnTerminalKind::Completed,
            Vec::new(),
            Some(TurnNoBriefReason::ReducerOnly {
                reason: "  ".into(),
            }),
        ),
    ] {
        let error = runtime
            .validate_terminal_brief_settlement(&transition)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("expected exactly one of canonical Brief or valid typed NoBrief reason"));
    }

    let diagnostics = runtime
        .all_events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "turn_terminal_brief_settlement_failed")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 3);
    assert_eq!(
        diagnostics[0].data["failure"],
        "terminal_missing_brief_settlement"
    );
    assert_eq!(
        diagnostics[1].data["failure"],
        "brief_and_no_brief_reason_are_mutually_exclusive"
    );
    assert_eq!(
        diagnostics[2].data["failure"],
        "terminal_missing_brief_settlement"
    );
}

#[tokio::test]
async fn atomic_wait_rolls_back_wait_tool_turn_and_queue_on_transition_failure() {
    run_atomic_wait_settlement_test(AtomicWaitScenario::External).await;
}

#[tokio::test]
async fn atomic_wait_rebases_over_concurrent_enqueue_without_losing_pending_state() {
    run_atomic_wait_settlement_test(AtomicWaitScenario::ConcurrentEnqueue).await;
}

#[tokio::test]
async fn atomic_task_ready_wait_preparation_is_pure_and_commit_rolls_back_all_evidence() {
    run_atomic_wait_settlement_test(AtomicWaitScenario::TaskReady).await;
}

#[tokio::test]
async fn atomic_wait_reprepares_when_task_completes_before_terminal_commit() {
    run_atomic_wait_settlement_test(AtomicWaitScenario::TaskCompletes).await;
}

#[derive(Clone, Copy)]
enum AtomicWaitScenario {
    External,
    ConcurrentEnqueue,
    TaskReady,
    TaskCompletes,
}

async fn run_atomic_wait_settlement_test(scenario: AtomicWaitScenario) {
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
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "prepare an atomic silent wait".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    );
    let message = runtime.enqueue(message).await.unwrap();
    assert!(matches!(
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap(),
        scheduler_executor::RunLoopPoll::Message(_)
    ));
    runtime
        .begin_interactive_turn(Some(&message), None, None)
        .await
        .unwrap();

    let work_item = if matches!(scenario, AtomicWaitScenario::ConcurrentEnqueue) {
        let work_item = runtime
            .create_work_item("wait owner".into(), None, None, Vec::new())
            .await
            .unwrap();
        runtime.pick_work_item(work_item.id.clone()).await.unwrap();
        Some(work_item)
    } else {
        None
    };
    let task_result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task-atomic-wait".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "atomic task result".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    let task_ready = matches!(scenario, AtomicWaitScenario::TaskReady);
    let task_wait = matches!(
        scenario,
        AtomicWaitScenario::TaskReady | AtomicWaitScenario::TaskCompletes
    );
    if task_wait {
        mark_blocking_task(&runtime, "task-atomic-wait").await;
    }
    if task_ready {
        let mut task = runtime
            .task_record("task-atomic-wait")
            .await
            .unwrap()
            .unwrap();
        task.status = TaskStatus::Completed;
        task.parent_message_id = Some(task_result.id.clone());
        task.updated_at = Utc::now();
        runtime.storage().append_task(&task).unwrap();
        runtime.storage().append_message(&task_result).unwrap();
    }
    let state_before_preparation = runtime.agent_state().await.unwrap();
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (mut result, mut tool_execution) = registry
        .execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &crate::tool::ToolCall {
                id: "atomic-wait".into(),
                name: "WaitFor".into(),
                input: serde_json::json!({
                    "wake": if task_wait { "task_result" } else { "external" },
                    "delivery": "silent",
                    "resource": if task_wait { "task-atomic-wait" } else { "github:holon-run/holon#atomic-wait" },
                    "reason": "verify atomic rollback"
                }),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        runtime.agent_state().await.unwrap(),
        state_before_preparation
    );
    assert!(runtime
        .storage()
        .latest_wait_conditions_for_agent("default")
        .unwrap()
        .is_empty());
    assert!(runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest(&task_result.id)
        .unwrap()
        .is_none());
    let state = runtime.agent_state().await.unwrap();
    tool_execution.turn_index = state.turn_index;
    tool_execution.turn_id = state.current_turn_id.clone();
    let mut prepared = result
        .prepared_wait_for
        .take()
        .expect("silent WaitFor should prepare canonical settlement");
    prepared.brief_publication_scope = Some(WaitForBriefPublicationScope {
        existing_brief_ids: Vec::new(),
    });
    prepared.tool_execution = Some(tool_execution.clone());
    // Roll back final report/outbox evidence together with task-result admission.
    if task_ready {
        prepared.delivery = crate::tool::tools::wait_for::WaitForDeliveryArg::Final;
        let mut brief = brief::make_result("default", &message, "Waiting for task delivery.");
        brief.turn_id = state.current_turn_id.clone();
        brief.finalizes_assistant_round_id = Some("assistant-round-atomic-wait".into());
        prepared.brief = Some(brief);
    }
    let terminal = TurnTerminalRecord {
        turn_id: state.current_turn_id.clone().unwrap(),
        turn_index: state.turn_index,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: (!task_ready).then_some(TurnNoBriefReason::ToolOnlyWait),
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    };
    let mut turn_record = runtime.build_turn_record(&terminal).await.unwrap();
    if let Some(brief) = prepared.brief.as_ref() {
        turn_record.produced_brief_ids.push(brief.id.clone());
    }
    turn_record
        .waiting_condition_ids
        .push(prepared.registration.condition.id.clone());
    turn_record
        .tool_execution_ids
        .push(tool_execution.id.clone());
    turn_record.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(&terminal));
    let transition = turn::TurnTerminalTransition {
        terminal,
        turn_record,
        prepared_work_item_completion: None,
        prepared_wait_for: Some(prepared),
        terminal_tool_executions: Vec::new(),
    };

    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites,
    );
    let error = runtime
        .commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Processed,
                created_at: message.created_at,
                updated_at: Utc::now(),
            },
            Vec::new(),
            true,
            Some(&transition),
        )
        .await
        .expect_err("injected transition fault should fail atomic WaitFor settlement");
    assert_injected_transition_fault(&error);

    assert!(runtime
        .storage()
        .active_wait_conditions_for_agent("default")
        .unwrap()
        .is_empty());
    assert!(runtime
        .storage()
        .read_recent_tool_executions(10)
        .unwrap()
        .iter()
        .all(|record| record.id != tool_execution.id));
    let persisted_turn = runtime
        .storage()
        .read_recent_turns(10)
        .unwrap()
        .iter()
        .find(|record| record.turn_id == transition.terminal.turn_id)
        .cloned()
        .expect("turn start record should remain after rollback");
    assert!(persisted_turn.terminal.is_none());
    assert!(persisted_turn.waiting_condition_ids.is_empty());
    assert!(!persisted_turn
        .tool_execution_ids
        .contains(&tool_execution.id));
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&message.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Dequeued
    );
    assert!(runtime.storage().read_recent_briefs(10).unwrap().is_empty());
    assert!(runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest(&task_result.id)
        .unwrap()
        .is_none());

    let concurrent_message = if matches!(scenario, AtomicWaitScenario::ConcurrentEnqueue) {
        Some(
            runtime
                .enqueue(
                    MessageEnvelope::new(
                        "default",
                        MessageKind::OperatorPrompt,
                        MessageOrigin::Operator {
                            actor_id: Some("control".into()),
                            actor_display_name: None,
                        },
                        AuthorityClass::OperatorInstruction,
                        Priority::Normal,
                        MessageBody::Text {
                            text: "concurrent operator input".into(),
                        },
                    )
                    .with_admission(
                        MessageDeliverySurface::HttpControlPrompt,
                        AdmissionContext::ControlAuthenticated,
                    ),
                )
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let state_before_commit = runtime.agent_state().await.unwrap();
    if matches!(scenario, AtomicWaitScenario::TaskCompletes) {
        let mut task = runtime
            .task_record("task-atomic-wait")
            .await
            .unwrap()
            .unwrap();
        task.status = TaskStatus::Completed;
        task.parent_message_id = Some(task_result.id.clone());
        task.updated_at = Utc::now();
        runtime.storage().append_task(&task).unwrap();
        runtime.storage().append_message(&task_result).unwrap();
    }
    assert!(runtime
        .commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Processed,
                created_at: message.created_at,
                updated_at: Utc::now(),
            },
            Vec::new(),
            true,
            Some(&transition),
        )
        .await
        .unwrap());
    let committed = runtime.agent_state().await.unwrap();
    if let Some(concurrent_message) = concurrent_message {
        assert_eq!(committed.pending, state_before_commit.pending);
        assert_eq!(
            committed.total_message_count,
            state_before_commit.total_message_count
        );
        assert_eq!(
            committed.last_wake_reason,
            state_before_commit.last_wake_reason
        );
        assert!(runtime
            .inner
            .agent
            .lock()
            .await
            .queue
            .peek_next_matching(|entry| entry.id == concurrent_message.id)
            .is_some());
        assert!(runtime
            .inner
            .runtime_db
            .work_items()
            .latest(&work_item.unwrap().id)
            .unwrap()
            .unwrap()
            .blocked_by
            .is_some());
        assert!(committed.current_turn_work_item_id.is_none());
    }
    if task_wait {
        let wait_id = &transition.turn_record.waiting_condition_ids[0];
        let wait = runtime
            .storage()
            .latest_wait_conditions_for_agent("default")
            .unwrap()
            .into_iter()
            .find(|wait| &wait.id == wait_id)
            .unwrap();
        assert_eq!(wait.status, crate::types::WaitConditionStatus::Triggered);
        assert_eq!(
            wait.trigger_message_id.as_deref(),
            Some(task_result.id.as_str())
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&task_result.id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        assert!(runtime
            .inner
            .agent
            .lock()
            .await
            .queue
            .peek_next_matching(|entry| entry.id == task_result.id)
            .is_some());
    }
    assert_eq!(
        runtime.storage().read_recent_briefs(10).unwrap().len(),
        usize::from(task_ready)
    );
}

#[async_trait]
impl AgentProvider for StablePrefixDiagnosticsProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "done".into(),
            }],
            stop_reason: None,
            input_tokens: 42,
            output_tokens: 7,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: Some(
                serde_json::from_value(serde_json::json!({
                    "request_lowering_mode": "full_replay",
                    "stable_prefix": {
                        "schema_version": 1,
                        "algorithm": "sha256",
                        "full_request_fingerprint": "full-fingerprint",
                        "stable_prefix_fingerprint": "stable-fingerprint",
                        "history_prefix_items": 2,
                        "dynamic_tail_items": 1,
                        "components": [{
                            "name": "tools",
                            "fingerprint": "tools-fingerprint",
                            "item_count": 3
                        }]
                    }
                }))
                .unwrap(),
            ),
        })
    }
}

#[tokio::test]
async fn terminal_pick_ends_turn_before_later_tools_or_provider_rounds() {
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
    let caller = runtime
        .create_work_item("activation caller".into(), None, None, Vec::new())
        .await
        .unwrap();
    let target = runtime
        .create_work_item("next activation target".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(caller.id.clone()).await.unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.current_turn_id = Some("turn-terminal-pick".into());
        guard.state.current_turn_work_item_id = Some(caller.id.clone());
        guard.state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some("activation-terminal-pick".into()),
            admission_provenance: None,
            source_message_id: "message-terminal-pick".into(),
            turn_id: "turn-terminal-pick".into(),
            owner: None,
            work_item_id: Some(caller.id.clone()),
            claimed_work_revision: Some(caller.revision),
        });
        guard.persist_state(&runtime.inner.storage).unwrap();
    }
    let provider = Arc::new(PickThenExecProvider {
        calls: Mutex::new(0),
        target_work_item_id: target.id.clone(),
    });
    *runtime.inner.provider.write().await = provider.clone();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(*provider.calls.lock().await, 1);
    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
    assert!(outcome.should_sleep);
    let tool_executions = runtime.storage().read_recent_tool_executions(10).unwrap();
    assert!(tool_executions
        .iter()
        .any(|record| record.tool_name == "PickWorkItem"));
    assert!(!tool_executions
        .iter()
        .any(|record| record.tool_name == "ExecCommand"));
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state.current_work_item_id.as_deref(),
        Some(target.id.as_str())
    );
    assert_eq!(
        state.current_turn_work_item_id.as_deref(),
        Some(caller.id.as_str())
    );
}

#[async_trait]
impl AgentProvider for PickThenExecProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls > 1 {
            panic!("terminal PickWorkItem should end the turn");
        }
        Ok(ProviderTurnResponse {
            blocks: vec![
                ModelBlock::ToolUse {
                    id: "pick-target".into(),
                    name: "PickWorkItem".into(),
                    input: serde_json::json!({
                        "work_item_id": self.target_work_item_id,
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                },
                ModelBlock::ToolUse {
                    id: "must-not-run".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf should-not-run",
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                },
            ],
            stop_reason: Some("tool_use".into()),
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn runtime_recovers_from_max_token_truncation() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(TruncatingProvider {
            calls: Mutex::new(0),
        }),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert!(outcome.final_text.contains("Partial report heading:"));
    assert!(outcome.final_text.contains("final grounded recommendation"));
    assert_eq!(
        outcome.final_citations,
        vec![
            crate::types::Citation {
                url: "https://example.com/first".into(),
                title: Some("First".into()),
            },
            crate::types::Citation {
                url: "https://example.com/second".into(),
                title: Some("Second".into()),
            },
        ]
    );
}

#[tokio::test]
async fn runtime_aborts_with_failure_brief_when_empty_output_recovery_is_exhausted() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(EmptyTruncatingProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Aborted);
    assert_eq!(outcome.terminal.kind, TurnTerminalKind::Aborted);
    assert!(outcome.final_text.contains("output recovery was exhausted"));
    assert!(outcome.terminal.no_brief_reason.is_none());

    let briefs = runtime.storage().read_recent_briefs(10).unwrap();
    let failure_brief = briefs
        .iter()
        .find(|brief| brief.turn_id.as_deref() == Some(outcome.terminal.turn_id.as_str()))
        .expect("empty recovery must produce a failure brief");
    assert_eq!(failure_brief.kind, BriefKind::Failure);
    assert_eq!(failure_brief.text, outcome.final_text);

    let events = runtime.storage().read_recent_events(200).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "max_output_tokens_recovery_exhausted"));
    assert!(!events.iter().any(|event| {
        event.kind == "terminal_missing_brief_settlement"
            || event.kind == "turn_terminal_brief_settlement_failed"
    }));
}

#[tokio::test]
async fn runtime_records_text_only_round_observations() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new(
            "I am still thinking through the runtime split before editing files.",
        )),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert!(outcome.final_text.contains("runtime split"));
    assert!(outcome.should_sleep);

    let events = runtime.storage().read_recent_events(10).unwrap();
    let provider_event = events
        .iter()
        .find(|event| event.kind == "provider_round_completed")
        .expect("missing provider_round_completed");
    assert_eq!(provider_event.data["round"], 1);
    assert_eq!(provider_event.data["tool_call_count"], 0);
    assert_eq!(provider_event.data["text_block_count"], 1);
    assert!(provider_event.data.get("text_preview").is_none());

    let assistant_event = events
        .iter()
        .find(|event| event.kind == "assistant_round_recorded")
        .expect("missing assistant_round_recorded");
    assert_eq!(assistant_event.data["round"], 1);
    assert_eq!(assistant_event.data["tool_call_count"], 0);
    assert_eq!(assistant_event.data["text_block_count"], 1);
    assert!(assistant_event.data.get("text").is_none());
    assert!(assistant_event.data.get("text_blocks").is_none());
    assert!(assistant_event.data.get("text_preview").is_none());
    assert!(
        assistant_event.data["text_char_count"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert!(assistant_event.data["has_text"].as_bool().unwrap_or(false));

    let text_only_event = events
        .iter()
        .find(|event| event.kind == "text_only_round_observed")
        .expect("missing text_only_round_observed");
    assert_eq!(text_only_event.data["has_text"], true);
    assert_eq!(text_only_event.data["triggered_recovery"], false);
    assert!(text_only_event.data["text_preview"]
        .as_str()
        .unwrap()
        .contains("runtime split"));
}

#[tokio::test]
async fn ordinary_final_response_persists_canonical_brief_settlement() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("Final operator-visible response.")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "finish the turn".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let turn = runtime
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .pop()
        .expect("terminal turn");
    assert_eq!(turn.produced_brief_ids.len(), 1);
    assert_eq!(
        turn.terminal
            .as_ref()
            .and_then(|terminal| terminal.no_brief_reason.as_ref()),
        None
    );
    let brief = runtime
        .storage()
        .read_brief_by_id(&turn.produced_brief_ids[0])
        .unwrap()
        .expect("canonical result brief");
    assert_eq!(brief.text, "Final operator-visible response.");
}

#[tokio::test]
async fn first_provider_round_records_prompt_cache_identity_fields() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
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
    let mut prompt = test_effective_prompt();
    prompt.cache_identity.compression_epoch = 3;
    prompt.cache_identity.prompt_cache_key = "default:ce3".into();
    prompt.cache_identity.context_fingerprint = "fingerprint-ce3".into();

    runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            prompt,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let events = runtime.storage().read_recent_events(10).unwrap();
    let provider_event = events
        .iter()
        .find(|event| event.kind == "provider_round_completed")
        .expect("missing provider_round_completed");
    assert_eq!(
        provider_event.data["prompt_cache_key"].as_str(),
        Some("default:ce3")
    );
    assert_eq!(provider_event.data["compression_epoch"].as_u64(), Some(3));
    assert_eq!(
        provider_event.data["context_fingerprint"].as_str(),
        Some("fingerprint-ce3")
    );

    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let assistant_round = transcript
        .iter()
        .find(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
        .expect("missing assistant round transcript");
    assert_eq!(
        assistant_round.data["prompt_cache_key"].as_str(),
        Some("default:ce3")
    );
    assert_eq!(assistant_round.data["compression_epoch"].as_u64(), Some(3));
    assert_eq!(
        assistant_round.data["context_fingerprint"].as_str(),
        Some("fingerprint-ce3")
    );
}

#[tokio::test]
async fn provider_round_records_secret_safe_stable_prefix_diagnostics() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StablePrefixDiagnosticsProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let events = runtime.storage().read_recent_events(10).unwrap();
    let provider_event = events
        .iter()
        .find(|event| event.kind == "provider_round_completed")
        .expect("missing provider_round_completed");
    let stable_prefix = &provider_event.data["provider_request_diagnostics"]["stable_prefix"];
    assert_eq!(
        stable_prefix["stable_prefix_fingerprint"].as_str(),
        Some("stable-fingerprint")
    );
    assert_eq!(stable_prefix["dynamic_tail_items"].as_u64(), Some(1));
    assert_eq!(
        stable_prefix["components"][0]["name"].as_str(),
        Some("tools")
    );
    let serialized = serde_json::to_string(&provider_event.data).unwrap();
    assert!(!serialized.contains("system_prompt"));
    assert!(!serialized.contains("conversation"));
    assert!(!serialized.contains("tool_arguments"));
}

#[tokio::test]
async fn sleep_only_tool_round_completes_without_extra_provider_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(SleepOnlyToolProvider {
        calls: Mutex::new(0),
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

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(*provider.calls.lock().await, 1);
    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
    assert!(outcome.final_text.is_empty());
    assert!(outcome.should_sleep);
    assert_eq!(outcome.sleep_duration_ms, Some(250));

    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    assert_eq!(
        transcript
            .iter()
            .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
            .count(),
        1
    );
    assert!(transcript
        .iter()
        .any(|entry| entry.kind == TranscriptEntryKind::ToolResults));
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state
            .last_turn_terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(TurnTerminalKind::Completed)
    );
    let turn = runtime
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .pop()
        .expect("sleep-only terminal turn");
    assert!(turn.produced_brief_ids.is_empty());
    assert_eq!(
        turn.terminal
            .as_ref()
            .and_then(|terminal| terminal.no_brief_reason.as_ref()),
        Some(&TurnNoBriefReason::ToolOnlyWait)
    );
}

#[tokio::test]
async fn wait_for_only_tool_round_completes_without_extra_provider_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(WaitForOnlyToolProvider {
        calls: Mutex::new(0),
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
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "wait for PR checks".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    );
    let mut runtime_task = tokio::spawn(runtime.clone().run());
    runtime.enqueue(message).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if !runtime
                .storage()
                .active_wait_conditions_for_agent("default")
                .unwrap()
                .is_empty()
            {
                break;
            }
            if runtime_task.is_finished() {
                panic!(
                    "runtime exited before WaitFor settlement: {:#}",
                    (&mut runtime_task)
                        .await
                        .expect("runtime task join")
                        .expect_err("runtime unexpectedly completed")
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for canonical WaitFor settlement");
    runtime_task.abort();

    assert_eq!(*provider.calls.lock().await, 1);

    let waiting = runtime
        .storage()
        .active_wait_conditions_for_agent("default")
        .unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].waiting_for, "waiting for PR checks");
    assert_eq!(
        waiting[0].subject_ref.as_deref(),
        Some("github:holon-run/holon#1939")
    );
    let turn = runtime
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .pop()
        .expect("wait-only terminal turn");
    assert!(turn.produced_brief_ids.is_empty());
    assert_eq!(
        turn.terminal
            .as_ref()
            .and_then(|terminal| terminal.no_brief_reason.as_ref()),
        Some(&TurnNoBriefReason::ToolOnlyWait)
    );
}

async fn run_wait_for_final_report_test(
    scenario: WaitForFinalReportScenario,
    silent_progress: Option<bool>,
    work_item_owned: bool,
) {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(WaitForFinalReportProvider {
        calls: Mutex::new(0),
        scenario,
        silent_progress,
        saw_settlement_error_follow_up: Mutex::new(false),
        invalid_final_result_count: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens: 32768,
            turn_projection_budget_ratio: 1.0,
            compaction_keep_recent_estimated_tokens: 2048,
            ..context_config()
        },
    )
    .unwrap();
    runtime
        .inner
        .agent
        .lock()
        .await
        .state
        .active_workspace_entry = None;
    let work_item = if work_item_owned {
        Some(
            runtime
                .create_work_item("owned wait".into(), None, None, Vec::new())
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let expected_work_item_id = work_item.as_ref().map(|record| record.id.clone());
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            // A new reference makes generic message bookkeeping update the WorkItem.
            text: "wait for https://github.com/holon-run/holon/pull/3016".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    );
    message.work_item_id = work_item.as_ref().map(|record| record.id.clone());
    assert!(!crate::work_item_refs::message_work_refs(&message).is_empty());
    let mut runtime_task = tokio::spawn(runtime.clone().run());
    runtime.enqueue(message.clone()).await.unwrap();
    let settled = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let wait_registered = !runtime
                .storage()
                .active_wait_conditions_for_agent("default")
                .unwrap()
                .is_empty();
            let settlement_error_recovered = scenario
                == WaitForFinalReportScenario::SettlementErrorRecovery
                && provider.call_count().await >= 3;
            if wait_registered || settlement_error_recovered {
                break;
            }
            if runtime_task.is_finished() {
                panic!(
                    "runtime exited before final WaitFor settlement: {:#}",
                    (&mut runtime_task)
                        .await
                        .expect("runtime task join")
                        .expect_err("runtime unexpectedly completed")
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    if settled.is_err() {
        panic!(
            "timed out waiting for final WaitFor settlement; calls={}; state={:#?}; events={:#?}",
            provider.call_count().await,
            runtime.agent_state().await.unwrap(),
            runtime.storage().read_recent_events(50).unwrap()
        );
    }
    runtime_task.abort();

    if scenario == WaitForFinalReportScenario::SettlementErrorRecovery {
        assert_eq!(provider.call_count().await, 3);
        assert!(*provider.saw_settlement_error_follow_up.lock().await);
        assert_eq!(
            *provider.invalid_final_result_count.lock().await,
            0,
            "final settlement errors must not inject a cross-round tool result"
        );
        let events = runtime.storage().read_recent_events(100).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "wait_report_error_follow_up_injected"));
        let transcript = runtime.storage().read_recent_transcript(50).unwrap();
        assert!(transcript.iter().any(|entry| {
            entry.kind == TranscriptEntryKind::ContinuationPrompt
                && entry.data["reason"] == "wait_report_error_follow_up"
        }));
        return;
    }

    if let Some(original) = work_item {
        let updated = runtime
            .storage()
            .latest_work_item(&original.id)
            .unwrap()
            .unwrap();
        let waiting = runtime
            .storage()
            .active_wait_conditions_for_agent("default")
            .unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(
            waiting[0].work_item_id.as_deref(),
            Some(original.id.as_str())
        );
        assert_eq!(updated.revision, original.revision + 1);
        assert!(updated.blocked_by.is_some());
        let events = runtime.storage().read_recent_events(100).unwrap();
        assert!(
            events
                .iter()
                .all(|event| event.kind != "work_item_refs_updated"),
            "prepared wait must not run generic WorkItem writes before its atomic commit"
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&message.id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Processed
        );
    }

    assert_eq!(
        provider.call_count().await,
        match silent_progress {
            Some(false) => 1,
            Some(true) => 2,
            None if scenario == WaitForFinalReportScenario::DisallowedToolCorrective => 3,
            None if scenario == WaitForFinalReportScenario::AllowedToolBudget => 4,
            None if scenario == WaitForFinalReportScenario::SettlementErrorRecovery => 3,
            None => 2,
        }
    );
    let briefs = runtime.storage().read_recent_briefs(10).unwrap();
    if silent_progress.is_some() {
        assert!(
            briefs.is_empty(),
            "silent waits must not promote progress to a brief"
        );
        let turn = runtime
            .storage()
            .read_recent_turns(1)
            .unwrap()
            .pop()
            .unwrap();
        assert!(turn.produced_brief_ids.is_empty());
        assert_eq!(
            turn.terminal.unwrap().no_brief_reason,
            Some(TurnNoBriefReason::ToolOnlyWait)
        );
        return;
    }
    let brief = briefs
        .iter()
        .find(|brief| {
            brief.kind == BriefKind::Result
                && brief.text == "Waiting for final verification; I will resume when it changes."
        })
        .expect("final WaitFor should atomically publish its result brief");
    let turn = runtime
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .pop()
        .expect("final WaitFor terminal turn");
    assert_eq!(brief.turn_id.as_deref(), Some(turn.turn_id.as_str()));
    assert_eq!(brief.workspace_id, "agent_home:default");
    assert_eq!(brief.work_item_id, expected_work_item_id);
    assert!(brief.finalizes_assistant_round_id.is_some());
    let brief_created_events = runtime
        .storage()
        .read_recent_events(100)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.kind == "brief_created"
                && event
                    .data
                    .get("brief_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(brief.id.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(brief_created_events.len(), 1);
    assert_eq!(
        brief.created_event_seq,
        Some(brief_created_events[0].event_seq)
    );
    let conversation = runtime
        .inner
        .runtime_db
        .conversation()
        .summary_page("default", 10, None, None)
        .unwrap();
    let conversation_turn = conversation
        .turns
        .iter()
        .find(|candidate| candidate.turn_id == turn.turn_id)
        .expect("conversation should expose final WaitFor turn");
    assert!(conversation_turn.brief_ids.contains(&brief.id));
    assert_eq!(
        conversation_turn.result,
        crate::domain::conversation::ResultState::Available
    );
    let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
    let wait_tool = tools
        .iter()
        .find(|tool| tool.tool_name == "WaitFor")
        .expect("final WaitFor should atomically persist successful tool evidence");
    assert_eq!(wait_tool.status, crate::types::ToolExecutionStatus::Success);
    match scenario {
        WaitForFinalReportScenario::DisallowedToolCorrective => assert!(
            tools.iter().all(|tool| tool.tool_name != "ExecCommand"),
            "the corrective report round must not execute disallowed tools"
        ),
        WaitForFinalReportScenario::AllowedToolBudget => assert_eq!(
            tools
                .iter()
                .filter(|tool| tool.tool_name == "GetAgent")
                .count(),
            2,
            "allowed report tools should execute for both budgeted rounds"
        ),
        WaitForFinalReportScenario::Direct => assert!(
            tools.iter().all(|tool| tool.tool_name != "GetAgent"),
            "the direct report path should not execute extra tools"
        ),
        WaitForFinalReportScenario::SettlementErrorRecovery => {
            unreachable!("settlement error recovery returns before successful wait assertions")
        }
    }
    assert!(turn.produced_brief_ids.contains(&brief.id));
    assert!(turn.tool_execution_ids.contains(&wait_tool.id));
    assert_eq!(turn.waiting_condition_ids.len(), 1);
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&message.id)
            .unwrap()
            .expect("queue entry")
            .status,
        QueueEntryStatus::Processed
    );
}

#[tokio::test]
async fn wait_for_final_report_commits_brief_wait_tool_and_turn_atomically() {
    run_wait_for_final_report_test(WaitForFinalReportScenario::Direct, None, false).await;
}

#[tokio::test]
async fn wait_for_final_report_corrects_extra_tool_once_without_executing_it() {
    run_wait_for_final_report_test(
        WaitForFinalReportScenario::DisallowedToolCorrective,
        None,
        false,
    )
    .await;
}

#[tokio::test]
async fn wait_for_final_report_executes_allowed_tools_then_uses_text_only_fallback() {
    run_wait_for_final_report_test(WaitForFinalReportScenario::AllowedToolBudget, None, false)
        .await;
}

#[tokio::test]
async fn wait_for_final_settlement_error_uses_text_follow_up_without_duplicate_tool_result() {
    run_wait_for_final_report_test(
        WaitForFinalReportScenario::SettlementErrorRecovery,
        None,
        false,
    )
    .await;
}

#[tokio::test]
async fn wait_for_silent_does_not_publish_same_round_text() {
    run_wait_for_final_report_test(WaitForFinalReportScenario::Direct, Some(false), false).await;
}

#[tokio::test]
async fn wait_for_silent_does_not_publish_prior_round_text() {
    run_wait_for_final_report_test(WaitForFinalReportScenario::Direct, Some(true), false).await;
}

#[tokio::test]
async fn work_item_wait_for_final_skips_precommit_message_bookkeeping() {
    run_wait_for_final_report_test(WaitForFinalReportScenario::Direct, None, true).await;
}

#[tokio::test]
async fn work_item_wait_for_silent_skips_precommit_message_bookkeeping() {
    run_wait_for_final_report_test(WaitForFinalReportScenario::Direct, Some(true), true).await;
}

struct PickThenSilentOperatorWaitProvider {
    calls: Mutex<usize>,
    target_work_item_id: Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait]
impl AgentProvider for PickThenSilentOperatorWaitProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        assert!(
            *calls <= 2,
            "silent operator wait should settle the turn quickly"
        );
        if *calls == 1 {
            let work_item_id = self
                .target_work_item_id
                .lock()
                .expect("target work item lock")
                .clone()
                .expect("target work item id must be set before the turn");
            return Ok(ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Focusing the WorkItem before waiting.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "pick-before-silent-wait".into(),
                        name: "PickWorkItem".into(),
                        input: serde_json::json!({
                            "work_item_id": work_item_id,
                            "reason": "focus the item before waiting for operator input",
                        }),
                        kind: crate::provider::ModelToolCallKind::Function,
                        provider_data: None,
                    },
                    ModelBlock::ToolUse {
                        id: "silent-operator-wait".into(),
                        name: "WaitFor".into(),
                        input: serde_json::json!({
                            "reason": "await the operator decision on the focused item",
                            "wake": "operator_input",
                            "delivery": "silent",
                        }),
                        kind: crate::provider::ModelToolCallKind::Function,
                        provider_data: None,
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 10,
                output_tokens: 10,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            });
        }
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "Waiting for the operator decision.".into(),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn work_item_silent_operator_wait_releases_current_focus_at_settlement() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(PickThenSilentOperatorWaitProvider {
        calls: Mutex::new(0),
        target_work_item_id: Arc::new(std::sync::Mutex::new(None)),
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
    let work = runtime
        .create_work_item("silent operator wait focus".into(), None, None, Vec::new())
        .await
        .unwrap();
    *provider
        .target_work_item_id
        .lock()
        .expect("target work item lock") = Some(work.id.clone());
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "focus the item and wait silently for operator input".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    );
    let runtime_task = tokio::spawn(runtime.clone().run());
    runtime.enqueue(message.clone()).await.unwrap();
    let settled = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if !runtime
                .storage()
                .active_wait_conditions_for_agent("default")
                .unwrap()
                .is_empty()
            {
                break;
            }
            if runtime_task.is_finished() {
                panic!("runtime exited before silent operator wait settlement");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "timed out waiting for silent wait settlement"
    );
    runtime_task.abort();

    let state = runtime.agent_state().await.unwrap();
    assert!(
        state.current_work_item_id.is_none(),
        "served agent state must release current focus after the silent operator wait; got {:?}",
        state.current_work_item_id
    );
    assert!(
        state.current_turn_work_item_id.is_none(),
        "served agent state must release the turn work item binding after the wait"
    );
    let latest = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .expect("durable agent state row");
    assert!(
        latest.current_work_item_id.is_none(),
        "durable agent state must record the focus release; got {:?}",
        latest.current_work_item_id
    );
    let projection = runtime.storage().work_queue_prompt_projection().unwrap();
    assert!(
        projection.current.is_none(),
        "prompt projection must not keep the waiting item current"
    );
    let waiting = runtime
        .storage()
        .active_wait_conditions_for_agent("default")
        .unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].work_item_id.as_deref(), Some(work.id.as_str()));
    let events = runtime.storage().read_recent_events(50).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "work_item_focus_released"
            && event.data["work_item_id"].as_str() == Some(work.id.as_str())
    }));
}

#[tokio::test]
async fn wait_for_final_report_abandonment_leaves_no_partial_wait_or_result_brief() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(AbandonWaitForFinalReportProvider {
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens: 32768,
            turn_projection_budget_ratio: 1.0,
            compaction_keep_recent_estimated_tokens: 2048,
            ..context_config()
        },
    )
    .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "attempt a final wait report".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    );
    let mut runtime_task = tokio::spawn(runtime.clone().run());
    runtime.enqueue(message).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let state = runtime.agent_state().await.unwrap();
            if *provider.calls.lock().await >= 3 && state.current_run_id.is_none() {
                break;
            }
            if runtime_task.is_finished() {
                panic!(
                    "runtime exited before abandoned WaitFor recovery: {:#}",
                    (&mut runtime_task)
                        .await
                        .expect("runtime task join")
                        .expect_err("runtime unexpectedly completed")
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for abandoned WaitFor recovery");
    runtime_task.abort();

    assert!(runtime
        .storage()
        .active_wait_conditions_for_agent("default")
        .unwrap()
        .is_empty());
    let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_name, "WaitFor");
    assert_eq!(
        tools[0].status,
        crate::types::ToolExecutionStatus::Interrupted
    );
    let briefs = runtime.storage().read_recent_briefs(10).unwrap();
    assert!(briefs.iter().any(|brief| brief.kind == BriefKind::Failure));
    assert!(briefs.iter().all(|brief| brief.kind != BriefKind::Result));
}

#[tokio::test]
async fn disallowed_tool_call_is_auditable_and_continuation_stays_valid() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(DisallowedToolThenTextProvider {
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        continuation_ready_context_config(&workspace, 1_000),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(outcome.final_text, "Recovered after unavailable tool.");
    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
    assert_eq!(*provider.calls.lock().await, 2);
    assert_eq!(
        runtime
            .storage()
            .read_recent_tool_executions(10)
            .unwrap()
            .len(),
        1
    );

    let events = runtime.storage().read_recent_events(20).unwrap();
    let failure_event = events
        .iter()
        .find(|event| event.kind == "tool_execution_failed")
        .expect("missing tool_execution_failed event");
    assert_eq!(failure_event.data["tool_name"].as_str(), Some("CreateTask"));
    assert_eq!(
        failure_event.data["reason"].as_str(),
        Some("tool_not_exposed_for_round")
    );
    assert_eq!(
        failure_event.data["error_kind"].as_str(),
        Some("tool_not_exposed_for_round")
    );
    assert_eq!(failure_event.data["agent_id"].as_str(), Some("default"));
    assert_eq!(failure_event.data["status"].as_str(), Some("error"));
    assert_eq!(failure_event.data["duration_ms"].as_u64(), Some(0));
    assert_eq!(
        failure_event.data["summary"].as_str(),
        Some("Failed: CreateTask not exposed for round")
    );
    assert_eq!(failure_event.data["tool_error"]["domain"].as_str(), None);
    let tool_execution_id = failure_event.data["tool_execution_id"]
        .as_str()
        .expect("tool execution id");
    let canonical = runtime
        .storage()
        .read_tool_execution_by_id(tool_execution_id)
        .unwrap()
        .expect("canonical tool execution");
    assert_eq!(
        canonical.output["tool_error"]["kind"],
        "tool_not_exposed_for_round"
    );

    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    assert_eq!(
        transcript
            .iter()
            .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
            .count(),
        2
    );
    let tool_results = transcript
        .iter()
        .find(|entry| entry.kind == TranscriptEntryKind::ToolResults)
        .expect("missing tool results transcript");
    // New format uses refs with tool_call_id
    let refs = tool_results
        .data
        .get("refs")
        .and_then(|v| v.as_array())
        .expect("missing refs array in new format");
    assert_eq!(refs.len(), 1);
    assert_eq!(
        refs[0].get("tool_call_id").and_then(|v| v.as_str()),
        Some("legacy-task")
    );
    assert_eq!(
        refs[0].get("is_error").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn max_output_mutation_tool_call_is_rejected_without_side_effects() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(MaxOutputMutationToolProvider {
        calls: Mutex::new(0),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens: 65_536,
            ..context_config()
        },
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
    assert_eq!(
        outcome.final_text,
        "Recovered after rejected truncated mutation."
    );
    assert_eq!(*provider.calls.lock().await, 2);
    assert!(
        !workspace.path().join("app.txt").exists(),
        "ApplyPatch must not execute when the provider stopped at max_output_tokens"
    );
    assert_eq!(
        runtime
            .storage()
            .read_recent_tool_executions(10)
            .unwrap()
            .len(),
        0
    );

    let events = runtime.storage().read_recent_events(20).unwrap();
    let rejection_event = events
        .iter()
        .find(|event| event.kind == "truncated_mutation_tool_call_rejected")
        .expect("missing truncated_mutation_tool_call_rejected event");
    assert_eq!(
        rejection_event.data["tool_call_id"].as_str(),
        Some("truncated-patch")
    );
    assert_eq!(
        rejection_event.data["tool_name"].as_str(),
        Some("ApplyPatch")
    );
    assert_eq!(
        rejection_event.data["error_kind"].as_str(),
        Some("truncated_mutation_tool_call")
    );

    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let tool_results = transcript
        .iter()
        .find(|entry| entry.kind == TranscriptEntryKind::ToolResults)
        .expect("missing tool results transcript");
    // New format uses refs with provider_visible_text
    let refs = tool_results
        .data
        .get("refs")
        .and_then(|v| v.as_array())
        .expect("missing refs array in new format");
    assert_eq!(refs.len(), 1);
    let content = refs[0]
        .get("provider_visible_text")
        .and_then(|v| v.as_str())
        .expect("tool result content");
    let receipt: serde_json::Value = serde_json::from_str(content).expect("tool error receipt");
    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["tool_name"], "ApplyPatch");
    assert_eq!(receipt["kind"], "truncated_mutation_tool_call");
    assert_eq!(receipt["retryable"], true);
    assert!(content.contains("truncated_mutation_tool_call"));
    assert!(content.contains("max_tokens"));
    assert!(content.contains("was not executed"));
    assert!(content.contains("do not resend the same huge patch unchanged"));
    assert!(content.contains("complete smaller patch"));
    assert!(content.contains("bounded ExecCommand/scripted rewrite"));
    assert!(content.contains("Inspect only the necessary context"));
    assert!(!content.contains("inspect the target file before retrying"));
    assert!(content.len() < 800);
}

#[tokio::test]
async fn detached_runtime_provider_request_still_exposes_agent_tools() {
    let dir = tempdir().unwrap();
    let provider = Arc::new(ToolCaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        InitialWorkspaceBinding::Detached,
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert!(outcome.final_text.contains("captured tool set"));
    let requests = provider.requests.lock().await;
    let tool_names = requests.last().expect("provider request should exist");
    assert!(
        tool_names.iter().any(|name| name == "CreateAgent")
            && tool_names.iter().any(|name| name == "InvokeAgent"),
        "detached runtime should still expose agent tools to provider requests: {tool_names:?}"
    );
    assert!(!tool_names.iter().any(|name| name == "SpawnAgent"));
}

#[tokio::test]
async fn turn_local_compaction_rewrites_older_rounds_into_runtime_recap() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(TurnLocalCompactionProbeProvider {
        calls: Mutex::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let available_tools = crate::tool::ToolRegistry::new(workspace.path().to_path_buf())
        .tool_specs_with_families()
        .unwrap()
        .into_iter()
        .filter(|(family, _)| {
            AgentProfilePreset::PublicNamed.allows_tool_capability_family(*family)
        })
        .filter(|(_, tool)| tool.name != crate::tool::names::X_SEARCH)
        .map(|(_, tool)| tool)
        .collect::<Vec<_>>();
    let continuation_effective_budget = 1_000;
    let prompt_budget_estimated_tokens = turn::estimate_tool_specs_tokens(&available_tools)
        + turn::CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
        + continuation_effective_budget;
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens,
            compaction_keep_recent_estimated_tokens: 180,
            turn_projection_budget_ratio: 1.0,
            turn_projection_min_budget: 0,
            turn_projection_max_budget: prompt_budget_estimated_tokens,
            callback_base_url: String::new(),
            ..context_config()
        },
    )
    .unwrap();

    let mut prompt = test_effective_prompt();
    prompt.system_sections = vec![PromptSection {
        name: "stable_system".into(),
        id: "stable_system".into(),
        content: "Keep runtime boundaries explicit.".into(),
        stability: PromptStability::Stable,
    }];
    prompt.context_sections = vec![PromptSection {
        name: "active_context".into(),
        id: "active_context".into(),
        content: "Preserve Anthropic prompt cache anchors across continuations.".into(),
        stability: PromptStability::AgentScoped,
    }];
    prompt.rendered_system_prompt = prompt
        .system_sections
        .iter()
        .map(render_section)
        .collect::<Vec<_>>()
        .join("\n\n");
    prompt.rendered_context_attachment = prompt
        .context_sections
        .iter()
        .map(render_section)
        .collect::<Vec<_>>()
        .join("\n\n");

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            prompt,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
    assert_eq!(outcome.final_text, "Finished after compacted continuation.");
    assert!(outcome.final_text_source_assistant_round_id.is_some());
    assert!(outcome.terminal.checkpoint.is_some());
    assert_eq!(outcome.terminal.no_brief_reason, None);
    assert_eq!(*provider.calls.lock().await, 6);

    let requests = provider.requests.lock().await;
    let continuation_request = requests.get(3).expect("missing round 4 request");
    let checkpoint_resume_request = requests.get(4).expect("missing round 5 request");
    let pending_delivery_retry_request = requests.get(5).expect("missing round 6 request");
    let first_tool_schema = serde_json::to_value(&requests[0].tools).unwrap();
    assert!(
        requests
            .iter()
            .all(|request| serde_json::to_value(&request.tools).unwrap() == first_tool_schema),
        "resolved route tool order and schema must remain stable across rounds"
    );
    let cache = continuation_request
        .prompt_frame
        .cache
        .as_ref()
        .expect("continuation request should retain prompt cache identity");
    assert_eq!(cache.prompt_cache_key, "default");
    assert!(
        continuation_request
            .prompt_frame
            .system_blocks
            .iter()
            .any(|block| block.cache_breakpoint),
        "continuation request should retain cacheable system anchors"
    );
    let context_blocks = continuation_request
        .conversation
        .first()
        .and_then(|message| match message {
            ConversationMessage::UserBlocks(blocks) => Some(blocks),
            _ => None,
        })
        .expect("continuation request should retain structured context blocks");
    assert!(
        context_blocks.iter().any(|block| block.cache_breakpoint),
        "continuation request should retain cacheable context anchors"
    );
    let serialized_conversation = format!("{:?}", continuation_request.conversation);
    let events = runtime.storage().read_recent_events(50).unwrap();
    let lineage_event = events
        .iter()
        .find(|event| event.kind == "lineage_selected")
        .expect("missing lineage_selected");
    assert_eq!(
        lineage_event.data["tool_capability_projection"]["pruning"].as_str(),
        Some("none")
    );
    assert!(
        lineage_event.data["tool_capability_projection"]["schema_fingerprint"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:"))
    );
    let round_four_event = events
        .iter()
        .find(|event| {
            event.kind == "provider_round_completed" && event.data["round"].as_u64() == Some(4)
        })
        .expect("missing round 4 provider completion event");
    assert_eq!(
        round_four_event.data["prompt_cache_key"].as_str(),
        Some("default")
    );
    assert_eq!(round_four_event.data["compression_epoch"].as_u64(), Some(0));
    let transcript = runtime.storage().read_recent_transcript(20).unwrap();
    let round_four_assistant = transcript
        .iter()
        .find(|entry| entry.kind == TranscriptEntryKind::AssistantRound && entry.round == Some(4))
        .expect("missing round 4 assistant transcript");
    assert_eq!(
        round_four_assistant.data["prompt_cache_key"].as_str(),
        Some("default")
    );
    let compaction_event = events.iter().find(|event| {
        event.kind == "turn_local_compaction_applied"
            && event.data["checkpoint_request_id"].as_str().is_some()
    });
    if let Some(compaction_event) = compaction_event {
        assert!(
            !serialized_conversation.contains("first-round-output-should-not-stay-exact"),
            "older exact tool output should not survive after compaction: {serialized_conversation}"
        );
        let recap = continuation_request
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserText(text)
                    if text.contains("Turn-local recap for older completed rounds") =>
                {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("missing deterministic recap after compaction");
        assert!(recap.contains("Round 1"), "unexpected recap: {recap}");
        assert!(
            recap.contains("ExecCommand completed exit_status=0")
                || recap.contains("ExecCommand promoted_to_task"),
            "unexpected recap: {recap}"
        );
        assert!(!recap.contains("first-round-output-should-not-stay-exact"));
        assert!(serialized_conversation.contains("second-round-output-should-remain-exact"));
        assert!(serialized_conversation.contains("third-round-output-should-remain-exact"));
        assert!(
            compaction_event.data["compacted_rounds"]
                .as_u64()
                .unwrap_or_default()
                >= 1
        );
        assert_eq!(
            round_four_event.data["turn_local_compaction"]["trigger_reason"].as_str(),
            Some("estimated_tokens_exceeded_trigger")
        );
        assert_eq!(
            round_four_event.data["turn_local_compaction"]["compacted_rounds"],
            compaction_event.data["compacted_rounds"]
        );
        let checkpoint_request_id = compaction_event.data["checkpoint_request_id"]
            .as_str()
            .expect("compaction event missing checkpoint_request_id");
        let checkpoint_requested = events
            .iter()
            .find(|event| {
                event.kind == "turn_local_checkpoint_requested"
                    && event.data["checkpoint_request_id"].as_str() == Some(checkpoint_request_id)
            })
            .expect("missing structured checkpoint request event");
        let checkpoint_recorded = events
            .iter()
            .find(|event| {
                event.kind == "turn_local_checkpoint_recorded"
                    && event.data["checkpoint_request_id"].as_str() == Some(checkpoint_request_id)
            })
            .expect("missing structured checkpoint recorded event");
        let checkpoint_assistant_event = events
            .iter()
            .find(|event| {
                event.kind == "assistant_round_recorded"
                    && event.data["checkpoint_request_id"].as_str() == Some(checkpoint_request_id)
            })
            .expect("missing checkpoint assistant round event");
        assert_eq!(
            checkpoint_assistant_event.data["round_purpose"].as_str(),
            Some("runtime_checkpoint")
        );
        assert_eq!(
            checkpoint_assistant_event.data["visibility"].as_str(),
            Some("runtime_private")
        );
        let checkpoint_assistant_round_id = checkpoint_assistant_event.data["assistant_round_id"]
            .as_str()
            .expect("checkpoint assistant round id");
        assert_ne!(
            outcome.final_text_source_assistant_round_id.as_deref(),
            Some(checkpoint_assistant_round_id)
        );
        let checkpoint_transcript = transcript
            .iter()
            .find(|entry| entry.id == checkpoint_assistant_round_id)
            .expect("missing checkpoint assistant transcript");
        assert_eq!(
            checkpoint_transcript.data["round_purpose"].as_str(),
            Some("runtime_checkpoint")
        );
        assert_eq!(
            Some(checkpoint_request_id),
            checkpoint_requested.data["checkpoint_request_id"].as_str()
        );
        assert_eq!(
            Some(checkpoint_request_id),
            checkpoint_recorded.data["checkpoint_request_id"].as_str()
        );
        assert_eq!(
            checkpoint_recorded.data["checkpoint_recorded"].as_bool(),
            Some(true)
        );
        assert!(checkpoint_recorded.data["text_preview"].as_str().is_some());
        assert!(events
            .iter()
            .any(|event| event.kind == "turn_local_checkpoint_resume_requested"));
        assert!(events
            .iter()
            .any(|event| event.kind == "checkpoint_operator_delivery_retry"));
        assert!(
            format!("{:?}", checkpoint_resume_request.conversation)
                .contains("Continue from the checkpoint's next goal-aligned action now"),
            "checkpoint-only compaction response should continue inside the same turn"
        );
        assert!(
            format!("{:?}", checkpoint_resume_request.conversation)
                .contains("has not been delivered to the operator"),
            "checkpoint continuation must state that private checkpoint content is undelivered"
        );
        assert!(
            format!("{:?}", pending_delivery_retry_request.conversation)
                .contains("has not been delivered to the operator"),
            "an empty visible response must not complete pending checkpoint delivery"
        );
    } else {
        assert!(serialized_conversation.contains("first-round-output-should-not-stay-exact"));
        assert!(serialized_conversation.contains("second-round-output-should-remain-exact"));
        assert!(serialized_conversation.contains("third-round-output-should-remain-exact"));
    }
    if let Some(checkpoint) = continuation_request
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserText(text) if text.contains("progress checkpoint request") => {
                Some(text.clone())
            }
            _ => None,
        })
    {
        if checkpoint.contains("delta progress checkpoint request") {
            assert!(checkpoint.contains("Base checkpoint preview"));
            assert!(checkpoint.contains("whether the next bounded action changed"));
        } else {
            assert!(checkpoint.contains("current user goal"));
            assert!(checkpoint.contains("what remains unknown"));
            assert!(checkpoint.contains("next goal-aligned action"));
            assert!(checkpoint.contains("Do not assume the task requires code changes"));
        }
        assert!(!checkpoint.contains("start editing"));
        assert!(!checkpoint.contains("begin implementation"));
    }
}

#[tokio::test]
async fn turn_local_compaction_fails_fast_when_baseline_exceeds_budget() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(BaselineOverBudgetProbeProvider {
        calls: Mutex::new(0),
    });
    let available_tools = crate::tool::ToolRegistry::new(workspace.path().to_path_buf())
        .tool_specs_with_families()
        .unwrap()
        .into_iter()
        .filter(|(family, _)| {
            AgentProfilePreset::PublicNamed.allows_tool_capability_family(*family)
        })
        .filter(|(_, tool)| tool.name != crate::tool::names::X_SEARCH)
        .map(|(_, tool)| tool)
        .collect::<Vec<_>>();
    let continuation_effective_budget = 320;
    let prompt_budget_estimated_tokens = turn::estimate_tool_specs_tokens(&available_tools)
        + turn::CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
        + continuation_effective_budget;
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens,
            compaction_keep_recent_estimated_tokens: 120,
            turn_projection_budget_ratio: 1.0,
            turn_projection_min_budget: 0,
            turn_projection_max_budget: prompt_budget_estimated_tokens,
            callback_base_url: String::new(),
            ..context_config()
        },
    )
    .unwrap();
    let mut prompt = test_effective_prompt();
    prompt.rendered_system_prompt = "system ".repeat(700);

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            prompt,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(*provider.calls.lock().await, 1);
    assert_eq!(outcome.terminal_kind, TurnTerminalKind::BaselineOverBudget);
    assert!(outcome
        .final_text
        .contains("continuation baseline exceeded the prompt budget"));

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state
            .last_turn_terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(TurnTerminalKind::BaselineOverBudget)
    );

    let events = runtime.storage().read_recent_events(20).unwrap();
    let baseline_event = events
        .iter()
        .find(|event| event.kind == "turn_local_baseline_over_budget")
        .expect("missing turn_local_baseline_over_budget event");
    assert_eq!(
        baseline_event.data["reason"].as_str(),
        Some("minimum_exact_round_unfit")
    );
    assert_eq!(
        baseline_event.data["recent_turns_retry_attempts"].as_u64(),
        Some(0)
    );
    assert!(baseline_event.data["final_recent_turns_budget"].is_null());
    assert!(
        baseline_event.data["estimated_baseline_tokens"]
            .as_u64()
            .unwrap_or_default()
            > baseline_event.data["effective_budget_estimated_tokens"]
                .as_u64()
                .unwrap_or_default()
    );
    assert!(
        events
            .iter()
            .all(|event| event.kind != "turn_local_compaction_applied"),
        "unrecoverable baseline-over-budget should not masquerade as compaction"
    );
}

#[tokio::test]
async fn turn_local_continuation_recovers_by_reprojecting_recent_turns() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(RecentTurnsRecoveryProbeProvider {
        calls: Mutex::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            // Assemble history independently of the continuation budget calibrated below.
            prompt_budget_estimated_tokens: 128_000,
            turn_projection_budget_ratio: 1.0,
            turn_projection_min_budget: 0,
            turn_projection_max_budget: 12_000,
            callback_base_url: String::new(),
            ..context_config()
        },
    )
    .unwrap();

    let identity = runtime.agent_identity_view().await.unwrap();
    let (_, available_tools, _, _, _) = runtime.provider_tool_selection(&identity).await.unwrap();
    let mut historical_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: format!(
                "historical context {}",
                "large-history-token ".repeat(12_000)
            ),
        },
    );
    historical_message.turn_id = Some("turn-large-history".into());
    runtime
        .storage()
        .append_message(&historical_message)
        .unwrap();
    let mut historical_turn = TurnRecord::new("default", "turn-large-history", 1);
    historical_turn.input_message_ids = vec![historical_message.id.clone()];
    historical_turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
        &historical_message,
    ));
    runtime.storage().append_turn(&historical_turn).unwrap();

    let prompt = runtime
        .preview_prompt(
            "continue after the large historical turn".into(),
            AuthorityClass::OperatorInstruction,
        )
        .await
        .unwrap();
    let reduced_prompt = prompt
        .reproject_recent_turns(runtime.storage(), 0, &available_tools)
        .expect("fixture must contain removable recent turns");
    let baseline_tokens = |prompt: &crate::prompt::EffectivePrompt| {
        let frame = super::super::provider_turn::build_provider_prompt_frame(prompt);
        frame
            .system_blocks
            .iter()
            .chain(&frame.context_blocks)
            // Match turn::projection's character-based estimator, not the
            // byte-based estimator used for initial prompt assembly.
            .map(|block| block.text.chars().count().saturating_add(3) / 4)
            .sum::<usize>()
    };
    let initial_baseline = baseline_tokens(&prompt);
    let reduced_baseline = baseline_tokens(&reduced_prompt);
    // The initial request fits, but the bounded tool output alone exceeds this
    // headroom. Removing history leaves ample room for the entire exact round.
    let continuation_effective_budget = initial_baseline + 128;
    assert!(
        reduced_baseline + 4096 < continuation_effective_budget,
        "fixture must leave recovery headroom after removing history"
    );
    runtime
        .inner
        .context_config
        .write()
        .await
        .prompt_budget_estimated_tokens = turn::estimate_tool_specs_tokens(&available_tools)
        + turn::CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
        + continuation_effective_budget;
    runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            prompt,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state
            .last_turn_terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(TurnTerminalKind::Completed)
    );
    assert_eq!(*provider.calls.lock().await, 2);

    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 2);
    let first_context = requests[0]
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserBlocks(blocks) => Some(
                blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let continuation_context = requests[1]
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserBlocks(blocks) => Some(
                blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    assert!(first_context.contains("recent_turns"));
    assert!(
        continuation_context.len() < first_context.len(),
        "continuation should carry a smaller recent-turns projection"
    );
    assert!(continuation_context.contains("continue after the large historical turn"));
    assert!(requests[1].conversation.iter().any(|message| {
        matches!(
            message,
            ConversationMessage::AssistantBlocks(blocks)
                if blocks.iter().any(|block| matches!(
                    block,
                    ModelBlock::ToolUse { id, .. } if id == "exec-recent-turns-recovery"
                ))
        )
    }));
    drop(requests);

    let events = runtime.storage().read_recent_events(40).unwrap();
    let retry_event = events
        .iter()
        .find(|event| event.kind == "turn_local_recent_turns_retry")
        .expect("missing recent-turns recovery event");
    assert_eq!(retry_event.data["attempt"].as_u64(), Some(1));
    assert_eq!(
        retry_event.data["reason"].as_str(),
        Some("minimum_exact_round_unfit")
    );
    assert!(
        retry_event.data["deficit_estimated_tokens"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert!(
        retry_event.data["next_recent_turns_budget"]
            .as_u64()
            .unwrap_or_default()
            < retry_event.data["previous_recent_turns_budget"]
                .as_u64()
                .unwrap_or_default()
    );
    assert!(events
        .iter()
        .all(|event| event.kind != "turn_local_baseline_over_budget"));
}

#[tokio::test]
async fn turn_local_continuation_uses_full_prompt_budget_above_history_ceiling() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(LargeBudgetContinuationProbeProvider {
        calls: Mutex::new(0),
    });
    let available_tools = crate::tool::ToolRegistry::new(workspace.path().to_path_buf())
        .tool_specs_with_families()
        .unwrap()
        .into_iter()
        .filter(|(family, _)| {
            AgentProfilePreset::PublicNamed.allows_tool_capability_family(*family)
        })
        .filter(|(_, tool)| tool.name != crate::tool::names::X_SEARCH)
        .map(|(_, tool)| tool)
        .collect::<Vec<_>>();
    let prompt_budget_estimated_tokens = turn::estimate_tool_specs_tokens(&available_tools)
        + turn::CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
        + 70_000;
    let context_config = ContextConfig {
        prompt_budget_estimated_tokens,
        turn_projection_budget_ratio: 1.0,
        turn_projection_min_budget: 0,
        turn_projection_max_budget: 64_000,
        callback_base_url: String::new(),
        ..context_config()
    };
    assert_eq!(context_config.turn_projection_budget(), 64_000);
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config,
    )
    .unwrap();
    let mut prompt = test_effective_prompt();
    prompt.rendered_system_prompt = "system ".repeat(36_000);

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            prompt,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(*provider.calls.lock().await, 2);
    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
    assert!(outcome
        .final_text
        .contains("Finished within the resolved model prompt budget."));
    assert!(runtime
        .storage()
        .read_recent_events(20)
        .unwrap()
        .iter()
        .all(|event| event.kind != "turn_local_baseline_over_budget"));
}

#[tokio::test]
async fn context_length_exceeded_turn_fails_fast_without_runtime_error() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(ContextLengthExceededProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "trigger provider context length fail-fast".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state
            .last_turn_terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(TurnTerminalKind::Aborted)
    );

    let briefs = runtime.recent_briefs(10).await.unwrap();
    let failure = briefs
        .iter()
        .rev()
        .find(|brief| brief.kind == BriefKind::Failure)
        .expect("failure brief should exist");
    assert!(failure.text.contains("context_length_exceeded"));
    assert_eq!(failure.turn_index, Some(1));
    let turn = runtime
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .pop()
        .expect("aborted terminal turn");
    assert_eq!(turn.produced_brief_ids, vec![failure.id.clone()]);
    assert_eq!(
        turn.terminal
            .as_ref()
            .and_then(|terminal| terminal.no_brief_reason.as_ref()),
        None
    );

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "turn_context_length_exceeded"));
    assert!(!events.iter().any(|event| event.kind == "runtime_error"));
}

#[tokio::test]
async fn context_length_exceeded_turn_recovers_once_with_recent_turns() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(RecoveringContextLengthProvider {
        calls: Mutex::new(0),
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

    let historical_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "historical context".repeat(2048),
        },
    );
    let mut historical_turn = TurnRecord::new("default", "turn-context-recovery-history", 1);
    let mut historical_message = historical_message;
    historical_message.turn_id = Some(historical_turn.turn_id.clone());
    historical_turn.input_message_ids = vec![historical_message.id.clone()];
    historical_turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
        &historical_message,
    ));
    runtime
        .storage()
        .append_message(&historical_message)
        .unwrap();
    runtime.storage().append_turn(&historical_turn).unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "trigger recoverable provider context overflow".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(*provider.calls.lock().await, 2);
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state
            .last_turn_terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(TurnTerminalKind::Completed)
    );
    let events = runtime.storage().read_recent_events(50).unwrap();
    let recovery = events
        .iter()
        .find(|event| event.kind == "turn_context_length_recovery")
        .expect("context recovery audit event should exist");
    assert_eq!(recovery.data["attempt"].as_u64(), Some(1));
    assert!(
        recovery.data["next_recent_turns_budget"].as_u64()
            < recovery.data["previous_recent_turns_budget"].as_u64()
    );
    assert!(!events
        .iter()
        .any(|event| event.kind == "turn_context_length_exceeded"));
}

#[tokio::test]
async fn runtime_persists_provider_attempt_timeline_on_successful_round() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(TimelineProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let _outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let assistant_round = transcript
        .iter()
        .find(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
        .expect("missing assistant round transcript");
    let timeline = assistant_round.data["provider_attempt_timeline"]
        .as_object()
        .expect("missing provider attempt timeline");
    assert_eq!(
        timeline["winning_model_ref"].as_str(),
        Some("anthropic/claude-sonnet-4-6")
    );
    assert_eq!(
        timeline["requested_model_ref"].as_str(),
        Some("openai/gpt-5.4")
    );
    assert_eq!(
        timeline["active_model_ref"].as_str(),
        Some("anthropic/claude-sonnet-4-6")
    );
    assert_eq!(
        assistant_round.data["requested_model"].as_str(),
        Some("openai@default/gpt-5.4")
    );
    assert_eq!(
        assistant_round.data["active_model"].as_str(),
        Some("anthropic@default/claude-sonnet-4-6")
    );
    assert_eq!(
        assistant_round.data["fallback_active"].as_bool(),
        Some(true)
    );
    assert_eq!(
        assistant_round.data["token_usage"]["total_tokens"].as_u64(),
        Some(18)
    );
    assert_eq!(timeline["attempts"].as_array().unwrap().len(), 2);
    for attempt in timeline["attempts"].as_array().unwrap() {
        assert!(attempt.get("started_at").is_none());
        assert!(attempt.get("completed_at").is_none());
        assert!(attempt["duration_ms"].as_u64().is_some());
    }
    assert_eq!(
        timeline["aggregated_token_usage"]["total_tokens"].as_u64(),
        Some(18)
    );

    let events = runtime.storage().read_recent_events(10).unwrap();
    let provider_event = events
        .iter()
        .find(|event| event.kind == "provider_round_completed")
        .expect("missing provider_round_completed");
    assert_eq!(
        provider_event.data["token_usage"]["total_tokens"].as_u64(),
        Some(18)
    );
    assert_eq!(
        provider_event.data["provider_attempt_timeline"]["attempts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(provider_event.data["provider_attempt_timeline"]["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|attempt| attempt["duration_ms"].as_u64().is_some()));
    assert!(provider_event.data["context_build_ms"].as_u64().is_some());
    assert!(provider_event.data["provider_round_ms"].as_u64().is_some());
    assert!(provider_event.data["provider_started_at"]
        .as_str()
        .is_some());
    assert!(provider_event.data["provider_completed_at"]
        .as_str()
        .is_some());
    assert_eq!(
        provider_event.data["requested_model"].as_str(),
        Some("openai@default/gpt-5.4")
    );
    assert_eq!(
        provider_event.data["active_model"].as_str(),
        Some("anthropic@default/claude-sonnet-4-6")
    );
    assert_eq!(provider_event.data["fallback_active"].as_bool(), Some(true));
}

#[tokio::test]
async fn provider_failure_before_output_defers_fallback_to_next_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(DeferredFallbackProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(outcome.terminal_kind, TurnTerminalKind::DeferredToFallback);
    assert!(!outcome.should_sleep);
    assert_eq!(outcome.sleep_duration_ms, None);
    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_fallback_model.is_none());
    assert_eq!(
        state.last_turn_terminal.as_ref().map(|record| record.kind),
        Some(TurnTerminalKind::DeferredToFallback)
    );
    let queued = {
        let guard = runtime.inner.agent.lock().await;
        guard.queue.peek().cloned().expect("fallback followup")
    };
    assert_eq!(queued.kind, MessageKind::InternalFollowup);
    assert_eq!(queued.priority, Priority::Next);
    assert!(matches!(
        queued.authority_class,
        AuthorityClass::RuntimeInstruction
    ));
    assert_eq!(
        queued
            .metadata
            .as_ref()
            .and_then(|metadata| { metadata["provider_recovery"]["fallback_model_ref"].as_str() }),
        Some("anthropic@default/claude-sonnet-4-6")
    );
    assert_eq!(
        queued
            .metadata
            .as_ref()
            .and_then(|metadata| metadata["provider_recovery"]["fallback_attempt"].as_u64()),
        Some(1)
    );
    assert_eq!(
        crate::runtime::turn::TurnModelSelection::from_message(&queued)
            .unwrap()
            .fallback_model()
            .map(|model| model.as_string())
            .as_deref(),
        Some("anthropic@default/claude-sonnet-4-6")
    );

    let events = wait_for_audit_events(
        &runtime,
        20,
        |events| {
            events
                .iter()
                .any(|event| event.kind == "lineage_retry_exhausted")
                && events
                    .iter()
                    .any(|event| event.kind == "deferred_to_fallback")
                && events.iter().any(|event| event.kind == "recovery_enqueued")
        },
        "provider failure fallback events",
    )
    .await;
    assert!(events
        .iter()
        .any(|event| event.kind == "lineage_retry_exhausted"));
    let failed_attempts = events
        .iter()
        .filter(|event| event.kind == "provider_attempt_failed")
        .collect::<Vec<_>>();
    assert_eq!(failed_attempts.len(), 1);
    let failed_attempt = failed_attempts[0];
    assert_eq!(
        failed_attempt.data["model_ref"].as_str(),
        Some("openai/gpt-5.4")
    );
    assert_eq!(failed_attempt.data["attempt"].as_u64(), Some(3));
    assert_eq!(
        failed_attempt.data["failure_kind"].as_str(),
        Some("server_error")
    );
    assert_eq!(
        failed_attempt.data["outcome"].as_str(),
        Some("retries_exhausted")
    );
    assert_eq!(
        failed_attempt.data["pending_fallback_model_ref"].as_str(),
        Some("anthropic/claude-sonnet-4-6")
    );
    let failed_attempt_json = failed_attempt.data.to_string();
    assert!(!failed_attempt_json.contains("Authorization"));
    assert!(!failed_attempt_json.contains("token"));
    let deferred = events
        .iter()
        .find(|event| event.kind == "deferred_to_fallback")
        .expect("deferred_to_fallback event");
    assert_eq!(
        deferred.data["fallback_model_ref"].as_str(),
        Some("anthropic/claude-sonnet-4-6")
    );
    assert!(deferred.data["error"]
        .as_str()
        .is_some_and(|error| error.contains("all configured providers failed")));
    assert!(deferred.data["operator_message"]
        .as_str()
        .is_some_and(|message| message.contains("Queued fallback turn")));
    assert!(events.iter().any(|event| event.kind == "recovery_enqueued"));
    assert!(!events
        .iter()
        .any(|event| event.kind == "recovery_turn_started"));
}

#[tokio::test]
async fn network_failure_delays_one_recovery_instead_of_immediate_fallback() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(DeferredNetworkFallbackProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(outcome.terminal_kind, TurnTerminalKind::DeferredToFallback);
    assert!(outcome.should_sleep);
    let delay_ms = outcome.sleep_duration_ms.expect("network recovery delay");
    assert!((crate::provider::PROVIDER_RECOVERY_BASE_BACKOFF_MS
        ..crate::provider::PROVIDER_RECOVERY_BASE_BACKOFF_MS * 5 / 4)
        .contains(&delay_ms));

    let queued = {
        let guard = runtime.inner.agent.lock().await;
        assert_eq!(guard.queue.len(), 1);
        guard.queue.peek().cloned().expect("one fallback followup")
    };
    assert_eq!(
        queued
            .metadata
            .as_ref()
            .and_then(|metadata| metadata["provider_recovery"]["fallback_attempt"].as_u64()),
        Some(1)
    );

    let events = runtime.storage().read_recent_events(20).unwrap();
    let recovery = events
        .iter()
        .find(|event| event.kind == "recovery_enqueued")
        .expect("recovery_enqueued event");
    assert_eq!(recovery.data["recovery_delay_ms"].as_u64(), Some(delay_ms));
}

#[tokio::test]
async fn provider_recovery_budget_exhaustion_stops_the_lineage() {
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
    let error = DeferredNetworkFallbackProvider
        .complete_turn(ProviderTurnRequest::plain("base", Vec::new(), Vec::new()))
        .await
        .expect_err("network provider failure");
    let recovery = crate::runtime::turn::ProviderRecoveryDirective {
        fallback_model_ref: crate::config::ModelRouteRef::parse_compatible("openai/gpt-5.4")
            .unwrap(),
        fallback_attempt: crate::provider::PROVIDER_RECOVERY_MAX_FALLBACKS,
        root_message_id: "message-root".into(),
        source_turn_id: "turn-source".into(),
        source_message_id: "message-source".into(),
        source_terminal_kind: TurnTerminalKind::DeferredToFallback,
        source_round: 1,
    };

    let outcome = runtime
        .maybe_defer_provider_lineage_failure(
            "default",
            2,
            &error,
            Some(&recovery),
            None,
            10,
            false,
            false,
        )
        .await
        .unwrap();

    assert!(outcome.is_none());
    assert_eq!(runtime.inner.agent.lock().await.queue.len(), 0);
    let events = runtime.storage().read_recent_events(20).unwrap();
    let exhausted = events
        .iter()
        .find(|event| event.kind == "provider_recovery_budget_exhausted")
        .expect("provider_recovery_budget_exhausted event");
    assert_eq!(
        exhausted.data["fallback_attempt"].as_u64(),
        Some(crate::provider::PROVIDER_RECOVERY_MAX_FALLBACKS as u64)
    );
    assert!(!events.iter().any(|event| event.kind == "recovery_enqueued"));
}

#[test]
fn provider_recovery_directive_requires_runtime_owned_recovery_provenance() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "try to select a model".into(),
        },
    );
    message.metadata = Some(serde_json::json!({
        "provider_recovery": {
            "fallback_model_ref": "anthropic/claude-sonnet-4-6",
            "source_turn_id": "turn-source",
            "source_message_id": "message-source",
            "source_terminal_kind": "deferred_to_fallback",
            "source_round": 1
        }
    }));

    let selection = crate::runtime::turn::TurnModelSelection::from_message(&message).unwrap();
    assert!(selection.fallback_model().is_none());
}

async fn assert_successful_same_owner_turn_supersedes_queued_provider_recovery(
    mut superseding: MessageEnvelope,
) {
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

    let mut source_turn = TurnRecord::new("default", "turn-source", 1);
    source_turn.current_work_item_id = Some("work-1".into());
    runtime.storage().append_turn(&source_turn).unwrap();

    let mut recovery = MessageEnvelope::new(
        "default",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "model_lineage_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "continue recovery".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        crate::types::AdmissionContext::RuntimeOwned,
    );
    recovery.work_item_id = Some("work-1".into());
    recovery.metadata = Some(serde_json::json!({
        "provider_recovery": {
            "fallback_model_ref": "anthropic/claude-sonnet-4-6",
            "source_turn_id": "turn-source",
            "source_message_id": "message-source",
            "source_terminal_kind": "deferred_to_fallback",
            "source_round": 1
        }
    }));
    let recovery = runtime.enqueue(recovery).await.unwrap();

    superseding.turn_id = Some("turn-success".into());
    let mut transition = terminal_transition(&superseding, Some("work-1"));
    transition.terminal.turn_index = 2;
    transition.turn_record.turn_index = 2;
    transition.turn_record.produced_brief_ids = vec!["brief-success".into()];

    assert_eq!(
        runtime
            .maybe_supersede_queued_provider_recovery(&superseding, Some(&transition))
            .await
            .unwrap(),
        1
    );
    let queue_entry = runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest_all()
        .unwrap()
        .into_iter()
        .find(|entry| entry.message_id == recovery.id)
        .expect("recovery queue entry");
    assert_eq!(queue_entry.status, QueueEntryStatus::Dropped);
    assert!(runtime
        .inner
        .agent
        .lock()
        .await
        .queue
        .peek_next_matching(|message| message.id == recovery.id)
        .is_none());
    assert!(runtime
        .storage()
        .read_recent_events(20)
        .unwrap()
        .iter()
        .any(|event| event.kind == "recovery_superseded"));
}

#[tokio::test]
async fn successful_ordinary_turn_supersedes_queued_provider_recovery() {
    assert_successful_same_owner_turn_supersedes_queued_provider_recovery(MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "continue successfully".into(),
        },
    ))
    .await;
}

#[tokio::test]
async fn successful_recovery_turn_supersedes_older_queued_provider_recovery() {
    let mut superseding = MessageEnvelope::new(
        "default",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "model_lineage_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "continue newer recovery".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        crate::types::AdmissionContext::RuntimeOwned,
    );
    superseding.metadata = Some(serde_json::json!({
        "provider_recovery": {
            "fallback_model_ref": "openai/gpt-5.4",
            "source_turn_id": "turn-newer-source",
            "source_message_id": "message-newer-source",
            "source_terminal_kind": "deferred_to_fallback",
            "source_round": 1
        }
    }));
    assert_successful_same_owner_turn_supersedes_queued_provider_recovery(superseding).await;
}

#[tokio::test]
async fn view_image_selection_uses_current_turn_fallback_model() {
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
    *runtime.inner.turn_fallback_model.write().await = Some(
        crate::config::ModelRouteRef::parse_compatible("anthropic/claude-sonnet-4-6").unwrap(),
    );

    let selection = runtime.current_view_image_vision_selection().await.unwrap();
    assert_eq!(selection.primary_provider.as_deref(), Some("anthropic"));
    assert_eq!(
        selection.primary_model.as_deref(),
        Some("claude-sonnet-4-6")
    );
}

#[tokio::test]
async fn apply_patch_surface_uses_current_turn_fallback_model() {
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

    // Without the turn-local fallback binding the surface follows the primary
    // chain head (anthropic in this test setup).
    assert_eq!(
        runtime.current_apply_patch_surface().await,
        ApplyPatchSurface::UnifiedDiffJson
    );

    *runtime.inner.turn_fallback_model.write().await = Some(
        crate::config::ModelRouteRef::parse_compatible("deepseek@responses/deepseek-v4-pro")
            .unwrap(),
    );

    let surface = runtime.current_apply_patch_surface().await;
    assert_eq!(surface, ApplyPatchSurface::CodexDslFreeform);
}

#[tokio::test]
async fn concurrent_view_image_selection_discovers_ollama_vision_once_from_cold_cache() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let stopped = Arc::new(AtomicBool::new(false));
    let tags_requests = Arc::new(AtomicUsize::new(0));
    let show_requests = Arc::new(AtomicUsize::new(0));
    let server = {
        let stopped = Arc::clone(&stopped);
        let tags_requests = Arc::clone(&tags_requests);
        let show_requests = Arc::clone(&show_requests);
        std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("Ollama test server accept failed: {error}"),
                };
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let body = if request.starts_with("GET /api/tags HTTP/1.1") {
                    tags_requests.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    r#"{"models":[{"name":"qwen3-vl:latest"}]}"#
                } else if request.starts_with("POST /api/show HTTP/1.1") {
                    show_requests.fetch_add(1, Ordering::SeqCst);
                    assert!(request.contains(r#""model":"qwen3-vl:latest""#));
                    r#"{"capabilities":["vision"],"model_info":{"qwen3.context_length":131072}}"#
                } else {
                    panic!("unexpected Ollama test request: {request}");
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        })
    };

    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"ollama/qwen3-vl:latest"}}"#,
    )
    .unwrap();
    let mut config = {
        let _env_lock = crate::test_env::lock_env();
        AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap()
    };
    config
        .providers
        .get_mut(&crate::config::ProviderId::parse("ollama").unwrap())
        .unwrap()
        .base_url = base_url;
    let host = RuntimeHost::new(config).unwrap();
    let runtime = host.default_runtime().await.unwrap();

    let (first, second) = tokio::join!(
        runtime.current_view_image_vision_selection(),
        runtime.current_view_image_vision_selection()
    );
    stopped.store(true, Ordering::SeqCst);
    server.join().unwrap();

    for selection in [first.unwrap(), second.unwrap()] {
        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::NativeImageWithObservation
        );
        assert_eq!(selection.primary_provider.as_deref(), Some("ollama"));
        assert_eq!(selection.primary_model.as_deref(), Some("qwen3-vl:latest"));
    }
    assert_eq!(tags_requests.load(Ordering::SeqCst), 1);
    assert_eq!(show_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn panicking_model_discovery_refresh_releases_waiters_and_in_flight_state() {
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
    let provider_id = crate::config::ProviderId::parse("ollama").unwrap();
    runtime
        .inner
        .model_discovery_refreshes
        .lock()
        .await
        .insert(provider_id.clone());

    let notified = runtime.inner.model_discovery_refresh_notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let refresh_guard = crate::runtime::bootstrap::ModelDiscoveryRefreshGuard::new(
        runtime.clone(),
        provider_id.clone(),
    );
    let refresh = tokio::spawn(async move {
        let _refresh_guard = refresh_guard;
        panic!("simulated provider discovery panic");
    });

    assert!(refresh.await.unwrap_err().is_panic());
    tokio::time::timeout(std::time::Duration::from_secs(1), notified)
        .await
        .expect("panic cleanup should notify discovery waiters");
    assert!(!runtime
        .inner
        .model_discovery_refreshes
        .lock()
        .await
        .contains(&provider_id));
}

#[tokio::test]
async fn fallback_turn_model_state_uses_fallback_model_policy() {
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
    let state = runtime.agent_state().await.unwrap();
    let fallback =
        crate::config::ModelRouteRef::parse("deepseek@responses/deepseek-v4-pro").unwrap();

    let model_state = runtime.model_state_for_turn(&state, Some(&fallback));
    let snapshot = runtime.inner.config_snapshot.load();
    let expected_policy = snapshot
        .model_catalog
        .resolved_model_policy(&snapshot.base_context_config, Some(&fallback));

    assert_eq!(model_state.active_model.as_ref(), Some(&fallback));
    assert_eq!(model_state.resolved_policy, expected_policy);
    assert_eq!(
        model_state.resolved_policy.model_ref.as_string(),
        "deepseek/deepseek-v4-pro"
    );
}

#[tokio::test]
async fn bootstrap_discards_legacy_fallback_slot_but_preserves_typed_recovery() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let recovery_id = {
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
            guard.state.pending_fallback_model = Some(
                crate::config::ModelRouteRef::parse_compatible("anthropic/claude-sonnet-4-6")
                    .unwrap(),
            );
            guard.persist_state(&runtime.inner.storage).unwrap();
        }
        let mut recovery = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "model_lineage_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "continue recovery".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        recovery.metadata = Some(serde_json::json!({
            "provider_recovery": {
                "fallback_model_ref": "anthropic/claude-sonnet-4-6",
                "source_turn_id": "turn-source",
                "source_message_id": "message-source",
                "source_terminal_kind": "deferred_to_fallback",
                "source_round": 1
            }
        }));
        runtime.enqueue(recovery).await.unwrap().id
    };

    let reopened = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    assert!(reopened
        .agent_state()
        .await
        .unwrap()
        .pending_fallback_model
        .is_none());
    let recovery = reopened
        .inner
        .agent
        .lock()
        .await
        .queue
        .peek_next_matching(|message| message.id == recovery_id)
        .cloned()
        .expect("typed recovery should survive bootstrap");
    assert_eq!(
        crate::runtime::turn::TurnModelSelection::from_message(&recovery)
            .unwrap()
            .fallback_model()
            .map(|model| model.as_string())
            .as_deref(),
        Some("anthropic@default/claude-sonnet-4-6")
    );
    assert!(reopened
        .storage()
        .read_recent_events(20)
        .unwrap()
        .iter()
        .any(|event| event.kind == "legacy_pending_fallback_discarded"));
}

#[tokio::test]
async fn provider_failure_after_accepted_output_queues_recovery_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(TextThenFailingFallbackProvider {
            calls: Mutex::new(0),
        }),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        outcome.terminal_kind,
        TurnTerminalKind::ProviderFailedNeedsRecovery
    );
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state.last_turn_terminal.as_ref().map(|record| record.kind),
        Some(TurnTerminalKind::ProviderFailedNeedsRecovery)
    );
    assert_eq!(
        state
            .last_turn_terminal
            .as_ref()
            .and_then(|record| record.last_assistant_message.as_deref()),
        Some("Partial report heading")
    );

    let events = runtime.storage().read_recent_events(30).unwrap();
    let recovery = events
        .iter()
        .find(|event| event.kind == "provider_failed_needs_recovery")
        .expect("provider_failed_needs_recovery event");
    assert!(recovery.data["operator_message"]
        .as_str()
        .is_some_and(|message| message.contains("Queued recovery turn")));
    let exhausted = events
        .iter()
        .find(|event| event.kind == "lineage_retry_exhausted")
        .expect("lineage retry exhausted event");
    assert_eq!(
        exhausted.data["side_effect_boundary_crossed"].as_bool(),
        Some(true)
    );
}

#[tokio::test]
async fn runtime_records_turn_latency_phase_events_for_provider_and_tool() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(OneToolThenTextProvider {
            calls: Mutex::new(0),
        }),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(outcome.final_text, "done");
    let events = wait_for_audit_events(
        &runtime,
        20,
        |events| {
            let provider_count = events
                .iter()
                .filter(|event| event.kind == "provider_round_completed")
                .count();
            provider_count == 2
                && events.iter().any(|event| event.kind == "tool_executed")
                && events.iter().any(|event| event.kind == "turn_terminal")
        },
        "turn latency phase events",
    )
    .await;
    let provider_events = events
        .iter()
        .filter(|event| event.kind == "provider_round_completed")
        .collect::<Vec<_>>();
    assert_eq!(provider_events.len(), 2);
    assert!(provider_events.iter().all(|event| {
        event.data["context_build_ms"].as_u64().is_some()
            && event.data["provider_round_ms"].as_u64().is_some()
            && event.data["provider_started_at"].as_str().is_some()
            && event.data["provider_completed_at"].as_str().is_some()
    }));
    let tool_event = events
        .iter()
        .find(|event| event.kind == "tool_executed")
        .expect("missing tool latency event");
    assert_eq!(tool_event.data["tool_name"].as_str(), Some("ExecCommand"));
    assert!(tool_event.data["duration_ms"].as_u64().is_some());
    let terminal = events
        .iter()
        .find(|event| event.kind == "turn_terminal")
        .expect("missing turn terminal event");
    assert_eq!(terminal.data["kind"].as_str(), Some("completed"));
    assert!(terminal.data["duration_ms"].as_u64().is_some());
}

#[tokio::test]
async fn runtime_failure_artifacts_preserve_provider_attempt_timeline() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(FailingTimelineProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "trigger provider failure".into(),
        },
    );
    let error = runtime
        .current_provider()
        .await
        .complete_turn(ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("prompt".into())],
            Vec::new(),
        ))
        .await
        .unwrap_err();
    runtime
        .persist_runtime_failure_artifacts(&message, &error)
        .await
        .unwrap();

    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let failure = transcript
        .iter()
        .find(|entry| entry.kind == TranscriptEntryKind::RuntimeFailure)
        .expect("missing runtime failure transcript");
    assert_eq!(
        failure.data["provider_attempt_timeline"]["attempts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        failure.data["provider_attempt_timeline"]["attempts"][0]["duration_ms"]
            .as_u64()
            .is_some()
    );
    assert_eq!(
        failure.data["provider_attempt_timeline"]["attempts"][0]["transport_diagnostics"]
            ["provider"],
        "openai"
    );
    assert_eq!(
        failure.data["provider_attempt_timeline"]["attempts"][0]["transport_diagnostics"]["stage"],
        "request_send"
    );
    assert_eq!(
        failure.data["failure_artifact"]["metadata"]["url"],
        "https://example.com/v1/responses"
    );
    assert_eq!(
        failure.data["failure_artifact"]["metadata"]["http_trace_path"],
        ".holon/http-trace/default/trace-1-1.jsonl"
    );
    assert_eq!(failure.data["failure_artifact"]["domain"], "provider");
    assert_eq!(failure.data["failure_artifact"]["retryable"], false);
    assert_eq!(
        failure.data["failure_artifact"]["context"]["message_id"],
        message.id
    );
    assert_eq!(
        failure.data["failure_artifact"]["context"]["provider"],
        "openai"
    );
    assert_eq!(
        failure.data["failure_artifact"]["context"]["model_ref"],
        "openai/gpt-5.4"
    );
    assert!(failure.data["token_usage"].is_null());
    assert!(failure.data["provider_attempt_timeline"]["winning_model_ref"].is_null());
    assert!(!failure.data["error_chain"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn runtime_failure_artifacts_append_turn_record_after_failure_brief() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(FailingTimelineProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "trigger runtime failure".into(),
        },
    );
    let error = match runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
    {
        Ok(_) => panic!("provider failure should abort the turn"),
        Err(error) => error,
    };
    let terminal = runtime
        .agent_state()
        .await
        .unwrap()
        .last_turn_terminal
        .expect("missing aborted turn terminal");
    assert_eq!(terminal.kind, TurnTerminalKind::Aborted);
    assert_eq!(terminal.no_brief_reason, Some(TurnNoBriefReason::Aborted));

    runtime
        .persist_runtime_failure_artifacts(&message, &error)
        .await
        .unwrap();

    let briefs = runtime.storage().read_recent_briefs(10).unwrap();
    let failure_brief = briefs
        .iter()
        .find(|brief| brief.kind == BriefKind::Failure)
        .expect("missing runtime failure brief");
    assert_eq!(
        failure_brief.turn_id.as_deref(),
        Some(terminal.turn_id.as_str())
    );

    let turns = runtime.storage().read_recent_turns(10).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].turn_id, terminal.turn_id);
    assert_eq!(turns[0].turn_index, terminal.turn_index);
    assert_eq!(
        turns[0].terminal.as_ref().map(|terminal| terminal.kind),
        Some(TurnTerminalKind::Aborted)
    );
    assert_eq!(
        turns[0]
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.no_brief_reason.as_ref()),
        None
    );
    assert_eq!(turns[0].produced_brief_ids, vec![failure_brief.id.clone()]);
}

#[tokio::test]
async fn runtime_failure_artifacts_create_terminal_turn_record_when_missing() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(FailingTimelineProvider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "trigger runtime failure".into(),
        },
    );
    let error = match runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
    {
        Ok(_) => panic!("provider failure should abort the turn"),
        Err(error) => error,
    };
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.last_turn_terminal = None;
        runtime.storage().write_agent(&guard.state).unwrap();
        guard.last_persisted_state = guard.state.clone();
    }

    runtime
        .persist_runtime_failure_artifacts(&message, &error)
        .await
        .unwrap();

    let state = runtime.agent_state().await.unwrap();
    let terminal = state
        .last_turn_terminal
        .as_ref()
        .expect("runtime failure should synthesize an aborted terminal");
    assert_eq!(terminal.kind, TurnTerminalKind::Aborted);
    assert_eq!(terminal.reason.as_deref(), Some("runtime_error"));

    let briefs = runtime.storage().read_recent_briefs(10).unwrap();
    let failure_brief = briefs
        .iter()
        .find(|brief| brief.kind == BriefKind::Failure)
        .expect("missing runtime failure brief");
    assert_eq!(failure_brief.turn_index, Some(terminal.turn_index));
    assert_eq!(
        failure_brief.turn_id.as_deref(),
        Some(terminal.turn_id.as_str())
    );

    let turns = runtime.storage().read_recent_turns(10).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].turn_id, terminal.turn_id);
    assert_eq!(
        turns[0].terminal.as_ref().map(|terminal| terminal.kind),
        Some(TurnTerminalKind::Aborted)
    );
    assert_eq!(turns[0].produced_brief_ids, vec![failure_brief.id.clone()]);
}
