use super::{
    app::TuiApp,
    chat::{
        build_chat_text, chat_text, collect_chat_items, is_operator_origin_value,
        paragraph_max_scroll, paragraph_max_scroll_unframed, ChatScrollState, ConversationCell,
    },
    composer::ComposerState,
    determine_alt_screen_mode_for_terminal,
    overlay::{centered_rect_rows, OverlayState},
    projection::{OperatorDisplayMode, TuiProjection},
    render::draw,
    runtime::{is_cursor_too_old_error, AgentListChange, TuiConnectionState, TuiRuntimeMessage},
    state::{tui_state_path, TuiClientState},
    view_model::{HeaderViewModel, StatusbarViewModel},
};
use crate::{
    client::{
        AgentStateSnapshot, AgentStreamEvent, LocalClient, StateSessionSnapshot,
        StateWorkspaceSnapshot, StreamEventEnvelope,
    },
    config::{AltScreenMode, AppConfig},
    system::{ExecutionProfile, ExecutionSnapshot},
    types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentListEntry, AgentModelSource,
        AgentModelState, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentStatus,
        AgentSummary, AgentTokenUsageSummary, AgentVisibility, BriefKind, BriefRecord,
        ChildAgentSummary, ClosureDecision, ClosureOutcome, LoadedAgentsMdView, MessageBody,
        OperatorMessageRecord, OperatorMessageStatus, RuntimePosture, SkillsRuntimeView,
        TokenUsage, TranscriptEntry, TranscriptEntryKind, WaitingIntentSummary,
    },
};
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Text};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use serde_json::json;
use std::{path::PathBuf, time::Instant};

fn test_config() -> AppConfig {
    let temp = tempfile::tempdir().unwrap().keep();
    AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        home_dir: temp.clone(),
        data_dir: temp.clone(),
        socket_path: temp.join("run").join("holon.sock"),
        workspace_dir: temp.join("workspace"),
        context_window_messages: 8,
        context_window_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("secret".into()),
        control_auth_mode: crate::config::ControlAuthMode::Auto,
        config_file_path: temp.join("config.json"),
        stored_config: Default::default(),
        default_model: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
        fallback_models: Vec::new(),
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
        max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
        disable_provider_fallback: false,
        tui_alternate_screen: AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        providers: crate::config::provider_registry_for_tests(
            None,
            Some("dummy"),
            temp.join(".codex"),
        ),
        web_config: crate::web::WebConfig::default(),
    }
}

#[test]
fn local_tui_restores_persisted_selected_agent_on_initial_agent_list() {
    let config = test_config();
    let state_path = config.home_dir.join("state").join("tui").join("local.json");
    TuiClientState::new("beta").save(&state_path).unwrap();
    let client = LocalClient::new(config).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    let change = app.apply_agent_list(vec![
        sample_agent_summary("default"),
        sample_agent_summary("beta"),
    ]);

    assert_eq!(change, AgentListChange::RequiresBootstrap);
    assert_eq!(app.selected_agent_id(), Some("beta"));
}

#[test]
fn missing_persisted_agent_falls_back_to_default_agent() {
    let config = test_config();
    let state_path = config.home_dir.join("state").join("tui").join("local.json");
    TuiClientState::new("missing").save(&state_path).unwrap();
    let client = LocalClient::new(config).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.apply_agent_list(vec![
        sample_agent_summary("alpha"),
        sample_agent_summary("default"),
    ]);

    assert_eq!(app.selected_agent_id(), Some("default"));
}

#[test]
fn remote_tui_state_scope_uses_hashed_connect_target_without_token() {
    let config = test_config();
    let client =
        LocalClient::remote(config, "http://example.test:7878/", "top-secret-token").unwrap();
    let path = tui_state_path(&client);
    let filename = path.file_name().unwrap().to_string_lossy();

    assert!(filename.starts_with("remote-"));
    assert!(filename.ends_with(".json"));
    assert!(!filename.contains("example"));
    assert!(!filename.contains("top-secret-token"));

    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.record_selected_agent("beta");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("beta"));
    assert!(!raw.contains("top-secret-token"));
    assert!(!raw.contains("example.test"));
}

fn sample_agent_summary(agent_id: &str) -> AgentSummary {
    let mut state = crate::types::AgentState::new(agent_id);
    state.status = AgentStatus::AwakeIdle;
    state.pending = 1;

    AgentSummary {
        identity: AgentIdentityView {
            agent_id: agent_id.into(),
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            status: AgentRegistryStatus::Active,
            is_default_agent: agent_id == "default",
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        },
        agent: state,
        active_task_count: 0,
        lifecycle: AgentLifecycleHint::default(),
        model: AgentModelState {
            effective_model: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            requested_model: Some(
                crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            ),
            active_model: Some(
                crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            ),
            fallback_active: false,
            runtime_default_model: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6")
                .unwrap(),
            override_model: None,
            override_reasoning_effort: None,
            source: AgentModelSource::RuntimeDefault,
            effective_fallback_models: Vec::new(),
            resolved_policy: crate::model_catalog::ResolvedRuntimeModelPolicy {
                model_ref: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
                display_name: "Claude Sonnet 4.6".into(),
                description: "Sample policy".into(),
                context_window_tokens: Some(200_000),
                effective_context_window_percent: 90,
                prompt_budget_estimated_tokens: 180_000,
                compaction_trigger_estimated_tokens: 180_000,
                compaction_keep_recent_estimated_tokens: 68_400,
                runtime_max_output_tokens: 32_000,
                tool_output_truncation_estimated_tokens: 2_500,
                max_output_tokens_upper_limit: Some(128_000),
                capabilities: crate::model_catalog::ModelCapabilityFlags {
                    image_input: true,
                    ..crate::model_catalog::ModelCapabilityFlags::default()
                },
                source: crate::model_catalog::ModelMetadataSource::BuiltInCatalog,
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
            attached_workspaces: vec![],
            workspace_id: None,
            workspace_anchor: "/tmp".into(),
            execution_root: "/tmp".into(),
            cwd: "/tmp".into(),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
        },
        active_workspace_occupancy: None,
        loaded_agents_md: LoadedAgentsMdView::default(),
        skills: SkillsRuntimeView::default(),
        active_children: Vec::<ChildAgentSummary>::new(),
        active_waiting_intents: Vec::<WaitingIntentSummary>::new(),
        active_external_triggers: Vec::new(),
        recent_operator_notifications: Vec::new(),
        recent_brief_count: 1,
        recent_event_count: 1,
    }
}

fn sample_snapshot(agent_id: &str, cursor: &str) -> AgentStateSnapshot {
    AgentStateSnapshot {
        agent: sample_agent_summary(agent_id),
        session: StateSessionSnapshot {
            current_run_id: None,
            pending_count: 0,
            last_turn: None,
        },
        tasks: Vec::new(),
        transcript_tail: Vec::new(),
        operator_messages: Vec::new(),
        briefs_tail: Vec::new(),
        timers: Vec::new(),
        work_items: Vec::new(),
        waiting_intents: Vec::new(),
        external_triggers: Vec::new(),
        operator_notifications: Vec::new(),
        workspace: StateWorkspaceSnapshot::default(),
        execution: None,
        brief: None,
        events_tail: Vec::new(),
        cursor: Some(cursor.into()),
    }
}

fn rendered_buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

fn rendered_buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let width = usize::from(buffer.area.width);
    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect()
}

