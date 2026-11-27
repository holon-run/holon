//! Canonical conversation read model. Batches commit only at checkpoints.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, ensure, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    client::LocalClient,
    types::conversation::{
        ConversationActivity, ConversationChange, ConversationTurnSummary, ExecutionState,
        PendingInput,
    },
    types::{BriefRecord, ToolExecutionRecord},
};

const RETAINED_DETAIL_TURNS: usize = 16;
const ACTIVITY_LIMIT: usize = 300;
const TOOL_LIMIT: usize = 128;

#[derive(Clone, Debug)]
struct ToolRetry {
    revision: u64,
    attempt: u32,
    retry_at: tokio::time::Instant,
}

#[derive(Debug)]
pub(super) enum ConversationRequest {
    Older,
    Details {
        turn_id: String,
        expanded: bool,
    },
    TaskResult {
        task_id: String,
        message_id: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct ReadModelReset(&'static str);

#[derive(Clone, Debug, Deserialize)]
pub(super) struct SummaryPage {
    schema_version: u32,
    query_version: u32,
    runtime_id: String,
    event_log_epoch: String,
    visibility_scope_id: String,
    snapshot_through_seq: u64,
    snapshot_cursor: String,
    turns: Vec<ConversationTurnSummary>,
    active_turns: Vec<ConversationTurnSummary>,
    pending_inputs: Vec<PendingInput>,
    next_before_cursor: Option<String>,
    has_more: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct ActivityPage {
    schema_version: u32,
    query_version: u32,
    runtime_id: String,
    event_log_epoch: String,
    visibility_scope_id: String,
    turn: ConversationTurnSummary,
    detail_revision: u64,
    activities: Vec<ConversationActivity>,
    next_before_cursor: Option<String>,
    has_more: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ConversationModel {
    pub agent_id: String,
    runtime_id: String,
    epoch: String,
    scope: String,
    pub cursor: String,
    through_seq: u64,
    pub turns: BTreeMap<String, ConversationTurnSummary>,
    pub inputs: BTreeMap<String, PendingInput>,
    pub activities: BTreeMap<String, Vec<ConversationActivity>>,
    pub briefs: BTreeMap<String, BriefRecord>,
    pub brief_errors: BTreeMap<String, String>,
    pub tools: BTreeMap<String, ToolExecutionRecord>,
    pub tool_errors: BTreeMap<String, String>,
    pub detail_errors: BTreeMap<String, String>,
    pub truncated_details: BTreeSet<String>,
    pub expanded_turns: BTreeMap<String, bool>,
    pub task_results: BTreeMap<String, String>,
    pub task_result_errors: BTreeMap<String, String>,
    tool_revisions: BTreeMap<String, u64>,
    tool_retries: BTreeMap<String, ToolRetry>,
    detail_order: Vec<String>,
    removed_inputs: BTreeMap<String, u64>,
    detail_revisions: BTreeMap<String, u64>,
    dirty_details: std::collections::BTreeSet<String>,
    next_before: Option<String>,
    batch: Option<Batch>,
}

#[derive(Clone, Debug)]
struct Batch {
    id: String,
    through: u64,
    changes: Vec<ConversationChange>,
}

fn active(turn: &ConversationTurnSummary) -> bool {
    matches!(turn.execution, ExecutionState::Active)
}

fn query(path: String, key: &str, cursor: Option<&str>) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("limit", "30");
    if let Some(cursor) = cursor {
        serializer.append_pair(key, cursor);
    }
    format!("{path}?{}", serializer.finish())
}

impl ConversationModel {
    pub(super) fn state_refresh_head(&self) -> (&str, &str, &str, u64) {
        (&self.runtime_id, &self.epoch, &self.scope, self.through_seq)
    }

    fn snapshot(agent_id: String, mut page: SummaryPage) -> Result<Self> {
        ensure!(
            page.schema_version == 2 && page.query_version == 2,
            "unsupported conversation contract"
        );
        let mut model = Self {
            agent_id,
            runtime_id: page.runtime_id.clone(),
            epoch: page.event_log_epoch.clone(),
            scope: page.visibility_scope_id.clone(),
            cursor: page.snapshot_cursor.clone(),
            through_seq: page.snapshot_through_seq,
            ..Self::default()
        };
        for input in std::mem::take(&mut page.pending_inputs) {
            model.inputs.insert(input.message_id.clone(), input);
        }
        model.merge_page(page)?;
        Ok(model)
    }

    fn merge_page(&mut self, page: SummaryPage) -> Result<()> {
        ensure!(
            page.schema_version == 2
                && page.query_version == 2
                && page.runtime_id == self.runtime_id
                && page.event_log_epoch == self.epoch
                && page.visibility_scope_id == self.scope,
            ReadModelReset("conversation history binding changed")
        );
        ensure!(
            !page.has_more || page.next_before_cursor.is_some(),
            "conversation history missing cursor"
        );
        self.next_before = page.next_before_cursor;
        for turn in page.turns.into_iter().chain(page.active_turns) {
            self.upsert_turn(turn);
        }
        // History pages do not replace live pending membership.
        Ok(())
    }

    fn upsert_turn(&mut self, turn: ConversationTurnSummary) {
        if self
            .turns
            .get(&turn.turn_id)
            .is_some_and(|old| old.revision >= turn.revision)
        {
            return;
        }
        if self
            .turns
            .get(&turn.turn_id)
            .is_some_and(|old| !active(old))
            && active(&turn)
        {
            return;
        }
        if active(&turn) && !self.activities.contains_key(&turn.turn_id) {
            self.dirty_details.insert(turn.turn_id.clone());
        } else if !active(&turn)
            && !self.activities.contains_key(&turn.turn_id)
            && self.expanded_turns.get(&turn.turn_id) != Some(&true)
        {
            self.dirty_details.remove(&turn.turn_id);
        }
        self.turns.insert(turn.turn_id.clone(), turn);
    }

    pub fn details_expanded(&self, turn: &ConversationTurnSummary) -> bool {
        self.expanded_turns
            .get(&turn.turn_id)
            .copied()
            .unwrap_or_else(|| {
                active(turn)
                    || (self.activities.contains_key(&turn.turn_id) && !self.result_readable(turn))
            })
    }

    fn result_readable(&self, turn: &ConversationTurnSummary) -> bool {
        use holon_domain::conversation::ResultState;
        matches!(turn.result, ResultState::None { .. })
            || (!turn.brief_ids.is_empty()
                && turn.brief_ids.iter().all(|id| self.briefs.contains_key(id)))
    }

    fn wants_details(&self, turn_id: &str) -> bool {
        self.turns
            .get(turn_id)
            .is_some_and(|turn| active(turn) || self.details_expanded(turn))
    }

    pub fn set_details(&mut self, turn_id: String, expanded: bool) {
        if !self.turns.contains_key(&turn_id) {
            return;
        }
        self.expanded_turns.insert(turn_id.clone(), expanded);
        if expanded {
            for id in self
                .activities
                .get(&turn_id)
                .into_iter()
                .flatten()
                .filter_map(tool_id)
            {
                self.tool_retries.remove(id);
            }
            self.dirty_details.insert(turn_id.clone());
            self.touch_details(&turn_id);
        }
    }

    fn touch_details(&mut self, turn_id: &str) {
        self.detail_order.retain(|id| id != turn_id);
        self.detail_order.push(turn_id.to_owned());
        while self.detail_order.len() > RETAINED_DETAIL_TURNS {
            let Some(index) = self
                .detail_order
                .iter()
                .position(|id| self.turns.get(id).is_none_or(|turn| !active(turn)))
            else {
                break;
            };
            let id = self.detail_order.remove(index);
            self.activities.remove(&id);
            self.detail_revisions.remove(&id);
            self.dirty_details.remove(&id);
            self.detail_errors.remove(&id);
            self.truncated_details.remove(&id);
            self.expanded_turns.remove(&id);
        }
    }

    pub fn delta(&mut self, value: Value) -> Result<bool> {
        match value["type"].as_str() {
            Some("reset_required") => bail!("conversation reset required: {}", value["reason"]),
            Some("batch_begin") => {
                ensure!(self.batch.is_none(), "nested conversation batch");
                ensure!(
                    value["schema_version"] == 2
                        && value["query_version"] == 2
                        && value["runtime_id"] == self.runtime_id
                        && value["event_log_epoch"] == self.epoch
                        && value["visibility_scope_id"] == self.scope,
                    "conversation stream binding changed"
                );
                ensure!(
                    value["from_seq"].as_u64() == Some(self.through_seq),
                    "conversation delta gap"
                );
                let through = value["through_seq"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("missing through_seq"))?;
                ensure!(
                    through >= self.through_seq,
                    "conversation delta went backwards"
                );
                self.batch = Some(Batch {
                    id: value["batch_id"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing batch id"))?
                        .into(),
                    through,
                    changes: Vec::new(),
                });
                Ok(false)
            }
            Some("checkpoint") => {
                let batch = self
                    .batch
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("checkpoint outside batch"))?;
                ensure!(
                    value["batch_id"] == batch.id
                        && value["through_seq"].as_u64() == Some(batch.through)
                        && value["event_log_epoch"] == self.epoch
                        && value["visibility_scope_id"] == self.scope,
                    "conversation checkpoint mismatch"
                );
                let cursor = value["checkpoint"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing checkpoint cursor"))?
                    .to_owned();
                for change in batch.changes {
                    self.change(change);
                }
                self.cursor = cursor;
                self.through_seq = batch.through;
                Ok(true)
            }
            _ => {
                let change = serde_json::from_value(value)?;
                let batch = self
                    .batch
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("change outside batch"))?;
                ensure!(
                    batch.changes.len() < 4096,
                    "conversation batch exceeded bound"
                );
                batch.changes.push(change);
                Ok(false)
            }
        }
    }

    fn change(&mut self, change: ConversationChange) {
        match change {
            ConversationChange::TurnSummaryUpsert { turn } => self.upsert_turn(turn),
            ConversationChange::OperatorUpsert { input } => {
                if self
                    .removed_inputs
                    .get(&input.message_id)
                    .is_some_and(|revision| *revision >= input.revision)
                {
                    return;
                }
                if self
                    .inputs
                    .get(&input.message_id)
                    .is_none_or(|old| old.revision < input.revision)
                {
                    self.inputs.insert(input.message_id.clone(), input);
                }
            }
            ConversationChange::OperatorRemove {
                message_id,
                revision,
            } => {
                self.removed_inputs
                    .entry(message_id.clone())
                    .and_modify(|old| *old = (*old).max(revision))
                    .or_insert(revision);
                if self
                    .inputs
                    .get(&message_id)
                    .is_some_and(|old| old.revision <= revision)
                {
                    self.inputs.remove(&message_id);
                }
            }
            ConversationChange::DetailInvalidated {
                turn_id,
                detail_revision,
            } => {
                if self.wants_details(&turn_id)
                    && self
                        .detail_revisions
                        .get(&turn_id)
                        .is_none_or(|old| *old < detail_revision)
                {
                    self.dirty_details.insert(turn_id);
                }
            }
            ConversationChange::ActivityUpsert { turn_id, activity } => {
                if self.wants_details(&turn_id) {
                    self.touch_details(&turn_id);
                    let activities = self.activities.entry(turn_id.clone()).or_default();
                    let existing = activities
                        .iter()
                        .position(|old| activity_view(old).id == activity_view(&activity).id);
                    if let Some(index) = existing {
                        if activity_view(&activities[index]).revision
                            < activity_view(&activity).revision
                        {
                            activities[index] = activity;
                        }
                    } else {
                        activities.push(activity);
                    }
                    activities.sort_by(|a, b| activity_view(a).key.cmp(activity_view(b).key));
                    if activities.len() > ACTIVITY_LIMIT {
                        activities.drain(..activities.len() - ACTIVITY_LIMIT);
                        self.truncated_details.insert(turn_id);
                    }
                }
            }
        }
    }

    async fn hydrate(&mut self, client: &LocalClient) -> Result<()> {
        let dirty: Vec<_> = self.dirty_details.iter().cloned().collect();
        for turn_id in dirty {
            if !self.wants_details(&turn_id) {
                self.dirty_details.remove(&turn_id);
                continue;
            }
            let result: Result<()> = async {
                let mut before = None;
                let mut activities = Vec::new();
                let mut revision = None;
                loop {
                    let page: ActivityPage = client
                        .get_json(&query(
                            format!("/agents/{}/turns/{turn_id}/activities", self.agent_id),
                            "before",
                            before.as_deref(),
                        ))
                        .await?;
                    ensure!(
                        page.schema_version == 2
                            && page.query_version == 2
                            && page.turn.turn_id == turn_id
                            && page.runtime_id == self.runtime_id
                            && page.event_log_epoch == self.epoch
                            && page.visibility_scope_id == self.scope,
                        ReadModelReset("invalid activity binding")
                    );
                    self.upsert_turn(page.turn);
                    ensure!(
                        revision.is_none_or(|revision| revision == page.detail_revision),
                        ReadModelReset("activity revision changed during pagination")
                    );
                    revision = Some(page.detail_revision);
                    activities.extend(page.activities);
                    if !page.has_more || activities.len() >= ACTIVITY_LIMIT {
                        if page.has_more {
                            self.truncated_details.insert(turn_id.clone());
                        }
                        activities.sort_by(|a, b| activity_view(a).key.cmp(activity_view(b).key));
                        activities.truncate(ACTIVITY_LIMIT);
                        self.touch_details(&turn_id);
                        self.activities.insert(turn_id.clone(), activities);
                        self.detail_revisions
                            .insert(turn_id.clone(), page.detail_revision);
                        break;
                    }
                    ensure!(
                        page.next_before_cursor.is_some() && page.next_before_cursor != before,
                        "activity pagination did not advance"
                    );
                    before = page.next_before_cursor;
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    self.dirty_details.remove(&turn_id);
                    self.detail_errors.remove(&turn_id);
                }
                Err(error) if error.is::<ReadModelReset>() => return Err(error),
                Err(error) => {
                    self.detail_errors.insert(turn_id, error.to_string());
                }
            }
        }
        self.hydrate_tools(client).await;
        let ids: Vec<_> = self
            .turns
            .values()
            .flat_map(|turn| turn.brief_ids.iter())
            .filter(|id| !self.briefs.contains_key(*id))
            .cloned()
            .collect();
        for id in ids {
            match client.agent_brief(&self.agent_id, &id).await {
                Ok(brief) if brief.id == id && brief.agent_id == self.agent_id => {
                    self.brief_errors.remove(&id);
                    self.briefs.insert(id, brief);
                }
                Ok(_) => {
                    self.brief_errors
                        .insert(id, "Canonical brief binding mismatch".into());
                }
                Err(error) => {
                    self.brief_errors.insert(id, error.to_string());
                }
            }
        }
        Ok(())
    }

    fn tool_candidates(&self) -> Vec<(String, String, holon_domain::conversation::ActivityItem)> {
        let mut requested: Vec<_> = self
            .activities
            .iter()
            .flat_map(|(turn_id, items)| {
                items.iter().filter_map(move |activity| {
                    Some((
                        turn_id.clone(),
                        tool_id(activity)?.to_owned(),
                        activity_item(activity),
                    ))
                })
            })
            .collect();
        requested.sort_by_key(|(turn_id, _, item)| {
            (
                self.wants_details(turn_id),
                self.turns.get(turn_id).is_some_and(active),
                self.detail_order.iter().position(|id| id == turn_id),
                item.key.clone(),
            )
        });
        requested.reverse();
        requested.truncate(TOOL_LIMIT);
        requested
    }

    fn tool_failed(&mut self, id: String, revision: u64, error: String) {
        let attempt = self
            .tool_retries
            .get(&id)
            .filter(|retry| retry.revision == revision)
            .map_or(0, |retry| retry.attempt.saturating_add(1));
        self.tool_retries.insert(
            id.clone(),
            ToolRetry {
                revision,
                attempt,
                retry_at: tokio::time::Instant::now()
                    + super::runtime::reconnect_delay_for_attempt(attempt),
            },
        );
        self.tool_errors.insert(id, error);
    }

    async fn hydrate_tools(&mut self, client: &LocalClient) {
        let requested = self.tool_candidates();
        let ids: BTreeSet<_> = requested.iter().map(|(_, id, _)| id.clone()).collect();
        let visible_ids: BTreeSet<_> = requested
            .iter()
            .filter(|(turn_id, _, _)| self.wants_details(turn_id))
            .map(|(_, id, _)| id.clone())
            .collect();
        self.tools.retain(|id, _| ids.contains(id));
        self.tool_revisions.retain(|id, _| ids.contains(id));
        self.tool_errors.retain(|id, _| visible_ids.contains(id));
        self.tool_retries.retain(|id, _| visible_ids.contains(id));
        for (turn_id, id, item) in requested {
            if !self.wants_details(&turn_id)
                || self.tool_revisions.get(&id) == Some(&item.revision)
                || self.tool_retries.get(&id).is_some_and(|retry| {
                    retry.revision == item.revision && tokio::time::Instant::now() < retry.retry_at
                })
            {
                continue;
            }
            match client.tool_execution(&self.agent_id, &id).await {
                Ok(record)
                    if record.id == id
                        && record.agent_id == self.agent_id
                        && record.turn_id.as_ref().is_none_or(|id| id == &turn_id) =>
                {
                    self.tools.insert(id.clone(), record);
                    self.tool_revisions.insert(id.clone(), item.revision);
                    self.tool_errors.remove(&id);
                    self.tool_retries.remove(&id);
                }
                Ok(_) => {
                    self.tool_failed(id, item.revision, "Tool record binding mismatch".into());
                }
                Err(error) => {
                    self.tool_failed(id, item.revision, error.to_string());
                }
            }
        }
    }

    async fn load_task_result(
        &mut self,
        client: &LocalClient,
        task_id: String,
        message_id: Option<String>,
    ) {
        if self.task_results.contains_key(&task_id) {
            return;
        }
        let selected_task_id = task_id.clone();
        let result: Result<String> = async {
            if let Some(id) = message_id {
                let message = client.agent_message(&self.agent_id, &id).await?;
                ensure!(
                    message.id == id && message.agent_id == self.agent_id,
                    "task reply binding mismatch"
                );
                Ok(match message.body {
                    crate::types::MessageBody::Text { text }
                    | crate::types::MessageBody::Brief { text, .. } => text,
                    crate::types::MessageBody::Json { value } => {
                        super::tool_details::structured_result_text(&value)
                    }
                })
            } else {
                let result = client
                    .task_output(&self.agent_id, &task_id, false, None)
                    .await?;
                ensure!(
                    result.task.task_id == task_id,
                    "task result binding mismatch"
                );
                let task = result.task;
                Ok(format!(
                    "Task {} · {:?}\n{}\n{}{}\nArtifacts: {}",
                    task.task_id,
                    task.status,
                    task.result_summary.unwrap_or_default(),
                    task.output_preview,
                    if task.output_truncated {
                        "\n[Preview truncated; inspect task artifacts for full output]"
                    } else {
                        ""
                    },
                    serde_json::to_string_pretty(&task.artifacts)?
                ))
            }
        }
        .await;
        match result {
            Ok(text) => {
                self.task_result_errors.remove(&task_id);
                let mut text: String = text
                    .chars()
                    .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
                    .collect();
                let mut end = text.len().min(65_536);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let truncated = end < text.len();
                text.truncate(end);
                if truncated {
                    text.push_str("\n[Display limit; inspect original task result]");
                }
                self.task_results.insert(task_id, text);
            }
            Err(error) => {
                self.task_result_errors.insert(task_id, error.to_string());
            }
        }
        while self.task_results.len() + self.task_result_errors.len() > RETAINED_DETAIL_TURNS {
            if let Some(id) = self
                .task_results
                .keys()
                .find(|id| *id != &selected_task_id)
                .cloned()
            {
                self.task_results.remove(&id);
            } else if let Some(id) = self
                .task_result_errors
                .keys()
                .find(|id| *id != &selected_task_id)
                .cloned()
            {
                self.task_result_errors.remove(&id);
            } else {
                break;
            }
        }
    }

    pub async fn load(client: &LocalClient, agent_id: &str) -> Result<Self> {
        let page = client
            .get_json(&query(
                format!("/agents/{agent_id}/conversation"),
                "before",
                None,
            ))
            .await?;
        Self::snapshot(agent_id.into(), page)
    }

    pub async fn older(&mut self, client: &LocalClient) -> Result<()> {
        let Some(before) = self.next_before.clone() else {
            return Ok(());
        };
        let page = client
            .get_json(&query(
                format!("/agents/{}/conversation", self.agent_id),
                "before",
                Some(&before),
            ))
            .await?;
        self.merge_page(page)?;
        ensure!(
            self.next_before.as_ref() != Some(&before),
            "history pagination did not advance"
        );
        self.hydrate(client).await
    }

    pub fn has_older(&self) -> bool {
        self.next_before.is_some()
    }
}

pub(super) fn activity_item(
    activity: &ConversationActivity,
) -> holon_domain::conversation::ActivityItem {
    match activity {
        ConversationActivity::Operator(item)
        | ConversationActivity::Assistant(item)
        | ConversationActivity::Tool(item)
        | ConversationActivity::Wait(item)
        | ConversationActivity::Error(item) => item.clone(),
        ConversationActivity::ModelRequest(item) => holon_domain::conversation::ActivityItem {
            id: item.id.clone(),
            key: item.key.clone(),
            revision: item.revision,
            summary: item.summary.clone(),
        },
        ConversationActivity::ModelSwitch(item) => holon_domain::conversation::ActivityItem {
            id: item.id.clone(),
            key: item.key.clone(),
            revision: item.revision,
            summary: item.summary.clone(),
        },
    }
}

/// Borrowed view over the fields every activity kind exposes.
///
/// The model activity kinds carry their own payload types, so no single
/// borrowed `ActivityItem` exists for them. Borrowing the shared fields keeps
/// the render path free of the per-activity clone that a materialized item
/// would cost.
pub(super) struct ActivityView<'a> {
    pub id: &'a str,
    pub key: &'a holon_domain::ActivityKey,
    pub revision: u64,
    pub summary: &'a str,
}

pub(super) fn activity_view(activity: &ConversationActivity) -> ActivityView<'_> {
    match activity {
        ConversationActivity::Operator(item)
        | ConversationActivity::Assistant(item)
        | ConversationActivity::Tool(item)
        | ConversationActivity::Wait(item)
        | ConversationActivity::Error(item) => ActivityView {
            id: &item.id,
            key: &item.key,
            revision: item.revision,
            summary: &item.summary,
        },
        ConversationActivity::ModelRequest(item) => ActivityView {
            id: &item.id,
            key: &item.key,
            revision: item.revision,
            summary: &item.summary,
        },
        ConversationActivity::ModelSwitch(item) => ActivityView {
            id: &item.id,
            key: &item.key,
            revision: item.revision,
            summary: &item.summary,
        },
    }
}

pub(super) fn tool_id(activity: &ConversationActivity) -> Option<&str> {
    if let ConversationActivity::Tool(item) = activity {
        item.id.strip_prefix("tool:")
    } else {
        None
    }
}

pub(super) async fn observe(
    client: LocalClient,
    agent_id: String,
    generation: u64,
    tx: tokio::sync::mpsc::UnboundedSender<super::runtime::TuiRuntimeMessage>,
    mut history: tokio::sync::mpsc::Receiver<ConversationRequest>,
) {
    let mut model: Option<ConversationModel> = None;
    let mut attempt = 0;
    loop {
        let result: Result<bool> = async {
            if model.is_none() { model = Some(ConversationModel::load(&client, &agent_id).await?); }
            let current = model.as_mut().expect("snapshot loaded");
            tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded { generation, agent_id: agent_id.clone(), model: current.clone() })?;
            current.hydrate(&client).await?;
            tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded { generation, agent_id: agent_id.clone(), model: current.clone() })?;
            let path = query(format!("/agents/{agent_id}/conversation/stream"), "after", Some(&current.cursor));
            let mut stream = match client.stream_read_path(&path).await {
                Ok(stream) => stream,
                Err(error) => {
                    if error.downcast_ref::<crate::client::LocalHttpError>()
                        .is_some_and(|error| error.has_code("conversation_reset_required")) {
                        model = None;
                    }
                    return Err(error);
                }
            };
            tx.send(super::runtime::TuiRuntimeMessage::ConversationConnected {
                generation, agent_id: agent_id.clone()
            })?;
            let mut brief_retry_attempt = 0;
            let mut brief_retry_deadline = tokio::time::Instant::now()
                + super::runtime::reconnect_delay_for_attempt(brief_retry_attempt);
            loop {
                tokio::select! {
                    event = stream.next_json() => {
                        let value = event?;
                        if current.delta(value).is_err() { model = None; bail!("conversation stream requires fresh snapshot"); }
                        if current.batch.is_none() {
                            // Execution stops before slow result/detail hydration finishes.
                            tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded { generation, agent_id: agent_id.clone(), model: current.clone() })?;
                            current.hydrate(&client).await?;
                            tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded { generation, agent_id: agent_id.clone(), model: current.clone() })?;
                            attempt = 0;
                        }
                    }
                    _ = tokio::time::sleep_until(brief_retry_deadline),
                        if current.batch.is_none() && (!current.brief_errors.is_empty() || !current.tool_errors.is_empty() || !current.detail_errors.is_empty()) => {
                        current.hydrate(&client).await?;
                        tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded {
                            generation, agent_id: agent_id.clone(), model: current.clone()
                        })?;
                        brief_retry_attempt = brief_retry_attempt.saturating_add(1);
                        brief_retry_deadline = tokio::time::Instant::now()
                            + super::runtime::reconnect_delay_for_attempt(brief_retry_attempt);
                    }
                    request = history.recv(), if current.batch.is_none() => {
                        let Some(request) = request else { return Ok(false); };
                        // Read history only between batches; discard any incomplete batch on reconnect.
                        current.batch = None;
                        match request {
                            ConversationRequest::Older => current.older(&client).await?,
                            ConversationRequest::Details { turn_id, expanded } => {
                                current.set_details(turn_id.clone(), expanded);
                                tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded { generation, agent_id: agent_id.clone(), model: current.clone() })?;
                                current.hydrate(&client).await?;
                                if expanded {
                                    let results: Vec<_> = current.turns.get(&turn_id).into_iter()
                                        .flat_map(|turn| turn.inputs.iter()).filter_map(|input| input.task_result.clone()).collect();
                                    for result in results {
                                        current.load_task_result(&client, result.task_id, result.response_message_id).await;
                                    }
                                }
                            }
                            ConversationRequest::TaskResult { task_id, message_id } => {
                                current.load_task_result(&client, task_id, message_id).await;
                            }
                        }
                        tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded { generation, agent_id: agent_id.clone(), model: current.clone() })?;
                        return Ok(true);
                    }
                }
            }
        }.await;
        if tx.is_closed() {
            return;
        }
        let error = match result {
            Ok(true) => {
                attempt = 0;
                continue;
            }
            Ok(false) => return,
            Err(error) => error,
        };
        if error.is::<ReadModelReset>()
            || error
                .downcast_ref::<crate::client::LocalHttpError>()
                .is_some_and(|error| error.has_code("conversation_reset_required"))
        {
            model = None;
        }
        let _ = tx.send(super::runtime::TuiRuntimeMessage::ConversationStatus {
            generation,
            error: error.to_string(),
            reset: model.is_none(),
        });
        if let Some(current) = model.as_mut() {
            current.batch = None;
        }
        attempt += 1;
        tokio::time::sleep(super::runtime::reconnect_delay_for_attempt(attempt)).await;
    }
}

#[cfg(test)]
mod tests;
