use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use chrono::Utc;
use holon::{
    context::ContextConfig,
    prompt::build_effective_prompt,
    storage::AppStorage,
    system::{ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AdmissionContext, AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentRegistryStatus, AgentState, AgentVisibility, AuthorityClass, BriefKind, BriefRecord,
        ContinuationClass, ContinuationResolution, ContinuationTriggerKind, LoadedAgentsMd,
        MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        SkillsRuntimeView, TodoItem, TodoItemState, ToolExecutionRecord, ToolExecutionStatus,
        WaitingReason, WorkItemRecord, WorkItemState, WorkingMemorySnapshot,
    },
};
use serde_json::{json, Value};
use tempfile::tempdir;

const EXECUTION_ENVIRONMENT: &str = r#"Execution environment summary (policy snapshot; host-local is not a strong sandbox guarantee):
Backend: host_local
Process execution exposed: true
Background tasks supported: true
Managed worktrees supported: true
Projection kind: canonical_root
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
  - cwd_rooting: runtime_shaped
  - projection_rooting: hard_enforced
  - path_confinement: not_enforced
  - write_confinement: not_enforced
  - network_confinement: not_enforced
  - secret_isolation: not_enforced
  - child_process_containment: not_enforced"#;

const CONTEXT_CONTRACT: &str = r#"Interpret the memory block with this priority: current work item objective first, durable plan artifact second, todo_list third, and current work refs after that. This is an interpretation priority, not a guarantee about section ordering. Use prior briefs and recent tool results as continuity evidence across turns. When sources differ on task scope, treat the current work item's `objective` and plan artifact as the ground truth unless the current input explicitly changes it."#;

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
        ..ContextConfig::default()
    }
}

fn render_context_snapshot(
    storage: &AppStorage,
    session: &AgentState,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
) -> Result<String> {
    render_context_snapshot_named(storage, session, current_message, continuation, None)
}

fn render_context_snapshot_named(
    storage: &AppStorage,
    session: &AgentState,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    scenario_name: Option<&str>,
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
        holon::tool::ApplyPatchSurface::UnifiedDiffJson,
        continuation,
    )?;
    let agent_home = storage.data_dir().display().to_string();
    let rendered = prompt
        .rendered_context_attachment
        .replace(&agent_home, "$AGENT_HOME")
        .replace('\\', "/");
    maybe_dump_prompt_context(scenario_name, &current_message.id, &rendered)?;
    Ok(rendered)
}

fn assert_snapshot(actual: &str, expected: &str) {
    assert_eq!(actual, expected);
}

#[derive(Debug, Clone)]
struct ContextDiagnostics {
    total_chars: usize,
    estimated_tokens: usize,
    sections: Vec<SectionDiagnostics>,
    repeated_line_ratio: f64,
    repeated_5gram_ratio: f64,
}

#[derive(Debug, Clone)]
struct SectionDiagnostics {
    name: String,
    chars: usize,
    estimated_tokens: usize,
    line_count: usize,
}

impl ContextDiagnostics {
    fn section(&self, name: &str) -> Option<&SectionDiagnostics> {
        self.sections.iter().find(|section| section.name == name)
    }

    fn section_share(&self, name: &str) -> f64 {
        let Some(section) = self.section(name) else {
            return 0.0;
        };
        if self.total_chars == 0 {
            0.0
        } else {
            section.chars as f64 / self.total_chars as f64
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "total_chars": self.total_chars,
            "estimated_tokens": self.estimated_tokens,
            "sections": self.sections.iter().map(|section| {
                json!({
                    "name": section.name,
                    "chars": section.chars,
                    "estimated_tokens": section.estimated_tokens,
                    "line_count": section.line_count,
                })
            }).collect::<Vec<_>>(),
            "duplication": {
                "repeated_line_ratio": self.repeated_line_ratio,
                "repeated_5gram_ratio": self.repeated_5gram_ratio,
            },
        })
    }
}

