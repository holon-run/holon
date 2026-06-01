use std::{
    collections::BTreeMap,
    io::{self, Stdout},
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::{
    config::{AppConfig, CredentialKind, ProviderId},
    onboarding::{
        apply_onboarding_wizard_draft, onboarding_model_choices, onboarding_provider_choices,
        onboarding_search_choices, OnboardingApplySummary, OnboardingModelChoice,
        OnboardingProviderChoice, OnboardingSearchChoice, OnboardingSearchSelection,
        OnboardingWizardDraft,
    },
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn run_onboarding_tui(config: AppConfig) -> Result<OnboardingApplySummary> {
    let mut app = OnboardingTuiApp::new(&config);
    let mut guard = TerminalGuard::new();
    enable_raw_mode()?;
    guard.raw_mode_enabled = true;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    guard.alternate_screen_enabled = true;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    loop {
        terminal.draw(|frame| draw(frame, &app))?;
        if app.should_quit {
            anyhow::bail!("onboarding cancelled");
        }
        if let Some(draft) = app.completed_draft.clone() {
            terminal.show_cursor()?;
            drop(guard);
            let summary =
                apply_onboarding_wizard_draft(&config, &draft, app.credential_material.clone())?;
            return Ok(summary);
        }
        if event::poll(POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    app.handle_key(key.code)?;
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct OnboardingTuiApp {
    providers: Vec<OnboardingProviderChoice>,
    models_by_provider: BTreeMap<ProviderId, Vec<OnboardingModelChoice>>,
    models: Vec<OnboardingModelChoice>,
    search_choices: Vec<OnboardingSearchChoice>,
    step: Step,
    provider_index: usize,
    model_index: usize,
    search_index: usize,
    selected_provider: Option<OnboardingProviderChoice>,
    selected_model: Option<OnboardingModelChoice>,
    selected_search: OnboardingSearchSelection,
    credential_input: String,
    credential_material: Option<String>,
    completed_draft: Option<OnboardingWizardDraft>,
    should_quit: bool,
    status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Provider,
    Auth,
    Model,
    Search,
    Confirm,
}

impl OnboardingTuiApp {
    fn new(config: &AppConfig) -> Self {
        let providers = onboarding_provider_choices(config);
        let models_by_provider = providers
            .iter()
            .map(|provider| {
                (
                    provider.id.clone(),
                    onboarding_model_choices(config, &provider.id),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let provider_index = providers
            .iter()
            .position(|provider| provider.id == config.default_model.provider)
            .unwrap_or(0);
        let selected_provider = providers.get(provider_index).cloned();
        let models = selected_provider
            .as_ref()
            .and_then(|provider| models_by_provider.get(&provider.id).cloned())
            .unwrap_or_default();
        let model_index = models
            .iter()
            .position(|model| model.model == config.default_model)
            .unwrap_or(0);
        let search_choices = onboarding_search_choices(config);
        let selected_search = if !config.web_config.search.enabled {
            OnboardingSearchSelection::Disabled
        } else if config.web_config.search.provider == "duck_duck_go" {
            OnboardingSearchSelection::DuckDuckGo
        } else {
            OnboardingSearchSelection::BuiltInProvider
        };
        let search_index = search_choices
            .iter()
            .position(|choice| choice.selection == selected_search)
            .unwrap_or(0);
        Self {
            providers,
            models_by_provider,
            models,
            search_choices,
            step: Step::Provider,
            provider_index,
            model_index,
            search_index,
            selected_provider,
            selected_model: None,
            selected_search,
            credential_input: String::new(),
            credential_material: None,
            completed_draft: None,
            should_quit: false,
            status: "Use ↑/↓ to move, Enter to select, Esc to cancel.".into(),
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Backspace if self.step == Step::Auth => {
                self.credential_input.pop();
            }
            KeyCode::Char(ch) if self.step == Step::Auth => self.credential_input.push(ch),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Enter => self.advance()?,
            KeyCode::Left => self.back(),
            _ => {}
        }
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) {
        let (index, len) = match self.step {
            Step::Provider => (&mut self.provider_index, self.providers.len()),
            Step::Model => (&mut self.model_index, self.models.len()),
            Step::Search => (&mut self.search_index, self.search_choices.len()),
            _ => return,
        };
        if len == 0 {
            *index = 0;
            return;
        }
        *index = (*index as isize + delta).rem_euclid(len as isize) as usize;
    }

    fn back(&mut self) {
        self.step = match self.step {
            Step::Provider => Step::Provider,
            Step::Auth => Step::Provider,
            Step::Model => Step::Auth,
            Step::Search => Step::Model,
            Step::Confirm => Step::Search,
        };
    }

    fn advance(&mut self) -> Result<()> {
        match self.step {
            Step::Provider => {
                let provider = self
                    .providers
                    .get(self.provider_index)
                    .cloned()
                    .context("no model provider choices are available")?;
                self.models = self
                    .models_by_provider
                    .get(&provider.id)
                    .cloned()
                    .unwrap_or_default();
                self.model_index = 0;
                self.selected_provider = Some(provider);
                self.step = Step::Auth;
            }
            Step::Auth => {
                let provider = self
                    .selected_provider
                    .as_ref()
                    .context("provider not selected")?;
                if provider.credential_kind == CredentialKind::OAuth {
                    if !provider.credential_configured {
                        self.status = "OAuth login initiation is not available yet; import a Holon OAuth profile or run codex login, then rerun onboard.".into();
                        return Ok(());
                    }
                    self.credential_material = None;
                } else {
                    if !provider.credential_configured && self.credential_input.trim().is_empty() {
                        self.status = "Enter a credential value before continuing.".into();
                        return Ok(());
                    }
                    if !self.credential_input.trim().is_empty() {
                        self.credential_material = Some(self.credential_input.clone());
                    }
                }
                self.step = Step::Model;
            }
            Step::Model => {
                self.selected_model = self.models.get(self.model_index).cloned();
                self.step = Step::Search;
            }
            Step::Search => {
                self.selected_search = self
                    .search_choices
                    .get(self.search_index)
                    .map(|choice| choice.selection)
                    .unwrap_or(OnboardingSearchSelection::BuiltInProvider);
                self.step = Step::Confirm;
            }
            Step::Confirm => {
                let provider = self
                    .selected_provider
                    .as_ref()
                    .context("provider not selected")?;
                let model = self.selected_model.as_ref().context("model not selected")?;
                self.completed_draft = Some(OnboardingWizardDraft {
                    provider: provider.id.clone(),
                    credential_profile: provider.credential_profile.clone(),
                    credential_kind: provider.credential_kind,
                    default_model: model.model.clone(),
                    search: self.selected_search,
                });
            }
        }
        Ok(())
    }
}

fn draw(frame: &mut Frame<'_>, app: &OnboardingTuiApp) {
    let area = centered(frame.area(), 90, 80);
    frame.render_widget(Clear, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(area);
    let title = Paragraph::new(Text::from(vec![
        Line::from(vec![Span::styled(
            "Holon onboarding",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(step_label(app.step)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);
    match app.step {
        Step::Provider => draw_list(
            frame,
            chunks[1],
            "Choose model provider",
            app.providers
                .iter()
                .map(|v| (&v.title, &v.detail))
                .collect(),
            app.provider_index,
        ),
        Step::Auth => draw_auth(frame, chunks[1], app),
        Step::Model => draw_list(
            frame,
            chunks[1],
            "Choose default model",
            app.models.iter().map(|v| (&v.title, &v.detail)).collect(),
            app.model_index,
        ),
        Step::Search => draw_list(
            frame,
            chunks[1],
            "Configure search",
            app.search_choices
                .iter()
                .map(|v| (&v.title, &v.detail))
                .collect(),
            app.search_index,
        ),
        Step::Confirm => draw_confirm(frame, chunks[1], app),
    }
    frame.render_widget(
        Paragraph::new(format!("{}  ·  ← back", app.status))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Help")),
        chunks[2],
    );
}

fn draw_list(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    rows: Vec<(&String, &String)>,
    selected: usize,
) {
    let items = rows
        .into_iter()
        .map(|(title, detail)| {
            ListItem::new(vec![Line::from(title.clone()), Line::from(detail.clone())])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len() - 1)));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_auth(frame: &mut Frame<'_>, area: Rect, app: &OnboardingTuiApp) {
    let provider = app.selected_provider.as_ref();
    let title = provider
        .map(|provider| format!("Authenticate {}", provider.title))
        .unwrap_or_else(|| "Authenticate provider".into());
    let body = provider
        .map(|provider| {
            let masked = "*".repeat(app.credential_input.chars().count());
            let instruction = match provider.credential_kind {
                CredentialKind::OAuth => "Use the existing Holon-owned OAuth profile or external Codex fallback. If it is missing, import credentials with `holon config credentials set --kind oauth --stdin openai-codex` or run `codex login`, then rerun onboard.",
                CredentialKind::ApiKey => {
                    "Enter API key. The value is masked and stored in the local Holon credential store."
                }
                _ => "Enter credential material if required.",
            };
            Text::from(vec![
                Line::from(format!("profile: {}", provider.credential_profile)),
                Line::from(format!("kind: {}", provider.credential_kind.as_str())),
                Line::from(format!(
                    "configured: {}",
                    if provider.credential_configured { "yes" } else { "no" }
                )),
                Line::from(""),
                Line::from(instruction),
                Line::from(""),
                if provider.credential_kind == CredentialKind::OAuth {
                    Line::from("credential input: handled by OAuth profile; nothing is echoed here")
                } else {
                    Line::from(format!("credential: {masked}"))
                },
            ])
        })
        .unwrap_or_else(|| Text::from("No provider selected."));
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn draw_confirm(frame: &mut Frame<'_>, area: Rect, app: &OnboardingTuiApp) {
    let provider = app.selected_provider.as_ref();
    let model = app.selected_model.as_ref();
    let text = Text::from(vec![
        Line::from("Review changes. Press Enter to write config, ← to go back, Esc to cancel."),
        Line::from(""),
        Line::from(format!(
            "provider: {}",
            provider
                .map(|provider| provider.id.as_str().to_string())
                .unwrap_or_else(|| "-".into())
        )),
        Line::from(format!(
            "credential profile: {}",
            provider
                .map(|provider| provider.credential_profile.clone())
                .unwrap_or_else(|| "-".into())
        )),
        Line::from(format!(
            "default model: {}",
            model
                .map(|model| model.model.as_string())
                .unwrap_or_else(|| "-".into())
        )),
        Line::from(format!("search: {}", app.selected_search.label())),
        Line::from(""),
        Line::from("Secret material is never printed in this summary."),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Confirm")),
        area,
    );
}

fn step_label(step: Step) -> &'static str {
    match step {
        Step::Provider => "Step 1/5 · model provider",
        Step::Auth => "Step 2/5 · authentication",
        Step::Model => "Step 3/5 · default model",
        Step::Search => "Step 4/5 · search",
        Step::Confirm => "Step 5/5 · confirm",
    }
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

struct TerminalGuard {
    raw_mode_enabled: bool,
    alternate_screen_enabled: bool,
}

impl TerminalGuard {
    fn new() -> Self {
        Self {
            raw_mode_enabled: false,
            alternate_screen_enabled: false,
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
        }
        if self.alternate_screen_enabled {
            let mut stdout: Stdout = io::stdout();
            let _ = execute!(stdout, LeaveAlternateScreen);
        }
    }
}
