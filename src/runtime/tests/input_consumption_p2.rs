//! Live inputs enter real provider requests only at bounded, paired safe points.
use super::super::*;
use super::support::*;
use crate::runtime::message_dispatch::message_text;
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementDisposition, TaskResultSettlementState,
};
use crate::runtime_db::transitions::AgentStateMutation;
use crate::types::{
    AdmissionContext, AgentMessageCallerContext, AgentMessageDeliveryState,
    AgentMessagePrincipalKind, AgentMessageSendRequest, QueueEntryRecord, WorkItemPlanStatus,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
enum Source {
    Operator,
    Message {
        sender: &'static str,
        valid_sender: bool,
    },
    Result {
        kind: TaskKind,
        same_turn: bool,
    },
}

#[derive(Clone)]
struct Injection {
    source: Source,
    owner: Option<String>,
    count: usize,
    body_bytes: usize,
}

#[derive(Clone, Copy)]
enum Reply {
    Final,
    CheckLiveReplyRoute,
    PairedTools,
    GatedTool,
    Interrupt,
    Fallback,
    WaitFinal,
    WaitSilent,
    Complete,
    Stop,
}

#[derive(Clone)]
struct Step {
    inputs: Vec<Injection>,
    reply: Reply,
}

#[derive(Default)]
struct LiveProvider {
    runtime: Mutex<Option<RuntimeHandle>>,
    steps: Mutex<Vec<Step>>,
    requests: Mutex<Vec<ProviderTurnRequest>>,
    sources: Mutex<Vec<String>>,
    attachment_fault: Mutex<Option<TransitionFaultPoint>>,
    before_attachment: Mutex<Option<serde_json::Value>>,
    interrupted: tokio::sync::Notify,
}

fn tool(id: impl Into<String>, name: &str, input: serde_json::Value) -> ModelBlock {
    ModelBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
        kind: crate::provider::ModelToolCallKind::Function,
        provider_data: None,
    }
}

#[async_trait]
impl AgentProvider for LiveProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let material = request.material.clone();
        let round = {
            let mut requests = self.requests.lock().await;
            requests.push(request);
            requests.len()
        };
        let runtime = self.runtime.lock().await.clone().unwrap();
        let step = self
            .steps
            .lock()
            .await
            .get(round - 1)
            .cloned()
            .unwrap_or(Step {
                inputs: Vec::new(),
                reply: Reply::Final,
            });
        let turn = latest_turn(&runtime);
        let batches_before = turn.live_input_batches.len();
        for injection in step.inputs {
            for index in 0..injection.count {
                let key = format!("p2-{round}-{}-{index}", self.sources.lock().await.len());
                let body = format!("{key}:{}", "x".repeat(injection.body_bytes));
                let id = match injection.source {
                    Source::Operator => {
                        let message = MessageEnvelope::new(
                            "default",
                            MessageKind::OperatorPrompt,
                            MessageOrigin::Operator {
                                actor_id: Some("p2-operator".into()),
                                actor_display_name: None,
                            },
                            AuthorityClass::OperatorInstruction,
                            Priority::Interject,
                            MessageBody::Text { text: body },
                        )
                        .with_admission(
                            MessageDeliverySurface::HttpControlPrompt,
                            AdmissionContext::ControlAuthenticated,
                        );
                        let id = message.id.clone();
                        runtime.enqueue(message).await.unwrap();
                        id
                    }
                    Source::Message {
                        sender,
                        valid_sender,
                    } => {
                        Box::pin(message(
                            &runtime,
                            injection.owner.as_deref(),
                            sender,
                            &key,
                            &body,
                            valid_sender,
                        ))
                        .await
                        .id
                    }
                    Source::Result { kind, same_turn } => {
                        Box::pin(task_result(
                            &runtime,
                            injection.owner.as_deref(),
                            &key,
                            &body,
                            kind,
                            if same_turn {
                                &turn.turn_id
                            } else {
                                "older-parent-turn"
                            },
                        ))
                        .await
                    }
                };
                self.sources.lock().await.push(id);
            }
        }
        // Delivery while the provider is in flight cannot attach to that request.
        assert_eq!(
            latest_turn(&runtime).live_input_batches.len(),
            batches_before
        );
        let blocks = match step.reply {
            Reply::Final => vec![ModelBlock::Text {
                text: "p2 done".into(),
            }],
            Reply::CheckLiveReplyRoute => {
                let source = self.sources.lock().await.last().cloned().unwrap();
                let batch = turn.live_input_batches.last().unwrap();
                let source_message = runtime.storage().read_message_by_id(&source)?.unwrap();
                let reply_source =
                    crate::wake_contract::agent_message_reply_reference(&source_message)
                        .unwrap_or(&source);
                if source_message.kind == MessageKind::TaskResult {
                    assert!(batch.ordinary_message_ids.is_empty());
                    assert!(batch
                        .items
                        .iter()
                        .any(|item| item.message_id == reply_source));
                    assert!(
                        runtime
                            .inner
                            .runtime_db
                            .agent_message_deliveries()
                            .resolve_reply_context(
                                "default",
                                Some(&turn.turn_id),
                                Some(&source),
                                "p2-peer",
                                false,
                            )
                            .is_err(),
                        "a task-result notification is not a reply route"
                    );
                } else {
                    assert_eq!(batch.ordinary_message_ids, [source.clone()]);
                }
                let original = runtime
                    .storage()
                    .read_message_by_id(reply_source)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    original.work_item_id.as_deref(),
                    turn.effective_owner().work_item_id(),
                );
                let resolved = runtime
                    .inner
                    .runtime_db
                    .agent_message_deliveries()
                    .resolve_reply_context(
                        "default",
                        Some(&turn.turn_id),
                        Some(reply_source),
                        "p2-peer",
                        false,
                    )
                    .unwrap()
                    .expect("a recovered activation must route its new live input");
                assert_eq!(resolved.activation_id, batch.activation_id);
                assert_eq!(resolved.turn_id, turn.turn_id);
                assert_eq!(resolved.routing_context, original.routing_context.unwrap());
                vec![ModelBlock::Text {
                    text: "p2 recovered reply route checked".into(),
                }]
            }
            Reply::PairedTools => (0..2)
                .map(|index| {
                    tool(
                        format!("p2-tool-{round}-{index}"),
                        "GetAgent",
                        serde_json::json!({}),
                    )
                })
                .collect(),
            Reply::GatedTool => vec![tool(
                format!("p2-gated-tool-{round}"),
                "ExecCommand",
                serde_json::json!({
                    "cmd": "touch p2-tool-ready; i=0; while [ ! -f p2-tool-release ] && [ \"$i\" -lt 500 ]; do sleep 0.01; i=$((i+1)); done; test -f p2-tool-release",
                    "workdir": runtime.workspace_root(),
                    "yield_time_ms": 120000,
                }),
            )],
            Reply::Interrupt => {
                self.interrupted.notify_one();
                std::future::pending().await
            }
            Reply::Fallback => {
                return DeferredFallbackProvider
                    .complete_turn(ProviderTurnRequest::plain("unused", Vec::new(), Vec::new()))
                    .await;
            }
            Reply::WaitFinal | Reply::WaitSilent => vec![tool(
                format!("p2-wait-{round}"),
                "WaitFor",
                serde_json::json!({
                    "wake": "operator_input",
                    "delivery": if matches!(step.reply, Reply::WaitFinal) { "final" } else { "silent" },
                    "reason": "p2 pending report fence",
                }),
            )],
            Reply::Complete => vec![tool(
                format!("p2-complete-{round}"),
                "CompleteWorkItem",
                serde_json::json!({ "work_item_id": turn.effective_owner().work_item_id().unwrap() }),
            )],
            Reply::Stop => {
                runtime.control(crate::types::ControlAction::Stop).await?;
                vec![ModelBlock::Text {
                    text: "stopped".into(),
                }]
            }
        };
        if let Some(fault) = self.attachment_fault.lock().await.take() {
            let sources = self.sources.lock().await.clone();
            *self.before_attachment.lock().await =
                Some(attachment_snapshot(&runtime, &sources).await);
            runtime.inject_next_transition_fault(fault);
        }
        Ok(ProviderTurnResponse {
            material,
            blocks,
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

struct LiveFallbackProvider {
    live: Arc<LiveProvider>,
}

#[async_trait]
impl AgentProvider for LiveFallbackProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.live.complete_turn(request).await
    }

    fn select_model_lineage(
        &self,
        _: &crate::config::ModelRouteRef,
    ) -> Option<Arc<dyn AgentProvider>> {
        Some(Arc::new(Self {
            live: self.live.clone(),
        }))
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![crate::provider::ProviderPromptCapability::FullRequestOnly]
    }
}

fn live_context_config() -> ContextConfig {
    ContextConfig {
        prompt_budget_estimated_tokens: 128_000,
        turn_projection_min_budget: 128_000,
        turn_projection_max_budget: 128_000,
        compaction_trigger_estimated_tokens: 128_000,
        ..context_config()
    }
}

async fn fixture() -> (TempDir, TempDir, RuntimeHandle, Arc<LiveProvider>) {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(LiveProvider::default());
    // Keep context pressure separate from the live-input byte-budget assertions.
    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        live_context_config(),
    )
    .unwrap();
    isolate_test_user_home(&runtime);
    *provider.runtime.lock().await = Some(runtime.clone());
    (data, workspace, runtime, provider)
}

async fn owner(runtime: &RuntimeHandle, label: &str) -> String {
    runtime
        .create_work_item(
            label.into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap()
        .id
}

fn prepare_message(
    recipient: &str,
    sender: &str,
    key: &str,
    body: &str,
) -> crate::runtime::agent_message_delivery::PreparedAgentMessageDelivery {
    AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id: recipient.into(),
            content: MessageBody::Text { text: body.into() },
            client_idempotency_key: key.into(),
            correlation_id: None,
            causation_id: None,
            requested_priority: Some(Priority::Normal),
            forward: false,
            reply_context_id: None,
        },
        AgentMessageCallerContext {
            caller_principal: format!("agent:{sender}"),
            caller_agent_id: Some(sender.into()),
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
    .unwrap()
}

async fn message(
    runtime: &RuntimeHandle,
    owner: Option<&str>,
    sender: &str,
    key: &str,
    body: &str,
    valid_sender: bool,
) -> MessageEnvelope {
    let mut prepared = prepare_message("default", sender, key, body);
    prepared.message.work_item_id = owner.map(str::to_owned);
    prepared.message.created_at = runtime.now();
    if !valid_sender {
        // This input never had an accepted sender route. Publish it once,
        // rather than corrupting the evidence of an accepted delivery.
        prepared.message.routing_context = None;
        return runtime.enqueue(prepared.message).await.unwrap();
    }
    let id = runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap()
        .message_id
        .unwrap();
    runtime.storage().read_message_by_id(&id).unwrap().unwrap()
}

async fn task_result(
    runtime: &RuntimeHandle,
    owner: Option<&str>,
    id: &str,
    body: &str,
    kind: TaskKind,
    parent_turn: &str,
) -> String {
    let recovery = if kind == TaskKind::AgentMessageWait {
        // The outbound receipt and its cursor must precede the real reply.
        let mut request = prepare_message(
            "p2-peer",
            "default",
            &format!("{id}-request"),
            "observe p2 reply",
        );
        request.record.outcome = crate::types::AgentMessageDeliveryOutcome::Accepted;
        request.record.state = AgentMessageDeliveryState::Queued;
        request.record.accepted_at = Some(runtime.now());
        let boundary = runtime
            .inner
            .runtime_db
            .transaction(|tx| {
                crate::runtime_db::agent_message_delivery::insert_delivery_tx(tx, &request.record)?;
                Ok(tx.last_insert_rowid())
            })
            .unwrap();
        Some(TaskRecoverySpec::AgentMessageWait {
            summary: "p2 reply waiter".into(),
            message: "observe p2 reply".into(),
            target_agent_id: "p2-peer".into(),
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_id: Some(request.record.delivery_id),
            after_delivery_rowid: Some(boundary),
            reply_deadline_at: None,
            requester_incarnation: None,
        })
    } else {
        None
    };
    let mut task = TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind,
        status: TaskStatus::Running,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: None,
        work_item_id: owner.map(str::to_owned),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": id,
            "rejoin_generation": 1,
            "parent_turn_id": parent_turn,
            "terminal_reentry": true,
        })),
        recovery,
    };
    runtime
        .persist_task_transition(&task, "task_created")
        .await
        .unwrap();
    if kind == TaskKind::AgentMessageWait {
        let reply = message(runtime, owner, "p2-peer", id, body, true).await;
        let mut detail = task.detail.clone().unwrap();
        let Some(TaskRecoverySpec::AgentMessageWait {
            after_delivery_rowid: Some(boundary),
            ..
        }) = task.recovery.as_ref()
        else {
            panic!("reply fixture must capture its request cursor");
        };
        detail["message_id"] = serde_json::json!(reply.id);
        detail["message_wait_after_delivery_rowid"] = serde_json::json!(boundary);
        detail["response_delivery_id"] =
            reply.metadata.as_ref().unwrap()["agent_message_delivery"]["delivery_id"].clone();
        Box::pin(runtime.finish_agent_message_wait(
            &task,
            TaskStatus::Completed,
            detail,
            String::new(),
        ))
        .await
        .unwrap();
        return runtime
            .storage()
            .latest_task_record(id)
            .unwrap()
            .unwrap()
            .parent_message_id
            .unwrap();
    }
    let mut source = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task { task_id: id.into() },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text { text: body.into() },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    source.work_item_id = owner.map(str::to_owned);
    source.task_id = Some(task.id.clone());
    source.metadata = Some(serde_json::json!({
        "task_id": id,
        "task_kind": kind.as_str(),
        "task_status": "completed",
        "task_detail": task.detail,
        "task_result_id": source.id,
        "work_item_id": owner,
    }));
    task.status = TaskStatus::Completed;
    task.parent_message_id = Some(source.id.clone());
    if kind == TaskKind::ChildAgentTask {
        Box::pin(runtime.commit_terminal_child_task_result(&task, "task_completed", &source, None))
            .await
            .unwrap();
    } else {
        Box::pin(runtime.commit_terminal_task_result(&task, "task_completed", &source))
            .await
            .unwrap();
    }
    source.id
}

