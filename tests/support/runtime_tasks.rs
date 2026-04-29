#![allow(dead_code)]

// Shared runtime integration fixtures and test implementations.
// Domain-focused suites in sibling files call into these functions so coverage
// stays grouped by runtime contract without duplicating the heavy setup layer.
//
// This file now focuses on domain-specific test logic, while reusable
// provider implementations and shared harness helpers have been moved to
// runtime_providers.rs and runtime_helpers.rs respectively.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    ingress::{WakeDisposition, WakeHint},
    policy::validate_message_kind_for_origin,
    provider::{
        AgentProvider, ConversationMessage, ModelBlock, ProviderTurnRequest, ProviderTurnResponse,
        StubProvider,
    },
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    tool::{ToolCall, ToolError, ToolRegistry, ToolResult},
    types::{
        AgentKind, AgentProfilePreset, AgentStatus, BriefKind, CallbackDeliveryMode,
        ChildAgentPhase, ClosureOutcome, CommandTaskSpec, ControlAction, ExternalTriggerStatus,
        FailureArtifactCategory, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        OperatorNotificationBoundary, OperatorTransportBinding, OperatorTransportBindingStatus,
        OperatorTransportCapabilities, OperatorTransportDeliveryAuth,
        OperatorTransportDeliveryAuthKind, Priority, TaskStatus, TokenUsage, TranscriptEntry,
        TranscriptEntryKind, TrustLevel, WaitingIntentStatus, WaitingReason, WorkItemStatus,
        WorkPlanItem, WorkPlanStepStatus,
    },
};
use serde_json::json;
use tokio::sync::Mutex;

use tokio::time::{sleep, Duration};

use crate::support::runtime_helpers::{
    aggressive_compaction_config, git, init_git_repo, operator_transport_binding,
    parse_tool_result_payload, parse_tool_result_value, test_config, wait_for_worktree_presence,
    wait_until, wait_until_async, wait_until_async_for,
};
use crate::support::runtime_providers::{
    DelayedTextProvider, DelegatedBoundaryProvider, FileEditingProvider, LongShellProvider,
    MaxOutputRecoveryProvider, MaxOutputThenCompactionProvider,
    MultiPassCompactionRecoveryFlowProvider, NotifyThenAgentGetProvider, RecordingPromptProvider,
    RepeatedCompactionProvider, RuntimeFailureProvider, ShellProvider,
    SleepOnlyCompletionAfterTextProvider, TerminalResultBriefProvider, ToolErrorProvider,
    ToolUsingProvider, TruncatedShellReinjectionProvider, UseWorkspaceProvider,
    VerboseRuntimeFailureProvider, WakeHintProvider, WorktreeCapturingProvider,
    WorktreeLifecycleProvider,
};
use crate::support::{
    attach_default_workspace, eventually, eventually_async, eventually_for, TestConfigBuilder,
};

// ============================================================================
// Runtime tasks domain test support
pub async fn background_task_rejoins_main_session() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let task = runtime
        .schedule_command_task(
            "bg task".into(),
            CommandTaskSpec {
                cmd: "sleep 1".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let summary = runtime.agent_summary().await?;
    assert_ne!(summary.agent.status, AgentStatus::AwaitingTask);
    assert_eq!(summary.closure.outcome, ClosureOutcome::Completed);
    assert_eq!(summary.closure.waiting_reason, None);

    wait_until(|| {
        let state = runtime.storage().read_agent()?;
        let tasks = runtime.storage().latest_task_records()?;
        Ok(state
            .as_ref()
            .map(|agent| !agent.active_task_ids.contains(&task.id))
            .unwrap_or(false)
            && tasks.iter().any(|record| {
                record.id == task.id && record.status == holon::types::TaskStatus::Completed
            }))
    })
    .await?;

    let state = runtime.agent_state().await?;
    assert!(!state.active_task_ids.contains(&task.id));

    let tasks = runtime.recent_tasks(10).await?;
    assert!(
        tasks
            .iter()
            .any(|record| record.id == task.id
                && record.status == holon::types::TaskStatus::Completed)
    );
    Ok(())
}

pub async fn stop_task_cancels_running_background_task() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let task = runtime
        .schedule_command_task(
            "long bg task".into(),
            CommandTaskSpec {
                cmd: "sleep 30".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Cancelled
        }))
    })
    .await?;

    let state = runtime.agent_state().await?;
    assert!(!state.active_task_ids.contains(&task.id));
    Ok(())
}

