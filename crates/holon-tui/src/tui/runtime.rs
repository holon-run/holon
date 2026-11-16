use super::state::TuiClientState;
use super::*;
use crate::client::{AgentStreamEvent, EventStreamRequest, LocalEventStream, LocalHttpError};
use crate::http_dto::AgentStateSnapshotDto;
use anyhow::Context;
use tokio::sync::mpsc;

const REFRESH_RETRY_DELAY: Duration = Duration::from_secs(1);
const AGENT_LIST_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TASK_LIMIT: usize = 40;
const OPTIMISTIC_OPERATOR_MESSAGE_LIMIT: usize = 64;
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
    Disconnected {
        reason: String,
    },
}

pub(super) enum TuiRuntimeMessage {
    ConversationLoaded {
        generation: u64,
        agent_id: String,
        model: super::conversation::ConversationModel,
    },
    ConversationStatus {
        generation: u64,
        error: String,
        reset: bool,
    },
    ConversationConnected {
        generation: u64,
        agent_id: String,
    },
    Event {
        generation: u64,
        agent_id: String,
        event: AgentStreamEvent,
    },
    Disconnected {
        generation: u64,
        agent_id: String,
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
    EventStreamOpened {
        request_id: u64,
        agent_id: String,
        result: Result<LocalEventStream, EventStreamOpenError>,
    },
}

type SnapshotBootstrapResult = AgentStateSnapshotDto;

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

fn agent_list_retry_delay_for_failures(failures: u32) -> Duration {
    reconnect_delay_for_attempt(failures.saturating_add(1)).max(AGENT_LIST_REFRESH_INTERVAL)
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
    StartupTargetMissing,
}

async fn preflight_agent(client: &LocalClient, agent_id: &str) -> Result<Vec<AgentListEntry>> {
    let roster = client
        .public_agent_roster()
        .await
        .context("failed to load public agent roster")?;
    client.agent_status_compact(agent_id).await.map_err(|err| {
        let reason = match err.downcast_ref::<LocalHttpError>() {
            Some(http) if http.has_code("agent_deleted") || http.status_code == 410 => {
                format!("agent {agent_id} has been deleted")
            }
            Some(http) if http.has_code("agent_deleting") => {
                format!("agent {agent_id} is being deleted")
            }
            Some(http) if http.status_code == 404 && http.has_code("not_found") => {
                format!("agent {agent_id} not found")
            }
            _ => format!("failed to read compact status for agent {agent_id}"),
        };
        err.context(reason)
    })?;
    anyhow::ensure!(
        roster
            .iter()
            .any(|entry| entry.identity.agent_id == agent_id),
        "agent {agent_id} is not in the currently selectable public roster"
    );
    Ok(roster)
}

#[derive(Debug, Clone)]
pub(super) struct TuiRuntimeCheckpoint {
    connection_state: TuiConnectionState,
    reconnect_deadline: Option<Instant>,
    reconnect_attempt: u32,
}

impl TuiApp {
    pub(super) async fn initialize_with_agent(&mut self, agent: Option<String>) -> Result<()> {
        let Some(agent_id) = agent else {
            self.initialize().await;
            return Ok(());
        };
        self.initial_agent_id = Some(agent_id.clone());
        self.persist_selection = false;
        let roster = preflight_agent(&self.client, &agent_id).await?;
        self.apply_loaded_agents(Ok(roster));
        Ok(())
    }

    pub(super) async fn initialize(&mut self) {
        self.schedule_agent_list_refresh();
        self.begin_load_agents();
    }

    pub(super) async fn tick(&mut self) -> Result<()> {
        self.process_runtime_messages();

        if self.initial_agent_id.is_some() {
            if let TuiConnectionState::Disconnected { reason } = &self.connection_state {
                return Err(anyhow!(reason.clone()));
            }
        }

        if self
            .agent_list_refresh_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.begin_load_agents();
        }