async fn claim(runtime: &RuntimeHandle) -> scheduler_executor::ScheduledMessage {
    for _ in 0..72 {
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap();
        let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            continue;
        };
        if scheduled.scheduler_decision.model_reentry {
            return scheduled;
        }
        Box::pin(process(runtime, scheduled)).await;
    }
    panic!(
        "expected canonical model reentry; queue: {:?}; execution: {:?}; rejection events: {:?}",
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .recent(Some("default"), 8)
            .unwrap(),
        runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap(),
        runtime
            .storage()
            .read_recent_events(200)
            .unwrap()
            .into_iter()
            .filter(|event| matches!(
                event.kind.as_str(),
                "scheduler_authority_input_rejected" | "brief_created" | "runtime_error"
            ))
            .collect::<Vec<_>>(),
    );
}

async fn process(runtime: &RuntimeHandle, scheduled: scheduler_executor::ScheduledMessage) {
    let source = scheduled.message;
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        source.clone(),
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await
    .unwrap();
    Box::pin(settle(runtime, source, &terminal)).await;
}

async fn settle(
    runtime: &RuntimeHandle,
    source: MessageEnvelope,
    terminal: &turn::TurnTerminalTransition,
) {
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
            Some(terminal),
        )
        .await
        .unwrap());
}

async fn run(runtime: &RuntimeHandle, receiver: Option<&str>) {
    message(runtime, receiver, "p2-peer", "anchor", "p2 anchor", true).await;
    let scheduled = Box::pin(claim(runtime)).await;
    Box::pin(process(runtime, scheduled)).await;
}

fn latest_turn(runtime: &RuntimeHandle) -> TurnRecord {
    runtime.storage().read_recent_turns(1).unwrap().remove(0)
}

fn visible(request: &ProviderTurnRequest) -> String {
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
        .join("\n")
}

fn live_ids(turn: &TurnRecord) -> Vec<String> {
    turn.live_input_batches
        .iter()
        .flat_map(|batch| batch.items.iter().map(|item| item.message_id.clone()))
        .collect()
}

fn assert_queued(runtime: &RuntimeHandle, source: &str) {
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(source)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued,
        "{source}"
    );
}

async fn attachment_snapshot(runtime: &RuntimeHandle, sources: &[String]) -> serde_json::Value {
    let turn = latest_turn(runtime);
    let mut message_ids = sources.to_vec();
    for source in sources {
        let message = runtime
            .storage()
            .read_message_by_id(source)
            .unwrap()
            .unwrap();
        if let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&message) {
            if !message_ids.iter().any(|id| id == reply_id) {
                message_ids.push(reply_id.to_owned());
            }
        }
    }
    let queues: Vec<_> = message_ids
        .iter()
        .map(|id| runtime.inner.runtime_db.queue_entries().latest(id).unwrap())
        .collect();
    let deliveries: Vec<_> = message_ids
        .iter()
        .map(|id| {
            runtime
                .inner
                .runtime_db
                .agent_message_deliveries()
                .latest_for_message(id)
                .unwrap()
        })
        .collect();
    let settlements: Vec<_> = sources
        .iter()
        .map(|id| {
            runtime
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(id)
                .unwrap()
        })
        .collect();
    let live_transcript: Vec<_> = runtime
        .storage()
        .read_all_transcript()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.data["boundary"] == "live_input_batch")
        .collect();
    let live_events: Vec<_> = runtime
        .storage()
        .read_recent_events(1000)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "live_input_attached")
        .collect();
    let project_state = |state: &AgentState| {
        serde_json::json!({
            "pending": state.pending,
            "status": state.status,
            "current_turn_id": state.current_turn_id,
            "binding": state.current_execution_binding,
        })
    };
    let persisted = runtime
        .inner
        .runtime_db
        .agent_states()
        .latest("default")
        .unwrap()
        .unwrap();
    let guard = runtime.inner.agent.lock().await;
    let in_memory: Vec<_> = message_ids
        .iter()
        .map(|id| {
            guard
                .queue
                .peek_next_matching(|message| message.id == *id)
                .is_some()
        })
        .collect();
    serde_json::json!({
        "input_message_ids": turn.input_message_ids,
        "live_input_batches": turn.live_input_batches,
        "queues": queues,
        "deliveries": deliveries,
        "settlements": settlements,
        "live_transcript": live_transcript,
        "live_events": live_events,
        "persisted": project_state(&persisted),
        "cached": project_state(&guard.state),
        "queue_length": guard.queue.len(),
        "in_memory": in_memory,
    })
}

fn ordinary(receiver: Option<&str>, sender: &'static str, valid_sender: bool) -> Injection {
    Injection {
        source: Source::Message {
            sender,
            valid_sender,
        },
        owner: receiver.map(str::to_owned),
        count: 1,
        body_bytes: 16,
    }
}

fn result(receiver: Option<&str>, kind: TaskKind, same_turn: bool, count: usize) -> Injection {
    Injection {
        source: Source::Result { kind, same_turn },
        owner: receiver.map(str::to_owned),
        count,
        body_bytes: 16,
    }
}

fn assert_live_bounds(turn: &TurnRecord) {
    assert!(turn.live_input_batches.len() <= 4);
    let mut identities = BTreeSet::new();
    let mut bytes = 0;
    let mut items = 0;
    for batch in &turn.live_input_batches {
        assert!(!batch.activation_id.is_empty());
        assert!(batch.safe_point.provider_round > 0);
        assert!(batch.max_items <= 8);
        assert!(batch.max_bytes <= 24 * 1024);
        assert!(batch.rendered_bytes <= batch.max_bytes);
        let identity_count = batch.result_identities.len() + batch.ordinary_message_ids.len();
        assert!(identity_count <= batch.max_items);
        assert!(batch.items.len() <= identity_count + batch.result_identities.len());
        for identity in &batch.result_identities {
            assert!(identities.insert(identity));
        }
        bytes += batch.rendered_bytes;
        items += identity_count;
    }
    assert!(bytes <= 96 * 1024);
    assert!(items <= 32);
}

fn assert_request_tool_pairs(request: &ProviderTurnRequest) -> usize {
    let mut calls = Vec::new();
    let mut receipts = Vec::new();
    for message in &request.conversation {
        match message {
            ConversationMessage::AssistantBlocks(blocks) => {
                calls.extend(blocks.iter().filter_map(|block| match block {
                    ModelBlock::ToolUse { id, .. } => Some(id.as_str()),
                    _ => None,
                }));
            }
            ConversationMessage::UserToolResults(results) => {
                receipts.extend(results.iter().map(|result| result.tool_use_id.as_str()));
            }
            _ => {}
        }
    }
    let unique_calls: BTreeSet<_> = calls.iter().copied().collect();
    let unique_receipts: BTreeSet<_> = receipts.iter().copied().collect();
    assert_eq!(calls.len(), unique_calls.len(), "duplicate tool call");
    assert_eq!(
        receipts.len(),
        unique_receipts.len(),
        "duplicate tool receipt"
    );
    assert_eq!(
        unique_calls, unique_receipts,
        "unpaired provider conversation"
    );
    calls.len()
}

fn source_bodies(runtime: &RuntimeHandle, sources: &[String]) -> Vec<String> {
    sources
        .iter()
        .map(|source| {
            let message = runtime
                .storage()
                .read_message_by_id(source)
                .unwrap()
                .unwrap();
            let MessageBody::Text { text } = message.body else {
                panic!("expected textual live evidence");
            };
            text
        })
        .collect()
}

async fn restart_live_runtime(
    data: &TempDir,
    workspace: &TempDir,
    runtime: RuntimeHandle,
    provider: &Arc<LiveProvider>,
) -> RuntimeHandle {
    *provider.runtime.lock().await = None;
    drop(runtime);
    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        live_context_config(),
    )
    .unwrap();
    isolate_test_user_home(&runtime);
    *provider.runtime.lock().await = Some(runtime.clone());
    assert_eq!(
        Box::pin(runtime.recover_scheduler_bootstrap_claims())
            .await
            .unwrap(),
        1,
    );
    runtime
}

#[tokio::test]
async fn p2_restart_reuses_local_round_two_for_same_owner_task_result() {
    assert_restart_reuses_local_round_two(Some(TaskKind::CommandTask)).await;
}

#[tokio::test]
async fn p2_restart_reuses_local_round_two_for_ordinary_input() {
    assert_restart_reuses_local_round_two(None).await;
}

#[tokio::test]
async fn p2_restart_routes_new_message_wait_reply() {
    assert_restart_reuses_local_round_two(Some(TaskKind::AgentMessageWait)).await;
}

#[tokio::test]
async fn p2_batchless_ancestor_survives_live_attachment_and_another_restart() {
    assert_batchless_ancestor_restart(None).await;
}

#[tokio::test]
async fn p2_batchless_ancestor_requires_exact_replay_source() {
    for defect in ["source_turn", "source_message"] {
        assert_batchless_ancestor_restart(Some(defect)).await;
    }
}

// Fixture-only rewrite of an already registered Turn. The canonical input
// evidence must be acknowledged in the same transaction that writes the body,
// otherwise later preparation correctly rejects the unregistered change.
async fn store_qualified_turn(runtime: &RuntimeHandle, turn: &TurnRecord) {
    let mut mutation = runtime.begin_mutation().await.unwrap();
    mutation
        .commit_input_evidence(crate::ids::audit_event_id(), |scope| {
            runtime
                .inner
                .runtime_db
                .turn_records()
                .upsert_in_scope(scope, turn)
        })
        .await
        .unwrap();
}

// Import malformed persisted replay evidence for this negative fixture only.
// Normal Turn writes still reject the immutable identity change; the fixture
// publishes the exact corrupted body and its qualification in one transaction.
async fn store_qualified_replay_defect(runtime: &RuntimeHandle, turn: &TurnRecord) {
    use crate::runtime::resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey};
    use crate::runtime_db::input_evidence_snapshot::{
        validate_existing_input_evidence_tx, CanonicalInputEvidence,
    };
    use crate::runtime_db::input_evidence_writes::{
        acknowledge_input_evidence_tx, validate_input_evidence_scope_tx,
    };
    use crate::runtime_db::repositories::{
        committed_turn_record_tx, validate_turn_record_identity,
    };

    let mut mutation = runtime.begin_mutation().await.unwrap();
    mutation
        .commit_input_evidence(crate::ids::audit_event_id(), |scope| {
            runtime.inner.runtime_db.transaction(|tx| {
                validate_input_evidence_scope_tx(tx, scope, &turn.agent_id)?;
                validate_existing_input_evidence_tx(
                    tx,
                    scope,
                    &InputEvidenceKey::new(InputEvidenceFamily::Turn, &turn.turn_id),
                )?;
                let original = committed_turn_record_tx(tx, &turn.agent_id, &turn.turn_id)?;
                let mut restored = turn.clone();
                restored.replay = original.record.replay.clone();
                anyhow::ensure!(
                    restored == original.record
                        && validate_turn_record_identity(&original.record, turn).is_err(),
                    "negative fixture must change only immutable replay provenance"
                );
                anyhow::ensure!(
                    tx.execute(
                        "UPDATE turn_records
                         SET replay_source_turn_id = ?1, payload_json = ?2
                         WHERE agent_id = ?3 AND turn_id = ?4",
                        rusqlite::params![
                            turn.replay
                                .as_ref()
                                .map(|replay| replay.source_turn_id.as_str()),
                            serde_json::to_string(turn)?,
                            turn.agent_id,
                            turn.turn_id,
                        ],
                    )? == 1,
                    "negative fixture Turn disappeared"
                );
                let actual = committed_turn_record_tx(tx, &turn.agent_id, &turn.turn_id)?;
                let facts = [CanonicalInputEvidence::Turn(actual.record.clone())];
                acknowledge_input_evidence_tx(tx, scope, actual, &facts)
            })
        })
        .await
        .unwrap();
}

fn assert_batchless_recovery_claim(
    runtime: &RuntimeHandle,
    anchor: &MessageEnvelope,
    predecessor: &TurnRecord,
) {
    use crate::domain::execution_protocol::ExecutionAttemptState;

    let execution = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized(&anchor.agent_id)
        .unwrap()
        .unwrap();
    let prior_id = &predecessor
        .live_input_batches
        .first()
        .expect("batchless predecessor live attachment")
        .activation_id;
    let prior = execution
        .attempts
        .get(prior_id)
        .expect("batchless recovery predecessor attempt");
    let actual_turn = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            crate::runtime_db::task_result_settlement::canonical_live_turn_id_tx(tx, prior)
        })
        .unwrap();
    assert_eq!(actual_turn.as_deref(), Some(predecessor.turn_id.as_str()));
    assert_eq!(prior.state, ExecutionAttemptState::Interrupted);
    let open: Vec<_> = execution
        .attempts
        .values()
        .filter(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
        })
        .collect();
    assert_eq!(open.len(), 1, "batchless recovery needs one open attempt");
    assert_eq!(
        open[0].recovery_of_attempt_id.as_deref(),
        Some(prior.attempt_id.as_str()),
        "batchless recovery must claim the immediate predecessor",
    );
}

