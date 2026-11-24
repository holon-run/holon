use super::{
    action_event_body, active_activity_speaker, agent_has_active_activity,
    assistant_message_from_event, latest_action_event, progress_event_body,
};
use crate::operator_event::{present_operator_event, OperatorPresentationContext};
use crate::tui::projection::{
    LiveWorkingActivityRecord, ProjectionEventLane, ProjectionEventRecord,
};
use crate::types::{
    AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
    AgentOwnership, AgentPostureProjection, AgentProfilePreset, AgentRegistryStatus,
    AgentSchedulingPosture, AgentState, AgentStatus, AgentSummary, AgentTokenUsageSummary,
    AgentVisibility, ClosureDecision, ClosureOutcome, RuntimePosture, TokenUsage,
};
use chrono::Utc;
use serde_json::{json, Value};

fn event(kind: &str, summary: &str, payload: Value) -> ProjectionEventRecord {
    let presentation = present_operator_event(
        kind,
        &payload,
        summary,
        &OperatorPresentationContext::default(),
    );
    ProjectionEventRecord {
        id: "evt-1".into(),
        event_seq: 1,
        ts: Utc::now(),
        lane: ProjectionEventLane::Debug,
        kind: kind.into(),
        summary: presentation.summary.clone(),
        presentation,
        payload,
    }
}

fn activity_record(event: &ProjectionEventRecord) -> LiveWorkingActivityRecord {
    LiveWorkingActivityRecord {
        event: event.clone(),
        rendered_body: action_event_body(event),
    }
}

fn model_ref() -> crate::config::ModelRouteRef {
    crate::config::ModelRouteRef::parse_compatible("anthropic/claude-sonnet-4-6").unwrap()
}

fn agent_summary(status: AgentStatus, posture: AgentSchedulingPosture) -> AgentSummary {
    let mut state = AgentState::new("default");
    state.status = status;
    AgentSummary {
        identity: AgentIdentityView {
            agent_id: "default".into(),
            name: None,
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            can_rename: false,
            status: AgentRegistryStatus::Active,
            is_default_agent: true,
            incarnation: 1,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        },
        agent: state,
        scheduling_posture: AgentPostureProjection {
            posture,
            reason: "test posture".into(),
            work_item_id: None,
            task_id: None,
            run_id: None,
        },
        active_task_count: 0,
        lifecycle: AgentLifecycleHint::default(),
        model: AgentModelState {
            source: AgentModelSource::RuntimeDefault,
            runtime_default_model: model_ref(),
            effective_model: model_ref(),
            requested_model: None,
            active_model: None,
            fallback_active: false,
            effective_fallback_models: Vec::new(),
            override_model: None,
            override_reasoning_effort: None,
            override_service_tier: None,
            effective_service_tier: None,
            override_unresolved: false,
            resolved_policy: holon_domain::model_values::ResolvedRuntimeModelPolicy::default(),
        },
        token_usage: AgentTokenUsageSummary {
            total: TokenUsage::new(0, 0),
            total_model_rounds: 0,
            last_turn: None,
        },
        closure: ClosureDecision {
            outcome: ClosureOutcome::Completed,
            waiting_reason: None,
            work_signal: None,
            runtime_posture: RuntimePosture::Awake,
            evidence: Vec::new(),
        },
        execution: crate::system::ExecutionSnapshot {
            profile: crate::system::ExecutionProfile::default(),
            policy: crate::system::ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: Vec::new(),
            workspace_id: None,
            workspace_anchor: Default::default(),
            execution_root: Default::default(),
            cwd: Default::default(),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
            execution_roots: Vec::new(),
        },
        active_workspace_occupancy: None,
        loaded_agents_md: Default::default(),
        skills: Default::default(),
        active_children: Vec::new(),
        active_wait_conditions: Vec::new(),
        active_external_triggers: Vec::new(),
        recent_operator_notifications: Vec::new(),
        recent_brief_count: 0,
        recent_event_count: 0,
        provider_cooldown_wait: None,
    }
}