pub async fn tool_use_round_trip_executes_and_returns_result() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(ToolUsingProvider::new()))?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "inspect session state".into(),
            },
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let briefs = runtime.recent_briefs(10).await?;
    assert!(briefs
        .iter()
        .any(|brief| brief.text.contains("tool loop complete")));
    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| event.kind == "tool_executed"));
    let session = runtime.agent_state().await?;
    assert_eq!(session.total_input_tokens, 200);
    assert_eq!(session.total_output_tokens, 100);
    assert_eq!(session.total_model_rounds, 2);
    let summary = runtime.agent_summary().await?;
    assert_eq!(summary.token_usage.total.input_tokens, 200);
    assert_eq!(summary.token_usage.total.output_tokens, 100);
    assert_eq!(summary.token_usage.total.total_tokens, 300);
    assert_eq!(summary.token_usage.total_model_rounds, 2);
    assert_eq!(
        summary
            .token_usage
            .last_turn
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(150)
    );
    let transcript = runtime.recent_transcript(10).await?;
    assert!(transcript.iter().any(|entry| {
        entry.kind == holon::types::TranscriptEntryKind::IncomingMessage
            && entry.related_message_id.is_some()
    }));
    assert!(transcript.iter().any(|entry| {
        entry.kind == holon::types::TranscriptEntryKind::AssistantRound && entry.round == Some(1)
    }));
    assert!(transcript.iter().any(|entry| {
        entry.kind == holon::types::TranscriptEntryKind::ToolResults && entry.round == Some(1)
    }));
    let tool_records = runtime.storage().read_recent_tool_executions(10)?;
    let state_record = tool_records
        .iter()
        .find(|record| record.tool_name == "AgentGet")
        .expect("AgentGet record should exist");
    assert!(state_record.completed_at.is_some());
    assert!(state_record.duration_ms <= 5_000);
    let payload = state_record
        .output
        .get("envelope")
        .and_then(|value| value.get("result"))
        .cloned()
        .expect("AgentGet output should contain envelope.result");
    let captured_summary: holon::types::AgentGetResult = serde_json::from_value(payload)?;
    assert_eq!(
        captured_summary
            .agent
            .agent
            .working_memory
            .working_memory_revision,
        0,
        "working memory should not update mid-tool-loop"
    );

    let state = runtime.agent_state().await?;
    assert_eq!(
        state.working_memory.working_memory_revision, 0,
        "tool-result prose alone should not churn working memory"
    );
    assert!(state.working_memory.pending_working_memory_delta.is_none());
    let prompt = runtime
        .preview_prompt("continue the work".into(), TrustLevel::TrustedOperator)
        .await?;
    assert!(!prompt
        .context_sections
        .iter()
        .any(|section| section.name == "working_memory"));
    assert!(!prompt
        .context_sections
        .iter()
        .any(|section| section.name == "working_memory_delta"));
    Ok(())
}

pub async fn file_tools_can_modify_workspace_and_reenter_context() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(FileEditingProvider::new()))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "create a note and confirm its content".into(),
            },
        ))
        .await?;
    wait_until(|| Ok(workspace.join("notes/result.txt").exists())).await?;

    let content = std::fs::read_to_string(workspace.join("notes/result.txt"))?;
    assert_eq!(content, "hello from holon\n");
    wait_until_async(|| {
        let runtime = runtime.clone();
        async move {
            let briefs = runtime.recent_briefs(10).await?;
            Ok(briefs
                .iter()
                .any(|brief| brief.text.contains("file tools complete")))
        }
    })
    .await?;
    Ok(())
}

pub async fn shell_tools_capture_command_output() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(ShellProvider::new()))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "run a shell command and summarize it".into(),
            },
        ))
        .await?;
    wait_until_async(|| {
        let runtime = runtime.clone();
        async move {
            let briefs = runtime.recent_briefs(10).await?;
            Ok(briefs
                .iter()
                .any(|brief| brief.text.contains("shell tools complete")))
        }
    })
    .await?;
    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| event.kind == "tool_executed"));
    Ok(())
}

pub async fn shell_tools_truncate_large_output_before_provider_reinjection() -> Result<()> {
    let payload = "shell_chunk_".repeat(300);
    let host = RuntimeHost::new_with_provider(
        test_config(),
        Arc::new(TruncatedShellReinjectionProvider::new(payload)),
    )?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "inspect a large shell output safely".into(),
            },
        ))
        .await?;
    wait_until_async(|| {
        let runtime = runtime.clone();
        async move {
            let briefs = runtime.recent_briefs(10).await?;
            Ok(briefs
                .iter()
                .any(|brief| brief.text.contains("truncated shell reinjection observed")))
        }
    })
    .await?;

    Ok(())
}

