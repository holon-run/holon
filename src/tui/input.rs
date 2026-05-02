use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashCommand {
    Help,
    Agents,
    Events,
    Model,
    Tasks,
    Transcript,
    Refresh,
    ClearStatus,
    DebugPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ComposerSubmission {
    Chat(String),
    Slash(SlashCommand),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SlashCommandSpec {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    command: SlashCommand,
}

const SLASH_COMMAND_SPECS: [SlashCommandSpec; 9] = [
    SlashCommandSpec {
        name: "/help",
        description: "show slash command help",
        command: SlashCommand::Help,
    },
    SlashCommandSpec {
        name: "/agents",
        description: "open agent picker",
        command: SlashCommand::Agents,
    },
    SlashCommandSpec {
        name: "/events",
        description: "open raw events overlay",
        command: SlashCommand::Events,
    },
    SlashCommandSpec {
        name: "/model",
        description: "open selected agent model picker",
        command: SlashCommand::Model,
    },
    SlashCommandSpec {
        name: "/tasks",
        description: "open task overlay",
        command: SlashCommand::Tasks,
    },
    SlashCommandSpec {
        name: "/transcript",
        description: "open transcript overlay",
        command: SlashCommand::Transcript,
    },
    SlashCommandSpec {
        name: "/refresh",
        description: "refresh selected agent",
        command: SlashCommand::Refresh,
    },
    SlashCommandSpec {
        name: "/clear-status",
        description: "clear local status line",
        command: SlashCommand::ClearStatus,
    },
    SlashCommandSpec {
        name: "/debug-prompt",
        description: "open debug prompt dialog",
        command: SlashCommand::DebugPrompt,
    },
];

fn slash_command_spec(command: &str) -> Option<SlashCommandSpec> {
    SLASH_COMMAND_SPECS
        .iter()
        .copied()
        .find(|spec| spec.name == command)
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
    let args = parts.collect::<Vec<_>>();

    let slash_command = match slash_command_spec(command) {
        Some(spec) if args.is_empty() => spec.command,
        Some(_) => {
            return Err(anyhow!("{} does not accept arguments", command));
        }
        None => return Err(anyhow!("unknown slash command {}; use /help", command)),
    };

    Ok(Some(ComposerSubmission::Slash(slash_command)))
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

impl TuiApp {
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
                let agent_id = self
                    .selected_agent_id()
                    .ok_or_else(|| anyhow!("no agent selected"))?
                    .to_string();
                self.client.control_prompt(&agent_id, text).await?;
                self.composer.clear();
                self.chat_scroll.follow_tail();
                self.status_line.clear();
                Ok(())
            }
            Some(ComposerSubmission::Slash(command)) => {
                self.execute_slash_command(command).await?;
                self.composer.clear();
                Ok(())
            }
        }
    }

    async fn execute_slash_command(&mut self, command: SlashCommand) -> Result<()> {
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
        }
        Ok(())
    }

    pub(super) async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
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
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Up => {
                        let next_index = current_index.saturating_sub(1);
                        self.overlay = OverlayState::Events {
                            selected_event_id: self.event_id_for_reverse_index(next_index),
                            detail_scroll: 0,
                        };
                    }
                    KeyCode::Down => {
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
                    other => {
                        detail_scroll = adjust_scroll_for_key(detail_scroll, other);
                        self.overlay = OverlayState::Events {
                            selected_event_id,
                            detail_scroll,
                        };
                    }
                }
                Ok(())
            }
            OverlayState::Transcript { mut scroll } => {
                if key.code != KeyCode::Esc {
                    scroll = adjust_scroll_for_key(scroll, key.code);
                    self.overlay = OverlayState::Transcript { scroll };
                }
                Ok(())
            }
            OverlayState::Tasks {
                mut selected,
                mut detail_scroll,
            } => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        self.overlay = OverlayState::Tasks {
                            selected,
                            detail_scroll: 0,
                        };
                    }
                    KeyCode::Down => {
                        let max = self.tasks.len().saturating_sub(1);
                        selected = (selected + 1).min(max);
                        self.overlay = OverlayState::Tasks {
                            selected,
                            detail_scroll: 0,
                        };
                    }
                    other => {
                        detail_scroll = adjust_scroll_for_key(detail_scroll, other);
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
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => {
                        self.apply_model_picker_selection(&filter, selected).await?;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let max = crate::tui::model_picker::model_picker_rows(
                            self.selected_agent_summary(),
                            &filter,
                        )
                        .len()
                        .saturating_sub(1);
                        selected = (selected + 1).min(max);
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        selected = crate::tui::model_picker::clamp_model_picker_selection(
                            self.selected_agent_summary(),
                            &filter,
                            selected,
                        );
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        filter.push(ch);
                        selected = crate::tui::model_picker::clamp_model_picker_selection(
                            self.selected_agent_summary(),
                            &filter,
                            selected,
                        );
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                    _ => {
                        self.overlay = OverlayState::ModelPicker { filter, selected };
                    }
                }
                Ok(())
            }
            OverlayState::DebugPromptInput { mut composer } => {
                match edit_buffer(key, &mut composer) {
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
                if key.code != KeyCode::Esc {
                    scroll = adjust_scroll_for_key(scroll, key.code);
                    self.overlay = OverlayState::DebugPromptView {
                        title,
                        dump,
                        scroll,
                    };
                }
                Ok(())
            }
            OverlayState::HelpView { mut scroll } => {
                if key.code != KeyCode::Esc {
                    scroll = adjust_scroll_for_key(scroll, key.code);
                    self.overlay = OverlayState::HelpView { scroll };
                }
                Ok(())
            }
        }
    }

    async fn handle_main_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.handle_slash_menu_key(key).await? {
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('a') => {
                    self.overlay = OverlayState::Agents;
                    return Ok(());
                }
                KeyCode::Char('e') => {
                    self.overlay = OverlayState::Events {
                        selected_event_id: self.event_id_for_reverse_index(0),
                        detail_scroll: 0,
                    };
                    return Ok(());
                }
                KeyCode::Char('t') => {
                    self.overlay = OverlayState::Transcript { scroll: 0 };
                    return Ok(());
                }
                KeyCode::Char('j') => {
                    self.overlay = OverlayState::Tasks {
                        selected: 0,
                        detail_scroll: 0,
                    };
                    return Ok(());
                }
                KeyCode::Char('s') => {
                    self.submit_prompt_buffer().await?;
                    return Ok(());
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('?') if self.composer.is_empty() => {
                self.overlay = OverlayState::HelpView { scroll: 0 };
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                self.chat_scroll
                    .scroll_with_key(key.code, self.chat_max_scroll);
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.composer.insert_newline();
            }
            KeyCode::Esc => {
                self.composer.clear();
                self.slash_menu_selected = 0;
                self.slash_menu_dismissed_for = None;
            }
            _ => {
                let before = self.composer.as_str().to_string();
                match edit_buffer(key, &mut self.composer) {
                    Some(BufferAction::Submit) => self.submit_prompt_buffer().await?,
                    Some(BufferAction::Cancel) => {
                        self.composer.clear();
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

        match key.code {
            KeyCode::Esc => {
                self.slash_menu_dismissed_for = Some(self.composer.as_str().to_string());
                self.slash_menu_selected = 0;
                Ok(true)
            }
            _ if specs.is_empty() => Ok(false),
            KeyCode::Up | KeyCode::Char('p')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || matches!(key.code, KeyCode::Up) =>
            {
                self.slash_menu_selected = self.slash_menu_selected.saturating_sub(1);
                Ok(true)
            }
            KeyCode::Down | KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || matches!(key.code, KeyCode::Down) =>
            {
                self.slash_menu_selected =
                    (self.slash_menu_selected + 1).min(specs.len().saturating_sub(1));
                Ok(true)
            }
            KeyCode::Tab => {
                let selected = self.slash_menu_selected.min(specs.len().saturating_sub(1));
                self.composer = ComposerState::from(specs[selected].name);
                self.slash_menu_selected = selected;
                self.slash_menu_dismissed_for = None;
                Ok(true)
            }
            KeyCode::Enter => {
                let selected = self.slash_menu_selected.min(specs.len().saturating_sub(1));
                let command = specs[selected].command;
                self.execute_slash_command(command).await?;
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
        match key.code {
            KeyCode::Esc => Ok(()),
            KeyCode::Enter => {
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
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_agent_selection(-1).await?;
                self.overlay = OverlayState::Agents;
                Ok(())
            }
            KeyCode::Down | KeyCode::Char('j') => {
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
            }
            crate::tui::model_picker::ModelPickerChoice::Model { model } => {
                self.client
                    .set_agent_model_override(&agent_id, model.clone())
                    .await?;
                self.status_line = format!("Set model override for {agent_id} to {model}");
            }
        }
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

enum BufferAction {
    Submit,
    Cancel,
}

fn edit_buffer(key: KeyEvent, composer: &mut ComposerState) -> Option<BufferAction> {
    match key.code {
        KeyCode::Enter => {
            // Ignore Shift+Enter - it's handled by the outer match to insert newline
            if !key.modifiers.contains(KeyModifiers::SHIFT) && !composer.as_str().trim().is_empty()
            {
                return Some(BufferAction::Submit);
            }
        }
        KeyCode::Esc => return Some(BufferAction::Cancel),
        KeyCode::Backspace => {
            composer.backspace();
        }
        KeyCode::Delete => composer.delete(),
        KeyCode::Left => composer.move_left(),
        KeyCode::Right => composer.move_right(),
        KeyCode::Home => composer.move_home(),
        KeyCode::End => composer.move_end(),
        KeyCode::Char(ch) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                composer.insert_char(ch);
            }
        }
        KeyCode::Tab => composer.insert_char('\t'),
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

fn adjust_scroll_for_key(scroll: u16, code: KeyCode) -> u16 {
    match code {
        KeyCode::Up => adjust_scroll(scroll, -1),
        KeyCode::Down => adjust_scroll(scroll, 1),
        KeyCode::PageUp => adjust_scroll(scroll, -10),
        KeyCode::PageDown => adjust_scroll(scroll, 10),
        KeyCode::Home => 0,
        KeyCode::End => u16::MAX,
        _ => scroll,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_composer_submission, slash_menu_specs, slash_prompt_lines, ComposerSubmission,
        SlashCommand,
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
            Some(ComposerSubmission::Slash(SlashCommand::Help))
        );
        assert_eq!(
            parse_composer_submission("/refresh").unwrap(),
            Some(ComposerSubmission::Slash(SlashCommand::Refresh))
        );
        assert_eq!(
            parse_composer_submission("/model").unwrap(),
            Some(ComposerSubmission::Slash(SlashCommand::Model))
        );
        assert_eq!(
            parse_composer_submission("/clear-status").unwrap(),
            Some(ComposerSubmission::Slash(SlashCommand::ClearStatus))
        );
        assert_eq!(
            parse_composer_submission("/debug-prompt").unwrap(),
            Some(ComposerSubmission::Slash(SlashCommand::DebugPrompt))
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
    fn slash_prompt_matches_submit_semantics_for_leading_whitespace() {
        let lines = slash_prompt_lines("   /help").expect("slash prompt should be active");
        assert!(lines[0].contains(">/help"));
    }
}
