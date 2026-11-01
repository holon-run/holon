use super::super::message_dispatch::message_text;
use super::super::*;
use super::support::*;
use crate::types::{
    AgentMessageCallerContext, AgentMessagePrincipalKind, AgentMessageSendRequest,
    QueueEntryRecord, TurnOwner, WorkItemPlanStatus,
};

mod canonical_not_runnable;
mod recovery_batching;
mod recovery_diagnostics;
mod rejected_producers;
mod retained_sources;
mod task_rejoin_recovery;

#[derive(Default)]
struct BatchProvider {
    requests: Mutex<Vec<ProviderTurnRequest>>,
    before_complete: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

#[derive(Clone, Default)]
struct ResultFallbackProvider {
    requests: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl AgentProvider for ResultFallbackProvider {
    fn select_model_lineage(
        &self,
        _: &crate::config::ModelRouteRef,
    ) -> Option<Arc<dyn AgentProvider>> {
        Some(Arc::new(self.clone()))
    }

    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut requests = self.requests.lock().await;
        requests.push(
            request
                .prompt_frame
                .context_blocks
                .iter()
                .map(|block| block.text.as_str())
                .chain(
                    request
                        .conversation
                        .iter()
                        .filter_map(|message| match message {
                            ConversationMessage::UserText(text) => Some(text.as_str()),
                            _ => None,
                        }),
                )
                .collect::<Vec<_>>()
                .join("\n"),
        );
        if requests.len() == 1 {
            return Err(provider_turn_error(
                "primary HTTP 400 contract_error before model output",
                ProviderAttemptTimeline {
                    attempts: vec![ProviderAttemptRecord {
                        provider: "openai".into(),
                        model_ref: "openai/gpt-5.4".into(),
                        attempt: 1,
                        max_attempts: 1,
                        started_at: None,
                        completed_at: None,
                        duration_ms: Some(1),
                        failure_kind: Some("contract_error".into()),
                        disposition: Some("fail_fast".into()),
                        outcome: ProviderAttemptOutcome::FailFastAborted,
                        advanced_to_fallback: true,
                        backoff_ms: None,
                        backoff_source: None,
                        token_usage: None,
                        cache_usage: None,
                        provider_message_id: None,
                        provider_request_id: None,
                        provider_http_trace_id: None,
                        transport_timeline: None,
                        transport_diagnostics: None,
                    }],
                    aggregated_token_usage: None,
                    requested_model_ref: "openai/gpt-5.4".into(),
                    active_model_ref: None,
                    winning_model_ref: None,
                    pending_fallback_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                    pending_fallback_disposition: Some(
                        crate::provider::ProviderFallbackDisposition::Immediate,
                    ),
                },
                anyhow!("HTTP 400 contract_error"),
            ));
        }
        Ok(ProviderTurnResponse {
            material: request.material,
            blocks: vec![ModelBlock::Text {
                text: "result handled".into(),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test(start_paused = true)]
async fn task_result_provider_fallback_transfers_initial_snapshot_and_converges() {
    assert_task_result_provider_fallback().await;
}

fn assert_task_result_provider_fallback(
) -> std::pin::Pin<Box<impl std::future::Future<Output = ()>>> {
    Box::pin(async move {
        for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
            for (status, count) in [
                (TaskStatus::Completed, 1usize),
                (TaskStatus::Completed, 2),
                (TaskStatus::Completed, 8),
                (TaskStatus::Completed, 9),
                (TaskStatus::Completed, 17),
                (TaskStatus::Failed, 1),
                (TaskStatus::Cancelled, 1),
            ] {
                let provider = Arc::new(ResultFallbackProvider::default());
                let harness = LifecycleHarness::with_provider(provider.clone());
                let runtime = harness.runtime();
                let mut messages = Vec::new();
                for index in 0..count {
                    let task_id = format!("fallback-result-{index}");
                    let mut message = MessageEnvelope::new(
                        "default",
                        MessageKind::TaskResult,
                        MessageOrigin::Task {
                            task_id: task_id.clone(),
                        },
                        AuthorityClass::RuntimeInstruction,
                        Priority::Normal,
                        MessageBody::Text {
                            text: format!("fallback-evidence-{index}"),
                        },
                    )
                    .with_admission(
                        MessageDeliverySurface::TaskRejoin,
                        AdmissionContext::RuntimeOwned,
                    );
                    message.metadata = Some(serde_json::json!({
                        "task_id": task_id, "task_kind": kind.as_str(),
                        "task_status": status, "task_result_id": message.id,
                        "task_detail": {
                            "rejoin_obligation_id": task_id, "rejoin_generation": 1,
                            "parent_turn_id": "fallback-result-parent", "terminal_reentry": true,
                        },
                    }));
                    let task = TaskRecord {
                        id: task_id.clone(),
                        agent_id: "default".into(),
                        kind,
                        status: status.clone(),
                        created_at: harness.now(),
                        updated_at: harness.now(),
                        parent_message_id: Some(message.id.clone()),
                        work_item_id: None,
                        summary: None,
                        recovery: None,
                        detail: Some(serde_json::json!({
                            "rejoin_obligation_id": task_id, "rejoin_generation": 1,
                            "parent_turn_id": "fallback-result-parent", "terminal_reentry": true,
                        })),
                    };
                    if kind == TaskKind::CommandTask {
                        Box::pin(runtime.commit_terminal_task_result(
                            &task,
                            "task_completed",
                            &message,
                        ))
                        .await
                        .unwrap();
                    } else {
                        Box::pin(runtime.commit_terminal_child_task_result(
                            &task,
                            "task_completed",
                            &message,
                            None,
                        ))
                        .await
                        .unwrap();
                    }
                    messages.push(message.id);
                }
                let first = Box::pin(claim_model_input(runtime)).await;
                Box::pin(process_scheduled(runtime, first)).await;
                assert_eq!(
                    provider.requests.lock().await.len(),
                    1,
                    "primary request for {kind:?}/{status:?}/{count}"
                );
                let before: Vec<_> = messages
                    .iter()
                    .map(|id| {
                        runtime
                            .inner
                            .runtime_db
                            .task_result_settlements()
                            .latest_for_message(id)
                            .unwrap()
                            .unwrap()
                    })
                    .collect();
                assert_eq!(
                    before
                        .iter()
                        .filter(|record| {
                            record.state ==
                crate::runtime_db::task_result_settlement::TaskResultSettlementState::CallerAdmitted
                        })
                        .count(),
                    count.min(8)
                );
                let recovery_message = runtime
                    .inner
                    .agent
                    .lock()
                    .await
                    .queue
                    .input_snapshot(64)
                    .into_iter()
                    .find(|message| {
                        super::super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message)
                    })
                    .expect("primary failure must enqueue its fallback");
                let execution = runtime
                    .runtime_db()
                    .transitions()
                    .load_execution_protocol_state_if_initialized("default")
                    .unwrap()
                    .unwrap();
                super::super::provider_recovery::resolve(
                    runtime.storage(),
                    &recovery_message,
                    &execution,
                )
                .unwrap_or_else(|error| {
                    panic!("invalid fallback for {kind:?}/{status:?}/{count}: {error:#}")
                });
                let fallback = Box::pin(claim_model_input(runtime)).await;
                assert!(
                super::super::turn::TurnModelSelection::message_has_provider_recovery_provenance(
                    &fallback.message
                )
            );
                Box::pin(process_scheduled(runtime, fallback)).await;
                {
                    let requests = provider.requests.lock().await;
                    assert_eq!(requests.len(), 2);
                    for index in 0..count.min(8) {
                        assert!(requests[1].contains(&format!("fallback-evidence-{index}")));
                    }
                    if count > 8 {
                        assert!(!requests[1].contains("fallback-evidence-8"));
                    }
                }
                for _ in 0..count * 3 {
                    if runtime.inner.agent.lock().await.queue.is_empty() {
                        break;
                    }
                    let poll = Box::pin(
                        scheduler_executor::SchedulerDecisionExecutor::new(runtime).poll(),
                    )
                    .await
                    .unwrap();
                    if let scheduler_executor::RunLoopPoll::Message(scheduled) = poll {
                        Box::pin(process_scheduled(runtime, scheduled)).await;
                    }
                }
                for id in &messages {
                    let record = runtime
                        .inner
                        .runtime_db
                        .task_result_settlements()
                        .latest_for_message(id)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                    record.state,
                    crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
                );
                    assert_eq!(
                        record.disposition,
                        Some(crate::runtime_db::TaskResultSettlementDisposition::ModelDelivered)
                    );
                }
                assert_eq!(provider.requests.lock().await.len(), count.div_ceil(8) + 1);
                let turns = runtime.storage().read_recent_turns(100).unwrap().len();
                let briefs = runtime.storage().read_recent_briefs(100).unwrap().len();
                for _ in 0..6 {
                    harness.advance(std::time::Duration::from_secs(600)).await;
                    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
                }
                assert_eq!(
                    runtime.storage().read_recent_turns(100).unwrap().len(),
                    turns
                );
                assert_eq!(
                    runtime.storage().read_recent_briefs(100).unwrap().len(),
                    briefs
                );
                assert_eq!(provider.requests.lock().await.len(), count.div_ceil(8) + 1);
            }
        }
    })
}

async fn start_without_provider(runtime: &RuntimeHandle) -> MessageEnvelope {
    Box::pin(try_start_without_provider(runtime)).await.unwrap()
}

pub(super) async fn publish_waiting_work_execution(
    runtime: &RuntimeHandle,
    work: &WorkItemRecord,
    wait_id: &str,
) {
    use crate::{
        domain::execution_protocol::{
            ExecutionProtocolState, WaitReference, WorkItemExecutionRecord, WorkItemExecutionState,
        },
        runtime_db::{
            input_evidence_snapshot::{
                register_committed_input_evidence_tx, CanonicalInputEvidence,
            },
            transitions::{PostCommitEffects, TransitionCommit},
        },
    };
    let mut mutation = runtime.begin_mutation().await.unwrap();
    let mut execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized(&work.agent_id)
        .unwrap()
        .unwrap_or_else(|| ExecutionProtocolState::empty(&work.agent_id));
    let record = WorkItemExecutionRecord {
        source_revision: work.revision,
        state: WorkItemExecutionState::Waiting {
            generation: work.revision.max(1),
            wait: WaitReference {
                wait_id: wait_id.into(),
            },
        },
    };
    execution.work_items.insert(work.id.clone(), record.clone());
    mutation
        .commit_transition(crate::ids::audit_event_id(), |scope| {
            runtime.runtime_db().transaction(|tx| {
                crate::runtime_db::transitions::persist_state_tx(tx, &execution)?;
                let input_evidence_delta = register_committed_input_evidence_tx(
                    tx,
                    scope,
                    &[CanonicalInputEvidence::WorkExecution {
                        work_item_id: work.id.clone(),
                        record: record.clone(),
                    }],
                )?;
                Ok(TransitionCommit {
                    applied: true,
                    effects: Box::new(PostCommitEffects {
                        input_evidence_delta,
                        execution_protocol: Some(execution.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            })
        })
        .await
        .unwrap();
}

async fn try_start_without_provider(runtime: &RuntimeHandle) -> Result<MessageEnvelope> {
    let scheduled = Box::pin(claim_model_input(runtime)).await;
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled.dispatch_plan.execution_admission_provenance,
            &scheduled.dispatch_plan.prior_closure,
        )
        .await?;
    runtime
        .record_incoming_transcript_entry(&scheduled.message)
        .await?;
    Ok(scheduled.message)
}

// All source runtimes have been dropped before this production preactivation
// phase. Bootstrap alone does not authorize finishing a stopped dispatch.
async fn recover_stopped_target(home: &std::path::Path, provider: Arc<dyn AgentProvider>) {
    let host_home = tempdir().unwrap();
    std::fs::write(
        host_home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let mut config = AppConfig::load_with_home(Some(host_home.path().to_path_buf())).unwrap();
    config.default_agent_id = "batch-target".into();
    config.data_dir = home.join("batch-target-state/.holon");
    config.workspace_dir = home.join("batch-workspace");
    config.user_home_dir = Some(host_home.path().to_path_buf());
    let host = RuntimeHost::new_with_provider(config, provider).unwrap();
    host.recover_startup_dispatch_memory_for_fixture()
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn p1_paused_and_backoff_results_do_not_reserve_recovery_capacity() {
    assert_budgeted_recovery_capacity(false).await;
}

#[tokio::test(start_paused = true)]
async fn p1_fresh_task_result_does_not_consume_paused_owner_results() {
    assert_budgeted_recovery_capacity(true).await;
}

fn assert_budgeted_recovery_capacity(
    normal_anchor: bool,
) -> std::pin::Pin<Box<impl std::future::Future<Output = ()>>> {
    Box::pin(async move {
        use crate::runtime::resident_task::{TaskFamily, TaskKey};
        use crate::runtime::task_result_recovery::budget_tests::owned_task_fixture_write;
        use crate::runtime_db::task_result_settlement::{
            charge_recovery_admission_tx, TaskResultSettlementState,
        };

        for paused in [false, true]
            .into_iter()
            .filter(|paused| !normal_anchor || *paused)
        {
            let provider = Arc::new(BatchProvider::default());
            let mut harness = LifecycleHarness::with_provider(provider.clone());
            let runtime = harness.runtime();
            let owner = runtime
                .create_work_item("recovery capacity".into(), None, None, Vec::new())
                .await
                .unwrap();
            let mut blocked = Vec::new();
            for index in 0..if normal_anchor { 1 } else { 64 } {
                let id = publish_owner_result_with_reentry(
                    runtime,
                    &owner.id,
                    format!("blocked-{index}"),
                    "older result",
                    Priority::Normal,
                    TaskKind::CommandTask,
                    false,
                )
                .await;
                let record = runtime
                    .apply_task_result_operation((&id, runtime.now()), || {
                        crate::runtime_db::transitions::TaskResultOperation::AdmitMessage {
                            message_id: id.clone(),
                            activation_id: "closed-source-admission".into(),
                            projection: None,
                            at: runtime.now(),
                        }
                    })
                    .await
                    .unwrap()
                    .record()
                    .unwrap()
                    .as_ref()
                    .cloned()
                    .unwrap();
                let message = runtime.storage().read_message_by_id(&id).unwrap().unwrap();
                runtime
                    .storage()
                    .append_queue_entry(&QueueEntryRecord {
                        message_id: id,
                        agent_id: message.agent_id,
                        priority: message.priority,
                        status: QueueEntryStatus::Processed,
                        created_at: message.created_at,
                        updated_at: runtime.now(),
                    })
                    .unwrap();
                blocked.push(record);
            }
            for root in 0..if paused { 5 } else { 1 } {
                let now = harness.runtime().now();
                for record in &blocked {
                    let result_identity = record.result_identity.clone();
                    let root_id = format!("older-root-{root}");
                    owned_task_fixture_write(
                        harness.runtime(),
                        std::collections::BTreeSet::from([TaskKey::new(
                            TaskFamily::ResultSettlement,
                            &result_identity,
                        )]),
                        move |tx| {
                            assert!(charge_recovery_admission_tx(
                                tx,
                                &result_identity,
                                &root_id,
                                now,
                            )?);
                            Ok(())
                        },
                    )
                    .await;
                }
                if paused {
                    harness.advance(std::time::Duration::from_secs(600)).await;
                }
            }
            let runtime = harness.runtime();
            let fresh = publish_owner_result_with_reentry(
                runtime,
                &owner.id,
                "fresh-result".into(),
                "new result must reach model",
                Priority::Normal,
                TaskKind::CommandTask,
                // The fresh result has original task re-entry authority.
                // Processing its queue row below still forces the repair-wake
                // path; a repair source must not invent that authority.
                true,
            )
            .await;
            let message = runtime
                .storage()
                .read_message_by_id(&fresh)
                .unwrap()
                .unwrap();
            if !normal_anchor {
                runtime
                    .storage()
                    .append_queue_entry(&QueueEntryRecord {
                        message_id: fresh.clone(),
                        agent_id: message.agent_id,
                        priority: message.priority,
                        status: QueueEntryStatus::Processed,
                        created_at: message.created_at,
                        updated_at: runtime.now(),
                    })
                    .unwrap();
            }
            let before: Vec<_> = blocked
                .iter()
                .map(|record| {
                    runtime
                        .runtime_db()
                        .task_result_settlements()
                        .latest_for_message(&record.message_id)
                        .unwrap()
                        .unwrap()
                })
                .collect();
            harness.restart();
            let runtime = harness.runtime();
            let record = runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&fresh)
                .unwrap()
                .unwrap();
            if !normal_anchor {
                // A generic owner wake cannot borrow the Task's re-entry
                // authority. Register the exact waiter through its native
                // repair writer before claiming the recovery admission.
                let outcome = runtime
                    .register_wait_for_outcome(
                        "default",
                        Some(owner.id.clone()),
                        WaitForWakeKind::TaskResult,
                        Some(record.task_id.clone()),
                        "recover the fresh result without reserving blocked capacity".into(),
                        None,
                    )
                    .await
                    .unwrap();
                let WaitForRegistrationOutcome::TaskResultRecoveryQueued {
                    wait_condition_id, ..
                } = outcome
                else {
                    panic!("expected exact result recovery: {outcome:?}");
                };
                let owner = runtime.latest_work_item(&owner.id).await.unwrap().unwrap();
                publish_waiting_work_execution(runtime, &owner, &wait_condition_id).await;
            }
            let scheduled = Box::pin(claim_model_input(runtime)).await;
            let admitted = Box::pin(process_result_batch(runtime, scheduled)).await;
            assert_eq!(admitted.len(), 1, "paused={paused}");
            assert_eq!(admitted[0].result_identity, record.result_identity);
            assert_eq!(admitted[0].recovery_attempts, u32::from(!normal_anchor));
            assert_eq!(
                admitted[0].last_recovery_admitted_at,
                (!normal_anchor).then_some(runtime.now()),
            );
            assert_eq!(provider.requests.lock().await.len(), 1);
            assert_eq!(
                runtime
                    .runtime_db()
                    .task_result_settlements()
                    .latest_for_message(&fresh)
                    .unwrap()
                    .unwrap()
                    .state,
                TaskResultSettlementState::Settled,
            );
            for record in before {
                assert_eq!(
                    runtime
                        .runtime_db()
                        .task_result_settlements()
                        .latest_for_message(&record.message_id)
                        .unwrap()
                        .unwrap(),
                    record,
                    "ineligible result must reserve no capacity or extra charge",
                );
            }
        }
    })
}

async fn claim_model_input(runtime: &RuntimeHandle) -> scheduler_executor::ScheduledMessage {
    for _ in 0..72 {
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap();
        let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            continue;
        };
        if !scheduled.scheduler_decision.model_reentry {
            process_scheduled(runtime, scheduled).await;
            continue;
        }
        return scheduled;
    }
    panic!(
        "expected model input; recent events: {:?}",
        runtime.storage().read_recent_events(8).unwrap()
    )
}

#[async_trait]
impl AgentProvider for BatchProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if let Some(before_complete) = self.before_complete.lock().await.as_ref() {
            before_complete();
        }
        let material = request.material.clone();
        self.requests.lock().await.push(request);
        Ok(ProviderTurnResponse {
            material,
            blocks: vec![ModelBlock::Text {
                text: "done".into(),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[tokio::test]
async fn p1_command_and_child_batches_include_anchor_and_reduce_settled_notifications() {
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        for count in [1usize, 2, 8, 9, 16, 17] {
            let provider = Arc::new(BatchProvider::default());
            let harness = LifecycleHarness::with_provider(provider.clone());
            let runtime = harness.runtime();
            let owner = runtime
                .create_work_item(
                    "bounded terminal results".into(),
                    Some(WorkItemPlanStatus::Ready),
                    None,
                    Vec::new(),
                )
                .await
                .unwrap();
            runtime.pick_work_item(owner.id.clone()).await.unwrap();
            let mut source_ids = Vec::new();
            for index in 0..count {
                let task_id = format!("p1-{}-{index}", kind.as_str());
                let mut message = MessageEnvelope::new(
                    "default",
                    MessageKind::TaskResult,
                    MessageOrigin::Task {
                        task_id: task_id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: format!("bounded-result-body-{index}"),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::TaskRejoin,
                    AdmissionContext::RuntimeOwned,
                );
                message.work_item_id = Some(owner.id.clone());
                message.metadata = Some(serde_json::json!({
                    "task_id": task_id,
                    "task_kind": kind.as_str(),
                    "task_status": "completed",
                    "task_result_id": message.id,
                    "work_item_id": owner.id,
                }));
                let task = TaskRecord {
                    id: task_id.clone(),
                    agent_id: "default".into(),
                    kind,
                    status: TaskStatus::Completed,
                    created_at: harness.now(),
                    updated_at: harness.now(),
                    parent_message_id: Some(message.id.clone()),
                    work_item_id: Some(owner.id.clone()),
                    summary: None,
                    detail: Some(serde_json::json!({
                        "rejoin_obligation_id": task_id,
                        "rejoin_generation": 1,
                        "parent_turn_id": "p1-result-parent",
                        "terminal_reentry": true,
                    })),
                    recovery: None,
                };
                if kind == TaskKind::CommandTask {
                    runtime
                        .commit_terminal_task_result(&task, "task_completed", &message)
                        .await
                        .unwrap();
                } else {
                    runtime
                        .commit_terminal_child_task_result(&task, "task_completed", &message, None)
                        .await
                        .unwrap();
                }
                source_ids.push(message.id);
            }
            let mut admissions = Vec::new();
            let mut unique = std::collections::BTreeSet::new();
            for _ in 0..count * 3 {
                if runtime.inner.agent.lock().await.queue.is_empty() {
                    break;
                }
                let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                    .poll()
                    .await
                    .unwrap();
                let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
                    continue;
                };
                let activation = scheduled
                    .dispatch_plan
                    .execution_admission_provenance
                    .as_ref()
                    .and_then(|provenance| match provenance {
                        ExecutionAdmissionProvenance::Canonical { activation_id, .. } => {
                            Some(activation_id.clone())
                        }
                        _ => None,
                    });
                let transition = runtime
                    .process_message_with_plan_deferred(
                        scheduled.message.clone(),
                        scheduled.dispatch_plan,
                        &scheduled.scheduler_decision,
                    )
                    .await
                    .unwrap();
                if let Some(activation) = activation {
                    let records = runtime
                        .inner
                        .runtime_db
                        .task_result_settlements()
                        .admitted_for_activation("default", &activation)
                        .unwrap();
                    assert!(records.len() <= 8);
                    admissions.push(records.len());
                    for record in records {
                        assert!(unique.insert(record.result_identity));
                    }
                }
                assert!(runtime
                    .commit_queue_terminal_settlement(
                        QueueEntryRecord {
                            message_id: scheduled.message.id,
                            agent_id: scheduled.message.agent_id,
                            priority: scheduled.message.priority,
                            status: QueueEntryStatus::Processed,
                            created_at: scheduled.message.created_at,
                            updated_at: Utc::now(),
                        },
                        Vec::new(),
                        true,
                        Some(&transition),
                    )
                    .await
                    .unwrap());
            }
            assert_eq!(unique.len(), count, "{kind:?}/{count}: {admissions:?}");
            assert_eq!(admissions.len(), count.div_ceil(8), "{kind:?}/{count}");
            assert_eq!(provider.requests.lock().await.len(), count.div_ceil(8));
            for id in &source_ids {
                assert_eq!(
                    runtime
                        .inner
                        .runtime_db
                        .task_result_settlements()
                        .latest_for_message(id)
                        .unwrap()
                        .unwrap()
                        .state,
                    crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
                );
            }
        }
    }
}

async fn fixture() -> (
    TempDir,
    RuntimeHost,
    RuntimeHandle,
    RuntimeHandle,
    Arc<BatchProvider>,
) {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    config.user_home_dir = None;
    let provider = Arc::new(BatchProvider::default());
    let host = RuntimeHost::new_with_provider(config, provider.clone()).unwrap();
    let sender = host.default_runtime().await.unwrap();
    std::fs::create_dir_all(home.path().join("batch-workspace")).unwrap();
    // No host background loop may race this deterministic scheduler fixture.
    let target = RuntimeHandle::new(
        "batch-target",
        home.path().join("batch-target-state"),
        home.path().join("batch-workspace"),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "fixture".into(),
        context_config(),
    )
    .unwrap();
    isolate_test_user_home(&target);
    (home, host, sender, target, provider)
}

async fn send(sender: &RuntimeHandle, target: &RuntimeHandle, key: &str, text: &str) -> String {
    send_with_authority(
        sender,
        target,
        key,
        text,
        AuthorityClass::RuntimeInstruction,
    )
    .await
}

async fn send_with_authority(
    sender: &RuntimeHandle,
    target: &RuntimeHandle,
    key: &str,
    text: &str,
    authority: AuthorityClass,
) -> String {
    send_with_priority_and_authority(sender, target, key, text, Priority::Normal, authority).await
}

async fn send_with_priority_and_authority(
    sender: &RuntimeHandle,
    target: &RuntimeHandle,
    key: &str,
    text: &str,
    priority: Priority,
    authority: AuthorityClass,
) -> String {
    send_with_priority_authority_and_correlation(
        sender, target, key, text, priority, authority, None,
    )
    .await
}

async fn send_with_priority_authority_and_correlation(
    sender: &RuntimeHandle,
    target: &RuntimeHandle,
    key: &str,
    text: &str,
    priority: Priority,
    authority: AuthorityClass,
    correlation_id: Option<String>,
) -> String {
    let prepared = prepare_sender_message(
        sender,
        target,
        key,
        text,
        priority,
        authority,
        correlation_id,
    )
    .await;
    target
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap()
        .message_id
        .unwrap()
}

async fn prepare_sender_message(
    sender: &RuntimeHandle,
    target: &RuntimeHandle,
    key: &str,
    text: &str,
    priority: Priority,
    authority: AuthorityClass,
    correlation_id: Option<String>,
) -> crate::runtime::agent_message_delivery::PreparedAgentMessageDelivery {
    let target_agent_id = target.inner.agent.lock().await.state.id.clone();
    let sender_id = sender.agent_id().await.unwrap();
    AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id,
            content: MessageBody::Text { text: text.into() },
            client_idempotency_key: key.into(),
            correlation_id,
            causation_id: None,
            requested_priority: Some(priority),
            forward: false,
            reply_context_id: None,
        },
        crate::types::AgentMessageCallerContext {
            caller_principal: format!("agent:{sender_id}"),
            caller_agent_id: Some(sender_id),
            principal_kind: crate::types::AgentMessagePrincipalKind::PeerAgent,
            route: "agent_message".into(),
            origin: MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            authority_class: authority,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: crate::types::AdmissionContext::RuntimeOwned,
            current_turn_id: None,
            current_task_id: None,
            current_work_item_id: None,
            inherited_routing_context: None,
            routing_activation_id: None,
        },
    )
    .unwrap()
}

async fn process_one(runtime: &RuntimeHandle) {
    for _ in 0..72 {
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap();
        let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            continue;
        };
        let model_reentry = scheduled.scheduler_decision.model_reentry;
        process_scheduled(runtime, scheduled).await;
        if model_reentry {
            return;
        }
    }
    panic!(
        "expected model input; recent events: {:?}",
        runtime.storage().read_recent_events(8).unwrap()
    )
}

async fn process_scheduled(
    runtime: &RuntimeHandle,
    scheduled: scheduler_executor::ScheduledMessage,
) {
    try_process_scheduled(runtime, scheduled).await.unwrap();
}

async fn try_process_scheduled(
    runtime: &RuntimeHandle,
    scheduled: scheduler_executor::ScheduledMessage,
) -> Result<()> {
    let message = scheduled.message;
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        message.clone(),
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await?;
    runtime
        .commit_queue_terminal_settlement(
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
        )
        .await?;
    Ok(())
}

async fn process_result_batch(
    runtime: &RuntimeHandle,
    scheduled: scheduler_executor::ScheduledMessage,
) -> Vec<crate::runtime_db::TaskResultSettlementRecord> {
    let activation = scheduled
        .dispatch_plan
        .execution_admission_provenance
        .as_ref()
        .and_then(|provenance| match provenance {
            ExecutionAdmissionProvenance::Canonical { activation_id, .. } => {
                Some(activation_id.clone())
            }
            _ => None,
        });
    let message = scheduled.message;
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        message.clone(),
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await
    .unwrap();
    let admitted = activation
        .map(|activation| {
            runtime
                .runtime_db()
                .task_result_settlements()
                .admitted_for_activation(&message.agent_id, &activation)
                .unwrap()
        })
        .unwrap_or_default();
    assert!(admitted.len() <= 8);
    assert!(runtime
        .commit_queue_terminal_settlement(
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
        )
        .await
        .unwrap());
    admitted
}

fn assert_anchor_reclaim_fences(runtime: &RuntimeHandle, message_id: &str, activation_id: &str) {
    use crate::domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding};
    let execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    let current = &execution.attempts[activation_id];
    let predecessor = &execution.attempts[current.recovery_of_attempt_id.as_ref().unwrap()];
    let before = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(message_id)
        .unwrap()
        .unwrap();
    for mutation in ["owner", "source", "lineage", "terminal"] {
        let original = if mutation == "terminal" {
            predecessor
        } else {
            current
        };
        let mut corrupt = original.clone();
        match mutation {
            "owner" => {
                corrupt.binding = ExecutionBinding::WorkItem {
                    work_item_id: "foreign-owner".into(),
                };
            }
            "source" => corrupt.source_message_id = Some("foreign-message".into()),
            "lineage" => corrupt.recovery_of_attempt_id = Some("foreign-attempt".into()),
            "terminal" => corrupt.state = ExecutionAttemptState::Settled,
            _ => unreachable!(),
        }
        let write = |attempt: &crate::domain::execution_protocol::ExecutionAttempt| {
            runtime
                .runtime_db()
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE execution_protocol_attempts SET payload_json = ?1
                         WHERE agent_id = ?2 AND attempt_id = ?3",
                        rusqlite::params![
                            serde_json::to_string(attempt)?,
                            attempt.agent_id,
                            attempt.attempt_id
                        ],
                    )?;
                    Ok(())
                })
                .unwrap();
        };
        write(&corrupt);
        assert!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .admit_message("default", message_id, activation_id, runtime.now())
                .unwrap()
                .is_none(),
            "{mutation}"
        );
        assert!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .admit_unsettled_snapshot(
                    "default",
                    before.work_item_id.as_deref(),
                    activation_id,
                    Some(message_id),
                    Some(&std::collections::BTreeSet::from([before
                        .result_identity
                        .clone(),])),
                    runtime.now(),
                )
                .unwrap()
                .is_empty(),
            "{mutation}"
        );
        assert_eq!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(message_id)
                .unwrap()
                .unwrap(),
            before,
            "{mutation}"
        );
        write(original);
    }
}

