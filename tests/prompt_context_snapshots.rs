use std::path::{Path, PathBuf};

use anyhow::Result;
use holon::{
    context::ContextConfig,
    prompt::build_effective_prompt,
    storage::AppStorage,
    system::{ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentRegistryStatus, AgentState, AgentVisibility, ContinuationClass,
        ContinuationResolution, ContinuationTriggerKind, LoadedAgentsMd, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        SkillsRuntimeView, TrustLevel, WaitingReason, WorkItemRecord, WorkItemState, WorkPlanItem,
        WorkPlanSnapshot, WorkPlanStepStatus, WorkingMemoryDelta, WorkingMemorySnapshot,
    },
};
use serde_json::json;
use tempfile::tempdir;

const EXECUTION_ENVIRONMENT: &str = r#"Execution environment summary (policy snapshot; host-local is not a strong sandbox guarantee):
Backend: host_local
Process execution exposed: true
Background tasks supported: true
Managed worktrees supported: true
Projection kind: canonical_root
Access mode: shared_read
Workspace id: none
Workspace anchor: /workspace
Execution root: /workspace
Cwd: /workspace
Worktree root: none
Resource authority:
  - message_ingress: hard_enforced
  - agent_state: hard_enforced
  - control_plane: hard_enforced
  - workspace_projection: hard_enforced
  - process_execution: runtime_shaped
Process execution guarantees:
  - cwd_rooting: hard_enforced
  - projection_rooting: hard_enforced
  - path_confinement: not_enforced
  - write_confinement: not_enforced
  - network_confinement: not_enforced
  - secret_isolation: not_enforced
  - child_process_containment: not_enforced"#;

const CONTEXT_CONTRACT: &str = r#"Interpret the memory block with this priority: current work item first for the committed delivery target and current runtime task, working memory delta next for the newest updates since the last prompt, and working memory after that for rolling agent context. This is an interpretation priority, not a guarantee about section ordering. Use prior briefs and recent tool results as the most reliable continuity evidence across turns. When these sources differ on task scope or delivery target, treat the current work item's `delivery_target` as the ground truth for the current committed task unless the current input explicitly changes it."#;

fn sample_identity() -> AgentIdentityView {
    AgentIdentityView {
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
    }
}

fn sample_execution() -> ExecutionSnapshot {
    let profile = ExecutionProfile::default();
    ExecutionSnapshot {
        profile: profile.clone(),
        policy: profile.policy_snapshot(),
        attached_workspaces: vec![],
        workspace_id: None,
        workspace_anchor: PathBuf::from("/workspace"),
        execution_root: PathBuf::from("/workspace"),
        cwd: PathBuf::from("/workspace"),
        execution_root_id: Some("canonical_root:workspace".into()),
        projection_kind: Some(WorkspaceProjectionKind::CanonicalRoot),
        access_mode: Some(WorkspaceAccessMode::SharedRead),
        worktree_root: None,
    }
}

fn test_config() -> ContextConfig {
    ContextConfig {
        recent_messages: 6,
        recent_briefs: 6,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 6,
        max_relevant_episodes: 2,
    }
}

fn render_context_snapshot(
    storage: &AppStorage,
    session: &AgentState,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
) -> Result<String> {
    storage.write_agent(session)?;
    let prompt = build_effective_prompt(
        storage,
        session,
        &sample_execution(),
        current_message,
        &test_config(),
        Path::new("/workspace"),
        Path::new("/tmp/agent-home"),
        &sample_identity(),
        LoadedAgentsMd::default(),
        &SkillsRuntimeView::default(),
        &[],
        continuation,
    )?;
    Ok(prompt.rendered_context_attachment)
}

fn assert_snapshot(actual: &str, expected: &str) {
    assert_eq!(actual, expected);
}

