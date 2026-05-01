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
    MaxOutputRecoveryProvider, MaxOutputThenCompactionProvider,
    MultiPassCompactionRecoveryFlowProvider, NotifyThenAgentGetProvider, RecordingPromptProvider,
    RepeatedCompactionProvider, RuntimeFailureProvider, ShellProvider,
    SleepOnlyCompletionAfterTextProvider, TerminalResultBriefProvider, ToolErrorProvider,
    ToolUsingProvider, TruncatedShellReinjectionProvider, UseWorkspaceProvider,
    VerboseRuntimeFailureProvider, WakeHintProvider, WorktreeCapturingProvider,
    WorktreeLifecycleProvider,
};
use support::{
    attach_default_workspace, eventually, eventually_async, eventually_for, test_work_item,
    TestConfigBuilder,
};

// ============================================================================
// Domain-specific test implementations
// ============================================================================

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
    assert_eq!(session.state, AgentStatus::Asleep);
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
                record.id == task.id && record.state == holon::types::TaskStatus::Completed
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
                && record.state == holon::types::TaskStatus::Completed)
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
            record.id == task.id && record.state == holon::types::TaskStatus::Cancelled
        }))
    })
    .await?;

    let state = runtime.agent_state().await?;
    assert!(!state.active_task_ids.contains(&task.id));
    Ok(())
}

pub async fn update_work_item_creates_and_updates_persisted_snapshot() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let (created, _) = runtime
        .create_work_item("Ship work-item runtime foundation".into(), None)
        .await?;
    assert!(created.id.starts_with("work_"));

    let (updated, _) = runtime
        .update_work_item_fields(
            created.id.clone(),
            Some(Some("queued follow-up after CI".into())),
            None,
        )
        .await?;

    let latest = runtime.latest_work_item(&created.id).await?.unwrap();
    assert_eq!(latest.id, created.id);
    assert_eq!(latest.state, WorkItemState::Open);
    assert_eq!(latest.blocked_by.as_deref(), Some("queued follow-up after CI"));
    assert_eq!(updated.created_at, created.created_at);
    assert!(updated.updated_at >= created.updated_at);
    Ok(())
}

pub async fn update_work_item_replaces_latest_plan_snapshot_for_existing_work_item() -> Result<()> {
    let host =
        RuntimeHost::new_with_provider(test_config(), Arc::new(StubProvider::new("ignored")))?;
    let runtime = host.default_runtime().await?;

    let (work_item, _) = runtime
        .create_work_item("Stabilize work plan projection".into(), None)
        .await?;

    runtime
        .update_work_item_fields(
            work_item.id.clone(),
            None,
            Some(vec![WorkPlanItem {
                step: "persist work-item store".into(),
                status: WorkPlanStepStatus::Completed,
            }]),
        )
        .await?;

    let (_, updated_plan) = runtime
        .update_work_item_fields(
            work_item.id.clone(),
            None,
            Some(
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
            ),
        )
        .await?;
    let updated_plan = updated_plan.expect("expected plan snapshot");

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

pub async fn preview_prompt_after_compaction_keeps_work_item_plan_and_pending_work_visible(
) -> Result<()> {
    let host = RuntimeHost::new_with_provider(
        aggressive_compaction_config(),
        Arc::new(StubProvider::new("ignored")),
    )?;
    let runtime = host.default_runtime().await?;

    let active = test_work_item(&runtime, "Stabilize long-running compaction", WorkItemState::Open, true, Some("survival matrix is in progress")).await?;
    runtime
        .update_work_plan(
            active.id.clone(),
            vec![
                WorkPlanItem {
                    step: "capture long-running survival case".into(),
                    status: WorkPlanStepStatus::Completed,
                },
                WorkPlanItem {
                    step: "cover task rejoin after compaction".into(),
                    status: WorkPlanStepStatus::InProgress,
                },
            ],
        )
        .await?;

    let queued = test_work_item(&runtime, "Queue wake-hint verification", WorkItemState::Open, false, None).await?;
    let waiting = test_work_item(&runtime, "Wait for CI rerun", WorkItemState::Open, false, Some("resume after workflow completes")).await?;
    let _completed = test_work_item(&runtime, "Already shipped shadow-state cleanup", WorkItemState::Done, false, None).await?;

    for idx in 0..4 {
        runtime.storage().append_message(&MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: format!(
                    "history-{idx}: {}",
                    "long-running headless context block ".repeat(24)
                ),
            },
        ))?;
    }

    let prompt = runtime
        .preview_prompt(
            "Continue the long-running compaction regression matrix.".into(),
            TrustLevel::TrustedOperator,
        )
        .await?;

    assert!(prompt.cache_identity.compression_epoch > 0);

    let active_section = prompt
        .context_sections
        .iter()
        .find(|section| section.name == "current_work_item")
        .expect("current work item section should be present after compaction");
    assert!(active_section
        .content
        .contains("Stabilize long-running compaction"));
    assert!(active_section
        .content
        .contains("cover task rejoin after compaction"));

    let queued_blocked_section = prompt
        .context_sections
        .iter()
        .find(|section| section.name == "queued_blocked_work_items")
        .expect("queued/blocked work section should be present after compaction");
    assert!(queued_blocked_section
        .content
        .contains(queued.delivery_target.as_str()));
    assert!(queued_blocked_section
        .content
        .contains(waiting.delivery_target.as_str()));
    assert!(!queued_blocked_section
        .content
        .contains("Already shipped shadow-state cleanup"));

    Ok(())
}