async fn assert_batchless_replay_source(
    runtime: &RuntimeHandle,
    anchor: &MessageEnvelope,
    predecessor: &TurnRecord,
    provenance: Option<&ExecutionAdmissionProvenance>,
) {
    use crate::domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding};
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = provenance else {
        panic!("batchless recovery must have canonical admission");
    };
    let mut mutation = runtime.begin_mutation().await.unwrap();
    mutation
        .prepare_owner_inputs(
            std::collections::BTreeSet::from([
                Key::new(Family::Message, &anchor.id),
                Key::new(Family::Attempt, activation_id),
                Key::new(
                    Family::Turn,
                    anchor
                        .source_refs
                        .get("replay_source_turn_id")
                        .expect("batchless predecessor hint"),
                ),
            ]),
            Default::default(),
        )
        .await
        .unwrap();
    let inputs = mutation.capture_input_evidence().unwrap();
    let protocol = inputs.execution_state_if_initialized().unwrap().unwrap();
    assert!(predecessor.idle_input_batch.is_none());
    assert!(!predecessor.live_input_batches.is_empty());
    let selected = RuntimeHandle::canonical_idle_batch_replay_source(
        &inputs,
        Some(&protocol),
        anchor,
        provenance,
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.0, predecessor.turn_id);
    let prior_id = protocol.attempts[activation_id]
        .recovery_of_attempt_id
        .as_ref()
        .unwrap();
    for fault in 0..7 {
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
            4 => prior.attempt_id = "unrelated-activation".into(),
            5 => {
                prior.binding = ExecutionBinding::WorkItem {
                    work_item_id: "unrelated-owner".into(),
                };
                changed.attempts.get_mut(activation_id).unwrap().binding = prior.binding.clone();
            }
            6 => {
                changed.attempts.get_mut(activation_id).unwrap().state =
                    ExecutionAttemptState::Interrupted
            }
            _ => unreachable!(),
        }
        assert!(
            RuntimeHandle::canonical_idle_batch_replay_source(
                &inputs,
                Some(&changed),
                anchor,
                provenance,
            )
            .unwrap()
            .is_none(),
            "live-only replay accepted fault {fault}"
        );
    }
}

async fn assert_batchless_ancestor_restart(defect: Option<&str>) {
    let (data, workspace, mut runtime, provider) = fixture().await;
    *provider.steps.lock().await = vec![
        Step {
            inputs: Vec::new(),
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Interrupt,
        },
        Step {
            inputs: vec![result(None, TaskKind::CommandTask, true, 1)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Interrupt,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Final,
        },
    ];
    let anchor = message(
        &runtime,
        None,
        "p2-peer",
        "batchless-anchor",
        "p2 old P1 anchor",
        true,
    )
    .await;
    let mut predecessors = Vec::<TurnRecord>::new();
    let mut original_settlement = None;
    for index in 0..3 {
        if index > 0 {
            runtime = restart_live_runtime(&data, &workspace, runtime, &provider).await;
            if index == 1 {
                let predecessor = predecessors.last_mut().unwrap();
                *predecessor = runtime
                    .storage()
                    .read_turn_by_id(&predecessor.turn_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    predecessor.terminal.as_ref().unwrap().kind,
                    TurnTerminalKind::Interrupted,
                );
                // Let bootstrap end the original dispatch before modeling a
                // pre-batch P1 turn; an administrative terminal is not proof.
                predecessor.idle_input_batch = None;
                store_qualified_turn(&runtime, predecessor).await;
            }
        }
        let scheduled = Box::pin(claim(&runtime)).await;
        assert_eq!(scheduled.message.id, anchor.id);
        if index == 2 {
            let predecessor = predecessors.last().unwrap();
            assert_batchless_recovery_claim(&runtime, &anchor, predecessor);
            assert_eq!(
                scheduled.message.source_refs.get("replay_source_turn_id"),
                Some(&predecessor.turn_id),
            );
            Box::pin(assert_batchless_replay_source(
                &runtime,
                &scheduled.message,
                predecessor,
                scheduled
                    .dispatch_plan
                    .execution_admission_provenance
                    .as_ref(),
            ))
            .await;
            let result_id = &provider.sources.lock().await[0];
            original_settlement = runtime
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(result_id)
                .unwrap();
            if let Some(defect) = defect {
                let mut invalid = runtime
                    .storage()
                    .read_turn_by_id(&predecessor.turn_id)
                    .unwrap()
                    .unwrap();
                let replay = invalid.replay.as_mut().unwrap();
                match defect {
                    "source_turn" => replay.source_turn_id = "unrelated-turn".into(),
                    "source_message" => {
                        assert_ne!(result_id, &anchor.id);
                        replay.source_message_id = result_id.clone();
                    }
                    _ => unreachable!(),
                }
                store_qualified_replay_defect(&runtime, &invalid).await;
            }
        }
        let source = scheduled.message.clone();
        let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
            scheduled.message,
            scheduled.dispatch_plan,
            &scheduled.scheduler_decision,
        ));
        if index < 2 {
            tokio::select! {
                outcome = &mut processing => panic!(
                    "batchless lineage attempt {index} must remain in flight: {:?}", outcome.err(),
                ),
                ready = tokio::time::timeout(
                    std::time::Duration::from_secs(10), provider.interrupted.notified(),
                ) => ready.unwrap(),
            }
        } else if defect.is_some() {
            let error = processing.as_mut().await.unwrap_err();
            assert!(
                error.to_string().contains("live input recovery"),
                "{error:#}"
            );
            let result_id = &provider.sources.lock().await[0];
            assert_eq!(
                runtime
                    .inner
                    .runtime_db
                    .task_result_settlements()
                    .latest_for_message(result_id)
                    .unwrap(),
                original_settlement,
            );
            assert!(latest_turn(&runtime).live_input_batches.is_empty());
            assert_eq!(
                latest_turn(&runtime)
                    .replay
                    .as_ref()
                    .unwrap()
                    .source_turn_id,
                predecessors.last().unwrap().turn_id,
            );
            assert_eq!(provider.requests.lock().await.len(), 4);
            return;
        } else {
            let terminal = processing.as_mut().await.unwrap();
            Box::pin(settle(&runtime, source, &terminal)).await;
        }
        drop(processing);
        let turn = latest_turn(&runtime);
        assert_eq!(turn.live_input_batches.len(), usize::from(index > 0));
        if index > 0 {
            assert!(turn.idle_input_batch.is_none());
            assert_eq!(turn.effective_owner(), predecessors[0].effective_owner());
            assert_eq!(
                turn.replay.as_ref().unwrap().source_turn_id,
                predecessors.last().unwrap().turn_id
            );
            assert_live_bounds(&turn);
        }
        predecessors.push(turn);
    }
    let result_id = &provider.sources.lock().await[0];
    let settled = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_id)
        .unwrap()
        .unwrap();
    let original = original_settlement.unwrap();
    assert_eq!(settled.result_identity, original.result_identity);
    assert_eq!(
        settled.source.parent_turn_id().unwrap(),
        predecessors[1].turn_id
    );
    assert_eq!(settled.work_item_id, original.work_item_id);
    assert_eq!(settled.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settled.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_eq!(
        predecessors[2].input_message_ids,
        predecessors[1].input_message_ids
    );
    assert_eq!(
        predecessors[2].live_input_batches[0].safe_point,
        predecessors[1].live_input_batches[0].safe_point
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 5);
    let rendered = &predecessors[1].live_input_batches[0].items[0].rendered;
    assert_eq!(visible(&requests[4]).matches(rendered.as_str()).count(), 1);
    assert_request_tool_pairs(&requests[4]);
}

#[tokio::test]
async fn p2_interrupted_restart_restores_mixed_operator_and_live_inputs() {
    Box::pin(assert_mixed_operator_live_restart(None, false)).await;
}

#[tokio::test]
async fn p2_repeated_lifecycle_restart_restores_mixed_operator_and_live_inputs() {
    Box::pin(assert_mixed_operator_live_restart(None, true)).await;
}

#[tokio::test]
async fn p2_mixed_operator_recovery_requires_admission_evidence() {
    for defect in [
        "missing_transcript",
        "foreign_transcript",
        "queued",
        "wrong_authority",
    ] {
        Box::pin(assert_mixed_operator_live_restart(Some(defect), false)).await;
        if defect != "queued" {
            Box::pin(assert_mixed_operator_live_restart_with_evidence(
                Some(defect),
                false,
                true,
            ))
            .await;
        }
    }
}

async fn assert_mixed_operator_live_restart(defect: Option<&str>, repeated: bool) {
    Box::pin(assert_mixed_operator_live_restart_with_evidence(
        defect, repeated, false,
    ))
    .await;
}

