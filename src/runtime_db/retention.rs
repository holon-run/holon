//! Bounded runtime database retention and explicit space reclamation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::Instant,
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime_db::{
        evidence::insert_runtime_index_changes_tx, write_queue::RuntimeDbWriteContext, RuntimeDb,
        RuntimeDbLock, RuntimeIndexChange, RuntimeIndexOperation,
    },
    tool::helpers::{command_output_source_ref, command_receipt_source_ref},
    types::{
        BriefRecord, ContextEpisodeRecord, TaskRecord, ToolExecutionRecord, ToolResultData,
        TranscriptEntry, TurnRecord, WaitConditionRecord, WorkItemRecord, WorkItemRefStatus,
    },
};

pub const DEFAULT_AUDIT_EVENTS_DAYS: u64 = 30;
pub const DEFAULT_TRANSCRIPT_ENTRIES_DAYS: u64 = 90;
pub const DEFAULT_TOOL_EXECUTIONS_DAYS: u64 = 90;
pub const DEFAULT_AUDIT_EVENTS_MIN_ROWS_PER_SCOPE: usize = 4096;
pub const DEFAULT_TRANSCRIPT_ENTRIES_MIN_ROWS: usize = 20_000;
pub const DEFAULT_TOOL_EXECUTIONS_MIN_ROWS: usize = 15_000;
pub const DEFAULT_RETENTION_INTERVAL_HOURS: u64 = 6;
pub const DEFAULT_INCREMENTAL_VACUUM_PAGES: u32 = 256;
pub const RETENTION_DELETE_BATCH_ROWS: usize = 1000;

const RECENT_COMPLETED_TURNS_PER_AGENT: usize = 64;
const MAX_RETENTION_DAYS: u64 = 3650;
const MAX_RETENTION_ROWS: usize = 10_000_000;
const MAX_RETENTION_INTERVAL_HOURS: u64 = 24 * 30;
const MAX_INCREMENTAL_VACUUM_PAGES: u32 = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeDbRetentionPolicy {
    pub enabled: bool,
    pub audit_events_days: u64,
    pub transcript_entries_days: u64,
    pub tool_executions_days: u64,
    pub audit_events_min_rows_per_scope: usize,
    pub transcript_entries_min_rows: usize,
    pub tool_executions_min_rows: usize,
    pub interval_hours: u64,
    pub incremental_vacuum_pages: u32,
}

impl Default for RuntimeDbRetentionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            audit_events_days: DEFAULT_AUDIT_EVENTS_DAYS,
            transcript_entries_days: DEFAULT_TRANSCRIPT_ENTRIES_DAYS,
            tool_executions_days: DEFAULT_TOOL_EXECUTIONS_DAYS,
            audit_events_min_rows_per_scope: DEFAULT_AUDIT_EVENTS_MIN_ROWS_PER_SCOPE,
            transcript_entries_min_rows: DEFAULT_TRANSCRIPT_ENTRIES_MIN_ROWS,
            tool_executions_min_rows: DEFAULT_TOOL_EXECUTIONS_MIN_ROWS,
            interval_hours: DEFAULT_RETENTION_INTERVAL_HOURS,
            incremental_vacuum_pages: DEFAULT_INCREMENTAL_VACUUM_PAGES,
        }
    }
}

impl RuntimeDbRetentionPolicy {
    pub fn validate(self) -> Result<Self> {
        validate_positive_bounded(
            "runtime.retention.audit_events_days",
            self.audit_events_days,
            MAX_RETENTION_DAYS,
        )?;
        validate_positive_bounded(
            "runtime.retention.transcript_entries_days",
            self.transcript_entries_days,
            MAX_RETENTION_DAYS,
        )?;
        validate_positive_bounded(
            "runtime.retention.tool_executions_days",
            self.tool_executions_days,
            MAX_RETENTION_DAYS,
        )?;
        if self.audit_events_min_rows_per_scope < crate::http::MAX_EVENT_STREAM_WINDOW {
            bail!(
                "runtime.retention.audit_events_min_rows_per_scope must be at least {}",
                crate::http::MAX_EVENT_STREAM_WINDOW
            );
        }
        validate_positive_bounded(
            "runtime.retention.audit_events_min_rows_per_scope",
            self.audit_events_min_rows_per_scope,
            MAX_RETENTION_ROWS,
        )?;
        validate_positive_bounded(
            "runtime.retention.transcript_entries_min_rows",
            self.transcript_entries_min_rows,
            MAX_RETENTION_ROWS,
        )?;
        validate_positive_bounded(
            "runtime.retention.tool_executions_min_rows",
            self.tool_executions_min_rows,
            MAX_RETENTION_ROWS,
        )?;
        validate_positive_bounded(
            "runtime.retention.interval_hours",
            self.interval_hours,
            MAX_RETENTION_INTERVAL_HOURS,
        )?;
        validate_positive_bounded(
            "runtime.retention.incremental_vacuum_pages",
            self.incremental_vacuum_pages,
            MAX_INCREMENTAL_VACUUM_PAGES,
        )?;
        Ok(self)
    }
}

