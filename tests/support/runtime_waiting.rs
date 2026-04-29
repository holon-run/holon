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
// Runtime waiting and reactivation domain test support
pub async fn message_processing_creates_briefs_and_sleeps() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("stub result")))?;
    let runtime = host.default_runtime().await?;

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "hello".into(),
        },
    );
    runtime.enqueue(message).await?;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let briefs = runtime.recent_briefs(10).await?;
    assert_eq!(briefs.len(), 2);
    assert_eq!(briefs[0].text, "Queued work: hello");
    assert_eq!(briefs[1].text, "stub result");

    let session = runtime.agent_state().await?;
    assert_eq!(session.status, AgentStatus::Asleep);
    Ok(())
}

pub async fn terminal_brief_uses_last_assistant_message_without_terminal_delivery_round(
) -> Result<()> {
    let host = RuntimeHost::new_with_provider(
        test_config(),
        Arc::new(TerminalResultBriefProvider::new()),
    )?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "write and verify a file".into(),
        },
    );
    runtime.enqueue(message.clone()).await?;

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs.iter().any(|brief| {
            brief.related_message_id.as_deref() == Some(message.id.as_str())
                && brief
                    .text
                    .contains("Verification is complete. I'll package the final answer now.")
        }))
    })
    .await?;

    let briefs = runtime.recent_briefs(10).await?;
    assert_eq!(briefs.len(), 2);
    assert_eq!(briefs[0].text, "Queued work: write and verify a file");
    assert_eq!(
        briefs[1].text,
        "Verification is complete. I'll package the final answer now."
    );
    assert!(
        !briefs[1]
            .text
            .contains("Let me create a summary document of what was changed."),
        "persisted result brief should come from the terminal turn, not a tool-round preamble: {}",
        briefs[1].text
    );
    let events = runtime.recent_events(20).await?;
    let terminal_event = events
        .iter()
        .find(|event| event.kind == "turn_terminal")
        .expect("turn terminal event should be recorded");
    assert_eq!(
        terminal_event
            .data
            .get("kind")
            .and_then(|value| value.as_str()),
        Some("completed")
    );
    assert_eq!(
        terminal_event
            .data
            .get("last_assistant_message")
            .and_then(|value| value.as_str()),
        Some("Verification is complete. I'll package the final answer now.")
    );
    assert!(
        events
            .iter()
            .all(|event| event.kind != "terminal_delivery_round_completed"),
        "terminal-delivery round events should no longer be emitted"
    );
    Ok(())
}

pub async fn sleep_only_completion_keeps_last_assistant_message_from_previous_round() -> Result<()>
{
    let host = RuntimeHost::new_with_provider(
        test_config(),
        Arc::new(SleepOnlyCompletionAfterTextProvider::new()),
    )?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "write a file and then sleep".into(),
        },
    );
    runtime.enqueue(message.clone()).await?;

    wait_until(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs.iter().any(|brief| {
            brief.related_message_id.as_deref() == Some(message.id.as_str())
                && brief
                    .text
                    .contains("Updated notes/result.txt and verified the requested change.")
        }))
    })
    .await?;

    let briefs = runtime.recent_briefs(10).await?;
    let result_brief = briefs
        .iter()
        .find(|brief| {
            brief.related_message_id.as_deref() == Some(message.id.as_str())
                && brief.kind == BriefKind::Result
        })
        .expect("result brief should exist");
    assert_eq!(
        result_brief.text,
        "Updated notes/result.txt and verified the requested change."
    );

    let events = runtime.recent_events(20).await?;
    let terminal_event = events
        .iter()
        .find(|event| event.kind == "turn_terminal")
        .expect("turn terminal event should be recorded");
    assert_eq!(
        terminal_event
            .data
            .get("last_assistant_message")
            .and_then(|value| value.as_str()),
        Some("Updated notes/result.txt and verified the requested change.")
    );

    Ok(())
}