async fn assert_mixed_operator_live_restart_with_evidence(
    defect: Option<&str>,
    repeated: bool,
    qualified_tamper: bool,
) {
    let (data, workspace, runtime, provider) = fixture().await;
    let producer = owner(&runtime, "p2 mixed original task owner").await;
    // Bound WorkItem claims retain their one-replay poison-message fence.
    let receiver = if repeated {
        None
    } else {
        Some(owner(&runtime, "p2 mixed interrupted owner").await)
    };
    *provider.steps.lock().await = vec![
        Step {
            inputs: vec![result(Some(&producer), TaskKind::CommandTask, true, 1)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: vec![Injection {
                source: Source::Operator,
                owner: None,
                count: 1,
                body_bytes: 6000,
            }],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Interrupt,
        },
        Step {
            inputs: vec![ordinary(receiver.as_deref(), "p2-peer", true)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: if repeated {
                Reply::Interrupt
            } else {
                Reply::Final
            },
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Final,
        },
    ];
    if !repeated {
        provider.steps.lock().await.truncate(5);
    }
    let anchor = message(
        &runtime,
        receiver.as_deref(),
        "p2-peer",
        "mixed-anchor",
        "p2 mixed anchor",
        true,
    )
    .await;
    let scheduled = Box::pin(claim(&runtime)).await;
    let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ));
    tokio::select! {
        _ = &mut processing => panic!("mixed predecessor must remain in flight"),
        ready = tokio::time::timeout(std::time::Duration::from_secs(10), provider.interrupted.notified()) => ready.unwrap(),
    }
    drop(processing);
    let predecessor = latest_turn(&runtime);
    let sources = provider.sources.lock().await.clone();
    let result_id = &sources[0];
    let operator_id = &sources[1];
    let operator = runtime
        .storage()
        .read_message_by_id(operator_id)
        .unwrap()
        .unwrap();
    let operator_body = message_text(&operator.body);
    assert_eq!(predecessor.live_input_batches.len(), 1);
    assert!(!live_ids(&predecessor).contains(operator_id));
    assert!(predecessor.input_message_ids.contains(operator_id));
    assert!(scheduler::is_operator_interjection_message(&operator));
    let original_activation = predecessor.live_input_batches[0].activation_id.clone();
    let original_settlement = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        original_settlement.state,
        TaskResultSettlementState::CallerAdmitted
    );
    assert_eq!(
        original_settlement.work_item_id.as_deref(),
        Some(producer.as_str())
    );
    assert_eq!(
        visible(&provider.requests.lock().await[2])
            .matches(operator_body.as_str())
            .count(),
        1
    );
    let runtime = restart_live_runtime(&data, &workspace, runtime, &provider).await;
    // Claim before corrupting Queue selection; keep the admitted recovery anchor.
    let scheduled = if qualified_tamper || defect == Some("queued") {
        Some(Box::pin(claim(&runtime)).await)
    } else {
        None
    };
    let transcript_id: String = runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .query_row(
            "SELECT evidence_id FROM transcript_entries
             WHERE message_id = ?1 AND kind = 'incoming_message'",
            [operator_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut historical_home = None;
    if let Some(defect) = defect {
        let amend = |tx: &rusqlite::Transaction<'_>| -> Result<()> {
            match defect {
                "missing_transcript" => {
                    tx.execute(
                        "DELETE FROM transcript_entries WHERE message_id = ?1",
                        [operator_id],
                    )?;
                }
                "foreign_transcript" => {
                    let payload: String = tx.query_row(
                        "SELECT payload_json FROM transcript_entries WHERE evidence_id = ?1",
                        [&transcript_id],
                        |row| row.get(0),
                    )?;
                    let mut entry: crate::types::TranscriptEntry = serde_json::from_str(&payload)?;
                    entry.data["turn_id"] = serde_json::json!("unrelated-turn");
                    let payload = serde_json::to_string(&entry)?;
                    // Historical evidence must be canonical before migration;
                    // live tamper intentionally retains its old qualification.
                    let hash = if qualified_tamper {
                        tx.query_row(
                            "SELECT content_hash FROM transcript_entries WHERE evidence_id = ?1",
                            [&transcript_id],
                            |row| row.get::<_, String>(0),
                        )?
                    } else {
                        crate::runtime_db::evidence::content_hash(&payload)
                    };
                    assert_eq!(
                        tx.execute(
                            "UPDATE transcript_entries SET turn_id = ?1, payload_json = ?2,
                                                           content_hash = ?4
                             WHERE evidence_id = ?3",
                            rusqlite::params!["unrelated-turn", payload, transcript_id, hash,],
                        )?,
                        1
                    );
                }
                "queued" => {
                    let mut payload: String = tx.query_row(
                        "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
                        [operator_id],
                        |row| row.get(0),
                    )?;
                    if !qualified_tamper {
                        let mut queue: QueueEntryRecord = serde_json::from_str(&payload)?;
                        queue.status = QueueEntryStatus::Queued;
                        payload = serde_json::to_string(&queue)?;
                    }
                    assert_eq!(
                        tx.execute(
                            "UPDATE queue_entries SET status = 'queued', payload_json = ?2
                             WHERE message_id = ?1",
                            rusqlite::params![operator_id, payload],
                        )?,
                        1
                    );
                }
                "wrong_authority" => {
                    let mut invalid = operator.clone();
                    invalid.authority_class = AuthorityClass::ExternalEvidence;
                    let payload = serde_json::to_string(&invalid)?;
                    let hash = if qualified_tamper {
                        tx.query_row(
                            "SELECT content_hash FROM messages WHERE message_id = ?1",
                            [operator_id],
                            |row| row.get::<_, String>(0),
                        )?
                    } else {
                        crate::runtime_db::evidence::content_hash(&payload)
                    };
                    tx.execute(
                        "UPDATE messages SET payload_json = ?2, content_hash = ?3
                         WHERE message_id = ?1",
                        rusqlite::params![operator_id, payload, hash],
                    )?;
                }
                _ => unreachable!(),
            }
            Ok(())
        };
        if qualified_tamper {
            // Deliberately bypass the qualified writer: this negative fixture
            // tests the captured reader's rejection of unacknowledged drift.
            let connection = runtime.inner.runtime_db.connection().unwrap();
            let tx = connection.unchecked_transaction().unwrap();
            amend(&tx).unwrap();
            tx.commit().unwrap();
        } else {
            historical_home = Some(
                super::input_consumption_p1::legacy_recovery_home(
                    &runtime.inner.runtime_db,
                    "default-state",
                    "default-workspace",
                    amend,
                )
                .unwrap(),
            );
        }
    }
    let runtime = if let Some(home) = historical_home.as_ref() {
        // Migrate an independent malformed history; never re-ACK live tamper.
        let in_flight = if scheduled.is_some() {
            Some(runtime.agent_state().await.unwrap())
        } else {
            None
        };
        *provider.runtime.lock().await = None;
        drop(runtime);
        let runtime = RuntimeHandle::new(
            "default",
            home.path().join("default-state"),
            home.path().join("default-workspace"),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            live_context_config(),
        )
        .unwrap();
        isolate_test_user_home(&runtime);
        *provider.runtime.lock().await = Some(runtime.clone());
        if let Some(in_flight) = in_flight {
            // Bootstrap clears posture, not this already-admitted claim.
            let mut mutation = runtime.begin_mutation().await.unwrap();
            assert_eq!(in_flight.status, AgentStatus::AwakeRunning);
            assert!(in_flight.current_run_id.is_some());
            assert_eq!(
                mutation.state.current_execution_binding,
                in_flight.current_execution_binding
            );
            assert_eq!(mutation.state.current_turn_id, in_flight.current_turn_id);
            assert!(mutation.state.current_run_id.is_none());
            mutation.state.status = in_flight.status;
            mutation.state.current_run_id = in_flight.current_run_id;
            mutation.persist_state(runtime.storage()).await.unwrap();
        }
        runtime
    } else {
        runtime
    };
    let scheduled = match scheduled {
        Some(scheduled) => scheduled,
        None => Box::pin(claim(&runtime)).await,
    };
    assert_eq!(scheduled.message.id, anchor.id);
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.status, AgentStatus::AwakeRunning);
    assert!(state.current_run_id.is_some());
    let execution = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution
            .attempts
            .values()
            .filter(|attempt| {
                attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                    && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
            })
            .count(),
        1,
        "the final runtime must own the recovery claim"
    );
    let recovered_source = scheduled.message.clone();
    let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ));
    if defect.is_some() {
        let error = processing.await.unwrap_err();
        if qualified_tamper {
            use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;
            let expected = match defect.unwrap() {
                "missing_transcript" => InputEvidenceSnapshotError::NeedsPreparation(format!(
                    "transcript:{transcript_id} unavailable without tombstone"
                )),
                "foreign_transcript" => {
                    InputEvidenceSnapshotError::ProjectionChanged(transcript_id)
                }
                "wrong_authority" => {
                    InputEvidenceSnapshotError::ProjectionChanged(operator_id.clone())
                }
                _ => unreachable!(),
            };
            assert_eq!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(&expected),
                "{error:#}"
            );
        } else {
            assert_eq!(
                error.to_string(),
                "live input recovery does not match canonical predecessor evidence",
                "{defect:?}: {error:#}"
            );
            assert!(!error
                .is::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>());
        }
        let unchanged = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(result_id)
            .unwrap()
            .unwrap();
        assert_eq!(unchanged, original_settlement);
        let failed = latest_turn(&runtime);
        assert!(failed.live_input_batches.is_empty());
        assert!(!failed.input_message_ids.contains(operator_id));
        assert_eq!(provider.requests.lock().await.len(), 3);
        return;
    }
    let runtime = if repeated {
        tokio::select! {
            outcome = &mut processing => panic!(
                "mixed recovered turn must remain in flight: {:?}", outcome.err(),
            ),
            ready = tokio::time::timeout(std::time::Duration::from_secs(10), provider.interrupted.notified()) => ready.unwrap(),
        }
        drop(processing);
        let first_recovery = latest_turn(&runtime);
        assert_eq!(
            first_recovery.effective_owner(),
            predecessor.effective_owner()
        );
        assert!(first_recovery.input_message_ids.contains(operator_id));
        let runtime = restart_live_runtime(&data, &workspace, runtime, &provider).await;
        let scheduled = Box::pin(claim(&runtime)).await;
        Box::pin(process(&runtime, scheduled)).await;
        assert_eq!(
            latest_turn(&runtime).input_message_ids,
            first_recovery.input_message_ids
        );
        runtime
    } else {
        let terminal = processing.await.unwrap();
        Box::pin(settle(&runtime, recovered_source, &terminal)).await;
        runtime
    };
    let recovered = latest_turn(&runtime);
    assert_eq!(recovered.effective_owner(), predecessor.effective_owner());
    assert_eq!(recovered.live_input_batches.len(), 2);
    assert_eq!(
        runtime
            .storage()
            .read_message_by_id(operator_id)
            .unwrap()
            .unwrap(),
        operator
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), if repeated { 7 } else { 5 });
    for request in &requests[3..] {
        let text = visible(request);
        assert_eq!(
            text.matches(operator_body.as_str()).count(),
            1,
            "full operator body exactly once"
        );
        assert!(text.contains(operator_id));
        assert!(text.contains("p2-operator"));
        assert!(text.contains("operator_instruction"));
        assert!(text.contains("http_control_prompt"));
        assert!(text.contains("control_authenticated"));
        assert_request_tool_pairs(request);
    }
    let settled = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_id)
        .unwrap()
        .unwrap();
    assert_eq!(settled.work_item_id, original_settlement.work_item_id);
    assert_eq!(
        settled.source.parent_turn_id().unwrap(),
        predecessor.turn_id
    );
    assert_eq!(settled.result_identity, original_settlement.result_identity);
    assert_eq!(settled.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settled.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_ne!(
        settled.activation_id.as_deref(),
        Some(original_activation.as_str())
    );
    assert_live_bounds(&recovered);
}

async fn assert_restart_reuses_local_round_two(result_kind: Option<TaskKind>) {
    let (data, workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 restart safe point owner").await;
    *provider.steps.lock().await = vec![
        Step {
            inputs: vec![ordinary(Some(&receiver), "p2-peer", true)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Interrupt,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::GatedTool,
        },
        Step {
            inputs: Vec::new(),
            reply: if result_kind == Some(TaskKind::AgentMessageWait) {
                Reply::CheckLiveReplyRoute
            } else {
                Reply::Final
            },
        },
    ];
    message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "anchor",
        "p2 anchor",
        true,
    )
    .await;
    let scheduled = Box::pin(claim(&runtime)).await;
    let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ));
    tokio::select! {
        _ = &mut processing => panic!("the second request must remain in flight"),
        ready = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.interrupted.notified(),
        ) => ready.unwrap(),
    }
    drop(processing);
    let predecessor = latest_turn(&runtime);
    assert_eq!(predecessor.live_input_batches.len(), 1);
    assert_eq!(
        predecessor.live_input_batches[0].safe_point.provider_round,
        2
    );
    let runtime = restart_live_runtime(&data, &workspace, runtime, &provider).await;
    let scheduled = Box::pin(claim(&runtime)).await;
    let _provider_admission = runtime
        .activate_provider_admission(scheduled.dispatch_plan.provider_admission.clone())
        .unwrap();
    let mut processing = Box::pin(
        runtime.process_interactive_dispatch_with_cleanup(
            &scheduled.message,
            scheduled.dispatch_plan.continuation_resolution.as_ref(),
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            LoopControlOptions {
                max_tool_rounds: Some(2),
            },
            None,
            Some(&scheduled.dispatch_plan.prior_closure),
        ),
    );
    let ready = workspace.path().join("p2-tool-ready");
    tokio::select! {
        _ = &mut processing => panic!("the recovered round-one tool must wait"),
        ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }) => ready.expect("the real tool did not start"),
    }
    let current = latest_turn(&runtime);
    let source = if let Some(kind) = result_kind {
        Box::pin(task_result(
            &runtime,
            Some(&receiver),
            "p2-restarted-tool-result",
            "p2 restarted tool result",
            kind,
            &current.turn_id,
        ))
        .await
    } else {
        message(
            &runtime,
            Some(&receiver),
            "p2-peer",
            "p2-restarted-input",
            "p2 restarted ordinary input",
            true,
        )
        .await
        .id
    };
    if result_kind == Some(TaskKind::AgentMessageWait) {
        provider.sources.lock().await.push(source.clone());
    }
    std::fs::write(workspace.path().join("p2-tool-release"), "").unwrap();
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(10), processing)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.terminal.kind, TurnTerminalKind::Completed);
    Box::pin(settle(&runtime, scheduled.message, &terminal)).await;
    let recovered = latest_turn(&runtime);
    assert_eq!(recovered.live_input_batches.len(), 2);
    let mut inherited = predecessor.live_input_batches[0].clone();
    inherited.activation_id = recovered.live_input_batches[1].activation_id.clone();
    assert_eq!(recovered.live_input_batches[0], inherited);
    assert_eq!(
        recovered.live_input_batches[1].safe_point.activation_id,
        recovered.live_input_batches[1].activation_id,
    );
    assert_ne!(
        recovered.live_input_batches[0].safe_point,
        recovered.live_input_batches[1].safe_point,
    );
    assert_eq!(recovered.live_input_batches[1].items[0].message_id, source);
    assert_eq!(
        recovered.live_input_batches[1].safe_point.provider_round, 2,
        "a safe point is the actual request ordinal within its attempt, not an inherited offset",
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 4);
    assert_request_tool_pairs(&requests[3]);
    let source_message = runtime
        .storage()
        .read_message_by_id(&source)
        .unwrap()
        .unwrap();
    let body_source =
        crate::wake_contract::agent_message_reply_reference(&source_message).unwrap_or(&source);
    let bodies = source_bodies(
        &runtime,
        &[
            provider.sources.lock().await[0].clone(),
            body_source.to_owned(),
        ],
    );
    for body in &bodies {
        assert_eq!(visible(&requests[3]).matches(body.as_str()).count(), 1);
    }
    assert!(!visible(&requests[2]).contains(&bodies[1]));
    if result_kind.is_some() {
        let settled = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&source)
            .unwrap()
            .unwrap();
        assert_eq!(settled.work_item_id.as_deref(), Some(receiver.as_str()));
        assert_eq!(settled.source.parent_turn_id().unwrap(), current.turn_id);
        assert_eq!(settled.state, TaskResultSettlementState::Settled);
        assert_eq!(
            settled.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
    }
    assert_live_bounds(&recovered);
}

#[tokio::test]
async fn p2_same_attempt_safe_point_rejects_runtime_and_database_readmission() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 safe point uniqueness").await;
    *provider.steps.lock().await = vec![
        Step {
            inputs: vec![ordinary(Some(&receiver), "p2-peer", true)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Interrupt,
        },
    ];
    message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "anchor",
        "p2 anchor",
        true,
    )
    .await;
    let scheduled = Box::pin(claim(&runtime)).await;
    let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ));
    tokio::select! {
        _ = &mut processing => panic!("the second request must remain in flight"),
        ready = tokio::time::timeout(
            std::time::Duration::from_secs(10), provider.interrupted.notified(),
        ) => ready.unwrap(),
    }
    let later = message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "later",
        "p2 later input",
        true,
    )
    .await;
    let before = attachment_snapshot(&runtime, &[later.id.clone()]).await;
    assert!(runtime
        .attach_live_inputs("default", 2, usize::MAX, false)
        .await
        .unwrap()
        .is_empty());

    // Bypass selection to prove the transactional guard rejects the same safe point too.
    let turn = latest_turn(&runtime);
    let state = runtime.agent_state().await.unwrap();
    let execution = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    let mut batch = turn.live_input_batches[0].clone();
    batch.cutoff = runtime.now();
    batch.result_identities.clear();
    batch.ordinary_message_ids = vec![later.id.clone()];
    batch.items = vec![input_consumption::item_evidence(&later, usize::MAX)
        .unwrap()
        .unwrap()];
    batch.rendered_bytes = batch.rendered_text().len();
    let mut queue = runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest(&later.id)
        .unwrap()
        .unwrap();
    queue.status = QueueEntryStatus::Interjected;
    let command = crate::runtime_db::transitions::LiveInputAttachmentCommand {
        expected_attempt: execution.attempts[&batch.activation_id].clone(),
        expected_turn: turn,
        batch,
        messages: vec![later.clone()],
        queues: vec![queue],
        agent_state: AgentStateMutation {
            expected: Some(Box::new(state.clone())),
            record: Box::new(state),
        },
        audit_event_id: crate::ids::audit_event_id(),
        fault: None,
    };
    assert!(
        !runtime
            .inner
            .runtime_db
            .transitions()
            .commit_live_input_attachment(&command)
            .unwrap()
            .applied
    );
    assert_eq!(attachment_snapshot(&runtime, &[later.id]).await, before);
    drop(processing);
}