pub async fn exec_command_reports_nonzero_exit_and_truncates_output() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let long_stdout = "out".repeat(40);
    let long_stderr = "err".repeat(40);
    let shell_cmd = format!(
        "printf '{}' ; printf '{}' >&2 ; exit 7",
        long_stdout, long_stderr
    );

    let (result, record) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-exec-nonzero".into(),
                name: "ExecCommand".into(),
                input: json!({
                    "cmd": shell_cmd,
                    "login": false,
                    "max_output_tokens": 50
                }),
            },
        )
        .await?;

    assert!(!result.is_error());
    assert_eq!(record.status, holon::types::ToolExecutionStatus::Success);
    assert_eq!(record.tool_name, "ExecCommand");
    let envelope: serde_json::Value = parse_tool_result_value(&result)?;
    let value = &envelope["result"];
    assert_eq!(envelope["tool_name"], "ExecCommand");
    assert_eq!(envelope["status"], "success");
    assert_eq!(value["disposition"], "completed");
    assert!(value.get("promoted_to_task").is_none());
    assert!(value.get("task_handle").is_none());
    assert_eq!(value["exit_status"], 7);
    assert!(value["stdout_preview"]
        .as_str()
        .expect("stdout should be present")
        .contains("[output truncated"));
    assert!(value["stderr_preview"]
        .as_str()
        .expect("stderr should be present")
        .contains("[output truncated"));
    assert_eq!(value["truncated"], true);
    assert!(envelope["summary_text"]
        .as_str()
        .expect("summary text should be present")
        .contains("command exited with status 7"));
    Ok(())
}

pub async fn exec_command_batch_returns_grouped_item_results() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());
    let long_stdout = "batch_chunk_".repeat(80);

    let (result, record) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-exec-batch".into(),
                name: "ExecCommandBatch".into(),
                input: json!({
                    "items": [
                        {
                            "cmd": "printf batch_ok",
                            "login": false
                        },
                        {
                            "cmd": "printf batch_fail >&2; exit 7",
                            "login": false
                        },
                        {
                            "cmd": "python -i",
                            "tty": true
                        },
                        {
                            "cmd": format!("printf '{}'", long_stdout),
                            "login": false,
                            "max_output_tokens": 20
                        }
                    ],
                    "stop_on_error": false
                }),
            },
        )
        .await?;

    assert!(!result.is_error());
    assert_eq!(record.tool_name, "ExecCommandBatch");
    assert_eq!(record.status, holon::types::ToolExecutionStatus::Success);
    let envelope = parse_tool_result_value(&result)?;
    let value = &envelope["result"];
    assert_eq!(envelope["tool_name"], "ExecCommandBatch");
    assert_eq!(value["item_count"], 4);
    assert_eq!(value["completed_count"], 2);
    assert_eq!(value["failed_count"], 1);
    assert_eq!(value["rejected_count"], 1);
    assert_eq!(value["skipped_count"], 0);
    assert_eq!(value["items"][0]["status"], "completed");
    assert_eq!(value["items"][0]["result"]["exit_status"], 0);
    assert_eq!(value["items"][1]["status"], "failed");
    assert_eq!(value["items"][1]["result"]["exit_status"], 7);
    assert_eq!(value["items"][2]["status"], "rejected");
    assert_eq!(
        value["items"][2]["error_kind"],
        "unsupported_batch_command_field"
    );
    assert!(value["items"][3]["result"]["stdout_preview"]
        .as_str()
        .expect("stdout preview")
        .contains("[output truncated"));
    assert!(
        runtime.latest_task_records_snapshot()?.is_empty(),
        "batch items should not promote into command_task records"
    );
    Ok(())
}

pub async fn exec_command_batch_stop_on_error_skips_later_items() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-exec-batch-stop".into(),
                name: "ExecCommandBatch".into(),
                input: json!({
                    "items": [
                        {
                            "cmd": "exit 9",
                            "login": false
                        },
                        {
                            "cmd": "printf should_not_run",
                            "login": false
                        }
                    ],
                    "stop_on_error": true
                }),
            },
        )
        .await?;

    let value = parse_tool_result_payload(&result)?;
    assert_eq!(value["item_count"], 2);
    assert_eq!(value["failed_count"], 1);
    assert_eq!(value["skipped_count"], 1);
    assert_eq!(value["items"][1]["status"], "skipped");
    assert!(value["items"][1]["result"].is_null());
    Ok(())
}

