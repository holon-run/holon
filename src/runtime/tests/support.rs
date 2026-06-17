#![allow(dead_code, unused_imports)]

pub(crate) use async_trait::async_trait;
pub(crate) use chrono::Utc;
pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::Arc;
pub(crate) use tempfile::{tempdir, TempDir};
pub(crate) use tokio::runtime::Runtime;
pub(crate) use tokio::sync::Mutex;

pub(crate) use crate::{
    config::AppConfig,
    context::ContextConfig,
    host::RuntimeHost,
    prompt::{render_section, PromptSection, PromptStability},
    provider::{
        provider_turn_error, AgentProvider, ConversationMessage, ModelBlock,
        ProviderAttemptOutcome, ProviderAttemptRecord, ProviderAttemptTimeline,
        ProviderHttpTraceDiagnostics, ProviderTransportDiagnostics, ProviderTurnRequest,
        ProviderTurnResponse, ReqwestTransportDiagnostics, StubProvider,
    },
    storage::AppStorage,
    system::{ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        ActiveWorkspaceEntry, AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentRegistryStatus, AgentState, AgentStatus, AgentVisibility, AuditEvent, AuthorityClass,
        BriefKind, BriefRecord, CallbackDeliveryMode, ClosureDecision, ClosureOutcome,
        ContinuationClass, ContinuationTriggerKind, DeliverySummaryRecord, LoadedAgentMemory,
        LoadedAgentsMd, MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin,
        PendingWakeHint, Priority, QueueEntryStatus, TaskKind, TaskOutputRetrievalStatus,
        TaskRecord, TaskRecoverySpec, TaskStatus, TimerRecord, TimerStatus, TodoItem,
        TodoItemState, TokenUsage, TurnTerminalKind, TurnTerminalRecord, WaitingIntentStatus,
        WaitingReason, WorkItemRecord, WorkItemState, WorkReactivationMode, WorkspaceEntry,
    },
};

use super::super::*;

pub(crate) fn context_config() -> ContextConfig {
    let available_tools =
        crate::tool::ToolRegistry::new(PathBuf::from("/tmp/holon-test-workspace"))
            .tool_specs_with_families()
            .unwrap()
            .into_iter()
            .filter(|(family, _)| {
                AgentProfilePreset::PublicNamed.allows_tool_capability_family(*family)
            })
            .map(|(_, tool)| tool)
            .collect::<Vec<_>>();
    let prompt_budget_estimated_tokens =
        super::super::turn::estimate_tool_specs_tokens(&available_tools) + 4096;
    ContextConfig {
        recent_messages: 8,
        recent_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens,
        turn_projection_min_budget: prompt_budget_estimated_tokens,
        ..ContextConfig::default()
    }
}

pub(crate) fn continuation_ready_context_config(
    workspace: &TempDir,
    continuation_effective_budget: usize,
) -> ContextConfig {
    let available_tools = crate::tool::ToolRegistry::new(workspace.path().to_path_buf())
        .tool_specs_with_families()
        .unwrap()
        .into_iter()
        .filter(|(family, _)| {
            AgentProfilePreset::PublicNamed.allows_tool_capability_family(*family)
        })
        .map(|(_, tool)| tool)
        .collect::<Vec<_>>();
    let prompt_budget_estimated_tokens =
        super::super::turn::estimate_tool_specs_tokens(&available_tools)
            + super::super::turn::CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
            + continuation_effective_budget;
    ContextConfig {
        prompt_budget_estimated_tokens,
        turn_projection_budget_ratio: 1.0,
        turn_projection_min_budget: 0,
        turn_projection_max_budget: prompt_budget_estimated_tokens,
        ..context_config()
    }
}

pub(crate) async fn wait_for_audit_events(
    runtime: &RuntimeHandle,
    limit: usize,
    mut predicate: impl FnMut(&[AuditEvent]) -> bool,
    label: &str,
) -> Vec<AuditEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let events = runtime.storage().read_recent_events(limit).unwrap();
            if predicate(&events) {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
}