fn validate_positive_bounded<T>(key: &str, value: T, max: T) -> Result<()>
where
    T: Copy + Ord + Default + std::fmt::Display,
{
    if value == T::default() || value > max {
        bail!("{key} must be between 1 and {max}");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimeDbRetentionTableReport {
    pub observed_rows: u64,
    pub candidate_rows: u64,
    pub protected_rows: u64,
    pub planned_delete_rows: u64,
    pub deleted_rows: u64,
    pub skipped_below_minimum_rows: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub protection_reasons: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDbRetentionReport {
    pub dry_run: bool,
    pub enabled: bool,
    pub started_at: DateTime<Utc>,
    pub elapsed_ms: u64,
    pub policy: RuntimeDbRetentionPolicy,
    pub audit_events: RuntimeDbRetentionTableReport,
    pub transcript_entries: RuntimeDbRetentionTableReport,
    pub tool_executions: RuntimeDbRetentionTableReport,
    pub audit_scopes_observed: u64,
    pub audit_scopes_skipped_below_minimum_rows: u64,
    pub page_size: u64,
    pub page_count_before: u64,
    pub freelist_pages_before: u64,
    pub page_count_after: u64,
    pub freelist_pages_after: u64,
    pub estimated_reclaimable_bytes_before: u64,
    pub incremental_vacuum_pages_requested: u32,
    pub incremental_vacuum_ran: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDbCompactReport {
    pub started_at: DateTime<Utc>,
    pub elapsed_ms: u64,
    pub file_bytes_before: u64,
    pub file_bytes_after: u64,
    pub page_size: u64,
    pub page_count_before: u64,
    pub page_count_after: u64,
    pub freelist_pages_before: u64,
    pub freelist_pages_after: u64,
    pub estimated_reclaimable_bytes_before: u64,
    pub auto_vacuum_before: u32,
    pub auto_vacuum_after: u32,
}

#[derive(Default)]
struct StrongRoots {
    turn_ids: BTreeSet<String>,
    message_ids: BTreeSet<String>,
    task_ids: BTreeSet<String>,
    work_item_ids: BTreeSet<String>,
    transcript_entry_ids: BTreeSet<String>,
    tool_execution_ids: BTreeSet<String>,
}

struct EvidenceCandidate {
    id: String,
    turn_id: Option<String>,
    message_id: Option<String>,
    task_id: Option<String>,
    work_item_id: Option<String>,
    payload_json: Option<String>,
}

struct DatabasePageStats {
    page_size: u64,
    page_count: u64,
    freelist_pages: u64,
}

impl RuntimeDb {
    pub fn plan_retention(
        &self,
        policy: RuntimeDbRetentionPolicy,
        now: DateTime<Utc>,
    ) -> Result<RuntimeDbRetentionReport> {
        let policy = policy.validate()?;
        let connection = self.connection()?;
        build_retention_report(&connection, &policy, now, true, None)
    }

    pub fn run_retention_pass(
        &self,
        policy: RuntimeDbRetentionPolicy,
        now: DateTime<Utc>,
    ) -> Result<RuntimeDbRetentionReport> {
        let policy = policy.validate()?;
        if !policy.enabled {
            let connection = self.connection()?;
            return build_retention_report(&connection, &policy, now, true, None);
        }
        let transaction_policy = policy.clone();
        let mut report = self.transaction_with_context(
            RuntimeDbWriteContext::sync("runtime_db.retention", "retained_evidence"),
            |tx| build_retention_report(tx, &transaction_policy, now, false, Some(tx)),
        )?;
        let connection = self.connection()?;
        if pragma_u64(&connection, "auto_vacuum")? == 2 {
            connection.execute_batch(&format!(
                "PRAGMA incremental_vacuum({});",
                policy.incremental_vacuum_pages
            ))?;
            report.incremental_vacuum_ran = true;
        }
        let stats = database_page_stats(&connection)?;
        report.page_count_after = stats.page_count;
        report.freelist_pages_after = stats.freelist_pages;
        report.elapsed_ms = elapsed_ms_since(report.started_at);
        persist_last_report(self, &report)?;
        Ok(report)
    }

    pub fn compact_offline(
        db_path: &Path,
        migration_lock_path: &Path,
        maintenance_lock_path: &Path,
    ) -> Result<RuntimeDbCompactReport> {
        let started_at = Utc::now();
        let started = Instant::now();
        let _maintenance_lock = RuntimeDbLock::try_lock(maintenance_lock_path)
            .context("runtime database compact requires the daemon to be stopped")?;
        let db = RuntimeDb::open_and_migrate(db_path, migration_lock_path)?;
        let connection = db.connection()?;
        let before = database_page_stats(&connection)?;
        let file_bytes_before = file_size(db_path)?;
        let auto_vacuum_before = pragma_u64(&connection, "auto_vacuum")? as u32;
        connection.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE);
             PRAGMA auto_vacuum = INCREMENTAL;
             VACUUM;",
        )?;
        let after = database_page_stats(&connection)?;
        Ok(RuntimeDbCompactReport {
            started_at,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            file_bytes_before,
            file_bytes_after: file_size(db_path)?,
            page_size: before.page_size,
            page_count_before: before.page_count,
            page_count_after: after.page_count,
            freelist_pages_before: before.freelist_pages,
            freelist_pages_after: after.freelist_pages,
            estimated_reclaimable_bytes_before: before
                .page_size
                .saturating_mul(before.freelist_pages),
            auto_vacuum_before,
            auto_vacuum_after: pragma_u64(&connection, "auto_vacuum")? as u32,
        })
    }
}

fn build_retention_report(
    connection: &Connection,
    policy: &RuntimeDbRetentionPolicy,
    now: DateTime<Utc>,
    dry_run: bool,
    delete_tx: Option<&Transaction<'_>>,
) -> Result<RuntimeDbRetentionReport> {
    let started = Instant::now();
    let stats = database_page_stats(connection)?;
    let roots = collect_strong_roots(connection)?;
    let (audit_events, audit_scopes_observed, audit_scopes_skipped) = plan_audit_events(
        connection,
        delete_tx,
        now - retention_days(policy.audit_events_days)?,
        policy.audit_events_min_rows_per_scope,
        dry_run,
    )?;
    let transcript_entries = plan_evidence_table(
        connection,
        delete_tx,
        "transcript_entries",
        now - retention_days(policy.transcript_entries_days)?,
        policy.transcript_entries_min_rows,
        &roots,
        dry_run,
        false,
    )?;
    let tool_executions = plan_evidence_table(
        connection,
        delete_tx,
        "tool_executions",
        now - retention_days(policy.tool_executions_days)?,
        policy.tool_executions_min_rows,
        &roots,
        dry_run,
        true,
    )?;
    Ok(RuntimeDbRetentionReport {
        dry_run,
        enabled: policy.enabled,
        started_at: now,
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        policy: policy.clone(),
        audit_events,
        transcript_entries,
        tool_executions,
        audit_scopes_observed,
        audit_scopes_skipped_below_minimum_rows: audit_scopes_skipped,
        page_size: stats.page_size,
        page_count_before: stats.page_count,
        freelist_pages_before: stats.freelist_pages,
        page_count_after: stats.page_count,
        freelist_pages_after: stats.freelist_pages,
        estimated_reclaimable_bytes_before: stats.page_size.saturating_mul(stats.freelist_pages),
        incremental_vacuum_pages_requested: policy.incremental_vacuum_pages,
        incremental_vacuum_ran: false,
    })
}

fn plan_audit_events(
    connection: &Connection,
    delete_tx: Option<&Transaction<'_>>,
    cutoff: DateTime<Utc>,
    minimum_rows: usize,
    dry_run: bool,
) -> Result<(RuntimeDbRetentionTableReport, u64, u64)> {
    let mut report = RuntimeDbRetentionTableReport::default();
    let mut statement = connection.prepare(
        "SELECT agent_id, COUNT(*) FROM audit_events GROUP BY agent_id ORDER BY agent_id",
    )?;
    let scopes = statement.query_map([], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, u64>(1)?))
    })?;
    let mut observed_scopes = 0_u64;
    let mut skipped_scopes = 0_u64;
    for scope in scopes {
        let (agent_id, observed_rows) = scope?;
        observed_scopes += 1;
        report.observed_rows = report.observed_rows.saturating_add(observed_rows);
        if observed_rows <= minimum_rows as u64 {
            skipped_scopes += 1;
            continue;
        }
        let floor_offset = i64::try_from(minimum_rows - 1).unwrap_or(i64::MAX);
        let floor_seq = audit_floor_sequence(connection, agent_id.as_deref(), floor_offset)?;
        let age_seq = audit_age_sequence(connection, agent_id.as_deref(), cutoff)?;
        let Some(first_retained_seq) = earliest_sequence(floor_seq, age_seq) else {
            continue;
        };
        let candidates =
            audit_candidate_count(connection, agent_id.as_deref(), first_retained_seq)?;
        report.candidate_rows = report.candidate_rows.saturating_add(candidates);
        let planned = candidates.min(RETENTION_DELETE_BATCH_ROWS as u64);
        report.planned_delete_rows = report.planned_delete_rows.saturating_add(planned);
        if !dry_run && planned > 0 {
            let tx =
                delete_tx.ok_or_else(|| anyhow!("audit retention delete requires transaction"))?;
            let deleted =
                delete_audit_prefix(tx, agent_id.as_deref(), first_retained_seq, planned)?;
            report.deleted_rows = report.deleted_rows.saturating_add(deleted as u64);
        }
    }
    report.skipped_below_minimum_rows = observed_scopes > 0 && observed_scopes == skipped_scopes;
    Ok((report, observed_scopes, skipped_scopes))
}

