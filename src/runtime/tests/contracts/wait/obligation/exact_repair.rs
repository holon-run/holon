use super::*;
use crate::{
    runtime::{scheduler_executor, RuntimeHandle},
    types::{
        AgentMessageCallerContext, AgentMessagePrincipalKind, AgentMessageSendRequest, TurnOwner,
        WorkItemPlanStatus,
    },
};

#[derive(Default)]
struct RepairProvider {
    requests: Mutex<Vec<String>>,
}

#[async_trait]
impl AgentProvider for RepairProvider {
    async fn complete_turn(
        &self,
        request: ProviderTurnRequest,
    ) -> anyhow::Result<ProviderTurnResponse> {
        self.requests.lock().await.push(
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
        Ok(ProviderTurnResponse {
            material: request.material,
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

fn fixture() -> (TempDir, TempDir, RuntimeHandle, Arc<RepairProvider>) {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(RepairProvider::default());
    let runtime = RuntimeHandle::new(
        "default",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    (home, workspace, runtime, provider)
}

async fn owners(runtime: &RuntimeHandle) -> (WorkItemRecord, WorkItemRecord) {
    let owner = runtime
        .create_work_item(
            "captured task owner".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let waiter = runtime
        .create_work_item(
            "exact repair waiter".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let waiter = runtime
        .update_work_item_fields(
            waiter.id,
            None,
            None,
            None,
            None,
            Some(Some("independent business approval".into())),
        )
        .await
        .unwrap();
    (owner, waiter)
}

async fn mark_notification_processed(runtime: &RuntimeHandle, message_id: &str) {
    runtime
        .inner
        .agent
        .lock()
        .await
        .queue
        .pop_next_matching(|message| message.id == message_id);
    let message = runtime
        .storage()
        .read_message_by_id(message_id)
        .unwrap()
        .unwrap();
    runtime
        .storage()
        .append_queue_entry(&QueueEntryRecord {
            message_id: message.id,
            agent_id: message.agent_id,
            priority: message.priority,
            status: QueueEntryStatus::Processed,
            created_at: message.created_at,
            updated_at: runtime.now(),
        })
        .unwrap();
}

async fn command_result(
    runtime: &RuntimeHandle,
    owner: &WorkItemRecord,
    kind: TaskKind,
) -> (TaskRecord, MessageEnvelope) {
    let mut running = running_task("exact-repair-task", &owner.id, runtime.now());
    running.kind = kind;
    let mut message = late_task_result_message(&running.id, &owner.id);
    message.created_at = runtime.now();
    message.body = MessageBody::Text {
        text: "exact-repair-original-evidence".into(),
    };
    message.metadata = Some(serde_json::json!({
        "task_id": running.id, "task_kind": kind.as_str(), "task_status": "completed",
        "task_result_id": message.id, "work_item_id": owner.id,
    }));
    let terminal = terminal_task_with_result(&running, &message, runtime.now());
    if kind == TaskKind::CommandTask {
        runtime
            .commit_terminal_task_result(&terminal, "task_completed", &message)
            .await
            .unwrap();
    } else {
        runtime
            .commit_terminal_child_task_result(&terminal, "task_completed", &message, None)
            .await
            .unwrap();
    }
    mark_notification_processed(runtime, &message.id).await;
    (terminal, message)
}

async fn register_repair(
    runtime: &RuntimeHandle,
    waiter: &WorkItemRecord,
    task: &TaskRecord,
) -> (String, String) {
    let outcome = runtime
        .register_wait_for_outcome(
            "default",
            Some(waiter.id.clone()),
            WaitForWakeKind::TaskResult,
            Some(task.id.clone()),
            "deliver this exact original identity".into(),
            None,
        )
        .await
        .unwrap();
    let WaitForRegistrationOutcome::TaskResultRecoveryQueued {
        wait_condition_id,
        recovery_message_id,
        ..
    } = outcome
    else {
        panic!("expected a canonical exact repair: {outcome:?}");
    };
    let waiter = runtime.latest_work_item(&waiter.id).await.unwrap().unwrap();
    // Registration outside a live execution has no yielding attempt to settle.
    seed_live_waiting_work_execution(runtime, &waiter, &wait_condition_id).await;
    (wait_condition_id, recovery_message_id)
}

async fn process_scheduled(
    runtime: &RuntimeHandle,
    scheduled: scheduler_executor::ScheduledMessage,
) {
    let message = scheduled.message;
    let terminal = Box::pin(runtime.process_message_with_plan_deferred(
        message.clone(),
        scheduled.dispatch_plan,
        &scheduled.scheduler_decision,
    ))
    .await
    .unwrap();
    runtime
        .commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: message.id,
                agent_id: message.agent_id,
                priority: message.priority,
                status: QueueEntryStatus::Processed,
                created_at: message.created_at,
                updated_at: runtime.now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await
        .unwrap();
}

async fn claim_model(runtime: &RuntimeHandle) -> scheduler_executor::ScheduledMessage {
    for _ in 0..24 {
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
        process_scheduled(runtime, scheduled).await;
    }
    panic!("exact repair never reached canonical model admission");
}

async fn assert_delivered(
    runtime: &RuntimeHandle,
    owner: &WorkItemRecord,
    waiter: &WorkItemRecord,
    task: &TaskRecord,
    result_message_id: &str,
    identity: &str,
    recovery_id: &str,
) {
    let scheduled = Box::pin(claim_model(runtime)).await;
    assert_eq!(scheduled.message.id, recovery_id);
    process_scheduled(runtime, scheduled).await;
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    assert_eq!(
        turn.effective_owner(),
        TurnOwner::WorkItem {
            work_item_id: waiter.id.clone()
        }
    );
    let record = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_message_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.result_identity, identity);
    assert_eq!(record.work_item_id.as_deref(), Some(owner.id.as_str()));
    assert_eq!(record.state, TaskResultSettlementState::Settled);
    assert_eq!(
        record.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_eq!(
        runtime
            .storage()
            .latest_task_record(&task.id)
            .unwrap()
            .unwrap()
            .work_item_id,
        Some(owner.id.clone()),
    );
    assert_eq!(
        runtime
            .latest_work_item(&waiter.id)
            .await
            .unwrap()
            .unwrap()
            .blocked_by,
        waiter.blocked_by,
    );
}

#[tokio::test]
async fn exact_repair_command_and_child_cross_owner_reach_the_canonical_model() {
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        let (_home, _workspace, runtime, provider) = fixture();
        let (owner, waiter) = owners(&runtime).await;
        let (task, message) = command_result(&runtime, &owner, kind).await;
        let identity = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&message.id)
            .unwrap()
            .unwrap()
            .result_identity;
        let (_, recovery) = register_repair(&runtime, &waiter, &task).await;
        let third = runtime
            .create_work_item(
                "cannot steal repaired promise".into(),
                Some(WorkItemPlanStatus::NeedsInput),
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            runtime.register_wait_for_outcome(
                "default", Some(third.id), WaitForWakeKind::TaskResult, Some(task.id.clone()),
                "must not steal".into(), None,
            ).await.unwrap(),
            WaitForRegistrationOutcome::TaskResultClaimedByOtherWaiter {
                wait_condition_id: _, claimed_by_work_item_id: Some(ref id), ..
            } if id == &waiter.id
        ));
        assert_delivered(
            &runtime,
            &owner,
            &waiter,
            &task,
            &message.id,
            &identity,
            &recovery,
        )
        .await;
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("exact-repair-original-evidence"));
    }
}

#[tokio::test]
async fn exact_repair_consumed_reply_cross_owner_reaches_the_canonical_model() {
    let (_home, _workspace, runtime, provider) = fixture();
    let (owner, waiter) = owners(&runtime).await;
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
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let mut prepared =
        crate::runtime::agent_message_delivery::AgentMessageDeliveryService::prepare(
            AgentMessageSendRequest {
                target_agent_id: "default".into(),
                content: MessageBody::Text {
                    text: "exact-repair-original-reply".into(),
                },
                client_idempotency_key: "exact-repair-reply".into(),
                correlation_id: None,
                causation_id: None,
                requested_priority: Some(Priority::Normal),
                forward: false,
                reply_context_id: None,
            },
            AgentMessageCallerContext {
                caller_principal: "agent:repair-peer".into(),
                caller_agent_id: Some("repair-peer".into()),
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
    let scheduled = Box::pin(claim_model(&runtime)).await;
    assert_eq!(scheduled.message.id, reply_id);
    process_scheduled(&runtime, scheduled).await;
    runtime
        .update_work_item_fields(
            owner.id.clone(),
            None,
            Some(WorkItemPlanStatus::NeedsInput),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let mut running = running_task("exact-repair-reply-task", &owner.id, runtime.now());
    running.kind = TaskKind::AgentMessageWait;
    running.detail = Some(serde_json::json!({
        "rejoin_obligation_id": running.id, "rejoin_generation": 1,
        "parent_turn_id": "exact-repair-reply-parent",
    }));
    runtime
        .persist_task_transition(&running, "task_created")
        .await
        .unwrap();
    let mut detail = running.detail.clone().unwrap();
    detail["message_id"] = serde_json::json!(reply_id);
    detail["response_delivery_id"] =
        reply.metadata.as_ref().unwrap()["agent_message_delivery"]["delivery_id"].clone();
    runtime
        .finish_agent_message_wait(&running, TaskStatus::Completed, detail, String::new())
        .await
        .unwrap();
    let terminal = runtime
        .storage()
        .latest_task_record(&running.id)
        .unwrap()
        .unwrap();
    let result_id = terminal.parent_message_id.as_deref().unwrap();
    mark_notification_processed(&runtime, result_id).await;
    let identity = runtime
        .inner
        .runtime_db
        .task_result_settlements()
        .latest_for_message(result_id)
        .unwrap()
        .unwrap()
        .result_identity;
    let (_, recovery) = register_repair(&runtime, &waiter, &terminal).await;
    assert_delivered(
        &runtime, &owner, &waiter, &terminal, result_id, &identity, &recovery,
    )
    .await;
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("exact-repair-original-reply"));
}

#[tokio::test]
async fn exact_repair_source_refs_alone_do_not_grant_another_waiter() {
    let (_home, _workspace, runtime, provider) = fixture();
    let (owner, waiter) = owners(&runtime).await;
    let (task, message) = command_result(&runtime, &owner, TaskKind::CommandTask).await;
    let (_, recovery) = register_repair(&runtime, &waiter, &task).await;
    let source = runtime
        .storage()
        .read_message_by_id(&recovery)
        .unwrap()
        .unwrap();
    let repository = runtime.inner.runtime_db.task_result_settlements();
    assert!(repository
        .exact_waiter_recovery_source(&source)
        .unwrap()
        .is_some());
    for wrong_owner in [false, true] {
        let mut forged = source.clone();
        forged.id = crate::ids::runtime_id("msg_forged_exact_repair");
        forged.message_seq = None;
        forged.turn_id = None;
        if wrong_owner {
            forged.work_item_id = Some(owner.id.clone());
        }
        runtime.enqueue(forged.clone()).await.unwrap();
        assert!(repository
            .exact_waiter_recovery_source(&forged)
            .unwrap()
            .is_none());
    }
    assert!(provider.requests.lock().await.is_empty());
    let record = repository.latest_for_message(&message.id).unwrap().unwrap();
    assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
    assert_eq!(record.recovery_attempts, 0);
    assert_eq!(record.work_item_id, Some(owner.id));
}

#[tokio::test]
async fn exact_repair_rejects_forged_or_stale_grants_at_canonical_admission() {
    for failure in [
        "hint_only",
        "missing_wait",
        "wrong_waiter",
        "wrong_trigger",
        "cancelled_wait",
        "wrong_task",
        "wrong_generation",
        "wrong_identity",
        "wrong_source",
        "stale_task_generation",
        "wrong_attempt",
    ] {
        let (_home, _workspace, runtime, provider) = fixture();
        let (owner, waiter) = owners(&runtime).await;
        let (task, message) = command_result(&runtime, &owner, TaskKind::CommandTask).await;
        let (wait_id, _) = register_repair(&runtime, &waiter, &task).await;
        let scheduled = Box::pin(claim_model(&runtime)).await;
        let anchor = scheduled.message;
        runtime
            .begin_interactive_turn_with_provenance(
                Some(&anchor),
                None,
                None,
                scheduled.dispatch_plan.execution_admission_provenance,
            )
            .await
            .unwrap();
        let binding = runtime
            .agent_state()
            .await
            .unwrap()
            .current_execution_binding
            .unwrap();
        let activation = binding.activation_id.as_deref().unwrap();
        let repository = runtime.inner.runtime_db.task_result_settlements();
        assert!(
            repository
                .exact_waiter_recovery_candidate("default", Some(&waiter.id), activation,)
                .unwrap()
                .is_some(),
            "{failure}: valid control"
        );
        assert!(runtime
            .idle_reply_recovery_source_valid(&anchor, &binding)
            .unwrap());
        let canonical_anchor = runtime
            .storage()
            .read_message_by_id(&anchor.id)
            .unwrap()
            .unwrap();
        let mut invalid = anchor.clone();
        if failure == "hint_only" {
            invalid
                .source_refs
                .insert("wait_id".into(), "forged-hint".into());
        } else if matches!(
            failure,
            "missing_wait" | "wrong_generation" | "wrong_identity" | "wrong_source"
        ) {
            match failure {
                "missing_wait" => {
                    invalid
                        .source_refs
                        .insert("wait_id".into(), "no-durable-wait".into());
                }
                "wrong_generation" => {
                    invalid
                        .source_refs
                        .insert("task_result_rejoin_generation".into(), "99".into());
                }
                "wrong_identity" => {
                    invalid
                        .source_refs
                        .insert("task_result_identity".into(), "forged-identity".into());
                }
                _ => {
                    invalid.origin = MessageOrigin::System {
                        subsystem: "forged-recovery".into(),
                    };
                }
            }
            // First reject only the forged request against untouched evidence.
            assert!(
                !runtime
                    .idle_reply_recovery_source_valid(&invalid, &binding)
                    .unwrap(),
                "{failure}: forged request"
            );
            assert_eq!(
                runtime
                    .storage()
                    .read_message_by_id(&anchor.id)
                    .unwrap()
                    .unwrap(),
                canonical_anchor,
            );
            runtime
                .inner
                .runtime_db
                .connection()
                .unwrap()
                .execute(
                    "UPDATE messages SET payload_json = ?1 WHERE message_id = ?2",
                    rusqlite::params![serde_json::to_string(&invalid).unwrap(), invalid.id],
                )
                .unwrap();
        } else if failure == "stale_task_generation" {
            let mut stale = task.clone();
            stale.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
            // Normal terminal transitions are idempotent; inject stale evidence
            // directly rather than asking the reducer to replace a terminal.
            runtime
                .inner
                .runtime_db
                .connection()
                .unwrap()
                .execute(
                    "UPDATE tasks SET payload_json = ?1 WHERE task_id = ?2",
                    rusqlite::params![serde_json::to_string(&stale).unwrap(), stale.id],
                )
                .unwrap();
        } else if failure == "wrong_attempt" {
            let connection = runtime.inner.runtime_db.connection().unwrap();
            let payload: String = connection.query_row(
                "SELECT payload_json FROM execution_protocol_attempts WHERE attempt_id = ?1 AND agent_id = 'default'",
                [activation], |row| row.get(0),
            ).unwrap();
            let mut attempt: crate::domain::execution_protocol::ExecutionAttempt =
                serde_json::from_str(&payload).unwrap();
            attempt.source.identity =
                crate::domain::execution_protocol::ExecutionSourceIdentity::InternalFollowup {
                    message_id: anchor.id.clone(),
                };
            connection.execute(
                "UPDATE execution_protocol_attempts SET payload_json = ?1 WHERE attempt_id = ?2 AND agent_id = 'default'",
                rusqlite::params![serde_json::to_string(&attempt).unwrap(), activation],
            ).unwrap();
        } else {
            let mut wait = runtime
                .storage()
                .latest_wait_conditions_for_agent("default")
                .unwrap()
                .into_iter()
                .find(|wait| wait.id == wait_id)
                .unwrap();
            match failure {
                "wrong_waiter" => wait.work_item_id = Some(owner.id.clone()),
                "wrong_trigger" => wait.trigger_message_id = Some(message.id.clone()),
                "cancelled_wait" => wait.status = WaitConditionStatus::Cancelled,
                "wrong_task" => {
                    wait.wake_sources = vec![crate::types::WakeSource::TaskResult {
                        task_id: "different-task".into(),
                    }]
                }
                _ => unreachable!(),
            }
            // Inject corrupt durable evidence without bypassing the real
            // transition guard in production.
            runtime
                .inner
                .runtime_db
                .connection()
                .unwrap()
                .execute(
                    "UPDATE wait_conditions SET payload_json = ?1, status = ?2,
                     work_item_id = ?3, trigger_message_id = ?4, wake_sources_json = ?5
                     WHERE wait_condition_id = ?6",
                    rusqlite::params![
                        serde_json::to_string(&wait).unwrap(),
                        serde_json::to_value(&wait.status)
                            .unwrap()
                            .as_str()
                            .unwrap(),
                        wait.work_item_id,
                        wait.trigger_message_id,
                        serde_json::to_string(&wait.wake_sources).unwrap(),
                        wait.id,
                    ],
                )
                .unwrap();
        }
        if matches!(
            failure,
            "missing_wait" | "wrong_generation" | "wrong_identity" | "wrong_source"
        ) {
            // A live raw rewrite is a separate integrity failure, not a
            // semantic false or permission to requalify the forged source.
            let error = runtime
                .idle_reply_recovery_source_valid(&anchor, &binding)
                .unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
                    Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(id))
                        if id == &anchor.id
                ),
                "{failure}: {error:#}"
            );
        } else if failure == "stale_task_generation" {
            let error = runtime
                .idle_reply_recovery_source_valid(&invalid, &binding)
                .unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
                    Some(crate::runtime_db::task_snapshot::TaskSnapshotError::CanonicalChanged(key))
                        if key.family == crate::runtime::resident_task::TaskFamily::Task
                            && key.id == task.id
                ),
                "{failure}: {error:#}"
            );
        } else if failure == "wrong_attempt" {
            // A live raw rewrite cannot replace the qualified Attempt identity.
            // Canonical admission below must still reject the forged grant
            // without rebinding or charging its pending Result.
            let error = runtime
                .idle_reply_recovery_source_valid(&invalid, &binding)
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
                Some(&crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ProjectionChanged(
                    activation.to_string()
                )),
                "{failure}: {error:#}"
            );
        } else {
            assert!(
                !runtime
                    .idle_reply_recovery_source_valid(&invalid, &binding)
                    .unwrap(),
                "{failure}"
            );
        }
        if failure != "hint_only" {
            assert!(
                repository
                    .exact_waiter_recovery_candidate("default", Some(&waiter.id), activation,)
                    .unwrap()
                    .is_none(),
                "{failure}"
            );
            let selected = [repository
                .latest_for_message(&message.id)
                .unwrap()
                .unwrap()
                .result_identity]
            .into_iter()
            .collect();
            assert!(
                repository
                    .admit_unsettled_snapshot(
                        "default",
                        Some(&waiter.id),
                        activation,
                        None,
                        Some(&selected),
                        runtime.now(),
                    )
                    .unwrap()
                    .is_empty(),
                "{failure}"
            );
        }
        assert!(provider.requests.lock().await.is_empty(), "{failure}");
        let record = repository.latest_for_message(&message.id).unwrap().unwrap();
        assert_eq!(
            record.state,
            TaskResultSettlementState::PersistedPending,
            "{failure}"
        );
        assert_eq!(
            record.work_item_id.as_deref(),
            Some(owner.id.as_str()),
            "{failure}"
        );
        assert_eq!(record.recovery_attempts, 0, "{failure}");
    }
}