#[test]
fn operator_origin_detection_accepts_structured_origin() {
    assert!(is_operator_origin_value(&json!({
        "kind": "operator",
        "actor_id": null
    })));
    assert!(!is_operator_origin_value(&json!({
        "kind": "system",
        "subsystem": "runtime"
    })));
}

#[test]
fn header_view_model_shows_agent_status_without_contract() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("holon-dev", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let view_model = HeaderViewModel::from_app(&app);

    assert_eq!(view_model.line, "holon-dev  running");
    assert!(!view_model.line.contains("public/self_owned"));
    assert!(!view_model.line.contains("public_named"));
}

#[test]
fn header_view_model_prefers_operator_waiting_label_and_resume_hint() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("holon-dev", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeIdle;
    snapshot.agent.closure.waiting_reason =
        Some(crate::types::WaitingReason::AwaitingOperatorInput);
    snapshot.agent.lifecycle.resume_required = true;
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let view_model = HeaderViewModel::from_app(&app);

    assert_eq!(
        view_model.line,
        "holon-dev  waiting for you · resume required"
    );
}

#[test]
fn statusbar_view_model_shows_workspace_label_execution_root_and_model() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.status_line.clear();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let workspace_anchor = home.join("opensource/src/github.com/holon-run/holon");
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
        workspace_id: "ws-random".into(),
        workspace_anchor: workspace_anchor.clone(),
        execution_root_id: "canonical_root:ws-random".into(),
        execution_root: workspace_anchor.clone(),
        projection_kind: crate::system::WorkspaceProjectionKind::CanonicalRoot,
        access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
        cwd: workspace_anchor.clone(),
        occupancy_id: None,
        projection_metadata: None,
    });
    snapshot.workspace.active_workspace_entry = snapshot.agent.agent.active_workspace_entry.clone();
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let view_model = StatusbarViewModel::from_app(&app, false);

    assert!(view_model.context_line.starts_with("holon ("));
    assert!(view_model
        .context_line
        .contains("opensource/src/github.com/holon-run/holon)"));
    assert!(view_model
        .context_line
        .contains("anthropic/claude-sonnet-4-6"));
    assert!(!view_model.context_line.contains("model:"));
}

#[test]
fn statusbar_view_model_uses_workspace_label_for_worktree_execution_root() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.status_line.clear();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let workspace_anchor = home.join("opensource/src/github.com/holon-run/holon");
    let execution_root =
        home.join("opensource/worktrees/github.com/holon-run/holon/issue-960-working-switch");
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
        workspace_id: "ws-random".into(),
        workspace_anchor: workspace_anchor.clone(),
        execution_root_id: format!("git_worktree_root:ws-random:{}", execution_root.display()),
        execution_root: execution_root.clone(),
        projection_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
        access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
        cwd: execution_root.clone(),
        occupancy_id: None,
        projection_metadata: None,
    });
    snapshot.workspace.active_workspace_entry = snapshot.agent.agent.active_workspace_entry.clone();
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let view_model = StatusbarViewModel::from_app(&app, false);

    assert!(view_model.context_line.starts_with("holon ("));
    assert!(view_model
        .context_line
        .contains("opensource/worktrees/github.com/holon-run/holon/issue-960-working-switch)"));
    assert!(!view_model
        .context_line
        .starts_with("issue-960-working-switch"));
}

#[test]
fn statusbar_view_model_prompts_for_active_tasks() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.status_line.clear();
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.tasks = vec![crate::types::TaskRecord {
        id: "task-1".into(),
        agent_id: "default".into(),
        kind: crate::types::TaskKind::CommandTask,
        status: crate::types::TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("cargo test".into()),
        detail: None,
        recovery: None,
    }];
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let view_model = StatusbarViewModel::from_app(&app, false);

    assert!(view_model
        .status_line
        .contains("1 active task · /tasks to inspect"));
}

#[test]
fn statusbar_view_model_prefers_overlay_hint_over_transient_status() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.status_line = "Opened tasks overlay".into();
    app.overlay = OverlayState::Tasks {
        selected: 0,
        detail_scroll: 0,
    };
    let snapshot = sample_snapshot("default", "evt-0");
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let view_model = StatusbarViewModel::from_app(&app, false);

    assert!(view_model.status_line.contains("Tasks:"));
    assert!(!view_model.status_line.contains("Opened tasks overlay"));
}

#[test]
fn build_chat_text_includes_structured_operator_messages() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.transcript = vec![TranscriptEntry {
        id: "msg-1".into(),
        agent_id: "default".into(),
        created_at: Utc::now(),
        kind: TranscriptEntryKind::IncomingMessage,
        round: None,
        related_message_id: Some("m1".into()),
        stop_reason: None,
        input_tokens: None,
        output_tokens: None,
        data: json!({
            "origin": {
                "kind": "operator",
                "actor_id": null
            },
            "body": {
                "type": "text",
                "text": "Fix the failing CI"
            }
        }),
    }];
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: Utc::now(),
        text: "I started a worktree task.".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let lines: Vec<String> = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(lines.iter().any(|line| line.contains("› ")));
    assert!(lines.iter().any(|line| line.contains("Fix the failing CI")));
    assert!(lines.iter().any(|line| line.contains("• ")));
    assert!(lines
        .iter()
        .any(|line| line.contains("I started a worktree task.")));
}

#[test]
fn build_chat_text_inlines_message_header_with_first_body_line() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: Utc::now(),
        text: "First line\nSecond line".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let lines: Vec<String> = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(lines
        .iter()
        .any(|line| line.contains("• ") && line.contains("First line")));
    assert!(lines.iter().any(|line| line.contains("Second line")));
}

#[test]
fn alternate_screen_mode_respects_override_and_zellij() {
    assert!(!determine_alt_screen_mode_for_terminal(
        true,
        AltScreenMode::Always,
        false
    ));
    assert!(determine_alt_screen_mode_for_terminal(
        false,
        AltScreenMode::Always,
        true
    ));
    assert!(!determine_alt_screen_mode_for_terminal(
        false,
        AltScreenMode::Never,
        false
    ));
    assert!(!determine_alt_screen_mode_for_terminal(
        false,
        AltScreenMode::Auto,
        true
    ));
    assert!(determine_alt_screen_mode_for_terminal(
        false,
        AltScreenMode::Auto,
        false
    ));
}

