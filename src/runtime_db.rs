use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;

use crate::types::{
    AuditEvent, BriefRecord, CallbackDeliveryMode, DeliverySummaryRecord, ExternalTriggerRecord,
    ExternalTriggerScope, ExternalTriggerStatus, MessageEnvelope, TaskRecord, TaskStatus,
    ToolExecutionRecord, TranscriptEntry, WorkItemRecord, WorkItemState,
};

const TASK_PAYLOAD_STRING_LIMIT: usize = 2048;
const TASK_PAYLOAD_ARRAY_LIMIT: usize = 64;
const EVIDENCE_PREVIEW_LIMIT: usize = 2048;

#[derive(Debug, Clone)]
pub struct RuntimeDb {
    path: PathBuf,
    lock_path: PathBuf,
}

pub struct WorkItemRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct TaskRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct ExternalTriggerRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct EvidenceRepository<'a> {
    db: &'a RuntimeDb,
}

pub struct AuditEventSink<'a> {
    db: &'a RuntimeDb,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDomainSnapshot {
    pub domain: String,
    pub schema_version: i64,
    pub import_status: String,
    pub canonical_source: String,
    pub source_checkpoint_json: Option<String>,
    pub imported_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyJsonlPosture {
    Disabled,
    CompatExport,
    AuditMirror,
    ImportSource,
}

impl LegacyJsonlPosture {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::CompatExport => "compat_export",
            Self::AuditMirror => "audit_mirror",
            Self::ImportSource => "import_source",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedStorageDomain {
    pub domain: &'static str,
    pub canonical_source: &'static str,
    pub legacy_jsonl_posture: LegacyJsonlPosture,
}

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
    fn table_name(self) -> &'static str {
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

#[derive(Debug, Default, Clone)]
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

impl WorkItemRepository<'_> {
    pub fn import_legacy(
        &self,
        records: Vec<WorkItemRecord>,
        current_work_item_id: Option<&str>,
    ) -> Result<()> {
        if self.db.storage_domain_is_complete("work_items", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("work_items", "jsonl", "db", |tx| {
                let mut latest = BTreeMap::<String, WorkItemRecord>::new();
                for record in records {
                    let should_replace = latest
                        .get(&record.id)
                        .is_none_or(|existing| newer_work_item_record(&record, existing));
                    if should_replace {
                        latest.insert(record.id.clone(), record);
                    }
                }
                for record in latest.values() {
                    upsert_work_item_tx(
                        tx,
                        record,
                        current_work_item_id == Some(record.id.as_str()),
                    )?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &WorkItemRecord, current_focus: bool) -> Result<()> {
        self.db
            .transaction(|tx| upsert_work_item_tx(tx, record, current_focus))
    }

    pub fn set_current_focus(&self, agent_id: &str, work_item_id: Option<&str>) -> Result<()> {
        self.db.transaction(|tx| {
            tx.execute(
                "UPDATE work_items SET current_focus = 0 WHERE agent_id = ?1 AND current_focus != 0",
                [agent_id],
            )?;
            if let Some(work_item_id) = work_item_id {
                tx.execute(
                    "UPDATE work_items SET current_focus = 1 WHERE agent_id = ?1 AND work_item_id = ?2",
                    params![agent_id, work_item_id],
                )?;
            }
            Ok(())
        })
    }

    pub fn latest(&self, work_item_id: &str) -> Result<Option<WorkItemRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
                [work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_work_item_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<WorkItemRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             WHERE agent_id = ?1
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }

    pub fn latest_all(&self) -> Result<Vec<WorkItemRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM work_items
             ORDER BY updated_at DESC, created_at DESC, work_item_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_work_item_payload(&row?)).collect()
    }
}

impl ExternalTriggerRepository<'_> {
    pub fn import_legacy(&self, records: Vec<ExternalTriggerRecord>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("external_triggers", "db")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("external_triggers", "jsonl", "db", |tx| {
                let latest = reduce_external_trigger_records(records);
                for record in latest.values() {
                    upsert_external_trigger_tx(tx, record)?;
                }
                Ok(serde_json::json!({ "imported_records": latest.len() }))
            })
    }

    pub fn upsert(&self, record: &ExternalTriggerRecord) -> Result<()> {
        self.db
            .transaction(|tx| upsert_external_trigger_tx(tx, record))
    }

    pub fn latest(&self, external_trigger_id: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM external_triggers WHERE external_trigger_id = ?1",
                [external_trigger_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> Result<Vec<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM external_triggers
             WHERE target_agent_id = ?1
             ORDER BY created_at DESC, external_trigger_id ASC",
        )?;
        let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_external_trigger_payload(&row?))
            .collect()
    }

    pub fn active_default_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE target_agent_id = ?1 AND status = 'active'
                 ORDER BY created_at DESC, external_trigger_id ASC
                 LIMIT 1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }

    pub fn active_by_token_hash(&self, token_hash: &str) -> Result<Option<ExternalTriggerRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM external_triggers
                 WHERE token_hash = ?1 AND status = 'active'
                 LIMIT 1",
                [token_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_external_trigger_payload(&payload))
            .transpose()
    }
}

impl TaskRepository<'_> {
    pub fn import_legacy(&self, records: Vec<TaskRecord>) -> Result<()> {
        if self.db.storage_domain_is_complete("tasks", "db")? {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("tasks", "jsonl", "db", |tx| {
                let latest = reduce_task_records(records);
                for record in latest.values() {
                    upsert_task_tx(tx, record)?;
                }
                let active_records = latest
                    .values()
                    .filter(|record| is_active_task_status(&record.status))
                    .count();
                Ok(serde_json::json!({
                    "imported_records": latest.len(),
                    "active_records": active_records,
                }))
            })
    }

    pub fn upsert(&self, record: &TaskRecord) -> Result<()> {
        self.db.transaction(|tx| upsert_task_tx(tx, record))
    }

    pub fn latest(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                [task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_task_payload(&payload))
            .transpose()
    }

    pub fn latest_all(&self) -> Result<Vec<TaskRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM tasks
             ORDER BY updated_at DESC, created_at DESC, task_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_task_payload(&row?)).collect()
    }

    pub fn latest_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(agent_id, "owner_agent_id = ?1", [agent_id], limit)
    }

    pub fn active_for_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<TaskRecord>> {
        self.query_for_agent(
            agent_id,
            "owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')",
            [agent_id],
            limit,
        )
    }

    fn query_for_agent(
        &self,
        _agent_id: &str,
        where_clause: &str,
        params: [&str; 1],
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let sql = format!(
            "SELECT payload_json
             FROM tasks
             WHERE {where_clause}
             ORDER BY updated_at DESC, created_at DESC, task_id ASC
             LIMIT {limit}",
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
        rows.map(|row| decode_task_payload(&row?)).collect()
    }
}

