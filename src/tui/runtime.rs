use super::projection::ProjectionSlice;
use super::state::TuiClientState;
use super::*;
use crate::client::{
    AgentStateSnapshot, AgentStreamEvent, EventPageRequest, EventPageResponse, EventStreamRequest,
    LocalEventStream, LocalHttpError, StreamEventEnvelope,
};
use tokio::sync::mpsc;

const REFRESH_RETRY_DELAY: Duration = Duration::from_secs(1);
const AGENT_LIST_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TASK_LIMIT: usize = 40;
const OPTIMISTIC_OPERATOR_MESSAGE_LIMIT: usize = 64;
pub(super) const BOOTSTRAP_EVENT_TAIL_LIMIT: usize = 50;
const EVENT_HISTORY_PAGE_LIMIT: usize = 128;
const STREAM_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) enum TuiConnectionState {
    Bootstrapping,
    Streaming,
    Reconnecting {
        attempt: u32,
        retry_after: Duration,
        last_error: String,
    },
    RefreshRequired {
        reason: String,
    },
    Disconnected {
        reason: String,
    },
}

pub(super) enum TuiRuntimeMessage {
    Event(AgentStreamEvent),
    Disconnected {
        error: String,
    },
    AgentListLoaded(Result<Vec<AgentListEntry>, String>),
    ModelsLoaded(Result<Vec<ResolvedModelAvailability>, String>),
    SnapshotLoaded {
        request_id: u64,
        target_index: usize,
        agent_id: String,
        checkpoint: Option<TuiRuntimeCheckpoint>,
        result: Result<SnapshotBootstrapResult, String>,
    },
    SnapshotBootstrapStatus {
        request_id: u64,
        agent_id: String,
        status_line: String,
    },
    EventHistoryPageLoaded {
        request_id: u64,
        agent_id: String,
        result: Result<EventPageResponse, String>,
    },
    EventStreamOpened {
        request_id: u64,
        agent_id: String,
        result: Result<LocalEventStream, EventStreamOpenError>,
    },
}

type SnapshotBootstrapResult = (
    AgentStateSnapshot,
    Vec<StreamEventEnvelope>,
    Option<String>,
    Option<String>,
    bool,
);

pub(super) struct EventStreamOpenError {
    message: String,
    cursor_not_found: bool,
}