#[tokio::test]
async fn characters_append_to_prompt_by_default() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "hi");
}

#[tokio::test]
async fn shift_enter_adds_new_line_to_prompt() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "h\ni");
}

#[tokio::test]
async fn shift_enter_adds_new_line_when_slash_menu_is_visible() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/de");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "/de\n");
}

#[tokio::test]
async fn paste_inserts_multiline_text_without_submitting() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.handle_paste("first\nsecond").await.unwrap();

    assert_eq!(app.composer.as_str(), "first\nsecond");
}

#[tokio::test]
async fn rapid_enter_after_large_key_burst_inserts_newline_for_paste_fallback() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.composer = ComposerState::from("pasted text");
    app.composer_key_burst_started_at = Some(Instant::now());
    app.composer_key_burst_last_at = app.composer_key_burst_started_at;
    app.composer_key_burst_len = 8;
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "pasted text\n");
}

#[tokio::test]
async fn non_bracketed_multiline_paste_fallback_keeps_short_lines_in_composer() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "a\nb");
}

#[tokio::test]
async fn non_bracketed_slash_prefixed_paste_fallback_does_not_run_menu_command() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    for ch in "/he".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    for ch in "body".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }

    assert_eq!(app.composer.as_str(), "/he\nbody");
    assert_eq!(app.overlay, OverlayState::None);
}

#[tokio::test]
async fn enter_after_normal_text_still_submits_when_not_paste_burst() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.composer = ComposerState::from("short");
    let err = app
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect_err("normal Enter should still submit");

    assert!(err.to_string().contains("no agent selected"));
    assert_eq!(app.composer.as_str(), "short");
}

#[tokio::test]
async fn paste_updates_model_picker_filter() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.overlay = OverlayState::ModelPicker {
        filter: "gpt".into(),
        selected: 0,
    };

    app.handle_paste("-5.3\n").await.unwrap();

    assert_eq!(
        app.overlay,
        OverlayState::ModelPicker {
            filter: "gpt-5.3".into(),
            selected: 0,
        }
    );
}

#[tokio::test]
async fn paste_into_debug_prompt_stays_single_line() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.overlay = OverlayState::DebugPromptInput {
        composer: ComposerState::from("explain "),
    };

    app.handle_paste("first\nsecond").await.unwrap();

    assert_eq!(
        app.overlay,
        OverlayState::DebugPromptInput {
            composer: ComposerState::from("explain first second"),
        }
    );
}

#[tokio::test]
async fn enter_submits_instead_of_inserting_new_line() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("hi");

    let err = app
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect_err("submit should fail without a selected agent");

    assert!(err.to_string().contains("no agent selected"));
    assert_eq!(app.composer.as_str(), "hi");
}

#[tokio::test]
async fn agent_overlay_stays_open_while_navigating() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 0;
    app.connection_state = TuiConnectionState::Streaming;
    app.status_line = "Streaming native events for agent alpha".into();
    app.overlay = OverlayState::Agents { selected: 0 };

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.overlay, OverlayState::Agents { selected: 1 });
    assert_eq!(app.selected_agent_id(), Some("alpha"));
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Streaming
    ));
    assert!(!app.snapshot_refresh_in_flight);
    assert_eq!(app.status_line, "Streaming native events for agent alpha");
}

#[tokio::test]
async fn agent_overlay_enter_starts_switch_without_awaiting_snapshot() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 1;
    app.overlay = OverlayState::Agents { selected: 1 };
    app.connection_state = TuiConnectionState::Streaming;

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.status_line, "Bootstrapping agent beta from /state");
    assert!(app.snapshot_refresh_in_flight);
}

#[tokio::test]
async fn esc_closes_active_overlay_before_touching_prompt() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("draft");
    app.overlay = OverlayState::HelpView { scroll: 0 };
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.composer.as_str(), "draft");
}

#[tokio::test]
async fn colon_behaves_as_normal_input_after_action_menu_removal() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.composer.as_str(), ":");

    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("draft");
    app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.composer.as_str(), "draft:");
}

#[tokio::test]
async fn slash_menu_navigation_and_tab_complete_selected_command() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/");

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.slash_menu_selected, 1);

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "/agents");
    assert_eq!(app.overlay, OverlayState::None);
}

#[tokio::test]
async fn slash_menu_esc_dismisses_without_clearing_prompt() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/mo");

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "/mo");
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.slash_menu_dismissed_for.as_deref(), Some("/mo"));
}

#[tokio::test]
async fn slash_menu_esc_clears_unknown_command_like_regular_input() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/unknown");

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "");
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.slash_menu_dismissed_for.as_deref(), None);
}

#[tokio::test]
async fn slash_menu_cursor_movement_preserves_dismissal() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/mo");

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "/mo");
    assert_eq!(app.slash_menu_dismissed_for.as_deref(), Some("/mo"));
}

#[tokio::test]
async fn slash_debug_prompt_opens_overlay() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/debug-prompt");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.overlay,
        OverlayState::DebugPromptInput {
            composer: ComposerState::new()
        }
    );
    assert_eq!(app.composer.as_str(), "");
}

#[tokio::test]
async fn slash_model_opens_model_picker_overlay() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/model");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.overlay,
        OverlayState::ModelPicker {
            filter: String::new(),
            selected: 0
        }
    );
    assert_eq!(app.composer.as_str(), "");
}

#[tokio::test]
async fn slash_state_opens_agent_state_overlay() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/state");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.overlay, OverlayState::AgentState { scroll: 0 });
    assert_eq!(app.composer.as_str(), "");
    assert_eq!(app.status_line, "Opened agent state overlay");
}

#[tokio::test]
async fn agent_state_overlay_scrolls_and_esc_closes() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.overlay = OverlayState::AgentState { scroll: 0 };

    app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.overlay, OverlayState::AgentState { scroll: 10 });

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.overlay, OverlayState::None);
}

#[tokio::test]
async fn slash_display_sets_chat_display_mode() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/display 5");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.display_mode, OperatorDisplayMode::Debug);
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.composer.as_str(), "");
    assert_eq!(app.status_line, "Display mode set to debug (5)");
}

#[tokio::test]
async fn slash_display_accepts_named_modes() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/display VERBOSE");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.display_mode, OperatorDisplayMode::Verbose);
    assert_eq!(app.status_line, "Display mode set to verbose (4)");
}

#[tokio::test]
async fn slash_menu_enter_runs_selected_prefix_command() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/mo");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.overlay,
        OverlayState::ModelPicker {
            filter: String::new(),
            selected: 0
        }
    );
    assert_eq!(app.composer.as_str(), "");
}

#[tokio::test]
async fn slash_menu_enter_runs_selected_command_from_root_menu() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/");
    app.slash_menu_selected = 1;

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.overlay, OverlayState::Agents { selected: 0 });
    assert_eq!(app.composer.as_str(), "");
}