pub async fn update_work_item_creates_and_updates_persisted_snapshot() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let created = runtime
        .update_work_item(
            None,
            "Ship work-item runtime foundation".into(),
            WorkItemStatus::Active,
            Some("bootstrapped from tool".into()),
            Some("persisted state landed".into()),
            None,
        )
        .await?;
    assert!(created.id.starts_with("work_"));

    let updated = runtime
        .update_work_item(
            Some(created.id.clone()),
            "Ship work-item runtime foundation".into(),
            WorkItemStatus::Waiting,
            Some("waiting on review".into()),
            Some("queued follow-up after CI".into()),
            Some("parent_1".into()),
        )
        .await?;

    let latest = runtime.latest_work_item(&created.id).await?.unwrap();
    assert_eq!(latest.id, created.id);
    assert_eq!(latest.status, WorkItemStatus::Waiting);
    assert_eq!(latest.summary.as_deref(), Some("waiting on review"));
    assert_eq!(
        latest.progress_note.as_deref(),
        Some("queued follow-up after CI")
    );
    assert_eq!(latest.parent_id.as_deref(), Some("parent_1"));
    assert_eq!(updated.created_at, created.created_at);
    assert!(updated.updated_at >= created.updated_at);
    Ok(())
}

pub async fn update_work_plan_replaces_latest_snapshot_for_existing_work_item() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let work_item = runtime
        .update_work_item(
            None,
            "Stabilize work plan projection".into(),
            WorkItemStatus::Active,
            None,
            None,
            None,
        )
        .await?;

    runtime
        .update_work_plan(
            work_item.id.clone(),
            vec![WorkPlanItem {
                step: "persist work-item store".into(),
                status: WorkPlanStepStatus::Completed,
            }],
        )
        .await?;

    let updated_plan = runtime
        .update_work_plan(
            work_item.id.clone(),
            vec![
                WorkPlanItem {
                    step: "persist work-item store".into(),
                    status: WorkPlanStepStatus::Completed,
                },
                WorkPlanItem {
                    step: "project work queue into prompt".into(),
                    status: WorkPlanStepStatus::InProgress,
                },
            ],
        )
        .await?;

    let latest = runtime.latest_work_plan(&work_item.id).await?.unwrap();
    assert_eq!(latest.items.len(), 2);
    assert_eq!(
        latest.items[1],
        WorkPlanItem {
            step: "project work queue into prompt".into(),
            status: WorkPlanStepStatus::InProgress,
        }
    );
    assert_eq!(latest.created_at, updated_plan.created_at);
    Ok(())
}

pub async fn timer_tick_wakes_sleeping_session() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("timer result")))?;
    let runtime = host.default_runtime().await?;
    runtime
        .schedule_timer(50, None, Some("timer fired".into()))
        .await?;

    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let briefs = runtime.recent_briefs(10).await?;
    assert!(briefs
        .iter()
        .any(|brief| brief.text.contains("timer result")));
    Ok(())
}

pub async fn wake_hint_coalesces_while_running_and_reenters_once() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(WakeHintProvider::new()))?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "do the first turn".into(),
            },
        ))
        .await?;

    sleep(Duration::from_millis(50)).await;
    let first = runtime
        .submit_wake_hint(WakeHint {
            agent_id: "default".into(),
            reason: "pr changed".into(),
            source: Some("github".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: Some("corr-1".into()),
            causation_id: None,
        })
        .await?;
    let second = runtime
        .submit_wake_hint(WakeHint {
            agent_id: "default".into(),
            reason: "ci changed".into(),
            source: Some("ci".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: Some("corr-2".into()),
            causation_id: None,
        })
        .await?;
    assert_eq!(first, WakeDisposition::Coalesced);
    assert_eq!(second, WakeDisposition::Coalesced);

    wait_until(|| {
        let messages = runtime.storage().read_recent_messages(20)?;
        let state = runtime
            .storage()
            .read_agent()?
            .expect("agent state should exist");
        Ok(messages
            .iter()
            .filter(|message| message.kind == MessageKind::SystemTick)
            .count()
            == 1
            && state.pending_wake_hint.is_none()
            && state
                .last_continuation
                .as_ref()
                .is_some_and(|continuation| {
                    continuation.class == holon::types::ContinuationClass::LivenessOnly
                        && !continuation.model_visible
                }))
    })
    .await?;

    Ok(())
}

pub async fn paused_agent_ignores_wake_hint() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    let runtime = host.default_runtime().await?;
    runtime.control(ControlAction::Pause).await?;

    let disposition = runtime
        .submit_wake_hint(WakeHint {
            agent_id: "default".into(),
            reason: "something changed".into(),
            source: Some("watcher".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
        })
        .await?;
    assert_eq!(disposition, WakeDisposition::Ignored);

    sleep(Duration::from_millis(150)).await;
    let state = runtime.agent_state().await?;
    let messages = runtime.storage().read_recent_messages(10)?;
    assert_eq!(state.status, AgentStatus::Paused);
    assert!(state.pending_wake_hint.is_none());
    assert!(messages
        .iter()
        .all(|message| message.kind != MessageKind::SystemTick));
    Ok(())
}

pub async fn multi_session_state_is_isolated() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    host.create_named_agent("alpha", None).await?;
    host.create_named_agent("beta", None).await?;
    let a = host.get_or_create_agent("alpha").await?;
    let b = host.get_or_create_agent("beta").await?;

    a.enqueue(MessageEnvelope::new(
        "alpha",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "alpha".into(),
        },
    ))
    .await?;
    b.enqueue(MessageEnvelope::new(
        "beta",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "beta".into(),
        },
    ))
    .await?;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let alpha_briefs = a.recent_briefs(10).await?;
    let beta_briefs = b.recent_briefs(10).await?;
    assert_eq!(alpha_briefs.len(), 2);
    assert_eq!(beta_briefs.len(), 2);
    assert_eq!(alpha_briefs[0].agent_id, "alpha");
    assert_eq!(beta_briefs[0].agent_id, "beta");
    Ok(())
}

