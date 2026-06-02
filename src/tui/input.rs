use super::*;
use crate::tui::keymap::{
    resolve_key, ComposerAction, KeyContext, ScrollAction, SlashMenuAction, TuiKeyAction,
};
use crossterm::event::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashCommand {
    Help,
    Agents,
    Events,
    Model,
    Tasks,
    Transcript,
    State,
    Refresh,
    ClearStatus,
    DebugPrompt,
    Display,
    Abort,
    Agent,
    Skills,
    SkillInstall,
    SkillUninstall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ComposerSubmission {
    Chat(String),
    Slash(SlashCommand, Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashArgRule {
    None,
    ExactlyOne,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlashArgHint {
    None,
    Values(&'static [&'static str]),
    Agent,
    SkillName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlashCommandCategory {
    Navigation,
    Runtime,
    Agent,
    Skills,
    Debug,
}

impl SlashCommandCategory {
    fn label(self) -> &'static str {
        match self {
            SlashCommandCategory::Navigation => "Navigation",
            SlashCommandCategory::Runtime => "Runtime",
            SlashCommandCategory::Agent => "Agent",
            SlashCommandCategory::Skills => "Skills",
            SlashCommandCategory::Debug => "Debug",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentSlashAction {
    Switch(String),
    Create(String),
    Control {
        action: crate::types::ControlAction,
        agent_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SlashCommandSpec {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    pub(super) usage: &'static str,
    pub(super) arg_hint: SlashArgHint,
    category: SlashCommandCategory,
    arg_rule: SlashArgRule,
    command: SlashCommand,
}

const DISPLAY_MODE_ARGS: &[&str] = &["info", "verbose", "debug", "3", "4", "5"];

const SLASH_COMMAND_SPECS: [SlashCommandSpec; 16] = [
    SlashCommandSpec {
        name: "/help",
        description: "show slash command help",
        usage: "/help",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Navigation,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Help,
    },
    SlashCommandSpec {
        name: "/agents",
        description: "open agent picker",
        usage: "/agents",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Agent,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Agents,
    },
    SlashCommandSpec {
        name: "/events",
        description: "open raw events overlay",
        usage: "/events",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Navigation,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Events,
    },
    SlashCommandSpec {
        name: "/model",
        description: "open selected agent model picker",
        usage: "/model",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Agent,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Model,
    },
    SlashCommandSpec {
        name: "/tasks",
        description: "open task overlay",
        usage: "/tasks",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Runtime,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Tasks,
    },
    SlashCommandSpec {
        name: "/transcript",
        description: "open conversation events overlay",
        usage: "/transcript",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Navigation,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Transcript,
    },
    SlashCommandSpec {
        name: "/state",
        description: "open agent state overlay",
        usage: "/state",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Agent,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::State,
    },
    SlashCommandSpec {
        name: "/refresh",
        description: "refresh selected agent",
        usage: "/refresh",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Runtime,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Refresh,
    },
    SlashCommandSpec {
        name: "/clear-status",
        description: "clear local status line",
        usage: "/clear-status",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Runtime,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::ClearStatus,
    },
    SlashCommandSpec {
        name: "/debug-prompt",
        description: "open debug prompt dialog",
        usage: "/debug-prompt",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Debug,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::DebugPrompt,
    },
    SlashCommandSpec {
        name: "/display",
        description: "set chat display mode",
        usage: "/display <info|verbose|debug|3|4|5>",
        arg_hint: SlashArgHint::Values(DISPLAY_MODE_ARGS),
        category: SlashCommandCategory::Runtime,
        arg_rule: SlashArgRule::ExactlyOne,
        command: SlashCommand::Display,
    },
    SlashCommandSpec {
        name: "/abort",
        description: "abort current agent run",
        usage: "/abort",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Agent,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Abort,
    },
    SlashCommandSpec {
        name: "/agent",
        description: "switch or control an agent",
        usage: "/agent switch <agent-id>|create <name>|start [agent-id]|stop [agent-id]",
        arg_hint: SlashArgHint::Agent,
        category: SlashCommandCategory::Agent,
        arg_rule: SlashArgRule::Agent,
        command: SlashCommand::Agent,
    },
    SlashCommandSpec {
        name: "/skills",
        description: "show installed skills",
        usage: "/skills",
        arg_hint: SlashArgHint::None,
        category: SlashCommandCategory::Skills,
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Skills,
    },
    SlashCommandSpec {
        name: "/skill-install",
        description: "install a builtin skill",
        usage: "/skill-install <name>",
        arg_hint: SlashArgHint::SkillName,
        category: SlashCommandCategory::Skills,
        arg_rule: SlashArgRule::ExactlyOne,
        command: SlashCommand::SkillInstall,
    },
    SlashCommandSpec {
        name: "/skill-uninstall",
        description: "uninstall a skill",
        usage: "/skill-uninstall <name>",
        arg_hint: SlashArgHint::SkillName,
        category: SlashCommandCategory::Skills,
        arg_rule: SlashArgRule::ExactlyOne,
        command: SlashCommand::SkillUninstall,
    },
];

/// Maximum number of entries to keep in input history
const MAX_INPUT_HISTORY: usize = 100;

pub(super) fn slash_help_lines() -> Vec<String> {
    let mut lines = vec![
        "Slash Commands".to_string(),
        "  Type / to browse commands; non-command /text is sent as chat.".to_string(),
        "  Prefix with // to send text that starts with a single slash.".to_string(),
    ];
    let categories = [
        SlashCommandCategory::Navigation,
        SlashCommandCategory::Agent,
        SlashCommandCategory::Skills,
        SlashCommandCategory::Runtime,
        SlashCommandCategory::Debug,
    ];

    for category in categories {
        lines.push(String::new());
        lines.push(category.label().to_string());
        lines.extend(
            SLASH_COMMAND_SPECS
                .iter()
                .filter(|spec| spec.category == category)
                .map(|spec| format!("  {:<58} {}", spec.usage, spec.description)),
        );
    }
    lines
}

fn slash_command_spec(command: &str) -> Option<SlashCommandSpec> {
    SLASH_COMMAND_SPECS
        .iter()
        .copied()
        .find(|spec| spec.name == command)
}

fn slash_command_argument_error(spec: SlashCommandSpec, args: usize) -> anyhow::Error {
    match spec.arg_rule {
        SlashArgRule::None => anyhow!(
            "{0} does not accept arguments; usage: {1}",
            spec.name,
            spec.usage
        ),
        SlashArgRule::ExactlyOne if args == 0 => {
            anyhow!(
                "{0} requires one argument; usage: {1}",
                spec.name,
                spec.usage
            )
        }
        SlashArgRule::ExactlyOne => {
            anyhow!(
                "{0} expects exactly one argument; usage: {1}",
                spec.name,
                spec.usage
            )
        }
        SlashArgRule::Agent => anyhow!(
            "{0} expects an agent id or lifecycle action; usage: {1}",
            spec.name,
            spec.usage
        ),
    }
}

fn parse_agent_slash_action(args: &[String]) -> Result<AgentSlashAction> {
    let Some(first) = args.first() else {
        return Err(anyhow!(
            "/agent requires a subcommand; usage: /agent switch <agent-id>|create <name>|start [agent-id]|stop [agent-id]"
        ));
    };
    match first.as_str() {
        "start" | "stop" => {
            if args.len() > 2 {
                return Err(anyhow!(
                    "/agent {first} accepts at most one agent id; usage: /agent {first} [agent-id]"
                ));
            }
            let action = match first.as_str() {
                "start" => crate::types::ControlAction::Start,
                "stop" => crate::types::ControlAction::Stop,
                _ => unreachable!("matched lifecycle action"),
            };
            Ok(AgentSlashAction::Control {
                action,
                agent_id: args.get(1).cloned(),
            })
        }
        "switch" => {
            if args.len() != 2 {
                return Err(anyhow!(
                    "/agent switch expects exactly one agent id; usage: /agent switch <agent-id>"
                ));
            }
            Ok(AgentSlashAction::Switch(args[1].clone()))
        }
        "create" => {
            if args.len() != 2 {
                return Err(anyhow!(
                    "/agent create expects exactly one agent name; usage: /agent create <name>"
                ));
            }
            Ok(AgentSlashAction::Create(args[1].clone()))
        }
        "pause" => Err(anyhow!(
            "/agent pause has been removed; use /agent stop [agent-id] instead"
        )),
        "resume" => Err(anyhow!(
            "/agent resume has been removed; use /agent start [agent-id] instead"
        )),
        _ => Err(anyhow!(
            "unknown /agent subcommand '{first}'; use /agent switch {first} to switch agents"
        )),
    }
}

pub(super) fn slash_menu_specs(buffer: &str) -> Vec<SlashCommandSpec> {
    let trimmed = buffer.trim_start();
    if !trimmed.starts_with('/') || trimmed.starts_with("//") || buffer.contains('\n') {
        return Vec::new();
    }

    let token = trimmed.split_whitespace().next().unwrap_or("/");
    let query = token.trim_start_matches('/');
    SLASH_COMMAND_SPECS
        .iter()
        .filter(|spec| query.is_empty() || spec.name[1..].starts_with(query))
        .copied()
        .collect()
}

fn parse_composer_submission(buffer: &str) -> Result<Option<ComposerSubmission>> {
    let text = buffer.trim().to_string();
    if text.is_empty() {
        return Ok(None);
    }
    if let Some(escaped) = text.strip_prefix("//") {
        return Ok(Some(ComposerSubmission::Chat(format!("/{escaped}"))));
    }
    if !text.starts_with('/') {
        return Ok(Some(ComposerSubmission::Chat(text)));
    }
    if text == "/" {
        return Err(anyhow!("empty slash command; use /help"));
    }

    let mut parts = text.split_whitespace();
    let command = parts
        .next()
        .expect("non-empty slash command must have a token");
    let Some(slash_command_spec) = slash_command_spec(command) else {
        return Ok(Some(ComposerSubmission::Chat(text)));
    };
    if text.contains('\n') {
        return Err(anyhow!("slash commands must be submitted on a single line"));
    }
    let args: Vec<String> = parts.map(ToString::to_string).collect();

    match slash_command_spec.arg_rule {
        SlashArgRule::None if !args.is_empty() => {
            return Err(slash_command_argument_error(slash_command_spec, args.len()));
        }
        SlashArgRule::ExactlyOne if args.len() != 1 => {
            return Err(slash_command_argument_error(slash_command_spec, args.len()));
        }
        SlashArgRule::Agent => {
            parse_agent_slash_action(&args)?;
        }
        SlashArgRule::None | SlashArgRule::ExactlyOne => {}
    }

    Ok(Some(ComposerSubmission::Slash(
        slash_command_spec.command,
        args,
    )))
}

#[cfg(test)]
fn slash_prompt_lines(buffer: &str) -> Option<Vec<String>> {
    if buffer.trim_start().starts_with("//") || buffer.contains('\n') {
        return None;
    }

    let token = buffer.trim_start().split_whitespace().next().unwrap_or("/");
    let matches = slash_menu_specs(buffer);

    if matches.is_empty() {
        return None;
    }

    let best = matches
        .iter()
        .find(|spec| spec.name == token)
        .copied()
        .unwrap_or(matches[0]);
    let preview = matches
        .iter()
        .take(4)
        .map(|spec| {
            if spec.name == best.name {
                format!(">{}", spec.name)
            } else {
                spec.name.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("  ");
    let overflow = matches.len().saturating_sub(4);
    let overflow_suffix = if overflow > 0 {
        format!("  +{overflow} more")
    } else {
        String::new()
    };

    Some(vec![
        format!("Slash: {preview}{overflow_suffix}"),
        format!("Best: {} {}", best.name, best.description),
    ])
}

fn paste_inline_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, '\r' | '\n'))
        .collect()
}

fn paste_single_line_text(text: &str) -> String {
    text.chars()
        .map(|ch| if matches!(ch, '\r' | '\n') { ' ' } else { ch })
        .collect()
}

impl TuiApp {
    fn should_treat_enter_as_paste_newline(&self, key: KeyEvent) -> bool {
        should_treat_enter_as_paste_newline_state(
            self.composer.as_str(),
            key,
            self.composer_key_burst_len,
            self.composer_key_burst_last_at,
        )
    }

    fn record_composer_key_edit(&mut self, action: TuiKeyAction) {
        const PASTE_BURST_INTER_KEY_WINDOW: Duration = Duration::from_millis(20);

        if !matches!(action, TuiKeyAction::InsertChar(_)) {
            self.reset_composer_key_burst();
            return;
        }

        let now = Instant::now();
        let continues_burst = self
            .composer_key_burst_last_at
            .is_some_and(|last_at| now.duration_since(last_at) <= PASTE_BURST_INTER_KEY_WINDOW);
        if continues_burst {
            self.composer_key_burst_len = self.composer_key_burst_len.saturating_add(1);
            self.composer_key_burst_last_at = Some(now);
        } else {
            self.composer_key_burst_started_at = Some(now);
            self.composer_key_burst_last_at = Some(now);
            self.composer_key_burst_len = 1;
        }
    }

    fn reset_composer_key_burst(&mut self) {
        self.composer_key_burst_started_at = None;
        self.composer_key_burst_last_at = None;
        self.composer_key_burst_len = 0;
    }

    pub(super) async fn handle_paste(&mut self, text: &str) -> Result<()> {
        let selected_agent = self.selected_agent_summary().cloned();
        let model_availability = self.model_availability.clone();
        match &mut self.overlay {
            OverlayState::None => {
                let before = self.composer.as_str().to_string();
                self.composer.insert_str(text);
                self.reset_composer_key_burst();
                self.sync_slash_menu_after_edit(before != self.composer.as_str());
            }
            OverlayState::ModelPicker { filter, selected } => {
                filter.push_str(&paste_inline_text(text));
                *selected = crate::tui::model_picker::clamp_model_picker_selection(
                    selected_agent.as_ref(),
                    &model_availability,
                    filter,
                    *selected,
                );
            }
            OverlayState::DebugPromptInput { composer } => {
                composer.insert_str(&paste_single_line_text(text));
            }
            _ => {}
        }
        Ok(())
    }

    async fn submit_prompt_buffer(&mut self) -> Result<()> {
        match parse_composer_submission(self.composer.as_str())? {
            None => Ok(()),
            Some(ComposerSubmission::Chat(text)) => {
                // Save to input history before sending
                if !text.is_empty() {
                    // Add to history, trimming if we exceed the cap
                    self.input_history.push(text.clone());
                    if self.input_history.len() > MAX_INPUT_HISTORY {
                        // Remove oldest entries (from the front)
                        let excess = self.input_history.len() - MAX_INPUT_HISTORY;
                        for _ in 0..excess {
                            self.input_history.remove(0);
                        }
                    }
                    self.history_index = None;
                }
                let agent_id = self
                    .selected_agent_id()
                    .ok_or_else(|| anyhow!("no agent selected"))?
                    .to_string();
                let local_message_id =
                    self.add_optimistic_operator_message(agent_id.clone(), text.clone());
                match self.client.control_prompt(&agent_id, text).await {
                    Ok(response) => self.reconcile_optimistic_operator_message(
                        &local_message_id,
                        &response.message_id,
                    ),
                    Err(err) => {
                        self.fail_optimistic_operator_message(&local_message_id, err.to_string());
                        return Err(err);
                    }
                }
                self.composer.clear();
                self.chat_scroll.follow_tail();
                self.status_line.clear();
                Ok(())
            }
            Some(ComposerSubmission::Slash(command, args)) => {
                self.execute_slash_command(command, args).await?;
                self.composer.clear();
                Ok(())
            }
        }
    }

    pub(super) fn navigate_history(&mut self, direction: i32) {
        if self.input_history.is_empty() {
            return;
        }

        let current_index = match self.history_index {
            None => {
                if direction < 0 {
                    Some(self.input_history.len().saturating_sub(1))
                } else {
                    // Can't go forward from the beginning
                    return;
                }
            }
            Some(idx) => {
                let new_idx = if direction < 0 {
                    idx.saturating_sub(1)
                } else {
                    (idx + 1).min(self.input_history.len())
                };
                Some(new_idx)
            }
        };

        match current_index {
            Some(idx) if idx == self.input_history.len() => {
                // Past the end - clear the composer
                self.composer.clear();
                self.history_index = None;
            }
            Some(idx) => {
                self.composer = ComposerState::from(self.input_history[idx].clone());
                self.history_index = Some(idx);
            }
            None => {}
        }
    }

    async fn execute_slash_command(
        &mut self,
        command: SlashCommand,
        args: Vec<String>,
    ) -> Result<()> {
        match command {
            SlashCommand::Help => {
                self.overlay = OverlayState::HelpView { scroll: 0 };
                self.status_line = "Opened slash command help".into();
            }
            SlashCommand::Agents => {
                self.overlay = OverlayState::Agents {
                    selected: self.selected_agent,
                };
                self.status_line = "Opened agents overlay".into();
            }
            SlashCommand::Events => {
                self.overlay = OverlayState::Events {
                    selected_event_id: self.event_id_for_reverse_index(0),
                    detail_scroll: 0,
                };
                self.status_line = "Opened raw events overlay".into();
            }
            SlashCommand::Model => {
                self.begin_load_models();
                self.overlay = OverlayState::ModelPicker {
                    filter: String::new(),
                    selected: 0,
                };
                self.status_line = "Opened model picker".into();
            }
            SlashCommand::Tasks => {
                self.overlay = OverlayState::Tasks {
                    selected: 0,
                    detail_scroll: 0,
                };
                self.status_line = "Opened tasks overlay".into();
            }
            SlashCommand::Transcript => {
                self.overlay = OverlayState::Transcript { scroll: 0 };
                self.status_line = "Opened conversation events overlay".into();
            }
            SlashCommand::State => {
                self.overlay = OverlayState::AgentState { scroll: 0 };
                self.status_line = "Opened agent state overlay".into();
            }
            SlashCommand::Refresh => {
                self.overlay = OverlayState::None;
                self.begin_bootstrap_selected_agent();
            }
            SlashCommand::ClearStatus => {
                self.overlay = OverlayState::None;
                self.status_line.clear();
            }
            SlashCommand::DebugPrompt => {
                self.overlay = OverlayState::DebugPromptInput {
                    composer: ComposerState::new(),
                };
                self.status_line = "Opened debug prompt dialog".into();
            }
            SlashCommand::Display => {
                let level = args
                    .into_iter()
                    .next()
                    .expect("slash command /display requires one argument");
                let display_mode = OperatorDisplayMode::parse(&level)
                    .ok_or_else(|| anyhow!("/display expects info, verbose, debug, or 3, 4, 5"))?;
                self.display_mode = display_mode;
                if let Some(projection) = self.projection.as_mut() {
                    projection.clear_event_history();
                }
                self.chat_text_cache.borrow_mut().take();
                self.overlay = OverlayState::None;
                self.status_line = format!(
                    "Display mode set to {} ({})",
                    display_mode.name(),
                    display_mode.display_level()
                );
                if self.selected_agent_id().is_some() {
                    self.begin_bootstrap_selected_agent();
                }
            }
            SlashCommand::Abort => {
                let agent_id = match self.selected_agent_id() {
                    Some(id) => id.to_string(),
                    None => {
                        self.status_line = "No agent selected".into();
                        return Ok(());
                    }
                };
                let run_id = self
                    .projection
                    .as_ref()
                    .and_then(|projection| projection.session.current_run_id.clone());
                self.client.abort_current_run(&agent_id, run_id).await?;
                self.overlay = OverlayState::None;
                self.status_line = format!("Aborted current run for {agent_id}");
                self.begin_bootstrap_selected_agent();
            }
            SlashCommand::Agent => match parse_agent_slash_action(&args)? {
                AgentSlashAction::Switch(requested_agent_id) => {
                    let target_index = self
                            .agents
                            .iter()
                            .position(|agent| agent.identity.agent_id == requested_agent_id)
                            .ok_or_else(|| {
                                anyhow!(
                                    "unknown agent '{requested_agent_id}'; use /agents to inspect valid ids"
                                )
                            })?;
                    self.overlay = OverlayState::None;
                    self.begin_bootstrap_agent_index(target_index);
                    self.status_line = format!("Switching to agent {requested_agent_id}");
                }
                AgentSlashAction::Create(agent_id) => {
                    self.client.create_agent(&agent_id).await?;
                    self.overlay = OverlayState::None;
                    self.status_line = format!("Created agent {agent_id}");
                    self.schedule_agent_list_refresh();
                }
                AgentSlashAction::Control { action, agent_id } => {
                    let agent_id = agent_id
                        .or_else(|| self.selected_agent_id().map(ToString::to_string))
                        .ok_or_else(|| anyhow!("no agent selected"))?;
                    self.client.control_agent(&agent_id, action.clone()).await?;
                    self.overlay = OverlayState::None;
                    self.status_line = format!(
                        "{} agent {agent_id}",
                        match action {
                            crate::types::ControlAction::Start => "Started",
                            crate::types::ControlAction::Stop => "Stopped",
                        }
                    );
                    if self.selected_agent_id() == Some(agent_id.as_str()) {
                        self.begin_bootstrap_selected_agent();
                    } else {
                        self.schedule_agent_list_refresh();
                    }
                }
            },
            SlashCommand::Skills => {
                let agent_id = match self.selected_agent_id() {
                    Some(id) => id.to_string(),
                    None => {
                        self.status_line = "No agent selected".into();
                        return Ok(());
                    }
                };
                let response = self.client.list_skills(&agent_id).await?;
                if let Some(skills) = response.get("skills").and_then(|s| s.as_array()) {
                    if skills.is_empty() {
                        self.status_line = "No skills installed".into();
                    } else {
                        let names: Vec<String> = skills
                            .iter()
                            .filter_map(|s| {
                                s.get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|n| n.to_string())
                            })
                            .collect();
                        self.status_line = format!("Skills: {}", names.join(", "));
                    }
                } else {
                    self.status_line = "Failed to list skills".into();
                }
            }
            SlashCommand::SkillInstall => {
                let skill_name = args
                    .into_iter()
                    .next()
                    .expect("slash command /skill-install requires one argument");
                let agent_id = match self.selected_agent_id() {
                    Some(id) => id.to_string(),
                    None => {
                        self.status_line = "No agent selected".into();
                        return Ok(());
                    }
                };
                let kind = crate::types::SkillInstallKind::Named {
                    name: skill_name.clone(),
                    mode: crate::types::SkillInstallMode::Linked,
                };
                match self.client.install_skill(&agent_id, kind).await {
                    Ok(_) => self.status_line = format!("Installed skill: {skill_name}"),
                    Err(error) => {
                        if error
                            .downcast_ref::<crate::client::LocalHttpError>()
                            .is_some_and(|error| error.has_code("skill_already_installed"))
                        {
                            self.status_line =
                                format!("Skill already installed: {skill_name}; uninstall first");
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
            SlashCommand::SkillUninstall => {
                let skill_name = args
                    .into_iter()
                    .next()
                    .expect("slash command /skill-uninstall requires one argument");
                let agent_id = match self.selected_agent_id() {
                    Some(id) => id.to_string(),
                    None => {
                        self.status_line = "No agent selected".into();
                        return Ok(());
                    }
                };
                self.client.uninstall_skill(&agent_id, &skill_name).await?;
                self.status_line = format!("Uninstalled skill: {skill_name}");
            }
        }
        Ok(())
    }

    pub(super) async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if resolve_key(KeyContext::Global, key) == TuiKeyAction::Quit {
            self.should_quit = true;
            return Ok(());
        }

        let overlay = std::mem::replace(&mut self.overlay, OverlayState::None);
        match overlay {
            OverlayState::None => self.handle_main_key(key).await,
            OverlayState::Agents { selected } => {
                self.handle_agents_overlay_key(key, selected).await
            }
            OverlayState::Events {
                selected_event_id,
                mut detail_scroll,
            } => {
                let current_index = self
                    .selected_event_reverse_index(selected_event_id.as_deref())
                    .unwrap_or(0);
                match resolve_key(KeyContext::EventsOverlay, key) {
                    TuiKeyAction::OverlayClose => {}
                    TuiKeyAction::OverlayMoveUp => {
                        let next_index = current_index.saturating_sub(1);
                        self.overlay = OverlayState::Events {
                            selected_event_id: self.event_id_for_reverse_index(next_index),
                            detail_scroll: 0,
                        };
                    }
                    TuiKeyAction::OverlayMoveDown => {
                        let max = self
                            .projection
                            .as_ref()
                            .map(|projection| projection.event_log().len().saturating_sub(1))
                            .unwrap_or(0);
                        let next_index = (current_index + 1).min(max);
                        self.overlay = OverlayState::Events {
                            selected_event_id: self.event_id_for_reverse_index(next_index),
                            detail_scroll: 0,
                        };
                    }
                    TuiKeyAction::OverlayScroll(action) => {
                        detail_scroll = adjust_scroll_for_action(detail_scroll, action);
                        self.overlay = OverlayState::Events {
                            selected_event_id,
                            detail_scroll,
                        };
                    }
                    _ => {
                        self.overlay = OverlayState::Events {
                            selected_event_id,
                            detail_scroll,
                        };
                    }
                }
                Ok(())
            }
            OverlayState::Transcript { mut scroll } => {
                match resolve_key(KeyContext::ScrollOverlay, key) {
                    TuiKeyAction::OverlayClose => {}
                    TuiKeyAction::OverlayScroll(action) => {
                        scroll = adjust_scroll_for_action(scroll, action);
                        self.overlay = OverlayState::Transcript { scroll };
                    }
                    _ => self.overlay = OverlayState::Transcript { scroll },
                }
                Ok(())
            }
            OverlayState::AgentState { mut scroll } => {
                match resolve_key(KeyContext::ScrollOverlay, key) {
                    TuiKeyAction::OverlayClose => {}
                    TuiKeyAction::OverlayScroll(action) => {
                        scroll = adjust_scroll_for_action(scroll, action);
                        self.overlay = OverlayState::AgentState { scroll };
                    }
                    _ => self.overlay = OverlayState::AgentState { scroll },
                }
                Ok(())
            }
            OverlayState::Tasks {
                selected,
                detail_scroll,
            } => {
                let mut selected = selected;
                let mut detail_scroll = detail_scroll;
                match resolve_key(KeyContext::TasksOverlay, key) {
                    TuiKeyAction::OverlayClose => {}
                    TuiKeyAction::Task(action) => {
                        self.handle_task_overlay_action(selected, detail_scroll, action)
                            .await?;
                    }
                    TuiKeyAction::OverlayMoveUp => {
                        selected = selected.saturating_sub(1);
                        self.overlay = OverlayState::Tasks {
                            selected,
                            detail_scroll: 0,
                        };
                    }
                    TuiKeyAction::OverlayMoveDown => {
                        let max = self.tasks.len().saturating_sub(1);
                        selected = (selected + 1).min(max);
                        self.overlay = OverlayState::Tasks {
                            selected,
                            detail_scroll: 0,
                        };
                    }
                    TuiKeyAction::OverlayScroll(action) => {
                        detail_scroll = adjust_scroll_for_action(detail_scroll, action);
                        self.overlay = OverlayState::Tasks {
                            selected,
                            detail_scroll,
                        };
                    }
                    _ => {
                        self.overlay = OverlayState::Tasks {
                            selected,
                            detail_scroll,
                        };
                    }
                }
                Ok(())
            }
            OverlayState::ModelPicker {
                mut filter,
                mut selected,
            } => {
                match resolve_key(KeyContext::ModelPicker, key) {
                    TuiKeyAction::OverlayClose => {}
                    TuiKeyAction::OverlayAccept => {
                        self.apply_model_picker_selection(&filter, selected).await?;
                    }
                    TuiKeyAction::OverlayMoveUp => {
                        selected = selected.saturating_sub(1);
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    TuiKeyAction::OverlayMoveDown => {
                        let max = crate::tui::model_picker::model_picker_rows(
                            self.selected_agent_summary(),
                            &self.model_availability,
                            &filter,
                        )
                        .len()
                        .saturating_sub(1);
                        selected = (selected + 1).min(max);
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    TuiKeyAction::ModelFilterBackspace => {
                        filter.pop();
                        selected = crate::tui::model_picker::clamp_model_picker_selection(
                            self.selected_agent_summary(),
                            &self.model_availability,
                            &filter,
                            selected,
                        );
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    TuiKeyAction::InsertChar(ch) => {
                        filter.push(ch);
                        selected = crate::tui::model_picker::clamp_model_picker_selection(
                            self.selected_agent_summary(),
                            &self.model_availability,
                            &filter,
                            selected,
                        );
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    _ => self.overlay = OverlayState::ModelPicker { filter, selected },
                }
                Ok(())
            }
            OverlayState::ModelEffortPicker {
                model,
                mut selected,
                return_filter,
                return_selected,
            } => {
                match resolve_key(KeyContext::ModelEffortPicker, key) {
                    TuiKeyAction::OverlayClose => {
                        self.overlay = OverlayState::ModelPicker {
                            filter: return_filter,
                            selected: return_selected,
                        };
                    }
                    TuiKeyAction::OverlayAccept => {
                        self.apply_model_effort_picker_selection(&model, selected)
                            .await?;
                    }
                    TuiKeyAction::OverlayMoveUp => {
                        selected = selected.saturating_sub(1);
                        self.overlay = OverlayState::ModelEffortPicker {
                            model,
                            selected,
                            return_filter,
                            return_selected,
                        };
                    }
                    TuiKeyAction::OverlayMoveDown => {
                        let max = crate::tui::overlay::MODEL_REASONING_EFFORT_OPTIONS.len() - 1;
                        selected = (selected + 1).min(max);
                        self.overlay = OverlayState::ModelEffortPicker {
                            model,
                            selected,
                            return_filter,
                            return_selected,
                        };
                    }
                    _ => {
                        self.overlay = OverlayState::ModelEffortPicker {
                            model,
                            selected,
                            return_filter,
                            return_selected,
                        }
                    }
                }
                Ok(())
            }
            OverlayState::DebugPromptInput { mut composer } => {
                match apply_composer_key_action(
                    resolve_key(KeyContext::DebugPromptInput, key),
                    &mut composer,
                ) {
                    Some(BufferAction::Submit) => {
                        let agent_id = self
                            .selected_agent_id()
                            .ok_or_else(|| anyhow!("no agent selected"))?
                            .to_string();
                        let dump = self
                            .client
                            .debug_prompt(
                                &agent_id,
                                composer.as_str().to_string(),
                                AuthorityClass::OperatorInstruction,
                            )
                            .await?;
                        self.overlay = OverlayState::DebugPromptView {
                            title: format!("Debug Prompt: {}", agent_id),
                            dump,
                            scroll: 0,
                        };
                        Ok(())
                    }
                    Some(BufferAction::Cancel) => Ok(()),
                    None => {
                        self.overlay = OverlayState::DebugPromptInput { composer };
                        Ok(())
                    }
                }
            }
            OverlayState::DebugPromptView {
                title,
                dump,
                mut scroll,
            } => {
                match resolve_key(KeyContext::ScrollOverlay, key) {
                    TuiKeyAction::OverlayClose => {}
                    TuiKeyAction::OverlayScroll(action) => {
                        scroll = adjust_scroll_for_action(scroll, action);
                        self.overlay = OverlayState::DebugPromptView {
                            title,
                            dump,
                            scroll,
                        };
                    }
                    _ => {
                        self.overlay = OverlayState::DebugPromptView {
                            title,
                            dump,
                            scroll,
                        }
                    }
                }
                Ok(())
            }
            OverlayState::HelpView { mut scroll } => {
                match resolve_key(KeyContext::ScrollOverlay, key) {
                    TuiKeyAction::OverlayClose => {}
                    TuiKeyAction::OverlayScroll(action) => {
                        scroll = adjust_scroll_for_action(scroll, action);
                        self.overlay = OverlayState::HelpView { scroll };
                    }
                    _ => self.overlay = OverlayState::HelpView { scroll },
                }
                Ok(())
            }
        }
    }

    async fn handle_main_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.handle_slash_menu_key(key).await? {
            return Ok(());
        }

        match resolve_key(KeyContext::Main, key) {
            TuiKeyAction::OpenHelp if self.composer.is_empty() => {
                self.overlay = OverlayState::HelpView { scroll: 0 };
            }
            TuiKeyAction::OpenHelp => {
                let before = self.composer.as_str().to_string();
                self.composer.insert_char('?');
                self.sync_slash_menu_after_edit(before != self.composer.as_str());
            }
            TuiKeyAction::HistoryPrevious
                if self.history_index.is_some() || self.composer.is_empty() =>
            {
                self.navigate_history(-1);
            }
            TuiKeyAction::HistoryNext
                if self.history_index.is_some() || self.composer.is_empty() =>
            {
                self.navigate_history(1);
            }
            TuiKeyAction::ChatScroll(action) => {
                self.chat_scroll
                    .scroll_with_key(scroll_action_key_code(action), self.chat_max_scroll);
                self.maybe_begin_load_older_events();
            }
            TuiKeyAction::HistoryPrevious | TuiKeyAction::HistoryNext => {
                let action = match key.code {
                    KeyCode::Up => TuiKeyAction::Composer(ComposerAction::MoveUp),
                    KeyCode::Down => TuiKeyAction::Composer(ComposerAction::MoveDown),
                    _ => TuiKeyAction::Ignore,
                };
                apply_composer_key_action(action, &mut self.composer);
            }
            action => {
                let before = self.composer.as_str().to_string();
                let action = if matches!(action, TuiKeyAction::Composer(ComposerAction::Submit))
                    && self.should_treat_enter_as_paste_newline(key)
                {
                    TuiKeyAction::Composer(ComposerAction::InsertNewline)
                } else {
                    action
                };
                match apply_composer_key_action(action, &mut self.composer) {
                    Some(BufferAction::Submit) => {
                        self.reset_composer_key_burst();
                        self.submit_prompt_buffer().await?;
                    }
                    Some(BufferAction::Cancel) => {
                        self.composer.clear();
                        self.reset_composer_key_burst();
                        self.history_index = None;
                        self.slash_menu_selected = 0;
                        self.slash_menu_dismissed_for = None;
                    }
                    None => {
                        let changed = before != self.composer.as_str();
                        if changed {
                            self.record_composer_key_edit(action);
                        }
                        self.sync_slash_menu_after_edit(changed);
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_slash_menu_key(&mut self, key: KeyEvent) -> Result<bool> {
        if !self.is_slash_menu_visible() {
            return Ok(false);
        }
        if self.should_treat_enter_as_paste_newline(key) {
            return Ok(false);
        }

        let specs = self.active_slash_menu_specs();

        match resolve_key(KeyContext::SlashMenu, key) {
            TuiKeyAction::Ignore => Ok(false),
            TuiKeyAction::SlashMenu(SlashMenuAction::Dismiss) => {
                self.slash_menu_dismissed_for = Some(self.composer.as_str().to_string());
                self.slash_menu_selected = 0;
                Ok(true)
            }
            _ if specs.is_empty() => Ok(false),
            TuiKeyAction::SlashMenu(SlashMenuAction::Previous) => {
                self.slash_menu_selected = self.slash_menu_selected.saturating_sub(1);
                Ok(true)
            }
            TuiKeyAction::SlashMenu(SlashMenuAction::Next) => {
                self.slash_menu_selected =
                    (self.slash_menu_selected + 1).min(specs.len().saturating_sub(1));
                Ok(true)
            }
            TuiKeyAction::SlashMenu(SlashMenuAction::Complete) => {
                let selected = self.slash_menu_selected.min(specs.len().saturating_sub(1));
                self.composer = ComposerState::from(slash_menu_enter_submission(
                    self.composer.as_str(),
                    specs[selected],
                ));
                self.slash_menu_selected = selected;
                self.slash_menu_dismissed_for = None;
                Ok(true)
            }
            TuiKeyAction::SlashMenu(SlashMenuAction::Submit) => {
                let selected = self.slash_menu_selected.min(specs.len().saturating_sub(1));
                let selection =
                    slash_menu_enter_submission(self.composer.as_str(), specs[selected]);
                let selection = parse_composer_submission(&selection)?;
                match selection {
                    Some(ComposerSubmission::Slash(command, args)) => {
                        self.execute_slash_command(command, args).await?
                    }
                    Some(ComposerSubmission::Chat(_)) => {}
                    None => {}
                }
                self.composer.clear();
                self.slash_menu_selected = 0;
                self.slash_menu_dismissed_for = None;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn is_slash_menu_visible(&self) -> bool {
        if self.overlay != OverlayState::None {
            return false;
        }
        if self
            .slash_menu_dismissed_for
            .as_deref()
            .is_some_and(|dismissed| dismissed == self.composer.as_str())
        {
            return false;
        }

        let buffer = self.composer.as_str();
        let slash_like = !buffer.contains('\n')
            && buffer.trim_start().starts_with('/')
            && !buffer.trim_start().starts_with("//");
        slash_like && !slash_menu_specs(buffer).is_empty()
    }

    fn active_slash_menu_specs(&self) -> Vec<SlashCommandSpec> {
        if !self.is_slash_menu_visible() {
            return Vec::new();
        }
        slash_menu_specs(self.composer.as_str())
    }

    fn sync_slash_menu_after_edit(&mut self, buffer_changed: bool) {
        if buffer_changed {
            self.slash_menu_dismissed_for = None;
        }
        let len = slash_menu_specs(self.composer.as_str()).len();
        if len == 0 {
            self.slash_menu_selected = 0;
        } else {
            self.slash_menu_selected = self.slash_menu_selected.min(len - 1);
        }
    }

    async fn handle_agents_overlay_key(&mut self, key: KeyEvent, selected: usize) -> Result<()> {
        match resolve_key(KeyContext::AgentsOverlay, key) {
            TuiKeyAction::OverlayClose => Ok(()),
            TuiKeyAction::OverlayAccept => {
                let selected = selected.min(self.agents.len().saturating_sub(1));
                if let Some(agent_id) = self
                    .agents
                    .get(selected)
                    .map(|agent| agent.identity.agent_id.clone())
                {
                    self.status_line = format!("Switching to agent {agent_id}");
                    self.begin_bootstrap_agent_index(selected);
                }
                self.overlay = OverlayState::None;
                Ok(())
            }
            TuiKeyAction::OverlayMoveUp => {
                let selected = self.next_agent_index_from(selected, -1).unwrap_or(selected);
                self.overlay = OverlayState::Agents { selected };
                Ok(())
            }
            TuiKeyAction::OverlayMoveDown => {
                let selected = self.next_agent_index_from(selected, 1).unwrap_or(selected);
                self.overlay = OverlayState::Agents { selected };
                Ok(())
            }
            _ => {
                self.overlay = OverlayState::Agents { selected };
                Ok(())
            }
        }
    }

    async fn apply_model_picker_selection(&mut self, filter: &str, selected: usize) -> Result<()> {
        let agent_id = self
            .selected_agent_id()
            .ok_or_else(|| anyhow!("no agent selected"))?
            .to_string();
        let choice = crate::tui::model_picker::selected_model_choice(
            self.selected_agent_summary(),
            &self.model_availability,
            filter,
            selected,
        )
        .ok_or_else(|| anyhow!("no model selection available"))?;

        match choice {
            crate::tui::model_picker::ModelPickerChoice::InheritDefault => {
                self.client.clear_agent_model_override(&agent_id).await?;
                self.status_line =
                    format!("Cleared model override for {agent_id}; inheriting runtime default");
                self.overlay = OverlayState::None;
                self.begin_bootstrap_selected_agent();
            }
            crate::tui::model_picker::ModelPickerChoice::Model { model } => {
                self.overlay = OverlayState::ModelEffortPicker {
                    model,
                    selected: 0,
                    return_filter: filter.to_string(),
                    return_selected: selected,
                };
            }
        }
        Ok(())
    }

    async fn apply_model_effort_picker_selection(
        &mut self,
        model: &str,
        selected: usize,
    ) -> Result<()> {
        let agent_id = self
            .selected_agent_id()
            .ok_or_else(|| anyhow!("no agent selected"))?
            .to_string();
        let reasoning_effort = crate::tui::overlay::MODEL_REASONING_EFFORT_OPTIONS
            .get(selected)
            .copied()
            .unwrap_or("xhigh");
        let reasoning_effort = if reasoning_effort == "inherit" {
            None
        } else {
            Some(reasoning_effort.to_string())
        };
        self.client
            .set_agent_model_override(&agent_id, model.to_string(), reasoning_effort.clone())
            .await?;
        let suffix = reasoning_effort
            .as_deref()
            .map(|value| format!(" with reasoning effort {value}"))
            .unwrap_or_default();
        self.status_line = format!("Set model override for {agent_id} to {model}{suffix}");
        self.overlay = OverlayState::None;
        self.begin_bootstrap_selected_agent();
        Ok(())
    }

    fn selected_event_reverse_index(&self, selected_event_id: Option<&str>) -> Option<usize> {
        let projection = self.projection.as_ref()?;
        selected_event_id
            .and_then(|event_id| {
                projection
                    .event_log()
                    .iter()
                    .rev()
                    .position(|event| event.id == event_id)
            })
            .or_else(|| (!projection.event_log().is_empty()).then_some(0))
    }

    fn event_id_for_reverse_index(&self, index: usize) -> Option<String> {
        self.projection
            .as_ref()
            .and_then(|projection| projection.event_log().iter().rev().nth(index))
            .map(|event| event.id.clone())
    }
}

fn should_treat_enter_as_paste_newline_state(
    composer_text: &str,
    key: KeyEvent,
    composer_key_burst_len: usize,
    composer_key_burst_last_at: Option<Instant>,
) -> bool {
    const PASTE_BURST_ENTER_WINDOW: Duration = Duration::from_millis(30);

    let trimmed = composer_text.trim_start();
    if key.code != KeyCode::Enter
        || !key.modifiers.is_empty()
        || trimmed.is_empty()
        || (trimmed.starts_with('/') && is_complete_slash_command_token(trimmed))
    {
        return false;
    }
    composer_key_burst_len > 0
        && composer_key_burst_last_at
            .is_some_and(|last_at| last_at.elapsed() <= PASTE_BURST_ENTER_WINDOW)
}

fn is_complete_slash_command_token(trimmed: &str) -> bool {
    let Some(token) = trimmed.split_whitespace().next() else {
        return false;
    };
    slash_command_spec(token).is_some()
}

fn slash_menu_enter_submission(buffer: &str, selected: SlashCommandSpec) -> String {
    let trimmed = buffer.trim();
    let Some((token, rest)) = trimmed.split_once(char::is_whitespace) else {
        return selected.name.to_string();
    };

    if token == selected.name || slash_command_spec(token).is_none() {
        let rest = rest.trim();
        if rest.is_empty() {
            selected.name.to_string()
        } else {
            format!("{} {rest}", selected.name)
        }
    } else {
        trimmed.to_string()
    }
}

enum BufferAction {
    Submit,
    Cancel,
}

fn apply_composer_key_action(
    action: TuiKeyAction,
    composer: &mut ComposerState,
) -> Option<BufferAction> {
    match action {
        TuiKeyAction::Composer(ComposerAction::Submit) => {
            if !composer.as_str().trim().is_empty() {
                return Some(BufferAction::Submit);
            }
        }
        TuiKeyAction::Composer(ComposerAction::Cancel) => return Some(BufferAction::Cancel),
        TuiKeyAction::Composer(ComposerAction::InsertNewline) => composer.insert_newline(),
        TuiKeyAction::Composer(ComposerAction::Backspace) => composer.backspace(),
        TuiKeyAction::Composer(ComposerAction::Delete) => composer.delete(),
        TuiKeyAction::Composer(ComposerAction::MoveLeft) => composer.move_left(),
        TuiKeyAction::Composer(ComposerAction::MoveRight) => composer.move_right(),
        TuiKeyAction::Composer(ComposerAction::MoveUp) => composer.move_up(),
        TuiKeyAction::Composer(ComposerAction::MoveDown) => composer.move_down(),
        TuiKeyAction::Composer(ComposerAction::MoveHome) => composer.move_home(),
        TuiKeyAction::Composer(ComposerAction::MoveEnd) => composer.move_end(),
        TuiKeyAction::Composer(ComposerAction::InsertTab) => composer.insert_char('\t'),
        TuiKeyAction::Composer(ComposerAction::MoveToStart) => composer.move_to_start(),
        TuiKeyAction::Composer(ComposerAction::MoveToEnd) => composer.move_to_end(),
        TuiKeyAction::Composer(ComposerAction::DeleteToEnd) => composer.delete_to_end(),
        TuiKeyAction::Composer(ComposerAction::DeleteToStart) => composer.delete_to_start(),
        TuiKeyAction::Composer(ComposerAction::DeleteWord) => composer.delete_word(),
        TuiKeyAction::InsertChar(ch) => composer.insert_char(ch),
        _ => {}
    }
    None
}

fn adjust_scroll(scroll: u16, delta: i16) -> u16 {
    if delta >= 0 {
        scroll.saturating_add(delta as u16)
    } else {
        scroll.saturating_sub(delta.unsigned_abs())
    }
}

fn adjust_scroll_for_action(scroll: u16, action: ScrollAction) -> u16 {
    match action {
        ScrollAction::Up => adjust_scroll(scroll, -1),
        ScrollAction::Down => adjust_scroll(scroll, 1),
        ScrollAction::PageUp => adjust_scroll(scroll, -10),
        ScrollAction::PageDown => adjust_scroll(scroll, 10),
        ScrollAction::Home => 0,
        ScrollAction::End => u16::MAX,
    }
}

fn scroll_action_key_code(action: ScrollAction) -> KeyCode {
    match action {
        ScrollAction::Up => KeyCode::Up,
        ScrollAction::Down => KeyCode::Down,
        ScrollAction::PageUp => KeyCode::PageUp,
        ScrollAction::PageDown => KeyCode::PageDown,
        ScrollAction::Home => KeyCode::Home,
        ScrollAction::End => KeyCode::End,
    }
}

impl TuiApp {
    async fn handle_task_overlay_action(
        &mut self,
        selected: usize,
        detail_scroll: u16,
        action: render::TaskOverlayAction,
    ) -> Result<()> {
        self.overlay = OverlayState::Tasks {
            selected,
            detail_scroll,
        };

        let Some(task) = self.tasks.iter().rev().nth(selected) else {
            self.status_line = "No task selected".into();
            return Ok(());
        };

        let availability = render::task_action_availability(task, action);
        if availability.enabled {
            self.status_line = format!(
                "{} entry point: {} is available for task {}",
                action.label(),
                action.tool_name(),
                task.id
            );
        } else {
            self.status_line = format!(
                "{} unavailable for task {}: {}",
                action.label(),
                task.id,
                availability.reason
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_agent_slash_action, parse_composer_submission,
        should_treat_enter_as_paste_newline_state, slash_command_spec, slash_help_lines,
        slash_menu_enter_submission, slash_menu_specs, slash_prompt_lines, AgentSlashAction,
        ComposerSubmission, SlashCommand,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::Instant;

    #[test]
    fn parses_plain_chat_submission() {
        assert_eq!(
            parse_composer_submission("hello world").unwrap(),
            Some(ComposerSubmission::Chat("hello world".into()))
        );
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
            parse_composer_submission("/display 4").unwrap(),
            Some(ComposerSubmission::Slash(
                SlashCommand::Display,
                vec!["4".into()]
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
    fn slash_display_requires_one_argument() {
        let err = parse_composer_submission("/display").unwrap_err();
        assert!(err.to_string().contains("requires one argument"));
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
            "/tasks",
            "/transcript",
            "/state",
            "/refresh",
            "/clear-status",
            "/debug-prompt",
            "/display <info|verbose|debug|3|4|5>",
            "/abort",
            "/agent switch <agent-id>|create <name>|start [agent-id]|stop [agent-id]",
            "/skills",
            "/skill-install <name>",
            "/skill-uninstall <name>",
        ] {
            assert!(help.contains(command), "help missing {command}");
        }
    }
}