impl EvidenceRepository<'_> {
    pub fn import_legacy(
        &self,
        messages: Vec<serde_json::Value>,
        transcript_entries: Vec<TranscriptEntry>,
        tool_executions: Vec<ToolExecutionRecord>,
        briefs: Vec<BriefRecord>,
        delivery_summaries: Vec<DeliverySummaryRecord>,
    ) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("evidence", "jsonl+db-index")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("evidence", "jsonl", "jsonl+db-index", |tx| {
                let mut imported_messages = 0_u64;
                let mut dropped_messages = 0_u64;
                for raw_message in messages {
                    match normalize_legacy_message_value(raw_message)? {
                        Some(message) => {
                            insert_message_evidence_tx(tx, &message)?;
                            imported_messages += 1;
                        }
                        None => dropped_messages += 1,
                    }
                }
                for entry in &transcript_entries {
                    insert_transcript_evidence_tx(tx, entry)?;
                }
                for record in &tool_executions {
                    insert_tool_evidence_tx(tx, record)?;
                }
                for brief in &briefs {
                    insert_brief_evidence_tx(tx, brief)?;
                }
                for summary in &delivery_summaries {
                    insert_delivery_summary_evidence_tx(tx, summary)?;
                }
                Ok(serde_json::json!({
                    "imported_messages": imported_messages,
                    "dropped_messages": dropped_messages,
                    "imported_transcript_entries": transcript_entries.len(),
                    "imported_tool_executions": tool_executions.len(),
                    "imported_briefs": briefs.len(),
                    "imported_delivery_summaries": delivery_summaries.len(),
                }))
            })
    }

    pub fn append_message(&self, message: &MessageEnvelope) -> Result<()> {
        self.db
            .transaction(|tx| insert_message_evidence_tx(tx, message))
    }

    pub fn append_transcript_entry(&self, entry: &TranscriptEntry) -> Result<()> {
        self.db
            .transaction(|tx| insert_transcript_evidence_tx(tx, entry))
    }

    pub fn append_tool_execution(&self, record: &ToolExecutionRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_tool_evidence_tx(tx, record))
    }

    pub fn append_brief(&self, brief: &BriefRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_brief_evidence_tx(tx, brief))
    }

    pub fn append_delivery_summary(&self, record: &DeliverySummaryRecord) -> Result<()> {
        self.db
            .transaction(|tx| insert_delivery_summary_evidence_tx(tx, record))
    }

    pub fn query(&self, kind: EvidenceKind, query: EvidenceQuery<'_>) -> Result<Vec<EvidenceRow>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
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
        let connection = self.db.connection()?;
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
}

impl AuditEventSink<'_> {
    pub fn append(&self, agent_id: Option<&str>, event: &AuditEvent) -> Result<()> {
        self.db
            .transaction(|tx| insert_audit_event_tx(tx, agent_id, event))
    }

    pub fn import_legacy(&self, agent_id: Option<&str>, events: Vec<AuditEvent>) -> Result<()> {
        if self
            .db
            .storage_domain_is_complete("audit_events", "jsonl+db-index")?
        {
            return Ok(());
        }
        self.db
            .run_storage_domain_import("audit_events", "jsonl", "jsonl+db-index", |tx| {
                for event in &events {
                    insert_audit_event_tx(tx, agent_id, event)?;
                }
                Ok(serde_json::json!({ "imported_records": events.len() }))
            })
    }

    pub fn page_after(
        &self,
        agent_id: Option<&str>,
        after_event_seq: u64,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.db.connection()?;
        let mut sql =
            String::from("SELECT data_json FROM audit_events WHERE COALESCE(event_seq, 0) > ?1");
        if agent_id.is_some() {
            sql.push_str(" AND agent_id = ?2");
        }
        sql.push_str(" ORDER BY event_seq ASC, created_at ASC LIMIT ");
        sql.push_str(&limit.to_string());
        let mut statement = connection.prepare(&sql)?;
        if let Some(agent_id) = agent_id {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows = statement.query_map(params![after_event_seq, agent_id], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        } else {
            let after_event_seq = i64::try_from(after_event_seq)
                .context("audit event cursor exceeds SQLite integer range")?;
            let rows =
                statement.query_map(params![after_event_seq], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(Into::into)
            })
            .collect()
        }
    }
}

fn read_storage_domain_connection(
    connection: &Connection,
    domain: &str,
) -> Result<Option<StorageDomainSnapshot>> {
    connection
        .query_row(
            "SELECT domain, schema_version, import_status, canonical_source,
                    source_checkpoint_json, imported_at, updated_at
             FROM storage_domains WHERE domain = ?1",
            [domain],
            |row| {
                Ok(StorageDomainSnapshot {
                    domain: row.get(0)?,
                    schema_version: row.get(1)?,
                    import_status: row.get(2)?,
                    canonical_source: row.get(3)?,
                    source_checkpoint_json: row.get(4)?,
                    imported_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn upsert_storage_domain(
    tx: &Transaction<'_>,
    domain: &str,
    import_status: &str,
    canonical_source: &str,
    checkpoint: Option<serde_json::Value>,
) -> Result<()> {
    let now = timestamp(Utc::now());
    let imported_at = (import_status == "complete").then(|| now.clone());
    let checkpoint = checkpoint.map(|value| value.to_string());
    tx.execute(
        "INSERT INTO storage_domains (
            domain, schema_version, import_status, canonical_source,
            source_checkpoint_json, imported_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(domain) DO UPDATE SET
            schema_version = excluded.schema_version,
            import_status = excluded.import_status,
            canonical_source = excluded.canonical_source,
            source_checkpoint_json = excluded.source_checkpoint_json,
            imported_at = excluded.imported_at,
            updated_at = excluded.updated_at",
        params![
            domain,
            max_known_migration_version(),
            import_status,
            canonical_source,
            checkpoint,
            imported_at,
            now
        ],
    )?;
    Ok(())
}

#[derive(Debug)]
struct EvidenceInsert<'a> {
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

fn insert_evidence_tx(tx: &Transaction<'_>, evidence: EvidenceInsert<'_>) -> Result<()> {
    let sql = format!(
        "INSERT INTO {} (
            evidence_id, agent_id, turn_id, message_id, task_id, work_item_id,
            created_at, kind, content_ref, content_hash, preview, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(evidence_id) DO NOTHING",
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
            Option::<String>::None,
            evidence.preview,
            evidence.payload_json,
        ],
    )?;
    Ok(())
}

fn insert_message_evidence_tx(tx: &Transaction<'_>, message: &MessageEnvelope) -> Result<()> {
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::Message.table_name(),
            evidence_id: &message.id,
            agent_id: &message.agent_id,
            turn_id: message.turn_id.as_deref(),
            message_id: Some(&message.id),
            task_id: message.task_id.as_deref(),
            work_item_id: message.work_item_id.as_deref(),
            created_at: message.created_at,
            kind: enum_string(&message.kind)?,
            preview: evidence_preview(&message.body),
            payload_json: bounded_payload_json(message)?,
        },
    )
}

fn insert_transcript_evidence_tx(tx: &Transaction<'_>, entry: &TranscriptEntry) -> Result<()> {
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
    insert_evidence_tx(
        tx,
        EvidenceInsert {
            table: EvidenceKind::TranscriptEntry.table_name(),
            evidence_id: &entry.id,
            agent_id: &entry.agent_id,
            turn_id,
            message_id: entry.related_message_id.as_deref(),
            task_id,
            work_item_id,
            created_at: entry.created_at,
            kind: enum_string(&entry.kind)?,
            preview: evidence_preview(&entry.data),
            payload_json: bounded_payload_json(entry)?,
        },
    )
}

fn insert_tool_evidence_tx(tx: &Transaction<'_>, record: &ToolExecutionRecord) -> Result<()> {
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
            payload_json: bounded_payload_json(record)?,
        },
    )
}

fn insert_brief_evidence_tx(tx: &Transaction<'_>, brief: &BriefRecord) -> Result<()> {
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
            payload_json: bounded_payload_json(brief)?,
        },
    )
}

fn insert_delivery_summary_evidence_tx(
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
            payload_json: bounded_payload_json(record)?,
        },
    )
}