#[tokio::test]
async fn p2_repeated_restart_preserves_cumulative_live_budgets() {
    // Only validated lifecycle deliveries permit repeated crash replay; bound
    // WorkItem attempts retain their existing one-replay poison-message fence.
    for (count, body_bytes) in [(8, 16), (1, 23_000)] {
        let (data, workspace, mut runtime, provider) = fixture().await;
        let mut steps = Vec::new();
        for attempt in 0..5 {
            let mut injection = result(None, TaskKind::CommandTask, true, count);
            injection.body_bytes = body_bytes;
            steps.push(Step {
                inputs: vec![injection],
                reply: Reply::PairedTools,
            });
            steps.push(Step {
                inputs: Vec::new(),
                reply: if attempt < 4 {
                    Reply::Interrupt
                } else {
                    Reply::Final
                },
            });
        }
        *provider.steps.lock().await = steps;
        message(&runtime, None, "p2-peer", "anchor", "p2 anchor", true).await;
        let mut predecessors = Vec::<TurnRecord>::new();
        for attempt in 0..5 {
            if attempt > 0 {
                assert_eq!(
                    latest_turn(&runtime)
                        .terminal
                        .as_ref()
                        .map(|terminal| terminal.kind),
                    Some(TurnTerminalKind::Interrupted),
                    "bootstrap must close the current attachment, not its ingress ancestor",
                );
            }
            let scheduled = Box::pin(claim(&runtime)).await;
            let source = scheduled.message.clone();
            let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
                scheduled.message,
                scheduled.dispatch_plan,
                &scheduled.scheduler_decision,
            ));
            if attempt < 4 {
                tokio::select! {
                    outcome = &mut processing => panic!(
                        "attempt {attempt} must be interrupted; error={:?}", outcome.err(),
                    ),
                    ready = tokio::time::timeout(
                        std::time::Duration::from_secs(10), provider.interrupted.notified(),
                    ) => ready.unwrap(),
                }
            } else {
                let terminal = processing.as_mut().await.unwrap();
                Box::pin(settle(&runtime, source, &terminal)).await;
            }
            drop(processing);
            let turn = latest_turn(&runtime);
            assert_eq!(turn.live_input_batches.len(), (attempt + 1).min(4));
            assert_live_bounds(&turn);
            assert_eq!(
                turn.live_input_batches
                    .iter()
                    .map(|batch| &batch.safe_point)
                    .collect::<BTreeSet<_>>()
                    .len(),
                turn.live_input_batches.len(),
            );
            assert!(turn
                .live_input_batches
                .iter()
                .all(|batch| batch.safe_point.provider_round == 2));
            if let Some(prior) = predecessors.last() {
                for (old, new) in prior
                    .live_input_batches
                    .iter()
                    .zip(&turn.live_input_batches)
                {
                    let mut expected = old.clone();
                    expected.activation_id = new.activation_id.clone();
                    assert_eq!(*new, expected);
                }
            }
            if attempt < 4 {
                predecessors.push(turn);
                runtime = restart_live_runtime(&data, &workspace, runtime, &provider).await;
            }
        }
        let turn = latest_turn(&runtime);
        let admitted = live_ids(&turn);
        assert_eq!(admitted.len(), 4 * count);
        let sources = provider.sources.lock().await.clone();
        assert_eq!(sources.len(), 5 * count);
        for source in &sources[4 * count..] {
            assert_queued(&runtime, source);
            let record = runtime
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(source)
                .unwrap()
                .unwrap();
            assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
        }
        for predecessor in predecessors {
            assert_eq!(
                runtime
                    .storage()
                    .read_turn_by_id(&predecessor.turn_id)
                    .unwrap()
                    .unwrap()
                    .live_input_batches,
                predecessor.live_input_batches
            );
        }
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), 10);
        for body in source_bodies(&runtime, &admitted) {
            assert_eq!(
                visible(&requests[9]).matches(&body).count(),
                1,
                "live body must appear once; count={count}, body_bytes={body_bytes}, \
                 source_prefix={:?}, context_occurrences={}, conversation_occurrences={}",
                body.chars().take(64).collect::<String>(),
                requests[9]
                    .prompt_frame
                    .context_blocks
                    .iter()
                    .map(|block| block.text.matches(&body).count())
                    .sum::<usize>(),
                requests[9]
                    .conversation
                    .iter()
                    .filter_map(|message| match message {
                        ConversationMessage::UserText(text) => Some(text.matches(&body).count()),
                        _ => None,
                    })
                    .sum::<usize>(),
            );
        }
    }
}