pub async fn task_result_rejoin_after_compaction_preserves_current_work_truth() -> Result<()> {
    let provider = Arc::new(RecordingPromptProvider::new(&[
        "initial turn complete",
        "task rejoin complete",
    ]));
    let host = RuntimeHost::new_with_provider(aggressive_compaction_config(), provider.clone())?;
    let runtime = host.default_runtime().await?;

    let work_item = test_work_item(&runtime, "Close the compaction regression gap", WorkItemState::Open, true, Some("waiting for command task evidence"))
        .await?;
    runtime
        .update_work_plan(
            work_item.id.clone(),
            vec![WorkPlanItem {
                step: "verify task rejoin survives compaction".into(),
                status: WorkPlanStepStatus::InProgress,
            }],
        )
        .await?;
    for idx in 0..3 {
        runtime.storage().append_message(&MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: format!("task-history-{idx}: {}", "rejoin context ".repeat(20)),
            },
        ))?;
    }

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Start the long-running compaction verification.".into(),
            },
        ))
        .await?;

    wait_until_async(|| {
        let provider = provider.clone();
        async move { Ok(provider.captured_requests().await.len() >= 1) }
    })
    .await?;

    let task = runtime
        .schedule_command_task(
            "emit task evidence".into(),
            holon::types::CommandTaskSpec {
                cmd: "printf task_done".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(128),
                accepts_input: false,
                continue_on_result: true,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    wait_until_async(|| {
        let provider = provider.clone();
        async move { Ok(provider.captured_requests().await.len() >= 2) }
    })
    .await?;

    let requests = provider.captured_requests().await;
    let task_rejoin = &requests[1];
    assert!(task_rejoin.compression_epoch > 0);
    assert!(task_rejoin.working_memory_revision > 0);
    assert!(task_rejoin
        .prompt_text
        .contains("Close the compaction regression gap"));
    assert!(task_rejoin
        .prompt_text
        .contains("verify task rejoin survives compaction"));

    eventually_for(Duration::from_secs(20), || {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks.iter().any(|record| {
            record.id == task.id && record.state == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let latest = runtime
        .latest_work_item(&work_item.id)
        .await?
        .expect("current work item should still exist");
    assert_eq!(latest.state, WorkItemState::Open);
    assert_eq!(latest.delivery_target, work_item.delivery_target);

    Ok(())
}

pub async fn contentful_wake_hint_after_compaction_keeps_active_work_truth() -> Result<()> {
    let provider = Arc::new(
        RecordingPromptProvider::new(&["first turn complete", "wake follow-up complete"])
            .with_first_delay(Duration::from_millis(250)),
    );
    let host = RuntimeHost::new_with_provider(aggressive_compaction_config(), provider.clone())?;
    let runtime = host.default_runtime().await?;

    let active = test_work_item(&runtime, "Keep active compaction work in focus", WorkItemState::Open, true, None).await?;
    runtime
        .update_work_plan(
            active.id.clone(),
            vec![WorkPlanItem {
                step: "resume from contentful wake hint".into(),
                status: WorkPlanStepStatus::InProgress,
            }],
        )
        .await?;
    let queued = test_work_item(
        &runtime,
        "Queued fallback work",
        WorkItemState::Open,
        false,
        None,
        )
        .await?;
    for idx in 0..3 {
        runtime.storage().append_message(&MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: format!("wake-history-{idx}: {}", "wake context ".repeat(20)),
            },
        ))?;
    }

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Do the first turn before the wake hint lands.".into(),
            },
        ))
        .await?;

    wait_until_async(|| {
        let provider = provider.clone();
        async move { Ok(provider.captured_requests().await.len() >= 1) }
    })
    .await?;

    let disposition = runtime
        .submit_wake_hint(WakeHint {
            agent_id: "default".into(),
            reason: "pr review requested".into(),
            source: Some("github".into()),
            resource: Some("pr/273".into()),
            body: Some(MessageBody::Json {
                value: json!({
                    "event": "review_requested",
                    "pr": 273
                }),
            }),
            content_type: Some("application/json".into()),
            correlation_id: Some("wake-273".into()),
            causation_id: None,
        })
        .await?;
    assert_eq!(disposition, WakeDisposition::Coalesced);

    wait_until_async(|| {
        let provider = provider.clone();
        async move { Ok(provider.captured_requests().await.len() >= 2) }
    })
    .await?;

    let requests = provider.captured_requests().await;
    let wake_follow_up = &requests[1];
    assert!(wake_follow_up.compression_epoch > 0);
    assert!(wake_follow_up
        .prompt_text
        .contains("Keep active compaction work in focus"));
    assert!(wake_follow_up
        .prompt_text
        .contains("resume from contentful wake hint"));
    assert!(wake_follow_up.prompt_text.contains("review_requested"));
    assert!(wake_follow_up.prompt_text.contains("\"pr\": 273"));

    let queued_latest = runtime
        .latest_work_item(&queued.id)
        .await?
        .expect("queued work item should still exist");
    assert_eq!(queued_latest.state, WorkItemState::Open);

    Ok(())
}

