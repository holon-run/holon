use super::chat::CachedChatText;
use super::state::{tui_state_path, TuiClientState};
use super::*;
use std::cell::RefCell;
use std::path::PathBuf;
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};

pub(super) struct TuiApp {
    pub(super) client: LocalClient,
    pub(super) agents: Vec<AgentSummary>,
    pub(super) briefs: Vec<BriefRecord>,
    pub(super) transcript: Vec<TranscriptEntry>,
    pub(super) optimistic_operator_messages: Vec<OperatorMessageRecord>,
    pub(super) tasks: Vec<TaskRecord>,
    pub(super) projection: Option<TuiProjection>,
    pub(super) connection_state: TuiConnectionState,
    pub(super) runtime_tx: UnboundedSender<TuiRuntimeMessage>,
    pub(super) runtime_messages: UnboundedReceiver<TuiRuntimeMessage>,
    pub(super) stream_task: Option<JoinHandle<()>>,
    pub(super) agent_list_refresh_deadline: Option<Instant>,
    pub(super) reconnect_deadline: Option<Instant>,
    pub(super) refresh_deadline: Option<Instant>,
    pub(super) agent_list_refresh_in_flight: bool,
    pub(super) snapshot_refresh_in_flight: bool,
    pub(super) stream_connect_in_flight: bool,
    pub(super) snapshot_refresh_request_id: u64,
    pub(super) stream_connect_request_id: u64,
    pub(super) reconnect_attempt: u32,
    pub(super) selected_agent: usize,
    pub(super) preferred_agent_id: Option<String>,
    pub(super) state_path: PathBuf,
    pub(super) chat_scroll: ChatScrollState,
    pub(super) chat_max_scroll: u16,
    pub(super) composer: ComposerState,
    pub(super) slash_menu_selected: usize,
    pub(super) slash_menu_dismissed_for: Option<String>,
    pub(super) overlay: OverlayState,
    pub(super) last_refresh_at: Option<DateTime<Local>>,
    pub(super) last_event_at: Option<DateTime<Local>>,
    pub(super) display_mode: OperatorDisplayMode,
    pub(crate) status_line: String,
    pub(super) should_quit: bool,
    pub(super) chat_text_cache: RefCell<Option<CachedChatText>>,
    pub(super) input_history: Vec<String>,
    pub(super) history_index: Option<usize>,
    pub(super) log_writer: TuiLogWriter,
}

impl TuiApp {
    pub(crate) fn new(client: LocalClient, log_writer: TuiLogWriter) -> Self {
        let connection_summary = client.connection_summary();
        let state_path = tui_state_path(&client);
        let preferred_agent_id = TuiClientState::load(&state_path)
            .ok()
            .map(|state| state.last_selected_agent_id);
        let (runtime_tx, runtime_messages) = mpsc::unbounded_channel();
        Self {
            client,
            agents: Vec::new(),
            briefs: Vec::new(),
            transcript: Vec::new(),
            optimistic_operator_messages: Vec::new(),
            tasks: Vec::new(),
            projection: None,
            connection_state: TuiConnectionState::Bootstrapping,
            runtime_tx,
            runtime_messages,
            stream_task: None,
            agent_list_refresh_deadline: None,
            reconnect_deadline: None,
            refresh_deadline: None,
            agent_list_refresh_in_flight: false,
            snapshot_refresh_in_flight: false,
            stream_connect_in_flight: false,
            snapshot_refresh_request_id: 0,
            stream_connect_request_id: 0,
            reconnect_attempt: 0,
            selected_agent: 0,
            preferred_agent_id,
            state_path,
            chat_scroll: ChatScrollState::new(),
            chat_max_scroll: 0,
            composer: ComposerState::new(),
            slash_menu_selected: 0,
            slash_menu_dismissed_for: None,
            overlay: OverlayState::None,
            last_refresh_at: None,
            last_event_at: None,
            display_mode: OperatorDisplayMode::DEFAULT,
            status_line: format!("Connecting to {connection_summary}..."),
            should_quit: false,
            chat_text_cache: RefCell::new(None),
            input_history: Vec::new(),
            history_index: None,
            log_writer,
        }
    }
}
