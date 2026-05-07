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
    Interrupt,
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
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SlashCommandSpec {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    usage: &'static str,
    arg_rule: SlashArgRule,
    command: SlashCommand,
}

const SLASH_COMMAND_SPECS: [SlashCommandSpec; 16] = [
    SlashCommandSpec {
        name: "/help",
        description: "show slash command help",
        usage: "/help",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Help,
    },
    SlashCommandSpec {
        name: "/agents",
        description: "open agent picker",
        usage: "/agents",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Agents,
    },
    SlashCommandSpec {
        name: "/events",
        description: "open raw events overlay",
        usage: "/events",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Events,
    },
    SlashCommandSpec {
        name: "/model",
        description: "open selected agent model picker",
        usage: "/model",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Model,
    },
    SlashCommandSpec {
        name: "/tasks",
        description: "open task overlay",
        usage: "/tasks",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Tasks,
    },
    SlashCommandSpec {
        name: "/transcript",
        description: "open transcript overlay",
        usage: "/transcript",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Transcript,
    },
    SlashCommandSpec {
        name: "/state",
        description: "open agent state overlay",
        usage: "/state",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::State,
    },
    SlashCommandSpec {
        name: "/refresh",
        description: "refresh selected agent",
        usage: "/refresh",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Refresh,
    },
    SlashCommandSpec {
        name: "/clear-status",
        description: "clear local status line",
        usage: "/clear-status",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::ClearStatus,
    },
    SlashCommandSpec {
        name: "/debug-prompt",
        description: "open debug prompt dialog",
        usage: "/debug-prompt",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::DebugPrompt,
    },
    SlashCommandSpec {
        name: "/display",
        description: "set chat display level",
        usage: "/display <3|4|5>",
        arg_rule: SlashArgRule::ExactlyOne,
        command: SlashCommand::Display,
    },
    SlashCommandSpec {
        name: "/interrupt",
        description: "interrupt current agent run",
        usage: "/interrupt",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Interrupt,
    },
    SlashCommandSpec {
        name: "/agent",
        description: "switch to agent by id",
        usage: "/agent <agent-id>",
        arg_rule: SlashArgRule::ExactlyOne,
        command: SlashCommand::Agent,
    },
    SlashCommandSpec {
        name: "/skills",
        description: "show installed skills",
        usage: "/skills",
        arg_rule: SlashArgRule::None,
        command: SlashCommand::Skills,
    },
    SlashCommandSpec {
        name: "/skill-install",
        description: "install a builtin skill",
        usage: "/skill-install <name>",
        arg_rule: SlashArgRule::ExactlyOne,
        command: SlashCommand::SkillInstall,
    },
    SlashCommandSpec {
        name: "/skill-uninstall",
        description: "uninstall a skill",
        usage: "/skill-uninstall <name>",
        arg_rule: SlashArgRule::ExactlyOne,
        command: SlashCommand::SkillUninstall,
    },
];