pub async fn queued_activation_after_compaction_promotes_the_correct_next_step() -> Result<()> {
    let provider = Arc::new(RecordingPromptProvider::new(&[
        "first turn complete",
        "queued follow-up complete",
    ]));
    let host = RuntimeHost::new_with_provider(aggressive_compaction_config(), provider.clone())?;
    let runtime = host.default_runtime().await?;

    let queued = test_work_item(&runtime, "Resume queued compaction validation", WorkItemState::Open, false, None)
        .await?;
    runtime
        .update_work_plan(
            queued.id.clone(),
            vec![WorkPlanItem {
                step: "activate queued work after compaction".into(),
                status: WorkPlanStepStatus::InProgress,
            }],
        )
        .await?;
    for idx in 0..3 {
        runtime.storage().append_message(&MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: format!("queue-history-{idx}: {}", "queued context ".repeat(20)),
            },
        ))?;
    }

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Wrap up the current turn so queued work can resume.".into(),
            },
        ))
        .await?;

    wait_until_async(|| {
        let provider = provider.clone();
        async move { Ok(provider.captured_requests().await.len() >= 2) }
    })
    .await?;

    let requests = provider.captured_requests().await;
    let queued_follow_up = &requests[1];
    assert!(queued_follow_up.compression_epoch > 0);
    assert!(queued_follow_up
        .prompt_text
        .contains("Resume queued compaction validation"));
    assert!(queued_follow_up
        .prompt_text
        .contains("activate queued work after compaction"));

    wait_until(|| {
        let agent = runtime.storage().read_agent()?.expect("agent should exist");
        Ok(agent
            .last_continuation
            .as_ref()
            .is_some_and(|continuation| {
                continuation.trigger_kind == holon::types::ContinuationTriggerKind::SystemTick
                    && continuation.model_visible
            }))
    })
    .await?;

    let latest = runtime
        .latest_work_item(&queued.id)
        .await?
        .expect("queued work item should still exist");
    assert_eq!(latest.state, WorkItemState::Open);

    Ok(())
}