fn insert_audit_event_tx(
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

fn normalize_legacy_message_value(
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
        let Some(turn_index) = object.get("turn_index").and_then(serde_json::Value::as_u64) else {
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

fn bounded_payload_json<T: Serialize>(value: &T) -> Result<String> {
    let mut payload = serde_json::to_value(value)?;
    bound_json_value(&mut payload);
    serde_json::to_string(&payload).map_err(Into::into)
}

fn evidence_preview(value: &impl Serialize) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .map(|value| truncate_evidence_string(&value))
}

fn bound_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) if text.len() > EVIDENCE_PREVIEW_LIMIT => {
            truncate_string_in_place(text, EVIDENCE_PREVIEW_LIMIT);
        }
        serde_json::Value::Array(items) => {
            if items.len() > TASK_PAYLOAD_ARRAY_LIMIT {
                items.truncate(TASK_PAYLOAD_ARRAY_LIMIT);
            }
            for item in items {
                bound_json_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                bound_json_value(value);
            }
        }
        _ => {}
    }
}

fn truncate_evidence_string(value: &str) -> String {
    let mut truncated = value.to_string();
    if truncated.len() > EVIDENCE_PREVIEW_LIMIT {
        truncate_string_in_place(&mut truncated, EVIDENCE_PREVIEW_LIMIT);
    }
    truncated
}

fn truncate_string_in_place(value: &mut String, max_bytes: usize) {
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

fn upsert_external_trigger_tx(tx: &Transaction<'_>, record: &ExternalTriggerRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let status = enum_string(&record.status)?;
    let revoked_at = record.revoked_at.map(timestamp);
    let last_delivered_at = record.last_delivered_at.map(timestamp);
    tx.execute(
        "INSERT INTO external_triggers (
            external_trigger_id, target_agent_id, waiting_intent_id, trigger_url,
            token_hash, status, created_at, revoked_at, last_delivered_at,
            delivery_count, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(external_trigger_id) DO UPDATE SET
            target_agent_id = excluded.target_agent_id,
            waiting_intent_id = excluded.waiting_intent_id,
            trigger_url = excluded.trigger_url,
            token_hash = excluded.token_hash,
            status = excluded.status,
            created_at = excluded.created_at,
            revoked_at = excluded.revoked_at,
            last_delivered_at = excluded.last_delivered_at,
            delivery_count = excluded.delivery_count,
            payload_json = excluded.payload_json
         WHERE excluded.delivery_count > external_triggers.delivery_count
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') > COALESCE(external_triggers.last_delivered_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') > COALESCE(external_triggers.revoked_at, '')
            )
            OR (
                excluded.delivery_count = external_triggers.delivery_count
                AND COALESCE(excluded.last_delivered_at, '') = COALESCE(external_triggers.last_delivered_at, '')
                AND COALESCE(excluded.revoked_at, '') = COALESCE(external_triggers.revoked_at, '')
                AND excluded.created_at >= external_triggers.created_at
            )",
        params![
            record.external_trigger_id,
            record.target_agent_id,
            record.waiting_intent_id,
            record.trigger_url,
            record.token_hash,
            status,
            timestamp(record.created_at),
            revoked_at,
            last_delivered_at,
            record.delivery_count as i64,
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    current_focus: bool,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let readiness = enum_string(&record.readiness())?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    tx.execute(
        "INSERT INTO work_items (
            work_item_id, agent_id, state, objective, plan_status, readiness,
            revision, current_focus, created_at, updated_at, completed_at,
            plan_artifact_path, last_turn_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(work_item_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            state = excluded.state,
            objective = excluded.objective,
            plan_status = excluded.plan_status,
            readiness = excluded.readiness,
            revision = excluded.revision,
            current_focus = excluded.current_focus,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            plan_artifact_path = excluded.plan_artifact_path,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json
         WHERE excluded.revision >= work_items.revision",
        params![
            record.id,
            record.agent_id,
            state,
            record.objective,
            plan_status,
            readiness,
            record.revision as i64,
            i64::from(current_focus),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
        ],
    )?;
    Ok(())
}

fn upsert_task_tx(tx: &Transaction<'_>, record: &TaskRecord) -> Result<()> {
    let kind = record.kind.as_str();
    let status = enum_string(&record.status)?;
    let child_agent_id = task_detail_string(&record.detail, "child_agent_id");
    let parent_agent_id = child_agent_id.as_ref().map(|_| record.agent_id.clone());
    let input_target = task_detail_string(&record.detail, "input_target");
    let wait_policy = enum_string(&record.wait_policy())?;
    let output_path = task_detail_string(&record.detail, "output_path");
    let result_summary = task_detail_string(&record.detail, "output_summary")
        .map(|summary| truncate_task_payload_string(&summary));
    let exit_status = task_detail_i64(&record.detail, "exit_status");
    let terminal_reentry = i64::from(record.terminal_reentry());
    let completed_at =
        is_terminal_task_status(&record.status).then(|| timestamp(record.updated_at));
    let payload_json = serde_json::to_string(&slim_task_record_for_payload(record))?;
    tx.execute(
        "INSERT INTO tasks (
            task_id, owner_agent_id, parent_agent_id, child_agent_id, kind, status,
            summary, input_target, wait_policy, output_path, result_summary,
            exit_status, terminal_reentry, revision, created_at, updated_at,
            completed_at, last_message_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(task_id) DO UPDATE SET
            owner_agent_id = excluded.owner_agent_id,
            parent_agent_id = excluded.parent_agent_id,
            child_agent_id = excluded.child_agent_id,
            kind = excluded.kind,
            status = excluded.status,
            summary = excluded.summary,
            input_target = excluded.input_target,
            wait_policy = excluded.wait_policy,
            output_path = excluded.output_path,
            result_summary = excluded.result_summary,
            exit_status = excluded.exit_status,
            terminal_reentry = excluded.terminal_reentry,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            last_message_id = excluded.last_message_id,
            payload_json = excluded.payload_json
         WHERE excluded.revision >= tasks.revision",
        params![
            record.id,
            record.agent_id,
            parent_agent_id,
            child_agent_id,
            kind,
            status,
            record.summary,
            input_target,
            wait_policy,
            output_path,
            result_summary,
            exit_status,
            terminal_reentry,
            task_revision(record),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            record.parent_message_id,
            payload_json,
        ],
    )?;
    Ok(())
}

fn newer_work_item_record(candidate: &WorkItemRecord, existing: &WorkItemRecord) -> bool {
    candidate
        .revision
        .cmp(&existing.revision)
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

fn reduce_external_trigger_records(
    records: Vec<ExternalTriggerRecord>,
) -> BTreeMap<String, ExternalTriggerRecord> {
    let mut latest_by_id = BTreeMap::<String, ExternalTriggerRecord>::new();
    for record in records {
        if latest_by_id
            .get(&record.external_trigger_id)
            .is_none_or(|existing| newer_external_trigger_record(&record, existing))
        {
            latest_by_id.insert(
                record.external_trigger_id.clone(),
                normalize_external_trigger_record(record),
            );
        }
    }

    let mut active_by_agent = BTreeMap::<String, String>::new();
    for record in latest_by_id.values() {
        if record.status != ExternalTriggerStatus::Active {
            continue;
        }
        let replace = active_by_agent
            .get(&record.target_agent_id)
            .and_then(|id| latest_by_id.get(id))
            .is_none_or(|existing| newer_external_trigger_record(record, existing));
        if replace {
            active_by_agent.insert(
                record.target_agent_id.clone(),
                record.external_trigger_id.clone(),
            );
        }
    }

    for record in latest_by_id.values_mut() {
        if record.status == ExternalTriggerStatus::Active
            && active_by_agent.get(&record.target_agent_id) != Some(&record.external_trigger_id)
        {
            record.status = ExternalTriggerStatus::Revoked;
            record.revoked_at.get_or_insert(record.created_at);
        }
    }
    latest_by_id
}

fn normalize_external_trigger_record(mut record: ExternalTriggerRecord) -> ExternalTriggerRecord {
    record.scope = ExternalTriggerScope::Agent;
    record.delivery_mode = CallbackDeliveryMode::WakeHint;
    record.waiting_intent_id = None;
    record
}

fn newer_external_trigger_record(
    candidate: &ExternalTriggerRecord,
    existing: &ExternalTriggerRecord,
) -> bool {
    candidate
        .delivery_count
        .cmp(&existing.delivery_count)
        .then_with(|| candidate.last_delivered_at.cmp(&existing.last_delivered_at))
        .then_with(|| candidate.revoked_at.cmp(&existing.revoked_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| {
            candidate
                .external_trigger_id
                .cmp(&existing.external_trigger_id)
        })
        .is_gt()
}

fn reduce_task_records(records: Vec<TaskRecord>) -> BTreeMap<String, TaskRecord> {
    let mut latest = BTreeMap::<String, TaskRecord>::new();
    for record in records {
        if let Some(previous) = latest.get(&record.id) {
            let mut merged = record.clone();
            if merged.summary.is_none() {
                merged.summary = previous.summary.clone();
            }
            if merged.detail.is_none() {
                merged.detail = previous.detail.clone();
            }
            if merged.recovery.is_none() {
                merged.recovery = previous.recovery.clone();
            }
            if newer_task_record(&merged, previous) {
                latest.insert(record.id.clone(), merged);
            }
        } else {
            latest.insert(record.id.clone(), record);
        }
    }
    latest
}

fn slim_task_record_for_payload(record: &TaskRecord) -> TaskRecord {
    let mut slim = record.clone();
    slim.detail = slim.detail.as_ref().map(slim_task_detail_value);
    slim
}

fn slim_task_detail_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut slim = serde_json::Map::new();
            for (key, value) in map {
                if key == "initial_output" {
                    continue;
                }
                slim.insert(key.clone(), slim_task_detail_value(value));
            }
            serde_json::Value::Object(slim)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(TASK_PAYLOAD_ARRAY_LIMIT)
                .map(slim_task_detail_value)
                .collect(),
        ),
        serde_json::Value::String(value) => {
            serde_json::Value::String(truncate_task_payload_string(value))
        }
        _ => value.clone(),
    }
}

fn truncate_task_payload_string(value: &str) -> String {
    value.chars().take(TASK_PAYLOAD_STRING_LIMIT).collect()
}

fn newer_task_record(candidate: &TaskRecord, existing: &TaskRecord) -> bool {
    task_revision(candidate)
        .cmp(&task_revision(existing))
        .then_with(|| candidate.updated_at.cmp(&existing.updated_at))
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .is_gt()
}

fn task_revision(record: &TaskRecord) -> i64 {
    record.updated_at.timestamp_millis()
}

fn decode_work_item_payload(payload: &str) -> Result<WorkItemRecord> {
    serde_json::from_str(payload).context("decoding work item payload from runtime db")
}

fn decode_external_trigger_payload(payload: &str) -> Result<ExternalTriggerRecord> {
    serde_json::from_str(payload).context("decoding external trigger payload from runtime db")
}

fn decode_task_payload(payload: &str) -> Result<TaskRecord> {
    serde_json::from_str(payload).context("decoding task payload from runtime db")
}

fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("expected enum to serialize as string"))
}

