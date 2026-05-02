use std::{
    cell::RefCell,
    env,
    io::{self, Stdout},
    time::{Duration, Instant},
};

use crate::{
    client::{AgentStreamEvent, EventStreamRequest, LocalClient, LocalEventStream, LocalHttpError},
    config::{AltScreenMode, AppConfig},
    system::{workspace_access_mode_label, workspace_projection_label},
    tui_markdown::render_markdown_text,
    types::{
        AgentSummary, BriefRecord, TaskRecord, TranscriptEntry, TranscriptEntryKind, TrustLevel,
    },
};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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
use tokio::{sync::mpsc, task::JoinHandle};

mod chat;
mod composer;
mod input;
mod logging;
mod model_picker;
mod overlay;
mod projection;
mod render;

#[cfg(test)]
use chat::{build_chat_text, collect_chat_items, is_operator_origin_value};
use chat::{chat_text, paragraph_max_scroll, CachedChatText, ChatScrollState};
use composer::ComposerState;
use logging::TuiLogWriter;
#[cfg(test)]
use overlay::centered_rect_rows;
use overlay::OverlayState;
use projection::{ProjectionSlice, TuiProjection};
use render::draw;

const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const STREAM_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const REFRESH_RETRY_DELAY: Duration = Duration::from_secs(1);
const AGENT_LIST_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const BRIEF_LIMIT: usize = 24;
const TRANSCRIPT_LIMIT: usize = 40;
const TASK_LIMIT: usize = 40;

pub async fn run_tui(config: AppConfig, no_alt_screen: bool) -> Result<()> {
    let client = LocalClient::new(config.clone())?;
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
            if let Event::Key(key) = event::read()? {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    if let Err(err) = app.handle_key(key).await {
                        app.status_line = format!("Action failed: {err}");
                    }
                }
            }
        }
    }
}

struct TuiApp {
    client: LocalClient,
    agents: Vec<AgentSummary>,
    briefs: Vec<BriefRecord>,
    transcript: Vec<TranscriptEntry>,
    tasks: Vec<TaskRecord>,
    projection: Option<TuiProjection>,
    connection_state: TuiConnectionState,
    stream_messages: Option<mpsc::UnboundedReceiver<TuiRuntimeMessage>>,
    stream_task: Option<JoinHandle<()>>,
    agent_list_refresh_deadline: Option<Instant>,
    reconnect_deadline: Option<Instant>,
    refresh_deadline: Option<Instant>,
    reconnect_attempt: u32,
    selected_agent: usize,
    chat_scroll: ChatScrollState,
    chat_max_scroll: u16,
    composer: ComposerState,
    slash_menu_selected: usize,
    slash_menu_dismissed_for: Option<String>,
    overlay: OverlayState,
    last_refresh_at: Option<DateTime<Local>>,
    last_event_at: Option<DateTime<Local>>,
    pub(crate) status_line: String,
    should_quit: bool,
    chat_text_cache: RefCell<Option<CachedChatText>>,
    log_writer: TuiLogWriter,
}

#[derive(Debug, Clone)]
enum TuiConnectionState {
    Bootstrapping,
    Streaming,
    Reconnecting { attempt: u32, last_error: String },
    RefreshRequired { reason: String },
    Disconnected { reason: String },
}

