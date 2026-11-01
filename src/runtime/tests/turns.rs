use super::super::*;
use super::support::*;

use crate::tool::ApplyPatchSurface;
use crate::types::{ToolExecutionStatus, WaitConditionKind};

#[tokio::test]
async fn canonical_turn_start_uses_retained_history_high_water() {
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
    runtime
        .inner
        .runtime_db
        .turn_records()
        .upsert(&TurnRecord::new(
            "default",
            "retained-before-recreation",
            22,
        ))
        .unwrap();
    assert_eq!(runtime.agent_state().await.unwrap().turn_index, 0);
    runtime
        .begin_interactive_turn(None, None, None)
        .await
        .unwrap();
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.turn_index, 23);
    let turn = runtime
        .inner
        .storage
        .read_turn_by_id(state.current_turn_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(turn.turn_index, state.turn_index);
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest("default")
            .unwrap(),
        Some(state)
    );
}

#[tokio::test]
async fn canonical_turn_start_failure_keeps_memory_and_state_unchanged() {
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
    runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER reject_turn_start BEFORE INSERT ON turn_records
                 BEGIN SELECT RAISE(ABORT, 'injected turn-start failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();
    assert!(runtime
        .begin_interactive_turn(None, Some("uncommitted-binding"), None)
        .await
        .is_err());
    assert_eq!(runtime.agent_state().await.unwrap(), before);
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .agent_states()
            .latest("default")
            .unwrap(),
        Some(before.clone())
    );
    assert_eq!(
        runtime.inner.agent.lock().await.last_persisted_state,
        before
    );
    let count: i64 = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            Ok(tx.query_row("SELECT COUNT(*) FROM turn_records", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn canonical_turn_start_rejects_non_open_attempts_including_retry() {
    use crate::domain::execution_protocol::{ExecutionAttempt, ExecutionAttemptState};

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
                    text: "admit canonical turn".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::HttpControlPrompt,
                AdmissionContext::ControlAuthenticated,
            ),
        )
        .await
        .unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("expected a canonical admission");
    };
    runtime
        .begin_interactive_turn(Some(&scheduled.message), None, None)
        .await
        .unwrap();
    let before = runtime.agent_state().await.unwrap();
    let turn = runtime
        .storage()
        .read_turn_by_id(before.current_turn_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let activation_id = before
        .current_execution_binding
        .as_ref()
        .unwrap()
        .activation_id
        .as_deref()
        .unwrap();
    let mut next = before.clone();
    next.current_turn_id = Some("rejected-non-open".into());
    next.current_execution_binding.as_mut().unwrap().turn_id = "rejected-non-open".into();
    let mut next_turn = turn.clone();
    next_turn.turn_id = "rejected-non-open".into();

    for lifecycle in [
        ExecutionAttemptState::Interrupted,
        ExecutionAttemptState::Settled,
        ExecutionAttemptState::ProtocolViolation,
    ] {
        // Inject the post-admission lifecycle race directly at the writer fence.
        runtime
            .inner
            .runtime_db
            .transaction(|tx| {
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM execution_protocol_attempts
                     WHERE agent_id = 'default' AND attempt_id = ?1",
                    [activation_id],
                    |row| row.get(0),
                )?;
                let mut attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
                attempt.state = lifecycle;
                let lifecycle = serde_json::to_value(lifecycle)?;
                assert_eq!(
                    tx.execute(
                        "UPDATE execution_protocol_attempts
                         SET lifecycle_state = ?1, payload_json = ?2
                         WHERE agent_id = 'default' AND attempt_id = ?3",
                        rusqlite::params![
                            lifecycle.as_str().unwrap(),
                            serde_json::to_string(&attempt)?,
                            activation_id,
                        ],
                    )?,
                    1
                );
                Ok(())
            })
            .unwrap();
        for (state, record) in [(&next, &next_turn), (&before, &turn)] {
            let error = runtime
                .inner
                .runtime_db
                .turn_records()
                .start(
                    Some(&before),
                    state,
                    record,
                    runtime.inner.identity_incarnation,
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("turn start canonical execution admission is stale or conflicting"),
                "{lifecycle:?}: {error:#}"
            );
        }
        assert_eq!(runtime.agent_state().await.unwrap(), before);
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .agent_states()
                .latest("default")
                .unwrap(),
            Some(before.clone())
        );
        assert_eq!(
            runtime.storage().read_turn_by_id(&turn.turn_id).unwrap(),
            Some(turn.clone())
        );
        assert!(runtime
            .storage()
            .read_turn_by_id(&next_turn.turn_id)
            .unwrap()
            .is_none());
    }
}

fn assert_original_turn_audit_notifications(
    runtime: &RuntimeHandle,
    rx: &mut tokio::sync::broadcast::Receiver<crate::storage::PublishedAuditEvent>,
    expected: &[&str],
) {
    let scope = resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let mut actual = Vec::new();
    let mut last_seq = 0;
    loop {
        let published = match rx.try_recv() {
            Ok(published) => published,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(error) => panic!("turn Audit channel: {error}"),
        };
        if !expected.contains(&published.event.kind.as_str()) {
            continue;
        }
        let original = published.qualified.as_ref().expect("raw turn Audit");
        original
            .validate(&scope, &runtime.inner.input_evidence_budget)
            .unwrap();
        assert!(std::ptr::eq(original.event(), &*published.event));
        assert_eq!(published.agent_id.as_deref(), Some("default"));
        assert!(published.event.event_seq > last_seq);
        last_seq = published.event.event_seq;
        actual.push(published.event.kind.clone());
    }
    assert_eq!(actual, expected);
}

struct PickThenExecProvider {
    calls: Mutex<usize>,
    target_work_item_id: String,
}

struct ImmediateWaitContinuationProvider {
    calls: Mutex<usize>,
    delivery: &'static str,
    disposition: &'static str,
    pending_runtime: Mutex<Option<RuntimeHandle>>,
}