fn analyze_context(rendered: &str) -> ContextDiagnostics {
    let sections = parse_context_sections(rendered);
    ContextDiagnostics {
        total_chars: rendered.chars().count(),
        estimated_tokens: estimated_tokens(rendered),
        sections: sections
            .iter()
            .map(|(name, content)| SectionDiagnostics {
                name: name.clone(),
                chars: content.chars().count(),
                estimated_tokens: estimated_tokens(content),
                line_count: content.lines().count(),
            })
            .collect(),
        repeated_line_ratio: repeated_line_ratio(rendered),
        repeated_5gram_ratio: repeated_ngram_ratio(rendered, 5),
    }
}

fn parse_context_sections(rendered: &str) -> Vec<(String, String)> {
    let mut sections = Vec::<(String, String)>::new();
    let mut current_name: Option<String> = None;
    let mut current_content = Vec::<String>::new();

    for line in rendered.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            if let Some(previous_name) = current_name.replace(name.trim().to_string()) {
                sections.push((previous_name, current_content.join("\n").trim().to_string()));
                current_content.clear();
            }
        } else if current_name.is_some() {
            current_content.push(line.to_string());
        }
    }

    if let Some(name) = current_name {
        sections.push((name, current_content.join("\n").trim().to_string()));
    }

    sections
}

fn section_content(rendered: &str, section_name: &str) -> Option<String> {
    parse_context_sections(rendered)
        .into_iter()
        .find_map(|(name, content)| (name == section_name).then_some(content))
}

fn estimated_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn repeated_line_ratio(text: &str) -> f64 {
    let mut counts = HashMap::<String, usize>::new();
    let mut total = 0usize;
    let mut repeated = 0usize;
    for line in text.lines().map(str::trim).filter(|line| {
        line.len() >= 12
            && !line.starts_with("- Plan artifact:")
            && !line.starts_with("- Plan preview complete:")
    }) {
        total += 1;
        let count = counts.entry(line.to_string()).or_default();
        *count += 1;
        if *count > 1 {
            repeated += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        repeated as f64 / total as f64
    }
}

fn repeated_ngram_ratio(text: &str, n: usize) -> f64 {
    let words = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
                .to_ascii_lowercase()
        })
        .filter(|word| word.len() >= 3)
        .collect::<Vec<_>>();
    if words.len() < n || n == 0 {
        return 0.0;
    }

    let mut seen = HashSet::<String>::new();
    let mut repeated = 0usize;
    let total = words.len() - n + 1;
    for window in words.windows(n) {
        let ngram = window.join(" ");
        if !seen.insert(ngram) {
            repeated += 1;
        }
    }
    repeated as f64 / total as f64
}

fn phrase_count(text: &str, phrase: &str) -> usize {
    text.match_indices(phrase).count()
}

// Manual prompt review mode for context evaluation tests:
//
//   HOLON_PROMPT_CONTEXT_DUMP_DIR=/tmp/holon-context-dumps \
//     cargo test -q --test prompt_context_snapshots multi_turn_context_
//
// The helper writes sanitized rendered-context and diagnostics artifacts for each named
// scenario. Keep dumps outside the repository; they are intentionally not committed.
fn maybe_dump_prompt_context(
    scenario_name: Option<&str>,
    fallback_id: &str,
    rendered: &str,
) -> Result<()> {
    let Ok(output_dir) = env::var("HOLON_PROMPT_CONTEXT_DUMP_DIR") else {
        return Ok(());
    };
    let scenario_name = scenario_name.unwrap_or(fallback_id);
    let file_stem = sanitize_file_stem(scenario_name);
    let output_dir = PathBuf::from(output_dir);
    fs::create_dir_all(&output_dir)?;

    let sanitized = sanitize_prompt_dump(rendered);
    let diagnostics = analyze_context(&sanitized);
    fs::write(
        output_dir.join(format!("{file_stem}.context.txt")),
        sanitized,
    )?;
    fs::write(
        output_dir.join(format!("{file_stem}.diagnostics.json")),
        serde_json::to_string_pretty(&diagnostics.to_json())?,
    )?;
    Ok(())
}