pub async fn exec_command_workdir_violation_returns_structured_error() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let error = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-exec-invalid-workdir".into(),
                name: "ExecCommand".into(),
                input: json!({
                    "cmd": "pwd",
                    "workdir": "../outside"
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = ToolError::from_anyhow(&error);

    assert_eq!(tool_error.kind, "execution_root_violation");
    assert_eq!(
        tool_error.message,
        "requested working directory is outside the current execution root"
    );
    assert_eq!(
        tool_error
            .details
            .as_ref()
            .and_then(|value| value.get("attempted_workdir")),
        Some(&json!("../outside"))
    );
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("omit `workdir`")));
    Ok(())
}

pub async fn exec_command_spawn_failure_returns_shell_recovery_hint() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let error = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-exec-invalid-shell".into(),
                name: "ExecCommand".into(),
                input: json!({
                    "cmd": "pwd",
                    "shell": "/definitely/not/a/real/shell",
                    "login": false
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = ToolError::from_anyhow(&error);

    assert_eq!(tool_error.kind, "command_spawn_failed");
    assert!(tool_error
        .details
        .as_ref()
        .and_then(|value| value.get("shell"))
        .is_some_and(|value| value == "/definitely/not/a/real/shell"));
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("omit `shell`")));
    Ok(())
}

pub async fn tool_schema_and_dispatch_errors_are_recorded_without_corrupting_runtime_state(
) -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(ToolErrorProvider::new()))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "trigger tool failures".into(),
            },
        ))
        .await?;

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains("tool failures handled")))
    })
    .await?;

    let events = runtime.recent_events(20).await?;
    let failed_events = events
        .iter()
        .filter(|event| event.kind == "tool_execution_failed")
        .collect::<Vec<_>>();
    assert_eq!(failed_events.len(), 3);
    assert!(failed_events.iter().any(|event| {
        event.data.get("tool_name").and_then(|value| value.as_str()) == Some("ExecCommand")
            && event.data["tool_error"]["kind"].as_str() == Some("invalid_tool_input")
            && event
                .data
                .get("tool_error")
                .and_then(|value| value.get("details"))
                .and_then(|value| value.get("parse_error"))
                .and_then(|value| value.as_str())
                .is_some_and(|parse_error| parse_error.contains("missing field `cmd`"))
    }));
    assert!(failed_events.iter().any(|event| {
        event.data.get("tool_name").and_then(|value| value.as_str()) == Some("DefinitelyNotATool")
            && event.data.get("reason").and_then(|value| value.as_str())
                == Some("tool_not_exposed_for_round")
            && event.data["tool_error"]["kind"].as_str() == Some("tool_not_exposed_for_round")
            && event
                .data
                .get("error")
                .and_then(|value| value.as_str())
                .is_some_and(|error| {
                    error.contains("tool DefinitelyNotATool was not exposed in this round")
                })
    }));
    assert!(failed_events.iter().any(|event| {
        event.data.get("tool_name").and_then(|value| value.as_str()) == Some("Read")
            && event.data.get("reason").and_then(|value| value.as_str())
                == Some("tool_not_exposed_for_round")
            && event.data["tool_error"]["kind"].as_str() == Some("tool_not_exposed_for_round")
            && event
                .data
                .get("error")
                .and_then(|value| value.as_str())
                .is_some_and(|error| error.contains("tool Read was not exposed in this round"))
    }));

    let transcript = runtime.recent_transcript(20).await?;
    let tool_results = transcript
        .iter()
        .find(|entry| entry.kind == holon::types::TranscriptEntryKind::ToolResults)
        .expect("tool results transcript entry should exist");
    let results = tool_results.data["results"]
        .as_array()
        .expect("tool results should be an array");
    assert_eq!(results.len(), 3);
    assert!(results
        .iter()
        .all(|result| { result.get("is_error").and_then(|value| value.as_bool()) == Some(true) }));
    assert!(results.iter().all(|result| {
        result
            .get("error")
            .and_then(|value| value.get("kind"))
            .and_then(|value| value.as_str())
            .is_some()
    }));

    let tool_records = runtime.storage().read_recent_tool_executions(10)?;
    assert!(!tool_records.iter().any(|record| {
        record.tool_name == "ExecCommand" || record.tool_name == "DefinitelyNotATool"
    }));

    let briefs = runtime.recent_briefs(10).await?;
    assert!(briefs
        .iter()
        .any(|brief| brief.text.contains("tool failures handled")));

    let state = runtime.agent_state().await?;
    assert_eq!(state.status, AgentStatus::Asleep);
    assert!(state.active_task_ids.is_empty());
    Ok(())
}