#[async_trait]
impl AgentProvider for ImmediateWaitContinuationProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let pending_runtime = self.pending_runtime.lock().await;
        if *calls == 2 {
            if let Some(runtime) = pending_runtime.as_ref() {
                assert!(request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "CreateWorkItem"));
                assert!(!request.tools.iter().any(|tool| tool.name == "WaitFor"));
                let dependency_runtime = runtime.clone();
                tokio::spawn(async move {
                    complete_immediate_wait_dependency(&dependency_runtime, false).await;
                })
                .await
                .unwrap();
                return Ok(ProviderTurnResponse {
                    material: Default::default(),
                    blocks: vec![ModelBlock::Text {
                        text: "Obsolete waiting report.".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 10,
                    output_tokens: 10,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    request_diagnostics: None,
                });
            }
        }
        let round = *calls - usize::from(pending_runtime.is_some() && *calls > 2);
        let tool = |id: &str, name: &str, input| ModelBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        };
        let blocks = match round {
            1 => vec![tool(
                "immediate-wait",
                "WaitFor",
                serde_json::json!({
                    "wake": "task_result",
                    "resource": "task-immediate-continuation",
                    "delivery": self.delivery,
                    "reason": "continue if this result cannot be claimed",
                }),
            )],
            2 => {
                if pending_runtime.is_some() {
                    assert!(request.conversation.iter().any(|message| matches!(
                        message,
                        ConversationMessage::UserText(text)
                            if text.contains("\"continuation\":\"continue_turn\"")
                                && text.contains(self.disposition)
                                && text.contains("no waiting report")
                    )));
                } else {
                    let receipt = request
                        .conversation
                        .iter()
                        .filter_map(|message| match message {
                            ConversationMessage::UserToolResults(results) => Some(results),
                            _ => None,
                        })
                        .flatten()
                        .find(|result| result.tool_use_id == "immediate-wait")
                        .expect("WaitFor receipt must reach the next model round");
                    assert!(!receipt.is_error, "{}", receipt.content);
                    let receipt: serde_json::Value =
                        serde_json::from_str(&receipt.content).unwrap();
                    assert_eq!(receipt["result"]["continuation"], "continue_turn");
                    assert_eq!(receipt["result"]["disposition"], self.disposition);
                }
                // Revalidation restores lifecycle tools when no report remains pending.
                assert!(request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "CreateWorkItem"));
                assert!(request.tools.iter().any(|tool| tool.name == "WaitFor"));
                vec![tool(
                    "continue-create",
                    "CreateWorkItem",
                    serde_json::json!({"objective": "progress after immediate wait"}),
                )]
            }
            3 => {
                let result = request
                    .conversation
                    .iter()
                    .filter_map(|message| match message {
                        ConversationMessage::UserToolResults(results) => Some(results),
                        _ => None,
                    })
                    .flatten()
                    .find(|result| result.tool_use_id == "continue-create")
                    .expect("the next mutating tool must execute in the same turn");
                assert!(!result.is_error, "{}", result.content);
                vec![ModelBlock::Text {
                    text: "Continued in the same turn.".into(),
                }]
            }
            _ => panic!(
                "immediate wait must not request an extra final report: {:?}",
                request.conversation
            ),
        };
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
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

async fn run_immediate_wait_continuation(delivery: &'static str, claimed: bool, pending: bool) {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(ImmediateWaitContinuationProvider {
        calls: Mutex::new(0),
        delivery,
        disposition: if claimed {
            "task_result_claimed_by_other_waiter"
        } else {
            "task_result_settled"
        },
        pending_runtime: Mutex::new(None),
    });
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens: 128_000,
            turn_projection_min_budget: 128_000,
            turn_projection_max_budget: 128_000,
            compaction_trigger_estimated_tokens: 128_000,
            ..context_config()
        },
    )
    .unwrap();
    // Claim the original provider invocation before seeding the dependency's
    // independent queue obligation; the scheduler must not consume that result.
    let source = runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue after immediate wait".into(),
            },
        ))
        .await
        .unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("immediate wait source must be claimed");
    };
    assert_eq!(scheduled.message.id, source.id);
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await
        .unwrap();
    let task_id = "task-immediate-continuation";
    mark_blocking_task(&runtime, task_id).await;
    if pending {
        *provider.pending_runtime.lock().await = Some(runtime.clone());
    } else {
        complete_immediate_wait_dependency(&runtime, claimed).await;
    }
    let waits_before = runtime
        .storage()
        .latest_wait_conditions_for_agent("default")
        .unwrap();
    let dependency_queue = |queue: Vec<QueueEntryRecord>| {
        queue
            .into_iter()
            .filter(|entry| entry.message_id != source.id)
            .collect::<Vec<_>>()
    };
    let queue_before = dependency_queue(runtime.storage().read_recent_queue_entries(100).unwrap());
    let processing_runtime = runtime.clone();
    let outcome = tokio::spawn(async move {
        processing_runtime
            .run_agent_loop_with_fixture_dispatch(
                "default",
                AuthorityClass::OperatorInstruction,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
                Some(scheduled),
            )
            .await
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(*provider.calls.lock().await, if pending { 4 } else { 3 });
    assert!(outcome.prepared_wait_for.is_none());
    assert_eq!(outcome.final_text, "Continued in the same turn.");
    assert_eq!(
        runtime
            .storage()
            .latest_wait_conditions_for_agent("default")
            .unwrap(),
        waits_before
    );
    if !pending {
        assert_eq!(
            dependency_queue(runtime.storage().read_recent_queue_entries(100).unwrap()),
            queue_before
        );
    }
    let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
    assert_eq!(tools.len(), 2);
    // Both tools finish in the original actual dispatch before its terminal ACK.
    assert!(tools
        .iter()
        .all(|tool| tool.status == ToolExecutionStatus::Success));
    // Break the provider/runtime cycle used by the report-race fixture.
    *provider.pending_runtime.lock().await = None;
}

async fn commit_wait_dependency(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
    parent_turn_id: &str,
) {
    let task_id = message.task_id.as_deref().unwrap();
    let mut task = runtime.task_record(task_id).await.unwrap().unwrap();
    assert_eq!(task.work_item_id, message.work_item_id);
    task.status = TaskStatus::Completed;
    task.parent_message_id = Some(message.id.clone());
    task.updated_at = runtime.now();
    task.detail = Some(serde_json::json!({
        "wait_policy": "blocking",
        "rejoin_obligation_id": task_id,
        "rejoin_generation": 1,
        "parent_turn_id": parent_turn_id,
    }));
    runtime
        .commit_terminal_task_result(&task, "task_completed", message)
        .await
        .unwrap();
    let settlement = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&message.id)
        .unwrap()
        .expect("terminal dependency must have a durable result obligation");
    assert_eq!(settlement.task_id, task.id);
    assert_eq!(settlement.work_item_id, task.work_item_id);
    assert_eq!(settlement.rejoin_generation, 1);
    assert_eq!(settlement.source.parent_turn_id().unwrap(), parent_turn_id);
}

