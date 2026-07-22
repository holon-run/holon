use super::{
    app::{ComposerEditMode, TuiApp},
    chat::{
        build_chat_text, build_chat_text_for_width, chat_text, collect_chat_items,
        is_operator_origin_value, paragraph_max_scroll, paragraph_max_scroll_unframed,
        ChatScrollState, ConversationCell, ConversationDisplayKind, LocalCommandOutput,
    },
    composer::ComposerState,
    determine_alt_screen_mode_for_terminal,
    overlay::{centered_rect_rows, conversation_events_overlay_lines, OverlayState},
    projection::{OperatorDisplayMode, TuiProjection},
    render::draw,
    runtime::{
        is_cursor_not_found_error, reconnect_delay_for_attempt, AgentListChange,
        TuiConnectionState, TuiRuntimeMessage, BOOTSTRAP_EVENT_TAIL_LIMIT,
        EVENT_HISTORY_PAGE_LIMIT,
    },
    state::{tui_state_path, TuiClientState},
    view_model::{HeaderViewModel, StatusbarViewModel},
};
use crate::tui::keymap::DEFAULT_BINDING_HINTS;
use crate::{
    client::{
        AgentStateSnapshot, AgentStreamEvent, LocalClient, StateSessionSnapshot,
        StateWorkspaceSnapshot, StreamEventEnvelope, TUI_LOCAL_NETWORK_POLICY,
        TUI_REMOTE_NETWORK_POLICY,
    },
    config::{AltScreenMode, AppConfig},
    system::{ExecutionProfile, ExecutionSnapshot},
    types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentListEntry, AgentModelSource,
        AgentModelState, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentStatus,
        AgentSummary, AgentTokenUsageSummary, AgentVisibility, BriefContentSource, BriefKind,
        BriefRecord, ChildAgentSummary, ClosureDecision, ClosureOutcome, LoadedAgentsMdView,
        MessageBody, OperatorMessageRecord, OperatorMessageStatus, RuntimePosture,
        SkillsRuntimeView, TokenUsage,
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
        api_cors: Default::default(),
        config_file_path: temp.join("config.json"),
        stored_config: Default::default(),
        default_model: crate::config::ModelRouteRef::parse_compatible(
            "anthropic/claude-sonnet-4-6",
        )
        .unwrap(),
        fallback_models: Vec::new(),
        vision_model: None,
        image_generation_model: None,
        vision_candidate_models: Vec::new(),
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
        max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
        disable_provider_fallback: false,
        tui_alternate_screen: AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        model_discovery_cache: Default::default(),
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
    let mut state = TuiClientState::new("beta");
    state.set_agent_display_mode("beta", OperatorDisplayMode::Verbose);
    state.save(&state_path).unwrap();
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
    assert_eq!(app.display_mode, OperatorDisplayMode::Verbose);
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

#[test]
fn recording_selected_agent_preserves_display_preferences() {
    let config = test_config();
    let state_path = config.home_dir.join("state").join("tui").join("local.json");
    let mut state = TuiClientState::new("alpha");
    state.set_agent_display_mode("alpha", OperatorDisplayMode::Debug);
    state.save(&state_path).unwrap();
    let client = LocalClient::new(config).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.record_selected_agent("beta");

    let loaded = TuiClientState::load(&state_path).unwrap();
    assert_eq!(loaded.last_selected_agent_id, "beta");
    assert_eq!(
        loaded.effective_display_mode("alpha"),
        OperatorDisplayMode::Debug
    );
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
        scheduling_posture: Default::default(),
        model: AgentModelState {
            effective_model: crate::config::ModelRouteRef::parse_compatible(
                "anthropic/claude-sonnet-4-6",
            )
            .unwrap(),
            requested_model: Some(
                crate::config::ModelRouteRef::parse_compatible("anthropic/claude-sonnet-4-6")
                    .unwrap(),
            ),
            active_model: Some(
                crate::config::ModelRouteRef::parse_compatible("anthropic/claude-sonnet-4-6")
                    .unwrap(),
            ),
            fallback_active: false,
            runtime_default_model: crate::config::ModelRouteRef::parse_compatible(
                "anthropic/claude-sonnet-4-6",
            )
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
                verbosity: None,
                tool_output_truncation_estimated_tokens: 2_500,
                max_output_tokens_upper_limit: Some(128_000),
                capabilities: crate::model_catalog::ModelCapabilityFlags {
                    image_input: true,
                    ..crate::model_catalog::ModelCapabilityFlags::default()
                },
                reasoning_effort_options: Vec::new(),
                source: crate::model_catalog::ModelMetadataSource::BuiltInCatalog,
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
            attached_workspaces: vec![],
            workspace_id: None,
            workspace_anchor: "/tmp".into(),
            execution_root: "/tmp".into(),
            cwd: "/tmp".into(),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
            execution_roots: Vec::new(),
        },
        active_workspace_occupancy: None,
        loaded_agents_md: LoadedAgentsMdView::default(),
        skills: SkillsRuntimeView::default(),
        active_children: Vec::<ChildAgentSummary>::new(),
        active_wait_conditions: Vec::new(),
        active_external_triggers: Vec::new(),
        recent_operator_notifications: Vec::new(),
        recent_brief_count: 1,
        recent_event_count: 1,
    }
}

fn sample_model_availability(
    model: &str,
    display_name: &str,
    available: bool,
    reasoning: bool,
) -> crate::types::ResolvedModelAvailability {
    let model_ref = crate::config::ModelRef::parse(model).unwrap();
    crate::types::ResolvedModelAvailability {
        model: model.into(),
        provider: model_ref.provider.as_str().into(),
        provider_family: model_ref.provider.as_str().into(),
        endpoint: "default".into(),
        route_provider: model_ref.provider.as_str().into(),
        display_name: display_name.into(),
        metadata_source: "remote_discovered".into(),
        provider_configured: true,
        provider_source: Some("config".into()),
        transport: Some(if model_ref.provider.as_str() == "openai_codex" {
            "openai_codex_responses".into()
        } else {
            "openai_chat_completions".into()
        }),
        credential_source: Some("env".into()),
        credential_kind: Some("api_key".into()),
        credential_configured: available,
        available,
        unavailable_reason: (!available).then_some("credential_missing".into()),
        policy: crate::model_catalog::ResolvedRuntimeModelPolicy {
            model_ref,
            display_name: display_name.into(),
            description: "Sample policy".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 90,
            prompt_budget_estimated_tokens: 115_200,
            compaction_trigger_estimated_tokens: 115_200,
            compaction_keep_recent_estimated_tokens: 43_776,
            runtime_max_output_tokens: 16_000,
            verbosity: None,
            tool_output_truncation_estimated_tokens: 2_500,
            max_output_tokens_upper_limit: Some(64_000),
            capabilities: crate::model_catalog::ModelCapabilityFlags {
                supports_reasoning: reasoning,
                ..crate::model_catalog::ModelCapabilityFlags::default()
            },
            reasoning_effort_options: reasoning
                .then(|| vec!["low".into(), "medium".into(), "high".into()])
                .unwrap_or_default(),
            source: crate::model_catalog::ModelMetadataSource::RemoteDiscovered,
            evidence: Default::default(),
        },
        resolved_capabilities: None,
    }
}

fn build_workspace_snapshot_from_active(
    active: Option<&crate::types::ActiveWorkspaceEntry>,
    attached: &[String],
) -> Vec<crate::types::AgentWorkspaceInfo> {
    use crate::types::{AgentWorkspaceInfo, WorktreeInfo};
    use std::collections::HashSet;

    let mut seen: HashSet<String> = HashSet::new();
    let mut result: Vec<AgentWorkspaceInfo> = Vec::new();

    if let Some(entry) = active {
        seen.insert(entry.workspace_id.clone());
        result.push(AgentWorkspaceInfo {
            workspace_id: entry.workspace_id.clone(),
            workspace_alias: None,
            workspace_anchor: Some(entry.workspace_anchor.display().to_string()),
            repo_name: None,
            is_active: true,
            execution_root_id: Some(entry.execution_root_id.clone()),
            execution_root: Some(entry.execution_root.display().to_string()),
            cwd: Some(entry.cwd.display().to_string()),
            projection_kind: Some(entry.projection_kind),
            access_mode: Some(entry.access_mode),
            worktree: entry.projection_metadata.as_ref().map(|m| {
                let (branch, path) = match m {
                    crate::types::WorkspaceProjectionMetadata::ManagedWorktree {
                        worktree_branch,
                        worktree_path,
                        ..
                    } => (
                        Some(worktree_branch.clone()),
                        Some(worktree_path.display().to_string()),
                    ),
                    crate::types::WorkspaceProjectionMetadata::ExistingGitWorktree {
                        worktree_root,
                    } => (None, Some(worktree_root.display().to_string())),
                };
                WorktreeInfo {
                    branch,
                    path,
                    original_branch: None,
                    original_cwd: None,
                }
            }),
        });
    }

    for ws_id in attached {
        if !seen.contains(ws_id) {
            seen.insert(ws_id.clone());
            result.push(AgentWorkspaceInfo {
                workspace_id: ws_id.clone(),
                workspace_alias: None,
                workspace_anchor: None,
                repo_name: None,
                is_active: false,
                execution_root_id: None,
                execution_root: None,
                cwd: None,
                projection_kind: None,
                access_mode: None,
                worktree: None,
            });
        }
    }

    result
}

fn sample_snapshot(agent_id: &str, _cursor: &str) -> AgentStateSnapshot {
    AgentStateSnapshot {
        agent: sample_agent_summary(agent_id),
        session: StateSessionSnapshot {
            current_run_id: None,
            pending_count: 0,
            last_turn: None,
        },
        tasks: Vec::new(),
        timers: Vec::new(),
        work_items: Vec::new(),
        external_triggers: Vec::new(),
        operator_notifications: Vec::new(),
        workspace: StateWorkspaceSnapshot::default(),
        execution: None,
    }
}

fn operator_message_event_envelope(
    id: &str,
    event_seq: u64,
    agent_id: &str,
    text: &str,
) -> StreamEventEnvelope {
    let mut message = crate::types::MessageEnvelope::new(
        agent_id,
        crate::types::MessageKind::OperatorPrompt,
        crate::types::MessageOrigin::Operator { actor_id: None },
        crate::types::AuthorityClass::OperatorInstruction,
        crate::types::Priority::Normal,
        MessageBody::Text { text: text.into() },
    );
    message.id = id.into();
    pipeline_event_envelope(
        id,
        event_seq,
        agent_id,
        "message_enqueued",
        serde_json::to_value(message).unwrap(),
    )
}

fn slim_operator_message_event_envelope(
    id: &str,
    event_seq: u64,
    agent_id: &str,
) -> StreamEventEnvelope {
    pipeline_event_envelope(
        &format!("evt-{id}"),
        event_seq,
        agent_id,
        "message_enqueued",
        json!({
            "message_id": id,
            "origin": {
                "kind": "operator",
                "actor_id": null
            }
        }),
    )
}

fn work_item_written_event_envelope(
    id: &str,
    event_seq: u64,
    agent_id: &str,
    objective: &str,
) -> StreamEventEnvelope {
    pipeline_event_envelope(
        id,
        event_seq,
        agent_id,
        "work_item_written",
        json!({
            "action": "created",
            "record": {
                "id": "work-1",
                "agent_id": agent_id,
                "workspace_id": crate::types::AGENT_HOME_WORKSPACE_ID,
                "objective": objective,
                "state": "open",
                "plan_status": "draft",
                "todo_list": [],
                "created_at": Utc::now(),
                "updated_at": Utc::now()
            }
        }),
    )
}

