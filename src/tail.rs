//! Bounded, read-only agent output projection over the conversation read model.
use std::{collections::BTreeMap, time::Duration};

use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    time::Instant,
};

pub mod stdout;

use crate::{
    client::{LocalClient, LocalHttpError},
    domain::conversation::{
        ConversationActivity, DetailCoverage, ExecutionState, TurnKey, CONVERSATION_QUERY_VERSION,
        CONVERSATION_SCHEMA_VERSION,
    },
    types::BriefRecord,
};

const END_RESERVE: usize = 2048;

#[derive(Args, Debug, Clone)]
pub struct TailOptions {
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Include persisted assistant text progress; output becomes NDJSON.
    #[arg(long)]
    pub include_progress: bool,
    /// Observe changes with a hard deadline; never waits for task completion.
    #[arg(long)]
    pub follow: bool,
    /// Resume after an opaque conversation stream checkpoint.
    #[arg(long, requires = "follow")]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 5000)]
    pub timeout_ms: u64,
    #[arg(long, default_value_t = 100)]
    pub max_events: usize,
    #[arg(long, default_value_t = 65536)]
    pub max_bytes: usize,
}

impl TailOptions {
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100).contains(&self.limit),
            "tail limit must be 1..=100"
        );
        ensure!(
            (1..=30_000).contains(&self.timeout_ms),
            "tail timeout-ms must be 1..=30000"
        );
        ensure!(
            (1..=1000).contains(&self.max_events),
            "tail max-events must be 1..=1000"
        );
        ensure!(
            (4096..=1_048_576).contains(&self.max_bytes),
            "tail max-bytes must be 4096..=1048576"
        );
        ensure!(
            self.cursor.is_none() || self.follow,
            "tail cursor requires follow"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Binding {
    schema_version: u32,
    query_version: u32,
    runtime_id: String,
    event_log_epoch: String,
    visibility_scope_id: String,
}

impl Binding {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == CONVERSATION_SCHEMA_VERSION
                && self.query_version == CONVERSATION_QUERY_VERSION,
            "unsupported conversation contract"
        );
        Ok(())
    }
}

// Deliberately omit inputs, pending messages, task evidence and tool details.
#[derive(Clone, Debug, Deserialize)]
struct Turn {
    turn_id: String,
    key: TurnKey,
    revision: u64,
    execution: ExecutionState,
    started_at: String,
    completed_at: Option<String>,
    brief_ids: Vec<String>,
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(flatten)]
    binding: Binding,
    snapshot_cursor: String,
    turns: Vec<Turn>,
    active_turns: Vec<Turn>,
    has_more: bool,
}

#[derive(Deserialize)]
struct ActivityPage {
    #[serde(flatten)]
    binding: Binding,
    activities: Vec<ConversationActivity>,
    coverage: DetailCoverage,
    has_more: bool,
}

#[derive(Clone)]
struct Record {
    id: String,
    revision: u64,
    value: Value,
}

fn progress(turn_id: &str, activity: ConversationActivity) -> Option<Record> {
    let ConversationActivity::Assistant(item) = activity else {
        return None;
    };
    if item.summary.trim().is_empty() {
        return None;
    }
    Some(Record {
        id: format!("progress:{}", item.id),
        revision: item.revision,
        value: json!({
            "type": "progress", "turn_id": turn_id, "activity_id": item.id,
            "revision": item.revision, "text": item.summary,
            "text_format": "conversation_activity_summary",
        }),
    })
}

fn turn_status(turn: &Turn) -> Record {
    Record {
        id: format!("turn:{}", turn.turn_id),
        revision: turn.revision,
        value: json!({
            "type": "turn_status", "turn_id": turn.turn_id, "revision": turn.revision,
            "execution": turn.execution, "started_at": turn.started_at,
            "completed_at": turn.completed_at,
        }),
    }
}

fn brief_record(agent_id: &str, brief: BriefRecord) -> Result<Record> {
    ensure!(brief.agent_id == agent_id, "canonical brief agent mismatch");
    Ok(Record {
        id: format!("brief:{}", brief.id),
        revision: 1,
        value: json!({
            "type": "brief", "brief_id": brief.id, "turn_id": brief.turn_id,
            "work_item_id": brief.work_item_id, "created_at": brief.created_at,
            "text": brief.text,
        }),
    })
}