#[test]
fn centered_rect_rows_uses_fixed_height() {
    let area = Rect::new(0, 0, 100, 40);
    let popup = centered_rect_rows(56, 7, area);
    assert_eq!(popup.width, 56);
    assert_eq!(popup.height, 7);
}

#[test]
fn chat_scroll_defaults_to_follow_tail() {
    let scroll = ChatScrollState::new();
    assert_eq!(scroll.effective_scroll(12), 12);
}

#[test]
fn chat_scroll_moves_away_from_and_back_to_tail() {
    let mut scroll = ChatScrollState::new();
    scroll.scroll_with_key(KeyCode::Up, 12);
    assert_eq!(scroll.effective_scroll(12), 11);
    assert!(!scroll.is_following_tail());

    scroll.scroll_with_key(KeyCode::Down, 12);
    assert_eq!(scroll.effective_scroll(12), 12);
    assert!(scroll.is_following_tail());
}

#[test]
fn chat_scroll_moves_predictably_toward_tail_after_home() {
    let mut scroll = ChatScrollState::new();
    scroll.scroll_with_key(KeyCode::Home, 12);
    assert_eq!(scroll.effective_scroll(12), 0);

    scroll.scroll_with_key(KeyCode::Down, 12);
    assert_eq!(scroll.effective_scroll(12), 1);
    assert!(!scroll.is_following_tail());

    scroll.scroll_with_key(KeyCode::PageDown, 12);
    assert_eq!(scroll.effective_scroll(12), 11);
    assert!(!scroll.is_following_tail());

    scroll.scroll_with_key(KeyCode::Down, 12);
    assert_eq!(scroll.effective_scroll(12), 12);
    assert!(scroll.is_following_tail());
}

#[test]
fn paragraph_max_scroll_tracks_wrapped_chat_height() {
    let area = Rect::new(0, 0, 20, 6);
    let text = Text::from(vec![
        Line::from("1234567890123456789"),
        Line::from(""),
        Line::from("abcdefghijklmnopqrs"),
    ]);
    assert_eq!(paragraph_max_scroll(&text, area), 1);
}

#[test]
fn paragraph_max_scroll_matches_word_wrapped_paragraph_height() {
    let area = Rect::new(0, 0, 14, 5);
    let text = Text::from("alpha beta gamma delta epsilon zeta");
    assert_eq!(paragraph_max_scroll(&text, area), 0);
}

#[test]
fn paragraph_max_scroll_counts_unicode_display_width() {
    let area = Rect::new(0, 0, 6, 4);
    let text = Text::from(vec![Line::from("你好你好你")]);
    assert_eq!(paragraph_max_scroll(&text, area), 1);
}

#[test]
fn paragraph_max_scroll_counts_wide_graphemes_in_narrow_panes() {
    let area = Rect::new(0, 0, 3, 3);
    let text = Text::from(vec![Line::from("你")]);
    assert_eq!(paragraph_max_scroll(&text, area), 0);
}

#[test]
fn paragraph_max_scroll_handles_long_whitespace_runs() {
    let area = Rect::new(0, 0, 6, 3);
    let text = Text::from(vec![Line::from("abcd      ")]);
    assert_eq!(paragraph_max_scroll(&text, area), 1);
}

#[test]
fn paragraph_max_scroll_unframed_uses_full_area() {
    let area = Rect::new(0, 0, 12, 2);
    let text = Text::from("one\ntwo\nthree");
    assert_eq!(paragraph_max_scroll_unframed(&text, area), 1);
    assert_eq!(paragraph_max_scroll(&text, area), 0);
}

#[test]
fn default_tui_render_omits_agent_state_panel_and_box_borders() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(TuiProjection::from_snapshot(sample_snapshot(
        "default", "evt-0",
    )));
    app.apply_projection_view();

    let backend = TestBackend::new(100, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();

    let rendered = rendered_buffer_text(&terminal);
    assert!(!rendered.contains("Agent State"));
    assert!(!rendered.contains("Conversation"));
    let rows = rendered_buffer_rows(&terminal);
    let main_rows = &rows[2..22];
    assert!(!main_rows.iter().any(|row| row.contains('│')));
}

#[test]
fn prompt_render_preserves_blank_multiline_rows() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(TuiProjection::from_snapshot(sample_snapshot(
        "default", "evt-0",
    )));
    app.apply_projection_view();
    app.composer = ComposerState::from("first\n\nsecond");

    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();

    let rows = rendered_buffer_rows(&terminal);
    let first_row = rows
        .iter()
        .position(|row| row.starts_with("> first"))
        .expect("first prompt row should render");
    assert!(rows[first_row + 1].starts_with("  "));
    assert!(rows[first_row + 1].trim().is_empty());
    assert!(rows[first_row + 2].starts_with("  second"));
}

#[test]
fn chat_text_uses_placeholder_when_empty() {
    let client = LocalClient::new(test_config()).unwrap();
    let app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let rendered: String = chat_text(&app)
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("No chat history yet"));
}

#[test]
fn chat_text_renders_markdown_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: Utc::now(),
        text: "**Done**\n- first\n- second".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let lines: Vec<String> = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(lines.iter().any(|line| line.contains("Done")));
    assert!(lines.iter().any(|line| line.contains("  - first")));
    assert!(lines.iter().any(|line| line.contains("  - second")));
}

#[test]
fn chat_text_keeps_markdown_block_separator_unindented() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: Utc::now(),
        text: "### Title\n\nBody".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let lines = build_chat_text(&collect_chat_items(&app)).lines;
    let title_index = lines
        .iter()
        .position(|line| {
            let rendered_line: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            rendered_line.contains("### Title")
        })
        .expect("heading should render");
    let blank_line = lines
        .get(title_index + 1)
        .expect("markdown block separator should render");
    assert!(blank_line.spans.is_empty());

    let body_line = lines
        .get(title_index + 2)
        .expect("heading body should render");
    let rendered_body: String = body_line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(rendered_body.ends_with("Body"));
}

#[test]
fn chat_text_skips_ack_briefs() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.briefs = vec![
        BriefRecord {
            id: "brief-ack".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            kind: BriefKind::Ack,
            created_at: Utc::now(),
            text: "Queued work: duplicate".into(),
            attachments: None,
            related_message_id: Some("msg-1".into()),
            related_task_id: None,
        },
        BriefRecord {
            id: "brief-result".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            text: "Real response".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    ];

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(!rendered.contains("Queued work: duplicate"));
    assert!(rendered.contains("Real response"));
}

#[test]
fn chat_text_summarizes_task_brief_output() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.briefs = vec![BriefRecord {
        id: "brief-task".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: Utc::now(),
        text: "Task task-1 completed: line one\nline two\nline three".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: Some("task-1".into()),
    }];

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Task task-1: Task task-1 completed: line one"));
    assert!(rendered.contains("Task output is available in the Tasks pane."));
    assert!(!rendered.contains("line two"));
}

