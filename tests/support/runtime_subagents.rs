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
        TranscriptEntryKind, TrustLevel, WaitingIntentStatus, WaitingReason, WorkItemState,
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
    NotifyThenAgentGetProvider, RecordingPromptProvider, RuntimeFailureProvider, ShellProvider,
    SleepOnlyCompletionAfterTextProvider, TerminalResultBriefProvider, ToolErrorProvider,
    ToolUsingProvider, TruncatedShellReinjectionProvider, UseWorkspaceProvider,
    VerboseRuntimeFailureProvider, WakeHintProvider, WorktreeCapturingProvider,
    WorktreeLifecycleProvider,
};
use crate::support::runtime_compaction_providers::{
    MaxOutputRecoveryProvider, MaxOutputThenCompactionProvider, MultiPassCompactionRecoveryFlowProvider,
    RepeatedCompactionProvider,
};
use crate::support::{
    attach_default_workspace, eventually, eventually_async, eventually_for, TestConfigBuilder,
};

// ============================================================================
// Runtime subagents domain test support
pub async fn task_output_returns_subagent_result_text() -> Result<()> {
    let host = RuntimeHost::new_with_provider(
        test_config(),
        Arc::new(StubProvider::new("subagent final result")),
    )?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let task = runtime
        .schedule_child_agent_task(
            "delegate bounded work".into(),
            "return a bounded result".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;

    let (result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-task-output-subagent".into(),
                name: "TaskOutput".into(),
                input: json!({ "task_id": task.id }),
            },
        )
        .await?;
    let value: serde_json::Value = parse_tool_result_payload(&result)?;
    assert_eq!(value["retrieval_status"], "success");
    assert_eq!(value["task"]["kind"], "child_agent_task");
    assert!(value["task"]["output_preview"]
        .as_str()
        .expect("subagent output should be text")
        .contains("subagent final result"));
    Ok(())
}

pub async fn subagent_task_updates_parent_state_and_child_summary_during_lifecycle() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(DelegatedBoundaryProvider))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate slow child".into(),
            "slow-child".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;

    let state = runtime.agent_state().await?;
    assert_eq!(state.status, AgentStatus::AwaitingTask);
    assert!(state.active_task_ids.contains(&task.id));

    let mut saw_child_summary = false;
    for _ in 0..20 {
        let summary = runtime.agent_summary().await?;
        if let Some(child) = summary.active_children.iter().find(|child| {
            child.identity.delegated_from_task_id.as_deref() == Some(task.id.as_str())
        }) {
            if child.observability.last_progress_brief.is_some() {
                saw_child_summary = true;
                assert_eq!(child.identity.kind, AgentKind::Child);
                assert_eq!(child.identity.parent_agent_id.as_deref(), Some("default"));
                assert_eq!(child.observability.phase, ChildAgentPhase::Running);
                assert!(child.observability.last_result_brief.is_none());
                break;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        saw_child_summary,
        "expected child agent summary while delegated task runs"
    );

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(output.task.status, holon::types::TaskStatus::Completed);
    assert!(output.task.output_preview.contains("slow child result"));

    let final_summary = runtime.agent_summary().await?;
    assert!(matches!(
        final_summary.agent.status,
        AgentStatus::AwakeIdle | AgentStatus::Asleep
    ));
    assert!(!final_summary.agent.active_task_ids.contains(&task.id));
    assert!(final_summary.active_children.is_empty());
    Ok(())
}

pub async fn subagent_task_status_exposes_live_and_terminal_child_observability() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(DelegatedBoundaryProvider))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate slow child".into(),
            "slow-child".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;

    wait_until_async(|| {
        let runtime = runtime.clone();
        let task_id = task.id.clone();
        async move {
            let snapshot = runtime.task_status_snapshot(&task_id).await?;
            let Some(child) = snapshot.child_observability.as_ref() else {
                return Ok(false);
            };
            Ok(matches!(
                child.phase,
                ChildAgentPhase::Running | ChildAgentPhase::Waiting
            ) && child.last_progress_brief.is_some()
                && snapshot.child_agent_id.is_some())
        }
    })
    .await?;

    let running_snapshot = runtime.task_status_snapshot(&task.id).await?;

    assert_eq!(running_snapshot.status, TaskStatus::Running);
    let live_child = running_snapshot
        .child_observability
        .as_ref()
        .expect("live child observability should be present");
    assert!(matches!(
        live_child.phase,
        ChildAgentPhase::Running | ChildAgentPhase::Waiting
    ));

    wait_until_async(|| {
        let runtime = runtime.clone();
        let task_id = task.id.clone();
        async move { Ok(runtime.task_status_snapshot(&task_id).await?.status == TaskStatus::Completed) }
    })
    .await?;

    let terminal_snapshot = runtime.task_status_snapshot(&task.id).await?;
    assert_eq!(terminal_snapshot.status, TaskStatus::Completed);
    assert_eq!(
        terminal_snapshot
            .child_observability
            .as_ref()
            .map(|child| &child.phase),
        Some(&ChildAgentPhase::Terminal)
    );
    assert!(terminal_snapshot
        .child_observability
        .as_ref()
        .and_then(|child| child.last_result_brief.as_deref())
        .is_some_and(|brief| brief.contains("slow child result")));
    Ok(())
}