async fn complete_immediate_wait_dependency(runtime: &RuntimeHandle, claimed: bool) {
    let task_id = "task-immediate-continuation";
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "completed dependency".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.task_id = Some(task_id.into());
    commit_wait_dependency(runtime, &message, "turn-immediate-wait-parent").await;
    if claimed {
        let claimant = runtime
            .create_work_item("original waiter".into(), None, None, Vec::new())
            .await
            .unwrap();
        let outcome = runtime
            .register_wait_for_outcome(
                "default",
                Some(claimant.id),
                WaitForWakeKind::TaskResult,
                Some(task_id.into()),
                "original dependency".into(),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            WaitForRegistrationOutcome::TaskResultQueued { .. }
        ));
    } else {
        assert!(runtime
            .inner
            .agent
            .lock()
            .await
            .queue
            .peek_next_matching(|entry| entry.id == message.id)
            .is_some());
        let admitted = runtime
            .apply_task_result_operation(&message.id, || {
                crate::runtime_db::transitions::TaskResultOperation::AdmitMessage {
                    message_id: message.id.clone(),
                    activation_id: "attempt-immediate-delivered".into(),
                    projection: None,
                    at: runtime.now(),
                }
            })
            .await
            .unwrap();
        assert!(admitted.record().unwrap().is_some());
        drop(admitted);
        let commit = {
            use crate::runtime_db::transitions::{
                PreparedInputTransition, PreparedQueueTransition, QueueMutation, QueueOperation,
                QueueTransitionCommand,
            };
            let mut mutation = runtime.begin_mutation().await.unwrap();
            let at = runtime.now();
            let mut queue = PreparedQueueTransition::new(QueueTransitionCommand {
                agent_id: "default".into(),
                operation: QueueOperation::Settle,
                mutation: QueueMutation::Upsert(QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: "default".into(),
                    priority: message.priority.clone(),
                    status: QueueEntryStatus::Processed,
                    created_at: message.created_at,
                    updated_at: at,
                }),
                scheduler_claim_work_item: None,
                agent_state: None,
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: Vec::new(),
                notify_scheduler: false,
                fault: None,
                brief_evidence: Vec::new(),
            });
            queue.task_result_settlement = Some(
                crate::runtime_db::task_result_settlement::TaskResultActivationSettlement {
                    activation_id: "attempt-immediate-delivered".into(),
                    disposition: crate::runtime_db::TaskResultSettlementDisposition::ModelDelivered,
                    settled_at: at,
                },
            );
            let prepared = PreparedInputTransition::queue(
                mutation.input_evidence_scope(),
                crate::ids::audit_event_id(),
                at,
                queue,
            )
            .unwrap();
            mutation
                .commit_prepared_input_transition(prepared)
                .await
                .unwrap()
        };
        runtime.apply_transition_commit(commit).await;
        assert!(!runtime
            .inner
            .agent
            .lock()
            .await
            .queue
            .peek_next_matching(|entry| entry.id == message.id)
            .is_some());
        let settled = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&message.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            settled.disposition,
            Some(crate::runtime_db::TaskResultSettlementDisposition::ModelDelivered)
        );
        assert_eq!(
            settled.activation_id.as_deref(),
            Some("attempt-immediate-delivered")
        );
        assert_eq!(
            settled.state,
            crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
        );
        let count: usize = runtime
            .inner
            .runtime_db
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM task_result_settlements WHERE agent_id = ?1 AND activation_id = ?2 AND state = 'settled'",
                rusqlite::params!["default", "attempt-immediate-delivered"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[tokio::test]
async fn wait_for_consumed_silent_continues_same_turn() {
    run_immediate_wait_continuation("silent", false, false).await;
}

#[tokio::test]
async fn wait_for_consumed_final_continues_same_turn() {
    run_immediate_wait_continuation("final", false, false).await;
}

#[tokio::test]
async fn wait_for_claimed_silent_continues_same_turn() {
    run_immediate_wait_continuation("silent", true, false).await;
}

#[tokio::test]
async fn wait_for_claimed_final_continues_same_turn() {
    run_immediate_wait_continuation("final", true, false).await;
}

#[tokio::test]
async fn wait_for_consumed_during_final_report_continues_same_turn() {
    run_immediate_wait_continuation("final", false, true).await;
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
    let message = runtime.enqueue(message).await.unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("exact evidence source was not claimed");
    };
    assert_eq!(scheduled.message.id, message.id);
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .agent_state()
            .await
            .unwrap()
            .current_turn_id
            .as_deref(),
        Some(turn_id)
    );

    let mut target_brief =
        BriefRecord::new("default", BriefKind::Result, "target brief", None, None);
    target_brief.id = "brief-exact-target".into();
    target_brief.turn_id = Some(turn_id.into());
    runtime.persist_brief(&target_brief).await.unwrap();

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
        .persist_tool_execution_evidence(&target_tool)
        .await
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
    {
        let mut mutation = runtime.begin_mutation().await.unwrap();
        mutation
            .commit_wait_transition(
                crate::ids::audit_event_id(),
                crate::runtime_db::transitions::WaitTransitionCommand {
                    agent_id: "default".into(),
                    work_items: Vec::new(),
                    expected_wait_conditions: Vec::new(),
                    wait_conditions: vec![target_wait.clone()],
                    timer_wake: None,
                    task_result_admission: None,
                    agent_state: None,
                    audit_events: Vec::new(),
                    index_changes: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                },
                Default::default(),
                None,
            )
            .await
            .unwrap();
    }

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
                    status: WaitConditionStatus::Cancelled,
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
                    cancelled_at: Some(noise.created_at),
                    turn_id: Some(noise_turn_id),
                    trigger_message_id: None,
                    triggered_at: None,
                };
                crate::runtime_db::repositories::upsert_wait_condition_tx(tx, &noise_wait)?;
            }
            Ok(())
        })
        .unwrap();

    // Only the actual Turn relation is explicitly prepared. Unrelated raw
    // historical artifacts remain outside the qualified resident domain.
    let mut request = resident_work::WorkCaptureRequest::current();
    request.include_live = false;
    request.nonopen_limit = 0;
    request.required_turn_ids.insert(turn_id.into());
    let mut mutation = runtime.begin_mutation().await.unwrap();
    mutation.prepare_owner_work_request(&request).await.unwrap();
    let selected = mutation.capture_work(&request).unwrap();
    assert_eq!(selected.records.len(), 1);
    assert_eq!(
        selected.waits_for_turn(turn_id).unwrap()[0].id,
        target_wait.id
    );
    drop(selected);
    drop(mutation);

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
    drop(scheduled);
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
        turn_record: turn::inputs::BuiltTurnRecord::fixture(turn_record),
        prepared_work_item_completion: None,
        prepared_wait_for: None,
        provider_recovery_message: None,
        terminal_tool_executions: Vec::new(),
    }
}