#[test]
fn chat_text_shows_active_assistant_preview_without_durable_system_event() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-work".into(),
            event: "work_item_written".into(),
            data: StreamEventEnvelope {
                id: "evt-work".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "work_item_written".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "action": "created",
                    "record": {
                        "id": "work-1",
                        "agent_id": "default",
                        "workspace_id": "agent_home",
                        "objective": "prepare rollout plan",
                        "state": "open",
                        "plan_status": "draft",
                        "todo_list": [],
                        "created_at": Utc::now(),
                        "updated_at": Utc::now()
                    }
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-assistant".into(),
            event: "assistant_round_recorded".into(),
            data: StreamEventEnvelope {
                id: "evt-assistant".into(),
                seq: 3,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "assistant_round_recorded".into(),
                projection: None,
                provenance: None,
                payload: json!({ "round": 1, "text_preview": "hidden assistant partial" }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Assistant hidden assistant partial"));
    assert!(!rendered.contains("Action    Waiting for activity"));
    assert!(!rendered.contains("Current   "));
}

#[test]
fn chat_display_mode_debug_shows_debug_events_and_keeps_working_row() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.display_mode = OperatorDisplayMode::Debug;
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                id: "evt-tool".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-state".into(),
            event: "agent_state_changed".into(),
            data: StreamEventEnvelope {
                id: "evt-state".into(),
                seq: 3,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "agent_state_changed".into(),
                projection: None,
                provenance: None,
                payload: json!({ "status": "AwakeRunning" }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(projection);

    let items = collect_chat_items(&app);
    let rendered: String = build_chat_text(&items)
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(items.iter().any(|item| matches!(
        item,
        ConversationCell::SystemNotice { body, .. }
            if body.contains("cargo test tui")
    )));
    assert!(rendered.contains("cargo test tui"));
    assert!(!rendered.contains("State sync"));
    assert!(!rendered.contains("agent_state_changed"));
    assert!(rendered.contains("Working"));
}

#[test]
fn chat_text_omits_task_system_events() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    projection.apply_event(AgentStreamEvent {
            id: "evt-task".into(),
            event: "task_result_received".into(),
            data: StreamEventEnvelope {
                id: "evt-task".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "task_result_received".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "id": "task-1",
                    "agent_id": "default",
                    "kind": "ExecCommand",
                    "status": "completed",
                    "created_at": Utc::now(),
                    "updated_at": Utc::now(),
                    "parent_message_id": null,
                    "summary": "Run command: cargo test --lib wake_hint_preserved_when_replaced_during_emission 2>&1",
                    "detail": null,
                    "recovery": null
                }),
            },
        }, &crate::tui::logging::TuiLogWriter::new_temp().unwrap());
    app.projection = Some(projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(!rendered.contains("Run command: cargo test"));
    assert!(!rendered.contains("System (work)"));
}

#[test]
fn chat_text_keeps_active_activity_after_brief_event() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    snapshot.agent.agent.working_memory.current_working_memory =
        crate::types::WorkingMemorySnapshot {
            current_work_item_id: Some("work-1".into()),
            objective: Some("fix TUI active activity".into()),
            work_summary: Some("Improve the Conversation working indicator".into()),
            ..Default::default()
        };
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                id: "evt-tool".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-brief".into(),
            event: "brief_created".into(),
            data: StreamEventEnvelope {
                id: "evt-brief".into(),
                seq: 3,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "brief_created".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "id": "brief-1",
                    "agent_id": "default",
                    "workspace_id": crate::types::AGENT_HOME_WORKSPACE_ID,
                    "work_item_id": null,
                    "kind": "result",
                    "created_at": Utc::now(),
                    "text": "Still working",
                    "attachments": null,
                    "related_message_id": null,
                    "related_task_id": null
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Working"));
    assert!(!rendered.contains("Current   "));
    assert!(!rendered.contains("Assistant ..."));
    assert!(!rendered.contains("Action    Waiting for activity"));
}

#[test]
fn chat_text_keeps_active_action_after_snapshot_refresh() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-refresh");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    snapshot.agent.agent.working_memory.current_working_memory =
        crate::types::WorkingMemorySnapshot {
            work_summary: Some("Keep the active action stable".into()),
            ..Default::default()
        };
    let mut previous_projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    previous_projection.apply_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                id: "evt-tool".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut refreshed_projection = TuiProjection::from_snapshot(snapshot);
    refreshed_projection.inherit_recent_event_logs_from(&mut previous_projection);
    app.projection = Some(refreshed_projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Action    Command finished: cargo test tui"));
    assert!(!rendered.contains("Action    Waiting for activity"));
    assert!(!rendered.contains("Current   "));
}

#[test]
fn chat_text_uses_selected_agent_events_tail_after_switch() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut previous_projection = TuiProjection::from_snapshot(sample_snapshot("agent-a", "a0"));
    previous_projection.agent.agent.status = AgentStatus::AwakeRunning;
    previous_projection.apply_event(
        AgentStreamEvent {
            id: "evt-a-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                id: "evt-a-tool".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "agent-a".into(),
                event_type: "tool_executed".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test agent-a"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(previous_projection);
    let before_switch: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(before_switch.contains("Action    Command finished: cargo test agent-a"));

    let mut switched_snapshot = sample_snapshot("agent-b", "evt-b-tool");
    switched_snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    switched_snapshot.events_tail = vec![StreamEventEnvelope {
        id: "evt-b-tool".into(),
        seq: 0,
        ts: Utc::now(),
        agent_id: "agent-b".into(),
        event_type: "tool_executed".into(),
        projection: Some(json!({
            "name": "operator",
            "raw_payload_included": true,
        })),
        provenance: None,
        payload: json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "cargo test agent-b"
        }),
    }];

    // Switching agents must use the selected agent's snapshot tail rather
    // than inheriting the previous agent's event log.
    app.projection = Some(TuiProjection::from_snapshot(switched_snapshot));

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Working"));
    assert!(rendered.contains("Action    Command finished: cargo test agent-b"));
    assert!(!rendered.contains("cargo test agent-a"));
}

#[test]
fn chat_text_does_not_show_stale_activity_when_agent_is_idle() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeIdle;
    snapshot.agent.agent.pending = 0;
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                id: "evt-tool".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test stale"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(!rendered.contains("Holon (working)"));
    assert!(!rendered.contains("cargo test stale"));
}

#[test]
fn chat_text_shows_pending_queue_as_active_activity() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeIdle;
    snapshot.agent.agent.pending = 1;
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();

    assert!(rendered.contains("Queued"));
    assert!(!rendered.contains("Current   "));
    assert!(!rendered.contains("Assistant ..."));
    assert!(!rendered.contains("Action    Waiting for activity"));
    assert!(!rendered.contains("Queue: pending 1, active tasks 0"));
}