pub(crate) async fn host_backed_test_runtime() -> (TempDir, RuntimeHost, RuntimeHandle) {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
    let runtime = host.default_runtime().await.unwrap();
    (home, host, runtime)
}

pub(crate) fn private_child_identity(agent_id: &str) -> AgentIdentityView {
    AgentIdentityView {
        agent_id: agent_id.into(),
        kind: AgentKind::Child,
        visibility: AgentVisibility::Private,
        ownership: AgentOwnership::ParentSupervised,
        profile_preset: AgentProfilePreset::PrivateChild,
        status: AgentRegistryStatus::Active,
        is_default_agent: false,
        parent_agent_id: Some("default".into()),
        lineage_parent_agent_id: Some("default".into()),
        delegated_from_task_id: Some("task-1".into()),
    }
}

pub(crate) fn test_effective_prompt() -> EffectivePrompt {
    EffectivePrompt {
        identity: AgentIdentityView {
            agent_id: "default".into(),
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            status: AgentRegistryStatus::Active,
            is_default_agent: true,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        },
        agent_home: PathBuf::from("/tmp/agent-home"),
        execution: ExecutionSnapshot {
            profile: ExecutionProfile::default(),
            policy: ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: None,
            workspace_anchor: PathBuf::from("/tmp/agent-home"),
            execution_root: PathBuf::from("/tmp/agent-home"),
            cwd: PathBuf::from("/tmp/agent-home"),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
        },
        loaded_agents_md: LoadedAgentsMd::default(),
        loaded_agent_memory: LoadedAgentMemory::default(),
        cache_identity: crate::prompt::PromptCacheIdentity {
            agent_id: "default".into(),
            prompt_cache_key: "default".into(),
            context_fingerprint: "fingerprint-default".into(),
            compression_epoch: 0,
        },
        system_sections: vec![],
        context_sections: vec![],
        rendered_system_prompt: "system".into(),
        rendered_context_attachment: "context".into(),
    }
}

pub(crate) fn closure_decision(
    outcome: ClosureOutcome,
    waiting_reason: Option<WaitingReason>,
) -> ClosureDecision {
    ClosureDecision {
        outcome,
        waiting_reason,
        work_signal: None,
        runtime_posture: RuntimePosture::Awake,
        evidence: Vec::new(),
    }
}

pub(crate) async fn bind_turn_to_work_item(runtime: &RuntimeHandle, work_item_id: &str) {
    let mut guard = runtime.inner.agent.lock().await;
    guard.state.turn_index = 1;
    guard.state.current_turn_work_item_id = Some(work_item_id.to_string());
    guard.state.last_turn_terminal = Some(TurnTerminalRecord {
        turn_id: "test-turn".into(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some("done".into()),
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 10,
    });
    runtime.inner.storage.write_agent(&guard.state).unwrap();
}

pub(crate) async fn seed_bound_work_item(
    runtime: &RuntimeHandle,
    state: WorkItemState,
    summary: Option<&str>,
    blocked_by: Option<&str>,
) -> String {
    let mut record = runtime
        .create_work_item(
            summary.unwrap_or("finish the bound objective").to_string(),
            None,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    if let Some(blocked_by) = blocked_by {
        record = runtime
            .update_work_item_fields(
                record.id.clone(),
                None,
                None,
                None,
                None,
                Some(Some(blocked_by.to_string())),
            )
            .await
            .unwrap();
    }
    if state == WorkItemState::Completed {
        record = runtime
            .complete_work_item(record.id.clone(), Vec::new())
            .await
            .unwrap();
        if let Some(summary) = summary {
            record = runtime
                .promote_work_item_completion_report(
                    record.id.clone(),
                    summary.to_string(),
                    None,
                    None,
                    Vec::new(),
                )
                .await
                .unwrap();
        }
    }
    bind_turn_to_work_item(runtime, &record.id).await;
    record.id
}

pub(crate) async fn mark_blocking_task(runtime: &RuntimeHandle, task_id: &str) {
    runtime
        .inner
        .storage
        .append_task(&TaskRecord {
            id: task_id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: Some("blocking command".into()),
            detail: Some(serde_json::json!({
                "wait_policy": "blocking"
            })),
            recovery: None,
        })
        .unwrap();
}

pub(crate) struct TruncatingProvider {
    pub(crate) calls: Mutex<usize>,
}

impl TruncatingProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct TimelineProvider;

pub(crate) struct OneToolThenTextProvider {
    pub(crate) calls: Mutex<usize>,
}

impl OneToolThenTextProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

#[async_trait]
impl AgentProvider for OneToolThenTextProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = if *calls == 1 {
            vec![ModelBlock::ToolUse {
                id: "verify".into(),
                name: "ExecCommand".into(),
                input: serde_json::json!({
                    "cmd": "printf 'ok'",
                    "shell": "sh",
                }),
            }]
        } else {
            vec![ModelBlock::Text {
                text: "done".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

pub(crate) struct FailingTimelineProvider;

pub(crate) struct ToolCaptureProvider {
    pub(crate) requests: Mutex<Vec<Vec<String>>>,
}

impl ToolCaptureProvider {
    pub(crate) async fn request_history(&self) -> tokio::sync::MutexGuard<'_, Vec<Vec<String>>> {
        self.requests.lock().await
    }
}

pub(crate) struct TurnLocalCompactionProbeProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) requests: Mutex<Vec<ProviderTurnRequest>>,
}

impl TurnLocalCompactionProbeProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
    pub(crate) async fn request_history(
        &self,
    ) -> tokio::sync::MutexGuard<'_, Vec<ProviderTurnRequest>> {
        self.requests.lock().await
    }
}