pub async fn runtime_provider_failure_surfaces_failure_brief_and_transcript_entry() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(RuntimeFailureProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "trigger runtime failure".into(),
            },
        ))
        .await?;

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs.iter().any(|brief| {
            brief.kind == BriefKind::Failure
                && brief
                    .text
                    .contains("Turn failed while processing operator_prompt")
                && brief.text.contains("provider transport broke")
        }))
    })
    .await?;

    let transcript = runtime.recent_transcript(20).await?;
    let failure_entry = transcript
        .iter()
        .rev()
        .find(|entry| entry.kind == TranscriptEntryKind::RuntimeFailure)
        .expect("runtime failure transcript entry should exist");
    assert_eq!(
        failure_entry
            .data
            .get("error")
            .and_then(|value| value.as_str()),
        Some("provider transport broke")
    );
    assert!(failure_entry
        .data
        .get("text")
        .and_then(|value| value.as_str())
        .is_some_and(|text| text.contains("Turn failed while processing operator_prompt")));

    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| {
        event.kind == "runtime_error"
            && event
                .data
                .get("message_kind")
                .and_then(|value| value.as_str())
                == Some("operator_prompt")
            && event.data.get("error").and_then(|value| value.as_str())
                == Some("provider transport broke")
    }));

    let summary = runtime.agent_summary().await?;
    assert_eq!(summary.closure.outcome, ClosureOutcome::Failed);
    let state = runtime.agent_state().await?;
    let last_failure = state
        .last_runtime_failure
        .expect("runtime failure should be persisted on agent state");
    assert_eq!(
        last_failure.phase,
        holon::types::RuntimeFailurePhase::RuntimeTurn
    );
    assert!(last_failure.summary.contains("provider transport broke"));
    let artifact = last_failure
        .failure_artifact
        .as_ref()
        .expect("runtime failure should include normalized failure artifact");
    assert_eq!(artifact.category, FailureArtifactCategory::Runtime);
    assert_eq!(artifact.kind, "runtime_error");
    assert!(artifact.summary.contains("provider transport broke"));
    Ok(())
}

pub async fn runtime_failure_brief_sanitizes_long_provider_error_but_transcript_keeps_full_error(
) -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(VerboseRuntimeFailureProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "trigger verbose runtime failure".into(),
            },
        ))
        .await?;

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs.iter().any(|brief| {
            brief.kind == BriefKind::Failure
                && brief
                    .text
                    .contains("Turn failed while processing operator_prompt:")
        }))
    })
    .await?;

    let failure_brief = runtime
        .recent_briefs(10)
        .await?
        .into_iter()
        .rev()
        .find(|brief| brief.kind == BriefKind::Failure)
        .expect("failure brief should exist");
    assert!(!failure_brief.text.contains("raw backend body"));
    assert!(failure_brief.text.ends_with('…'));
    assert!(
        failure_brief.text.chars().count()
            <= 200
                + "Turn failed while processing operator_prompt: "
                    .chars()
                    .count()
    );

    let failure_entry = runtime
        .recent_transcript(20)
        .await?
        .into_iter()
        .rev()
        .find(|entry| entry.kind == TranscriptEntryKind::RuntimeFailure)
        .expect("runtime failure transcript entry should exist");
    assert!(failure_entry
        .data
        .get("error")
        .and_then(|value| value.as_str())
        .is_some_and(|error| error.contains("raw backend body")));

    Ok(())
}

pub async fn command_task_runs_to_completion_and_persists_detail() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_command_task(
            "run a managed command".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf managed_ok".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(512),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let stored = runtime.task_record(&task.id).await?.unwrap();
    assert_eq!(stored.kind.as_str(), "command_task");
    let detail = stored.detail.unwrap_or_default();
    assert_eq!(detail["cmd"], "printf managed_ok");
    assert_eq!(detail["continue_on_result"], false);
    let output_path = detail["output_path"]
        .as_str()
        .expect("command task should expose output_path");
    assert!(std::path::Path::new(output_path).exists());
    assert_eq!(std::fs::read_to_string(output_path)?, "managed_ok");
    Ok(())
}

pub async fn task_output_returns_completed_command_task_output() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "print managed task output".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf managed_task_output".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(512),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-command".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["kind"], "command_task");
    assert_eq!(value["task"]["output_preview"], "managed_task_output");
    assert_eq!(value["task"]["exit_status"], 0);
    assert!(value["task"]["artifacts"][0]["path"].as_str().is_some());
    Ok(())
}