fn tool_executed_event_envelope(
    id: &str,
    event_seq: u64,
    agent_id: &str,
    tool_name: &str,
) -> StreamEventEnvelope {
    pipeline_event_envelope(
        id,
        event_seq,
        agent_id,
        "tool_executed",
        json!({
            "duration_ms": 0,
            "status": "success",
            "summary": tool_name,
            "tool_name": tool_name
        }),
    )
}

fn apply_brief_event(app: &mut TuiApp, brief: BriefRecord) {
    let event_id = format!("evt-{}", brief.id);
    let projection = app.projection.get_or_insert_with(|| {
        TuiProjection::from_snapshot(sample_snapshot(&brief.agent_id, "evt-0"))
    });
    projection.apply_event(
        AgentStreamEvent {
            id: event_id.clone(),
            event: "brief_created".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: event_id,
                event_seq: 0,
                ts: brief.created_at,
                agent_id: brief.agent_id.clone(),
                event_type: "brief_created".into(),
                provenance: None,
                payload: serde_json::to_value(brief).unwrap(),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
}

fn apply_event(app: &mut TuiApp, event_type: &str, payload: serde_json::Value) {
    let event_id = format!("evt-{event_type}");
    let projection = app
        .projection
        .get_or_insert_with(|| TuiProjection::from_snapshot(sample_snapshot("default", "evt-0")));
    projection.apply_event(
        AgentStreamEvent {
            id: event_id.clone(),
            event: event_type.into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: event_id,
                event_seq: 0,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: event_type.into(),
                provenance: None,
                payload,
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
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
fn collect_chat_items_does_not_write_presentation_debug_log() {
    let client = LocalClient::new(test_config()).unwrap();
    let log_writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();
    let mut app = TuiApp::new(client, log_writer);
    let snapshot = sample_snapshot("default", "evt-assistant");
    let events_tail = vec![StreamEventEnvelope {
        event_log_epoch: Some("epoch-test".into()),
        contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
        payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
        payload_schema_version: 1,
        id: "evt-assistant".into(),
        event_seq: 1,
        ts: Utc::now(),
        agent_id: "default".into(),
        event_type: "assistant_round_recorded".into(),
        provenance: None,
        payload: json!({ "round": 1, "text_preview": "history progress" }),
    }];
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(events_tail, Some(1));
    app.projection = Some(projection);

    let first = collect_chat_items(&app);
    let second = collect_chat_items(&app);

    assert_eq!(first, second);
    assert!(!app.log_writer.root().join("presentation.jsonl").exists());
}

#[test]
fn collect_chat_items_includes_local_command_outputs() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.local_command_outputs.push(LocalCommandOutput {
        created_at: Utc::now(),
        title: "Skill added".into(),
        body: "Added `ghx` to the user_global Skill Library.".into(),
        is_error: false,
    });

    let items = collect_chat_items(&app);

    assert!(items.iter().any(|item| matches!(
        item,
        ConversationCell::SystemNotice {
            speaker,
            body,
            header_hint,
            ..
        } if speaker == "command"
            && body.contains("Added `ghx`")
            && header_hint.as_deref() == Some("Skill added")
    )));
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
fn header_view_model_prefers_operator_waiting_label() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("holon-dev", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeIdle;
    snapshot.agent.closure.waiting_reason =
        Some(crate::types::WaitingReason::AwaitingOperatorInput);
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let view_model = HeaderViewModel::from_app(&app);

    assert_eq!(view_model.line, "holon-dev  waiting for you");
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
    snapshot.workspace.workspaces = build_workspace_snapshot_from_active(
        snapshot.agent.agent.active_workspace_entry.as_ref(),
        snapshot.agent.agent.attached_workspaces.as_slice(),
    );
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
fn statusbar_view_model_converges_from_workspace_used_event() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.status_line.clear();
    let snapshot = sample_snapshot("default", "evt-0");
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let projection = app.projection.as_mut().unwrap();
    projection.apply_event(
        pipeline_event(
            "evt-workspace-used",
            1,
            "default",
            "workspace_used",
            json!({
                "workspace_id": crate::types::AGENT_HOME_WORKSPACE_ID,
                "workspace_anchor": "/tmp/agent-home",
                "execution_root_id": "canonical_root:agent_home",
                "execution_root": "/tmp/agent-home",
                "projection_kind": "canonical_root",
                "access_mode": "exclusive_write",
                "cwd": "/tmp/agent-home"
            }),
        ),
        &app.log_writer,
    );
    app.apply_projection_view();

    let view_model = StatusbarViewModel::from_app(&app, false);

    assert!(view_model.context_line.starts_with("agent_home ("));
    assert!(view_model.context_line.contains("/tmp/agent-home)"));
}

#[test]
fn statusbar_view_model_converges_from_provider_round_model_event() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.status_line.clear();
    let snapshot = sample_snapshot("default", "evt-0");
    app.agents = vec![snapshot.agent.clone()];
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let projection = app.projection.as_mut().unwrap();
    projection.apply_event(
        pipeline_event(
            "evt-provider-model",
            1,
            "default",
            "provider_round_completed",
            json!({
                "requested_model": "openai/gpt-5.4",
                "active_model": "anthropic/claude-sonnet-4-6"
            }),
        ),
        &app.log_writer,
    );
    app.apply_projection_view();

    let view_model = StatusbarViewModel::from_app(&app, false);

    assert!(view_model
        .context_line
        .contains("anthropic/claude-sonnet-4-6 (fallback from openai/gpt-5.4)"));
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
    snapshot.workspace.workspaces = build_workspace_snapshot_from_active(
        snapshot.agent.agent.active_workspace_entry.as_ref(),
        snapshot.agent.agent.attached_workspaces.as_slice(),
    );
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
fn statusbar_view_model_prefers_transient_status_over_vim_hint() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer_edit_mode = ComposerEditMode::VimNormal;
    app.status_line = "Loaded older events".into();

    let view_model = StatusbarViewModel::from_app(&app, false);

    assert!(view_model.status_line.contains("Loaded older events"));
    assert!(!view_model.status_line.contains("VIM NORMAL"));
}

#[test]
fn build_chat_text_includes_structured_operator_messages() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let projection = app
        .projection
        .get_or_insert_with(|| TuiProjection::from_snapshot(sample_snapshot("default", "evt-0")));
    let operator_event = operator_message_event_envelope("m1", 0, "default", "Fix the failing CI");
    projection.apply_event(
        AgentStreamEvent {
            id: operator_event.id.clone(),
            event: operator_event.event_type.clone(),
            data: operator_event,
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "I started a worktree task.".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let lines: Vec<String> = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(lines.iter().any(|line| line.contains("> operator ")));
    assert!(lines.iter().any(|line| line.contains("Fix the failing CI")));
    assert!(lines.iter().any(|line| line.contains("• default ")));
    assert!(lines
        .iter()
        .any(|line| line.contains("I started a worktree task.")));
}

#[test]
fn build_chat_text_renders_message_block_header_above_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "First line\nSecond line".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let lines: Vec<String> = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(lines.iter().any(|line| line.contains("• default ")));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("  ") && line.contains("First line")));
    assert!(lines.iter().any(|line| line.starts_with("  Second line")));
}

#[test]
fn build_chat_text_groups_activity_by_kind_not_minute() {
    let first_created_at = Utc::now();
    let second_created_at = first_created_at + chrono::Duration::minutes(2);
    let items = vec![
        ConversationCell::SystemNotice {
            created_at: first_created_at,
            event_seq: 1,
            speaker: "holon-pm".into(),
            body: "first activity".into(),
            display_kind: ConversationDisplayKind::Activity,
            group_id: Some("agent:holon-pm:turn:1".into()),
            header_hint: None,
        },
        ConversationCell::SystemNotice {
            created_at: second_created_at,
            event_seq: 2,
            speaker: "holon-pm".into(),
            body: "second activity".into(),
            display_kind: ConversationDisplayKind::Activity,
            group_id: Some("agent:holon-pm:turn:1".into()),
            header_hint: None,
        },
        ConversationCell::SystemNotice {
            created_at: second_created_at,
            event_seq: 3,
            speaker: "holon-pm".into(),
            body: "narrative line".into(),
            display_kind: ConversationDisplayKind::Narrative,
            group_id: Some("agent:holon-pm:turn:2".into()),
            header_hint: None,
        },
    ];

    let lines: Vec<String> = build_chat_text(&items)
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    let header_count = lines
        .iter()
        .filter(|line| line.contains("• holon-pm "))
        .count();
    assert_eq!(header_count, 2);
    assert!(lines
        .iter()
        .any(|line| line.starts_with("    first activity")));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("    second activity")));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("  narrative line")));
}

#[test]
fn build_chat_text_groups_agent_cells_by_turn_index() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.display_mode = OperatorDisplayMode::Verbose;
    let ts = Utc::now();
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("holon-pm", "evt-0"));
    let resume_event = pipeline_event_envelope(
        "evt-resume",
        1,
        "holon-pm",
        "continuation_trigger_received",
        json!({
            "agent_id": "holon-pm",
            "trigger_kind": "operator_input"
        }),
    );
    projection.apply_event(
        AgentStreamEvent {
            id: resume_event.id.clone(),
            event: resume_event.event_type.clone(),
            data: StreamEventEnvelope { ts, ..resume_event },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    for event in [
        pipeline_event_envelope(
            "evt-progress",
            2,
            "holon-pm",
            "assistant_round_recorded",
            json!({
                "agent_id": "holon-pm",
                "turn_index": 7,
                "round": 1,
                "text_preview": "I will inspect the PR.",
                "has_text": true,
                "has_tool_calls": false
            }),
        ),
        pipeline_event_envelope(
            "evt-tool",
            3,
            "holon-pm",
            "tool_executed",
            json!({
                "agent_id": "holon-pm",
                "turn_index": 7,
                "tool_name": "ExecCommand",
                "exec_command_cmd": "gh pr view 1497",
                "duration_ms": 100,
                "status": "success",
                "summary": "gh pr view 1497"
            }),
        ),
    ] {
        projection.apply_event(
            AgentStreamEvent {
                id: event.id.clone(),
                event: event.event_type.clone(),
                data: StreamEventEnvelope { ts, ..event },
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
    }
    let brief = BriefRecord {
        id: "brief-1".into(),
        agent_id: "holon-pm".into(),
        workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
        work_item_id: None,
        turn_index: Some(7),
        turn_id: None,
        kind: BriefKind::Result,
        created_at: ts,
        content_source: BriefContentSource::Inline,
        finalizes_assistant_round_id: None,
        text: "PR is merged.".into(),
        attachments: None,
        related_message_id: None,
        related_task_id: None,
    };
    let brief_event = StreamEventEnvelope {
        event_log_epoch: Some("epoch-test".into()),
        contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
        payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
        payload_schema_version: 1,
        id: "evt-brief".into(),
        event_seq: 4,
        ts,
        agent_id: "holon-pm".into(),
        event_type: "brief_created".into(),
        provenance: None,
        payload: serde_json::to_value(brief).unwrap(),
    };
    projection.apply_event(
        AgentStreamEvent {
            id: brief_event.id.clone(),
            event: brief_event.event_type.clone(),
            data: brief_event,
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.projection = Some(projection);

    let lines: Vec<String> = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    let header_count = lines
        .iter()
        .filter(|line| line.contains("• holon-pm "))
        .count();
    assert_eq!(header_count, 1);
    assert!(lines
        .iter()
        .any(|line| line.contains("• holon-pm ") && line.contains("operator input")));
    assert!(!lines
        .iter()
        .any(|line| line.contains("Continuation triggered")));
    assert!(lines.iter().any(|line| line.contains("I will inspect")));
    assert!(lines.iter().any(|line| line.contains("gh pr view 1497")));
    assert!(lines.iter().any(|line| line.contains("PR is merged.")));
}

#[test]
fn build_chat_text_wraps_body_lines_with_indent() {
    let ts = Utc::now();
    let items = vec![ConversationCell::SystemNotice {
        created_at: ts,
        event_seq: 1,
        speaker: "holon-pm".into(),
        body: "abcdefghij klmnopqrst uvwxyz".into(),
        display_kind: ConversationDisplayKind::Activity,
        group_id: Some("agent:holon-pm:turn:1".into()),
        header_hint: None,
    }];

    let lines: Vec<String> = build_chat_text_for_width(&items, 18)
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(lines.iter().any(|line| line.starts_with("    abc")));
    assert!(
        lines
            .iter()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .all(|line| line.starts_with("    ")),
        "wrapped body lines should keep activity indentation: {lines:?}"
    );
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
        provider: None,
        filter: "gpt".into(),
        selected: 0,
    };

    app.handle_paste("-5.3\n").await.unwrap();

    assert_eq!(
        app.overlay,
        OverlayState::ModelPicker {
            provider: None,
            filter: "gpt-5.3".into(),
            selected: 0
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
            composer: ComposerState::from("explain first second")
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
async fn empty_composer_up_and_down_navigate_input_history() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.input_history = vec!["first".into(), "second".into()];

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "second");
    assert_eq!(app.history_index, Some(1));

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.composer.is_empty());
    assert_eq!(app.history_index, None);
}

#[tokio::test]
async fn non_empty_single_line_composer_up_down_do_not_scroll_chat() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.chat_max_scroll = 12;
    app.composer = ComposerState::from("draft");

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "draft");
    assert_eq!(app.composer.cursor(), "draft".len());
    assert!(app.chat_scroll.is_following_tail());
}

#[tokio::test]
async fn multiline_composer_up_down_move_cursor_between_lines() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("alpha\nbeta\ncharlie");
    app.chat_max_scroll = 12;
    app.composer.move_to_start();
    for _ in 0..8 {
        app.composer.move_right();
    }

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.cursor(), 2);

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.cursor(), "alpha\nbe".len());
    assert!(app.chat_scroll.is_following_tail());
}