pub(crate) struct BaselineOverBudgetProbeProvider {
    pub(crate) calls: Mutex<usize>,
}

impl BaselineOverBudgetProbeProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct ContextLengthExceededProvider;

pub(crate) struct DeferredFallbackProvider;

pub(crate) struct TextThenFailingFallbackProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct SleepOnlyToolProvider {
    pub(crate) calls: Mutex<usize>,
}

impl SleepOnlyToolProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct DisallowedToolThenTextProvider {
    pub(crate) calls: Mutex<usize>,
}

impl DisallowedToolThenTextProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct MaxOutputMutationToolProvider {
    pub(crate) calls: Mutex<usize>,
}

impl MaxOutputMutationToolProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

#[async_trait]
impl AgentProvider for TruncatingProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Partial report heading:".into(),
                }],
                stop_reason: Some("max_tokens".into()),
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        assert!(request.conversation.iter().any(|message| match message {
            ConversationMessage::UserText(text) => {
                text.contains("Output token limit hit")
                    && text.contains("Continue exactly where you left off")
                    && text.contains("Do not restart from the top, repeat analysis")
                    && text.contains("re-read context already provided")
            }
            _ => false,
        }));

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "\n\n- final grounded recommendation".into(),
            }],
            stop_reason: None,
            input_tokens: 50,
            output_tokens: 25,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for TimelineProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "done with fallback history".into(),
            }],
            stop_reason: None,
            input_tokens: 12,
            output_tokens: 6,
            cache_usage: None,
            request_diagnostics: None,
        })
    }

    async fn complete_turn_with_diagnostics(
        &self,
        request: ProviderTurnRequest,
    ) -> Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)> {
        let response = self.complete_turn(request).await?;
        Ok((
            response,
            Some(ProviderAttemptTimeline {
                attempts: vec![
                    ProviderAttemptRecord {
                        provider: "openai".into(),
                        model_ref: "openai/gpt-5.4".into(),
                        attempt: 1,
                        max_attempts: 3,
                        started_at: None,
                        completed_at: None,
                        duration_ms: Some(125),
                        failure_kind: Some("server_error".into()),
                        disposition: Some("retryable".into()),
                        outcome: ProviderAttemptOutcome::Retrying,
                        advanced_to_fallback: false,
                        backoff_ms: Some(200),
                        token_usage: None,
                        transport_diagnostics: None,
                    },
                    ProviderAttemptRecord {
                        provider: "anthropic".into(),
                        model_ref: "anthropic/claude-sonnet-4-6".into(),
                        attempt: 1,
                        max_attempts: 3,
                        started_at: None,
                        completed_at: None,
                        duration_ms: Some(250),
                        failure_kind: None,
                        disposition: None,
                        outcome: ProviderAttemptOutcome::Succeeded,
                        advanced_to_fallback: false,
                        backoff_ms: None,
                        token_usage: Some(TokenUsage::new(12, 6)),
                        transport_diagnostics: None,
                    },
                ],
                aggregated_token_usage: Some(TokenUsage::new(12, 6)),
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                winning_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                pending_fallback_model_ref: None,
            }),
        ))
    }
}