async fn send_to_owner(
    sender: &RuntimeHandle,
    target: &RuntimeHandle,
    key: &str,
    text: &str,
    owner: &str,
) -> String {
    let mut prepared = prepare_sender_message(
        sender,
        target,
        key,
        text,
        Priority::Normal,
        AuthorityClass::RuntimeInstruction,
        None,
    )
    .await;
    // This fixture supplies the receiver's binding before original ingress,
    // never rewriting an already qualified Message or its queued copy.
    prepared.message.work_item_id = Some(owner.to_owned());
    target
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap()
        .message_id
        .unwrap()
}

async fn publish_owner_result(
    runtime: &RuntimeHandle,
    owner_id: &str,
    task_id: String,
    body: &str,
) -> String {
    publish_owner_result_with_priority(runtime, owner_id, task_id, body, Priority::Normal).await
}

async fn publish_owner_result_with_priority(
    runtime: &RuntimeHandle,
    owner_id: &str,
    task_id: String,
    body: &str,
    priority: Priority,
) -> String {
    publish_owner_result_of_kind(
        runtime,
        owner_id,
        task_id,
        body,
        priority,
        TaskKind::CommandTask,
    )
    .await
}

async fn publish_owner_result_of_kind(
    runtime: &RuntimeHandle,
    owner_id: &str,
    task_id: String,
    body: &str,
    priority: Priority,
    kind: TaskKind,
) -> String {
    publish_owner_result_with_reentry(runtime, owner_id, task_id, body, priority, kind, true).await
}

async fn publish_owner_result_with_reentry(
    runtime: &RuntimeHandle,
    owner_id: &str,
    task_id: String,
    body: &str,
    priority: Priority,
    kind: TaskKind,
    terminal_reentry: bool,
) -> String {
    let agent_id = runtime.agent_id().await.unwrap();
    let parent_turn_id = format!("p1-result-parent-{task_id}");
    persist_completed_task_parent_fixture(runtime, &parent_turn_id, Some(owner_id))
        .await
        .unwrap();
    let mut message = MessageEnvelope::new(
        &agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        priority,
        MessageBody::Text { text: body.into() },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.work_item_id = Some(owner_id.to_owned());
    message.metadata = Some(serde_json::json!({
        "task_id": task_id,
        "task_kind": kind.as_str(),
        "task_status": "completed",
        "task_result_id": message.id,
        "work_item_id": owner_id,
    }));
    let task = TaskRecord {
        id: task_id.clone(),
        agent_id,
        kind,
        status: TaskStatus::Completed,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: Some(message.id.clone()),
        work_item_id: Some(owner_id.to_owned()),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": task_id,
            "rejoin_generation": 1,
            "parent_turn_id": parent_turn_id,
            "terminal_reentry": terminal_reentry,
        })),
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

#[tokio::test(start_paused = true)]
async fn p1_command_and_child_recovery_wakes_drain_pending_results_once() {
    Box::pin(assert_command_and_child_recovery_wakes_drain_pending_results_once()).await;
}