pub async fn repeated_turn_local_compaction_evolves_checkpoint_mode_and_keeps_latest_exact_tail(
) -> Result<()> {
    let provider = Arc::new(RepeatedCompactionProvider::new());
    let host = RuntimeHost::new_with_provider(aggressive_compaction_config(), provider.clone())?;
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
                text: "Run a long-running review loop that repeatedly requires compaction and checkpointing.".into(),
            },
        ))
        .await?;

    wait_until_async_for(Duration::from_secs(180), {
        let runtime = runtime.clone();
        let provider = provider.clone();
        move || {
            let runtime = runtime.clone();
            let provider = provider.clone();
            async move {
                let events = runtime.storage().read_recent_events(5000)?;
                let requests = provider.captured_requests().await;
                let has_compaction = events
                    .iter()
                    .any(|event| event.kind == "turn_local_compaction_applied");
                let has_full_checkpoint = requests
                    .iter()
                    .any(|request| request.has_full_checkpoint_request);
                let has_delta_checkpoint = requests
                    .iter()
                    .any(|request| request.has_delta_checkpoint_request);
                let has_exact_tail = requests.iter().any(|request| {
                    request
                        .assistant_text_snapshot
                        .contains("checkpoint-ready continuation")
                });
                Ok(has_compaction && has_full_checkpoint && has_delta_checkpoint && has_exact_tail)
            }
        }
    })
    .await?;

    let requests = provider.captured_requests().await;

    let events = runtime.recent_events(120).await?;
    let compaction_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "turn_local_compaction_applied")
        .collect();
    assert!(
        compaction_events.len() >= 2,
        "multiple compaction passes should occur in a long turn"
    );

    let checkpoint_modes = compaction_events
        .iter()
        .map(|event| {
            event
                .data
                .get("checkpoint_mode")
                .and_then(|value| value.as_str())
        })
        .collect::<Vec<_>>();
    let first_full_mode = checkpoint_modes
        .iter()
        .position(|mode| *mode == Some("full"));
    assert!(first_full_mode.is_some(), "first checkpoint should be full");
    let later_delta_mode = checkpoint_modes
        .iter()
        .skip(first_full_mode.unwrap() + 1)
        .any(|mode| *mode == Some("delta"));
    assert!(
        later_delta_mode,
        "subsequent checkpoints should use delta after the first full"
    );

    let first_checkpoint_request = requests
        .iter()
        .find(|request| request.has_full_checkpoint_request)
        .expect("should observe full checkpoint prompt in continuation request");
    assert!(
        first_checkpoint_request
            .user_text_snapshot
            .contains("current user goal"),
        "full checkpoint prompt should include progress context"
    );

    let delta_checkpoint_request = requests
        .iter()
        .filter(|request| request.call_index > first_checkpoint_request.call_index)
        .find(|request| request.has_delta_checkpoint_request)
        .expect("should observe delta checkpoint after full checkpoint");
    assert!(
        delta_checkpoint_request
            .user_text_snapshot
            .contains("Base checkpoint round"),
        "delta checkpoint should retain base checkpoint reference"
    );

    assert!(
        delta_checkpoint_request
            .assistant_text_snapshot
            .contains("checkpoint-ready continuation"),
        "latest exact tail should remain visible after repeated compaction"
    );
    assert!(
        first_checkpoint_request
            .user_text_snapshot
            .contains("current user goal"),
        "first checkpoint request should include progress prompt context"
    );

    assert!(
        requests.iter().any(|request| request.has_turn_local_recap),
        "compacted request should retain deterministic recap markers"
    );

    Ok(())
}