#[test]
fn active_activity_timestamp_does_not_sort_before_tail_history() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    app.briefs = vec![BriefRecord {
        id: "brief-latest".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: ts + chrono::Duration::seconds(10),
        text: "Latest durable response".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                id: "evt-tool".into(),
                seq: 2,
                ts,
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                projection: None,
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(projection);

    let items = collect_chat_items(&app);
    let active_item = items.last().expect("active activity item");
    let previous_item = items
        .get(items.len().saturating_sub(2))
        .expect("durable item before active activity");

    match active_item {
        ConversationCell::ActiveActivity {
            speaker,
            created_at,
            ..
        } => {
            assert!(speaker.starts_with("Holon (working)"));
            assert!(*created_at >= previous_item.created_at());
        }
        other => panic!("expected active activity item, got {other:?}"),
    }
}

#[test]
fn active_activity_cells_stay_stable_without_new_events() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    snapshot.agent.agent.working_memory.current_working_memory =
        crate::types::WorkingMemorySnapshot {
            work_summary: Some("Keep cache stable while working".into()),
            ..Default::default()
        };
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let first_items = collect_chat_items(&app);
    let second_items = collect_chat_items(&app);
    assert_eq!(first_items, second_items);

    let _ = chat_text(&app);
    let cached_cells = app
        .chat_text_cache
        .borrow()
        .as_ref()
        .expect("active activity should be cached")
        .cells
        .clone();
    let _ = chat_text(&app);
    assert_eq!(
        cached_cells,
        app.chat_text_cache
            .borrow()
            .as_ref()
            .expect("active activity should remain cached")
            .cells
    );
}

#[test]
fn collect_chat_items_orders_equal_timestamps_deterministically() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    app.transcript = vec![TranscriptEntry {
        id: "msg-1".into(),
        agent_id: "default".into(),
        created_at: ts,
        kind: TranscriptEntryKind::IncomingMessage,
        round: None,
        related_message_id: Some("m1".into()),
        stop_reason: None,
        input_tokens: None,
        output_tokens: None,
        data: json!({
            "origin": { "kind": "operator", "actor_id": null },
            "body": { "type": "text", "text": "same instant" }
        }),
    }];
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: ts,
        text: "same instant".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let items = collect_chat_items(&app);
    assert!(matches!(items[0], ConversationCell::UserMessage { .. }));
    assert!(matches!(items[1], ConversationCell::AssistantMarkdown(_)));
}

#[test]
fn chat_includes_pending_operator_message_from_snapshot() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.operator_messages = vec![OperatorMessageRecord {
        message_id: "message-queued".into(),
        agent_id: "default".into(),
        status: OperatorMessageStatus::WaitingForSafePoint,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        body: MessageBody::Text {
            text: "please stop soon".into(),
        },
        error: None,
    }];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let items = collect_chat_items(&app);
    let user_messages = items
        .iter()
        .filter(|item| matches!(item, ConversationCell::UserMessage { .. }))
        .collect::<Vec<_>>();

    assert_eq!(user_messages.len(), 1);
    assert!(matches!(
        user_messages[0],
        ConversationCell::UserMessage {
            body,
            status: Some(OperatorMessageStatus::WaitingForSafePoint),
            ..
        } if body == "please stop soon"
    ));
}

#[test]
fn chat_dedupes_pending_operator_message_when_transcript_contains_it() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.transcript_tail = vec![TranscriptEntry {
        id: "tr-message-1".into(),
        agent_id: "default".into(),
        created_at: ts,
        kind: TranscriptEntryKind::IncomingMessage,
        round: None,
        related_message_id: Some("message-1".into()),
        stop_reason: None,
        input_tokens: None,
        output_tokens: None,
        data: json!({
            "origin": { "kind": "operator", "actor_id": null },
            "body": { "type": "text", "text": "persisted operator text" }
        }),
    }];
    snapshot.operator_messages = vec![OperatorMessageRecord {
        message_id: "message-1".into(),
        agent_id: "default".into(),
        status: OperatorMessageStatus::Processing,
        created_at: ts,
        updated_at: ts,
        body: MessageBody::Text {
            text: "persisted operator text".into(),
        },
        error: None,
    }];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    app.apply_projection_view();

    let items = collect_chat_items(&app);
    let user_messages = items
        .iter()
        .filter(|item| matches!(item, ConversationCell::UserMessage { .. }))
        .collect::<Vec<_>>();

    assert_eq!(user_messages.len(), 1);
    assert!(matches!(
        user_messages[0],
        ConversationCell::UserMessage {
            body,
            status: Some(OperatorMessageStatus::Processing),
            ..
        } if body == "persisted operator text"
    ));
}

#[test]
fn chat_text_omits_processing_and_processed_operator_status_labels() {
    for status in [
        OperatorMessageStatus::Processing,
        OperatorMessageStatus::Processed,
    ] {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        let ts = Utc::now();
        let mut snapshot = sample_snapshot("default", "evt-0");
        snapshot.operator_messages = vec![OperatorMessageRecord {
            message_id: "message-1".into(),
            agent_id: "default".into(),
            status: status.clone(),
            created_at: ts,
            updated_at: ts,
            body: MessageBody::Text {
                text: "operator text".into(),
            },
            error: None,
        }];
        app.projection = Some(TuiProjection::from_snapshot(snapshot));
        app.apply_projection_view();

        let rendered: String = build_chat_text(&collect_chat_items(&app))
            .lines
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content))
            .collect();
        assert!(rendered.contains("operator text"));
        assert!(!rendered.contains("[processing]"));
        assert!(!rendered.contains("[processed]"));
    }
}

#[test]
fn projection_operator_message_prunes_reconciled_optimistic_entry() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    app.optimistic_operator_messages = vec![OperatorMessageRecord {
        message_id: "message-1".into(),
        agent_id: "default".into(),
        status: OperatorMessageStatus::Queued,
        created_at: ts,
        updated_at: ts,
        body: MessageBody::Text {
            text: "optimistic text".into(),
        },
        error: None,
    }];

    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.operator_messages = vec![OperatorMessageRecord {
        message_id: "message-1".into(),
        agent_id: "default".into(),
        status: OperatorMessageStatus::Processing,
        created_at: ts,
        updated_at: ts,
        body: MessageBody::Text {
            text: "durable text".into(),
        },
        error: None,
    }];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    app.apply_projection_view();

    assert!(app.optimistic_operator_messages.is_empty());
    let items = collect_chat_items(&app);
    let user_messages = items
        .iter()
        .filter(|item| matches!(item, ConversationCell::UserMessage { .. }))
        .collect::<Vec<_>>();
    assert_eq!(user_messages.len(), 1);
    assert!(matches!(
        user_messages[0],
        ConversationCell::UserMessage {
            body,
            status: Some(OperatorMessageStatus::Processing),
            ..
        } if body == "durable text"
    ));
}