async fn assert_command_and_child_recovery_wakes_drain_pending_results_once() {
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementDisposition, TaskResultSettlementState,
    };

    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        for count in [1usize, 9, 17] {
            let provider = Arc::new(BatchProvider::default());
            let mut harness = LifecycleHarness::with_provider(provider.clone());
            let runtime = harness.runtime();
            let owner = runtime
                .create_work_item(
                    "deferred terminal results".into(),
                    Some(WorkItemPlanStatus::NeedsInput),
                    None,
                    Vec::new(),
                )
                .await
                .unwrap();
            let mut wait_message = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "prepare deferred result wait".into(),
                },
            );
            wait_message.turn_id = Some(crate::ids::turn_id());
            runtime.enqueue(wait_message).await.unwrap();
            let wait_message = start_without_provider(runtime).await;
            runtime
                .register_wait_for(
                    "default",
                    Some(owner.id.clone()),
                    WaitForWakeKind::OperatorInput,
                    None,
                    "defer until the operator releases this owner".into(),
                    None,
                )
                .await
                .unwrap();
            finish_claimed_test_run(runtime).await;
            let terminal = terminal_transition_from_started_turn(runtime, &wait_message);
            runtime
                .commit_queue_terminal_settlement(
                    QueueEntryRecord {
                        message_id: wait_message.id,
                        agent_id: wait_message.agent_id,
                        priority: wait_message.priority,
                        status: QueueEntryStatus::Processed,
                        created_at: wait_message.created_at,
                        updated_at: Utc::now(),
                    },
                    Vec::new(),
                    true,
                    Some(&terminal),
                )
                .await
                .unwrap();
            assert!(matches!(
                runtime
                    .runtime_db()
                    .transitions()
                    .load_execution_protocol_state_if_initialized("default")
                    .unwrap()
                    .unwrap()
                    .work_items[&owner.id]
                    .state,
                crate::domain::execution_protocol::WorkItemExecutionState::Waiting { .. }
            ));
            let mut sources = Vec::new();
            for index in 0..count {
                sources.push(
                    publish_owner_result_with_reentry(
                        runtime,
                        &owner.id,
                        format!("wake-{}-{index}", kind.as_str()),
                        &format!("recovered terminal body {index}"),
                        if index % 2 == 0 {
                            Priority::Normal
                        } else {
                            Priority::Next
                        },
                        kind,
                        false,
                    )
                    .await,
                );
            }
            // The original notifications are really reduced and committed
            // while the owner is ineligible; only the result ledger survives.
            for _ in 0..count * 4 + 4 {
                if runtime.inner.agent.lock().await.queue.is_empty() {
                    break;
                }
                let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                    .poll()
                    .await
                    .unwrap();
                if let scheduler_executor::RunLoopPoll::Message(scheduled) = poll {
                    assert!(
                        !scheduled.scheduler_decision.model_reentry,
                        "deferred source {:?}: {:?}",
                        scheduled.message, scheduled.scheduler_decision,
                    );
                    assert!(process_result_batch(runtime, scheduled).await.is_empty());
                }
            }
            assert!(runtime.inner.agent.lock().await.queue.is_empty());
            assert!(provider.requests.lock().await.is_empty());
            let repository = runtime.runtime_db().task_result_settlements();
            let selector = repository.latest_for_message(&sources[0]).unwrap().unwrap();
            assert!(repository
                .non_reply_source_matches_owner(&selector)
                .unwrap());
            for mutation in ["agent", "owner", "source", "generation", "parent", "status"] {
                let mut corrupt = selector.clone();
                match mutation {
                    "agent" => corrupt.agent_id = "foreign-agent".into(),
                    "owner" => corrupt.work_item_id = Some("foreign-owner".into()),
                    "source" => corrupt.message_id = "foreign-message".into(),
                    "generation" => corrupt.rejoin_generation += 1,
                    "parent" => {
                        corrupt.source =
                            crate::domain::execution_protocol::TaskRejoinSource::ParentTurn {
                                parent_turn_id: "foreign-turn".into(),
                            };
                    }
                    "status" => corrupt.task_status = "running".into(),
                    _ => unreachable!(),
                }
                assert!(!repository.non_reply_source_matches_owner(&corrupt).unwrap());
            }
            runtime
                .update_work_item_fields(
                    owner.id.clone(),
                    None,
                    Some(WorkItemPlanStatus::Ready),
                    None,
                    None,
                    None,
                )
                .await
                .unwrap();
            runtime
                .pick_work_item_with_reason_and_clear_blocker(
                    owner.id.clone(),
                    Some("operator released the recovery fixture".into()),
                    true,
                )
                .await
                .unwrap();
            harness.restart();
            let runtime = harness.runtime();
            runtime.recover_scheduler_bootstrap_claims().await.unwrap();
            let mut unique = std::collections::BTreeSet::new();
            let mut batches = Vec::new();
            for _ in 0..count * 4 + 4 {
                if sources.iter().all(|source| {
                    runtime
                        .runtime_db()
                        .task_result_settlements()
                        .latest_for_message(source)
                        .unwrap()
                        .is_some_and(|record| record.state == TaskResultSettlementState::Settled)
                }) {
                    break;
                }
                if runtime.inner.agent.lock().await.queue.is_empty() {
                    harness.advance(std::time::Duration::from_secs(31)).await;
                    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
                    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
                }
                let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                    .poll()
                    .await
                    .unwrap();
                if let scheduler_executor::RunLoopPoll::Message(scheduled) = poll {
                    assert!(matches!(
                        &scheduled.message.origin,
                        MessageOrigin::System { subsystem } if subsystem == "task_result_recovery"
                    ));
                    assert_eq!(scheduled.message.priority, Priority::Background);
                    let records = process_result_batch(runtime, scheduled).await;
                    assert!(
                        !records.is_empty(),
                        "{kind:?}/{count}: recovery made no progress"
                    );
                    batches.push(records.len());
                    for record in records {
                        assert_eq!(record.work_item_id.as_deref(), Some(owner.id.as_str()));
                        assert!(unique.insert(record.result_identity), "duplicate admission");
                    }
                }
            }
            assert_eq!(unique.len(), count, "{kind:?}/{count}: {batches:?}");
            assert_eq!(batches.len(), count.div_ceil(8));
            assert_eq!(provider.requests.lock().await.len(), count.div_ceil(8));
            for source in sources {
                let record = runtime
                    .runtime_db()
                    .task_result_settlements()
                    .latest_for_message(&source)
                    .unwrap()
                    .unwrap();
                assert_eq!(record.state, TaskResultSettlementState::Settled);
                assert_eq!(
                    record.disposition,
                    Some(TaskResultSettlementDisposition::ModelDelivered)
                );
                assert_eq!(
                    runtime
                        .runtime_db()
                        .queue_entries()
                        .latest(&source)
                        .unwrap()
                        .unwrap()
                        .status,
                    QueueEntryStatus::Processed
                );
            }
            assert!(runtime.inner.agent.lock().await.queue.is_empty());
            harness.advance(std::time::Duration::from_secs(31)).await;
            assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
            assert!(runtime
                .runtime_db()
                .task_result_settlements()
                .unsettled_for_agent("default", 64)
                .unwrap()
                .is_empty());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn p1_command_and_child_admitted_anchor_survives_two_runtime_rebuilds() {
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementDisposition, TaskResultSettlementState,
    };

    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        for count in [1usize, 9] {
            let provider = Arc::new(BatchProvider::default());
            let harness = LifecycleHarness::with_provider(provider.clone());
            let runtime = harness.runtime();
            let owner = runtime
                .create_work_item(
                    "interrupted terminal admission".into(),
                    Some(WorkItemPlanStatus::Ready),
                    None,
                    Vec::new(),
                )
                .await
                .unwrap();
            runtime.pick_work_item(owner.id.clone()).await.unwrap();
            let mut sources = Vec::new();
            for index in 0..count {
                sources.push(
                    publish_owner_result_of_kind(
                        runtime,
                        &owner.id,
                        format!("restart-{}-{index}", kind.as_str()),
                        &format!("admitted before restart body {index}"),
                        Priority::Normal,
                        kind,
                    )
                    .await,
                );
            }
            let (anchor, predecessor) = if count == 1 {
                let scheduled = claim_model_input(runtime).await;
                let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) =
                    scheduled.dispatch_plan.execution_admission_provenance
                else {
                    panic!("result must have a canonical activation");
                };
                (scheduled.message, activation_id)
            } else {
                let anchor = start_without_provider(runtime).await;
                let predecessor = runtime
                    .agent_state()
                    .await
                    .unwrap()
                    .current_execution_binding
                    .unwrap()
                    .activation_id
                    .unwrap();
                (anchor, predecessor)
            };
            // Stop at the real source reducer's durable admission boundary,
            // before any provider response or activation settlement exists.
            let repository = runtime.runtime_db().task_result_settlements();
            let anchor_record = runtime
                .begin_mutation()
                .await
                .unwrap()
                .commit_task_result_operation(
                    crate::runtime_db::transitions::TaskResultOperation::AdmitMessage {
                        message_id: anchor.id.clone(),
                        activation_id: predecessor.clone(),
                        projection: None,
                        at: runtime.now(),
                    },
                )
                .await
                .unwrap()
                .record()
                .unwrap()
                .clone()
                .unwrap();
            if count > 1 {
                runtime
                    .admit_idle_task_results(&anchor, 24 * 1024)
                    .await
                    .unwrap();
            }
            let previous = repository
                .admitted_for_activation("default", &predecessor)
                .unwrap();
            assert_eq!(previous.len(), count.min(8));
            assert!(provider.requests.lock().await.is_empty());
            // Only exclusive Host preactivation can finish the old Memory;
            // opening a second handle does not prove old producers ended.
            let clock = harness.clock();
            let (data_dir, workspace, runtime) = harness.into_startup_fixture_parts();
            let (runtime, _) = super::runtime_state::recover_quiescent_startup_fixture(
                runtime,
                data_dir.path(),
                workspace.path(),
            )
            .await;
            runtime.recover_scheduler_bootstrap_claims().await.unwrap();
            assert!(provider.requests.lock().await.is_empty());
            // Two physical rebuilds precede the sole WorkItem result replay.
            // Interrupting a claimed replay would exhaust its existing fence.
            let (runtime, _) = super::runtime_state::recover_quiescent_startup_fixture(
                runtime,
                data_dir.path(),
                workspace.path(),
            )
            .await;
            let runtime = &runtime;
            runtime.recover_scheduler_bootstrap_claims().await.unwrap();
            let mut unique = std::collections::BTreeSet::new();
            let mut batches = Vec::new();
            let mut successor = None;
            for _ in 0..count * 4 + 4 {
                if runtime.inner.agent.lock().await.queue.is_empty() {
                    break;
                }
                let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                    .poll()
                    .await
                    .unwrap();
                let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
                    continue;
                };
                if scheduled.message.id == anchor.id && batches.is_empty() {
                    let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) =
                        scheduled
                            .dispatch_plan
                            .execution_admission_provenance
                            .as_ref()
                    else {
                        panic!("recovery must have a canonical activation");
                    };
                    let execution = runtime
                        .runtime_db()
                        .transitions()
                        .load_execution_protocol_state_if_initialized("default")
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        execution.attempts[activation_id]
                            .recovery_of_attempt_id
                            .as_deref(),
                        Some(predecessor.as_str())
                    );
                    assert_anchor_reclaim_fences(runtime, &anchor.id, activation_id);
                    successor = Some(activation_id.clone());
                }
                let records = process_result_batch(runtime, scheduled).await;
                if !records.is_empty() {
                    if batches.is_empty() {
                        assert_eq!(records.len(), previous.len());
                        for record in &previous {
                            let recovered = records
                                .iter()
                                .find(|item| item.result_identity == record.result_identity)
                                .expect("the interrupted snapshot must be reclaimed first");
                            assert_ne!(recovered.activation_id, record.activation_id);
                            assert_eq!(recovered.admitted_at, record.admitted_at);
                            assert_eq!(recovered.work_item_id, record.work_item_id);
                            assert_eq!(recovered.message_id, record.message_id);
                            assert_eq!(recovered.rejoin_generation, record.rejoin_generation);
                            assert_eq!(recovered.source, record.source);
                        }
                    }
                    batches.push(records.len());
                }
                for record in records {
                    assert!(unique.insert(record.result_identity), "duplicate admission");
                }
            }
            assert_eq!(unique.len(), count, "{kind:?}/{count}: {batches:?}");
            assert_eq!(batches.len(), count.div_ceil(8));
            assert_eq!(provider.requests.lock().await.len(), count.div_ceil(8));
            let repository = runtime.runtime_db().task_result_settlements();
            assert!(repository
                .admitted_for_activation("default", &predecessor)
                .unwrap()
                .is_empty());
            assert!(repository
                .admitted_for_activation(
                    "default",
                    successor.as_deref().expect("the replay must be dispatched"),
                )
                .unwrap()
                .is_empty());
            for source in sources {
                let record = repository.latest_for_message(&source).unwrap().unwrap();
                assert_eq!(record.state, TaskResultSettlementState::Settled);
                assert_eq!(
                    record.disposition,
                    Some(TaskResultSettlementDisposition::ModelDelivered)
                );
                if source == anchor.id {
                    assert_eq!(record.result_identity, anchor_record.result_identity);
                    assert_eq!(record.admitted_at, anchor_record.admitted_at);
                }
            }
            assert!(repository
                .unsettled_for_agent("default", 64)
                .unwrap()
                .is_empty());
            assert!(runtime.inner.agent.lock().await.queue.is_empty());
            advance_lifecycle_time(&clock, std::time::Duration::from_secs(31)).await;
            assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        }
    }
}