#[async_trait]
impl AgentProvider for ToolCaptureProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.lock().await.push(
            request
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
        );
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: "captured tool set".into(),
            }],
            stop_reason: None,
            input_tokens: 8,
            output_tokens: 4,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for TurnLocalCompactionProbeProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.lock().await.push(request);
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let response = match *calls {
            1 => ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: format!("Round 1 planning {}", "very detailed preamble ".repeat(150)),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-1".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'first-round-output-should-not-stay-exact'",
                            "yield_time_ms": 30000,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            },
            2 => ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 2 planning keep recent exact.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-2".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'second-round-output-should-remain-exact'",
                            "yield_time_ms": 30000,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            },
            3 => ProviderTurnResponse {
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 3 planning keep recent exact too.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-3".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'third-round-output-should-remain-exact'",
                            "yield_time_ms": 30000,
                        }),
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            },
            _ => ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Finished after compacted continuation.".into(),
                }],
                stop_reason: Some("end_turn".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            },
        };
        Ok(response)
    }
}

#[async_trait]
impl AgentProvider for BaselineOverBudgetProbeProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "exec-baseline-over-budget".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'baseline-over-budget'",
                    }),
                }],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                request_diagnostics: None,
            }),
            _ => panic!("continuation request should not be sent after baseline-over-budget"),
        }
    }
}

#[async_trait]
impl AgentProvider for SleepOnlyToolProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls > 1 {
            anyhow::bail!("sleep-only round should not force another provider turn");
        }
        assert!(
            request.tools.iter().all(|tool| tool.name != "Sleep"),
            "Sleep must stay hidden from the provider tool surface"
        );

        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::ToolUse {
                id: "sleep-1".into(),
                name: "Sleep".into(),
                input: serde_json::json!({
                    "reason": "waiting for review",
                    "duration_ms": 250,
                }),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for DisallowedToolThenTextProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "legacy-task".into(),
                    name: "CreateTask".into(),
                    input: serde_json::json!({
                        "prompt": "removed public task surface",
                    }),
                }],
                stop_reason: None,
                input_tokens: 10,
                output_tokens: 5,
                cache_usage: None,
                request_diagnostics: None,
            }),
            2 => {
                assert!(
                    request.conversation.iter().any(|message| matches!(
                        message,
                        ConversationMessage::UserToolResults(results)
                            if results.iter().any(|result|
                                result.tool_use_id == "legacy-task"
                                    && result.is_error
                                    && result
                                        .error
                                        .as_ref()
                                        .is_some_and(|error| error.kind == "tool_not_exposed_for_round")
                            )
                    )),
                    "continuation should receive a structured error for the unavailable tool"
                );
                Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::Text {
                        text: "Recovered after unavailable tool.".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            _ => anyhow::bail!("unexpected provider call after recovery text"),
        }
    }
}

