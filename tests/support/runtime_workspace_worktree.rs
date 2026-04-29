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
use crate::support::{attach_default_workspace, TestConfigBuilder};

// ============================================================================
// Runtime workspace and worktree domain test support
pub async fn task_output_returns_worktree_subagent_result_text() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let provider = Arc::new(WorktreeCapturingProvider::new("worktree subagent result"));
    let host = RuntimeHost::new_with_provider(config, provider)?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_child_agent_task(
            "delegate worktree task".into(),
            "return a worktree result".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-worktree".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id, "timeout_ms": 10_000 }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["kind"], "child_agent_task");
    assert!(value["task"]["output_preview"]
        .as_str()
        .expect("worktree task output should be text")
        .contains("worktree subagent result"));
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

pub async fn enter_worktree_tool_switches_workspace_and_restores_on_reload() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    let original_branch = init_git_repo(&workspace)?;
    let branch_name = "feature-enter-worktree";

    let host = RuntimeHost::new_with_provider(
        config.clone(),
        Arc::new(UseWorkspaceProvider::new(workspace.clone(), branch_name)),
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
                text: "enter a managed worktree".into(),
            },
        ))
        .await?;

    for _ in 0..30 {
        if runtime.agent_state().await?.worktree_session.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let session = runtime.agent_state().await?;
    let worktree = session
        .worktree_session
        .clone()
        .expect("missing worktree state");
    assert_eq!(worktree.original_cwd, workspace);
    assert_eq!(worktree.original_branch, original_branch);
    assert_eq!(worktree.worktree_branch, branch_name);
    assert!(worktree.worktree_path.exists());
    assert_eq!(runtime.workspace_root(), worktree.worktree_path);
    assert_eq!(
        git(
            &runtime.workspace_root(),
            &["rev-parse", "--abbrev-ref", "HEAD"]
        )?,
        branch_name
    );

    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| event.kind == "workspace_entered"));

    let restarted_host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let restarted_runtime = restarted_host.default_runtime().await?;
    assert_eq!(restarted_runtime.workspace_root(), worktree.worktree_path);
    Ok(())
}

pub async fn enter_workspace_conflict_preserves_existing_occupancy() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let workspace_entry = host.ensure_workspace_entry(workspace.clone())?;

    let default_runtime = host.default_runtime().await?;
    default_runtime.attach_workspace(&workspace_entry).await?;
    default_runtime
        .enter_workspace(
            &workspace_entry,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            Some(workspace.clone()),
            None,
        )
        .await?;

    host.create_named_agent("alpha", None).await?;
    let alpha = host.get_or_create_agent("alpha").await?;
    alpha.attach_workspace(&workspace_entry).await?;
    alpha
        .enter_workspace(
            &workspace_entry,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            Some(workspace.clone()),
            None,
        )
        .await?;

    let error = default_runtime
        .enter_workspace(
            &workspace_entry,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            Some(workspace.clone()),
            None,
        )
        .await
        .expect_err("exclusive_write conflict should fail");
    assert!(error
        .to_string()
        .contains("already has an exclusive_write holder"));

    let state = default_runtime.agent_state().await?;
    let active_entry = state
        .active_workspace_entry
        .expect("shared_read entry should still be active");
    assert_eq!(active_entry.access_mode, WorkspaceAccessMode::SharedRead);

    let summary = default_runtime.agent_summary().await?;
    let occupancy = summary
        .active_workspace_occupancy
        .expect("shared_read occupancy should still be held");
    assert_eq!(occupancy.access_mode, WorkspaceAccessMode::SharedRead);
    Ok(())
}

pub async fn detach_workspace_persists_empty_binding_across_restart() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host =
        RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("unused")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let workspace_id = runtime
        .agent_state()
        .await?
        .active_workspace_entry
        .as_ref()
        .map(|e| e.workspace_id.clone())
        .expect("default workspace should be active");

    runtime.exit_workspace().await?;
    runtime.detach_workspace(&workspace_id).await?;
    let state = runtime.agent_state().await?;
    assert_eq!(
        state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.as_str()),
        Some("agent_home")
    );
    assert!(state.active_workspace_entry.is_some());
    assert_eq!(state.attached_workspaces, vec!["agent_home".to_string()]);

    let restarted_host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let restarted_runtime = restarted_host.default_runtime().await?;
    let restarted_state = restarted_runtime.agent_state().await?;
    assert_eq!(
        restarted_state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
            .as_deref(),
        Some("agent_home")
    );
    assert!(restarted_state.active_workspace_entry.is_some());
    Ok(())
}