fn audit_floor_sequence(
    connection: &Connection,
    agent_id: Option<&str>,
    floor_offset: i64,
) -> Result<Option<i64>> {
    if let Some(agent_id) = agent_id {
        connection
            .query_row(
                "SELECT event_seq FROM audit_events
                 WHERE agent_id = ?1 ORDER BY event_seq DESC LIMIT 1 OFFSET ?2",
                params![agent_id, floor_offset],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    } else {
        connection
            .query_row(
                "SELECT event_seq FROM audit_events
                 WHERE agent_id IS NULL ORDER BY event_seq DESC LIMIT 1 OFFSET ?1",
                [floor_offset],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
}

fn audit_age_sequence(
    connection: &Connection,
    agent_id: Option<&str>,
    cutoff: DateTime<Utc>,
) -> Result<Option<i64>> {
    let cutoff = timestamp(cutoff);
    if let Some(agent_id) = agent_id {
        connection
            .query_row(
                "SELECT MIN(event_seq) FROM audit_events
                 WHERE agent_id = ?1 AND created_at >= ?2",
                params![agent_id, cutoff],
                |row| row.get(0),
            )
            .map_err(Into::into)
    } else {
        connection
            .query_row(
                "SELECT MIN(event_seq) FROM audit_events
                 WHERE agent_id IS NULL AND created_at >= ?1",
                [cutoff],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}

fn audit_candidate_count(
    connection: &Connection,
    agent_id: Option<&str>,
    first_retained_seq: i64,
) -> Result<u64> {
    if let Some(agent_id) = agent_id {
        connection
            .query_row(
                "SELECT COUNT(*) FROM audit_events
                 WHERE agent_id = ?1 AND event_seq < ?2",
                params![agent_id, first_retained_seq],
                |row| row.get(0),
            )
            .map_err(Into::into)
    } else {
        connection
            .query_row(
                "SELECT COUNT(*) FROM audit_events
                 WHERE agent_id IS NULL AND event_seq < ?1",
                [first_retained_seq],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}

fn delete_audit_prefix(
    tx: &Transaction<'_>,
    agent_id: Option<&str>,
    first_retained_seq: i64,
    limit: u64,
) -> Result<usize> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    if let Some(agent_id) = agent_id {
        tx.execute(
            "DELETE FROM audit_events WHERE audit_event_id IN (
               SELECT audit_event_id FROM audit_events
               WHERE agent_id = ?1 AND event_seq < ?2
               ORDER BY event_seq ASC LIMIT ?3
             )",
            params![agent_id, first_retained_seq, limit],
        )
        .map_err(Into::into)
    } else {
        tx.execute(
            "DELETE FROM audit_events WHERE audit_event_id IN (
               SELECT audit_event_id FROM audit_events
               WHERE agent_id IS NULL AND event_seq < ?1
               ORDER BY event_seq ASC LIMIT ?2
             )",
            params![first_retained_seq, limit],
        )
        .map_err(Into::into)
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_evidence_table(
    connection: &Connection,
    delete_tx: Option<&Transaction<'_>>,
    table: &'static str,
    cutoff: DateTime<Utc>,
    minimum_rows: usize,
    roots: &StrongRoots,
    dry_run: bool,
    delete_index_sources: bool,
) -> Result<RuntimeDbRetentionTableReport> {
    let mut report = RuntimeDbRetentionTableReport::default();
    report.observed_rows =
        connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
    if report.observed_rows <= minimum_rows as u64 {
        report.skipped_below_minimum_rows = true;
        return Ok(report);
    }
    let removable_capacity = report.observed_rows.saturating_sub(minimum_rows as u64);
    let sql = format!(
        "SELECT evidence_id, turn_id, message_id, task_id, work_item_id,
                CASE WHEN ?2 THEN payload_json ELSE NULL END
         FROM {table}
         WHERE created_at < ?1
         ORDER BY created_at ASC, evidence_id ASC"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![timestamp(cutoff), delete_index_sources], |row| {
        Ok(EvidenceCandidate {
            id: row.get(0)?,
            turn_id: row.get(1)?,
            message_id: row.get(2)?,
            task_id: row.get(3)?,
            work_item_id: row.get(4)?,
            payload_json: row.get(5)?,
        })
    })?;
    let mut deletions = Vec::new();
    for candidate in rows {
        let candidate = candidate?;
        report.candidate_rows = report.candidate_rows.saturating_add(1);
        if let Some(reason) = candidate_protection_reason(&candidate, roots) {
            report.protected_rows = report.protected_rows.saturating_add(1);
            *report.protection_reasons.entry(reason.into()).or_default() += 1;
        } else if deletions.len() < RETENTION_DELETE_BATCH_ROWS
            && deletions.len() < removable_capacity as usize
        {
            deletions.push(candidate);
        }
    }
    report.planned_delete_rows = deletions.len() as u64;
    if dry_run || deletions.is_empty() {
        return Ok(report);
    }
    let tx = delete_tx.ok_or_else(|| anyhow!("{table} retention delete requires transaction"))?;
    for candidate in &deletions {
        if delete_index_sources {
            let record: ToolExecutionRecord =
                serde_json::from_str(candidate.payload_json.as_deref().ok_or_else(|| {
                    anyhow!("tool execution payload missing for {}", candidate.id)
                })?)?;
            insert_runtime_index_changes_tx(tx, &tool_index_delete_changes(&record))?;
        }
        tx.execute(
            &format!("DELETE FROM {table} WHERE evidence_id = ?1"),
            [&candidate.id],
        )?;
    }
    report.deleted_rows = deletions.len() as u64;
    Ok(report)
}

fn candidate_protection_reason(
    candidate: &EvidenceCandidate,
    roots: &StrongRoots,
) -> Option<&'static str> {
    if candidate
        .turn_id
        .as_ref()
        .is_some_and(|id| roots.turn_ids.contains(id))
    {
        Some("turn")
    } else if candidate
        .message_id
        .as_ref()
        .is_some_and(|id| roots.message_ids.contains(id))
    {
        Some("message")
    } else if candidate
        .task_id
        .as_ref()
        .is_some_and(|id| roots.task_ids.contains(id))
    {
        Some("task")
    } else if candidate
        .work_item_id
        .as_ref()
        .is_some_and(|id| roots.work_item_ids.contains(id))
    {
        Some("work_item")
    } else if roots.tool_execution_ids.contains(&candidate.id) {
        Some("source_ref")
    } else if roots.transcript_entry_ids.contains(&candidate.id) {
        Some("transcript_ref")
    } else {
        None
    }
}

fn collect_strong_roots(connection: &Connection) -> Result<StrongRoots> {
    let mut roots = StrongRoots::default();
    collect_turn_roots(connection, &mut roots)?;
    collect_work_item_roots(connection, &mut roots)?;
    collect_task_roots(connection, &mut roots)?;
    collect_wait_roots(connection, &mut roots)?;
    collect_queue_roots(connection, &mut roots)?;
    collect_episode_roots(connection, &mut roots)?;
    collect_brief_roots(connection, &mut roots)?;
    collect_transcript_tool_roots(connection, &mut roots)?;
    collect_outbox_roots(connection, &mut roots)?;
    Ok(roots)
}

fn collect_turn_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT payload_json FROM turn_records
         ORDER BY agent_id ASC, turn_index DESC, created_at DESC",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut completed_per_agent = BTreeMap::<String, usize>::new();
    for row in rows {
        let record: TurnRecord = serde_json::from_str(&row?)?;
        let protect = if record.terminal.is_none() {
            true
        } else {
            let count = completed_per_agent
                .entry(record.agent_id.clone())
                .or_default();
            let protect = *count < RECENT_COMPLETED_TURNS_PER_AGENT;
            *count += 1;
            protect
        };
        if protect {
            roots.turn_ids.insert(record.turn_id);
        }
    }
    Ok(())
}

fn collect_work_item_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement = connection.prepare("SELECT payload_json FROM work_items")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let record: WorkItemRecord = serde_json::from_str(&row?)?;
        if record.state == crate::types::WorkItemState::Open {
            roots.work_item_ids.insert(record.id.clone());
        }
        for work_ref in record
            .work_refs
            .iter()
            .filter(|work_ref| work_ref.status == WorkItemRefStatus::Active)
        {
            if let Some(source_ref) = work_ref.source_ref.as_deref() {
                collect_source_ref(source_ref, roots);
            }
        }
    }
    Ok(())
}

fn collect_task_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT payload_json FROM tasks
         WHERE status IN ('queued', 'running', 'cancelling')",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let record: TaskRecord = serde_json::from_str(&row?)?;
        roots.task_ids.insert(record.id);
        if let Some(message_id) = record.parent_message_id {
            roots.message_ids.insert(message_id);
        }
        if let Some(work_item_id) = record.work_item_id {
            roots.work_item_ids.insert(work_item_id);
        }
    }
    Ok(())
}