pub async fn notify_operator_records_default_public_and_private_child_targets() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    let default_runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(default_runtime.workspace_root());

    let (default_result, default_record) = registry
        .execute(
            &default_runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "notify-default".into(),
                name: "NotifyOperator".into(),
                input: json!({ "message": "\nDefault operator note\nsecond line" }),
            },
        )
        .await?;
    assert!(!default_result.should_sleep);
    assert_eq!(default_record.tool_name, "NotifyOperator");
    let default_value: serde_json::Value = parse_tool_result_payload(&default_result)?;
    assert_eq!(
        default_value["notification"]["target_operator_boundary"],
        "primary_operator"
    );
    assert_eq!(
        default_value["notification"]["summary"],
        "Default operator note"
    );

    host.create_named_agent("public-agent", None).await?;
    let public_runtime = host.get_public_agent("public-agent").await?;
    let (public_result, _) = registry
        .execute(
            &public_runtime,
            "public-agent",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "notify-public".into(),
                name: "NotifyOperator".into(),
                input: json!({ "message": "Public named note" }),
            },
        )
        .await?;
    let public_value: serde_json::Value = parse_tool_result_payload(&public_result)?;
    assert_eq!(
        public_value["notification"]["target_operator_boundary"],
        "primary_operator"
    );
    assert_eq!(public_value["notification"]["agent_id"], "public-agent");

    let spawned = default_runtime
        .spawn_agent(
            "child summary".into(),
            "child prompt".into(),
            TrustLevel::TrustedOperator,
            AgentProfilePreset::PrivateChild,
            None,
            false,
            None,
        )
        .await?;
    let child_runtime = host.get_or_create_agent(&spawned.agent_id).await?;
    let (child_result, _) = registry
        .execute(
            &child_runtime,
            &spawned.agent_id,
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "notify-child".into(),
                name: "NotifyOperator".into(),
                input: json!({ "message": "Child needs supervision visibility" }),
            },
        )
        .await?;
    let child_value: serde_json::Value = parse_tool_result_payload(&child_result)?;
    assert_eq!(
        child_value["notification"]["target_operator_boundary"],
        "parent_supervisor"
    );
    assert_eq!(child_value["notification"]["agent_id"], "default");
    assert_eq!(
        child_value["notification"]["requested_by_agent_id"],
        spawned.agent_id
    );
    assert_eq!(
        child_value["notification"]["target_parent_agent_id"],
        "default"
    );

    let child_notifications = child_runtime.recent_operator_notifications(10).await?;
    assert_eq!(
        child_notifications[0].target_operator_boundary,
        OperatorNotificationBoundary::ParentSupervisor
    );
    let parent_notifications = default_runtime.recent_operator_notifications(10).await?;
    assert!(parent_notifications.iter().any(|notification| {
        notification.requested_by_agent_id == spawned.agent_id
            && notification.target_operator_boundary
                == OperatorNotificationBoundary::ParentSupervisor
    }));
    let events = child_runtime.storage().read_recent_events(20)?;
    assert!(events
        .iter()
        .any(|event| event.kind == "operator_notification_requested"));
    let summary = child_runtime.agent_summary().await?;
    assert_ne!(
        summary.closure.waiting_reason,
        Some(WaitingReason::AwaitingOperatorInput)
    );
    Ok(())
}

