//! Evidence insertion helpers and evidence query types.

use anyhow::Context;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Transaction};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::runtime_db::index_outbox::RuntimeIndexChange;
use crate::runtime_db::EVIDENCE_PREVIEW_LIMIT;
use crate::types::{
    AgentIdentityRecord, AgentState, AuditEvent, BriefRecord, DeliverySummaryRecord,
    ExecutionRootEntry, MessageEnvelope, ToolExecutionRecord, TranscriptEntry, WorkspaceEntry,
    WorkspaceOccupancyRecord,
};

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
    Ok(())
}

pub(crate) fn insert_message_evidence_tx(
    tx: &Transaction<'_>,
    message: &MessageEnvelope,
) -> Result<()> {
    upsert_message_tx(tx, message)
}

pub(crate) fn insert_transcript_evidence_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<()> {
    upsert_transcript_entry_tx(tx, entry)
}

pub(crate) fn upsert_agent_state_tx(tx: &Transaction<'_>, record: &AgentState) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let now = timestamp(Utc::now());
    tx.execute(
        "INSERT INTO agent_states (
            agent_id, status, turn_index, current_run_id, current_work_item_id,
            active_workspace_id, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(agent_id) DO UPDATE SET
            status = excluded.status,
            turn_index = excluded.turn_index,
            current_run_id = excluded.current_run_id,
            current_work_item_id = excluded.current_work_item_id,
            active_workspace_id = excluded.active_workspace_id,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json
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
    let payload_json = serde_json::to_string(record)?;
    let root_kind = enum_string(&record.root_kind)?;
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
            record.execution_root_id,
            record.workspace_id,
            record.filesystem_path.display().to_string(),
            root_kind,
            timestamp(record.created_at),
            record.removed_at.map(timestamp),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_agent_identity_tx(
    tx: &Transaction<'_>,
    record: &AgentIdentityRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let kind = enum_string(&record.kind)?;
    let visibility = enum_string(&record.visibility)?;
    let ownership = record.ownership.as_ref().map(enum_string).transpose()?;
    let profile_preset = record
        .profile_preset
        .as_ref()
        .map(enum_string)
        .transpose()?;
    let status = enum_string(&record.status)?;
    tx.execute(
        "INSERT INTO agent_identities (
            agent_id, kind, visibility, ownership, profile_preset, status,
            parent_agent_id, lineage_parent_agent_id, delegated_from_task_id,
            created_at, updated_at, archived_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(agent_id) DO UPDATE SET
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
            kind,
            visibility,
            ownership,
            profile_preset,
            status,
            record.parent_agent_id,
            record.lineage_parent_agent_id,
            record.delegated_from_task_id,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.archived_at.map(timestamp),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_message_tx(tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
    let payload_json = serde_json::to_string(message)?;
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
    Ok(())
}

pub(crate) fn upsert_transcript_entry_tx(
    tx: &Transaction<'_>,
    entry: &TranscriptEntry,
) -> Result<()> {
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
    Ok(())
}

pub(crate) fn insert_runtime_index_changes_tx(
    tx: &Transaction<'_>,
    changes: &[RuntimeIndexChange],
) -> Result<()> {
    for change in changes {
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
                timestamp(Utc::now()),
            ],
        )?;
    }
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
    )
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

pub(crate) fn insert_audit_event_tx(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    event: &AuditEvent,
) -> Result<()> {
    let event_seq = i64::try_from(event.event_seq)
        .context("audit event sequence exceeds SQLite integer range")?;
    tx.execute(
        "INSERT INTO audit_events (
            audit_event_id, event_seq, agent_id, kind, created_at, data_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(audit_event_id) DO NOTHING",
        params![
            event.id,
            event_seq,
            agent_id,
            event.kind,
            timestamp(event.created_at),
            serde_json::to_string(event)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn normalize_legacy_message_value(
    mut raw_message: serde_json::Value,
) -> Result<Option<MessageEnvelope>> {
    let Some(object) = raw_message.as_object_mut() else {
        return Ok(None);
    };
    let has_turn_id = object
        .get("turn_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if !has_turn_id {
        let Some(turn_index) = object
            .get("turn_index")
            .or_else(|| object.get("message_seq"))
            .and_then(serde_json::Value::as_u64)
        else {
            return Ok(None);
        };
        object.insert(
            "turn_id".to_string(),
            serde_json::Value::String(format!("legacy-turn-{turn_index}")),
        );
    }
    serde_json::from_value(raw_message)
        .map(Some)
        .map_err(Into::into)
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