fn collect_wait_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement =
        connection.prepare("SELECT payload_json FROM wait_conditions WHERE status = 'active'")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let record: WaitConditionRecord = serde_json::from_str(&row?)?;
        if let Some(turn_id) = record.turn_id {
            roots.turn_ids.insert(turn_id);
        }
        if let Some(work_item_id) = record.work_item_id {
            roots.work_item_ids.insert(work_item_id);
        }
        if let Some(subject_ref) = record.subject_ref.as_deref() {
            collect_source_ref(subject_ref, roots);
        }
    }
    Ok(())
}

fn collect_queue_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT message_id FROM queue_entries
         WHERE status IN ('queued', 'interrupted', 'dequeued', 'interjected')",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        roots.message_ids.insert(row?);
    }
    Ok(())
}

fn collect_episode_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement = connection.prepare("SELECT payload_json FROM context_episode_anchors")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let record: ContextEpisodeRecord = serde_json::from_str(&row?)?;
        roots.turn_ids.extend(record.source_turn_ids);
        for source_ref in record.source_refs {
            collect_source_ref(&source_ref, roots);
        }
    }
    Ok(())
}

fn collect_brief_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement = connection.prepare("SELECT payload_json FROM briefs")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let record: BriefRecord = serde_json::from_str(&row?)?;
        if let Some(entry_id) = record.content_source.transcript_entry_id() {
            roots.transcript_entry_ids.insert(entry_id.to_string());
        }
        if let Some(entry_id) = record.finalizes_assistant_round_id {
            roots.transcript_entry_ids.insert(entry_id);
        }
    }
    Ok(())
}