pub async fn blocking_subagent_result_does_not_regress_to_running_task_status() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(DelegatedBoundaryProvider))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate slow child".into(),
            "slow-child".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks
            .iter()
            .any(|record| record.id == task.id && record.status == TaskStatus::Completed))
    })
    .await?;

    let stale_running = MessageEnvelope {
        metadata: Some(json!({
            "task_id": task.id,
            "task_kind": "child_agent_task",
            "task_status": "running",
            "task_summary": "stale task status",
        })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            TrustLevel::TrustedOperator,
            Priority::Background,
            MessageBody::Text {
                text: "stale running update".into(),
            },
        )
    };
    runtime.enqueue(stale_running).await?;

    wait_until(|| {
        let state = runtime.storage().read_agent()?;
        let tasks = runtime.storage().latest_task_records()?;
        let latest = tasks
            .into_iter()
            .find(|task_record| task_record.id == task.id);
        let is_terminal = latest.is_some_and(|record| {
            matches!(
                record.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            )
        });
        if let Some(state) = state {
            Ok(!state.active_task_ids.contains(&task.id)
                && state.status != AgentStatus::AwakeRunning
                && state.status != AgentStatus::AwaitingTask
                && state.current_run_id.is_none()
                && is_terminal)
        } else {
            Ok(false)
        }
    })
    .await?;

    Ok(())
}

pub async fn subagent_task_failure_propagates_failed_output_to_parent() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(DelegatedBoundaryProvider))?;
    let runtime = host.default_runtime().await?;

    let task = runtime
        .schedule_child_agent_task(
            "delegate failing child".into(),
            "fail-child".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.status == holon::types::TaskStatus::Failed
        }))
    })
    .await?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(output.task.status, holon::types::TaskStatus::Failed);
    assert!(
        output
            .task
            .output_preview
            .contains("child execution exploded")
            || output.task.output_preview.contains("child agent failed"),
        "unexpected delegated failure output: {}",
        output.task.output_preview
    );

    let events = runtime.recent_events(20).await?;
    assert!(events.iter().any(|event| {
        event.kind == "task_result_received"
            && event.data.get("id").and_then(|value| value.as_str()) == Some(task.id.as_str())
            && event.data.get("status").and_then(|value| value.as_str()) == Some("failed")
    }));
    Ok(())
}

pub async fn multiple_subagent_tasks_do_not_cross_contaminate_outputs() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(DelegatedBoundaryProvider))?;
    let runtime = host.default_runtime().await?;

    let alpha = runtime
        .schedule_child_agent_task(
            "delegate alpha".into(),
            "alpha-child".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;
    let beta = runtime
        .schedule_child_agent_task(
            "delegate beta".into(),
            "beta-child".into(),
            TrustLevel::TrustedOperator,
            holon::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await?;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        let alpha_done = tasks.iter().any(|record| {
            record.id == alpha.id && record.status == holon::types::TaskStatus::Completed
        });
        let beta_done = tasks.iter().any(|record| {
            record.id == beta.id && record.status == holon::types::TaskStatus::Completed
        });
        Ok(alpha_done && beta_done)
    })
    .await?;

    let alpha_output = runtime.task_output(&alpha.id, false, 0).await?;
    let beta_output = runtime.task_output(&beta.id, false, 0).await?;

    assert!(alpha_output
        .task
        .output_preview
        .contains("alpha child result"));
    assert!(!alpha_output
        .task
        .output_preview
        .contains("beta child result"));
    assert!(beta_output
        .task
        .output_preview
        .contains("beta child result"));
    assert!(!beta_output
        .task
        .output_preview
        .contains("alpha child result"));
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