#[async_trait]
impl AgentProvider for MaxOutputMutationToolProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "truncated-patch".into(),
                    name: "ApplyPatch".into(),
                    input: serde_json::json!({
                        "patch": "--- /dev/null\n+++ b/app.txt\n@@ -0,0 +1 @@\n+should-not-be-written\n",
                    }),
                }],
                stop_reason: Some("max_tokens".into()),
                input_tokens: 20,
                output_tokens: 10,
                cache_usage: None,
                request_diagnostics: None,
            }),
            2 => {
                assert!(
                    request.conversation.iter().any(|message| matches!(
                        message,
                        ConversationMessage::UserToolResults(results)
                            if results.iter().any(|result|
                                result.tool_use_id == "truncated-patch"
                                    && result.is_error
                                    && result
                                        .error
                                        .as_ref()
                                        .is_some_and(|error| error.kind == "truncated_mutation_tool_call")
                            )
                    )),
                    "continuation should receive a structured truncation error"
                );
                Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::Text {
                        text: "Recovered after rejected truncated mutation.".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 15,
                    output_tokens: 8,
                    cache_usage: None,
                    request_diagnostics: None,
                })
            }
            _ => panic!("provider should stop after recovery"),
        }
    }
}

#[async_trait]
impl AgentProvider for FailingTimelineProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Err(provider_turn_error(
            "all configured providers failed for this turn: openai/gpt-5.4: fail_fast (contract_error): bad request",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai".into(),
                    model_ref: "openai/gpt-5.4".into(),
                    attempt: 1,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("contract_error".into()),
                    disposition: Some("fail_fast".into()),
                    outcome: ProviderAttemptOutcome::FailFastAborted,
                    advanced_to_fallback: false,
                    backoff_ms: None,
                    token_usage: None,
                    transport_diagnostics: Some(ProviderTransportDiagnostics {
                        stage: "request_send".into(),
                        provider: Some("openai".into()),
                        model_ref: Some("openai/gpt-5.4".into()),
                        url: Some(
                            "https://user:secret@example.com/v1/responses?api_key=token#frag"
                                .into(),
                        ),
                        status: None,
                        reqwest: Some(ReqwestTransportDiagnostics {
                            is_timeout: false,
                            is_connect: false,
                            is_request: false,
                            is_body: true,
                            is_decode: false,
                            is_redirect: false,
                            status: None,
                        }),
                        http_trace: Some(ProviderHttpTraceDiagnostics {
                            mode: "failure_only".into(),
                            path: "/Users/example/.holon/http-trace/default/trace-1-1.jsonl"
                                .into(),
                            status: None,
                        }),
                        source_chain: vec!["connection reset by peer".into()],
                    }),
                }],
                aggregated_token_usage: None,
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: None,
            },
            anyhow!("bad request"),
        ))
    }
}

#[async_trait]
impl AgentProvider for ContextLengthExceededProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Err(provider_turn_error(
            "all configured providers failed for this turn: openai-codex/gpt-5.3-codex-spark: fail_fast (contract_error): context_length_exceeded",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai-codex".into(),
                    model_ref: "openai-codex/gpt-5.3-codex-spark".into(),
                    attempt: 1,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("contract_error".into()),
                    disposition: Some("fail_fast".into()),
                    outcome: ProviderAttemptOutcome::FailFastAborted,
                    advanced_to_fallback: false,
                    backoff_ms: None,
                    token_usage: Some(TokenUsage::new(125_166, 0)),
                    transport_diagnostics: None,
                }],
                aggregated_token_usage: Some(TokenUsage::new(125_166, 0)),
                requested_model_ref: "openai-codex/gpt-5.3-codex-spark".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: None,
            },
            anyhow!("context_length_exceeded: input too long"),
        ))
    }
}

#[async_trait]
impl AgentProvider for DeferredFallbackProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Err(provider_turn_error(
            "all configured providers failed for this turn: openai/gpt-5.4: retryable exhausted",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai".into(),
                    model_ref: "openai/gpt-5.4".into(),
                    attempt: 3,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("server_error".into()),
                    disposition: Some("retryable".into()),
                    outcome: ProviderAttemptOutcome::RetriesExhausted,
                    advanced_to_fallback: true,
                    backoff_ms: None,
                    token_usage: None,
                    transport_diagnostics: None,
                }],
                aggregated_token_usage: None,
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
            },
            anyhow!("server unavailable"),
        ))
    }
}

