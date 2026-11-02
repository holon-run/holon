//! Evidence insertion helpers and evidence query types.

use anyhow::Context;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;

use crate::runtime_db::conversation::{
    assigned_turn_id_tx, bump_source_revision_tx, bump_turn_revision_tx, settle_turn_result_tx,
    SOURCE_ASSISTANT, SOURCE_ERROR, SOURCE_OPERATOR, SOURCE_TOOL,
};
use crate::runtime_db::index_outbox::RuntimeIndexChange;
use crate::runtime_db::EVIDENCE_PREVIEW_LIMIT;
use crate::types::AgentRegistryStatus;
use crate::types::{
    AgentDeletionJob, AgentIdentityRecord, AgentKind, AgentState, AuditEvent, BriefRecord,
    DeliverySummaryRecord, ExecutionRootEntry, MessageEnvelope, ToolExecutionRecord,
    TranscriptEntry, WorkspaceEntry, WorkspaceOccupancyRecord,
};

pub(crate) const AUDIT_EVENT_SEQUENCE_DOMAIN: &str = "audit_event";
pub(crate) const MESSAGE_SEQUENCE_DOMAIN: &str = "message";
pub(crate) const TRANSCRIPT_SEQUENCE_DOMAIN: &str = "transcript";

#[cfg(test)]
#[path = "evidence/audit_sql_budget_tests.rs"]
mod audit_sql_budget_tests;

#[cfg(test)]
mod registry_incarnation_budget_tests;

#[cfg(test)]
mod registry_post_write_budget_tests;

#[cfg(test)]
mod registry_enum_tests;

#[cfg(test)]
mod registry_name_key_budget_tests;

#[cfg(test)]
mod registry_timestamp_budget_tests;

#[cfg(test)]
mod registry_clock_budget_tests;

pub(crate) fn agent_sequence_scope(agent_id: &str) -> String {
    format!("agent:{agent_id}")
}

pub(crate) fn audit_event_sequence_scope(agent_id: Option<&str>) -> String {
    agent_id.map_or_else(|| "host".to_string(), agent_sequence_scope)
}

fn audit_write_sequence_scope(agent_id: Option<&str>) -> Result<Cow<'static, str>> {
    let Some(agent_id) = agent_id else {
        return Ok(Cow::Borrowed("host"));
    };
    crate::diagnostics::resource_io::charge_owned_metadata(
        ("agent:".len() + agent_id.len() + std::mem::size_of::<String>()) as u64,
        1,
    )?;
    Ok(Cow::Owned(agent_sequence_scope(agent_id)))
}

pub(crate) fn allocate_runtime_sequence_tx(
    tx: &Transaction<'_>,
    domain: &str,
    scope_key: &str,
) -> Result<u64> {
    let value = tx.query_row(
        "INSERT INTO runtime_sequences(domain, scope_key, last_value)
         VALUES (?1, ?2, 1)
         ON CONFLICT(domain, scope_key) DO UPDATE SET
           last_value = runtime_sequences.last_value + 1
         RETURNING last_value",
        params![domain, scope_key],
        |row| row.get::<_, i64>(0),
    )?;
    u64::try_from(value).context("runtime sequence is negative")
}

pub(crate) fn observe_runtime_sequence_tx(
    tx: &Transaction<'_>,
    domain: &str,
    scope_key: &str,
    value: u64,
) -> Result<()> {
    let value = i64::try_from(value).context("runtime sequence exceeds SQLite integer range")?;
    tx.execute(
        "INSERT INTO runtime_sequences(domain, scope_key, last_value)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(domain, scope_key) DO UPDATE SET
           last_value = MAX(runtime_sequences.last_value, excluded.last_value)",
        params![domain, scope_key, value],
    )?;
    Ok(())
}

/// Evidence kind for table routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    Message,
    TranscriptEntry,
    ToolExecution,
    ModelRequest,
    ModelResponse,
    Brief,
    DeliverySummary,
    ArtifactMetadata,
}

impl EvidenceKind {
    pub(crate) fn table_name(self) -> &'static str {
        match self {
            Self::Message => "messages",
            Self::TranscriptEntry => "transcript_entries",
            Self::ToolExecution => "tool_executions",
            Self::ModelRequest => "model_requests",
            Self::ModelResponse => "model_responses",
            Self::Brief => "briefs",
            Self::DeliverySummary => "delivery_summaries",
            Self::ArtifactMetadata => "artifact_metadata",
        }
    }
}

pub struct EvidenceQuery<'a> {
    pub agent_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub work_item_id: Option<&'a str>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    pub evidence_id: String,
    pub agent_id: String,
    pub turn_id: Option<String>,
    pub message_id: Option<String>,
    pub task_id: Option<String>,
    pub work_item_id: Option<String>,
    pub created_at: String,
    pub preview: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvidencePayloadRow {
    pub payload_json: String,
}

/// Canonical message-to-turn link; replay may keep the source Turn as owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedTurnInputAssignment {
    pub agent_id: String,
    pub message_id: String,
    pub turn_id: String,
    pub revision: u64,
    pub assigned_at: DateTime<Utc>,
}