pub async fn task_output_non_blocking_reports_running_command_task() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "sleep in background".into(),
            holon::types::CommandTaskSpec {
                cmd: "sleep 5".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-running".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id, "block": false }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "not_ready");
    assert_eq!(value["task"]["kind"], "command_task");

    runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await?;
    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Cancelled
        }))
    })
    .await?;
    Ok(())
}

pub async fn task_output_waits_for_command_task_completion() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "wait for command completion".into(),
            CommandTaskSpec {
                cmd: "sleep 0.2; printf done".into(),
                workdir: None,
                shell: Some("sh".into()),
                login: false,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-command".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["kind"], "command_task");
    assert!(value["task"]["output_preview"]
        .as_str()
        .expect("command task output should be text")
        .contains("done"));
    assert!(value["task"]["artifacts"][0]["path"].is_string());
    Ok(())
}

pub async fn task_input_delivers_stdin_to_managed_command_task() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "wait for stdin".into(),
            holon::types::CommandTaskSpec {
                cmd: "IFS= read -r line; printf \"heard:%s\" \"$line\"".into(),
                workdir: None,
                shell: Some("sh".into()),
                login: false,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: true,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let (input_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-input-command".into(),
                name: "TaskInput".into(),
                input: json!({ "task_id": task.id, "input": "hello\n" }),
            },
        )
        .await?;
    let input_value: serde_json::Value = parse_tool_result_payload(&input_result)?;
    assert_eq!(input_value["accepted_input"], true);
    assert_eq!(input_value["input_target"], "stdin");
    assert_eq!(input_value["bytes_written"], 6);
    assert_eq!(input_value["task"]["kind"], "command_task");

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-after-input".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["output_preview"], "heard:hello");
    Ok(())
}

pub async fn task_output_times_out_for_long_running_task() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "sleep for timeout".into(),
            holon::types::CommandTaskSpec {
                cmd: "sleep 5".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-timeout".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id, "timeout_ms": 50 }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "timeout");
    assert_eq!(value["task"]["kind"], "command_task");
    assert!(
        matches!(value["task"]["status"].as_str(), Some("queued" | "running")),
        "expected active command task status, got {:?}",
        value["task"]["status"]
    );

    runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await?;
    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Cancelled
        }))
    })
    .await?;
    Ok(())
}

pub async fn task_output_prefers_terminal_task_record_over_stale_task_message() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "complete before stale message".into(),
            CommandTaskSpec {
                cmd: "printf done".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let stale_running = MessageEnvelope {
        metadata: Some(json!({
            "task_id": task.id,
            "task_kind": "command_task",
            "task_status": "running",
            "task_summary": "stale running status",
        })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            TrustLevel::TrustedSystem,
            Priority::Background,
            MessageBody::Text {
                text: "stale running message".into(),
            },
        )
    };
    runtime.storage().append_message(&stale_running)?;

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-stale-message".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id, "timeout_ms": 10 }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["status"], "completed");
    Ok(())
}

pub async fn task_output_accepts_terminal_command_snapshot_without_explicit_readiness_flag(
) -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_command_task(
            "legacy terminal detail".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf before_fail && exit 7".into(),
                workdir: None,
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id
                && record.status == holon::types::TaskStatus::Failed
                && record
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("terminal_snapshot_ready"))
                    .and_then(|value| value.as_bool())
                    == Some(true)
        }))
    })
    .await?;

    let mut terminal = runtime.task_record(&task.id).await?.unwrap();
    if let Some(detail) = terminal
        .detail
        .as_mut()
        .and_then(|value| value.as_object_mut())
    {
        detail.remove("terminal_snapshot_ready");
    }
    terminal.updated_at = Utc::now();
    runtime.storage().append_task(&terminal)?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(
        output.retrieval_status,
        holon::types::TaskOutputRetrievalStatus::Success
    );
    assert_eq!(output.task.status, holon::types::TaskStatus::Failed);
    assert_eq!(output.task.exit_status, Some(7));
    assert_eq!(output.task.output_preview, "before_fail");
    Ok(())
}