        let now = Instant::now();
        let active = self.projection.as_ref().is_some_and(|projection| {
            projection.agent.agent.current_run_id.is_some() || !projection.tasks.is_empty()
        });
        if !self.snapshot_refresh_in_flight && self.state_refresh.is_due(now, active) {
            self.begin_bootstrap_agent_index(self.selected_agent);
        }
        self.sync_diagnostic_stream();

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
                .public_agent_roster()
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
        let entries = match result {
            Ok(entries) => {
                self.agent_list_refresh_failures = 0;
                self.schedule_agent_list_refresh();
                entries
            }
            Err(err) => {
                if self.client.remote_base_url().is_some() {
                    self.agent_list_refresh_failures =
                        self.agent_list_refresh_failures.saturating_add(1);
                    self.schedule_agent_list_refresh_after(agent_list_retry_delay_for_failures(
                        self.agent_list_refresh_failures,
                    ));
                } else {
                    self.schedule_agent_list_refresh();
                }
                if self.agents.is_empty() {
                    self.set_disconnected(format!("failed to list public agents: {err}"));
                } else {
                    self.status_line = format!("Public agent refresh failed: {err}");
                }
                return;
            }
        };
        if self
            .snapshot_refresh_agent_id
            .as_ref()
            .is_some_and(|pending| {
                !entries
                    .iter()
                    .any(|entry| entry.identity.agent_id == *pending)
            })
        {
            self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
            self.snapshot_refresh_in_flight = false;
            self.snapshot_refresh_agent_id = None;
            self.state_refresh.settled(Instant::now(), false);
        }
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
                    if selected.identity.agent_id == agent.identity.agent_id
                        && selected.identity.incarnation == agent.identity.incarnation
                    {
                        return selected.clone();
                    }
                }
                agent
            })
            .collect();
        match self.apply_agent_list(agents) {
            AgentListChange::Ready => {}
            AgentListChange::StartupTargetMissing => {}
            AgentListChange::RequiresBootstrap => self.begin_bootstrap_selected_agent(),
            AgentListChange::Empty => {
                self.set_disconnected("no public agents are available".into());
            }
        }
    }

    pub(super) fn apply_agent_list(&mut self, agents: Vec<AgentSummary>) -> AgentListChange {
        let previously_selected = self.selected_agent_id().map(ToString::to_string);
        self.optimistic_operator_messages.retain(|message| {
            agents
                .iter()
                .any(|agent| agent.identity.agent_id == message.agent_id)
        });
        if let Some(agent_id) = self.initial_agent_id.clone() {
            let Some(index) = find_agent_index(&agents, &agent_id) else {
                self.clear_agent_view();
                self.set_disconnected(format!(
                    "startup agent {agent_id} is no longer in the selectable public roster"
                ));
                return AgentListChange::StartupTargetMissing;
            };
            self.agents = agents;
            self.selected_agent = index;
            return AgentListChange::RequiresBootstrap;
        }
        if agents.is_empty() {
            self.clear_agent_view();
            return AgentListChange::Empty;
        }

        let selected_missing = previously_selected.as_ref().is_some_and(|agent_id| {
            !agents.iter().any(|agent| {
                agent.identity.agent_id == *agent_id
                    && self.projection.as_ref().is_none_or(|projection| {
                        projection.agent.identity.incarnation == agent.identity.incarnation
                    })
            })
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
        if let Some(task) = self.conversation_task.take() {
            task.abort();
        }
        self.conversation_generation = self.conversation_generation.saturating_add(1);
        self.conversation = None;
        self.conversation_history_tx = None;
        self.chat_scroll.follow_tail();
        self.chat_text_cache.borrow_mut().take();
        self.optimistic_operator_messages.clear();
        self.tasks.clear();
        self.projection = None;
        self.last_refresh_at = None;
        self.last_event_at = None;
        self.reconnect_deadline = None;
        self.reconnect_attempt = 0;
        self.snapshot_refresh_in_flight = false;
        self.stream_connect_in_flight = false;
        self.snapshot_refresh_agent_id = None;
        self.state_refresh = Default::default();
        self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
        self.stream_connect_request_id = self.stream_connect_request_id.saturating_add(1);
    }

    pub(super) fn begin_bootstrap_selected_agent(&mut self) {
        let now = Instant::now();
        self.state_refresh.request(now);
        if !self.state_refresh.is_due(now, false) {
            return;
        }
        self.begin_bootstrap_agent_index(self.selected_agent);
    }

    fn begin_conversation(&mut self, agent_id: String) {
        if let Some(task) = self.conversation_task.take() {
            task.abort();
        }
        self.conversation_generation = self.conversation_generation.saturating_add(1);
        self.conversation = None;
        self.chat_scroll.follow_tail();
        let (history_tx, history_rx) = mpsc::channel(16);
        self.conversation_history_tx = Some(history_tx);
        self.conversation_task = Some(tokio::spawn(super::conversation::observe(
            self.client.clone(),
            agent_id,
            self.conversation_generation,
            self.runtime_tx.clone(),
            history_rx,
        )));
        self.chat_text_cache.borrow_mut().take();
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
        if self
            .initial_agent_id
            .as_ref()
            .is_some_and(|initial| initial != &agent_id)
        {
            self.status_line = "Waiting for the explicit startup agent to bootstrap".into();
            return;
        }
        let switching_agents = target_index != self.selected_agent;
        let checkpoint = switching_agents.then(|| self.runtime_checkpoint());
        if self.snapshot_refresh_in_flight
            && self.snapshot_refresh_agent_id.as_deref() == Some(&agent_id)
        {
            self.state_refresh.request(Instant::now());
            return;
        }
        if self.snapshot_refresh_in_flight {
            self.state_refresh = Default::default();
        }
        self.snapshot_refresh_in_flight = true;
        self.snapshot_refresh_agent_id = Some(agent_id.clone());
        self.state_refresh.started(Instant::now());
        self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
        let request_id = self.snapshot_refresh_request_id;
        if self.projection.is_none() || switching_agents {
            self.connection_state = TuiConnectionState::Bootstrapping;
        }
        self.status_line = format!("Loading agent state for {agent_id}");
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn({
            let agent_id = agent_id.clone();
            async move {
                let result = client
                    .agent_state_snapshot(&agent_id)
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
        _target_index: usize,
        agent_id: String,
        checkpoint: Option<TuiRuntimeCheckpoint>,
        result: Result<SnapshotBootstrapResult, String>,
    ) {
        if request_id != self.snapshot_refresh_request_id {
            return;
        }
        if self
            .initial_agent_id
            .as_ref()
            .is_some_and(|initial| initial != &agent_id)
        {
            return;
        }
        let Some(target_index) = self
            .agents
            .iter()
            .position(|agent| agent.identity.agent_id == agent_id)
        else {
            return;
        };
        self.snapshot_refresh_in_flight = false;
        self.snapshot_refresh_agent_id = None;
        self.state_refresh.settled(Instant::now(), result.is_ok());
        let snapshot = match result {
            Ok(result) => result,
            Err(err) => {
                if self.initial_agent_id.is_some() {
                    self.set_disconnected(format!(
                        "failed to bootstrap startup agent {agent_id} from /state: {err}"
                    ));
                    return;
                }
                if let Some(checkpoint) = checkpoint {
                    self.restore_runtime_checkpoint(checkpoint);
                    self.status_line = format!("Failed to switch to agent {agent_id}: {err}");
                } else {
                    self.status_line = format!("Snapshot refresh failed for {agent_id}: {err}");
                }
                return;
            }
        };
        let mut projection = TuiProjection::from_bootstrap_dto(snapshot);
        if projection.agent.identity.agent_id != agent_id
            || projection.agent.identity.incarnation
                != self.agents[target_index].identity.incarnation
        {
            self.state_refresh.hint(Instant::now());
            self.status_line = format!("State identity changed for {agent_id}; retrying");
            return;
        }
        let same_agent = self.projection.as_ref().is_some_and(|current| {
            current.agent.identity.agent_id == agent_id
                && current.agent.identity.incarnation == projection.agent.identity.incarnation
        });
        if same_agent {
            if let Some(previous) = &mut self.projection {
                projection.take_diagnostics_from(previous);
            }
        } else {
            self.stop_stream_task();
            self.reconnect_deadline = None;
            self.reconnect_attempt = 0;
            self.state_refresh = Default::default();
            self.state_refresh.started(Instant::now());
            self.state_refresh.settled(Instant::now(), true);
        }
        self.selected_agent = target_index;
        self.initial_agent_id = None;
        if !same_agent || self.conversation_task.is_none() {
            self.begin_conversation(agent_id.clone());
        }
        self.record_selected_agent(&agent_id);
        self.projection = Some(projection);
        self.apply_projection_view();
        self.last_refresh_at = Some(Local::now());
        if self.conversation.is_some() {
            if matches!(self.connection_state, TuiConnectionState::Bootstrapping) {
                self.connection_state = TuiConnectionState::Streaming;
            }
            self.status_line = format!("Refreshed agent {agent_id} from /state");
        } else {
            self.status_line = format!("Loading conversation for {agent_id}");
        }
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

    pub(super) fn begin_connect_event_stream_for(&mut self, agent_id: String, cursor: Option<u64>) {
        if !matches!(self.overlay, OverlayState::Events { .. }) {
            return;
        }
        self.stream_connect_in_flight = true;
        self.stream_connect_request_id = self.stream_connect_request_id.saturating_add(1);
        let request_id = self.stream_connect_request_id;
        self.reconnect_deadline = None;
        let request = EventStreamRequest {
            after_seq: cursor,
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
        if self.selected_agent_id() != Some(agent_id.as_str())
            || !matches!(self.overlay, OverlayState::Events { .. })
        {
            return;
        }
        match result {
            Ok(stream) => {
                self.spawn_stream_task(stream);
                self.reconnect_attempt = 0;
                self.reconnect_deadline = None;
                self.status_line = "Raw diagnostics connected (live events only)".into();
            }
            Err(err) if err.cursor_not_found => {
                if let Some(projection) = &mut self.projection {
                    projection.cursor = None;
                    projection.clear_event_history();
                }
                self.schedule_reconnect(err.message);
                self.status_line =
                    format!("Raw diagnostic cursor expired for {agent_id}; reconnecting live");
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
        if !self.persist_selection {
            return;
        }
        let mut state = TuiClientState::load_or_new(&self.state_path, agent_id);
        state.set_selected_agent(agent_id);
        if let Err(err) = state.save(&self.state_path) {
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
        let generation = self.stream_connect_request_id;
        let agent_id = self.selected_agent_id().unwrap_or_default().to_string();
        let task = tokio::spawn(async move {
            loop {
                match stream.next_event().await {
                    Ok(event) => {
                        if tx
                            .send(TuiRuntimeMessage::Event {
                                generation,
                                agent_id: agent_id.clone(),
                                event,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(TuiRuntimeMessage::Disconnected {
                            generation,
                            agent_id: agent_id.clone(),
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
        self.stream_connect_in_flight = false;
        self.stream_connect_request_id = self.stream_connect_request_id.saturating_add(1);
    }

    fn accepts_diagnostic_message(&self, generation: u64, agent_id: &str) -> bool {
        generation == self.stream_connect_request_id
            && self.selected_agent_id() == Some(agent_id)
            && matches!(self.overlay, OverlayState::Events { .. })
    }

    fn sync_diagnostic_stream(&mut self) {
        if !matches!(self.overlay, OverlayState::Events { .. }) {
            if self.stream_task.is_some() || self.stream_connect_in_flight {
                self.stop_stream_task();
            }
            self.reconnect_deadline = None;
            return;
        }
        if self.projection.is_some()
            && self.stream_task.is_none()
            && !self.stream_connect_in_flight
            && self
                .reconnect_deadline
                .is_none_or(|deadline| Instant::now() >= deadline)
        {
            self.begin_connect_event_stream();
        }
    }

    fn invalidate_conversation_scope(&mut self) {
        self.stop_stream_task();
        self.reconnect_deadline = None;
        if let Some(projection) = &mut self.projection {
            projection.clear_event_history();
            projection.cursor = None;
            projection.event_log_epoch = None;
        }
        if self.snapshot_refresh_in_flight
            && self.snapshot_refresh_agent_id.as_deref() == self.selected_agent_id()
        {
            self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
            self.snapshot_refresh_in_flight = false;
            self.snapshot_refresh_agent_id = None;
            self.state_refresh.settled(Instant::now(), true);
        }
        self.state_refresh.hint(Instant::now());
    }

    pub(super) fn process_runtime_messages(&mut self) -> bool {
        let mut disconnected = false;
        loop {
            // A failed explicit startup must reach tick before another message can retry it.
            if self.initial_agent_id.is_some()
                && matches!(
                    self.connection_state,
                    TuiConnectionState::Disconnected { .. }
                )
            {
                break;
            }
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
                TuiRuntimeMessage::ConversationStatus {
                    generation,
                    error,
                    reset,
                } => {
                    if generation == self.conversation_generation {
                        if reset && self.conversation.is_some() {
                            self.invalidate_conversation_scope();
                            self.conversation = None;
                            self.chat_scroll.follow_tail();
                            self.chat_text_cache.borrow_mut().take();
                        }
                        self.connection_state = TuiConnectionState::Reconnecting {
                            attempt: 1,
                            retry_after: REFRESH_RETRY_DELAY,
                            last_error: error.clone(),
                        };
                        self.status_line = format!("Conversation reconnecting: {error}");
                    }
                }
                TuiRuntimeMessage::ConversationConnected {
                    generation,
                    agent_id,
                } => {
                    if generation == self.conversation_generation
                        && self.selected_agent_id() == Some(agent_id.as_str())
                    {
                        self.connection_state = TuiConnectionState::Streaming;
                        self.state_refresh.hint(Instant::now());
                    }
                }
                TuiRuntimeMessage::ConversationLoaded {
                    generation,
                    agent_id,
                    model,
                } => {
                    if generation == self.conversation_generation
                        && model.agent_id == agent_id
                        && self.selected_agent_id() == Some(agent_id.as_str())
                        && self
                            .agents
                            .iter()
                            .any(|agent| agent.identity.agent_id == agent_id)
                    {
                        let scope_changed = self.conversation.as_ref().is_some_and(|previous| {
                            let old = previous.state_refresh_head();
                            let new = model.state_refresh_head();
                            (old.0, old.1, old.2) != (new.0, new.1, new.2)
                        });
                        if scope_changed {
                            self.invalidate_conversation_scope();
                        }
                        if self.conversation.as_ref().is_none_or(|previous| {
                            previous.state_refresh_head() != model.state_refresh_head()
                        }) {
                            self.state_refresh.hint(Instant::now());
                        }
                        self.conversation = Some(model);
                        self.chat_text_cache.borrow_mut().take();
                    }
                }
                TuiRuntimeMessage::Event {
                    generation,
                    agent_id,
                    event,
                } => {
                    if self.accepts_diagnostic_message(generation, &agent_id) {
                        self.apply_stream_event(event);
                    }
                }
                TuiRuntimeMessage::Disconnected {
                    generation,
                    agent_id,
                    error,
                } => {
                    if self.accepts_diagnostic_message(generation, &agent_id) {
                        self.stop_stream_task();
                        self.schedule_reconnect(error.clone());
                        self.status_line = format!("Raw diagnostics disconnected: {error}");
                    }
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
                TuiRuntimeMessage::EventStreamOpened {
                    request_id,
                    agent_id,
                    result,
                } => self.apply_event_stream_opened(request_id, agent_id, result),
            }
        }

        disconnected
    }

    pub(super) fn apply_stream_event(&mut self, event: AgentStreamEvent) {
        if let Some(projection) = self.projection.as_mut() {
            projection.append_diagnostic_event(event, &self.log_writer);
            self.last_event_at = Some(Local::now());
        }
    }

    pub(super) fn maybe_begin_load_older_conversation(&mut self) {
        let Some(model) = &self.conversation else {
            self.status_line = "Conversation loading or recovering; history is not ready".into();
            return;
        };
        if model.has_older() && self.chat_scroll.is_at_top(self.chat_max_scroll) {
            self.chat_scroll
                .prepare_for_history_prepend(self.chat_max_scroll);
            if let Some(tx) = &self.conversation_history_tx {
                let _ = tx.try_send(super::conversation::ConversationRequest::Older);
            }
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
        self.status_line = format!(
            "Raw diagnostics retrying in {}: {error}",
            format_duration(retry_after)
        );
    }

    pub(super) fn schedule_agent_list_refresh(&mut self) {
        self.schedule_agent_list_refresh_after(AGENT_LIST_REFRESH_INTERVAL);
    }

    fn schedule_agent_list_refresh_after(&mut self, delay: Duration) {
        self.agent_list_refresh_deadline = Some(Instant::now() + delay);
    }

    pub(super) fn set_disconnected(&mut self, reason: String) {
        self.stop_stream_task();
        self.reconnect_deadline = None;
        self.snapshot_refresh_in_flight = false;
        self.stream_connect_in_flight = false;
        self.snapshot_refresh_agent_id = None;
        self.state_refresh = Default::default();
        self.snapshot_refresh_request_id = self.snapshot_refresh_request_id.saturating_add(1);
        self.stream_connect_request_id = self.stream_connect_request_id.saturating_add(1);
        self.connection_state = TuiConnectionState::Disconnected {
            reason: reason.clone(),
        };
        self.status_line = format!("Disconnected: {reason}");
    }

    fn runtime_checkpoint(&self) -> TuiRuntimeCheckpoint {
        TuiRuntimeCheckpoint {
            connection_state: self.connection_state.clone(),
            reconnect_deadline: self.reconnect_deadline,
            reconnect_attempt: self.reconnect_attempt,
        }
    }

    fn restore_runtime_checkpoint(&mut self, checkpoint: TuiRuntimeCheckpoint) {
        self.connection_state = checkpoint.connection_state;
        self.reconnect_deadline = checkpoint.reconnect_deadline;
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
            TuiConnectionState::Disconnected { .. } => "disconnected".into(),
        };
        format!("{state} via {}", self.client.connection_summary())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn connection_detail(&self) -> Option<&str> {
        match &self.connection_state {
            TuiConnectionState::Reconnecting { last_error, .. } => Some(last_error.as_str()),
            TuiConnectionState::Disconnected { reason } => Some(reason.as_str()),
            TuiConnectionState::Bootstrapping | TuiConnectionState::Streaming => None,
        }
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
        if let Some(task) = self.conversation_task.take() {
            task.abort();
        }
    }
}

pub(super) fn is_cursor_not_found_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<LocalHttpError>()
        .is_some_and(|error| error.has_code("cursor_not_found"))
}