#[async_trait]
impl AgentProvider for TextThenFailingFallbackProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "Partial report heading".into(),
                }],
                stop_reason: Some("max_output_tokens".into()),
                input_tokens: 20,
                output_tokens: 10,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        Err(provider_turn_error(
            "all configured providers failed for this turn: openai/gpt-5.4: retryable exhausted",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai".into(),
                    model_ref: "openai/gpt-5.4".into(),
                    attempt: 3,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("server_error".into()),
                    disposition: Some("retryable".into()),
                    outcome: ProviderAttemptOutcome::RetriesExhausted,
                    advanced_to_fallback: true,
                    backoff_ms: None,
                    token_usage: None,
                    transport_diagnostics: None,
                }],
                aggregated_token_usage: None,
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
            },
            anyhow!("server unavailable"),
        ))
    }
}

pub(crate) struct StagnatingAfterVerificationProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct SkillReadProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct SkillActivationCommandProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) tool_name: &'static str,
    pub(crate) input: serde_json::Value,
}

pub(crate) struct CountingProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) reply: &'static str,
}

impl StagnatingAfterVerificationProvider {
    pub(crate) fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

impl SkillReadProvider {
    pub(crate) fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

impl SkillActivationCommandProvider {
    pub(crate) fn new(tool_name: &'static str, input: serde_json::Value) -> Self {
        Self {
            calls: Mutex::new(0),
            tool_name,
            input,
        }
    }
}

impl CountingProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

#[async_trait]
impl AgentProvider for StagnatingAfterVerificationProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if request.tools.is_empty() {
            return Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "What changed: app.txt\nWhy: to address the requested task.\nVerification: successful verification command completed.".into(),
                }],
                stop_reason: None,
                input_tokens: 25,
                output_tokens: 25,
                cache_usage: None,
                request_diagnostics: None,
            });
        }

        let mut calls = self.calls.lock().await;
        *calls += 1;

        let blocks = match *calls {
            1 => vec![
                ModelBlock::ToolUse {
                    id: "patch".into(),
                    name: "ApplyPatch".into(),
                    input: serde_json::json!({
                        "patch": "--- a/app.txt\n+++ b/app.txt\n@@ -1,1 +1,1 @@\n-before\n+after\n",
                    }),
                },
                ModelBlock::ToolUse {
                    id: "verify".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'tests passed'",
                        "shell": "sh",
                    }),
                },
            ],
            2 => vec![ModelBlock::ToolUse {
                id: "read".into(),
                name: "ExecCommand".into(),
                input: serde_json::json!({
                    "cmd": "cat app.txt",
                    "workdir": ".",
                }),
            }],
            _ => vec![ModelBlock::ToolUse {
                id: "agent".into(),
                name: "AgentGet".into(),
                input: serde_json::json!({}),
            }],
        };

        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 25,
            output_tokens: 25,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for SkillReadProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        let blocks = match *calls {
            1 => vec![ModelBlock::ToolUse {
                id: "read-skill".into(),
                name: "ExecCommand".into(),
                input: serde_json::json!({
                    "cmd": "cat .agents/skills/demo/SKILL.md",
                    "workdir": ".",
                }),
            }],
            _ => vec![ModelBlock::Text {
                text: "Skill loaded and applied.".into(),
            }],
        };

        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 20,
            output_tokens: 20,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for SkillActivationCommandProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        let blocks = match *calls {
            1 => vec![ModelBlock::ToolUse {
                id: "activate-skill".into(),
                name: self.tool_name.into(),
                input: self.input.clone(),
            }],
            _ => vec![ModelBlock::Text {
                text: "Skill activation observed.".into(),
            }],
        };

        Ok(ProviderTurnResponse {
            blocks,
            stop_reason: None,
            input_tokens: 20,
            output_tokens: 20,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for CountingProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        Ok(ProviderTurnResponse {
            blocks: vec![ModelBlock::Text {
                text: self.reply.into(),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}
