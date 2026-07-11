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
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::{
    auth::{
        oauth_provider_config, run_codex_oauth_login_profile_material,
        run_oauth_device_login_profile_material,
    },
    config::{AppConfig, CredentialKind, ModelRef, ModelRouteRef, ProviderId},
    onboarding::{
        onboarding_model_choices, onboarding_provider_choices, onboarding_search_choices,
        OnboardingModelChoice, OnboardingProviderChoice, OnboardingSearchChoice,
        OnboardingSearchSelection, OnboardingWizardDraft, OnboardingWizardSubmission,
    },
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const DUCKDUCKGO_SEARCH_PROVIDER_ID: &str = "duckduckgo";

pub fn run_onboarding_tui(config: AppConfig) -> Result<OnboardingWizardSubmission> {
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
            drop(guard);
            anyhow::bail!("Onboarding cancelled.");
        }
        if let Some(draft) = app.completed_draft.clone() {
            terminal.show_cursor()?;
            drop(guard);
            return Ok(OnboardingWizardSubmission {
                draft,
                credential_material: app.credential_material.clone(),
            });
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
    custom_model_input: String,
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
    CustomModel,
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
        } else if config.web_config.search.builtin_provider_enabled {
            OnboardingSearchSelection::Auto
        } else if config.web_config.search.provider == DUCKDUCKGO_SEARCH_PROVIDER_ID {
            OnboardingSearchSelection::ManagedDuckDuckGo
        } else {
            OnboardingSearchSelection::Auto
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
            custom_model_input: String::new(),
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
            KeyCode::Backspace if self.step == Step::CustomModel => {
                self.custom_model_input.pop();
            }
            KeyCode::Char('l') | KeyCode::Char('L') if self.can_run_codex_login() => {
                self.run_codex_login_for_selected_provider()?;
            }
            KeyCode::Char(ch) if self.step == Step::Auth => self.credential_input.push(ch),
            KeyCode::Char(ch) if self.step == Step::CustomModel => {
                self.custom_model_input.push(ch);
            }
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
            Step::CustomModel => Step::Model,
            Step::Search if !self.custom_model_input.trim().is_empty() => Step::CustomModel,
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
                if self
                    .selected_provider
                    .as_ref()
                    .is_some_and(|selected| selected.id != provider.id)
                {
                    self.credential_input.clear();
                    self.credential_material = None;
                }
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
                        self.run_codex_login_for_selected_provider()?;
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
                let model = self
                    .models
                    .get(self.model_index)
                    .cloned()
                    .context("no model choices are available")?;
                if model.custom {
                    self.selected_model = None;
                    self.custom_model_input.clear();
                    self.status =
                        "Enter a model id. Use `provider@endpoint/model`, legacy `provider/model`, or a model name for this provider."
                            .into();
                    self.step = Step::CustomModel;
                } else {
                    self.selected_model = Some(model);
                    self.custom_model_input.clear();
                    self.step = Step::Search;
                }
            }
            Step::CustomModel => {
                let provider = self
                    .selected_provider
                    .as_ref()
                    .context("provider not selected")?;
                let model_ref = parse_custom_model_ref(provider, &self.custom_model_input)?;
                self.selected_model = Some(OnboardingModelChoice {
                    title: model_ref.as_string(),
                    detail: "custom model".into(),
                    model: model_ref,
                    custom: false,
                });
                self.step = Step::Search;
            }
            Step::Search => {
                self.selected_search = self
                    .search_choices
                    .get(self.search_index)
                    .map(|choice| choice.selection)
                    .unwrap_or(OnboardingSearchSelection::Auto);
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

    fn run_codex_login_for_selected_provider(&mut self) -> Result<()> {
        let provider = self
            .selected_provider
            .as_mut()
            .context("provider not selected")?;
        let oauth_config = oauth_provider_config(provider.id.as_str());
        if !provider.id.is_openai_codex() && oauth_config.is_none() {
            self.status =
                "This OAuth provider does not have an onboard-managed login command yet.".into();
            return Ok(());
        }

        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
        println!(
            "Starting Holon {} OAuth login. Complete the browser/device login, then return here.",
            provider.id.as_str()
        );
        let login_result = if provider.id.is_openai_codex() {
            run_codex_oauth_login_profile_material()
        } else {
            run_oauth_device_login_profile_material(oauth_config.expect("checked above"))
        };
        execute!(io::stdout(), EnterAlternateScreen)?;
        enable_raw_mode()?;
        let login = match login_result {
            Ok(login) => login,
            Err(error) => {
                self.status = format!("Holon {} OAuth login failed: {error}", provider.id.as_str());
                return Ok(());
            }
        };
        if login.material.trim().is_empty() {
            self.status =
                "Holon OpenAI Codex OAuth login did not return credential material.".into();
            return Ok(());
        }
        provider.credential_configured = true;
        self.credential_material = Some(login.material);
        if provider.credential_configured {
            self.status = login
                .account_id
                .map(|account| {
                    format!(
                        "Holon {} OAuth credential saved for account {account}; continuing to model selection.",
                        provider.id.as_str()
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "Holon {} OAuth credential saved; continuing to model selection.",
                        provider.id.as_str()
                    )
                });
            self.step = Step::Model;
        }
        Ok(())
    }

    fn can_run_codex_login(&self) -> bool {
        self.step == Step::Auth
            && self.selected_provider.as_ref().is_some_and(|provider| {
                provider.credential_kind == CredentialKind::OAuth
                    && oauth_provider_config(provider.id.as_str()).is_some()
            })
    }
}

fn parse_custom_model_ref(
    provider: &OnboardingProviderChoice,
    input: &str,
) -> Result<ModelRouteRef> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("enter a model id before continuing");
    }
    let model_ref = if trimmed.contains('/') {
        ModelRouteRef::parse_compatible(trimmed)?
    } else {
        ModelRouteRef::from_legacy_model_ref(&ModelRef::new(provider.id.clone(), trimmed))
    };
    if model_ref.provider != provider.id {
        anyhow::bail!(
            "custom model provider {} does not match selected provider {}",
            model_ref.provider.as_str(),
            provider.id.as_str()
        );
    }
    Ok(model_ref)
}