pub async fn max_output_recovery_followed_by_turn_local_compaction_preserves_progress_signal(
) -> Result<()> {
    let provider = Arc::new(MaxOutputThenCompactionProvider::new());
    let host = RuntimeHost::new_with_provider(aggressive_compaction_config(), provider.clone())?;
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
                text: "Produce analysis in constrained output, then continue after recovery while preserving checkpoint progress.".into(),
            },
        ))
        .await?;

    wait_until_async_for(Duration::from_secs(180), {
        let runtime = runtime.clone();
        let provider = provider.clone();
        move || {
            let runtime = runtime.clone();
            let provider = provider.clone();
            async move {
                let events = runtime.storage().read_recent_events(5000)?;
                let requests = provider.captured_requests().await;
                let has_recovery = events
                    .iter()
                    .any(|event| event.kind == "max_output_tokens_recovery");
                let has_compaction = events
                    .iter()
                    .any(|event| event.kind == "turn_local_compaction_applied");
                let has_progress_checkpoint = requests
                    .iter()
                    .any(|request| request.has_progress_checkpoint_request);
                let has_recovery_tail = requests.iter().any(|request| {
                    request
                        .assistant_text_snapshot
                        .contains("recovery continuation introduces structured output evidence.")
                });
                let has_follow_up_tail = requests.iter().any(|request| {
                    request
                        .assistant_text_snapshot
                        .contains("follow-up synthesis for compacted checkpoint continuity")
                });
                Ok(has_recovery
                    && has_compaction
                    && has_progress_checkpoint
                    && has_recovery_tail
                    && has_follow_up_tail)
            }
        }
    })
    .await?;

    let events = runtime.recent_events(120).await?;
    let recovery_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "max_output_tokens_recovery")
        .collect();
    assert!(
        !recovery_events.is_empty(),
        "max-output recovery should be triggered"
    );

    let compaction_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "turn_local_compaction_applied")
        .collect();
    assert!(
        compaction_events.len() >= 2,
        "max-output continuation should still support repeated turn-local compaction"
    );

    let checkpoint_modes = compaction_events
        .iter()
        .map(|event| {
            event
                .data
                .get("checkpoint_mode")
                .and_then(|value| value.as_str())
        })
        .collect::<Vec<_>>();
    assert!(
        checkpoint_modes.iter().any(|mode| *mode == Some("full")),
        "should transition to full checkpoint mode when compaction starts"
    );
    assert!(
        checkpoint_modes.iter().any(|mode| *mode == Some("delta")),
        "later compactions should use delta mode for unchanged anchors"
    );

    let requests = provider.captured_requests().await;
    assert!(requests.len() >= 2);

    let checkpoint_request = requests
        .iter()
        .find(|request| request.has_progress_checkpoint_request)
        .expect("a progress checkpoint should be requested after compaction");

    assert!(
        checkpoint_request
            .user_text_snapshot
            .contains("current work plan state"),
        "progress checkpoint should include work-plan continuity"
    );
    assert!(
        checkpoint_request
            .user_text_snapshot
            .contains("what remains unknown"),
        "progress checkpoint should include remaining uncertainty"
    );
    assert!(
        checkpoint_request
            .user_text_snapshot
            .contains("next goal-aligned action"),
        "progress checkpoint should include bounded next action"
    );

    assert!(
        requests.iter().any(|request| request
            .assistant_text_snapshot
            .contains("recovery continuation introduces structured output evidence.")),
        "recovery follow-up should still be visible after continuation"
    );
    assert!(
        requests.iter().any(|request| request
            .assistant_text_snapshot
            .contains("follow-up synthesis for compacted checkpoint continuity")),
        "latest exact round should remain in prompt after repeated compaction"
    );

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
    assert_eq!(state.state, AgentStatus::Paused);
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
    stopped_binding.state = OperatorTransportBindingStatus::Revoked;
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
    assert_eq!(waiting[0].state, WaitingIntentStatus::Active);
    assert_eq!(descriptors[0].state, ExternalTriggerStatus::Active);

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
    assert_eq!(waiting[0].state, WaitingIntentStatus::Cancelled);
    assert_eq!(descriptors[0].state, ExternalTriggerStatus::Revoked);
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
    assert_eq!(record.state, holon::types::ToolExecutionStatus::Success);
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
    assert_eq!(record.state, holon::types::ToolExecutionStatus::Success);
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
    assert_eq!(state.state, AgentStatus::Asleep);
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
            record.id == task.id && record.state == holon::types::TaskStatus::Completed
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
            record.id == task.id && record.state == holon::types::TaskStatus::Completed
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
            record.id == task.id && record.state == holon::types::TaskStatus::Cancelled
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
    assert_eq!(state.state, AgentStatus::AwaitingTask);
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
            record.id == task.id && record.state == holon::types::TaskStatus::Completed
        }))
    })
    .await?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(output.task.state, holon::types::TaskStatus::Completed);
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

    assert_eq!(running_snapshot.state, TaskStatus::Running);
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
        async move { Ok(runtime.task_status_snapshot(&task_id).await?.state == TaskStatus::Completed) }
    })
    .await?;

    let terminal_snapshot = runtime.task_status_snapshot(&task.id).await?;
    assert_eq!(terminal_snapshot.state, TaskStatus::Completed);
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
            .any(|record| record.id == task.id && record.state == TaskStatus::Completed))
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
                record.state,
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
            record.id == task.id && record.state == holon::types::TaskStatus::Failed
        }))
    })
    .await?;

    let output = runtime.task_output(&task.id, false, 0).await?;
    assert_eq!(output.task.state, holon::types::TaskStatus::Failed);
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
            record.id == alpha.id && record.state == holon::types::TaskStatus::Completed
        });
        let beta_done = tasks.iter().any(|record| {
            record.id == beta.id && record.state == holon::types::TaskStatus::Completed
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
pub async fn runtime_compaction_multi_pass_recovery_preserves_progress_and_artifacts() -> Result<()>
{
    let config = {
        let mut config = aggressive_compaction_config();
        config.compaction_trigger_estimated_tokens = 1_000;
        config.compaction_keep_recent_estimated_tokens = 200;
        config.prompt_budget_estimated_tokens = 24_000;

        let model_override = holon::model_catalog::ModelRuntimeOverride {
            prompt_budget_estimated_tokens: Some(24_000),
            compaction_trigger_estimated_tokens: Some(1_000),
            compaction_keep_recent_estimated_tokens: Some(200),
            ..holon::model_catalog::ModelRuntimeOverride::default()
        };
        config
            .stored_config
            .models
            .catalog
            .insert("anthropic/claude-sonnet-4-6".into(), model_override.clone());
        config.validated_model_overrides.insert(
            holon::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            model_override,
        );
        config
    };

    let provider = Arc::new(MultiPassCompactionRecoveryFlowProvider::new());
    let host = RuntimeHost::new_with_provider(config, provider.clone())?;
    attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;

    let seed_task = runtime
        .schedule_command_task(
            "seed long output artifact".into(),
            CommandTaskSpec {
                cmd: "awk 'BEGIN { for (i=0; i<200; i++) printf \"seed_task_output \" }'".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: Some(24),
                accepts_input: false,
                continue_on_result: true,
            },
            TrustLevel::TrustedOperator,
        )
        .await?;

    provider.set_task_id(seed_task.id.clone()).await;

    wait_until(|| {
        let tasks = runtime.storage().latest_task_records()?;
        Ok(tasks
            .iter()
            .any(|record| record.id == seed_task.id && record.state == TaskStatus::Completed))
    })
    .await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Run the compaction recovery checkpoint scenario end-to-end.".into(),
            },
        ))
        .await?;

    let _ = tokio::time::timeout(
        Duration::from_secs(12),
        eventually_async({
            let runtime = runtime.clone();
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                let runtime = runtime.clone();
                async move {
                    let requests = provider.captured_requests().await;
                    if requests.len() < 2 {
                        return Ok(false);
                    }
                    let events = runtime.recent_events(80).await?;
                    Ok(events.iter().any(|event| event.kind == "turn_terminal"))
                }
            }
        }),
    )
    .await
    .map_err(|err| anyhow::anyhow!("timed out waiting for multi-round recovery flow: {err}"))?;

    let requests = provider.captured_requests().await;
    assert!(
        requests.len() >= 2,
        "expected multi-round progression through recovery and continuation: {}",
        requests.len()
    );

    let events = runtime.recent_events(80).await?;
    let compaction_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "turn_local_compaction_applied")
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "max_output_tokens_recovery"),
        "expected max-output recovery to be triggered"
    );
    if !compaction_events.is_empty() {
        let checkpoint_modes: Vec<_> = compaction_events
            .iter()
            .filter_map(|event| event.data["checkpoint_mode"].as_str())
            .collect();
        assert!(
            !checkpoint_modes.is_empty(),
            "expected at least one checkpointed compaction mode"
        );
    }

    let checkpoint_prompts: Vec<_> = requests
        .iter()
        .filter_map(|request| {
            request
                .prompt_text
                .find("progress checkpoint request")
                .map(|_| request.prompt_text.clone())
        })
        .collect();
    if !compaction_events.is_empty() {
        assert!(
            !checkpoint_prompts.is_empty(),
            "expected checkpoint prompt injection in subsequent turns"
        );
    }

    let tool_records = runtime.storage().read_recent_tool_executions(30)?;
    assert!(
        tool_records
            .iter()
            .any(|record| record.tool_name == "ExecCommand" || record.tool_name == "TaskOutput"),
        "expected structured ExecCommand/TaskOutput tool records"
    );

    let task_output = runtime.task_output(&seed_task.id, false, 0).await?;
    assert!(
        !task_output.task.artifacts.is_empty(),
        "expected task output artifacts for structured output preservation"
    );

    let state = runtime.agent_state().await?;
    let last_terminal = state
        .last_turn_terminal
        .as_ref()
        .expect("expected terminal state after interactive processing");
    assert_eq!(
        last_terminal.kind,
        holon::types::TurnTerminalKind::Completed
    );

    let briefs = runtime.recent_briefs(8).await?;
    assert!(briefs.iter().rev().any(|brief| brief
        .text
        .contains("Completed after bounded repeated compaction.")));

    Ok(())
}
