#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::logging::TuiLogWriter;

use crate::{
    client::{AgentStateSnapshot, AgentStreamEvent, StateSessionSnapshot, StateWorkspaceSnapshot},
    system::{WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        ActiveWorkspaceEntry, AgentState, AgentSummary, BriefRecord, ClosureDecision,
        ExternalTriggerStateSnapshot, MessageEnvelope, TaskRecord, TimerRecord, TimerStatus,
        TranscriptEntry, TranscriptEntryKind, WaitingIntentRecord, WorkItemRecord, WorkItemState,
        WorkPlanSnapshot, WorktreeSession,
    },
};

const BRIEF_TAIL_LIMIT: usize = 24;
const TRANSCRIPT_TAIL_LIMIT: usize = 100;
const TASK_TAIL_LIMIT: usize = 50;
const TIMER_TAIL_LIMIT: usize = 50;
const EVENT_LOG_LIMIT: usize = 256;
const DURABLE_CONVERSATION_LOG_LIMIT: usize = 256;
static TUI_LOG_WRITE_WARNED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ProjectionSlice {
    Agent,
    Session,
    Tasks,
    TranscriptTail,
    BriefsTail,
    Timers,
    WorkItems,
    WorkPlan,
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
    pub(crate) seq: u64,
    pub(crate) ts: DateTime<Utc>,
    pub(crate) kind: String,
    pub(crate) lane: ProjectionEventLane,
    pub(crate) summary: String,
    pub(crate) payload: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct TuiProjection {
    pub(crate) agent: AgentSummary,
    pub(crate) session: StateSessionSnapshot,
    pub(crate) tasks: Vec<TaskRecord>,
    pub(crate) transcript_tail: Vec<TranscriptEntry>,
    pub(crate) briefs_tail: Vec<BriefRecord>,
    pub(crate) timers: Vec<TimerRecord>,
    pub(crate) work_items: Vec<WorkItemRecord>,
    pub(crate) work_plan: Option<WorkPlanSnapshot>,
    pub(crate) waiting_intents: Vec<WaitingIntentRecord>,
    pub(crate) external_triggers: Vec<ExternalTriggerStateSnapshot>,
    pub(crate) operator_notifications: Vec<crate::types::OperatorNotificationRecord>,
    pub(crate) workspace: StateWorkspaceSnapshot,
    pub(crate) cursor: Option<String>,
    pub(crate) stale_slices: BTreeSet<ProjectionSlice>,
    event_log: Vec<ProjectionEventRecord>,
    durable_conversation_log: Vec<ProjectionEventRecord>,
}

impl TuiProjection {
    pub(crate) fn from_snapshot(snapshot: AgentStateSnapshot) -> Self {
        let external_triggers = snapshot.external_triggers;
        let operator_notifications = snapshot.operator_notifications;

        Self {
            agent: snapshot.agent,
            session: snapshot.session,
            tasks: snapshot.tasks,
            transcript_tail: snapshot.transcript_tail,
            briefs_tail: snapshot.briefs_tail,
            timers: snapshot.timers,
            work_items: snapshot.work_items,
            work_plan: snapshot.work_plan,
            waiting_intents: snapshot.waiting_intents,
            external_triggers,
            operator_notifications,
            workspace: snapshot.workspace,
            cursor: snapshot.cursor,
            stale_slices: BTreeSet::new(),
            event_log: Vec::new(),
            durable_conversation_log: Vec::new(),
        }
    }

    pub(crate) fn reset_from_snapshot(&mut self, snapshot: AgentStateSnapshot) {
        *self = Self::from_snapshot(snapshot);
    }

    pub(crate) fn apply_event(&mut self, event: AgentStreamEvent, log_writer: &TuiLogWriter) {
        let record = ProjectionEventRecord {
            id: event.id.clone(),
            seq: event.data.seq,
            ts: event.data.ts,
            kind: event.data.event_type.clone(),
            lane: classify_event_lane(&event.data.event_type),
            summary: summarize_event(&event),
            payload: event.data.payload.clone(),
        };
        push_limited(&mut self.event_log, record.clone(), EVENT_LOG_LIMIT);
        if is_durable_conversation_kind(&record.kind) {
            push_limited(
                &mut self.durable_conversation_log,
                record.clone(),
                DURABLE_CONVERSATION_LOG_LIMIT,
            );
        }
        if let Err(error) = log_writer.write_event(&record) {
            if !TUI_LOG_WRITE_WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!("failed to persist TUI log event: {error}");
            }
        }
        self.cursor = Some(event.id.clone());

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
            "message_enqueued" => {
                if let Some(message) = decode_payload::<MessageEnvelope>(&event.data.payload) {
                    self.append_transcript_message(message);
                    self.stale_slices.remove(&ProjectionSlice::TranscriptTail);
                } else {
                    self.mark_stale([ProjectionSlice::TranscriptTail]);
                }
            }
            "brief_created" => {
                if let Some(brief) = decode_payload::<BriefRecord>(&event.data.payload) {
                    push_limited_sorted(
                        &mut self.briefs_tail,
                        brief,
                        BRIEF_TAIL_LIMIT,
                        |left, right| left.created_at.cmp(&right.created_at),
                    );
                    self.stale_slices.remove(&ProjectionSlice::BriefsTail);
                } else {
                    self.mark_stale([ProjectionSlice::BriefsTail]);
                }
            }
            "task_created" | "task_status_updated" | "task_result_received" => {
                if let Some(task) = decode_payload::<TaskRecord>(&event.data.payload) {
                    push_limited_sorted(&mut self.tasks, task, TASK_TAIL_LIMIT, |left, right| {
                        left.updated_at.cmp(&right.updated_at)
                    });
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
            "work_plan_snapshot_written" => {
                if let Some(plan) = decode_payload::<WorkPlanSnapshot>(&event.data.payload) {
                    self.work_plan = Some(plan);
                    self.stale_slices.remove(&ProjectionSlice::WorkPlan);
                } else {
                    self.mark_stale([ProjectionSlice::WorkPlan]);
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
            "workspace_entered" => {
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
            "provider_round_completed"
            | "text_only_round_observed"
            | "max_output_tokens_recovery"
            | "runtime_error" => {
                self.mark_stale([ProjectionSlice::TranscriptTail]);
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
            "message_admitted" | "message_processing_started" | "control_applied" => {
                self.mark_stale([ProjectionSlice::Agent, ProjectionSlice::Session]);
            }
            _ => {}
        }
    }

    pub(crate) fn event_log(&self) -> &[ProjectionEventRecord] {
        &self.event_log
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

    pub(crate) fn recent_activity_events(&self) -> Vec<&ProjectionEventRecord> {
        let mut events = Vec::new();
        for event in self.event_log.iter().rev() {
            if is_activity_reset_kind(&event.kind) {
                break;
            }
            if is_ephemeral_activity_kind(&event.kind) {
                events.push(event);
            }
            if events.len() >= 4 {
                break;
            }
        }
        events.reverse();
        events
    }

    pub(crate) fn recent_log_events(&self, limit: usize) -> Vec<&ProjectionEventRecord> {
        self.event_log
            .iter()
            .rev()
            .filter(|event| is_loggable_event_kind(&event.kind))
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

    fn append_transcript_message(&mut self, message: MessageEnvelope) {
        let entry = TranscriptEntry {
            id: format!("stream-message-{}", message.id),
            agent_id: message.agent_id.clone(),
            created_at: message.created_at,
            kind: TranscriptEntryKind::IncomingMessage,
            round: None,
            related_message_id: Some(message.id.clone()),
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            data: json!({
                "kind": message.kind,
                "origin": message.origin,
                "trust": message.trust,
                "authority_class": message.authority_class,
                "delivery_surface": message.delivery_surface,
                "admission_context": message.admission_context,
                "priority": message.priority,
                "body": message.body,
                "metadata": message.metadata,
                "correlation_id": message.correlation_id,
                "causation_id": message.causation_id,
            }),
        };
        push_limited_sorted(
            &mut self.transcript_tail,
            entry,
            TRANSCRIPT_TAIL_LIMIT,
            |left, right| left.created_at.cmp(&right.created_at),
        );
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

        self.agent.agent.active_workspace_entry = Some(entry);
        self.agent.active_workspace_occupancy = None;
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
        WorkItemState::Done => 2,
    }
}

pub(crate) fn is_durable_conversation_kind(kind: &str) -> bool {
    matches!(
        kind,
        "message_enqueued"
            | "brief_created"
            | "runtime_error"
            | "turn_terminal"
            | "task_created"
            | "task_status_updated"
            | "task_result_received"
            | "work_item_written"
            | "waiting_intent_created"
            | "waiting_intent_cancelled"
            | "callback_delivered"
            | "operator_notification_requested"
            | "workspace_entered"
            | "workspace_exited"
            | "workspace_detached"
            | "worktree_entered"
            | "worktree_exited"
    )
}

pub(crate) fn is_ephemeral_activity_kind(kind: &str) -> bool {
    matches!(
        kind,
        "provider_round_completed"
            | "text_only_round_observed"
            | "tool_executed"
            | "tool_execution_failed"
            | "skill_activated"
            | "system_tick_emitted"
            | "workspace_attached"
            | "worktree_auto_cleaned_up"
            | "worktree_auto_cleanup_failed"
            | "task_worktree_branch_cleanup_retained"
    )
}

fn is_loggable_event_kind(kind: &str) -> bool {
    matches!(
        kind,
        "message_enqueued"
            | "provider_round_completed"
            | "text_only_round_observed"
            | "tool_executed"
            | "tool_execution_failed"
            | "task_created"
            | "task_status_updated"
            | "task_result_received"
            | "work_item_written"
            | "waiting_intent_created"
            | "waiting_intent_cancelled"
            | "callback_delivered"
            | "operator_notification_requested"
            | "workspace_entered"
            | "workspace_exited"
            | "worktree_entered"
            | "worktree_exited"
            | "runtime_error"
            | "turn_terminal"
    )
}

fn is_activity_reset_kind(kind: &str) -> bool {
    matches!(kind, "brief_created" | "turn_terminal" | "runtime_error")
}

fn classify_event_lane(kind: &str) -> ProjectionEventLane {
    if is_durable_conversation_kind(kind) {
        ProjectionEventLane::Timeline
    } else if is_ephemeral_activity_kind(kind) {
        ProjectionEventLane::Debug
    } else {
        ProjectionEventLane::State
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
            .map(|record| format!("{} [{:?}]", record.delivery_target, record.state))
            .unwrap_or_else(|| event.data.event_type.clone()),
        "waiting_intent_created" => decode_payload::<WaitingIntentRecord>(&event.data.payload)
            .map(|waiting| format!("waiting: {}", trim_summary(&waiting.summary)))
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
        "provider_round_completed" => event
            .data
            .payload
            .get("text_preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| text.to_owned())
            .unwrap_or_else(|| "provider round completed".into()),
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
    use super::{ProjectionEventLane, ProjectionSlice, TuiProjection};
    use crate::{
        client::{
            AgentStateSnapshot, AgentStreamEvent, StateSessionSnapshot, StateWorkspaceSnapshot,
            StreamEventEnvelope,
        },
        system::{
            ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind,
        },
        types::{
            AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
            AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentSummary,
            AgentTokenUsageSummary, AgentVisibility, BriefKind, BriefRecord, CallbackDeliveryMode,
            ChildAgentSummary, ClosureDecision, ClosureOutcome, ExternalTriggerStateSnapshot,
            ExternalTriggerStatus, LoadedAgentsMdView, MessageBody, MessageDeliverySurface,
            MessageEnvelope, MessageKind, MessageOrigin, Priority, RuntimePosture,
            SkillsRuntimeView, TaskRecord, TaskStatus, TimerRecord, TimerStatus, TokenUsage,
            TranscriptEntry, TranscriptEntryKind, TurnTerminalKind, TurnTerminalRecord,
            WaitingIntentRecord, WaitingIntentStatus, WaitingIntentSummary, WorkItemRecord,
            WorkItemState, WorkPlanItem, WorkPlanSnapshot, WorkPlanStepStatus,
            WorkspaceOccupancyRecord, WorktreeSession,
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
        assert_eq!(projection.briefs_tail.len(), snapshot.briefs_tail.len());
        assert_eq!(
            projection.transcript_tail.len(),
            snapshot.transcript_tail.len()
        );
        assert_eq!(
            projection.external_triggers.len(),
            snapshot.external_triggers.len()
        );
        assert!(projection.stale_slices.is_empty());
        assert_eq!(projection.cursor, snapshot.cursor);
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

        assert_eq!(
            projection.briefs_tail.last().map(|item| item.text.as_str()),
            Some("streamed brief")
        );
        assert_eq!(
            projection
                .transcript_tail
                .last()
                .and_then(|entry| entry.related_message_id.as_deref()),
            Some(message.id.as_str())
        );
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
    fn projection_marks_transcript_stale_for_round_events() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());

        projection.apply_event(
            sample_event(
                "provider_round_completed",
                json!({ "round": 1, "text_preview": "" }),
            ),
            &test_log_writer(),
        );

        assert!(projection
            .stale_slices
            .contains(&ProjectionSlice::TranscriptTail));
        assert_eq!(
            projection.event_log().last().map(|event| event.lane),
            Some(ProjectionEventLane::Debug)
        );
        assert_eq!(
            projection
                .event_log()
                .last()
                .map(|event| event.summary.as_str()),
            Some("provider round completed")
        );
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
                        "delivery_target": "operator",
                        "state": "open",
                        "created_at": Utc::now(),
                        "updated_at": Utc::now()
                    }
                }),
            ),
            &test_log_writer(),
        );

        for index in 0..300 {
            projection.apply_event(
                AgentStreamEvent {
                    id: format!("evt-debug-{index}"),
                    event: "provider_round_completed".into(),
                    data: StreamEventEnvelope {
                        id: format!("evt-debug-{index}"),
                        seq: index + 2,
                        ts: Utc::now(),
                        agent_id: "default".into(),
                        event_type: "provider_round_completed".into(),
                        payload: json!({ "text_preview": format!("partial-{index}") }),
                    },
                },
                &test_log_writer(),
            );
        }

        assert_eq!(
            projection
                .durable_conversation_events()
                .last()
                .map(|event| event.summary.as_str()),
            Some("durable card [Queued]")
        );
        assert!(projection
            .event_log()
            .iter()
            .all(|event| event.id != "evt-work_item_written"));
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
    fn projection_tracks_task_and_work_item_updates() {
        let mut projection = TuiProjection::from_snapshot(sample_snapshot());
        let mut task = sample_task();
        task.id = "task-stream".into();
        task.status = TaskStatus::Completed;
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
        AgentStreamEvent {
            id: format!("evt-{kind}"),
            event: kind.to_string(),
            data: StreamEventEnvelope {
                id: format!("evt-{kind}"),
                seq: 1,
                ts: Utc::now(),
                agent_id: "default".into(),
                event_type: kind.to_string(),
                payload,
            },
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
                    last_assistant_message: Some("done".into()),
                    completed_at: Utc::now(),
                    duration_ms: 12,
                }),
            },
            tasks: vec![sample_task()],
            transcript_tail: vec![TranscriptEntry {
                id: "tr-1".into(),
                agent_id: "default".into(),
                created_at: Utc::now(),
                kind: TranscriptEntryKind::IncomingMessage,
                round: None,
                related_message_id: Some("msg-1".into()),
                stop_reason: None,
                input_tokens: None,
                output_tokens: None,
                data: json!({ "body": { "type": "text", "text": "hi" } }),
            }],
            briefs_tail: vec![BriefRecord::new(
                "default",
                BriefKind::Ack,
                "queued",
                Some("msg-1".into()),
                None,
            )],
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
            work_items: vec![WorkItemRecord::new(
                "default",
                "active delivery",
                WorkItemState::Open,
            )],
            work_plan: Some(WorkPlanSnapshot {
                agent_id: "default".into(),
                work_item_id: "work-1".into(),
                created_at: Utc::now(),
                items: vec![WorkPlanItem {
                    step: "do it".into(),
                    status: WorkPlanStepStatus::InProgress,
                }],
            }),
            waiting_intents: vec![WaitingIntentRecord {
                id: "wait-1".into(),
                agent_id: "default".into(),
                work_item_id: None,
                summary: "wait".into(),
                source: "github".into(),
                resource: Some("pull_request:251".into()),
                condition: "review".into(),
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
                waiting_intent_id: "wait-1".into(),
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
            brief: None,
            cursor: Some("evt-bootstrap".into()),
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
            crate::types::TrustLevel::TrustedOperator,
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
            active_external_triggers: Vec::new(),
            recent_operator_notifications: Vec::new(),
            recent_brief_count: 1,
            recent_event_count: 1,
        }
    }
}