#[test]
fn operator_turn_context_snapshot_includes_work_memory_and_active_work() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut work_item = WorkItemRecord::new(
        "default",
        "Ship prompt snapshot coverage",
        WorkItemState::Open,
    );
    work_item.id = "work_prompt".into();
    work_item.blocked_by = Some("baseline operator snapshot first".into());
    storage.append_work_item(&work_item)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        work_item.id.clone(),
        vec![
            WorkPlanItem {
                step: "Capture baseline operator layout".into(),
                status: WorkPlanStepStatus::InProgress,
            },
            WorkPlanItem {
                step: "Cover callback and task result surfaces".into(),
                status: WorkPlanStepStatus::Pending,
            },
        ],
    ))?;

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue the prompt snapshot work and note any missing surfaces.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );

    let mut session = AgentState::new("default");
    session.current_work_item_id = Some(work_item.id.clone());
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        current_work_item_id: Some(work_item.id.clone()),
        delivery_target: Some(work_item.delivery_target.clone()),
        work_summary: Some("prompt snapshot coverage".into()),
        current_plan: vec!["capture operator surface snapshot".into()],
        ..WorkingMemorySnapshot::default()
    };
    session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
        from_revision: 2,
        to_revision: 3,
        created_at_turn: 4,
        reason: holon::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
        changed_fields: vec!["current_plan".into()],
        summary_lines: vec![
            "captured the baseline operator turn layout".into(),
            "queued callback and task-result follow-up snapshots".into(),
        ],
    });

    let rendered = render_context_snapshot(&storage, &session, &current_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## working_memory
Working memory:
- Current work item id: work_prompt
- Delivery target: Ship prompt snapshot coverage
- Work summary: prompt snapshot coverage
- Current plan:
  - capture operator surface snapshot

## working_memory_delta
Working memory updated since the last prompt:
- Revision: 2 -> 3
- Reason: terminal_turn_completed
- Changed fields:
  - current_plan
- Summary:
  - captured the baseline operator turn layout
  - queued callback and task-result follow-up snapshots

## current_work_item
Current work item:
- Id: work_prompt
- State: Open
- Delivery target: Ship prompt snapshot coverage
- Blocked by: baseline operator snapshot first
- Current work plan:
  - [InProgress] Capture baseline operator layout
  - [Pending] Cover callback and task result surfaces

## context_contract
{CONTEXT_CONTRACT}

## current_input
Current input:
- [operator][cli_prompt][local_process][operator_instruction][OperatorPrompt]
  Continue the prompt snapshot work and note any missing surfaces."#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn system_tick_context_snapshot_renders_wake_continuation() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut system_tick = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "wake_hint".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Next,
        MessageBody::Text {
            text: "wake hint: github inbox updated".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    system_tick.metadata = Some(json!({
        "wake_hint": {
            "reason": "github inbox updated",
            "source": "agentinbox",
            "resource": "interest/pr-reviews",
            "content_type": "application/json",
            "body": {
                "type": "json",
                "value": {
                    "notification_type": "pr_review_requested",
                    "pr": 465
                }
            }
        }
    }));

    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::SystemTick,
        class: ContinuationClass::ResumeExpectedWait,
        model_visible: true,
        prior_closure_outcome: holon::types::ClosureOutcome::Waiting,
        prior_waiting_reason: Some(WaitingReason::AwaitingExternalChange),
        matched_waiting_reason: true,
        evidence: vec![],
    };

    let rendered = render_context_snapshot(
        &storage,
        &AgentState::new("default"),
        &system_tick,
        Some(&continuation),
    )?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## context_contract
{CONTEXT_CONTRACT}

## continuation_context
Continuation context:
 - Trigger kind: system_tick
 - Continuation class: resume_expected_wait
 - Prior closure outcome: waiting
 - Prior waiting reason: awaiting_external_change
 - Waiting reason matched: true
 - Wake hint:
- Source: agentinbox
- Resource: interest/pr-reviews
- Reason: github inbox updated
- Content-Type: application/json
- Payload:
{{
  "notification_type": "pr_review_requested",
  "pr": 465
}}

## current_input
Current input:
- [system][runtime_system][runtime_owned][runtime_instruction][SystemTick]
  wake hint: github inbox updated"#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn callback_turn_context_snapshot_preserves_provenance_labels() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let callback_message = MessageEnvelope::new(
        "default",
        MessageKind::CallbackEvent,
        MessageOrigin::Callback {
            descriptor_id: "cb_pr_review".into(),
            source: Some("github".into()),
        },
        TrustLevel::TrustedIntegration,
        Priority::Normal,
        MessageBody::Text {
            text: "CI completed success for PR #465.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpCallbackEnqueue,
        AdmissionContext::ExternalTriggerCapability,
    );

    let rendered = render_context_snapshot(
        &storage,
        &AgentState::new("default"),
        &callback_message,
        None,
    )?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## context_contract
{CONTEXT_CONTRACT}

## current_input
Current input:
- [callback][http_callback_enqueue][external_trigger_capability][integration_signal][CallbackEvent]
  CI completed success for PR #465."#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn task_result_context_snapshot_renders_follow_up_continuation() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let task_result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task_exec_1".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Next,
        MessageBody::Text {
            text: "Command task completed successfully: cargo test runtime_flow".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );

    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::TaskResult,
        class: ContinuationClass::ResumeExpectedWait,
        model_visible: true,
        prior_closure_outcome: holon::types::ClosureOutcome::Waiting,
        prior_waiting_reason: Some(WaitingReason::AwaitingTaskResult),
        matched_waiting_reason: true,
        evidence: vec![],
    };

    let rendered = render_context_snapshot(
        &storage,
        &AgentState::new("default"),
        &task_result,
        Some(&continuation),
    )?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## context_contract
{CONTEXT_CONTRACT}

