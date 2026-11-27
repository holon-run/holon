//! Canonical evidence rows and transaction-scoped writes.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, ToSql, Transaction};
use serde::Serialize;
use sha2::{Digest, Sha256};

use holon_domain::{
    AgentDeletionJob, AuditEvent, BriefRecord, DeliverySummaryRecord, ExecutionRootEntry,
    MessageEnvelope, ToolExecutionRecord, TranscriptEntry, WorkspaceEntry,
    WorkspaceOccupancyRecord,
};

use crate::conversation::{
    assigned_turn_id_tx, RevisionWrites, SOURCE_ASSISTANT, SOURCE_ERROR, SOURCE_MODEL_REQUEST,
    SOURCE_MODEL_SWITCH, SOURCE_OPERATOR, SOURCE_TOOL,
};
use crate::sequences::{
    agent_sequence_scope, allocate_runtime_sequence_tx, observe_runtime_sequence_tx,
};

pub const EVIDENCE_PREVIEW_LIMIT: usize = 2048;
pub const MESSAGE_SEQUENCE_DOMAIN: &str = "message";
pub const TRANSCRIPT_SEQUENCE_DOMAIN: &str = "transcript";

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
    pub fn table_name(self) -> &'static str {
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
    pub revision: crate::conversation::ConversationTurnRevision,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommittedTurnRecord {
    pub record: holon_domain::TurnRecord,
    pub input_assignments: Vec<CommittedTurnInputAssignment>,
    pub revision: crate::conversation::ConversationTurnRevision,
    /// Includes unchanged input sources so replay can install exact facts idempotently.
    pub affected_source_revisions: Vec<CommittedConversationSourceRevision>,
    /// Requested Turn plus actual input assignment targets, each once by ID.
    pub affected_turn_revisions: Vec<CommittedConversationTurnRevision>,
}

pub struct EvidenceInsert<'a> {
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