#[derive(Debug)]
enum TuiRuntimeMessage {
    Event(AgentStreamEvent),
    Disconnected { error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentListChange {
    Ready,
    RequiresBootstrap,
    Empty,
}

#[derive(Debug, Clone)]
struct TuiRuntimeCheckpoint {
    connection_state: TuiConnectionState,
    reconnect_deadline: Option<Instant>,
    refresh_deadline: Option<Instant>,
    reconnect_attempt: u32,
}

impl TuiApp {
    pub(crate) fn new(client: LocalClient, log_writer: TuiLogWriter) -> Self {
        Self {
            client,
            agents: Vec::new(),
            briefs: Vec::new(),
            transcript: Vec::new(),
            tasks: Vec::new(),
            projection: None,
            connection_state: TuiConnectionState::Bootstrapping,
            stream_messages: None,
            stream_task: None,
            agent_list_refresh_deadline: None,
            reconnect_deadline: None,
            refresh_deadline: None,
            reconnect_attempt: 0,
            selected_agent: 0,
            chat_scroll: ChatScrollState::new(),
            chat_max_scroll: 0,
            composer: ComposerState::new(),
            slash_menu_selected: 0,
            slash_menu_dismissed_for: None,
            overlay: OverlayState::None,
            last_refresh_at: None,
            last_event_at: None,
            status_line: "Connecting to local Holon runtime...".into(),
            should_quit: false,
            chat_text_cache: RefCell::new(None),
            log_writer,
        }
    }

    async fn initialize(&mut self) {
        self.schedule_agent_list_refresh();
        if let Err(err) = self.load_agents().await {
            self.set_disconnected(format!("failed to list public agents: {err}"));
            return;
        }
        if self.agents.is_empty() {
            self.set_disconnected("no public agents are available".into());
            return;
        }
        let _ = self.bootstrap_selected_agent().await;
    }

    async fn tick(&mut self) -> Result<()> {
        self.process_runtime_messages();

        if self
            .agent_list_refresh_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.schedule_agent_list_refresh();
            if let Err(err) = self.refresh_public_agents().await {
                if self.agents.is_empty() {
                    self.set_disconnected(format!("failed to refresh public agents: {err}"));
                } else {
                    self.status_line = format!("Public agent refresh failed: {err}");
                }
            }
        }

        match self.connection_state.clone() {
            TuiConnectionState::RefreshRequired { .. } => {
                if self
                    .refresh_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    if let Err(err) = self.bootstrap_selected_agent().await {
                        self.schedule_refresh(format!("snapshot refresh failed: {err}"));
                    }
                }
            }
            TuiConnectionState::Reconnecting { .. } => {
                if self
                    .reconnect_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    self.connect_event_stream().await?;
                }
            }
            TuiConnectionState::Bootstrapping
            | TuiConnectionState::Streaming
            | TuiConnectionState::Disconnected { .. } => {}
        }