pub(super) fn reconnect_delay_for_attempt(attempt: u32) -> Duration {
    let delay_secs = match attempt {
        0 | 1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        5 => 15,
        _ => STREAM_RECONNECT_MAX_DELAY.as_secs(),
    };
    Duration::from_secs(delay_secs).min(STREAM_RECONNECT_MAX_DELAY)
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 && duration.subsec_millis() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentListChange {
    Ready,
    RequiresBootstrap,
    Empty,
}

#[derive(Debug, Clone)]
pub(super) struct TuiRuntimeCheckpoint {
    connection_state: TuiConnectionState,
    reconnect_deadline: Option<Instant>,
    refresh_deadline: Option<Instant>,
    reconnect_attempt: u32,
}

impl TuiApp {
    pub(super) async fn initialize(&mut self) {
        self.schedule_agent_list_refresh();
        self.begin_load_agents();
    }

    pub(super) async fn tick(&mut self) -> Result<()> {
        self.process_runtime_messages();

        if self
            .agent_list_refresh_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.begin_load_agents();
        }

        match self.connection_state.clone() {
            TuiConnectionState::RefreshRequired { .. } => {
                if self
                    .refresh_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    self.begin_bootstrap_selected_agent();
                }
            }
            TuiConnectionState::Reconnecting { .. } => {
                if self
                    .reconnect_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    self.begin_connect_event_stream();
                }
            }
            TuiConnectionState::Bootstrapping
            | TuiConnectionState::Streaming
            | TuiConnectionState::Disconnected { .. } => {}
        }

        Ok(())
    }

    pub(super) fn begin_load_agents(&mut self) {
        if self.agent_list_refresh_in_flight {
            return;
        }
        self.agent_list_refresh_in_flight = true;
        self.agent_list_refresh_deadline = None;
        if self.agents.is_empty() {
            self.connection_state = TuiConnectionState::Bootstrapping;
            self.status_line = "Loading public agents".into();
        }
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_agent_entries()
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(TuiRuntimeMessage::AgentListLoaded(result));
        });
    }

    pub(super) fn begin_load_models(&mut self) {
        if self.model_availability_load_in_flight {
            return;
        }
        self.model_availability_load_in_flight = true;
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn(async move {
            let result = client
                .fetch_models()
                .await
                .map(|models| models.model_availability)
                .map_err(|err| err.to_string());
            let _ = tx.send(TuiRuntimeMessage::ModelsLoaded(result));
        });
    }

    pub(super) fn apply_loaded_models(
        &mut self,
        result: Result<Vec<ResolvedModelAvailability>, String>,
    ) {
        self.model_availability_load_in_flight = false;
        match result {
            Ok(model_availability) => self.model_availability = model_availability,
            Err(error) => self.status_line = format!("Model availability load failed: {error}"),
        }
    }

    pub(super) fn apply_loaded_agents(&mut self, result: Result<Vec<AgentListEntry>, String>) {
        self.agent_list_refresh_in_flight = false;
        self.schedule_agent_list_refresh();
        let entries = match result {
            Ok(entries) => entries,
            Err(err) => {
                if self.agents.is_empty() {
                    self.set_disconnected(format!("failed to list public agents: {err}"));
                } else {
                    self.status_line = format!("Public agent refresh failed: {err}");
                }
                return;
            }
        };
        let selected_projection_agent = self.projection.as_ref().and_then(|projection| {
            let selected_agent_id = self.selected_agent_id()?;
            (projection.agent.identity.agent_id == selected_agent_id)
                .then(|| projection.agent.clone())
        });
        let agents = entries
            .into_iter()
            .map(|entry| {
                let agent = entry.into_agent_summary_placeholder();
                if let Some(selected) = selected_projection_agent.as_ref() {
                    if selected.identity.agent_id == agent.identity.agent_id {
                        return selected.clone();
                    }
                }
                agent
            })
            .collect();
        match self.apply_agent_list(agents) {
            AgentListChange::Ready => {}
            AgentListChange::RequiresBootstrap => self.begin_bootstrap_selected_agent(),
            AgentListChange::Empty => {
                self.set_disconnected("no public agents are available".into());
            }
        }
    }

    pub(super) fn apply_agent_list(&mut self, agents: Vec<AgentSummary>) -> AgentListChange {
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
            .as_deref()
            .and_then(|agent_id| find_agent_index(&agents, agent_id))
            .or_else(|| {
                self.preferred_agent_id
                    .as_deref()
                    .and_then(|agent_id| find_agent_index(&agents, agent_id))
            })
            .or_else(|| find_agent_index(&agents, self.client.default_agent_id()))
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

    pub(super) fn selected_agent_id(&self) -> Option<&str> {
        self.agents
            .get(self.selected_agent)
            .map(|agent| agent.identity.agent_id.as_str())
    }

    pub(super) fn selected_agent_summary(&self) -> Option<&AgentSummary> {
        self.agents.get(self.selected_agent)
    }

    pub(super) fn add_optimistic_operator_message(
        &mut self,
        agent_id: String,
        body: String,
    ) -> String {
        let message_id = format!("local-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now();
        self.optimistic_operator_messages
            .push(OperatorMessageRecord {
                message_id: message_id.clone(),
                agent_id,
                status: OperatorMessageStatus::Sending,
                created_at: now,
                updated_at: now,
                body: MessageBody::Text { text: body },
                error: None,
            });
        *self.chat_text_cache.borrow_mut() = None;
        message_id
    }

    pub(super) fn reconcile_optimistic_operator_message(
        &mut self,
        local_message_id: &str,
        accepted_id: &str,
    ) {
        if let Some(message) = self
            .optimistic_operator_messages
            .iter_mut()
            .find(|message| message.message_id == local_message_id)
        {
            message.message_id = accepted_id.to_string();
            message.status = OperatorMessageStatus::Queued;
            message.updated_at = chrono::Utc::now();
            message.error = None;
        }
        *self.chat_text_cache.borrow_mut() = None;
    }

    pub(super) fn fail_optimistic_operator_message(
        &mut self,
        local_message_id: &str,
        error: String,
    ) {
        if let Some(message) = self
            .optimistic_operator_messages
            .iter_mut()
            .find(|message| message.message_id == local_message_id)
        {
            message.status = OperatorMessageStatus::Failed;
            message.updated_at = chrono::Utc::now();
            message.error = Some(error);
        }
        *self.chat_text_cache.borrow_mut() = None;
    }

    pub(super) fn prune_optimistic_operator_messages(&mut self) {
        let Some(projection) = self.projection.as_ref() else {
            return;
        };

        let durable_message_ids = projection.durable_operator_message_ids();

        self.optimistic_operator_messages
            .retain(|message| !durable_message_ids.contains(&message.message_id));
        if self.optimistic_operator_messages.len() > OPTIMISTIC_OPERATOR_MESSAGE_LIMIT {
            self.optimistic_operator_messages.drain(
                0..self.optimistic_operator_messages.len() - OPTIMISTIC_OPERATOR_MESSAGE_LIMIT,
            );
        }
    }

    pub(super) fn clear_agent_view(&mut self) {
        self.clear_projection_view();
        self.agents.clear();
        self.selected_agent = 0;
    }

    pub(super) fn clear_projection_view(&mut self) {
        self.stop_stream_task();
        self.optimistic_operator_messages.clear();
        self.tasks.clear();
        self.projection = None;
        self.last_refresh_at = None;
        self.last_event_at = None;
        self.refresh_deadline = None;
        self.reconnect_deadline = None;
        self.reconnect_attempt = 0;
        self.snapshot_refresh_in_flight = false;
        self.stream_connect_in_flight = false;
        self.event_history_load_in_flight = false;
        self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
        self.stream_connect_request_id = self.stream_connect_request_id.saturating_add(1);
        self.event_history_request_id = self.event_history_request_id.saturating_add(1);
    }

    pub(super) fn begin_bootstrap_selected_agent(&mut self) {
        self.begin_bootstrap_agent_index(self.selected_agent);
    }

    pub(super) fn begin_bootstrap_agent_index(&mut self, target_index: usize) {
        let Some(agent_id) = self
            .agents
            .get(target_index)
            .map(|agent| agent.identity.agent_id.clone())
        else {
            self.status_line = "No agent selected".into();
            return;
        };
        let switching_agents = target_index != self.selected_agent;
        let checkpoint = switching_agents.then(|| self.runtime_checkpoint());
        self.refresh_deadline = None;
        self.reconnect_deadline = None;
        self.snapshot_refresh_in_flight = true;
        self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
        self.event_history_load_in_flight = false;
        self.event_history_request_id = self.event_history_request_id.saturating_add(1);
        let request_id = self.snapshot_refresh_request_id;
        self.connection_state = TuiConnectionState::Bootstrapping;
        self.status_line = format!("Loading agent state for {agent_id}");
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn({
            let agent_id = agent_id.clone();
            async move {
                let result = async {
                    let snapshot = client.agent_state_snapshot(&agent_id).await?;
                    let _ = tx.send(TuiRuntimeMessage::SnapshotBootstrapStatus {
                        request_id,
                        agent_id: agent_id.clone(),
                        status_line: format!("Loading recent events for {agent_id}"),
                    });
                    let mut events_page = client
                        .agent_events_page(
                            &agent_id,
                            EventPageRequest {
                                limit: Some(BOOTSTRAP_EVENT_TAIL_LIMIT),
                                order: Some("desc".into()),
                                ..Default::default()
                            },
                        )
                        .await?;
                    let newest_cursor = events_page.newest_cursor.clone();
                    events_page.events.reverse();
                    let oldest_cursor = events_page.oldest_cursor;
                    let has_older = events_page.has_older;
                    anyhow::Ok((
                        snapshot,
                        events_page.events,
                        newest_cursor,
                        oldest_cursor,
                        has_older,
                    ))
                }
                .await
                .map_err(|err| err.to_string());
                let _ = tx.send(TuiRuntimeMessage::SnapshotLoaded {
                    request_id,
                    target_index,
                    agent_id,
                    checkpoint,
                    result,
                });
            }
        });
    }

    pub(super) fn apply_snapshot_result(
        &mut self,
        request_id: u64,
        target_index: usize,
        agent_id: String,
        checkpoint: Option<TuiRuntimeCheckpoint>,
        result: Result<SnapshotBootstrapResult, String>,
    ) {
        if request_id != self.snapshot_refresh_request_id {
            return;
        }
        self.snapshot_refresh_in_flight = false;
        let (snapshot, events_tail, newest_cursor, oldest_cursor, has_older) = match result {
            Ok(result) => result,
            Err(err) => {
                if let Some(checkpoint) = checkpoint {
                    self.restore_runtime_checkpoint(checkpoint);
                    self.status_line = format!("Failed to switch to agent {agent_id}: {err}");
                } else {
                    let reason = format!("failed to bootstrap {agent_id} from /state: {err}");
                    self.schedule_refresh(reason);
                    self.status_line = format!("Snapshot refresh failed for {agent_id}: {err}");
                }
                return;
            }
        };
        let same_agent = self
            .projection
            .as_ref()
            .is_some_and(|current| current.agent.identity.agent_id == agent_id);
        let mut projection = if same_agent {
            self.chat_scroll
                .preserve_across_refresh(self.chat_max_scroll);
            if let Some(mut projection) = self.projection.take() {
                projection.reset_from_snapshot_preserving_event_history(snapshot);
                projection
            } else {
                TuiProjection::from_snapshot(snapshot)
            }
        } else {
            TuiProjection::from_snapshot(snapshot)
        };
        projection.merge_event_tail(events_tail, newest_cursor);
        projection.set_event_history_state_from_tail(oldest_cursor, has_older);
        let cursor = projection.cursor.clone();

        self.stop_stream_task();
        self.selected_agent = target_index;
        self.record_selected_agent(&agent_id);
        self.projection = Some(projection);
        self.apply_projection_view();
        self.last_refresh_at = Some(Local::now());
        self.last_event_at = None;
        self.reconnect_attempt = 0;
        self.reconnect_deadline = None;
        self.status_line = format!("Bootstrapped agent {agent_id} from /state");

        self.begin_connect_event_stream_for(agent_id, cursor);
    }

    pub(super) fn apply_snapshot_bootstrap_status(
        &mut self,
        request_id: u64,
        agent_id: String,
        status_line: String,
    ) {
        if request_id != self.snapshot_refresh_request_id {
            return;
        }
        if !matches!(self.connection_state, TuiConnectionState::Bootstrapping) {
            return;
        }
        let _ = agent_id;
        self.status_line = status_line;
    }

    pub(super) fn begin_connect_event_stream(&mut self) {
        let Some(agent_id) = self.selected_agent_id().map(ToString::to_string) else {
            self.status_line = "No agent selected".into();
            return;
        };
        let cursor = self
            .projection
            .as_ref()
            .and_then(|projection| projection.cursor.clone());
        self.begin_connect_event_stream_for(agent_id, cursor);
    }

    pub(super) fn begin_connect_event_stream_for(
        &mut self,
        agent_id: String,
        cursor: Option<String>,
    ) {
        self.stream_connect_in_flight = true;
        self.stream_connect_request_id = self.stream_connect_request_id.saturating_add(1);
        let request_id = self.stream_connect_request_id;
        self.reconnect_deadline = None;
        let request = EventStreamRequest {
            cursor,
            ..Default::default()
        };
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn({
            let agent_id = agent_id.clone();
            async move {
                let result = client
                    .stream_agent_events(&agent_id, request)
                    .await
                    .map_err(|err| EventStreamOpenError {
                        cursor_not_found: is_cursor_not_found_error(&err),
                        message: err.to_string(),
                    });
                let _ = tx.send(TuiRuntimeMessage::EventStreamOpened {
                    request_id,
                    agent_id,
                    result,
                });
            }
        });
    }

    pub(super) fn apply_event_stream_opened(
        &mut self,
        request_id: u64,
        agent_id: String,
        result: Result<LocalEventStream, EventStreamOpenError>,
    ) {
        if request_id != self.stream_connect_request_id {
            return;
        }
        self.stream_connect_in_flight = false;
        match result {
            Ok(stream) => {
                self.spawn_stream_task(stream);
                self.connection_state = TuiConnectionState::Streaming;
                self.reconnect_attempt = 0;
                self.reconnect_deadline = None;
                self.refresh_deadline = None;
                self.status_line.clear();
            }
            Err(err) if err.cursor_not_found => {
                self.schedule_refresh(format!(
                    "replay cursor not found for {agent_id}; rebuilding from /state"
                ));
                self.status_line =
                    format!("Replay cursor not found for {agent_id}; resetting from /state");
            }
            Err(err) => {
                self.schedule_reconnect(err.message.clone());
                self.status_line =
                    format!("Event stream disconnected for {agent_id}: {}", err.message);
            }
        };
    }

    pub(super) fn record_selected_agent(&mut self, agent_id: &str) {
        self.preferred_agent_id = Some(agent_id.to_string());
        if let Err(err) = TuiClientState::new(agent_id).save(&self.state_path) {
            tracing::warn!(
                error = %err,
                path = %self.state_path.display(),
                "failed to persist TUI selected agent"
            );
        }
    }

    pub(super) fn next_agent_index_from(&self, selected: usize, delta: i32) -> Option<usize> {
        if self.agents.is_empty() {
            return None;
        }
        let selected = selected.min(self.agents.len().saturating_sub(1));

        Some(if delta > 0 {
            (selected + 1) % self.agents.len()
        } else if selected == 0 {
            self.agents.len() - 1
        } else {
            selected - 1
        })
    }

    pub(super) fn spawn_stream_task(&mut self, mut stream: LocalEventStream) {
        self.stop_stream_task();
        let tx = self.runtime_tx.clone();
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
        self.stream_task = Some(task);
    }

    pub(super) fn stop_stream_task(&mut self) {
        if let Some(task) = self.stream_task.take() {
            task.abort();
        }
    }

    pub(super) fn process_runtime_messages(&mut self) -> bool {
        let mut disconnected = false;
        loop {
            let message = match self.runtime_messages.try_recv() {
                Ok(message) => Some(message),
                Err(mpsc::error::TryRecvError::Empty) => None,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    disconnected = true;
                    None
                }
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
                TuiRuntimeMessage::AgentListLoaded(result) => self.apply_loaded_agents(result),
                TuiRuntimeMessage::ModelsLoaded(result) => self.apply_loaded_models(result),
                TuiRuntimeMessage::SnapshotLoaded {
                    request_id,
                    target_index,
                    agent_id,
                    checkpoint,
                    result,
                } => self.apply_snapshot_result(
                    request_id,
                    target_index,
                    agent_id,
                    checkpoint,
                    result,
                ),
                TuiRuntimeMessage::SnapshotBootstrapStatus {
                    request_id,
                    agent_id,
                    status_line,
                } => self.apply_snapshot_bootstrap_status(request_id, agent_id, status_line),
                TuiRuntimeMessage::EventHistoryPageLoaded {
                    request_id,
                    agent_id,
                    result,
                } => self.apply_event_history_page_result(request_id, agent_id, result),
                TuiRuntimeMessage::EventStreamOpened {
                    request_id,
                    agent_id,
                    result,
                } => self.apply_event_stream_opened(request_id, agent_id, result),
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

    pub(super) fn apply_stream_event(&mut self, event: AgentStreamEvent) {
        if let Some(projection) = self.projection.as_mut() {
            projection.apply_event(event, &self.log_writer);
            self.last_event_at = Some(Local::now());
            self.apply_projection_view();
            self.schedule_projection_refresh_if_stale();
        }
    }

    pub(super) fn maybe_begin_load_older_events(&mut self) {
        if self.event_history_load_in_flight {
            return;
        }
        if !self.chat_scroll.is_at_top(self.chat_max_scroll) {
            return;
        }
        let Some(agent_id) = self.selected_agent_id().map(ToString::to_string) else {
            return;
        };
        let Some(projection) = self.projection.as_ref() else {
            return;
        };
        if !projection.history_has_older {
            return;
        }
        let Some(cursor) = projection.history_oldest_cursor.clone() else {
            return;
        };

        self.event_history_load_in_flight = true;
        self.event_history_request_id = self.event_history_request_id.saturating_add(1);
        let request_id = self.event_history_request_id;
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        self.status_line = "Loading older events".into();
        tokio::spawn({
            let agent_id = agent_id.clone();
            async move {
                let result = client
                    .agent_events_page(
                        &agent_id,
                        EventPageRequest {
                            cursor: Some(cursor),
                            limit: Some(EVENT_HISTORY_PAGE_LIMIT),
                            order: Some("desc".into()),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|err| err.to_string());
                let _ = tx.send(TuiRuntimeMessage::EventHistoryPageLoaded {
                    request_id,
                    agent_id,
                    result,
                });
            }
        });
    }

    pub(super) fn apply_event_history_page_result(
        &mut self,
        request_id: u64,
        agent_id: String,
        result: Result<EventPageResponse, String>,
    ) {
        if request_id != self.event_history_request_id {
            return;
        }
        self.event_history_load_in_flight = false;
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                self.status_line = format!("Older event load failed for {agent_id}: {error}");
                return;
            }
        };
        self.chat_scroll
            .prepare_for_history_prepend(self.chat_max_scroll);
        let (added, has_older) = {
            let Some(projection) = self.projection.as_mut() else {
                return;
            };
            if projection.agent.identity.agent_id != agent_id {
                return;
            }
            let added = projection.prepend_event_history_page(
                page.events,
                page.oldest_cursor,
                page.has_older,
            );
            (added, projection.history_has_older)
        };
        if added > 0 {
            self.chat_text_cache.borrow_mut().take();
            self.apply_projection_view();
            self.status_line = format!("Loaded {added} older events");
        } else if has_older {
            self.status_line = "No new older events in page".into();
        } else if self
            .projection
            .as_ref()
            .is_some_and(|projection| projection.event_history_at_local_cap())
        {
            self.status_line = "Reached local event history limit".into();
        } else {
            self.status_line = "Reached beginning of event history".into();
        }
    }

    pub(super) fn apply_projection_view(&mut self) {
        let Some(projection) = self.projection.as_ref() else {
            return;
        };

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
        self.prune_optimistic_operator_messages();
    }

    pub(super) fn schedule_reconnect(&mut self, error: String) {
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        let retry_after = reconnect_delay_for_attempt(self.reconnect_attempt);
        self.reconnect_deadline = Some(Instant::now() + retry_after);
        self.refresh_deadline = None;
        self.connection_state = TuiConnectionState::Reconnecting {
            attempt: self.reconnect_attempt,
            retry_after,
            last_error: error,
        };
    }

    pub(super) fn schedule_refresh(&mut self, reason: String) {
        self.refresh_deadline = Some(Instant::now() + REFRESH_RETRY_DELAY);
        self.reconnect_deadline = None;
        self.connection_state = TuiConnectionState::RefreshRequired { reason };
    }

    pub(super) fn schedule_projection_refresh_if_stale(&mut self) {
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

    pub(super) fn schedule_agent_list_refresh(&mut self) {
        self.agent_list_refresh_deadline = Some(Instant::now() + AGENT_LIST_REFRESH_INTERVAL);
    }

    pub(super) fn set_disconnected(&mut self, reason: String) {
        self.stop_stream_task();
        self.refresh_deadline = None;
        self.reconnect_deadline = None;
        self.snapshot_refresh_in_flight = false;
        self.stream_connect_in_flight = false;
        self.event_history_load_in_flight = false;
        self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
        self.stream_connect_request_id = self.stream_connect_request_id.saturating_add(1);
        self.event_history_request_id = self.event_history_request_id.saturating_add(1);
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

    pub(super) fn connection_label(&self) -> String {
        let state = match &self.connection_state {
            TuiConnectionState::Bootstrapping => "bootstrapping".into(),
            TuiConnectionState::Streaming => "streaming".into(),
            TuiConnectionState::Reconnecting {
                attempt,
                retry_after,
                ..
            } => {
                format!(
                    "reconnecting in {} (attempt {attempt})",
                    format_duration(*retry_after)
                )
            }
            TuiConnectionState::RefreshRequired { .. } => "refresh-required".into(),
            TuiConnectionState::Disconnected { .. } => "disconnected".into(),
        };
        format!("{state} via {}", self.client.connection_summary())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn connection_detail(&self) -> Option<&str> {
        match &self.connection_state {
            TuiConnectionState::Reconnecting { last_error, .. } => Some(last_error.as_str()),
            TuiConnectionState::RefreshRequired { reason } => Some(reason.as_str()),
            TuiConnectionState::Disconnected { reason } => Some(reason.as_str()),
            TuiConnectionState::Bootstrapping | TuiConnectionState::Streaming => None,
        }
    }

    pub(super) fn stale_slice_summary(&self) -> Option<String> {
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
                ProjectionSlice::Timers => "timers",
                ProjectionSlice::WorkItems => "work-items",
                ProjectionSlice::WaitingIntents => "waiting",
                ProjectionSlice::ExternalTriggers => "external-triggers",
                ProjectionSlice::OperatorNotifications => "operator-notifications",
                ProjectionSlice::Workspace => "workspace",
            })
            .collect::<Vec<_>>();
        Some(labels.join(", "))
    }
}

fn find_agent_index(agents: &[AgentSummary], agent_id: &str) -> Option<usize> {
    agents
        .iter()
        .position(|agent| agent.identity.agent_id == agent_id)
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        self.stop_stream_task();
    }
}

pub(super) fn is_cursor_not_found_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<LocalHttpError>()
        .is_some_and(|error| error.has_code("cursor_not_found"))
}