#[tokio::test]
async fn p2_interrupted_restart_recovers_nine_live_results_without_identity_migration() {
    let (data, workspace, runtime, provider) = fixture().await;
    let producer = owner(&runtime, "p2 original task owner").await;
    let receiver = owner(&runtime, "p2 interrupted receiver").await;
    *provider.steps.lock().await = vec![
        Step {
            inputs: vec![result(Some(&producer), TaskKind::CommandTask, true, 9)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Interrupt,
        },
        Step {
            inputs: vec![ordinary(Some(&receiver), "p2-peer", true)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::CheckLiveReplyRoute,
        },
    ];
    let anchor = message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "anchor",
        "p2 anchor",
        true,
    )
    .await;
    let follower = message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "initial-follower",
        "p2 initial follower",
        true,
    )
    .await;
    let mut initial_parent = TurnRecord::new("default", "p2-initial-parent", 0);
    initial_parent.created_at = runtime.now();
    initial_parent.current_work_item_id = Some(receiver.clone());
    initial_parent.owner = Some(crate::types::TurnOwner::WorkItem {
        work_item_id: receiver.clone(),
    });
    store_qualified_turn(&runtime, &initial_parent).await;
    let initial_result = Box::pin(task_result(
        &runtime,
        Some(&receiver),
        "p2-initial-result",
        "p2 initial result",
        TaskKind::CommandTask,
        "p2-initial-parent",
    ))
    .await;
    let scheduled = Box::pin(claim(&runtime)).await;
    assert_eq!(scheduled.message.id, anchor.id);
    let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ));
    tokio::select! {
        _ = &mut processing => panic!("the third provider request must remain in flight"),
        ready = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.interrupted.notified(),
        ) => ready.expect("live evidence did not reach the third request"),
    }
    // Drop an actually in-flight provider future, leaving its canonical attempt
    // open for startup recovery rather than synthesizing an interrupted record.
    drop(processing);
    let predecessor = latest_turn(&runtime);
    let sources = provider.sources.lock().await.clone();
    assert_eq!(sources.len(), 9);
    assert_eq!(live_ids(&predecessor), sources);
    assert_eq!(predecessor.live_input_batches.len(), 2);
    assert_eq!(predecessor.live_input_batches[0].result_identities.len(), 8);
    assert_eq!(predecessor.live_input_batches[1].result_identities.len(), 1);
    assert!(predecessor.terminal.is_none());
    let original_activation = predecessor.live_input_batches[0].activation_id.clone();
    let initial_batch = predecessor.idle_input_batch.as_ref().unwrap();
    assert_eq!(
        initial_batch
            .items
            .iter()
            .map(|item| item.message_id.as_str())
            .collect::<Vec<_>>(),
        [anchor.id.as_str(), follower.id.as_str()],
    );
    assert_eq!(
        initial_batch.activation_id.as_ref(),
        Some(&original_activation)
    );
    let initial_before = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&initial_result)
        .unwrap()
        .unwrap();
    assert_eq!(
        initial_before.state,
        TaskResultSettlementState::CallerAdmitted
    );
    assert_eq!(
        initial_before.activation_id.as_ref(),
        Some(&original_activation)
    );
    assert_eq!(
        initial_before.work_item_id.as_deref(),
        Some(receiver.as_str())
    );
    assert_eq!(
        initial_before.source.parent_turn_id(),
        Some("p2-initial-parent")
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .admitted_for_activation("default", &original_activation)
            .unwrap()
            .len(),
        10,
    );
    let initial_bodies = source_bodies(&runtime, &[follower.id.clone(), initial_result.clone()]);
    for body in &initial_bodies {
        assert_eq!(
            visible(&provider.requests.lock().await[0])
                .matches(body.as_str())
                .count(),
            1
        );
    }
    let before: Vec<_> = sources
        .iter()
        .map(|source| {
            runtime
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(source)
                .unwrap()
                .unwrap()
        })
        .collect();
    let bodies = source_bodies(&runtime, &sources);
    for settlement in &before {
        assert_eq!(settlement.state, TaskResultSettlementState::CallerAdmitted);
        assert_eq!(settlement.work_item_id.as_deref(), Some(producer.as_str()));
        assert_eq!(
            settlement.source.parent_turn_id().unwrap(),
            predecessor.turn_id
        );
        assert_eq!(settlement.rejoin_generation, 1);
    }
    assert_eq!(
        assert_request_tool_pairs(&provider.requests.lock().await[2]),
        4
    );
    *provider.runtime.lock().await = None;
    drop(runtime);

    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        live_context_config(),
    )
    .unwrap();
    isolate_test_user_home(&runtime);
    *provider.runtime.lock().await = Some(runtime.clone());
    assert_eq!(
        Box::pin(runtime.recover_scheduler_bootstrap_claims())
            .await
            .unwrap(),
        1
    );
    let execution = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution.attempts[&original_activation].state,
        crate::domain::execution_protocol::ExecutionAttemptState::Interrupted,
    );
    let persisted_source = runtime
        .storage()
        .read_turn_by_id(&predecessor.turn_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted_source
            .terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(TurnTerminalKind::Interrupted),
        "bootstrap must close the canonical source turn, not its ingress turn",
    );
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .interrupted_admitted_result_anchor("default", &initial_result, runtime.now())
            .unwrap(),
        Some(anchor.id.clone()),
        "initial snapshot membership: {}; anchor: {:?}; attempt: {:?}; initial: {:?}; cutoff: {:?}",
        predecessor.input_message_ids.contains(&initial_result),
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&anchor.id)
            .unwrap(),
        execution.attempts[&original_activation],
        initial_before,
        initial_batch.cutoff,
    );
    let scheduled = Box::pin(claim(&runtime)).await;
    assert_eq!(
        scheduled.message.id, anchor.id,
        "claimed {:?} with sources {:?}",
        scheduled.message.origin, scheduled.message.source_refs,
    );
    Box::pin(process(&runtime, scheduled)).await;

    let recovered = latest_turn(&runtime);
    assert_ne!(recovered.turn_id, predecessor.turn_id);
    assert_eq!(recovered.effective_owner(), predecessor.effective_owner());
    assert_eq!(
        recovered.effective_owner().work_item_id(),
        Some(receiver.as_str())
    );
    assert_eq!(
        recovered.replay.as_ref().unwrap().source_turn_id,
        predecessor.turn_id
    );
    let all_sources = provider.sources.lock().await.clone();
    assert_eq!(all_sources.len(), 10);
    assert_eq!(&all_sources[..sources.len()], sources.as_slice());
    assert_eq!(live_ids(&recovered), all_sources);
    assert_eq!(recovered.live_input_batches.len(), 3);
    let safe_points: std::collections::BTreeSet<_> = recovered
        .live_input_batches
        .iter()
        .map(|batch| &batch.safe_point)
        .collect();
    assert_eq!(safe_points.len(), recovered.live_input_batches.len());
    assert_eq!(recovered.live_input_batches[2].safe_point.provider_round, 2);
    let new_activation = &recovered.live_input_batches[0].activation_id;
    assert_ne!(new_activation, &original_activation);
    let mut expected_initial = initial_batch.clone();
    expected_initial.activation_id = Some(new_activation.clone());
    assert_eq!(recovered.idle_input_batch.as_ref(), Some(&expected_initial));
    for input in &predecessor.input_message_ids {
        assert!(recovered.input_message_ids.contains(input));
    }
    for (old, new) in predecessor
        .live_input_batches
        .iter()
        .zip(&recovered.live_input_batches)
    {
        let mut expected = old.clone();
        expected.activation_id = new_activation.clone();
        assert_eq!(*new, expected);
    }
    assert_eq!(
        runtime
            .storage()
            .read_turn_by_id(&predecessor.turn_id)
            .unwrap()
            .unwrap()
            .live_input_batches,
        predecessor.live_input_batches,
    );
    assert_eq!(
        runtime
            .storage()
            .read_turn_by_id(&predecessor.turn_id)
            .unwrap()
            .unwrap()
            .idle_input_batch,
        predecessor.idle_input_batch,
    );
    let live_message = all_sources.last().unwrap();
    assert_eq!(
        recovered.live_input_batches[2].ordinary_message_ids,
        [live_message.clone()]
    );
    assert!(recovered.live_input_batches[2].result_identities.is_empty());
    assert!(recovered.input_message_ids.contains(live_message));
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 5);
    for request in &requests[3..] {
        let restored = visible(request);
        for body in bodies.iter().chain(&initial_bodies) {
            assert_eq!(
                restored.matches(body.as_str()).count(),
                1,
                "recovered input evidence"
            );
        }
        assert_request_tool_pairs(request);
    }
    let live_body = source_bodies(&runtime, &[live_message.clone()]).remove(0);
    assert!(!visible(&requests[3]).contains(&live_body));
    assert_eq!(visible(&requests[4]).matches(live_body.as_str()).count(), 1);
    drop(requests);
    let initial_after = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&initial_result)
        .unwrap()
        .unwrap();
    assert_eq!(
        initial_after.result_identity,
        initial_before.result_identity
    );
    assert_eq!(initial_after.task_id, initial_before.task_id);
    assert_eq!(initial_after.work_item_id, initial_before.work_item_id);
    assert_eq!(initial_after.source, initial_before.source);
    assert_eq!(
        initial_after.rejoin_generation,
        initial_before.rejoin_generation
    );
    assert_eq!(initial_after.activation_id.as_ref(), Some(new_activation));
    assert_eq!(initial_after.state, TaskResultSettlementState::Settled);
    assert_eq!(
        initial_after.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    for original in before {
        let settled = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&original.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(settled.result_identity, original.result_identity);
        assert_eq!(settled.task_id, original.task_id);
        assert_eq!(settled.work_item_id, original.work_item_id);
        assert_eq!(settled.source, original.source);
        assert_eq!(settled.rejoin_generation, original.rejoin_generation);
        assert_eq!(settled.state, TaskResultSettlementState::Settled);
        assert_eq!(
            settled.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
        assert_eq!(settled.activation_id.as_ref(), Some(new_activation));
        let task = runtime
            .task_record(&settled.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.work_item_id.as_deref(), Some(producer.as_str()));
        let detail = task.detail.unwrap();
        assert_eq!(
            detail["parent_turn_id"],
            serde_json::json!(predecessor.turn_id)
        );
        assert_eq!(detail["rejoin_generation"], serde_json::json!(1));
    }
    assert_live_bounds(&recovered);
}

#[tokio::test]
async fn p2_provider_fallback_keeps_nine_live_results_reachable_once() {
    Box::pin(assert_provider_fallback_keeps_live_results(1)).await;
}

#[tokio::test]
async fn p2_two_provider_fallbacks_keep_ancestor_live_results_reachable_once() {
    Box::pin(assert_provider_fallback_keeps_live_results(2)).await;
}

async fn assert_provider_fallback_keeps_live_results(fallback_count: usize) {
    Box::pin(assert_provider_result_handoff(
        fallback_count,
        ProviderHandoffCheck::Normal,
    ))
    .await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderHandoffCheck {
    Normal,
    Mixed,
    Atomic,
    RestartClaimOnly,
    RestartBeforeTransfer,
    RestartAfterTransfer,
}

#[tokio::test]
async fn p2_provider_result_handoff_rolls_back_every_precommit_fault() {
    Box::pin(assert_provider_result_handoff(
        1,
        ProviderHandoffCheck::Atomic,
    ))
    .await;
}

#[tokio::test]
async fn p2_provider_result_handoff_survives_restart_before_transfer() {
    Box::pin(assert_provider_result_handoff(
        1,
        ProviderHandoffCheck::RestartBeforeTransfer,
    ))
    .await;
}

#[tokio::test]
async fn p2_provider_result_handoff_survives_restart_after_transfer() {
    Box::pin(assert_provider_result_handoff(
        1,
        ProviderHandoffCheck::RestartAfterTransfer,
    ))
    .await;
}

#[tokio::test]
async fn p2_provider_result_handoff_survives_restart_then_another_fallback() {
    Box::pin(assert_provider_result_handoff(
        2,
        ProviderHandoffCheck::RestartBeforeTransfer,
    ))
    .await;
}

#[tokio::test]
async fn p2_provider_result_handoff_survives_claim_only_restart_then_another_fallback() {
    Box::pin(assert_provider_result_handoff(
        2,
        ProviderHandoffCheck::RestartClaimOnly,
    ))
    .await;
}

#[tokio::test]
async fn p2_provider_result_handoff_preserves_mixed_operator_and_ordinary_inputs() {
    Box::pin(assert_provider_result_handoff(
        1,
        ProviderHandoffCheck::Mixed,
    ))
    .await;
}

fn assert_provider_result_handoff(
    fallback_count: usize,
    check: ProviderHandoffCheck,
) -> std::pin::Pin<Box<impl std::future::Future<Output = ()>>> {
    // Keep the large fixture future out of its caller's construction frame.
    Box::pin(async move {
        let data = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(LiveProvider::default());
        let runtime = RuntimeHandle::new(
            "default",
            data.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(LiveFallbackProvider {
                live: provider.clone(),
            }),
            "default".into(),
            ContextConfig {
                recent_messages: 1,
                ..live_context_config()
            },
        )
        .unwrap();
        isolate_test_user_home(&runtime);
        *provider.runtime.lock().await = Some(runtime.clone());
        let receiver = owner(&runtime, "p2 fallback receiver").await;
        let mut first_inputs = vec![result(Some(&receiver), TaskKind::CommandTask, true, 9)];
        if check == ProviderHandoffCheck::Mixed {
            first_inputs.extend([
                ordinary(Some(&receiver), "p2-provider-peer", true),
                Injection {
                    source: Source::Operator,
                    owner: Some(receiver.clone()),
                    count: 1,
                    body_bytes: 16,
                },
            ]);
        }
        *provider.steps.lock().await = vec![
            Step {
                inputs: first_inputs,
                reply: Reply::PairedTools,
            },
            Step {
                inputs: Vec::new(),
                reply: Reply::PairedTools,
            },
            Step {
                inputs: Vec::new(),
                reply: Reply::Fallback,
            },
        ];
        provider
            .steps
            .lock()
            .await
            .extend((1..fallback_count).map(|_| Step {
                inputs: Vec::new(),
                reply: Reply::Fallback,
            }));
        Box::pin(run(&runtime, Some(&receiver))).await;
        let source_turn = latest_turn(&runtime);
        let sources = provider.sources.lock().await.clone();
        assert_eq!(
            sources.len(),
            if check == ProviderHandoffCheck::Mixed {
                11
            } else {
                9
            }
        );
        let live_sources: Vec<_> = sources
            .iter()
            .filter(|id| {
                runtime
                    .storage()
                    .read_message_by_id(id)
                    .unwrap()
                    .unwrap()
                    .authority_class
                    != AuthorityClass::OperatorInstruction
            })
            .cloned()
            .collect();
        assert_eq!(live_ids(&source_turn), live_sources);
        assert_eq!(source_turn.live_input_batches.len(), 2);
        assert!(matches!(
            source_turn.terminal.as_ref().unwrap().kind,
            TurnTerminalKind::DeferredToFallback | TurnTerminalKind::ProviderFailedNeedsRecovery,
        ));
        let bodies = source_bodies(&runtime, &sources);
        let before: Vec<_> = sources
            .iter()
            .filter_map(|source| {
                runtime
                    .inner
                    .runtime_db
                    .task_result_settlements()
                    .latest_for_message(source)
                    .unwrap()
            })
            .collect();
        assert_eq!(before.len(), 9);
        let runtime = if matches!(
            check,
            ProviderHandoffCheck::Atomic
                | ProviderHandoffCheck::RestartClaimOnly
                | ProviderHandoffCheck::RestartBeforeTransfer
                | ProviderHandoffCheck::RestartAfterTransfer
        ) {
            let scheduled = Box::pin(claim(&runtime)).await;
            let claimed_activation = match scheduled
                .dispatch_plan
                .execution_admission_provenance
                .as_ref()
            {
                Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) => {
                    activation_id.clone()
                }
                _ => panic!("fallback claim must carry its canonical activation"),
            };
            if check != ProviderHandoffCheck::RestartClaimOnly {
                Box::pin(
                    runtime.begin_dispatch_turn(
                        &scheduled.message,
                        None,
                        None,
                        scheduled
                            .dispatch_plan
                            .execution_admission_provenance
                            .clone(),
                        &scheduled.dispatch_plan.prior_closure,
                    ),
                )
                .await
                .unwrap();
                runtime
                    .record_incoming_transcript_entry(&scheduled.message)
                    .await
                    .unwrap();
            }
            if check == ProviderHandoffCheck::Atomic {
                let current = latest_turn(&runtime);
                let state = runtime.agent_state().await.unwrap();
                let activation = state
                    .current_execution_binding
                    .as_ref()
                    .unwrap()
                    .activation_id
                    .as_deref()
                    .unwrap();
                let execution = runtime
                    .inner
                    .runtime_db
                    .transitions()
                    .load_execution_protocol_state_if_initialized("default")
                    .unwrap()
                    .unwrap();
                let attempt = &execution.attempts[activation];
                for fault in PRE_COMMIT_FAULTS {
                    assert!(
                        runtime
                            .inner
                            .runtime_db
                            .transitions()
                            .commit_provider_result_handoff(
                                attempt,
                                &current,
                                runtime.now(),
                                Some(fault),
                            )
                            .is_err(),
                        "{fault:?}"
                    );
                    assert_eq!(latest_turn(&runtime), current);
                    for original in &before {
                        assert_eq!(
                            &runtime
                                .inner
                                .runtime_db
                                .task_result_settlements()
                                .latest_for_message(&original.message_id)
                                .unwrap()
                                .unwrap(),
                            original
                        );
                    }
                }
                let mut stale = attempt.clone();
                stale.source_message_id = Some("unrelated-source".into());
                assert!(
                    !runtime
                        .inner
                        .runtime_db
                        .transitions()
                        .commit_provider_result_handoff(&stale, &current, runtime.now(), None,)
                        .unwrap()
                        .applied
                );
                let mut invalid_source = source_turn.clone();
                invalid_source.terminal.as_mut().unwrap().kind = TurnTerminalKind::Interrupted;
                runtime
                    .inner
                    .runtime_db
                    .transaction(|tx| {
                        tx.execute(
                            "UPDATE turn_records SET payload_json = ?1 WHERE turn_id = ?2",
                            rusqlite::params![
                                serde_json::to_string(&invalid_source)?,
                                source_turn.turn_id
                            ],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                assert!(runtime
                    .inner
                    .runtime_db
                    .transitions()
                    .commit_provider_result_handoff(attempt, &current, runtime.now(), None,)
                    .is_err());
                assert_eq!(latest_turn(&runtime), current);
                for original in &before {
                    assert_eq!(
                        &runtime
                            .inner
                            .runtime_db
                            .task_result_settlements()
                            .latest_for_message(&original.message_id)
                            .unwrap()
                            .unwrap(),
                        original
                    );
                }
                runtime
                    .inner
                    .runtime_db
                    .transaction(|tx| {
                        tx.execute(
                            "UPDATE turn_records SET payload_json = ?1 WHERE turn_id = ?2",
                            rusqlite::params![
                                serde_json::to_string(&source_turn)?,
                                source_turn.turn_id
                            ],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                assert!(
                    runtime
                        .inner
                        .runtime_db
                        .transitions()
                        .commit_provider_result_handoff(attempt, &current, runtime.now(), None,)
                        .unwrap()
                        .applied
                );
                let recovered = latest_turn(&runtime);
                let count = runtime
                    .storage()
                    .read_recent_events(100)
                    .unwrap()
                    .iter()
                    .filter(|event| event.kind == "provider_task_results_transferred")
                    .count();
                assert_eq!(count, 1);
                assert!(
                    runtime
                        .inner
                        .runtime_db
                        .transitions()
                        .commit_provider_result_handoff(attempt, &recovered, runtime.now(), None,)
                        .unwrap()
                        .applied
                );
                assert_eq!(
                    runtime
                        .storage()
                        .read_recent_events(100)
                        .unwrap()
                        .iter()
                        .filter(|event| event.kind == "provider_task_results_transferred")
                        .count(),
                    count
                );
                assert_eq!(provider.requests.lock().await.len(), 3);
                return;
            }
            if check == ProviderHandoffCheck::RestartAfterTransfer {
                assert!(
                    Box::pin(runtime.recover_provider_result_inputs(&scheduled.message))
                        .await
                        .unwrap()
                );
            }
            let interrupted_turn_id = (check != ProviderHandoffCheck::RestartClaimOnly)
                .then(|| latest_turn(&runtime).turn_id);
            *provider.runtime.lock().await = None;
            drop(runtime);
            let reopened = RuntimeHandle::new(
                "default",
                data.path().to_path_buf(),
                workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),
                Arc::new(LiveFallbackProvider {
                    live: provider.clone(),
                }),
                "default".into(),
                live_context_config(),
            )
            .unwrap();
            isolate_test_user_home(&reopened);
            *provider.runtime.lock().await = Some(reopened.clone());
            assert_eq!(
                Box::pin(reopened.recover_scheduler_bootstrap_claims())
                    .await
                    .unwrap(),
                1
            );
            if let Some(interrupted_turn_id) = interrupted_turn_id {
                assert_eq!(
                    reopened
                        .storage()
                        .read_turn_by_id(&interrupted_turn_id)
                        .unwrap()
                        .unwrap()
                        .terminal
                        .as_ref()
                        .map(|terminal| terminal.kind),
                    Some(TurnTerminalKind::Interrupted),
                );
            } else {
                assert_eq!(latest_turn(&reopened), source_turn);
            }
            assert_eq!(
                reopened
                    .inner
                    .runtime_db
                    .transitions()
                    .load_execution_protocol_state_if_initialized("default")
                    .unwrap()
                    .unwrap()
                    .attempts[&claimed_activation]
                    .state,
                crate::domain::execution_protocol::ExecutionAttemptState::Interrupted,
            );
            reopened
        } else {
            runtime
        };
        for _ in 0..fallback_count {
            let predecessor = latest_turn(&runtime);
            let scheduled = Box::pin(claim(&runtime)).await;
            if matches!(
                check,
                ProviderHandoffCheck::Normal | ProviderHandoffCheck::Mixed
            ) {
                assert_eq!(
                    scheduled.message.source_refs.get("source_turn_id"),
                    Some(&predecessor.turn_id),
                );
            }
            Box::pin(process(&runtime, scheduled)).await;
            assert_eq!(
                latest_turn(&runtime).live_input_batches.len(),
                source_turn.live_input_batches.len()
            );
        }

        let requests = provider.requests.lock().await;
        assert_eq!(
            requests.len(),
            3 + fallback_count,
            "fallback must produce a new actual provider request"
        );
        assert_eq!(assert_request_tool_pairs(&requests[2]), 4);
        for request in requests.iter().skip(3) {
            assert_request_tool_pairs(request);
            let recovered = visible(request);
            let recovery_batches = request
                .prompt_frame
                .context_blocks
                .iter()
                .filter(|block| {
                    block
                        .text
                        .contains("Provider recovery evidence from source turn")
                })
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            for body in &bodies {
                assert_eq!(
                    recovered.matches(body.as_str()).count(),
                    1,
                    "fallback live evidence"
                );
                if check != ProviderHandoffCheck::Mixed {
                    assert_eq!(
                        recovery_batches.matches(body.as_str()).count(),
                        1,
                        "ancestor live evidence must not depend on recent history"
                    );
                }
            }
        }
        drop(requests);
        for original in before {
            let after = runtime
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(&original.message_id)
                .unwrap()
                .unwrap();
            assert_eq!(after.result_identity, original.result_identity);
            assert_eq!(after.work_item_id, original.work_item_id);
            assert_eq!(after.rejoin_generation, original.rejoin_generation);
            assert_ne!(after.activation_id, original.activation_id);
            assert_eq!(after.state, TaskResultSettlementState::Settled);
            assert_eq!(
                after.disposition,
                Some(TaskResultSettlementDisposition::ModelDelivered)
            );
        }
        assert_eq!(
            runtime
                .storage()
                .read_turn_by_id(&source_turn.turn_id)
                .unwrap()
                .unwrap()
                .live_input_batches,
            source_turn.live_input_batches,
        );
        assert_eq!(
            latest_turn(&runtime).live_input_batches.len(),
            source_turn.live_input_batches.len()
        );
    })
}

#[tokio::test]
async fn p2_provider_fallback_restart_preserves_operator_without_live_batches() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(LiveProvider::default());
    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(LiveFallbackProvider {
            live: provider.clone(),
        }),
        "default".into(),
        ContextConfig {
            recent_messages: 1,
            ..live_context_config()
        },
    )
    .unwrap();
    isolate_test_user_home(&runtime);
    *provider.runtime.lock().await = Some(runtime.clone());
    let receiver = owner(&runtime, "p2 batchless fallback operator").await;
    *provider.steps.lock().await = vec![
        Step {
            inputs: Vec::new(),
            reply: Reply::Fallback,
        },
        Step {
            inputs: vec![Injection {
                source: Source::Operator,
                owner: None,
                count: 1,
                body_bytes: 6000,
            }],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Interrupt,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::Final,
        },
    ];
    Box::pin(run(&runtime, Some(&receiver))).await;
    let scheduled = Box::pin(claim(&runtime)).await;
    let fallback_message_id = scheduled.message.id.clone();
    let mut processing = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ));
    tokio::select! {
        outcome = &mut processing => panic!("fallback must remain in flight: {:?}", outcome.err()),
        ready = tokio::time::timeout(std::time::Duration::from_secs(10), provider.interrupted.notified()) => ready.unwrap(),
    }
    drop(processing);
    let predecessor = latest_turn(&runtime);
    let operator_id = provider.sources.lock().await[0].clone();
    let operator = runtime
        .storage()
        .read_message_by_id(&operator_id)
        .unwrap()
        .unwrap();
    let body = message_text(&operator.body);
    assert!(predecessor.live_input_batches.is_empty());
    assert!(predecessor.input_message_ids.contains(&operator_id));
    assert_eq!(provider.requests.lock().await.len(), 3);

    *provider.runtime.lock().await = None;
    drop(runtime);
    let reopened = RuntimeHandle::new(
        "default",
        data.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(LiveFallbackProvider {
            live: provider.clone(),
        }),
        "default".into(),
        ContextConfig {
            recent_messages: 1,
            ..live_context_config()
        },
    )
    .unwrap();
    isolate_test_user_home(&reopened);
    *provider.runtime.lock().await = Some(reopened.clone());
    assert_eq!(
        Box::pin(reopened.recover_scheduler_bootstrap_claims())
            .await
            .unwrap(),
        1
    );
    let scheduled = Box::pin(claim(&reopened)).await;
    assert_eq!(scheduled.message.id, fallback_message_id);
    Box::pin(process(&reopened, scheduled)).await;
    let recovered = latest_turn(&reopened);
    assert!(recovered.live_input_batches.is_empty());
    assert!(recovered.input_message_ids.contains(&operator_id));
    assert_eq!(recovered.effective_owner(), predecessor.effective_owner());
    assert_eq!(
        reopened
            .storage()
            .read_message_by_id(&operator_id)
            .unwrap()
            .unwrap(),
        operator
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 4);
    let request = &requests[3];
    assert_request_tool_pairs(request);
    let text = visible(request);
    assert_eq!(text.matches(body.as_str()).count(), 1);
    for provenance in [
        operator_id.as_str(),
        "p2-operator",
        "operator_instruction",
        "http_control_prompt",
        "control_authenticated",
    ] {
        assert!(text.contains(provenance), "{provenance}");
    }
    assert!(request.prompt_frame.context_blocks.iter().any(|block| {
        block.text.contains("Recovered operator interjection") && block.text.contains(body.as_str())
    }));
}

#[tokio::test]
async fn p2_compaction_keeps_multiple_live_batches_reachable_once() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(LiveProvider::default());
    // Leave room for every admitted body, but force turn-local compaction.
    let compaction_trigger = public_tools_prompt_budget_estimated_tokens() - 4096
        + turn::CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
        + 1;
    let runtime = RuntimeHandle::new(
        "default",
        data.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        ContextConfig {
            compaction_trigger_estimated_tokens: compaction_trigger,
            compaction_keep_recent_estimated_tokens: 1,
            ..live_context_config()
        },
    )
    .unwrap();
    isolate_test_user_home(&runtime);
    *provider.runtime.lock().await = Some(runtime.clone());
    let receiver = owner(&runtime, "p2 compaction receiver").await;
    *provider.steps.lock().await = vec![
        Step {
            inputs: vec![result(Some(&receiver), TaskKind::CommandTask, true, 17)],
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::PairedTools,
        },
        Step {
            inputs: Vec::new(),
            reply: Reply::PairedTools,
        },
    ];
    run(&runtime, Some(&receiver)).await;

    let turn = latest_turn(&runtime);
    let sources = provider.sources.lock().await.clone();
    assert_eq!(sources.len(), 17);
    assert_eq!(live_ids(&turn), sources);
    assert_eq!(
        turn.live_input_batches
            .iter()
            .map(|batch| batch.result_identities.len())
            .collect::<Vec<_>>(),
        [8, 8, 1],
    );
    assert_eq!(
        turn.terminal.as_ref().unwrap().kind,
        TurnTerminalKind::Completed
    );
    assert_live_bounds(&turn);

    let events = runtime.storage().read_recent_events(512).unwrap();
    let compacted_rounds: BTreeSet<_> = events
        .iter()
        .filter(|event| {
            event.kind == "turn_local_compaction_applied"
                && event.data["compacted_rounds"].as_u64().unwrap_or_default() > 0
                && event.data["pre_compaction_estimated_tokens"]
                    .as_u64()
                    .unwrap()
                    > event.data["projected_estimated_tokens"].as_u64().unwrap()
        })
        .map(|event| {
            assert_eq!(
                event.data["compaction_trigger_estimated_tokens"].as_u64(),
                Some(compaction_trigger as u64),
            );
            event.data["round"].as_u64().unwrap()
        })
        .collect();
    assert!(
        compacted_rounds.contains(&5),
        "the last real provider request must compact older admitted batches",
    );

    let bodies = source_bodies(&runtime, &sources);
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 5);
    for (index, request) in requests.iter().enumerate() {
        let round = index + 1;
        let pairs = assert_request_tool_pairs(request);
        assert_eq!(pairs == 0, index == 0, "provider round {round}");
        let rendered = visible(request);
        for (source, body) in sources.iter().zip(&bodies) {
            let admitted = turn.live_input_batches.iter().any(|batch| {
                batch.safe_point.provider_round <= round
                    && batch.items.iter().any(|item| &item.message_id == source)
            });
            assert_eq!(
                rendered.matches(body.as_str()).count(),
                usize::from(admitted),
                "provider round {round} must preserve admitted body {source} exactly once",
            );
        }
    }
    drop(requests);

    let mut settled_identities = BTreeSet::new();
    for source in &sources {
        let settlement = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(source)
            .unwrap()
            .unwrap();
        assert!(settled_identities.insert(settlement.result_identity.clone()));
        let batches: Vec<_> = turn
            .live_input_batches
            .iter()
            .filter(|batch| {
                batch
                    .result_identities
                    .contains(&settlement.result_identity)
            })
            .collect();
        assert_eq!(batches.len(), 1, "compaction must not readmit {source}");
        assert_eq!(
            settlement.activation_id.as_deref(),
            Some(batches[0].activation_id.as_str())
        );
        assert_eq!(settlement.source.parent_turn_id().unwrap(), turn.turn_id);
        assert_eq!(settlement.work_item_id.as_deref(), Some(receiver.as_str()));
        assert_eq!(settlement.state, TaskResultSettlementState::Settled);
        assert_eq!(
            settlement.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(source)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Interjected,
        );
    }
    assert_eq!(settled_identities.len(), 17);
}

