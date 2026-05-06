use super::*;

pub(crate) struct TuiApp {
    pub(crate) client: LocalClient,
    pub(crate) agents: Vec<AgentSummary>,
    pub(crate) briefs: Vec<BriefRecord>,
    pub(crate) transcript: Vec<TranscriptEntry>,
    pub(crate) optimistic_operator_messages: Vec<OperatorMessageRecord>,
    pub(crate) tasks: Vec<TaskRecord>,
    pub(crate) projection: Option<TuiProjection>,
    pub(crate) connection_state: TuiConnectionState,
    pub(crate) stream_messages: Option<mpsc::UnboundedReceiver<TuiRuntimeMessage>>,
    pub(crate) stream_task: Option<JoinHandle<()>>,
    pub(crate) agent_list_refresh_deadline: Option<Instant>,
    pub(crate) reconnect_deadline: Option<Instant>,
    pub(crate) refresh_deadline: Option<Instant>,
    pub(crate) reconnect_attempt: u32,
    pub(crate) selected_agent: usize,
    pub(crate) chat_scroll: ChatScrollState,
    pub(crate) chat_max_scroll: u16,
    pub(crate) composer: ComposerState,
    pub(crate) slash_menu_selected: usize,
    pub(crate) slash_menu_dismissed_for: Option<String>,
    pub(crate) overlay: OverlayState,
    pub(crate) last_refresh_at: Option<DateTime<Local>>,
    pub(crate) last_event_at: Option<DateTime<Local>>,
    pub(crate) display_level: OperatorVisibility,
    pub(crate) status_line: String,
    pub(crate) should_quit: bool,
    pub(crate) chat_text_cache: RefCell<Option<CachedChatText>>,
    pub(crate) input_history: Vec<String>,
    pub(crate) history_index: Option<usize>,
    pub(crate) log_writer: TuiLogWriter,
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

    pub(crate) fn selected_agent_id(&self) -> Option<&str> {
        self.agents
            .get(self.selected_agent)
            .map(|agent| agent.identity.agent_id.as_str())
    }

    pub(crate) fn selected_agent_summary(&self) -> Option<&AgentSummary> {
        self.agents.get(self.selected_agent)
    }

    pub(crate) fn add_optimistic_operator_message(
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

    pub(crate) fn reconcile_optimistic_operator_message(
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

    pub(crate) fn fail_optimistic_operator_message(
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

    pub(crate) fn prune_optimistic_operator_messages(&mut self) {
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

    pub(crate) fn clear_agent_view(&mut self) {
        self.clear_projection_view();
        self.agents.clear();
        self.selected_agent = 0;
    }

    pub(crate) fn clear_projection_view(&mut self) {
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

    pub(crate) fn next_agent_index(&self, delta: i32) -> Option<usize> {
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

    pub(crate) fn apply_projection_view(&mut self) {
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
}

pub(crate) fn transcript_merge_key(entry: &TranscriptEntry) -> &str {
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