#[test]
fn events_overlay_selection_stays_pinned_to_same_event_id() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-old".into(),
            event: "provider_round_completed".into(),
            data: StreamEventEnvelope {
                id: "evt-old".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "provider_round_completed".into(),
                projection: None,
                provenance: None,
                payload: json!({"round": 1, "stop_reason": "end_turn"}),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(projection);
    app.overlay = OverlayState::Events {
        selected_event_id: Some("evt-old".into()),
        detail_scroll: 0,
    };

    if let Some(projection) = app.projection.as_mut() {
        projection.apply_event(
            AgentStreamEvent {
                id: "evt-new".into(),
                event: "provider_round_completed".into(),
                data: StreamEventEnvelope {
                    id: "evt-new".into(),
                    seq: 3,
                    ts: Utc::now(),
                    agent_id: "default".into(),
                    event_type: "provider_round_completed".into(),
                    projection: None,
                    provenance: None,
                    payload: json!({"round": 2, "stop_reason": "end_turn"}),
                },
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
    }
    app.apply_projection_view();

    assert_eq!(
        app.overlay,
        OverlayState::Events {
            selected_event_id: Some("evt-old".into()),
            detail_scroll: 0
        }
    );
}

#[test]
fn streaming_transcript_merge_dedupes_persisted_message_by_related_message_id() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.transcript_tail = vec![TranscriptEntry {
        id: "persisted-transcript-entry".into(),
        agent_id: "default".into(),
        created_at: Utc::now(),
        kind: TranscriptEntryKind::IncomingMessage,
        round: None,
        related_message_id: Some("message-1".into()),
        stop_reason: None,
        input_tokens: None,
        output_tokens: None,
        data: json!({
            "body": { "type": "text", "text": "persisted" }
        }),
    }];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    app.connection_state = TuiConnectionState::Streaming;
    app.transcript = vec![TranscriptEntry {
        id: "stream-message-1".into(),
        agent_id: "default".into(),
        created_at: Utc::now(),
        kind: TranscriptEntryKind::IncomingMessage,
        round: None,
        related_message_id: Some("message-1".into()),
        stop_reason: None,
        input_tokens: None,
        output_tokens: None,
        data: json!({
            "body": { "type": "text", "text": "streamed" }
        }),
    }];

    app.apply_projection_view();

    assert_eq!(app.transcript.len(), 1);
    assert_eq!(app.transcript[0].id, "persisted-transcript-entry");
}

#[tokio::test]
async fn snapshot_refresh_preserves_sse_only_transcript_entries() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("default")];
    app.selected_agent = 0;
    app.projection = Some(TuiProjection::from_snapshot(sample_snapshot(
        "default", "cursor-1",
    )));
    app.transcript = vec![TranscriptEntry {
        id: "stream-only-entry".into(),
        agent_id: "default".into(),
        created_at: Utc::now(),
        kind: TranscriptEntryKind::AssistantRound,
        round: Some(1),
        related_message_id: None,
        stop_reason: None,
        input_tokens: None,
        output_tokens: None,
        data: json!({
            "body": { "type": "text", "text": "streamed only" }
        }),
    }];
    app.snapshot_refresh_request_id = 1;

    app.apply_snapshot_result(
        1,
        0,
        "default".into(),
        None,
        Ok(sample_snapshot("default", "cursor-2")),
    );

    assert!(app
        .transcript
        .iter()
        .any(|entry| entry.id == "stream-only-entry"));
}

#[test]
fn snapshot_refresh_failure_updates_status_line() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("default")];
    app.selected_agent = 0;
    app.snapshot_refresh_request_id = 7;

    app.apply_snapshot_result(7, 0, "default".into(), None, Err("network down".into()));

    assert_eq!(
        app.status_line,
        "Snapshot refresh failed for default: network down"
    );
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::RefreshRequired { .. }
    ));
}

#[test]
fn chat_text_keeps_long_brief_content() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let long_text = format!(
        "{}\n{}",
        "intro ".repeat(220),
        "tail marker that used to be trimmed away"
    );
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: Utc::now(),
        text: long_text,
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("tail marker that used to be trimmed away"));
}

#[test]
fn chat_text_cache_reuses_unchanged_content_and_replaces_stale_entries() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let first_created_at = Utc::now();
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: first_created_at,
        text: "**Done**".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let first = chat_text(&app);
    let second = chat_text(&app);
    assert_eq!(first.lines, second.lines);

    {
        let cache_ref = app.chat_text_cache.borrow();
        let cached = cache_ref.as_ref().expect("chat text should be cached");
        assert_eq!(cached.cells, collect_chat_items(&app));
    }

    app.briefs = vec![BriefRecord {
        id: "brief-2".into(),
        agent_id: "default".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Failure,
        created_at: first_created_at,
        text: "**Failed**".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let refreshed = chat_text(&app);
    let refreshed_lines: Vec<String> = refreshed
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(refreshed_lines.iter().any(|line| line.contains("Failed")));

    let cache_ref = app.chat_text_cache.borrow();
    let cached = cache_ref.as_ref().expect("chat text should be recached");
    assert_eq!(cached.cells, collect_chat_items(&app));

    drop(cache_ref);
    app.briefs.clear();
    let placeholder = chat_text(&app);
    let placeholder_text: String = placeholder
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(placeholder_text.contains("No chat history yet"));
    assert!(app.chat_text_cache.borrow().is_none());
}

#[test]
fn disconnect_message_schedules_reconnect() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let tx = app.runtime_tx.clone();
    app.connection_state = TuiConnectionState::Streaming;

    tx.send(TuiRuntimeMessage::Disconnected {
        error: "socket closed".into(),
    })
    .unwrap();
    assert!(app.process_runtime_messages());

    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Reconnecting { attempt: 1, .. }
    ));
    assert_eq!(app.connection_detail(), Some("socket closed"));
    assert!(app.reconnect_deadline.is_some());
}

#[test]
fn cursor_expiry_marks_refresh_required() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.schedule_refresh("cursor evt_123 is too old or not found".into());

    assert!(matches!(
        app.connection_state,
        TuiConnectionState::RefreshRequired { .. }
    ));
    assert_eq!(
        app.connection_detail(),
        Some("cursor evt_123 is too old or not found")
    );
    assert!(app.refresh_deadline.is_some());
}

#[test]
fn cursor_too_old_detection_uses_typed_http_error() {
    let err = crate::client::LocalHttpError {
        path: "/agents/default/events".into(),
        status_code: 410,
        message: "cursor evt_123 is too old or not found".into(),
        code: Some("cursor_too_old".into()),
        hint: None,
    };
    let err = anyhow::Error::new(err);
    assert!(is_cursor_too_old_error(&err));
}

