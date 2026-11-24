use super::{
    prompt_cursor_position, prompt_pane_height, prompt_pane_scroll, prompt_top_gap_height,
    prompt_visual_rows, render_header, render_model_status, render_prompt_buffer,
    render_prompt_text, render_prompt_text_for_width, render_summary, render_task_detail,
    status_bar_height, todo_summary_work_item,
};
use crate::system::{ExecutionProfile, ExecutionSnapshot};
use crate::types::{
    AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
    AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentSummary,
    AgentTokenUsageSummary, AgentVisibility, ChildAgentObservabilitySnapshot, ChildAgentPhase,
    ChildAgentSummary, ClosureDecision, ClosureOutcome, LoadedAgentsMdView, RuntimePosture,
    SkillsRuntimeView, TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState, TokenUsage,
    WorkItemRecord, WorkItemState,
};
use chrono::{TimeZone, Utc};
use ratatui::prelude::{Line, Rect};
use serde_json::{json, Value};
use std::path::PathBuf;

fn route_ref(value: &str) -> crate::config::ModelRouteRef {
    crate::config::ModelRouteRef::parse_compatible(value).unwrap()
}

fn sample_agent_summary() -> AgentSummary {
    let mut state = AgentState::new("default");
    state.status = crate::types::AgentStatus::AwakeIdle;
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
        active_task_count: 0,
        lifecycle: AgentLifecycleHint::default(),
        scheduling_posture: Default::default(),
        model: AgentModelState {
            effective_model: route_ref("anthropic/claude-sonnet-4-6"),
            requested_model: Some(route_ref("anthropic/claude-sonnet-4-6")),
            active_model: Some(route_ref("anthropic/claude-sonnet-4-6")),
            fallback_active: false,
            runtime_default_model: route_ref("anthropic/claude-sonnet-4-6"),
            override_model: None,
            override_reasoning_effort: None,
            override_service_tier: None,
            effective_service_tier: None,
            source: AgentModelSource::RuntimeDefault,
            effective_fallback_models: Vec::new(),
            override_unresolved: false,
            resolved_policy: holon_domain::model_values::ResolvedRuntimeModelPolicy {
                model_ref: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
                display_name: "Claude Sonnet 4.6".into(),
                description: "Sample policy".into(),
                context_window_tokens: Some(200_000),
                effective_context_window_percent: 90,
                prompt_budget_estimated_tokens: 180_000,
                compaction_trigger_estimated_tokens: 180_000,
                compaction_keep_recent_estimated_tokens: 68_400,
                runtime_max_output_tokens: 32_000,
                verbosity: None,
                tool_output_truncation_estimated_tokens: 2_500,
                max_output_tokens_upper_limit: Some(128_000),
                capabilities: holon_domain::model_values::ModelCapabilityFlags {
                    image_input: true,
                    ..holon_domain::model_values::ModelCapabilityFlags::default()
                },
                agent_turn: true,
                decision_capable: false,
                decision_protocol: None,
                reasoning_effort_options: Vec::new(),
                source: holon_domain::model_values::ModelMetadataSource::BuiltInCatalog,
                evidence: Default::default(),
            },
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
        execution: ExecutionSnapshot {
            profile: ExecutionProfile::default(),
            policy: ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: Vec::new(),
            workspace_id: None,
            workspace_anchor: PathBuf::from("/tmp"),
            execution_root: PathBuf::from("/tmp"),
            cwd: PathBuf::from("/tmp"),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
            execution_roots: Vec::new(),
        },
        active_workspace_occupancy: None,
        loaded_agents_md: LoadedAgentsMdView::default(),
        skills: SkillsRuntimeView::default(),
        active_children: vec![ChildAgentSummary {
            identity: AgentIdentityView {
                agent_id: "child_1".into(),
                name: None,
                kind: AgentKind::Child,
                visibility: AgentVisibility::Private,
                ownership: AgentOwnership::ParentSupervised,
                profile_preset: AgentProfilePreset::PrivateChild,
                can_rename: false,
                status: AgentRegistryStatus::Active,
                is_default_agent: false,
                incarnation: 1,
                parent_agent_id: Some("default".into()),
                lineage_parent_agent_id: Some("default".into()),
                delegated_from_task_id: Some("task-1".into()),
            },
            status: crate::types::AgentStatus::AwakeRunning,
            current_run_id: Some("run-1".into()),
            pending: 0,
            active_task_count: 1,
            observability: ChildAgentObservabilitySnapshot {
                _read_material: None,
                _source_material: None,
                phase: ChildAgentPhase::Running,
                blocked_reason: None,
                waiting_reason: None,
                current_work_item_id: Some("work-1".into()),
                work_summary: Some("child running".into()),
                last_progress_brief: None,
                last_result_brief: None,
            },
        }],
        active_wait_conditions: Vec::new(),
        active_external_triggers: Vec::new(),
        recent_operator_notifications: Vec::new(),
        recent_brief_count: 0,
        recent_event_count: 0,
        provider_cooldown_wait: None,
    }
}

