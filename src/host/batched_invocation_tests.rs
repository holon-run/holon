use super::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::domain::execution_protocol::{ConversationOutcome, ExecutionOutcome};
use crate::provider::{ConversationMessage, ModelBlock, ModelToolCallKind};
use crate::runtime_db::task_result_settlement::{
    TaskResultSettlementDisposition, TaskResultSettlementRecord, TaskResultSettlementState,
};

#[derive(Default)]
struct BatchedResultProvider {
    result_count: usize,
    command_barrier: PathBuf,
    command_to_wait: AsyncMutex<Option<String>>,
    parent: AsyncMutex<Option<RuntimeHandle>>,
    parent_calls: AtomicU64,
    child_calls: AtomicU64,
    root_ready: Notify,
    release_root: Notify,
    consumer_ready: Notify,
    release_consumer: Notify,
    parent_resumed: Notify,
    consumed_batches: AsyncMutex<Vec<Vec<usize>>>,
}

fn batch_tool(id: String, name: &str, input: Value) -> ModelBlock {
    ModelBlock::ToolUse {
        id,
        name: name.into(),
        input,
        kind: ModelToolCallKind::Function,
        provider_data: None,
    }
}

#[async_trait]
impl AgentProvider for BatchedResultProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let is_parent = request.prompt_frame.cache.as_ref().unwrap().agent_id == "default";
        let blocks = if is_parent {
            match self.parent_calls.fetch_add(1, Ordering::SeqCst) {
                0 => vec![batch_tool(
                    "batch-invoke".into(),
                    "InvokeAgent",
                    json!({
                        "target": {"kind": "new_subagent", "workspace_mode": "inherit"},
                        "initial_message": "batch-3927-child"
                    }),
                )],
                1 => {
                    let parent = self.parent.lock().await.clone().unwrap();
                    let invocation = parent
                        .runtime_db()
                        .tasks()
                        .latest_for_agent("default", 20)?
                        .into_iter()
                        .find(|task| task.kind == TaskKind::ActorInvocation)
                        .unwrap();
                    vec![batch_tool(
                        "batch-parent-wait".into(),
                        "WaitFor",
                        json!({
                            "wake": "task_result",
                            "resource": invocation.id,
                            "delivery": "silent",
                            "reason": "wait for this exact supervised invocation"
                        }),
                    )]
                }
                _ => {
                    self.parent_resumed.notify_one();
                    vec![ModelBlock::Text {
                        text: "batch parent resumed".into(),
                    }]
                }
            }
        } else {
            match self.child_calls.fetch_add(1, Ordering::SeqCst) {
                0 => (0..self.result_count)
                    .map(|index| {
                        // The file is a barrier, not a timing assumption: no
                        // command can finish before the root has registered its wait.
                        let path = self
                            .command_barrier
                            .to_string_lossy()
                            .replace('\'', "'\\''");
                        batch_tool(
                            format!("batch-command-{index}"),
                            "ExecCommand",
                            json!({
                                "cmd": format!(
                                    "while [ ! -f '{path}' ]; do sleep 0.01; done; \
                                     printf 'batch-3927-output-{index}: complete\\n'"
                                ),
                                "shell": "sh",
                                "login": false,
                                "yield_time_ms": 0
                            }),
                        )
                    })
                    .collect(),
                1 => {
                    self.root_ready.notify_one();
                    self.release_root.notified().await;
                    vec![batch_tool(
                        "batch-child-wait".into(),
                        "WaitFor",
                        json!({
                            "wake": "task_result",
                            "resource": self.command_to_wait.lock().await.clone().unwrap(),
                            "delivery": "silent",
                            "reason": "consume the exact command result and its pending batch"
                        }),
                    )]
                }
                _ => {
                    // The anchor is current_input; non-anchor results are
                    // independently rendered in pending_task_results.
                    let visible_results = request
                        .conversation
                        .iter()
                        .filter_map(|message| match message {
                            ConversationMessage::UserText(text)
                                if text.contains("Current live input batch")
                                    || text.contains("Current input batch (receive order;")
                                    || text.contains("pending_task_results") =>
                            {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .chain(
                            request
                                .prompt_frame
                                .context_blocks
                                .iter()
                                .map(|block| block.text.as_str())
                                .filter(|text| {
                                    text.contains("Current live input batch")
                                        || text.contains("Current input batch (receive order;")
                                        || text.contains("pending_task_results")
                                }),
                        )
                        .collect::<Vec<_>>();
                    let indices = (0..self.result_count)
                        .filter(|index| {
                            visible_results.iter().any(|text| {
                                text.contains(&format!("batch-3927-output-{index}: complete"))
                            })
                        })
                        .collect::<Vec<_>>();
                    assert!(!indices.is_empty(), "consumer must process command output");
                    let processed = {
                        let mut batches = self.consumed_batches.lock().await;
                        let previous = batches.iter().flatten().copied().collect::<BTreeSet<_>>();
                        assert!(indices.iter().all(|index| !previous.contains(index)));
                        batches.push(indices);
                        batches.iter().map(Vec::len).sum::<usize>()
                    };
                    // Return a final only after processing every item delivered
                    // to this consumer. Hold each final so the test can also
                    // check that the outer invocation has not settled early.
                    self.consumer_ready.notify_one();
                    self.release_consumer.notified().await;
                    vec![ModelBlock::Text {
                        text: format!(
                            "batch child consumed {processed} of {} command results",
                            self.result_count
                        ),
                    }]
                }
            }
        };
        let mut response = StubProvider::new("unused").complete_turn(request).await?;
        response.blocks = blocks;
        Ok(response)
    }
}

async fn batch_notified(notify: &Notify, reason: &str) {
    tokio::time::timeout(Duration::from_secs(10), notify.notified())
        .await
        .expect(reason);
}

fn result_messages(runtime: &RuntimeHandle, task_ids: &BTreeSet<String>) -> Vec<MessageEnvelope> {
    runtime
        .storage()
        .read_recent_messages(128)
        .unwrap()
        .into_iter()
        .filter(|message| {
            message.kind == MessageKind::TaskResult
                && matches!(&message.origin, MessageOrigin::Task { task_id }
                    if task_ids.contains(task_id))
        })
        .collect()
}

fn assert_single_delivered_result(
    runtime: &RuntimeHandle,
    task_id: &str,
) -> TaskResultSettlementRecord {
    let results = result_messages(runtime, &BTreeSet::from([task_id.to_owned()]));
    assert_eq!(results.len(), 1, "publish exactly one result for {task_id}");
    let count = runtime
        .runtime_db()
        .with_read(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM task_result_settlements WHERE task_id = ?1",
                [task_id],
                |row| row.get::<_, u64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 1, "persist exactly one settlement for {task_id}");
    let settlement = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(&results[0].id)
        .unwrap()
        .unwrap();
    assert_eq!(settlement.task_id, task_id);
    assert_eq!(settlement.message_id, results[0].id);
    assert_eq!(settlement.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settlement.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    settlement
}

#[tokio::test]
async fn supervised_invocation_two_real_results_share_one_consumer() {
    assert_real_batched_invocation(2).await;
}

async fn assert_real_batched_invocation(result_count: usize) {
    let home = tempdir().unwrap();
    write_test_model_config(home.path());
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    config.user_home_dir = Some(home.path().to_path_buf());
    config.default_agent_id = "default".into();
    let provider = Arc::new(BatchedResultProvider {
        result_count,
        command_barrier: home.path().join("release-command-results"),
        ..Default::default()
    });
    let host = RuntimeHost::new_with_provider(config, provider.clone()).unwrap();
    let parent = host.default_runtime().await.unwrap();
    *provider.parent.lock().await = Some(parent.clone());
    parent
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
                text: "batch-3927-parent".into(),
            },
        ))
        .await
        .unwrap();
    batch_notified(
        &provider.root_ready,
        "child must launch real background commands",
    )
    .await;
    let invocation = parent
        .runtime_db()
        .tasks()
        .latest_for_agent("default", 20)
        .unwrap()
        .into_iter()
        .find(|task| task.kind == TaskKind::ActorInvocation)
        .unwrap();
    let child_id = invocation.target().unwrap().agent_id;
    let child = host.get_or_create_agent(&child_id).await.unwrap();
    let commands = child
        .runtime_db()
        .tasks()
        .latest_for_agent(&child_id, 32)
        .unwrap()
        .into_iter()
        .filter(|task| task.kind == TaskKind::CommandTask)
        .collect::<Vec<_>>();
    assert_eq!(commands.len(), result_count);
    *provider.command_to_wait.lock().await = Some(commands[0].id.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        while parent
            .storage()
            .active_wait_conditions_for_agent("default")
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("parent must commit its exact invocation wait");

    let pause = child.pause_run_loop_for_monitor_test();
    tokio::pin!(pause);
    // Poll the write lock before releasing the provider. Tokio's fair RwLock
    // then parks the loop at the root's ACK boundary, before idle admission.
    tokio::select! {
        biased;
        _ = pause.as_mut() => panic!("the blocked root must still own the run-loop gate"),
        _ = std::future::ready(()) => {}
    }
    provider.release_root.notify_one();
    let run_loop = tokio::time::timeout(Duration::from_secs(10), pause.as_mut())
        .await
        .expect("root must finish before the result-consumer barrier");
    let root_state = host
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized(&child_id)
        .unwrap()
        .unwrap();
    assert_eq!(root_state.attempts.len(), 1);
    let root = root_state.attempts.values().next().unwrap();
    assert!(matches!(
        root_state.outcomes[root.terminal_outcome_id.as_ref().unwrap()].outcome,
        ExecutionOutcome::Conversation(ConversationOutcome::Wait { .. })
    ));
    for command in &commands {
        assert_eq!(
            command.detail.as_ref().unwrap()["parent_turn_id"].as_str(),
            root.turn_id.as_deref()
        );
    }

    fs::write(&provider.command_barrier, b"release").unwrap();
    let task_ids = commands
        .iter()
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let results = result_messages(&child, &task_ids);
            if results.len() == result_count
                && results.iter().all(|message| {
                    child
                        .runtime_db()
                        .task_result_settlements()
                        .latest_for_message(&message.id)
                        .unwrap()
                        .is_some_and(|settlement| {
                            settlement.state == TaskResultSettlementState::PersistedPending
                        })
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all real command results must be queued before admitting any consumer");
    drop(run_loop);

    let expected_batch_sizes = vec![result_count];
    for (index, expected_size) in expected_batch_sizes.iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(10), provider.consumer_ready.notified())
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "a result batch must reach the model: {error}; child_calls={}; tasks={:?}; settlements={:?}",
                    provider.child_calls.load(Ordering::SeqCst),
                    child.runtime_db().tasks().latest_for_agent(&child_id, 8).unwrap(),
                    result_messages(&child, &task_ids).iter().map(|message| {
                        child.runtime_db().task_result_settlements()
                            .latest_for_message(&message.id).unwrap()
                    }).collect::<Vec<_>>()
                )
            });
        let batches = provider.consumed_batches.lock().await;
        assert_eq!(batches.len(), index + 1);
        assert_eq!(batches[index].len(), *expected_size);
        drop(batches);
        assert_eq!(
            parent
                .storage()
                .latest_task_record(&invocation.id)
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running,
            "the outer invocation must wait for every consumer's final"
        );
        assert_eq!(provider.parent_calls.load(Ordering::SeqCst), 2);
        provider.release_consumer.notify_one();
    }
    let terminal = wait_for_terminal_task(&parent, &invocation.id).await;
    assert_eq!(terminal.status, TaskStatus::Completed);
    batch_notified(
        &provider.parent_resumed,
        "the exact parent wait must resume",
    )
    .await;
    wait_for_agent_inputs_settled(&child).await;
    wait_for_agent_inputs_settled(&parent).await;
    assert!(parent
        .storage()
        .active_wait_conditions_for_agent("default")
        .unwrap()
        .is_empty());
    assert_eq!(
        provider.parent_calls.load(Ordering::SeqCst),
        3,
        "resume only once"
    );
    assert_eq!(
        provider.child_calls.load(Ordering::SeqCst),
        2 + expected_batch_sizes.len() as u64
    );

    let execution = host
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized(&child_id)
        .unwrap()
        .unwrap();
    assert_eq!(execution.attempts.len(), 1 + expected_batch_sizes.len());
    let mut consumers = BTreeMap::<String, Vec<TaskResultSettlementRecord>>::new();
    for command in &commands {
        assert_eq!(
            child
                .storage()
                .latest_task_record(&command.id)
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Completed
        );
        let settlement = assert_single_delivered_result(&child, &command.id);
        consumers
            .entry(settlement.activation_id.clone().unwrap())
            .or_default()
            .push(settlement);
    }
    let mut actual_sizes = consumers.values().map(Vec::len).collect::<Vec<_>>();
    actual_sizes.sort_unstable();
    let mut expected_sizes = expected_batch_sizes.clone();
    expected_sizes.sort_unstable();
    assert_eq!(
        actual_sizes, expected_sizes,
        "canonical settlements must share consumers"
    );
    for (activation, settlements) in &consumers {
        let consumer = &execution.attempts[activation];
        let turn = child
            .storage()
            .read_turn_by_id(consumer.turn_id.as_ref().unwrap())
            .unwrap()
            .unwrap();
        let batch = turn.idle_input_batch.as_ref().unwrap();
        assert_eq!(batch.activation_id.as_deref(), Some(activation.as_str()));
        let result_ids = settlements
            .iter()
            .map(|record| record.message_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(result_ids.contains(batch.anchor_message_id.as_str()));
        assert!(batch
            .items
            .iter()
            .all(|item| result_ids.contains(item.message_id.as_str())));
        for settlement in settlements {
            assert!(
                batch
                    .items
                    .iter()
                    .any(|item| item.message_id == settlement.message_id)
                    || settlement.source_projection.as_ref().is_some_and(|item| {
                        item.message_id == settlement.message_id
                            && item.source_ref == format!("message:{}", settlement.message_id)
                    }),
                "each result needs exact current-input or pending-result evidence"
            );
        }
        assert!(matches!(
            &consumer.source.identity,
            ExecutionSourceIdentity::TriggeredWait { trigger_message_id, .. }
                if trigger_message_id == &batch.anchor_message_id
        ));
        assert!(matches!(
            execution.outcomes[consumer.terminal_outcome_id.as_ref().unwrap()].outcome,
            ExecutionOutcome::Conversation(ConversationOutcome::Replied)
        ));
        let briefs = child.storage().read_briefs_for_turn(&turn.turn_id).unwrap();
        assert_eq!(briefs.len(), 1);
        assert_eq!(
            briefs[0].text,
            format!("batch child consumed {result_count} of {result_count} command results")
        );
        for settlement in settlements {
            if settlement.message_id != batch.anchor_message_id {
                assert!(
                    !execution.attempts.values().any(|attempt| matches!(
                        &attempt.source.identity,
                        ExecutionSourceIdentity::TaskResult { result_message_id, .. }
                            if result_message_id == &settlement.message_id
                    )),
                    "a non-anchor result must not have its own execution source"
                );
            }
        }
    }
    assert_single_delivered_result(&parent, &invocation.id);
    host.shutdown().await.unwrap();
}