fn collect_transcript_tool_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement = connection
        .prepare("SELECT payload_json FROM transcript_entries WHERE kind = 'tool_results'")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let record: TranscriptEntry = serde_json::from_str(&row?)?;
        let Ok(ToolResultData::RefsWithWrapper { refs }) =
            serde_json::from_value::<ToolResultData>(record.data)
        else {
            continue;
        };
        for tool_execution_id in refs
            .into_iter()
            .filter_map(|reference| reference.tool_execution_id)
        {
            roots.tool_execution_ids.insert(tool_execution_id);
        }
    }
    Ok(())
}

fn collect_outbox_roots(connection: &Connection, roots: &mut StrongRoots) -> Result<()> {
    let mut statement =
        connection.prepare("SELECT source_id, source_ref FROM runtime_index_outbox")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (source_id, source_ref) = row?;
        collect_source_ref(&source_id, roots);
        collect_source_ref(&source_ref, roots);
    }
    Ok(())
}

fn collect_source_ref(source_ref: &str, roots: &mut StrongRoots) {
    if let Some(id) = source_ref.strip_prefix("turn:") {
        roots.turn_ids.insert(id.to_string());
    } else if let Some(id) = source_ref.strip_prefix("message:") {
        roots.message_ids.insert(id.to_string());
    } else if let Some(id) = source_ref.strip_prefix("task:") {
        roots.task_ids.insert(id.to_string());
    } else if let Some(id) = source_ref.strip_prefix("work_item:") {
        roots.work_item_ids.insert(id.to_string());
    } else if let Some(rest) = source_ref.strip_prefix("tool_execution:") {
        if let Some(id) = rest.split(':').next().filter(|id| !id.is_empty()) {
            roots.tool_execution_ids.insert(id.to_string());
        }
    }
}