fn draw(frame: &mut Frame<'_>, app: &OnboardingTuiApp) {
    let area = centered(frame.area(), 92, 84);
    frame.render_widget(Clear, area);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
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
    frame.render_widget(title, outer[0]);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(48)])
        .split(outer[1]);
    draw_sidebar(frame, body[0], app);
    match app.step {
        Step::Provider => draw_list(
            frame,
            body[1],
            "Choose model provider",
            app.providers
                .iter()
                .map(|v| (v.title.clone(), provider_detail(v)))
                .collect(),
            app.provider_index,
        ),
        Step::Auth => draw_auth(frame, body[1], app),
        Step::Model => draw_list(
            frame,
            body[1],
            "Choose default model",
            app.models
                .iter()
                .map(|v| (v.title.clone(), v.detail.clone()))
                .collect(),
            app.model_index,
        ),
        Step::CustomModel => draw_custom_model(frame, body[1], app),
        Step::Search => draw_list(
            frame,
            body[1],
            "Configure search",
            app.search_choices
                .iter()
                .map(|v| (v.title.clone(), v.detail.clone()))
                .collect(),
            app.search_index,
        ),
        Step::Confirm => draw_confirm(frame, body[1], app),
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{}  ·  ↑/↓ move · Enter select/continue · ← back · Esc cancel",
            app.status
        ))
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Help")),
        outer[2],
    );
}