struct Output<'a, W> {
    writer: &'a mut W,
    agent_id: &'a str,
    max_events: usize,
    max_bytes: usize,
    count: usize,
    bytes: usize,
    seen: BTreeMap<String, u64>,
    delivery_in_flight: bool,
}

impl<W: AsyncWrite + Unpin> Output<'_, W> {
    // Emit batches atomically relative to the cursor. A partial batch must never
    // be acknowledged, even when only a later record exhausts the byte budget.
    async fn emit(&mut self, records: Vec<Record>) -> Result<bool> {
        let mut revisions = self.seen.clone();
        let mut lines = Vec::new();
        for mut record in records {
            if revisions
                .get(&record.id)
                .is_some_and(|revision| *revision >= record.revision)
            {
                continue;
            }
            revisions.insert(record.id, record.revision);
            record.value["scope"] = json!("agent");
            record.value["agent_id"] = json!(self.agent_id);
            let mut line = serde_json::to_vec(&record.value)?;
            line.push(b'\n');
            lines.push(line);
        }
        let bytes: usize = lines.iter().map(Vec::len).sum();
        if self.count + lines.len() > self.max_events
            || self.bytes + bytes + END_RESERVE > self.max_bytes
        {
            return Ok(false);
        }
        self.delivery_in_flight = true;
        for line in &lines {
            self.writer.write_all(line).await?;
        }
        self.writer.flush().await?;
        self.delivery_in_flight = false;
        self.count += lines.len();
        self.bytes += bytes;
        self.seen = revisions;
        Ok(true)
    }
}

#[derive(Default)]
struct Observation {
    cursor: Option<String>,
    runtime_status: Option<Value>,
    snapshot_truncated: bool,
    reset_reason: Option<String>,
    activity_heads: BTreeMap<String, u64>,
    resume_coverage_unknown: bool,
    activity_coverage: Option<DetailCoverage>,
}

struct Batch {
    id: String,
    through_seq: u64,
    records: Vec<Record>,
    brief_ids: Vec<String>,
    invalidated_turns: Vec<String>,
    frames: usize,
}

struct StreamProjection {
    binding: Binding,
    batch: Option<Batch>,
}

impl StreamProjection {
    fn accept(&mut self, frame: Value, include_progress: bool) -> Result<Option<(Batch, String)>> {
        match frame["type"]
            .as_str()
            .context("conversation frame missing type")?
        {
            "batch_begin" => {
                ensure!(self.batch.is_none(), "nested conversation batch");
                let binding: Binding = serde_json::from_value(frame.clone())?;
                ensure!(
                    binding == self.binding,
                    "conversation stream binding changed"
                );
                self.batch = Some(Batch {
                    id: frame["batch_id"]
                        .as_str()
                        .context("batch id missing")?
                        .into(),
                    through_seq: frame["through_seq"]
                        .as_u64()
                        .context("batch sequence missing")?,
                    records: Vec::new(),
                    brief_ids: Vec::new(),
                    invalidated_turns: Vec::new(),
                    frames: 0,
                });
            }
            "checkpoint" => {
                let batch = self.batch.take().context("checkpoint without batch")?;
                ensure!(
                    frame["batch_id"].as_str() == Some(batch.id.as_str())
                        && frame["through_seq"].as_u64() == Some(batch.through_seq)
                        && frame["event_log_epoch"].as_str()
                            == Some(self.binding.event_log_epoch.as_str())
                        && frame["visibility_scope_id"].as_str()
                            == Some(self.binding.visibility_scope_id.as_str()),
                    "conversation checkpoint mismatch"
                );
                let cursor = frame["checkpoint"]
                    .as_str()
                    .context("checkpoint cursor missing")?;
                ensure!(
                    cursor.len() <= 1024,
                    "conversation cursor exceeds tail receipt bound"
                );
                return Ok(Some((batch, cursor.into())));
            }
            "reset_required" => bail!("unexpected reset frame in batch projection"),
            kind => {
                let batch = self
                    .batch
                    .as_mut()
                    .context("conversation change outside batch")?;
                batch.frames += 1;
                ensure!(
                    batch.frames <= 1024,
                    "conversation batch frame budget exceeded"
                );
                match kind {
                    "activity_upsert" if include_progress => {
                        let turn_id = frame["turn_id"].as_str().context("activity turn missing")?;
                        if frame["activity"]["kind"] == "assistant" {
                            let activity = serde_json::from_value(frame["activity"].clone())?;
                            if let Some(record) = progress(turn_id, activity) {
                                batch.records.push(record);
                            }
                        }
                    }
                    "turn_summary_upsert" => {
                        let turn: Turn = serde_json::from_value(frame["turn"].clone())?;
                        batch.brief_ids.extend(turn.brief_ids.iter().cloned());
                        if include_progress {
                            batch.records.push(turn_status(&turn));
                        }
                    }
                    "detail_invalidated" if include_progress => {
                        batch.invalidated_turns.push(
                            frame["turn_id"]
                                .as_str()
                                .context("invalidated turn missing")?
                                .into(),
                        );
                    }
                    "operator_upsert" | "operator_remove" | "activity_upsert"
                    | "detail_invalidated" => {}
                    _ => bail!("unknown conversation change type: {kind}"),
                }
            }
        }
        Ok(None)
    }
}