fn tool_index_delete_changes(record: &ToolExecutionRecord) -> Vec<RuntimeIndexChange> {
    let mut changes = Vec::new();
    let mut push = |source_kind: &str, source_ref: String| {
        changes.push(RuntimeIndexChange {
            agent_id: record.agent_id.clone(),
            source_kind: source_kind.into(),
            source_id: source_ref.clone(),
            source_ref,
            operation: RuntimeIndexOperation::Delete,
            source_updated_at: record.completed_at.or(Some(record.created_at)),
            reason: "tool_execution_retention_deleted".into(),
        });
    };
    match record.tool_name.as_str() {
        crate::tool::names::EXEC_COMMAND => {
            if record.input.get("cmd").and_then(Value::as_str).is_some() {
                push(
                    "tool_command_receipt",
                    command_receipt_source_ref(&record.id, None),
                );
            }
        }
        crate::tool::names::EXEC_COMMAND_BATCH => {
            if let Some(items) = record.input.get("items").and_then(Value::as_array) {
                for (offset, item) in items.iter().enumerate() {
                    if item.get("cmd").and_then(Value::as_str).is_some() {
                        push(
                            "tool_command_receipt",
                            command_receipt_source_ref(&record.id, Some(offset + 1)),
                        );
                    }
                }
            }
        }
        _ => push(
            "tool_execution_output_preview",
            command_output_source_ref(&record.id, None, "output"),
        ),
    }
    changes
}