fn sanitize_file_stem(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn sanitize_prompt_dump(rendered: &str) -> String {
    let mut sanitized = String::with_capacity(rendered.len());
    for token in rendered.split_inclusive(char::is_whitespace) {
        let token_body = token.trim_end_matches(char::is_whitespace);
        let trailing_whitespace = &token[token_body.len()..];
        if token_body.contains("/callbacks/") {
            sanitized.push_str("$CALLBACK_URL");
        } else {
            sanitized.push_str(token_body);
        }
        sanitized.push_str(trailing_whitespace);
    }
    sanitized
}

#[test]
fn sanitize_prompt_dump_preserves_formatting_and_redacts_callback_tokens() {
    let rendered = "## default_external_ingress\n  - url: http://127.0.0.1:7878/callbacks/wake/wake-secret\n\t- enqueue: http://127.0.0.1:7878/callbacks/enqueue/enqueue-secret\n  - keep: http://127.0.0.1:7878/not-secret\n";

    let sanitized = sanitize_prompt_dump(rendered);

    assert_eq!(
        sanitized,
        "## default_external_ingress\n  - url: $CALLBACK_URL\n\t- enqueue: $CALLBACK_URL\n  - keep: http://127.0.0.1:7878/not-secret\n"
    );
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
fn recent_turns_snapshot_links_operator_input_to_result_brief() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut previous_operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Run the focused prompt projection test.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    previous_operator.id = "msg_focused_prompt_projection".into();
    storage.append_message(&previous_operator)?;
    let mut result_brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "Focused prompt projection test passed.",
        Some(previous_operator.id.clone()),
        None,
    );
    result_brief.id = "brief_focused_prompt_projection".into();
    storage.append_brief(&result_brief)?;

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue with the next prompt projection case.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );

    let rendered = render_context_snapshot(
        &storage,
        &AgentState::new("default"),
        &current_message,
        None,
    )?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Latest trusted operator input: current_input.
Current input relation: current_input is the latest trusted operator input.

## recent_turns
Recent turns:
- Turn message_seq 1:
  - trigger: trusted operator input
  - operator input full: Run the focused prompt projection test. message_ref=message:msg_focused_prompt_projection
  - produced briefs:
    - Result full:
      Focused prompt projection test passed.
      brief_ref=brief:brief_focused_prompt_projection

## current_input
Current input:
- [operator][cli_prompt][local_process][operator_instruction][OperatorPrompt]
  Continue with the next prompt projection case."#
    );
    assert_snapshot(&rendered, &expected);
    Ok(())
}

#[test]
fn recent_turns_snapshot_links_task_result_continuation_to_operator_turn() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut operator_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Run cargo test runtime_flow and report back.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    operator_message.id = "msg_runtime_flow_operator".into();
    let mut operator_message_with_turn = operator_message.clone();
    operator_message_with_turn.turn_id = Some("turn_op_test".into());
    storage.append_message(&operator_message_with_turn)?;
    let mut turn_index_brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "Captured completion report promotion.",
        None,
        None,
    );
    turn_index_brief.id = "brief_completion_report_promotion".into();
    turn_index_brief.turn_index = Some(1);
    storage.append_brief(&turn_index_brief)?;
    storage.append_tool_execution(&ToolExecutionRecord {
        id: "tool_exec_1".into(),
        agent_id: "default".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: Some("turn_op_test".into()),
        tool_name: "ExecCommand".into(),
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        duration_ms: 42,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: json!({ "fixture": true }),
        output: json!({ "exit": 0 }),
        summary: "Run command: cargo test runtime_flow".into(),
        invocation_surface: Some("commentary".into()),
    })?;

    let mut work_item = WorkItemRecord::new("default", "Track\nruntime flow", WorkItemState::Open);
    work_item.id = "work_runtime_flow".into();
    storage.append_work_item(&work_item)?;

    let mut task_result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task_exec_1".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "Command task completed successfully: cargo test runtime_flow".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    task_result.turn_id = Some("turn_task_result".into());

    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::TaskResult,
        class: ContinuationClass::ResumeExpectedWait,
        model_reentry: true,
        prior_closure_outcome: holon::types::ClosureOutcome::Waiting,
        prior_waiting_reason: Some(WaitingReason::AwaitingTaskResult),
        matched_waiting_reason: true,
        evidence: vec![],
    };

    let rendered = render_context_snapshot(
        &storage,
        &{
            let mut session = AgentState::new("default");
            session.current_work_item_id = Some(work_item.id.clone());
            session
        },
        &task_result,
        Some(&continuation),
    )?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## current_work_item
