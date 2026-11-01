use super::*;
use crate::provider::ProviderTurnResponse;
use crate::runtime::tests::support::{
    persist_completed_task_parent_fixture, seed_live_waiting_work_execution,
    DeferredFallbackProvider, LifecycleHarness,
};
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementRecord, TaskResultSettlementState,
};
use crate::types::{
    AgentMessageCallerContext, AgentMessagePrincipalKind, AgentMessageSendRequest, TaskKind,
    TaskRecord, TaskStatus, TurnOwner, WorkItemPlanStatus,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "live_command_tests.rs"]
mod live_command_tests;

#[path = "terminal_reply_tests.rs"]
mod terminal_reply_tests;

#[tokio::test(start_paused = true)]
async fn exact_recovery_restarts_after_anchor_snapshot_before_first_result_admission() {
    Box::pin(assert_exact_recovery_restart_before_admission(
        true, None, false,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn exact_recovery_restarts_before_any_input_batch_or_result_admission() {
    Box::pin(assert_exact_recovery_restart_before_admission(
        false, None, false,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn batchless_exact_recovery_rejects_missing_original_memory_start() {
    Box::pin(assert_exact_recovery_restart_before_admission(
        false,
        Some("missing_start"),
        false,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn exact_recovery_restart_preserves_promised_result_beyond_owner_candidate_capacity() {
    Box::pin(assert_exact_recovery_restart_before_admission(
        true, None, true,
    ))
    .await;
}

#[tokio::test(start_paused = true)]
async fn interrupted_unadmitted_exact_recovery_respects_replacement_stop_and_owner_closure() {
    for revoke in [
        "replacement",
        "stop",
        "captured_owner_closed",
        "waiter_closed",
    ] {
        Box::pin(assert_exact_recovery_restart_before_admission(
            false,
            Some(revoke),
            false,
        ))
        .await;
    }
}

async fn assert_exact_recovery_restart_before_admission(
    anchor_snapshot: bool,
    revoke: Option<&str>,
    fill_owner_capacity: bool,
) {
    let provider = Arc::new(FailureProvider::default());
    provider.failures.store(1, Ordering::SeqCst);
    let mut harness = LifecycleHarness::with_provider(provider.clone());
    let (owner, waiter, task, original_wait) =
        repaired_command(harness.runtime(), TaskKind::CommandTask).await;
    let result_id = task.parent_message_id.as_deref().unwrap();
    let first = Box::pin(claim_model(harness.runtime())).await;
    process(harness.runtime(), first).await;
    discard_provider_handoffs(harness.runtime()).await;
    let admitted = latest(harness.runtime(), result_id);
    assert_eq!(admitted.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(admitted.recovery_attempts, 1);
    harness.advance(std::time::Duration::from_secs(180)).await;
    assert!(harness
        .runtime()
        .emit_due_task_result_recovery()
        .await
        .unwrap());
    let successor = Box::pin(claim_model(harness.runtime())).await;
    let source = successor.message.clone();
    assert_eq!(source.work_item_id.as_deref(), Some(waiter.id.as_str()));
    assert_ne!(source.source_refs["wait_id"], original_wait);
    assert_eq!(
        source.source_refs["task_result_identity"],
        admitted.result_identity
    );
    let interrupted_activation = match successor
        .dispatch_plan
        .execution_admission_provenance
        .as_ref()
    {
        Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) => {
            activation_id.clone()
        }
        _ => panic!("successor requires canonical execution admission"),
    };
    let before_admission = latest(harness.runtime(), result_id);
    Box::pin(
        harness.runtime().begin_dispatch_turn(
            &source,
            None,
            None,
            successor
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &successor.dispatch_plan.prior_closure,
        ),
    )
    .await
    .unwrap();
    if anchor_snapshot {
        harness
            .runtime()
            .runtime_db()
            .transaction(|tx| {
                tx.execute_batch(
                    "CREATE TRIGGER reject_first_recovery_admission
                     BEFORE UPDATE ON task_result_settlements
                     WHEN NEW.state = 'caller_admitted'
                     BEGIN SELECT RAISE(ABORT, 'first recovery admission interrupted'); END;",
                )?;
                Ok(())
            })
            .unwrap();
        // Crash before the ordinary error path can terminalize the turn.
        let result = Box::pin(
            harness
                .runtime()
                .admit_idle_task_results(&source, usize::MAX),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("first recovery admission must fail before provider delivery"),
        };
        assert!(
            format!("{error:#}").contains("first recovery admission interrupted"),
            "{error:#}"
        );
        harness
            .runtime()
            .runtime_db()
            .transaction(|tx| {
                tx.execute_batch("DROP TRIGGER reject_first_recovery_admission")?;
                Ok(())
            })
            .unwrap();
    }
    let interrupted_turn = harness
        .runtime()
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .remove(0);
    assert!(interrupted_turn.terminal.is_none());
    if anchor_snapshot {
        let batch = interrupted_turn.idle_input_batch.as_ref().unwrap();
        assert_eq!(batch.anchor_message_id, source.id);
        assert_eq!(
            batch.activation_id.as_deref(),
            Some(interrupted_activation.as_str())
        );
        assert_eq!(batch.items.len(), 1);
        assert_eq!(batch.items[0].message_id, source.id);
    } else {
        assert!(interrupted_turn.idle_input_batch.is_none());
    }
    assert_eq!(latest(harness.runtime(), result_id), before_admission);
    assert_eq!(provider.requests.lock().await.len(), 1);

    // Bootstrap must not timestamp an interruption before its actual turn.
    let advance = (interrupted_turn.created_at - harness.now())
        .to_std()
        .unwrap_or_default()
        + std::time::Duration::from_secs(1);
    harness.advance(advance).await;
    if revoke == Some("missing_start") {
        harness
            .runtime()
            .runtime_db()
            .transaction(|tx| {
                assert_eq!(
                    tx.execute(
                        "DELETE FROM dispatch_memory_checkpoints WHERE agent_id='default' AND turn_id=?1",
                        [&interrupted_turn.turn_id],
                    )?,
                    1,
                );
                Ok(())
            })
            .unwrap();
    }
    harness.restart();
    if revoke == Some("missing_start") {
        let before = harness.snapshot();
        let error = Box::pin(harness.runtime().recover_scheduler_bootstrap_claims())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("original memory start"),
            "{error:#}"
        );
        harness.assert_unchanged(&before);
        assert_eq!(latest(harness.runtime(), result_id), before_admission);
        assert_eq!(provider.requests.lock().await.len(), 1);
        return;
    }
    assert_eq!(
        Box::pin(harness.runtime().recover_scheduler_bootstrap_claims())
            .await
            .unwrap(),
        1
    );
    let recovered_turn = harness
        .runtime()
        .storage()
        .read_turn_by_id(&interrupted_turn.turn_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered_turn
            .terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(crate::types::TurnTerminalKind::Interrupted),
        "{recovered_turn:#?}"
    );
    assert_eq!(
        recovered_turn.idle_input_batch,
        interrupted_turn.idle_input_batch
    );
    assert_eq!(latest(harness.runtime(), result_id), before_admission);
    assert_eq!(provider.requests.lock().await.len(), 1);
    if let Some(revoke) = revoke {
        let runtime = harness.runtime();
        match revoke {
            "replacement" => {
                runtime
                    .register_wait_for_outcome(
                        "default",
                        Some(waiter.id.clone()),
                        crate::runtime::waiting::WaitForWakeKind::OperatorInput,
                        None,
                        "replace the interrupted exact promise".into(),
                        None,
                    )
                    .await
                    .unwrap();
            }
            "stop" => runtime
                .control(crate::types::ControlAction::Stop)
                .await
                .unwrap(),
            "captured_owner_closed" => {
                runtime
                    .complete_work_item(owner.id.clone(), Vec::new())
                    .await
                    .unwrap();
            }
            "waiter_closed" => {
                runtime
                    .complete_work_item(waiter.id.clone(), Vec::new())
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        for _ in 0..8 {
            let poll = Box::pin(scheduler_executor::SchedulerDecisionExecutor::new(runtime).poll())
                .await
                .unwrap();
            if let scheduler_executor::RunLoopPoll::Message(scheduled) = poll {
                assert!(
                    !scheduled.scheduler_decision.model_reentry,
                    "{revoke}: {:?}; {:?}",
                    scheduled.message, scheduled.scheduler_decision
                );
                process(runtime, scheduled).await;
            }
        }
        assert_eq!(provider.requests.lock().await.len(), 1, "{revoke}");
        assert_eq!(latest(runtime, result_id).recovery_attempts, 1, "{revoke}");
        assert_ne!(
            latest(runtime, result_id).disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered),
            "{revoke}"
        );
        return;
    }
    let replay = Box::pin(claim_model(harness.runtime())).await;
    assert_eq!(replay.message.id, source.id);
    let execution = harness
        .runtime()
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution.attempts[&interrupted_activation].state,
        crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
    );
    let mut unrelated = Vec::new();
    if fill_owner_capacity {
        // Populate genuine durable results, then model their earlier closed
        // admission without granting them this cross-owner recovery's promise.
        for index in 0..64 {
            let mut result = MessageEnvelope::new(
                "default",
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: format!("capacity-owner-result-{index}"),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: format!("unrelated-capacity-evidence-{index}"),
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            );
            result.created_at = admitted.created_at - chrono::Duration::seconds(120);
            result.task_id = Some(format!("capacity-owner-result-{index}"));
            result.work_item_id = Some(waiter.id.clone());
            let mut other_task = task.clone();
            other_task.id = result.task_id.clone().unwrap();
            other_task.work_item_id = Some(waiter.id.clone());
            other_task.parent_message_id = Some(result.id.clone());
            other_task.created_at = result.created_at;
            other_task.updated_at = result.created_at;
            other_task.detail = Some(serde_json::json!({
                "rejoin_obligation_id": other_task.id,
                "rejoin_generation": 1,
                "parent_turn_id": "capacity-owner-parent",
                "terminal_reentry": true,
            }));
            result.metadata = Some(serde_json::json!({
                "task_id": other_task.id, "task_kind": TaskKind::CommandTask.as_str(),
                "task_status": "completed", "task_result_id": result.id,
                "task_detail": other_task.detail, "work_item_id": waiter.id,
            }));
            harness
                .runtime()
                .commit_terminal_task_result(&other_task, "task_completed", &result)
                .await
                .unwrap();
            remove_source(harness.runtime(), &result, QueueEntryStatus::Processed).await;
            let mut record = latest(harness.runtime(), &result.id);
            record.state = TaskResultSettlementState::CallerAdmitted;
            record.activation_id = admitted.activation_id.clone();
            record.created_at = result.created_at;
            record.admitted_at = Some(result.created_at);
            let persisted = record.clone();
            super::budget_tests::owned_task_fixture_write(
                harness.runtime(),
                std::collections::BTreeSet::from([crate::runtime::resident_task::TaskKey::new(
                    crate::runtime::resident_task::TaskFamily::ResultSettlement,
                    &record.result_identity,
                )]),
                move |tx| {
                    assert_eq!(
                        tx.execute(
                            "UPDATE task_result_settlements
                             SET state = 'caller_admitted', activation_id = ?1,
                                 created_at = ?2, admitted_at = ?2, payload_json = ?3
                             WHERE result_identity = ?4",
                            rusqlite::params![
                                persisted.activation_id,
                                persisted.created_at.to_rfc3339(),
                                serde_json::to_string(&persisted)?,
                                persisted.result_identity,
                            ],
                        )?,
                        1
                    );
                    Ok(())
                },
            )
            .await;
            assert_eq!(latest(harness.runtime(), &result.id), record);
            unrelated.push(record);
        }
        let activation_id = match replay.dispatch_plan.execution_admission_provenance.as_ref() {
            Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) => activation_id,
            _ => panic!("replay requires canonical admission"),
        };
        let repository = harness.runtime().runtime_db().task_result_settlements();
        let mut candidates = repository
            .recovery_snapshot_candidates_for_owner(
                "default",
                Some(&waiter.id),
                activation_id,
                harness.now(),
                64,
            )
            .unwrap();
        assert_eq!(candidates.len(), 64);
        for record in &unrelated {
            assert!(candidates.contains(record));
            assert!(record.created_at < admitted.created_at);
            assert_eq!(record.state, TaskResultSettlementState::CallerAdmitted);
            assert_eq!(record.recovery_attempts, 0);
            assert!(record.recovery_paused_at.is_none());
        }
        // This models the old combined ordering, not live-turn eligibility.
        // Ordinary processing below performs the canonical candidate lookup.
        assert_eq!(
            replay.message.source_refs.get("task_result_identity"),
            Some(&admitted.result_identity),
        );
        candidates.push(admitted.clone());
        assert_eq!(candidates.len(), 65);
        assert_eq!(candidates[64].result_identity, admitted.result_identity);
        // The old order (capacity before source-snapshot filtering) loses
        // precisely the promised target, despite its separate exact lookup.
        candidates.sort_by_key(|record| {
            (
                record.state != TaskResultSettlementState::CallerAdmitted,
                record.created_at,
                record.result_identity.clone(),
            )
        });
        candidates.truncate(64);
        assert!(candidates
            .iter()
            .all(|record| record.result_identity != admitted.result_identity));
    }
    process(harness.runtime(), replay).await;
    for record in &unrelated {
        assert_eq!(latest(harness.runtime(), &record.message_id), *record);
        assert_eq!(
            harness
                .runtime()
                .storage()
                .latest_task_record(&record.task_id)
                .unwrap()
                .unwrap()
                .work_item_id,
            Some(waiter.id.clone())
        );
        let charges: i64 = harness
            .runtime()
            .runtime_db()
            .transaction(|tx| {
                Ok(tx.query_row(
                    "SELECT COUNT(*) FROM task_result_recovery_admissions
                     WHERE result_identity = ?1",
                    [&record.result_identity],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(charges, 0, "unrelated owner result must not be charged");
    }
    let settled = latest(harness.runtime(), result_id);
    assert_eq!(settled.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settled.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert_eq!(settled.recovery_attempts, 2);
    assert_eq!(
        settled.last_recovery_root_attempt_id.as_deref(),
        Some(interrupted_activation.as_str())
    );
    assert_ne!(settled.activation_id, admitted.activation_id);
    assert_eq!(settled.work_item_id.as_deref(), Some(owner.id.as_str()));
    assert_eq!(
        harness
            .runtime()
            .storage()
            .latest_task_record(&task.id)
            .unwrap()
            .unwrap()
            .work_item_id,
        Some(owner.id)
    );
    assert_eq!(
        harness
            .runtime()
            .latest_work_item(&waiter.id)
            .await
            .unwrap()
            .unwrap()
            .blocked_by,
        waiter.blocked_by
    );
    let turn = harness
        .runtime()
        .storage()
        .read_recent_turns(1)
        .unwrap()
        .remove(0);
    for record in &unrelated {
        assert!(!turn.input_message_ids.contains(&record.message_id));
    }
    assert_eq!(
        turn.effective_owner(),
        TurnOwner::WorkItem {
            work_item_id: waiter.id
        }
    );
    assert_eq!(
        turn.input_message_ids
            .iter()
            .filter(|id| *id == &source.id)
            .count(),
        1
    );
    let charges: i64 = harness
        .runtime()
        .runtime_db()
        .transaction(|tx| {
            Ok(tx.query_row(
                "SELECT COUNT(*) FROM task_result_recovery_admissions
                 WHERE result_identity = ?1 AND root_attempt_id = ?2",
                rusqlite::params![settled.result_identity, interrupted_activation],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(charges, 1, "replay must charge its recovery root only once");
    let requests = provider.requests.lock().await;
    assert_eq!(
        requests.len(),
        2,
        "failed admission cannot reach the provider"
    );
    assert_eq!(
        requests[1].matches("cross-owner-recovery-evidence").count(),
        1
    );
}

#[derive(Default)]
struct FailureProvider {
    failures: AtomicUsize,
    requests: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AgentProvider for FailureProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
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
        let mut remaining = self.failures.load(Ordering::SeqCst);
        while remaining > 0 {
            match self.failures.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return DeferredFallbackProvider.complete_turn(request).await,
                Err(current) => remaining = current,
            }
        }
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::Text {
                text: "delivered".into(),
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

async fn remove_source(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
    status: QueueEntryStatus,
) {
    use crate::runtime_db::transitions::{
        AgentStateMutation, PreparedQueueTransition, QueueMutation, QueueOperation,
        QueueTransitionCommand,
    };

    assert!(matches!(
        status,
        QueueEntryStatus::Processed | QueueEntryStatus::Dropped
    ));
    let (entry, commit) = {
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let mut entry = runtime
            .runtime_db()
            .queue_entries()
            .latest(&message.id)
            .unwrap()
            .unwrap();
        mutation
            .queue
            .pop_next_matching(|queued| queued.id == message.id);
        mutation.state.pending = mutation.queue.len();
        entry.status = status;
        entry.updated_at = runtime.now().max(entry.updated_at);
        let prepared = PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: message.agent_id.clone(),
            operation: QueueOperation::Settle,
            mutation: QueueMutation::Upsert(entry.clone()),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(mutation.last_persisted_state.clone())),
                record: Box::new(mutation.state.clone()),
            }),
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: Vec::new(),
        });
        let commit = mutation
            .commit_prepared_queue(crate::ids::audit_event_id(), prepared)
            .await
            .unwrap();
        assert!(commit.applied);
        (entry, commit)
    };
    runtime.apply_transition_commit(commit).await;
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&message.id)
            .unwrap(),
        Some(entry)
    );
}

async fn discard_provider_handoffs(runtime: &RuntimeHandle) {
    let messages = runtime.inner.agent.lock().await.queue.input_snapshot(64);
    let mut removed = 0;
    for message in messages {
        if crate::runtime::turn::TurnModelSelection::message_has_provider_recovery_provenance(
            &message,
        ) {
            remove_source(runtime, &message, QueueEntryStatus::Dropped).await;
            removed += 1;
        }
    }
    assert_eq!(
        removed, 1,
        "simulate one genuinely missing provider handoff"
    );
}

fn claim_model(
    runtime: &RuntimeHandle,
) -> std::pin::Pin<Box<impl std::future::Future<Output = scheduler_executor::ScheduledMessage> + '_>>
{
    Box::pin(async move {
        for _ in 0..24 {
            let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                .poll()
                .await
                .unwrap();
            if let scheduler_executor::RunLoopPoll::Message(scheduled) = poll {
                if scheduled.scheduler_decision.model_reentry {
                    return scheduled;
                }
                process(runtime, scheduled).await;
            }
        }
        panic!(
            "recovery did not reach canonical admission: {:?}",
            runtime.storage().read_recent_events(4).unwrap()
        );
    })
}

fn process(
    runtime: &RuntimeHandle,
    scheduled: scheduler_executor::ScheduledMessage,
) -> std::pin::Pin<Box<impl std::future::Future<Output = ()> + '_>> {
    Box::pin(async move {
        let message = scheduled.message;
        let terminal = runtime
            .process_message_with_plan_deferred(
                message.clone(),
                scheduled.dispatch_plan,
                &scheduled.scheduler_decision,
            )
            .await
            .unwrap();
        let mut entry = runtime
            .runtime_db()
            .queue_entries()
            .latest(&message.id)
            .unwrap()
            .unwrap();
        entry.status = QueueEntryStatus::Processed;
        entry.updated_at = runtime.now().max(entry.updated_at);
        runtime
            .commit_queue_terminal_settlement(entry.clone(), Vec::new(), true, Some(&terminal))
            .await
            .unwrap();
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(&message.id)
                .unwrap(),
            Some(entry)
        );
    })
}

fn latest(runtime: &RuntimeHandle, result_id: &str) -> TaskResultSettlementRecord {
    runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(result_id)
        .unwrap()
        .unwrap()
}

fn assert_recovery_snapshot(runtime: &RuntimeHandle, record: &TaskResultSettlementRecord) {
    let activation = record.activation_id.as_deref().unwrap();
    let db = runtime.runtime_db();
    let source_id: String = db
        .transaction(|tx| {
            Ok(tx.query_row(
                "SELECT json_extract(payload_json, '$.source_message_id')
                 FROM execution_protocol_attempts WHERE agent_id = ?1 AND attempt_id = ?2",
                rusqlite::params![record.agent_id, activation],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let batch = turn
        .idle_input_batch
        .as_ref()
        .expect("recovery admission must persist its canonical source snapshot");
    assert_eq!(batch.activation_id.as_deref(), Some(activation));
    assert_eq!(batch.anchor_message_id, source_id);
    assert_eq!(batch.items[0].message_id, source_id);
    assert_eq!(batch.items[0].source_ref, format!("message:{source_id}"));
    assert_eq!(
        turn.trigger.as_ref().unwrap().message_id.as_deref(),
        Some(source_id.as_str())
    );
    assert!(turn.input_message_ids.contains(&source_id));
    assert!(db
        .transaction(|tx| {
            Ok(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_result_recovery_admissions
                 WHERE result_identity = ?1 AND recovery_epoch = ?2
                   AND root_attempt_id = ?3)",
                rusqlite::params![
                    record.result_identity,
                    record.recovery_epoch,
                    record.last_recovery_root_attempt_id,
                ],
                |row| row.get::<_, bool>(0),
            )?)
        })
        .unwrap());
}

async fn reply_result(runtime: &RuntimeHandle, owner: &WorkItemRecord) -> (TaskRecord, String) {
    let mut prepared =
        crate::runtime::agent_message_delivery::AgentMessageDeliveryService::prepare(
            AgentMessageSendRequest {
                target_agent_id: "default".into(),
                content: MessageBody::Text {
                    text: "repeated-reply-evidence".into(),
                },
                client_idempotency_key: "repeated-recovery-reply".into(),
                correlation_id: None,
                causation_id: None,
                requested_priority: Some(Priority::Normal),
                forward: false,
                reply_context_id: None,
            },
            AgentMessageCallerContext {
                caller_principal: "agent:repeated-recovery-peer".into(),
                caller_agent_id: Some("repeated-recovery-peer".into()),
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
    let running = TaskRecord {
        id: "repeated-reply-task".into(),
        agent_id: "default".into(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Running,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: None,
        work_item_id: Some(owner.id.clone()),
        summary: None,
        recovery: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": "repeated-reply-task",
            "rejoin_generation": 1, "parent_turn_id": "repeated-reply-parent",
        })),
    };
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
    let result = runtime
        .storage()
        .read_message_by_id(terminal.parent_message_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    remove_source(runtime, &result, QueueEntryStatus::Processed).await;
    (terminal, reply_id)
}

fn assert_recovery_witness_fails_closed(
    runtime: &RuntimeHandle,
    record: &TaskResultSettlementRecord,
) {
    let db = runtime.runtime_db();
    let activation = record.activation_id.as_deref().unwrap();
    let source_id: String = db
        .transaction(|tx| {
            Ok(tx.query_row(
                "SELECT json_extract(payload_json, '$.source_message_id')
                 FROM execution_protocol_attempts WHERE agent_id = ?1 AND attempt_id = ?2",
                rusqlite::params![record.agent_id, activation],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let source = runtime
        .storage()
        .read_message_by_id(&source_id)
        .unwrap()
        .unwrap();
    for field in [
        "task_result_identity",
        "task_result_parent_turn_id",
        "task_result_rejoin_generation",
    ] {
        let mut forged = source.clone();
        forged.source_refs.insert(field.into(), "stale".into());
        db.messages().upsert(&forged).unwrap();
        assert!(
            !db.task_result_settlements()
                .reply_source_is_recoverable(record)
                .unwrap(),
            "{field}"
        );
        db.messages().upsert(&source).unwrap();
    }
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let mut without_snapshot = turn.clone();
    without_snapshot.idle_input_batch = None;
    without_snapshot.live_input_batches.clear();
    db.turn_records().upsert(&without_snapshot).unwrap();
    assert!(!db
        .task_result_settlements()
        .reply_source_is_recoverable(record)
        .unwrap());
    db.turn_records().upsert(&turn).unwrap();
    let admission: (u64, String, String) = db
        .transaction(|tx| {
            Ok(tx.query_row(
                "SELECT recovery_epoch, root_attempt_id, admitted_at
             FROM task_result_recovery_admissions WHERE result_identity = ?1",
                [&record.result_identity],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM task_result_recovery_admissions WHERE result_identity = ?1",
            [&record.result_identity],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        !db.task_result_settlements()
            .reply_source_is_recoverable(record)
            .unwrap(),
        "processed queue state without the selected-identity witness is not a grant"
    );
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO task_result_recovery_admissions
             (result_identity, recovery_epoch, root_attempt_id, admitted_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![record.result_identity, admission.0, admission.1, admission.2],
        )?;
        Ok(())
    }).unwrap();
    assert!(db
        .task_result_settlements()
        .reply_source_is_recoverable(record)
        .unwrap());
}

#[tokio::test(start_paused = true)]
async fn reply_failure_then_recovery_failure_preserves_the_second_root_grant() {
    let provider = Arc::new(FailureProvider::default());
    provider.failures.store(2, Ordering::SeqCst);
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "repeated reply recovery".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let (task, reply_id) = reply_result(runtime, &owner).await;
    let result_id = task.parent_message_id.as_deref().unwrap();
    let original = Box::pin(claim_model(runtime)).await;
    assert_eq!(original.message.id, reply_id);
    process(runtime, original).await;
    discard_provider_handoffs(runtime).await;
    assert_eq!(latest(runtime, result_id).recovery_attempts, 0);

    for root in 1..=2 {
        harness.advance(std::time::Duration::from_secs(180)).await;
        assert!(
            runtime.emit_due_task_result_recovery().await.unwrap(),
            "root {root}: {:?}",
            latest(runtime, result_id)
        );
        let scheduled = Box::pin(claim_model(runtime)).await;
        assert_eq!(
            scheduled.message.source_refs["task_result_identity"],
            latest(runtime, result_id).result_identity
        );
        process(runtime, scheduled).await;
        assert_recovery_snapshot(runtime, &latest(runtime, result_id));
        let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
        assert!(
            !turn.input_message_ids.contains(&reply_id),
            "reply is rendered by the result settlement renderer"
        );
        assert_eq!(latest(runtime, result_id).recovery_attempts, root);
        if root == 1 {
            discard_provider_handoffs(runtime).await;
            let record = latest(runtime, result_id);
            assert_eq!(record.state, TaskResultSettlementState::CallerAdmitted);
            assert!(runtime
                .runtime_db()
                .task_result_settlements()
                .reply_source_is_recoverable(&record)
                .unwrap());
            assert_recovery_witness_fails_closed(runtime, &record);
        }
    }
    let record = latest(runtime, result_id);
    assert_eq!(record.state, TaskResultSettlementState::Settled);
    assert_eq!(
        record.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|request| request.contains("repeated-reply-evidence")));
}

fn repaired_command(
    runtime: &RuntimeHandle,
    kind: TaskKind,
) -> std::pin::Pin<
    Box<
        impl std::future::Future<Output = (WorkItemRecord, WorkItemRecord, TaskRecord, String)> + '_,
    >,
> {
    Box::pin(async move {
        let owner = runtime
            .create_work_item(
                "captured owner A".into(),
                Some(WorkItemPlanStatus::NeedsInput),
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        let waiter = runtime
            .create_work_item(
                "consuming waiter B".into(),
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
        let mut result = MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "repeated-cross-owner-task".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "cross-owner-recovery-evidence".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        persist_completed_task_parent_fixture(
            runtime,
            "repeated-cross-owner-parent",
            Some(&owner.id),
        )
        .await
        .unwrap();
        result.created_at = runtime.now();
        result.task_id = Some("repeated-cross-owner-task".into());
        result.work_item_id = Some(owner.id.clone());
        let task = TaskRecord {
            id: result.task_id.clone().unwrap(),
            agent_id: "default".into(),
            kind,
            status: TaskStatus::Completed,
            created_at: runtime.now(),
            updated_at: runtime.now(),
            parent_message_id: Some(result.id.clone()),
            work_item_id: Some(owner.id.clone()),
            summary: None,
            recovery: None,
            detail: Some(serde_json::json!({
                "rejoin_obligation_id": "repeated-cross-owner-task",
                "rejoin_generation": 1, "parent_turn_id": "repeated-cross-owner-parent",
                "terminal_reentry": true,
            })),
        };
        result.metadata = Some(serde_json::json!({
            "task_id": task.id, "task_kind": kind.as_str(), "task_status": "completed",
            "task_result_id": result.id, "task_detail": task.detail, "work_item_id": owner.id,
        }));
        if kind == TaskKind::CommandTask {
            runtime
                .commit_terminal_task_result(&task, "task_completed", &result)
                .await
                .unwrap();
        } else {
            runtime
                .commit_terminal_child_task_result(&task, "task_completed", &result, None)
                .await
                .unwrap();
        }
        remove_source(runtime, &result, QueueEntryStatus::Processed).await;
        let outcome = runtime
            .register_wait_for_outcome(
                "default",
                Some(waiter.id.clone()),
                crate::runtime::waiting::WaitForWakeKind::TaskResult,
                Some(task.id.clone()),
                "exact B continuation".into(),
                None,
            )
            .await
            .unwrap();
        let crate::runtime::waiting::WaitForRegistrationOutcome::TaskResultRecoveryQueued {
            wait_condition_id,
            ..
        } = outcome
        else {
            panic!("expected exact repair: {outcome:?}");
        };
        let updated_waiter = runtime.latest_work_item(&waiter.id).await.unwrap().unwrap();
        seed_live_waiting_work_execution(runtime, &updated_waiter, &wait_condition_id).await;
        (owner, waiter, task, wait_condition_id)
    })
}

#[tokio::test(start_paused = true)]
async fn admitted_exact_repair_recovers_its_consuming_owner_without_migrating_the_task() {
    for kind in [TaskKind::CommandTask, TaskKind::ChildAgentTask] {
        let provider = Arc::new(FailureProvider::default());
        provider.failures.store(1, Ordering::SeqCst);
        let harness = LifecycleHarness::with_provider(provider.clone());
        let runtime = harness.runtime();
        let (owner, waiter, task, original_wait) = repaired_command(runtime, kind).await;
        let result_id = task.parent_message_id.as_deref().unwrap();
        let first = Box::pin(claim_model(runtime)).await;
        process(runtime, first).await;
        discard_provider_handoffs(runtime).await;
        let admitted = latest(runtime, result_id);
        assert_eq!(admitted.state, TaskResultSettlementState::CallerAdmitted);
        assert_eq!(admitted.recovery_attempts, 1);
        assert_recovery_snapshot(runtime, &admitted);
        let wait = runtime
            .storage()
            .latest_wait_conditions_for_agent("default")
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == original_wait)
            .unwrap();
        assert_eq!(wait.status, crate::types::WaitConditionStatus::Resolved);
        assert!(
            !runtime.emit_due_task_result_recovery().await.unwrap(),
            "respect root backoff"
        );
        harness.advance(std::time::Duration::from_secs(180)).await;
        let source = runtime
            .storage()
            .read_message_by_id(wait.trigger_message_id().unwrap())
            .unwrap()
            .unwrap();
        let mut forged = source.clone();
        forged
            .source_refs
            .insert("task_result_identity".into(), "forged".into());
        runtime.runtime_db().messages().upsert(&forged).unwrap();
        assert!(runtime
            .runtime_db()
            .task_result_settlements()
            .exact_waiter_recovery_continuation(&admitted, runtime.now())
            .unwrap()
            .is_none());
        runtime.runtime_db().messages().upsert(&source).unwrap();
        for fault in crate::runtime::tests::support::PRE_COMMIT_FAULTS {
            let before = harness.snapshot();
            let before_result = latest(runtime, result_id);
            let mut successor = source.clone();
            successor.id = recovery_message_id();
            successor.created_at = runtime.now();
            harness.arm_fault(fault);
            let error = runtime
                .enqueue_exact_recovery_continuation(
                    before_result.clone(),
                    &wait,
                    successor,
                    runtime.now(),
                )
                .await
                .unwrap_err();
            crate::runtime::tests::support::assert_injected_transition_fault(&error);
            harness.assert_unchanged(&before);
            assert_eq!(latest(runtime, result_id), before_result);
            let agent = runtime.inner.agent.lock().await;
            assert_eq!(
                agent.state.pending,
                before.agent_state.as_ref().unwrap().pending
            );
            assert_eq!(agent.state.pending, agent.queue.len());
        }
        let pending_before = runtime.agent_state().await.unwrap().pending;
        assert!(runtime.emit_due_task_result_recovery().await.unwrap());
        let snapshot = harness.snapshot();
        let queued = snapshot
            .queue_entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                )
            })
            .count();
        assert_eq!(queued, pending_before + 1);
        assert_eq!(snapshot.agent_state.as_ref().unwrap().pending, queued);
        {
            let agent = runtime.inner.agent.lock().await;
            assert_eq!(agent.state.pending, queued);
            assert_eq!(agent.queue.len(), queued);
        }
        let next = Box::pin(claim_model(runtime)).await;
        assert_eq!(next.message.work_item_id, Some(waiter.id.clone()));
        assert_ne!(next.message.source_refs["wait_id"], original_wait);
        assert_eq!(
            latest(runtime, result_id).recovery_attempts,
            1,
            "scanner cannot charge admission"
        );
        process(runtime, next).await;
        let record = latest(runtime, result_id);
        assert_recovery_snapshot(runtime, &record);
        assert_eq!(record.state, TaskResultSettlementState::Settled);
        assert_eq!(
            record.disposition,
            Some(TaskResultSettlementDisposition::ModelDelivered)
        );
        assert_eq!(record.recovery_attempts, 2);
        assert_ne!(
            record.last_recovery_root_attempt_id,
            admitted.last_recovery_root_attempt_id
        );
        assert_eq!(record.work_item_id, Some(owner.id.clone()));
        assert_eq!(
            runtime
                .storage()
                .latest_task_record(&task.id)
                .unwrap()
                .unwrap()
                .work_item_id,
            Some(owner.id)
        );
        assert_eq!(
            runtime
                .latest_work_item(&waiter.id)
                .await
                .unwrap()
                .unwrap()
                .blocked_by,
            waiter.blocked_by
        );
        let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(
            turn.effective_owner(),
            TurnOwner::WorkItem {
                work_item_id: waiter.id
            }
        );
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.contains("cross-owner-recovery-evidence")));
    }
}

#[tokio::test(start_paused = true)]
async fn prepared_exact_successor_rejects_a_detached_captured_owner_close_atomically() {
    use crate::domain::execution_protocol::{
        ExecutionProtocolCommand, SetWorkItemWaiting, WaitReference, WorkItemExecutionRecord,
    };
    use crate::runtime_db::transitions::{
        AgentStateMutation, ExecutionProtocolTransition, TransitionFaultPoint,
        WaitTaskResultAdmission, WaitTaskResultObligation, WaitTransitionCommand, WorkItemMutation,
    };

    let provider = Arc::new(FailureProvider::default());
    provider.failures.store(1, Ordering::SeqCst);
    let harness = LifecycleHarness::with_provider(provider);
    let runtime = harness.runtime();
    let (owner, waiter, task, _) = repaired_command(runtime, TaskKind::CommandTask).await;
    let result_id = task.parent_message_id.as_deref().unwrap();
    let scheduled = claim_model(runtime).await;
    process(runtime, scheduled).await;
    discard_provider_handoffs(runtime).await;
    harness.advance(std::time::Duration::from_secs(180)).await;
    let repository = runtime.runtime_db().task_result_settlements();
    let record = latest(runtime, result_id);
    let predecessor = repository
        .exact_waiter_recovery_continuation(&record, runtime.now())
        .unwrap()
        .expect("the closed continuation is eligible before preparation");
    let source_id = predecessor.trigger_message_id().unwrap();
    let mut source = runtime
        .storage()
        .read_message_by_id(source_id)
        .unwrap()
        .unwrap();
    source.id = recovery_message_id();
    source.created_at = runtime.now();
    let mut successor = predecessor.clone();
    successor.id = crate::ids::wait_condition_id();
    successor.status = crate::types::WaitConditionStatus::Triggered;
    successor.created_at = runtime.now();
    successor.updated_at = runtime.now();
    successor.resolved_at = None;
    successor.cancelled_at = None;
    successor.turn_id = None;
    successor.trigger_message_id = Some(source.id.clone());
    successor.triggered_at = Some(runtime.now());
    source
        .source_refs
        .insert("wait_id".into(), successor.id.clone());
    let state = runtime.agent_state().await.unwrap();
    let execution = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert!(execution.open_attempt().is_none());
    let expected_waiter = runtime.latest_work_item(&waiter.id).await.unwrap().unwrap();
    let updated_waiter = WorkItemRecord {
        revision: expected_waiter.revision + 1,
        updated_at: runtime.now(),
        ..expected_waiter.clone()
    };
    let expected_execution = execution.work_items[&waiter.id].clone();
    let protocol = ExecutionProtocolTransition {
        commands: vec![ExecutionProtocolCommand::SetWorkItemWaiting(Box::new(
            SetWorkItemWaiting {
                command_id: format!("task-result-recovery-wait:{}", successor.id),
                work_item_id: waiter.id.clone(),
                record: WorkItemExecutionRecord {
                    source_revision: updated_waiter.revision,
                    state: WorkItemExecutionState::Waiting {
                        generation: expected_execution.generation() + 1,
                        wait: WaitReference {
                            wait_id: successor.id.clone(),
                        },
                    },
                },
                expected: Some(expected_execution.clone()),
            },
        ))],
        ..ExecutionProtocolTransition::default()
    };
    let capture_wait_witness = || async {
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let mut work_request = crate::runtime::work_reads::live_work_request();
        work_request
            .required_trigger_message_ids
            .insert(source_id.into());
        work_request
            .mandatory_roots
            .insert(crate::runtime::resident_work::WorkKey::new(
                crate::runtime::resident_work::WorkFamily::Wait,
                &predecessor.id,
            ));
        mutation
            .prepare_owner_work_request(&work_request)
            .await
            .unwrap();
        let work = mutation.capture_work(&work_request).unwrap();
        let witness = runtime
            .capture_task_result_wait_witness(&mutation, &work, Some(&predecessor.id), &[source_id])
            .unwrap();
        drop(mutation);
        witness
    };
    let (wait_witness, _wait_material) = capture_wait_witness().await;
    let mut command = WaitTransitionCommand {
        agent_id: "default".into(),
        work_items: vec![WorkItemMutation::Update {
            expected_revision: expected_waiter.revision,
            record: updated_waiter.clone(),
        }],
        expected_wait_conditions: Vec::new(),
        wait_conditions: vec![successor.clone()],
        timer_wake: None,
        task_result_admission: Some(WaitTaskResultAdmission {
            expected: None,
            record: QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: source.agent_id.clone(),
                priority: source.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: source.created_at,
                updated_at: runtime.now(),
            },
            message: source.clone(),
            obligation: WaitTaskResultObligation {
                expected: record.clone(),
                current_activation_id: None,
                waiter_work_item_id: Some(waiter.id.clone()),
                canonical_recovery: true,
                source_message_id: source_id.into(),
                source_queue: runtime
                    .runtime_db()
                    .queue_entries()
                    .latest(source_id)
                    .unwrap(),
                dropped_reply_source: None,
                wait_witness,
            },
        }),
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(state.clone())),
            record: Box::new(state),
        }),
        audit_events: vec![AuditEvent::legacy(
            "task_result_exact_continuation_recovery_queued",
            serde_json::json!({ "wait_condition_id": successor.id }),
        )],
        index_changes: runtime
            .storage()
            .index_changes_for_work_item(&updated_waiter)
            .unwrap(),
        notify_scheduler: true,
        fault: Some(TransitionFaultPoint::AfterCanonicalWrites),
    };
    let prepared = harness.snapshot();
    let error = runtime
        .runtime_db()
        .transitions()
        .commit_wait_with_execution_protocol(&command, &protocol)
        .unwrap_err();
    crate::runtime::tests::support::assert_injected_transition_fault(&error);
    harness.assert_unchanged(&prepared);
    command.fault = None;

    runtime
        .complete_work_item(owner.id.clone(), Vec::new())
        .await
        .unwrap();
    let closed = harness.snapshot();
    assert_eq!(closed.agent_state, prepared.agent_state);
    assert_eq!(closed.wait_conditions, prepared.wait_conditions);
    assert_eq!(closed.queue_entries, prepared.queue_entries);
    assert_eq!(closed.messages, prepared.messages);
    assert_eq!(closed.tasks, prepared.tasks);
    assert_eq!(latest(runtime, result_id), record);
    assert_eq!(
        runtime.latest_work_item(&waiter.id).await.unwrap().unwrap(),
        expected_waiter,
    );
    assert_eq!(
        closed.execution_protocol.as_ref().unwrap().work_items[&waiter.id],
        expected_execution,
    );
    let error = runtime
        .runtime_db()
        .transitions()
        .commit_wait_with_execution_protocol(&command, &protocol)
        .unwrap_err();
    let conflict = error
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .expect("owner closure must invalidate the captured Work witness");
    assert_eq!(conflict.domain(), "task_result_wait_witness");
    harness.assert_unchanged(&closed);
    let (wait_witness, _fresh_wait_material) = capture_wait_witness().await;
    command
        .task_result_admission
        .as_mut()
        .unwrap()
        .obligation
        .wait_witness = wait_witness;
    let error = runtime
        .runtime_db()
        .transitions()
        .commit_wait_with_execution_protocol(&command, &protocol)
        .unwrap_err();
    let conflict = error
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .expect("owner closure must invalidate the prepared obligation");
    assert_eq!(conflict.domain(), "task_result_obligation");
    harness.assert_unchanged(&closed);
    assert_eq!(latest(runtime, result_id), record);
}

#[tokio::test(start_paused = true)]
async fn repeated_exact_recovery_preserves_the_five_root_pause_and_rearm_budget() {
    let provider = Arc::new(FailureProvider::default());
    provider.failures.store(5, Ordering::SeqCst);
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let (owner, waiter, task, _) = repaired_command(runtime, TaskKind::CommandTask).await;
    let result_id = task.parent_message_id.as_deref().unwrap();
    for root in 1..=5 {
        if root > 1 {
            harness.advance(std::time::Duration::from_secs(600)).await;
            assert!(runtime.emit_due_task_result_recovery().await.unwrap());
            assert_eq!(latest(runtime, result_id).recovery_attempts, root - 1);
        }
        let scheduled = Box::pin(claim_model(runtime)).await;
        process(runtime, scheduled).await;
        assert_recovery_snapshot(runtime, &latest(runtime, result_id));
        discard_provider_handoffs(runtime).await;
        assert_eq!(latest(runtime, result_id).recovery_attempts, root);
    }
    let paused = latest(runtime, result_id);
    assert_eq!(paused.state, TaskResultSettlementState::CallerAdmitted);
    assert!(paused.recovery_paused_at.is_some());
    harness.advance(std::time::Duration::from_secs(600)).await;
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    assert_eq!(provider.requests.lock().await.len(), 5);
    assert_eq!(latest(runtime, result_id).recovery_attempts, 5);
    let result_identity = paused.result_identity.clone();
    let now = runtime.now();
    super::budget_tests::owned_task_fixture_write(
        runtime,
        std::collections::BTreeSet::from([crate::runtime::resident_task::TaskKey::new(
            crate::runtime::resident_task::TaskFamily::ResultSettlement,
            &result_identity,
        )]),
        move |tx| {
            assert!(
                crate::runtime_db::task_result_settlement::recovery_budget::rearm_recovery_tx(
                    tx,
                    "default",
                    &result_identity,
                    now,
                )?
            );
            Ok(())
        },
    )
    .await;
    assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
    harness.advance(std::time::Duration::from_secs(30)).await;
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let scheduled = Box::pin(claim_model(runtime)).await;
    process(runtime, scheduled).await;
    let delivered = latest(runtime, result_id);
    assert_recovery_snapshot(runtime, &delivered);
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(delivered.recovery_epoch, 1);
    assert_eq!(delivered.recovery_attempts, 1);
    assert_eq!(delivered.work_item_id, Some(owner.id));
    assert_eq!(
        runtime
            .latest_work_item(&waiter.id)
            .await
            .unwrap()
            .unwrap()
            .blocked_by,
        waiter.blocked_by
    );
}

#[tokio::test(start_paused = true)]
async fn closed_exact_continuation_fails_closed_after_replacement_stop_owner_close_or_stale_task() {
    for revoke in [
        "replacement",
        "stop",
        "waiter_closed",
        "captured_owner_closed",
        "stale_task",
    ] {
        let provider = Arc::new(FailureProvider::default());
        provider.failures.store(1, Ordering::SeqCst);
        let harness = LifecycleHarness::with_provider(provider);
        let runtime = harness.runtime();
        let (owner, waiter, mut task, _) = repaired_command(runtime, TaskKind::CommandTask).await;
        let result_id = task.parent_message_id.clone().unwrap();
        let scheduled = Box::pin(claim_model(runtime)).await;
        process(runtime, scheduled).await;
        assert_recovery_snapshot(runtime, &latest(runtime, &result_id));
        discard_provider_handoffs(runtime).await;
        harness.advance(std::time::Duration::from_secs(600)).await;
        assert!(runtime
            .runtime_db()
            .task_result_settlements()
            .exact_waiter_recovery_continuation(&latest(runtime, &result_id), runtime.now())
            .unwrap()
            .is_some());
        match revoke {
            "replacement" => {
                runtime
                    .register_wait_for_outcome(
                        "default",
                        Some(waiter.id.clone()),
                        crate::runtime::waiting::WaitForWakeKind::OperatorInput,
                        None,
                        "a new explicit promise replaces the failed continuation".into(),
                        None,
                    )
                    .await
                    .unwrap();
            }
            "stop" => runtime
                .control(crate::types::ControlAction::Stop)
                .await
                .unwrap(),
            "waiter_closed" => {
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
                    .complete_work_item(waiter.id.clone(), Vec::new())
                    .await
                    .unwrap();
            }
            "captured_owner_closed" => {
                runtime
                    .complete_work_item(owner.id.clone(), Vec::new())
                    .await
                    .unwrap();
            }
            "stale_task" => {
                task.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
                super::stale_source_tests::replace_terminal_task_fixture(runtime, &task).await;
                assert_eq!(
                    runtime
                        .storage()
                        .latest_task_record(&task.id)
                        .unwrap()
                        .unwrap()
                        .rejoin_fence()
                        .unwrap()
                        .generation,
                    2
                );
            }
            _ => unreachable!(),
        }
        assert!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .exact_waiter_recovery_continuation(&latest(runtime, &result_id), runtime.now())
                .unwrap()
                .is_none(),
            "{revoke}"
        );
        assert!(
            !runtime.emit_due_task_result_recovery().await.unwrap(),
            "{revoke}"
        );
        assert_eq!(latest(runtime, &result_id).recovery_attempts, 1, "{revoke}");
    }
}

#[tokio::test(start_paused = true)]
async fn cross_owner_reply_repair_survives_a_failed_recovery_snapshot() {
    let provider = Arc::new(FailureProvider::default());
    provider.failures.store(2, Ordering::SeqCst);
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "captured reply owner A".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();
    let (task, reply_id) = reply_result(runtime, &owner).await;
    let result_id = task.parent_message_id.as_deref().unwrap();
    let first = Box::pin(claim_model(runtime)).await;
    assert_eq!(first.message.id, reply_id);
    process(runtime, first).await;
    discard_provider_handoffs(runtime).await;
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
    let waiter = runtime
        .create_work_item(
            "consuming reply waiter B".into(),
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
            Some(Some("independent approval".into())),
        )
        .await
        .unwrap();
    let outcome = runtime
        .register_wait_for_outcome(
            "default",
            Some(waiter.id.clone()),
            crate::runtime::waiting::WaitForWakeKind::TaskResult,
            Some(task.id.clone()),
            "exact cross-owner reply".into(),
            None,
        )
        .await
        .unwrap();
    let crate::runtime::waiting::WaitForRegistrationOutcome::TaskResultRecoveryQueued {
        wait_condition_id,
        ..
    } = outcome
    else {
        panic!("expected exact reply repair: {outcome:?}");
    };
    let updated_waiter = runtime.latest_work_item(&waiter.id).await.unwrap().unwrap();
    seed_live_waiting_work_execution(runtime, &updated_waiter, &wait_condition_id).await;
    let repair = Box::pin(claim_model(runtime)).await;
    process(runtime, repair).await;
    discard_provider_handoffs(runtime).await;
    let admitted = latest(runtime, result_id);
    assert_eq!(admitted.recovery_attempts, 1);
    assert_recovery_snapshot(runtime, &admitted);
    assert!(runtime
        .runtime_db()
        .task_result_settlements()
        .reply_source_is_recoverable(&admitted)
        .unwrap());
    harness.advance(std::time::Duration::from_secs(180)).await;
    assert!(runtime.emit_due_task_result_recovery().await.unwrap());
    let recovery = Box::pin(claim_model(runtime)).await;
    assert_eq!(recovery.message.work_item_id, Some(waiter.id.clone()));
    process(runtime, recovery).await;
    let delivered = latest(runtime, result_id);
    assert_recovery_snapshot(runtime, &delivered);
    assert_eq!(delivered.state, TaskResultSettlementState::Settled);
    assert_eq!(delivered.recovery_attempts, 2);
    assert_eq!(delivered.work_item_id, Some(owner.id));
    assert_eq!(
        runtime
            .latest_work_item(&waiter.id)
            .await
            .unwrap()
            .unwrap()
            .blocked_by,
        waiter.blocked_by
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|request| request.contains("repeated-reply-evidence")));
}