pub async fn enter_worktree_projection_honors_requested_cwd() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let workspace_entry = host.ensure_workspace_entry(workspace.clone())?;

    runtime
        .enter_workspace(
            &workspace_entry,
            WorkspaceProjectionKind::GitWorktreeRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            Some(Path::new("nested/src").to_path_buf()),
            Some("feature-worktree-cwd".into()),
        )
        .await?;

    let state = runtime.agent_state().await?;
    let worktree = state
        .worktree_session
        .clone()
        .expect("missing worktree session");
    let active_entry = state
        .active_workspace_entry
        .expect("missing active workspace entry");
    assert_eq!(active_entry.cwd, worktree.worktree_path.join("nested/src"));

    runtime.exit_workspace().await?;
    assert!(worktree.worktree_path.exists());
    Ok(())
}

pub async fn exit_worktree_keep_restores_workspace_and_persists_state() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;
    let branch_name = "feature-exit-keep";

    let host = RuntimeHost::new_with_provider(
        config.clone(),
        Arc::new(WorktreeLifecycleProvider::new(
            workspace.clone(),
            branch_name,
            "\"workspace_id\":\"agent_home\"",
        )),
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
                text: "enter a managed worktree".into(),
            },
        ))
        .await?;
    wait_for_worktree_presence(&runtime, true).await?;

    let worktree = runtime
        .agent_state()
        .await?
        .worktree_session
        .clone()
        .expect("missing worktree state");

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "exit the worktree but keep it".into(),
            },
        ))
        .await?;
    wait_for_worktree_presence(&runtime, false).await?;

    assert_eq!(
        runtime
            .agent_state()
            .await?
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
            .as_deref(),
        Some("agent_home")
    );
    assert!(worktree.worktree_path.exists());

    let restarted_host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let restarted_runtime = restarted_host.default_runtime().await?;
    let restarted_state = restarted_runtime.agent_state().await?;
    assert_eq!(
        restarted_state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
            .as_deref(),
        Some("agent_home")
    );
    assert!(restarted_state.worktree_session.is_none());
    Ok(())
}

pub async fn exit_worktree_does_not_remove_clean_worktree() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;
    let branch_name = "feature-exit-remove";

    let host = RuntimeHost::new_with_provider(
        config.clone(),
        Arc::new(WorktreeLifecycleProvider::new(
            workspace.clone(),
            branch_name,
            "\"workspace_id\":\"agent_home\"",
        )),
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
                text: "enter a managed worktree".into(),
            },
        ))
        .await?;
    wait_for_worktree_presence(&runtime, true).await?;

    let worktree = runtime
        .agent_state()
        .await?
        .worktree_session
        .clone()
        .expect("missing worktree state");

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "exit the worktree".into(),
            },
        ))
        .await?;
    wait_for_worktree_presence(&runtime, false).await?;

    assert_eq!(
        runtime
            .agent_state()
            .await?
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
            .as_deref(),
        Some("agent_home")
    );
    assert!(worktree.worktree_path.exists());

    let restarted_host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let restarted_runtime = restarted_host.default_runtime().await?;
    let restarted_state = restarted_runtime.agent_state().await?;
    assert_eq!(
        restarted_state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
            .as_deref(),
        Some("agent_home")
    );
    assert!(restarted_state.worktree_session.is_none());
    Ok(())
}

pub async fn exit_worktree_does_not_remove_dirty_worktree() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;
    let branch_name = "feature-exit-refuse";

    let host = RuntimeHost::new_with_provider(
        config.clone(),
        Arc::new(WorktreeLifecycleProvider::new(
            workspace.clone(),
            branch_name,
            "\"workspace_id\":\"agent_home\"",
        )),
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
                text: "enter a managed worktree".into(),
            },
        ))
        .await?;
    wait_for_worktree_presence(&runtime, true).await?;

    let worktree = runtime
        .agent_state()
        .await?
        .worktree_session
        .clone()
        .expect("missing worktree state");
    std::fs::write(worktree.worktree_path.join("README.md"), "changed\n")?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "exit the dirty worktree".into(),
            },
        ))
        .await?;
    wait_for_worktree_presence(&runtime, false).await?;

    let session = runtime.agent_state().await?;
    assert!(session.worktree_session.is_none());
    assert_eq!(
        session
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
            .as_deref(),
        Some("agent_home")
    );
    assert!(session.active_workspace_entry.is_some());
    assert!(worktree.worktree_path.exists());

    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| event.kind == "workspace_used"));

    let restarted_host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let restarted_runtime = restarted_host.default_runtime().await?;
    let restarted_state = restarted_runtime.agent_state().await?;
    assert_eq!(
        restarted_state
            .active_workspace_entry
            .as_ref()
            .map(|e| e.workspace_id.clone())
            .as_deref(),
        Some("agent_home")
    );
    assert!(restarted_state.worktree_session.is_none());
    Ok(())
}