## continuation_context
Continuation context:
 - Trigger kind: task_result
 - Continuation class: resume_expected_wait
 - Prior closure outcome: waiting
 - Prior waiting reason: awaiting_task_result
 - Waiting reason matched: true

## current_input
Current input:
- [task][task_rejoin][runtime_owned][runtime_instruction][TaskResult]
  Command task completed successfully: cargo test runtime_flow"#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn active_work_with_queued_work_shows_both_items() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    // Create an current work item
    let mut active_work = WorkItemRecord::new(
        "default",
        "Complete snapshot coverage expansion",
        WorkItemState::Open,
    );
    active_work.id = "work_active".into();
    active_work.blocked_by = Some("currently adding queued work interaction tests".into());
    storage.append_work_item(&active_work)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        active_work.id.clone(),
        vec![
            WorkPlanItem {
                step: "Add active work with queued work test".into(),
                status: WorkPlanStepStatus::InProgress,
            },
            WorkPlanItem {
                step: "Add post-compaction snapshot tests".into(),
                status: WorkPlanStepStatus::Pending,
            },
        ],
    ))?;

    // Create a queued work item
    let mut queued_work =
        WorkItemRecord::new("default", "Review and merge PR #485", WorkItemState::Open);
    queued_work.id = "work_queued".into();
    queued_work.blocked_by = Some("blocked on active work completion".into());
    storage.append_work_item(&queued_work)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        queued_work.id.clone(),
        vec![
            WorkPlanItem {
                step: "Review expanded snapshot coverage changes".into(),
                status: WorkPlanStepStatus::Pending,
            },
            WorkPlanItem {
                step: "Verify tests pass".into(),
                status: WorkPlanStepStatus::Pending,
            },
        ],
    ))?;

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue with the snapshot expansion work.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );

    let mut session = AgentState::new("default");
    session.current_work_item_id = Some(active_work.id.clone());
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        current_work_item_id: Some(active_work.id.clone()),
        delivery_target: Some(active_work.delivery_target.clone()),
        work_summary: Some("expand prompt context snapshot coverage".into()),
        current_plan: vec!["add active work with queued work test".into()],
        ..WorkingMemorySnapshot::default()
    };

    let rendered = render_context_snapshot(&storage, &session, &current_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## working_memory
Working memory:
- Current work item id: work_active
- Delivery target: Complete snapshot coverage expansion
- Work summary: expand prompt context snapshot coverage
- Current plan:
  - add active work with queued work test

## current_work_item
Current work item:
- Id: work_active
- State: Open
- Delivery target: Complete snapshot coverage expansion
- Blocked by: currently adding queued work interaction tests
- Current work plan:
  - [InProgress] Add active work with queued work test
  - [Pending] Add post-compaction snapshot tests

## queued_blocked_work_items
Queued and blocked work items:
- [blocked] work_queued :: Review and merge PR #485 :: blocked_by=blocked on active work completion

## context_contract
{CONTEXT_CONTRACT}

## current_input
Current input:
- [operator][cli_prompt][local_process][operator_instruction][OperatorPrompt]
  Continue with the snapshot expansion work."#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn operator_turn_without_working_memory_delta() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut work_item = WorkItemRecord::new("default", "Test delta absence", WorkItemState::Open);
    work_item.id = "work_no_delta".into();
    work_item.blocked_by = Some("verifying snapshot without delta".into());
    storage.append_work_item(&work_item)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        work_item.id.clone(),
        vec![WorkPlanItem {
            step: "Verify delta absence".into(),
            status: WorkPlanStepStatus::InProgress,
        }],
    ))?;

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue testing without delta.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );

    let mut session = AgentState::new("default");
    session.current_work_item_id = Some(work_item.id.clone());
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        current_work_item_id: Some(work_item.id.clone()),
        delivery_target: Some(work_item.delivery_target.clone()),
        work_summary: Some("test working memory delta absence".into()),
        current_plan: vec!["verify delta absence".into()],
        ..WorkingMemorySnapshot::default()
    };
    // No pending_working_memory_delta set

    let rendered = render_context_snapshot(&storage, &session, &current_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## working_memory
Working memory:
- Current work item id: work_no_delta
- Delivery target: Test delta absence
- Work summary: test working memory delta absence
- Current plan:
  - verify delta absence

## current_work_item
Current work item:
- Id: work_no_delta
- State: Open
- Delivery target: Test delta absence
- Blocked by: verifying snapshot without delta
- Current work plan:
  - [InProgress] Verify delta absence

## context_contract
{CONTEXT_CONTRACT}

## current_input
Current input:
- [operator][cli_prompt][local_process][operator_instruction][OperatorPrompt]
  Continue testing without delta."#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn callback_with_active_work_and_delta() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut work_item = WorkItemRecord::new("default", "Handle CI callback", WorkItemState::Open);
    work_item.id = "work_ci".into();
    work_item.blocked_by = Some("awaiting CI result".into());
    storage.append_work_item(&work_item)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        work_item.id.clone(),
        vec![
            WorkPlanItem {
                step: "Wait for CI callback".into(),
                status: WorkPlanStepStatus::Completed,
            },
            WorkPlanItem {
                step: "Process CI result".into(),
                status: WorkPlanStepStatus::InProgress,
            },
            WorkPlanItem {
                step: "Update work item status".into(),
                status: WorkPlanStepStatus::Pending,
            },
        ],
    ))?;

    let callback_message = MessageEnvelope::new(
        "default",
        MessageKind::CallbackEvent,
        MessageOrigin::Callback {
            descriptor_id: "cb_ci_result".into(),
            source: Some("github_actions".into()),
        },
        TrustLevel::TrustedIntegration,
        Priority::Normal,
        MessageBody::Text {
            text: "CI pipeline completed successfully for commit abc123.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpCallbackEnqueue,
        AdmissionContext::ExternalTriggerCapability,
    );

    let mut session = AgentState::new("default");
    session.current_work_item_id = Some(work_item.id.clone());
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        current_work_item_id: Some(work_item.id.clone()),
        delivery_target: Some(work_item.delivery_target.clone()),
        work_summary: Some("process CI completion callback".into()),
        current_plan: vec![
            "wait for CI callback".into(),
            "process CI result".into(),
            "update work item status".into(),
        ],
        ..WorkingMemorySnapshot::default()
    };
    session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
        from_revision: 5,
        to_revision: 6,
        created_at_turn: 12,
        reason: holon::types::WorkingMemoryUpdateReason::TaskRejoined,
        changed_fields: vec!["current_plan".into(), "waiting_on".into()],
        summary_lines: vec![
            "CI pipeline completed successfully".into(),
            "ready to proceed with next work item".into(),
        ],
    });

    let rendered = render_context_snapshot(&storage, &session, &callback_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## working_memory
Working memory:
- Current work item id: work_ci
- Delivery target: Handle CI callback
- Work summary: process CI completion callback
- Current plan:
  - wait for CI callback
  - process CI result
  - update work item status

## working_memory_delta
Working memory updated since the last prompt:
- Revision: 5 -> 6
- Reason: task_rejoined
- Changed fields:
  - current_plan
  - waiting_on
- Summary:
  - CI pipeline completed successfully
  - ready to proceed with next work item

## current_work_item
Current work item:
- Id: work_ci
- State: Open
- Delivery target: Handle CI callback
- Blocked by: awaiting CI result
- Current work plan:
  - [Completed] Wait for CI callback
  - [InProgress] Process CI result
  - [Pending] Update work item status

## context_contract
{CONTEXT_CONTRACT}

## current_input
Current input:
- [callback][http_callback_enqueue][external_trigger_capability][integration_signal][CallbackEvent]
  CI pipeline completed successfully for commit abc123."#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn system_tick_with_waiting_work_item() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut waiting_work = WorkItemRecord::new(
        "default",
        "External service integration",
        WorkItemState::Open,
    );
    waiting_work.id = "work_waiting".into();
    waiting_work.blocked_by = Some("blocked on API rate limit".into());
    storage.append_work_item(&waiting_work)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        waiting_work.id.clone(),
        vec![
            WorkPlanItem {
                step: "Wait for rate limit reset".into(),
                status: WorkPlanStepStatus::InProgress,
            },
            WorkPlanItem {
                step: "Retry API request".into(),
                status: WorkPlanStepStatus::Pending,
            },
        ],
    ))?;

    let mut system_tick = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "wake_hint".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Next,
        MessageBody::Text {
            text: "wake hint: rate limit reset".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    system_tick.metadata = Some(json!({
        "wake_hint": {
            "reason": "rate limit reset",
            "source": "timer",
            "resource": "rate_limit/external_api",
            "content_type": "text/plain",
            "body": {
                "type": "text",
                "text": "Rate limit reset, ready to retry"
            }
        }
    }));

    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::SystemTick,
        class: ContinuationClass::ResumeExpectedWait,
        model_visible: true,
        prior_closure_outcome: holon::types::ClosureOutcome::Waiting,
        prior_waiting_reason: Some(WaitingReason::AwaitingExternalChange),
        matched_waiting_reason: true,
        evidence: vec![],
    };

    let mut session = AgentState::new("default");
    session.current_work_item_id = Some(waiting_work.id.clone());
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        current_work_item_id: Some(waiting_work.id.clone()),
        delivery_target: Some(waiting_work.delivery_target.clone()),
        work_summary: Some("waiting for external service response".into()),
        current_plan: vec![
            "wait for rate limit reset".into(),
            "retry API request".into(),
        ],
        ..WorkingMemorySnapshot::default()
    };

    let rendered = render_context_snapshot(&storage, &session, &system_tick, Some(&continuation))?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## working_memory
