use std::{
    env,
    io::{self, Stdout},
    time::{Duration, Instant},
};

use crate::{
    client::LocalClient,
    config::{AltScreenMode, AppConfig},
    system::{workspace_access_mode_label, workspace_projection_label},
    tui_markdown::{render_markdown_text, render_markdown_text_spaced},
    types::{
        AgentListEntry, AgentSummary, BriefRecord, MessageBody, OperatorMessageRecord,
        OperatorMessageStatus, TaskRecord, TranscriptEntry, TranscriptEntryKind, TrustLevel,
    },
};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEvent, KeyEventKind,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
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
use serde_json::Value;

mod app;
mod chat;
mod composer;
mod input;
mod keymap;
mod logging;
mod model_picker;
mod overlay;
mod projection;
mod render;
mod runtime;
mod state;
mod view_model;

use app::TuiApp;
use chat::{chat_text_for_width, paragraph_max_scroll_unframed, ChatScrollState};
use composer::ComposerState;
use logging::TuiLogWriter;
use overlay::OverlayState;
use projection::{OperatorDisplayMode, TuiProjection};
use render::draw;
use runtime::{TuiConnectionState, TuiRuntimeMessage};

const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub async fn run_tui(
    config: AppConfig,
    no_alt_screen: bool,
    client: Option<LocalClient>,
) -> Result<()> {
    let client = match client {
        Some(client) => client,
        None => LocalClient::new(config.clone())?,
    };
    let log_writer = TuiLogWriter::new(config.agent_root_dir())?;
    let mut app = TuiApp::new(client, log_writer);
    app.initialize().await;

    let mut terminal_guard = TerminalCleanupGuard::new();
    enable_raw_mode()?;
    terminal_guard.raw_mode_enabled = true;

    let alt_screen_enabled = determine_alt_screen_mode(no_alt_screen, config.tui_alternate_screen);
    let mut stdout = io::stdout();
    if alt_screen_enabled {
        execute!(stdout, EnterAlternateScreen)?;
        terminal_guard.alternate_screen_enabled = true;
    }
    if execute!(stdout, EnableBracketedPaste).is_ok() {
        terminal_guard.bracketed_paste_enabled = true;
    }
    if execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        terminal_guard.keyboard_enhancement_enabled = true;
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let run_result = run_event_loop(&mut terminal, &mut app).await;
    terminal.show_cursor()?;
    run_result
}

fn determine_alt_screen_mode(no_alt_screen: bool, mode: AltScreenMode) -> bool {
    determine_alt_screen_mode_for_terminal(no_alt_screen, mode, env::var_os("ZELLIJ").is_some())
}

fn determine_alt_screen_mode_for_terminal(
    no_alt_screen: bool,
    mode: AltScreenMode,
    is_zellij: bool,
) -> bool {
    if no_alt_screen {
        return false;
    }

    match mode {
        AltScreenMode::Always => true,
        AltScreenMode::Never => false,
        AltScreenMode::Auto => !is_zellij,
    }
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    loop {
        if let Err(err) = app.tick().await {
            app.status_line = format!("TUI runtime update failed: {err}");
        }
        terminal.draw(|frame| draw(frame, app))?;

        if app.should_quit {
            return Ok(());
        }

        if event::poll(INPUT_POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    if let Err(err) = app.handle_key(key).await {
                        app.status_line = format!("Action failed: {err}");
                    }
                }
                Event::Paste(text) => {
                    if let Err(err) = app.handle_paste(&text).await {
                        app.status_line = format!("Paste failed: {err}");
                    }
                }
                _ => {}
            }
        }
    }
}

struct TerminalCleanupGuard {
    raw_mode_enabled: bool,
    alternate_screen_enabled: bool,
    bracketed_paste_enabled: bool,
    keyboard_enhancement_enabled: bool,
}

impl TerminalCleanupGuard {
    fn new() -> Self {
        Self {
            raw_mode_enabled: false,
            alternate_screen_enabled: false,
            bracketed_paste_enabled: false,
            keyboard_enhancement_enabled: false,
        }
    }
}

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        if self.keyboard_enhancement_enabled {
            let mut stdout = io::stdout();
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        if self.bracketed_paste_enabled {
            let mut stdout = io::stdout();
            let _ = execute!(stdout, DisableBracketedPaste);
        }
        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
        }
        if self.alternate_screen_enabled {
            let mut stdout = io::stdout();
            let _ = execute!(stdout, LeaveAlternateScreen);
        }
    }
}

#[cfg(test)]
mod tests;