pub async fn worktree_subagent_task_creates_dedicated_per_task_worktree() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let provider = Arc::new(WorktreeCapturingProvider::new("worktree subagent result"));
    let host = RuntimeHost::new_with_provider(config.clone(), provider.clone())?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate work in worktree".into(),
            "return a worktree-isolated result".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(20)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains("worktree subagent result")))
    })
    .await?;

    let expected_worktree = workspace
        .parent()
        .unwrap_or(workspace.as_path())
        .join(format!(
            ".holon-worktrees-{}/task-{}",
            workspace
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo"),
            task.id
        ));

    // WT-104: Worktree is auto-removed when no changes are made, but we can verify it was created
    let events = runtime.recent_events(50).await?;
    assert!(
        events
            .iter()
            .any(|event| event.kind == "worktree_created_for_task"
                && event.data["task_id"] == task.id
                && event.data["worktree_path"].as_str()
                    == Some(expected_worktree.to_str().unwrap())),
        "worktree should have been created during task execution"
    );

    assert_eq!(runtime.workspace_root(), workspace);

    // Verify the subagent ran in the worktree by checking transcript
    let transcript = runtime.storage().read_all_transcript()?;
    assert!(transcript.iter().any(|entry| {
        entry.kind == holon::types::TranscriptEntryKind::SubagentPrompt
            && entry.data["task_id"] == task.id
            && entry.data["workspace_root"]
                .as_str()
                .map(|path| path.contains(&task.id))
                .unwrap_or(false)
    }));

    let prompts = provider.prompts().await;
    let expected_worktree_text = expected_worktree.to_string_lossy().to_string();
    assert!(
        prompts
            .iter()
            .any(|prompt| prompt.contains(expected_worktree_text.as_str())),
        "subagent prompt should be rooted in the dedicated worktree"
    );

    // Verify auto-cleanup occurred (WT-104)
    assert!(
        !expected_worktree.exists(),
        "worktree should be auto-removed when no changes were made (WT-104)"
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "worktree_auto_cleaned_up"
                && event.data["task_id"] == task.id
                && event.data["reason"].as_str() == Some("terminal_task_result")),
        "worktree should have been auto-cleaned up after task completion (WT-104)"
    );

    Ok(())
}

pub async fn subagent_task_returns_result_to_parent_session() -> Result<()> {
    let host = RuntimeHost::new_with_provider(
        test_config(),
        Arc::new(StubProvider::new("subagent result payload")),
    )?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate work".into(),
            "return a concise subagent result".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;
    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(20)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains("subagent result payload")))
    })
    .await?;

    let tasks = runtime.recent_tasks(10).await?;
    assert!(tasks
        .iter()
        .any(|record| record.id == task.id && record.kind.as_str() == "child_agent_task"));
    let briefs = runtime.recent_briefs(10).await?;
    assert!(briefs
        .iter()
        .any(|brief| brief.text.contains("subagent result payload")));
    Ok(())
}