        Ok(())
    }

    async fn load_agents(&mut self) -> Result<()> {
        let agents = self.client.list_agents().await?;
        let _ = self.apply_agent_list(agents);
        Ok(())
    }

    async fn refresh_public_agents(&mut self) -> Result<()> {
        let agents = self.client.list_agents().await?;
        match self.apply_agent_list(agents) {
            AgentListChange::Ready => Ok(()),
            AgentListChange::RequiresBootstrap => self.bootstrap_selected_agent().await,
            AgentListChange::Empty => {
                self.set_disconnected("no public agents are available".into());
                Ok(())
            }
        }
    }

    fn apply_agent_list(&mut self, agents: Vec<AgentSummary>) -> AgentListChange {
        let previously_selected = self.selected_agent_id().map(ToString::to_string);
        if agents.is_empty() {
            self.clear_agent_view();
            return AgentListChange::Empty;
        }

        let selected_missing = previously_selected.as_ref().is_some_and(|agent_id| {
            !agents
                .iter()
                .any(|agent| agent.identity.agent_id == *agent_id)
        });

        self.selected_agent = previously_selected
            .and_then(|agent_id| {
                agents
                    .iter()
                    .position(|agent| agent.identity.agent_id == agent_id)
            })
            .unwrap_or_else(|| self.selected_agent.min(agents.len().saturating_sub(1)));
        self.agents = agents;

        if selected_missing {
            self.clear_projection_view();
        }

        if self.projection.is_none() {
            AgentListChange::RequiresBootstrap
        } else {
            AgentListChange::Ready
        }
    }

    fn selected_agent_id(&self) -> Option<&str> {
        self.agents
            .get(self.selected_agent)
            .map(|agent| agent.identity.agent_id.as_str())
    }

    fn selected_agent_summary(&self) -> Option<&AgentSummary> {
        self.agents.get(self.selected_agent)
    }

    fn clear_agent_view(&mut self) {
        self.clear_projection_view();
        self.agents.clear();
        self.selected_agent = 0;
    }

    fn clear_projection_view(&mut self) {
        self.stop_stream_task();
        self.briefs.clear();
        self.transcript.clear();
        self.tasks.clear();
        self.projection = None;
        self.last_refresh_at = None;
        self.last_event_at = None;
        self.refresh_deadline = None;
        self.reconnect_deadline = None;
        self.reconnect_attempt = 0;
    }

    async fn bootstrap_selected_agent(&mut self) -> Result<()> {
        self.bootstrap_agent_index(self.selected_agent).await
    }

    async fn bootstrap_agent_index(&mut self, target_index: usize) -> Result<()> {
        let agent_id = self
            .agents
            .get(target_index)
            .map(|agent| agent.identity.agent_id.clone())
            .ok_or_else(|| anyhow!("no agent selected"))?;
        let switching_agents = target_index != self.selected_agent;
        let checkpoint = switching_agents.then(|| self.runtime_checkpoint());
        self.refresh_deadline = None;
        self.reconnect_deadline = None;
        self.connection_state = TuiConnectionState::Bootstrapping;
        self.status_line = format!("Bootstrapping agent {agent_id} from /state");

        let snapshot = match self.client.agent_state_snapshot(&agent_id).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                if let Some(checkpoint) = checkpoint {
                    self.restore_runtime_checkpoint(checkpoint);
                    self.status_line = format!("Failed to switch to agent {agent_id}: {err}");
                } else {
                    self.schedule_refresh(format!(
                        "failed to bootstrap {agent_id} from /state: {err}"
                    ));
                }
                return Err(err);
            }
        };
        let mut projection = TuiProjection::from_snapshot(snapshot);
        if !switching_agents {
            if let Some(previous) = self.projection.as_ref() {
                projection.inherit_recent_event_logs_from(previous);
            }
        }
        let cursor = projection.cursor.clone();

        self.stop_stream_task();
        self.selected_agent = target_index;
        self.projection = Some(projection);
        self.apply_projection_view();
        self.last_refresh_at = Some(Local::now());
        self.last_event_at = None;
        self.reconnect_attempt = 0;
        self.reconnect_deadline = None;
        self.status_line = format!("Bootstrapped agent {agent_id} from /state");

        self.connect_event_stream_for(agent_id, cursor).await
    }

    async fn connect_event_stream(&mut self) -> Result<()> {
        let agent_id = self
            .selected_agent_id()
            .ok_or_else(|| anyhow!("no agent selected"))?
            .to_string();
        let since = self
            .projection
            .as_ref()
            .and_then(|projection| projection.cursor.clone());
        self.connect_event_stream_for(agent_id, since).await
    }

    async fn connect_event_stream_for(
        &mut self,
        agent_id: String,
        since: Option<String>,
    ) -> Result<()> {
        let request = EventStreamRequest {
            since,
            ..Default::default()
        };
        match self.client.stream_agent_events(&agent_id, request).await {
            Ok(stream) => {
                self.spawn_stream_task(stream);
                self.connection_state = TuiConnectionState::Streaming;
                self.reconnect_attempt = 0;
                self.reconnect_deadline = None;
                self.refresh_deadline = None;
                self.status_line.clear();
                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                if is_cursor_too_old_error(&err) {
                    self.schedule_refresh(format!(
                        "replay cursor expired for {agent_id}; rebuilding from /state"
                    ));
                    self.status_line =
                        format!("Replay cursor expired for {agent_id}; resetting from /state");
                } else {
                    self.schedule_reconnect(message.clone());
                    self.status_line =
                        format!("Event stream disconnected for {agent_id}: {message}");
                }
                Ok(())
            }
        }
    }

    fn next_agent_index(&self, delta: i32) -> Option<usize> {
        if self.agents.is_empty() {
            return None;
        }

        Some(if delta > 0 {
            (self.selected_agent + 1) % self.agents.len()
        } else if self.selected_agent == 0 {
            self.agents.len() - 1
        } else {
            self.selected_agent - 1
        })
    }

    fn spawn_stream_task(&mut self, mut stream: LocalEventStream) {
        self.stop_stream_task();
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            loop {
                match stream.next_event().await {
                    Ok(event) => {
                        if tx.send(TuiRuntimeMessage::Event(event)).is_err() {
                            return;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(TuiRuntimeMessage::Disconnected {
                            error: err.to_string(),
                        });
                        return;
                    }
                }
            }
        });
        self.stream_messages = Some(rx);
        self.stream_task = Some(task);
    }

    fn stop_stream_task(&mut self) {
        if let Some(task) = self.stream_task.take() {
            task.abort();
        }
        self.stream_messages = None;
    }

    fn process_runtime_messages(&mut self) -> bool {
        let mut disconnected = false;
        loop {
            let message = match self.stream_messages.as_mut() {
                Some(receiver) => match receiver.try_recv() {
                    Ok(message) => Some(message),
                    Err(mpsc::error::TryRecvError::Empty) => None,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        None
                    }
                },
                None => None,
            };

            let Some(message) = message else {
                break;
            };

            match message {
                TuiRuntimeMessage::Event(event) => self.apply_stream_event(event),
                TuiRuntimeMessage::Disconnected { error } => {
                    disconnected = true;
                    self.schedule_reconnect(error.clone());
                    self.status_line = format!("Event stream disconnected: {error}");
                }
            }
        }

        if disconnected {
            if matches!(self.connection_state, TuiConnectionState::Streaming) {
                self.schedule_reconnect("event stream reader stopped".into());
                self.status_line = "Event stream reader stopped unexpectedly".into();
            }
            self.stop_stream_task();
        }

        disconnected
    }

    fn apply_stream_event(&mut self, event: AgentStreamEvent) {
        if let Some(projection) = self.projection.as_mut() {
            projection.apply_event(event, &self.log_writer);
            self.last_event_at = Some(Local::now());
            self.apply_projection_view();
            self.schedule_projection_refresh_if_stale();
        }
    }

    fn apply_projection_view(&mut self) {
        let Some(projection) = self.projection.as_ref() else {
            return;
        };

        // When streaming, merge the HTTP response with existing transcript data
        // to avoid losing messages that arrived via SSE but haven't been persisted yet
        let is_streaming = matches!(self.connection_state, TuiConnectionState::Streaming);

        // Create a merged transcript view if streaming
        let merged_transcript = if is_streaming && !self.transcript.is_empty() {
            // Start with HTTP response, then add any SSE-only messages not yet in HTTP response
            let mut merged = projection.transcript_tail.clone();
            for entry in &self.transcript {
                let key = transcript_merge_key(entry);
                // /state uses persisted ids while SSE entries can be synthetic.
                // related_message_id is the stable identity once persistence catches up.
                if !merged
                    .iter()
                    .any(|persisted| transcript_merge_key(persisted) == key)
                {
                    merged.push(entry.clone());
                }
            }
            merged
        } else {
            projection.transcript_tail.clone()
        };

        self.briefs = projection
            .briefs_tail
            .iter()
            .cloned()
            .rev()
            .take(BRIEF_LIMIT)
            .collect::<Vec<_>>();
        self.briefs.reverse();

        self.transcript = merged_transcript
            .iter()
            .cloned()
            .rev()
            .take(TRANSCRIPT_LIMIT)
            .collect::<Vec<_>>();
        self.transcript.reverse();

        self.tasks = projection
            .tasks
            .iter()
            .cloned()
            .rev()
            .take(TASK_LIMIT)
            .collect::<Vec<_>>();
        self.tasks.reverse();

        if let Some(selected_agent) = self.agents.get_mut(self.selected_agent) {
            *selected_agent = projection.agent.clone();
        }

        self.overlay = match &self.overlay {
            OverlayState::Events {
                selected_event_id,
                detail_scroll,
            } => OverlayState::Events {
                selected_event_id: selected_event_id
                    .as_ref()
                    .filter(|event_id| {
                        projection
                            .event_log()
                            .iter()
                            .any(|event| event.id == **event_id)
                    })
                    .cloned()
                    .or_else(|| projection.event_log().last().map(|event| event.id.clone())),
                detail_scroll: *detail_scroll,
            },
            OverlayState::Tasks {
                selected,
                detail_scroll,
            } => OverlayState::Tasks {
                selected: (*selected).min(self.tasks.len().saturating_sub(1)),
                detail_scroll: *detail_scroll,
            },
            other => other.clone(),
        };
    }

    fn schedule_reconnect(&mut self, error: String) {
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        self.reconnect_deadline = Some(Instant::now() + STREAM_RECONNECT_DELAY);
        self.refresh_deadline = None;
        self.connection_state = TuiConnectionState::Reconnecting {
            attempt: self.reconnect_attempt,
            last_error: error,
        };
    }

    fn schedule_refresh(&mut self, reason: String) {
        self.refresh_deadline = Some(Instant::now() + REFRESH_RETRY_DELAY);
        self.reconnect_deadline = None;
        self.connection_state = TuiConnectionState::RefreshRequired { reason };
    }

    fn schedule_projection_refresh_if_stale(&mut self) {
        let Some(projection) = self.projection.as_ref() else {
            return;
        };
        if projection.stale_slices.is_empty() || self.refresh_deadline.is_some() {
            return;
        }
        if !matches!(self.connection_state, TuiConnectionState::Streaming) {
            return;
        }
        let summary = self
            .stale_slice_summary()
            .unwrap_or_else(|| "projection".into());
        self.schedule_refresh(format!(
            "projection stale: {summary}; refreshing from /state"
        ));
    }

    fn schedule_agent_list_refresh(&mut self) {
        self.agent_list_refresh_deadline = Some(Instant::now() + AGENT_LIST_REFRESH_INTERVAL);
    }

    fn set_disconnected(&mut self, reason: String) {
        self.stop_stream_task();
        self.refresh_deadline = None;
        self.reconnect_deadline = None;
        self.connection_state = TuiConnectionState::Disconnected {
            reason: reason.clone(),
        };
        self.status_line = format!("Disconnected: {reason}");
    }

    fn runtime_checkpoint(&self) -> TuiRuntimeCheckpoint {
        TuiRuntimeCheckpoint {
            connection_state: self.connection_state.clone(),
            reconnect_deadline: self.reconnect_deadline,
            refresh_deadline: self.refresh_deadline,
            reconnect_attempt: self.reconnect_attempt,
        }
    }

    fn restore_runtime_checkpoint(&mut self, checkpoint: TuiRuntimeCheckpoint) {
        self.connection_state = checkpoint.connection_state;
        self.reconnect_deadline = checkpoint.reconnect_deadline;
        self.refresh_deadline = checkpoint.refresh_deadline;
        self.reconnect_attempt = checkpoint.reconnect_attempt;
    }

    fn connection_label(&self) -> String {
        match &self.connection_state {
            TuiConnectionState::Bootstrapping => "bootstrapping".into(),
            TuiConnectionState::Streaming => "streaming".into(),
            TuiConnectionState::Reconnecting { attempt, .. } => {
                format!("reconnecting (attempt {attempt})")
            }
            TuiConnectionState::RefreshRequired { .. } => "refresh-required".into(),
            TuiConnectionState::Disconnected { .. } => "disconnected".into(),
        }
    }

    fn connection_detail(&self) -> Option<&str> {
        match &self.connection_state {
            TuiConnectionState::Reconnecting { last_error, .. } => Some(last_error.as_str()),
            TuiConnectionState::RefreshRequired { reason } => Some(reason.as_str()),
            TuiConnectionState::Disconnected { reason } => Some(reason.as_str()),
            TuiConnectionState::Bootstrapping | TuiConnectionState::Streaming => None,
        }
    }

    fn stale_slice_summary(&self) -> Option<String> {
        let projection = self.projection.as_ref()?;
        if projection.stale_slices.is_empty() {
            return None;
        }

        let labels = projection
            .stale_slices
            .iter()
            .map(|slice| match slice {
                ProjectionSlice::Agent => "agent",
                ProjectionSlice::Session => "session",
                ProjectionSlice::Tasks => "tasks",
                ProjectionSlice::TranscriptTail => "transcript",
                ProjectionSlice::BriefsTail => "briefs",
                ProjectionSlice::Timers => "timers",
                ProjectionSlice::WorkItems => "work-items",
                ProjectionSlice::WorkPlan => "work-plan",
                ProjectionSlice::WaitingIntents => "waiting",
                ProjectionSlice::ExternalTriggers => "external-triggers",
                ProjectionSlice::OperatorNotifications => "operator-notifications",
                ProjectionSlice::Workspace => "workspace",
            })
            .collect::<Vec<_>>();
        Some(labels.join(", "))
    }
}

