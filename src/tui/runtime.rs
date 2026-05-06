use super::projection::ProjectionSlice;
use super::*;
use crate::client::{AgentStreamEvent, EventStreamRequest, LocalEventStream, LocalHttpError};
use tokio::sync::mpsc;

const STREAM_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const REFRESH_RETRY_DELAY: Duration = Duration::from_secs(1);
const AGENT_LIST_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const BRIEF_LIMIT: usize = 24;
const TRANSCRIPT_LIMIT: usize = 40;
const TASK_LIMIT: usize = 40;
const OPTIMISTIC_OPERATOR_MESSAGE_LIMIT: usize = 64;

#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) enum TuiConnectionState {
    Bootstrapping,
    Streaming,
    Reconnecting { attempt: u32, last_error: String },
    RefreshRequired { reason: String },
    Disconnected { reason: String },
}

#[derive(Debug)]
pub(super) enum TuiRuntimeMessage {
    Event(AgentStreamEvent),
    Disconnected { error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentListChange {
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
    pub(super) async fn initialize(&mut self) {
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

    pub(super) async fn tick(&mut self) -> Result<()> {
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

    pub(super) async fn load_agents(&mut self) -> Result<()> {
        let agents = self.client.list_agents().await?;
        let _ = self.apply_agent_list(agents);
        Ok(())
    }

    pub(super) async fn refresh_public_agents(&mut self) -> Result<()> {
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

        let mut durable_message_ids = projection
            .operator_messages
            .iter()
            .map(|message| message.message_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        durable_message_ids.extend(
            self.transcript
                .iter()
                .filter_map(|entry| entry.related_message_id.clone()),
        );

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
        self.briefs.clear();
        self.transcript.clear();
        self.optimistic_operator_messages.clear();
        self.tasks.clear();
        self.projection = None;
        self.last_refresh_at = None;
        self.last_event_at = None;
        self.refresh_deadline = None;
        self.reconnect_deadline = None;
        self.reconnect_attempt = 0;
    }

    pub(super) async fn bootstrap_selected_agent(&mut self) -> Result<()> {
        self.bootstrap_agent_index(self.selected_agent).await
    }

    pub(super) async fn bootstrap_agent_index(&mut self, target_index: usize) -> Result<()> {
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
            if let Some(previous) = self.projection.as_mut() {
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

    pub(super) async fn connect_event_stream(&mut self) -> Result<()> {
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

    pub(super) async fn connect_event_stream_for(
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

    pub(super) fn next_agent_index(&self, delta: i32) -> Option<usize> {
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

    pub(super) fn spawn_stream_task(&mut self, mut stream: LocalEventStream) {
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

    pub(super) fn stop_stream_task(&mut self) {
        if let Some(task) = self.stream_task.take() {
            task.abort();
        }
        self.stream_messages = None;
    }

    pub(super) fn process_runtime_messages(&mut self) -> bool {
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

    pub(super) fn apply_stream_event(&mut self, event: AgentStreamEvent) {
        if let Some(projection) = self.projection.as_mut() {
            projection.apply_event(event, &self.log_writer);
            self.last_event_at = Some(Local::now());
            self.apply_projection_view();
            self.schedule_projection_refresh_if_stale();
        }
    }

    pub(super) fn apply_projection_view(&mut self) {
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
        self.prune_optimistic_operator_messages();
    }

    pub(super) fn schedule_reconnect(&mut self, error: String) {
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        self.reconnect_deadline = Some(Instant::now() + STREAM_RECONNECT_DELAY);
        self.refresh_deadline = None;
        self.connection_state = TuiConnectionState::Reconnecting {
            attempt: self.reconnect_attempt,
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
            TuiConnectionState::Reconnecting { attempt, .. } => {
                format!("reconnecting (attempt {attempt})")
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
                ProjectionSlice::TranscriptTail => "transcript",
                ProjectionSlice::BriefsTail => "briefs",
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

pub(super) fn is_cursor_too_old_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<LocalHttpError>()
        .is_some_and(|error| error.has_code("cursor_too_old"))
}