/// Exact final source projection read in the canonical writer transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedConversationSourceRevision {
    pub agent_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub turn_id: Option<String>,
    pub revision: u64,
    pub activity_seq: Option<u64>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedConversationTurnRevision {
    pub agent_id: String,
    pub turn_id: String,
    pub revision: super::conversation::ConversationTurnRevision,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommittedTurnRecord {
    pub record: crate::types::TurnRecord,
    pub input_assignments: Vec<CommittedTurnInputAssignment>,
    pub revision: super::conversation::ConversationTurnRevision,
    /// Includes unchanged input sources so replay can install exact facts idempotently.
    pub affected_source_revisions: Vec<CommittedConversationSourceRevision>,
    /// Requested Turn plus actual input assignment targets, each once by ID.
    pub affected_turn_revisions: Vec<CommittedConversationTurnRevision>,
}

pub(crate) fn committed_evidence_record_tx<T: serde::de::DeserializeOwned>(
    tx: &Transaction<'_>,
    kind: EvidenceKind,
    agent_id: &str,
    id: &str,
) -> Result<T> {
    let payload: String = tx
        .query_row(
            &format!(
                "SELECT payload_json FROM {} WHERE agent_id = ?1 AND evidence_id = ?2",
                kind.table_name()
            ),
            params![agent_id, id],
            |row| row.get(0),
        )
        .with_context(|| {
            format!(
                "reading committed {} {id} for agent {agent_id}",
                kind.table_name()
            )
        })?;
    serde_json::from_str(&payload).context("decoding committed evidence")
}

pub(crate) fn committed_message_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    id: &str,
) -> Result<MessageEnvelope> {
    let record: MessageEnvelope =
        committed_evidence_record_tx(tx, EvidenceKind::Message, agent_id, id)?;
    let sequence: Option<i64> = tx.query_row(
        "SELECT message_seq FROM messages WHERE agent_id = ?1 AND evidence_id = ?2",
        params![agent_id, id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        record.id == id
            && record.agent_id == agent_id
            && record.message_seq == sequence.map(u64::try_from).transpose()?,
        "committed message payload/sequence projection mismatch for {id}"
    );
    Ok(record)
}

pub(crate) fn committed_transcript_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    id: &str,
) -> Result<TranscriptEntry> {
    let record: TranscriptEntry =
        committed_evidence_record_tx(tx, EvidenceKind::TranscriptEntry, agent_id, id)?;
    let sequence: Option<i64> = tx.query_row(
        "SELECT transcript_seq FROM transcript_entries WHERE agent_id = ?1 AND evidence_id = ?2",
        params![agent_id, id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        record.id == id
            && record.agent_id == agent_id
            && record.transcript_seq == sequence.map(u64::try_from).transpose()?,
        "committed transcript payload/sequence projection mismatch for {id}"
    );
    Ok(record)
}

pub(crate) fn committed_tool_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    id: &str,
) -> Result<ToolExecutionRecord> {
    let record: ToolExecutionRecord =
        committed_evidence_record_tx(tx, EvidenceKind::ToolExecution, agent_id, id)?;
    anyhow::ensure!(
        record.id == id && record.agent_id == agent_id,
        "committed tool scope mismatch for {id}"
    );
    Ok(record)
}

pub(crate) fn committed_brief_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    id: &str,
) -> Result<BriefRecord> {
    let record: BriefRecord = committed_evidence_record_tx(tx, EvidenceKind::Brief, agent_id, id)?;
    let event_seq: Option<i64> = tx.query_row(
        "SELECT created_event_seq FROM briefs WHERE agent_id = ?1 AND evidence_id = ?2",
        params![agent_id, id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        record.id == id
            && record.agent_id == agent_id
            && record.created_event_seq == event_seq.map(u64::try_from).transpose()?,
        "committed brief payload/event projection mismatch for {id}"
    );
    Ok(record)
}

pub(crate) struct EvidenceInsert<'a> {
    table: &'static str,
    evidence_id: &'a str,
    agent_id: &'a str,
    turn_id: Option<&'a str>,
    message_id: Option<&'a str>,
    task_id: Option<&'a str>,
    work_item_id: Option<&'a str>,
    created_at: DateTime<Utc>,
    kind: String,
    preview: Option<String>,
    payload_json: String,
}