#[test]
fn progress_event_body_shows_full_exec_command() {
    let event = event(
        "tool_executed",
        "tool executed: ExecCommand",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "git status --short --branch"
        }),
    );
    let rendered = progress_event_body(&event);
    assert_eq!(rendered, "Command finished: git status --short --branch");
}

#[test]
fn chat_scroll_preserves_view_after_history_prepend() {
    let mut scroll = super::ChatScrollState::new();
    scroll.scroll_with_key(crossterm::event::KeyCode::Home, 20);
    assert_eq!(scroll.effective_scroll(20), 0);

    scroll.prepare_for_history_prepend(20);
    scroll.apply_history_prepend_adjustment(35);

    assert_eq!(scroll.effective_scroll(35), 15);
}

#[test]
fn chat_scroll_clamps_non_tail_view_without_following_tail() {
    let mut scroll = super::ChatScrollState::new();
    scroll.scroll_with_key(crossterm::event::KeyCode::PageUp, 20);
    assert!(!scroll.is_following_tail());

    assert!(!scroll.is_following_tail());
    assert_eq!(scroll.effective_scroll(5), 0);
}

#[test]
fn semantic_reading_anchor_survives_append_prepend_reflow_and_fold() {
    use super::{ChatRowAnchor, ChatScrollState};
    use crossterm::event::KeyCode;
    let rows = |prefix: &str, count: usize| {
        (0..count)
            .map(|offset| ChatRowAnchor {
                key: prefix.into(),
                turn_id: prefix.split('/').next().unwrap().into(),
                offset,
            })
            .collect::<Vec<_>>()
    };
    let mut original = rows("old/header", 2);
    original.extend(rows("old/tool:1", 20));
    original.extend(rows("new/header", 20));
    let mut scroll = ChatScrollState::new();
    scroll.scroll_with_key(KeyCode::PageUp, 32);
    scroll.scroll_with_key(KeyCode::PageUp, 32);
    let top = scroll.resolve_rows(&original, 32);
    let anchor = original[top as usize].clone();
    assert_eq!(anchor.key, "old/tool:1");

    let mut appended = original.clone();
    appended.extend(rows("new/tool:2", 30));
    assert_eq!(scroll.resolve_rows(&appended, 62), top);
    assert_eq!(scroll.reading_anchor, Some(anchor.clone()));

    let mut prepended = rows("older/brief:1", 7);
    prepended.extend(appended);
    assert_eq!(scroll.resolve_rows(&prepended, 69), top + 7);
    assert_eq!(scroll.reading_anchor, Some(anchor));

    // Reflow can remove a visual subrow; retain the same item at its last row.
    let mut reflowed = rows("older/brief:1", 7);
    reflowed.extend(rows("old/header", 2));
    reflowed.extend(rows("old/tool:1", 3));
    reflowed.extend(rows("new/header", 60));
    assert_eq!(scroll.resolve_rows(&reflowed, 62), 11);
    let mut folded = rows("older/brief:1", 7);
    folded.extend(rows("old/header", 2));
    folded.extend(rows("new/header", 60));
    assert_eq!(scroll.resolve_rows(&folded, 59), 7);
    assert!(!scroll.is_following_tail());
    scroll.scroll_with_key(KeyCode::End, 59);
    assert_eq!(scroll.resolve_rows(&folded, 59), 59);
    assert!(scroll.is_following_tail());
}

#[test]
fn semantic_layout_rows_match_actual_paragraph_wrap_for_unicode() {
    let items = vec![super::keyed_cell(
        "turn-1",
        "tool:1",
        super::canonical_notice(
            chrono::Utc::now(),
            "tool".into(),
            "中文与 English very-long-path/".repeat(40),
        ),
    )];
    for width in [1, 20, 80, 84, 120] {
        let (text, rows) = super::build_chat_layout(&items, width);
        let actual = ratatui::widgets::Paragraph::new(text)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .line_count(width);
        assert_eq!(rows.len(), actual, "width {width}");
        assert!(rows.iter().all(|row| row.key == "turn-1/tool:1"));
    }
}

