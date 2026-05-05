use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
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
        SkillsRuntimeView, TodoItem, TodoItemState, TrustLevel, WaitingReason, WorkItemRecord,
        WorkItemState, WorkingMemoryDelta, WorkingMemorySnapshot,
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

const CONTEXT_CONTRACT: &str = r#"Interpret the memory block with this priority: current work item objective first, durable plan second, todo_list third, working memory delta next, and rolling working memory after that. This is an interpretation priority, not a guarantee about section ordering. Use prior briefs and recent tool results as continuity evidence across turns. When sources differ on task scope, treat the current work item's `objective` and `plan` as the ground truth unless the current input explicitly changes it."#;

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

fn append_work_item_todo(
    storage: &AppStorage,
    work_item_id: String,
    todo_list: Vec<TodoItem>,
) -> Result<()> {
    let Some(mut work_item) = storage.latest_work_item(&work_item_id)? else {
        anyhow::bail!("missing work item {work_item_id}");
    };
    work_item.todo_list = todo_list;
    work_item.updated_at = Utc::now();
    storage.append_work_item(&work_item)?;
    Ok(())
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
    append_work_item_todo(
        &storage,
        work_item.id.clone(),
        vec![
            TodoItem {
                text: "Capture baseline operator layout".into(),
                state: TodoItemState::InProgress,
            },
            TodoItem {
                text: "Cover callback and task result surfaces".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

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
        objective: Some(work_item.objective.clone()),
        work_summary: Some("prompt snapshot coverage".into()),
        plan: Some(vec!["capture operator surface snapshot"].join("\n")),
        ..WorkingMemorySnapshot::default()
    };
    session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
        from_revision: 2,
        to_revision: 3,
        created_at_turn: 4,
        reason: holon::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
        changed_fields: vec!["plan".into()],
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
- Objective: Ship prompt snapshot coverage
- Work summary: prompt snapshot coverage
- Plan:
  capture operator surface snapshot

## working_memory_delta
Working memory updated since the last prompt:
- Revision: 2 -> 3
- Reason: terminal_turn_completed
- Changed fields:
  - plan
- Summary:
  - captured the baseline operator turn layout
  - queued callback and task-result follow-up snapshots

## current_work_item
Current work item:
- Id: work_prompt
- State: Open
- Readiness: Blocked
- Objective: Ship prompt snapshot coverage
- Plan status: draft
- Todo list:
  - [in_progress] Capture baseline operator layout
  - [pending] Cover callback and task result surfaces
- Blocked by: baseline operator snapshot first

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
            "description": "Check AgentInbox for unread GitHub review notifications",
            "source": "agentinbox",
            "scope": "agent",
            "external_trigger_id": "trig_agentinbox_reviews",
            "waiting_intent_id": "wait_agentinbox_reviews",
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
- Scope: agent
- External trigger id: trig_agentinbox_reviews
- Waiting intent id: wait_agentinbox_reviews
- Description: Check AgentInbox for unread GitHub review notifications
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
    append_work_item_todo(
        &storage,
        active_work.id.clone(),
        vec![
            TodoItem {
                text: "Add active work with queued work test".into(),
                state: TodoItemState::InProgress,
            },
            TodoItem {
                text: "Add post-compaction snapshot tests".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

    // Create a queued work item
    let mut queued_work =
        WorkItemRecord::new("default", "Review and merge PR #485", WorkItemState::Open);
    queued_work.id = "work_queued".into();
    queued_work.blocked_by = Some("blocked on active work completion".into());
    storage.append_work_item(&queued_work)?;
    append_work_item_todo(
        &storage,
        queued_work.id.clone(),
        vec![
            TodoItem {
                text: "Review expanded snapshot coverage changes".into(),
                state: TodoItemState::Pending,
            },
            TodoItem {
                text: "Verify tests pass".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

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
        objective: Some(active_work.objective.clone()),
        work_summary: Some("expand prompt context snapshot coverage".into()),
        plan: Some(vec!["add active work with queued work test"].join("\n")),
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
- Objective: Complete snapshot coverage expansion
- Work summary: expand prompt context snapshot coverage
- Plan:
  add active work with queued work test

## current_work_item
Current work item:
- Id: work_active
- State: Open
- Readiness: Blocked
- Objective: Complete snapshot coverage expansion
- Plan status: draft
- Todo list:
  - [in_progress] Add active work with queued work test
  - [pending] Add post-compaction snapshot tests
- Blocked by: currently adding queued work interaction tests

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
    append_work_item_todo(
        &storage,
        work_item.id.clone(),
        vec![TodoItem {
            text: "Verify delta absence".into(),
            state: TodoItemState::InProgress,
        }],
    )?;

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
        objective: Some(work_item.objective.clone()),
        work_summary: Some("test working memory delta absence".into()),
        plan: Some(vec!["verify delta absence"].join("\n")),
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
- Objective: Test delta absence
- Work summary: test working memory delta absence
- Plan:
  verify delta absence

## current_work_item
Current work item:
- Id: work_no_delta
- State: Open
- Readiness: Blocked
- Objective: Test delta absence
- Plan status: draft
- Todo list:
  - [in_progress] Verify delta absence
- Blocked by: verifying snapshot without delta

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
    append_work_item_todo(
        &storage,
        work_item.id.clone(),
        vec![
            TodoItem {
                text: "Wait for CI callback".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "Process CI result".into(),
                state: TodoItemState::InProgress,
            },
            TodoItem {
                text: "Update work item status".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

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
        objective: Some(work_item.objective.clone()),
        work_summary: Some("process CI completion callback".into()),
        plan: Some(
            vec![
                "wait for CI callback",
                "process CI result",
                "update work item status",
            ]
            .join("\n"),
        ),
        ..WorkingMemorySnapshot::default()
    };
    session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
        from_revision: 5,
        to_revision: 6,
        created_at_turn: 12,
        reason: holon::types::WorkingMemoryUpdateReason::TaskRejoined,
        changed_fields: vec!["plan".into(), "waiting_on".into()],
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
- Objective: Handle CI callback
- Work summary: process CI completion callback
- Plan:
  wait for CI callback process CI result update work item status

## working_memory_delta
Working memory updated since the last prompt:
- Revision: 5 -> 6
- Reason: task_rejoined
- Changed fields:
  - plan
  - waiting_on
- Summary:
  - CI pipeline completed successfully
  - ready to proceed with next work item

## current_work_item
Current work item:
- Id: work_ci
- State: Open
- Readiness: Blocked
- Objective: Handle CI callback
- Plan status: draft
- Todo list:
  - [completed] Wait for CI callback
  - [in_progress] Process CI result
  - [pending] Update work item status
- Blocked by: awaiting CI result

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
    append_work_item_todo(
        &storage,
        waiting_work.id.clone(),
        vec![
            TodoItem {
                text: "Wait for rate limit reset".into(),
                state: TodoItemState::InProgress,
            },
            TodoItem {
                text: "Retry API request".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

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
        objective: Some(waiting_work.objective.clone()),
        work_summary: Some("waiting for external service response".into()),
        plan: Some(vec!["wait for rate limit reset", "retry API request"].join("\n")),
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
- Objective: External service integration
- Work summary: waiting for external service response
- Plan:
  wait for rate limit reset retry API request

## current_work_item
Current work item:
- Id: work_waiting
- State: Open
- Readiness: Blocked
- Objective: External service integration
- Plan status: draft
- Todo list:
  - [in_progress] Wait for rate limit reset
  - [pending] Retry API request
- Blocked by: blocked on API rate limit

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
    append_work_item_todo(
        &storage,
        work_item.id.clone(),
        vec![
            TodoItem {
                text: "Complete initial phase".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "Work on expanded coverage".into(),
                state: TodoItemState::InProgress,
            },
            TodoItem {
                text: "Final verification".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

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
        objective: Some(work_item.objective.clone()),
        work_summary: Some("task spanning multiple compaction points".into()),
        plan: Some(
            vec![
                "complete initial phase",
                "work on expanded coverage",
                "final verification",
            ]
            .join("\n"),
        ),
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
- Objective: Long-running task with compaction
- Work summary: task spanning multiple compaction points
- Plan:
  complete initial phase work on expanded coverage final verification

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
- Readiness: Blocked
- Objective: Long-running task with compaction
- Plan status: draft
- Todo list:
  - [completed] Complete initial phase
  - [in_progress] Work on expanded coverage
  - [pending] Final verification
- Blocked by: continuing after compaction

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
        WorkItemRecord::new("default", "Build task execution", WorkItemState::Completed);
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
    append_work_item_todo(
        &storage,
        active_work.id.clone(),
        vec![
            TodoItem {
                text: "Execute cargo test".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "Verify test results".into(),
                state: TodoItemState::InProgress,
            },
            TodoItem {
                text: "Document any failures".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

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
        objective: Some(active_work.objective.clone()),
        work_summary: Some("run cargo test and verify results".into()),
        plan: Some(
            vec![
                "execute cargo test",
                "verify test results",
                "document any failures",
            ]
            .join("\n"),
        ),
        ..WorkingMemorySnapshot::default()
    };
    session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
        from_revision: 3,
        to_revision: 4,
        created_at_turn: 8,
        reason: holon::types::WorkingMemoryUpdateReason::TaskRejoined,
        changed_fields: vec!["plan".into()],
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
- Objective: Test execution and verification
- Work summary: run cargo test and verify results
- Plan:
  execute cargo test verify test results document any failures

## working_memory_delta
Working memory updated since the last prompt:
- Revision: 3 -> 4
- Reason: task_rejoined
- Changed fields:
  - plan
- Summary:
  - cargo test completed successfully
  - 120 tests passed, 0 failed
  - ready to proceed with verification

## current_work_item
Current work item:
- Id: work_test
- State: Open
- Readiness: Blocked
- Objective: Test execution and verification
- Plan status: draft
- Todo list:
  - [completed] Execute cargo test
  - [in_progress] Verify test results
  - [pending] Document any failures
- Blocked by: awaiting test completion

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