pub(crate) fn insert_evidence_tx(tx: &Transaction<'_>, evidence: EvidenceInsert<'_>) -> Result<()> {
    if evidence.table == "artifact_metadata" {
        super::reclamation::ensure_work_admission_tx(tx, evidence.agent_id)?;
    }
    let existing_payload = tx
        .query_row(
            &format!(
                "SELECT payload_json FROM {} WHERE evidence_id = ?1",
                evidence.table
            ),
            [evidence.evidence_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let changed = existing_payload.as_deref() != Some(evidence.payload_json.as_str());
    let content_hash = content_hash(&evidence.payload_json);
    let sql = format!(
        "INSERT INTO {} (
            evidence_id, agent_id, turn_id, message_id, task_id, work_item_id,
            created_at, kind, content_ref, content_hash, preview, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(evidence_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            turn_id = excluded.turn_id,
            message_id = excluded.message_id,
            task_id = excluded.task_id,
            work_item_id = excluded.work_item_id,
            created_at = excluded.created_at,
            kind = excluded.kind,
            content_ref = excluded.content_ref,
            content_hash = excluded.content_hash,
            preview = excluded.preview,
            payload_json = excluded.payload_json",
        evidence.table
    );
    tx.execute(
        &sql,
        params![
            evidence.evidence_id,
            evidence.agent_id,
            evidence.turn_id,
            evidence.message_id,
            evidence.task_id,
            evidence.work_item_id,
            timestamp(evidence.created_at),
            evidence.kind,
            Option::<String>::None,
            content_hash,
            evidence.preview,
            evidence.payload_json,
        ],
    )?;
    if changed {
        match evidence.table {
            "tool_executions" => {
                let _ = bump_source_revision_tx(
                    tx,
                    SOURCE_TOOL,
                    evidence.evidence_id,
                    evidence.agent_id,
                    evidence.turn_id,
                    evidence.created_at,
                )?;
                if let Some(turn_id) = evidence.turn_id {
                    let _ = bump_turn_revision_tx(
                        tx,
                        evidence.agent_id,
                        turn_id,
                        false,
                        true,
                        evidence.created_at,
                    )?;
                }
            }
            "delivery_summaries" => {
                if let Some(turn_id) = evidence.turn_id {
                    let _ =
                        settle_turn_result_tx(tx, evidence.agent_id, turn_id, evidence.created_at)?;
                }
            }
            "briefs" => {
                if let Some(turn_id) = evidence.turn_id {
                    let _ = bump_turn_revision_tx(
                        tx,
                        evidence.agent_id,
                        turn_id,
                        true,
                        false,
                        evidence.created_at,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn insert_message_evidence_tx(
    tx: &Transaction<'_>,
    message: &MessageEnvelope,
) -> Result<()> {
    upsert_message_tx(tx, message)
}

pub(crate) fn append_message_tx(
    tx: &Transaction<'_>,
    message: &MessageEnvelope,
) -> Result<(MessageEnvelope, bool)> {
    if let Some(payload) = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE evidence_id = ?1",
            [&message.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok((serde_json::from_str(&payload)?, false));
    }
    let mut message = message.clone();
    message.message_seq = Some(allocate_runtime_sequence_tx(
        tx,
        MESSAGE_SEQUENCE_DOMAIN,
        &agent_sequence_scope(&message.agent_id),
    )?);
    upsert_message_tx(tx, &message)?;
    Ok((
        committed_message_tx(tx, &message.agent_id, &message.id)?,
        true,
    ))
}

pub(crate) fn insert_transcript_evidence_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<()> {
    upsert_transcript_entry_tx(tx, entry)
}

pub(crate) fn append_transcript_entry_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<(TranscriptEntry, bool)> {
    if let Some(payload) = tx
        .query_row(
            "SELECT payload_json FROM transcript_entries WHERE evidence_id = ?1",
            [&entry.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok((serde_json::from_str(&payload)?, false));
    }
    let mut entry = entry.clone();
    entry.transcript_seq = Some(allocate_runtime_sequence_tx(
        tx,
        TRANSCRIPT_SEQUENCE_DOMAIN,
        &agent_sequence_scope(&entry.agent_id),
    )?);
    upsert_transcript_entry_tx(tx, &entry)?;
    Ok((
        committed_transcript_tx(tx, &entry.agent_id, &entry.id)?,
        true,
    ))
}

pub(crate) fn upsert_agent_state_tx(tx: &Transaction<'_>, record: &AgentState) -> Result<()> {
    upsert_agent_state_at_tx(tx, record, Utc::now())
}

pub(crate) fn upsert_agent_state_at_tx(
    tx: &Transaction<'_>,
    record: &AgentState,
    updated_at: DateTime<Utc>,
) -> Result<()> {
    if record.current_run_id.is_some() || record.status == crate::types::AgentStatus::AwakeRunning {
        super::reclamation::ensure_work_admission_tx(tx, &record.id)?;
    }
    crate::diagnostics::resource_io::admit_serialized_value(record)?;
    let payload_json = serde_json::to_string(record)?;
    let status = match record.status {
        crate::types::AgentStatus::Booting => "booting",
        crate::types::AgentStatus::AwakeIdle => "awake_idle",
        crate::types::AgentStatus::AwakeRunning => "awake_running",
        crate::types::AgentStatus::AwaitingTask => "awaiting_task",
        crate::types::AgentStatus::Asleep => "asleep",
        crate::types::AgentStatus::Stopped => "stopped",
    };
    crate::diagnostics::resource_io::admit_serialized_value(&updated_at)?;
    let now = timestamp(updated_at);
    tx.execute(
        "INSERT INTO agent_states (
            agent_id, status, turn_index, current_run_id, current_work_item_id,
            active_workspace_id, updated_at, payload_json, control_revision
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)
         ON CONFLICT(agent_id) DO UPDATE SET
            status = excluded.status,
            turn_index = excluded.turn_index,
            current_run_id = excluded.current_run_id,
            current_work_item_id = excluded.current_work_item_id,
            active_workspace_id = excluded.active_workspace_id,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json,
            control_revision = agent_states.control_revision + 1
         WHERE excluded.turn_index >= agent_states.turn_index",
        params![
            record.id,
            status,
            record.turn_index as i64,
            record.current_run_id,
            record.current_work_item_id,
            record
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_id.as_str()),
            now,
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_workspace_entry_tx(
    tx: &Transaction<'_>,
    record: &WorkspaceEntry,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT INTO workspace_entries (
            workspace_id, workspace_alias, workspace_kind, owner_agent_id,
            workspace_anchor, repo_name, created_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(workspace_id) DO UPDATE SET
            workspace_alias = excluded.workspace_alias,
            workspace_kind = excluded.workspace_kind,
            owner_agent_id = excluded.owner_agent_id,
            workspace_anchor = excluded.workspace_anchor,
            repo_name = excluded.repo_name,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= workspace_entries.updated_at",
        params![
            record.workspace_id,
            record.workspace_alias,
            record.workspace_kind,
            record.owner_agent_id,
            record.workspace_anchor.display().to_string(),
            record.repo_name,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_workspace_occupancy_tx(
    tx: &Transaction<'_>,
    record: &WorkspaceOccupancyRecord,
) -> Result<()> {
    if record.released_at.is_none() {
        super::reclamation::ensure_work_admission_tx(tx, &record.holder_agent_id)?;
        super::reclamation::ensure_root_admission_tx(tx, &record.execution_root_id)?;
    }
    let payload_json = serde_json::to_string(record)?;
    let access_mode = enum_string(&record.access_mode)?;
    tx.execute(
        "INSERT INTO workspace_occupancies (
            occupancy_id, execution_root_id, workspace_id, holder_agent_id,
            access_mode, acquired_at, released_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(occupancy_id) DO UPDATE SET
            execution_root_id = excluded.execution_root_id,
            workspace_id = excluded.workspace_id,
            holder_agent_id = excluded.holder_agent_id,
            access_mode = excluded.access_mode,
            acquired_at = excluded.acquired_at,
            released_at = excluded.released_at,
            payload_json = excluded.payload_json
         WHERE COALESCE(excluded.released_at, '') >= COALESCE(workspace_occupancies.released_at, '')",
        params![
            record.occupancy_id,
            record.execution_root_id,
            record.workspace_id,
            record.holder_agent_id,
            access_mode,
            timestamp(record.acquired_at),
            record.released_at.map(timestamp),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_execution_root_entry_tx(
    tx: &Transaction<'_>,
    record: &ExecutionRootEntry,
) -> Result<()> {
    if record.removed_at.is_none() {
        super::reclamation::ensure_root_admission_tx(tx, &record.execution_root_id)?;
        if let Some(worktree) = &record.worktree {
            for agent_id in worktree
                .authorized_agent_ids
                .iter()
                .chain(worktree.registered_by_agent_id.iter())
            {
                super::reclamation::ensure_work_admission_tx(tx, agent_id)?;
            }
        }
    }
    let existing_removed_at = tx
        .query_row(
            "SELECT removed_at FROM execution_root_entries
             WHERE execution_root_id = ?1",
            [&record.execution_root_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .with_context(|| format!("parsing execution root removed_at timestamp: {value}"))
                .map(|value| value.with_timezone(&Utc))
        })
        .transpose()?;

    // Tombstones are monotonic. A normal re-registration may update metadata,
    // but it must not implicitly reactivate a removed execution root.
    let mut effective_record = record.clone();
    effective_record.removed_at = existing_removed_at.or(record.removed_at);
    let payload_json = serde_json::to_string(&effective_record)?;
    let root_kind = enum_string(&effective_record.root_kind)?;
    tx.execute(
        "INSERT INTO execution_root_entries (
            execution_root_id, workspace_id, filesystem_path, root_kind,
            created_at, removed_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(execution_root_id) DO UPDATE SET
            workspace_id = excluded.workspace_id,
            filesystem_path = excluded.filesystem_path,
            root_kind = excluded.root_kind,
            created_at = excluded.created_at,
            removed_at = excluded.removed_at,
            payload_json = excluded.payload_json",
        params![
            effective_record.execution_root_id,
            effective_record.workspace_id,
            effective_record.filesystem_path.display().to_string(),
            root_kind,
            timestamp(effective_record.created_at),
            effective_record.removed_at.map(timestamp),
            payload_json,
        ],
    )?;
    Ok(())
}

/// Enforces the durable Agent identity reservation invariant inside every
/// registry write: an id retired in this runtime event-log epoch can never
/// regain availability. Deletion only moves the reservation to `retired`;
/// the reservation row itself is never removed.
pub(crate) fn reserve_agent_identity_tx(
    tx: &Transaction<'_>,
    record: &AgentIdentityRecord,
) -> Result<()> {
    let retired: Option<bool> = tx
        .query_row(
            "SELECT reservation_state = 'retired'
             FROM agent_identity_reservations
             WHERE agent_id = ?1",
            [&record.agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let now = Utc::now();
    if retired == Some(true) {
        if record.status != AgentRegistryStatus::Deleted {
            return Err(anyhow!(
                "agent id {} was retired in this runtime event-log epoch; \
                 retired agent ids are never reused",
                record.agent_id
            ));
        }
        // Re-asserting the tombstone keeps the reservation retired; the
        // registry write below remains an idempotent stale-safe no-op.
        return Ok(());
    }
    let now = registry_timestamp(now)?;
    if record.status == AgentRegistryStatus::Deleted {
        tx.execute(
            "INSERT INTO agent_identity_reservations (agent_id, reservation_state, reserved_at, retired_at, source)
             VALUES (?1, 'retired', ?2, ?2, 'agent_registry')
             ON CONFLICT(agent_id) DO UPDATE SET
               reservation_state = 'retired',
               retired_at = excluded.retired_at",
            params![record.agent_id, now],
        )?;
        return Ok(());
    }
    tx.execute(
        "INSERT OR IGNORE INTO agent_identity_reservations (agent_id, reservation_state, reserved_at, retired_at, source)
         VALUES (?1, 'active', ?2, NULL, 'agent_registry')",
        params![record.agent_id, now],
    )?;
    Ok(())
}

/// Re-activates a retired identity reservation. This is the only sanctioned
/// `retired -> active` transition: it runs inside the reincarnation
/// transaction, after the latest deletion job for the agent id has fully
/// completed, and in the same transaction as the fresh Active registry
/// write. Every other registry write path keeps the standing invariant that
/// a retired id never regains availability implicitly.
pub(crate) fn release_agent_identity_reservation_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<()> {
    let now = registry_timestamp(Utc::now())?;
    let released = tx.execute(
        "UPDATE agent_identity_reservations
         SET reservation_state = 'active',
             reserved_at = ?2,
             retired_at = NULL,
             source = 'reincarnation'
         WHERE agent_id = ?1 AND reservation_state = 'retired'",
        params![agent_id, now],
    )?;
    anyhow::ensure!(
        released == 1,
        "agent_reincarnation_rejected: agent {agent_id} identity reservation is not retired"
    );
    Ok(())
}

fn registry_agent_name_key(name: &str) -> Result<String> {
    use unicode_casefold::UnicodeCaseFold;

    let bytes =
        name.case_fold()
            .try_fold(std::mem::size_of::<String>() as u64, |bytes, character| {
                bytes
                    .checked_add(character.len_utf8() as u64)
                    .ok_or_else(|| anyhow!("ResourceExhausted: registry name key"))
            })?;
    crate::diagnostics::resource_io::charge_owned_metadata(bytes, 1)?;
    Ok(crate::types::agent_name_key(name))
}

fn registry_timestamp(value: chrono::DateTime<Utc>) -> Result<String> {
    let year = chrono::Datelike::year(&value);
    let year_bytes = if (0..=9999).contains(&year) {
        4
    } else {
        (year.unsigned_abs().ilog10() as usize + 1).max(4) + 1
    };
    // UTC RFC3339 milliseconds: year plus "-MM-DDTHH:MM:SS.sssZ".
    crate::diagnostics::resource_io::charge_owned_metadata(
        (year_bytes + 20 + std::mem::size_of::<String>()) as u64,
        1,
    )?;
    Ok(timestamp(value))
}

pub(crate) fn upsert_agent_identity_tx(
    tx: &Transaction<'_>,
    record: &AgentIdentityRecord,
) -> Result<()> {
    let previous_incarnation = {
        let mut statement =
            tx.prepare("SELECT payload_json FROM agent_identities WHERE agent_id = ?1")?;
        let mut rows = statement.query([&record.agent_id])?;
        if let Some(row) = rows.next()? {
            let payload = row.get_ref(0)?.as_str()?;
            crate::diagnostics::resource_io::admit_json_payload(payload)?;
            Some(serde_json::from_str::<AgentIdentityRecord>(payload)?.incarnation)
        } else {
            None
        }
    };
    // The reservation guard runs before the registry write so a retired id
    // can never regain availability, not even through a stale replay.
    reserve_agent_identity_tx(tx, record)?;
    let payload_json = serde_json::to_string(record)?;
    let name_key = record
        .name
        .as_deref()
        .map(registry_agent_name_key)
        .transpose()?;
    let kind = match record.kind {
        AgentKind::Default => "default",
        AgentKind::Named => "named",
        AgentKind::Child => "child",
    };
    let visibility = record.visibility.label();
    let ownership = record.ownership.map(|value| value.label());
    let profile_preset = record.profile_preset.map(|value| value.label());
    let status = match record.status {
        AgentRegistryStatus::Active => "active",
        AgentRegistryStatus::Deleting => "deleting",
        AgentRegistryStatus::Deleted => "deleted",
    };
    tx.execute(
        "INSERT INTO agent_identities (
            agent_id, name, name_key, kind, visibility, ownership, profile_preset, status,
            parent_agent_id, lineage_parent_agent_id, delegated_from_task_id,
            created_at, updated_at, archived_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(agent_id) DO UPDATE SET
            name = excluded.name,
            name_key = excluded.name_key,
            kind = excluded.kind,
            visibility = excluded.visibility,
            ownership = excluded.ownership,
            profile_preset = excluded.profile_preset,
            status = excluded.status,
            parent_agent_id = excluded.parent_agent_id,
            lineage_parent_agent_id = excluded.lineage_parent_agent_id,
            delegated_from_task_id = excluded.delegated_from_task_id,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            archived_at = excluded.archived_at,
            payload_json = excluded.payload_json
         WHERE excluded.updated_at >= agent_identities.updated_at",
        params![
            record.agent_id,
            record.name,
            name_key,
            kind,
            visibility,
            ownership,
            profile_preset,
            status,
            record.parent_agent_id,
            record.lineage_parent_agent_id,
            record.delegated_from_task_id,
            registry_timestamp(record.created_at)?,
            registry_timestamp(record.updated_at)?,
            record.deleted_at.map(registry_timestamp).transpose()?,
            payload_json,
        ],
    )?;
    if previous_incarnation != Some(record.incarnation) {
        let matches_record = {
            let mut statement =
                tx.prepare("SELECT payload_json FROM agent_identities WHERE agent_id = ?1")?;
            let mut rows = statement.query([&record.agent_id])?;
            let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let payload = row.get_ref(0)?.as_str()?;
            crate::diagnostics::resource_io::admit_json_payload(payload)?;
            serde_json::from_str::<AgentIdentityRecord>(payload)? == *record
        };
        if matches_record {
            super::episode_snapshot::ensure_fresh_episode_continuity_tx(
                tx,
                record,
                record.updated_at,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn insert_agent_deletion_job_tx(
    tx: &Transaction<'_>,
    job: &AgentDeletionJob,
) -> Result<()> {
    let payload_json = serde_json::to_string(job)?;
    tx.execute(
        "INSERT INTO agent_deletion_jobs (
            deletion_id, agent_id, status, phase, created_at, updated_at,
            completed_at, next_attempt_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            job.deletion_id,
            job.agent_id,
            enum_string(&job.status)?,
            enum_string(&job.phase)?,
            timestamp(job.created_at),
            timestamp(job.updated_at),
            job.completed_at.map(timestamp),
            job.next_attempt_at.map(timestamp),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_message_tx(tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
    if let Some(message_seq) = message.message_seq {
        observe_runtime_sequence_tx(
            tx,
            MESSAGE_SEQUENCE_DOMAIN,
            &agent_sequence_scope(&message.agent_id),
            message_seq,
        )?;
    }
    let payload_json = serde_json::to_string(message)?;
    let existing_payload = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE evidence_id = ?1",
            [&message.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let changed = existing_payload.as_deref() != Some(payload_json.as_str());
    let content_hash = content_hash(&payload_json);
    let kind = enum_string(&message.kind)?;
    let preview = evidence_preview(&message.body);
    tx.execute(
        "INSERT INTO messages (
            evidence_id, agent_id, turn_id, message_id, message_seq, task_id, work_item_id,
            created_at, kind, content_ref, content_hash, preview, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(evidence_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            turn_id = excluded.turn_id,
            message_id = excluded.message_id,
            message_seq = excluded.message_seq,
            task_id = excluded.task_id,
            work_item_id = excluded.work_item_id,
            created_at = excluded.created_at,
            kind = excluded.kind,
            content_ref = excluded.content_ref,
            content_hash = excluded.content_hash,
            preview = excluded.preview,
            payload_json = excluded.payload_json",
        params![
            message.id,
            message.agent_id,
            message.turn_id,
            message.id,
            message.message_seq.map(|seq| seq as i64),
            message.task_id,
            message.work_item_id,
            timestamp(message.created_at),
            kind,
            Option::<String>::None,
            content_hash,
            preview,
            payload_json,
        ],
    )?;
    if changed {
        let assigned_turn_id = assigned_turn_id_tx(tx, &message.agent_id, &message.id)?;
        let _ = bump_source_revision_tx(
            tx,
            SOURCE_OPERATOR,
            &message.id,
            &message.agent_id,
            assigned_turn_id.as_deref(),
            message.created_at,
        )?;
        if let Some(turn_id) = assigned_turn_id.as_deref() {
            let _ = bump_turn_revision_tx(
                tx,
                &message.agent_id,
                turn_id,
                true,
                true,
                message.created_at,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn upsert_transcript_entry_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<()> {
    if let Some(transcript_seq) = entry.transcript_seq {
        observe_runtime_sequence_tx(
            tx,
            TRANSCRIPT_SEQUENCE_DOMAIN,
            &agent_sequence_scope(&entry.agent_id),
            transcript_seq,
        )?;
    }
    let turn_id = entry
        .data
        .get("turn_id")
        .and_then(serde_json::Value::as_str);
    let task_id = entry
        .data
        .get("task_id")
        .and_then(serde_json::Value::as_str);
    let work_item_id = entry
        .data
        .get("work_item_id")
        .and_then(serde_json::Value::as_str);
    let payload_json = serde_json::to_string(entry)?;
    let existing_payload = tx
        .query_row(
            "SELECT payload_json FROM transcript_entries WHERE evidence_id = ?1",
            [&entry.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let changed = existing_payload.as_deref() != Some(payload_json.as_str());
    let content_hash = content_hash(&payload_json);
    let kind = enum_string(&entry.kind)?;
    tx.execute(
        "INSERT INTO transcript_entries (
            evidence_id, agent_id, turn_id, message_id, transcript_seq, task_id, work_item_id,
            created_at, kind, content_ref, content_hash, preview, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(evidence_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            turn_id = excluded.turn_id,
            message_id = excluded.message_id,
            transcript_seq = excluded.transcript_seq,
            task_id = excluded.task_id,
            work_item_id = excluded.work_item_id,
            created_at = excluded.created_at,
            kind = excluded.kind,
            content_ref = excluded.content_ref,
            content_hash = excluded.content_hash,
            preview = excluded.preview,
            payload_json = excluded.payload_json",
        params![
            entry.id,
            entry.agent_id,
            turn_id,
            entry.related_message_id,
            entry.transcript_seq.map(|seq| seq as i64),
            task_id,
            work_item_id,
            timestamp(entry.created_at),
            kind,
            Option::<String>::None,
            content_hash,
            evidence_preview(&entry.data),
            payload_json,
        ],
    )?;
    if changed {
        let source_kind = match entry.kind {
            crate::types::TranscriptEntryKind::AssistantRound
            | crate::types::TranscriptEntryKind::SubagentAssistantRound => Some(SOURCE_ASSISTANT),
            crate::types::TranscriptEntryKind::RuntimeFailure => Some(SOURCE_ERROR),
            _ => None,
        };
        if let Some(source_kind) = source_kind {
            let _ = bump_source_revision_tx(
                tx,
                source_kind,
                &entry.id,
                &entry.agent_id,
                turn_id,
                entry.created_at,
            )?;
        }
        if let Some(turn_id) = turn_id {
            // Transcript kinds also determine the coverage exposed on the
            // summary. Publish a new summary revision with every transcript
            // change so stream and detail reads cannot disagree at one revision.
            let _ =
                bump_turn_revision_tx(tx, &entry.agent_id, turn_id, true, true, entry.created_at)?;
        }
    }
    Ok(())
}

pub(crate) fn insert_runtime_index_changes_tx(
    tx: &Transaction<'_>,
    changes: &[RuntimeIndexChange],
) -> Result<()> {
    insert_runtime_index_changes_with_time_tx(tx, changes, None)
}

pub(crate) fn insert_runtime_index_changes_at_tx(
    tx: &Transaction<'_>,
    changes: &[RuntimeIndexChange],
    created_at: DateTime<Utc>,
) -> Result<()> {
    insert_runtime_index_changes_with_time_tx(tx, changes, Some(created_at))
}

fn insert_runtime_index_changes_with_time_tx(
    tx: &Transaction<'_>,
    changes: &[RuntimeIndexChange],
    created_at: Option<DateTime<Utc>>,
) -> Result<()> {
    let mut agent_ids = Vec::new();
    for change in changes {
        // Outbox inserts and the produced watermark advance below share one
        // transaction, so a committed append always advances its watermark.
        tx.execute(
            "INSERT INTO runtime_index_outbox (
                agent_id, source_kind, source_id, source_ref, operation,
                source_updated_at, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                change.agent_id,
                change.source_kind,
                change.source_id,
                change.source_ref,
                change.operation.as_str(),
                change.source_updated_at.map(timestamp),
                change.reason,
                timestamp(created_at.unwrap_or_else(Utc::now)),
            ],
        )?;
        if !agent_ids.contains(&change.agent_id) {
            agent_ids.push(change.agent_id.clone());
        }
    }
    for agent_id in &agent_ids {
        update_runtime_index_outbox_watermark_at_tx(
            tx,
            agent_id,
            created_at.unwrap_or_else(Utc::now),
        )?;
    }
    Ok(())
}

/// Advance the agent's monotonic produced watermark to the highest appended
/// `change_seq`. Deleting consumed outbox rows never lowers this value, which
/// keeps `produced - applied` meaningful after the outbox is drained.
fn update_runtime_index_outbox_watermark_at_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    updated_at: DateTime<Utc>,
) -> Result<()> {
    let produced: i64 = tx.query_row(
        "SELECT COALESCE(MAX(change_seq), 0) FROM runtime_index_outbox WHERE agent_id = ?1",
        params![agent_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO runtime_index_outbox_watermarks (
            agent_id, produced_change_seq, updated_at
         ) VALUES (?1, ?2, ?3)
         ON CONFLICT(agent_id) DO UPDATE SET
            produced_change_seq = MAX(
                runtime_index_outbox_watermarks.produced_change_seq,
                excluded.produced_change_seq
            ),
            updated_at = excluded.updated_at",
        params![agent_id, produced, timestamp(updated_at)],
    )?;
    Ok(())
}

pub(crate) fn insert_tool_evidence_tx(
    tx: &Transaction<'_>,
    record: &ToolExecutionRecord,
) -> Result<()> {
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::ToolExecution.table_name(),
            evidence_id: &record.id,
            agent_id: &record.agent_id,
            turn_id: record.turn_id.as_deref(),
            message_id: None,
            task_id: None,
            work_item_id: record.work_item_id.as_deref(),
            created_at: record.created_at,
            kind: record.tool_name.clone(),
            preview: Some(truncate_evidence_string(&record.summary)),
            payload_json: full_payload_json(record)?,
        },
    )
}

pub(crate) fn insert_brief_evidence_tx(tx: &Transaction<'_>, brief: &BriefRecord) -> Result<()> {
    if let Some(payload_json) = tx
        .query_row(
            "SELECT payload_json FROM briefs WHERE evidence_id = ?1",
            [&brief.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        let existing: BriefRecord = serde_json::from_str(&payload_json)?;
        anyhow::ensure!(
            existing == *brief,
            "conflicting brief content for evidence_id {}",
            brief.id
        );
        return Ok(());
    }
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::Brief.table_name(),
            evidence_id: &brief.id,
            agent_id: &brief.agent_id,
            turn_id: brief.turn_id.as_deref(),
            message_id: brief.related_message_id.as_deref(),
            task_id: brief.related_task_id.as_deref(),
            work_item_id: brief.work_item_id.as_deref(),
            created_at: brief.created_at,
            kind: enum_string(&brief.kind)?,
            preview: Some(truncate_evidence_string(&brief.text)),
            payload_json: full_payload_json(brief)?,
        },
    )?;
    Ok(())
}

/// Outcome of the single-transition Brief publication: the stored Brief
/// record (with its immutable `created_event_seq` linkage), the committed
/// `brief_created` audit event, and whether this call inserted the event or
/// reused an idempotently identical existing row.
#[derive(Debug, Clone)]
pub struct BriefCreatedCommit {
    pub brief: BriefRecord,
    pub event: AuditEvent,
    pub event_inserted: bool,
}

/// Inserts or links a Brief record against an already-allocated
/// `brief_created` event sequence, inside the caller's transaction. The
/// linkage is immutable: an existing different linkage is a conflict, an
/// absent linkage is filled, and the payload's non-linkage content must
/// match on retry.
pub(crate) fn upsert_brief_with_created_event_seq_tx(
    tx: &Transaction<'_>,
    brief: &BriefRecord,
    event_seq: u64,
) -> Result<BriefRecord> {
    if let Some(payload_json) = tx
        .query_row(
            "SELECT payload_json FROM briefs WHERE evidence_id = ?1",
            [&brief.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        let mut existing: BriefRecord = serde_json::from_str(&payload_json)?;
        let stored_linkage = existing.created_event_seq;
        let mut incoming = brief.clone();
        incoming.created_event_seq = None;
        existing.created_event_seq = None;
        anyhow::ensure!(
            existing == incoming,
            "conflicting brief content for evidence_id {}",
            brief.id
        );
        if stored_linkage == Some(event_seq) {
            existing.created_event_seq = stored_linkage;
            return Ok(existing);
        }
        anyhow::ensure!(
            stored_linkage.is_none(),
            "brief {} already linked to created_event_seq {}, refusing relink to {}",
            brief.id,
            stored_linkage.unwrap_or_default(),
            event_seq
        );
        existing.created_event_seq = Some(event_seq);
        let payload = full_payload_json(&existing)?;
        tx.execute(
            "UPDATE briefs SET payload_json = ?1, created_event_seq = ?2, content_hash = ?4
             WHERE evidence_id = ?3",
            params![
                payload,
                i64::try_from(event_seq).ok(),
                &brief.id,
                content_hash(&payload),
            ],
        )?;
        if let Some(turn_id) = existing.turn_id.as_deref() {
            let _ = bump_turn_revision_tx(
                tx,
                &existing.agent_id,
                turn_id,
                true,
                false,
                existing.created_at,
            )?;
        }
        return Ok(existing);
    }
    let mut linked = brief.clone();
    linked.created_event_seq = Some(event_seq);
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::Brief.table_name(),
            evidence_id: &linked.id,
            agent_id: &linked.agent_id,
            turn_id: linked.turn_id.as_deref(),
            message_id: linked.related_message_id.as_deref(),
            task_id: linked.related_task_id.as_deref(),
            work_item_id: linked.work_item_id.as_deref(),
            created_at: linked.created_at,
            kind: enum_string(&linked.kind)?,
            preview: Some(truncate_evidence_string(&linked.text)),
            payload_json: full_payload_json(&linked)?,
        },
    )?;
    tx.execute(
        "UPDATE briefs SET created_event_seq = ?1 WHERE evidence_id = ?2",
        params![i64::try_from(event_seq).ok(), &linked.id],
    )?;
    Ok(linked)
}

/// Single-transition Brief publication: allocates the audit event sequence,
/// appends the idempotent `brief_created` event, links the Brief record to
/// the committed sequence, and records index changes, all in one SQLite
/// transaction. When the event is observed, the Brief record is already
/// readable.
pub(crate) fn append_brief_with_created_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    brief: &BriefRecord,
    event: &AuditEvent,
    index_changes: &[RuntimeIndexChange],
) -> Result<BriefCreatedCommit> {
    let (event, event_inserted) = append_audit_event_tx(tx, agent_id, event)?;
    let brief = upsert_brief_with_created_event_seq_tx(tx, brief, event.event_seq)?;
    insert_runtime_index_changes_tx(tx, index_changes)?;
    let brief = committed_brief_tx(tx, &brief.agent_id, &brief.id)?;
    Ok(BriefCreatedCommit {
        brief,
        event,
        event_inserted,
    })
}

/// Links transition `brief_evidence` rows against the `brief_created` events
/// committed by the same transition. At most one event may reference a
/// carried Brief; a Brief without its event in this transition stays
/// unlinked rather than blocking the committed transition.
pub(crate) fn link_transition_brief_created_events_tx(
    tx: &Transaction<'_>,
    brief_evidence: &[BriefRecord],
    committed_events: &[super::transitions::AuditEventAck],
) -> Result<()> {
    const BRIEF_CREATED_WIRE_NAME: &str = "brief_created";
    for brief in brief_evidence {
        let matches = committed_events
            .iter()
            .filter(|event| {
                event.kind == BRIEF_CREATED_WIRE_NAME
                    && event
                        .data
                        .get("brief_id")
                        .and_then(|value| value.as_str())
                        .is_some_and(|brief_id| brief_id == brief.id)
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matches.len() <= 1,
            "transition carried {} brief_created events for brief {}",
            matches.len(),
            brief.id
        );
        if let Some(event) = matches.first() {
            upsert_brief_with_created_event_seq_tx(tx, brief, event.event_seq)?;
        }
    }
    Ok(())
}

pub(crate) fn insert_delivery_summary_evidence_tx(
    tx: &Transaction<'_>,
    record: &DeliverySummaryRecord,
) -> Result<()> {
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::DeliverySummary.table_name(),
            evidence_id: &record.id,
            agent_id: &record.agent_id,
            turn_id: record.turn_id.as_deref(),
            message_id: None,
            task_id: None,
            work_item_id: Some(&record.work_item_id),
            created_at: record.created_at,
            kind: "delivery_summary".to_string(),
            preview: Some(truncate_evidence_string(&record.text)),
            payload_json: full_payload_json(record)?,
        },
    )
}

#[cfg(test)]
pub(crate) fn insert_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    append_audit_event_tx(tx, agent_id, event).map(|_| ())
}

pub(crate) fn append_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<(AuditEvent, bool)> {
    if let Some(existing) = audit_event_by_id_tx(tx, agent_id, &event.id)? {
        ensure_same_audit_event_content(&existing, event, "audit_event_id")?;
        return Ok((existing, false));
    }
    crate::diagnostics::resource_io::admit_cloned_value(event)?;
    let mut event = event.clone();
    event.event_log_epoch = event_log_epoch_tx(tx, None)?.into_owned();
    event.event_seq = allocate_runtime_sequence_tx(
        tx,
        AUDIT_EVENT_SEQUENCE_DOMAIN,
        &audit_write_sequence_scope(agent_id)?,
    )?;
    insert_audit_event_with_sequence_tx(tx, agent_id, &event)?;
    Ok((event, true))
}

/// Capture material only at the canonical writer/decoder, not from a later
/// ambient phase or an arbitrary owned event. This carries no Current authority.
pub(crate) fn append_audit_event_ack_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<(super::transitions::AuditEventAck, bool)> {
    let (event, inserted) = append_audit_event_tx(tx, agent_id, event)?;
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?;
    Ok((
        super::transitions::AuditEventAck::with_original_material(event, material)?,
        inserted,
    ))
}

// Historical SQL replay fixtures; runtime writers use append_audit_event_tx.
#[cfg(test)]
pub(crate) fn import_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    let current_epoch = event_log_epoch_tx(
        tx,
        (!event.event_log_epoch.is_empty()).then_some(event.event_log_epoch.as_str()),
    )?;
    crate::diagnostics::resource_io::admit_cloned_value(event)?;
    let mut event = event.clone();
    if let Cow::Owned(current_epoch) = current_epoch {
        event.event_log_epoch = current_epoch;
    }
    if event.event_seq == 0 {
        append_audit_event_tx(tx, agent_id, &event)?;
        return Ok(());
    }
    if let Some(existing) = audit_event_by_id_tx(tx, agent_id, &event.id)? {
        anyhow::ensure!(
            existing.event_seq == event.event_seq,
            "conflicting audit event sequence for audit_event_id {}",
            event.id
        );
        ensure_same_audit_event_identity_content(&existing, &event)?;
        return Ok(());
    }
    let event_seq = i64::try_from(event.event_seq)
        .context("audit event sequence exceeds SQLite integer range")?;
    let mut statement =
        tx.prepare("SELECT data_json FROM audit_events WHERE agent_id IS ?1 AND event_seq = ?2")?;
    let existing = statement
        .query(params![agent_id, event_seq])?
        .next()?
        .map(|row| decode_audit_event_row(row, 0))
        .transpose()?;
    if let Some(existing) = existing {
        ensure_same_audit_event_identity_content(&existing, &event)?;
        return Ok(());
    }
    insert_audit_event_with_sequence_tx(tx, agent_id, &event)?;
    observe_runtime_sequence_tx(
        tx,
        AUDIT_EVENT_SEQUENCE_DOMAIN,
        &audit_write_sequence_scope(agent_id)?,
        event.event_seq,
    )
}

fn audit_event_by_id_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event_id: &str,
) -> Result<Option<AuditEvent>> {
    let mut statement =
        tx.prepare("SELECT agent_id, data_json FROM audit_events WHERE audit_event_id = ?1")?;
    let existing = statement
        .query([event_id])?
        .next()?
        .map(|row| {
            let identity = row.get_ref(0)?;
            let existing_agent_id = match identity {
                rusqlite::types::ValueRef::Null => None,
                value => Some(value.as_str()?),
            };
            anyhow::ensure!(
                existing_agent_id == agent_id,
                "conflicting audit event agent identity for audit_event_id {}",
                event_id
            );
            decode_audit_event_row(row, 1)
        })
        .transpose();
    existing
}

fn decode_audit_event_row(row: &rusqlite::Row<'_>, column: usize) -> Result<AuditEvent> {
    let payload = row.get_ref(column)?.as_str()?;
    crate::diagnostics::resource_io::admit_json_payload(payload)?;
    serde_json::from_str(payload).map_err(Into::into)
}

fn event_log_epoch_tx<'a>(tx: &Transaction<'_>, expected: Option<&'a str>) -> Result<Cow<'a, str>> {
    let mut statement =
        tx.prepare("SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'")?;
    let mut rows = statement.query([])?;
    let row = rows.next()?.context("runtime event-log epoch is missing")?;
    let current = row.get_ref(0)?.as_str()?;
    if let Some(expected) = expected {
        anyhow::ensure!(
            expected == current,
            "audit event event-log epoch {} does not match runtime epoch {}",
            expected,
            current
        );
        return Ok(Cow::Borrowed(expected));
    }
    crate::diagnostics::resource_io::charge_owned_metadata(
        (current.len() + std::mem::size_of::<String>()) as u64,
        1,
    )?;
    Ok(Cow::Owned(current.to_owned()))
}

fn ensure_same_audit_event_content(
    existing: &AuditEvent,
    incoming: &AuditEvent,
    identity: &str,
) -> Result<()> {
    anyhow::ensure!(
        existing.created_at == incoming.created_at
            && existing.kind == incoming.kind
            && existing.contract_version == incoming.contract_version
            && existing.payload_schema == incoming.payload_schema
            && existing.payload_schema_version == incoming.payload_schema_version
            && existing.data == incoming.data,
        "conflicting audit event content for {identity} {}",
        existing.id
    );
    Ok(())
}

#[cfg(test)]
fn ensure_same_audit_event_identity_content(
    existing: &AuditEvent,
    incoming: &AuditEvent,
) -> Result<()> {
    anyhow::ensure!(
        existing.id == incoming.id
            && existing.created_at == incoming.created_at
            && existing.event_log_epoch == incoming.event_log_epoch,
        "conflicting audit event identity ({}, agent sequence {})",
        existing.event_log_epoch,
        existing.event_seq
    );
    ensure_same_audit_event_content(existing, incoming, "event identity")
}

fn insert_audit_event_with_sequence_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    let event_seq = i64::try_from(event.event_seq)
        .context("audit event sequence exceeds SQLite integer range")?;
    crate::diagnostics::resource_io::admit_serialized_value(&event.created_at)?;
    let created_at = timestamp(event.created_at);
    crate::diagnostics::resource_io::admit_serialized_value(event)?;
    let data_json = serde_json::to_string(event)?;
    let inserted = tx.execute(
        "INSERT INTO audit_events (
            audit_event_id, event_seq, agent_id, kind, created_at, data_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![event.id, event_seq, agent_id, event.kind, created_at, data_json,],
    )?;
    anyhow::ensure!(
        inserted == 1,
        "failed to insert audit event {} at sequence {}",
        event.id,
        event.event_seq
    );
    crate::runtime_db::observer_sync::record_appended_event_projection_effect(tx, event)?;
    Ok(())
}

pub(crate) fn push_optional_clause(
    clauses: &mut Vec<String>,
    params: &mut Vec<String>,
    column: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        params.push(value.to_string());
        clauses.push(format!("{column} = ?{}", params.len()));
    }
}

pub(crate) fn full_payload_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(Into::into)
}

pub(crate) fn content_hash(payload_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload_json.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

pub(crate) fn evidence_preview(value: &impl Serialize) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .map(|value| truncate_evidence_string(&value))
}

pub(crate) fn truncate_evidence_string(value: &str) -> String {
    let mut truncated = value.to_string();
    if truncated.len() > EVIDENCE_PREVIEW_LIMIT {
        truncate_string_in_place(&mut truncated, EVIDENCE_PREVIEW_LIMIT);
    }
    truncated
}

pub(crate) fn truncate_string_in_place(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or(0);
    value.truncate(boundary);
}

/// Serialize an enum value to its string representation.
fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("expected enum to serialize as string"))
}

/// Convert a DateTime<Utc> to an RFC 3339 timestamp string.
fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "standalone_evidence_tests.rs"]
mod standalone_evidence_tests;
