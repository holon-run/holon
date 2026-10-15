//! Shared original replies retain independently budgeted waiter obligations.
use super::super::*;
use super::support::*;
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementDisposition, TaskResultSettlementState,
};
use crate::types::{
    AdmissionContext, AgentMessageCallerContext, AgentMessagePrincipalKind,
    AgentMessageSendRequest, QueueEntryRecord, TurnOwner, WorkItemPlanStatus,
};
use std::collections::BTreeSet;

#[derive(Default)]
struct SharedReplyProvider {
    requests: Mutex<Vec<ProviderTurnRequest>>,
}

fn current_source_context(request: &ProviderTurnRequest) -> String {
    request
        .prompt_frame
        .context_blocks
        .iter()
        .flat_map(|block| block.text.split("\n## "))
        .filter(|section| {
            let section = section.trim_start_matches("## ");
            section.starts_with("current_input\n") || section.starts_with("pending_task_results\n")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_exact_wait_is_reserved_before_same_owner_overflow() {
    let provider = Arc::new(SharedReplyProvider::default());
    let harness = LifecycleHarness::with_provider(provider);
    let runtime = harness.runtime();
    let receiver = owner(runtime, "overflow exact waiter").await;
    runtime.pick_work_item(receiver.clone()).await.unwrap();
    let mut tasks = Vec::new();
    for index in 0..17 {
        tasks.push(waiter(runtime, Some(&receiver), &format!("exact-overflow-{index}")).await);
    }
    let promised = tasks.last().unwrap();
    let outcome = runtime
        .register_wait_for_outcome(
            "default",
            Some(receiver.clone()),
            WaitForWakeKind::TaskResult,
            Some(promised.id.clone()),
            "promised identity is not ordinary overflow".into(),
            None,
        )
        .await
        .unwrap();
    let WaitForRegistrationOutcome::Registered { registration } = outcome else {
        panic!("running task must register an exact waiter");
    };
    let receiver_record = runtime.latest_work_item(&receiver).await.unwrap().unwrap();
    seed_waiting_work_execution(
        runtime.storage(),
        &receiver_record,
        &registration.condition.id,
    );
    let original = reply(
        runtime,
        Some(&receiver),
        "overflow-exact",
        "overflow exact body",
    )
    .await;
    let mut promised_source = None;
    for task in &tasks {
        let source = finish(runtime, task, &original).await;
        if task.id == promised.id {
            promised_source = Some(source);
        }
    }
    let mut batch = None;
    for _ in 0..24 {
        if let Some(observed) = process_next(runtime).await {
            batch = Some(observed);
            break;
        }
    }
    let batch = batch.expect("exact shared waiter must resume");
    let promised_record = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&promised_source.unwrap())
        .unwrap()
        .unwrap();
    assert!(
        batch.identities.contains(&promised_record.result_identity),
        "{batch:?}"
    );
    assert_eq!(
        promised_record.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert!(batch.identities.len() <= 7, "{batch:?}");
    assert_eq!(batch.ordinary_sources, 1, "{batch:?}");
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_rejects_terminal_waiter_replacement_and_foreign_owner() {
    let provider = Arc::new(SharedReplyProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let receiver = owner(runtime, "selected receiver").await;
    let foreign_owner = owner(runtime, "independent receiver").await;
    runtime.pick_work_item(receiver.clone()).await.unwrap();
    let original = reply(runtime, Some(&receiver), "isolated", "isolated shared body").await;
    let legal = waiter(runtime, Some(&receiver), "legal-waiter").await;
    let foreign = waiter(runtime, Some(&foreign_owner), "foreign-waiter").await;
    let stale = waiter(runtime, Some(&receiver), "replaced-waiter").await;
    let legal_source = finish(runtime, &legal, &original).await;
    let foreign_source = finish(runtime, &foreign, &original).await;
    let stale_source = finish(runtime, &stale, &original).await;
    let mut replacement = runtime
        .storage()
        .latest_task_record(&stale.id)
        .unwrap()
        .unwrap();
    replacement.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    assert!(runtime.runtime_db().tasks().upsert(&replacement).is_err());
    let mut batches = Vec::new();
    for _ in 0..12 {
        if let Some(batch) = process_next(runtime).await {
            batches.push(batch);
        }
        if all_settled(runtime, std::slice::from_ref(&legal_source)) {
            break;
        }
    }
    assert!(all_settled(runtime, &[legal_source, stale_source.clone()]));
    assert_eq!(
        batches
            .iter()
            .map(|batch| batch.identities.len())
            .sum::<usize>(),
        2
    );
    let repository = runtime.runtime_db().task_result_settlements();
    let foreign_record = repository
        .latest_for_message(&foreign_source)
        .unwrap()
        .unwrap();
    assert_eq!(
        foreign_record.state,
        TaskResultSettlementState::PersistedPending
    );
    assert!(foreign_record.activation_id.is_none());
    let mut stale_record = repository
        .latest_for_message(&stale_source)
        .unwrap()
        .unwrap();
    assert_eq!(
        stale_record.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    stale_record.rejoin_generation += 1;
    assert!(!repository
        .reply_source_is_consumed_for_owner(&stale_record)
        .unwrap());
    // Even an explicitly selected foreign identity must fail its own source
    // transaction; the neighboring legal task is not a transferable grant.
    let selected = BTreeSet::from([foreign_record.result_identity]);
    assert!(repository
        .admit_reply_message_snapshot(
            "default",
            Some(&receiver),
            &original.id,
            &batches[0].activation,
            &selected,
            runtime.now(),
        )
        .unwrap()
        .is_empty());
    assert_eq!(provider.requests.lock().await.len(), batches.len());
    assert_eq!(
        runtime
            .storage()
            .latest_task_record(&foreign.id)
            .unwrap()
            .unwrap()
            .work_item_id,
        Some(foreign_owner)
    );
    harness.advance(std::time::Duration::from_secs(31)).await;
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let elsewhere = repository
        .latest_for_message(&foreign_source)
        .unwrap()
        .unwrap();
    assert_eq!(
        elsewhere.disposition,
        Some(TaskResultSettlementDisposition::ReplyConsumedElsewhere)
    );
    assert!(elsewhere.activation_id.is_none());
    assert_eq!(provider.requests.lock().await.len(), batches.len());
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_exact_cross_owner_wait_only_admits_its_promised_identity() {
    let provider = Arc::new(SharedReplyProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let receiver = owner(runtime, "exact waiter").await;
    let captured_owner = owner(runtime, "captured task owner").await;
    let outsider = owner(runtime, "unrelated task owner").await;
    runtime.pick_work_item(receiver.clone()).await.unwrap();
    let mut blocked = runtime
        .runtime_db()
        .work_items()
        .latest(&captured_owner)
        .unwrap()
        .unwrap();
    blocked.blocked_by = Some("independent business gate".into());
    blocked.revision += 1;
    runtime.storage().append_work_item(&blocked).unwrap();
    let promised = waiter(runtime, Some(&captured_owner), "promised-cross-owner").await;
    let sibling = waiter(runtime, Some(&outsider), "unpromised-sibling").await;
    let outcome = runtime
        .register_wait_for_outcome(
            "default",
            Some(receiver.clone()),
            WaitForWakeKind::TaskResult,
            Some(promised.id.clone()),
            "exact shared reply dependency".into(),
            None,
        )
        .await
        .unwrap();
    let WaitForRegistrationOutcome::Registered { registration } = outcome else {
        panic!("running task must register an exact waiter");
    };
    let receiver_record = runtime.latest_work_item(&receiver).await.unwrap().unwrap();
    seed_waiting_work_execution(
        runtime.storage(),
        &receiver_record,
        &registration.condition.id,
    );
    let original = reply(
        runtime,
        Some(&receiver),
        "exact-original",
        "exact shared body",
    )
    .await;
    let promised_source = finish(runtime, &promised, &original).await;
    let sibling_source = finish(runtime, &sibling, &original).await;
    let mut batches = Vec::new();
    for _ in 0..12 {
        if let Some(batch) = process_next(runtime).await {
            batches.push(batch);
        }
        if all_settled(runtime, std::slice::from_ref(&promised_source)) {
            break;
        }
    }
    assert!(all_settled(runtime, std::slice::from_ref(&promised_source)));
    assert_eq!(
        batches
            .iter()
            .map(|batch| batch.identities.len())
            .sum::<usize>(),
        1
    );
    let sibling = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&sibling_source)
        .unwrap()
        .unwrap();
    let repository = runtime.runtime_db().task_result_settlements();
    assert!(repository
        .reply_source_is_consumed_for_owner(
            &repository
                .latest_for_message(&promised_source)
                .unwrap()
                .unwrap(),
        )
        .unwrap());
    assert!(!repository
        .reply_source_is_consumed_for_owner(&sibling)
        .unwrap());
    assert_eq!(sibling.state, TaskResultSettlementState::PersistedPending);
    assert_eq!(sibling.work_item_id.as_deref(), Some(outsider.as_str()));
    assert_eq!(
        runtime
            .storage()
            .latest_task_record(&promised.id)
            .unwrap()
            .unwrap()
            .work_item_id,
        Some(captured_owner.clone())
    );
    assert_eq!(
        runtime
            .runtime_db()
            .work_items()
            .latest(&captured_owner)
            .unwrap()
            .unwrap()
            .blocked_by
            .as_deref(),
        Some("independent business gate"),
        "resuming another owner's exact waiter must not clear the task owner's blocker"
    );
    let wait = runtime
        .storage()
        .latest_wait_conditions_for_agent("default")
        .unwrap()
        .into_iter()
        .find(|wait| wait.id == registration.condition.id)
        .unwrap();
    assert_eq!(wait.status, crate::types::WaitConditionStatus::Resolved);
    assert_eq!(wait.trigger_message_id(), Some(original.id.as_str()));
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(
        turn.effective_owner(),
        crate::types::TurnOwner::WorkItem {
            work_item_id: receiver
        }
    );
    assert_eq!(provider.requests.lock().await.len(), batches.len());
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_oversize_and_ordinary_inputs_share_identity_and_byte_limits() {
    for oversized in [false, true] {
        let provider = Arc::new(SharedReplyProvider::default());
        let harness = LifecycleHarness::with_provider(provider.clone());
        let runtime = harness.runtime();
        let receiver = owner(runtime, "mixed shared reply").await;
        runtime.pick_work_item(receiver.clone()).await.unwrap();
        let body = if oversized {
            format!("oversized-original-marker {}", "🦀".repeat(16000))
        } else {
            "mixed-original-marker".into()
        };
        let original = reply(runtime, Some(&receiver), "mixed-original", &body).await;
        let mut sources = Vec::new();
        for index in 0..9 {
            let task = waiter(runtime, Some(&receiver), &format!("mixed-wait-{index}")).await;
            sources.push(finish(runtime, &task, &original).await);
        }
        let commands = [
            command_result(runtime, &receiver, "mixed-command-one").await,
            command_result(runtime, &receiver, "mixed-command-two").await,
        ];
        let first = reply(runtime, Some(&receiver), "ordinary-one", "ordinary first").await;
        let second = reply(runtime, Some(&receiver), "ordinary-two", "ordinary second").await;
        let mut first_batch = None;
        for _ in 0..14 {
            if let Some(batch) = process_next(runtime).await {
                first_batch = Some(batch);
                break;
            }
        }
        let batch = first_batch.expect("shared reply opens a canonical execution");
        assert!(batch.identities.len() <= 5, "{batch:?}");
        assert!(!batch.identities.is_empty(), "{batch:?}");
        assert!(batch.input_ids.contains(&original.id), "{batch:?}");
        assert!(batch.input_ids.contains(&first.id), "{batch:?}");
        assert!(batch.input_ids.contains(&second.id), "{batch:?}");
        assert!(
            batch.identities.len() + batch.ordinary_sources <= 8,
            "{batch:?}"
        );
        assert!(batch.rendered_bytes <= 24 * 1024, "{batch:?}");
        assert!(
            !all_settled(runtime, &sources),
            "overflow must remain durable"
        );
        for source in commands {
            let record = runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&source)
                .unwrap()
                .unwrap();
            if batch.identities.contains(&record.result_identity) {
                assert_eq!(record.state, TaskResultSettlementState::Settled);
            } else {
                assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
            }
        }
        let requests = provider.requests.lock().await;
        let context = current_source_context(&requests[0]);
        let marker = if oversized {
            "oversized-original-marker"
        } else {
            "mixed-original-marker"
        };
        assert_eq!(context.matches(marker).count(), 1);
        if oversized {
            assert!(context.contains("bounded preview"));
            assert!(context.contains(&format!("message:{}", original.id)));
            assert!(!context.contains(&body));
        }
    }
}

#[async_trait]
impl AgentProvider for SharedReplyProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.lock().await.push(request);
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "shared reply handled".into(),
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

async fn reply(
    runtime: &RuntimeHandle,
    owner: Option<&str>,
    key: &str,
    body: &str,
) -> MessageEnvelope {
    let mut prepared = AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id: "default".into(),
            content: MessageBody::Text { text: body.into() },
            client_idempotency_key: key.into(),
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
    // Explicit receiver ownership is fixture input, never inferred from sender.
    prepared.message.work_item_id = owner.map(str::to_owned);
    prepared.message.created_at = runtime.now();
    let id = runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await
        .unwrap()
        .message_id
        .unwrap();
    runtime.storage().read_message_by_id(&id).unwrap().unwrap()
}

async fn waiter(runtime: &RuntimeHandle, owner: Option<&str>, id: &str) -> TaskRecord {
    let task = TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Running,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: None,
        work_item_id: owner.map(str::to_owned),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": id,
            "rejoin_generation": 1,
            "parent_turn_id": "shared-reply-parent",
        })),
        recovery: Some(crate::types::TaskRecoverySpec::AgentMessageWait {
            summary: "shared reply observation".into(),
            message: "observe shared reply".into(),
            target_agent_id: "shared-reply-peer".into(),
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_id: None,
            after_delivery_rowid: Some(0),
        }),
    };
    runtime
        .persist_task_transition(&task, "task_created")
        .await
        .unwrap();
    task
}

async fn finish(runtime: &RuntimeHandle, task: &TaskRecord, reply: &MessageEnvelope) -> String {
    let mut detail = task.detail.clone().unwrap();
    detail["message_id"] = serde_json::json!(reply.id);
    detail["response_delivery_id"] =
        reply.metadata.as_ref().unwrap()["agent_message_delivery"]["delivery_id"].clone();
    runtime
        .finish_agent_message_wait(task, TaskStatus::Completed, detail, String::new())
        .await
        .unwrap();
    runtime
        .storage()
        .latest_task_record(&task.id)
        .unwrap()
        .unwrap()
        .parent_message_id
        .unwrap()
}

async fn command_result(runtime: &RuntimeHandle, owner: &str, id: &str) -> String {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task { task_id: id.into() },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: format!("ordinary command result {id}"),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.work_item_id = Some(owner.into());
    message.metadata = Some(serde_json::json!({
        "task_id": id,
        "task_kind": "command_task",
        "task_status": "completed",
    }));
    let task = TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: Some(message.id.clone()),
        work_item_id: Some(owner.into()),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": id,
            "rejoin_generation": 1,
            "parent_turn_id": "shared-reply-command-parent",
            "terminal_reentry": true,
        })),
        recovery: None,
    };
    runtime
        .commit_terminal_task_result(&task, "task_completed", &message)
        .await
        .unwrap();
    message.id
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_restart_replays_admitted_identities_before_fresh_sources() {
    let provider = Arc::new(SharedReplyProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let work = owner(harness.runtime(), "interrupted shared reply").await;
    harness
        .runtime()
        .pick_work_item(work.clone())
        .await
        .unwrap();
    let original = reply(
        harness.runtime(),
        Some(&work),
        "interrupted-original",
        "crash original",
    )
    .await;
    for index in 0..17 {
        let task = waiter(
            harness.runtime(),
            Some(&work),
            &format!("interrupted-{index}"),
        )
        .await;
        finish(harness.runtime(), &task, &original).await;
    }
    let anchor = start_without_provider(harness.runtime()).await;
    assert_eq!(anchor.id, original.id);
    assert!(harness
        .runtime()
        .admit_idle_task_results(&anchor, 24 * 1024)
        .await
        .unwrap());
    assert!(harness
        .runtime()
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    let original_turn = harness
        .runtime()
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .remove(0);
    let original_activation = original_turn
        .idle_input_batch
        .as_ref()
        .unwrap()
        .activation_id
        .as_ref()
        .unwrap();
    let previous = harness
        .runtime()
        .runtime_db()
        .task_result_settlements()
        .admitted_for_activation("default", original_activation)
        .unwrap()
        .into_iter()
        .map(|record| record.result_identity)
        .collect::<BTreeSet<_>>();
    assert_eq!(previous.len(), 7);
    assert!(provider.requests.lock().await.is_empty());
    let after_turn_creation = (original_turn.created_at - harness.now())
        .to_std()
        .unwrap_or_default()
        + std::time::Duration::from_secs(1);
    harness.advance(after_turn_creation).await;
    let late = reply(
        harness.runtime(),
        Some(&work),
        "late-unclaimed",
        "late fresh content",
    )
    .await;
    harness.restart();
    harness
        .runtime()
        .recover_scheduler_bootstrap_claims()
        .await
        .unwrap();
    let interrupted_turn = harness
        .runtime()
        .storage()
        .read_turn_by_id(&original_turn.turn_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        interrupted_turn.terminal.as_ref().unwrap().kind,
        TurnTerminalKind::Interrupted
    );
    let execution = harness
        .runtime()
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution.attempts[original_activation].state,
        crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
    );
    let batch = process_next(harness.runtime())
        .await
        .expect("interrupted source must resume");
    assert_eq!(batch.identities, previous);
    assert_eq!(batch.input_ids, original_turn.input_message_ids);
    assert!(!batch.input_ids.contains(&late.id));
    assert_eq!(
        harness
            .runtime()
            .runtime_db()
            .queue_entries()
            .latest(&late.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 1);
    let context = current_source_context(&requests[0]);
    assert_eq!(context.matches("crash original").count(), 1);
    assert!(!context.contains("late fresh content"));
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_processed_source_alone_does_not_authorize_recovery() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let work = owner(runtime, "forged consumption").await;
    let original = reply(
        runtime,
        Some(&work),
        "processed-without-effect",
        "not executed",
    )
    .await;
    let task = waiter(runtime, Some(&work), "unconsumed-waiter").await;
    let source = finish(runtime, &task, &original).await;
    runtime
        .runtime_db()
        .queue_entries()
        .upsert(&QueueEntryRecord {
            message_id: original.id,
            agent_id: "default".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Processed,
            created_at: original.created_at,
            updated_at: Utc::now(),
        })
        .unwrap();
    let record = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&source)
        .unwrap()
        .unwrap();
    assert!(!runtime
        .runtime_db()
        .task_result_settlements()
        .reply_source_is_recoverable(&record)
        .unwrap());
    assert!(record.activation_id.is_none());
    harness.advance(std::time::Duration::from_secs(31)).await;
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    let record = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&source)
        .unwrap()
        .unwrap();
    assert!(record.activation_id.is_none());
    assert!(record.admitted_at.is_none());
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_manifest_keeps_waiter_rows_and_deduplicates_only_reply_bodies() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let work = owner(runtime, "shared manifest").await;
    let original = reply(
        runtime,
        Some(&work),
        "manifest-original",
        "manifest original body",
    )
    .await;
    let other = reply(
        runtime,
        Some(&work),
        "manifest-other",
        "manifest other body",
    )
    .await;
    let mut records = Vec::new();
    for (index, source) in [&original, &original, &original, &other]
        .into_iter()
        .enumerate()
    {
        let task = waiter(runtime, Some(&work), &format!("manifest-{index}")).await;
        let message_id = finish(runtime, &task, source).await;
        records.push(
            runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&message_id)
                .unwrap()
                .unwrap(),
        );
    }
    let mut unrelated = original.clone();
    unrelated.id = "manifest-unrelated-anchor".into();
    for anchor in [&original, &unrelated] {
        let text =
            crate::context::render_pending_task_result_records(runtime.storage(), anchor, &records)
                .unwrap()
                .unwrap();
        assert_eq!(text.matches("- result_identity: ").count(), records.len());
        for record in &records {
            assert_eq!(text.matches(&record.result_identity).count(), 1);
            assert!(text.contains(&format!("task_id: {}", record.task_id)));
        }
        assert_eq!(
            text.matches("manifest original body").count(),
            usize::from(anchor.id != original.id)
        );
        assert_eq!(text.matches("manifest other body").count(), 1);
        assert_eq!(
            text.matches(&format!("reply_source_ref: message:{}", original.id))
                .count(),
            3
        );
    }
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_recovery_scope_requires_durable_source_binding_and_consuming_owner() {
    let provider = Arc::new(SharedReplyProvider::default());
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let work = owner(runtime, "recovery scope").await;
    let foreign = runtime
        .create_work_item(
            "foreign recovery scope".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            None,
            Vec::new(),
        )
        .await
        .unwrap()
        .id;
    runtime.pick_work_item(work.clone()).await.unwrap();
    let original = reply(
        runtime,
        Some(&work),
        "scope-original",
        "scope original body",
    )
    .await;
    let mut sources = Vec::new();
    for index in 0..9 {
        let task = waiter(runtime, Some(&work), &format!("scope-{index}")).await;
        sources.push(finish(runtime, &task, &original).await);
    }
    let mut first = None;
    for _ in 0..24 {
        if let Some(batch) = process_next(runtime).await {
            first = Some(batch);
            break;
        }
    }
    assert_eq!(first.unwrap().identities.len(), 7);
    let consumed_turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    // Lose only the remaining transport notifications, not their result ledger.
    for source in sources {
        let record = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&source)
            .unwrap()
            .unwrap();
        if record.state == TaskResultSettlementState::PersistedPending {
            let mut queue = runtime
                .runtime_db()
                .queue_entries()
                .latest(&source)
                .unwrap()
                .unwrap();
            queue.status = QueueEntryStatus::Processed;
            runtime
                .commit_queue_terminal_settlement(queue, Vec::new(), false, None)
                .await
                .unwrap();
        }
    }
    harness.restart();
    let runtime = harness.runtime();
    runtime.recover_scheduler_bootstrap_claims().await.unwrap();
    harness.advance(std::time::Duration::from_secs(31)).await;
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let anchor = start_without_provider(runtime).await;
    assert!(
        matches!(&anchor.origin, MessageOrigin::System { subsystem }
            if subsystem == "task_result_recovery"),
        "unexpected recovery anchor: {anchor:?}"
    );
    let binding = runtime
        .agent_state()
        .await
        .unwrap()
        .current_execution_binding
        .unwrap();
    assert!(runtime
        .idle_reply_recovery_source_valid(&anchor, &binding)
        .unwrap());
    for key in [
        "task_result_message_id",
        "task_result_identity",
        "task_result_owner_key",
        "task_result_rejoin_generation",
        "task_result_parent_turn_id",
    ] {
        let mut forged = anchor.clone();
        forged
            .source_refs
            .insert(key.into(), "foreign-or-stale".into());
        assert!(
            !runtime
                .idle_reply_recovery_source_valid(&forged, &binding)
                .unwrap(),
            "{key}"
        );
    }
    let mut forged = anchor.clone();
    forged.agent_id = "foreign-agent".into();
    assert!(!runtime
        .idle_reply_recovery_source_valid(&forged, &binding)
        .unwrap());
    forged = anchor.clone();
    forged.id = "missing-recovery-ledger-source".into();
    let mut unbound = binding.clone();
    unbound.source_message_id = forged.id.clone();
    assert!(!runtime
        .idle_reply_recovery_source_valid(&forged, &unbound)
        .unwrap());
    unbound = binding.clone();
    unbound.work_item_id = Some(foreign.clone());
    unbound.owner = Some(TurnOwner::WorkItem {
        work_item_id: foreign.clone(),
    });
    assert!(!runtime
        .idle_reply_recovery_source_valid(&anchor, &unbound)
        .unwrap());
    unbound = binding.clone();
    unbound.admission_provenance = None;
    assert!(!runtime
        .idle_reply_recovery_source_valid(&anchor, &unbound)
        .unwrap());

    let repository = runtime.runtime_db().task_result_settlements();
    let selector = repository
        .latest_for_message(&anchor.source_refs["task_result_message_id"])
        .unwrap()
        .unwrap();
    let task = runtime
        .runtime_db()
        .tasks()
        .latest(&selector.task_id)
        .unwrap()
        .unwrap();
    let mut replacement = task.clone();
    replacement.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    assert!(runtime.runtime_db().tasks().upsert(&replacement).is_err());
    let mut stale_selector = selector.clone();
    stale_selector.rejoin_generation += 1;
    assert!(!repository
        .reply_source_is_consumed_for_owner(&stale_selector)
        .unwrap());

    let mut elsewhere = consumed_turn.clone();
    elsewhere.current_work_item_id = Some(foreign.clone());
    elsewhere.owner = Some(TurnOwner::WorkItem {
        work_item_id: foreign,
    });
    assert!(runtime.storage().append_turn(&elsewhere).is_err());
    assert!(runtime
        .idle_reply_recovery_source_valid(&anchor, &binding)
        .unwrap());

    assert!(runtime
        .admit_idle_task_results(&anchor, 24 * 1024)
        .await
        .unwrap());
    assert_eq!(
        repository
            .settle_activation(
                "default",
                binding.activation_id.as_deref().unwrap(),
                TaskResultSettlementDisposition::ModelDelivered,
                runtime.now(),
            )
            .unwrap(),
        2
    );
    assert!(
        runtime
            .idle_reply_recovery_source_valid(&anchor, &binding)
            .unwrap(),
        "a settled selector does not invalidate the owner wake's source proof"
    );
    let mut terminal_queue = runtime
        .runtime_db()
        .queue_entries()
        .latest(&anchor.id)
        .unwrap()
        .unwrap();
    terminal_queue.status = QueueEntryStatus::Processed;
    runtime
        .runtime_db()
        .queue_entries()
        .upsert(&terminal_queue)
        .unwrap();
    assert!(!runtime
        .idle_reply_recovery_source_valid(&anchor, &binding)
        .unwrap());
    assert_eq!(provider.requests.lock().await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_rendered_byte_capacity_is_reserved_before_source_admission() {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let work = owner(runtime, "small shared byte capacity").await;
    runtime.pick_work_item(work.clone()).await.unwrap();
    let original = reply(
        runtime,
        Some(&work),
        "small-capacity",
        "small capacity body",
    )
    .await;
    let mut sources = Vec::new();
    for index in 0..17 {
        let task = waiter(runtime, Some(&work), &format!("small-capacity-{index}")).await;
        sources.push(finish(runtime, &task, &original).await);
    }
    let anchor = start_without_provider(runtime).await;
    let limit = 3000usize;
    assert!(runtime
        .admit_idle_task_results(&anchor, limit)
        .await
        .unwrap());
    let state = runtime.agent_state().await.unwrap();
    let activation = state
        .current_execution_binding
        .unwrap()
        .activation_id
        .unwrap();
    let records = runtime
        .runtime_db()
        .task_result_settlements()
        .admitted_for_activation("default", &activation)
        .unwrap();
    assert!(!records.is_empty());
    assert!(
        records.len() < 8,
        "render cost must constrain admission: {records:?}"
    );
    let pending =
        crate::context::render_pending_task_result_records(runtime.storage(), &anchor, &records)
            .unwrap()
            .unwrap();
    assert!(runtime
        .attach_idle_sender_inputs(&anchor, limit.saturating_sub(pending.len()))
        .await
        .unwrap());
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let batch = turn.idle_input_batch.unwrap();
    assert!(batch.rendered_bytes + pending.len() <= limit);
    let admitted = records
        .iter()
        .map(|record| record.result_identity.as_str())
        .collect::<BTreeSet<_>>();
    for source in sources {
        let record = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&source)
            .unwrap()
            .unwrap();
        if !admitted.contains(record.result_identity.as_str()) {
            assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
            assert!(record.activation_id.is_none());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_source_rehoming_requires_interrupted_predecessor() {
    use crate::domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionBinding,
    };

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let work = owner(runtime, "reply admission recovery fences").await;
    runtime.pick_work_item(work.clone()).await.unwrap();
    let original = reply(runtime, Some(&work), "rehome-original", "rehome body").await;
    let task = waiter(runtime, Some(&work), "rehome-waiter").await;
    let source = finish(runtime, &task, &original).await;
    let anchor = start_without_provider(runtime).await;
    assert!(runtime
        .admit_idle_task_results(&anchor, 24 * 1024)
        .await
        .unwrap());
    let repository = runtime.runtime_db().task_result_settlements();
    let record = repository.latest_for_message(&source).unwrap().unwrap();
    let prior_id = record.activation_id.clone().unwrap();
    let execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    let original_attempt = execution.attempts[&prior_id].clone();
    let selected = BTreeSet::from([record.result_identity]);
    let store = |prior: &ExecutionAttempt, current: &ExecutionAttempt| {
        runtime
            .runtime_db()
            .transaction(|tx| {
                for attempt in [prior, current] {
                    tx.execute(
                        "INSERT INTO execution_protocol_attempts (
                           agent_id, attempt_id, lifecycle_state, source_identity_json,
                           source_generation, recovery_of_attempt_id, payload_json
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT(agent_id, attempt_id) DO UPDATE SET
                           lifecycle_state = excluded.lifecycle_state,
                           recovery_of_attempt_id = excluded.recovery_of_attempt_id,
                           payload_json = excluded.payload_json",
                        rusqlite::params![
                            attempt.agent_id,
                            attempt.attempt_id,
                            serde_json::to_value(attempt.state)?.as_str().unwrap(),
                            serde_json::to_string(&attempt.source.identity)?,
                            attempt.source.generation as i64,
                            attempt.recovery_of_attempt_id,
                            serde_json::to_string(attempt)?,
                        ],
                    )?;
                }
                Ok(())
            })
            .unwrap();
    };
    let mut recovered = None;
    for case in [
        "missing-link",
        "missing-source",
        "foreign-source",
        "foreign-owner",
        "closed-current",
        "settled-predecessor",
        "missing-predecessor",
        "continuous",
    ] {
        let mut prior = original_attempt.clone();
        prior.state = ExecutionAttemptState::Interrupted;
        let mut current = original_attempt.clone();
        current.attempt_id = format!("{prior_id}:reply-recovery");
        current.recovery_of_attempt_id = Some(prior_id.clone());
        match case {
            "missing-link" => current.recovery_of_attempt_id = None,
            "missing-source" => current.source_message_id = None,
            "foreign-source" => current.source_message_id = Some("unrelated-source".into()),
            "foreign-owner" => {
                current.binding = ExecutionBinding::WorkItem {
                    work_item_id: "unrelated-owner".into(),
                };
            }
            "closed-current" => current.state = ExecutionAttemptState::Settled,
            "settled-predecessor" => prior.state = ExecutionAttemptState::Settled,
            "missing-predecessor" => {
                current.recovery_of_attempt_id = Some("missing-attempt".into());
            }
            _ => {}
        }
        store(&prior, &current);
        let admitted = repository
            .admit_reply_message_snapshot(
                "default",
                Some(&work),
                &original.id,
                &current.attempt_id,
                &selected,
                Utc::now(),
            )
            .unwrap();
        assert_eq!(admitted.len(), usize::from(case == "continuous"), "{case}");
        let latest = repository.latest_for_message(&source).unwrap().unwrap();
        assert_eq!(
            latest.activation_id.as_deref(),
            Some(if case == "continuous" {
                current.attempt_id.as_str()
            } else {
                prior_id.as_str()
            }),
            "{case}"
        );
        if case == "continuous" {
            recovered = Some(current);
        }
    }
    let mut prior = recovered.unwrap();
    prior.state = ExecutionAttemptState::Interrupted;
    let mut current = prior.clone();
    current.attempt_id = format!("{}:again", prior.attempt_id);
    current.recovery_of_attempt_id = Some(prior.attempt_id.clone());
    current.state = ExecutionAttemptState::Open;
    store(&prior, &current);
    assert_eq!(
        repository
            .admit_reply_message_snapshot(
                "default",
                Some(&work),
                &original.id,
                &current.attempt_id,
                &selected,
                Utc::now(),
            )
            .unwrap()
            .len(),
        1
    );
}

async fn start_without_provider(runtime: &RuntimeHandle) -> MessageEnvelope {
    for _ in 0..72 {
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap();
        let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            continue;
        };
        if !scheduled.scheduler_decision.model_reentry {
            let terminal = runtime
                .process_message_with_plan_deferred(
                    scheduled.message.clone(),
                    scheduled.dispatch_plan,
                    &scheduled.scheduler_decision,
                )
                .await
                .unwrap();
            runtime
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
                    Some(&terminal),
                )
                .await
                .unwrap();
            continue;
        }
        runtime
            .begin_interactive_turn_with_provenance(
                Some(&scheduled.message),
                None,
                None,
                scheduled.dispatch_plan.execution_admission_provenance,
            )
            .await
            .unwrap();
        runtime
            .record_incoming_transcript_entry(&scheduled.message)
            .unwrap();
        return scheduled.message;
    }
    panic!("shared reply must receive a canonical source execution");
}

