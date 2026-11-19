use super::{
    onboarding_runtime_config_status, parse_agent_slash_action, parse_composer_submission,
    parse_skill_catalog_entries, should_treat_enter_as_paste_newline_state, slash_command_spec,
    slash_help_lines, slash_menu_enter_submission, slash_menu_specs, slash_prompt_lines,
    AgentSlashAction, ComposerSubmission, SlashCommand,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use std::time::Instant;

#[test]
fn parses_plain_chat_submission() {
    assert_eq!(
        parse_composer_submission("hello world").unwrap(),
        Some(ComposerSubmission::Chat("hello world".into()))
    );
}

#[test]
fn parses_skill_catalog_entries_from_response() {
    let entries = parse_skill_catalog_entries(&json!({
        "catalog": [{
            "name": "ghx",
            "skill_id": "agent:ghx",
            "description": "GitHub CLI workflows",
            "scope": "agent",
            "root_id": "agent_home:holon-dev",
            "skill_dir": "ghx",
            "path": "/agent/skills/ghx"
        }]
    }))
    .expect("catalog entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "ghx");
    assert_eq!(entries[0].description, "GitHub CLI workflows");
}

#[test]
fn escapes_slash_prefixed_chat() {
    assert_eq!(
        parse_composer_submission("//hello").unwrap(),
        Some(ComposerSubmission::Chat("/hello".into()))
    );
}

#[test]
fn paste_newline_heuristic_does_not_intercept_slash_commands() {
    assert!(!should_treat_enter_as_paste_newline_state(
        "/skills",
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        1,
        Some(Instant::now()),
    ));
}

#[test]
fn paste_newline_heuristic_still_applies_to_plain_text_bursts() {
    assert!(should_treat_enter_as_paste_newline_state(
        "hello",
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        1,
        Some(Instant::now()),
    ));
}

#[test]
fn parses_safe_slash_commands() {
    assert_eq!(
        parse_composer_submission("/help").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::Help, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/refresh").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::Refresh, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/model").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::Model, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/onboard").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::Onboard, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/clear-status").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::ClearStatus, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/debug-prompt").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::DebugPrompt, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/abort").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::Abort, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/vim").unwrap(),
        Some(ComposerSubmission::Slash(SlashCommand::Vim, vec![]))
    );
    assert_eq!(
        parse_composer_submission("/agent start").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::Agent,
            vec!["start".into()]
        ))
    );
    assert_eq!(
        parse_composer_submission("/agent stop").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::Agent,
            vec!["stop".into()]
        ))
    );
    assert_eq!(
        parse_composer_submission("/agent switch pause").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::Agent,
            vec!["switch".into(), "pause".into()]
        ))
    );
    assert_eq!(
        parse_composer_submission("/agent create worker").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::Agent,
            vec!["create".into(), "worker".into()]
        ))
    );
    assert_eq!(
        parse_composer_submission("/skill-catalog").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::SkillCatalog,
            vec![]
        ))
    );
    assert_eq!(
        parse_composer_submission("/skill-add ghx --copy").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::SkillAdd,
            vec!["ghx".into(), "--copy".into()]
        ))
    );
    assert_eq!(
        parse_composer_submission("/skill-enable ghx --copy").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::SkillEnable,
            vec!["ghx".into(), "--copy".into()]
        ))
    );
    assert_eq!(
        parse_composer_submission("/skill-remove ghx").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::SkillRemove,
            vec!["ghx".into()]
        ))
    );
    assert_eq!(
        parse_composer_submission("/skill-disable ghx").unwrap(),
        Some(ComposerSubmission::Slash(
            SlashCommand::SkillDisable,
            vec!["ghx".into()]
        ))
    );
}

#[test]
fn slash_only_shows_helpful_empty_command_error() {
    let err = parse_composer_submission("/").unwrap_err();
    assert!(err.to_string().contains("empty slash command; use /help"));
}

#[test]
fn slash_commands_reject_arguments() {
    let err = parse_composer_submission("/help extra").unwrap_err();
    assert!(err.to_string().contains("does not accept arguments"));
}

#[test]
fn slash_commands_require_arguments_for_agent() {
    let err = parse_composer_submission("/agent").unwrap_err();
    assert!(err.to_string().contains("requires a subcommand"));
}

#[test]
fn slash_display_is_not_a_command() {
    assert_eq!(
        parse_composer_submission("/display").unwrap(),
        Some(ComposerSubmission::Chat("/display".into()))
    );
}