fn draw_sidebar(frame: &mut Frame<'_>, area: Rect, app: &OnboardingTuiApp) {
    let steps = [
        (Step::Provider, "Provider"),
        (Step::Auth, "Auth"),
        (Step::Model, "Model"),
        (Step::Search, "Search"),
        (Step::Confirm, "Confirm"),
    ];
    let mut lines = steps
        .iter()
        .map(|(step, label)| {
            let marker =
                if *step == app.step || (app.step == Step::CustomModel && *step == Step::Model) {
                    "●"
                } else {
                    "○"
                };
            let style =
                if *step == app.step || (app.step == Step::CustomModel && *step == Step::Model) {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
            Line::from(vec![
                Span::styled(marker, style),
                Span::raw(" "),
                Span::styled(*label, style),
            ])
        })
        .collect::<Vec<_>>();
    lines.push(Line::from(""));
    lines.push(Line::from("Current"));
    lines.push(Line::from(format!(
        "provider: {}",
        app.selected_provider
            .as_ref()
            .map(|provider| provider.id.as_str())
            .unwrap_or("-")
    )));
    lines.push(Line::from(format!(
        "model: {}",
        app.selected_model
            .as_ref()
            .map(|model| model.model.as_string())
            .unwrap_or_else(|| "-".into())
    )));
    lines.push(Line::from(format!(
        "search: {}",
        app.selected_search.label()
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Progress")),
        area,
    );
}

fn provider_detail(provider: &OnboardingProviderChoice) -> String {
    let origin = if provider.configured {
        "configured"
    } else {
        "built-in"
    };
    let auth = if provider.credential_configured {
        "credential ready"
    } else {
        "needs credential"
    };
    format!("{} · {} · {}", provider.detail, origin, auth)
}

fn draw_list(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    rows: Vec<(String, String)>,
    selected: usize,
) {
    let items = rows
        .into_iter()
        .map(|(title, detail)| {
            ListItem::new(vec![
                Line::from(title),
                Line::from(Span::styled(detail, Style::default().fg(Color::DarkGray))),
            ])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len() - 1)));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_auth(frame: &mut Frame<'_>, area: Rect, app: &OnboardingTuiApp) {
    let provider = app.selected_provider.as_ref();
    let title = provider
        .map(|provider| format!("Authenticate {}", provider.title))
        .unwrap_or_else(|| "Authenticate provider".into());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(provider) = provider else {
        frame.render_widget(Paragraph::new("No provider selected."), inner);
        return;
    };

    let input_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let masked = "*".repeat(app.credential_input.chars().count());
    let input_prefix = if provider.credential_kind == CredentialKind::OAuth {
        "credential input: handled by OAuth profile; nothing is echoed here"
    } else {
        "credential: "
    };
    let input_line = if provider.credential_kind == CredentialKind::OAuth {
        Line::from(input_prefix)
    } else {
        Line::from(format!("{input_prefix}{masked}"))
    };
    let body = {
        let instruction = match provider.credential_kind {
                CredentialKind::OAuth if oauth_provider_config(provider.id.as_str()).is_some() => "Press Enter to continue when credential is ready. If missing, Enter starts Holon's built-in OAuth device login; press `l` anytime to run/refresh it.",
                CredentialKind::OAuth => "Use an existing Holon-owned OAuth profile. This provider does not have an onboard-managed login command.",
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
                if provider.credential_configured {
                    "yes"
                } else {
                    "no"
                }
            )),
            Line::from(""),
            Line::from(instruction),
            Line::from(""),
        ])
    };
    frame.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: true }),
        input_area[0],
    );
    frame.render_widget(Paragraph::new(input_line), input_area[1]);
    if provider.credential_kind != CredentialKind::OAuth {
        frame.set_cursor_position(Position {
            x: input_area[1]
                .x
                .saturating_add((input_prefix.len() + app.credential_input.chars().count()) as u16),
            y: input_area[1].y,
        });
    }
}

fn draw_custom_model(frame: &mut Frame<'_>, area: Rect, app: &OnboardingTuiApp) {
    let provider = app.selected_provider.as_ref();
    let text = Text::from(vec![
        Line::from("Enter a custom model id, then press Enter."),
        Line::from(""),
        Line::from(format!(
            "selected provider: {}",
            provider
                .map(|provider| provider.id.as_str().to_string())
                .unwrap_or_else(|| "-".into())
        )),
        Line::from("accepted forms: provider@endpoint/model, legacy provider/model, or model-name"),
        Line::from(""),
        Line::from(format!("model: {}", app.custom_model_input)),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Custom model")),
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
        Step::CustomModel => "Step 3/5 · custom model",
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