#[derive(Debug)]
struct BatchEvidence {
    activation: String,
    identities: BTreeSet<String>,
    ordinary_sources: usize,
    input_ids: Vec<String>,
    rendered_bytes: usize,
}

async fn process_next(runtime: &RuntimeHandle) -> Option<BatchEvidence> {
    let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await
        .unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
        return None;
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
    let message = scheduled.message;
    let terminal = runtime
        .process_message_with_plan_deferred(
            message.clone(),
            scheduled.dispatch_plan,
            &scheduled.scheduler_decision,
        )
        .await
        .unwrap();
    let evidence = activation.map(|activation| {
        let records = runtime
            .runtime_db()
            .task_result_settlements()
            .admitted_for_activation("default", &activation)
            .unwrap();
        assert!(records.len() <= input_consumption::INPUT_BATCH_ITEMS);
        let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
        let ordinary_sources = turn
            .input_message_ids
            .iter()
            .filter(|id| {
                runtime
                    .storage()
                    .read_message_by_id(id)
                    .unwrap()
                    .is_some_and(|message| crate::wake_contract::is_agent_message(&message))
            })
            .count();
        assert!(records.len() + ordinary_sources <= input_consumption::INPUT_BATCH_ITEMS);
        let pending_bytes = crate::context::render_pending_task_result_records(
            runtime.storage(),
            &message,
            &records,
        )
        .unwrap()
        .map_or(0, |text| text.len());
        let input_bytes = turn
            .idle_input_batch
            .as_ref()
            .map_or(0, |batch| batch.rendered_bytes);
        let rendered_bytes = input_bytes + pending_bytes;
        assert!(rendered_bytes <= input_consumption::INPUT_BATCH_BYTES);
        BatchEvidence {
            activation,
            identities: records
                .into_iter()
                .map(|record| record.result_identity)
                .collect(),
            ordinary_sources,
            input_ids: turn.input_message_ids,
            rendered_bytes,
        }
    });
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
    evidence
}