#[test]
fn slash_commands_reject_too_many_arguments() {
    let err = parse_composer_submission("/agent default extra").unwrap_err();
    assert!(err
        .to_string()
        .contains("unknown /agent subcommand 'default'"));
    let err = parse_composer_submission("/agent start default extra").unwrap_err();
    assert!(err.to_string().contains("accepts at most one agent id"));
    let err = parse_composer_submission("/agent switch").unwrap_err();
    assert!(err
        .to_string()
        .contains("switch expects exactly one agent id"));
    let err = parse_composer_submission("/agent create").unwrap_err();
    assert!(err
        .to_string()
        .contains("create expects exactly one agent name"));
}

#[test]
fn agent_slash_lifecycle_actions_map_to_control_actions() {
    assert_eq!(
        parse_agent_slash_action(&["start".into()]).unwrap(),
        AgentSlashAction::Control {
            action: crate::types::ControlAction::Start,
            agent_id: None,
        }
    );
    assert_eq!(
        parse_agent_slash_action(&["stop".into()]).unwrap(),
        AgentSlashAction::Control {
            action: crate::types::ControlAction::Stop,
            agent_id: None,
        }
    );
    assert_eq!(
        parse_agent_slash_action(&["switch".into(), "pause".into()]).unwrap(),
        AgentSlashAction::Switch("pause".into())
    );
    assert_eq!(
        parse_agent_slash_action(&["create".into(), "worker".into()]).unwrap(),
        AgentSlashAction::Create("worker".into())
    );
    let err = parse_agent_slash_action(&["status".into()]).unwrap_err();
    assert!(err
        .to_string()
        .contains("unknown /agent subcommand 'status'"));
    let err = parse_agent_slash_action(&["pause".into()]).unwrap_err();
    assert!(err.to_string().contains("use /agent stop [agent-id]"));
    let err = parse_agent_slash_action(&["resume".into()]).unwrap_err();
    assert!(err.to_string().contains("use /agent start [agent-id]"));
}

#[test]
fn agent_slash_delete_action_parses() {
    assert_eq!(
        parse_agent_slash_action(&["delete".into()]).unwrap(),
        AgentSlashAction::Delete {
            agent_id: None,
            cascade_private_children: false,
        }
    );
    assert_eq!(
        parse_agent_slash_action(&["delete".into(), "agent-1".into()]).unwrap(),
        AgentSlashAction::Delete {
            agent_id: Some("agent-1".into()),
            cascade_private_children: false,
        }
    );
    assert_eq!(
        parse_agent_slash_action(&["delete".into(), "--cascade-private-children".into()]).unwrap(),
        AgentSlashAction::Delete {
            agent_id: None,
            cascade_private_children: true,
        }
    );
    assert_eq!(
        parse_agent_slash_action(&[
            "delete".into(),
            "agent-1".into(),
            "--cascade-private-children".into()
        ])
        .unwrap(),
        AgentSlashAction::Delete {
            agent_id: Some("agent-1".into()),
            cascade_private_children: true,
        }
    );
    let err = parse_agent_slash_action(&["delete".into(), "agent-1".into(), "agent-2".into()])
        .unwrap_err();
    assert!(err.to_string().contains("accepts at most one agent id"));
}

#[test]
fn unknown_slash_inputs_submit_as_chat() {
    assert_eq!(
        parse_composer_submission("/unknown").unwrap(),
        Some(ComposerSubmission::Chat("/unknown".into()))
    );
    assert_eq!(
        parse_composer_submission("/interrupt").unwrap(),
        Some(ComposerSubmission::Chat("/interrupt".into()))
    );
    assert_eq!(
        parse_composer_submission("/tmp/file").unwrap(),
        Some(ComposerSubmission::Chat("/tmp/file".into()))
    );
    assert_eq!(
        parse_composer_submission("/Users/example/project").unwrap(),
        Some(ComposerSubmission::Chat("/Users/example/project".into()))
    );
}

#[test]
fn slash_commands_reject_multiline_submission() {
    let err = parse_composer_submission("/help\nmore").unwrap_err();
    assert!(err
        .to_string()
        .contains("slash commands must be submitted on a single line"));
}

#[test]
fn slash_prompt_lists_candidates_for_prefix() {
    let lines = slash_prompt_lines("/de").expect("slash prompt should be active");
    assert!(lines[0].contains(">/debug-prompt"));
    assert!(lines[1].contains("open debug prompt dialog"));
}

#[test]
fn slash_prompt_ignores_escaped_slash_chat() {
    assert!(slash_prompt_lines("//hello").is_none());
}

#[test]
fn slash_menu_ignores_multiline_input() {
    assert!(slash_menu_specs("/mo\nextra").is_empty());
}

#[test]
fn slash_menu_enter_submission_uses_selected_command_for_prefix() {
    let model = slash_command_spec("/model").expect("model command");
    assert_eq!(slash_menu_enter_submission("/", model), "/model");
    assert_eq!(slash_menu_enter_submission("/mo", model), "/model");
    assert_eq!(slash_menu_enter_submission("   /mo  ", model), "/model");
}