async fn publish_shared_reply(
    runtime: &RuntimeHandle,
    owners: &[String],
    body: &str,
) -> (String, Vec<String>) {
    let agent_id = runtime.agent_id().await.unwrap();
    let mut prepared = AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id: agent_id.clone(),
            content: MessageBody::Text { text: body.into() },
            client_idempotency_key: format!("p1-shared-{}", uuid::Uuid::new_v4()),
            correlation_id: None,
            causation_id: None,
            requested_priority: Some(Priority::Normal),
            forward: false,
            reply_context_id: None,
        },
        AgentMessageCallerContext {
            caller_principal: "agent:shared-reply-peer".into(),
            caller_agent_id: Some("shared-reply-peer".into()),
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
    prepared.message.work_item_id = owners.first().cloned();
    prepared.message.created_at = runtime.now();
    let reply_id = runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap()
        .message_id
        .unwrap();
    let reply = runtime
        .storage()
        .read_message_by_id(&reply_id)
        .unwrap()
        .unwrap();
    let delivery_id =
        reply.metadata.as_ref().unwrap()["agent_message_delivery"]["delivery_id"].clone();
    for owner in owners.iter().collect::<std::collections::BTreeSet<_>>() {
        persist_completed_task_parent_fixture(
            runtime,
            &format!("p1-shared-parent-{owner}"),
            Some(owner),
        )
        .await
        .unwrap();
    }
    let mut results = Vec::new();
    for (index, owner) in owners.iter().enumerate() {
        let task_id = format!("p1-shared-{}-{index}", reply.id);
        let running = TaskRecord {
            id: task_id.clone(),
            agent_id: agent_id.clone(),
            kind: TaskKind::AgentMessageWait,
            status: TaskStatus::Running,
            created_at: runtime.now(),
            updated_at: runtime.now(),
            parent_message_id: None,
            work_item_id: Some(owner.clone()),
            summary: None,
            detail: Some(serde_json::json!({
                "rejoin_obligation_id": task_id,
                "rejoin_generation": 1,
                "parent_turn_id": format!("p1-shared-parent-{owner}"),
            })),
            recovery: None,
        };
        runtime
            .persist_task_transition(&running, "task_created")
            .await
            .unwrap();
        let mut detail = running.detail.clone().unwrap();
        detail["message_id"] = serde_json::json!(reply.id);
        detail["response_delivery_id"] = serde_json::json!(delivery_id);
        runtime
            .finish_agent_message_wait(&running, TaskStatus::Completed, detail, String::new())
            .await
            .unwrap();
        let terminal = runtime
            .storage()
            .latest_task_record(&task_id)
            .unwrap()
            .unwrap();
        results.push(terminal.parent_message_id.unwrap());
    }
    (reply.id, results)
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_counts_each_waiter_with_one_body_per_bounded_batch() {
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementDisposition, TaskResultSettlementState,
    };

    for count in [1usize, 2, 8, 9, 16, 17] {
        let provider = Arc::new(BatchProvider::default());
        let harness = LifecycleHarness::with_provider(provider.clone());
        let target = harness.runtime();
        let agent_id = target.agent_id().await.unwrap();
        let owner = target
            .create_work_item(
                "shared reply bounded waiters".into(),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        target.pick_work_item(owner.id.clone()).await.unwrap();
        let marker = "p1-shared-original-body-marker";
        let (reply, results) =
            publish_shared_reply(&target, &vec![owner.id.clone(); count], marker).await;
        let repository = target.inner.runtime_db.task_result_settlements();
        let mut unique = std::collections::BTreeSet::new();
        for _ in 0..count * 4 + 4 {
            if results.iter().all(|id| {
                repository
                    .latest_for_message(id)
                    .unwrap()
                    .is_some_and(|record| record.state == TaskResultSettlementState::Settled)
            }) {
                break;
            }
            if target.inner.agent.lock().await.queue.is_empty() {
                harness.advance(std::time::Duration::from_secs(31)).await;
                assert!(
                    target.emit_due_task_result_recovery().await.unwrap(),
                    "{count}: overflow has no recoverable notification"
                );
            }
            let poll = scheduler_executor::SchedulerDecisionExecutor::new(&target)
                .poll()
                .await
                .unwrap();
            let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
                continue;
            };
            let before = provider.requests.lock().await.len();
            let activation = scheduled
                .dispatch_plan
                .execution_admission_provenance
                .as_ref()
                .and_then(|provenance| match provenance {
                    ExecutionAdmissionProvenance::Canonical { activation_id, .. } => {
                        Some(activation_id.clone())
                    }
                    _ => None,
                });
            let transition = target
                .process_message_with_plan_deferred(
                    scheduled.message.clone(),
                    scheduled.dispatch_plan,
                    &scheduled.scheduler_decision,
                )
                .await
                .unwrap();
            if provider.requests.lock().await.len() > before {
                let admitted = repository
                    .admitted_for_activation(&agent_id, activation.as_deref().unwrap())
                    .unwrap();
                assert!(
                    !admitted.is_empty(),
                    "{count}: source={:?} activation={activation:?} results={:?}",
                    scheduled.message.kind,
                    results
                        .iter()
                        .map(|id| repository.latest_for_message(id).unwrap())
                        .collect::<Vec<_>>()
                );
                assert!(admitted.len() <= 8, "{count}: {}", admitted.len());
                for record in admitted {
                    assert!(unique.insert(record.result_identity), "{count}: duplicate");
                    assert_eq!(record.work_item_id.as_deref(), Some(owner.id.as_str()));
                }
                let requests = provider.requests.lock().await;
                let context = requests
                    .last()
                    .unwrap()
                    .prompt_frame
                    .context_blocks
                    .iter()
                    .flat_map(|block| block.text.split("\n## "))
                    .filter(|section| {
                        let section = section.trim_start_matches("## ");
                        section.starts_with("current_input\n")
                            || section.starts_with("pending_task_results\n")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert_eq!(context.matches(marker).count(), 1, "{count}: {reply}");
            }
            assert!(target
                .commit_queue_terminal_settlement(
                    QueueEntryRecord {
                        message_id: scheduled.message.id,
                        agent_id: scheduled.message.agent_id,
                        priority: scheduled.message.priority,
                        status: QueueEntryStatus::Processed,
                        created_at: scheduled.message.created_at,
                        updated_at: Utc::now(),
                    },
                    Vec::new(),
                    true,
                    Some(&transition),
                )
                .await
                .unwrap());
        }
        assert_eq!(unique.len(), count, "shared reply waiter count {count}");
        assert_eq!(
            provider.requests.lock().await.len(),
            1 + count.saturating_sub(7).div_ceil(8)
        );
        for id in results {
            let record = repository.latest_for_message(&id).unwrap().unwrap();
            assert_eq!(record.state, TaskResultSettlementState::Settled);
            assert_eq!(
                record.disposition,
                Some(TaskResultSettlementDisposition::ModelDelivered)
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_overflow_recovers_after_queued_notifications_finish() {
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementDisposition, TaskResultSettlementState,
    };

    let provider = Arc::new(BatchProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let agent_id = runtime.agent_id().await.unwrap();
    let owner = runtime
        .create_work_item(
            "shared reply overflow survives notification consumption".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let (reply, results) =
        publish_shared_reply(runtime, &vec![owner.id.clone(); 17], "shared overflow body").await;
    let mut identities = std::collections::BTreeSet::new();
    for _ in 0..results.len() * 4 + 4 {
        if runtime.inner.agent.lock().await.queue.is_empty() {
            break;
        }
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap();
        let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            continue;
        };
        let original_reply = scheduled.message.id == reply;
        if original_reply {
            assert_eq!(scheduled.message.priority, Priority::Normal);
        } else {
            assert!(results.contains(&scheduled.message.id));
            assert_eq!(scheduled.message.priority, Priority::Next);
            assert!(!scheduled.scheduler_decision.model_reentry);
        }
        let admitted = process_result_batch(runtime, scheduled).await;
        if original_reply {
            assert_eq!(admitted.len(), 7);
            for record in admitted {
                assert!(identities.insert(record.result_identity));
            }
        } else {
            assert!(admitted.is_empty());
        }
    }
    assert!(runtime.inner.agent.lock().await.queue.is_empty());
    assert_eq!(provider.requests.lock().await.len(), 1);
    assert_eq!(identities.len(), 7);
    let repository = runtime.runtime_db().task_result_settlements();
    let pending = repository
        .unsettled_for_owner(&agent_id, Some(&owner.id), 64)
        .unwrap();
    assert_eq!(pending.len(), 10);
    for record in &pending {
        assert_ne!(record.state, TaskResultSettlementState::CallerAdmitted);
        assert!(record.activation_id.is_none());
        assert!(repository.reply_source_is_recoverable(record).unwrap());
    }
    // Queue completion does not settle overflow. Its independent ledger survives
    // restart even though every original notification was already reduced.
    for message_id in results.iter().chain(std::iter::once(&reply)) {
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(message_id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Processed
        );
    }
    harness.restart();
    let runtime = harness.runtime();
    // Exercise the identity cap, independently of evolving tool-schema overhead.
    let mut recovery_context = context_config();
    recovery_context.prompt_budget_estimated_tokens += 16_384;
    recovery_context.turn_projection_min_budget = recovery_context.prompt_budget_estimated_tokens;
    recovery_context.turn_projection_max_budget = recovery_context.prompt_budget_estimated_tokens;
    *runtime.inner.context_config.write().await = recovery_context;
    runtime.recover_scheduler_bootstrap_claims().await.unwrap();
    assert!(runtime.inner.agent.lock().await.queue.is_empty());
    assert_eq!(
        runtime
            .runtime_db()
            .task_result_settlements()
            .unsettled_for_owner(&agent_id, Some(&owner.id), 64)
            .unwrap()
            .len(),
        10
    );
    for expected in [8, 2] {
        harness.advance(std::time::Duration::from_secs(31)).await;
        assert!(runtime.emit_due_task_result_recovery().await.unwrap());
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        let scheduled = claim_model_input(runtime).await;
        assert!(matches!(
            &scheduled.message.origin,
            MessageOrigin::System { subsystem } if subsystem == "task_result_recovery"
        ));
        assert_eq!(scheduled.message.priority, Priority::Background);
        assert_eq!(scheduled.message.work_item_id, Some(owner.id.clone()));
        let admitted = process_result_batch(runtime, scheduled).await;
        assert_eq!(admitted.len(), expected);
        for record in admitted {
            assert_eq!(record.work_item_id.as_deref(), Some(owner.id.as_str()));
            assert!(
                identities.insert(record.result_identity),
                "duplicate admission"
            );
        }
        assert!(runtime.inner.agent.lock().await.queue.is_empty());
    }
    assert_eq!(identities.len(), results.len());
    assert_eq!(provider.requests.lock().await.len(), 3);
    let repository = runtime.runtime_db().task_result_settlements();
    for message_id in results {
        let record = repository.latest_for_message(&message_id).unwrap().unwrap();
        assert_eq!(record.state, TaskResultSettlementState::Settled);
        assert_eq!(
            record.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
    }
    assert!(repository
        .unsettled_for_owner(&agent_id, Some(&owner.id), 64)
        .unwrap()
        .is_empty());
    harness.advance(std::time::Duration::from_secs(31)).await;
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
}

#[tokio::test]
async fn p1_shared_reply_does_not_settle_a_different_captured_owner() {
    use crate::runtime_db::task_result_settlement::TaskResultSettlementState;

    let (_home, _host, _sender, target, provider) = fixture().await;
    let first = target
        .create_work_item(
            "first shared reply owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let second = target
        .create_work_item(
            "second shared reply owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let (_, results) = publish_shared_reply(
        &target,
        &[first.id.clone(), second.id.clone()],
        "shared body must not bridge owners",
    )
    .await;
    target.pick_work_item(first.id.clone()).await.unwrap();
    process_one(&target).await;
    let repository = target.inner.runtime_db.task_result_settlements();
    let delivered = repository.latest_for_message(&results[0]).unwrap().unwrap();
    let pending = repository.latest_for_message(&results[1]).unwrap().unwrap();
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(delivered.work_item_id, Some(first.id));
    assert_eq!(pending.state, TaskResultSettlementState::PersistedPending);
    assert_eq!(pending.work_item_id, Some(second.id));
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_recovery_reuses_admitted_waiters_before_overflow() {
    use crate::runtime_db::task_result_settlement::TaskResultSettlementState;

    let provider = Arc::new(BatchProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let target = harness.runtime();
    let agent_id = target.agent_id().await.unwrap();
    let owner = target
        .create_work_item(
            "shared reply restart".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    target.pick_work_item(owner.id.clone()).await.unwrap();
    let (_, results) =
        publish_shared_reply(&target, &vec![owner.id; 9], "restart shared reply marker").await;
    let anchor = start_without_provider(&target).await;
    target
        .admit_idle_task_results(&anchor, 24 * 1024)
        .await
        .unwrap();
    let activation = target
        .inner
        .agent
        .lock()
        .await
        .state
        .current_execution_binding
        .as_ref()
        .unwrap()
        .activation_id
        .clone()
        .unwrap();
    let before = target
        .inner
        .runtime_db
        .task_result_settlements()
        .admitted_for_activation(&agent_id, &activation)
        .unwrap();
    assert!(!before.is_empty());
    assert!(before.len() <= 8);
    let (data_dir, workspace, target) = harness.into_startup_fixture_parts();
    let (target, _) = super::runtime_state::recover_quiescent_startup_fixture(
        target,
        data_dir.path(),
        workspace.path(),
    )
    .await;
    let target = &target;
    target.recover_scheduler_bootstrap_claims().await.unwrap();
    process_one(&target).await;
    let repository = target.inner.runtime_db.task_result_settlements();
    let before_ids: std::collections::BTreeSet<_> =
        before.iter().map(|record| &record.message_id).collect();
    for result in &results {
        let record = repository.latest_for_message(result).unwrap().unwrap();
        assert_eq!(
            record.state,
            if before_ids.contains(result) {
                TaskResultSettlementState::Settled
            } else {
                TaskResultSettlementState::PersistedPending
            }
        );
    }
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn p1_closed_admitted_reply_recovers_through_fenced_runtime_wake() {
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementDisposition, TaskResultSettlementState,
    };
    for (count, reject_corrupt_wakes) in [(1usize, false), (3, false), (9, false), (1, true)] {
        let provider = Arc::new(BatchProvider::default());
        let mut harness = LifecycleHarness::with_provider(provider.clone());
        let runtime = harness.runtime();
        let owner = runtime
            .create_work_item(
                "closed reply recovery".into(),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        runtime.pick_work_item(owner.id.clone()).await.unwrap();
        let (reply, results) = publish_shared_reply(
            runtime,
            &vec![owner.id.clone(); count],
            "closed reply recovery full body",
        )
        .await;
        let source = Box::pin(start_without_provider(runtime)).await;
        Box::pin(runtime.admit_idle_task_results(&source, 24 * 1024))
            .await
            .unwrap();
        // Close a real canonical admission before any provider request. This
        // covers the source-terminal / fallback-enqueue crash window.
        let mut terminal = Box::pin(runtime.build_turn_aborted_record(
            "pre-request provider handoff fixture",
            None,
            0,
        ))
        .await;
        terminal.kind = TurnTerminalKind::DeferredToFallback;
        terminal.no_brief_reason = None;
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Failure,
            "pre-request provider handoff fixture",
            Some(source.id.clone()),
            None,
        );
        brief.turn_id = Some(terminal.turn_id.clone());
        brief.work_item_id = Some(owner.id.clone());
        runtime.persist_brief(&brief).await.unwrap();
        let turn_record = runtime.build_turn_record(&terminal).await.unwrap();
        let transition = turn::TurnTerminalTransition {
            terminal,
            turn_record,
            prepared_work_item_completion: None,
            prepared_wait_for: None,
            provider_recovery_message: None,
            terminal_tool_executions: Vec::new(),
        };
        assert!(runtime
            .commit_queue_terminal_settlement(
                QueueEntryRecord {
                    message_id: source.id,
                    agent_id: source.agent_id,
                    priority: source.priority,
                    status: QueueEntryStatus::Processed,
                    created_at: source.created_at,
                    updated_at: runtime.now(),
                },
                Vec::new(),
                true,
                Some(&transition),
            )
            .await
            .unwrap());
        assert!(provider.requests.lock().await.is_empty());
        let repository = runtime.runtime_db().task_result_settlements();
        let admitted: Vec<_> = results
            .iter()
            .filter_map(|id| {
                repository
                    .latest_for_message(id)
                    .unwrap()
                    .filter(|record| record.state == TaskResultSettlementState::CallerAdmitted)
            })
            .collect();
        assert!(!admitted.is_empty() && admitted.len() <= count.min(8));
        for record in &admitted {
            assert_eq!(record.disposition, None);
        }
        let original_stamp = runtime
            .storage()
            .read_message_by_id(&reply)
            .unwrap()
            .unwrap()
            .turn_id;
        harness.restart();
        let runtime = harness.runtime();
        runtime.recover_scheduler_bootstrap_claims().await.unwrap();
        harness.advance(std::time::Duration::from_secs(31)).await;
        assert!(
            runtime.emit_due_task_result_recovery().await.unwrap(),
            "recovery must be due: {:?}",
            runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&admitted[0].message_id)
                .unwrap()
        );
        let scheduled = Box::pin(claim_model_input(runtime)).await;
        assert!(matches!(&scheduled.message.origin,
            MessageOrigin::System { subsystem } if subsystem == "task_result_recovery"));
        if reject_corrupt_wakes {
            let source = &scheduled.message;
            Box::pin(
                runtime.begin_interactive_turn_with_provenance(
                    Some(source),
                    None,
                    None,
                    scheduled
                        .dispatch_plan
                        .execution_admission_provenance
                        .clone(),
                ),
            )
            .await
            .unwrap();
            runtime
                .record_incoming_transcript_entry(source)
                .await
                .unwrap();
            let state = runtime.agent_state().await.unwrap();
            let activation = state
                .current_execution_binding
                .as_ref()
                .unwrap()
                .activation_id
                .as_deref()
                .unwrap();
            let selected = admitted
                .iter()
                .map(|record| record.result_identity.clone())
                .collect();
            let repository = runtime.runtime_db().task_result_settlements();
            let original = repository
                .latest_for_message(&admitted[0].message_id)
                .unwrap()
                .unwrap();
            for mutation in [
                "identity",
                "generation",
                "parent",
                "selector",
                "owner",
                "trust",
                "surface",
                "admission",
            ] {
                let mut corrupt = source.clone();
                match mutation {
                    "identity" => {
                        corrupt
                            .source_refs
                            .insert("task_result_identity".into(), "foreign".into());
                    }
                    "generation" => {
                        corrupt
                            .source_refs
                            .insert("task_result_rejoin_generation".into(), "99".into());
                    }
                    "parent" => {
                        corrupt
                            .source_refs
                            .insert("task_result_parent_turn_id".into(), "foreign".into());
                    }
                    "selector" => {
                        corrupt
                            .source_refs
                            .insert("task_result_message_id".into(), "foreign".into());
                    }
                    "owner" => corrupt.work_item_id = Some("foreign".into()),
                    "trust" => corrupt.authority_class = AuthorityClass::ExternalEvidence,
                    "surface" => {
                        corrupt.delivery_surface = Some(MessageDeliverySurface::HttpControlPrompt)
                    }
                    "admission" => {
                        corrupt.admission_context = Some(AdmissionContext::ControlAuthenticated)
                    }
                    _ => unreachable!(),
                }
                runtime
                    .runtime_db()
                    .transaction(|tx| {
                        tx.execute(
                            "UPDATE messages SET payload_json = ?1 WHERE message_id = ?2",
                            rusqlite::params![serde_json::to_string(&corrupt)?, source.id],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                assert!(
                    repository
                        .admit_reply_message_snapshot(
                            "default",
                            Some(&owner.id),
                            &reply,
                            activation,
                            &selected,
                            Utc::now(),
                        )
                        .unwrap()
                        .is_empty(),
                    "{mutation}"
                );
                assert_eq!(
                    repository
                        .latest_for_message(&original.message_id)
                        .unwrap()
                        .unwrap(),
                    original
                );
            }
            runtime
                .runtime_db()
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE messages SET payload_json = ?1 WHERE message_id = ?2",
                        rusqlite::params![serde_json::to_string(source)?, source.id],
                    )?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                repository
                    .admit_reply_message_snapshot(
                        "default",
                        Some(&owner.id),
                        &reply,
                        activation,
                        &selected,
                        Utc::now(),
                    )
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                repository
                    .latest_for_message(&original.message_id)
                    .unwrap()
                    .unwrap()
                    .recovery_attempts,
                1
            );
            assert!(provider.requests.lock().await.is_empty());
            continue;
        }
        let recovered = Box::pin(process_result_batch(runtime, scheduled)).await;
        for original in &admitted {
            assert!(
                recovered
                    .iter()
                    .any(|record| record.result_identity == original.result_identity),
                "missing admission; recovered: {:?}; current: {:?}",
                recovered,
                runtime
                    .runtime_db()
                    .task_result_settlements()
                    .latest_for_message(&original.message_id)
                    .unwrap()
            );
            let record = runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&original.message_id)
                .unwrap()
                .unwrap();
            assert_eq!(record.state, TaskResultSettlementState::Settled);
            assert_eq!(
                record.disposition,
                Some(TaskResultSettlementDisposition::ModelDelivered)
            );
            assert_eq!(record.recovery_attempts, 1);
            assert_eq!(record.work_item_id, original.work_item_id);
        }
        assert_eq!(provider.requests.lock().await.len(), 1);
        assert_eq!(
            runtime
                .storage()
                .read_message_by_id(&reply)
                .unwrap()
                .unwrap()
                .turn_id,
            original_stamp,
        );
    }
}

#[tokio::test]
async fn p1_mixed_owner_reserves_two_ordinary_inputs_before_result_admission() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let owner = target
        .create_work_item(
            "mixed owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut ordinary = Vec::new();
    for index in 0..6 {
        let id = send_to_owner(
            &sender,
            &target,
            &format!("peer-{index}"),
            &format!("ordinary-{index}"),
            &owner.id,
        )
        .await;
        ordinary.push(id);
    }
    let mut results = Vec::new();
    for index in 0..8 {
        results.push(
            publish_owner_result(
                &target,
                &owner.id,
                format!("mixed-result-{index}"),
                &format!("result-{index}"),
            )
            .await,
        );
    }
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, ordinary);
    let settled = results
        .iter()
        .filter(|id| {
            target
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(id)
                .unwrap()
                .unwrap()
                .state
                == crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
        })
        .count();
    assert_eq!(settled, 2);
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn p1_result_flood_reserves_two_ordinary_inputs_in_the_same_owner_budget() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let owner = target
        .create_work_item(
            "result-first mixed owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut results = Vec::new();
    for index in 0..8 {
        results.push(
            publish_owner_result(
                &target,
                &owner.id,
                format!("flood-result-{index}"),
                &format!("flood-body-{index}"),
            )
            .await,
        );
    }
    let mut ordinary = Vec::new();
    for index in 0..6 {
        let id = send_to_owner(
            &sender,
            &target,
            &format!("late-peer-{index}"),
            &format!("late-ordinary-{index}"),
            &owner.id,
        )
        .await;
        ordinary.push(id);
    }
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(
        turn.input_message_ids,
        [results[0].clone(), ordinary[0].clone(), ordinary[1].clone()]
    );
    for (index, id) in results.iter().enumerate() {
        let record = target
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(id)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.state
                == crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled,
            index < 6
        );
    }
    for id in &ordinary[2..] {
        assert_eq!(
            target
                .inner
                .runtime_db
                .queue_entries()
                .latest(id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
    }
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn p1_mixed_budget_reserves_ordinary_bytes_before_result_admission() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let owner = target
        .create_work_item(
            "mixed byte fairness".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let first = send_to_owner(&sender, &target, "first", "ordinary anchor", &owner.id).await;
    let mut results = Vec::new();
    for index in 0..6 {
        results.push(
            publish_owner_result(
                &target,
                &owner.id,
                format!("fair-result-{index}-{}", "x".repeat(1900)),
                "terminal result",
            )
            .await,
        );
    }
    let second = send_to_owner(
        &sender,
        &target,
        "large-ordinary",
        &"y".repeat(12000),
        &owner.id,
    )
    .await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first, second]);
    let settled = results
        .iter()
        .filter(|id| {
            target
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(id)
                .unwrap()
                .unwrap()
                .state
                == crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
        })
        .count();
    // Versioned source/provenance wrappers consume the same byte allowance.
    assert_eq!(settled, 1);
    let requests = provider.requests.lock().await;
    let visible = requests[0]
        .prompt_frame
        .context_blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains(&"y".repeat(12000)));
}

#[tokio::test]
async fn p1_result_bytes_are_reserved_before_the_source_ledger_admits() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let owner = target
        .create_work_item(
            "result byte budget".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let anchor = send_to_owner(
        &sender,
        &target,
        "budget-anchor",
        "check pending result budget",
        &owner.id,
    )
    .await;
    let mut results = Vec::new();
    for index in 0..7 {
        results.push(
            publish_owner_result(
                &target,
                &owner.id,
                format!("large-task-{index}-{}", "x".repeat(1900)),
                "terminal result",
            )
            .await,
        );
    }
    let first = results[0].clone();
    let second = results.last().unwrap().clone();
    let candidates = target
        .inner
        .runtime_db
        .task_result_settlements()
        .unsettled_for_owner("batch-target", Some(&owner.id), 8)
        .unwrap();
    let anchor = target
        .storage()
        .read_message_by_id(&anchor)
        .unwrap()
        .unwrap();
    let pending =
        crate::context::render_pending_task_result_records(&target.storage(), &anchor, &candidates)
            .unwrap()
            .unwrap();
    assert!(pending.len() > input_consumption::INPUT_BATCH_BYTES);
    process_one(&target).await;
    let first_record = target
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&first)
        .unwrap()
        .unwrap();
    let second_record = target
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&second)
        .unwrap()
        .unwrap();
    assert_eq!(
        (first_record.state, first_record.disposition),
        (
            crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled,
            Some(crate::runtime_db::task_result_settlement::TaskResultSettlementDisposition::ModelDelivered)
        )
    );
    assert_eq!(
        (second_record.state, second_record.disposition),
        (
            crate::runtime_db::task_result_settlement::TaskResultSettlementState::PersistedPending,
            None
        )
    );
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn p1_exact_waiter_precedes_owner_batch_without_migrating_result_owner() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let owner = target
        .create_work_item(
            "task producer".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let waiter = target
        .create_work_item(
            "independent waiter".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let first = send_to_owner(
        &sender,
        &target,
        "ordinary-first",
        "must not absorb exact result",
        &owner.id,
    )
    .await;
    let second = send_to_owner(
        &sender,
        &target,
        "ordinary-second",
        "same producer owner",
        &owner.id,
    )
    .await;
    let result = publish_owner_result(
        &target,
        &owner.id,
        "p1-exact-cross-owner".into(),
        "promised exact result",
    )
    .await;
    let outcome = target
        .register_wait_for_outcome(
            "batch-target",
            Some(waiter.id.clone()),
            crate::runtime::WaitForWakeKind::TaskResult,
            Some("p1-exact-cross-owner".into()),
            "exact dependency must retain its own execution".into(),
            None,
        )
        .await
        .unwrap();
    let crate::runtime::WaitForRegistrationOutcome::TaskResultQueued {
        wait_condition_id, ..
    } = outcome
    else {
        panic!("completed dependency must queue the exact result");
    };
    // This fixture registers outside an open execution; seed the authority
    // that a real yielding WaitFor commits with its execution settlement.
    let waiter = target.latest_work_item(&waiter.id).await.unwrap().unwrap();
    publish_waiting_work_execution(&target, &waiter, &wait_condition_id).await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [result.clone()]);
    assert_eq!(
        turn.effective_owner(),
        TurnOwner::WorkItem {
            work_item_id: waiter.id
        }
    );
    assert_eq!(
        target
            .storage()
            .latest_task_record("p1-exact-cross-owner")
            .unwrap()
            .unwrap()
            .work_item_id,
        Some(owner.id.clone())
    );
    let record = target
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&result)
        .unwrap()
        .unwrap();
    assert_eq!(record.work_item_id, Some(owner.id));
    assert_eq!(
        record.state,
        crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
    );
    let queued = target.inner.agent.lock().await.queue.input_snapshot(8);
    assert_eq!(
        queued.iter().map(|message| &message.id).collect::<Vec<_>>(),
        [&first, &second]
    );
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn p1_receiver_owner_combines_multiple_senders_without_using_current_focus() {
    let (_home, host, sender, target, provider) = fixture().await;
    host.create_named_agent("other-sender", None).await.unwrap();
    let other = host.get_public_agent("other-sender").await.unwrap();
    let owner = target
        .create_work_item(
            "explicit receiver owner".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let focus = target
        .create_work_item(
            "unrelated focus".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    target.pick_work_item(focus.id).await.unwrap();
    let first = send_to_owner(&sender, &target, "first", "request", &owner.id).await;
    let second = send_to_owner(
        &other,
        &target,
        "second",
        "correction from another sender",
        &owner.id,
    )
    .await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first, second]);
    assert_eq!(
        turn.current_work_item_id.as_deref(),
        Some(owner.id.as_str())
    );
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn p1_sender_never_bridges_receiver_owners_through_an_unbound_message() {
    let (_home, _host, sender, target, _) = fixture().await;
    let owner_a = target
        .create_work_item(
            "owner A".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let owner_b = target
        .create_work_item(
            "owner B".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let first = send_to_owner(&sender, &target, "first", "owner A request", &owner_a.id).await;
    let unbound = send(&sender, &target, "unbound", "unassociated message").await;
    let other_owner =
        send_to_owner(&sender, &target, "other", "owner B request", &owner_b.id).await;
    let last = send_to_owner(&sender, &target, "last", "owner A correction", &owner_a.id).await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first, last]);
    let queued = target.inner.agent.lock().await.queue.input_snapshot(64);
    assert_eq!(
        queued.iter().map(|message| &message.id).collect::<Vec<_>>(),
        [&unbound, &other_owner]
    );
}

#[tokio::test]
async fn p1_sender_messages_1_2_8_9_use_bounded_provider_batches() {
    for count in [1usize, 2, 8, 9] {
        let (_home, _host, sender, target, provider) = fixture().await;
        let mut ids = Vec::new();
        for index in 0..count {
            ids.push(
                send(
                    &sender,
                    &target,
                    &format!("message-{index}"),
                    &format!("input-{index}"),
                )
                .await,
            );
        }
        for _ in 0..count.div_ceil(input_consumption::INPUT_BATCH_ITEMS) {
            process_one(&target).await;
        }
        assert_eq!(
            provider.requests.lock().await.len(),
            count.div_ceil(8),
            "{count}"
        );
        let turns = target.storage().read_recent_turns(20).unwrap();
        let mut delivered = Vec::new();
        for turn in &turns {
            let batch = turn
                .idle_input_batch
                .as_ref()
                .expect("durable sender batch");
            assert!(batch.items.len() <= 8);
            assert!(batch.rendered_bytes <= batch.max_bytes);
            assert_eq!(
                input_consumption::render_batch(batch).len(),
                batch.rendered_bytes
            );
            assert_eq!(
                turn.input_message_ids,
                batch
                    .items
                    .iter()
                    .map(|item| item.message_id.clone())
                    .collect::<Vec<_>>()
            );
            delivered.extend(turn.input_message_ids.clone());
        }
        assert_eq!(delivered, ids);
        assert!(target.inner.agent.lock().await.queue.is_empty());
        let requests = provider.requests.lock().await;
        for (index, request) in requests.iter().enumerate() {
            let context = request
                .prompt_frame
                .context_blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            for input in index * 8..((index + 1) * 8).min(count) {
                assert!(context.contains(&format!("input-{input}")));
                assert!(context.contains(&format!("reply_context_id: {}", ids[input])));
            }
        }
    }
}

#[tokio::test]
async fn p1_different_runtime_senders_are_not_combined() {
    let (_home, host, sender, target, provider) = fixture().await;
    host.create_named_agent("other-sender", None).await.unwrap();
    let other = host.get_public_agent("other-sender").await.unwrap();
    let first = send(&sender, &target, "first", "original request").await;
    let second = send(&other, &target, "second", "unrelated sender").await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first]);
    assert_eq!(
        target.inner.agent.lock().await.queue.peek().unwrap().id,
        second
    );
    process_one(&target).await;
    assert_eq!(provider.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn p1_oversize_input_preserves_utf8_preview_and_reachable_source() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let id = send_with_authority(
        &sender,
        &target,
        "large",
        &"更正".repeat(40_000),
        AuthorityClass::ExternalEvidence,
    )
    .await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    let batch = turn.idle_input_batch.unwrap();
    assert!(batch.rendered_bytes <= input_consumption::INPUT_BATCH_BYTES);
    assert!(batch.items[0].rendered.contains("[bounded preview"));
    assert_eq!(batch.items[0].source_ref, format!("message:{id}"));
    let source = target.storage().read_message_by_id(&id).unwrap().unwrap();
    assert_eq!(message_text(&source.body), "更正".repeat(40_000));
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn p1_turn_start_cutoff_excludes_arrivals_before_later_context_rebuild() {
    let (_home, _host, sender, target, _) = fixture().await;
    let first = send(&sender, &target, "first", "already present").await;
    let anchor = start_without_provider(&target).await;
    let late = send(&sender, &target, "late", "arrived after turn start").await;
    assert!(target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first]);
    assert_eq!(
        target.inner.agent.lock().await.queue.peek().unwrap().id,
        late
    );
}

#[tokio::test]
async fn p1_concurrent_attachment_and_fixed_cutoff_do_not_double_consume() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let first = send(&sender, &target, "one", "original request").await;
    let second = send(&sender, &target, "two", "correction: use the second value").await;
    let anchor = start_without_provider(&target).await;
    let (left, right) = tokio::join!(
        target.attach_idle_sender_inputs(&anchor, 24 * 1024),
        target.attach_idle_sender_inputs(&anchor, 24 * 1024),
    );
    assert_ne!(left.unwrap(), right.unwrap());
    let late = send(&sender, &target, "late", "after fixed cutoff").await;
    assert!(!target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first, second.clone()]);
    let batch = turn.idle_input_batch.unwrap();
    let rendered = input_consumption::render_batch(&batch);
    assert!(rendered.find("original request").unwrap() < rendered.find("correction:").unwrap());
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&second)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Interjected
    );
    assert_eq!(
        target.inner.agent.lock().await.queue.peek().unwrap().id,
        late
    );
    assert!(provider.requests.lock().await.is_empty());
}

#[tokio::test]
async fn p1_unreconciled_reply_wait_is_not_sender_fallback() {
    let (_home, _host, sender, target, _) = fixture().await;
    send(&sender, &target, "one", "anchor").await;
    let follower = send(&sender, &target, "two", "reply candidate").await;
    let anchor = start_without_provider(&target).await;
    target
        .storage()
        .append_task(&TaskRecord {
            id: "unreconciled-reply".into(),
            agent_id: "batch-target".into(),
            kind: TaskKind::AgentMessageWait,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: Some(anchor.id.clone()),
            work_item_id: None,
            summary: Some("receipt is still being attached".into()),
            detail: None,
            recovery: Some(crate::types::TaskRecoverySpec::AgentMessageWait {
                summary: "pending reply".into(),
                message: String::new(),
                target_agent_id: sender.agent_id().await.unwrap(),
                authority_class: AuthorityClass::RuntimeInstruction,
                delivery_id: None,
                after_delivery_rowid: None,
                reply_deadline_at: None,
                requester_incarnation: None,
            }),
        })
        .unwrap();
    assert!(!target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&follower)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
}

#[tokio::test]
async fn p1_byte_budget_defers_whole_normal_item_without_claiming_it() {
    let (_home, _host, sender, target, _) = fixture().await;
    send(&sender, &target, "one", &"a".repeat(1200)).await;
    let follower = send(&sender, &target, "two", &"b".repeat(1200)).await;
    let anchor = start_without_provider(&target).await;
    assert!(target
        .attach_idle_sender_inputs(&anchor, 2400)
        .await
        .unwrap());
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    let batch = turn.idle_input_batch.unwrap();
    assert_eq!(batch.items.len(), 1);
    assert!(batch.rendered_bytes <= 2400);
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&follower)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
}

#[tokio::test]
async fn p1_current_focus_is_not_receiver_ownership() {
    let (_home, _host, sender, target, _) = fixture().await;
    let owner = target
        .create_work_item(
            "unrelated focus".into(),
            Some(crate::types::WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    target.pick_work_item(owner.id.clone()).await.unwrap();
    let first = send(&sender, &target, "one", "unassociated").await;
    let second = send(&sender, &target, "two", "also unassociated").await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first, second]);
    assert_eq!(turn.current_work_item_id, None);
    assert!(matches!(
        turn.effective_owner(),
        crate::types::TurnOwner::AgentLifecycle { .. }
    ));
}

#[tokio::test]
async fn p1_operator_interject_remains_ahead_of_sender_batch() {
    let (_home, _host, sender, target, provider) = fixture().await;
    send(&sender, &target, "one", "peer-one").await;
    send(&sender, &target, "two", "peer-two").await;
    let operator = MessageEnvelope::new(
        "batch-target",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("test".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Interject,
        MessageBody::Text {
            text: "operator-priority".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        crate::types::AdmissionContext::LocalProcess,
    );
    let id = target.enqueue(operator).await.unwrap().id;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [id]);
    assert!(turn.idle_input_batch.is_none());
    assert_eq!(target.inner.agent.lock().await.queue.len(), 2);
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn p1_same_sender_different_authority_is_not_combined() {
    let (_home, _host, sender, target, _) = fixture().await;
    let first = send(&sender, &target, "one", "runtime instruction").await;
    let second = send_with_authority(
        &sender,
        &target,
        "two",
        "external evidence",
        AuthorityClass::ExternalEvidence,
    )
    .await;
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first]);
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&second)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
}

#[tokio::test]
async fn p1_scan_stops_at_64_snapshot_candidates() {
    let (_home, host, sender, target, _) = fixture().await;
    host.create_named_agent("other-sender", None).await.unwrap();
    let other = host.get_public_agent("other-sender").await.unwrap();
    let first = send(&sender, &target, "one", "anchor").await;
    for index in 0..64 {
        send(
            &other,
            &target,
            &format!("other-{index}"),
            "incompatible sender",
        )
        .await;
    }
    let beyond = send(&sender, &target, "beyond", "outside scan").await;
    let anchor = start_without_provider(&target).await;
    assert!(target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first]);
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&beyond)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
}