fn all_settled(runtime: &RuntimeHandle, sources: &[String]) -> bool {
    sources.iter().all(|id| {
        runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(id)
            .unwrap()
            .is_some_and(|record| {
                record.state == TaskResultSettlementState::Settled
                    && record.disposition == Some(TaskResultSettlementDisposition::ModelDelivered)
            })
    })
}

#[tokio::test(start_paused = true)]
async fn p1_shared_reply_waiter_matrix_drains_by_identity_with_restart() {
    for count in [1usize, 2, 8, 9, 16, 17] {
        let provider = Arc::new(SharedReplyProvider::default());
        let mut harness = LifecycleHarness::with_provider(provider.clone());
        let work = owner(harness.runtime(), "shared reply owner").await;
        harness
            .runtime()
            .pick_work_item(work.clone())
            .await
            .unwrap();
        let marker = format!("shared-original-body-{count}");
        let original = reply(harness.runtime(), Some(&work), "shared-original", &marker).await;
        let mut sources = Vec::new();
        for index in 0..count {
            let task = waiter(
                harness.runtime(),
                Some(&work),
                &format!("shared-wait-{index:02}"),
            )
            .await;
            sources.push(finish(harness.runtime(), &task, &original).await);
            assert_eq!(
                finish(harness.runtime(), &task, &original).await,
                sources[index],
                "duplicate terminal publication must retain the same source"
            );
        }
        let mut batches = Vec::new();
        let mut unique = BTreeSet::new();
        let mut restarted = false;
        for _ in 0..count * 6 + 12 {
            if all_settled(harness.runtime(), &sources) {
                break;
            }
            if harness.runtime().inner.agent.lock().await.queue.is_empty() {
                harness.advance(std::time::Duration::from_secs(31)).await;
                harness
                    .runtime()
                    .emit_due_task_result_recovery()
                    .await
                    .unwrap();
            }
            if let Some(batch) = process_next(harness.runtime()).await {
                for identity in &batch.identities {
                    assert!(unique.insert(identity.clone()), "{count}: {batches:?}");
                }
                assert!(!batch.activation.is_empty());
                assert!(!batch.input_ids.is_empty());
                assert!(batch.rendered_bytes <= input_consumption::INPUT_BATCH_BYTES);
                batches.push(batch);
                if count > 7 && !restarted {
                    harness.restart();
                    harness
                        .runtime()
                        .recover_scheduler_bootstrap_claims()
                        .await
                        .unwrap();
                    restarted = true;
                }
            }
        }
        assert!(
            all_settled(harness.runtime(), &sources),
            "{count}: {batches:?}"
        );
        assert_eq!(unique.len(), count, "{count}: {batches:?}");
        assert_eq!(
            batches[0].identities.len(),
            count.min(7),
            "{count}: {batches:?}"
        );
        assert_eq!(batches[0].ordinary_sources, 1, "{count}: {batches:?}");
        assert_eq!(
            batches.len(),
            1 + count.saturating_sub(7).div_ceil(8),
            "{count}: {batches:?}"
        );
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), batches.len());
        for request in requests.iter() {
            let context = current_source_context(request);
            assert_eq!(context.matches(&marker).count(), 1, "{count}: {batches:?}");
        }
        eprintln!(
            "shared reply count={count}, activations={}, provider_requests={}, manifest={batches:?}",
            batches.len(),
            requests.len()
        );
    }
}