#[test]
fn slash_menu_enter_submission_preserves_arguments_with_selected_command() {
    let agent = slash_command_spec("/agent").expect("agent command");
    assert_eq!(
        slash_menu_enter_submission("/ag default", agent),
        "/agent default"
    );
    assert_eq!(
        slash_menu_enter_submission("   /agent default  ", agent),
        "/agent default"
    );
}

#[test]
fn slash_prompt_matches_submit_semantics_for_leading_whitespace() {
    let lines = slash_prompt_lines("   /help").expect("slash prompt should be active");
    assert!(lines[0].contains(">/help"));
}

#[test]
fn slash_help_includes_current_commands() {
    let help = slash_help_lines().join("\n");
    for command in [
        "/help",
        "/agents",
        "/events",
        "/model",
        "/onboard",
        "/tasks",
        "/transcript",
        "/state",
        "/refresh",
        "/clear-status",
        "/debug-prompt",
        "/abort",
        "/vim",
        "/agent switch <agent-id>|create <name>|start [agent-id]|stop [agent-id]|delete [agent-id]",
        "/skills",
        "/skill-catalog",
        "/skill-add <source> [--remote] [--skill <name>] [--copy]",
        "/skill-remove <name>",
        "/skill-enable <name> [--copy]",
        "/skill-disable <name>",
    ] {
        assert!(help.contains(command), "help missing {command}");
    }
}

#[test]
fn onboarding_runtime_config_status_reports_restart_and_rejection() {
    let runtime_surface = holon_client::wire::RuntimeConfigSurface {
        model_default: "anthropic/claude-sonnet-4-6".into(),
        model_fallbacks: Vec::new(),
        vision_default: None,
        image_generation_default: None,
        decision: Default::default(),
        model_catalog: Vec::new(),
        unknown_model_fallback_configured: false,
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: 4096,
        max_tool_output_tokens: 16384,
        command_task_output_retention_bytes: 8 * 1024 * 1024,
        command_task_output_quota_bytes: 64 * 1024 * 1024,
        command_task_min_free_disk_bytes: 512 * 1024 * 1024,
        command_task_min_free_disk_percent: 5,
        disable_provider_fallback: false,
        runtime_db_retention: Default::default(),
        providers: Vec::new(),
        web_search: holon_client::wire::RuntimeWebSearchSummary {
            enabled: false,
            builtin_provider_enabled: false,
            provider: "duckduckgo".into(),
            mode: "fallback".into(),
            providers: Vec::new(),
            max_results: 5,
            max_provider_attempts: 2,
        },
        available_search_provider_kinds: Vec::new(),
        web_search_providers: Vec::new(),
    };
    let accepted = holon_client::wire::RuntimeConfigUpdateResponse {
        ok: true,
        changed: true,
        config_file_path: "/tmp/config.json".into(),
        results: vec![holon_client::wire::RuntimeConfigUpdateResult {
            key: "model.default".into(),
            effect: holon_client::wire::RuntimeConfigUpdateEffect::AcceptedRequiresRestart,
            reason: "persisted; restart required".into(),
        }],
        runtime_surface: runtime_surface.clone(),
        reload: Default::default(),
        setup: holon_client::wire::RuntimeSetup {
            status: holon_client::wire::RuntimeSetupStatus::Blocked,
            default_model: Some("anthropic/claude-sonnet-4-6".into()),
            provider: Some("anthropic".into()),
            actions: vec!["configure_model_credential".into()],
        },
    };
    let status = onboarding_runtime_config_status("openai/gpt-5.4", &accepted);
    assert!(status.contains("via daemon config"));
    assert!(status.contains("restart/reload required"));

    let rejected = holon_client::wire::RuntimeConfigUpdateResponse {
        ok: true,
        changed: false,
        config_file_path: "/tmp/config.json".into(),
        results: vec![holon_client::wire::RuntimeConfigUpdateResult {
            key: "model.default".into(),
            effect: holon_client::wire::RuntimeConfigUpdateEffect::Rejected,
            reason: "unsupported or startup-only config key".into(),
        }],
        runtime_surface: runtime_surface.clone(),
        reload: Default::default(),
        setup: accepted.setup.clone(),
    };
    let status = onboarding_runtime_config_status("openai/gpt-5.4", &rejected);
    assert!(status.contains("rejected"));
    assert!(status.contains("unsupported"));

    let omitted = holon_client::wire::RuntimeConfigUpdateResponse {
        ok: true,
        changed: true,
        config_file_path: "/tmp/config.json".into(),
        results: Vec::new(),
        runtime_surface,
        reload: Default::default(),
        setup: accepted.setup.clone(),
    };
    let status = onboarding_runtime_config_status("openai/gpt-5.4", &omitted);
    assert!(status.contains("requested"));
    assert!(!status.contains("updated"));
}