/// Maximum number of entries to keep in input history
const MAX_INPUT_HISTORY: usize = 100;

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
    if text.contains('\n') {
        return Err(anyhow!("slash commands must be submitted on a single line"));
    }

    let mut parts = text.split_whitespace();
    let command = parts
        .next()
        .expect("non-empty slash command must have a token");
    let args: Vec<String> = parts.map(ToString::to_string).collect();
    let slash_command_spec = slash_command_spec(command)
        .ok_or_else(|| anyhow!("unknown slash command {}; use /help", command))?;

    match slash_command_spec.arg_rule {
        SlashArgRule::None if !args.is_empty() => {
            return Err(slash_command_argument_error(slash_command_spec, args.len()));
        }
        SlashArgRule::ExactlyOne if args.len() != 1 => {
            return Err(slash_command_argument_error(slash_command_spec, args.len()));
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
        if !token.starts_with('/') {
            return None;
        }
        return Some(vec![format!(
            "Slash: no command matches {token}. Use /help for the full list."
        )]);
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
    pub(super) async fn handle_paste(&mut self, text: &str) -> Result<()> {
        let selected_agent = self.selected_agent_summary().cloned();
        match &mut self.overlay {
            OverlayState::None => {
                let before = self.composer.as_str().to_string();
                self.composer.insert_str(text);
                self.sync_slash_menu_after_edit(before != self.composer.as_str());
            }
            OverlayState::ModelPicker { filter, selected } => {
                filter.push_str(&paste_inline_text(text));
                *selected = crate::tui::model_picker::clamp_model_picker_selection(
                    selected_agent.as_ref(),
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

    pub(super) async fn move_agent_selection(&mut self, delta: i32) -> Result<()> {
        let Some(target_index) = self.next_agent_index(delta) else {
            return Ok(());
        };
        self.chat_scroll.follow_tail();
        self.bootstrap_agent_index(target_index).await
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
                self.overlay = OverlayState::Agents;
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
                self.status_line = "Opened transcript overlay".into();
            }
            SlashCommand::State => {
                self.overlay = OverlayState::AgentState { scroll: 0 };
                self.status_line = "Opened agent state overlay".into();
            }
            SlashCommand::Refresh => {
                self.overlay = OverlayState::None;
                self.status_line = "Refreshing selected agent from /state".into();
                self.bootstrap_selected_agent().await?;
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
                    .expect("slash command /display requires one argument")
                    .parse::<u8>()
                    .map_err(|_| anyhow!("/display expects 3, 4, or 5"))?;
                let display_level = OperatorVisibility::from_display_level(level)
                    .ok_or_else(|| anyhow!("/display expects 3, 4, or 5"))?;
                self.display_level = display_level;
                self.chat_text_cache.borrow_mut().take();
                self.overlay = OverlayState::None;
                self.status_line = format!("Display level set to {level}");
            }
            SlashCommand::Interrupt => {
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
                self.client.interrupt_current_run(&agent_id, run_id).await?;
                self.overlay = OverlayState::None;
                self.status_line = format!("Interrupted current run for {agent_id}");
                let _ = self.bootstrap_selected_agent().await;
            }
            SlashCommand::Agent => {
                let requested_agent_id = args
                    .into_iter()
                    .next()
                    .expect("slash command /agent requires one argument");
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
                self.bootstrap_agent_index(target_index).await?;
                self.status_line = format!("Switched to agent {requested_agent_id}");
            }
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
                let kind = crate::types::SkillInstallKind::Builtin {
                    name: skill_name.clone(),
                };
                self.client.install_skill(&agent_id, kind).await?;
                self.status_line = format!("Installed skill: {skill_name}");
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
            OverlayState::Agents => self.handle_agents_overlay_key(key).await,
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
                            &filter,
                            selected,
                        );
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    TuiKeyAction::InsertChar(ch) => {
                        filter.push(ch);
                        selected = crate::tui::model_picker::clamp_model_picker_selection(
                            self.selected_agent_summary(),
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
                                TrustLevel::TrustedOperator,
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
            }
            TuiKeyAction::HistoryPrevious | TuiKeyAction::HistoryNext => {
                self.chat_scroll
                    .scroll_with_key(key.code, self.chat_max_scroll);
            }
            action => {
                let before = self.composer.as_str().to_string();
                match apply_composer_key_action(action, &mut self.composer) {
                    Some(BufferAction::Submit) => self.submit_prompt_buffer().await?,
                    Some(BufferAction::Cancel) => {
                        self.composer.clear();
                        self.history_index = None;
                        self.slash_menu_selected = 0;
                        self.slash_menu_dismissed_for = None;
                    }
                    None => self.sync_slash_menu_after_edit(before != self.composer.as_str()),
                }
            }
        }

        Ok(())
    }

    async fn handle_slash_menu_key(&mut self, key: KeyEvent) -> Result<bool> {
        if !self.is_slash_menu_visible() {
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
                self.composer = ComposerState::from(specs[selected].name);
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
        !buffer.contains('\n')
            && buffer.trim_start().starts_with('/')
            && !buffer.trim_start().starts_with("//")
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

    async fn handle_agents_overlay_key(&mut self, key: KeyEvent) -> Result<()> {
        match resolve_key(KeyContext::AgentsOverlay, key) {
            TuiKeyAction::OverlayClose => Ok(()),
            TuiKeyAction::OverlayAccept => {
                let agent_id = self.selected_agent_id().unwrap_or("").to_string();
                match self.bootstrap_agent_index(self.selected_agent).await {
                    Ok(()) => {
                        self.overlay = OverlayState::None;
                        Ok(())
                    }
                    Err(err) => {
                        if !agent_id.is_empty() {
                            self.status_line =
                                format!("Failed to switch to agent {agent_id}: {err}");
                        }
                        self.overlay = OverlayState::Agents;
                        Err(err)
                    }
                }
            }
            TuiKeyAction::OverlayMoveUp => {
                self.move_agent_selection(-1).await?;
                self.overlay = OverlayState::Agents;
                Ok(())
            }
            TuiKeyAction::OverlayMoveDown => {
                self.move_agent_selection(1).await?;
                self.overlay = OverlayState::Agents;
                Ok(())
            }
            _ => {
                self.overlay = OverlayState::Agents;
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
                self.bootstrap_selected_agent().await?;
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
        self.bootstrap_selected_agent().await?;
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
        parse_composer_submission, slash_command_spec, slash_menu_enter_submission,
        slash_menu_specs, slash_prompt_lines, ComposerSubmission, SlashCommand,
    };

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
            parse_composer_submission("/interrupt").unwrap(),
            Some(ComposerSubmission::Slash(SlashCommand::Interrupt, vec![]))
        );
        assert_eq!(
            parse_composer_submission("/agent default").unwrap(),
            Some(ComposerSubmission::Slash(
                SlashCommand::Agent,
                vec!["default".into()]
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
        assert!(err.to_string().contains("requires one argument"));
    }

    #[test]
    fn slash_display_requires_one_argument() {
        let err = parse_composer_submission("/display").unwrap_err();
        assert!(err.to_string().contains("requires one argument"));
    }

    #[test]
    fn slash_commands_reject_too_many_arguments() {
        let err = parse_composer_submission("/agent default extra").unwrap_err();
        assert!(err.to_string().contains("expects exactly one argument"));
    }

    #[test]
    fn slash_commands_reject_unknown_names() {
        let err = parse_composer_submission("/unknown").unwrap_err();
        assert!(err.to_string().contains("unknown slash command"));
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
}
