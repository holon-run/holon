//! Turn-scoped conversation content deletion.
//!
//! Deletes one settled turn's conversation content in a single transaction:
//! physically removes turn-exclusive evidence rows, erases body copies held in
//! retained rows, invalidates derived context artifacts, and records the
//! authoritative `content_deleted_at` marker. Fail-closed: any unmet safety
//! check aborts with a typed conflict before any row changes. See
//! docs/rfcs/conversation-turn-deletion.md.

use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use serde_json::Value;

use holon_domain::{command_receipt_source_ref, TurnRecord};
use holon_storage::index_outbox::{
    insert_runtime_index_changes_tx, RuntimeIndexChange, RuntimeIndexOperation,
};

use crate::runtime_db::conversation::bump_turn_revision_tx;
use crate::runtime_db::RuntimeDb;

/// Evidence tables whose rows carry conversation bodies and are keyed by
/// `(agent_id, turn_id)`. Turn-exclusive rows are removed physically.
const EVIDENCE_TABLES: [&str; 8] = [
    "messages",
    "transcript_entries",
    "tool_executions",
    "model_requests",
    "model_responses",
    "briefs",
    "delivery_summaries",
    "artifact_metadata",
];

/// `conversation_source_revisions.source_kind` values joined against evidence
/// tables for activity display.
const SOURCE_KIND_TABLES: [(&str, &str); 3] = [
    ("operator", "messages"),
    ("assistant", "transcript_entries"),
    ("tool", "tool_executions"),
];

/// `input_evidence_current.family` values whose `record_id` names turn-owned
/// evidence. Matching rows flip to `availability = 'pruned'`.
const INPUT_EVIDENCE_FAMILIES: [(&str, &str); 4] = [
    ("message", "messages"),
    ("tool_execution", "tool_executions"),
    ("brief", "briefs"),
    ("transcript", "transcript_entries"),
];

/// Managed artifact path prefixes eligible for post-commit asynchronous
/// unlink. Paths are evidence, never authority to touch arbitrary files.
const ARTIFACT_PATH_PREFIXES: [&str; 3] = ["tool-artifacts/", "media/inbox/", "media/generated/"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnContentDeletionError {
    NotFound,
    Conflict(TurnContentDeletionConflict),
}

impl std::fmt::Display for TurnContentDeletionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "turn not found for this agent"),
            Self::Conflict(conflict) => write!(f, "{conflict}"),
        }
    }
}

impl std::error::Error for TurnContentDeletionError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TurnContentDeletionConflict {
    /// The turn is still executing or holds uncommitted results.
    ActiveExecution,
    /// The confirmed scope changed before submission.
    RevisionConflict,
    /// Turn content is shared with another turn or a replay chain.
    SharedContent,
    /// Sealed input-evidence eligibility still requires this content.
    SealedReference,
}

impl std::fmt::Display for TurnContentDeletionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveExecution => write!(
                f,
                "turn is still executing or holds unsettled results; stop execution and retry after settlement"
            ),
            Self::RevisionConflict => {
                write!(f, "turn revision changed; re-confirm the deletion scope")
            }
            Self::SharedContent => write!(
                f,
                "turn content is shared with another turn; refusing partial deletion"
            ),
            Self::SealedReference => {
                write!(f, "sealed input-evidence eligibility still requires this content")
            }
        }
    }
}

/// Confirmation impact scope for a turn deletion. Never carries bodies.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnContentDeletionPreview {
    pub agent_id: String,
    pub turn_id: String,
    pub turn_index: u64,
    pub revision: u64,
    pub input_count: usize,
    pub brief_count: usize,
    pub tool_execution_count: usize,
    pub transcript_count: usize,
    pub already_deleted: bool,
    pub content_deleted_at: Option<String>,
    /// Human-readable handling notes for derived content (episodes,
    /// compression, search index, artifacts). No bodies.
    pub derived_handling: Vec<&'static str>,
}

/// Successful deletion outcome. `already_deleted` retries keep the first
/// timestamp and bump no revision.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnContentDeletion {
    pub agent_id: String,
    pub turn_id: String,
    pub turn_index: u64,
    pub content_deleted_at: String,
    pub revision: u64,
    pub already_deleted: bool,
    pub removed_evidence: usize,
    pub invalidated_episodes: usize,
}