#[test]
fn progress_event_body_marks_failed_exec_command() {
    let event = event(
        "tool_execution_failed",
        "tool execution failed: ExecCommand",
        json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "cargo test tui"
        }),
    );

    let rendered = progress_event_body(&event);

    assert_eq!(rendered, "Command failed: cargo test tui");
}

#[test]
fn sleep_tool_is_not_activity_content() {
    let event = event(
        "tool_executed",
        "tool executed: Sleep",
        json!({
            "tool_name": "Sleep"
        }),
    );
    assert!(action_event_body(&event).is_empty());
}

#[test]
fn empty_provider_round_is_not_activity_content() {
    let empty_round = event(
        "provider_round_completed",
        "provider round completed",
        json!({ "round": 1, "stop_reason": "end_turn" }),
    );
    let command = event(
        "process_execution_requested",
        "process_execution_requested",
        json!({
            "surface": "ExecCommand",
            "cmd_preview": "cargo test tui::chat"
        }),
    );
    let empty_round_activity = activity_record(&empty_round);
    let command_activity = activity_record(&command);
    let events = vec![&empty_round_activity, &command_activity];

    assert!(assistant_message_from_event(&empty_round).is_none());
    assert_eq!(
        latest_action_event(events.as_slice()).map(|event| event.event.summary.as_str()),
        Some("Command started: cargo test tui::chat")
    );
}

#[test]
fn assistant_round_recorded_is_activity_content() {
    let assistant = event(
        "assistant_round_recorded",
        "assistant round",
        json!({ "text_preview": "I will inspect the event path first." }),
    );

    assert_eq!(
        assistant_message_from_event(&assistant).as_deref(),
        Some("I will inspect the event path first.")
    );
}

#[test]
fn assistant_round_tool_request_is_not_activity_message() {
    let assistant = event(
        "assistant_round_recorded",
        "Assistant requested tools: ExecCommand",
        json!({
            "text_preview": null,
            "tool_names": ["ExecCommand"]
        }),
    );

    assert!(assistant_message_from_event(&assistant).is_none());
}

#[test]
fn posture_runnable_overrides_asleep_activity_label() {
    let agent = agent_summary(AgentStatus::Asleep, AgentSchedulingPosture::HasRunnableWork);

    assert!(agent_has_active_activity(&agent));
    assert_eq!(active_activity_speaker(&agent), "Holon (continuing)");
}

#[test]
fn posture_waiting_and_blocked_labels_are_distinct() {
    let waiting_external = agent_summary(
        AgentStatus::Asleep,
        AgentSchedulingPosture::WaitingForExternal,
    );
    let needs_input = agent_summary(
        AgentStatus::Asleep,
        AgentSchedulingPosture::WaitingForOperator,
    );
    let blocked = agent_summary(AgentStatus::Asleep, AgentSchedulingPosture::Blocked);

    assert_eq!(
        active_activity_speaker(&waiting_external),
        "Holon (waiting external)"
    );
    assert_eq!(active_activity_speaker(&needs_input), "Holon (needs input)");
    assert_eq!(active_activity_speaker(&blocked), "Holon (blocked)");
}

#[test]
fn unknown_posture_keeps_legacy_activity_fallback() {
    let mut agent = agent_summary(AgentStatus::AwakeIdle, AgentSchedulingPosture::Unknown);
    agent.agent.pending = 1;

    assert!(agent_has_active_activity(&agent));
    assert_eq!(active_activity_speaker(&agent), "Holon (queued)");
}

#[test]
fn retained_child_summary_does_not_imply_parent_activity() {
    for posture in [
        AgentSchedulingPosture::Unknown,
        AgentSchedulingPosture::Idle,
    ] {
        let mut agent = agent_summary(AgentStatus::AwakeIdle, posture);
        agent.active_children = vec![serde_json::from_value(json!({
            "identity": agent.identity,
            "status": "awake_running",
            "pending": 1,
            "active_task_count": 1,
            "observability": {"phase": "running"}
        }))
        .unwrap()];
        assert!(!agent_has_active_activity(&agent));
        assert_eq!(active_activity_speaker(&agent), "Holon (idle)");
    }
}
