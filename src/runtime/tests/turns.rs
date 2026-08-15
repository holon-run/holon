use super::super::*;
use super::support::*;

struct PickThenExecProvider {
    calls: Mutex<usize>,
    target_work_item_id: String,
}

struct StablePrefixDiagnosticsProvider;

#[async_trait]
impl AgentProvider for StablePrefixDiagnosticsProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
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
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.current_turn_id = Some("turn-terminal-pick".into());
        guard.state.current_turn_work_item_id = Some(caller.id.clone());
        guard.state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some("activation-terminal-pick".into()),
            admission_provenance: None,
            source_message_id: "message-terminal-pick".into(),
            turn_id: "turn-terminal-pick".into(),
            work_item_id: Some(caller.id.clone()),
            claimed_work_revision: Some(caller.revision),
        });
        guard.persist_state(&runtime.inner.storage).unwrap();
    }
    let provider = Arc::new(PickThenExecProvider {
        calls: Mutex::new(0),
        target_work_item_id: target.id.clone(),
    });
    *runtime.inner.provider.write().await = provider.clone();

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
            blocks: vec![
                ModelBlock::ToolUse {
                    id: "pick-target".into(),
                    name: "PickWorkItem".into(),
                    input: serde_json::json!({
                        "work_item_id": self.target_work_item_id,
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                },
                ModelBlock::ToolUse {
                    id: "must-not-run".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf should-not-run",
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
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
    let serialized = serde_json::to_string(&provider_event.data).unwrap();
    assert!(!serialized.contains("system_prompt"));
    assert!(!serialized.contains("conversation"));
    assert!(!serialized.contains("tool_arguments"));
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
    assert_eq!(outcome.sleep_duration_ms, None);

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

#[tokio::test]
async fn turn_local_continuation_recovers_by_reprojecting_recent_turns() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(RecentTurnsRecoveryProbeProvider {
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
    let continuation_effective_budget = 12_000;
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
            turn_projection_budget_ratio: 1.0,
            turn_projection_min_budget: 0,
            turn_projection_max_budget: 10_000,
            callback_base_url: String::new(),
            ..context_config()
        },
    )
    .unwrap();

    let mut historical_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: format!(
                "historical context {}",
                "large-history-token ".repeat(12_000)
            ),
        },
    );
    historical_message.turn_id = Some("turn-large-history".into());
    runtime
        .storage()
        .append_message(&historical_message)
        .unwrap();
    let mut historical_turn = TurnRecord::new("default", "turn-large-history", 1);
    historical_turn.input_message_ids = vec![historical_message.id.clone()];
    historical_turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
        &historical_message,
    ));
    runtime.storage().append_turn(&historical_turn).unwrap();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "continue after the large historical turn".into(),
        },
    );
    runtime
        .process_interactive_message(
            &current_message,
            None,
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
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
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
    assert_eq!(failure.turn_index, Some(1));

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
        .is_some_and(|error| error.contains("all configured providers failed")));
    assert!(deferred.data["operator_message"]
        .as_str()
        .is_some_and(|message| message.contains("Queued fallback turn")));
    assert!(events.iter().any(|event| event.kind == "recovery_enqueued"));
    assert!(!events
        .iter()
        .any(|event| event.kind == "recovery_turn_started"));
}

#[test]
fn provider_recovery_directive_requires_runtime_owned_recovery_provenance() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
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
        MessageOrigin::Operator { actor_id: None },
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
        AuthorityClass::OperatorInstruction,
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
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "trigger runtime failure".into(),
        },
    );
    let error = match runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
    {
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
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "trigger runtime failure".into(),
        },
    );
    let error = match runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
    {
        Ok(_) => panic!("provider failure should abort the turn"),
        Err(error) => error,
    };
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.last_turn_terminal = None;
        runtime.storage().write_agent(&guard.state).unwrap();
        guard.last_persisted_state = guard.state.clone();
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