#[tokio::test]
async fn p2_same_owner_messages_attach_after_all_tool_receipts() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 owner").await;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![ordinary(Some(&receiver), "p2-peer", true)],
        reply: Reply::PairedTools,
    }];
    run(&runtime, Some(&receiver)).await;
    let sources = provider.sources.lock().await.clone();
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(!visible(&requests[0]).contains("Current live input batch"));
    assert!(visible(&requests[1]).contains("Current input batch"));
    assert!(visible(&requests[1]).contains("Current live input batch"));
    let calls: BTreeSet<_> = requests[1]
        .conversation
        .iter()
        .flat_map(|message| match message {
            ConversationMessage::AssistantBlocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    ModelBlock::ToolUse { id, .. } => Some(id.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    let receipts: BTreeSet<_> = requests[1]
        .conversation
        .iter()
        .flat_map(|message| match message {
            ConversationMessage::UserToolResults(results) => results
                .iter()
                .map(|result| result.tool_use_id.as_str())
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls, receipts);
    let turn = latest_turn(&runtime);
    assert_eq!(live_ids(&turn), sources);
    assert_eq!(turn.live_input_batches.len(), 1);
    assert_eq!(turn.live_input_batches[0].safe_point.provider_round, 2);
    assert_eq!(turn.idle_input_batch.as_ref().unwrap().items.len(), 1);
    assert!(turn.input_message_ids.contains(&sources[0]));
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&sources[0])
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Interjected
    );
    assert_live_bounds(&turn);
}

#[tokio::test]
async fn p2_work_owner_does_not_acquire_an_idle_sender_scope() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 current owner").await;
    let foreign = owner(&runtime, "p2 foreign owner").await;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![
            ordinary(Some(&foreign), "p2-peer", true),
            ordinary(None, "p2-peer", true),
            ordinary(Some(&receiver), "p2-peer", false),
        ],
        reply: Reply::PairedTools,
    }];
    run(&runtime, Some(&receiver)).await;
    assert_eq!(provider.requests.lock().await.len(), 2);
    assert!(latest_turn(&runtime).live_input_batches.is_empty());
    for source in provider.sources.lock().await.iter() {
        assert_queued(&runtime, source);
    }
}