struct TargetTurn {
    turn_id: String,
    turn_index: u64,
    record: TurnRecord,
    content_deleted_at: Option<String>,
    revision: u64,
}

/// Turn-exclusive evidence discovered through canonical associations.
#[derive(Default)]
struct TurnEvidence {
    /// `(table, evidence_id)` pairs.
    rows: Vec<(&'static str, String)>,
}

impl TurnEvidence {
    fn ids_for(&self, table: &str) -> Vec<String> {
        self.rows
            .iter()
            .filter(|(row_table, _)| *row_table == table)
            .map(|(_, id)| id.clone())
            .collect()
    }

    fn all_ids(&self) -> Vec<String> {
        self.rows.iter().map(|(_, id)| id.clone()).collect()
    }
}

pub fn preview_turn_content_deletion(
    db: &RuntimeDb,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<TurnContentDeletionPreview>> {
    db.with_read(|connection| {
        let transaction = connection.unchecked_transaction()?;
        let Some(target) = load_target_turn(&transaction, agent_id, turn_id)? else {
            return Ok(None);
        };
        let evidence = collect_turn_evidence(&transaction, agent_id, &target)?;
        Ok(Some(TurnContentDeletionPreview {
            agent_id: agent_id.to_owned(),
            turn_id: target.turn_id.clone(),
            turn_index: target.turn_index,
            revision: target.revision,
            input_count: evidence.ids_for("messages").len(),
            brief_count: evidence.ids_for("briefs").len(),
            tool_execution_count: evidence.ids_for("tool_executions").len(),
            transcript_count: evidence.ids_for("transcript_entries").len(),
            already_deleted: target.content_deleted_at.is_some(),
            content_deleted_at: target.content_deleted_at.clone(),
            derived_handling: vec![
                "episode summaries covering this turn are invalidated and rebuilt from surviving history",
                "search index entries are removed asynchronously; reads verify main-database state",
                "turn-exclusive artifact files are unlinked asynchronously; downloads stop at commit",
            ],
        }))
    })
}

pub fn delete_turn_content(
    db: &RuntimeDb,
    agent_id: &str,
    turn_id: &str,
    expected_revision: Option<u64>,
    now: DateTime<Utc>,
) -> Result<Result<TurnContentDeletion, TurnContentDeletionError>> {
    db.transaction(|tx| {
        let Some(target) = load_target_turn(tx, agent_id, turn_id)? else {
            return Ok(Err(TurnContentDeletionError::NotFound));
        };
        if let Some(deleted_at) = target.content_deleted_at.as_deref() {
            // Idempotent retry: the first deletion timestamp is authoritative
            // and never rewritten.
            return Ok(Ok(TurnContentDeletion {
                agent_id: agent_id.to_owned(),
                turn_id: target.turn_id,
                turn_index: target.turn_index,
                content_deleted_at: deleted_at.to_owned(),
                revision: target.revision,
                already_deleted: true,
                removed_evidence: 0,
                invalidated_episodes: 0,
            }));
        }
        if let Some(expected) = expected_revision {
            if expected != target.revision {
                return Ok(Err(TurnContentDeletionError::Conflict(
                    TurnContentDeletionConflict::RevisionConflict,
                )));
            }
        }
        let evidence = collect_turn_evidence(tx, agent_id, &target)?;
        if let Some(conflict) = audit_deletion_conflicts(tx, agent_id, &target, &evidence)? {
            return Ok(Err(TurnContentDeletionError::Conflict(conflict)));
        }

        // Collect derived inputs before removing the source rows.
        let tool_payloads = tool_execution_payloads(tx, agent_id, &target)?;
        let artifact_paths = managed_artifact_paths(&tool_payloads);

        let removed_evidence =
            remove_exclusive_evidence(tx, agent_id, &target, &evidence, &tool_payloads, now)?;
        scrub_retained_payloads(tx, agent_id, &target)?;
        let invalidated_episodes = invalidate_episodes(tx, agent_id, &target, now)?;
        update_agent_working_memory(tx, agent_id, &target)?;
        mark_input_evidence_pruned(tx, agent_id, &target, &evidence)?;
        enqueue_artifact_deletions(tx, agent_id, &artifact_paths, now)?;

        let deleted_at = holon_storage::codec::timestamp(now);
        let revision =
            bump_turn_revision_tx(tx, agent_id, &target.turn_id, true, true, now)?.summary;
        tx.execute(
            "UPDATE turn_records
                SET content_deleted_at = ?3,
                    payload_json = ?4
              WHERE agent_id = ?1 AND turn_id = ?2 AND content_deleted_at IS NULL",
            params![
                agent_id,
                turn_id,
                deleted_at,
                erased_payload_json(&target.record)?
            ],
        )?;
        append_deletion_audit_event(tx, agent_id, turn_id, &deleted_at)?;
        Ok(Ok(TurnContentDeletion {
            agent_id: agent_id.to_owned(),
            turn_id: target.turn_id,
            turn_index: target.turn_index,
            content_deleted_at: deleted_at,
            revision,
            already_deleted: false,
            removed_evidence,
            invalidated_episodes,
        }))
    })
}

fn load_target_turn(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<TargetTurn>> {
    validate_turn_id(turn_id)?;
    let row = connection
        .query_row(
            "SELECT turns.payload_json,
                    turns.turn_index,
                    turns.content_deleted_at,
                    COALESCE(revisions.summary_revision, 1)
             FROM turn_records AS turns
             LEFT JOIN conversation_turn_revisions AS revisions
               ON revisions.agent_id = turns.agent_id
              AND revisions.turn_id = turns.turn_id
             WHERE turns.agent_id = ?1 AND turns.turn_id = ?2",
            params![agent_id, turn_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((payload_json, turn_index, content_deleted_at, revision)) = row else {
        return Ok(None);
    };
    let record: TurnRecord = serde_json::from_str(&payload_json)?;
    Ok(Some(TargetTurn {
        turn_id: turn_id.to_owned(),
        turn_index: u64::try_from(turn_index).unwrap_or(0),
        record,
        content_deleted_at,
        revision: u64::try_from(revision).unwrap_or(1),
    }))
}

fn validate_turn_id(turn_id: &str) -> Result<()> {
    ensure!(
        !turn_id.is_empty() && turn_id.len() <= 128 && turn_id.chars().all(is_turn_id_char),
        "invalid turn id"
    );
    Ok(())
}

fn is_turn_id_char(candidate: char) -> bool {
    candidate.is_ascii_alphanumeric() || matches!(candidate, '_' | '-' | ':')
}

fn collect_turn_evidence(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
) -> Result<TurnEvidence> {
    let mut rows = Vec::new();
    for table in EVIDENCE_TABLES {
        let mut statement = tx.prepare(&format!(
            "SELECT evidence_id FROM {table} WHERE agent_id = ?1 AND turn_id = ?2"
        ))?;
        let ids = statement
            .query_map(params![agent_id, target.turn_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.extend(ids.into_iter().map(|id| (table, id)));
    }
    Ok(TurnEvidence { rows })
}

fn placeholders(count: usize) -> String {
    std::iter::repeat("?")
        .take(count)
        .collect::<Vec<_>>()
        .join(",")
}

/// Safety checks that reject the whole deletion. Runs before any mutation.
fn audit_deletion_conflicts(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
    evidence: &TurnEvidence,
) -> Result<Option<TurnContentDeletionConflict>> {
    // The turn itself must be terminal.
    let terminal = tx.query_row(
        "SELECT terminal_kind IS NOT NULL FROM turn_records
          WHERE agent_id = ?1 AND turn_id = ?2",
        params![agent_id, target.turn_id],
        |row| row.get::<_, bool>(0),
    )?;
    // Unsettled results that reference this turn.
    let unsettled = exists(
        tx,
        "SELECT 1 FROM task_result_settlements
          WHERE agent_id = ?1 AND parent_turn_id = ?2 AND state <> 'settled'",
        &[agent_id, &target.turn_id],
    )?;
    let queue_claimed = exists(
        tx,
        "SELECT 1 FROM queue_entries
          WHERE agent_id = ?1
            AND status IN ('queued','dequeued','interrupted','interjected')
            AND message_id IN (
              SELECT message_id FROM conversation_input_assignments
               WHERE agent_id = ?1 AND turn_id = ?2
            )",
        &[agent_id, &target.turn_id],
    )?;
    let open_attempt = exists(
        tx,
        "SELECT 1 FROM execution_protocol_attempts
          WHERE agent_id = ?1 AND lifecycle_state = 'open'
            AND json_extract(payload_json, '$.turn_id') = ?2",
        &[agent_id, &target.turn_id],
    )?;
    if !terminal || unsettled || queue_claimed || open_attempt {
        return Ok(Some(TurnContentDeletionConflict::ActiveExecution));
    }

    // Evidence shared through the activity spine of another turn.
    for (source_kind, table) in SOURCE_KIND_TABLES {
        let ids = evidence.ids_for(table);
        if ids.is_empty() {
            continue;
        }
        let mut bindings = vec![agent_id.to_owned(), source_kind.to_owned()];
        bindings.extend(ids);
        bindings.push(target.turn_id.clone());
        let sql = format!(
            "SELECT 1 FROM conversation_source_revisions
              WHERE agent_id = ? AND source_kind = ?
                AND source_id IN ({})
                AND turn_id IS NOT ?",
            placeholders(bindings.len() - 3)
        );
        let shared = {
            let mut statement = tx.prepare(&sql)?;
            statement.exists(rusqlite::params_from_iter(bindings.iter()))?
        };
        if shared {
            return Ok(Some(TurnContentDeletionConflict::SharedContent));
        }
    }

    // Another turn's payload claims the same evidence ids.
    let all_ids = evidence.all_ids();
    if !all_ids.is_empty() {
        for json_path in [
            "$.input_message_ids",
            "$.tool_execution_ids",
            "$.produced_brief_ids",
            "$.delivery_summary_ids",
        ] {
            let mut bindings = vec![json_path.to_owned(), agent_id.to_owned()];
            bindings.push(target.turn_id.clone());
            bindings.extend(all_ids.clone());
            let sql = format!(
                "SELECT 1 FROM turn_records AS t, json_each(t.payload_json, ?1) AS j
                  WHERE t.agent_id = ?2 AND t.turn_id <> ?3 AND j.value IN ({})",
                placeholders(bindings.len() - 3)
            );
            let shared = {
                let mut statement = tx.prepare(&sql)?;
                statement.exists(rusqlite::params_from_iter(bindings.iter()))?
            };
            if shared {
                return Ok(Some(TurnContentDeletionConflict::SharedContent));
            }
        }
    }

    // Replay chains through this turn must not be rewritten.
    if exists(
        tx,
        "SELECT 1 FROM turn_records WHERE agent_id = ?1 AND replay_source_turn_id = ?2",
        &[agent_id, &target.turn_id],
    )? {
        return Ok(Some(TurnContentDeletionConflict::SharedContent));
    }

    // Sealed input-evidence eligibility with mandatory membership.
    for (family, table) in INPUT_EVIDENCE_FAMILIES {
        let ids = evidence.ids_for(table);
        if ids.is_empty() {
            continue;
        }
        let mut bindings = vec![family.to_owned()];
        bindings.extend(ids);
        let sql = format!(
            "SELECT 1 FROM input_evidence_eligibility
              WHERE mandatory = 1 AND family = ? AND record_id IN ({})",
            placeholders(bindings.len() - 1)
        );
        let sealed = {
            let mut statement = tx.prepare(&sql)?;
            statement.exists(rusqlite::params_from_iter(bindings.iter()))?
        };
        if sealed {
            return Ok(Some(TurnContentDeletionConflict::SealedReference));
        }
    }
    // The turn itself may be sealed evidence.
    if exists(
        tx,
        "SELECT 1 FROM input_evidence_eligibility
          WHERE mandatory = 1 AND family = 'turn' AND record_id = ?1",
        &[&target.turn_id],
    )? {
        return Ok(Some(TurnContentDeletionConflict::SealedReference));
    }
    Ok(None)
}

fn exists(tx: &Transaction<'_>, sql: &str, bindings: &[&str]) -> Result<bool> {
    let mut statement = tx.prepare(sql)?;
    Ok(statement.exists(rusqlite::params_from_iter(bindings.iter()))?)
}

fn tool_execution_payloads(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
) -> Result<Vec<Value>> {
    let mut statement = tx
        .prepare("SELECT payload_json FROM tool_executions WHERE agent_id = ?1 AND turn_id = ?2")?;
    let payloads = statement
        .query_map(params![agent_id, target.turn_id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    Ok(payloads
        .into_iter()
        .map(|payload| serde_json::from_str(&payload).unwrap_or(Value::Null))
        .collect())
}

/// Managed artifact paths referenced by removed tool executions. Only
/// `tool-artifacts/` and `media/` prefixed paths under the agent home
/// qualify; other strings are never treated as deletable files.
fn managed_artifact_paths(tool_payloads: &[Value]) -> Vec<String> {
    let mut paths = std::collections::BTreeSet::new();
    for payload in tool_payloads {
        collect_artifact_paths(payload, &mut paths);
    }
    paths.into_iter().collect()
}

fn collect_artifact_paths(value: &Value, paths: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if key == "path" {
                    if let Value::String(path) = item {
                        if ARTIFACT_PATH_PREFIXES
                            .iter()
                            .any(|prefix| path.starts_with(prefix))
                            && !path.contains("..")
                        {
                            paths.insert(path.clone());
                            continue;
                        }
                    }
                    collect_artifact_paths(item, paths);
                } else {
                    collect_artifact_paths(item, paths);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_artifact_paths(item, paths);
            }
        }
        _ => {}
    }
}

fn remove_exclusive_evidence(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
    evidence: &TurnEvidence,
    tool_payloads: &[Value],
    now: DateTime<Utc>,
) -> Result<usize> {
    let mut removed = 0usize;
    for table in EVIDENCE_TABLES {
        removed += tx.execute(
            &format!("DELETE FROM {table} WHERE agent_id = ?1 AND turn_id = ?2"),
            params![agent_id, target.turn_id],
        )?;
    }
    tx.execute(
        "DELETE FROM conversation_source_revisions WHERE agent_id = ?1 AND turn_id = ?2",
        params![agent_id, target.turn_id],
    )?;
    tx.execute(
        "DELETE FROM conversation_input_assignments WHERE agent_id = ?1 AND turn_id = ?2",
        params![agent_id, target.turn_id],
    )?;

    let mut changes = Vec::new();
    for (table, id) in &evidence.rows {
        let (source_kind, source_ref) = match *table {
            "messages" => ("message", format!("message:{id}")),
            "transcript_entries" => ("transcript", format!("transcript:{id}")),
            "briefs" => ("brief", format!("brief:{id}")),
            _ => {
                // Tool/model/delivery rows are indexed through command
                // receipt refs, not raw evidence ids; skip them here.
                continue;
            }
        };
        changes.push(RuntimeIndexChange {
            agent_id: agent_id.to_owned(),
            source_kind: source_kind.to_owned(),
            source_id: id.clone(),
            source_ref,
            operation: RuntimeIndexOperation::Delete,
            source_updated_at: Some(now),
            reason: "turn_content_deleted".to_owned(),
        });
    }
    // Command receipts are indexed per tool execution with a derived source
    // ref; emit matching deletes so FTS documents disappear.
    for payload in tool_payloads {
        let Some(id) = payload.get("id").and_then(Value::as_str) else {
            continue;
        };
        let tool_name = payload.get("tool_name").and_then(Value::as_str);
        let input = payload.get("input");
        match tool_name {
            Some("exec_command") if input.is_some_and(has_cmd_value) => {
                let source_ref = command_receipt_source_ref(id, None);
                changes.push(receipt_delete(agent_id, source_ref, now));
            }
            Some("exec_command_batch") => {
                if let Some(items) = input.and_then(Value::as_array) {
                    for (offset, item) in items.iter().enumerate() {
                        if has_cmd_value(item) {
                            let source_ref = command_receipt_source_ref(id, Some(offset + 1));
                            changes.push(receipt_delete(agent_id, source_ref, now));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    insert_runtime_index_changes_tx(tx, &changes)?;
    Ok(removed)
}

fn has_cmd_value(input: &Value) -> bool {
    input.get("cmd").and_then(Value::as_str).is_some()
}

fn receipt_delete(agent_id: &str, source_ref: String, now: DateTime<Utc>) -> RuntimeIndexChange {
    RuntimeIndexChange {
        agent_id: agent_id.to_owned(),
        source_kind: "tool_command_receipt".to_owned(),
        source_id: source_ref.clone(),
        source_ref,
        operation: RuntimeIndexOperation::Delete,
        source_updated_at: Some(now),
        reason: "turn_content_deleted".to_owned(),
    }
}

/// Erase body copies held in retained rows: settled execution attempts,
/// settled task-result settlements, and audit projections.
fn scrub_retained_payloads(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
) -> Result<()> {
    tx.execute(
        "UPDATE execution_protocol_attempts
            SET payload_json = json_set(
                  json_set(payload_json, '$.input_projection.rendered', ''),
                  '$.input_projection.rendered_bytes', 0)
          WHERE agent_id = ?1
            AND json_extract(payload_json, '$.turn_id') = ?2",
        params![agent_id, target.turn_id],
    )?;
    tx.execute(
        "UPDATE task_result_settlements
            SET payload_json = json_set(
                  json_set(payload_json, '$.source_projection.rendered', ''),
                  '$.reply_projection.rendered', '')
          WHERE agent_id = ?1 AND parent_turn_id = ?2 AND state = 'settled'",
        params![agent_id, target.turn_id],
    )?;
    // Audit projections: strip body-bearing fields for this turn's events so
    // replay cannot leak content; diagnostics keep kind, ids, and timestamps.
    tx.execute(
        "UPDATE audit_events
            SET data_json = json_remove(data_json,
                  '$.data.input', '$.data.text_preview', '$.data.last_assistant_preview')
          WHERE agent_id = ?1
            AND kind = 'tool_executed'
            AND (json_extract(data_json, '$.data.turn_id') = ?2
                 OR json_extract(data_json, '$.turn_id') = ?2)",
        params![agent_id, target.turn_id],
    )?;
    tx.execute(
        "UPDATE audit_events
            SET data_json = json_remove(data_json,
                  '$.data.text_preview', '$.data.last_assistant_preview')
          WHERE agent_id = ?1
            AND kind IN ('operator_interjection_admitted',
                         'text_only_round_observed',
                         'turn_local_checkpoint_recorded',
                         'lineage_retry_exhausted')
            AND (json_extract(data_json, '$.data.turn_id') = ?2
                 OR json_extract(data_json, '$.turn_id') = ?2)",
        params![agent_id, target.turn_id],
    )?;
    Ok(())
}

fn invalidate_episodes(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
    now: DateTime<Utc>,
) -> Result<usize> {
    // Match by source membership AND turn-index interval: hard-capped and
    // fallback episodes may omit source turn ids.
    let mut statement = tx.prepare(
        "SELECT episode_id FROM context_episode_anchors
          WHERE agent_id = ?1
            AND (
              EXISTS (
                SELECT 1 FROM json_each(payload_json, '$.source_turn_ids') AS s
                 WHERE s.value = ?2
              )
              OR (start_turn_index <= ?3 AND ?3 <= end_turn_index)
            )",
    )?;
    let episode_ids = statement
        .query_map(
            params![
                agent_id,
                target.turn_id,
                i64::try_from(target.turn_index).unwrap_or(0)
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let mut changes = Vec::new();
    for episode_id in &episode_ids {
        tx.execute(
            "DELETE FROM context_episode_anchors WHERE episode_id = ?1",
            params![episode_id],
        )?;
        changes.push(RuntimeIndexChange {
            agent_id: agent_id.to_owned(),
            source_kind: "context_episode".to_owned(),
            source_id: episode_id.clone(),
            source_ref: format!("episode:{episode_id}"),
            operation: RuntimeIndexOperation::Delete,
            source_updated_at: Some(now),
            reason: "turn_content_deleted".to_owned(),
        });
    }
    insert_runtime_index_changes_tx(tx, &changes)?;
    Ok(episode_ids.len())
}

fn update_agent_working_memory(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
) -> Result<()> {
    let epoch = tx
        .query_row(
            "SELECT COALESCE(json_extract(payload_json,
                     '$.working_memory.compression_epoch'), 0)
             FROM agent_states WHERE agent_id = ?1",
            params![agent_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    tx.execute(
        "UPDATE agent_states
            SET payload_json = json_set(payload_json,
                  '$.working_memory.compression_epoch', ?2)
          WHERE agent_id = ?1",
        params![agent_id, epoch + 1],
    )?;
    let builder_touches = tx
        .query_row(
            "SELECT EXISTS (
               SELECT 1 FROM json_each(payload_json,
                       '$.working_memory.active_episode_builder.source_turn_ids') AS s
                WHERE s.value = ?2)
             FROM agent_states WHERE agent_id = ?1",
            params![agent_id, target.turn_id],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false);
    if builder_touches {
        tx.execute(
            "UPDATE agent_states
                SET payload_json = json_remove(payload_json,
                      '$.working_memory.active_episode_builder')
              WHERE agent_id = ?1",
            params![agent_id],
        )?;
    }
    Ok(())
}

/// One queued asynchronous artifact unlink. Paths are agent-home-relative and
/// confined to managed prefixes by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingArtifactDeletion {
    pub agent_id: String,
    pub relative_path: String,
}

/// Lists queued artifact deletions for the async unlink worker. Crashes and
/// restarts resume from this queue.
pub fn pending_artifact_deletions(db: &RuntimeDb) -> Result<Vec<PendingArtifactDeletion>> {
    db.with_read(|connection| {
        let mut statement =
            connection.prepare("SELECT agent_id, relative_path FROM pending_artifact_deletions")?;
        let rows = statement
            .query_map([], |row| {
                Ok(PendingArtifactDeletion {
                    agent_id: row.get(0)?,
                    relative_path: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

/// Removes one queue row after the unlink succeeded (or the file is already
/// gone); failed unlinks keep their row and retry on the next pass.
pub fn remove_pending_artifact_deletion(
    db: &RuntimeDb,
    agent_id: &str,
    relative_path: &str,
) -> Result<()> {
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM pending_artifact_deletions
              WHERE agent_id = ?1 AND relative_path = ?2",
            params![agent_id, relative_path],
        )?;
        Ok(())
    })
}

/// True when the pending queue currently fences this agent-home-relative
/// path. Reads block from deletion-commit time, not from unlink time.
pub fn pending_artifact_deletion_exists(db: &RuntimeDb, relative_path: &str) -> Result<bool> {
    db.with_read(|connection| {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_artifact_deletions
              WHERE relative_path = ?1)",
            params![relative_path],
            |row| row.get(0),
        )?;
        Ok(exists)
    })
}

fn mark_input_evidence_pruned(
    tx: &Transaction<'_>,
    agent_id: &str,
    target: &TargetTurn,
    evidence: &TurnEvidence,
) -> Result<()> {
    for (family, table) in INPUT_EVIDENCE_FAMILIES {
        let ids = evidence.ids_for(table);
        if ids.is_empty() {
            continue;
        }
        let mut bindings = vec![agent_id.to_owned(), family.to_owned()];
        bindings.extend(ids);
        let sql = format!(
            "UPDATE input_evidence_current
                SET availability = 'pruned', content_digest = NULL
              WHERE agent_id = ? AND family = ? AND record_id IN ({})",
            placeholders(bindings.len() - 2)
        );
        tx.execute(&sql, rusqlite::params_from_iter(bindings.iter()))?;
    }
    tx.execute(
        "UPDATE input_evidence_current
            SET availability = 'pruned', content_digest = NULL
          WHERE agent_id = ?1 AND family = 'turn' AND record_id = ?2",
        params![agent_id, target.turn_id],
    )?;
    Ok(())
}

fn enqueue_artifact_deletions(
    tx: &Transaction<'_>,
    agent_id: &str,
    artifact_paths: &[String],
    now: DateTime<Utc>,
) -> Result<()> {
    if artifact_paths.is_empty() {
        return Ok(());
    }
    let deleted_at = holon_storage::codec::timestamp(now);
    for path in artifact_paths {
        tx.execute(
            "INSERT INTO pending_artifact_deletions (agent_id, relative_path, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(agent_id, relative_path) DO NOTHING",
            params![agent_id, path, deleted_at],
        )?;
    }
    Ok(())
}

fn erased_payload_json(record: &TurnRecord) -> Result<String> {
    let mut erased = record.clone();
    erased.erase_conversation_content();
    Ok(serde_json::to_string(&erased)?)
}

fn append_deletion_audit_event(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    deleted_at: &str,
) -> Result<()> {
    let event = holon_domain::AuditEvent::legacy(
        "turn_content_deleted",
        serde_json::json!({
            "agent_id": agent_id,
            "turn_id": turn_id,
            "content_deleted_at": deleted_at,
        }),
    );
    tx.execute(
        "INSERT INTO audit_events (audit_event_id, agent_id, kind, created_at, data_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.id,
            agent_id,
            "turn_content_deleted",
            event.created_at.to_rfc3339(),
            serde_json::to_string(&event)?,
        ],
    )?;
    Ok(())
}
