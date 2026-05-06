use super::chat::CachedChatText;
use super::*;
use std::cell::RefCell;
use tokio::{sync::mpsc, task::JoinHandle};

pub(super) struct TuiApp {
    pub(super) client: LocalClient,
    pub(super) agents: Vec<AgentSummary>,
    pub(super) briefs: Vec<BriefRecord>,
    pub(super) transcript: Vec<TranscriptEntry>,
    pub(super) optimistic_operator_messages: Vec<OperatorMessageRecord>,
    pub(super) tasks: Vec<TaskRecord>,
    pub(super) projection: Option<TuiProjection>,
    pub(super) connection_state: TuiConnectionState,
    pub(super) stream_messages: Option<mpsc::UnboundedReceiver<TuiRuntimeMessage>>,
    pub(super) stream_task: Option<JoinHandle<()>>,
    pub(super) agent_list_refresh_deadline: Option<Instant>,
    pub(super) reconnect_deadline: Option<Instant>,
    pub(super) refresh_deadline: Option<Instant>,
    pub(super) reconnect_attempt: u32,
    pub(super) selected_agent: usize,
    pub(super) chat_scroll: ChatScrollState,
    pub(super) chat_max_scroll: u16,
    pub(super) composer: ComposerState,
    pub(super) slash_menu_selected: usize,
    pub(super) slash_menu_dismissed_for: Option<String>,
    pub(super) overlay: OverlayState,
    pub(super) last_refresh_at: Option<DateTime<Local>>,
    pub(super) last_event_at: Option<DateTime<Local>>,
    pub(super) display_level: OperatorVisibility,
    pub(crate) status_line: String,
    pub(super) should_quit: bool,
    pub(super) chat_text_cache: RefCell<Option<CachedChatText>>,
    pub(super) input_history: Vec<String>,
    pub(super) history_index: Option<usize>,
    pub(super) log_writer: TuiLogWriter,
}

impl TuiApp {
    pub(crate) fn new(client: LocalClient, log_writer: TuiLogWriter) -> Self {
        Self {
            client,
            agents: Vec::new(),
            briefs: Vec::new(),
            transcript: Vec::new(),
            optimistic_operator_messages: Vec::new(),
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
            display_level: OperatorVisibility::DEFAULT_DISPLAY_LEVEL,
            status_line: "Connecting to local Holon runtime...".into(),
            should_quit: false,
            chat_text_cache: RefCell::new(None),
            input_history: Vec::new(),
            history_index: None,
            log_writer,
        }
    }
}