#[tokio::test]
async fn page_keys_still_scroll_chat_when_composer_has_content() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.chat_max_scroll = 12;
    app.composer = ComposerState::from("draft");

    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.chat_scroll.effective_scroll(12), 2);
    assert!(!app.chat_scroll.is_following_tail());
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
    assert_eq!(app.status_line, "Loading agent state for beta");
    assert!(app.snapshot_refresh_in_flight);
}

#[tokio::test]
async fn agent_overlay_enter_clamps_stale_selection() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 0;
    app.overlay = OverlayState::Agents { selected: 9 };
    app.connection_state = TuiConnectionState::Streaming;

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.status_line, "Loading agent state for beta");
    assert_eq!(app.selected_agent_id(), Some("alpha"));
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
            provider: None,
            filter: String::new(),
            selected: 0
        }
    );
    assert_eq!(app.composer.as_str(), "");
    assert!(app.model_availability_load_in_flight);
}

#[tokio::test]
async fn initialize_does_not_eagerly_load_model_availability() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.initialize().await;

    assert!(!app.model_availability_load_in_flight);
}

#[test]
fn loaded_models_clear_lazy_load_in_flight_flag() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.model_availability_load_in_flight = true;

    app.apply_loaded_models(Ok(Vec::new()));

    assert!(!app.model_availability_load_in_flight);
}

#[tokio::test]
async fn model_picker_enter_opens_provider_model_page() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.apply_agent_list(vec![sample_agent_summary("default")]);
    app.model_availability = vec![sample_model_availability(
        "openrouter/deepseek-v3",
        "DeepSeek V3",
        true,
        false,
    )];
    app.overlay = OverlayState::ModelPicker {
        provider: None,
        filter: String::new(),
        selected: 1,
    };

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.overlay,
        OverlayState::ModelPicker {
            provider: Some("openrouter".into()),
            filter: String::new(),
            selected: 0
        }
    );
}

#[tokio::test]
async fn model_picker_opens_effort_for_models_with_reasoning_support() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.apply_agent_list(vec![sample_agent_summary("default")]);
    app.model_availability = vec![sample_model_availability(
        "openai_codex/gpt-5.4",
        "GPT-5.4 Codex",
        true,
        true,
    )];
    app.overlay = OverlayState::ModelPicker {
        provider: Some("openai_codex".into()),
        filter: String::new(),
        selected: 1,
    };

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.overlay,
        OverlayState::ModelEffortPicker {
            model: "openai_codex/gpt-5.4".into(),
            options: vec!["low".into(), "medium".into(), "high".into()],
            selected: 0,
            return_filter: String::new(),
            return_selected: 1
        }
    );
}

#[test]
fn model_picker_omits_unavailable_provider_choices() {
    let agent = sample_agent_summary("default");
    let model_availability = vec![sample_model_availability(
        "openrouter/deepseek-v3",
        "DeepSeek V3",
        false,
        false,
    )];
    let rows = super::model_picker::model_picker_rows(Some(&agent), &model_availability, None, "");

    assert_eq!(rows.len(), 1);
    assert!(rows[0].title.contains("inherit runtime default"));
    assert!(!rows.iter().any(|row| row.title == "openrouter"));
}

#[tokio::test]
async fn model_effort_picker_esc_returns_to_provider_model_page() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.overlay = OverlayState::ModelEffortPicker {
        model: "openai_codex/gpt-5.4".into(),
        options: vec!["low".into(), "medium".into(), "high".into()],
        selected: 0,
        return_filter: "gpt".into(),
        return_selected: 1,
    };

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.overlay,
        OverlayState::ModelPicker {
            provider: Some("openai_codex".into()),
            filter: "gpt".into(),
            selected: 1
        }
    );
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
    let state_path = tui_state_path(&client);
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.apply_agent_list(vec![sample_agent_summary("default")]);
    app.composer = ComposerState::from("/display 5");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.display_mode, OperatorDisplayMode::Debug);
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.composer.as_str(), "");
    assert_eq!(app.status_line, "Loading agent state for default");
    let loaded = TuiClientState::load(&state_path).unwrap();
    assert_eq!(
        loaded.effective_display_mode("default"),
        OperatorDisplayMode::Debug
    );
}

#[tokio::test]
async fn slash_display_accepts_named_modes() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.apply_agent_list(vec![sample_agent_summary("default")]);
    app.composer = ComposerState::from("/display VERBOSE");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.display_mode, OperatorDisplayMode::Verbose);
    assert_eq!(app.status_line, "Loading agent state for default");
}

#[tokio::test]
async fn slash_display_reset_clears_selected_agent_override() {
    let client = LocalClient::new(test_config()).unwrap();
    let state_path = tui_state_path(&client);
    let mut state = TuiClientState::new("default");
    state.set_agent_display_mode("default", OperatorDisplayMode::Debug);
    state.save(&state_path).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.apply_agent_list(vec![sample_agent_summary("default")]);
    app.composer = ComposerState::from("/display reset");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.display_mode, OperatorDisplayMode::Info);
    let loaded = TuiClientState::load(&state_path).unwrap();
    assert_eq!(
        loaded.effective_display_mode("default"),
        OperatorDisplayMode::Info
    );
    assert!(loaded.display.per_agent.is_empty());
}

#[tokio::test]
async fn slash_vim_toggles_session_local_composer_mode() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.composer = ComposerState::from("/vim");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "");
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimNormal);
    assert!(app.status_line.is_empty());

    app.composer = ComposerState::from("/vim");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer_edit_mode, ComposerEditMode::Default);
    assert!(app.status_line.contains("disabled"));
}

#[tokio::test]
async fn slash_vim_enters_normal_mode_without_repositioning_submitted_command_text() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/vim");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "");
    assert_eq!(app.composer.cursor(), 0);
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimNormal);
}

#[tokio::test]
async fn vim_mode_preserves_page_scroll_keys() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.chat_max_scroll = 12;
    app.composer = ComposerState::from("draft");
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.chat_scroll.effective_scroll(12), 2);
    assert!(!app.chat_scroll.is_following_tail());

    app.composer_edit_mode = ComposerEditMode::VimInsert;
    app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.chat_scroll.effective_scroll(12), 12);
    assert_eq!(app.composer.as_str(), "draft");
}

#[tokio::test]
async fn vim_page_scroll_clears_pending_normal_command() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.chat_max_scroll = 12;
    app.composer = ComposerState::from("first\nsecond");
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "first\nsecond");
    assert_eq!(app.vim_pending_command, Some('d'));
    assert_eq!(app.chat_scroll.effective_scroll(12), 2);
}

#[tokio::test]
async fn vim_mode_preserves_empty_composer_help_shortcut() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer_edit_mode = ComposerEditMode::VimNormal;
    app.vim_pending_command = Some('d');

    app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(matches!(app.overlay, OverlayState::HelpView { scroll: 0 }));
    assert_eq!(app.vim_pending_command, None);
    assert!(app.composer.is_empty());
}

#[tokio::test]
async fn vim_mode_preserves_empty_composer_history_shortcuts() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.input_history = vec!["first".into(), "second".into()];
    app.composer_edit_mode = ComposerEditMode::VimNormal;
    app.vim_pending_command = Some('d');

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "second");
    assert_eq!(app.history_index, Some(1));
    assert_eq!(app.vim_pending_command, None);

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.composer.is_empty());
    assert_eq!(app.history_index, None);
}

#[tokio::test]
async fn ctrl_o_prefix_opens_overlay_without_touching_composer() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("draft");
    app.selected_agent = 2;

    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.overlay_shortcut_pending);
    assert_eq!(app.composer.as_str(), "draft");

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.overlay, OverlayState::Agents { selected: 2 });
    assert!(!app.overlay_shortcut_pending);
    assert_eq!(app.composer.as_str(), "draft");
}

#[tokio::test]
async fn ctrl_o_prefix_can_cancel_or_reject_unknown_shortcut() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.overlay_shortcut_pending);
    assert_eq!(app.overlay, OverlayState::None);
    assert_eq!(app.status_line, "Overlay shortcut cancelled");

    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.overlay_shortcut_pending);
    assert_eq!(app.overlay, OverlayState::None);
    assert!(app.status_line.contains("Unknown overlay shortcut"));
}

#[tokio::test]
async fn ctrl_o_k_shows_selected_agent_skills_not_global_catalog() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.apply_agent_list(vec![sample_agent_summary("default")]);

    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.overlay, OverlayState::None);
    assert_ne!(
        app.status_line, "Opened Skill Catalog: 0 skills",
        "Ctrl+O K must not open the global skill catalog"
    );
    assert!(
        app.status_line.contains("Discoverable skills")
            || app.status_line.contains("Failed to list skills")
    );
}

#[test]
fn default_keymap_documents_overlay_shortcuts_and_mac_paging() {
    let default_keymap = DEFAULT_BINDING_HINTS
        .iter()
        .map(|hint| format!("{} {}", hint.action, hint.keys))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(default_keymap.contains("Ctrl+O"));
    assert!(default_keymap.contains("Mac: Fn+Up/Fn+Down"));
}

