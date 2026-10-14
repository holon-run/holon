//! Real scheduler/terminal-settlement measurements, not a batch-size model.
use super::super::*;
use super::support::*;
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementDisposition, TaskResultSettlementState,
};
use crate::types::{AdmissionContext, QueueEntryRecord, WorkItemPlanStatus};
use std::collections::BTreeSet;

#[derive(Default)]
struct BaselineProvider {
    requests: Mutex<Vec<ProviderTurnRequest>>,
}

#[tokio::test]
async fn reply_wait_terminal_result_preaccumulation_baseline() {
    let mut passed = true;
    for count in [1, 2, 8, 9, 16, 17] {
        passed &= terminal_result_baseline(TaskKind::AgentMessageWait, count).await;
    }
    assert!(passed, "reply-wait matrix violated result consumption");
}

#[async_trait]
impl AgentProvider for BaselineProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.lock().await.push(request);
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "baseline fixture response".into(),
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

async fn terminal_result_baseline(kind: TaskKind, count: usize) -> bool {
    let provider = Arc::new(BaselineProvider::default());
    let harness = LifecycleHarness::with_provider(provider.clone());
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            format!("{} preaccumulated results", kind.as_str()),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.pick_work_item(owner.id.clone()).await.unwrap();

    // Business ownership is not an explicit task-result wait. Reply-wait tasks
    // below each reference their own original reply, not one shared reply.
    // Publish all terminal results through the real task transition before the
    // first scheduler claim. No shell, child runtime or production model runs.
    let mut source_ids = Vec::new();
    let mut reply_source_ids = Vec::new();
    let mut result_bodies = Vec::new();
    let mut result_ids = BTreeSet::new();
    for index in 0..count {
        let task_id = format!("baseline-{}-{count}-{index:02}", kind.as_str());
        let mut body_marker = format!("terminal-result-body-{index:02}");
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task_id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: body_marker.clone(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        message.work_item_id = Some(owner.id.clone());
        message.turn_id = Some(format!("baseline-result-turn-{index:02}"));
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
            summary: Some(format!("terminal fixture {index:02}")),
            detail: Some(serde_json::json!({
                "rejoin_obligation_id": task_id,
                "rejoin_generation": 1,
                "parent_turn_id": "baseline-parent-turn",
                "terminal_reentry": true,
            })),
            recovery: None,
        };
        match kind {
            TaskKind::CommandTask => runtime
                .commit_terminal_task_result(&task, "task_completed", &message)
                .await
                .unwrap(),
            TaskKind::ChildAgentTask => {
                assert!(!runtime
                    .commit_terminal_child_task_result(&task, "task_completed", &message, None)
                    .await
                    .unwrap());
            }
            TaskKind::AgentMessageWait => {
                body_marker = format!("original-reply-body-{index:02}");
                let mut reply = MessageEnvelope::new(
                    "default",
                    MessageKind::InternalFollowup,
                    MessageOrigin::System {
                        subsystem: "agent_message".into(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: body_marker.clone(),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::RuntimeSystem,
                    AdmissionContext::RuntimeOwned,
                );
                reply.work_item_id = Some(owner.id.clone());
                reply.turn_id = Some(format!("baseline-reply-turn-{index:02}"));
                reply.routing_context = Some(crate::types::AgentMessageRoutingContext {
                    message_id: reply.id.clone(),
                    sender_agent_id: Some(format!("peer-{index:02}")),
                    recipient_agent_id: "default".into(),
                    reply_to_agent_id: Some(format!("peer-{index:02}")),
                    correlation_id: None,
                    in_reply_to_message_id: None,
                    original_sender_agent_id: None,
                    original_reply_to_agent_id: None,
                });
                let delivery_id = format!("baseline-reply-delivery-{index:02}");
                reply.metadata = Some(serde_json::json!({
                    "agent_message_delivery": {"delivery_id": delivery_id},
                }));
                let reply = runtime.enqueue(reply).await.unwrap();
                let running = TaskRecord {
                    status: TaskStatus::Running,
                    parent_message_id: None,
                    ..task.clone()
                };
                runtime
                    .persist_task_transition(&running, "task_created")
                    .await
                    .unwrap();
                let mut detail = running.detail.clone().unwrap();
                detail["message_id"] = serde_json::json!(reply.id);
                detail["response_delivery_id"] = serde_json::json!(delivery_id);
                runtime
                    .finish_agent_message_wait(
                        &running,
                        TaskStatus::Completed,
                        detail,
                        String::new(),
                    )
                    .await
                    .unwrap();
                let terminal = runtime
                    .storage()
                    .latest_task_record(&task_id)
                    .unwrap()
                    .unwrap();
                message = runtime
                    .storage()
                    .read_message_by_id(terminal.parent_message_id.as_deref().unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    crate::wake_contract::agent_message_reply_reference(&message),
                    Some(reply.id.as_str())
                );
                assert!(!serde_json::to_string(&message.body)
                    .unwrap()
                    .contains(&body_marker));
                reply_source_ids.push(reply.id);
            }
            _ => unreachable!("only command, supervised-child and reply-wait fixtures"),
        }
        let record = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(&message.id)
            .unwrap()
            .expect("terminal transition must create a settlement obligation");
        assert_eq!(record.state, TaskResultSettlementState::PersistedPending);
        assert_eq!(record.work_item_id.as_deref(), Some(owner.id.as_str()));
        assert!(result_ids.insert(record.result_identity));
        result_bodies.push(body_marker);
        source_ids.push(message.id);
    }
    assert!(provider.requests.lock().await.is_empty());

    let mut admitted_ids = BTreeSet::new();
    let mut admissions = Vec::new();
    let mut anchor_admissions = Vec::new();
    let mut backlog_admissions = Vec::new();
    let mut admitted_sources_per_activation = Vec::new();
    let mut admitted_identities_per_activation = Vec::new();
    let mut activation_ids = BTreeSet::new();
    let mut reducer_only = 0;
    let mut processed_notifications = 0;
    let mut dispatched_source_ids = BTreeSet::new();
    let mut scheduler_polls = 0;
    let mut first_response_after_sources = None;
    let mut notifications_before_first_response = None;
    let queued_sources = count + reply_source_ids.len();
    // Followers may become Processed without an independent dispatch. Drain
    // the finite fixture by source terminal state, with room for idle reducers.
    let max_polls = queued_sources * 4 + 1;
    for _ in 0..max_polls {
        if source_ids.iter().chain(&reply_source_ids).all(|id| {
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(id)
                .unwrap()
                .is_some_and(|entry| entry.status == QueueEntryStatus::Processed)
        }) {
            break;
        }
        scheduler_polls += 1;
        let poll = scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap();
        let scheduled = match poll {
            scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
            scheduler_executor::RunLoopPoll::Idle => continue,
            _ => panic!("preaccumulated source drain was stopped or blocked"),
        };
        assert!(
            source_ids.contains(&scheduled.message.id)
                || reply_source_ids.contains(&scheduled.message.id)
        );
        assert!(dispatched_source_ids.insert(scheduled.message.id.clone()));
        if source_ids.contains(&scheduled.message.id) {
            processed_notifications += 1;
            if kind == TaskKind::AgentMessageWait {
                assert!(
                    !scheduled.scheduler_decision.model_reentry,
                    "reply references must not independently deliver the reply body"
                );
            }
        }
        let message = scheduled.message;
        let terminal = runtime
            .process_message_with_plan_deferred(
                message.clone(),
                scheduled.dispatch_plan,
                &scheduled.scheduler_decision,
            )
            .await
            .unwrap();
        // The task-result reducer can admit the triggering result after poll.
        // Sample after dispatch, before committing the deferred settlement.
        if scheduled.scheduler_decision.model_reentry {
            let activation_id = scheduler_executor::canonical_activation_id(&message.id);
            assert!(activation_ids.insert(activation_id.clone()));
            let admitted = runtime
                .inner
                .runtime_db
                .task_result_settlements()
                .admitted_for_activation("default", &activation_id)
                .unwrap();
            admissions.push(admitted.len());
            let anchor_count = admitted
                .iter()
                .filter(|record| record.message_id == message.id)
                .count();
            anchor_admissions.push(anchor_count);
            backlog_admissions.push(admitted.len() - anchor_count);
            admitted_sources_per_activation.push(
                admitted
                    .iter()
                    .map(|record| record.message_id.clone())
                    .collect::<BTreeSet<_>>(),
            );
            admitted_identities_per_activation.push(
                admitted
                    .iter()
                    .map(|record| record.result_identity.clone())
                    .collect::<BTreeSet<_>>(),
            );
            for record in admitted {
                assert!(result_ids.contains(&record.result_identity));
                assert!(admitted_ids.insert(record.result_identity));
            }
        } else {
            reducer_only += 1;
        }
        assert!(runtime
            .commit_queue_terminal_settlement(
                QueueEntryRecord {
                    message_id: message.id,
                    agent_id: message.agent_id,
                    priority: message.priority,
                    status: QueueEntryStatus::Processed,
                    created_at: message.created_at,
                    // Scheduler claims use wall time rather than the harness clock.
                    updated_at: Utc::now(),
                },
                Vec::new(),
                true,
                Some(&terminal),
            )
            .await
            .unwrap());
        if first_response_after_sources.is_none()
            && !runtime.storage().read_recent_briefs(1).unwrap().is_empty()
        {
            first_response_after_sources = Some(dispatched_source_ids.len());
            notifications_before_first_response = Some(processed_notifications);
        }
    }

    let requests = provider.requests.lock().await;
    let execution = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(execution.attempts.len(), activation_ids.len());
    assert_eq!(requests.len(), activation_ids.len());
    assert!(processed_notifications <= count);
    assert!(first_response_after_sources.is_some());
    let mut settled_ids = BTreeSet::new();
    let mut settlement_states = Vec::new();
    for source_id in &source_ids {
        let record = runtime
            .inner
            .runtime_db
            .task_result_settlements()
            .latest_for_message(source_id)
            .unwrap()
            .unwrap();
        settlement_states.push((
            source_id.clone(),
            record.result_identity.clone(),
            record.state,
            record.disposition,
            record.activation_id.clone(),
        ));
        if record.state == TaskResultSettlementState::Settled
            && record.disposition == Some(TaskResultSettlementDisposition::ModelDelivered)
        {
            assert!(settled_ids.insert(record.result_identity.clone()));
        }
    }
    // The triggering result is rendered as current input, not as a pending
    // result identity. Measure both surfaces without assuming all admitted
    // results fit the existing bounded context.
    let request_texts: Vec<_> = requests
        .iter()
        .map(|request| {
            format!(
                "{}\n{:?}",
                serde_json::to_string(&request.prompt_frame).unwrap(),
                request.conversation,
            )
        })
        .collect();
    let visible_bodies_per_request: Vec<_> = request_texts
        .iter()
        .map(|text| {
            result_bodies
                .iter()
                .filter(|body| text.contains(*body))
                .count()
        })
        .collect();
    let visible_result_identities_per_request: Vec<_> = request_texts
        .iter()
        .map(|text| result_ids.iter().filter(|id| text.contains(*id)).count())
        .collect();
    let distinct_visible_bodies = result_bodies
        .iter()
        .filter(|body| request_texts.iter().any(|text| text.contains(*body)))
        .count();
    let distinct_visible_identities = result_ids
        .iter()
        .filter(|id| request_texts.iter().any(|text| text.contains(*id)))
        .count();
    let visible_admitted_bodies_per_request: Vec<_> = request_texts
        .iter()
        .zip(&admitted_sources_per_activation)
        .map(|(text, admitted)| {
            source_ids
                .iter()
                .zip(&result_bodies)
                .filter(|(source_id, body)| admitted.contains(*source_id) && text.contains(*body))
                .count()
        })
        .collect();
    let visible_admitted_identities_per_request: Vec<_> = request_texts
        .iter()
        .zip(&admitted_identities_per_activation)
        .map(|(text, admitted)| admitted.iter().filter(|id| text.contains(*id)).count())
        .collect();
    let queues = runtime.storage().latest_queue_entries().unwrap();
    let queue_states: Vec<_> = source_ids
        .iter()
        .chain(&reply_source_ids)
        .map(|id| {
            (
                id.clone(),
                queues
                    .iter()
                    .find(|entry| entry.message_id == *id)
                    .map(|entry| entry.status.clone()),
                runtime
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(id)
                    .unwrap()
                    .map(|entry| entry.status),
            )
        })
        .collect();
    println!(
        "INPUT_BASELINE {}",
        serde_json::json!({
            "kind": kind.as_str(),
            "terminal_reentry": kind != TaskKind::AgentMessageWait,
            "explicit_task_result_wait": false,
            "independent_reply_per_handle": kind == TaskKind::AgentMessageWait,
            "preaccumulated": count,
            "queued_sources": queued_sources,
            "scheduler_polls": scheduler_polls,
            "max_scheduler_polls": max_polls,
            "dispatched_sources": dispatched_source_ids.len(),
            "dispatched_result_notifications": processed_notifications,
            "source_message_ids": source_ids,
            "original_reply_message_ids": reply_source_ids,
            "source_result_ids": result_ids,
            "admission_sample": "after_dispatch_before_terminal_commit",
            "admitted": admitted_ids.len(),
            "settled": settled_ids.len(),
            "missing_admitted_result_ids": result_ids.difference(&admitted_ids).collect::<Vec<_>>(),
            "missing_settled_result_ids": result_ids.difference(&settled_ids).collect::<Vec<_>>(),
            "settlement_states": settlement_states,
            "admitted_per_activation": admissions,
            "admitted_anchor_per_activation": anchor_admissions,
            "admitted_backlog_per_activation": backlog_admissions,
            "activation_starts": execution.attempts.len(),
            "provider_requests": requests.len(),
            "visible_result_bodies_per_request": visible_bodies_per_request,
            "visible_result_identities_per_request": visible_result_identities_per_request,
            "distinct_visible_result_bodies": distinct_visible_bodies,
            "distinct_visible_result_identities": distinct_visible_identities,
            "visible_admitted_bodies_per_request": visible_admitted_bodies_per_request,
            "visible_admitted_identities_per_request": visible_admitted_identities_per_request,
            "reducer_only_notifications": reducer_only,
            "first_response_after_processed_sources": first_response_after_sources,
            "result_notifications_before_first_response": notifications_before_first_response,
            "queue_states_message_id_projection_latest": queue_states,
        })
    );
    admitted_ids == result_ids
        && settled_ids == result_ids
        && admissions == visible_admitted_bodies_per_request
        && queue_states.iter().all(|(_, projected, latest)| {
            *projected == Some(QueueEntryStatus::Processed)
                && *latest == Some(QueueEntryStatus::Processed)
        })
}

#[tokio::test]
async fn command_terminal_result_preaccumulation_baseline() {
    let mut passed = true;
    for count in [1, 2, 8, 9, 16, 17] {
        passed &= terminal_result_baseline(TaskKind::CommandTask, count).await;
    }
    assert!(passed, "command matrix violated result consumption");
}

#[tokio::test]
async fn child_terminal_result_preaccumulation_baseline() {
    let mut passed = true;
    for count in [1, 2, 8, 9, 16, 17] {
        passed &= terminal_result_baseline(TaskKind::ChildAgentTask, count).await;
    }
    assert!(passed, "child matrix violated result consumption");
}