pub async fn task_output_accepts_terminal_command_without_snapshot_fields() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_command_task(
            "legacy terminal fields".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf before_fail && exit 7".into(),
                workdir: None,
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id
                && record.status == holon::types::TaskStatus::Failed
                && record
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("terminal_snapshot_ready"))
                    .and_then(|value| value.as_bool())
                    == Some(true)
        }))
    })
    .await?;

    let mut terminal = runtime.task_record(&task.id).await?.unwrap();
    if let Some(detail) = terminal
        .detail
        .as_mut()
        .and_then(|value| value.as_object_mut())
    {
        detail.remove("terminal_snapshot_ready");
        detail.remove("exit_status");
        detail.remove("output_summary");
        detail.remove("initial_output");
    }
    terminal.updated_at = Utc::now();
    runtime.storage().append_task(&terminal)?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(
        output.retrieval_status,
        holon::types::TaskOutputRetrievalStatus::Success
    );
    assert_eq!(output.task.status, holon::types::TaskStatus::Failed);
    assert_eq!(output.task.output_preview, "before_fail");
    Ok(())
}

pub async fn task_output_rejects_message_only_terminal_status_for_running_command() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_command_task(
            "wait for terminal persistence".into(),
            holon::types::CommandTaskSpec {
                cmd: "sleep 10".into(),
                workdir: None,
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Running
        }))
    })
    .await?;

    let message_only_terminal = MessageEnvelope {
        metadata: Some(json!({
            "task_id": task.id,
            "task_kind": "command_task",
            "task_status": "completed",
            "task_summary": "message-only terminal status",
        })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            TrustLevel::TrustedSystem,
            Priority::Next,
            MessageBody::Text {
                text: "synthetic terminal result".into(),
            },
        )
    };
    runtime.storage().append_message(&message_only_terminal)?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(
        output.retrieval_status,
        holon::types::TaskOutputRetrievalStatus::NotReady
    );
    assert_eq!(output.task.status, holon::types::TaskStatus::Completed);

    runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await?;
    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Cancelled
        }))
    })
    .await?;
    Ok(())
}

pub async fn command_task_stop_cancels_running_command() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "sleep for a while".into(),
            holon::types::CommandTaskSpec {
                cmd: "sleep 10".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Cancelled
        }))
    })
    .await?;

    let stored = runtime.task_record(&task.id).await?.unwrap();
    let detail = stored.detail.unwrap_or_default();
    assert_eq!(detail["cmd"], "sleep 10");

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(
        output.retrieval_status,
        holon::types::TaskOutputRetrievalStatus::Success
    );
    assert_eq!(output.task.status, holon::types::TaskStatus::Cancelled);
    assert_eq!(output.task.kind, "command_task");

    let (tool_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-cancelled-command".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": &task.id }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&tool_result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["status"], "cancelled");
    assert_eq!(value["task"]["kind"], "command_task");

    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| {
        event.kind == "task_result_received"
            && event.data.get("id").and_then(|value| value.as_str()) == Some(task.id.as_str())
            && event.data.get("status").and_then(|value| value.as_str()) == Some("cancelled")
    }));

    let state = runtime.agent_state().await?;
    assert!(!state.active_task_ids.contains(&task.id));
    Ok(())
}

pub async fn background_command_task_persists_terminal_state_while_runtime_paused() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    runtime.control(ControlAction::Pause).await?;

    let task = runtime
        .schedule_command_task(
            "complete while paused".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf paused_ok".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(
        output.retrieval_status,
        holon::types::TaskOutputRetrievalStatus::Success
    );
    assert_eq!(output.task.status, holon::types::TaskStatus::Completed);
    assert_eq!(output.task.output_preview, "paused_ok");
    let stored = runtime.task_record(&task.id).await?.unwrap();
    assert_eq!(
        stored
            .detail
            .as_ref()
            .and_then(|detail| detail.get("terminal_snapshot_ready"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );

    let state = runtime.agent_state().await?;
    assert!(!state.active_task_ids.contains(&task.id));
    assert_eq!(state.status, AgentStatus::Paused);
    Ok(())
}

pub async fn command_task_result_is_canonical_follow_up_on_completion() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_command_task(
            "complete then continue".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf continue_ok".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: true,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(20)?;
        let agent = runtime.storage().read_agent()?.expect("agent should exist");
        Ok(messages.iter().any(|message| {
            message.kind == MessageKind::TaskResult
                && message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("task_id"))
                    .and_then(|value| value.as_str())
                    == Some(task.id.as_str())
        }) && agent
            .last_continuation
            .as_ref()
            .is_some_and(|continuation| {
                continuation.model_visible
                    && continuation.trigger_kind
                        == holon::types::ContinuationTriggerKind::TaskResult
            }))
    })
    .await?;

    Ok(())
}