pub async fn worktree_child_agent_task_records_workspace_mode() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(
        config,
        Arc::new(StubProvider::new("worktree subagent result")),
    )?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate work in worktree".into(),
            "return a worktree-isolated result".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;
    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(20)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains("worktree subagent result")))
    })
    .await?;

    let tasks = runtime.recent_tasks(10).await?;
    let record = tasks
        .iter()
        .find(|record| record.id == task.id)
        .expect("worktree child task record");
    assert_eq!(record.kind.as_str(), "child_agent_task");
    assert_eq!(
        record
            .detail
            .as_ref()
            .and_then(|detail| detail.get("workspace_mode"))
            .and_then(|value| value.as_str()),
        Some("worktree")
    );
    let messages = runtime.storage().read_recent_messages(20)?;
    let running_status = messages
        .iter()
        .find(|message| {
            matches!(message.kind, MessageKind::TaskStatus)
                && message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("task_id"))
                    .and_then(|value| value.as_str())
                    == Some(task.id.as_str())
        })
        .expect("worktree child task should emit a running task status message");
    let running_detail = running_status
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_detail"))
        .expect("running task status should carry task detail");
    assert_eq!(
        running_detail
            .get("workspace_mode")
            .and_then(|value| value.as_str()),
        Some("worktree")
    );
    assert!(
        running_detail.get("worktree").is_some(),
        "running task status should preserve task-owned worktree metadata"
    );
    let briefs = runtime.recent_briefs(10).await?;
    assert!(briefs
        .iter()
        .any(|brief| brief.text.contains("worktree subagent result")));
    Ok(())
}

pub async fn worktree_subagent_task_returns_metadata_to_parent_session() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(
        config,
        Arc::new(StubProvider::new("worktree metadata result")),
    )?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate work in worktree".into(),
            "return worktree metadata".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(20)?;
        Ok(briefs.iter().any(|brief| {
            brief.text.contains("Worktree path:")
                && brief.text.contains("Worktree branch:")
                && brief.text.contains("Changed files:")
        }))
    })
    .await?;

    let messages = runtime.storage().read_recent_messages(20)?;
    let task_result = messages
        .iter()
        .find(|message| matches!(message.kind, MessageKind::TaskResult))
        .expect("missing task result message");
    let worktree = task_result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("worktree"))
        .expect("missing worktree metadata");

    let expected_branch = format!("task-{}", task.id);
    assert_eq!(
        worktree["worktree_branch"].as_str(),
        Some(expected_branch.as_str())
    );
    assert!(worktree["worktree_path"]
        .as_str()
        .map(|path| path.contains(&task.id))
        .unwrap_or(false));
    assert_eq!(
        worktree["changed_files"]
            .as_array()
            .expect("changed_files should be an array")
            .len(),
        0
    );

    let briefs = runtime.recent_briefs(10).await?;
    assert!(briefs.iter().any(|brief| {
        brief.text.contains("Task")
            && brief.text.contains("Worktree path:")
            && brief.text.contains("Worktree branch:")
            && brief.text.contains("Changed files: none")
    }));

    Ok(())
}
pub fn policy_blocks_mismatched_origin() {
    let mismatch = validate_message_kind_for_origin(
        &MessageKind::WebhookEvent,
        &MessageOrigin::Operator { actor_id: None },
    );
    assert!(!mismatch.allowed);
}

pub async fn worktree_subagent_task_auto_removes_worktree_when_no_changes_wt104() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    // Provider that does nothing (no changes)
    let host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("no changes made")))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "do nothing in worktree".into(),
            "just return a result without making changes".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    // Wait for task to complete
    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let expected_worktree = workspace
        .parent()
        .unwrap_or(workspace.as_path())
        .join(format!(
            ".holon-worktrees-{}/task-{}",
            workspace
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo"),
            task.id
        ));

    // Verify the worktree was automatically removed (WT-104)
    assert!(
        !expected_worktree.exists(),
        "worktree should be auto-removed when no changes were made"
    );

    // Verify the cleanup event was logged
    let events = runtime.recent_events(50).await?;
    assert!(
        events
            .iter()
            .any(|event| event.kind == "worktree_auto_cleaned_up"),
        "should have logged worktree_auto_cleaned_up event"
    );

    let cleanup_event = events
        .iter()
        .find(|event| event.kind == "worktree_auto_cleaned_up")
        .expect("cleanup event should exist");

    assert_eq!(cleanup_event.data["task_id"], task.id);
    assert_eq!(
        cleanup_event.data["reason"].as_str(),
        Some("terminal_task_result")
    );

    // Verify task result still contains worktree metadata
    let messages = runtime.storage().read_recent_messages(20)?;
    let task_result = messages
        .iter()
        .find(|message| {
            matches!(message.kind, MessageKind::TaskResult)
                && message
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("task_id"))
                    .and_then(|id| id.as_str())
                    == Some(&task.id[..])
        })
        .expect("missing task result message");

    let worktree_metadata = task_result
        .metadata
        .as_ref()
        .and_then(|m| m.get("worktree"))
        .expect("should still have worktree metadata even after cleanup");

    assert_eq!(
        worktree_metadata["changed_files"]
            .as_array()
            .expect("changed_files should be an array")
            .len(),
        0
    );
    assert_eq!(worktree_metadata["auto_cleaned_up"].as_bool(), Some(true));
    let task_result_text = match &task_result.body {
        MessageBody::Text { text } => text,
        other => panic!("expected text task result, got {other:?}"),
    };
    assert!(
        task_result_text.contains("Worktree cleanup: auto-removed clean task-owned artifact."),
        "task result should report cleanup status: {task_result_text}"
    );

    Ok(())
}