pub async fn notify_operator_does_not_stop_same_turn_tool_execution() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(NotifyThenAgentGetProvider::new()))?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "notify and continue".into(),
            },
        ))
        .await?;
    wait_until(|| {
        Ok(runtime
            .storage()
            .read_recent_briefs(10)?
            .iter()
            .any(|brief| brief.text.contains("continued after notifying operator")))
    })
    .await?;

    let tool_records = runtime.storage().read_recent_tool_executions(10)?;
    assert!(tool_records
        .iter()
        .any(|record| record.tool_name == "NotifyOperator"));
    assert!(tool_records
        .iter()
        .any(|record| record.tool_name == "AgentGet"));
    let notification = runtime
        .recent_operator_notifications(10)
        .await?
        .into_iter()
        .next()
        .expect("notification should be recorded");
    assert_eq!(notification.summary, "Operator FYI");
    let summary = runtime.agent_summary().await?;
    assert_ne!(
        summary.closure.waiting_reason,
        Some(WaitingReason::AwaitingOperatorInput)
    );
    assert_eq!(summary.recent_operator_notifications.len(), 1);
    Ok(())
}

pub async fn notify_operator_prefers_reply_route_for_delivery() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    let runtime = host.default_runtime().await?;

    runtime
        .upsert_operator_transport_binding(operator_transport_binding(
            "opbind-z-ingress",
            "route-ingress-default",
        ))
        .await?;
    runtime
        .upsert_operator_transport_binding(operator_transport_binding(
            "opbind-a-default",
            "route-default",
        ))
        .await?;

    let inbound = MessageEnvelope {
        metadata: Some(json!({
            "operator_transport": {
                "binding_id": "opbind-z-ingress",
                "reply_route_id": "route-reply-preferred",
            },
        })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:jolestar".into()),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "route preference check".into(),
            },
        )
    };
    runtime.enqueue(inbound).await?;
    wait_until_async(|| {
        let runtime = runtime.clone();
        async move {
            Ok(runtime
                .storage()
                .read_recent_events(20)?
                .iter()
                .any(|event| event.kind == "message_processing_started"))
        }
    })
    .await?;

    let notify_result = runtime
        .notify_operator("reply-route route check".into())
        .await?;
    let records = runtime.recent_operator_delivery_records(10).await?;
    let record = records
        .into_iter()
        .find(|record| record.output_event_id == notify_result.notification_id)
        .expect("delivery record should be stored");
    assert_eq!(record.binding_id, "opbind-z-ingress");
    assert_eq!(record.route_id, "route-reply-preferred");

    Ok(())
}

pub async fn notify_operator_ignores_reply_route_when_binding_no_longer_matches() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    let runtime = host.default_runtime().await?;

    runtime
        .upsert_operator_transport_binding(operator_transport_binding(
            "opbind-z-ingress",
            "route-ingress-default",
        ))
        .await?;
    runtime
        .upsert_operator_transport_binding(operator_transport_binding(
            "opbind-a-default",
            "route-default",
        ))
        .await?;

    let inbound = MessageEnvelope {
        metadata: Some(json!({
            "operator_transport": {
                "binding_id": "opbind-z-ingress",
                "reply_route_id": "route-reply-preferred",
            },
        })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:jolestar".into()),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "route mismatch fallback check".into(),
            },
        )
    };
    runtime.enqueue(inbound).await?;
    wait_until_async(|| {
        let runtime = runtime.clone();
        async move {
            Ok(runtime
                .storage()
                .read_recent_events(20)?
                .iter()
                .any(|event| event.kind == "message_processing_started"))
        }
    })
    .await?;

    let mut stopped_binding =
        operator_transport_binding("opbind-z-ingress", "route-ingress-default");
    stopped_binding.status = OperatorTransportBindingStatus::Revoked;
    runtime
        .storage()
        .append_operator_transport_binding(&stopped_binding)?;

    let notify_result = runtime
        .notify_operator("reply-route binding mismatch fallback".into())
        .await?;
    let records = runtime.recent_operator_delivery_records(10).await?;
    let record = records
        .into_iter()
        .find(|record| record.output_event_id == notify_result.notification_id)
        .expect("delivery record should be stored");
    assert_eq!(record.binding_id, "opbind-a-default");
    assert_eq!(record.route_id, "route-default");

    Ok(())
}