#[tokio::test]
async fn terminal_settlement_accepts_exactly_one_brief_or_typed_no_brief_reason() {
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
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn terminal_settlement_rejects_missing_or_ambiguous_settlement_with_diagnostic() {
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
            .await
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
    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        _ => panic!("atomic wait source must be claimed"),
    };
    assert_eq!(scheduled.message.id, message.id);
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
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
    let mut task_result = MessageEnvelope::new(
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
    task_result.task_id = Some("task-atomic-wait".into());
    let task_ready = matches!(scenario, AtomicWaitScenario::TaskReady);
    let task_wait = matches!(
        scenario,
        AtomicWaitScenario::TaskReady | AtomicWaitScenario::TaskCompletes
    );
    if task_wait {
        mark_blocking_task(&runtime, "task-atomic-wait").await;
    }
    if task_ready {
        commit_wait_dependency(&runtime, &task_result, "turn-atomic-wait-parent").await;
    }
    let task_queue_before_preparation = runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest(&task_result.id)
        .unwrap();
    let task_settlement_before_preparation = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&task_result.id)
        .unwrap();
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
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&task_result.id)
            .unwrap(),
        task_queue_before_preparation
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&task_result.id)
            .unwrap(),
        task_settlement_before_preparation
    );
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
        provider_recovery_message: None,
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
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&task_result.id)
            .unwrap(),
        task_queue_before_preparation
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&task_result.id)
            .unwrap(),
        task_settlement_before_preparation
    );

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
        commit_wait_dependency(&runtime, &task_result, "turn-atomic-wait-parent").await;
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
            material: Default::default(),
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
                    "anthropic_cache": {
                        "explicit_cache_control_enabled": false,
                        "cache_control_source": "route_override",
                        "model_route": "dashscope@token-plan/deepseek-v4.1-flash",
                        "tools_count": 0,
                        "tools_hash": "tools-fingerprint",
                        "system_hash": "system-fingerprint",
                        "system_block_count": 1,
                        "estimated_system_tokens": 100,
                        "context_hash_by_stability": {},
                        "conversation_message_count": 1,
                        "conversation_content_block_count": 1,
                        "cache_breakpoints": [],
                        "tokens_before_last_breakpoint": 0,
                        "tokens_after_last_breakpoint": 110,
                        "automatic_cache_control_requested": false
                    },
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
    let mut source = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "pick the next work item".into(),
        },
    );
    source.work_item_id = Some(caller.id.clone());
    source.turn_id = Some("turn-terminal-pick".into());
    let source = runtime.enqueue(source).await.unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("terminal pick source must be claimed");
    };
    assert_eq!(scheduled.message.id, source.id);
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await
        .unwrap();
    let provider = Arc::new(PickThenExecProvider {
        calls: Mutex::new(0),
        target_work_item_id: target.id.clone(),
    });
    *runtime.inner.provider.write().await = provider.clone();

    let outcome = runtime
        .run_agent_loop_with_fixture_dispatch(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
            Some(scheduled),
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
            material: Default::default(),
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
    let bus = crate::storage::EventBus::new(128);
    runtime.storage().enable_event_bus(bus.clone()).unwrap();
    let mut notifications = bus.subscribe();

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
    assert_original_turn_audit_notifications(
        &runtime,
        &mut notifications,
        &["max_output_tokens_recovery"],
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
    let bus = crate::storage::EventBus::new(128);
    runtime.storage().enable_event_bus(bus.clone()).unwrap();
    let mut notifications = bus.subscribe();

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
    assert_original_turn_audit_notifications(
        &runtime,
        &mut notifications,
        &[
            "lineage_selected",
            "provider_round_completed",
            "assistant_round_recorded",
            "text_only_round_observed",
        ],
    );
}

#[tokio::test]
async fn independent_audit_provider_round_failure_keeps_prior_state_ack() -> Result<()> {
    let dir = tempdir()?;
    let workspace = tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new(
            "The round must not pass the rejected Audit.",
        )),
        "default".into(),
        context_config(),
    )?;
    runtime.await_initial_resources().await;
    let bus = crate::storage::EventBus::new(128);
    runtime.storage().enable_event_bus(bus.clone())?;
    let mut notifications = bus.subscribe();
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_provider_round_audit BEFORE INSERT ON audit_events
             WHEN NEW.kind = 'provider_round_completed'
             BEGIN SELECT RAISE(ABORT,'provider round Audit rejected'); END;",
        )?;
        Ok(())
    })?;
    let error = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .err()
        .expect("provider Audit rejection");
    assert!(format!("{error:#}").contains("provider round Audit rejected"));
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state.total_model_rounds, 1);
    assert_eq!(runtime.agent_state().await?, *root.state);
    let events = runtime.storage().read_recent_events(128)?;
    assert!(!events.iter().any(|event| matches!(
        event.kind.as_str(),
        "provider_round_completed" | "assistant_round_recorded" | "text_only_round_observed"
    )));
    assert_original_turn_audit_notifications(&runtime, &mut notifications, &["lineage_selected"]);
    Ok(())
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
    let transcript = runtime.storage().read_recent_transcript(10).unwrap();
    let assistant_round = transcript
        .iter()
        .find(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
        .expect("missing assistant round transcript");
    for data in [&provider_event.data, &assistant_round.data] {
        let cache = &data["provider_request_diagnostics"]["anthropic_cache"];
        assert_eq!(cache["explicit_cache_control_enabled"], false);
        assert_eq!(cache["cache_control_source"], "route_override");
        assert_eq!(
            cache["model_route"],
            "dashscope@token-plan/deepseek-v4.1-flash"
        );
        assert_eq!(cache["system_cache_control_count"], 0);
        assert_eq!(cache["message_cache_control_count"], 0);
        let serialized = serde_json::to_string(data).unwrap();
        assert!(!serialized.contains("system_prompt"));
        // Count-only diagnostics are safe; raw conversation payloads are not.
        assert!(!serialized.contains("\"conversation\""));
        assert!(!serialized.contains("tool_arguments"));
    }
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
    run_wait_for_final_report_test_with_compaction(
        scenario,
        silent_progress,
        work_item_owned,
        None,
    )
    .await;
}