fn read_path(agent_id: &str, suffix: &str, parameters: &[(&str, String)]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in parameters {
        query.append_pair(key, value);
    }
    format!("/agents/{agent_id}/{suffix}?{}", query.finish())
}

fn activity_seq(activity: &ConversationActivity) -> u64 {
    match activity {
        ConversationActivity::Assistant(item)
        | ConversationActivity::Operator(item)
        | ConversationActivity::Tool(item)
        | ConversationActivity::Wait(item)
        | ConversationActivity::Error(item) => item.key.event_seq,
    }
}

async fn activities(client: &LocalClient, agent_id: &str, turn_id: &str) -> Result<ActivityPage> {
    client
        .get_json(&read_path(
            agent_id,
            &format!("turns/{turn_id}/activities"),
            &[("limit", "100".into())],
        ))
        .await
}

async fn observe_inner<W: AsyncWrite + Unpin>(
    client: &LocalClient,
    agent_id: &str,
    options: &TailOptions,
    output: &mut Output<'_, W>,
    state: &mut Observation,
) -> Result<&'static str> {
    let handshake: Value = client.get_json("/handshake").await?;
    ensure!(
        handshake["capabilities"].as_array().is_some_and(|values| values
            .iter().any(|value| value == "agents.conversation-read.v1")),
        "agent tail observation requires agents.conversation-read.v1; default brief tail remains available"
    );
    let status = client.get_agent_compact(agent_id).await?;
    ensure!(status.agent.id == agent_id, "runtime status agent mismatch");
    state.runtime_status = Some(json!({
        "status": status.agent.status, "sleeping_until": status.agent.sleeping_until,
        "pending": status.agent.pending, "current_turn_id": status.agent.current_turn_id,
        "last_brief_at": status.agent.last_brief_at, "observed_at": chrono::Utc::now(),
    }));
    let snapshot: Snapshot = client
        .get_json(&read_path(
            agent_id,
            "conversation",
            &[("limit", options.limit.to_string())],
        ))
        .await?;
    snapshot.binding.validate()?;
    ensure!(
        snapshot.snapshot_cursor.len() <= 1024,
        "conversation cursor exceeds tail receipt bound"
    );
    state.snapshot_truncated = snapshot.has_more;
    if options.cursor.is_none() {
        state.cursor = Some(snapshot.snapshot_cursor);
        // Current/most recent turns first: a busy agent's live progress should
        // not be crowded out by old turn details.
        let mut turns: BTreeMap<TurnKey, Turn> = BTreeMap::new();
        for turn in snapshot.turns.into_iter().chain(snapshot.active_turns) {
            turns.insert(turn.key.clone(), turn);
        }
        if options.include_progress {
            for turn in turns.into_values().rev() {
                let page = activities(client, agent_id, &turn.turn_id).await?;
                ensure!(
                    page.binding == snapshot.binding,
                    "conversation detail binding changed"
                );
                state.snapshot_truncated |= page.has_more;
                if page.coverage != DetailCoverage::Complete {
                    state.snapshot_truncated = true;
                    state.activity_coverage = Some(page.coverage.clone());
                }
                if let Some(head) = page.activities.iter().map(activity_seq).max() {
                    state.activity_heads.insert(turn.turn_id.clone(), head);
                }
                let mut records = vec![turn_status(&turn)];
                records.extend(
                    page.activities
                        .into_iter()
                        .filter_map(|item| progress(&turn.turn_id, item)),
                );
                for record in records {
                    if !output.emit(vec![record]).await? {
                        state.snapshot_truncated = true;
                        return Ok("budget");
                    }
                }
            }
        }
        for brief in client.agent_briefs(agent_id, options.limit).await? {
            if !output.emit(vec![brief_record(agent_id, brief)?]).await? {
                state.snapshot_truncated = true;
                return Ok("budget");
            }
        }
    }
    if !options.follow {
        return Ok("snapshot");
    }
    let mut stream = client
        .stream_read_path(&read_path(
            agent_id,
            "conversation/stream",
            &[
                (
                    "after",
                    state.cursor.clone().context("tail stream cursor missing")?,
                ),
                ("limit", "100".into()),
                ("activity_limit", "64".into()),
            ],
        ))
        .await?;
    let mut projection = StreamProjection {
        binding: snapshot.binding,
        batch: None,
    };
    loop {
        let frame = stream.next_json().await?;
        if frame["type"] == "reset_required" {
            state.reset_reason = Some(frame["reason"].as_str().unwrap_or("unknown").into());
            return Ok("reset_required");
        }
        let Some((mut batch, cursor)) = projection.accept(frame, options.include_progress)? else {
            continue;
        };
        let mut heads = state.activity_heads.clone();
        batch.invalidated_turns.sort();
        batch.invalidated_turns.dedup();
        for turn_id in batch.invalidated_turns {
            let page = activities(client, agent_id, &turn_id).await?;
            ensure!(
                page.binding == projection.binding,
                "conversation detail binding changed"
            );
            state.snapshot_truncated |= page.has_more || page.coverage != DetailCoverage::Complete;
            if page.coverage != DetailCoverage::Complete {
                state.activity_coverage = Some(page.coverage);
                state.reset_reason = Some("activity_coverage_unavailable".into());
                return Ok("reset_required");
            }
            let oldest = page.activities.iter().map(activity_seq).min();
            if page.has_more {
                if let Some(previous) = state.activity_heads.get(&turn_id) {
                    if !oldest.is_some_and(|first| first <= *previous) {
                        state.reset_reason = Some("activity_gap".into());
                        return Ok("reset_required");
                    }
                } else {
                    // Checkpoints are notification positions, not activity
                    // watermarks. Resume exposes bounded upserts with explicit
                    // unknown prior coverage, never promises lossless replay.
                    state.resume_coverage_unknown = true;
                }
            }
            if let Some(head) = page.activities.iter().map(activity_seq).max() {
                heads.insert(turn_id.clone(), head);
            }
            batch.records.extend(
                page.activities
                    .into_iter()
                    .filter_map(|item| progress(&turn_id, item)),
            );
        }
        batch.brief_ids.sort();
        batch.brief_ids.dedup();
        for id in batch.brief_ids {
            if output.seen.contains_key(&format!("brief:{id}")) {
                continue;
            }
            let brief = client.agent_brief(agent_id, &id).await?;
            ensure!(brief.id == id, "canonical brief identity mismatch");
            batch.records.push(brief_record(agent_id, brief)?);
        }
        if !output.emit(batch.records).await? {
            return Ok("budget");
        }
        state.cursor = Some(cursor);
        state.activity_heads = heads;
    }
}