#[tokio::test]
async fn vim_insert_esc_takes_precedence_over_slash_menu_dismissal() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("/v");
    app.composer_edit_mode = ComposerEditMode::VimInsert;

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "/v");
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimNormal);
    assert_eq!(app.slash_menu_dismissed_for, None);
}

#[tokio::test]
async fn vim_insert_and_normal_modes_switch_without_changing_default_behavior() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "hi");
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimInsert);

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimNormal);

    let client = LocalClient::new(test_config()).unwrap();
    let mut default_app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    default_app
        .handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(default_app.composer.as_str(), "h");
}

#[tokio::test]
async fn vim_undo_restores_insert_entry_snapshot() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("hi");
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "hi!");

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.composer.as_str(), "hi");
}

#[tokio::test]
async fn vim_normal_mode_moves_over_multiline_utf8_text() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("你 好\nworld");
    app.composer.move_to_start();
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.cursor(), "你 ".len());

    app.handle_key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.cursor(), "你 好".len());

    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.cursor(), "你 好\nwor".len());

    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.cursor(), "你 好\n".len());
}

#[tokio::test]
async fn vim_normal_mode_edits_lines_words_and_undoes_last_change() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("alpha beta\ngamma");
    app.composer.move_to_start();
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "alpha \ngamma");
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimInsert);

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "alpha beta\ngamma");

    app.handle_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "\ngamma");

    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "gamma");

    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "amma");
}

#[tokio::test]
async fn vim_open_line_commands_enter_insert_mode() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("alpha");
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "alpha\nb");
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimInsert);

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "alpha\nx\nb");
}

#[tokio::test]
async fn vim_normal_enter_submits_non_empty_composer() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer = ComposerState::from("hi");
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    let err = app
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect_err("submit should fail without a selected agent");

    assert!(err.to_string().contains("no agent selected"));
    assert_eq!(app.composer.as_str(), "hi");
}

#[tokio::test]
async fn vim_normal_slash_enters_insert_mode_and_keeps_slash_menu_usable() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.composer_edit_mode = ComposerEditMode::VimNormal;

    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "/");
    assert_eq!(app.composer_edit_mode, ComposerEditMode::VimInsert);

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.slash_menu_selected, 1);

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer.as_str(), "/agents");
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
            provider: None,
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
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "**Done**\n- first\n- second".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let lines: Vec<String> = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect();
    assert!(lines.iter().any(|line| line.contains("Done")));
    assert!(lines.iter().any(|line| line.contains("first")));
    assert!(lines.iter().any(|line| line.contains("second")));
}

#[test]
fn chat_text_renders_brief_events_from_projection() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "### Title\n\nBody".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Title"));
    assert!(rendered.contains("Body"));
}

#[test]
fn chat_text_ignores_ack_lifecycle_event_but_keeps_result_brief_events() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    apply_event(
        &mut app,
        "message_acknowledged",
        json!({
            "agent_id": "default",
            "message_id": "msg-duplicate",
            "summary": "Queued work: duplicate"
        }),
    );
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-result".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "Real response".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

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
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-task".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "Task task-1 completed: line one\nline two\nline three".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: Some("task-1".into()),
        },
    );

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Task task-1 completed: line one"));
    assert!(rendered.contains("line two"));
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
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-work".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "work_item_written".into(),
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
    projection.apply_stream_event(
        AgentStreamEvent {
            id: "evt-assistant".into(),
            event: "assistant_round_recorded".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-assistant".into(),
                event_seq: 3,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "assistant_round_recorded".into(),
                provenance: None,
                payload: json!({ "round": 1, "text_preview": "hidden assistant partial" }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        app.display_mode,
    );
    app.projection = Some(projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Assistant hidden assistant partial"));
    assert!(!rendered.contains("Action    hidden assistant partial"));
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
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
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
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-state".into(),
                event_seq: 3,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "agent_state_changed".into(),
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
fn chat_display_mode_info_shows_hidden_stream_activity_in_working_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.display_mode = OperatorDisplayMode::Info;
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_stream_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        app.display_mode,
    );
    app.projection = Some(projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Working"));
    assert!(rendered.contains("cargo test tui"));
}

#[test]
fn chat_display_mode_info_suppresses_successful_work_item_tool_activity() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.display_mode = OperatorDisplayMode::Info;
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_stream_event(
        AgentStreamEvent {
            id: "evt-work-item-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-work-item-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                provenance: None,
                payload: json!({
                    "tool_name": "UpdateWorkItem",
                    "status": "success",
                    "summary": "updated work item"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        app.display_mode,
    );
    app.projection = Some(projection);

    let items = collect_chat_items(&app);
    let active_item = items.last().expect("active activity item");
    match active_item {
        ConversationCell::ActiveActivity { body, .. } => {
            assert!(body.is_empty());
        }
        other => panic!("expected active activity item, got {other:?}"),
    }
}

#[test]
fn chat_display_mode_info_uses_rendered_list_work_items_activity() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.display_mode = OperatorDisplayMode::Info;
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_stream_event(
        AgentStreamEvent {
            id: "evt-list-work-items".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-list-work-items".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                provenance: None,
                payload: json!({
                    "tool_name": "ListWorkItems",
                    "status": "success",
                    "summary": "Tool finished: ListWorkItems",
                    "tool_result": {
                        "filter": "open",
                        "returned": 1,
                        "total_matching": 1,
                        "limit": 20,
                        "work_items": [{
                            "id": "work_123456789abcdef",
                            "objective": "fix work item tui rendering",
                            "state": "open",
                            "readiness": "runnable"
                        }]
                    }
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        app.display_mode,
    );
    app.projection = Some(projection);

    let items = collect_chat_items(&app);
    let active_item = items.last().expect("active activity item");
    match active_item {
        ConversationCell::ActiveActivity { body, .. } => {
            assert!(body.contains("Work items: filter=open returned=1 total=1 limit=20"));
            assert!(body.contains("fix work item tui rendering"));
            assert!(!body.contains("Tool finished: ListWorkItems"));
        }
        other => panic!("expected active activity item, got {other:?}"),
    }
}

#[test]
fn chat_display_mode_verbose_keeps_working_marker_without_activity_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.display_mode = OperatorDisplayMode::Verbose;
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.apply_stream_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        app.display_mode,
    );
    app.projection = Some(projection);

    let items = collect_chat_items(&app);
    assert!(items.iter().any(|item| matches!(
        item,
        ConversationCell::SystemNotice { body, .. }
            if body.contains("cargo test tui")
    )));
    let active_item = items.last().expect("active activity item");
    match active_item {
        ConversationCell::ActiveActivity { body, .. } => assert!(body.is_empty()),
        other => panic!("expected active activity item, got {other:?}"),
    }
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
            event_log_epoch: Some("epoch-test".into()),
            contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
            payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
            payload_schema_version: 1,
                id: "evt-task".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "task_result_received".into(),
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
                })
            }
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
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
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
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-brief".into(),
                event_seq: 3,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "brief_created".into(),
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
    let mut refreshed_projection = TuiProjection::from_snapshot(snapshot);
    refreshed_projection.apply_stream_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        app.display_mode,
    );
    app.projection = Some(refreshed_projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("cargo test tui"));
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
    previous_projection.apply_stream_event(
        AgentStreamEvent {
            id: "evt-a-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-a-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "agent-a".into(),
                event_type: "tool_executed".into(),
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test agent-a"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        app.display_mode,
    );
    app.projection = Some(previous_projection);
    let before_switch: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(before_switch.contains("cargo test agent-a"));

    let mut switched_snapshot = sample_snapshot("agent-b", "evt-b-tool");
    switched_snapshot.agent.agent.status = AgentStatus::AwakeRunning;
    let events_tail = vec![StreamEventEnvelope {
        event_log_epoch: Some("epoch-test".into()),
        contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
        payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
        payload_schema_version: 1,
        id: "evt-b-tool".into(),
        event_seq: 0,
        ts: Utc::now(),
        agent_id: "agent-b".into(),
        event_type: "tool_executed".into(),
        provenance: None,
        payload: json!({
            "tool_name": "ExecCommand",
            "exec_command_cmd": "cargo test agent-b"
        }),
    }];

    // Switching agents must use the selected agent's event page rather
    // than inheriting the previous agent's event log.
    let mut switched_projection = TuiProjection::from_snapshot(switched_snapshot);
    switched_projection.replace_event_window(events_tail, Some(0));
    app.projection = Some(switched_projection);

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("Working"));
    assert!(!rendered.contains("cargo test agent-b"));
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
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-tool".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
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
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-latest".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: ts + chrono::Duration::seconds(10),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "Latest durable response".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );
    let projection = app.projection.as_mut().expect("projection");
    projection.agent.agent.status = AgentStatus::AwakeRunning;
    projection.apply_event(
        AgentStreamEvent {
            id: "evt-tool".into(),
            event: "tool_executed".into(),
            data: StreamEventEnvelope {
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-tool".into(),
                event_seq: 2,
                ts,
                agent_id: "default".into(),
                event_type: "tool_executed".into(),
                provenance: None,
                payload: json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test tui"
                }),
            },
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

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
fn chat_keeps_distinct_operator_messages_with_same_timestamp_and_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    for id in ["message-a", "message-b"] {
        let mut envelope = operator_message_event_envelope(id, 0, "default", "repeat");
        envelope.ts = ts;
        projection.apply_event(
            AgentStreamEvent {
                id: envelope.id.clone(),
                event: envelope.event_type.clone(),
                data: envelope,
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
    }
    app.projection = Some(projection);

    let matching_messages = collect_chat_items(&app)
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationCell::UserMessage { body, .. } if body == "repeat"
            )
        })
        .count();
    assert_eq!(matching_messages, 2);
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
fn sleeping_agent_activity_cell_does_not_display_working() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut snapshot = sample_snapshot("default", "evt-0");
    snapshot.agent.agent.status = AgentStatus::Asleep;
    snapshot.agent.agent.pending = 0;
    snapshot.agent.active_task_count = 1;
    app.projection = Some(TuiProjection::from_snapshot(snapshot));

    let items = collect_chat_items(&app);
    let active_item = items.last().expect("active activity item");
    match active_item {
        ConversationCell::ActiveActivity { speaker, .. } => {
            assert!(speaker.starts_with("Holon (sleeping)"));
            assert!(!speaker.starts_with("Holon (working)"));
        }
        other => panic!("expected active activity item, got {other:?}"),
    }

    let rendered: String = build_chat_text(&items)
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();

    assert!(rendered.contains("Sleeping"));
    assert!(!rendered.contains("Working"));
}

#[test]
fn collect_chat_items_orders_equal_timestamps_deterministically() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    let projection = app
        .projection
        .get_or_insert_with(|| TuiProjection::from_snapshot(sample_snapshot("default", "evt-0")));
    let mut operator_event = operator_message_event_envelope("m1", 0, "default", "same instant");
    operator_event.ts = ts;
    projection.apply_event(
        AgentStreamEvent {
            id: operator_event.id.clone(),
            event: operator_event.event_type.clone(),
            data: operator_event,
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: ts,
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "same instant".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let items = collect_chat_items(&app);
    assert!(matches!(items[0], ConversationCell::UserMessage { .. }));
    assert!(matches!(items[1], ConversationCell::SystemNotice { .. }));
}

#[test]
fn chat_includes_pending_operator_message_from_snapshot() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![operator_message_event_envelope(
            "evt-message-queued",
            0,
            "default",
            "please stop soon",
        )],
        Some(0),
    );
    app.projection = Some(projection);

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
            status: None,
            ..
        } if body == "please stop soon"
    ));
}