async fn run_wait_for_final_report_test_with_compaction(
    scenario: WaitForFinalReportScenario,
    silent_progress: Option<bool>,
    work_item_owned: bool,
    checkpoint_output: Option<ReportCheckpointOutput>,
) {
    let force_compaction = checkpoint_output.is_some();
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(WaitForFinalReportProvider {
        calls: Mutex::new(0),
        scenario,
        silent_progress,
        report_path: workspace.path().join("wait-report-cleanup.txt"),
        report_runtime: Mutex::new(None),
        saw_settlement_error_follow_up: Mutex::new(false),
        settlement_result_count: Mutex::new(0),
        settlement_timer: Mutex::new(None),
    });
    let compaction_provider = Arc::new(ReportCompactionProvider {
        inner: provider.clone(),
        checkpoint_output: checkpoint_output.unwrap_or(ReportCheckpointOutput::Text),
        checkpoint_calls: Mutex::new(0),
        response_calls: Mutex::new(0),
    });
    let runtime_provider: Arc<dyn AgentProvider> = if force_compaction {
        compaction_provider.clone()
    } else {
        provider.clone()
    };
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        runtime_provider,
        "default".into(),
        if force_compaction {
            report_compaction_context_config(&workspace)
        } else {
            ContextConfig {
                prompt_budget_estimated_tokens: 65536,
                compaction_trigger_estimated_tokens: 65536,
                turn_projection_budget_ratio: 1.0,
                compaction_keep_recent_estimated_tokens: 2048,
                ..context_config()
            }
        },
    )
    .unwrap();
    runtime
        .update_agent_state(|state| {
            state.active_workspace_entry = None;
            Ok(())
        })
        .await
        .unwrap();
    *provider.report_runtime.lock().await = Some(runtime.clone());
    if scenario == WaitForFinalReportScenario::SettlementErrorRecovery {
        let timer = runtime
            .schedule_timer(60_000, None, Some("final settlement race".into()))
            .await
            .unwrap();
        *provider.settlement_timer.lock().await = Some((runtime.clone(), timer.id));
    }
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
    *provider.report_runtime.lock().await = None;
    if force_compaction {
        assert!(
            *compaction_provider.checkpoint_calls.lock().await > 0,
            "no private checkpoint after {} ordinary responses; compactions: {:?}",
            *compaction_provider.response_calls.lock().await,
            runtime
                .storage()
                .read_recent_events(200)
                .unwrap()
                .into_iter()
                .filter(|event| event.kind == "turn_local_compaction_applied")
                .map(|event| (
                    event.data["round"].clone(),
                    event.data["checkpoint_request_id"].clone(),
                    event.data["projected_estimated_tokens"].clone(),
                    event.data["effective_budget_estimated_tokens"].clone(),
                ))
                .collect::<Vec<_>>()
        );
        let events = runtime.storage().read_recent_events(200).unwrap();
        if checkpoint_output != Some(ReportCheckpointOutput::Text) {
            assert!(events.iter().any(|event| {
                event.kind == "turn_local_checkpoint_recorded"
                    && event.data["checkpoint_recorded"] == false
                    && event.data["text_char_count"] == 0
            }));
        }
        assert!(events.iter().any(|event| {
            event.kind == "assistant_round_recorded"
                && event.data["round_purpose"] == "runtime_checkpoint"
                && event.data["visibility"] == "runtime_private"
        }));
        assert!(events
            .iter()
            .any(|event| event.kind == "turn_local_checkpoint_resume_requested"));
    }

    if scenario == WaitForFinalReportScenario::SettlementErrorRecovery {
        assert_eq!(provider.call_count().await, 3);
        assert!(*provider.saw_settlement_error_follow_up.lock().await);
        assert_eq!(
            *provider.settlement_result_count.lock().await,
            1,
            "only the original deferred result may remain; final settlement errors must not inject a second tool result"
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
            None if scenario == WaitForFinalReportScenario::NestedLifecycleCorrective => 3,
            None if scenario == WaitForFinalReportScenario::CleanupTools => 7,
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
    let mut turns = runtime.storage().read_recent_turns(10).unwrap();
    assert_eq!(
        turns.len(),
        1,
        "the wait must have exactly one terminal turn"
    );
    let turn = turns.pop().expect("final WaitFor terminal turn");
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
    assert_eq!(
        tools
            .iter()
            .filter(|tool| tool.tool_name == "WaitFor")
            .count(),
        1,
        "the report must commit exactly one wait tool result"
    );
    assert_eq!(
        briefs
            .iter()
            .filter(|brief| brief.kind == BriefKind::Result)
            .count(),
        1,
        "text accompanying cleanup tools must not publish an early result brief"
    );
    match scenario {
        WaitForFinalReportScenario::NestedLifecycleCorrective => {
            assert_eq!(tools.len(), 1, "recursive lifecycle tools must not execute");
            let waits = runtime
                .storage()
                .active_wait_conditions_for_agent("default")
                .unwrap();
            assert_eq!(waits.len(), 1);
            assert_eq!(waits[0].kind, WaitConditionKind::External);
        }
        WaitForFinalReportScenario::CleanupTools => {
            assert_eq!(
                tools
                    .iter()
                    .filter(|tool| tool.tool_name == "ExecCommand")
                    .count(),
                5
            );
            assert_eq!(
                tools
                    .iter()
                    .filter(|tool| tool.tool_name == "ApplyPatch")
                    .count(),
                1
            );
            assert!(tools
                .iter()
                .all(|tool| tool.status == ToolExecutionStatus::Success));
            assert!(!provider.report_path.exists());
            let events = runtime.storage().read_recent_events(200).unwrap();
            assert!(events
                .iter()
                .all(|event| event.kind != "report_fallback_notice_injected"));
        }
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
async fn wait_for_final_report_rejects_nested_lifecycle_tools_without_executing_them() {
    run_wait_for_final_report_test(
        WaitForFinalReportScenario::NestedLifecycleCorrective,
        None,
        false,
    )
    .await;
}

#[tokio::test]
async fn wait_for_final_report_allows_five_cleanup_tool_rounds_before_final_text() {
    run_wait_for_final_report_test(WaitForFinalReportScenario::CleanupTools, None, false).await;
}

#[tokio::test]
async fn wait_for_final_report_survives_private_compaction_before_final_text() {
    run_wait_for_final_report_test_with_compaction(
        WaitForFinalReportScenario::CleanupTools,
        None,
        false,
        Some(ReportCheckpointOutput::Text),
    )
    .await;
}

#[tokio::test]
async fn wait_for_final_report_survives_empty_private_compaction() {
    run_wait_for_final_report_test_with_compaction(
        WaitForFinalReportScenario::CleanupTools,
        None,
        false,
        Some(ReportCheckpointOutput::Empty),
    )
    .await;
}

#[tokio::test]
async fn wait_for_final_report_survives_reasoning_only_private_compaction() {
    run_wait_for_final_report_test_with_compaction(
        WaitForFinalReportScenario::CleanupTools,
        None,
        false,
        Some(ReportCheckpointOutput::ReasoningOnly),
    )
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
                material: Default::default(),
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
            material: Default::default(),
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
            compaction_keep_recent_estimated_tokens: 2048,
            ..continuation_ready_context_config(&workspace, 32768)
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

    assert_eq!(*provider.calls.lock().await, 3);
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
    let bus = crate::storage::EventBus::new(128);
    runtime.storage().enable_event_bus(bus.clone()).unwrap();
    let mut notifications = bus.subscribe();

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
    assert_original_turn_audit_notifications(
        &runtime,
        &mut notifications,
        &["tool_execution_failed"],
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

// History fixtures use the original owner and transaction, including actual
// source sequence, canonical assignment, State projection and input publication.
fn publish_committed_history_fixture<'a>(
    runtime: &'a RuntimeHandle,
    turn_id: &'a str,
    index: u64,
    owner: Option<&'a str>,
    text: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        use crate::runtime::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key,
        };
        use crate::runtime_db::{
            evidence::{append_message_tx, upsert_agent_state_tx},
            input_evidence_snapshot::{
                register_committed_input_evidence_tx, validate_existing_input_evidence_tx,
                CanonicalInputEvidence as Fact,
            },
            repositories::{committed_turn_record_tx, upsert_turn_record_tx},
            transitions::{AgentStateMutation, PostCommitEffects, TransitionCommit},
        };
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text },
        );
        message.turn_id = Some(turn_id.into());
        message.work_item_id = owner.map(str::to_string);
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let expected = mutation.state.clone();
        let committed = mutation
            .commit_transition(crate::ids::audit_event_id(), |scope| {
                runtime.inner.runtime_db.transaction(|tx| {
                    validate_existing_input_evidence_tx(
                        tx,
                        scope,
                        &Key::new(Family::Message, &message.id),
                    )?;
                    validate_existing_input_evidence_tx(
                        tx,
                        scope,
                        &Key::new(Family::Turn, turn_id),
                    )?;
                    let actual_message = append_message_tx(tx, &message)?.0;
                    let mut turn = TurnRecord::new("default", turn_id, index);
                    turn.owner = owner.map(|id| crate::types::TurnOwner::WorkItem {
                        work_item_id: id.into(),
                    });
                    turn.current_work_item_id = owner.map(str::to_string);
                    turn.input_message_ids = vec![actual_message.id.clone()];
                    turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
                        &actual_message,
                    ));
                    upsert_turn_record_tx(tx, &turn)?;
                    let actual_turn =
                        committed_turn_record_tx(tx, &turn.agent_id, &turn.turn_id)?.record;
                    let payload: String = tx.query_row(
                        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                        [&turn.agent_id],
                        |row| row.get(0),
                    )?;
                    let mut state: AgentState = serde_json::from_str(&payload)?;
                    anyhow::ensure!(
                        state == expected,
                        "history fixture State changed outside original owner"
                    );
                    state.turn_index = state.turn_index.max(actual_turn.turn_index);
                    let message_count: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
                        [&turn.agent_id],
                        |row| row.get(0),
                    )?;
                    state.total_message_count = usize::try_from(message_count)?;
                    upsert_agent_state_tx(tx, &state)?;
                    let payload: String = tx.query_row(
                        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                        [&turn.agent_id],
                        |row| row.get(0),
                    )?;
                    let actual_state: AgentState = serde_json::from_str(&payload)?;
                    let input_evidence_delta = register_committed_input_evidence_tx(
                        tx,
                        scope,
                        &[
                            Fact::Message(actual_message.clone()),
                            Fact::Turn(actual_turn.clone()),
                        ],
                    )?;
                    Ok(TransitionCommit {
                        applied: true,
                        effects: Box::new(PostCommitEffects {
                            agent_state: Some(AgentStateMutation {
                                expected: Some(Box::new(expected.clone())),
                                record: Box::new(actual_state),
                            }),
                            input_evidence_delta,
                            messages: vec![actual_message],
                            turns: vec![actual_turn],
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                })
            })
            .await
            .unwrap();
        drop(mutation);
        runtime.apply_transition_commit(committed).await;
    })
}

