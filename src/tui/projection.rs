#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::logging::TuiLogWriter;

pub(crate) use crate::operator_event::{OperatorDisplayMode, OperatorVisibility};
use crate::{
    client::{
        AgentStateSnapshot, AgentStreamEvent, StateSessionSnapshot, StateWorkspaceSnapshot,
        StreamEventEnvelope,
    },
    operator_event::{
        is_activity_reset_event_kind, is_durable_operator_event_kind, present_operator_event,
        OperatorEventCategory, OperatorEventPresentation, OperatorPresentationContext,
    },
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        ActiveWorkspaceEntry, AgentState, AgentSummary, BriefRecord, ClosureDecision,
        ExternalTriggerStateSnapshot, MessageEnvelope, MessageOrigin, TaskRecord, TimerRecord,
        TimerStatus, WaitingIntentRecord, WorkItemRecord, WorkItemState, WorktreeSession,
    },
};

const TASK_TAIL_LIMIT: usize = 50;
const TIMER_TAIL_LIMIT: usize = 50;
pub(crate) const EVENT_LOG_LIMIT: usize = 1024;
const EVENT_HISTORY_LOG_LIMIT: usize = 16_384;
const DURABLE_CONVERSATION_LOG_LIMIT: usize = 256;
static TUI_EVENT_LOG_WRITE_WARNED: AtomicBool = AtomicBool::new(false);
static TUI_PRESENTATION_LOG_WRITE_WARNED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ProjectionSlice {
    Agent,
    Session,
    Tasks,
    Timers,
    WorkItems,
    WaitingIntents,
    ExternalTriggers,
    OperatorNotifications,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionEventLane {
    Timeline,
    State,
    Debug,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProjectionEventRecord {
    pub(crate) id: String,
    pub(crate) event_seq: u64,
    pub(crate) ts: DateTime<Utc>,
    pub(crate) kind: String,
    pub(crate) lane: ProjectionEventLane,
    pub(crate) summary: String,
    pub(crate) presentation: OperatorEventPresentation,
    pub(crate) payload: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct TuiProjection {
    pub(crate) agent: AgentSummary,
    pub(crate) session: StateSessionSnapshot,
    pub(crate) presentation_reducer: crate::presentation::PresentationReducer,
    pub(crate) tasks: Vec<TaskRecord>,
    pub(crate) timers: Vec<TimerRecord>,
    pub(crate) work_items: Vec<WorkItemRecord>,
    pub(crate) waiting_intents: Vec<WaitingIntentRecord>,
    pub(crate) external_triggers: Vec<ExternalTriggerStateSnapshot>,
    pub(crate) operator_notifications: Vec<crate::types::OperatorNotificationRecord>,
    pub(crate) workspace: StateWorkspaceSnapshot,
    pub(crate) cursor: Option<u64>,
    pub(crate) history_oldest_cursor: Option<u64>,
    pub(crate) history_has_older: bool,
    history_paging_active: bool,
    pub(crate) stale_slices: BTreeSet<ProjectionSlice>,
    event_log: Vec<ProjectionEventRecord>,
    durable_conversation_log: Vec<ProjectionEventRecord>,
}

impl TuiProjection {
    pub(crate) fn from_snapshot(snapshot: AgentStateSnapshot) -> Self {
        let presentation_reducer = crate::presentation::PresentationReducer::new();
        let external_triggers = snapshot.external_triggers;
        let operator_notifications = snapshot.operator_notifications;
        let tasks = active_tasks_for_projection(snapshot.tasks);

        let projection = Self {
            agent: snapshot.agent,
            session: snapshot.session,
            presentation_reducer,
            tasks,
            timers: snapshot.timers,
            work_items: snapshot.work_items,
            waiting_intents: snapshot.waiting_intents,
            external_triggers,
            operator_notifications,
            workspace: snapshot.workspace,
            cursor: None,
            history_oldest_cursor: None,
            history_has_older: false,
            history_paging_active: false,
            stale_slices: BTreeSet::new(),
            event_log: Vec::new(),
            durable_conversation_log: Vec::new(),
        };
        projection
    }

    pub(crate) fn reset_from_snapshot(&mut self, snapshot: AgentStateSnapshot) {
        *self = Self::from_snapshot(snapshot);
    }

    pub(crate) fn reset_from_snapshot_preserving_event_history(
        &mut self,
        snapshot: AgentStateSnapshot,
    ) {
        let mut refreshed = Self::from_snapshot(snapshot);
        refreshed.cursor = self.cursor.take();
        refreshed.history_oldest_cursor = self.history_oldest_cursor.take();
        refreshed.history_has_older = self.history_has_older;
        refreshed.history_paging_active = self.history_paging_active;
        refreshed.event_log = mem::take(&mut self.event_log);
        refreshed.durable_conversation_log = mem::take(&mut self.durable_conversation_log);
        *self = refreshed;
    }

    pub(crate) fn replace_event_window(
        &mut self,
        events_tail: Vec<StreamEventEnvelope>,
        cursor: Option<u64>,
    ) {
        self.event_log.clear();
        self.durable_conversation_log.clear();
        self.cursor = cursor;
        self.seed_event_log(events_tail);
    }

    pub(crate) fn merge_event_tail(
        &mut self,
        events_tail: Vec<StreamEventEnvelope>,
        cursor: Option<u64>,
    ) {
        self.cursor = cursor.or_else(|| self.cursor.clone());
        let event_log_limit = if self.history_paging_active {
            EVENT_HISTORY_LOG_LIMIT
        } else {
            EVENT_LOG_LIMIT
        };
        let mut existing_ids = self
            .event_log
            .iter()
            .filter(|event| !event.id.is_empty())
            .map(|event| event.id.clone())
            .collect::<BTreeSet<_>>();
        let mut existing_seqs_without_ids = self
            .event_log
            .iter()
            .filter_map(|event| {
                (event.id.is_empty() && event.event_seq != 0).then_some(event.event_seq)
            })
            .collect::<BTreeSet<_>>();
        for envelope in events_tail {
            if !envelope.id.is_empty() && !existing_ids.insert(envelope.id.clone()) {
                continue;
            }
            if envelope.id.is_empty()
                && envelope.event_seq != 0
                && !existing_seqs_without_ids.insert(envelope.event_seq)
            {
                continue;
            }
            let record = self.projection_event_record_from_envelope(envelope);
            self.event_log.push(record);
        }
        self.event_log.sort_by(|left, right| {
            left.event_seq
                .cmp(&right.event_seq)
                .then_with(|| left.ts.cmp(&right.ts))
                .then_with(|| left.id.cmp(&right.id))
        });
        if self.event_log.len() > event_log_limit {
            self.event_log
                .drain(0..(self.event_log.len() - event_log_limit));
        }
        self.rebuild_durable_conversation_log();
        if self.cursor.is_none() {
            self.cursor = self.event_log.last().map(|event| event.event_seq);
        }
    }

    pub(crate) fn set_event_history_state(&mut self, oldest_cursor: Option<u64>, has_older: bool) {
        self.history_oldest_cursor = oldest_cursor;
        self.history_has_older = has_older;
    }

    pub(crate) fn set_event_history_state_from_tail(
        &mut self,
        oldest_cursor: Option<u64>,
        has_older: bool,
    ) {
        if self.history_paging_active && self.history_oldest_cursor.is_some() {
            self.history_has_older = self.history_has_older || has_older;
            return;
        }
        self.set_event_history_state(oldest_cursor, has_older);
    }

    pub(crate) fn prepend_event_history_page(
        &mut self,
        mut events: Vec<StreamEventEnvelope>,
        oldest_cursor: Option<u64>,
        has_older: bool,
    ) -> usize {
        self.history_paging_active = true;
        let available = EVENT_HISTORY_LOG_LIMIT.saturating_sub(self.event_log.len());
        if available == 0 {
            self.history_has_older = false;
            return 0;
        }
        if events.is_empty() {
            self.history_oldest_cursor = oldest_cursor;
            self.history_has_older = has_older;
            return 0;
        }

        let mut existing_ids = self
            .event_log
            .iter()
            .filter(|event| !event.id.is_empty())
            .map(|event| event.id.clone())
            .collect::<BTreeSet<_>>();
        let mut existing_seqs_without_ids = self
            .event_log
            .iter()
            .filter_map(|event| {
                (event.id.is_empty() && event.event_seq != 0).then_some(event.event_seq)
            })
            .collect::<BTreeSet<_>>();
        events.reverse();

        let mut prepended = Vec::new();
        for envelope in events {
            if !envelope.id.is_empty() && !existing_ids.insert(envelope.id.clone()) {
                continue;
            }
            if envelope.id.is_empty()
                && envelope.event_seq != 0
                && !existing_seqs_without_ids.insert(envelope.event_seq)
            {
                continue;
            }
            prepended.push(self.projection_event_record_from_envelope(envelope));
        }
        let capped = prepended.len() > available;
        if capped {
            let first_retained = prepended.len() - available;
            prepended.drain(0..first_retained);
        }
        let added = prepended.len();
        if added == 0 {
            self.history_oldest_cursor = oldest_cursor;
            self.history_has_older = has_older;
            return 0;
        }

        self.history_oldest_cursor = prepended.first().map(|event| event.event_seq);
        self.history_has_older = has_older
            && !capped
            && self.event_log.len().saturating_add(added) < EVENT_HISTORY_LOG_LIMIT;
        prepended.extend(mem::take(&mut self.event_log));
        self.event_log = prepended;
        self.rebuild_durable_conversation_log();
        added
    }

    pub(crate) fn apply_event(&mut self, event: AgentStreamEvent, log_writer: &TuiLogWriter) {
        if self.has_event_identity(effective_stream_event_id(&event), event.data.event_seq) {
            return;
        }
        let presentation_context = self.operator_presentation_context();
        let record = projection_event_record_from_stream_event(&event, &presentation_context);
        let event_log_limit = if self.history_paging_active {
            EVENT_HISTORY_LOG_LIMIT
        } else {
            EVENT_LOG_LIMIT
        };
        push_limited(&mut self.event_log, record.clone(), event_log_limit);
        if is_durable_conversation_kind(&record.kind) {
            push_limited(
                &mut self.durable_conversation_log,
                record.clone(),
                DURABLE_CONVERSATION_LOG_LIMIT,
            );
        }
        if let Err(error) = log_writer.write_event(&record) {
            if !TUI_EVENT_LOG_WRITE_WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!("failed to persist TUI log event: {error}");
            }
        }
        if is_presentation_reducer_event(&record) {
            let timed_items = self
                .presentation_reducer
                .reduce(std::slice::from_ref(&record));
            let (reducer_events, log_items) = presentation_debug_items_for_event(
                self.event_log.as_slice(),
                &record,
                timed_items.as_slice(),
            );
            if let Err(error) =
                log_writer.write_presentation_items(reducer_events.as_slice(), log_items.as_slice())
            {
                if !TUI_PRESENTATION_LOG_WRITE_WARNED.swap(true, Ordering::Relaxed) {
                    tracing::warn!("failed to persist TUI presentation log: {error}");
                }
            }
        }
        self.cursor = Some(event.data.event_seq);

        match event.data.event_type.as_str() {
            "agent_state_changed" | "session_state_changed" => {
                if let Some(state) = decode_payload::<AgentState>(&event.data.payload) {
                    self.apply_agent_state(state);
                } else {
                    self.mark_stale([ProjectionSlice::Agent, ProjectionSlice::Session]);
                }
            }
            "closure_decided" => {
                if let Some(closure) = event
                    .data
                    .payload
                    .get("closure")
                    .cloned()
                    .and_then(decode_value::<ClosureDecision>)
                {
                    self.agent.closure = closure;
                    self.stale_slices.remove(&ProjectionSlice::Agent);
                } else {
                    self.mark_stale([ProjectionSlice::Agent]);
                }
            }
            "message_enqueued" => {}
            "task_created" | "task_status_updated" | "task_result_received" => {
                if let Some(task) = decode_payload::<TaskRecord>(&event.data.payload) {
                    upsert_active_task(&mut self.tasks, task);
                    self.stale_slices.remove(&ProjectionSlice::Tasks);
                } else {
                    self.mark_stale([ProjectionSlice::Tasks]);
                }
            }
            "timer_created" => {
                if let Some(timer) = decode_payload::<TimerRecord>(&event.data.payload) {
                    push_limited_sorted(
                        &mut self.timers,
                        timer,
                        TIMER_TAIL_LIMIT,
                        |left, right| left.created_at.cmp(&right.created_at),
                    );
                    self.stale_slices.remove(&ProjectionSlice::Timers);
                } else {
                    self.mark_stale([ProjectionSlice::Timers]);
                }
            }
            "timer_fired" => {
                if self.apply_timer_fired(&event.data.payload, event.data.ts) {
                    self.stale_slices.remove(&ProjectionSlice::Timers);
                } else {
                    self.mark_stale([ProjectionSlice::Timers]);
                }
            }
            "work_item_written" => {
                if let Some(record) = event
                    .data
                    .payload
                    .get("record")
                    .cloned()
                    .and_then(decode_value::<WorkItemRecord>)
                {
                    upsert_work_item(&mut self.work_items, record);
                    self.stale_slices.remove(&ProjectionSlice::WorkItems);
                } else {
                    self.mark_stale([ProjectionSlice::WorkItems]);
                }
            }
            "workspace_attached" => {
                if let Some(workspace_id) = read_string(&event.data.payload, "workspace_id") {
                    if !self.workspace.attached_workspaces.contains(&workspace_id) {
                        self.workspace
                            .attached_workspaces
                            .push(workspace_id.clone());
                        self.agent.agent.attached_workspaces.push(workspace_id);
                    }
                    self.stale_slices.remove(&ProjectionSlice::Agent);
                    self.stale_slices.insert(ProjectionSlice::Workspace);
                } else {
                    self.mark_stale([ProjectionSlice::Workspace, ProjectionSlice::Agent]);
                }
            }
            "workspace_detached" => {
                if let Some(workspace_id) = read_string(&event.data.payload, "workspace_id") {
                    self.workspace
                        .attached_workspaces
                        .retain(|id| id != &workspace_id);
                    self.agent
                        .agent
                        .attached_workspaces
                        .retain(|id| id != &workspace_id);
                    self.stale_slices.remove(&ProjectionSlice::Agent);
                    self.stale_slices.insert(ProjectionSlice::Workspace);
                } else {
                    self.mark_stale([ProjectionSlice::Workspace, ProjectionSlice::Agent]);
                }
            }
            "workspace_entered" | "workspace_used" => {
                if self.apply_workspace_entered(&event.data.payload) {
                    self.stale_slices.remove(&ProjectionSlice::Agent);
                    self.stale_slices.insert(ProjectionSlice::Workspace);
                } else {
                    self.mark_stale([ProjectionSlice::Workspace, ProjectionSlice::Agent]);
                }
            }
            "workspace_exited" => {
                self.clear_workspace();
                self.stale_slices.remove(&ProjectionSlice::Agent);
                self.stale_slices.insert(ProjectionSlice::Workspace);
            }
            "worktree_entered" => {
                if let Some(worktree) = event
                    .data
                    .payload
                    .get("worktree")
                    .cloned()
                    .and_then(decode_value::<WorktreeSession>)
                {
                    self.workspace.worktree_session = Some(worktree.clone());
                    self.agent.agent.worktree_session = Some(worktree.clone());
                    if let Some(entry) = &mut self.workspace.active_workspace_entry {
                        entry.cwd = worktree.worktree_path.clone();
                    }
                    if let Some(entry) = &mut self.agent.agent.active_workspace_entry {
                        entry.cwd = worktree.worktree_path;
                    }
                    self.mark_stale([ProjectionSlice::Workspace, ProjectionSlice::Agent]);
                } else {
                    self.mark_stale([ProjectionSlice::Workspace, ProjectionSlice::Agent]);
                }
            }
            "worktree_exited" => {
                self.workspace.worktree_session = None;
                self.agent.agent.worktree_session = None;
                // Restore cwd to workspace_anchor from active_workspace_entry
                if let Some(entry) = &mut self.workspace.active_workspace_entry {
                    entry.cwd = entry.workspace_anchor.clone();
                }
                if let Some(entry) = &mut self.agent.agent.active_workspace_entry {
                    entry.cwd = entry.workspace_anchor.clone();
                }
                self.mark_stale([ProjectionSlice::Workspace, ProjectionSlice::Agent]);
            }
            "turn_terminal" => {
                if let Some(record) =
                    decode_payload::<crate::types::TurnTerminalRecord>(&event.data.payload)
                {
                    self.session.last_turn = Some(record.clone());
                    self.agent.agent.last_turn_terminal = Some(record);
                    self.stale_slices.remove(&ProjectionSlice::Session);
                    self.stale_slices.remove(&ProjectionSlice::Agent);
                } else {
                    self.mark_stale([ProjectionSlice::Session, ProjectionSlice::Agent]);
                }
            }
            "agent_model_override_requested"
            | "agent_model_override_set"
            | "agent_model_override_clear_requested"
            | "agent_model_override_cleared" => {
                if self.apply_model_state_event(&event.data.payload) {
                    self.stale_slices.remove(&ProjectionSlice::Agent);
                } else {
                    self.mark_stale([ProjectionSlice::Agent]);
                }
            }
            "provider_round_completed" => {
                if self.apply_provider_round_model_event(&event.data.payload) {
                    self.stale_slices.remove(&ProjectionSlice::Agent);
                }
                // This event already carries enough payload for the active
                // Conversation preview and event overlays. Avoid forcing a
                // full /state refresh during normal turn progress.
            }
            "assistant_round_recorded"
            | "text_only_round_observed"
            | "max_output_tokens_recovery"
            | "runtime_error"
            | "deferred_to_fallback"
            | "provider_failed_needs_recovery" => {
                // These events already carry enough payload for the active
                // Conversation preview and event overlays. Avoid forcing a
                // full /state refresh during normal turn progress.
            }
            "waiting_intent_created" | "waiting_intent_cancelled" => {
                self.mark_stale([
                    ProjectionSlice::WaitingIntents,
                    ProjectionSlice::ExternalTriggers,
                ]);
            }
            "callback_delivered" => {
                self.mark_stale([
                    ProjectionSlice::ExternalTriggers,
                    ProjectionSlice::WaitingIntents,
                ]);
            }
            "operator_notification_requested" => {
                if let Some(record) =
                    decode_payload::<crate::types::OperatorNotificationRecord>(&event.data.payload)
                {
                    self.operator_notifications.push(record);
                } else {
                    self.mark_stale([ProjectionSlice::OperatorNotifications]);
                }
            }
            "message_admitted" | "control_applied" => {}
            _ => {}
        }
    }

    fn has_event_identity(&self, id: &str, event_seq: u64) -> bool {
        if !id.is_empty() {
            return self.event_log.iter().any(|event| event.id == id);
        }
        event_seq != 0
            && self
                .event_log
                .iter()
                .any(|event| event.id.is_empty() && event.event_seq == event_seq)
    }

    fn seed_event_log(&mut self, events_tail: Vec<StreamEventEnvelope>) {
        let mut seen_ids = BTreeSet::new();
        let mut seen_seqs_without_ids = BTreeSet::new();
        for envelope in events_tail {
            if !envelope.id.is_empty() && !seen_ids.insert(envelope.id.clone()) {
                continue;
            }
            if envelope.id.is_empty()
                && envelope.event_seq != 0
                && !seen_seqs_without_ids.insert(envelope.event_seq)
            {
                continue;
            }
            let record = self.projection_event_record_from_envelope(envelope);
            push_limited(&mut self.event_log, record, EVENT_LOG_LIMIT);
        }
        self.rebuild_durable_conversation_log();
        if let Some(last_event) = self.event_log.last() {
            if self.cursor.is_none() {
                self.cursor = Some(last_event.event_seq);
            }
        }
    }

    fn projection_event_record_from_envelope(
        &self,
        envelope: StreamEventEnvelope,
    ) -> ProjectionEventRecord {
        let event = AgentStreamEvent {
            id: envelope.id.clone(),
            event: envelope.event_type.clone(),
            data: envelope,
        };
        let presentation_context = self.operator_presentation_context();
        projection_event_record_from_stream_event(&event, &presentation_context)
    }

    fn rebuild_durable_conversation_log(&mut self) {
        self.durable_conversation_log = self
            .event_log
            .iter()
            .filter(|record| is_durable_conversation_kind(&record.kind))
            .rev()
            .take(DURABLE_CONVERSATION_LOG_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        self.durable_conversation_log.reverse();
    }

    pub(crate) fn event_log(&self) -> &[ProjectionEventRecord] {
        &self.event_log
    }

    pub(crate) fn event_history_at_local_cap(&self) -> bool {
        self.history_paging_active
            && !self.history_has_older
            && self.event_log.len() >= EVENT_HISTORY_LOG_LIMIT
    }

    pub(crate) fn timeline_events(&self) -> impl Iterator<Item = &ProjectionEventRecord> {
        self.event_log
            .iter()
            .filter(|event| event.lane == ProjectionEventLane::Timeline)
    }

    pub(crate) fn durable_conversation_events(
        &self,
    ) -> impl Iterator<Item = &ProjectionEventRecord> {
        self.durable_conversation_log.iter()
    }

    /// Returns events for the conversation timeline at the given display mode.
    ///
    /// At Info level (3), this includes all conversation-candidate events from
    /// the full `event_log`, not just the truncated `durable_conversation_log`.
    /// The durable log is capped at 256 entries and can fill with infrastructure
    /// events (e.g. `callback_delivered`), pushing real conversation events
    /// (`brief_created`, `message_enqueued`) out of view.
    pub(crate) fn presentation_events(
        &self,
        display_mode: OperatorDisplayMode,
    ) -> Vec<ProjectionEventRecord> {
        let mut events = Vec::new();
        let mut seen = BTreeSet::new();
        // Always start with events that may survive event_log rotation.
        for event in &self.durable_conversation_log {
            if seen.insert(event.id.clone()) {
                events.push(event.clone());
            }
        }
        if display_mode.display_level() <= OperatorDisplayMode::Info.display_level() {
            // Info level: also collect conversation-candidate events from the
            // full event_log so they aren't lost when the durable log fills with
            // infrastructure events.
            for event in &self.event_log {
                if is_durable_conversation_kind(&event.kind) && seen.insert(event.id.clone()) {
                    events.push(event.clone());
                }
            }
        } else {
            for event in &self.event_log {
                if seen.insert(event.id.clone()) {
                    events.push(event.clone());
                }
            }
        }
        events.sort_by(|left, right| {
            left.ts
                .cmp(&right.ts)
                .then_with(|| left.event_seq.cmp(&right.event_seq))
                .then_with(|| left.id.cmp(&right.id))
        });
        events
    }

    pub(crate) fn durable_operator_message_ids(&self) -> BTreeSet<String> {
        self.durable_conversation_log
            .iter()
            .filter(|event| event.kind == "message_enqueued")
            .filter_map(|event| {
                serde_json::from_value::<MessageEnvelope>(event.payload.clone())
                    .ok()
                    .filter(|message| matches!(message.origin, MessageOrigin::Operator { .. }))
                    .map(|message| message.id)
            })
            .collect()
    }

    pub(crate) fn recent_activity_events(&self) -> Vec<&ProjectionEventRecord> {
        let mut events = Vec::new();
        for event in self.current_turn_events_rev() {
            if event.presentation.is_current_activity_candidate() {
                events.push(event);
            }
            if events.len() >= 4 {
                break;
            }
        }
        events.reverse();
        events
    }

    /// Current live presentation item, if any (for active activity display).
    pub(crate) fn current_live_item(&self) -> Option<crate::presentation::TimedItem> {
        self.presentation_reducer.current_live_item()
    }

    pub(crate) fn visible_events(
        &self,
        display_mode: OperatorDisplayMode,
    ) -> impl Iterator<Item = &ProjectionEventRecord> {
        self.event_log
            .iter()
            .filter(move |event| self.is_visible_in_display_mode(event, display_mode))
    }

    pub(crate) fn hidden_current_turn_events(
        &self,
        display_mode: OperatorDisplayMode,
    ) -> Vec<&ProjectionEventRecord> {
        let mut events = self
            .current_turn_events_rev()
            .filter(|event| !self.is_visible_in_display_mode(event, display_mode))
            .filter(|event| self.is_visible_in_display_mode(event, OperatorDisplayMode::Debug))
            .take(8)
            .collect::<Vec<_>>();
        events.reverse();
        events
    }

    pub(crate) fn operator_visibility(&self, event: &ProjectionEventRecord) -> OperatorVisibility {
        event.presentation.visibility
    }

    pub(crate) fn is_visible_in_display_mode(
        &self,
        event: &ProjectionEventRecord,
        display_mode: OperatorDisplayMode,
    ) -> bool {
        match display_mode {
            OperatorDisplayMode::Info => is_info_event(event),
            OperatorDisplayMode::Verbose => is_info_event(event) || is_verbose_event(event),
            OperatorDisplayMode::Debug => {
                is_info_event(event) || is_verbose_event(event) || is_debug_event(event)
            }
        }
    }

    fn operator_presentation_context(&self) -> OperatorPresentationContext {
        let completed_work_item_ids = self
            .work_items
            .iter()
            .filter(|item| item.state == WorkItemState::Completed)
            .map(|item| item.id.clone())
            .collect();
        OperatorPresentationContext {
            awaiting_operator_input: self.agent.closure.waiting_reason
                == Some(crate::types::WaitingReason::AwaitingOperatorInput),
            completed_work_item_ids,
        }
    }

    fn current_turn_events_rev(&self) -> impl Iterator<Item = &ProjectionEventRecord> {
        let active_turn = self.agent.agent.turn_index;
        let active_run = self.agent.agent.current_run_id.as_deref();
        self.event_log.iter().rev().take_while(move |event| {
            if is_activity_reset_kind(&event.kind) {
                return false;
            }
            let event_turn = event.payload.get("turn_index").and_then(Value::as_u64);
            let event_run = event.payload.get("run_id").and_then(Value::as_str);
            if let Some(event_run) = event_run {
                return Some(event_run) == active_run;
            }
            if let Some(event_turn) = event_turn {
                return event_turn == active_turn;
            }
            !matches!(
                event.kind.as_str(),
                "turn_started" | "message_processing_started" | "message_enqueued"
            )
        })
    }

    pub(crate) fn recent_log_events(&self, limit: usize) -> Vec<&ProjectionEventRecord> {
        self.event_log
            .iter()
            .rev()
            .filter(|event| event.presentation.is_loggable())
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn mark_stale<const N: usize>(&mut self, slices: [ProjectionSlice; N]) {
        self.stale_slices.extend(slices);
    }

    fn apply_agent_state(&mut self, state: AgentState) {
        self.session.current_run_id = state.current_run_id.clone();
        self.session.pending_count = state.pending;
        self.session.last_turn = state.last_turn_terminal.clone();

        self.workspace.attached_workspaces = state.attached_workspaces.clone();
        self.workspace.active_workspace_entry = state.active_workspace_entry.clone();
        self.workspace.active_workspace_occupancy = None;
        self.workspace.worktree_session = state.worktree_session.clone();

        self.agent.agent = state;
        self.agent.active_workspace_occupancy = None;
        self.stale_slices.remove(&ProjectionSlice::Agent);
        self.stale_slices.remove(&ProjectionSlice::Session);
        self.stale_slices.insert(ProjectionSlice::Workspace);
    }

    fn apply_timer_fired(&mut self, payload: &Value, fired_at: DateTime<Utc>) -> bool {
        let Some(timer_id) = read_string(payload, "timer_id") else {
            return false;
        };
        let Some(latest_index) = self.timers.iter().rposition(|timer| timer.id == timer_id) else {
            return false;
        };
        let mut timer = self.timers[latest_index].clone();
        if let Some(status) = payload
            .get("status")
            .cloned()
            .and_then(decode_value::<TimerStatus>)
        {
            timer.status = status;
        }
        timer.fire_count = payload
            .get("fire_count")
            .and_then(Value::as_u64)
            .unwrap_or(timer.fire_count);
        timer.next_fire_at = payload
            .get("next_fire_at")
            .cloned()
            .and_then(decode_value::<Option<DateTime<Utc>>>)
            .unwrap_or(timer.next_fire_at);
        timer.last_fired_at = Some(fired_at);
        push_limited_sorted(&mut self.timers, timer, TIMER_TAIL_LIMIT, |left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.last_fired_at.cmp(&right.last_fired_at))
                .then_with(|| left.fire_count.cmp(&right.fire_count))
                .then_with(|| left.id.cmp(&right.id))
        });
        true
    }

    fn apply_workspace_entered(&mut self, payload: &Value) -> bool {
        let Some(workspace_id) = read_string(payload, "workspace_id") else {
            return false;
        };
        let Some(workspace_anchor) = payload
            .get("workspace_anchor")
            .cloned()
            .and_then(decode_value::<std::path::PathBuf>)
        else {
            return false;
        };
        let Some(execution_root_id) = read_string(payload, "execution_root_id") else {
            return false;
        };
        let Some(execution_root) = payload
            .get("execution_root")
            .cloned()
            .and_then(decode_value::<std::path::PathBuf>)
        else {
            return false;
        };
        let Some(projection_kind) = payload
            .get("projection_kind")
            .cloned()
            .and_then(decode_value::<WorkspaceProjectionKind>)
        else {
            return false;
        };
        let Some(access_mode) = payload
            .get("access_mode")
            .cloned()
            .and_then(decode_value::<WorkspaceAccessMode>)
        else {
            return false;
        };
        let Some(cwd) = payload
            .get("cwd")
            .cloned()
            .and_then(decode_value::<std::path::PathBuf>)
        else {
            return false;
        };

        if !self.workspace.attached_workspaces.contains(&workspace_id) {
            self.workspace
                .attached_workspaces
                .push(workspace_id.clone());
        }
        if !self.agent.agent.attached_workspaces.contains(&workspace_id) {
            self.agent
                .agent
                .attached_workspaces
                .push(workspace_id.clone());
        }

        let entry = ActiveWorkspaceEntry {
            workspace_id: workspace_id.clone(),
            workspace_anchor: workspace_anchor.clone(),
            execution_root_id,
            execution_root,
            projection_kind,
            access_mode,
            cwd: cwd.clone(),
            occupancy_id: None,
            projection_metadata: None,
        };

        self.workspace.active_workspace_entry = Some(entry.clone());
        self.workspace.active_workspace_occupancy = None;
        self.workspace.worktree_session = None;

        self.agent.agent.active_workspace_entry = Some(entry);
        self.agent.active_workspace_occupancy = None;
        self.agent.agent.worktree_session = None;
        true
    }

    fn apply_model_state_event(&mut self, payload: &Value) -> bool {
        let Some(model) = payload
            .get("model")
            .cloned()
            .and_then(decode_value::<crate::types::AgentModelState>)
        else {
            return false;
        };
        self.agent.model = model;
        true
    }

    fn apply_provider_round_model_event(&mut self, payload: &Value) -> bool {
        let has_requested_model = payload.get("requested_model").is_some();
        let has_active_model = payload.get("active_model").is_some();
        let requested_model = payload.get("requested_model").and_then(|value| {
            decode_value::<Option<crate::config::ModelRef>>(value.clone()).unwrap_or(None)
        });
        let active_model = payload.get("active_model").and_then(|value| {
            decode_value::<Option<crate::config::ModelRef>>(value.clone()).unwrap_or(None)
        });
        let fallback_active = payload.get("fallback_active").and_then(Value::as_bool);

        if !has_requested_model && !has_active_model && fallback_active.is_none() {
            return false;
        }

        if has_requested_model {
            self.agent.model.requested_model = requested_model;
        }
        if has_active_model {
            self.agent.model.active_model = active_model;
        }
        self.agent.model.fallback_active = fallback_active.unwrap_or_else(|| {
            self.agent
                .model
                .requested_model
                .as_ref()
                .zip(self.agent.model.active_model.as_ref())
                .is_some_and(|(requested, active)| requested != active)
        });
        true
    }

    fn clear_workspace(&mut self) {
        self.workspace.active_workspace_entry = None;
        self.workspace.active_workspace_occupancy = None;
        self.workspace.worktree_session = None;

        self.agent.agent.active_workspace_entry = None;
        self.agent.active_workspace_occupancy = None;
        self.agent.agent.worktree_session = None;
    }
}

fn decode_payload<T: DeserializeOwned>(payload: &Value) -> Option<T> {
    decode_value(payload.clone())
}

fn decode_value<T: DeserializeOwned>(value: Value) -> Option<T> {
    serde_json::from_value(value).ok()
}

fn read_string(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn push_limited<T>(items: &mut Vec<T>, item: T, limit: usize) {
    items.push(item);
    if items.len() > limit {
        items.drain(0..(items.len() - limit));
    }
}

fn push_limited_sorted<T, F>(items: &mut Vec<T>, item: T, limit: usize, sort: F)
where
    F: Fn(&T, &T) -> std::cmp::Ordering,
{
    items.push(item);
    items.sort_by(sort);
    if items.len() > limit {
        items.drain(0..(items.len() - limit));
    }
}

fn upsert_active_task(tasks: &mut Vec<TaskRecord>, task: TaskRecord) {
    tasks.retain(|existing| existing.id != task.id);
    if crate::storage::is_active_task_status(&task.status) {
        push_limited_sorted(tasks, task, TASK_TAIL_LIMIT, |left, right| {
            left.updated_at.cmp(&right.updated_at)
        });
    }
}

fn active_tasks_for_projection(tasks: Vec<TaskRecord>) -> Vec<TaskRecord> {
    let mut tasks = tasks
        .into_iter()
        .filter(|task| crate::storage::is_active_task_status(&task.status))
        .collect::<Vec<_>>();
    // TUI keeps tasks oldest-first internally because the overlay reverses them for display.
    tasks.sort_by(|left, right| left.updated_at.cmp(&right.updated_at));
    if tasks.len() > TASK_TAIL_LIMIT {
        tasks.drain(0..(tasks.len() - TASK_TAIL_LIMIT));
    }
    tasks
}

fn upsert_work_item(items: &mut Vec<WorkItemRecord>, item: WorkItemRecord) {
    if let Some(index) = items.iter().position(|existing| existing.id == item.id) {
        items[index] = item;
    } else {
        items.push(item);
    }
    items.sort_by(|left, right| {
        work_item_rank(left)
            .cmp(&work_item_rank(right))
            .then_with(|| {
                if left.state == WorkItemState::Open && right.state == WorkItemState::Open {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.updated_at.cmp(&right.updated_at))
                } else {
                    right
                        .updated_at
                        .cmp(&left.updated_at)
                        .then_with(|| right.created_at.cmp(&left.created_at))
                }
            })
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn work_item_rank(item: &WorkItemRecord) -> u8 {
    match item.state {
        WorkItemState::Open if item.blocked_by.is_none() => 0,
        WorkItemState::Open => 1,
        WorkItemState::Completed => 2,
    }
}

fn work_item_event_completed(event: &ProjectionEventRecord) -> bool {
    event
        .payload
        .get("record")
        .cloned()
        .and_then(decode_value::<WorkItemRecord>)
        .is_some_and(|record| record.state == WorkItemState::Completed)
}

fn is_info_event(event: &ProjectionEventRecord) -> bool {
    event.presentation.is_conversation_candidate()
        && matches!(
            event.presentation.visibility,
            OperatorVisibility::ActionRequired
                | OperatorVisibility::TurnResult
                | OperatorVisibility::WorkDone
        )
}

fn is_verbose_event(event: &ProjectionEventRecord) -> bool {
    match event.kind.as_str() {
        "assistant_round_recorded" => assistant_round_has_text(event),
        "text_only_round_observed" => text_only_round_has_useful_text(event),
        "max_output_tokens_recovery"
        | "turn_local_compaction_applied"
        | "turn_local_checkpoint_resume_requested"
        | "turn_local_baseline_over_budget" => true,
        "process_execution_requested" => process_execution_has_preview(event),
        "tool_executed" | "tool_execution_failed" => true,
        "truncated_mutation_tool_call_rejected" => true,
        "task_result_received"
        | "task_child_spawned"
        | "supervised_child_task_recovery_failed"
        | "command_task_runner_failed"
        | "command_task_result_enqueue_failed" => true,
        "task_status_updated" => task_status_is_terminal(event),
        "work_item_written" => work_item_event_completed(event),
        "work_item_delegation_completed"
        | "work_item_waiting_intents_cancelled"
        | "missing_current_work_item_before_wait"
        | "waiting_intent_created"
        | "stale_waiting_intents_cancelled"
        | "callback_delivered"
        | "timer_fired"
        | "timer_fire_failed"
        | "workspace_attached"
        | "workspace_entered"
        | "workspace_exited"
        | "workspace_detached"
        | "worktree_entered"
        | "worktree_exited"
        | "worktree_created_for_task"
        | "worktree_retained_for_review"
        | "worktree_auto_cleaned_up"
        | "worktree_auto_cleanup_failed"
        | "task_worktree_cleanup_failed"
        | "skill_installed"
        | "skill_uninstalled"
        | "agent_created"
        | "agent_model_override_set"
        | "agent_model_override_cleared"
        | "current_run_aborted"
        | "control_applied"
        | "runtime_service_shutdown_requested"
        | "turn_context_length_exceeded"
        | "recovery_cleared_missing_worktree_session"
        | "operator_notification_mirror_failed" => true,
        _ => false,
    }
}

fn is_debug_event(event: &ProjectionEventRecord) -> bool {
    match event.kind.as_str() {
        "provider_round_completed" => provider_round_has_useful_telemetry(event),
        "message_processing_aborted"
        | "operator_interjection_admitted"
        | "task_created"
        | "task_status_updated"
        | "task_input_delivered"
        | "task_create_requested"
        | "supervised_child_task_monitor_reattached"
        | "work_item_picked"
        | "work_item_enqueue_requested"
        | "work_item_turn_end_committed"
        | "work_item_turn_end_commit_skipped"
        | "work_item_stale_reminder_injected"
        | "work_item_stale_reminder_skipped"
        | "work_item_delegation_created"
        | "waiting_intent_cancelled"
        | "timer_create_requested"
        | "timer_created"
        | "workspace_attach_requested"
        | "workspace_exit_requested"
        | "workspace_detach_requested"
        | "workspace_used"
        | "task_worktree_metadata_recorded"
        | "task_worktree_cleanup_already_removed"
        | "task_worktree_cleanup_retained"
        | "task_worktree_branch_cleanup_retained"
        | "skill_activated"
        | "agent_model_override_requested"
        | "agent_model_override_clear_requested"
        | "control_request_admitted"
        | "wake_requested"
        | "continuation_trigger_received"
        | "continuation_resolved"
        | "closure_decided"
        | "debug_prompt_requested"
        | "turn_context_built"
        | "turn_local_checkpoint_requested"
        | "turn_local_checkpoint_recorded"
        | "episode_memory_finalized"
        | "working_memory_updated"
        | "operator_delivery_submitted"
        | "operator_delivery_completed"
        | "operator_transport_binding_upserted"
        | "command_task_running_persisted" => true,
        _ => false,
    }
}

fn assistant_round_has_text(event: &ProjectionEventRecord) -> bool {
    event
        .payload
        .get("text_preview")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

fn text_only_round_has_useful_text(event: &ProjectionEventRecord) -> bool {
    event
        .payload
        .get("text_preview")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
        || event
            .payload
            .get("triggered_recovery")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn provider_round_has_useful_telemetry(event: &ProjectionEventRecord) -> bool {
    let model = event
        .payload
        .get("active_model")
        .and_then(Value::as_str)
        .or_else(|| event.payload.get("requested_model").and_then(Value::as_str))
        .is_some_and(|model| {
            let model = model.trim();
            !model.is_empty() && model != "model"
        });
    let stop = event
        .payload
        .get("stop_reason")
        .and_then(Value::as_str)
        .is_some_and(|stop| {
            let stop = stop.trim();
            !stop.is_empty() && stop != "unknown"
        });
    let tokens = event
        .payload
        .get("input_tokens")
        .and_then(Value::as_u64)
        .is_some()
        || event
            .payload
            .get("output_tokens")
            .and_then(Value::as_u64)
            .is_some();
    let tools = event
        .payload
        .get("tool_call_count")
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0);
    model || stop || tokens || tools
}

fn process_execution_has_preview(event: &ProjectionEventRecord) -> bool {
    event
        .payload
        .get("cmd_preview")
        .and_then(Value::as_str)
        .is_some_and(|cmd| !cmd.trim().is_empty())
        || event
            .payload
            .get("command_cost")
            .and_then(|value| value.get("cmd_preview"))
            .and_then(Value::as_str)
            .is_some_and(|cmd| !cmd.trim().is_empty())
}

fn task_status_is_terminal(event: &ProjectionEventRecord) -> bool {
    event
        .payload
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| {
            matches!(
                status,
                "Completed"
                    | "completed"
                    | "Failed"
                    | "failed"
                    | "Cancelled"
                    | "cancelled"
                    | "Interrupted"
                    | "interrupted"
            )
        })
}

pub(crate) fn is_presentation_reducer_event(event: &ProjectionEventRecord) -> bool {
    !matches!(
        event.presentation.category,
        OperatorEventCategory::StateSync
    )
}

pub(crate) fn is_durable_conversation_kind(kind: &str) -> bool {
    is_durable_operator_event_kind(kind)
}

fn presentation_debug_items_for_event(
    event_log: &[ProjectionEventRecord],
    record: &ProjectionEventRecord,
    timed_items: &[crate::presentation::TimedItem],
) -> (
    Vec<ProjectionEventRecord>,
    Vec<crate::presentation::TimedItem>,
) {
    if matches!(
        record.kind.as_str(),
        "tool_executed" | "tool_execution_failed"
    ) {
        if let Some(previous) = event_log
            .iter()
            .rev()
            .find(|event| event.id != record.id && event.kind == "process_execution_requested")
        {
            let reducer_events = vec![previous.clone(), record.clone()];
            let mut reducer = crate::presentation::PresentationReducer::new();
            let items = reducer.reduce(reducer_events.as_slice());
            if !items.is_empty() {
                return (reducer_events, items);
            }
        }
    }
    (vec![record.clone()], timed_items.to_vec())
}

fn is_activity_reset_kind(kind: &str) -> bool {
    is_activity_reset_event_kind(kind)
}

fn classify_event_lane(presentation: &OperatorEventPresentation) -> ProjectionEventLane {
    if presentation.is_conversation_candidate() {
        ProjectionEventLane::Timeline
    } else if matches!(
        presentation.category,
        OperatorEventCategory::AssistantProgress
            | OperatorEventCategory::Tool
            | OperatorEventCategory::Trace
    ) {
        ProjectionEventLane::Debug
    } else {
        ProjectionEventLane::State
    }
}

fn projection_event_record_from_stream_event(
    event: &AgentStreamEvent,
    presentation_context: &OperatorPresentationContext,
) -> ProjectionEventRecord {
    let fallback_summary = summarize_event(event);
    let presentation = present_operator_event(
        &event.data.event_type,
        &event.data.payload,
        &fallback_summary,
        presentation_context,
    );
    ProjectionEventRecord {
        id: effective_stream_event_id(event).to_string(),
        event_seq: event.data.event_seq,
        ts: event.data.ts,
        kind: event.data.event_type.clone(),
        lane: classify_event_lane(&presentation),
        summary: presentation.summary.clone(),
        presentation,
        payload: event.data.payload.clone(),
    }
}

fn effective_stream_event_id(event: &AgentStreamEvent) -> &str {
    if event.data.id.is_empty() {
        &event.id
    } else {
        &event.data.id
    }
}

fn summarize_event(event: &AgentStreamEvent) -> String {
    match event.data.event_type.as_str() {
        "message_enqueued" => decode_payload::<MessageEnvelope>(&event.data.payload)
            .map(|message| match message.body {
                crate::types::MessageBody::Text { text } => trim_summary(&text),
                crate::types::MessageBody::Json { value } => trim_summary(&value.to_string()),
                crate::types::MessageBody::Brief { text, .. } => trim_summary(&text),
            })
            .unwrap_or_else(|| event.data.event_type.clone()),
        "turn_started" => event
            .data
            .payload
            .get("message_id")
            .and_then(Value::as_str)
            .map(|message_id| format!("turn started for {message_id}"))
            .unwrap_or_else(|| "turn started".into()),
        "operator_interjection_admitted" => event
            .data
            .payload
            .get("text_preview")
            .and_then(Value::as_str)
            .map(|text| format!("operator message applied: {}", trim_summary(text)))
            .unwrap_or_else(|| "operator message applied".into()),
        "brief_created" => decode_payload::<BriefRecord>(&event.data.payload)
            .map(|brief| trim_summary(&brief.text))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "task_created" | "task_status_updated" | "task_result_received" => {
            decode_payload::<TaskRecord>(&event.data.payload)
                .map(|task| {
                    format!(
                        "{} [{:?}]",
                        task.summary.as_deref().unwrap_or(task.kind.as_str()),
                        task.status
                    )
                })
                .unwrap_or_else(|| event.data.event_type.clone())
        }
        "work_item_written" => event
            .data
            .payload
            .get("record")
            .cloned()
            .and_then(decode_value::<WorkItemRecord>)
            .map(|record| format!("{} [{:?}]", record.objective, record.state))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "waiting_intent_created" => decode_payload::<WaitingIntentRecord>(&event.data.payload)
            .map(|waiting| format!("waiting: {}", trim_summary(&waiting.description)))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "waiting_intent_cancelled" => event
            .data
            .payload
            .get("waiting_intent_id")
            .and_then(Value::as_str)
            .map(|id| format!("waiting cancelled: {id}"))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "operator_notification_requested" => {
            decode_payload::<crate::types::OperatorNotificationRecord>(&event.data.payload)
                .map(|notification| format!("operator notified: {}", notification.summary))
                .unwrap_or_else(|| event.data.event_type.clone())
        }
        "callback_delivered" => event
            .data
            .payload
            .get("waiting_intent_id")
            .and_then(Value::as_str)
            .map(|id| format!("callback delivered for {id}"))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "workspace_entered" => event
            .data
            .payload
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(|workspace_id| format!("entered workspace {workspace_id}"))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "workspace_exited" => event
            .data
            .payload
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(|workspace_id| format!("exited workspace {workspace_id}"))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "workspace_detached" => event
            .data
            .payload
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(|workspace_id| format!("detached workspace {workspace_id}"))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "worktree_entered" => event
            .data
            .payload
            .get("worktree")
            .and_then(|value| value.get("worktree_branch"))
            .and_then(Value::as_str)
            .map(|branch| format!("entered worktree {branch}"))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "worktree_exited" => event
            .data
            .payload
            .get("worktree_branch")
            .and_then(Value::as_str)
            .map(|branch| format!("exited worktree {branch}"))
            .unwrap_or_else(|| "exited worktree".into()),
        "assistant_round_recorded" => event
            .data
            .payload
            .get("text_preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(trim_summary)
            .or_else(|| {
                event
                    .data
                    .payload
                    .get("tool_names")
                    .and_then(Value::as_array)
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|tools| !tools.trim().is_empty())
                    .map(|tools| format!("assistant requested tools: {tools}"))
            })
            .unwrap_or_else(|| "assistant round recorded".into()),
        "provider_round_completed" => {
            let round = event
                .data
                .payload
                .get("round")
                .and_then(Value::as_u64)
                .map(|round| format!("round {round}"))
                .unwrap_or_else(|| "round".into());
            let stop = event
                .data
                .payload
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("provider {round} completed: stop={stop}")
        }
        "text_only_round_observed" => "text-only round observed".into(),
        "tool_executed" => event
            .data
            .payload
            .get("tool_name")
            .and_then(Value::as_str)
            .map(|tool| format!("tool executed: {tool}"))
            .unwrap_or_else(|| "tool executed".into()),
        "tool_execution_failed" => event
            .data
            .payload
            .get("tool_name")
            .and_then(Value::as_str)
            .map(|tool| format!("tool failed: {tool}"))
            .unwrap_or_else(|| "tool execution failed".into()),
        "runtime_error" => event
            .data
            .payload
            .get("message")
            .and_then(Value::as_str)
            .map(trim_summary)
            .unwrap_or_else(|| "runtime error".into()),
        "turn_terminal" => event
            .data
            .payload
            .get("kind")
            .and_then(Value::as_str)
            .map(|kind| format!("turn {kind}"))
            .unwrap_or_else(|| "turn terminal".into()),
        _ => event.data.event_type.clone(),
    }
}

fn trim_summary(value: &str) -> String {
    const LIMIT: usize = 120;
    if value.chars().count() <= LIMIT {
        value.to_string()
    } else {
        let mut trimmed = value
            .chars()
            .take(LIMIT.saturating_sub(1))
            .collect::<String>();
        trimmed.push('…');
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OperatorDisplayMode, OperatorVisibility, ProjectionEventLane, ProjectionSlice,
        TuiProjection, EVENT_LOG_LIMIT, TASK_TAIL_LIMIT,
    };
    use crate::{
        client::{
            AgentStateSnapshot, AgentStreamEvent, StateSessionSnapshot, StateWorkspaceSnapshot,
            StreamEventEnvelope,
        },
        operator_event::{present_operator_event, OperatorPresentationContext},
        system::{
            ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind,
        },
        types::{
            AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
            AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentSummary,
            AgentTokenUsageSummary, AgentVisibility, BriefKind, BriefRecord, CallbackDeliveryMode,
            ChildAgentSummary, ClosureDecision, ClosureOutcome, ExternalTriggerScope,
            ExternalTriggerStateSnapshot, ExternalTriggerStatus, LoadedAgentsMdView, MessageBody,
            MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
            RuntimePosture, SkillsRuntimeView, TaskRecord, TaskStatus, TimerRecord, TimerStatus,
            TodoItem, TodoItemState, TokenUsage, TurnTerminalKind, TurnTerminalRecord,
            WaitingIntentRecord, WaitingIntentScope, WaitingIntentStatus, WaitingIntentSummary,
            WaitingReason, WorkItemRecord, WorkItemState, WorkspaceOccupancyRecord,
            WorktreeSession,
        },
    };
    use chrono::Utc;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn test_log_writer() -> crate::tui::logging::TuiLogWriter {
        crate::tui::logging::TuiLogWriter::new_temp().unwrap()
    }

    #[test]
    fn projection_bootstraps_from_snapshot() {
        let snapshot = sample_snapshot();
        let projection = TuiProjection::from_snapshot(snapshot.clone());

        assert_eq!(projection.agent.identity.agent_id, "default");
        assert_eq!(
            projection.external_triggers.len(),
            snapshot.external_triggers.len()
        );
        assert!(projection.stale_slices.is_empty());
        assert!(projection.cursor.is_none());
    }

    #[test]
    fn projection_bootstrap_uses_external_triggers_without_legacy_fallback() {
        let mut snapshot = sample_snapshot();
        snapshot.external_triggers.clear();
        let projection = TuiProjection::from_snapshot(snapshot.clone());

        assert_eq!(
            projection.external_triggers.len(),
            snapshot.external_triggers.len()
        );
    }

    #[test]
    fn projection_bootstrap_seeds_raw_event_log_from_snapshot_tail() {
        let snapshot = sample_snapshot();
        let events_tail = vec![StreamEventEnvelope {
            id: "evt-tail-1".into(),
            event_seq: 0,
            ts: Utc::now(),
            agent_id: "default".into(),
            event_type: "assistant_round_recorded".into(),
            projection: Some(json!({
                "name": "operator",
                "raw_payload_included": true,
                "redactions": [],
            })),
            provenance: None,
            payload: json!({
                "stop_reason": "tool_use",
                "tool_names": ["ExecCommand"],
                "tool_call_count": 1,
                "has_tool_calls": true,
                "raw_text": "debug assistant body",
            }),
        }];
        let mut projection = TuiProjection::from_snapshot(snapshot);
        projection.replace_event_window(events_tail, Some(0));

        assert_eq!(projection.event_log().len(), 1);
        assert_eq!(
            projection
                .event_log()
                .last()
                .map(|event| event.summary.as_str()),
            Some("Assistant requested tools: ExecCommand")
        );
        assert_eq!(projection.cursor, Some(0));
        assert!(projection.durable_conversation_events().next().is_none());
    }

    #[test]
    fn stream_event_writes_enabled_presentation_debug_log_incrementally() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let writer =
            crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();

        projection.apply_event(
            sample_event(
                "assistant_round_recorded",
                json!({ "round": 1, "text_preview": "streamed assistant progress" }),
            ),
            &writer,
        );

        let line = std::fs::read_to_string(writer.root().join("presentation.jsonl")).unwrap();
        let record: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["item_kind"], "assistant_progress");
        assert_eq!(
            record["reducer_event_ids"],
            json!(["evt-assistant_round_recorded"])
        );
    }

    #[test]
    fn presentation_debug_log_uses_adjacent_command_window() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let writer =
            crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();

        projection.apply_event(
            sample_event(
                "process_execution_requested",
                json!({ "exec_command_cmd": "cargo test" }),
            ),
            &writer,
        );
        projection.apply_event(
            sample_event(
                "tool_executed",
                json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test",
                    "duration_ms": 12,
                    "exit_status": 0,
                    "stdout_preview": "ok"
                }),
            ),
            &writer,
        );

        let line = std::fs::read_to_string(writer.root().join("presentation.jsonl")).unwrap();
        let record: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["item_kind"], "command_executed");
        assert_eq!(
            record["reducer_event_ids"],
            json!(["evt-process_execution_requested", "evt-tool_executed"])
        );
    }

    #[test]
    fn stream_event_does_not_write_presentation_debug_log_by_default() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let writer = crate::tui::logging::TuiLogWriter::new_temp().unwrap();

        projection.apply_event(
            sample_event(
                "assistant_round_recorded",
                json!({ "round": 1, "text_preview": "streamed assistant progress" }),
            ),
            &writer,
        );

        assert!(!writer.root().join("presentation.jsonl").exists());
    }

    #[test]
    fn projection_prepends_older_event_page_and_updates_history_cursor() {
        let snapshot = sample_snapshot();
        let events_tail = vec![sample_event_envelope("evt-newer", 3)];
        let mut projection = TuiProjection::from_snapshot(snapshot);
        projection.replace_event_window(events_tail, Some(3));

        let added = projection.prepend_event_history_page(
            vec![
                sample_event_envelope("evt-older-2", 2),
                sample_event_envelope("evt-older-1", 1),
            ],
            Some(1),
            true,
        );

        let ids = projection
            .event_log()
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(added, 2);
        assert_eq!(ids, vec!["evt-older-1", "evt-older-2", "evt-newer"]);
        assert_eq!(projection.history_oldest_cursor, Some(1));
        assert!(projection.history_has_older);
    }

    #[test]
    fn projection_merges_tail_refresh_without_dropping_paged_history() {
        let snapshot = sample_snapshot();
        let events_tail = vec![sample_event_envelope("evt-newer", 3)];
        let mut projection = TuiProjection::from_snapshot(snapshot);
        projection.replace_event_window(events_tail, Some(3));
        projection.set_event_history_state(Some(3), true);

        let added = projection.prepend_event_history_page(
            vec![
                sample_event_envelope("evt-older-2", 2),
                sample_event_envelope("evt-older-1", 1),
            ],
            Some(1),
            true,
        );
        assert_eq!(added, 2);

        projection.merge_event_tail(
            vec![
                sample_event_envelope("evt-newer", 3),
                sample_event_envelope("evt-live", 4),
            ],
            Some(4),
        );

        let ids = projection
            .event_log()
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["evt-older-1", "evt-older-2", "evt-newer", "evt-live"]
        );
        assert_eq!(projection.cursor, Some(4));
    }

    #[test]
    fn projection_merges_tail_refresh_truncates_after_chronological_sort() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let events_tail = (0..EVENT_LOG_LIMIT)
            .map(|index| {
                sample_event_envelope(&format!("evt-existing-{index}"), (index + 100) as u64)
            })
            .collect::<Vec<_>>();
        projection.replace_event_window(events_tail, Some((EVENT_LOG_LIMIT + 99) as u64));

        projection.merge_event_tail(vec![sample_event_envelope("evt-old-refresh", 1)], None);

        let ids = projection
            .event_log()
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        let newest_id = format!("evt-existing-{}", EVENT_LOG_LIMIT - 1);
        assert_eq!(projection.event_log().len(), EVENT_LOG_LIMIT);
        assert!(!ids.contains(&"evt-old-refresh"));
        assert!(ids.contains(&"evt-existing-0"));
        assert!(ids.iter().any(|id| *id == newest_id.as_str()));
    }

    #[test]
    fn projection_refresh_keeps_latest_live_event_at_log_cap() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let events_tail = (1..=EVENT_LOG_LIMIT as u64)
            .map(|seq| sample_event_envelope(&format!("evt-existing-{seq}"), seq))
            .collect::<Vec<_>>();
        projection.replace_event_window(events_tail, Some(EVENT_LOG_LIMIT as u64));

        projection.apply_event(
            AgentStreamEvent {
                id: "evt-live".into(),
                event: "assistant_round_recorded".into(),
                data: sample_event_envelope("evt-live", EVENT_LOG_LIMIT as u64 + 1),
            },
            &test_log_writer(),
        );
        projection.merge_event_tail(vec![sample_event_envelope("evt-old-refresh", 1)], None);

        let ids = projection
            .event_log()
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(projection.event_log().len(), EVENT_LOG_LIMIT);
        assert!(ids.contains(&"evt-live"));
        assert!(!ids.contains(&"evt-old-refresh"));
        assert_eq!(projection.cursor, Some(EVENT_LOG_LIMIT as u64 + 1));
    }

    #[test]
    fn projection_caps_prepended_history_and_keeps_cursor_on_retained_oldest() {
        let snapshot = sample_snapshot();
        let events_tail = vec![sample_event_envelope("evt-live", 5000)];
        let mut projection = TuiProjection::from_snapshot(snapshot);
        projection.replace_event_window(events_tail, Some(5000));

        let first_page = (0..16_378)
            .rev()
            .map(|seq| sample_event_envelope(&format!("evt-page-a-{seq}"), seq))
            .collect::<Vec<_>>();
        let first_added = projection.prepend_event_history_page(first_page, Some(0), true);
        assert_eq!(first_added, 16_378);
        assert_eq!(projection.event_log().len(), 16_379);
        assert!(projection.history_has_older);

        let second_page = (16_378..16_398)
            .rev()
            .map(|seq| sample_event_envelope(&format!("evt-page-b-{seq}"), seq))
            .collect::<Vec<_>>();
        let second_added = projection.prepend_event_history_page(second_page, Some(16_378), true);

        assert_eq!(second_added, 5);
        assert_eq!(projection.event_log().len(), 16_384);
        assert_eq!(
            projection
                .event_log()
                .first()
                .map(|event| event.id.as_str()),
            Some("evt-page-b-16393")
        );
        assert_eq!(projection.history_oldest_cursor, Some(16_393));
        assert!(!projection.history_has_older);
        assert!(projection.event_history_at_local_cap());
    }

    #[test]
    fn projection_bootstrap_preserves_agent_identity_contract() {
        let projection = TuiProjection::from_snapshot(sample_snapshot());

        assert_eq!(
            projection.agent.identity.ownership,
            AgentOwnership::SelfOwned
        );
        assert_eq!(
            projection.agent.identity.profile_preset,
            AgentProfilePreset::PublicNamed
        );
        assert_eq!(
            projection.agent.identity.contract_badge(),
            "public/self_owned (public_named)"
        );
    }

    #[test]
    fn projection_applies_brief_and_message_events() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let brief = BriefRecord::new("default", BriefKind::Result, "streamed brief", None, None);
        let message = sample_message();

        projection.apply_event(
            sample_event("brief_created", serde_json::to_value(&brief).unwrap()),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event("message_enqueued", serde_json::to_value(&message).unwrap()),
            &test_log_writer(),
        );

        assert!(projection
            .event_log()
            .iter()
            .any(|event| event.kind == "brief_created"));
        assert!(projection
            .durable_operator_message_ids()
            .contains(&message.id));
        let lanes = projection
            .timeline_events()
            .map(|event| event.lane)
            .collect::<Vec<_>>();
        assert_eq!(
            lanes,
            vec![ProjectionEventLane::Timeline, ProjectionEventLane::Timeline]
        );
    }

    #[test]
    fn projection_logs_provider_rounds_as_progress_activity() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "provider_round_completed",
                json!({ "round": 1, "stop_reason": "end_turn" }),
            ),
            &test_log_writer(),
        );

        let activity = projection.recent_activity_events();
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].kind, "provider_round_completed");
        assert_eq!(
            projection.event_log().last().map(|event| event.lane),
            Some(ProjectionEventLane::Debug)
        );
        assert_eq!(
            projection
                .event_log()
                .last()
                .map(|event| event.summary.as_str()),
            Some("Provider round 1: model=model; stop=end_turn; tokens unavailable; tools=0")
        );
    }

    #[test]
    fn hidden_current_turn_events_stop_at_current_run_boundary() {
        let mut snapshot = sample_snapshot();
        snapshot.agent.agent.status = crate::types::AgentStatus::AwakeRunning;
        snapshot.agent.agent.turn_index = 2;
        snapshot.agent.agent.current_run_id = Some("run-2".into());
        let mut projection = TuiProjection::from_snapshot(snapshot);

        projection.apply_event(
            sample_event_with_id(
                "evt-prev-assistant-round",
                "assistant_round_recorded",
                json!({
                    "run_id": "run-1",
                    "turn_index": 1,
                    "round": 1,
                    "text_preview": "previous turn"
                }),
            ),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event(
                "turn_started",
                json!({
                    "run_id": "run-2",
                    "turn_index": 2,
                    "message_id": "msg-2"
                }),
            ),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event_with_id(
                "evt-current-assistant-round",
                "assistant_round_recorded",
                json!({
                    "run_id": "run-2",
                    "turn_index": 2,
                    "round": 1,
                    "text_preview": "current turn"
                }),
            ),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event(
                "tool_executed",
                json!({
                    "tool_name": "ExecCommand",
                    "exec_command_cmd": "cargo test"
                }),
            ),
            &test_log_writer(),
        );

        let events = projection.hidden_current_turn_events(OperatorDisplayMode::Info);
        assert_eq!(
            events
                .iter()
                .map(|event| event.summary.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Assistant round: current turn",
                "Command finished: cargo test"
            ]
        );
    }

    #[test]
    fn brief_visibility_marks_operator_wait_and_completed_work() {
        let mut snapshot = sample_snapshot();
        snapshot.agent.closure.waiting_reason = Some(WaitingReason::AwaitingOperatorInput);
        let mut projection = TuiProjection::from_snapshot(snapshot);
        let wait_brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Need operator input",
            None,
            None,
        );
        assert_eq!(
            {
                projection.apply_event(
                    sample_event("brief_created", serde_json::to_value(&wait_brief).unwrap()),
                    &test_log_writer(),
                );
                projection
                    .event_log()
                    .last()
                    .map(|event| projection.operator_visibility(event))
            },
            Some(OperatorVisibility::ActionRequired)
        );

        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let mut completed = WorkItemRecord::new("default", "finish docs", WorkItemState::Completed);
        completed.id = "work-done".into();
        projection.work_items = vec![completed];
        let mut work_brief =
            BriefRecord::new("default", BriefKind::Result, "Work done", None, None);
        work_brief.work_item_id = Some("work-done".into());
        projection.apply_event(
            sample_event("brief_created", serde_json::to_value(&work_brief).unwrap()),
            &test_log_writer(),
        );
        assert_eq!(
            projection
                .event_log()
                .last()
                .map(|event| projection.operator_visibility(event)),
            Some(OperatorVisibility::WorkDone)
        );
    }

    #[test]
    fn projection_event_record_helper_uses_default_context() {
        let wait_brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Need operator input",
            None,
            None,
        );
        let wait_event =
            projection_event_record("brief_created", serde_json::to_value(&wait_brief).unwrap());
        assert_eq!(
            wait_event.presentation.visibility,
            OperatorVisibility::TurnResult
        );
    }

    #[test]
    fn assistant_round_operator_projection_renders_progress_fields() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event_with_id(
                "evt-assistant-tools",
                "assistant_round_recorded",
                json!({
                    "redacted": true,
                    "stop_reason": "tool_use",
                    "text_preview": null,
                    "tool_names": ["ExecCommand", "ReadFile"],
                    "tool_call_count": 2,
                    "has_tool_calls": true,
                }),
            ),
            &test_log_writer(),
        );
        assert_eq!(
            projection
                .event_log()
                .last()
                .map(|event| event.summary.as_str()),
            Some("Assistant requested tools: ExecCommand, ReadFile")
        );

        projection.apply_event(
            sample_event_with_id(
                "evt-assistant-empty",
                "assistant_round_recorded",
                json!({
                    "redacted": true,
                    "stop_reason": "end_turn",
                    "text_preview": null,
                    "tool_names": [],
                    "tool_call_count": 0,
                    "has_text": false,
                    "has_tool_calls": false,
                }),
            ),
            &test_log_writer(),
        );
        assert_eq!(
            projection
                .event_log()
                .last()
                .map(|event| event.summary.as_str()),
            Some("Assistant round completed without text (stop=end_turn)")
        );
    }

    #[test]
    fn transient_activity_events_do_not_force_state_refresh() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        for (kind, payload) in [
            (
                "provider_round_completed",
                json!({ "round": 1, "text_preview": "working" }),
            ),
            (
                "text_only_round_observed",
                json!({ "round": 2, "text_preview": "still working" }),
            ),
            (
                "max_output_tokens_recovery",
                json!({ "agent_id": "default", "attempt": 1 }),
            ),
            (
                "runtime_error",
                json!({
                    "message_id": "message-1",
                    "message_kind": "operator_prompt",
                    "error": "visible error",
                    "token_usage": null,
                }),
            ),
            (
                "deferred_to_fallback",
                json!({
                    "operator_message": "OpenAI Codex authentication failed. Queued fallback turn on anthropic/claude-sonnet-4-6.",
                    "fallback_model_ref": "anthropic/claude-sonnet-4-6",
                }),
            ),
            (
                "provider_failed_needs_recovery",
                json!({
                    "operator_message": "Provider failed after output. Queued recovery turn on anthropic/claude-sonnet-4-6.",
                    "fallback_model_ref": "anthropic/claude-sonnet-4-6",
                }),
            ),
            (
                "message_admitted",
                json!({
                    "message_id": "message-1",
                    "agent_id": "default",
                    "kind": "operator_prompt",
                    "origin": { "kind": "operator", "actor_id": null },
                    "authority_class": "operator_instruction",
                    "delivery_surface": "http_control_prompt",
                    "admission_context": "local_process",
                    "correlation_id": null,
                    "causation_id": null,
                }),
            ),
            ("control_applied", json!({ "action": "pause" })),
        ] {
            projection.apply_event(sample_event(kind, payload), &test_log_writer());
        }

        assert!(projection.stale_slices.is_empty());
    }

    #[test]
    fn provider_lineage_failure_events_are_visible_in_info_conversation() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "deferred_to_fallback",
                json!({
                    "operator_message": "OpenAI Codex authentication failed. Queued fallback turn on anthropic/claude-sonnet-4-6.",
                    "fallback_model_ref": "anthropic/claude-sonnet-4-6",
                }),
            ),
            &test_log_writer(),
        );

        let events = projection.presentation_events(OperatorDisplayMode::Info);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "deferred_to_fallback");
        assert_eq!(events[0].lane, ProjectionEventLane::Timeline);
        assert!(events[0]
            .summary
            .contains("OpenAI Codex authentication failed"));
    }

    #[test]
    fn projection_recent_activity_clears_after_brief_or_terminal_event() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "provider_round_completed",
                json!({ "round": 1, "text_preview": "partial" }),
            ),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event("tool_executed", json!({ "tool_name": "ExecCommand" })),
            &test_log_writer(),
        );

        let activity = projection.recent_activity_events();
        assert_eq!(activity.len(), 2);
        assert_eq!(activity[0].kind, "provider_round_completed");
        assert_eq!(activity[1].kind, "tool_executed");

        projection.apply_event(
            sample_event(
                "brief_created",
                serde_json::to_value(BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    "done",
                    None,
                    None,
                ))
                .unwrap(),
            ),
            &test_log_writer(),
        );
        assert!(projection.recent_activity_events().is_empty());
    }

    #[test]
    fn projection_recent_log_events_keep_high_signal_and_drop_noise() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event("agent_state_changed", json!({ "status": "Idle" })),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event(
                "provider_round_completed",
                json!({ "round": 1, "text_preview": "partial" }),
            ),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event(
                "tool_executed",
                json!({ "tool_name": "ExecCommand", "exec_command_cmd": "git status" }),
            ),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event(
                "operator_notification_requested",
                json!({
                    "id": "notification-1",
                    "summary": "needs review",
                    "message": "needs review",
                    "created_at": Utc::now(),
                }),
            ),
            &test_log_writer(),
        );

        let events = projection.recent_log_events(10);
        let kinds = events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "provider_round_completed",
                "tool_executed",
                "operator_notification_requested"
            ]
        );
    }

    #[test]
    fn info_presentation_events_survive_callback_delivered_churn() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        // Seed a brief and an operator message early in the event log.
        let brief = BriefRecord::new("default", BriefKind::Result, "work completed", None, None);
        projection.apply_event(
            sample_event("brief_created", serde_json::to_value(&brief).unwrap()),
            &test_log_writer(),
        );
        let message = sample_message();
        projection.apply_event(
            sample_event("message_enqueued", serde_json::to_value(&message).unwrap()),
            &test_log_writer(),
        );

        // Fill with enough callback_delivered events to exceed the
        // durable conversation log limit (256) so the early brief/message
        // are pushed out of the durable conversation log, but stay within
        // the event_log limit (1024) so they remain in the event_log.
        let callback_count = 266;
        for index in 0..callback_count {
            projection.apply_event(
                AgentStreamEvent {
                    id: format!("evt-callback-{index}"),
                    event: "callback_delivered".into(),
                    data: StreamEventEnvelope {
                        id: format!("evt-callback-{index}"),
                        event_seq: (index + 3) as u64,
                        ts: Utc::now(),
                        agent_id: "default".into(),
                        event_type: "callback_delivered".into(),
                        projection: None,
                        provenance: None,
                        payload: json!({
                            "waiting_intent_id": format!("wait-{index}"),
                            "source": "github",
                        }),
                    },
                },
                &test_log_writer(),
            );
        }

        // The durable conversation log should be full of callback_delivered,
        // pushing the early brief/message out.
        assert!(
            projection
                .durable_conversation_events()
                .all(|e| e.kind == "callback_delivered"),
            "durable log should be all callback_delivered"
        );

        // But presentation_events(Info) must still surface the brief and message
        // because it also scans the full event_log for durable events.
        let info_events = projection.presentation_events(OperatorDisplayMode::Info);
        let brief_count = info_events
            .iter()
            .filter(|e| e.kind == "brief_created")
            .count();
        let message_count = info_events
            .iter()
            .filter(|e| e.kind == "message_enqueued")
            .count();
        assert!(
            brief_count >= 1,
            "Info presentation should include brief_created even after callback_delivered churn"
        );
        assert!(
            message_count >= 1,
            "Info presentation should include message_enqueued even after callback_delivered churn"
        );
    }

    #[test]
    fn durable_conversation_events_survive_debug_event_churn() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "work_item_written",
                json!({
                    "action": "created",
                    "record": {
                        "id": "work-1",
                        "agent_id": "default",
                        "workspace_id": "agent_home",
                        "objective": "operator",
                        "state": "open",
                        "plan_status": "draft",
                        "todo_list": [],
                        "created_at": Utc::now(),
                        "updated_at": Utc::now()
                    }
                }),
            ),
            &test_log_writer(),
        );

        for index in 0..=EVENT_LOG_LIMIT {
            projection.apply_event(
                AgentStreamEvent {
                    id: format!("evt-debug-{index}"),
                    event: "provider_round_completed".into(),
                    data: StreamEventEnvelope {
                        id: format!("evt-debug-{index}"),
                        event_seq: (index + 2) as u64,
                        ts: Utc::now(),
                        agent_id: "default".into(),
                        event_type: "provider_round_completed".into(),
                        projection: None,
                        provenance: None,
                        payload: json!({ "text_preview": format!("partial-{index}") }),
                    },
                },
                &test_log_writer(),
            );
        }

        assert_eq!(projection.event_log().len(), EVENT_LOG_LIMIT);
        assert_eq!(
            projection
                .durable_conversation_events()
                .last()
                .map(|event| event.summary.as_str()),
            Some("Work item Open: operator")
        );
        assert!(projection
            .event_log()
            .iter()
            .any(|event| event.id == format!("evt-debug-{EVENT_LOG_LIMIT}")));
    }

    #[test]
    fn projection_updates_workspace_from_workspace_events() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "workspace_entered",
                json!({
                    "workspace_id": "ws-main",
                    "workspace_anchor": "/tmp/ws-main",
                    "execution_root_id": "root-1",
                    "execution_root": "/tmp/ws-main/worktree",
                    "projection_kind": "git_worktree_root",
                    "access_mode": "exclusive_write",
                    "cwd": "/tmp/ws-main/worktree",
                }),
            ),
            &test_log_writer(),
        );

        assert_eq!(
            projection
                .workspace
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_id.as_str()),
            Some("ws-main")
        );
        assert_eq!(
            projection
                .workspace
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.projection_kind),
            Some(WorkspaceProjectionKind::GitWorktreeRoot)
        );
        assert_eq!(
            projection
                .agent
                .agent
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.cwd.as_path()),
            Some(PathBuf::from("/tmp/ws-main/worktree").as_path())
        );
        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::Workspace));
        assert!(projection.workspace.active_workspace_occupancy.is_none());
    }

    #[test]
    fn projection_updates_workspace_from_workspace_used_event() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        projection.agent.agent.worktree_session = projection.workspace.worktree_session.clone();

        projection.apply_event(
            sample_event(
                "workspace_used",
                json!({
                    "workspace_id": crate::types::AGENT_HOME_WORKSPACE_ID,
                    "workspace_anchor": "/tmp/agent-home",
                    "execution_root_id": "canonical_root:agent_home",
                    "execution_root": "/tmp/agent-home",
                    "projection_kind": "canonical_root",
                    "access_mode": "exclusive_write",
                    "cwd": "/tmp/agent-home",
                }),
            ),
            &test_log_writer(),
        );

        let entry = projection
            .workspace
            .active_workspace_entry
            .as_ref()
            .expect("workspace_used should update active workspace");
        assert_eq!(entry.workspace_id, crate::types::AGENT_HOME_WORKSPACE_ID);
        assert_eq!(entry.execution_root, PathBuf::from("/tmp/agent-home"));
        assert_eq!(
            projection
                .agent
                .agent
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_id.as_str()),
            Some(crate::types::AGENT_HOME_WORKSPACE_ID)
        );
        assert!(!projection.stale_slices.contains(&ProjectionSlice::Agent));
        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::Workspace));
        assert_eq!(
            projection
                .workspace
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.cwd.as_path()),
            Some(PathBuf::from("/tmp/agent-home").as_path())
        );
        assert!(projection.workspace.worktree_session.is_none());
        assert!(projection.agent.agent.worktree_session.is_none());
    }

    #[test]
    fn projection_updates_model_from_override_lifecycle_events() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let mut model = projection.agent.model.clone();
        let override_model = crate::config::ModelRef::parse("openai/gpt-5.4").unwrap();
        model.source = AgentModelSource::AgentOverride;
        model.effective_model = override_model.clone();
        model.requested_model = Some(override_model.clone());
        model.active_model = Some(override_model.clone());
        model.override_model = Some(override_model);
        model.override_reasoning_effort = Some("high".into());

        projection.apply_event(
            sample_event(
                "agent_model_override_set",
                json!({
                    "agent_id": "default",
                    "model": model,
                    "pending_next_turn": false,
                }),
            ),
            &test_log_writer(),
        );

        assert_eq!(
            projection
                .agent
                .model
                .override_model
                .as_ref()
                .map(|model| model.as_string()),
            Some("openai/gpt-5.4".into())
        );
        assert_eq!(
            projection.agent.model.override_reasoning_effort.as_deref(),
            Some("high")
        );
        assert!(!projection.stale_slices.contains(&ProjectionSlice::Agent));
    }

    #[test]
    fn projection_updates_model_from_provider_round_events() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "provider_round_completed",
                json!({
                    "requested_model": "openai/gpt-5.4",
                    "active_model": "anthropic/claude-sonnet-4-6",
                }),
            ),
            &test_log_writer(),
        );

        assert_eq!(
            projection
                .agent
                .model
                .requested_model
                .as_ref()
                .map(|model| model.as_string()),
            Some("openai/gpt-5.4".into())
        );
        assert_eq!(
            projection
                .agent
                .model
                .active_model
                .as_ref()
                .map(|model| model.as_string()),
            Some("anthropic/claude-sonnet-4-6".into())
        );
        assert!(projection.agent.model.fallback_active);
        assert!(!projection.stale_slices.contains(&ProjectionSlice::Agent));

        projection.apply_event(
            sample_event_with_id(
                "evt-provider-round-completed-clear",
                "provider_round_completed",
                json!({
                    "requested_model": null,
                    "active_model": null,
                    "fallback_active": false,
                }),
            ),
            &test_log_writer(),
        );

        assert!(projection.agent.model.requested_model.is_none());
        assert!(projection.agent.model.active_model.is_none());
        assert!(!projection.agent.model.fallback_active);
    }

    #[test]
    fn projection_tracks_task_and_work_item_updates() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let mut task = sample_task();
        task.id = "task-stream".into();
        task.status = TaskStatus::Running;
        task.updated_at = Utc::now();

        let mut work_item = WorkItemRecord::new("default", "queued delivery", WorkItemState::Open);
        work_item.id = "work-stream".into();

        projection.apply_event(
            sample_event("task_status_updated", serde_json::to_value(&task).unwrap()),
            &test_log_writer(),
        );
        projection.apply_event(
            sample_event(
                "work_item_written",
                json!({
                    "action": "created",
                    "record": work_item,
                }),
            ),
            &test_log_writer(),
        );

        assert_eq!(
            projection.tasks.last().map(|record| record.id.as_str()),
            Some("task-stream")
        );
        assert!(projection
            .work_items
            .iter()
            .any(|record| record.id == "work-stream" && record.state == WorkItemState::Open));
    }

    #[test]
    fn projection_upserts_active_tasks_and_removes_terminal_updates() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        projection.tasks.clear();
        let mut task = sample_task();
        task.id = "task-active".into();
        task.status = TaskStatus::Queued;

        projection.apply_event(
            sample_event("task_created", serde_json::to_value(&task).unwrap()),
            &test_log_writer(),
        );
        assert_eq!(projection.tasks.len(), 1);
        assert_eq!(projection.tasks[0].status, TaskStatus::Queued);

        task.status = TaskStatus::Running;
        task.summary = Some("running latest".into());
        task.updated_at = task.updated_at + chrono::Duration::seconds(1);
        projection.apply_event(
            sample_event("task_status_updated", serde_json::to_value(&task).unwrap()),
            &test_log_writer(),
        );
        assert_eq!(projection.tasks.len(), 1);
        assert_eq!(
            projection.tasks[0].summary.as_deref(),
            Some("running latest")
        );
        assert_eq!(projection.tasks[0].status, TaskStatus::Running);

        task.status = TaskStatus::Completed;
        task.updated_at = task.updated_at + chrono::Duration::seconds(1);
        projection.apply_event(
            sample_event("task_result_received", serde_json::to_value(&task).unwrap()),
            &test_log_writer(),
        );
        assert!(projection.tasks.is_empty());
    }

    #[test]
    fn projection_bootstrap_keeps_only_active_tasks_in_chronological_order() {
        let mut old_running = sample_task();
        old_running.id = "old-running".into();
        old_running.status = TaskStatus::Running;
        old_running.updated_at = Utc::now() - chrono::Duration::seconds(10);

        let mut completed = sample_task();
        completed.id = "completed".into();
        completed.status = TaskStatus::Completed;
        completed.updated_at = Utc::now() - chrono::Duration::seconds(5);

        let mut new_running = sample_task();
        new_running.id = "new-running".into();
        new_running.status = TaskStatus::Running;
        new_running.updated_at = Utc::now();

        let mut snapshot = sample_snapshot();
        snapshot.tasks = vec![new_running, completed, old_running];

        let projection = TuiProjection::from_snapshot(snapshot);
        let task_ids = projection
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(task_ids, vec!["old-running", "new-running"]);
    }

    #[test]
    fn projection_bootstrap_bounds_active_tasks() {
        let now = Utc::now();
        let mut snapshot = sample_snapshot();
        snapshot.tasks = (0..(TASK_TAIL_LIMIT + 5))
            .map(|index| {
                let mut task = sample_task();
                task.id = format!("task-{index}");
                task.status = TaskStatus::Running;
                task.updated_at = now + chrono::Duration::seconds(index as i64);
                task
            })
            .collect();

        let projection = TuiProjection::from_snapshot(snapshot);
        let task_ids = projection
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(projection.tasks.len(), TASK_TAIL_LIMIT);
        assert_eq!(task_ids.first().copied(), Some("task-5"));
        assert_eq!(task_ids.last().copied(), Some("task-54"));
    }

    #[test]
    fn projection_marks_waiting_and_external_triggers_stale_when_event_payload_is_partial() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "waiting_intent_created",
                json!({
                    "waiting_intent_id": "wait-2",
                    "external_trigger_id": "cb-2",
                    "agent_id": "default",
                    "source": "github",
                    "delivery_mode": "enqueue_message",
                }),
            ),
            &test_log_writer(),
        );

        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::WaitingIntents));
        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::ExternalTriggers));
    }

    #[test]
    fn projection_marks_workspace_stale_for_agent_state_events_without_occupancy() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let mut state = projection.agent.agent.clone();
        if let Some(entry) = &mut state.active_workspace_entry {
            entry.workspace_id = "ws-updated".into();
        }

        projection.apply_event(
            sample_event("agent_state_changed", serde_json::to_value(&state).unwrap()),
            &test_log_writer(),
        );

        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::Workspace));
        assert!(projection.workspace.active_workspace_occupancy.is_none());
        assert!(projection.agent.active_workspace_occupancy.is_none());
        assert_eq!(
            projection
                .workspace
                .active_workspace_entry
                .as_ref()
                .map(|e| e.workspace_id.as_str()),
            Some("ws-updated")
        );
    }

    #[test]
    fn projection_keeps_workspace_stale_during_worktree_transitions() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "worktree_entered",
                json!({
                    "worktree": {
                        "original_cwd": "/tmp/ws-boot",
                        "original_branch": "main",
                        "worktree_path": "/tmp/ws-boot/feature",
                        "worktree_branch": "feature/demo"
                    }
                }),
            ),
            &test_log_writer(),
        );

        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::Workspace));
        assert!(projection.stale_slices.contains(&ProjectionSlice::Agent));
        assert_eq!(
            projection
                .workspace
                .worktree_session
                .as_ref()
                .map(|ws| ws.worktree_branch.as_str()),
            Some("feature/demo")
        );
        assert_eq!(
            projection
                .workspace
                .active_workspace_entry
                .as_ref()
                .map(|e| e.cwd.as_path()),
            Some(PathBuf::from("/tmp/ws-boot/feature").as_path())
        );

        projection.apply_event(
            sample_event(
                "worktree_exited",
                json!({
                    "worktree_path": "/tmp/ws-boot/feature",
                    "worktree_branch": "feature/demo",
                    "removed": false
                }),
            ),
            &test_log_writer(),
        );

        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::Workspace));
        assert!(projection.workspace.worktree_session.is_none());
    }

    fn sample_event(kind: &str, payload: Value) -> AgentStreamEvent {
        sample_event_with_id(&format!("evt-{kind}"), kind, payload)
    }

    fn sample_event_with_id(id: &str, kind: &str, payload: Value) -> AgentStreamEvent {
        AgentStreamEvent {
            id: id.to_string(),
            event: kind.to_string(),
            data: sample_event_envelope_with_payload(id, 1, kind, payload),
        }
    }

    fn sample_event_envelope(id: &str, event_seq: u64) -> StreamEventEnvelope {
        sample_event_envelope_with_payload(
            id,
            event_seq,
            "tool_executed",
            json!({ "tool_name": "ExecCommand" }),
        )
    }

    fn sample_event_envelope_with_payload(
        id: &str,
        event_seq: u64,
        kind: &str,
        payload: Value,
    ) -> StreamEventEnvelope {
        StreamEventEnvelope {
            id: id.into(),
            event_seq,
            ts: Utc::now(),
            agent_id: "default".into(),
            event_type: kind.into(),
            projection: None,
            provenance: None,
            payload,
        }
    }

    fn projection_event_record(kind: &str, payload: Value) -> super::ProjectionEventRecord {
        let presentation = present_operator_event(
            kind,
            &payload,
            kind,
            &OperatorPresentationContext::default(),
        );
        super::ProjectionEventRecord {
            id: format!("evt-{kind}"),
            event_seq: 1,
            ts: Utc::now(),
            kind: kind.to_string(),
            lane: super::classify_event_lane(&presentation),
            summary: presentation.summary.clone(),
            presentation,
            payload,
        }
    }

    fn sample_snapshot() -> AgentStateSnapshot {
        AgentStateSnapshot {
            agent: sample_agent_summary(),
            session: StateSessionSnapshot {
                current_run_id: Some("run-1".into()),
                pending_count: 1,
                last_turn: Some(TurnTerminalRecord {
                    turn_index: 1,
                    kind: TurnTerminalKind::Completed,
                    reason: None,
                    last_assistant_message: Some("done".into()),
                    checkpoint: None,
                    completed_at: Utc::now(),
                    duration_ms: 12,
                }),
            },
            tasks: vec![sample_task()],
            timers: vec![TimerRecord {
                id: "timer-1".into(),
                agent_id: "default".into(),
                created_at: Utc::now(),
                duration_ms: 1000,
                interval_ms: None,
                repeat: false,
                status: TimerStatus::Active,
                summary: Some("timer".into()),
                next_fire_at: Some(Utc::now()),
                last_fired_at: None,
                fire_count: 0,
            }],
            work_items: {
                let mut item =
                    WorkItemRecord::new("default", "active delivery", WorkItemState::Open);
                item.todo_list = vec![TodoItem {
                    text: "do it".into(),
                    state: TodoItemState::InProgress,
                }];
                vec![item]
            },
            waiting_intents: vec![WaitingIntentRecord {
                id: "wait-1".into(),
                agent_id: "default".into(),
                scope: WaitingIntentScope::WorkItem,
                work_item_id: None,
                description: "wait".into(),
                source: "github".into(),
                resource: Some("pull_request:251".into()),
                condition: Some("review".into()),
                delivery_mode: CallbackDeliveryMode::EnqueueMessage,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-1".into(),
                created_at: Utc::now(),
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            }],
            external_triggers: vec![ExternalTriggerStateSnapshot {
                external_trigger_id: "cb-1".into(),
                target_agent_id: "default".into(),
                waiting_intent_id: Some("wait-1".into()),
                scope: ExternalTriggerScope::Agent,
                delivery_mode: CallbackDeliveryMode::EnqueueMessage,
                status: ExternalTriggerStatus::Active,
                created_at: Utc::now(),
                revoked_at: None,
                last_delivered_at: None,
                delivery_count: 0,
            }],
            operator_notifications: Vec::new(),
            workspace: StateWorkspaceSnapshot {
                attached_workspaces: vec!["ws-boot".into()],
                active_workspace_entry: Some(crate::types::ActiveWorkspaceEntry {
                    workspace_id: "ws-boot".into(),
                    workspace_anchor: PathBuf::from("/tmp/ws-boot"),
                    execution_root_id: "root-boot".into(),
                    execution_root: PathBuf::from("/tmp/ws-boot"),
                    projection_kind: WorkspaceProjectionKind::CanonicalRoot,
                    access_mode: WorkspaceAccessMode::ExclusiveWrite,
                    cwd: PathBuf::from("/tmp/ws-boot"),
                    occupancy_id: None,
                    projection_metadata: None,
                }),
                active_workspace_occupancy: Some(WorkspaceOccupancyRecord {
                    occupancy_id: "occ-1".into(),
                    execution_root_id: "root-boot".into(),
                    workspace_id: "ws-boot".into(),
                    holder_agent_id: "default".into(),
                    access_mode: WorkspaceAccessMode::SharedRead,
                    acquired_at: Utc::now(),
                    released_at: None,
                }),
                worktree_session: Some(WorktreeSession {
                    original_cwd: PathBuf::from("/tmp/ws-boot"),
                    original_branch: "main".into(),
                    worktree_path: PathBuf::from("/tmp/ws-boot/wt"),
                    worktree_branch: "feature/demo".into(),
                }),
            },
            execution: None,
        }
    }

    fn sample_task() -> TaskRecord {
        TaskRecord {
            id: "task-1".into(),
            agent_id: "default".into(),
            kind: crate::types::TaskKind::ChildAgentTask,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: Some("demo task".into()),
            detail: Some(json!({ "wait_policy": "blocking" })),
            recovery: None,
        }
    }

    fn sample_message() -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            crate::types::AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "stream prompt".into(),
            },
        );
        message.delivery_surface = Some(MessageDeliverySurface::CliPrompt);
        message
    }

    fn sample_agent_summary() -> AgentSummary {
        let mut state = AgentState::new("default");
        state.status = crate::types::AgentStatus::AwakeIdle;
        state.pending = 1;
        state.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
            workspace_id: "ws-boot".into(),
            workspace_anchor: PathBuf::from("/tmp/ws-boot"),
            execution_root_id: "canonical_root:ws-boot".into(),
            execution_root: PathBuf::from("/tmp/ws-boot"),
            projection_kind: WorkspaceProjectionKind::CanonicalRoot,
            access_mode: WorkspaceAccessMode::ExclusiveWrite,
            cwd: PathBuf::from("/tmp/ws-boot"),
            occupancy_id: None,
            projection_metadata: None,
        });
        AgentSummary {
            identity: AgentIdentityView {
                agent_id: "default".into(),
                kind: AgentKind::Default,
                visibility: AgentVisibility::Public,
                ownership: AgentOwnership::SelfOwned,
                profile_preset: AgentProfilePreset::PublicNamed,
                status: AgentRegistryStatus::Active,
                is_default_agent: true,
                parent_agent_id: None,
                lineage_parent_agent_id: None,
                delegated_from_task_id: None,
            },
            agent: state,
            active_task_count: 0,
            lifecycle: AgentLifecycleHint::default(),
            scheduling_posture: Default::default(),
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
                override_reasoning_effort: None,
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
                workspace_anchor: PathBuf::from("/tmp"),
                execution_root: PathBuf::from("/tmp"),
                cwd: PathBuf::from("/tmp"),
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
            active_wait_conditions: Vec::new(),
            active_external_triggers: Vec::new(),
            recent_operator_notifications: Vec::new(),
            recent_brief_count: 1,
            recent_event_count: 1,
        }
    }
}