#[test]
fn prompt_buffer_renders_as_multiline_block() {
    let rendered = render_prompt_buffer("first\nsecond");
    assert_eq!(rendered, "> first\n  second");
}

#[test]
fn prompt_buffer_keeps_empty_trailing_line_visible() {
    let rendered = render_prompt_buffer("first\n");
    assert_eq!(rendered, "> first\n  ");
}

#[test]
fn prompt_pane_height_accounts_for_soft_wrapped_lines() {
    assert_eq!(prompt_pane_height("abcdefgh", 0, 10), 1);
    assert_eq!(prompt_pane_height("abcdefghabcdefgh", 0, 10), 2);
    assert_eq!(prompt_pane_height("abcdefgh", 2, 10), 3);
}

#[test]
fn prompt_visual_rows_preserve_blank_lines_and_soft_wraps() {
    assert_eq!(
        prompt_visual_rows("first\n\nsecond", 20),
        vec!["> first", "  ", "  second"]
    );
    assert_eq!(
        prompt_visual_rows("abcdefghabcdefgh", 10),
        vec!["> abcdefgh", "abcdefgh"]
    );
}

#[test]
fn prompt_pane_height_saturates_for_large_pastes() {
    let buffer = "abcdefghij\n".repeat(70_000);
    assert_eq!(prompt_pane_height(&buffer, usize::MAX, 4), 12);
    assert_eq!(prompt_pane_height(&"a".repeat(70_000), 0, 4), 12);
}

#[test]
fn empty_prompt_renders_dim_placeholder() {
    let rendered = render_prompt_text("", &[]);
    let line = rendered.lines.first().expect("placeholder line");
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("Message Holon."));
}

#[test]
fn render_header_uses_agent_status_without_identity_contract() {
    let rendered = render_header(&sample_agent_summary());
    assert_eq!(rendered, "default  idle");
    assert!(!rendered.contains("public/self_owned (public_named)"));
}

#[test]
fn render_summary_uses_canonical_identity_relations() {
    let rendered = render_summary(&sample_agent_summary());
    assert!(rendered.contains("Identity relation: independently addressable agent identity"));
    assert!(rendered.contains("Tool access: canonical capability policy"));
    assert!(rendered.contains("Cleanup authority: canonical lifecycle attachment and supervision"));
    assert!(!rendered.contains("Children:"));
    assert!(rendered.contains("Execution backend: host_local"));
    assert!(rendered.contains("Process execution: runtime_shaped"));
    assert!(rendered.contains(
            "Confinement: path_confinement=not_enforced, write_confinement=not_enforced, network_confinement=not_enforced, secret_isolation=not_enforced, child_process_containment=not_enforced"
        ));
}

#[test]
fn model_status_distinguishes_inherited_override_and_fallback() {
    let inherited = sample_agent_summary();
    assert_eq!(
        render_model_status(&inherited),
        "model: anthropic/claude-sonnet-4-6"
    );

    let mut overridden = sample_agent_summary();
    overridden.model.override_model = Some(route_ref("openai/gpt-5.4"));
    overridden.model.effective_model = route_ref("openai/gpt-5.4");
    overridden.model.active_model = Some(route_ref("openai/gpt-5.4"));
    assert_eq!(
        render_model_status(&overridden),
        "model: openai/gpt-5.4 (agent override)"
    );

    let mut fallback = overridden;
    fallback.model.requested_model = Some(route_ref("openai/gpt-5.4"));
    fallback.model.active_model = Some(route_ref("anthropic/claude-sonnet-4-6"));
    fallback.model.fallback_active = true;
    assert_eq!(
        render_model_status(&fallback),
        "model: anthropic/claude-sonnet-4-6 (fallback from openai/gpt-5.4)"
    );
}

#[test]
fn todo_summary_prefers_current_work_item_over_first_open_item() {
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some("work-current".into());
    let mut first_open = WorkItemRecord::new("default", "first open", WorkItemState::Open);
    first_open.id = "work-first".into();
    first_open.todo_list = vec![TodoItem {
        text: "wrong todo".into(),
        state: TodoItemState::InProgress,
    }];
    let mut current = WorkItemRecord::new("default", "selected work", WorkItemState::Open);
    current.id = "work-current".into();
    current.blocked_by = Some("waiting on review".into());
    current.todo_list = vec![TodoItem {
        text: "selected todo".into(),
        state: TodoItemState::Pending,
    }];

    let work_items = [
        super::super::tests::sample_work_item_projection(first_open, false),
        super::super::tests::sample_work_item_projection(current, true),
    ];
    let selected =
        todo_summary_work_item(&agent, &work_items).expect("current work item should be selected");
    assert_eq!(selected.id, "work-current");
    assert_eq!(selected.todo_list[0].text, "selected todo");
}

#[test]
fn prompt_cursor_tracks_multiline_end_position() {
    let area = Rect::new(10, 5, 40, 6);
    assert_eq!(
        prompt_cursor_position(area, "first\nsecond", "first\nsecond".len(), 0, 0),
        (18, 6)
    );
}