#[tokio::test]
async fn p2_idle_sender_batch_extends_without_switching_sender() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![
            ordinary(None, "p2-peer", true),
            ordinary(None, "foreign-peer", true),
        ],
        reply: Reply::PairedTools,
    }];
    run(&runtime, None).await;
    let sources = provider.sources.lock().await.clone();
    let turn = latest_turn(&runtime);
    assert_eq!(
        turn.idle_input_batch.as_ref().unwrap().sender_agent_id,
        "p2-peer"
    );
    assert_eq!(live_ids(&turn), vec![sources[0].clone()]);
    assert_queued(&runtime, &sources[1]);
    assert_eq!(provider.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn p2_pure_final_reenters_for_each_same_turn_result_kind() {
    for kind in [
        TaskKind::CommandTask,
        TaskKind::ChildAgentTask,
        TaskKind::AgentMessageWait,
    ] {
        let (_data, _workspace, runtime, provider) = fixture().await;
        let receiver = owner(&runtime, "p2 result parent").await;
        *provider.steps.lock().await = vec![Step {
            inputs: vec![result(Some(&receiver), kind, true, 1)],
            reply: Reply::Final,
        }];
        run(&runtime, Some(&receiver)).await;
        let source = provider.sources.lock().await[0].clone();
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), 2, "{kind:?}");
        assert!(!visible(&requests[0]).contains(&source), "{kind:?}");
        assert!(visible(&requests[1]).contains(&source), "{kind:?}");
        assert!(
            requests[1].conversation.iter().any(|message| matches!(
                message,
                ConversationMessage::UserText(text) if text.contains("Current live input batch")
            )),
            "{kind:?}"
        );
        assert!(
            requests[1]
                .conversation
                .iter()
                .any(|message| match message {
                    ConversationMessage::AssistantBlocks(blocks) =>
                        blocks.iter().any(|block| matches!(
                            block, ModelBlock::Text { text } if text == "p2 done"
                        )),
                    _ => false,
                }),
            "{kind:?}"
        );
        assert!(
            !requests[1]
                .conversation
                .iter()
                .any(|message| matches!(message, ConversationMessage::UserToolResults(_))),
            "{kind:?}"
        );
        let settlement = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&source)
            .unwrap()
            .unwrap();
        assert_eq!(
            settlement.state,
            TaskResultSettlementState::Settled,
            "{kind:?}"
        );
        assert_eq!(
            settlement.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
        let turn = latest_turn(&runtime);
        assert_eq!(turn.live_input_batches.len(), 1);
        assert_eq!(turn.live_input_batches[0].safe_point.provider_round, 2);
        assert!(turn.live_input_batches[0].ordinary_message_ids.is_empty());
        assert!(turn.live_input_batches[0]
            .result_identities
            .contains(&settlement.result_identity));
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&source)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Interjected,
        );
        if kind == TaskKind::AgentMessageWait {
            let result = runtime
                .storage()
                .read_message_by_id(&source)
                .unwrap()
                .unwrap();
            let reply = crate::wake_contract::agent_message_reply_reference(&result).unwrap();
            assert!(visible(&requests[1]).contains(reply));
            assert!(live_ids(&turn).iter().any(|id| id == reply));
        }
        assert_live_bounds(&turn);
    }
}

async fn attachment_fault_case(kind: TaskKind, reply: Reply, fault: TransitionFaultPoint) {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 atomic attachment rollback").await;
    let results_only = matches!(reply, Reply::Final);
    let mut inputs = vec![result(Some(&receiver), kind, true, 1)];
    if !results_only {
        inputs.push(ordinary(Some(&receiver), "p2-peer", true));
    }
    *provider.steps.lock().await = vec![Step { inputs, reply }];
    *provider.attachment_fault.lock().await = Some(fault);
    message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "anchor",
        "p2 anchor",
        true,
    )
    .await;
    let scheduled = Box::pin(claim(&runtime)).await;
    let outcome = Box::pin(runtime.process_message_with_plan_deferred(
        scheduled.message,
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await;
    let error = match outcome {
        Err(error) => error,
        Ok(_) => panic!("attachment unexpectedly committed at {fault:?}/{kind:?}"),
    };
    assert!(
        format!("{error:#}").contains(&format!("injected runtime transition fault at {fault:?}")),
        "{error:#}"
    );
    assert_eq!(
        provider.requests.lock().await.len(),
        1,
        "{fault:?}/{kind:?}"
    );
    let sources = provider.sources.lock().await.clone();
    let before = provider.before_attachment.lock().await.clone().unwrap();
    assert_eq!(
        attachment_snapshot(&runtime, &sources).await,
        before,
        "{fault:?}/{kind:?}",
    );
    let result_source = &sources[0];
    let pending = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_source)
        .unwrap()
        .unwrap();
    assert_eq!(pending.state, TaskResultSettlementState::PersistedPending);
    for source in &sources {
        assert_queued(&runtime, source);
    }
    // Resume the same safe point after rollback; a repeated round must be a no-op.
    let attached = Box::pin(runtime.attach_live_inputs("default", 2, 24 * 1024, results_only))
        .await
        .unwrap();
    assert_eq!(attached.len(), 1, "{fault:?}/{kind:?}");
    {
        let mutation = runtime.begin_mutation().await.unwrap();
        assert_eq!(mutation.state.pending, mutation.queue.len());
        for source in &sources {
            assert!(
                mutation
                    .queue
                    .peek_next_matching(|message| &message.id == source)
                    .is_none(),
                "attached source remains in the runtime queue: {source}"
            );
        }
    }
    let committed = attachment_snapshot(&runtime, &sources).await;
    assert!(
        Box::pin(runtime.attach_live_inputs("default", 2, 24 * 1024, results_only))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(attachment_snapshot(&runtime, &sources).await, committed);
    let admitted = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_source)
        .unwrap()
        .unwrap();
    assert_eq!(admitted.state, TaskResultSettlementState::CallerAdmitted);
    let turn = latest_turn(&runtime);
    assert_eq!(turn.live_input_batches.len(), 1);
    assert_eq!(
        turn.live_input_batches[0].ordinary_message_ids.len(),
        usize::from(!results_only)
    );
    for source in live_ids(&turn) {
        assert!(turn.input_message_ids.contains(&source));
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&source)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Interjected,
        );
        if let Some(delivery) = runtime
            .inner
            .runtime_db
            .agent_message_deliveries()
            .latest_for_message(&source)
            .unwrap()
        {
            assert_eq!(delivery.state, AgentMessageDeliveryState::Consumed);
        }
    }
    assert_eq!(committed["live_events"].as_array().unwrap().len(), 1);
    assert_eq!(
        committed["live_transcript"].as_array().unwrap().len(),
        live_ids(&turn).len()
    );
    assert_live_bounds(&turn);
}

#[tokio::test]
async fn p2_final_attachment_faults_roll_back_all_result_kinds_and_retry_once() {
    for kind in [
        TaskKind::CommandTask,
        TaskKind::ChildAgentTask,
        TaskKind::AgentMessageWait,
    ] {
        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            Box::pin(attachment_fault_case(kind, Reply::Final, fault)).await;
        }
    }
}

#[tokio::test]
async fn p2_paired_tool_attachment_rolls_back_results_and_ordinary_delivery_together() {
    Box::pin(attachment_fault_case(
        TaskKind::CommandTask,
        Reply::PairedTools,
        TransitionFaultPoint::AfterCanonicalWrites,
    ))
    .await;
}

#[tokio::test]
async fn p2_pure_final_does_not_reenter_for_ordinary_or_older_parent_input() {
    for older_result in [false, true] {
        let (_data, _workspace, runtime, provider) = fixture().await;
        let receiver = owner(&runtime, "p2 no final extension").await;
        *provider.steps.lock().await = vec![Step {
            inputs: vec![if older_result {
                result(Some(&receiver), TaskKind::CommandTask, false, 1)
            } else {
                ordinary(Some(&receiver), "p2-peer", true)
            }],
            reply: Reply::Final,
        }];
        run(&runtime, Some(&receiver)).await;
        assert_eq!(provider.requests.lock().await.len(), 1);
        assert!(latest_turn(&runtime).live_input_batches.is_empty());
        assert_queued(&runtime, &provider.sources.lock().await[0]);
    }
}

#[tokio::test]
async fn p2_parent_turn_grant_does_not_migrate_the_result_owner() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 consuming owner").await;
    let foreign = owner(&runtime, "p2 result owner").await;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![result(Some(&foreign), TaskKind::CommandTask, true, 1)],
        reply: Reply::Final,
    }];
    run(&runtime, Some(&receiver)).await;
    assert_eq!(provider.requests.lock().await.len(), 2);
    let source = provider.sources.lock().await[0].clone();
    let settlement = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(&source)
        .unwrap()
        .unwrap();
    assert_eq!(settlement.state, TaskResultSettlementState::Settled);
    assert_eq!(
        runtime
            .storage()
            .read_message_by_id(&source)
            .unwrap()
            .unwrap()
            .work_item_id
            .as_deref(),
        Some(foreign.as_str())
    );
    assert_eq!(settlement.work_item_id.as_deref(), Some(foreign.as_str()));
    assert_eq!(
        latest_turn(&runtime).effective_owner().work_item_id(),
        Some(receiver.as_str())
    );
}

#[tokio::test]
async fn p2_final_result_backlog_stops_at_four_batches_and_32_identities() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 bounded final backlog").await;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![result(Some(&receiver), TaskKind::CommandTask, true, 40)],
        reply: Reply::Final,
    }];
    run(&runtime, Some(&receiver)).await;
    let turn = latest_turn(&runtime);
    assert_eq!(provider.requests.lock().await.len(), 5);
    assert_eq!(turn.live_input_batches.len(), 4);
    assert_eq!(
        turn.live_input_batches
            .iter()
            .map(|batch| batch.result_identities.len())
            .sum::<usize>(),
        32
    );
    assert_live_bounds(&turn);
    let mut settled = 0;
    for source in provider.sources.lock().await.iter() {
        let record = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(source)
            .unwrap()
            .unwrap();
        if record.state == TaskResultSettlementState::Settled {
            settled += 1;
        } else {
            assert_queued(&runtime, source);
        }
    }
    assert_eq!(settled, 32);
}

#[tokio::test]
async fn p2_rendered_byte_limits_bound_oversized_live_result_backlog() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 bounded result bytes").await;
    let mut inputs = result(Some(&receiver), TaskKind::CommandTask, true, 40);
    inputs.body_bytes = 50_000;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![inputs],
        reply: Reply::Final,
    }];
    run(&runtime, Some(&receiver)).await;
    let turn = latest_turn(&runtime);
    assert!(!turn.live_input_batches.is_empty());
    assert!(provider.requests.lock().await.len() <= 5);
    assert_live_bounds(&turn);
    assert!(turn
        .live_input_batches
        .iter()
        .flat_map(|batch| &batch.items)
        .all(|item| item.rendered.len() < 50_000));
    let queued = provider
        .sources
        .lock()
        .await
        .iter()
        .filter(|source| {
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(source)
                .unwrap()
                .unwrap()
                .status
                == QueueEntryStatus::Queued
        })
        .count();
    assert!(queued > 0);
}

#[tokio::test]
async fn p2_pending_report_and_silent_terminal_refuse_new_attachments() {
    for reply in [Reply::WaitFinal, Reply::WaitSilent, Reply::Complete] {
        let (_data, _workspace, runtime, provider) = fixture().await;
        let receiver = owner(&runtime, "p2 terminal fence").await;
        *provider.steps.lock().await = vec![Step {
            inputs: vec![
                ordinary(Some(&receiver), "p2-peer", true),
                result(Some(&receiver), TaskKind::CommandTask, true, 1),
            ],
            reply,
        }];
        run(&runtime, Some(&receiver)).await;
        assert!(latest_turn(&runtime).live_input_batches.is_empty());
        assert_eq!(
            provider.requests.lock().await.len(),
            if matches!(reply, Reply::WaitSilent) {
                1
            } else {
                2
            }
        );
        for source in provider.sources.lock().await.iter() {
            assert_queued(&runtime, source);
        }
    }
}

#[tokio::test]
async fn p2_stop_during_provider_flight_does_not_consume_new_inputs() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 stop fence").await;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![ordinary(Some(&receiver), "p2-peer", true)],
        reply: Reply::Stop,
    }];
    message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "anchor",
        "p2 anchor",
        true,
    )
    .await;
    // Exercise the production abort settlement, not only its deferred result.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Box::pin(runtime.clone().run()),
    )
    .await
    .expect("Stop must finish the production run loop")
    .unwrap();
    // Stop exits the run loop only after its aborted turn is durably settled.
    assert_eq!(
        runtime.agent_state().await.unwrap().status,
        AgentStatus::Stopped
    );
    assert_eq!(provider.requests.lock().await.len(), 1);
    let turn = latest_turn(&runtime);
    assert!(turn.live_input_batches.is_empty());
    assert_eq!(
        turn.terminal.as_ref().unwrap().kind,
        TurnTerminalKind::Interrupted
    );
    let source = provider.sources.lock().await[0].clone();
    assert!(!turn.input_message_ids.contains(&source));
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&source)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Aborted
    );
}

#[tokio::test]
async fn p2_round_limit_refuses_sources_without_an_available_next_request() {
    let (_data, _workspace, runtime, provider) = fixture().await;
    let receiver = owner(&runtime, "p2 round capacity").await;
    *provider.steps.lock().await = vec![Step {
        inputs: vec![result(Some(&receiver), TaskKind::CommandTask, true, 1)],
        reply: Reply::PairedTools,
    }];
    message(
        &runtime,
        Some(&receiver),
        "p2-peer",
        "anchor",
        "p2 anchor",
        true,
    )
    .await;
    let scheduled = Box::pin(claim(&runtime)).await;
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled.dispatch_plan.execution_admission_provenance,
            &scheduled.dispatch_plan.prior_closure,
        )
        .await
        .unwrap();
    runtime
        .record_incoming_transcript_entry(&scheduled.message)
        .await
        .unwrap();
    let outcome = Box::pin(runtime.run_agent_loop(
        "default",
        AuthorityClass::RuntimeInstruction,
        test_effective_prompt(),
        LoopControlOptions {
            max_tool_rounds: Some(1),
        },
    ))
    .await
    .unwrap();
    assert!(outcome.final_text.contains("maximum tool loop depth"));
    assert_eq!(provider.requests.lock().await.len(), 1);
    assert!(latest_turn(&runtime).live_input_batches.is_empty());
    assert_queued(&runtime, &provider.sources.lock().await[0]);
}