#[tokio::test]
async fn prepared_scoped_prompt_and_budget_retry_survive_storage_removal() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        ContextConfig {
            recent_messages: 1,
            prompt_budget_estimated_tokens: 128_000,
            turn_projection_budget_ratio: 1.0,
            turn_projection_min_budget: 0,
            turn_projection_max_budget: 64_000,
            ..context_config()
        },
    )
    .unwrap();
    let work_item = runtime
        .create_work_item(
            "keep owner history through retries".into(),
            Some(crate::types::WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(work_item.id.clone()).await.unwrap();
    publish_committed_history_fixture(
        &runtime,
        "turn-owner-snapshot",
        1,
        Some(&work_item.id),
        format!("OWNER_SNAPSHOT_INPUT {}", "owner-evidence ".repeat(1000)),
    )
    .await;
    publish_committed_history_fixture(
        &runtime,
        "turn-unrelated-snapshot",
        2,
        Some("work-unrelated"),
        "UNRELATED_RECENT_INPUT".into(),
    )
    .await;
    // Scoped history requires the actual execution owner, not only UI focus.
    // Keep the real current source out of both prepared history windows.
    let mut source = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "continue the owner task".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    );
    source.work_item_id = Some(work_item.id.clone());
    let source = runtime.enqueue(source).await.unwrap();
    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        _ => panic!("scoped history source must be claimed"),
    };
    assert_eq!(scheduled.message.id, source.id);
    assert!(scheduled.scheduler_decision.model_reentry);
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await
        .unwrap();
    // This direct capture bypasses the operator dispatcher's preparation phase.
    runtime.ensure_context_resources().await.unwrap();
    let original = runtime
        .build_loaded_context_prompt(&scheduled.message, None, None, true)
        .await
        .unwrap();
    assert!(matches!(
        &original.projection_owner,
        crate::projection_eval::ProjectionOwner::WorkItem { work_item_id }
            if work_item_id == &work_item.id
    ));
    assert!(
        original.context_sections.iter().any(|section| {
            section.name == "current_work_item" && section.content.contains(&work_item.id)
        }),
        "prepared prompt must display its actual execution WorkItem"
    );
    let prepared = original.clone();
    let window = prepared.history_policy().work_item_scoped_window.unwrap();
    let prepared_id = prepared.context_snapshot_id();
    assert_eq!(prepared_id, original.context_snapshot_id());
    publish_committed_history_fixture(
        &runtime,
        "turn-late-snapshot",
        4,
        Some(&work_item.id),
        "LATE_STORAGE_INPUT".into(),
    )
    .await;
    drop(runtime);
    drop(dir);
    drop(workspace);

    let outcome = prepared
        .reproject_for_history_selector(
            64_000,
            &[],
            crate::projection_eval::HistorySelector::WorkItemScoped,
            window,
        )
        .unwrap();
    let prompt = match outcome {
        crate::prompt::HistoryReprojectionOutcome::Adopted {
            prompt,
            turns_loaded,
        }
        | crate::prompt::HistoryReprojectionOutcome::IdenticalRenderNoop {
            prompt,
            turns_loaded,
        } => {
            assert_eq!(turns_loaded, 1);
            prompt
        }
        other => panic!("prepared owner history should render: {other:?}"),
    };
    let history = |prompt: &crate::prompt::EffectivePrompt| {
        prompt
            .context_sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .unwrap()
            .content
            .clone()
    };
    assert_eq!(prompt.context_snapshot_id(), prepared_id);
    assert!(history(&prompt).contains("OWNER_SNAPSHOT_INPUT"));
    assert!(!history(&prompt).contains("UNRELATED_RECENT_INPUT"));
    assert!(!history(&prompt).contains("LATE_STORAGE_INPUT"));
    let reduced = prompt
        .reproject_recent_turns(1_000, &[])
        .expect("fixture must contain reducible owner history");
    assert_eq!(reduced.context_snapshot_id(), prepared_id);
    assert!(history(&reduced).contains("turn-owner-snapshot"));
    assert!(!history(&reduced).contains("UNRELATED_RECENT_INPUT"));
    assert!(!history(&reduced).contains("LATE_STORAGE_INPUT"));
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

    let (_, available_tools, _, _, _) = runtime.provider_tool_selection().await.unwrap();
    publish_committed_history_fixture(
        &runtime,
        "turn-large-history",
        1,
        None,
        format!(
            "historical context {}",
            "large-history-token ".repeat(12_000)
        ),
    )
    .await;

    let prompt = runtime
        .preview_prompt(
            "continue after the large historical turn".into(),
            AuthorityClass::OperatorInstruction,
        )
        .await
        .unwrap();
    let reduced_prompt = prompt
        .reproject_recent_turns(0, &available_tools)
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

    // Recovery only retries when the initial recent-turns projection includes
    // the historical turn but the reduced retry budget (initial minus
    // max(initial / 4, 1024) tokens) drops it. context_config() derives that
    // budget from live tool descriptions, so scale the fixture to ~7/8 of it:
    // inside the window with ~1/8-budget margins against description drift.
    let historical_repeats =
        public_tools_prompt_budget_estimated_tokens() * 7 / 8 * 4 / "historical context".len();
    publish_committed_history_fixture(
        &runtime,
        "turn-context-recovery-history",
        1,
        None,
        "historical context".repeat(historical_repeats),
    )
    .await;

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
        .is_some_and(|error| error.contains("current provider failed")));
    assert!(!deferred.data["error"]
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

    let outcome = outcome.expect("terminal outcome after recovery budget exhaustion");
    assert_eq!(outcome.terminal_kind, TurnTerminalKind::Aborted);
    assert!(outcome
        .final_text
        .starts_with("Provider recovery budget exhausted"));
    assert!(!outcome.final_text.contains("current provider failed"));
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

#[tokio::test(start_paused = true)]
async fn single_candidate_quota_failures_park_without_recovery_enqueue() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("unused")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let work = runtime
        .create_work_item(
            "single candidate quota recovery".into(),
            None,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some("activation-quota-park".into()),
            admission_provenance: None,
            source_message_id: "message-quota-park".into(),
            turn_id: "turn-quota-park".into(),
            owner: None,
            work_item_id: Some(work.id.clone()),
            claimed_work_revision: Some(work.revision),
        });
        guard.persist_state(&runtime.inner.storage).unwrap();
    }
    let error = crate::provider::provider_turn_error(
        "all configured providers failed for this turn: test/model: retries_exhausted (rate_limited)",
        crate::provider::ProviderAttemptTimeline {
            attempts: vec![crate::provider::ProviderAttemptRecord {
                provider: "test".into(),
                model_ref: "test/model".into(),
                attempt: 1,
                max_attempts: 1,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                failure_kind: Some("rate_limited".into()),
                disposition: Some("deferred".into()),
                outcome: crate::provider::ProviderAttemptOutcome::RetriesExhausted,
                advanced_to_fallback: false,
                backoff_ms: None,
                backoff_source: None,
                token_usage: None,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                provider_http_trace_id: None,
                transport_diagnostics: None,
                transport_timeline: None,
            }],
            requested_model_ref: "test/model".into(),
            active_model_ref: Some("test/model".into()),
            winning_model_ref: None,
            pending_fallback_model_ref: None,
            pending_fallback_disposition: None,
            aggregated_token_usage: None,
        },
        anyhow::anyhow!("quota exhausted"),
    );

    for round in 1..=2 {
        assert!(runtime
            .maybe_defer_provider_lineage_failure(
                "default", round, &error, None, None, 10, false, false
            )
            .await
            .unwrap()
            .is_none());
    }
    let outcome = runtime
        .maybe_defer_provider_lineage_failure("default", 3, &error, None, None, 10, false, false)
        .await
        .unwrap()
        .expect("third quota failure should park the turn");

    assert_eq!(outcome.terminal_kind, TurnTerminalKind::DeferredToFallback);
    assert!(runtime.inner.agent.lock().await.queue.is_empty());
    let blocked = runtime
        .storage()
        .latest_work_item(&work.id)
        .unwrap()
        .expect("parked WorkItem");
    assert!(blocked
        .blocked_by
        .as_deref()
        .is_some_and(|reason| reason.contains("provider quota incident")));
    assert!(blocked.recheck_at.is_some());
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "provider_quota_exhausted" && event.data["parked"].as_bool() == Some(true)
    }));
    assert!(!events.iter().any(|event| event.kind == "recovery_enqueued"));
}