pub fn committed_evidence_record_tx<T: serde::de::DeserializeOwned>(
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
pub fn committed_message_tx(
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
pub fn committed_transcript_tx(
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
pub fn committed_tool_tx(
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
pub fn committed_brief_tx(tx: &Transaction<'_>, agent_id: &str, id: &str) -> Result<BriefRecord> {
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
fn push_optional_clause(
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
pub fn full_payload_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(Into::into)
}
pub fn content_hash(payload_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload_json.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}
pub fn evidence_preview(value: &impl Serialize) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .map(|value| truncate_evidence_string(&value))
}
pub fn truncate_evidence_string(value: &str) -> String {
    let mut truncated = value.to_string();
    if truncated.len() > EVIDENCE_PREVIEW_LIMIT {
        truncate_string_in_place(&mut truncated, EVIDENCE_PREVIEW_LIMIT);
    }
    truncated
}
pub fn truncate_string_in_place(value: &mut String, max_bytes: usize) {
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
pub fn insert_agent_deletion_job_tx(tx: &Transaction<'_>, job: &AgentDeletionJob) -> Result<()> {
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
pub fn upsert_workspace_entry_tx(tx: &Transaction<'_>, record: &WorkspaceEntry) -> Result<bool> {
    let payload_json = serde_json::to_string(record)?;
    let changed = tx.execute(
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
    Ok(changed != 0)
}
pub fn upsert_workspace_occupancy_tx(
    tx: &Transaction<'_>,
    record: &WorkspaceOccupancyRecord,
) -> Result<()> {
    if record.released_at.is_none() {
        crate::reclamation::ensure_work_admission_tx(tx, &record.holder_agent_id)?;
        crate::reclamation::ensure_root_admission_tx(tx, &record.execution_root_id)?;
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
pub fn upsert_execution_root_entry_tx(
    tx: &Transaction<'_>,
    record: &ExecutionRootEntry,
) -> Result<Option<String>> {
    if record.removed_at.is_none() {
        crate::reclamation::ensure_root_admission_tx(tx, &record.execution_root_id)?;
        if let Some(worktree) = &record.worktree {
            for agent_id in worktree
                .authorized_agent_ids
                .iter()
                .chain(worktree.registered_by_agent_id.iter())
            {
                crate::reclamation::ensure_work_admission_tx(tx, agent_id)?;
            }
        }
    }
    let existing: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT workspace_id, removed_at FROM execution_root_entries
             WHERE execution_root_id = ?1",
            [&record.execution_root_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (previous_workspace, existing_removed_at) = match existing {
        Some((workspace, removed_at)) => (Some(workspace), removed_at),
        None => (None, None),
    };
    let existing_removed_at = existing_removed_at
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
    Ok(previous_workspace)
}

pub struct EvidenceWrites {
    revisions: RevisionWrites,
}

impl EvidenceWrites {
    pub const fn new(revisions: RevisionWrites) -> Self {
        Self { revisions }
    }

    pub fn insert_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        evidence: EvidenceInsert<'_>,
    ) -> Result<()> {
        if evidence.table == "artifact_metadata" {
            crate::reclamation::ensure_work_admission_tx(tx, evidence.agent_id)?;
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
                    let _ = self.revisions.bump_source_revision_tx(
                        tx,
                        SOURCE_TOOL,
                        evidence.evidence_id,
                        evidence.agent_id,
                        evidence.turn_id,
                        evidence.created_at,
                    )?;
                    if let Some(turn_id) = evidence.turn_id {
                        let _ = self.revisions.bump_turn_revision_tx(
                            tx,
                            evidence.agent_id,
                            turn_id,
                            false,
                            true,
                            evidence.created_at,
                        )?;
                    }
                }
                "model_requests" => {
                    let source = if evidence.kind == "provider_switch" {
                        SOURCE_MODEL_SWITCH
                    } else {
                        SOURCE_MODEL_REQUEST
                    };
                    let _ = self.revisions.bump_source_revision_tx(
                        tx,
                        source,
                        evidence.evidence_id,
                        evidence.agent_id,
                        evidence.turn_id,
                        evidence.created_at,
                    )?;
                    if let Some(turn_id) = evidence.turn_id {
                        let _ = self.revisions.bump_turn_revision_tx(
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
                        let _ = self.revisions.settle_turn_result_tx(
                            tx,
                            evidence.agent_id,
                            turn_id,
                            evidence.created_at,
                        )?;
                    }
                }
                "briefs" => {
                    if let Some(turn_id) = evidence.turn_id {
                        let _ = self.revisions.bump_turn_revision_tx(
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

    pub fn insert_message_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        message: &MessageEnvelope,
    ) -> Result<()> {
        self.upsert_message_tx(tx, message)
    }

    pub fn append_message_tx(
        &self,
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
        self.upsert_message_tx(tx, &message)?;
        Ok((
            committed_message_tx(tx, &message.agent_id, &message.id)?,
            true,
        ))
    }

    pub fn insert_transcript_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        entry: &TranscriptEntry,
    ) -> Result<()> {
        self.upsert_transcript_entry_tx(tx, entry)
    }

    pub fn append_transcript_entry_tx(
        &self,
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
        self.upsert_transcript_entry_tx(tx, &entry)?;
        Ok((
            committed_transcript_tx(tx, &entry.agent_id, &entry.id)?,
            true,
        ))
    }

    pub fn upsert_message_tx(&self, tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
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
            let _ = self.revisions.bump_source_revision_tx(
                tx,
                SOURCE_OPERATOR,
                &message.id,
                &message.agent_id,
                assigned_turn_id.as_deref(),
                message.created_at,
            )?;
            if let Some(turn_id) = assigned_turn_id.as_deref() {
                let _ = self.revisions.bump_turn_revision_tx(
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

    pub fn upsert_transcript_entry_tx(
        &self,
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
                holon_domain::TranscriptEntryKind::AssistantRound
                | holon_domain::TranscriptEntryKind::SubagentAssistantRound => {
                    Some(SOURCE_ASSISTANT)
                }
                holon_domain::TranscriptEntryKind::RuntimeFailure => Some(SOURCE_ERROR),
                _ => None,
            };
            if let Some(source_kind) = source_kind {
                let _ = self.revisions.bump_source_revision_tx(
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
                let _ = self.revisions.bump_turn_revision_tx(
                    tx,
                    &entry.agent_id,
                    turn_id,
                    true,
                    true,
                    entry.created_at,
                )?;
            }
        }
        Ok(())
    }

    pub fn insert_tool_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        record: &ToolExecutionRecord,
    ) -> Result<()> {
        self.insert_evidence_tx(
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

    pub fn insert_model_request_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
        work_item_id: Option<&str>,
        evidence: &holon_domain::conversation::ModelRequestEvidence,
    ) -> Result<()> {
        self.insert_evidence_tx(
            tx,
            EvidenceInsert {
                table: EvidenceKind::ModelRequest.table_name(),
                evidence_id: &evidence.request_id,
                agent_id,
                turn_id: Some(&evidence.turn_id),
                message_id: None,
                task_id: None,
                work_item_id,
                created_at: evidence.started_at,
                kind: "provider_request".to_string(),
                preview: None,
                payload_json: serde_json::to_string(evidence)?,
            },
        )
    }

    pub fn insert_model_switch_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
        work_item_id: Option<&str>,
        evidence: &holon_domain::conversation::ModelSwitchEvidence,
    ) -> Result<()> {
        self.insert_evidence_tx(
            tx,
            EvidenceInsert {
                table: EvidenceKind::ModelRequest.table_name(),
                evidence_id: &evidence.switch_id,
                agent_id,
                turn_id: Some(&evidence.turn_id),
                message_id: None,
                task_id: None,
                work_item_id,
                created_at: evidence.occurred_at,
                kind: "provider_switch".to_string(),
                preview: None,
                payload_json: serde_json::to_string(evidence)?,
            },
        )
    }

    pub fn insert_brief_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        brief: &BriefRecord,
    ) -> Result<()> {
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
        self.insert_evidence_tx(
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

    pub fn upsert_brief_with_created_event_seq_tx(
        &self,
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
                let _ = self.revisions.bump_turn_revision_tx(
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
        self.insert_evidence_tx(
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

    pub fn insert_delivery_summary_evidence_tx(
        &self,
        tx: &Transaction<'_>,
        record: &DeliverySummaryRecord,
    ) -> Result<()> {
        self.insert_evidence_tx(
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
}

fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("expected enum to serialize as string"))
}

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn audit_event_by_id_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event_id: &str,
    admit_payload: fn(&str) -> Result<()>,
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
            let payload = row.get_ref(1)?.as_str()?;
            admit_payload(payload)?;
            serde_json::from_str(payload).map_err(Into::into)
        })
        .transpose();
    existing
}

pub fn insert_audit_event_with_sequence_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
    admit_timestamp: fn(&DateTime<Utc>) -> Result<()>,
    admit_event: fn(&AuditEvent) -> Result<()>,
) -> Result<()> {
    let event_seq = i64::try_from(event.event_seq)
        .context("audit event sequence exceeds SQLite integer range")?;
    admit_timestamp(&event.created_at)?;
    let created_at = timestamp(event.created_at);
    admit_event(event)?;
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
    Ok(())
}

pub fn query_in(
    connection: &Connection,
    kind: EvidenceKind,
    query: EvidenceQuery<'_>,
) -> Result<Vec<EvidenceRow>> {
    let mut clauses = Vec::new();
    let mut params = Vec::<String>::new();
    push_optional_clause(&mut clauses, &mut params, "agent_id", query.agent_id);
    push_optional_clause(&mut clauses, &mut params, "turn_id", query.turn_id);
    push_optional_clause(&mut clauses, &mut params, "message_id", query.message_id);
    push_optional_clause(&mut clauses, &mut params, "task_id", query.task_id);
    push_optional_clause(
        &mut clauses,
        &mut params,
        "work_item_id",
        query.work_item_id,
    );
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
    let sql = format!(
            "SELECT evidence_id, agent_id, turn_id, message_id, task_id, work_item_id, created_at, preview
             FROM {}{}
             ORDER BY created_at DESC, evidence_id ASC
             LIMIT {}",
            kind.table_name(),
            where_clause,
            limit
        );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        Ok(EvidenceRow {
            evidence_id: row.get(0)?,
            agent_id: row.get(1)?,
            turn_id: row.get(2)?,
            message_id: row.get(3)?,
            task_id: row.get(4)?,
            work_item_id: row.get(5)?,
            created_at: row.get(6)?,
            preview: row.get(7)?,
        })
    })?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

pub fn payloads_for_turn_in(
    connection: &Connection,
    kind: EvidenceKind,
    agent_id: &str,
    turn_id: &str,
) -> Result<Vec<EvidencePayloadRow>> {
    let sql = format!(
        "SELECT payload_json
             FROM {}
             WHERE agent_id = ?1 AND turn_id = ?2
             ORDER BY created_at ASC, evidence_id ASC",
        kind.table_name()
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![agent_id, turn_id], |row| {
        Ok(EvidencePayloadRow {
            payload_json: row.get(0)?,
        })
    })?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

pub fn payloads_by_ids_in(
    connection: &Connection,
    kind: EvidenceKind,
    agent_id: &str,
    evidence_ids: &[String],
) -> Result<Vec<EvidencePayloadRow>> {
    let placeholders = evidence_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT payload_json FROM {} WHERE agent_id = ?1 AND evidence_id IN ({})",
        kind.table_name(),
        placeholders
    );
    let mut statement = connection.prepare(&sql)?;
    let mut all_params: Vec<Box<dyn ToSql>> = Vec::with_capacity(1 + evidence_ids.len());
    all_params.push(Box::new(agent_id.to_string()));
    for id in evidence_ids {
        all_params.push(Box::new(id.clone()));
    }
    let param_refs: Vec<&dyn ToSql> = all_params.iter().map(|p| p.as_ref()).collect();
    let rows = statement.query_map(param_refs.as_slice(), |row| {
        Ok(EvidencePayloadRow {
            payload_json: row.get(0)?,
        })
    })?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

pub fn context_evidence_by_id_on(
    connection: &rusqlite::Connection,
    kind: EvidenceKind,
    agent_id: &str,
    evidence_id: &str,
) -> Result<Option<EvidencePayloadRow>> {
    let sql = format!(
        "SELECT payload_json
         FROM {}
         WHERE agent_id = ?1 AND evidence_id = ?2
         LIMIT 1",
        kind.table_name()
    );
    connection
        .query_row(&sql, params![agent_id, evidence_id], |row| {
            Ok(EvidencePayloadRow {
                payload_json: row.get(0)?,
            })
        })
        .optional()
        .map_err(Into::into)
}

pub fn context_recent_evidence_on(
    connection: &Connection,
    kind: EvidenceKind,
    agent_id: &str,
    limit: usize,
    append_payload: fn(&rusqlite::Row<'_>, &mut Vec<EvidencePayloadRow>) -> Result<()>,
) -> Result<Vec<EvidencePayloadRow>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT payload_json
         FROM {}
         WHERE agent_id = ?1
         ORDER BY created_at DESC, evidence_id DESC
         LIMIT ?2",
        kind.table_name()
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(params![agent_id, limit])?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        append_payload(row, &mut records)?;
    }
    records.reverse();
    Ok(records)
}