pub async fn blocking_command_task_sets_awaiting_task_closure() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_command_task(
            "wait for command".into(),
            holon::types::CommandTaskSpec {
                cmd: "sleep 5".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(64),
                accepts_input: false,
                continue_on_result: true,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let summary = runtime.agent_summary().await?;
    assert_eq!(summary.agent.status, AgentStatus::AwaitingTask);
    assert_eq!(summary.closure.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        summary.closure.waiting_reason,
        Some(WaitingReason::AwaitingTaskResult)
    );

    runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Cancelled
        }))
    })
    .await?;

    Ok(())
}

pub async fn command_task_runner_failure_marks_task_failed_and_cleans_up() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_command_task(
            "fail during task output setup".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf should_not_hang".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;
    let output_path = runtime
        .storage()
        .data_dir()
        .join("task-output")
        .join(format!("{}.log", task.id));
    std::fs::create_dir_all(&output_path)?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Failed
        }))
    })
    .await?;

    let stored = runtime.task_record(&task.id).await?.unwrap();
    let detail = stored.detail.unwrap_or_default();
    assert_eq!(detail["cmd"], "printf should_not_hang");
    assert!(detail["error"].as_str().is_some());
    assert!(!runtime
        .agent_state()
        .await?
        .active_task_ids
        .contains(&task.id));

    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| {
        event.kind == "task_result_received"
            && event.data.get("id").and_then(|value| value.as_str()) == Some(task.id.as_str())
            && event.data.get("status").and_then(|value| value.as_str()) == Some("failed")
    }));
    Ok(())
}

pub async fn command_task_nonzero_exit_produces_failed_output_and_runtime_state() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_command_task(
            "exit nonzero".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf before_fail && exit 7".into(),
                workdir: None,
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(256),
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    let output = runtime.task_output(&task.id, true, 15_000).await?;
    assert_eq!(
        output.retrieval_status,
        holon::types::TaskOutputRetrievalStatus::Success
    );
    assert_eq!(output.task.status, holon::types::TaskStatus::Failed);
    assert_eq!(output.task.kind, "command_task");
    assert_eq!(output.task.output_preview, "before_fail");
    assert_eq!(output.task.exit_status, Some(7));
    let task_artifact = output
        .task
        .failure_artifact
        .as_ref()
        .expect("failed command task should expose normalized task artifact");
    assert_eq!(task_artifact.category, FailureArtifactCategory::Task);
    assert_eq!(task_artifact.kind, "command_task_exit_nonzero");
    assert_eq!(task_artifact.exit_status, Some(7));
    assert_eq!(task_artifact.task_id.as_deref(), Some(task.id.as_str()));
    assert_eq!(task_artifact.summary, "command task exited with status 7");
    assert!(!task_artifact.summary.contains("before_fail"));
    assert_eq!(
        task_artifact
            .metadata
            .get("error_present")
            .map(String::as_str),
        None
    );
    let stored = runtime.task_record(&task.id).await?.unwrap();
    assert_eq!(
        stored
            .detail
            .as_ref()
            .and_then(|detail| detail.get("terminal_snapshot_ready"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert!(output
        .task
        .result_summary
        .as_deref()
        .is_some_and(|summary| summary.contains("before_fail")));

    let (tool_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-failed-command".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": &task.id }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&tool_result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["status"], "failed");
    assert_eq!(value["task"]["kind"], "command_task");
    assert_eq!(value["task"]["output_preview"], "before_fail");
    assert_eq!(value["task"]["exit_status"], 7);
    assert!(value["task"]["failure_artifact"].is_object());

    let state = runtime.agent_state().await?;
    assert!(!state.active_task_ids.contains(&task.id));

    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| {
        event.kind == "task_result_received"
            && event.data.get("id").and_then(|value| value.as_str()) == Some(task.id.as_str())
            && event.data.get("status").and_then(|value| value.as_str()) == Some("failed")
    }));
    Ok(())
}

pub async fn exec_command_auto_promotes_long_running_command_task() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(LongShellProvider::new()))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "run a long command".into(),
            },
        ))
        .await?;

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains("auto promotion observed")))
    })
    .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.kind.as_str() == "command_task"
                && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let command_task = runtime
        .storage()
        .latest_task_records()?
        .into_iter()
        .find(|task| task.kind.as_str() == "command_task")
        .expect("auto-promoted command task should exist");
    let detail = command_task.detail.unwrap_or_default();
    assert_eq!(detail["promoted_from_exec_command"], true);
    assert_eq!(detail["cmd"], "printf start && sleep 1 && printf done");
    let messages = runtime.storage().read_recent_messages(20)?;
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::TaskResult
            && matches!(&message.body, MessageBody::Text { text } if text.contains("exit_status: 0"))
    }));
    Ok(())
}