#[tokio::test(start_paused = true)]
async fn parked_quota_work_item_probes_after_deadline_and_resolves_on_success() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("probe succeeded")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let work = runtime
        .create_work_item("quota probe recovery".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    {
        let mut guard = runtime.begin_mutation().await.unwrap();
        guard.state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some("activation-quota-probe".into()),
            admission_provenance: None,
            source_message_id: "message-quota-probe".into(),
            turn_id: "turn-quota-probe".into(),
            owner: None,
            work_item_id: Some(work.id.clone()),
            claimed_work_revision: Some(work.revision),
        });
        guard.persist_state(&runtime.inner.storage).await.unwrap();
    }
    let error = crate::provider::provider_turn_error(
        "all configured providers failed for this turn: default: retries_exhausted (rate_limited)",
        crate::provider::ProviderAttemptTimeline {
            attempts: vec![crate::provider::ProviderAttemptRecord {
                provider: "stub".into(),
                model_ref: "openai@default/gpt-5.4".into(),
                attempt: 1,
                max_attempts: 1,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                failure_kind: Some("rate_limited".into()),
                disposition: Some("deferred".into()),
                outcome: crate::provider::ProviderAttemptOutcome::RetriesExhausted,
                advanced_to_fallback: false,
                backoff_ms: None,
                backoff_source: None,
                token_usage: None,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                provider_http_trace_id: None,
                transport_diagnostics: None,
                transport_timeline: None,
            }],
            requested_model_ref: "default".into(),
            active_model_ref: Some("default".into()),
            winning_model_ref: None,
            pending_fallback_model_ref: None,
            pending_fallback_disposition: None,
            aggregated_token_usage: None,
        },
        anyhow::anyhow!("quota exhausted"),
    );

    for round in 1..=3 {
        let outcome = runtime
            .maybe_defer_provider_lineage_failure(
                "default", round, &error, None, None, 10, false, false,
            )
            .await
            .unwrap();
        if round < 3 {
            assert!(outcome.is_none());
        } else {
            assert_eq!(
                outcome
                    .expect("third failure parks the WorkItem")
                    .terminal_kind,
                TurnTerminalKind::DeferredToFallback
            );
        }
    }
    {
        let mut guard = runtime.begin_mutation().await.unwrap();
        guard.state.current_execution_binding = None;
        guard.persist_state(&runtime.inner.storage).await.unwrap();
    }

    let blocked = runtime
        .storage()
        .latest_work_item(&work.id)
        .unwrap()
        .expect("parked WorkItem");
    let recheck_at = blocked.recheck_at.expect("durable recheck deadline");
    let incident_identity = runtime
        .storage()
        .read_recent_events(20)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "provider_quota_exhausted")
        .and_then(|event| event.data["identity_key"].as_str().map(str::to_owned))
        .expect("quota identity");
    let run_task = tokio::spawn(runtime.clone().run());

    let before_deadline = (recheck_at - clock.now())
        .to_std()
        .expect("recheck deadline is in the future")
        .saturating_sub(std::time::Duration::from_millis(1));
    advance_lifecycle_time(&clock, before_deadline).await;
    assert!(runtime
        .inner
        .runtime_db
        .provider_quota_incident(&incident_identity)
        .unwrap()
        .expect("quota incident")
        .resolved_at
        .is_none());
    assert!(runtime
        .storage()
        .latest_work_item(&work.id)
        .unwrap()
        .expect("parked WorkItem")
        .blocked_by
        .is_some());

    advance_lifecycle_time(&clock, std::time::Duration::from_millis(2)).await;
    wait_for_audit_events(
        &runtime,
        100,
        |events| {
            events
                .iter()
                .any(|event| event.kind == "provider_quota_resolved")
        },
        "successful provider quota probe",
    )
    .await;
    assert!(runtime
        .inner
        .runtime_db
        .provider_quota_incident(&incident_identity)
        .unwrap()
        .expect("quota incident")
        .resolved_at
        .is_some());
    run_task.abort();
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

    let mut preserved = Vec::new();
    for kind in [MessageKind::OperatorPrompt, MessageKind::InternalFollowup] {
        let mut message = MessageEnvelope::new(
            "default",
            kind,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "not runtime recovery".into(),
            },
        );
        message.work_item_id = recovery.work_item_id.clone();
        message.metadata = recovery.metadata.clone();
        preserved.push(runtime.enqueue(message).await.unwrap().id);
    }

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
    for id in preserved {
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&id)
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
            .peek_next_matching(|message| message.id == id)
            .is_some());
    }
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
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
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
    let ollama_id = crate::config::ProviderId::parse("ollama").unwrap();
    config
        .providers
        .retain(|provider_id, _| provider_id == &ollama_id);
    config.vision_candidate_models.clear();
    let discovery_cache = crate::model_discovery::discovery_cache_path(&config.home_dir);
    let host = RuntimeHost::new(config).unwrap();
    let runtime = host.default_runtime().await.unwrap();

    let (first, second) = tokio::join!(
        runtime.current_view_image_vision_selection(),
        runtime.current_view_image_vision_selection()
    );
    let cooled_selection = async {
        let selection = runtime.current_view_image_vision_route_selection().await?;
        runtime
            .record_view_image_candidate_protocol_failure(&selection)
            .await
            .ok_or_else(|| anyhow::anyhow!("expected the selected Ollama model to be tracked"))?;
        let failure = runtime
            .record_view_image_candidate_protocol_failure(&selection)
            .await
            .ok_or_else(|| anyhow::anyhow!("expected the selected Ollama model to be tracked"))?;
        if !failure.circuit_open {
            anyhow::bail!("expected the selected Ollama model cooldown to open");
        }
        std::fs::remove_file(discovery_cache)?;
        runtime.current_view_image_vision_route_selection().await
    }
    .await;
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
    let unavailable = cooled_selection.unwrap();
    assert_eq!(
        unavailable.selection.selected_mode,
        crate::types::ViewImageSelectedMode::Unavailable
    );
    assert_eq!(
        unavailable.selection.selection_reason,
        "all_configured_view_image_candidates_cooling_after_protocol_validation_failures"
    );
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

