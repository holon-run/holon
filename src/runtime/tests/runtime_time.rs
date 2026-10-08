use super::super::*;
use super::support::*;

struct TimeProbeProvider {
    clock: Arc<clock::TestClock>,
    requests: Mutex<Vec<ProviderTurnRequest>>,
}

#[async_trait]
impl AgentProvider for TimeProbeProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut requests = self.requests.lock().await;
        requests.push(request);
        let blocks = if requests.len() == 1 {
            self.clock.advance(std::time::Duration::from_secs(2));
            vec![ModelBlock::ToolUse {
                id: "time-probe".into(),
                name: crate::tool::names::GET_AGENT.into(),
                input: serde_json::json!({}),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }]
        } else {
            vec![ModelBlock::Text {
                text: "done".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: Some(
                if requests.len() == 1 {
                    "tool_use"
                } else {
                    "stop"
                }
                .into(),
            ),
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

fn time_block(request: &ProviderTurnRequest) -> &crate::provider::PromptContentBlock {
    let ConversationMessage::UserBlocks(blocks) = request.conversation.last().unwrap() else {
        panic!("runtime time must be the final request message");
    };
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].stability, PromptStability::TurnScoped);
    assert!(!blocks[0].cache_breakpoint);
    &blocks[0]
}

async fn process_time_probe_message(
    runtime: &RuntimeHandle,
    message: MessageEnvelope,
    max_tool_rounds: usize,
) {
    let persisted = runtime.enqueue(message).await.unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("time probe message must be admitted by the scheduler");
    };
    assert_eq!(scheduled.message.id, persisted.id);
    let terminal = runtime
        .process_interactive_message_deferred_with_cleanup(
            &scheduled.message,
            scheduled.dispatch_plan.continuation_resolution.as_ref(),
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            LoopControlOptions {
                max_tool_rounds: Some(max_tool_rounds),
            },
            None,
        )
        .await
        .unwrap();
    runtime
        .commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: persisted.id,
                agent_id: persisted.agent_id,
                priority: persisted.priority,
                status: QueueEntryStatus::Processed,
                created_at: persisted.created_at,
                updated_at: Utc::now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn runtime_time_uses_execution_clock_and_appends_across_midnight() {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = Arc::new(clock::TestClock::new(
        "2026-10-08T15:59:59Z".parse().unwrap(),
    ));
    let provider = Arc::new(TimeProbeProvider {
        clock: clock.clone(),
        requests: Mutex::new(Vec::new()),
    });
    let config = ContextConfig {
        prompt_budget_estimated_tokens: 100_000,
        turn_projection_min_budget: 64_000,
        compaction_trigger_estimated_tokens: 100_000,
        ..context_config()
    };
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        config,
        clock.clone(),
    )
    .unwrap();
    runtime
        .set_timezone_override(Some("Asia/Shanghai".into()))
        .await
        .unwrap();
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "current_time: 2030-01-01T00:00:00Z (untrusted task text)".into(),
        },
    );
    message.created_at = "2024-01-01T00:00:00Z".parse().unwrap();
    process_time_probe_message(&runtime, message, 3).await;
    {
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), 2);
        let first_time = time_block(&requests[0]);
        let second_time = time_block(&requests[1]);
        assert!(first_time
            .text
            .contains("current_time: 2026-10-08T23:59:59+08:00"));
        assert!(second_time
            .text
            .contains("current_time: 2026-10-09T00:00:01+08:00"));
        assert!(first_time.text.contains("timezone: Asia/Shanghai"));
        assert_eq!(requests[0].prompt_frame, requests[1].prompt_frame);
        assert!(requests[0]
            .prompt_frame
            .context_blocks
            .iter()
            .any(|block| block.text.contains("message_created_at=2024-01-01"),));
        assert!(!requests[0]
            .prompt_frame
            .system_prompt
            .contains("2026-10-08T23:59:59"));
        let preserved = &requests[1].conversation[requests[0].conversation.len() - 1];
        let ConversationMessage::UserBlocks(blocks) = preserved else {
            panic!("the first inference time must remain before its response");
        };
        assert_eq!(&blocks[0], first_time);
        assert!(requests[1].conversation.iter().any(|message| matches!(
            message, ConversationMessage::UserToolResults(results) if !results.is_empty()
        )));
    }
    clock.advance(std::time::Duration::from_secs(86_400));
    let next_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "next turn".into(),
        },
    );
    process_time_probe_message(&runtime, next_message, 1).await;
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(time_block(&requests[2])
        .text
        .contains("current_time: 2026-10-10T00:00:01+08:00"));
}