pub async fn notify_operator_falls_back_to_default_route_without_reply_route() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    let runtime = host.default_runtime().await?;

    runtime
        .upsert_operator_transport_binding(operator_transport_binding(
            "opbind-default",
            "route-default",
        ))
        .await?;

    let inbound = MessageEnvelope {
        metadata: Some(json!({
            "operator_transport": {
                "binding_id": "opbind-default",
            }
        })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:jolestar".into()),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "default route fallback check".into(),
            },
        )
    };
    runtime.enqueue(inbound).await?;
    wait_until_async(|| {
        let runtime = runtime.clone();
        async move {
            Ok(runtime
                .storage()
                .read_recent_events(20)?
                .iter()
                .any(|event| event.kind == "message_processing_started"))
        }
    })
    .await?;

    let notify_result = runtime
        .notify_operator("fallback route check".into())
        .await?;
    let records = runtime.recent_operator_delivery_records(10).await?;
    let record = records
        .into_iter()
        .find(|record| record.output_event_id == notify_result.notification_id)
        .expect("delivery record should be stored");
    assert_eq!(record.binding_id, "opbind-default");
    assert_eq!(record.route_id, "route-default");

    Ok(())
}

pub async fn agent_summary_last_turn_token_usage_survives_transcript_windowing() -> Result<()> {
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

    for i in 0..60 {
        runtime
            .storage()
            .append_transcript_entry(&TranscriptEntry::new(
                "default",
                TranscriptEntryKind::IncomingMessage,
                None,
                None,
                json!({ "noise": i }),
            ))?;
    }

    let persisted = runtime.storage().read_agent()?.expect("agent should exist");
    assert_eq!(
        persisted.last_turn_token_usage,
        Some(TokenUsage::new(100, 50))
    );

    let summary = runtime.agent_summary().await?;
    assert_eq!(
        summary.token_usage.last_turn,
        Some(TokenUsage::new(100, 50))
    );
    Ok(())
}

pub async fn callback_tools_register_and_revoke_waiting_state() -> Result<()> {
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ok")))?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(runtime.workspace_root());

    let (created, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-create-callback".into(),
                name: "CreateExternalTrigger".into(),
                input: json!({
                    "summary": "wait for CI callback",
                    "source": "github",
                    "condition": "required_checks_passed",
                    "resource": "pull_request:123",
                    "delivery_mode": "enqueue_message"
                }),
            },
        )
        .await?;
    let capability: serde_json::Value = parse_tool_result_payload(&created)?;
    let trigger_url = capability["trigger_url"]
        .as_str()
        .expect("trigger_url should be present");
    assert!(!capability.as_object().unwrap().contains_key("callback_url"));
    assert!(!capability
        .as_object()
        .unwrap()
        .contains_key("callback_descriptor_id"));
    let callback_token = trigger_url
        .rsplit('/')
        .next()
        .expect("trigger url should end with a token");

    let waiting = runtime.latest_waiting_intents().await?;
    let descriptors = runtime.latest_external_triggers().await?;
    assert_eq!(waiting.len(), 1);
    assert_eq!(descriptors.len(), 1);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Active);
    assert_eq!(descriptors[0].status, ExternalTriggerStatus::Active);

    let summary = runtime.agent_summary().await?;
    assert_eq!(summary.active_waiting_intents.len(), 1);
    assert_eq!(summary.active_external_triggers.len(), 1);
    assert_eq!(summary.closure.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        summary.closure.waiting_reason,
        Some(WaitingReason::AwaitingExternalChange)
    );
    assert_eq!(
        summary.active_waiting_intents[0].delivery_mode,
        CallbackDeliveryMode::EnqueueMessage
    );
    let summary_json = serde_json::to_string(&summary)?;
    assert!(!summary_json.contains(callback_token));

    let (state_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-get-state".into(),
                name: "AgentGet".into(),
                input: json!({}),
            },
        )
        .await?;
    let state_text = state_result.content_text()?;
    assert!(state_text.contains("active_waiting_intents"));
    assert!(state_text.contains("active_external_triggers"));
    assert!(state_text.contains("external_trigger_id"));
    assert!(!state_text.contains(callback_token));
    assert!(!state_text.contains(trigger_url));

    let (cancelled, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &ToolCall {
                id: "tool-cancel-waiting".into(),
                name: "CancelExternalTrigger".into(),
                input: json!({
                    "waiting_intent_id": waiting[0].id,
                }),
            },
        )
        .await?;
    let cancelled: serde_json::Value = parse_tool_result_payload(&cancelled)?;
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled["external_trigger_id"].is_string());
    let events = runtime.storage().read_recent_events(20)?;
    let cancelled_event = events
        .iter()
        .rev()
        .find(|event| event.kind == "waiting_intent_cancelled")
        .expect("waiting_intent_cancelled event");
    assert!(cancelled_event.data["external_trigger_id"].is_string());

    let waiting = runtime.latest_waiting_intents().await?;
    let descriptors = runtime.latest_external_triggers().await?;
    assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
    assert_eq!(descriptors[0].status, ExternalTriggerStatus::Revoked);
    let summary = runtime.agent_summary().await?;
    assert!(summary.active_waiting_intents.is_empty());
    assert!(summary.active_external_triggers.is_empty());
    let summary_value = serde_json::to_value(&summary)?;
    assert!(summary_value["active_external_triggers"].is_array());
    Ok(())
}

