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
use crate::support::{
    attach_default_workspace, eventually, eventually_async, eventually_for, test_work_item,
    TestConfigBuilder,
};

// ============================================================================
// Runtime compaction domain test support
pub async fn preview_prompt_after_compaction_keeps_work_item_plan_and_pending_work_visible(
) -> Result<()> {
    let host = RuntimeHost::new_with_provider(
        aggressive_compaction_config(),
        Arc::new(StubProvider::new("ignored")),
    )?;
    let runtime = host.default_runtime().await?;

    let active = test_work_item(
        &runtime,
        "Stabilize long-running compaction",
        WorkItemState::Open,
        true,
        Some("survival matrix is in progress"),
    )
    .await?;
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

    let queued = test_work_item(
        &runtime,
        "Queue wake-hint verification",
        WorkItemState::Open,
        false,
        None,
    )
    .await?;
    let waiting = test_work_item(
        &runtime,
        "Wait for CI rerun",
        WorkItemState::Open,
        false,
        Some("resume after workflow completes"),
    )
    .await?;
    let _completed = test_work_item(
        &runtime,
        "Already shipped shadow-state cleanup",
        WorkItemState::Done,
        false,
        None,
    )
    .await?;

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

    let work_item = test_work_item(
        &runtime,
        "Close the compaction regression gap",
        WorkItemState::Open,
        true,
        Some("waiting for command task evidence"),
    )
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
            record.id == task.id && record.status == holon::types::TaskStatus::Completed
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

    let active = test_work_item(
        &runtime,
        "Keep active compaction work in focus",
        WorkItemState::Open,
        true,
        None,
    )
    .await?;
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
            description: None,
            scope: None,
            waiting_intent_id: None,
            external_trigger_id: None,
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

    let queued = test_work_item(
        &runtime,
        "Resume queued compaction validation",
        WorkItemState::Open,
        false,
        None,
    )
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
            .any(|record| record.id == seed_task.id && record.status == TaskStatus::Completed))
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