fn transcript_merge_key(entry: &TranscriptEntry) -> &str {
    entry
        .related_message_id
        .as_deref()
        .unwrap_or(entry.id.as_str())
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        self.stop_stream_task();
    }
}

fn is_cursor_too_old_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<LocalHttpError>()
        .is_some_and(|error| error.has_code("cursor_too_old"))
}

struct TerminalCleanupGuard {
    raw_mode_enabled: bool,
    alternate_screen_enabled: bool,
    keyboard_enhancement_enabled: bool,
}

impl TerminalCleanupGuard {
    fn new() -> Self {
        Self {
            raw_mode_enabled: false,
            alternate_screen_enabled: false,
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
mod tests {
    use chrono::Utc;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::{
        build_chat_text, centered_rect_rows, chat_text, collect_chat_items,
        determine_alt_screen_mode_for_terminal, is_cursor_too_old_error, is_operator_origin_value,
        paragraph_max_scroll, projection::TuiProjection, AgentListChange, ChatScrollState,
        ComposerState, OverlayState, TuiApp, TuiConnectionState, TuiRuntimeMessage,
    };
    use crate::{
        client::{
            AgentStateSnapshot, AgentStreamEvent, LocalClient, StateSessionSnapshot,
            StateWorkspaceSnapshot, StreamEventEnvelope,
        },
        config::{AltScreenMode, AppConfig},
        system::{ExecutionProfile, ExecutionSnapshot},
        types::{
            AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
            AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentStatus, AgentSummary,
            AgentTokenUsageSummary, AgentVisibility, BriefKind, BriefRecord, ChildAgentSummary,
            ClosureDecision, ClosureOutcome, LoadedAgentsMdView, RuntimePosture, SkillsRuntimeView,
            TokenUsage, TranscriptEntry, TranscriptEntryKind, WaitingIntentSummary,
        },
    };
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Text};

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
        }
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
            lifecycle: AgentLifecycleHint::default(),
            model: AgentModelState {
                effective_model: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6")
                    .unwrap(),
                requested_model: Some(
                    crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
                ),
                active_model: Some(
                    crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
                ),
                fallback_active: false,
                runtime_default_model: crate::config::ModelRef::parse(
                    "anthropic/claude-sonnet-4-6",
                )
                .unwrap(),
                override_model: None,
                source: AgentModelSource::RuntimeDefault,
                effective_fallback_models: Vec::new(),
                resolved_policy: crate::model_catalog::ResolvedRuntimeModelPolicy {
                    model_ref: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6")
                        .unwrap(),
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
                available_models: Vec::new(),
                model_availability: Vec::new(),
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
            briefs_tail: Vec::new(),
            timers: Vec::new(),
            work_items: Vec::new(),
            work_plan: None,
            waiting_intents: Vec::new(),
            external_triggers: Vec::new(),
            operator_notifications: Vec::new(),
            workspace: StateWorkspaceSnapshot::default(),
            execution: None,
            brief: None,
            cursor: Some(cursor.into()),
        }
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
    async fn ctrl_a_opens_agents_overlay() {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(app.overlay, OverlayState::Agents);
    }

    #[tokio::test]
    async fn ctrl_e_opens_events_overlay() {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        let mut projection = TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
        projection.apply_event(
            AgentStreamEvent {
                id: "evt-1".into(),
                event: "provider_round_completed".into(),
                data: StreamEventEnvelope {
                    id: "evt-1".into(),
                    seq: 1,
                    ts: Utc::now(),
                    agent_id: "default".into(),
                    event_type: "provider_round_completed".into(),
                    payload: json!({"text_preview":"partial"}),
                },
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        app.projection = Some(projection);
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(
            app.overlay,
            OverlayState::Events {
                selected_event_id: Some("evt-1".into()),
                detail_scroll: 0
            }
        );
    }

    #[tokio::test]
    async fn agent_overlay_stays_open_while_navigating() {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        app.overlay = OverlayState::Agents;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.overlay, OverlayState::Agents);
    }

    #[tokio::test]
    async fn agent_overlay_enter_keeps_open_on_failed_switch() {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
        app.selected_agent = 1;
        app.overlay = OverlayState::Agents;
        app.connection_state = TuiConnectionState::Streaming;

        let err = app
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap_err();

        assert_eq!(app.overlay, OverlayState::Agents);
        assert_eq!(
            app.status_line,
            format!("Failed to switch to agent beta: {err}")
        );
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
    async fn slash_menu_esc_dismisses_unknown_command_without_clearing_prompt() {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        app.composer = ComposerState::from("/unknown");

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(app.composer.as_str(), "/unknown");
        assert_eq!(app.overlay, OverlayState::None);
        assert_eq!(app.slash_menu_dismissed_for.as_deref(), Some("/unknown"));
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

        assert_eq!(app.overlay, OverlayState::Agents);
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
                    payload: json!({
                        "action": "created",
                        "record": {
                            "id": "work-1",
                            "agent_id": "default",
                            "workspace_id": "agent_home",
                            "delivery_target": "prepare rollout plan",
                            "state": "open",
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
                id: "evt-provider".into(),
                event: "provider_round_completed".into(),
                data: StreamEventEnvelope {
                    id: "evt-provider".into(),
                    seq: 3,
                    ts: Utc::now(),
                    agent_id: "default".into(),
                    event_type: "provider_round_completed".into(),
                    payload: json!({ "round": 1, "text_preview": "hidden provider partial" }),
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
        assert!(!rendered.contains("System (work)"));
        assert!(rendered.contains("Assistant hidden provider partial"));
        assert!(rendered.contains("Action    prepare rollout plan [Open]"));
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
                delivery_target: Some("fix TUI active activity".into()),
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
        assert!(rendered.contains("Current   Improve the Conversation working indicator"));
        assert!(rendered.contains("Assistant ..."));
        assert!(rendered.contains("Action    Still working"));
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
        let mut previous_projection =
            TuiProjection::from_snapshot(sample_snapshot("default", "evt-0"));
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
                    payload: json!({
                        "tool_name": "ExecCommand",
                        "exec_command_cmd": "cargo test tui"
                    }),
                },
            },
            &crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        let mut refreshed_projection = TuiProjection::from_snapshot(snapshot);
        refreshed_projection.inherit_recent_event_logs_from(&previous_projection);
        app.projection = Some(refreshed_projection);

        let rendered: String = build_chat_text(&collect_chat_items(&app))
            .lines
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content))
            .collect();
        assert!(rendered.contains("Action    ExecCommand: cargo test tui"));
        assert!(!rendered.contains("Action    Waiting for activity"));
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
        assert!(rendered.contains("Current   Queued work is waiting to run"));
        assert!(rendered.contains("Assistant ..."));
        assert!(rendered.contains("Action    Waiting for activity"));
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

        assert!(active_item.speaker.starts_with("Holon (working)"));
        assert!(active_item.created_at >= previous_item.created_at);
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
        assert_eq!(items[0].speaker, "You");
        assert_eq!(items[1].speaker, "Holon");
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
                    payload: json!({"text_preview":"older"}),
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
                        payload: json!({"text_preview":"newer"}),
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
            assert_eq!(cached.items, collect_chat_items(&app));
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
        assert_eq!(cached.items, collect_chat_items(&app));

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
        let (tx, rx) = mpsc::unbounded_channel();
        app.connection_state = TuiConnectionState::Streaming;
        app.stream_messages = Some(rx);

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
        let (tx, rx) = mpsc::unbounded_channel();
        app.stream_messages = Some(rx);

        tx.send(TuiRuntimeMessage::Event(AgentStreamEvent {
            id: "evt-stale".into(),
            event: "waiting_intent_created".into(),
            data: StreamEventEnvelope {
                id: "evt-stale".into(),
                seq: 2,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: "waiting_intent_created".into(),
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
    async fn failed_agent_switch_keeps_existing_selection() {
        let client = LocalClient::new(test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
        );
        app.agents = vec![sample_agent_summary("alpha"), sample_agent_summary("beta")];
        app.selected_agent = 0;
        app.connection_state = TuiConnectionState::Streaming;
        app.status_line = "Streaming native events for agent alpha".into();

        let err = app.move_agent_selection(1).await.unwrap_err();

        assert!(err.to_string().contains("/agents/beta/state"));
        assert_eq!(app.selected_agent_id(), Some("alpha"));
        assert!(matches!(
            app.connection_state,
            TuiConnectionState::Streaming
        ));
        assert_eq!(
            app.status_line,
            format!("Failed to switch to agent beta: {err}")
        );
    }
}