#[test]
fn prompt_cursor_tracks_soft_wrapped_long_lines() {
    let area = Rect::new(10, 5, 10, 6);
    assert_eq!(
        prompt_cursor_position(area, "abcdefgh", "abcdefgh".len(), 0, 0),
        (19, 5)
    );
}

#[test]
fn prompt_cursor_uses_display_width_for_wide_characters() {
    let area = Rect::new(10, 5, 10, 6);
    assert_eq!(
        prompt_cursor_position(area, "你好你好", "你好你好".len(), 0, 0),
        (19, 5)
    );
}

#[test]
fn prompt_cursor_clamps_to_prompt_pane_height() {
    let area = Rect::new(10, 5, 10, 4);
    assert_eq!(
        prompt_cursor_position(area, "abcdefgh\nabcdefgh", "abcdefgh\nabcdefgh".len(), 0, 0),
        (19, 6)
    );
}

#[test]
fn prompt_cursor_tracks_insert_position_inside_line() {
    let area = Rect::new(10, 5, 20, 6);
    assert_eq!(prompt_cursor_position(area, "hello", 2, 0, 0), (14, 5));
}

#[test]
fn prompt_pane_scroll_keeps_multiline_cursor_visible() {
    let area = Rect::new(10, 5, 20, 3);
    let buffer = "first\n\nsecond\nthird\nfourth";
    let scroll = prompt_pane_scroll(area, buffer, buffer.len(), 0);
    assert!(scroll > 0);
    let (_, y) = prompt_cursor_position(area, buffer, buffer.len(), 0, scroll);
    assert!(y <= area.y + area.height.saturating_sub(1));
}

#[test]
fn prompt_pane_scroll_keeps_single_line_visible_in_short_pane() {
    let area = Rect::new(10, 5, 20, 3);
    assert_eq!(prompt_pane_scroll(area, "hello", "hello".len(), 0), 0);
}

#[test]
fn slash_menu_renders_above_prompt_buffer() {
    let rendered = render_prompt_text(
        "/de",
        &[
            Line::from("  /debug-prompt open debug prompt dialog"),
            Line::from("  /help         show slash command help"),
        ],
    );
    let joined = rendered
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("/debug-prompt open debug prompt dialog"));
    assert!(joined.contains("> /de"));
}

#[test]
fn slash_menu_prompt_buffer_uses_real_text_lines() {
    let rendered = render_prompt_text(
        "/debug\nsecond",
        &[Line::from("  /debug-prompt open debug prompt dialog")],
    );
    let joined = rendered
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        joined,
        vec![
            "  /debug-prompt open debug prompt dialog",
            "> /debug",
            "  second"
        ]
    );
}

#[test]
fn width_aware_prompt_text_uses_prewrapped_visual_lines() {
    let rendered = render_prompt_text_for_width("abcdefghabcdefgh", &[], 10, 0, 12);
    let rows = rendered
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(rows, vec!["> abcdefgh", "abcdefgh"]);
}

#[test]
fn prompt_cursor_offsets_for_slash_menu_rows() {
    let area = Rect::new(10, 5, 20, 8);
    assert_eq!(prompt_cursor_position(area, "/de", 3, 2, 0), (15, 7));
}

#[test]
fn status_bar_uses_fixed_two_line_height() {
    assert_eq!(status_bar_height(), 3);
}

#[test]
fn prompt_top_gap_uses_one_spacing_row() {
    assert_eq!(prompt_top_gap_height(), 1);
}

fn task(status: TaskStatus, kind: TaskKind, detail: Option<Value>) -> TaskRecord {
    let now = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
    TaskRecord {
        id: "task-1".into(),
        agent_id: "agent-1".into(),
        kind,
        status,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("sample task".into()),
        detail,
        recovery: None,
    }
}

#[test]
fn render_task_detail_marks_available_task_actions() {
    let task = task(
        TaskStatus::Running,
        TaskKind::CommandTask,
        Some(json!({
            "output_path": "target/task-output.log",
            "accepts_input": true
        })),
    );

    let detail = render_task_detail(&task);

    assert!(detail.contains("[f] full output: available (bounded read of confirmed command file)"));
    assert!(detail.contains(
        "[l] follow/live output: available (bounded observation of confirmed command file)"
    ));
    assert!(detail.contains("[x] stop/cancel: available (TaskStop)"));
    assert!(detail.contains("[i] input/continue: available (TaskInput)"));
}

#[test]
fn render_task_detail_marks_unavailable_task_actions() {
    let task = task(
        TaskStatus::Completed,
        TaskKind::SleepJob,
        Some(json!({ "accepts_input": false })),
    );

    let detail = render_task_detail(&task);

    assert!(detail.contains("[f] full output: unavailable"));
    assert!(detail.contains("[l] follow/live output: unavailable"));
    assert!(detail.contains("[x] stop/cancel: unavailable"));
    assert!(detail.contains("[i] input/continue: unavailable"));
}