#[tokio::test]
async fn p1_precommit_fault_leaves_follower_queued_and_unattached() {
    let (_home, _host, sender, target, _) = fixture().await;
    let first = send(&sender, &target, "one", "original").await;
    let second = send(&sender, &target, "two", "follow-up").await;
    let anchor = start_without_provider(&target).await;
    target.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit,
    );
    assert!(target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .is_err());
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first]);
    assert_eq!(turn.idle_input_batch.unwrap().items.len(), 1);
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&second)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
}

#[tokio::test]
async fn p1_restart_presents_attached_inputs_before_claiming_new_arrivals() {
    let (home, host, sender, target, provider) = fixture().await;
    let first = send(&sender, &target, "one", "original request").await;
    let second = send(&sender, &target, "two", "correction before crash").await;
    let anchor = start_without_provider(&target).await;
    assert!(target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    let original_batch = target
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .remove(0)
        .idle_input_batch
        .unwrap();
    let late = send_with_priority_and_authority(
        &sender,
        &target,
        "late",
        "after original snapshot",
        Priority::Next,
        AuthorityClass::RuntimeInstruction,
    )
    .await;
    drop(target);
    drop(sender);
    drop(host);
    recover_stopped_target(home.path(), provider.clone()).await;
    let target = RuntimeHandle::new(
        "batch-target",
        home.path().join("batch-target-state"),
        home.path().join("batch-workspace"),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "fixture".into(),
        context_config(),
    )
    .unwrap();
    target.recover_scheduler_bootstrap_claims().await.unwrap();
    assert_eq!(
        target.inner.agent.lock().await.queue.peek().unwrap().id,
        late
    );
    process_one(&target).await;
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first.clone(), second.clone()]);
    let recovered_batch = turn.idle_input_batch.unwrap();
    assert_ne!(recovered_batch.activation_id, original_batch.activation_id);
    assert_eq!(recovered_batch.items, original_batch.items);
    assert_eq!(recovered_batch.cutoff, original_batch.cutoff);
    assert_eq!(
        target.inner.agent.lock().await.queue.peek().unwrap().id,
        late
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 1);
    let context = requests[0]
        .prompt_frame
        .context_blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(context.contains("correction before crash"));
    assert!(!context.contains("after original snapshot"));
    drop(requests);
    process_one(&target).await;
    assert_eq!(
        target.storage().read_recent_turns(1).unwrap()[0].input_message_ids,
        [late]
    );
    assert!(target.inner.agent.lock().await.queue.is_empty());
    assert_eq!(provider.requests.lock().await.len(), 2);
    for (id, status) in [
        (first, QueueEntryStatus::Processed),
        (second, QueueEntryStatus::Interjected),
    ] {
        assert_eq!(
            target
                .inner
                .runtime_db
                .queue_entries()
                .latest(&id)
                .unwrap()
                .unwrap()
                .status,
            status
        );
    }
}