pub async fn observe<W: AsyncWrite + Unpin>(
    client: &LocalClient,
    agent_id: &str,
    options: &TailOptions,
    writer: &mut W,
) -> Result<()> {
    options.validate()?;
    ensure!(
        options
            .cursor
            .as_ref()
            .is_none_or(|cursor| cursor.len() <= 1024),
        "tail cursor exceeds receipt bound"
    );
    let mut state = Observation {
        cursor: options.cursor.clone(),
        ..Observation::default()
    };
    let mut output = Output {
        writer,
        agent_id,
        max_events: options.max_events,
        max_bytes: options.max_bytes,
        count: 0,
        bytes: 0,
        seen: BTreeMap::new(),
        delivery_in_flight: false,
    };
    let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
    // Reserve a small part of the same hard deadline for the final receipt.
    let read_deadline = deadline - Duration::from_millis(options.timeout_ms.min(100) / 10);
    let mut result = tokio::select! {
        result = observe_inner(client, agent_id, options, &mut output, &mut state) => result,
        _ = tokio::time::sleep_until(read_deadline) => Ok("timeout"),
        result = tokio::signal::ctrl_c() => {
            result.context("failed to listen for observer interruption")?;
            Ok("interrupted")
        }
    };
    if let Err(error) = &result {
        if let Some(http) = error.downcast_ref::<LocalHttpError>() {
            if http.has_code("conversation_reset_required")
                || http
                    .code
                    .as_deref()
                    .is_some_and(|code| code.starts_with("conversation_cursor_"))
            {
                state.reset_reason = http.code.clone();
                state.cursor = None;
                result = Ok("reset_required");
            }
        }
    }
    if output.delivery_in_flight {
        result.context("tail output delivery failed")?;
        bail!("tail output delivery interrupted; no complete end receipt could be written");
    }
    let stop_reason = result.as_ref().copied().unwrap_or("error");
    let end = json!({
        "type": "end", "scope": "agent", "agent_id": agent_id,
        "cursor": state.cursor, "stop_reason": stop_reason,
        "record_count": output.count, "snapshot_truncated": state.snapshot_truncated,
        "runtime_status": state.runtime_status, "reset_reason": state.reset_reason,
        "resume_coverage_unknown": state.resume_coverage_unknown,
        "activity_coverage": state.activity_coverage,
    });
    let mut line = serde_json::to_vec(&end)?;
    line.push(b'\n');
    ensure!(
        line.len() <= END_RESERVE,
        "tail end receipt exceeds reserve"
    );
    tokio::time::timeout_at(deadline, async {
        output.writer.write_all(&line).await?;
        output.writer.flush().await
    })
    .await
    .context("tail end receipt delivery deadline exceeded")??;
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::conversation::{ActivityItem, ActivityKey};

    fn item(id: &str, revision: u64, text: &str) -> ActivityItem {
        ActivityItem {
            id: id.into(),
            revision,
            summary: text.into(),
            key: ActivityKey {
                event_seq: revision,
                activity_id: id.into(),
            },
        }
    }

    fn binding() -> Binding {
        Binding {
            schema_version: CONVERSATION_SCHEMA_VERSION,
            query_version: CONVERSATION_QUERY_VERSION,
            runtime_id: "runtime".into(),
            event_log_epoch: "epoch".into(),
            visibility_scope_id: "scope".into(),
        }
    }

    #[test]
    fn projection_selects_only_nonempty_assistant_text() {
        let secret = item("other", 1, "private");
        for activity in [
            ConversationActivity::Operator(secret.clone()),
            ConversationActivity::Tool(secret.clone()),
            ConversationActivity::Wait(secret.clone()),
            ConversationActivity::Error(secret),
            ConversationActivity::Assistant(item("empty", 1, "  ")),
        ] {
            assert!(progress("turn", activity).is_none());
        }
        let record = progress(
            "turn",
            ConversationActivity::Assistant(item("a", 2, "working")),
        )
        .unwrap();
        assert_eq!(record.value["text"], "working");
        assert_eq!(record.value["revision"], 2);
        assert_eq!(record.value["turn_id"], "turn");
    }

    #[tokio::test]
    async fn serialized_byte_budget_is_atomic_and_revisions_are_deduplicated() {
        let mut bytes = Vec::new();
        let mut output = Output {
            writer: &mut bytes,
            agent_id: "child",
            max_events: 3,
            max_bytes: 4096,
            count: 0,
            bytes: 0,
            seen: BTreeMap::new(),
            delivery_in_flight: false,
        };
        let make = |id, rev, text| {
            progress("turn", ConversationActivity::Assistant(item(id, rev, text))).unwrap()
        };
        assert!(output.emit(vec![make("a", 1, "你好")]).await.unwrap());
        let previous = output.bytes;
        assert!(output.emit(vec![make("a", 1, "你好")]).await.unwrap());
        assert_eq!(output.bytes, previous);
        let oversized = "界".repeat(900);
        assert!(!output
            .emit(vec![make("b", 1, "small"), make("c", 1, &oversized)])
            .await
            .unwrap());
        assert_eq!(output.bytes, previous);
        assert!(!output.seen.contains_key("progress:b"));
        assert!(output
            .emit(vec![make("a", 2, "new revision")])
            .await
            .unwrap());
        assert_eq!(output.count, 2);
        assert!(output.bytes + END_RESERVE <= 4096);
    }

    #[tokio::test]
    async fn blocked_batch_delivery_is_cancellable_without_acknowledging_records() {
        let (mut writer, _unread_reader) = tokio::io::duplex(1);
        let mut output = Output {
            writer: &mut writer,
            agent_id: "child",
            max_events: 100,
            max_bytes: 65536,
            count: 0,
            bytes: 0,
            seen: BTreeMap::new(),
            delivery_in_flight: false,
        };
        let record = progress(
            "turn",
            ConversationActivity::Assistant(item("a", 1, "working")),
        )
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), output.emit(vec![record]),)
                .await
                .is_err()
        );
        assert!(output.delivery_in_flight);
        assert_eq!(output.count, 0);
        assert!(output.seen.is_empty());
    }

    fn begin() -> Value {
        json!({
            "type": "batch_begin", "batch_id": "batch", "through_seq": 10,
            "schema_version": CONVERSATION_SCHEMA_VERSION, "query_version": CONVERSATION_QUERY_VERSION,
            "runtime_id": "runtime", "event_log_epoch": "epoch", "visibility_scope_id": "scope",
        })
    }

    fn checkpoint() -> Value {
        json!({
            "type": "checkpoint", "batch_id": "batch", "through_seq": 10,
            "event_log_epoch": "epoch", "visibility_scope_id": "scope", "checkpoint": "opaque",
        })
    }

    #[test]
    fn stream_stages_until_matching_checkpoint_and_excludes_messages() {
        let mut projection = StreamProjection {
            binding: binding(),
            batch: None,
        };
        assert!(projection.accept(begin(), true).unwrap().is_none());
        assert!(projection
            .accept(
                json!({"type":"operator_upsert","input":{"preview":"private"}}),
                true
            )
            .unwrap()
            .is_none());
        assert!(projection
            .accept(
                json!({
                    "type":"activity_upsert", "turn_id":"turn",
                    "activity": ConversationActivity::Assistant(item("a", 1, "working")),
                }),
                true
            )
            .unwrap()
            .is_none());
        assert!(projection
            .accept(json!({"type":"detail_invalidated","turn_id":"turn"}), true)
            .unwrap()
            .is_none());
        let (batch, cursor) = projection.accept(checkpoint(), true).unwrap().unwrap();
        assert_eq!(cursor, "opaque");
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.invalidated_turns, vec!["turn"]);
        assert!(projection.batch.is_none());
    }

    #[test]
    fn stream_rejects_changed_binding_and_mismatched_checkpoint() {
        let mut projection = StreamProjection {
            binding: binding(),
            batch: None,
        };
        let mut changed = begin();
        changed["runtime_id"] = json!("replacement");
        assert!(projection.accept(changed, true).is_err());
        projection.accept(begin(), true).unwrap();
        let mut changed = checkpoint();
        changed["through_seq"] = json!(11);
        assert!(projection.accept(changed, true).is_err());
    }

    #[test]
    fn briefs_only_mode_ignores_progress_and_detail_invalidation() {
        let mut projection = StreamProjection {
            binding: binding(),
            batch: None,
        };
        projection.accept(begin(), false).unwrap();
        projection
            .accept(
                json!({"type":"activity_upsert","activity":{"kind":"assistant"}}),
                false,
            )
            .unwrap();
        projection
            .accept(json!({"type":"detail_invalidated"}), false)
            .unwrap();
        let (batch, _) = projection.accept(checkpoint(), false).unwrap().unwrap();
        assert!(batch.records.is_empty());
        assert!(batch.invalidated_turns.is_empty());
    }

    #[test]
    fn cli_requires_follow_for_cursor_and_rejects_unbounded_requests() {
        use clap::Parser;
        assert!(crate::cli::Cli::try_parse_from(["holon", "tail", "--cursor", "opaque"]).is_err());
        let mut options = TailOptions {
            limit: 20,
            include_progress: true,
            follow: true,
            cursor: None,
            timeout_ms: 5000,
            max_events: 100,
            max_bytes: 65536,
        };
        assert!(options.validate().is_ok());
        options.timeout_ms = 30001;
        assert!(options.validate().is_err());
        options.timeout_ms = 1000;
        options.max_bytes = 0;
        assert!(options.validate().is_err());
    }
}