fn persist_last_report(db: &RuntimeDb, report: &RuntimeDbRetentionReport) -> Result<()> {
    let value = serde_json::to_string(report)?;
    let now = timestamp(Utc::now());
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
             VALUES ('runtime_db_retention_last_report', ?1, ?2, ?2)
             ON CONFLICT(key) DO UPDATE SET
               value = excluded.value, updated_at = excluded.updated_at",
            params![value, now],
        )?;
        Ok(())
    })
}

fn database_page_stats(connection: &Connection) -> Result<DatabasePageStats> {
    Ok(DatabasePageStats {
        page_size: pragma_u64(connection, "page_size")?,
        page_count: pragma_u64(connection, "page_count")?,
        freelist_pages: pragma_u64(connection, "freelist_count")?,
    })
}

fn pragma_u64(connection: &Connection, name: &str) -> Result<u64> {
    connection
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .with_context(|| format!("reading SQLite PRAGMA {name}"))
}

fn earliest_sequence(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn retention_days(value: u64) -> Result<Duration> {
    Ok(Duration::days(
        i64::try_from(value).context("retention days exceed chrono range")?,
    ))
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn elapsed_ms_since(started_at: DateTime<Utc>) -> u64 {
    Utc::now()
        .signed_duration_since(started_at)
        .num_milliseconds()
        .max(0) as u64
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("reading runtime database metadata {}", path.display()))?
        .len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AuthorityClass, ToolExecutionStatus, TranscriptEntryKind};
    use tempfile::TempDir;

    fn runtime_db() -> Result<(TempDir, RuntimeDb)> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("state/runtime.sqlite"),
            directory.path().join("state/runtime.lock"),
        )?;
        Ok((directory, db))
    }

    fn policy() -> RuntimeDbRetentionPolicy {
        RuntimeDbRetentionPolicy {
            enabled: true,
            audit_events_days: 30,
            transcript_entries_days: 30,
            tool_executions_days: 30,
            audit_events_min_rows_per_scope: crate::http::MAX_EVENT_STREAM_WINDOW,
            transcript_entries_min_rows: 1,
            tool_executions_min_rows: 1,
            interval_hours: 1,
            incremental_vacuum_pages: 1,
        }
    }

    fn tool(id: &str, turn_id: Option<&str>, created_at: DateTime<Utc>) -> ToolExecutionRecord {
        ToolExecutionRecord {
            id: id.into(),
            agent_id: "agent-a".into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: turn_id.map(ToString::to_string),
            tool_name: "test_tool".into(),
            created_at,
            completed_at: Some(created_at),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({}),
            output: serde_json::json!({"ok": true}),
            summary: id.into(),
            invocation_surface: None,
        }
    }

    #[test]
    fn policy_defaults_to_disabled_and_rejects_small_audit_floor() {
        assert!(!RuntimeDbRetentionPolicy::default().enabled);
        let error = RuntimeDbRetentionPolicy {
            audit_events_min_rows_per_scope: crate::http::MAX_EVENT_STREAM_WINDOW - 1,
            ..RuntimeDbRetentionPolicy::default()
        }
        .validate()
        .expect_err("audit floor below replay window must fail");
        assert!(error.to_string().contains("must be at least"));
    }

    #[test]
    fn disabled_retention_reports_candidates_without_deleting() -> Result<()> {
        let (_directory, db) = runtime_db()?;
        let now = Utc::now();
        for id in ["transcript-old-a", "transcript-old-b"] {
            let mut entry = TranscriptEntry::new(
                "agent-a",
                TranscriptEntryKind::IncomingMessage,
                None,
                None,
                serde_json::json!({"text": id}),
            );
            entry.id = id.into();
            entry.created_at = now - Duration::days(60);
            db.transcript_entries().append(&entry)?;
        }
        let mut disabled = policy();
        disabled.enabled = false;

        let report = db.run_retention_pass(disabled, now)?;

        assert!(report.dry_run);
        assert!(!report.enabled);
        assert_eq!(report.transcript_entries.candidate_rows, 2);
        assert_eq!(report.transcript_entries.planned_delete_rows, 1);
        assert_eq!(report.transcript_entries.deleted_rows, 0);
        assert!(db
            .transcript_entries()
            .by_id(None, "transcript-old-a")?
            .is_some());
        assert!(db
            .transcript_entries()
            .by_id(None, "transcript-old-b")?
            .is_some());
        Ok(())
    }

    #[test]
    fn transcript_retention_requires_age_and_excess_rows() -> Result<()> {
        let (_directory, db) = runtime_db()?;
        let now = Utc::now();
        let mut old = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({"text": "old"}),
        );
        old.id = "transcript-old".into();
        old.created_at = now - Duration::days(60);
        let mut recent = TranscriptEntry::new(
            "agent-a",
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({"text": "recent"}),
        );
        recent.id = "transcript-recent".into();
        recent.created_at = now;
        db.transcript_entries().append(&old)?;
        db.transcript_entries().append(&recent)?;

        let report = db.run_retention_pass(policy(), now)?;
        assert_eq!(report.transcript_entries.candidate_rows, 1);
        assert_eq!(report.transcript_entries.deleted_rows, 1);
        assert!(db
            .transcript_entries()
            .by_id(None, "transcript-old")?
            .is_none());
        assert!(db
            .transcript_entries()
            .by_id(None, "transcript-recent")?
            .is_some());

        let second = db.run_retention_pass(policy(), now)?;
        assert!(second.transcript_entries.skipped_below_minimum_rows);
        Ok(())
    }

    #[test]
    fn audit_retention_deletes_only_the_prefix_outside_floor() -> Result<()> {
        let (_directory, db) = runtime_db()?;
        let now = Utc::now();
        let old = timestamp(now - Duration::days(60));
        db.transaction(|tx| {
            for sequence in 1..=515_i64 {
                tx.execute(
                    "INSERT INTO audit_events (
                       audit_event_id, event_seq, agent_id, kind, created_at, data_json
                     ) VALUES (?1, ?2, 'agent-a', 'test', ?3, '{}')",
                    params![format!("audit-{sequence}"), sequence, old],
                )?;
            }
            Ok(())
        })?;

        let report = db.run_retention_pass(policy(), now)?;
        assert_eq!(report.audit_events.candidate_rows, 3);
        assert_eq!(report.audit_events.deleted_rows, 3);
        let connection = db.connection()?;
        let sequences = connection
            .prepare(
                "SELECT event_seq FROM audit_events
                 WHERE agent_id = 'agent-a' ORDER BY event_seq",
            )?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(sequences.len(), crate::http::MAX_EVENT_STREAM_WINDOW);
        assert_eq!(sequences.first().copied(), Some(4));
        assert_eq!(sequences.last().copied(), Some(515));
        Ok(())
    }

    #[test]
    fn active_turn_protects_tool_and_delete_writes_index_tombstone() -> Result<()> {
        let (_directory, db) = runtime_db()?;
        let now = Utc::now();
        let old = now - Duration::days(60);
        let mut active_turn = TurnRecord::new("agent-a", "turn-active", 1);
        active_turn.created_at = old;
        db.turn_records().upsert(&active_turn)?;
        db.evidence()
            .append_tool_execution(&tool("tool-protected", Some("turn-active"), old))?;
        db.evidence()
            .append_tool_execution(&tool("tool-deleted", None, old))?;

        let report = db.run_retention_pass(policy(), now)?;
        assert_eq!(report.tool_executions.protected_rows, 1);
        assert_eq!(report.tool_executions.deleted_rows, 1);
        assert!(db
            .evidence()
            .tool_execution_by_id("agent-a", "tool-protected")?
            .is_some());
        assert!(db
            .evidence()
            .tool_execution_by_id("agent-a", "tool-deleted")?
            .is_none());
        let pending = db.runtime_index_outbox().read_after("agent-a", 0, 10)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation, RuntimeIndexOperation::Delete);
        assert_eq!(pending[0].source_ref, "tool_execution:tool-deleted:output");
        Ok(())
    }

    #[test]
    fn new_database_uses_incremental_auto_vacuum() -> Result<()> {
        let (_directory, db) = runtime_db()?;
        assert_eq!(pragma_u64(&db.connection()?, "auto_vacuum")?, 2);
        Ok(())
    }

    #[test]
    fn offline_compact_fails_while_maintenance_lock_is_held() -> Result<()> {
        let (directory, _db) = runtime_db()?;
        let db_path = directory.path().join("state/runtime.sqlite");
        let migration_lock = directory.path().join("state/runtime.lock");
        let maintenance_lock = directory.path().join("state/runtime-maintenance.lock");
        let _held = RuntimeDbLock::lock(&maintenance_lock)?;
        let error = RuntimeDb::compact_offline(&db_path, &migration_lock, &maintenance_lock)
            .expect_err("compact must fail while daemon maintenance lock is held");
        assert!(error.to_string().contains("daemon to be stopped"));
        Ok(())
    }
}