// Keep fixture preparation off the subsequent provider execution poll stack.
fn seed_recovery_priority_barrier<'a>(
    target: &'a RuntimeHandle,
    barrier: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + 'a>> {
    Box::pin(async move {
        if barrier == "operator" {
            let message = MessageEnvelope::new(
                "batch-target",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: Some("test".into()),
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Interject,
                MessageBody::Text {
                    text: "operator before interrupted batch".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::CliPrompt,
                AdmissionContext::LocalProcess,
            );
            target.enqueue(message).await.unwrap().id
        } else {
            let owner = target
                .create_work_item(
                    "priority result producer".into(),
                    Some(WorkItemPlanStatus::Ready),
                    None,
                    Vec::new(),
                )
                .await
                .unwrap();
            let task_id = format!("recovery-barrier-{barrier}");
            let result = publish_owner_result_with_priority(
                target,
                &owner.id,
                task_id.clone(),
                "priority result before interrupted batch",
                if barrier == "task-rejoin" {
                    Priority::Next
                } else {
                    Priority::Normal
                },
            )
            .await;
            if barrier == "exact-waiter" {
                let waiter = target
                    .create_work_item(
                        "independent exact waiter".into(),
                        Some(WorkItemPlanStatus::Ready),
                        None,
                        Vec::new(),
                    )
                    .await
                    .unwrap();
                let outcome = target
                    .register_wait_for_outcome(
                        "batch-target",
                        Some(waiter.id.clone()),
                        crate::runtime::WaitForWakeKind::TaskResult,
                        Some(task_id),
                        "exact result before interrupted batch".into(),
                        None,
                    )
                    .await
                    .unwrap();
                let crate::runtime::WaitForRegistrationOutcome::TaskResultQueued {
                    wait_condition_id,
                    ..
                } = outcome
                else {
                    panic!("completed dependency must queue the exact result");
                };
                let waiter = target.latest_work_item(&waiter.id).await.unwrap().unwrap();
                publish_waiting_work_execution(target, &waiter, &wait_condition_id).await;
            }
            result
        }
    })
}

#[tokio::test]
async fn p1_recovered_batch_never_precedes_operator_task_rejoin_or_exact_waiter() {
    for barrier in ["operator", "task-rejoin", "exact-waiter"] {
        let (home, _host, sender, target, provider) = fixture().await;
        let first = send(&sender, &target, "one", "interrupted original").await;
        let second = send(&sender, &target, "two", "interrupted follower").await;
        let anchor = Box::pin(start_without_provider(&target)).await;
        assert!(target
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .unwrap());
        drop(target);
        recover_stopped_target(home.path(), provider.clone()).await;
        let target = RuntimeHandle::new(
            "batch-target",
            home.path().join("batch-target-state"),
            home.path().join("batch-workspace"),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "fixture".into(),
            context_config(),
        )
        .unwrap();
        Box::pin(target.recover_scheduler_bootstrap_claims())
            .await
            .unwrap();
        let blocker = seed_recovery_priority_barrier(&target, barrier).await;
        let late = send_with_priority_and_authority(
            &sender,
            &target,
            "late",
            "fresh ordinary backlog",
            Priority::Next,
            AuthorityClass::RuntimeInstruction,
        )
        .await;
        Box::pin(process_one(&target)).await;
        let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(turn.input_message_ids, [blocker.clone()], "{barrier}");
        if let Some(batch) = turn.idle_input_batch {
            assert_eq!(batch.anchor_message_id, blocker, "{barrier}");
            assert_eq!(batch.items.len(), 1, "{barrier}");
        }
        Box::pin(process_one(&target)).await;
        let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(turn.input_message_ids, [first, second], "{barrier}");
        Box::pin(process_one(&target)).await;
        assert_eq!(
            target.storage().read_recent_turns(1).unwrap()[0].input_message_ids,
            [late],
            "{barrier}"
        );
        for _ in 0..8 {
            if target
                .inner
                .agent
                .lock()
                .await
                .queue
                .peek()
                .is_none_or(|message| {
                    matches!(&message.origin,
                    MessageOrigin::System { subsystem } if subsystem == "work_queue")
                })
            {
                break;
            }
            let poll = scheduler_executor::SchedulerDecisionExecutor::new(&target)
                .poll()
                .await
                .unwrap();
            if let scheduler_executor::RunLoopPoll::Message(scheduled) = poll {
                assert!(
                    !scheduled.scheduler_decision.model_reentry,
                    "{barrier}: a settled notification must not reenter the model: {:?}",
                    scheduled.message
                );
                process_scheduled(&target, scheduled).await;
            }
        }
        // Ready business objectives are not settled task notifications.
        assert!(
            target
                .inner
                .agent
                .lock()
                .await
                .queue
                .peek_next_matching(|message| !matches!(&message.origin,
                    MessageOrigin::System { subsystem } if subsystem == "work_queue"))
                .is_none(),
            "{barrier}"
        );
        if barrier != "operator" {
            let record = target
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(&blocker)
                .unwrap()
                .unwrap();
            assert_eq!(
                record.state,
                crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
            );
            assert_eq!(record.disposition,
                Some(crate::runtime_db::task_result_settlement::TaskResultSettlementDisposition::ModelDelivered));
        }
        assert_eq!(provider.requests.lock().await.len(), 3, "{barrier}");
    }
}

#[tokio::test]
async fn p1_null_ingress_turn_recovers_twice_without_rebinding_attempt_turns() {
    for null_ingress in [false, true] {
        let (home, host, sender, target, provider) = fixture().await;
        let sender_id = sender.agent_id().await.unwrap();
        let first = if null_ingress {
            // Restore a pre-assignment source through the real writer/ACK,
            // before public enqueue would stamp its ingress Turn ID.
            let prepared = prepare_sender_message(
                &sender,
                &target,
                "one",
                "recovery original",
                Priority::Normal,
                AuthorityClass::RuntimeInstruction,
                None,
            )
            .await;
            assert!(prepared.message.turn_id.is_none());
            let commit = target
                .enqueue_attempt(&prepared.message, Some(&prepared.record), None, None)
                .await
                .unwrap();
            target.apply_transition_commit(commit).await;
            prepared.message.id
        } else {
            send(&sender, &target, "one", "recovery original").await
        };
        let second = send_with_priority_authority_and_correlation(
            &sender,
            &target,
            "two",
            "recovery follower",
            Priority::Normal,
            AuthorityClass::RuntimeInstruction,
            Some("recovery-follower-correlation".into()),
        )
        .await;
        let original_route = target
            .storage()
            .read_message_by_id(&second)
            .unwrap()
            .unwrap()
            .routing_context
            .unwrap();
        let anchor = start_without_provider(&target).await;
        assert_eq!(anchor.turn_id.is_none(), null_ingress);
        let ingress_turn_id = anchor.turn_id.clone();
        assert!(target
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .unwrap());
        let original = target.storage().read_recent_turns(1).unwrap().remove(0);
        let original_batch = original.idle_input_batch.clone().unwrap();
        let execution = target
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("batch-target")
            .unwrap()
            .unwrap();
        assert_eq!(
            execution.attempts[original_batch.activation_id.as_ref().unwrap()].turn_id,
            ingress_turn_id
        );
        drop(target);
        drop(sender);
        drop(host);
        recover_stopped_target(home.path(), provider.clone()).await;
        let reopen = || {
            RuntimeHandle::new(
                "batch-target",
                home.path().join("batch-target-state"),
                home.path().join("batch-workspace"),
                "http://127.0.0.1:7878".into(),
                provider.clone(),
                "fixture".into(),
                context_config(),
            )
            .unwrap()
        };
        let target = reopen();
        target.recover_scheduler_bootstrap_claims().await.unwrap();
        let anchor = start_without_provider(&target).await;
        assert_eq!(anchor.turn_id, ingress_turn_id);
        {
            use crate::domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding};
            let mutation = target.begin_mutation().await.unwrap();
            let inputs = mutation.capture_input_evidence().unwrap();
            let protocol = inputs.execution_state_if_initialized().unwrap().unwrap();
            let provenance = mutation
                .state
                .current_execution_binding
                .as_ref()
                .unwrap()
                .admission_provenance
                .as_ref();
            let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = provenance
            else {
                panic!("recovery must have canonical admission");
            };
            let selected = RuntimeHandle::canonical_idle_batch_replay_source(
                &inputs,
                Some(&protocol),
                &anchor,
                provenance,
            )
            .unwrap()
            .unwrap();
            assert_eq!(selected.0, original.turn_id);
            let prior_id = protocol.attempts[activation_id]
                .recovery_of_attempt_id
                .as_ref()
                .unwrap();
            for fault in 0..4 {
                let mut changed = protocol.clone();
                let prior = changed.attempts.get_mut(prior_id).unwrap();
                match fault {
                    0 => prior.state = ExecutionAttemptState::Open,
                    1 => prior.source_message_id = Some("unrelated-source".into()),
                    2 => prior.agent_id = "another-agent".into(),
                    3 => {
                        prior.binding = ExecutionBinding::AgentLifecycle {
                            agent_id: "another-owner".into(),
                        }
                    }
                    _ => unreachable!(),
                }
                assert!(RuntimeHandle::canonical_idle_batch_replay_source(
                    &inputs,
                    Some(&changed),
                    &anchor,
                    provenance,
                )
                .unwrap()
                .is_none());
            }
        }
        assert!(target
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .unwrap());
        let recovered = target.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(
            recovered.replay.as_ref().unwrap().source_turn_id,
            original.turn_id
        );
        assert_eq!(recovered.input_message_ids, [first.clone(), second.clone()]);
        assert_eq!(
            recovered.idle_input_batch.as_ref().unwrap().items,
            original_batch.items
        );
        drop(target);
        recover_stopped_target(home.path(), provider.clone()).await;
        let target = reopen();
        target.recover_scheduler_bootstrap_claims().await.unwrap();
        let runtime_db = target.inner.runtime_db.clone();
        let expected_inputs = [first.clone(), second.clone()];
        let expected_items = original_batch.items.clone();
        let original_turn_id = original.turn_id.clone();
        let source_turn_id = recovered.turn_id.clone();
        let predecessor = recovered
            .idle_input_batch
            .as_ref()
            .unwrap()
            .activation_id
            .clone()
            .unwrap();
        // Inspect the real live replay after admission, before the model responds.
        // Manually starting it here would consume the scheduler's model input.
        *provider.before_complete.lock().await = Some(Box::new(move || {
            let state = runtime_db
                .agent_states()
                .latest("batch-target")
                .unwrap()
                .unwrap();
            let replay = runtime_db
                .turn_records()
                .by_id(
                    Some("batch-target"),
                    state.current_turn_id.as_ref().unwrap(),
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                replay.replay.as_ref().unwrap().source_turn_id,
                source_turn_id
            );
            assert_eq!(replay.input_message_ids, expected_inputs);
            let batch = replay.idle_input_batch.as_ref().unwrap();
            assert_eq!(batch.items, expected_items);
            let activation_id = batch.activation_id.as_ref().unwrap();
            let execution = runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized("batch-target")
                .unwrap()
                .unwrap();
            let attempt = &execution.attempts[activation_id];
            assert_eq!(
                attempt.recovery_of_attempt_id.as_deref(),
                Some(predecessor.as_str())
            );
            for attempt in execution.attempts.values() {
                assert_eq!(attempt.turn_id, ingress_turn_id);
            }
            let resolved = runtime_db
                .agent_message_deliveries()
                .resolve_reply_context(
                    "batch-target",
                    Some(&replay.turn_id),
                    Some(&expected_inputs[1]),
                    &sender_id,
                    false,
                )
                .unwrap()
                .unwrap();
            assert_eq!(&resolved.activation_id, activation_id);
            assert_eq!(resolved.turn_id, replay.turn_id);
            assert_eq!(resolved.routing_context, original_route);
            assert_eq!(
                resolved.routing_context.correlation_id.as_deref(),
                Some("recovery-follower-correlation")
            );
            assert_eq!(
                resolved.routing_context.sender_agent_id.as_deref(),
                Some(sender_id.as_str())
            );
            assert_eq!(
                resolved.routing_context.reply_to_agent_id.as_deref(),
                Some(sender_id.as_str())
            );
            for foreign_turn in [
                original_turn_id.as_str(),
                source_turn_id.as_str(),
                "foreign-turn",
            ] {
                assert!(runtime_db
                    .agent_message_deliveries()
                    .resolve_reply_context(
                        "batch-target",
                        Some(foreign_turn),
                        Some(&expected_inputs[1]),
                        &sender_id,
                        false,
                    )
                    .is_err());
            }
        }));
        process_one(&target).await;
        provider.before_complete.lock().await.take();
        let final_turn = target.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(
            final_turn.replay.as_ref().unwrap().source_turn_id,
            recovered.turn_id
        );
        assert_eq!(final_turn.input_message_ids, [first, second]);
        assert_eq!(
            final_turn.idle_input_batch.unwrap().items,
            original_batch.items
        );
        assert_eq!(
            final_turn.terminal.unwrap().kind,
            TurnTerminalKind::Completed
        );
        assert_eq!(provider.requests.lock().await.len(), 1);
    }
}