pub async fn timer_wait_surfaces_waiting_reason() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    runtime
        .schedule_timer(5_000, None, Some("wait for time".into()))
        .await?;

    let summary = runtime.agent_summary().await?;
    assert_eq!(summary.closure.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        summary.closure.waiting_reason,
        Some(WaitingReason::AwaitingTimer)
    );
    Ok(())
}

pub fn policy_blocks_mismatched_origin() {
    let mismatch = validate_message_kind_for_origin(
        &MessageKind::WebhookEvent,
        &MessageOrigin::Operator { actor_id: None },
    );
    assert!(!mismatch.allowed);
}

pub async fn sleep_only_completion_preserves_brief_after_max_output_recovery() -> anyhow::Result<()>
{
    let provider = Arc::new(MaxOutputRecoveryProvider::new());
    let host = RuntimeHost::new_with_provider(test_config(), provider.clone())?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    // Send a prompt that will trigger max-output recovery followed by Sleep-only completion
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Generate a comprehensive technical report covering multiple domains. \
                      Include detailed sections on: 1) System architecture patterns 2) Data flow strategies \
                      3) Security considerations 4) Performance optimization 5) Monitoring approaches. \
                      After completing your analysis, finish with Sleep.".into(),
            },
        ))
        .await?;

    // Wait for the agent to complete
    wait_until(|| {
        let agent = runtime.storage().read_agent()?.expect("agent should exist");
        Ok(agent.status == AgentStatus::Asleep)
    })
    .await?;

    // Verify that the brief was preserved despite max-output recovery + Sleep-only completion
    let briefs = runtime.storage().read_recent_briefs(10)?;
    let last_brief = briefs.last();

    assert!(
        last_brief.is_some(),
        "brief should be preserved after max-output recovery and Sleep-only completion"
    );

    let brief_text = last_brief.map(|b| b.text.as_str()).unwrap_or("");
    assert!(!brief_text.is_empty(), "brief should not be empty");

    // Verify the brief contains expected content from the max-output recovery
    assert!(
        brief_text.contains("analysis")
            || brief_text.contains("report")
            || brief_text.contains("architecture"),
        "brief should contain relevant content from the generated analysis, got: {}",
        brief_text
    );

    // Verify that max-output recovery was triggered
    let events = runtime.storage().read_recent_events(50)?;
    let recovery_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "max_output_tokens_recovery")
        .collect();

    assert!(
        !recovery_events.is_empty(),
        "max-output recovery should have been triggered"
    );

    Ok(())
}

// Minimal provider that simulates max-output recovery followed by Sleep-only completion