#[test]
fn conversation_events_overlay_uses_event_projection() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![operator_message_event_envelope(
            "evt-message-1",
            0,
            "default",
            "event sourced transcript",
        )],
        Some(0),
    );
    app.projection = Some(projection);

    let lines = conversation_events_overlay_lines(&app);

    assert!(lines
        .iter()
        .any(|line| line.contains("event sourced transcript")));
}

#[test]
fn chat_dedupes_pending_operator_message_when_event_log_contains_it() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![operator_message_event_envelope(
            "evt-message-1",
            0,
            "default",
            "persisted operator text",
        )],
        Some(0),
    );
    app.projection = Some(projection);
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
            status: None,
            ..
        } if body == "persisted operator text"
    ));
}

#[test]
fn chat_deduplicates_replayed_projected_work_item_events() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let event = work_item_written_event_envelope(
        "evt-work-item",
        42,
        "default",
        "Resolve and close the M1.15 GitHub issue list",
    );
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    for _ in 0..2 {
        projection.apply_event(
            AgentStreamEvent {
                id: event.id.clone(),
                event: event.event_type.clone(),
                data: event.clone(),
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
    }
    assert_eq!(projection.event_log().len(), 1);
    app.projection = Some(projection);

    let matching = collect_chat_items(&app)
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationCell::SystemNotice { body, .. }
                    if body.contains("Resolve and close the M1.15 GitHub issue list")
            )
        })
        .count();
    assert_eq!(matching, 1);
}

#[test]
fn chat_deduplicates_replayed_projected_tool_events() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let event = tool_executed_event_envelope("evt-tool", 43, "default", "AgentGet");
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    for _ in 0..2 {
        projection.apply_event(
            AgentStreamEvent {
                id: event.id.clone(),
                event: event.event_type.clone(),
                data: event.clone(),
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
    }
    assert_eq!(projection.event_log().len(), 1);
    app.display_mode = OperatorDisplayMode::Verbose;
    app.projection = Some(projection);

    let matching = collect_chat_items(&app)
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationCell::SystemNotice { body, .. }
                    if body.contains("AgentGet")
            )
        })
        .count();
    assert_eq!(matching, 1);
}

#[test]
fn chat_keeps_distinct_projected_tool_events_with_same_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    for (id, event_seq) in [("evt-tool-1", 43), ("evt-tool-2", 44)] {
        let event = tool_executed_event_envelope(id, event_seq, "default", "AgentGet");
        projection.apply_event(
            AgentStreamEvent {
                id: event.id.clone(),
                event: event.event_type.clone(),
                data: event,
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
    }
    app.display_mode = OperatorDisplayMode::Verbose;
    app.projection = Some(projection);

    let matching = collect_chat_items(&app)
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationCell::SystemNotice { body, .. }
                    if body.contains("AgentGet")
            )
        })
        .count();
    assert_eq!(matching, 2);
}

#[test]
fn chat_deduplicates_bootstrap_event_when_stream_replays_it() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let event = work_item_written_event_envelope("evt-work-item", 42, "default", "bootstrap work");
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    projection.replace_event_window(vec![event.clone()], Some(42));
    projection.apply_event(
        AgentStreamEvent {
            id: event.id.clone(),
            event: event.event_type.clone(),
            data: event,
        },
        &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    assert_eq!(projection.event_log().len(), 1);
    app.projection = Some(projection);

    let matching = collect_chat_items(&app)
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationCell::SystemNotice { body, .. }
                    if body.contains("bootstrap work")
            )
        })
        .count();
    assert_eq!(matching, 1);
}

#[test]
fn projection_deduplicates_stream_events_using_outer_id_fallback() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    projection.replace_event_window(Vec::new(), None);
    let mut envelope = work_item_written_event_envelope("", 0, "default", "outer id work");
    envelope.id.clear();

    for _ in 0..2 {
        projection.apply_event(
            AgentStreamEvent {
                id: "sse-event-1".into(),
                event: envelope.event_type.clone(),
                data: envelope.clone(),
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
    }

    assert_eq!(projection.event_log().len(), 1);
    assert_eq!(projection.event_log()[0].id, "sse-event-1");
}

#[test]
fn projection_deduplicates_duplicates_within_history_page() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
    projection.replace_event_window(Vec::new(), None);
    let event = work_item_written_event_envelope("evt-history-work", 42, "default", "history work");

    let added = projection.prepend_event_history_page(vec![event.clone(), event], Some(42), true);

    assert_eq!(added, 1);
    assert_eq!(projection.event_log().len(), 1);
    assert_eq!(projection.event_log()[0].id, "evt-history-work");
}

#[test]
fn chat_text_omits_processing_and_processed_operator_status_labels() {
    for _status in [
        OperatorMessageStatus::Processing,
        OperatorMessageStatus::Processed,
    ] {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        let snapshot = sample_snapshot("default", "evt-0");
        let mut projection = TuiProjection::from_snapshot(snapshot);
        projection.replace_event_window(
            vec![operator_message_event_envelope(
                "evt-message-1",
                0,
                "default",
                "operator text",
            )],
            Some(0),
        );
        app.projection = Some(projection);
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

    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![operator_message_event_envelope(
            "message-1",
            0,
            "default",
            "durable text",
        )],
        Some(0),
    );
    app.projection = Some(projection);
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
            status: None,
            ..
        } if body == "durable text"
    ));
}

#[test]
fn slim_projection_operator_message_hydrates_from_message_evidence() {
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

    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![slim_operator_message_event_envelope(
            "message-1",
            0,
            "default",
        )],
        Some(0),
    );
    assert_eq!(
        projection.missing_message_ids_for_hydration(),
        vec!["message-1".to_string()]
    );
    app.projection = Some(projection);
    app.apply_projection_view();
    assert!(app.optimistic_operator_messages.is_empty());

    let mut message = crate::types::MessageEnvelope::new(
        "default",
        crate::types::MessageKind::OperatorPrompt,
        crate::types::MessageOrigin::Operator { actor_id: None },
        crate::types::AuthorityClass::OperatorInstruction,
        crate::types::Priority::Normal,
        MessageBody::Text {
            text: "durable hydrated text".into(),
        },
    );
    message.id = "message-1".into();
    app.projection
        .as_mut()
        .unwrap()
        .hydrate_messages(vec![message]);

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
            status: None,
            ..
        } if body == "durable hydrated text"
    ));
}

#[tokio::test]
async fn stale_message_hydration_completion_does_not_block_selected_agent() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![slim_operator_message_event_envelope(
            "message-1",
            0,
            "default",
        )],
        Some(0),
    );
    app.projection = Some(projection);
    app.message_hydration_in_flight = Some("stale-agent".into());

    app.apply_messages_hydrated("stale-agent".into(), Ok(Vec::new()));

    assert_eq!(app.message_hydration_in_flight, Some("default".to_string()));
}

#[test]
fn projection_operator_message_deduplicates_unreconciled_optimistic_entry_by_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    app.optimistic_operator_messages = vec![OperatorMessageRecord {
        message_id: "local-message-1".into(),
        agent_id: "default".into(),
        status: OperatorMessageStatus::Queued,
        created_at: ts,
        updated_at: ts,
        body: MessageBody::Text {
            text: "same operator text".into(),
        },
        error: None,
    }];

    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![operator_message_event_envelope(
            "message-1",
            0,
            "default",
            "same operator text",
        )],
        Some(0),
    );
    app.projection = Some(projection);

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
            status: None,
            ..
        } if body == "same operator text"
    ));
}

#[test]
fn projection_operator_message_filters_optimistic_entries_to_selected_agent() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let ts = Utc::now();
    app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
    app.selected_agent = 1;
    app.optimistic_operator_messages = vec![OperatorMessageRecord {
        message_id: "local-alpha-message-1".into(),
        agent_id: "alpha".into(),
        status: OperatorMessageStatus::Queued,
        created_at: ts,
        updated_at: ts,
        body: MessageBody::Text {
            text: "same operator text".into(),
        },
        error: None,
    }];

    let snapshot = sample_snapshot("beta", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![operator_message_event_envelope(
            "beta-message-1",
            0,
            "beta",
            "same operator text",
        )],
        Some(0),
    );
    app.projection = Some(projection);

    let user_messages = collect_chat_items(&app)
        .into_iter()
        .filter(|item| matches!(item, ConversationCell::UserMessage { .. }))
        .collect::<Vec<_>>();
    assert_eq!(user_messages.len(), 1);
    assert!(matches!(
        &user_messages[0],
        ConversationCell::UserMessage {
            body,
            status: None,
            ..
        } if body == "same operator text"
    ));
}

#[test]
fn projection_operator_message_keeps_distinct_durable_messages_with_same_body() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let snapshot = sample_snapshot("default", "evt-0");
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(
        vec![
            operator_message_event_envelope("message-1", 0, "default", "repeat"),
            operator_message_event_envelope("message-2", 1, "default", "repeat"),
        ],
        Some(0),
    );
    app.projection = Some(projection);

    let user_message_count = collect_chat_items(&app)
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationCell::UserMessage {
                    body,
                    status: None,
                    ..
                } if body == "repeat"
            )
        })
        .count();
    assert_eq!(user_message_count, 2);
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
                event_log_epoch: Some("epoch-test".into()),
                contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                payload_schema_version: 1,
                id: "evt-old".into(),
                event_seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "provider_round_completed".into(),
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
                    event_log_epoch: Some("epoch-test".into()),
                    contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                    payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
                    payload_schema_version: 1,
                    id: "evt-new".into(),
                    event_seq: 3,
                    ts: Utc::now(),
                    agent_id: "default".into(),
                    event_type: "provider_round_completed".into(),
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
fn chat_text_renders_full_long_brief_events() {
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
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: long_text,
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let rendered: String = build_chat_text(&collect_chat_items(&app))
        .lines
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content))
        .collect();
    assert!(rendered.contains("intro"));
    assert!(rendered.contains("tail marker that used to be trimmed away"));
}