#[tokio::test]
async fn p1_terminal_replay_lineage_rejects_missing_foreign_cycles_and_overflow() {
    let (_home, _host, sender, target, _) = fixture().await;
    send(&sender, &target, "one", "terminal replay lineage").await;
    let anchor = start_without_provider(&target).await;
    let original = target.storage().read_recent_turns(1).unwrap().remove(0);
    let owner = original.effective_owner();
    let ingress_turn_id = anchor.turn_id.clone().unwrap();
    let queue = target
        .runtime_db()
        .queue_entries()
        .latest(&anchor.id)
        .unwrap()
        .unwrap();
    let mut intermediate = original.clone();
    intermediate.turn_id = crate::ids::turn_id();
    intermediate.replay = Some(crate::types::TurnReplayProvenance {
        source_message_id: anchor.id.clone(),
        source_turn_id: ingress_turn_id.clone(),
        reason: "interrupted_queue_claim_reentry".into(),
        prior_terminal: None,
    });
    target.storage().append_turn(&intermediate).unwrap();
    let mut terminal = intermediate.clone();
    terminal.turn_id = crate::ids::turn_id();
    terminal.replay.as_mut().unwrap().source_turn_id = intermediate.turn_id.clone();
    terminal.terminal = Some(crate::types::TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    });
    assert_eq!(
        canonical_matching_terminal_turn(
            target.runtime_db(),
            &queue,
            &anchor,
            &owner,
            Some(&terminal),
        )
        .unwrap()
        .unwrap()
        .turn_id,
        terminal.turn_id
    );
    for mutation in [
        "missing",
        "agent",
        "source",
        "trigger",
        "owner",
        "cycle",
        "terminal_agent",
        "terminal_owner",
    ] {
        let mut prior = intermediate.clone();
        prior.turn_id = crate::ids::turn_id();
        let mut candidate = terminal.clone();
        candidate.replay.as_mut().unwrap().source_turn_id = prior.turn_id.clone();
        match mutation {
            "agent" => {
                prior.agent_id = "foreign-agent".into();
                prior.input_message_ids.clear();
            }
            "source" => prior.replay.as_mut().unwrap().source_message_id = "foreign-message".into(),
            "trigger" => {
                prior.trigger.as_mut().unwrap().message_id = Some("foreign-message".into())
            }
            "owner" => prior.owner = Some(TurnOwner::Command),
            "cycle" => prior.replay.as_mut().unwrap().source_turn_id = prior.turn_id.clone(),
            "terminal_agent" => candidate.agent_id = "foreign-agent".into(),
            "terminal_owner" => candidate.owner = Some(TurnOwner::Command),
            "missing" => {}
            _ => unreachable!(),
        }
        if mutation != "missing" {
            target.storage().append_turn(&prior).unwrap();
        }
        assert!(
            canonical_matching_terminal_turn(
                target.runtime_db(),
                &queue,
                &anchor,
                &owner,
                Some(&candidate),
            )
            .unwrap()
            .is_none(),
            "{mutation}"
        );
    }
    let mut source_turn_id = ingress_turn_id;
    for _ in 0..64 {
        let mut prior = intermediate.clone();
        prior.turn_id = crate::ids::turn_id();
        prior.replay.as_mut().unwrap().source_turn_id = source_turn_id;
        source_turn_id = prior.turn_id.clone();
        target.storage().append_turn(&prior).unwrap();
    }
    terminal.replay.as_mut().unwrap().source_turn_id = source_turn_id;
    assert!(canonical_matching_terminal_turn(
        target.runtime_db(),
        &queue,
        &anchor,
        &owner,
        Some(&terminal),
    )
    .unwrap()
    .is_none());
}

#[tokio::test]
async fn p1_claim_only_replay_without_source_turn_does_not_admit_a_batch() {
    for operator in [true, false] {
        let (home, host, sender, target, provider) = fixture().await;
        let first = if operator {
            let mut message = MessageEnvelope::new(
                "batch-target",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "operator claim interrupted before turn start".into(),
                },
            );
            message.turn_id = Some(crate::ids::turn_id());
            target.enqueue(message).await.unwrap().id
        } else {
            send(
                &sender,
                &target,
                "one",
                "peer claim interrupted before turn start",
            )
            .await
        };
        let scheduled = claim_model_input(&target).await;
        assert_eq!(scheduled.message.id, first);
        let Some(ExecutionAdmissionProvenance::Canonical {
            activation_id: predecessor,
            ..
        }) = scheduled.dispatch_plan.execution_admission_provenance
        else {
            panic!("claim-only recovery requires a canonical attempt");
        };
        let source_turn_id = scheduled.message.turn_id;
        if let Some(source_turn_id) = &source_turn_id {
            assert!(target
                .storage()
                .read_turn_by_id(source_turn_id)
                .unwrap()
                .is_none());
        }
        drop(target);
        drop(host);
        let target = RuntimeHandle::new(
            "batch-target",
            home.path().join("batch-target-state"),
            home.path().join("batch-workspace"),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "fixture".into(),
            context_config(),
        )
        .unwrap();
        target.recover_scheduler_bootstrap_claims().await.unwrap();
        let second = send(
            &sender,
            &target,
            "two",
            "must remain queued without a snapshot",
        )
        .await;
        let scheduled = claim_model_input(&target).await;
        assert_eq!(scheduled.message.id, first);
        process_scheduled(&target, scheduled).await;
        let recovered = target.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(
            recovered
                .replay
                .as_ref()
                .map(|replay| replay.source_turn_id.as_str()),
            source_turn_id.as_deref()
        );
        if let Some(replay) = recovered.replay.as_ref() {
            assert_eq!(replay.source_message_id, first);
            assert_eq!(replay.reason, "interrupted_queue_claim_reentry");
            assert!(target
                .storage()
                .read_turn_by_id(&replay.source_turn_id)
                .unwrap()
                .is_none());
        }
        let execution = target
            .runtime_db()
            .transitions()
            .load_execution_protocol_state_if_initialized("batch-target")
            .unwrap()
            .unwrap();
        let successor = execution
            .attempts
            .values()
            .find(|attempt| attempt.recovery_of_attempt_id.as_deref() == Some(predecessor.as_str()))
            .expect("claim-only recovery retains its canonical predecessor");
        assert_eq!(
            execution.attempts[&predecessor].state,
            crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
        );
        assert_eq!(successor.binding, execution.attempts[&predecessor].binding);
        assert_eq!(successor.source_message_id.as_deref(), Some(first.as_str()));
        assert_eq!(recovered.input_message_ids, [first.clone()]);
        assert!(recovered.idle_input_batch.is_none());
        assert_eq!(
            recovered.terminal.as_ref().unwrap().kind,
            TurnTerminalKind::Completed
        );
        assert_eq!(provider.requests.lock().await.len(), 1);
        assert_eq!(
            target
                .inner
                .runtime_db
                .queue_entries()
                .latest(&first)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Processed
        );
        assert_eq!(
            target
                .inner
                .runtime_db
                .queue_entries()
                .latest(&second)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
    }
}

#[tokio::test]
async fn p1_recovery_rejects_unqualified_snapshot_admission_drift() {
    use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;

    for invalid_evidence in ["activation", "owner", "input"] {
        let (home, host, sender, target, provider) = fixture().await;
        let first = send(&sender, &target, "one", "original request").await;
        let second = send(&sender, &target, "two", "already attached follower").await;
        let anchor = start_without_provider(&target).await;
        assert!(target
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .unwrap());
        let mut source = target.storage().read_recent_turns(1).unwrap().remove(0);
        match invalid_evidence {
            "activation" => {
                source.idle_input_batch.as_mut().unwrap().activation_id =
                    Some("foreign-attempt".into());
            }
            "owner" => source.owner = Some(TurnOwner::Command),
            "input" => source.input_message_ids.retain(|id| id != &second),
            _ => unreachable!(),
        }
        // This deliberately bypasses the qualified writer on an existing
        // projection. It is drift, not a qualified semantic-invalid snapshot.
        let database = target.runtime_db().clone();
        assert_eq!(
            database
                .connection()
                .unwrap()
                .execute(
                    "UPDATE turn_records SET payload_json = ?1 WHERE turn_id = ?2",
                    rusqlite::params![serde_json::to_string(&source).unwrap(), source.turn_id],
                )
                .unwrap(),
            1
        );
        let entries =
            [&first, &second].map(|id| database.queue_entries().latest(id).unwrap().unwrap());
        drop(target);
        drop(sender);
        drop(host);
        let error = RuntimeHandle::new(
            "batch-target",
            home.path().join("batch-target-state"),
            home.path().join("batch-workspace"),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "fixture".into(),
            context_config(),
        )
        .err()
        .expect("unqualified canonical drift must fail closed before recovery");
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::ProjectionChanged(_))
            ),
            "{invalid_evidence}: {error:#}"
        );
        for (id, expected) in [&first, &second].into_iter().zip(entries) {
            assert_eq!(
                database.queue_entries().latest(id).unwrap().unwrap(),
                expected
            );
        }
        assert!(provider.requests.lock().await.is_empty());
    }
}

// Build an independent pre-Input-qualification database. Retain the source's
// original Current proofs separately: Input's fixed upgrade cannot supply them.
// Never reset/adopt the source projections or requalify changed Queue/Event data.
fn legacy_replay_home(
    database: &crate::runtime_db::RuntimeDb,
    source: &TurnRecord,
) -> Result<TempDir> {
    legacy_recovery_home(database, "batch-target-state", "batch-workspace", |tx| {
        // The foreign owner precedes production qualification cutover.
        assert_eq!(
            tx.execute(
                "UPDATE turn_records
                     SET owner_kind = 'command', owner_id = NULL, payload_json = ?1
                     WHERE turn_id = ?2",
                rusqlite::params![serde_json::to_string(source)?, source.turn_id],
            )?,
            1
        );
        Ok(())
    })
}

pub(super) fn legacy_recovery_home(
    database: &crate::runtime_db::RuntimeDb,
    state_directory: &str,
    workspace_directory: &str,
    amend: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<()>,
) -> Result<TempDir> {
    let home = legacy_recovery_home_with_current_upgrade(
        database,
        state_directory,
        workspace_directory,
        amend,
    )?;
    let mut connection = rusqlite::Connection::open(
        home.path()
            .join(state_directory)
            .join(".holon/state/runtime.sqlite"),
    )?;
    connection.execute(
        "ATTACH DATABASE ?1 AS original",
        [database.path().to_str().unwrap()],
    )?;
    // Input-only history changes retain the original Current writer's proof,
    // including negative facts; changed Queue/Event bytes still fail its digest.
    let tx = connection.transaction()?;
    tx.execute_batch(
        "DELETE FROM main.current_read_qualification;
         DELETE FROM main.current_read_revisions;",
    )?;
    for table in ["current_read_revisions", "current_read_qualification"] {
        tx.execute_batch(&format!(
            "INSERT INTO main.\"{table}\" SELECT * FROM original.\"{table}\";"
        ))?;
    }
    tx.commit()?;
    Ok(home)
}

fn legacy_recovery_home_with_current_upgrade(
    database: &crate::runtime_db::RuntimeDb,
    state_directory: &str,
    workspace_directory: &str,
    amend: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<()>,
) -> Result<TempDir> {
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};

    let home = tempdir()?;
    let state_dir = home.path().join(state_directory).join(".holon/state");
    std::fs::create_dir_all(&state_dir)?;
    std::fs::create_dir_all(home.path().join(workspace_directory))?;
    let mut connection = rusqlite::Connection::open(state_dir.join("runtime.sqlite"))?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 89)
    {
        apply_migration(&mut connection, migration)?;
    }
    let tables = {
        let mut statement = connection.prepare(
            "SELECT name FROM main.sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
               AND name NOT IN ('schema_migrations', 'schema_migration_baselines')
             ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    connection.execute(
        "ATTACH DATABASE ?1 AS original",
        [database.path().to_str().unwrap()],
    )?;
    let tx = connection.transaction()?;
    for table in tables {
        let table = table.replace('"', "\"\"");
        let source_columns = {
            let mut statement = tx.prepare(&format!("PRAGMA original.table_info(\"{table}\")"))?;
            let names = statement.query_map([], |row| row.get::<_, String>(1))?;
            names.collect::<rusqlite::Result<BTreeSet<_>>>()?
        };
        let columns = {
            let mut statement = tx.prepare(&format!("PRAGMA main.table_info(\"{table}\")"))?;
            let names = statement.query_map([], |row| row.get::<_, String>(1))?;
            let names = names.collect::<rusqlite::Result<Vec<_>>>()?;
            anyhow::ensure!(
                names.iter().all(|name| source_columns.contains(name)),
                "legacy replay source lacks a column required by {table}"
            );
            names
                .into_iter()
                .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(", ")
        };
        // The schema-89 copy has no qualification tables. Post-cutover
        // sentinels would suppress the original migration's one-time capture.
        let historical_rows = if table == "runtime_metadata" {
            " WHERE key NOT IN ('episode_continuity/v1/upgrade_boundary', \
              'input_evidence/v1/fixed_upgrade')"
        } else {
            ""
        };
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.\"{table}\" ({columns})
             SELECT {columns} FROM original.\"{table}\"{historical_rows};"
        ))?;
    }
    // Amend only the independent historical database, before qualification.
    amend(&tx)?;
    tx.commit()?;
    for migration in MIGRATIONS.iter().filter(|migration| migration.version > 89) {
        apply_migration(&mut connection, migration)?;
    }
    Ok(home)
}

#[tokio::test]
async fn p1_recovery_quarantines_qualified_foreign_owner_snapshot_admission() {
    let (_original_home, host, sender, target, provider) = fixture().await;
    let first = send(&sender, &target, "one", "original request").await;
    let second = send(&sender, &target, "two", "already attached follower").await;
    let anchor = start_without_provider(&target).await;
    assert!(target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    let mut source = target.storage().read_recent_turns(1).unwrap().remove(0);
    source.owner = Some(TurnOwner::Command);
    let database = target.runtime_db().clone();
    drop(target);
    drop(sender);
    drop(host);
    let home = legacy_replay_home(&database, &source)
        .expect("foreign-owner canonical fixture must precede qualification cutover");
    recover_stopped_target(home.path(), provider.clone()).await;
    let target = RuntimeHandle::new(
        "batch-target",
        home.path().join("batch-target-state"),
        home.path().join("batch-workspace"),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "fixture".into(),
        context_config(),
    )
    .expect("qualified semantic-invalid owner must reach scheduler quarantine");
    target.recover_scheduler_bootstrap_claims().await.unwrap();
    // A foreign-owner attached turn is deterministic corrupted
    // local state: the claim must quarantine the queue head
    // instead of failing the loop, before a replay turn or any
    // source admission can be committed (#3607).
    let turns = target.storage().read_recent_turns(2).unwrap();
    let entries = [&first, &second].map(|id| {
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(id)
            .unwrap()
            .unwrap()
    });
    let settlements = target
        .inner
        .runtime_db
        .task_result_settlements()
        .unsettled_for_agent("batch-target", 8)
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            scheduler_executor::SchedulerDecisionExecutor::new(&target)
                .poll()
                .await
                .unwrap(),
            scheduler_executor::RunLoopPoll::AuthorityBlocked
        ));
    }
    assert!(matches!(
        scheduler_executor::SchedulerDecisionExecutor::new(&target)
            .poll()
            .await
            .unwrap(),
        scheduler_executor::RunLoopPoll::Idle
    ));
    let events = target.storage().read_recent_events(usize::MAX).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "scheduling_advisory"
            && event.data["kind"] == "interrupted_replay_source_invalid"
            && event.data["evidence"]
                .to_string()
                .contains("violation=attachment_invalid_or_ambiguous")
    }));
    assert!(events.iter().any(|event| {
        event.kind == "scheduler_queue_head_quarantined"
            && event.data["message_id"] == first
            && event.data["reason"] == "interrupted_replay_source_invalid"
    }));
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&first)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Quarantined
    );
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].turn_id, source.turn_id);
    assert_eq!(target.storage().read_recent_turns(2).unwrap(), turns);
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&second)
            .unwrap()
            .unwrap(),
        entries[1]
    );
    assert_eq!(entries[1].status, QueueEntryStatus::Interjected);
    assert_eq!(
        target
            .inner
            .runtime_db
            .task_result_settlements()
            .unsettled_for_agent("batch-target", 8)
            .unwrap(),
        settlements
    );
    assert!(provider.requests.lock().await.is_empty());
}