Working memory:
- Current work item id: work_waiting
- Delivery target: External service integration
- Work summary: waiting for external service response
- Current plan:
  - wait for rate limit reset
  - retry API request

## current_work_item
Current work item:
- Id: work_waiting
- State: Open
- Delivery target: External service integration
- Blocked by: blocked on API rate limit
- Current work plan:
  - [InProgress] Wait for rate limit reset
  - [Pending] Retry API request

## context_contract
{CONTEXT_CONTRACT}

## continuation_context
Continuation context:
 - Trigger kind: system_tick
 - Continuation class: resume_expected_wait
 - Prior closure outcome: waiting
 - Prior waiting reason: awaiting_external_change
 - Waiting reason matched: true
 - Wake hint:
- Source: timer
- Resource: rate_limit/external_api
- Reason: rate limit reset
- Content-Type: text/plain
- Payload:
Rate limit reset, ready to retry

## current_input
Current input:
- [system][runtime_system][runtime_owned][runtime_instruction][SystemTick]
  wake hint: rate limit reset"#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn post_compaction_snapshot_preserves_continuity() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut work_item = WorkItemRecord::new(
        "default",
        "Long-running task with compaction",
        WorkItemState::Open,
    );
    work_item.id = "work_compaction".into();
    work_item.blocked_by = Some("continuing after compaction".into());
    storage.append_work_item(&work_item)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        work_item.id.clone(),
        vec![
            WorkPlanItem {
                step: "Complete initial phase".into(),
                status: WorkPlanStepStatus::Completed,
            },
            WorkPlanItem {
                step: "Work on expanded coverage".into(),
                status: WorkPlanStepStatus::InProgress,
            },
            WorkPlanItem {
                step: "Final verification".into(),
                status: WorkPlanStepStatus::Pending,
            },
        ],
    ))?;

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue with the expanded coverage work after compaction.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );

    let mut session = AgentState::new("default");
    // Simulate post-compaction state with higher revision numbers
    session.working_memory.working_memory_revision = 8;
    session.current_work_item_id = Some(work_item.id.clone());
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        current_work_item_id: Some(work_item.id.clone()),
        delivery_target: Some(work_item.delivery_target.clone()),
        work_summary: Some("task spanning multiple compaction points".into()),
        current_plan: vec![
            "complete initial phase".into(),
            "work on expanded coverage".into(),
            "final verification".into(),
        ],
        ..WorkingMemorySnapshot::default()
    };
    session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
        from_revision: 7,
        to_revision: 8,
        created_at_turn: 15,
        reason: holon::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
        changed_fields: vec!["working_memory_revision".into()],
        summary_lines: vec![
            "compaction applied, context compressed".into(),
            "continuity preserved through working memory snapshot".into(),
        ],
    });

    let rendered = render_context_snapshot(&storage, &session, &current_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## working_memory
Working memory:
- Current work item id: work_compaction
- Delivery target: Long-running task with compaction
- Work summary: task spanning multiple compaction points
- Current plan:
  - complete initial phase
  - work on expanded coverage
  - final verification

## working_memory_delta
Working memory updated since the last prompt:
- Revision: 7 -> 8
- Reason: terminal_turn_completed
- Changed fields:
  - working_memory_revision
- Summary:
  - compaction applied, context compressed
  - continuity preserved through working memory snapshot

## current_work_item
Current work item:
- Id: work_compaction
- State: Open
- Delivery target: Long-running task with compaction
- Blocked by: continuing after compaction
- Current work plan:
  - [Completed] Complete initial phase
  - [InProgress] Work on expanded coverage
  - [Pending] Final verification

## context_contract
{CONTEXT_CONTRACT}

## current_input
Current input:
- [operator][cli_prompt][local_process][operator_instruction][OperatorPrompt]
  Continue with the expanded coverage work after compaction."#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn task_result_with_multiple_work_items() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    // Create completed work item
    let mut completed_work =
        WorkItemRecord::new("default", "Build task execution", WorkItemState::Done);
    completed_work.id = "work_build".into();
    storage.append_work_item(&completed_work)?;

    // Create current work item
    let mut active_work = WorkItemRecord::new(
        "default",
        "Test execution and verification",
        WorkItemState::Open,
    );
    active_work.id = "work_test".into();
    active_work.blocked_by = Some("awaiting test completion".into());
    storage.append_work_item(&active_work)?;
    storage.append_work_plan(&WorkPlanSnapshot::new(
        "default",
        active_work.id.clone(),
        vec![
            WorkPlanItem {
                step: "Execute cargo test".into(),
                status: WorkPlanStepStatus::Completed,
            },
            WorkPlanItem {
                step: "Verify test results".into(),
                status: WorkPlanStepStatus::InProgress,
            },
            WorkPlanItem {
                step: "Document any failures".into(),
                status: WorkPlanStepStatus::Pending,
            },
        ],
    ))?;

    let task_result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task_cargo_test".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Next,
        MessageBody::Text {
            text: "Test task completed: 120 tests passed, 0 failed".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );

    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::TaskResult,
        class: ContinuationClass::ResumeExpectedWait,
        model_visible: true,
        prior_closure_outcome: holon::types::ClosureOutcome::Waiting,
        prior_waiting_reason: Some(WaitingReason::AwaitingTaskResult),
        matched_waiting_reason: true,
        evidence: vec![],
    };

    let mut session = AgentState::new("default");
    session.current_work_item_id = Some(active_work.id.clone());
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        current_work_item_id: Some(active_work.id.clone()),
        delivery_target: Some(active_work.delivery_target.clone()),
        work_summary: Some("run cargo test and verify results".into()),
        current_plan: vec![
            "execute cargo test".into(),
            "verify test results".into(),
            "document any failures".into(),
        ],
        ..WorkingMemorySnapshot::default()
    };
    session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
        from_revision: 3,
        to_revision: 4,
        created_at_turn: 8,
        reason: holon::types::WorkingMemoryUpdateReason::TaskRejoined,
        changed_fields: vec!["current_plan".into()],
        summary_lines: vec![
            "cargo test completed successfully".into(),
            "120 tests passed, 0 failed".into(),
            "ready to proceed with verification".into(),
        ],
    });

    let rendered = render_context_snapshot(&storage, &session, &task_result, Some(&continuation))?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## working_memory
Working memory:
- Current work item id: work_test
- Delivery target: Test execution and verification
- Work summary: run cargo test and verify results
- Current plan:
  - execute cargo test
  - verify test results
  - document any failures

## working_memory_delta
Working memory updated since the last prompt:
- Revision: 3 -> 4
- Reason: task_rejoined
- Changed fields:
  - current_plan
- Summary:
  - cargo test completed successfully
  - 120 tests passed, 0 failed
  - ready to proceed with verification

## current_work_item
Current work item:
- Id: work_test
- State: Open
- Delivery target: Test execution and verification
- Blocked by: awaiting test completion
- Current work plan:
  - [Completed] Execute cargo test
  - [InProgress] Verify test results
  - [Pending] Document any failures

## context_contract
{CONTEXT_CONTRACT}

## continuation_context
Continuation context:
 - Trigger kind: task_result
 - Continuation class: resume_expected_wait
 - Prior closure outcome: waiting
 - Prior waiting reason: awaiting_task_result
 - Waiting reason matched: true

## current_input
Current input:
- [task][task_rejoin][runtime_owned][runtime_instruction][TaskResult]
  Test task completed: 120 tests passed, 0 failed"#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}