fn task_detail_string(detail: &Option<serde_json::Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn task_detail_i64(detail: &Option<serde_json::Value>, key: &str) -> Option<i64> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_i64())
}

fn is_active_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    )
}

fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Debug)]
pub struct RuntimeDbLock {
    file: File,
    path: PathBuf,
}

#[derive(Debug)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "runtime_db_foundation",
        sql: r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS storage_domains (
  domain TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL,
  import_status TEXT NOT NULL,
  canonical_source TEXT NOT NULL,
  source_checkpoint_json TEXT,
  imported_at TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agents (
  agent_id TEXT PRIMARY KEY,
  status TEXT,
  visibility TEXT,
  ownership TEXT,
  profile_preset TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT
);

CREATE TABLE IF NOT EXISTS audit_events (
  audit_event_id TEXT PRIMARY KEY,
  event_seq INTEGER,
  agent_id TEXT,
  kind TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_storage_domains_import_status
  ON storage_domains(import_status);

CREATE INDEX IF NOT EXISTS idx_agents_status
  ON agents(status);

CREATE INDEX IF NOT EXISTS idx_audit_events_agent_created
  ON audit_events(agent_id, created_at);

CREATE INDEX IF NOT EXISTS idx_audit_events_event_seq
  ON audit_events(event_seq);
"#,
    },
    Migration {
        version: 2,
        name: "work_items_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS work_items (
  work_item_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  state TEXT NOT NULL,
  objective TEXT NOT NULL,
  plan_status TEXT,
  readiness TEXT,
  revision INTEGER NOT NULL,
  current_focus INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT,
  plan_artifact_path TEXT,
  last_turn_id TEXT,
  last_message_id TEXT,
  causation_id TEXT,
  correlation_id TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_work_items_agent
  ON work_items(agent_id);

CREATE INDEX IF NOT EXISTS idx_work_items_state
  ON work_items(state);

CREATE INDEX IF NOT EXISTS idx_work_items_readiness
  ON work_items(readiness);

CREATE INDEX IF NOT EXISTS idx_work_items_current_focus
  ON work_items(agent_id, current_focus);
"#,
    },
    Migration {
        version: 3,
        name: "tasks_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS tasks (
  task_id TEXT PRIMARY KEY,
  owner_agent_id TEXT NOT NULL,
  parent_agent_id TEXT,
  child_agent_id TEXT,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  summary TEXT,
  input_target TEXT,
  wait_policy TEXT,
  output_path TEXT,
  result_summary TEXT,
  exit_status INTEGER,
  terminal_reentry INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT,
  last_turn_id TEXT,
  last_message_id TEXT,
  causation_id TEXT,
  correlation_id TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tasks_owner_agent
  ON tasks(owner_agent_id);

CREATE INDEX IF NOT EXISTS idx_tasks_parent_agent
  ON tasks(parent_agent_id);

CREATE INDEX IF NOT EXISTS idx_tasks_child_agent
  ON tasks(child_agent_id);

CREATE INDEX IF NOT EXISTS idx_tasks_status
  ON tasks(status);

CREATE INDEX IF NOT EXISTS idx_tasks_owner_active
  ON tasks(owner_agent_id, status, updated_at);
"#,
    },
    Migration {
        version: 4,
        name: "external_triggers_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS external_triggers (
  external_trigger_id TEXT PRIMARY KEY,
  target_agent_id TEXT NOT NULL,
  waiting_intent_id TEXT,
  trigger_url TEXT,
  token_hash TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  revoked_at TEXT,
  last_delivered_at TEXT,
  delivery_count INTEGER NOT NULL DEFAULT 0,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_external_triggers_agent_status
  ON external_triggers(target_agent_id, status);

CREATE INDEX IF NOT EXISTS idx_external_triggers_token_hash
  ON external_triggers(token_hash);

CREATE UNIQUE INDEX IF NOT EXISTS idx_external_triggers_active_default_agent
  ON external_triggers(target_agent_id)
  WHERE status = 'active';
"#,
    },
    Migration {
        version: 5,
        name: "evidence_indexing_and_audit_sink",
        sql: r#"
CREATE TABLE IF NOT EXISTS messages (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS transcript_entries (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_executions (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS model_requests (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS model_responses (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS briefs (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS delivery_summaries (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_metadata (
  evidence_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  turn_id TEXT,
  message_id TEXT,
  task_id TEXT,
  work_item_id TEXT,
  created_at TEXT NOT NULL,
  kind TEXT NOT NULL,
  content_ref TEXT,
  content_hash TEXT,
  preview TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_agent_turn
  ON messages(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_messages_message
  ON messages(message_id);
CREATE INDEX IF NOT EXISTS idx_messages_task
  ON messages(task_id);
CREATE INDEX IF NOT EXISTS idx_messages_work_item
  ON messages(work_item_id);

CREATE INDEX IF NOT EXISTS idx_transcript_entries_agent_turn
  ON transcript_entries(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_message
  ON transcript_entries(message_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_task
  ON transcript_entries(task_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_work_item
  ON transcript_entries(work_item_id);

CREATE INDEX IF NOT EXISTS idx_tool_executions_agent_turn
  ON tool_executions(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_tool_executions_message
  ON tool_executions(message_id);
CREATE INDEX IF NOT EXISTS idx_tool_executions_task
  ON tool_executions(task_id);
CREATE INDEX IF NOT EXISTS idx_tool_executions_work_item
  ON tool_executions(work_item_id);

CREATE INDEX IF NOT EXISTS idx_model_requests_agent_turn
  ON model_requests(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_model_requests_message
  ON model_requests(message_id);
CREATE INDEX IF NOT EXISTS idx_model_requests_task
  ON model_requests(task_id);
CREATE INDEX IF NOT EXISTS idx_model_requests_work_item
  ON model_requests(work_item_id);

CREATE INDEX IF NOT EXISTS idx_model_responses_agent_turn
  ON model_responses(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_model_responses_message
  ON model_responses(message_id);
CREATE INDEX IF NOT EXISTS idx_model_responses_task
  ON model_responses(task_id);
CREATE INDEX IF NOT EXISTS idx_model_responses_work_item
  ON model_responses(work_item_id);

CREATE INDEX IF NOT EXISTS idx_briefs_agent_turn
  ON briefs(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_briefs_message
  ON briefs(message_id);
CREATE INDEX IF NOT EXISTS idx_briefs_task
  ON briefs(task_id);
CREATE INDEX IF NOT EXISTS idx_briefs_work_item
  ON briefs(work_item_id);

CREATE INDEX IF NOT EXISTS idx_delivery_summaries_agent_turn
  ON delivery_summaries(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_delivery_summaries_message
  ON delivery_summaries(message_id);
CREATE INDEX IF NOT EXISTS idx_delivery_summaries_task
  ON delivery_summaries(task_id);
CREATE INDEX IF NOT EXISTS idx_delivery_summaries_work_item
  ON delivery_summaries(work_item_id);

CREATE INDEX IF NOT EXISTS idx_artifact_metadata_agent_turn
  ON artifact_metadata(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_artifact_metadata_message
  ON artifact_metadata(message_id);
CREATE INDEX IF NOT EXISTS idx_artifact_metadata_task
  ON artifact_metadata(task_id);
CREATE INDEX IF NOT EXISTS idx_artifact_metadata_work_item
  ON artifact_metadata(work_item_id);
"#,
    },
];

impl RuntimeDb {
    pub fn open_and_migrate(
        path: impl Into<PathBuf>,
        lock_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let db = Self {
            path: path.into(),
            lock_path: lock_path.into(),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub fn connection(&self) -> Result<Connection> {
        open_connection(&self.path)
    }

    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        match f(&transaction) {
            Ok(value) => {
                transaction.commit()?;
                Ok(value)
            }
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        }
    }

    pub fn current_schema_version(&self) -> Result<i64> {
        let connection = self.connection()?;
        current_schema_version(&connection)
    }

    pub fn work_items(&self) -> WorkItemRepository<'_> {
        WorkItemRepository { db: self }
    }

    pub fn tasks(&self) -> TaskRepository<'_> {
        TaskRepository { db: self }
    }

    pub fn external_triggers(&self) -> ExternalTriggerRepository<'_> {
        ExternalTriggerRepository { db: self }
    }

    pub fn evidence(&self) -> EvidenceRepository<'_> {
        EvidenceRepository { db: self }
    }

    pub fn audit_events(&self) -> AuditEventSink<'_> {
        AuditEventSink { db: self }
    }

    pub const fn expected_storage_domains() -> &'static [ExpectedStorageDomain] {
        &[
            ExpectedStorageDomain {
                domain: "work_items",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::CompatExport,
            },
            ExpectedStorageDomain {
                domain: "tasks",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::CompatExport,
            },
            ExpectedStorageDomain {
                domain: "external_triggers",
                canonical_source: "db",
                legacy_jsonl_posture: LegacyJsonlPosture::CompatExport,
            },
            ExpectedStorageDomain {
                domain: "evidence",
                canonical_source: "jsonl+db-index",
                legacy_jsonl_posture: LegacyJsonlPosture::ImportSource,
            },
            ExpectedStorageDomain {
                domain: "audit_events",
                canonical_source: "jsonl+db-index",
                legacy_jsonl_posture: LegacyJsonlPosture::AuditMirror,
            },
        ]
    }

    pub fn storage_domain(&self, domain: &str) -> Result<Option<StorageDomainSnapshot>> {
        let connection = self.connection()?;
        read_storage_domain_connection(&connection, domain)
    }

    pub fn storage_domains(&self) -> Result<Vec<StorageDomainSnapshot>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT domain, schema_version, import_status, canonical_source,
                    source_checkpoint_json, imported_at, updated_at
             FROM storage_domains
             ORDER BY domain ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(StorageDomainSnapshot {
                domain: row.get(0)?,
                schema_version: row.get(1)?,
                import_status: row.get(2)?,
                canonical_source: row.get(3)?,
                source_checkpoint_json: row.get(4)?,
                imported_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn diagnose_cutover(&self, expected: &[ExpectedStorageDomain]) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut diagnostics = Vec::new();
        let current_version = current_schema_version(&connection)?;
        let max_known_version = max_known_migration_version();
        if current_version < max_known_version {
            diagnostics.push(format!(
                "missing runtime db migration: schema_migrations is at {current_version}, expected {max_known_version}"
            ));
        }
        for expected_domain in expected {
            match read_storage_domain_connection(&connection, expected_domain.domain)? {
                None => diagnostics.push(format!(
                    "storage domain {} is missing; expected canonical_source={} legacy_jsonl_posture={}",
                    expected_domain.domain,
                    expected_domain.canonical_source,
                    expected_domain.legacy_jsonl_posture.as_str()
                )),
                Some(snapshot) => {
                    if snapshot.import_status == "failed" {
                        diagnostics.push(format!(
                            "storage domain {} import failed: {}",
                            snapshot.domain,
                            snapshot
                                .source_checkpoint_json
                                .as_deref()
                                .unwrap_or("no failure checkpoint recorded")
                        ));
                    } else if snapshot.import_status != "complete" {
                        diagnostics.push(format!(
                            "storage domain {} import is {}; expected complete",
                            snapshot.domain, snapshot.import_status
                        ));
                    }
                    if snapshot.canonical_source != expected_domain.canonical_source {
                        diagnostics.push(format!(
                            "storage domain {} has canonical_source={}; expected {}",
                            snapshot.domain,
                            snapshot.canonical_source,
                            expected_domain.canonical_source
                        ));
                    }
                }
            }
        }
        Ok(diagnostics)
    }

    pub fn validate_expected_storage_domains(
        &self,
        expected: &[ExpectedStorageDomain],
    ) -> Result<()> {
        let diagnostics = self.diagnose_cutover(expected)?;
        if diagnostics.is_empty() {
            return Ok(());
        }
        bail!(
            "runtime db cutover diagnostics failed:\n{}",
            diagnostics.join("\n")
        )
    }

    fn storage_domain_is_complete(&self, domain: &str, canonical_source: &str) -> Result<bool> {
        let connection = self.connection()?;
        let Some(snapshot) = read_storage_domain_connection(&connection, domain)? else {
            return Ok(false);
        };
        Ok(snapshot.import_status == "complete" && snapshot.canonical_source == canonical_source)
    }

    fn run_storage_domain_import(
        &self,
        domain: &'static str,
        importing_source: &'static str,
        complete_source: &'static str,
        import: impl FnOnce(&Transaction<'_>) -> Result<serde_json::Value>,
    ) -> Result<()> {
        self.transaction(|tx| {
            upsert_storage_domain(tx, domain, "importing", importing_source, None)
        })?;
        let result = self.transaction(|tx| {
            let checkpoint = import(tx)?;
            upsert_storage_domain(tx, domain, "complete", complete_source, Some(checkpoint))?;
            Ok(())
        });
        if let Err(error) = result {
            let checkpoint = serde_json::json!({
                "error": error.to_string(),
                "retry": "restart runtime to retry legacy import",
            });
            self.transaction(|tx| {
                upsert_storage_domain(tx, domain, "failed", importing_source, Some(checkpoint))
            })?;
            return Err(error).with_context(|| format!("importing legacy storage domain {domain}"));
        }
        Ok(())
    }

    pub fn migrate(&self) -> Result<()> {
        let _lock = RuntimeDbLock::lock(&self.lock_path)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
        }
        let mut connection = self.connection()?;
        ensure_migration_table(&connection)?;
        let current_version = current_schema_version(&connection)?;
        let max_known_version = max_known_migration_version();
        if current_version > max_known_version {
            bail!(
                "runtime db schema version {} is newer than this binary supports ({})",
                current_version,
                max_known_version
            );
        }
        for migration in MIGRATIONS {
            apply_migration(&mut connection, migration)?;
        }
        Ok(())
    }
}

impl RuntimeDbLock {
    pub fn lock(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(path.into(), LockMode::Blocking)
    }

    pub fn try_lock(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open(path.into(), LockMode::NonBlocking)
    }

    fn open(path: PathBuf, mode: LockMode) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating runtime lock directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening runtime db lock {}", path.display()))?;
        flock(&file, mode).with_context(|| format!("locking runtime db {}", path.display()))?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RuntimeDbLock {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

#[derive(Debug, Clone, Copy)]
enum LockMode {
    Blocking,
    NonBlocking,
}

fn open_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
    }
    let connection =
        Connection::open(path).with_context(|| format!("opening runtime db {}", path.display()))?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
"#,
    )?;
    Ok(())
}

fn ensure_migration_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
"#,
    )?;
    Ok(())
}

fn apply_migration(connection: &mut Connection, migration: &Migration) -> Result<()> {
    let existing_name: Option<String> = connection
        .query_row(
            "SELECT name FROM schema_migrations WHERE version = ?1",
            [migration.version],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_name) = existing_name {
        if existing_name != migration.name {
            bail!(
                "runtime db migration {} name mismatch: expected {}, found {}",
                migration.version,
                migration.name,
                existing_name
            );
        }
        return Ok(());
    }

    let transaction = connection.transaction()?;
    transaction.execute_batch(migration.sql)?;
    transaction.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        (
            migration.version,
            migration.name,
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
    )?;
    transaction.commit()?;
    Ok(())
}

fn current_schema_version(connection: &Connection) -> Result<i64> {
    ensure_migration_table(connection)?;
    let version = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}

fn max_known_migration_version() -> i64 {
    MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

#[cfg(unix)]
fn flock(file: &File, mode: LockMode) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut operation = libc::LOCK_EX;
    if matches!(mode, LockMode::NonBlocking) {
        operation |= libc::LOCK_NB;
    }
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if matches!(mode, LockMode::NonBlocking) {
        if let Some(raw_error) = error.raw_os_error() {
            if raw_error == libc::EWOULDBLOCK || raw_error == libc::EAGAIN {
                return Err(anyhow!("runtime db lock is already held"));
            }
        }
    }
    Err(error.into())
}

#[cfg(unix)]
fn unlock(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(not(unix))]
fn flock(_file: &File, _mode: LockMode) -> Result<()> {
    bail!("runtime db file lock is only implemented on Unix platforms")
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use tempfile::TempDir;

    pub struct TempRuntimeDb {
        pub db: RuntimeDb,
        _temp_dir: TempDir,
    }

    impl TempRuntimeDb {
        pub fn new() -> Result<Self> {
            let temp_dir = tempfile::tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                temp_dir.path().join("state/runtime.sqlite"),
                temp_dir.path().join("state/runtime.lock"),
            )?;
            Ok(Self {
                db,
                _temp_dir: temp_dir,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn temp_paths() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp_dir = tempdir()?;
        let db_path = temp_dir.path().join("state/runtime.sqlite");
        let lock_path = temp_dir.path().join("state/runtime.lock");
        Ok((temp_dir, db_path, lock_path))
    }

    fn task_record(id: &str, agent_id: &str, status: TaskStatus, offset: i64) -> TaskRecord {
        let created_at = Utc::now();
        TaskRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            kind: crate::types::TaskKind::CommandTask,
            status,
            created_at,
            updated_at: created_at + chrono::Duration::seconds(offset),
            parent_message_id: None,
            work_item_id: None,
            summary: Some(id.into()),
            detail: Some(serde_json::json!({
                "cmd": "printf test",
                "output_path": format!("/tmp/{id}.log"),
                "output_summary": format!("{id} summary"),
                "exit_status": 0,
                "accepts_input": true,
                "input_target": "stdin",
            })),
            recovery: None,
        }
    }

    fn external_trigger_record(
        id: &str,
        agent_id: &str,
        status: ExternalTriggerStatus,
        offset: i64,
    ) -> ExternalTriggerRecord {
        let created_at = Utc::now() + chrono::Duration::seconds(offset);
        ExternalTriggerRecord {
            external_trigger_id: id.into(),
            target_agent_id: agent_id.into(),
            waiting_intent_id: Some(format!("wait-{id}")),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            trigger_url: Some(format!("https://example.test/{id}")),
            token_hash: format!("hash-{id}"),
            status,
            created_at,
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        }
    }

    #[test]
    fn runtime_db_fresh_migration_creates_foundation_schema() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;

        let version = db.current_schema_version()?;
        assert_eq!(version, 5);
        for table in [
            "schema_migrations",
            "storage_domains",
            "agents",
            "audit_events",
            "work_items",
            "tasks",
            "external_triggers",
            "messages",
            "transcript_entries",
            "tool_executions",
            "model_requests",
            "model_responses",
            "briefs",
            "delivery_summaries",
            "artifact_metadata",
        ] {
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "missing table {table}");
        }

        Ok(())
    }

    #[test]
    fn runtime_db_migration_is_idempotent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let connection = open_connection(&db_path)?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })?;
        assert_eq!(count, 5);
        assert_eq!(current_schema_version(&connection)?, 5);
        Ok(())
    }

    #[test]
    fn runtime_db_schema_version_comes_from_schema_migrations() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = open_connection(&db_path)?;
        connection.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            (7_i64, "future_test", Utc::now().to_rfc3339()),
        )?;

        assert_eq!(current_schema_version(&connection)?, 7);
        Ok(())
    }

    #[test]
    fn runtime_db_migration_name_mismatch_fails() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (1_i64, "wrong_name", Utc::now().to_rfc3339()),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error.to_string().contains("name mismatch"));
        Ok(())
    }

    #[test]
    fn runtime_db_migration_rejects_newer_schema_version() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        {
            let connection = open_connection(&db_path)?;
            ensure_migration_table(&connection)?;
            connection.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                (
                    max_known_migration_version() + 1,
                    "future_test",
                    Utc::now().to_rfc3339(),
                ),
            )?;
        }

        let error = RuntimeDb::open_and_migrate(&db_path, &lock_path).unwrap_err();
        assert!(error
            .to_string()
            .contains("newer than this binary supports"));
        Ok(())
    }

    #[test]
    fn runtime_db_foreign_keys_are_enabled_per_connection() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let connection = db.connection()?;
        let enabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        assert_eq!(enabled, 1);
        Ok(())
    }

    #[test]
    fn runtime_db_transaction_helper_commits_and_rolls_back() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO storage_domains (
                    domain, schema_version, import_status, canonical_source, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                ("test", 1_i64, "pending", "jsonl", Utc::now().to_rfc3339()),
            )?;
            Ok(())
        })?;
        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);

        let error = db
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO storage_domains (
                        domain, schema_version, import_status, canonical_source, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    (
                        "rollback",
                        1_i64,
                        "pending",
                        "jsonl",
                        Utc::now().to_rfc3339(),
                    ),
                )?;
                Err::<(), anyhow::Error>(anyhow!("force rollback"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "force rollback");

        let connection = db.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM storage_domains", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn storage_domain_import_failure_is_visible_and_retryable() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let error = db
            .evidence()
            .import_legacy(
                vec![serde_json::json!({ "turn_index": 1 })],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("importing legacy storage domain evidence"));
        let failed = db
            .storage_domain("evidence")?
            .expect("failed storage domain row");
        assert_eq!(failed.import_status, "failed");
        assert_eq!(failed.canonical_source, "jsonl");
        assert!(failed
            .source_checkpoint_json
            .as_deref()
            .is_some_and(|checkpoint| checkpoint.contains("restart runtime to retry")));

        db.evidence()
            .import_legacy(Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())?;
        let complete = db
            .storage_domain("evidence")?
            .expect("complete storage domain row");
        assert_eq!(complete.import_status, "complete");
        assert_eq!(complete.canonical_source, "jsonl+db-index");
        Ok(())
    }

    #[test]
    fn cutover_diagnostics_detect_missing_failed_and_mixed_sources() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;

        let missing = db.diagnose_cutover(RuntimeDb::expected_storage_domains())?;
        assert!(missing
            .iter()
            .any(|diagnostic| diagnostic.contains("storage domain work_items is missing")));

        db.transaction(|tx| {
            upsert_storage_domain(tx, "work_items", "complete", "jsonl", None)?;
            upsert_storage_domain(
                tx,
                "tasks",
                "failed",
                "jsonl",
                Some(serde_json::json!({ "error": "forced failure" })),
            )?;
            upsert_storage_domain(tx, "external_triggers", "complete", "db", None)?;
            upsert_storage_domain(tx, "evidence", "complete", "jsonl+db-index", None)?;
            upsert_storage_domain(tx, "audit_events", "complete", "jsonl+db-index", None)?;
            Ok(())
        })?;

        let diagnostics = db.diagnose_cutover(RuntimeDb::expected_storage_domains())?;
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("storage domain work_items has canonical_source=jsonl")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("storage domain tasks import failed")
                && diagnostic.contains("forced failure")
        }));
        assert!(db
            .validate_expected_storage_domains(RuntimeDb::expected_storage_domains())
            .is_err());
        Ok(())
    }

    #[test]
    fn runtime_db_temp_helper_uses_isolated_state_dir() -> Result<()> {
        let temp_db = test_support::TempRuntimeDb::new()?;
        assert!(temp_db.db.path().ends_with("state/runtime.sqlite"));
        assert!(temp_db.db.lock_path().ends_with("state/runtime.lock"));
        assert_eq!(temp_db.db.current_schema_version()?, 5);
        Ok(())
    }

    #[test]
    fn external_trigger_import_normalizes_to_one_default_active_per_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let older =
            external_trigger_record("trigger-older", "agent-a", ExternalTriggerStatus::Active, 0);
        let newer = external_trigger_record(
            "trigger-newer",
            "agent-a",
            ExternalTriggerStatus::Active,
            10,
        );

        db.external_triggers()
            .import_legacy(vec![older.clone(), newer.clone()])?;
        db.external_triggers()
            .import_legacy(vec![older.clone(), newer.clone()])?;

        let active = db
            .external_triggers()
            .active_default_for_agent("agent-a")?
            .expect("active default trigger");
        assert_eq!(active.external_trigger_id, "trigger-newer");
        assert_eq!(active.scope, ExternalTriggerScope::Agent);
        assert_eq!(active.delivery_mode, CallbackDeliveryMode::WakeHint);
        assert_eq!(active.waiting_intent_id, None);

        let all = db.external_triggers().latest_for_agent("agent-a")?;
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.into_iter()
                .filter(|record| record.status == ExternalTriggerStatus::Active)
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn external_trigger_upsert_tracks_delivery_and_token_lookup() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;
        let mut trigger = external_trigger_record(
            "trigger-active",
            "agent-a",
            ExternalTriggerStatus::Active,
            0,
        );
        trigger.delivery_mode = CallbackDeliveryMode::WakeHint;
        trigger.waiting_intent_id = None;
        db.external_triggers().upsert(&trigger)?;

        trigger.delivery_count = 2;
        trigger.last_delivered_at = Some(trigger.created_at + chrono::Duration::seconds(30));
        db.external_triggers().upsert(&trigger)?;

        let by_token = db
            .external_triggers()
            .active_by_token_hash("hash-trigger-active")?
            .expect("active trigger by token");
        assert_eq!(by_token.delivery_count, 2);
        assert_eq!(by_token.last_delivered_at, trigger.last_delivered_at);
        Ok(())
    }

    #[test]
    fn external_trigger_upsert_does_not_revert_newer_revocation() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.external_triggers().import_legacy(Vec::new())?;
        let active = external_trigger_record(
            "trigger-active",
            "agent-a",
            ExternalTriggerStatus::Active,
            0,
        );
        db.external_triggers().upsert(&active)?;

        let mut revoked = active.clone();
        revoked.status = ExternalTriggerStatus::Revoked;
        revoked.revoked_at = Some(active.created_at + chrono::Duration::seconds(30));
        db.external_triggers().upsert(&revoked)?;
        db.external_triggers().upsert(&active)?;

        let latest = db
            .external_triggers()
            .latest("trigger-active")?
            .expect("latest trigger");
        assert_eq!(latest.status, ExternalTriggerStatus::Revoked);
        assert_eq!(latest.revoked_at, revoked.revoked_at);
        Ok(())
    }

    #[test]
    fn work_item_import_is_idempotent_and_preserves_latest_revision() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut older = WorkItemRecord::new("agent-a", "older objective", WorkItemState::Open);
        older.id = "work-test".into();
        older.revision = 1;
        older.updated_at = older.created_at;
        let mut newer = older.clone();
        newer.objective = "newer objective".into();
        newer.revision = 3;
        newer.updated_at = older.updated_at + chrono::Duration::seconds(10);

        db.work_items()
            .import_legacy(vec![older.clone(), newer.clone()], Some("work-test"))?;
        db.work_items()
            .import_legacy(vec![older.clone(), newer.clone()], Some("work-test"))?;

        let imported = db
            .work_items()
            .latest("work-test")?
            .expect("work item imported");
        assert_eq!(imported.revision, 3);
        assert_eq!(imported.objective, "newer objective");
        let connection = db.connection()?;
        let rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM work_items", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        let current_focus: i64 = connection.query_row(
            "SELECT current_focus FROM work_items WHERE work_item_id = 'work-test'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(current_focus, 1);
        Ok(())
    }

    #[test]
    fn work_item_upsert_rejects_revision_rollback() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.work_items().import_legacy(Vec::new(), None)?;
        let mut current = WorkItemRecord::new("agent-a", "current", WorkItemState::Open);
        current.id = "work-revision".into();
        current.revision = 5;
        db.work_items().upsert(&current, false)?;

        let mut stale = current.clone();
        stale.objective = "stale".into();
        stale.revision = 4;
        stale.updated_at = current.updated_at + chrono::Duration::seconds(10);
        db.work_items().upsert(&stale, false)?;

        let persisted = db
            .work_items()
            .latest("work-revision")?
            .expect("work item persisted");
        assert_eq!(persisted.revision, 5);
        assert_eq!(persisted.objective, "current");
        Ok(())
    }

    #[test]
    fn work_item_listing_is_partitioned_by_agent() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.work_items().import_legacy(Vec::new(), None)?;
        let mut first = WorkItemRecord::new("agent-a", "first", WorkItemState::Open);
        first.id = "work-first".into();
        let mut second = WorkItemRecord::new("agent-b", "second", WorkItemState::Open);
        second.id = "work-second".into();
        db.work_items().upsert(&first, false)?;
        db.work_items().upsert(&second, false)?;

        let agent_items = db.work_items().latest_for_agent("agent-a", 20)?;
        assert_eq!(agent_items.len(), 1);
        assert_eq!(agent_items[0].id, "work-first");
        Ok(())
    }

    #[test]
    fn task_import_is_idempotent_and_preserves_latest_lifecycle_state() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let queued = task_record("task-import", "agent-a", TaskStatus::Queued, 0);
        let completed = task_record("task-import", "agent-a", TaskStatus::Completed, 10);

        db.tasks()
            .import_legacy(vec![queued.clone(), completed.clone()])?;
        db.tasks().import_legacy(vec![queued, completed])?;

        let imported = db.tasks().latest("task-import")?.expect("task imported");
        assert_eq!(imported.status, TaskStatus::Completed);
        assert_eq!(
            imported
                .detail
                .as_ref()
                .and_then(|detail| detail.get("output_path"))
                .and_then(serde_json::Value::as_str),
            Some("/tmp/task-import.log")
        );
        let connection = db.connection()?;
        let rows: i64 = connection.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        let terminal_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'completed' AND completed_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(terminal_rows, 1);
        Ok(())
    }

    #[test]
    fn task_import_merges_legacy_metadata_when_latest_update_is_sparse() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let queued = task_record("task-sparse", "agent-a", TaskStatus::Queued, 0);
        let mut completed = task_record("task-sparse", "agent-a", TaskStatus::Completed, 10);
        completed.summary = None;
        completed.detail = None;
        completed.recovery = None;

        db.tasks().import_legacy(vec![queued, completed])?;

        let imported = db.tasks().latest("task-sparse")?.expect("task imported");
        assert_eq!(imported.status, TaskStatus::Completed);
        assert_eq!(imported.summary.as_deref(), Some("task-sparse"));
        assert_eq!(
            imported
                .detail
                .as_ref()
                .and_then(|detail| detail.get("output_path"))
                .and_then(serde_json::Value::as_str),
            Some("/tmp/task-sparse.log")
        );
        Ok(())
    }

    #[test]
    fn task_parent_agent_column_is_only_set_for_child_agent_tasks() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let command = task_record("task-command", "agent-a", TaskStatus::Running, 0);
        let mut child = task_record("task-child", "agent-a", TaskStatus::Running, 1);
        child.detail = Some(serde_json::json!({
            "child_agent_id": "child-a",
            "input_target": "child_followup",
        }));

        db.tasks().upsert(&command)?;
        db.tasks().upsert(&child)?;

        let connection = db.connection()?;
        let command_parent: Option<String> = connection.query_row(
            "SELECT parent_agent_id FROM tasks WHERE task_id = 'task-command'",
            [],
            |row| row.get(0),
        )?;
        let child_parent: Option<String> = connection.query_row(
            "SELECT parent_agent_id FROM tasks WHERE task_id = 'task-child'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(command_parent, None);
        assert_eq!(child_parent.as_deref(), Some("agent-a"));
        Ok(())
    }

    #[test]
    fn task_payload_json_slimguards_large_preview_fields() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let mut task = task_record("task-large", "agent-a", TaskStatus::Running, 0);
        task.detail = Some(serde_json::json!({
            "output_path": "/tmp/task-large.log",
            "initial_output": "i".repeat(TASK_PAYLOAD_STRING_LIMIT + 10),
            "output_summary": "s".repeat(TASK_PAYLOAD_STRING_LIMIT + 10),
            "lines": (0..(TASK_PAYLOAD_ARRAY_LIMIT + 10)).collect::<Vec<_>>(),
        }));

        db.tasks().upsert(&task)?;

        let connection = db.connection()?;
        let (payload_json, result_summary): (String, Option<String>) = connection.query_row(
            "SELECT payload_json, result_summary FROM tasks WHERE task_id = 'task-large'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
        let detail = &payload["detail"];
        assert!(detail.get("initial_output").is_none());
        assert_eq!(
            detail["output_summary"].as_str().expect("summary").len(),
            TASK_PAYLOAD_STRING_LIMIT
        );
        assert_eq!(
            detail["lines"].as_array().expect("lines").len(),
            TASK_PAYLOAD_ARRAY_LIMIT
        );
        assert_eq!(
            result_summary.expect("result summary").len(),
            TASK_PAYLOAD_STRING_LIMIT
        );
        Ok(())
    }

    #[test]
    fn task_active_listing_is_partitioned_by_agent_and_excludes_terminal() -> Result<()> {
        let (_temp_dir, db_path, lock_path) = temp_paths()?;
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.tasks().import_legacy(Vec::new())?;
        db.tasks().upsert(&task_record(
            "agent-a-running",
            "agent-a",
            TaskStatus::Running,
            1,
        ))?;
        db.tasks().upsert(&task_record(
            "agent-a-completed",
            "agent-a",
            TaskStatus::Completed,
            2,
        ))?;
        db.tasks().upsert(&task_record(
            "agent-b-running",
            "agent-b",
            TaskStatus::Running,
            3,
        ))?;

        let active = db.tasks().active_for_agent("agent-a", 20)?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "agent-a-running");
        let all_agent_a = db.tasks().latest_for_agent("agent-a", 20)?;
        assert_eq!(all_agent_a.len(), 2);
        Ok(())
    }

    #[test]
    fn runtime_db_lock_rejects_second_nonblocking_holder() -> Result<()> {
        if let Ok(lock_path) = std::env::var("HOLON_RUNTIME_DB_LOCK_CHILD_PATH") {
            RuntimeDbLock::try_lock(lock_path).expect_err("second process should not get lock");
            return Ok(());
        }

        let temp_dir = tempdir()?;
        let lock_path = temp_dir.path().join("state/runtime.lock");
        let first = RuntimeDbLock::lock(&lock_path)?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_db::tests::runtime_db_lock_rejects_second_nonblocking_holder")
            .arg("--nocapture")
            .env("HOLON_RUNTIME_DB_LOCK_CHILD_PATH", &lock_path)
            .output()?;
        assert!(
            output.status.success(),
            "child lock assertion failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        drop(first);

        let second = RuntimeDbLock::try_lock(&lock_path)?;
        assert_eq!(second.path(), lock_path.as_path());
        Ok(())
    }
}