#[test]
fn chat_text_cache_reuses_unchanged_content_and_replaces_stale_entries() {
    fn normalize_active_activity_spinners(mut text: Text<'static>) -> Text<'static> {
        for line in &mut text.lines {
            let is_active_activity_header = line.spans.len() >= 3
                && line.spans.get(1).is_some_and(|span| span.content == " ")
                && line.spans.get(2).is_some_and(|span| {
                    matches!(
                        span.content.as_ref(),
                        "Working"
                            | "Queued"
                            | "Continuing"
                            | "Starting"
                            | "Waiting task"
                            | "Waiting external"
                            | "Waiting"
                            | "Needs input"
                            | "Blocked"
                            | "Delegating"
                    )
                });
            if is_active_activity_header {
                line.spans[0].content = "*".into();
            }
        }
        text
    }

    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    let first_created_at = Utc::now();
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: first_created_at,
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "**Done**".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let first = chat_text(&app);
    let second = chat_text(&app);
    assert_eq!(
        normalize_active_activity_spinners(first).lines,
        normalize_active_activity_spinners(second).lines
    );

    {
        let cache_ref = app.chat_text_cache.borrow();
        let cached = cache_ref.as_ref().expect("chat text should be cached");
        assert_eq!(cached.cells, collect_chat_items(&app));
    }

    app.projection = Some(TuiProjection::from_snapshot(sample_snapshot(
        "default",
        "evt-reset",
    )));
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-2".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Failure,
            created_at: first_created_at,
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "**Failed**".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

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
    app.projection = None;
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
fn heartbeat_interval_is_less_than_client_stream_idle_timeout() {
    assert!(
        crate::http::EVENT_STREAM_HEARTBEAT_INTERVAL < TUI_LOCAL_NETWORK_POLICY.stream_idle_timeout
    );
}

#[test]
fn remote_read_idle_timeout_allows_slow_tailnet_responses() {
    assert_eq!(
        TUI_REMOTE_NETWORK_POLICY.read_idle_timeout,
        std::time::Duration::from_secs(30)
    );
    assert_eq!(
        TUI_LOCAL_NETWORK_POLICY.read_idle_timeout,
        std::time::Duration::from_secs(10)
    );
}

#[test]
fn reconnect_backoff_increases_and_caps() {
    assert_eq!(
        reconnect_delay_for_attempt(1),
        std::time::Duration::from_secs(1)
    );
    assert_eq!(
        reconnect_delay_for_attempt(2),
        std::time::Duration::from_secs(2)
    );
    assert_eq!(
        reconnect_delay_for_attempt(3),
        std::time::Duration::from_secs(4)
    );
    assert_eq!(
        reconnect_delay_for_attempt(4),
        std::time::Duration::from_secs(8)
    );
    assert_eq!(
        reconnect_delay_for_attempt(5),
        std::time::Duration::from_secs(15)
    );
    assert_eq!(
        reconnect_delay_for_attempt(6),
        std::time::Duration::from_secs(30)
    );
    assert_eq!(
        reconnect_delay_for_attempt(9),
        std::time::Duration::from_secs(30)
    );
}

#[test]
fn bootstrap_event_tail_limit_stays_small_for_remote_startup() {
    assert_eq!(BOOTSTRAP_EVENT_TAIL_LIMIT, 20);
}

#[test]
fn event_history_page_limit_stays_screen_sized_for_filtered_loads() {
    assert_eq!(EVENT_HISTORY_PAGE_LIMIT, 32);
}

#[test]
fn reader_idle_timeout_message_schedules_reconnect() {
    let client = LocalClient::new(test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.connection_state = TuiConnectionState::Streaming;
    let tx = app.runtime_tx.clone();

    tx.send(TuiRuntimeMessage::Disconnected {
        error: "event stream idle timeout after 45s".into(),
    })
    .unwrap();
    assert!(app.process_runtime_messages());

    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Reconnecting { attempt: 1, .. }
    ));
    assert_eq!(
        app.connection_detail(),
        Some("event stream idle timeout after 45s")
    );
    assert!(app.reconnect_deadline.is_some());
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
    app.schedule_refresh("cursor evt_123 was not found".into());

    assert!(matches!(
        app.connection_state,
        TuiConnectionState::RefreshRequired { .. }
    ));
    assert_eq!(
        app.connection_detail(),
        Some("cursor evt_123 was not found")
    );
    assert!(app.refresh_deadline.is_some());
}

#[test]
fn cursor_not_found_detection_uses_typed_http_error() {
    let err = crate::client::LocalHttpError {
        path: "/agents/default/events".into(),
        status_code: 404,
        message: "cursor evt_123 was not found".into(),
        code: Some("cursor_not_found".into()),
        hint: None,
        domain: None,
        retryable: None,
        context: Default::default(),
        correlation: Default::default(),
    };
    let err = anyhow::Error::new(err);
    assert!(is_cursor_not_found_error(&err));
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
        event: "callback_delivered".into(),
        data: StreamEventEnvelope {
            event_log_epoch: Some("epoch-test".into()),
            contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
            payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
            payload_schema_version: 1,
            id: "evt-stale".into(),
            event_seq: 2,
            ts: Utc::now(),
            agent_id: "default".into(),
            event_type: "callback_delivered".into(),
            provenance: None,
            payload: json!({
                "external_trigger_id": "cb-2",
                "agent_id": "default"
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
    apply_brief_event(
        &mut app,
        BriefRecord {
            id: "brief-1".into(),
            agent_id: "beta".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            work_item_id: None,
            turn_index: None,
            turn_id: None,
            kind: BriefKind::Result,
            created_at: Utc::now(),
            content_source: BriefContentSource::Inline,
            finalizes_assistant_round_id: None,
            text: "stale brief".into(),
            attachments: None,
            related_message_id: None,
            related_task_id: None,
        },
    );

    let change = app.apply_agent_list(vec![sample_agent_summary("gamma")]);

    assert_eq!(change, AgentListChange::RequiresBootstrap);
    assert_eq!(app.selected_agent_id(), Some("gamma"));
    assert!(app.projection.is_none());
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
    assert_eq!(app.status_line, "Loading agent state for beta");
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

#[test]
fn remote_agent_list_refresh_failures_back_off() {
    let client = LocalClient::remote(test_config(), "http://example.test:7878", "secret").unwrap();
    let mut app = TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    );

    app.apply_loaded_agents(Err("timeout one".into()));
    assert_eq!(app.agent_list_refresh_failures, 1);
    let first_deadline = app.agent_list_refresh_deadline.unwrap();

    app.apply_loaded_agents(Err("timeout two".into()));
    assert_eq!(app.agent_list_refresh_failures, 2);
    let second_deadline = app.agent_list_refresh_deadline.unwrap();

    assert!(second_deadline > first_deadline + std::time::Duration::from_secs(1));
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

// ── Pipeline integration tests ────────────────────────────────────────────

/// Helper: build a `StreamEventEnvelope` with the given fields.
fn pipeline_event_envelope(
    id: &str,
    event_seq: u64,
    agent_id: &str,
    event_type: &str,
    payload: serde_json::Value,
) -> StreamEventEnvelope {
    StreamEventEnvelope {
        event_log_epoch: Some("epoch-test".into()),
        contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
        payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
        payload_schema_version: 1,
        id: id.into(),
        event_seq,
        ts: Utc::now(),
        agent_id: agent_id.into(),
        event_type: event_type.into(),
        provenance: None,
        payload,
    }
}

/// Helper: build an `AgentStreamEvent` for a known kind.
fn pipeline_event(
    id: &str,
    event_seq: u64,
    agent_id: &str,
    kind: &str,
    payload: serde_json::Value,
) -> AgentStreamEvent {
    AgentStreamEvent {
        id: id.into(),
        event: kind.into(),
        data: pipeline_event_envelope(id, event_seq, agent_id, kind, payload),
    }
}

/// Basic end-to-end pipeline test: drive a minimal agent through a single
/// turn (`echo hello`) and verify all expected entries land in
/// `presentation.jsonl` with valid JSON structure and correct display-level
/// decisions.
#[test]
fn pipeline_single_turn_presentation_jsonl() {
    let client = LocalClient::new(test_config()).unwrap();
    let log_writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let log_root = log_writer.root().to_path_buf();
    let mut app = TuiApp::new(client, log_writer);

    // Bootstrap projection so events have a home.
    let snapshot = sample_snapshot("default", "evt-0");
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    let projection = app.projection.as_mut().unwrap();

    // Simulate a complete single turn: ExecCommand("echo hello").
    projection.apply_event(
        pipeline_event(
            "evt-1",
            1,
            "default",
            "process_execution_requested",
            json!({ "exec_command_cmd": "echo hello" }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-2",
            2,
            "default",
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "echo hello",
                "duration_ms": 5,
                "exit_status": 0,
                "stdout_preview": "hello"
            }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-3",
            3,
            "default",
            "assistant_round_recorded",
            json!({ "round": 1, "text_preview": "Done — echo hello succeeded." }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-4",
            4,
            "default",
            "turn_terminal",
            json!({ "kind": "completed" }),
        ),
        &app.log_writer,
    );

    // ── Verify presentation.jsonl ──────────────────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "presentation.jsonl should exist after pipeline events"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(!lines.is_empty(), "presentation.jsonl should have records");

    let mut seen_command = false;
    let mut seen_assistant = false;
    let mut seen_turn_terminal = false;
    let mut seen_tool_executed = false;

    for line in &lines {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("every line must be valid JSON");

        let reducer_kinds: Vec<&str> = record["reducer_event_kinds"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if reducer_kinds.contains(&"tool_executed") {
            seen_command = true;
            assert_eq!(record["item_kind"], "command_executed");
        }
        if reducer_kinds.contains(&"assistant_round_recorded") {
            seen_assistant = true;
            assert_eq!(record["item_kind"], "assistant_progress");
        }
        if reducer_kinds.contains(&"tool_executed") {
            seen_tool_executed = true;
        }
        if reducer_kinds.contains(&"turn_terminal") {
            seen_turn_terminal = true;
        }

        // Verify display-level decisions: for each display level (3,4,5),
        // decision must be "shown" iff min_display_level ≤ that level.
        let min_level = record["min_display_level"].as_u64().unwrap_or(0) as u8;
        let displays = record["displays"]
            .as_array()
            .expect("displays must be an array");
        for display in displays {
            let dl = display["display_level"].as_u64().unwrap() as u8;
            let decision = display["decision"].as_str().unwrap();
            if min_level <= dl {
                assert_eq!(
                    decision, "shown",
                    "min_display_level={min_level} ≤ display_level={dl} → decision must be shown"
                );
            } else {
                assert_eq!(
                    decision, "hidden",
                    "min_display_level={min_level} > display_level={dl} → decision must be hidden"
                );
            }
        }
    }

    assert!(seen_command, "should contain command result event");
    assert!(
        seen_assistant,
        "should contain assistant_round_recorded event"
    );
    assert!(seen_tool_executed, "should contain tool_executed event");
    assert!(seen_turn_terminal, "should contain turn_terminal event");
}

/// Pipeline test: display level filtering across all visibility levels.
///
/// Verifies that the TUI pipeline correctly applies `min_display_level` →
/// `decision=shown|hidden` filtering across all three operator display
/// levels (Info=3, Verbose=4, Debug=5), and that invalid level values are
/// handled gracefully.
#[test]
fn pipeline_display_level_filtering() {
    let client = LocalClient::new(test_config()).unwrap();
    let log_writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let log_root = log_writer.root().to_path_buf();
    let mut app = TuiApp::new(client, log_writer);

    let snapshot = sample_snapshot("default", "evt-0");
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    let projection = app.projection.as_mut().unwrap();

    // ── Feed events spanning all five OperatorVisibility levels ─────────
    //
    // Visibility 1 (ActionRequired): operator_notification_requested
    // Visibility 2 (WorkDone):       work_item_written with completed state
    // Visibility 3 (TurnResult):     brief_created (normal brief)
    // Visibility 4 (Progress):       assistant_round_recorded
    // Visibility 5 (Trace):          tool_executed
    //
    // process_execution_requested is an input-only runtime trace for command
    // lifecycle state and must not create a durable presentation item on its
    // own.

    projection.apply_event(
        pipeline_event(
            "evt-notify",
            1,
            "default",
            "operator_notification_requested",
            json!({ "summary": "action required notification", "severity": "info" }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-work-done",
            2,
            "default",
            "work_item_written",
            json!({ "record": { "id": "wi-test", "objective": "test", "state": "completed" } }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-brief",
            3,
            "default",
            "brief_created",
            json!({ "id": "b1", "agent_id": "default", "text": "turn result brief", "created_at": "2025-01-01T00:00:00Z" }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-assistant",
            4,
            "default",
            "assistant_round_recorded",
            json!({ "round": 1, "text_preview": "assistant progress update" }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-cmd",
            5,
            "default",
            "process_execution_requested",
            json!({ "exec_command_cmd": "echo trace" }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-tool",
            6,
            "default",
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "echo trace",
                "duration_ms": 3,
                "exit_status": 0,
                "stdout_preview": "trace"
            }),
        ),
        &app.log_writer,
    );

    // ── Verify presentation.jsonl ──────────────────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "presentation.jsonl should exist after pipeline events"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(!lines.is_empty(), "presentation.jsonl should have records");

    let mut seen_notify = false;
    let mut seen_work_done = false;
    let mut seen_brief = false;
    let mut seen_assistant = false;
    let mut seen_command = false;
    let mut seen_tool = false;

    for line in &lines {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("every line must be valid JSON");

        let reducer_kinds: Vec<&str> = record["reducer_event_kinds"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if reducer_kinds.contains(&"operator_notification_requested") {
            seen_notify = true;
        }
        if reducer_kinds.contains(&"work_item_written") {
            seen_work_done = true;
        }
        if reducer_kinds.contains(&"brief_created") {
            seen_brief = true;
        }
        if reducer_kinds.contains(&"assistant_round_recorded") {
            seen_assistant = true;
        }
        if reducer_kinds.contains(&"process_execution_requested") {
            seen_command = true;
        }
        if reducer_kinds.contains(&"tool_executed") {
            seen_tool = true;
        }

        let min_level = record["min_display_level"].as_u64().unwrap_or(0) as u8;
        let displays = record["displays"]
            .as_array()
            .expect("displays must be an array");

        // Verify exactly 3 display levels (3, 4, 5).
        assert_eq!(
            displays.len(),
            3,
            "displays should have exactly 3 entries (levels 3, 4, 5)"
        );

        let mut seen_levels = std::collections::BTreeSet::new();
        for display in displays {
            let dl = display["display_level"].as_u64().unwrap() as u8;
            assert!(
                (3..=5).contains(&dl),
                "display_level must be 3, 4, or 5, got {dl}"
            );
            let decision = display["decision"].as_str().unwrap();
            let cells = display["cells"].as_array().expect("cells must be an array");

            if min_level <= dl {
                assert_eq!(
                    decision, "shown",
                    "min_display_level={min_level} ≤ display_level={dl} → decision must be shown"
                );
                assert!(
                    !cells.is_empty(),
                    "shown display must have non-empty cells at level {dl}"
                );
                for cell in cells {
                    let body_preview = cell["body_preview"].as_str().unwrap_or("");
                    assert!(
                        !body_preview.is_empty(),
                        "shown cell must have non-empty body_preview at level {dl}"
                    );
                    assert!(
                        cell["body_char_count"].as_u64().unwrap_or(0) > 0,
                        "shown cell must have body_char_count > 0 at level {dl}"
                    );
                }
            } else {
                assert_eq!(
                    decision, "hidden",
                    "min_display_level={min_level} > display_level={dl} → decision must be hidden"
                );
                assert!(
                    cells.is_empty(),
                    "hidden display must have empty cells at level {dl}, got {} cells",
                    cells.len()
                );
            }

            seen_levels.insert(dl);
        }

        // Verify all three display levels are present.
        assert!(
            seen_levels.contains(&3) && seen_levels.contains(&4) && seen_levels.contains(&5),
            "displays must contain all three levels 3, 4, 5"
        );
    }

    assert!(
        seen_notify,
        "should contain operator_notification_requested (visibility 1)"
    );
    assert!(
        seen_work_done,
        "should contain work_item_written (visibility 2)"
    );
    assert!(seen_brief, "should contain brief_created (visibility 3)");
    assert!(
        seen_assistant,
        "should contain assistant_round_recorded (visibility 4)"
    );
    assert!(
        !seen_command,
        "process_execution_requested should not create a standalone presentation item"
    );
    assert!(seen_tool, "should contain tool_executed (visibility 5)");
}

/// Reducer aggregation pipeline test: verify how `reducer_event_summaries`,
/// `reducer_event_kinds`, and related fields are composed across multi-event
/// turns and written to `presentation.jsonl`.
///
/// Verification points from issue #1116:
/// - `reducer_event_summaries` array is non-empty for multi-event turns
/// - `reducer_event_kinds` statistics match the actual summaries
/// - Single JSON record < 50 KB
/// - 0 summaries → empty array, no panic
/// - Truncation marker when summaries exceed configured limit
/// - `reducer_event_summaries_truncated` field present only when truncated
#[test]
fn pipeline_reducer_aggregation() {
    let client = LocalClient::new(test_config()).unwrap();
    let log_writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let log_root = log_writer.root().to_path_buf();
    let mut app = TuiApp::new(client, log_writer);

    let snapshot = sample_snapshot("default", "evt-0");
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    let projection = app.projection.as_mut().unwrap();

    // ── Feed a multi-event turn. Standalone process_execution_requested
    // events are audit/runtime inputs only; presentation output starts at the
    // corresponding tool_executed result records.
    //
    // Event 1: process_execution_requested → no standalone presentation item
    // Event 2: tool_executed → command_executed presentation item
    // Event 3: brief_created → standalone (1 reducer event)
    // Event 4: assistant_round_recorded → standalone (1 reducer event)
    // Event 5: second process_execution_requested → no standalone item
    // Event 6: second tool_executed → command_executed presentation item
    // Event 7: assistant_round_recorded (second round) → standalone

    // Pair 1: command execution
    projection.apply_event(
        pipeline_event(
            "evt-req-1",
            1,
            "default",
            "process_execution_requested",
            json!({ "ExecCommand": { "cmd": "cargo build" } }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-tool-1",
            2,
            "default",
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo build",
                "duration_ms": 4200,
                "exit_status": 0,
                "stdout_preview": "Compiling holon v0.13.0\nFinished dev"
            }),
        ),
        &app.log_writer,
    );

    // Standalone events
    projection.apply_event(
        pipeline_event(
            "evt-brief-1",
            3,
            "default",
            "brief_created",
            json!({
                "id": "b1",
                "agent_id": "default",
                "kind": "Result",
                "text": "Build succeeded after cargo build",
                "created_at": "2025-01-01T00:00:00Z"
            }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-assistant-1",
            4,
            "default",
            "assistant_round_recorded",
            json!({ "round": 1, "text_preview": "Let me run the build and check the results." }),
        ),
        &app.log_writer,
    );

    // Pair 2: second command execution
    projection.apply_event(
        pipeline_event(
            "evt-req-2",
            5,
            "default",
            "process_execution_requested",
            json!({ "ExecCommand": { "cmd": "cargo test" } }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-tool-2",
            6,
            "default",
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo test",
                "duration_ms": 2500,
                "exit_status": 0,
                "stdout_preview": "test result: ok. 14 passed"
            }),
        ),
        &app.log_writer,
    );

    projection.apply_event(
        pipeline_event(
            "evt-assistant-2",
            7,
            "default",
            "assistant_round_recorded",
            json!({ "round": 2, "text_preview": "All tests pass. The fix is verified." }),
        ),
        &app.log_writer,
    );

    // ── Verify presentation.jsonl exists ──────────────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "presentation.jsonl should exist after pipeline events"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(!lines.is_empty(), "presentation.jsonl should have records");

    // ── Parse all records ─────────────────────────────────────────────────
    struct AggRecord {
        item_kind: String,
        reducer_event_ids_count: usize,
        reducer_event_kinds: Vec<String>,
        reducer_event_summaries_count: usize,
        truncated: Option<String>,
    }

    let mut records: Vec<AggRecord> = Vec::new();
    let mut all_kinds = Vec::new();

    for line in &lines {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("every line must be valid JSON");

        let item_kind = record["item_kind"].as_str().unwrap_or("?").to_string();
        let reducer_event_ids = record["reducer_event_ids"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        let reducer_event_kinds: Vec<String> = record["reducer_event_kinds"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let reducer_event_summaries: Vec<String> = record["reducer_event_summaries"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let truncated = record["reducer_event_summaries_truncated"]
            .as_str()
            .map(|s| s.to_string());

        // Verification 3: Each JSON record < 50 KB
        assert!(
            line.len() < 50_000,
            "JSON record must be < 50 KB, got {} bytes for item_kind={}",
            line.len(),
            item_kind
        );

        records.push(AggRecord {
            item_kind: item_kind.clone(),
            reducer_event_ids_count: reducer_event_ids.len(),
            reducer_event_kinds: reducer_event_kinds.clone(),
            reducer_event_summaries_count: reducer_event_summaries.len(),
            truncated,
        });

        all_kinds.extend(reducer_event_kinds);
    }

    // ── Verification 1: reducer_event_summaries match reducer_event_ids ──
    for rec in &records {
        assert_eq!(
            rec.reducer_event_summaries_count, rec.reducer_event_ids_count,
            "reducer_event_summaries count must match reducer_event_ids count"
        );
    }

    // ── Verification 2: reducer_event_kinds statistics match summaries ────
    for rec in &records {
        assert_eq!(
            rec.reducer_event_kinds.len(),
            rec.reducer_event_summaries_count,
            "reducer_event_kinds count must match summaries count for item_kind={}",
            rec.item_kind
        );
        // Every summary must be a non-empty string
        assert!(
            rec.reducer_event_kinds.iter().all(|k| !k.is_empty()),
            "all reducer_event_kinds must be non-empty for item_kind={}",
            rec.item_kind
        );
    }

    // ── Verification: kinds across all records match expected ───
    let mut kind_counts = std::collections::HashMap::new();
    for k in &all_kinds {
        *kind_counts.entry(k.as_str()).or_insert(0) += 1;
    }

    assert!(
        !kind_counts.contains_key("process_execution_requested"),
        "process_execution_requested should not create presentation output; got: {:?}",
        kind_counts
    );
    assert!(
        kind_counts.contains_key("tool_executed"),
        "should contain tool_executed kind; got: {:?}",
        kind_counts
    );
    assert!(
        kind_counts.contains_key("brief_created"),
        "should contain brief_created kind; got: {:?}",
        kind_counts
    );
    assert!(
        kind_counts.contains_key("assistant_round_recorded"),
        "should contain assistant_round_recorded kind; got: {:?}",
        kind_counts
    );

    // ── Verification 4: 0 summaries → empty array, no panic ───────────────
    //
    // Test that writing presentation items with an empty reducer_events
    // produces empty arrays and does not panic.
    let empty_writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let empty_root = empty_writer.root().to_path_buf();

    // Write with empty reducer_events and empty items → should succeed
    let result = empty_writer.write_presentation_items(&[], &[]);
    assert!(result.is_ok(), "write with empty arrays should succeed");

    // Write with empty reducer_events but non-empty items
    use crate::presentation::{PresentationItem, TimedItem};
    let dummy_item = TimedItem::with_key(
        PresentationItem::AssistantProgress {
            text: "empty test".into(),
            state: crate::presentation::ItemState::Stable,
        },
        chrono::Utc::now(),
        "empty-test",
    );
    let result2 = empty_writer.write_presentation_items(&[], &[dummy_item]);
    assert!(
        result2.is_ok(),
        "write with empty reducer_events should succeed"
    );

    // Read back: should have 1 record with empty reducer arrays
    let empty_presentation_path = empty_root.join("presentation.jsonl");
    if empty_presentation_path.exists() {
        let raw = std::fs::read_to_string(&empty_presentation_path).unwrap();
        let lines: Vec<&str> = raw.trim().lines().collect();
        for line in &lines {
            let record: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            let ids = record["reducer_event_ids"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            let summaries = record["reducer_event_summaries"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            assert_eq!(
                ids, 0,
                "empty reducer_events should produce empty reducer_event_ids"
            );
            assert_eq!(
                summaries, 0,
                "empty reducer_events should produce empty reducer_event_summaries"
            );
            // No truncation marker when arrays are empty
            assert!(
                record["reducer_event_summaries_truncated"].is_null(),
                "no truncation marker for empty arrays"
            );
        }
    }

    // ── Verification: truncated field is None for normal records ──
    for rec in &records {
        assert!(
            rec.truncated.is_none(),
            "reducer_event_summaries_truncated should be absent for {} events (item_kind={})",
            rec.reducer_event_summaries_count,
            rec.item_kind
        );
    }
}

/// Multi-turn continuity pipeline test: verify turn boundaries, event
/// ordering, and log completeness across 3 consecutive turns.
///
/// Verification points from issue #1117:
/// - Each turn produces a presentation record with `turn_terminal` in
///   `reducer_event_kinds`
/// - Events across turns are ordered by timestamp (monotonic `ts`)
/// - No turns are dropped (turn count matches)
/// - `presentation.jsonl` is append-only with no corruption at boundaries
#[test]
fn pipeline_multi_turn_continuity() {
    let client = LocalClient::new(test_config()).unwrap();
    let log_writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let log_root = log_writer.root().to_path_buf();
    let mut app = TuiApp::new(client, log_writer);

    let snapshot = sample_snapshot("default", "evt-0");
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    let projection = app.projection.as_mut().unwrap();

    // ── Turn 1: cargo build ────────────────────────────────────────────
    projection.apply_event(
        pipeline_event(
            "t1-req",
            1,
            "default",
            "process_execution_requested",
            json!({ "ExecCommand": { "cmd": "cargo build" } }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t1-tool",
            2,
            "default",
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo build",
                "duration_ms": 3500,
                "exit_status": 0,
                "stdout_preview": "Compiling holon v0.13.0"
            }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t1-asst",
            3,
            "default",
            "assistant_round_recorded",
            json!({ "round": 1, "text_preview": "Build succeeded." }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t1-term",
            4,
            "default",
            "turn_terminal",
            json!({ "kind": "completed", "turn_number": 1 }),
        ),
        &app.log_writer,
    );

    // ── Turn 2: cargo test ─────────────────────────────────────────────
    projection.apply_event(
        pipeline_event(
            "t2-req",
            5,
            "default",
            "process_execution_requested",
            json!({ "ExecCommand": { "cmd": "cargo test" } }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t2-tool",
            6,
            "default",
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo test",
                "duration_ms": 8200,
                "exit_status": 0,
                "stdout_preview": "test result: ok. 14 passed"
            }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t2-asst",
            7,
            "default",
            "assistant_round_recorded",
            json!({ "round": 2, "text_preview": "All tests pass." }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t2-term",
            8,
            "default",
            "turn_terminal",
            json!({ "kind": "completed", "turn_number": 2 }),
        ),
        &app.log_writer,
    );

    // ── Turn 3: echo done ──────────────────────────────────────────────
    projection.apply_event(
        pipeline_event(
            "t3-req",
            9,
            "default",
            "process_execution_requested",
            json!({ "ExecCommand": { "cmd": "echo done" } }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t3-tool",
            10,
            "default",
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "echo done",
                "duration_ms": 5,
                "exit_status": 0,
                "stdout_preview": "done"
            }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t3-asst",
            11,
            "default",
            "assistant_round_recorded",
            json!({ "round": 3, "text_preview": "Done." }),
        ),
        &app.log_writer,
    );
    projection.apply_event(
        pipeline_event(
            "t3-term",
            12,
            "default",
            "turn_terminal",
            json!({ "kind": "completed", "turn_number": 3 }),
        ),
        &app.log_writer,
    );

    // ── Verify presentation.jsonl ──────────────────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "presentation.jsonl should exist after multi-turn pipeline events"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(!lines.is_empty(), "presentation.jsonl should have records");

    // ── Parse all records ──────────────────────────────────────────────
    struct TurnRecord {
        ts: String,
        reducer_event_ids: Vec<String>,
        reducer_event_kinds: Vec<String>,
    }

    let mut records: Vec<TurnRecord> = Vec::new();
    let mut turn_terminal_count: usize = 0;

    for line in &lines {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("every line must be valid JSON");

        let ts = record["ts"].as_str().unwrap_or("").to_string();
        let reducer_event_ids: Vec<String> = record["reducer_event_ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let reducer_event_kinds: Vec<String> = record["reducer_event_kinds"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        if reducer_event_kinds.contains(&"turn_terminal".to_string()) {
            turn_terminal_count += 1;
        }

        records.push(TurnRecord {
            ts,
            reducer_event_ids,
            reducer_event_kinds,
        });
    }

    // ── Verification 1: Exactly 3 turn_terminal records ────────────────
    assert_eq!(
        turn_terminal_count, 3,
        "should have exactly 3 presentation records containing turn_terminal reducer event, got {turn_terminal_count}"
    );

    // ── Verification 2: Events ordered by timestamp (monotonic ts) ─────
    for i in 1..records.len() {
        assert!(
            records[i].ts >= records[i - 1].ts,
            "presentation.jsonl records must be ordered by timestamp; record {} ts={} < previous ts={}",
            i,
            records[i].ts,
            records[i - 1].ts
        );
    }

    // ── Verification 3: Every line is valid JSON (no corruption) ──────
    for (i, line) in lines.iter().enumerate() {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "line {} in presentation.jsonl must be valid JSON",
            i
        );
    }

    // ── Verification 4: Each turn has expected reduer event kinds ────
    let mut seen_turn_1 = false;
    let mut seen_turn_2 = false;
    let mut seen_turn_3 = false;
    let mut seen_command = false;
    let mut seen_assistant = false;

    for rec in &records {
        if rec.reducer_event_ids.contains(&"t1-term".to_string()) {
            seen_turn_1 = true;
        }
        if rec
            .reducer_event_kinds
            .contains(&"tool_executed".to_string())
        {
            seen_command = true;
            // command_executed records should reference a turn's tool result event.
            let has_turn_event = rec
                .reducer_event_ids
                .iter()
                .any(|id| id == "t1-tool" || id == "t2-tool" || id == "t3-tool");
            assert!(
                has_turn_event,
                "command_executed record should reference a turn's tool result event"
            );
        }
        if rec
            .reducer_event_kinds
            .contains(&"assistant_round_recorded".to_string())
        {
            seen_assistant = true;
        }
        if rec.reducer_event_ids.contains(&"t2-term".to_string()) {
            seen_turn_2 = true;
        }
        if rec.reducer_event_ids.contains(&"t3-term".to_string()) {
            seen_turn_3 = true;
        }
    }

    assert!(
        seen_turn_1,
        "should have a record with turn 1 terminal event"
    );
    assert!(
        seen_turn_2,
        "should have a record with turn 2 terminal event"
    );
    assert!(
        seen_turn_3,
        "should have a record with turn 3 terminal event"
    );
    assert!(seen_command, "should have command_executed records");
    assert!(seen_assistant, "should have assistant_progress records");

    // ── Verification 5: No state leak between turns ───────────────────
    // Turn 2 record should NOT contain turn 1 event ids
    for rec in &records {
        if rec.reducer_event_ids.contains(&"t2-term".to_string()) {
            assert!(
                !rec.reducer_event_ids.contains(&"t1-tool".to_string()),
                "turn 2 record should not contain turn 1 tool event ids"
            );
            assert!(
                !rec.reducer_event_ids.contains(&"t1-tool".to_string()),
                "turn 2 record should not contain turn 1 tool event"
            );
        }
        if rec.reducer_event_ids.contains(&"t3-term".to_string()) {
            assert!(
                !rec.reducer_event_ids.contains(&"t1-tool".to_string()),
                "turn 3 record should not contain turn 1 tool event ids"
            );
            assert!(
                !rec.reducer_event_ids.contains(&"t2-tool".to_string()),
                "turn 3 record should not contain turn 2 tool event ids"
            );
        }
    }
}

/// Stress test: drive 50+ tool calls through the TUI pipeline in a single
/// turn and verify performance boundaries — JSON record size, JSONL
/// validity, reducer completeness, and bounded execution time.
#[test]
fn pipeline_stress_50_tool_calls() {
    let client = LocalClient::new(test_config()).unwrap();
    let log_writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let log_root = log_writer.root().to_path_buf();
    let mut app = TuiApp::new(client, log_writer);

    let snapshot = sample_snapshot("default", "evt-0");
    app.projection = Some(TuiProjection::from_snapshot(snapshot));
    let projection = app.projection.as_mut().unwrap();

    const TOOL_COUNT: usize = 55;

    let start = std::time::Instant::now();

    // Feed 55 pairs of process_execution_requested + tool_executed.
    let mut event_seq: u64 = 0;
    for step in 1..=TOOL_COUNT {
        event_seq += 1;
        let cmd = format!("echo step {step}");
        let stdout = format!("step {step}");
        let req_id = format!("evt-req-{step}");
        let tool_id = format!("evt-tool-{step}");

        projection.apply_event(
            pipeline_event(
                &req_id,
                event_seq,
                "default",
                "process_execution_requested",
                json!({ "exec_command_cmd": cmd }),
            ),
            &app.log_writer,
        );

        event_seq += 1;
        projection.apply_event(
            pipeline_event(
                &tool_id,
                event_seq,
                "default",
                "tool_executed",
                json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": cmd,
                    "duration_ms": 1,
                    "exit_status": 0,
                    "stdout_preview": stdout
                }),
            ),
            &app.log_writer,
        );
    }

    // End the turn.
    event_seq += 1;
    projection.apply_event(
        pipeline_event(
            "evt-assistant",
            event_seq,
            "default",
            "assistant_round_recorded",
            json!({ "round": 1, "text_preview": "All 55 steps completed." }),
        ),
        &app.log_writer,
    );

    let elapsed = start.elapsed();

    // ── Verify presentation.jsonl ──────────────────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "presentation.jsonl should exist after pipeline events"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(
        lines.len() >= TOOL_COUNT,
        "presentation.jsonl should have at least {TOOL_COUNT} records (one per command pair), got {}",
        lines.len()
    );

    for line in &lines {
        // Verify valid JSON.
        let record: serde_json::Value =
            serde_json::from_str(line).expect("every line must be valid JSON");

        // Verify record size < 100 KB (serialized JSON bytes).
        let line_bytes = line.len();
        assert!(
            line_bytes < 100 * 1024,
            "each presentation record must be < 100 KB, got {line_bytes} bytes"
        );

        // Verify display decisions structure.
        let displays = record["displays"].as_array();
        assert!(displays.is_some(), "record must have displays array");
        let displays = displays.unwrap();
        assert_eq!(displays.len(), 3, "displays must have exactly 3 entries");

        for display in displays {
            let dl = display["display_level"].as_u64().unwrap() as u8;
            assert!((3..=5).contains(&dl), "display_level must be 3, 4, or 5");
            let decision = display["decision"].as_str().unwrap();
            assert!(decision == "shown" || decision == "hidden");
        }
    }

    // Performance boundary: 55 tool calls must complete in under 5 seconds.
    assert!(
        elapsed.as_secs() < 5,
        "stress test must complete in under 5 seconds, took {} ms",
        elapsed.as_millis()
    );
}
