use super::*;
use super::support::*;
    async fn interactive_turn_keeps_pending_working_memory_delta_when_prompt_omits_it() {
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
                recent_messages: 4,
                recent_briefs: 4,
                prompt_budget_estimated_tokens: 140,
                ..context_config()
            },
        )
        .unwrap();

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.working_memory.current_working_memory =
                crate::types::WorkingMemorySnapshot {
                    delivery_target: Some("ship the prompt delta gating fix".into()),
                    current_plan: vec!["[InProgress] wire prompt render acknowledgement".into()],
                    ..crate::types::WorkingMemorySnapshot::default()
                };
            guard.state.working_memory.working_memory_revision = 5;
            guard.state.working_memory.pending_working_memory_delta =
                Some(crate::types::WorkingMemoryDelta {
                    from_revision: 4,
                    to_revision: 5,
                    created_at_turn: 7,
                    reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
                    changed_fields: vec!["current_plan".into()],
                    summary_lines: vec![
                        "updated the current plan with a long-form explanation of why prompt rendering acknowledgement must happen after budgeted assembly rather than before prompt construction".into(),
                        "recorded the continuity decision that pending deltas stay durable across turns until the model actually sees the delta section in a rendered prompt".into(),
                        "captured low-budget prompt coverage for the interactive runtime path that previously cleared the delta too early".into(),
                    ],
                });
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let preview = runtime
            .preview_prompt(
                "Continue the runtime memory work and report the latest status.".into(),
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap();
        assert!(!preview
            .context_sections
            .iter()
            .any(|section| section.name == "working_memory_delta"));

        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the runtime memory work and report the latest status.".into(),
            },
        );
        runtime
            .process_interactive_message(
                &message,
                None,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        let state = runtime.agent_state().await.unwrap();
        let pending = state
            .working_memory
            .pending_working_memory_delta
            .as_ref()
            .expect("pending delta should remain until rendered");
        assert_eq!(pending.to_revision, 5);
        assert_eq!(
            state.working_memory.last_prompted_working_memory_revision,
            None
        );
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
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert!(outcome.final_text.contains("Partial report heading:"));
        assert!(outcome.final_text.contains("final grounded recommendation"));
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
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert!(outcome.final_text.contains("runtime split"));

        let events = runtime.storage().read_recent_events(10).unwrap();
        let provider_event = events
            .iter()
            .find(|event| event.kind == "provider_round_completed")
            .expect("missing provider_round_completed");
        assert_eq!(provider_event.data["round"], 1);
        assert_eq!(provider_event.data["tool_call_count"], 0);
        assert_eq!(provider_event.data["text_block_count"], 1);
        assert!(provider_event.data["text_preview"]
            .as_str()
            .unwrap()
            .contains("runtime split"));

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
        prompt.cache_identity.working_memory_revision = 7;
        prompt.cache_identity.compression_epoch = 3;
        prompt.cache_identity.prompt_cache_key = "default:wm7:ce3".into();

        runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
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
            Some("default:wm7:ce3")
        );
        assert_eq!(
            provider_event.data["working_memory_revision"].as_u64(),
            Some(7)
        );
        assert_eq!(provider_event.data["compression_epoch"].as_u64(), Some(3));

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        let assistant_round = transcript
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
            .expect("missing assistant round transcript");
        assert_eq!(
            assistant_round.data["prompt_cache_key"].as_str(),
            Some("default:wm7:ce3")
        );
        assert_eq!(
            assistant_round.data["working_memory_revision"].as_u64(),
            Some(7)
        );
        assert_eq!(assistant_round.data["compression_epoch"].as_u64(), Some(3));
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
                TrustLevel::TrustedOperator,
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
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
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
            0
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
        assert_eq!(
            tool_results.data["results"][0]["tool_use_id"].as_str(),
            Some("legacy-task")
        );
        assert_eq!(
            tool_results.data["results"][0]["is_error"].as_bool(),
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
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
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
        let content = tool_results.data["results"][0]["content"]
            .as_str()
            .expect("tool result content");
        assert!(content.contains("ApplyPatch failed"));
        assert!(content.contains("truncated_mutation_tool_call"));
        assert!(content.contains("max_tokens"));
        assert!(content.contains("retryable: true"));
        assert!(content.len() < 800);
    }

    #[tokio::test]
    async fn detached_runtime_provider_request_still_exposes_spawn_agent() {
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
                TrustLevel::TrustedOperator,
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
            tool_names.iter().any(|name| name == "SpawnAgent"),
            "detached runtime should still expose SpawnAgent to provider requests: {tool_names:?}"
        );
    }

    #[tokio::test]
    async fn turn_local_compaction_rewrites_older_rounds_into_runtime_recap() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(TurnLocalCompactionProbeProvider {
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
                prompt_budget_estimated_tokens: 3600,
                compaction_keep_recent_estimated_tokens: 180,
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
                TrustLevel::TrustedOperator,
                prompt,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
        assert_eq!(*provider.calls.lock().await, 4);

        let requests = provider.requests.lock().await;
        let continuation_request = requests.get(3).expect("missing round 4 request");
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
        assert_eq!(
            round_four_event.data["working_memory_revision"].as_u64(),
            Some(1)
        );
        assert_eq!(round_four_event.data["compression_epoch"].as_u64(), Some(0));
        let transcript = runtime.storage().read_recent_transcript(20).unwrap();
        let round_four_assistant = transcript
            .iter()
            .find(|entry| {
                entry.kind == TranscriptEntryKind::AssistantRound && entry.round == Some(4)
            })
            .expect("missing round 4 assistant transcript");
        assert_eq!(
            round_four_assistant.data["prompt_cache_key"].as_str(),
            Some("default")
        );
        let compaction_event = events
            .iter()
            .rev()
            .find(|event| event.kind == "turn_local_compaction_applied");
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
                recap.contains("ExecCommand completed exit_status=0"),
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
            let checkpoint_request_id = compaction_event.data["checkpoint_request_id"]
                .as_str()
                .expect("compaction event missing checkpoint_request_id");
            let checkpoint_requested = events
                .iter()
                .find(|event| {
                    event.kind == "turn_local_checkpoint_requested"
                        && event.data["checkpoint_request_id"].as_str()
                            == Some(checkpoint_request_id)
                })
                .expect("missing structured checkpoint request event");
            let checkpoint_recorded = events
                .iter()
                .find(|event| {
                    event.kind == "turn_local_checkpoint_recorded"
                        && event.data["checkpoint_request_id"].as_str()
                            == Some(checkpoint_request_id)
                })
                .expect("missing structured checkpoint recorded event");
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
            assert!(checkpoint_recorded.data["text_preview"]
                .as_str()
                .is_some_and(|preview| preview.contains("Finished after compacted continuation")));
        } else {
            assert!(serialized_conversation.contains("first-round-output-should-not-stay-exact"));
            assert!(serialized_conversation.contains("second-round-output-should-remain-exact"));
            assert!(serialized_conversation.contains("third-round-output-should-remain-exact"));
        }
        if let Some(checkpoint) = continuation_request
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserText(text)
                    if text.contains("progress checkpoint request") =>
                {
                    Some(text.clone())
                }
                _ => None,
            })
        {
            assert!(checkpoint.contains("current user goal"));
            assert!(checkpoint.contains("what remains unknown"));
            assert!(checkpoint.contains("next goal-aligned action"));
            assert!(checkpoint.contains("Do not assume the task requires code changes"));
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
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            ContextConfig {
                prompt_budget_estimated_tokens: 320,
                compaction_keep_recent_estimated_tokens: 120,
                ..context_config()
            },
        )
        .unwrap();
        let mut prompt = test_effective_prompt();
        prompt.rendered_system_prompt = "system ".repeat(700);

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
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
            Some("baseline_unfit")
        );
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
            "baseline-over-budget should fail fast, not masquerade as compaction"
        );
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
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "trigger provider context length fail-fast".into(),
            },
        );

        runtime
            .process_interactive_message(
                &message,
                None,
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

        let events = runtime.storage().read_recent_events(20).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "turn_context_length_exceeded"));
        assert!(!events.iter().any(|event| event.kind == "runtime_error"));
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
                TrustLevel::TrustedOperator,
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
            Some("openai/gpt-5.4")
        );
        assert_eq!(
            assistant_round.data["active_model"].as_str(),
            Some("anthropic/claude-sonnet-4-6")
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
        assert_eq!(
            provider_event.data["requested_model"].as_str(),
            Some("openai/gpt-5.4")
        );
        assert_eq!(
            provider_event.data["active_model"].as_str(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(provider_event.data["fallback_active"].as_bool(), Some(true));
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
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
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
        assert_eq!(
            failure.data["provider_attempt_timeline"]["attempts"][0]["transport_diagnostics"]
                ["provider"],
            "openai"
        );
        assert_eq!(
            failure.data["provider_attempt_timeline"]["attempts"][0]["transport_diagnostics"]
                ["stage"],
            "request_send"
        );
        assert_eq!(
            failure.data["failure_artifact"]["metadata"]["url"],
            "https://example.com/v1/responses"
        );
        assert!(failure.data["token_usage"].is_null());
        assert!(failure.data["provider_attempt_timeline"]["winning_model_ref"].is_null());
        assert!(!failure.data["error_chain"].as_array().unwrap().is_empty());
    }