#[test]
fn stale_projection_event_schedules_refresh() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("default")];
    app.selected_agent = 0;
    app.projection = Some(TuiProjection::from_snapshot(sample_snapshot(
        "default", "cursor-1",
    )));
    app.connection_state = TuiConnectionState::Streaming;
    let tx = app.runtime_tx.clone();

    tx.send(TuiRuntimeMessage::Event(AgentStreamEvent {
        id: "evt-stale".into(),
        event: "waiting_intent_created".into(),
        data: StreamEventEnvelope {
            id: "evt-stale".into(),
            seq: 2,
            ts: Utc::now(),
            agent_id: "default".into(),
            event_type: "waiting_intent_created".into(),
            projection: None,
            provenance: None,
            payload: json!({
                "waiting_intent_id": "wait-2",
                "external_trigger_id": "cb-2",
                "agent_id": "default",
                "source": "github"
            }),
        },
    }))
    .unwrap();

    assert!(!app.process_runtime_messages());
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::RefreshRequired { .. }
    ));
    assert!(app
        .connection_detail()
        .is_some_and(|detail| detail.contains("projection stale")));
    assert!(app.refresh_deadline.is_some());
}

#[test]
fn apply_agent_list_preserves_selected_agent_by_id() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 1;
    app.projection = Some(crate::tui::projection::TuiProjection::from_snapshot(
        sample_snapshot("beta", "cursor-1"),
    ));

    let change = app.apply_agent_list(vec![
        sample_agent_summary("gamma"),
        sample_agent_summary("beta"),
        sample_agent_summary("alpha"),
    ]);

    assert_eq!(change, AgentListChange::Ready);
    assert_eq!(app.selected_agent_id(), Some("beta"));
    assert!(app.projection.is_some());
}

#[test]
fn slim_agent_list_refresh_preserves_selected_projection_summary() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 1;
    app.projection = Some(crate::tui::projection::TuiProjection::from_snapshot(
        sample_snapshot("beta", "cursor-1"),
    ));

    let beta_entry = AgentListEntry::from_summary(&sample_agent_summary("beta"));
    app.apply_loaded_agents(Ok(vec![
        AgentListEntry::from_summary(&sample_agent_summary("alpha")),
        beta_entry,
    ]));

    let selected = app.selected_agent_summary().unwrap();
    assert_eq!(selected.identity.agent_id, "beta");
    assert_eq!(selected.recent_event_count, 1);
    assert_eq!(selected.model.resolved_policy.description, "Sample policy");
    assert!(app.projection.is_some());
}

#[test]
fn apply_agent_list_clears_stale_projection_when_selected_agent_disappears() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 1;
    app.projection = Some(crate::tui::projection::TuiProjection::from_snapshot(
        sample_snapshot("beta", "cursor-1"),
    ));
    app.briefs = vec![BriefRecord {
        id: "brief-1".into(),
        agent_id: "beta".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        kind: BriefKind::Result,
        created_at: Utc::now(),
        text: "stale brief".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    }];

    let change = app.apply_agent_list(vec![sample_agent_summary("gamma")]);

    assert_eq!(change, AgentListChange::RequiresBootstrap);
    assert_eq!(app.selected_agent_id(), Some("gamma"));
    assert!(app.projection.is_none());
    assert!(app.briefs.is_empty());
}

#[tokio::test]
async fn agent_switch_starts_snapshot_refresh_without_awaiting_network() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 0;
    app.connection_state = TuiConnectionState::Streaming;
    app.status_line = "Streaming native events for agent alpha".into();

    app.begin_bootstrap_agent_index(1);

    assert_eq!(app.selected_agent_id(), Some("alpha"));
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Bootstrapping
    ));
    assert!(app.snapshot_refresh_in_flight);
    assert_eq!(app.status_line, "Bootstrapping agent beta from /state");
}

#[tokio::test]
async fn remote_tick_does_not_await_slow_agent_list_refresh() {
    let client = slow_remote_client().await;
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agent_list_refresh_deadline = Some(std::time::Instant::now());

    tokio::time::timeout(std::time::Duration::from_millis(50), app.tick())
        .await
        .expect("tick should not wait for slow /agents/list")
        .unwrap();

    assert!(app.agent_list_refresh_in_flight);
}

#[tokio::test]
async fn remote_tick_does_not_await_slow_snapshot_refresh() {
    let client = slow_remote_client().await;
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("default")];
    app.selected_agent = 0;
    app.schedule_refresh("test refresh".into());
    app.refresh_deadline = Some(std::time::Instant::now());

    tokio::time::timeout(std::time::Duration::from_millis(50), app.tick())
        .await
        .expect("tick should not wait for slow /state")
        .unwrap();

    assert!(app.snapshot_refresh_in_flight);
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Bootstrapping
    ));
}

#[tokio::test]
async fn remote_tick_does_not_await_slow_event_stream_reconnect() {
    let client = slow_remote_client().await;
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("default")];
    app.selected_agent = 0;
    app.projection = Some(TuiProjection::from_snapshot(sample_snapshot(
        "default", "cursor-1",
    )));
    app.schedule_reconnect("test reconnect".into());
    app.reconnect_deadline = Some(std::time::Instant::now());

    tokio::time::timeout(std::time::Duration::from_millis(50), app.tick())
        .await
        .expect("tick should not wait for slow event stream reconnect")
        .unwrap();

    assert!(app.stream_connect_in_flight);
}

async fn slow_remote_client() -> LocalClient {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _peer)) = listener.accept().await {
            tokio::spawn(async move {
                let _socket = socket;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            });
        }
    });
    LocalClient::remote(test_config(), format!("http://{addr}"), "secret").unwrap()
}

fn test_app() -> TuiApp {
    let client = LocalClient::new(test_config()).unwrap();
    TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    )
}

#[test]
fn history_navigation_browses_multiple_entries() {
    let mut app = test_app();
    app.input_history = vec!["cmd1".into(), "cmd2".into(), "cmd3".into()];
    app.history_index = None;
    app.composer.clear();

    // Navigate up: should go to cmd3 (most recent)
    app.navigate_history(-1);
    assert_eq!(app.history_index, Some(2));
    assert_eq!(app.composer.as_str(), "cmd3");

    // Navigate up again: should go to cmd2
    app.navigate_history(-1);
    assert_eq!(app.history_index, Some(1));
    assert_eq!(app.composer.as_str(), "cmd2");

    // Navigate down: should go back to cmd3
    app.navigate_history(1);
    assert_eq!(app.history_index, Some(2));
    assert_eq!(app.composer.as_str(), "cmd3");

    // Navigate down past the end: should clear composer
    app.navigate_history(1);
    assert_eq!(app.history_index, None);
    assert!(app.composer.is_empty());
}