pub async fn worktree_subagent_task_retains_worktree_when_changes_detected_wt105() -> Result<()> {
    let config = test_config();
    let workspace = config.workspace_dir.clone();
    std::fs::create_dir_all(&workspace)?;
    init_git_repo(&workspace)?;

    let host = RuntimeHost::new_with_provider(config, Arc::new(DelayedTextProvider))?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "make changes in worktree".into(),
            "modify files in the worktree".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await?;

    let expected_worktree = workspace
        .parent()
        .unwrap_or(workspace.as_path())
        .join(format!(
            ".holon-worktrees-{}/task-{}",
            workspace
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo"),
            task.id
        ));

    wait_until(|| {
        let events = runtime.storage().read_recent_events(50)?;
        Ok(events.iter().any(|event| {
            event.kind == "worktree_created_for_task"
                && event.data["task_id"] == task.id
                && event.data["worktree_path"].as_str() == Some(expected_worktree.to_str().unwrap())
        }))
    })
    .await?;

    std::fs::write(
        expected_worktree.join("changed_file.txt"),
        "This file was changed in the worktree",
    )?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    // WT-105: Verify the worktree was retained (not auto-removed) when changes were made
    assert!(
        expected_worktree.exists(),
        "worktree should be retained when changes were made (WT-105)"
    );

    // Verify the retained event was logged
    let events = runtime.recent_events(50).await?;
    let retained_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "worktree_retained_for_review")
        .collect();

    assert!(
        !retained_events.is_empty(),
        "should have logged worktree_retained_for_review event when changes were detected"
    );

    let retained_event = retained_events
        .iter()
        .find(|event| event.data["task_id"] == task.id)
        .expect("retained event should exist for this task");

    assert_eq!(retained_event.data["task_id"], task.id);
    assert_eq!(
        retained_event.data["reason"].as_str(),
        Some("changes detected in worktree")
    );

    // Verify changed_files is in the event
    let changed_files = retained_event.data["changed_files"]
        .as_array()
        .expect("changed_files should be an array in retained event");
    assert!(
        !changed_files.is_empty(),
        "retained event should list changed files"
    );

    // Verify task result contains worktree metadata with retained_for_review flag
    let messages = runtime.storage().read_recent_messages(20)?;
    let task_result = messages
        .iter()
        .find(|message| {
            matches!(message.kind, MessageKind::TaskResult)
                && message
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("task_id"))
                    .and_then(|id| id.as_str())
                    == Some(&task.id[..])
        })
        .expect("missing task result message");

    let worktree_metadata = task_result
        .metadata
        .as_ref()
        .and_then(|m| m.get("worktree"))
        .expect("should have worktree metadata");

    assert_eq!(
        worktree_metadata["retained_for_review"].as_bool(),
        Some(true)
    );

    let changed_files_in_metadata = worktree_metadata["changed_files"]
        .as_array()
        .expect("changed_files should be in metadata");
    assert!(
        !changed_files_in_metadata.is_empty(),
        "metadata should list changed files"
    );

    let task_result_text = match &task_result.body {
        MessageBody::Text { text } => text,
        other => panic!("expected text task result, got {other:?}"),
    };

    assert!(
        task_result_text.contains("Worktree retained for review"),
        "task result should indicate worktree is retained: {task_result_text}"
    );
    assert!(
        task_result_text.contains("changes detected"),
        "task result should mention changes were detected: {task_result_text}"
    );

    // Verify the actual changed file exists in the worktree
    let changed_file_path = expected_worktree.join("changed_file.txt");
    assert!(
        changed_file_path.exists(),
        "changed file should exist in retained worktree"
    );

    let content = std::fs::read_to_string(&changed_file_path)?;
    assert!(
        content.contains("This file was changed in the worktree"),
        "changed file should have the expected content"
    );

    Ok(())
}
