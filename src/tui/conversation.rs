//! Canonical conversation read model. Batches commit only at checkpoints.
use std::collections::BTreeMap;

use anyhow::{bail, ensure, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    client::LocalClient,
    domain::conversation::{
        ConversationActivity, ConversationChange, ConversationTurnSummary, ExecutionState,
        PendingInput,
    },
    types::BriefRecord,
};

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
        if !active(&turn) {
            self.activities.remove(&turn.turn_id);
            self.detail_revisions.remove(&turn.turn_id);
            self.dirty_details.remove(&turn.turn_id);
        } else if !self.activities.contains_key(&turn.turn_id) {
            self.dirty_details.insert(turn.turn_id.clone());
        }
        self.turns.insert(turn.turn_id.clone(), turn);
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
                if self.turns.get(&turn_id).is_some_and(active)
                    && self
                        .detail_revisions
                        .get(&turn_id)
                        .is_none_or(|old| *old < detail_revision)
                {
                    self.dirty_details.insert(turn_id);
                }
            }
            ConversationChange::ActivityUpsert { turn_id, activity } => {
                if self.turns.get(&turn_id).is_some_and(active) {
                    let activities = self.activities.entry(turn_id).or_default();
                    let item = activity_item(&activity);
                    if let Some(old) = activities
                        .iter_mut()
                        .find(|old| activity_item(old).id == item.id)
                    {
                        if activity_item(old).revision < item.revision {
                            *old = activity;
                        }
                    } else {
                        activities.push(activity);
                    }
                    activities.sort_by(|a, b| activity_item(a).key.cmp(&activity_item(b).key));
                }
            }
        }
    }

    async fn hydrate(&mut self, client: &LocalClient) -> Result<()> {
        let dirty: Vec<_> = self.dirty_details.iter().cloned().collect();
        for turn_id in dirty {
            if !self.turns.get(&turn_id).is_some_and(active) {
                continue;
            }
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
                if !active(&page.turn) {
                    self.upsert_turn(page.turn);
                    break;
                }
                ensure!(
                    revision.is_none_or(|revision| revision == page.detail_revision),
                    ReadModelReset("activity revision changed during pagination")
                );
                revision = Some(page.detail_revision);
                activities.extend(page.activities);
                if !page.has_more {
                    activities.sort_by(|a, b| activity_item(a).key.cmp(&activity_item(b).key));
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
            self.dirty_details.remove(&turn_id);
        }
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
) -> &crate::domain::conversation::ActivityItem {
    match activity {
        ConversationActivity::Operator(item)
        | ConversationActivity::Assistant(item)
        | ConversationActivity::Tool(item)
        | ConversationActivity::Wait(item)
        | ConversationActivity::Error(item) => item,
    }
}

pub(super) async fn observe(
    client: LocalClient,
    agent_id: String,
    generation: u64,
    tx: tokio::sync::mpsc::UnboundedSender<super::runtime::TuiRuntimeMessage>,
    mut history: tokio::sync::mpsc::Receiver<()>,
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
            let mut brief_retry_attempt = 0;
            let mut brief_retry_deadline = tokio::time::Instant::now()
                + super::runtime::reconnect_delay_for_attempt(brief_retry_attempt);
            loop {
                tokio::select! {
                    event = stream.next_json() => {
                        let value = event?;
                        if current.delta(value).is_err() { model = None; bail!("conversation stream requires fresh snapshot"); }
                        if current.batch.is_none() {
                            current.hydrate(&client).await?;
                            tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded { generation, agent_id: agent_id.clone(), model: current.clone() })?;
                            attempt = 0;
                        }
                    }
                    _ = tokio::time::sleep_until(brief_retry_deadline),
                        if current.batch.is_none() && !current.brief_errors.is_empty() => {
                        current.hydrate(&client).await?;
                        tx.send(super::runtime::TuiRuntimeMessage::ConversationLoaded {
                            generation, agent_id: agent_id.clone(), model: current.clone()
                        })?;
                        brief_retry_attempt = brief_retry_attempt.saturating_add(1);
                        brief_retry_deadline = tokio::time::Instant::now()
                            + super::runtime::reconnect_delay_for_attempt(brief_retry_attempt);
                    }
                    request = history.recv() => {
                        if request.is_none() { return Ok(false); }
                        // Read history only between batches; discard any incomplete batch on reconnect.
                        current.batch = None;
                        current.older(&client).await?;
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
mod tests {
    use super::super::{
        app::TuiApp,
        chat::{collect_chat_items, ConversationCell},
        runtime::TuiRuntimeMessage,
    };
    use super::*;
    use serde_json::json;

    fn turn(
        id: &str,
        index: u64,
        revision: u64,
        execution: Value,
        briefs: Vec<&str>,
    ) -> ConversationTurnSummary {
        serde_json::from_value(json!({
            "turn_id": id, "key": {"turn_index": index, "turn_id": id}, "revision": revision,
            "presentation_class": "operator", "inputs": [{"message_id": format!("input-{id}"), "preview": "operator prompt"}],
            "execution": execution, "started_at": "2026-01-01T00:00:00Z", "completed_at": null,
            "duration_ms": null, "result": {"kind": if briefs.is_empty() { "pending" } else { "available" }},
            "settled": false, "attention": null, "detail_coverage": {"kind": "complete"}, "brief_ids": briefs
        })).unwrap()
    }

    fn page(
        turns: Vec<ConversationTurnSummary>,
        active_turns: Vec<ConversationTurnSummary>,
        before: Option<&str>,
    ) -> SummaryPage {
        serde_json::from_value(json!({
            "schema_version": 2, "query_version": 2, "runtime_id": "runtime", "event_log_epoch": "epoch",
            "visibility_scope_id": "public", "snapshot_through_seq": 10, "snapshot_cursor": "snapshot-10",
            "turns": turns, "active_turns": active_turns, "pending_inputs": [],
            "next_before_cursor": before, "has_more": before.is_some()
        })).unwrap()
    }

    fn model() -> ConversationModel {
        ConversationModel::snapshot(
            "default".into(),
            page(
                vec![],
                vec![turn("active", 2, 1, json!({"kind": "active"}), vec![])],
                None,
            ),
        )
        .unwrap()
    }

    fn begin(from: u64, through: u64) -> Value {
        json!({"type": "batch_begin", "batch_id": "batch", "schema_version": 2, "query_version": 2,
            "runtime_id": "runtime", "event_log_epoch": "epoch", "visibility_scope_id": "public", "from_seq": from, "through_seq": through})
    }

    fn checkpoint(through: u64) -> Value {
        json!({"type": "checkpoint", "batch_id": "batch", "event_log_epoch": "epoch", "visibility_scope_id": "public", "through_seq": through, "checkpoint": format!("checkpoint-{through}")})
    }

    fn activity() -> ConversationActivity {
        serde_json::from_value(json!({"kind": "tool", "id": "tool-1", "key": {"event_seq": 11, "activity_id": "tool-1"}, "revision": 1, "summary": "tool detail"})).unwrap()
    }

    fn app(model: ConversationModel) -> TuiApp {
        let client = LocalClient::new(super::super::tests::test_config()).unwrap();
        let mut app = TuiApp::new(
            client,
            super::super::logging::TuiLogWriter::new_temp().unwrap(),
        );
        app.agents = vec![super::super::tests::sample_agent_summary("default")];
        app.conversation = Some(model);
        app
    }

    fn bodies(app: &TuiApp) -> Vec<String> {
        collect_chat_items(app)
            .iter()
            .map(|cell| match cell {
                ConversationCell::UserMessage { body, .. }
                | ConversationCell::ActiveActivity { body, .. }
                | ConversationCell::SystemNotice { body, .. } => body.clone(),
            })
            .collect()
    }

    #[test]
    fn summary_pages_merge_by_revision_without_replacing_live_membership_or_cursor() {
        let mut model = model();
        model.change(ConversationChange::OperatorRemove {
            message_id: "removed".into(),
            revision: 2,
        });
        let mut older = page(
            vec![turn(
                "old",
                1,
                1,
                json!({"kind": "terminal", "outcome": "completed"}),
                vec![],
            )],
            vec![turn("active", 2, 0, json!({"kind": "active"}), vec![])],
            Some("older"),
        );
        older.pending_inputs = vec![serde_json::from_value(json!({"message_id": "removed", "revision": 1, "state": "queued", "preview": "stale", "presentation_class": "operator", "created_at": "2026-01-01T00:00:00Z"})).unwrap()];
        model.merge_page(older).unwrap();
        assert_eq!(model.turns.len(), 2);
        assert_eq!(model.turns["active"].revision, 1);
        assert_eq!(model.cursor, "snapshot-10");
        assert!(model.has_older());
        assert!(model.inputs.is_empty());
    }

    #[test]
    fn deltas_are_atomic_and_gap_reset_and_mismatched_checkpoint_are_rejected() {
        let mut model = model();
        assert!(model.delta(begin(9, 11)).is_err());
        assert!(!model.delta(begin(10, 11)).unwrap());
        model
            .delta(json!({"type": "activity_upsert", "turn_id": "active", "activity": activity()}))
            .unwrap();
        assert!(model.activities.is_empty());
        assert_eq!(model.cursor, "snapshot-10");
        assert!(model.delta(checkpoint(12)).is_err());
        assert!(model.activities.is_empty());
        assert!(model
            .delta(json!({"type": "reset_required", "reason": "event_log_epoch_mismatch"}))
            .is_err());
    }

    #[test]
    fn schema_versions_and_detail_revisions_are_checked_without_reviving_terminal_details() {
        let mut unsupported = page(vec![], vec![], None);
        unsupported.query_version = 1;
        assert!(ConversationModel::snapshot("default".into(), unsupported).is_err());
        let mut model = model();
        model.dirty_details.clear();
        model.detail_revisions.insert("active".into(), 2);
        model.change(ConversationChange::DetailInvalidated {
            turn_id: "active".into(),
            detail_revision: 2,
        });
        assert!(model.dirty_details.is_empty());
        model.change(ConversationChange::DetailInvalidated {
            turn_id: "active".into(),
            detail_revision: 3,
        });
        assert!(model.dirty_details.contains("active"));
        model.upsert_turn(turn(
            "active",
            2,
            2,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec![],
        ));
        model.change(ConversationChange::DetailInvalidated {
            turn_id: "active".into(),
            detail_revision: 4,
        });
        assert!(model.dirty_details.is_empty());
    }

    #[test]
    fn interim_brief_does_not_terminate_execution_or_hide_details() {
        let mut model = model();
        model.activities.insert("active".into(), vec![activity()]);
        model.delta(begin(10, 12)).unwrap();
        model.delta(json!({"type": "turn_summary_upsert", "turn": turn("active", 2, 2, json!({"kind": "active"}), vec!["brief"])})).unwrap();
        model.delta(checkpoint(12)).unwrap();
        assert!(active(&model.turns["active"]));
        assert_eq!(model.activities["active"].len(), 1);
        let bodies = bodies(&app(model));
        assert!(bodies.iter().any(|body| body == "Working…"));
        assert!(bodies.iter().any(|body| body == "tool detail"));
        assert!(bodies
            .iter()
            .any(|body| body == "Loading canonical result…"));
    }

    #[test]
    fn terminal_clears_details_without_fabricating_a_brief_for_failed_or_cancelled_turns() {
        for outcome in [
            "completed",
            "aborted",
            "interrupted",
            "provider_failed_needs_recovery",
        ] {
            let mut model = model();
            model.activities.insert("active".into(), vec![activity()]);
            model.delta(begin(10, 12)).unwrap();
            model.delta(json!({"type": "turn_summary_upsert", "turn": turn("active", 2, 2, json!({"kind": "terminal", "outcome": outcome}), vec![])})).unwrap();
            model.delta(checkpoint(12)).unwrap();
            assert!(model.activities.is_empty());
            assert!(model.briefs.is_empty());
            model.change(ConversationChange::ActivityUpsert {
                turn_id: "active".into(),
                activity: activity(),
            });
            assert!(model.activities.is_empty());
            model.upsert_turn(turn("active", 2, 3, json!({"kind": "active"}), vec![]));
            assert!(!active(&model.turns["active"]));
            let bodies = bodies(&app(model));
            assert!(!bodies
                .iter()
                .any(|body| body == "Working…" || body == "tool detail"));
            assert_eq!(
                bodies
                    .iter()
                    .filter(|body| body.starts_with("Turn ended:"))
                    .count(),
                usize::from(outcome != "completed")
            );
        }
    }

    #[test]
    fn terminal_keeps_only_canonical_brief_and_inputs() {
        let mut model = model();
        model.activities.insert("active".into(), vec![activity()]);
        let brief: BriefRecord = serde_json::from_value(json!({"id": "brief", "agent_id": "default", "kind": "result", "created_at": "2026-01-01T00:00:01Z", "text": "canonical result", "attachments": null, "related_message_id": null, "related_task_id": null})).unwrap();
        model.briefs.insert("brief".into(), brief);
        model.upsert_turn(turn(
            "active",
            2,
            2,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec!["brief"],
        ));
        let app = app(model);
        assert_eq!(bodies(&app), ["operator prompt", "canonical result"]);
        assert!(collect_chat_items(&app)
            .iter()
            .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
        let rendered = super::super::chat::chat_text_for_width(&app, 100)
            .lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("canonical result"));
        assert!(rendered.contains("Holon"));
        assert!(!rendered.contains("Working"));
    }

    #[test]
    fn canonical_inputs_decode_message_bodies_and_bounded_text_previews() {
        for (preview, expected) in [
            (
                r#"{"type":"text","text":"readable\nprompt"}"#,
                "readable\nprompt",
            ),
            (r#"{"type":"text","text":"truncated\u4"#, "truncated…"),
            (
                r#"{"type":"text","text":"中文\"quoted\""#,
                "中文\"quoted\"…",
            ),
            (
                r#"{"type":"json","value":{"key":"value"}}"#,
                r#"{"key":"value"}"#,
            ),
            ("plain prompt", "plain prompt"),
            (r#"{"unknown":"shape"#, r#"{"unknown":"shape"#),
        ] {
            let mut model = model();
            model.turns.get_mut("active").unwrap().inputs[0].preview = preview.into();
            model.inputs.insert(
                "pending".into(),
                serde_json::from_value(json!({
                    "message_id": "pending", "revision": 1, "state": "queued",
                    "preview": preview, "presentation_class": "operator",
                    "created_at": "2026-01-01T00:00:01Z"
                }))
                .unwrap(),
            );
            let cells = collect_chat_items(&app(model));
            let input_bodies: Vec<_> = cells
                .iter()
                .filter_map(|cell| {
                    if let ConversationCell::UserMessage { body, .. } = cell {
                        Some(body.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(input_bodies, [expected, expected]);
        }
    }

    #[test]
    fn terminal_notices_and_nonoperator_inputs_are_static() {
        let mut model = model();
        let mut terminal = turn(
            "active",
            2,
            2,
            json!({"kind": "terminal", "outcome": "aborted"}),
            vec![],
        );
        terminal.presentation_class = crate::domain::conversation::PresentationClass::Internal;
        model.upsert_turn(terminal);
        let app = app(model);
        assert!(collect_chat_items(&app)
            .iter()
            .all(|cell| matches!(cell, ConversationCell::SystemNotice { .. })));
        let rendered = super::super::chat::chat_text_for_width(&app, 100)
            .lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("internal"));
        assert!(rendered.contains("Turn ended: Aborted"));
        assert!(!rendered.contains("Working"));
    }

    #[test]
    fn completed_turn_with_pending_delivery_keeps_static_result_status() {
        let mut model = model();
        model.upsert_turn(turn(
            "active",
            2,
            2,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec![],
        ));
        let app = app(model);
        assert_eq!(
            bodies(&app),
            ["operator prompt", "Awaiting canonical result"]
        );
        assert!(collect_chat_items(&app)
            .iter()
            .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
    }

    #[test]
    fn terminal_turn_without_result_preserves_typed_reason_as_static_notice() {
        for (reason, notice) in [
            (
                json!({"kind": "reducer_only", "reason": "handled by reducer"}),
                "No canonical result: reducer-only (handled by reducer)",
            ),
            (
                json!({"kind": "aborted"}),
                "No canonical result: turn aborted",
            ),
            (
                json!({"kind": "interrupted"}),
                "No canonical result: turn interrupted",
            ),
            (
                json!({"kind": "tool_only_wait"}),
                "No canonical result: tool-only wait",
            ),
        ] {
            let mut model = model();
            model.activities.insert("active".into(), vec![activity()]);
            let mut terminal = turn(
                "active",
                2,
                2,
                json!({"kind": "terminal", "outcome": "completed"}),
                vec![],
            );
            terminal.result =
                serde_json::from_value(json!({"kind": "none", "reason": reason})).unwrap();
            model.upsert_turn(terminal);
            assert!(model.activities.is_empty());
            assert!(model.briefs.is_empty());
            let app = app(model);
            assert_eq!(bodies(&app), ["operator prompt", notice]);
            assert!(collect_chat_items(&app)
                .iter()
                .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
        }
    }

    #[test]
    fn completed_available_result_only_shows_brief_fetch_status_until_hydrated() {
        let mut model = model();
        model.upsert_turn(turn(
            "active",
            2,
            2,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec!["brief"],
        ));
        assert_eq!(
            bodies(&app(model.clone())),
            ["operator prompt", "Loading canonical result…"]
        );
        model
            .brief_errors
            .insert("brief".into(), "temporary failure".into());
        assert_eq!(
            bodies(&app(model.clone())),
            ["operator prompt", "Canonical result unavailable; retrying"]
        );
        model.briefs.insert(
            "brief".into(),
            serde_json::from_value(json!({
                "id": "brief", "agent_id": "default", "kind": "result",
                "created_at": "2026-01-01T00:00:01Z", "text": "canonical result",
                "attachments": null, "related_message_id": null, "related_task_id": null
            }))
            .unwrap(),
        );
        model.brief_errors.remove("brief");
        let app = app(model);
        assert_eq!(bodies(&app), ["operator prompt", "canonical result"]);
        assert!(collect_chat_items(&app)
            .iter()
            .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
    }

    #[test]
    fn pending_inputs_follow_queue_time_and_preserve_assignment_state() {
        let mut model = model();
        for (id, timestamp, state) in [
            ("z-first", "2026-01-01T00:00:01Z", "queued"),
            ("a-later", "2026-01-01T00:00:02Z", "assigning"),
        ] {
            model.inputs.insert(
                id.into(),
                serde_json::from_value(json!({
                    "message_id": id, "revision": 1, "state": state, "preview": id,
                    "presentation_class": "operator", "created_at": timestamp,
                }))
                .unwrap(),
            );
        }
        let cells = collect_chat_items(&app(model));
        let pending: Vec<_> = cells
            .iter()
            .filter_map(|cell| {
                if let ConversationCell::UserMessage {
                    body,
                    status: Some(status),
                    ..
                } = cell
                {
                    Some((body.as_str(), status))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            pending,
            [
                ("z-first", &crate::types::OperatorMessageStatus::Queued),
                ("a-later", &crate::types::OperatorMessageStatus::Processing),
            ]
        );
    }

    #[test]
    fn assistant_activity_only_exposes_visible_text_from_legacy_envelopes() {
        for (summary, expected) in [
            ("plain assistant text", Some("plain assistant text")),
            (
                r#"{"blocks":[{"type":"text","text":"visible"},{"type":"thinking","text":"PRIVATE"},{"type":"text","text":"second"}],"signature":"PRIVATE"}"#,
                Some("visible\n\nsecond"),
            ),
            (r#"{"blocks":[{"type":"thinking","text":"PRIVATE"}]}"#, None),
            (r#"{"blocks":[{"type":"text","text":"cut"#, None),
            (r#"{"thinking":"PRIVATE","signature":"PRIVATE"}"#, None),
            ("", None),
        ] {
            let mut model = model();
            let item = serde_json::from_value(json!({
                "id": "assistant", "key": {"event_seq": 11, "activity_id": "assistant"},
                "revision": 1, "summary": summary
            }))
            .unwrap();
            model
                .activities
                .insert("active".into(), vec![ConversationActivity::Assistant(item)]);
            let app = app(model);
            let bodies = bodies(&app);
            let mut expected_bodies = vec!["operator prompt", "Working…"];
            expected_bodies.extend(expected);
            assert_eq!(bodies, expected_bodies);
            assert!(bodies.iter().all(|body| !body.contains("PRIVATE")));
        }
    }

    #[tokio::test]
    async fn brief_hydration_retries_without_new_stream_events() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let request_calls = calls.clone();
        let server = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let calls = request_calls.clone();
                handlers.spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let read = socket.read(&mut buffer).await.unwrap();
                        assert!(read > 0);
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let request = String::from_utf8(request).unwrap();
                    if request.starts_with("GET /api/agents/default/conversation/stream?") {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n: heartbeat\n\n").await.unwrap();
                        while socket.read(&mut buffer).await.unwrap_or(0) > 0 {}
                        return;
                    }
                    let (status, body) = if request.starts_with("GET /api/agents/default/briefs/brief ") {
                        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            ("500 Internal Server Error", json!({"error": "transient"}))
                        } else {
                            ("200 OK", json!({
                                "id": "brief", "agent_id": "default", "kind": "result",
                                "created_at": "2026-01-01T00:00:01Z", "text": "recovered canonical result",
                                "attachments": null, "related_message_id": null, "related_task_id": null
                            }))
                        }
                    } else {
                        assert!(request.starts_with("GET /api/agents/default/conversation?"));
                        ("200 OK", json!({
                            "schema_version": 2, "query_version": 2, "runtime_id": "runtime",
                            "event_log_epoch": "epoch", "visibility_scope_id": "public",
                            "snapshot_through_seq": 10, "snapshot_cursor": "snapshot-10",
                            "turns": [turn("done", 1, 1, json!({"kind": "terminal", "outcome": "completed"}), vec!["brief"])],
                            "active_turns": [], "pending_inputs": [], "next_before_cursor": null, "has_more": false
                        }))
                    };
                    let body = body.to_string();
                    socket.write_all(format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ).as_bytes()).await.unwrap();
                });
            }
        });
        let client = LocalClient::remote(
            super::super::tests::test_config(),
            format!("http://{addr}"),
            "test-token",
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (_history, history_rx) = tokio::sync::mpsc::channel(1);
        let observer = tokio::spawn(observe(client, "default".into(), 1, tx, history_rx));
        let recovered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(message) = rx.recv().await {
                if let TuiRuntimeMessage::ConversationLoaded { model, .. } = message {
                    if model.briefs.contains_key("brief") {
                        return true;
                    }
                }
            }
            false
        })
        .await;
        observer.abort();
        server.abort();
        assert!(recovered.unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn activity_replay_deduplicates_ids_but_keeps_distinct_same_body_items() {
        let mut model = model();
        for _ in 0..2 {
            model.change(ConversationChange::ActivityUpsert {
                turn_id: "active".into(),
                activity: activity(),
            });
        }
        assert_eq!(model.activities["active"].len(), 1);
        let distinct = serde_json::from_value(json!({
            "kind": "tool", "id": "tool-2", "key": {"event_seq": 12, "activity_id": "tool-2"},
            "revision": 1, "summary": "tool detail"
        }))
        .unwrap();
        model.change(ConversationChange::ActivityUpsert {
            turn_id: "active".into(),
            activity: distinct,
        });
        assert_eq!(
            bodies(&app(model))
                .iter()
                .filter(|body| *body == "tool detail")
                .count(),
            2
        );
    }

    #[test]
    fn turn_inputs_keep_turn_order_independent_of_snapshot_page_order() {
        let mut model = model();
        let mut older = turn(
            "old",
            1,
            1,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec![],
        );
        older.inputs[0].preview = "older input".into();
        model.upsert_turn(older);
        let bodies = bodies(&app(model));
        assert_eq!(bodies[0], "older input");
        assert_eq!(bodies[1], "Awaiting canonical result");
        assert_eq!(bodies[2], "operator prompt");
        assert_eq!(bodies[3], "Working…");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn projected_unix_sse_does_not_require_runtime_event_contract_header() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(String::from_utf8(request)
                .unwrap()
                .starts_with("GET /api/agents/default/conversation/stream?"));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n: heartbeat\n\ndata: {\"type\":\"reset_required\",\"reason\":\"retention_expired\"}\n\n").await.unwrap();
        });
        let mut config = super::super::tests::test_config();
        config.socket_path = socket_path;
        let client = LocalClient::new(config).unwrap();
        let mut stream = client
            .stream_read_path("/agents/default/conversation/stream?after=checkpoint")
            .await
            .unwrap();
        assert_eq!(stream.next_json().await.unwrap()["type"], "reset_required");
        server.await.unwrap();
    }

    #[test]
    fn stale_agent_switch_and_removed_agent_responses_do_not_restore_old_cache() {
        let mut app = app(model());
        app.conversation_generation = 2;
        app.runtime_tx
            .send(TuiRuntimeMessage::ConversationLoaded {
                generation: 1,
                agent_id: "default".into(),
                model: model(),
            })
            .unwrap();
        app.process_runtime_messages();
        assert_eq!(app.conversation_generation, 2);
        app.clear_projection_view();
        app.runtime_tx
            .send(TuiRuntimeMessage::ConversationLoaded {
                generation: 2,
                agent_id: "default".into(),
                model: model(),
            })
            .unwrap();
        app.process_runtime_messages();
        assert!(app.conversation.is_none());
        app.agents.clear();
        let generation = app.conversation_generation;
        app.runtime_tx
            .send(TuiRuntimeMessage::ConversationLoaded {
                generation,
                agent_id: "default".into(),
                model: model(),
            })
            .unwrap();
        app.process_runtime_messages();
        assert!(app.conversation.is_none());
    }

    #[tokio::test]
    async fn http_summary_pagination_and_active_activity_hydration_use_v2_contract() {
        use axum::{routing::get, Json, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = requests.clone();
        let router = Router::new()
            .route("/api/agents/default/conversation", get(move |axum::extract::Query(params): axum::extract::Query<BTreeMap<String, String>>| {
                let count = request_count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let page = if params.contains_key("before") {
                        page(vec![turn("old", 1, 1, json!({"kind": "terminal", "outcome": "completed"}), vec![])], vec![], None)
                    } else { page(vec![], vec![turn("active", 2, 1, json!({"kind": "active"}), vec![])], Some("older&cursor")) };
                    Json(json!({"schema_version": page.schema_version, "query_version": page.query_version, "runtime_id": page.runtime_id, "event_log_epoch": page.event_log_epoch, "visibility_scope_id": page.visibility_scope_id, "snapshot_through_seq": page.snapshot_through_seq, "snapshot_cursor": page.snapshot_cursor, "turns": page.turns, "active_turns": page.active_turns, "pending_inputs": page.pending_inputs, "next_before_cursor": page.next_before_cursor, "has_more": page.has_more}))
                }
            }))
            .route("/api/agents/default/turns/active/activities", get(|| async { Json(json!({
                "schema_version": 2, "query_version": 2, "runtime_id": "runtime", "event_log_epoch": "epoch", "visibility_scope_id": "public",
                "turn": turn("active", 2, 1, json!({"kind": "active"}), vec![]), "detail_revision": 1, "activities": [activity()], "has_more": false, "next_before_cursor": null
            })) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = LocalClient::remote(
            super::super::tests::test_config(),
            format!("http://{address}"),
            "test-token",
        )
        .unwrap();
        let mut model = ConversationModel::load(&client, "default").await.unwrap();
        model.hydrate(&client).await.unwrap();
        assert_eq!(model.activities["active"], vec![activity()]);
        model.older(&client).await.unwrap();
        assert_eq!(model.turns.len(), 2);
        assert!(!model.has_older());
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn http_reset_required_discards_cursor_and_loads_a_fresh_snapshot() {
        use axum::{routing::get, Json, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let router = Router::new()
            .route("/api/agents/default/conversation", get(move || {
                let count = count.clone();
                async move {
                    let read = count.fetch_add(1, Ordering::SeqCst);
                    let turns = if read == 0 { vec![] } else { vec![turn("fresh", 1, 1, json!({"kind":"terminal","outcome":"completed"}), vec![])] };
                    Json(json!({"schema_version":2,"query_version":2,"runtime_id":"runtime","event_log_epoch":"epoch","visibility_scope_id":"public","snapshot_through_seq":10,"snapshot_cursor":"snapshot-10","turns":turns,"active_turns":[],"pending_inputs":[],"next_before_cursor":null,"has_more":false}))
                }
            }))
            .route("/api/agents/default/conversation/stream", get(|| async {
                (axum::http::StatusCode::CONFLICT, Json(json!({"error":"expired","code":"conversation_reset_required"})))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = LocalClient::remote(
            super::super::tests::test_config(),
            format!("http://{address}"),
            "test-token",
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (_history_tx, history_rx) = tokio::sync::mpsc::channel(1);
        let worker = tokio::spawn(observe(client, "default".into(), 1, tx, history_rx));
        let mut saw_reset = false;
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            loop {
                match rx.recv().await.unwrap() {
                    TuiRuntimeMessage::ConversationStatus { reset, .. } => {
                        saw_reset |= reset;
                    }
                    TuiRuntimeMessage::ConversationLoaded { model, .. }
                        if model.turns.contains_key("fresh") =>
                    {
                        break
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(saw_reset);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        worker.abort();
        server.abort();
    }

    #[tokio::test]
    async fn sse_disconnect_replays_from_last_committed_checkpoint_not_partial_batch() {
        use axum::{
            response::sse::{Event, Sse},
            routing::get,
            Json, Router,
        };
        use std::sync::{Arc, Mutex};
        let cursors = Arc::new(Mutex::new(Vec::new()));
        let recorded = cursors.clone();
        let router = Router::new()
            .route("/api/agents/default/conversation", get(|| async { Json(json!({
                "schema_version": 2, "query_version": 2, "runtime_id": "runtime", "event_log_epoch": "epoch", "visibility_scope_id": "public", "snapshot_through_seq": 10, "snapshot_cursor": "snapshot-10", "turns": [], "active_turns": [], "pending_inputs": [], "next_before_cursor": null, "has_more": false
            })) }))
            .route("/api/agents/default/conversation/stream", get(move |axum::extract::Query(params): axum::extract::Query<BTreeMap<String, String>>| {
                let recorded = recorded.clone();
                async move {
                    let count = { let mut cursors = recorded.lock().unwrap(); cursors.push(params["after"].clone()); cursors.len() };
                    let events = if count == 1 {
                        vec![begin(10, 11), json!({"type": "turn_summary_upsert", "turn": turn("partial", 1, 1, json!({"kind": "active"}), vec![])})]
                    } else {
                        vec![begin(10, 12), json!({"type": "turn_summary_upsert", "turn": turn("done", 2, 1, json!({"kind": "terminal", "outcome": "completed"}), vec![])}), checkpoint(12)]
                    };
                    Sse::new(tokio_stream::iter(events.into_iter().map(|value| Ok::<_, std::convert::Infallible>(Event::default().data(value.to_string())))))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = LocalClient::remote(
            super::super::tests::test_config(),
            format!("http://{address}"),
            "test-token",
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (_history_tx, history_rx) = tokio::sync::mpsc::channel(1);
        let worker = tokio::spawn(observe(client, "default".into(), 4, tx, history_rx));
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                if let Some(TuiRuntimeMessage::ConversationLoaded {
                    generation, model, ..
                }) = rx.recv().await
                {
                    assert_eq!(generation, 4);
                    assert!(!model.turns.contains_key("partial"));
                    if model.turns.contains_key("done") {
                        assert_eq!(model.cursor, "checkpoint-12");
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            &cursors.lock().unwrap()[..2],
            ["snapshot-10", "snapshot-10"]
        );
        worker.abort();
        server.abort();
    }
}