#[tokio::test]
async fn p1_recovery_rejects_missing_or_foreign_snapshot_admission() {
    for invalid_evidence in [
        "prior_id",
        "prior_state",
        "prior_agent",
        "prior_source",
        "prior_binding",
        "source_turn",
        "prior_none",
        "replay_reason",
        "replay_message",
        "source_agent",
        "source_record_id",
        "source_owner",
        "source_trigger",
        "batch_anchor",
        "batch_first",
        "item_count",
        "rendered_bytes",
        "max_bytes",
        "global_bytes",
        "duplicate_item",
        "source_ref",
        "message_missing",
        "message_agent",
        "queue_missing",
        "queue_agent",
        "queue_status",
    ] {
        let (home, host, sender, target, provider) = fixture().await;
        let first = send(&sender, &target, "one", "original request").await;
        let second = send(&sender, &target, "two", "already attached follower").await;
        let anchor = start_without_provider(&target).await;
        assert!(target
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .unwrap());
        let mut source = target.storage().read_recent_turns(1).unwrap().remove(0);
        drop(target);
        drop(sender);
        drop(host);
        recover_stopped_target(home.path(), provider.clone()).await;
        let target = RuntimeHandle::new(
            "batch-target",
            home.path().join("batch-target-state"),
            home.path().join("batch-workspace"),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "fixture".into(),
            context_config(),
        )
        .unwrap();
        target.recover_scheduler_bootstrap_claims().await.unwrap();
        let mut anchor = start_without_provider(&target).await;
        assert_eq!(anchor.id, first);
        let mut recovered = target.storage().read_recent_turns(1).unwrap().remove(0);
        let mut forged_predecessor_rejection = None;
        let mut tampered_attempt_id = None;
        let mut historical_home = None;
        if invalid_evidence == "message_missing" {
            // The historical Turn must reference a real, natively admitted
            // Message during bootstrap; remove it only after qualification.
            let mut message = MessageEnvelope::new(
                "batch-target",
                MessageKind::InternalFollowup,
                MessageOrigin::System {
                    subsystem: "fixture".into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "missing-message fixture".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::RuntimeSystem,
                AdmissionContext::RuntimeOwned,
            );
            message.id = "msg_deadbeef".into();
            let commit = target
                .enqueue_attempt(&message, None, None, None)
                .await
                .unwrap();
            assert!(commit.applied);
            target.apply_transition_commit(commit).await;
        }
        if invalid_evidence.starts_with("prior_") {
            let activation_id = target
                .inner
                .agent
                .lock()
                .await
                .state
                .current_execution_binding
                .as_ref()
                .unwrap()
                .activation_id
                .clone()
                .unwrap();
            let mut execution = target
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized("batch-target")
                .unwrap()
                .unwrap();
            let prior_id = execution.attempts[&activation_id]
                .recovery_of_attempt_id
                .clone()
                .unwrap();
            let corrupt = if matches!(invalid_evidence, "prior_id" | "prior_none") {
                execution.attempts.get_mut(&activation_id).unwrap()
            } else {
                execution.attempts.get_mut(&prior_id).unwrap()
            };
            match invalid_evidence {
                "prior_id" => corrupt.recovery_of_attempt_id = Some("foreign-attempt".into()),
                "prior_none" => corrupt.recovery_of_attempt_id = None,
                "prior_state" => {
                    corrupt.state =
                        crate::domain::execution_protocol::ExecutionAttemptState::Settled;
                }
                "prior_agent" => corrupt.agent_id = "foreign-agent".into(),
                "prior_source" => corrupt.source_message_id = Some(second.clone()),
                "prior_binding" => {
                    corrupt.binding = crate::domain::execution_protocol::ExecutionBinding::Command;
                }
                _ => unreachable!(),
            }
            tampered_attempt_id = Some(corrupt.attempt_id.clone());
            // Forge only validator arguments for the semantic negative. The
            // separately tampered durable row must still fail qualification.
            let attempt = &execution.attempts[&activation_id];
            forged_predecessor_rejection = Some(
                input_consumption::validate_idle_recovery_predecessor(
                    &anchor,
                    attempt,
                    attempt
                        .recovery_of_attempt_id
                        .as_ref()
                        .and_then(|id| execution.attempts.get(id)),
                    source.idle_input_batch.as_ref().unwrap(),
                )
                .unwrap_err(),
            );
            let corrupt = &execution.attempts[tampered_attempt_id.as_ref().unwrap()];
            assert_eq!(
                target
                    .inner
                    .runtime_db
                    .connection()
                    .unwrap()
                    .execute(
                        "UPDATE execution_protocol_attempts SET payload_json = ?1
                         WHERE agent_id = 'batch-target' AND attempt_id = ?2",
                        rusqlite::params![
                            serde_json::to_string(corrupt).unwrap(),
                            corrupt.attempt_id
                        ],
                    )
                    .unwrap(),
                1
            );
        } else if invalid_evidence == "source_turn" {
            // A forged selector is not the canonical replay predecessor.
            // Keep the accepted Turn and its input assignments unchanged.
            anchor.source_refs.insert(
                "replay_source_turn_id".into(),
                format!("{}-foreign", source.turn_id),
            );
        } else if invalid_evidence == "replay_reason" {
            anchor.source_refs.insert(
                "replay_reason".into(),
                "sentinel-secret?token=credential".into(),
            );
        } else {
            source = target
                .storage()
                .read_turn_by_id(&source.turn_id)
                .unwrap()
                .unwrap();
            let source_key = source.turn_id.clone();
            let mut attachment_message = (invalid_evidence == "message_agent").then(|| {
                target
                    .storage()
                    .read_message_by_id(&second)
                    .unwrap()
                    .unwrap()
            });
            let mut attachment_queue = matches!(invalid_evidence, "queue_agent" | "queue_status")
                .then(|| {
                    target
                        .inner
                        .runtime_db
                        .queue_entries()
                        .latest(&second)
                        .unwrap()
                        .unwrap()
                });
            let mut amend = |tx: &rusqlite::Transaction<'_>| -> Result<()> {
                let batch = source.idle_input_batch.as_mut().unwrap();
                match invalid_evidence {
                    "replay_message" => {
                        recovered.replay.as_mut().unwrap().source_message_id = second.clone()
                    }
                    "source_agent" => source.agent_id = "sentinel-secret?token=credential".into(),
                    "source_record_id" => {
                        source.turn_id = "sentinel-secret?token=credential".into()
                    }
                    "source_owner" => source.owner = Some(TurnOwner::Command),
                    "source_trigger" => {
                        source.trigger.as_mut().unwrap().message_id = Some(second.clone())
                    }
                    "batch_anchor" => batch.anchor_message_id = second.clone(),
                    "batch_first" => batch.items[0].message_id = second.clone(),
                    "item_count" => {
                        batch.items.resize(9, batch.items[0].clone());
                        batch.rendered_bytes = input_consumption::render_batch(batch).len();
                    }
                    "rendered_bytes" => batch.rendered_bytes += 1,
                    "max_bytes" => batch.max_bytes = batch.rendered_bytes - 1,
                    "global_bytes" => batch.max_bytes = input_consumption::INPUT_BATCH_BYTES + 1,
                    "duplicate_item" => {
                        // Reject the duplicate before reading any follower Queue.
                        batch.items.insert(1, batch.items[0].clone());
                        batch.rendered_bytes = input_consumption::render_batch(batch).len();
                    }
                    "source_ref" => {
                        batch.items[1].source_ref = "sentinel-secret?token=credential".into();
                        batch.rendered_bytes = input_consumption::render_batch(batch).len();
                    }
                    "message_missing" => {
                        let missing = "msg_deadbeef".to_string();
                        batch.items[1].message_id = missing.clone();
                        batch.items[1].source_ref = format!("message:{missing}");
                        source.input_message_ids.push(missing);
                        batch.rendered_bytes = input_consumption::render_batch(batch).len();
                    }
                    "message_agent" => {
                        let message = attachment_message.as_mut().unwrap();
                        message.agent_id = "foreign-agent".into();
                        assert_eq!(
                            tx.execute(
                                "UPDATE messages SET payload_json = ?1 WHERE evidence_id = ?2",
                                rusqlite::params![serde_json::to_string(&message)?, second],
                            )?,
                            1
                        );
                    }
                    "queue_missing" => {
                        assert_eq!(
                            tx.execute(
                                "DELETE FROM queue_entries WHERE message_id = ?1",
                                [&second]
                            )?,
                            1
                        );
                    }
                    "queue_status" => {
                        // A consumed follower cannot be requeued by its writer.
                        // Qualify this malformed history only in the legacy copy.
                        let queue = attachment_queue.as_mut().unwrap();
                        assert_eq!(queue.status, QueueEntryStatus::Interjected);
                        queue.status = QueueEntryStatus::Queued;
                        assert_eq!(
                            tx.execute(
                                "UPDATE queue_entries SET status = 'queued', payload_json = ?1
                                 WHERE message_id = ?2",
                                rusqlite::params![serde_json::to_string(&queue)?, second],
                            )?,
                            1
                        );
                    }
                    "queue_agent" => {
                        let queue = attachment_queue.as_mut().unwrap();
                        queue.agent_id = "foreign-agent".into();
                        assert_eq!(
                            tx.execute(
                                "UPDATE queue_entries SET payload_json = ?1 WHERE message_id = ?2",
                                rusqlite::params![serde_json::to_string(&queue)?, second],
                            )?,
                            1
                        );
                    }
                    _ => unreachable!(),
                }
                for (key, record) in [(&source_key, &source), (&recovered.turn_id, &recovered)] {
                    let owner = record.effective_owner();
                    let (owner_kind, owner_id) = owner.index_parts();
                    assert_eq!(
                        tx.execute(
                            "UPDATE turn_records SET owner_kind = ?1, owner_id = ?2,
                             replay_source_turn_id = ?3, payload_json = ?4 WHERE turn_id = ?5",
                            rusqlite::params![
                                owner_kind,
                                owner_id,
                                record
                                    .replay
                                    .as_ref()
                                    .map(|replay| replay.source_turn_id.as_str()),
                                serde_json::to_string(record)?,
                                key,
                            ],
                        )?,
                        1
                    );
                }
                Ok(())
            };
            if matches!(
                invalid_evidence,
                "source_agent"
                    | "source_record_id"
                    | "message_agent"
                    | "queue_missing"
                    | "queue_agent"
            ) {
                // Deliberately bypass the qualified writer: this negative fixture
                // tests the captured reader's rejection of unacknowledged drift.
                let connection = target.inner.runtime_db.connection().unwrap();
                let tx = connection.unchecked_transaction().unwrap();
                amend(&tx).unwrap();
                tx.commit().unwrap();
            } else {
                let home = if invalid_evidence == "queue_status" {
                    // Retain the real fixed-cut Current upgrade for this Queue;
                    // the original writer qualified its old interjected bytes.
                    legacy_recovery_home_with_current_upgrade(
                        &target.inner.runtime_db,
                        "batch-target-state",
                        "batch-workspace",
                        amend,
                    )
                } else {
                    legacy_recovery_home(
                        &target.inner.runtime_db,
                        "batch-target-state",
                        "batch-workspace",
                        amend,
                    )
                };
                historical_home = Some(home.unwrap());
            }
        }
        let target = if let Some(home) = historical_home.as_ref() {
            let in_flight = target.agent_state().await.unwrap();
            assert_eq!(in_flight.status, AgentStatus::AwakeRunning);
            assert!(in_flight.current_run_id.is_some());
            drop(target);
            let target = RuntimeHandle::new(
                "batch-target",
                home.path().join("batch-target-state"),
                home.path().join("batch-workspace"),
                "http://127.0.0.1:7878".into(),
                provider.clone(),
                "fixture".into(),
                context_config(),
            )
            .unwrap();
            isolate_test_user_home(&target);
            // Bootstrap clears the running posture even though this historical
            // fixture retains the same admitted Attempt and Turn. Restore only
            // that posture through its original State writer, not a new claim.
            let mut mutation = target.begin_mutation().await.unwrap();
            assert_eq!(
                mutation.state.current_execution_binding,
                in_flight.current_execution_binding
            );
            assert_eq!(mutation.state.current_turn_id, in_flight.current_turn_id);
            assert!(mutation.state.current_run_id.is_none());
            mutation.state.status = in_flight.status;
            mutation.state.current_run_id = in_flight.current_run_id;
            mutation.persist_state(target.storage()).await.unwrap();
            drop(mutation);
            target
        } else {
            target
        };
        if invalid_evidence == "message_missing" {
            // Deliberately delete without the retention writer/tombstone:
            // the live snapshot must reject absence, not re-ACK corruption.
            target
                .inner
                .runtime_db
                .transaction(|tx| {
                    assert_eq!(
                        tx.execute(
                            "DELETE FROM messages
                             WHERE agent_id = 'batch-target' AND evidence_id = 'msg_deadbeef'",
                            [],
                        )?,
                        1
                    );
                    Ok(())
                })
                .unwrap();
        }
        let follower_before = target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&second)
            .unwrap();
        let error = target
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .expect_err(invalid_evidence);
        let error = if let Some(rejection) = forged_predecessor_rejection {
            use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;
            if invalid_evidence == "prior_source" {
                // Moving an existing Attempt to another source invalidates
                // the qualified source inventory before its body is selected.
                assert_eq!(
                    error.to_string(),
                    "execution source changed without qualified writer",
                    "{invalid_evidence}: {error:#}"
                );
                assert!(!error.is::<InputEvidenceSnapshotError>());
            } else {
                assert_eq!(
                    error.downcast_ref::<InputEvidenceSnapshotError>(),
                    Some(&InputEvidenceSnapshotError::ProjectionChanged(
                        tampered_attempt_id.unwrap()
                    )),
                    "{invalid_evidence}: {error:#}"
                );
            }
            assert!(!error.is::<crate::runtime_error::IdleInputRecoveryRejection>());
            anyhow::Error::new(rejection)
        } else {
            error
        };
        use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;
        let attempt_turn_integrity =
            matches!(invalid_evidence, "source_agent" | "source_record_id");
        if attempt_turn_integrity {
            // Attempt relation validation rejects either canonical identity
            // mismatch before Input projection or attachment validation.
            assert_eq!(
                error.to_string(),
                "Attempt Turn has a different canonical scope",
                "{invalid_evidence}: {error:#}"
            );
            assert!(!error.is::<InputEvidenceSnapshotError>());
            assert!(!error.is::<crate::runtime_error::IdleInputRecoveryRejection>());
        }
        let input_integrity = match invalid_evidence {
            "message_agent" => Some(InputEvidenceSnapshotError::ProjectionChanged(
                second.clone(),
            )),
            // No canonical Message or tombstone exists to qualify this address.
            "message_missing" => Some(InputEvidenceSnapshotError::NeedsPreparation(
                "message:msg_deadbeef unavailable without tombstone".into(),
            )),
            _ => None,
        };
        if let Some(expected) = input_integrity.as_ref() {
            assert_eq!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(expected),
                "{invalid_evidence}: {error:#}"
            );
            assert!(!error.is::<crate::runtime_error::IdleInputRecoveryRejection>());
        }
        let current_integrity = match invalid_evidence {
            "queue_missing" => Some("Current absent address needs preparation"),
            "queue_agent" => Some("Current body needs original writer preparation"),
            _ => None,
        };
        if let Some(expected) = current_integrity {
            // The native Current boundary rejects live tampering before the
            // attachment predicate. This is not attachment semantic coverage.
            assert_eq!(error.to_string(), expected);
            assert!(!error.is::<crate::runtime_error::IdleInputRecoveryRejection>());
        }
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        if !attempt_turn_integrity && input_integrity.is_none() && current_integrity.is_none() {
            assert!(
                error.is::<crate::runtime_error::IdleInputRecoveryRejection>(),
                "{invalid_evidence}: {error:#}"
            );
            let (predicate, field) = match invalid_evidence {
                "activation" => ("activation_mismatch", "batch.activation_id"),
                "input" => (
                    "attachment_evidence_mismatch",
                    "attachment.source_membership",
                ),
                "prior_id" => ("predecessor_missing", "predecessor"),
                "prior_state" => ("predecessor_not_interrupted", "predecessor.state"),
                "prior_agent" => ("predecessor_agent_mismatch", "predecessor.agent"),
                "prior_source" => ("predecessor_source_mismatch", "predecessor.source_message"),
                "prior_binding" => ("predecessor_binding_mismatch", "predecessor.binding"),
                "source_turn" => ("replay_source_mismatch", "replay.source_selector"),
                "prior_none" => ("predecessor_missing", "attempt.recovery_of_attempt_id"),
                "replay_reason" => ("replay_reason_mismatch", "replay.reason"),
                "replay_message" => ("replay_message_mismatch", "replay.source_message"),
                "source_agent" => ("source_agent_mismatch", "source.agent"),
                "source_record_id" => ("replay_source_mismatch", "source.turn_id"),
                "source_owner" => ("owner_mismatch", "source.owner"),
                "source_trigger" => ("source_trigger_mismatch", "source.trigger.message"),
                "batch_anchor" => ("batch_anchor_mismatch", "batch.anchor_message"),
                "batch_first" => ("batch_anchor_mismatch", "batch.first_item"),
                "item_count" => ("batch_integrity_mismatch", "batch.item_count"),
                "rendered_bytes" => ("batch_integrity_mismatch", "batch.rendered_bytes"),
                "max_bytes" => ("batch_integrity_mismatch", "batch.max_bytes"),
                "global_bytes" => ("batch_integrity_mismatch", "batch.global_byte_budget"),
                "duplicate_item" => ("attachment_evidence_mismatch", "attachment.duplicate_item"),
                "source_ref" => ("attachment_evidence_mismatch", "attachment.source_ref"),
                "message_missing" => ("attachment_evidence_mismatch", "attachment.message_missing"),
                "message_agent" => ("attachment_evidence_mismatch", "attachment.message_agent"),
                "queue_missing" => ("attachment_evidence_mismatch", "attachment.queue_missing"),
                "queue_status" => ("attachment_evidence_mismatch", "attachment.queue_status"),
                _ => unreachable!(),
            };
            assert_eq!(
                descriptor.code,
                format!("idle_input_recovery_{predicate}"),
                "{invalid_evidence}: {error:#}"
            );
            assert_eq!(descriptor.safe_context["field"], field);
            assert_eq!(
                descriptor.domain,
                crate::runtime_error::RuntimeErrorDomain::Validation
            );
            assert!(!descriptor.retryable);
        }
        assert!(!serde_json::to_string(&descriptor)
            .unwrap()
            .contains("original request"));
        assert!(!serde_json::to_string(&descriptor)
            .unwrap()
            .contains("sentinel-secret"));
        let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(turn.input_message_ids, [first]);
        assert!(turn.idle_input_batch.is_none());
        assert_eq!(
            target
                .inner
                .runtime_db
                .queue_entries()
                .latest(&second)
                .unwrap(),
            follower_before
        );
        assert!(provider.requests.lock().await.is_empty());
    }
}

#[tokio::test]
async fn p1_changed_or_invalid_receiver_binding_never_falls_back_to_sender() {
    let (_home, _host, sender, target, provider) = fixture().await;
    let first = send(&sender, &target, "one", "ordinary anchor").await;
    let second = send(&sender, &target, "two", "bound follower").await;
    let scheduled = claim_model_input(&target).await;
    assert_eq!(scheduled.message.id, first);
    let mut message = target
        .storage()
        .read_message_by_id(&second)
        .unwrap()
        .unwrap();
    message.work_item_id = Some("invalid-receiver-owner".into());
    target
        .inner
        .runtime_db
        .transaction(|tx| crate::runtime_db::evidence::insert_message_evidence_tx(tx, &message))
        .unwrap();
    let error = try_process_scheduled(&target, scheduled).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
            Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(id))
                if id == &second
        ),
        "{error:#}"
    );
    assert!(provider.requests.lock().await.is_empty());
    let turn = target.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(turn.input_message_ids, [first]);
    assert_eq!(
        target
            .inner
            .runtime_db
            .queue_entries()
            .latest(&second)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
}