async fn run_failure_artifact_provider_fixture(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
) -> Result<turn::AgentLoopOutcome> {
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await?
    else {
        anyhow::bail!("failure artifact source was not claimed");
    };
    anyhow::ensure!(
        scheduled.message.id == message.id,
        "wrong failure artifact source"
    );
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await?;
    runtime
        .run_agent_loop_with_fixture_dispatch(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
            Some(scheduled),
        )
        .await
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
    // Failure briefs reference an ingress-accepted message, never a raw value.
    let message = runtime.enqueue(message).await.unwrap();
    let error = match run_failure_artifact_provider_fixture(&runtime, &message).await {
        Ok(_) => panic!("provider must fail"),
        Err(error) => error,
    };
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
    // Failure briefs reference an ingress-accepted message, never a raw value.
    let message = runtime.enqueue(message).await.unwrap();
    let error = match run_failure_artifact_provider_fixture(&runtime, &message).await {
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
    // Failure briefs reference an ingress-accepted message, never a raw value.
    let message = runtime.enqueue(message).await.unwrap();
    let error = match run_failure_artifact_provider_fixture(&runtime, &message).await {
        Ok(_) => panic!("provider failure should abort the turn"),
        Err(error) => error,
    };
    {
        // Simulate the missing terminal summary as an acknowledged State
        // mutation, so the next assembler sees the same published owner view.
        let mut guard = runtime.begin_mutation().await.unwrap();
        guard.state.last_turn_terminal = None;
        guard.persist_state(&runtime.inner.storage).await.unwrap();
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