Current work item:
- Id: work_runtime_flow
- State: Open
- Readiness: Runnable
- Objective: Track
runtime flow
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_runtime_flow/plan.md
- Plan preview complete: true

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Latest trusted operator input: message_seq 1.
Current input relation: current_input is a task-result continuation, not a new operator request. Continue the latest trusted operator input above unless the current WorkItem projection is more specific.

## recent_turns
Recent turns:
- Turn message_seq 1:
  - trigger: trusted operator input
  - continues input: message_seq 1
  - continuation trigger: a task-result continuation
  - operator input full: Run cargo test runtime_flow and report back. message_ref=message:msg_runtime_flow_operator
  - produced briefs:
    - Result full:
      Captured completion report promotion.
      brief_ref=brief:brief_completion_report_promotion
  - tool executions:
    - summary: total=1 success=1 error=0 promoted=0 refs=[tool_execution:tool_exec_1:output]
  - current relation: a task-result continuation
  - current input: Command task completed successfully: cargo test runtime_flow
  - current work item: work_runtime_flow :: Track runtime flow

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
        AuthorityClass::OperatorInstruction,
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
    let rendered = render_context_snapshot(&storage, &session, &current_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## current_work_item
Current work item:
- Id: work_prompt
- State: Open
- Readiness: Blocked
- Objective: Ship prompt snapshot coverage
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_prompt/plan.md
- Plan preview complete: true
- Todo list:
  - [in_progress] Capture baseline operator layout
  - [pending] Cover callback and task result surfaces
- Blocked by: baseline operator snapshot first

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Latest trusted operator input: current_input.
Current input relation: current_input is the latest trusted operator input.

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
        AuthorityClass::RuntimeInstruction,
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
        model_reentry: true,
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

## continuation_anchor
Continuation anchor:
Current input relation: current_input is a runtime system-tick continuation, not a trusted operator request.

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
        AuthorityClass::IntegrationSignal,
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

## continuation_anchor
Continuation anchor:
Current input relation: current_input is an external-event continuation, not a trusted operator request.

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

    let mut task_result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task_exec_1".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "Command task completed successfully: cargo test runtime_flow".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    task_result.turn_id = Some("turn_task_result".into());

    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::TaskResult,
        class: ContinuationClass::ResumeExpectedWait,
        model_reentry: true,
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

## continuation_anchor
Continuation anchor:
Current input relation: current_input is a task-result continuation, not a trusted operator request.

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
        AuthorityClass::OperatorInstruction,
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

## current_work_item
Current work item:
- Id: work_active
- State: Open
- Readiness: Blocked
- Objective: Complete snapshot coverage expansion
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_active/plan.md
- Plan preview complete: true
- Todo list:
  - [in_progress] Add active work with queued work test
  - [pending] Add post-compaction snapshot tests
- Blocked by: currently adding queued work interaction tests

## queued_blocked_work_items
Work item candidates by scheduler ranking:
Blocked work items:
- [blocked] work_queued :: Review and merge PR #485 :: current_todo=Review expanded snapshot coverage changes :: blocked_by=blocked on active work completion
  - Plan artifact: $AGENT_HOME/work-items/work_queued/plan.md
  - Plan preview complete: true

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Latest trusted operator input: current_input.
Current input relation: current_input is the latest trusted operator input.

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
        AuthorityClass::OperatorInstruction,
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
    let rendered = render_context_snapshot(&storage, &session, &current_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## current_work_item
Current work item:
- Id: work_no_delta
- State: Open
- Readiness: Blocked
- Objective: Test delta absence
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_no_delta/plan.md
- Plan preview complete: true
- Todo list:
  - [in_progress] Verify delta absence
- Blocked by: verifying snapshot without delta

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Latest trusted operator input: current_input.
Current input relation: current_input is the latest trusted operator input.

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
        AuthorityClass::IntegrationSignal,
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
    let rendered = render_context_snapshot(&storage, &session, &callback_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## current_work_item
Current work item:
- Id: work_ci
- State: Open
- Readiness: Blocked
- Objective: Handle CI callback
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_ci/plan.md
- Plan preview complete: true
- Todo list:
  - [completed] Wait for CI callback
  - [in_progress] Process CI result
  - [pending] Update work item status
- Blocked by: awaiting CI result

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Current input relation: current_input is an external-event continuation, not a trusted operator request.

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
        AuthorityClass::RuntimeInstruction,
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
        model_reentry: true,
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

## current_work_item
Current work item:
- Id: work_waiting
- State: Open
- Readiness: Blocked
- Objective: External service integration
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_waiting/plan.md
- Plan preview complete: true
- Todo list:
  - [in_progress] Wait for rate limit reset
  - [pending] Retry API request
- Blocked by: blocked on API rate limit

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Current input relation: current_input is a runtime system-tick continuation, not a trusted operator request.

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
        AuthorityClass::OperatorInstruction,
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
    let rendered = render_context_snapshot(&storage, &session, &current_message, None)?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## current_work_item
Current work item:
- Id: work_compaction
- State: Open
- Readiness: Blocked
- Objective: Long-running task with compaction
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_compaction/plan.md
- Plan preview complete: true
- Todo list:
  - [completed] Complete initial phase
  - [in_progress] Work on expanded coverage
  - [pending] Final verification
- Blocked by: continuing after compaction

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Latest trusted operator input: current_input.
Current input relation: current_input is the latest trusted operator input.

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

    let mut task_result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task_cargo_test".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "Test task completed: 120 tests passed, 0 failed".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    task_result.turn_id = Some("turn_task_result".into());

    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::TaskResult,
        class: ContinuationClass::ResumeExpectedWait,
        model_reentry: true,
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
    let rendered = render_context_snapshot(&storage, &session, &task_result, Some(&continuation))?;
    let expected = format!(
        r#"## agent
Agent id: default

## execution_environment
{EXECUTION_ENVIRONMENT}

## current_work_item
Current work item:
- Id: work_test
- State: Open
- Readiness: Blocked
- Objective: Test execution and verification
- Plan status: draft
- Plan artifact: $AGENT_HOME/work-items/work_test/plan.md
- Plan preview complete: true
- Todo list:
  - [completed] Execute cargo test
  - [in_progress] Verify test results
  - [pending] Document any failures
- Blocked by: awaiting test completion

## context_contract
{CONTEXT_CONTRACT}

## continuation_anchor
Continuation anchor:
Current input relation: current_input is a task-result continuation, not a trusted operator request.

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

#[test]
fn multi_turn_context_eval_preserves_long_task_continuity_and_efficiency() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let mut work_item = WorkItemRecord::new(
        "default",
        "Evaluate context continuity for issue 1634",
        WorkItemState::Open,
    );
    work_item.id = "work_context_eval".into();
    storage.append_work_item(&work_item)?;
    append_work_item_todo(
        &storage,
        work_item.id.clone(),
        vec![
            TodoItem {
                text: "Build deterministic long-task prompt fixture".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "Inspect diagnostics and duplicate ratios".into(),
                state: TodoItemState::InProgress,
            },
            TodoItem {
                text: "Emit prompt artifact for manual review".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

    let first_operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Start the multi-turn context quality evaluation and keep fact alpha visible."
                .into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    storage.append_message(&first_operator)?;
    storage.append_brief(&BriefRecord::new(
        "default",
        BriefKind::Result,
        "Started deterministic fixture construction.",
        Some(first_operator.id.clone()),
        None,
    ))?;

    storage.append_tool_execution(&ToolExecutionRecord {
        id: "tool_context_eval".into(),
        agent_id: "default".into(),
        work_item_id: Some(work_item.id.clone()),
        turn_index: 2,
        turn_id: Some("turn_context_eval_tool".into()),
        tool_name: "ExecCommand".into(),
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        duration_ms: 120,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: json!({ "cmd": "cargo test -q --test prompt_context_snapshots" }),
        output: json!({ "exit_status": 0 }),
        summary: "Run command: cargo test -q --test prompt_context_snapshots".into(),
        invocation_surface: Some("commentary".into()),
    })?;

    let task_result = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task_context_eval".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "Focused snapshot test completed; fact beta remains visible.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    storage.append_message(&task_result)?;
    storage.append_brief(&BriefRecord::new(
        "default",
        BriefKind::Result,
        "Diagnostics helper reports stable section sizes.",
        Some(task_result.id.clone()),
        Some("task_context_eval".into()),
    ))?;

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Now review the long task context and confirm fact gamma is still available."
                .into(),
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
        work_summary: Some("long task context eval retains alpha beta gamma facts".into()),
        plan: Some(
            vec![
                "build deterministic long-task prompt fixture",
                "inspect diagnostics and duplicate ratios",
                "emit prompt artifact for manual review",
            ]
            .join("\n"),
        ),
        ..WorkingMemorySnapshot::default()
    };

    let rendered = render_context_snapshot_named(
        &storage,
        &session,
        &current_message,
        None,
        Some("multi_turn_context_long_task"),
    )?;
    let diagnostics = analyze_context(&rendered);
    let current_work_item =
        section_content(&rendered, "current_work_item").expect("current work section");
    let recent_turns = section_content(&rendered, "recent_turns").expect("recent turns section");
    let current_input = section_content(&rendered, "current_input").expect("current input");

    assert!(current_work_item.contains("work_context_eval"));
    assert!(current_work_item.contains("Evaluate context continuity for issue 1634"));
    assert!(current_work_item.contains("[in_progress] Inspect diagnostics and duplicate ratios"));
    assert!(recent_turns.contains("fact alpha visible"));
    assert!(recent_turns.contains("fact beta remains visible"));
    assert!(recent_turns.contains("tool_execution:tool_context_eval:output"));
    assert!(!recent_turns.contains("Run command: cargo test -q --test prompt_context_snapshots"));
    assert!(current_input.contains("fact gamma is still available"));
    assert!(!recent_turns.contains("Plan artifact:"));
    assert!(!recent_turns.contains("[in_progress] Inspect diagnostics and duplicate ratios"));
    assert!(diagnostics.section("current_work_item").is_some());
    assert!(diagnostics.section("recent_turns").is_some());
    assert!(diagnostics.section("current_input").is_some());
    assert!(
        diagnostics.section_share("recent_turns") < 0.45,
        "recent_turns should not dominate the rendered context: {diagnostics:?}"
    );
    assert!(
        diagnostics.repeated_line_ratio < 0.05,
        "line duplication should stay low: {diagnostics:?}"
    );
    assert!(
        diagnostics.repeated_5gram_ratio < 0.20,
        "ngram duplication should stay bounded: {diagnostics:?}"
    );
    assert!(
        phrase_count(&rendered, "Evaluate context continuity for issue 1634") <= 2,
        "authoritative work objective should not be copied throughout the context:\n{rendered}"
    );
    Ok(())
}

#[test]
fn multi_turn_context_eval_preserves_initial_issue_list_during_item_by_item_discussion(
) -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let initial_issue_list = [
        "alpha: turn-local projection repeats stable runtime guidance",
        "beta: tool conclusions may drift when only recent_turns preserves them",
        "gamma: item-by-item discussion can lose the next requested issue",
        "delta: continuation sections duplicate or blur resume semantics",
        "epsilon: callback capability redaction must remain explicit",
    ];

    let mut work_item = WorkItemRecord::new(
        "default",
        "Evaluate prompt context issue-list continuity",
        WorkItemState::Open,
    );
    work_item.id = "work_issue_list_continuity".into();
    storage.append_work_item(&work_item)?;
    append_work_item_todo(
        &storage,
        work_item.id.clone(),
        vec![
            TodoItem {
                text: "alpha: turn-local projection repeats stable runtime guidance".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "beta: tool conclusions may drift when only recent_turns preserves them"
                    .into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "gamma: item-by-item discussion can lose the next requested issue".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "delta: continuation sections duplicate or blur resume semantics".into(),
                state: TodoItemState::Pending,
            },
            TodoItem {
                text: "epsilon: callback capability redaction must remain explicit".into(),
                state: TodoItemState::Pending,
            },
        ],
    )?;

    let first_operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Analyze prompt context improvements and list five stable issues.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    storage.append_message(&first_operator)?;
    storage.append_tool_execution(&ToolExecutionRecord {
        id: "tool_issue_list_analysis".into(),
        agent_id: "default".into(),
        work_item_id: Some(work_item.id.clone()),
        turn_index: 1,
        turn_id: Some("turn_issue_list_analysis".into()),
        tool_name: "ExecCommand".into(),
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        duration_ms: 95,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: json!({ "cmd": "rg -n \"build_effective_prompt|recent_turns\" src tests" }),
        output: json!({ "exit_status": 0 }),
        summary: "Inspected prompt projection and produced a five-item issue list.".into(),
        invocation_surface: Some("commentary".into()),
    })?;
    storage.append_brief(&BriefRecord::new(
        "default",
        BriefKind::Result,
        "Initial issue list was promoted into the active work item and working memory.",
        Some(first_operator.id.clone()),
        None,
    ))?;

    for (turn_index, (prompt, result)) in [
        (
            "Discuss alpha and mark it resolved.",
            "Resolved alpha without restating the full issue list.",
        ),
        (
            "Now discuss beta and mark it resolved.",
            "Resolved beta without restating the full issue list.",
        ),
        (
            "Continue with gamma and mark it resolved.",
            "Resolved gamma without restating the full issue list.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let operator = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:jolestar".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: prompt.into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::CliPrompt,
            AdmissionContext::LocalProcess,
        );
        storage.append_message(&operator)?;
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            result,
            Some(operator.id.clone()),
            None,
        );
        brief.turn_index = Some((turn_index + 2) as u64);
        storage.append_brief(&brief)?;
    }

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Before handling delta, recall the original five issues and mark alpha/beta/gamma discussed while delta/epsilon remain open.".into(),
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
        work_summary: Some(
            "original prompt-context issue list is authoritative in work state".into(),
        ),
        plan: Some("Use the active work item todo list as the authoritative issue list.".into()),
        ..WorkingMemorySnapshot::default()
    };

    let rendered = render_context_snapshot_named(
        &storage,
        &session,
        &current_message,
        None,
        Some("multi_turn_context_issue_list_continuity"),
    )?;
    let diagnostics = analyze_context(&rendered);
    let current_work_item =
        section_content(&rendered, "current_work_item").expect("current work section");
    let recent_turns = section_content(&rendered, "recent_turns").expect("recent turns section");
    let current_input = section_content(&rendered, "current_input").expect("current input");

    for issue in initial_issue_list {
        assert!(
            current_work_item.contains(issue),
            "initial issue should remain in authoritative state: {issue}\n{rendered}"
        );
    }
    assert!(current_work_item.contains("[completed] alpha:"));
    assert!(current_work_item.contains("[completed] beta:"));
    assert!(current_work_item.contains("[completed] gamma:"));
    assert!(current_work_item.contains("[pending] delta:"));
    assert!(current_work_item.contains("[pending] epsilon:"));
    assert!(current_input.contains("Before handling delta"));
    assert!(current_input.contains("alpha/beta/gamma discussed"));
    assert!(
        !recent_turns.contains("Initial issue list:"),
        "recent_turns should not be the authoritative carrier for the original list:\n{rendered}"
    );
    assert!(
        diagnostics.section_share("recent_turns") < 0.45,
        "recent_turns should not dominate issue-list continuity: {diagnostics:?}"
    );
    assert!(
        diagnostics.repeated_line_ratio < 0.10,
        "line duplication should stay low: {diagnostics:?}"
    );
    Ok(())
}

#[test]
fn multi_turn_context_eval_keeps_compacted_and_interleaved_work_items_clear() -> Result<()> {
    let dir = tempdir()?;
    let storage = AppStorage::new(dir.path())?;

    let stale_operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Archive stale objective delta before compaction.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    storage.append_message(&stale_operator)?;
    storage.append_brief(&BriefRecord::new(
        "default",
        BriefKind::Result,
        "Compaction summary should carry only the preserved budget decision.",
        Some(stale_operator.id.clone()),
        None,
    ))?;

    let recent_operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Resume active context eval after compacted history.".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );
    storage.append_message(&recent_operator)?;
    storage.append_brief(&BriefRecord::new(
        "default",
        BriefKind::Result,
        "Active work resumed after compaction.",
        Some(recent_operator.id.clone()),
        None,
    ))?;

    let mut active_work = WorkItemRecord::new(
        "default",
        "Ship compacted interleaving eval",
        WorkItemState::Open,
    );
    active_work.id = "work_active_eval".into();
    storage.append_work_item(&active_work)?;
    append_work_item_todo(
        &storage,
        active_work.id.clone(),
        vec![TodoItem {
            text: "Check compacted continuity fact".into(),
            state: TodoItemState::InProgress,
        }],
    )?;

    let mut queued_work = WorkItemRecord::new(
        "default",
        "Follow up on context benchmark integration",
        WorkItemState::Open,
    );
    queued_work.id = "work_queued_eval".into();
    storage.append_work_item(&queued_work)?;

    let mut blocked_work = WorkItemRecord::new(
        "default",
        "Wait for prompt dump review",
        WorkItemState::Open,
    );
    blocked_work.id = "work_blocked_eval".into();
    blocked_work.blocked_by = Some("blocked until human prompt dump review finishes".into());
    storage.append_work_item(&blocked_work)?;

    let mut completed_work = WorkItemRecord::new(
        "default",
        "Finish old context cleanup",
        WorkItemState::Completed,
    );
    completed_work.id = "work_completed_eval".into();
    completed_work.result_summary =
        Some("Old cleanup finished without becoming current truth".into());
    storage.append_work_item(&completed_work)?;

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue the compacted interleaving evaluation with preserved fact omega."
                .into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::CliPrompt,
        AdmissionContext::LocalProcess,
    );

    let mut session = AgentState::new("default");
    session.compacted_message_count = 1;
    session.context_summary =
        Some("Compacted summary: preserved budget decision and fact omega.".into());
    session.current_work_item_id = Some(active_work.id.clone());

    let rendered = render_context_snapshot_named(
        &storage,
        &session,
        &current_message,
        None,
        Some("multi_turn_context_compacted_interleaving"),
    )?;
    let diagnostics = analyze_context(&rendered);
    let compacted_summary =
        section_content(&rendered, "compacted_summary").expect("compacted summary section");
    let current_work_item =
        section_content(&rendered, "current_work_item").expect("current work section");
    let queued_blocked =
        section_content(&rendered, "queued_blocked_work_items").expect("queued work section");
    let recent_turns = section_content(&rendered, "recent_turns").expect("recent turns section");
    let current_input = section_content(&rendered, "current_input").expect("current input");

    assert!(compacted_summary.contains("preserved budget decision and fact omega"));
    assert!(current_work_item.contains("work_active_eval"));
    assert!(current_work_item.contains("Ship compacted interleaving eval"));
    assert!(queued_blocked.contains("work_queued_eval"));
    assert!(queued_blocked.contains("work_blocked_eval"));
    assert!(queued_blocked.contains("work_completed_eval"));
    assert!(queued_blocked.contains("Old cleanup finished without becoming current truth"));
    assert!(recent_turns.contains("Resume active context eval after compacted history"));
    assert!(!recent_turns.contains("Archive stale objective delta"));
    assert!(!current_work_item.contains("work_queued_eval"));
    assert!(current_input.contains("preserved fact omega"));
    assert!(diagnostics.section("queued_blocked_work_items").is_some());
    assert!(
        diagnostics.section_share("queued_blocked_work_items") < 0.40,
        "queued/blocked projection should stay bounded: {diagnostics:?}"
    );
    assert!(
        diagnostics.repeated_line_ratio < 0.05,
        "line duplication should stay low: {diagnostics:?}"
    );
    assert!(
        phrase_count(&rendered, "Archive stale objective delta") == 0,
        "compacted stale objective should not be replayed from recent_turns:\n{rendered}"
    );
    Ok(())
}
