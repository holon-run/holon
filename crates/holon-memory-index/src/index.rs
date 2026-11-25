use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{
    params, params_from_iter,
    types::{Type, Value as SqlValue},
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::write_coordinator::{
    memory_index_write_coordinator, MemoryIndexWriteClass, MemoryIndexWriteCoordinator,
    MemoryIndexWriteTurn, WriterMetrics,
};

pub const INDEX_FILENAME: &str = "memory.v2.sqlite3";

const SEARCH_LIMIT_MAX: usize = 50;

const MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION: i64 = 2;

const MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION: i64 = 1;

pub const MEMORY_INDEX_BACKFILL_CURSOR: &str = "full";

pub const MEMORY_INDEX_OUTBOX_CURSOR: &str = "runtime_index_outbox";

pub const MEMORY_INDEX_REBUILD_SOURCE_KIND: &str = "memory_index_rebuild";

const MEMORY_INDEX_REBUILD_SOURCE_ID: &str = "agent";

const MEMORY_INDEX_REBUILD_SOURCE_REF_PREFIX: &str = "memory_index_rebuild";

const MEMORY_INDEX_SEARCH_TEXT_MAX_CHARS: usize = 12_000;

pub const MEMORY_INDEX_REBUILD_PHASE_SCAN: &str = "scan";

pub const MEMORY_INDEX_REBUILD_PHASE_PRUNE: &str = "prune";

pub const MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIR: &str = "prune_repair";

pub const MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIRED: &str = "prune_repaired";

pub const MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING: &str = "cleanup_pending";

pub const MEMORY_INDEX_REBUILD_PHASE_CLEANUP_SEEN: &str = "cleanup_seen";

pub const MEMORY_INDEX_REBUILD_PHASE_FINALIZE: &str = "finalize";

pub const MEMORY_INDEX_REBUILD_PHASE_FINALIZE_COMMIT: &str = "finalize_commit";

const MEMORY_INDEX_MAINTENANCE_BATCH: usize = 64;

const MEMORY_INDEX_MAINTENANCE_SLICE_TIME: Duration = Duration::from_millis(25);

const MEMORY_INDEX_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

const MEMORY_INDEX_TRANSACTION_DEADLINE: Duration = Duration::from_secs(5);

const MEMORY_INDEX_TRANSACTION_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(10);

const MEMORY_INDEX_TRANSACTION_RETRY_MAX_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchResult {
    pub kind: String,
    pub source_ref: String,
    pub scope_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub title: String,
    pub snippet: String,
    pub score: f64,
    pub updated_at: DateTime<Utc>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemoryGetResult {
    pub kind: String,
    pub source_ref: String,
    pub scope_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub title: String,
    pub content: String,
    pub truncated: bool,
    pub updated_at: DateTime<Utc>,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct MemoryDocument {
    pub source_ref: String,
    pub source_kind: String,
    pub scope_kind: String,
    pub workspace_id: Option<String>,
    pub agent_id: String,
    pub source_path: Option<PathBuf>,
    pub title: String,
    pub body: String,
    pub sanitized_excerpt: String,
    pub metadata: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryIndexAccess {
    Reader,
    Writer,
}

#[derive(Debug, Clone)]
pub struct RebuildJob {
    pub agent_id: String,
    pub generation: String,
    pub phase: String,
    pub source_kind_index: usize,
    pub source_cursor: String,
    pub source_offset: usize,
    pub runtime_high_watermark: i64,
    pub documents_processed: u64,
    pub started_at: DateTime<Utc>,
    pub last_progress_at: DateTime<Utc>,
}

fn log_memory_index_write_error<T>(
    coordinator: &MemoryIndexWriteCoordinator,
    operation: &'static str,
    result: &Result<T>,
) {
    let Err(error) = result else {
        return;
    };
    let sqlite_error = error
        .chain()
        .find_map(|source| source.downcast_ref::<rusqlite::Error>());
    match sqlite_error {
        Some(rusqlite::Error::SqliteFailure(code, _)) => tracing::warn!(
            db_role = "index",
            db_path_hash = %coordinator.db_path_hash(),
            operation,
            sqlite_primary_code = ?code.code,
            sqlite_extended_code = code.extended_code,
            error = %error,
            "memory index write failed"
        ),
        _ => tracing::warn!(
            db_role = "index",
            db_path_hash = %coordinator.db_path_hash(),
            operation,
            error = %error,
            "memory index write failed"
        ),
    }
}

fn is_retryable_memory_index_error(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        matches!(
            source.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ffi::ErrorCode::DatabaseBusy
                        | rusqlite::ffi::ErrorCode::DatabaseLocked,
                    ..
                },
                _
            ))
        )
    })
}

fn rollback_failed_transaction(
    transaction: Transaction<'_>,
    operation: &'static str,
    error: anyhow::Error,
) -> anyhow::Error {
    match transaction.rollback() {
        Ok(()) => error,
        Err(rollback_error) => error.context(format!(
            "failed to rollback memory index write {operation}: {rollback_error}"
        )),
    }
}

fn elapsed_millis(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone)]
pub struct PendingSource {
    pub document_key: String,
    pub source_ref: String,
    pub source_kind: String,
    pub source_id: String,
    pub operation: String,
}

#[derive(Debug, Clone)]
pub struct PendingSourceSnapshot {
    pub source: PendingSource,
    pub revision: i64,
}

pub enum PreparedPendingSource {
    Delete,
    Upsert(MemoryDocument),
}

fn has_stale_source_states_for_agent_tx(connection: &Connection, agent_id: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM memory_index_source_state
             WHERE agent_id = ?1
               AND (index_schema_version != ?2 OR projection_version != ?3)
             LIMIT 1",
            params![
                agent_id,
                MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION,
                MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION
            ],
            |_| Ok(true),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(Into::into)
}

fn apply_prepared_pending_source_tx(
    connection: &Connection,
    source: &PendingSource,
    prepared: &PreparedPendingSource,
) -> Result<()> {
    match prepared {
        PreparedPendingSource::Upsert(document) => {
            let hash = projection_hash(document)?;
            let existing_state = connection
                .query_row(
                    "SELECT content_hash, index_schema_version, projection_version
                     FROM memory_index_source_state
                     WHERE document_key = ?1",
                    [&source.document_key],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            let needs_projection_refresh = existing_state.as_ref().is_none_or(
                |(existing_hash, schema_version, projection_version)| {
                    existing_hash != &hash
                        || *schema_version != MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION
                        || *projection_version != MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION
                },
            );
            if needs_projection_refresh {
                upsert_document_tx(connection, document)?;
            }
            upsert_source_state_tx(connection, document, &source.source_id)
        }
        PreparedPendingSource::Delete => delete_document_tx(connection, &source.document_key),
    }
}

fn upsert_cursor_tx(
    connection: &Connection,
    runtime_id: &str,
    agent_id: &str,
    cursor_kind: &str,
    last_change_seq: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO memory_index_cursors (
            runtime_id, agent_id, cursor_kind, last_change_seq, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(runtime_id, agent_id, cursor_kind) DO UPDATE SET
            last_change_seq=MAX(memory_index_cursors.last_change_seq, excluded.last_change_seq),
            updated_at=excluded.updated_at",
        params![
            runtime_id,
            agent_id,
            cursor_kind,
            last_change_seq,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn upsert_document_tx(connection: &Connection, document: &MemoryDocument) -> Result<()> {
    let metadata_json = serde_json::to_string(&document.metadata)?;
    let hash = projection_hash(document)?;
    let document_key = document_key(document);
    let search_text = indexed_text(&bounded_search_text(&document.body));
    let source_path = document
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());
    delete_fts_document_tx(connection, &document_key)?;
    connection.execute(
        r#"
        INSERT INTO memory_documents (
            document_key, source_ref, source_kind, scope_kind, workspace_id, agent_id, source_path,
            title, body, sanitized_excerpt, metadata_json, content_hash, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(document_key) DO UPDATE SET
            source_ref=excluded.source_ref,
            source_kind=excluded.source_kind,
            scope_kind=excluded.scope_kind,
            workspace_id=excluded.workspace_id,
            agent_id=excluded.agent_id,
            source_path=excluded.source_path,
            title=excluded.title,
            body=excluded.body,
            sanitized_excerpt=excluded.sanitized_excerpt,
            metadata_json=excluded.metadata_json,
            content_hash=excluded.content_hash,
            updated_at=excluded.updated_at
        "#,
        params![
            document_key,
            document.source_ref,
            document.source_kind,
            document.scope_kind,
            document.workspace_id,
            document.agent_id,
            source_path,
            document.title,
            search_text,
            document.sanitized_excerpt,
            metadata_json,
            hash,
            document.updated_at.to_rfc3339(),
        ],
    )?;
    connection.execute(
        "INSERT INTO memory_documents_fts(document_key, title, body, sanitized_excerpt) VALUES (?1, ?2, ?3, ?4)",
        params![
            document_key,
            indexed_text(&document.title),
            indexed_text(&bounded_search_text(&document.body)),
            indexed_text(&document.sanitized_excerpt)
        ],
    )?;
    connection.execute(
        "INSERT INTO memory_documents_fts_rows (document_key, fts_rowid)
         VALUES (?1, ?2)
         ON CONFLICT(document_key) DO UPDATE SET fts_rowid=excluded.fts_rowid",
        params![document_key, connection.last_insert_rowid()],
    )?;
    Ok(())
}

fn upsert_document_if_needed_tx(connection: &Connection, document: &MemoryDocument) -> Result<()> {
    let hash = projection_hash(document)?;
    let document_key = document_key(document);
    let existing_state = connection
        .query_row(
            "SELECT content_hash, index_schema_version, projection_version
             FROM memory_index_source_state
             WHERE document_key = ?1",
            [&document_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let state_matches = existing_state.as_ref().is_some_and(
        |(existing_hash, schema_version, projection_version)| {
            existing_hash == &hash
                && *schema_version == MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION
                && *projection_version == MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION
        },
    );
    let needs_projection_refresh =
        !state_matches || !document_projection_is_complete_tx(connection, &document_key, &hash)?;
    if needs_projection_refresh {
        upsert_document_tx(connection, document)?;
    }
    Ok(())
}

fn document_projection_is_complete_tx(
    connection: &Connection,
    document_key: &str,
    expected_hash: &str,
) -> Result<bool> {
    let projected_hash = connection
        .query_row(
            "SELECT content_hash FROM memory_documents WHERE document_key = ?1",
            [document_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if projected_hash.as_deref() != Some(expected_hash) {
        return Ok(false);
    }
    let fts_rowid = connection
        .query_row(
            "SELECT fts_rowid FROM memory_documents_fts_rows WHERE document_key = ?1",
            [document_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(fts_rowid) = fts_rowid else {
        return Ok(false);
    };
    let mapped_document_key = connection
        .query_row(
            "SELECT document_key FROM memory_documents_fts WHERE rowid = ?1",
            [fts_rowid],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(mapped_document_key.as_deref() == Some(document_key))
}

fn bounded_search_text(value: &str) -> String {
    truncate_chars(value, MEMORY_INDEX_SEARCH_TEXT_MAX_CHARS).0
}

fn upsert_source_state_tx(
    connection: &Connection,
    document: &MemoryDocument,
    source_id: &str,
) -> Result<()> {
    connection.execute(
        r#"
        INSERT INTO memory_index_source_state (
            document_key, source_ref, agent_id, source_kind, source_id, source_updated_at,
            content_hash, indexed_at, index_schema_version, projection_version
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(document_key) DO UPDATE SET
            source_ref=excluded.source_ref,
            agent_id=excluded.agent_id,
            source_kind=excluded.source_kind,
            source_id=excluded.source_id,
            source_updated_at=excluded.source_updated_at,
            content_hash=excluded.content_hash,
            indexed_at=excluded.indexed_at,
            index_schema_version=excluded.index_schema_version,
            projection_version=excluded.projection_version
        "#,
        params![
            document_key(document),
            document.source_ref,
            document.agent_id,
            document.source_kind,
            source_id,
            document.updated_at.to_rfc3339(),
            projection_hash(document)?,
            Utc::now().to_rfc3339(),
            MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION,
            MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION,
        ],
    )?;
    Ok(())
}

fn upsert_index_meta_tx(
    connection: &Connection,
    agent_id: &str,
    rebuilt_at: Option<DateTime<Utc>>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    connection.execute(
        r#"
        INSERT INTO memory_index_meta (
            agent_id, schema_version, projection_version, rebuilt_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(agent_id) DO UPDATE SET
            schema_version=excluded.schema_version,
            projection_version=excluded.projection_version,
            rebuilt_at=COALESCE(excluded.rebuilt_at, memory_index_meta.rebuilt_at),
            updated_at=excluded.updated_at
        "#,
        params![
            agent_id,
            MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION,
            MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION,
            rebuilt_at.map(|dt| dt.to_rfc3339()),
            now,
        ],
    )?;
    Ok(())
}

fn delete_document_tx(connection: &Connection, document_key: &str) -> Result<()> {
    delete_fts_document_tx(connection, document_key)?;
    delete_document_records_tx(connection, document_key)
}

fn delete_document_records_tx(connection: &Connection, document_key: &str) -> Result<()> {
    connection.execute(
        "DELETE FROM memory_documents WHERE document_key = ?1",
        [document_key],
    )?;
    connection.execute(
        "DELETE FROM memory_index_source_state WHERE document_key = ?1",
        [document_key],
    )?;
    Ok(())
}

fn delete_fts_document_tx(connection: &Connection, document_key: &str) -> Result<()> {
    let projection_was_known = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM memory_documents WHERE document_key = ?1
             UNION ALL
             SELECT 1 FROM memory_index_source_state WHERE document_key = ?1
         )",
        [document_key],
        |row| row.get::<_, bool>(0),
    )?;
    let removed_mapped_row = delete_mapped_fts_document_tx(connection, document_key)?;
    if projection_was_known && !removed_mapped_row {
        // Ordinary single-source repair retains its existing fallback. Bulk
        // maintenance uses a separately sliced rowid scan instead.
        connection.execute(
            "DELETE FROM memory_documents_fts WHERE document_key = ?1",
            [document_key],
        )?;
    }
    Ok(())
}

fn delete_mapped_fts_document_tx(connection: &Connection, document_key: &str) -> Result<bool> {
    let fts_rowid = connection
        .query_row(
            "SELECT fts_rowid FROM memory_documents_fts_rows WHERE document_key = ?1",
            [document_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let mut removed_mapped_row = false;
    if let Some(fts_rowid) = fts_rowid {
        let mapped_document_key = connection
            .query_row(
                "SELECT document_key FROM memory_documents_fts WHERE rowid = ?1",
                [fts_rowid],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if mapped_document_key.as_deref() == Some(document_key) {
            removed_mapped_row = connection.execute(
                "DELETE FROM memory_documents_fts WHERE rowid = ?1",
                [fts_rowid],
            )? > 0;
        }
        connection.execute(
            "DELETE FROM memory_documents_fts_rows WHERE document_key = ?1",
            [document_key],
        )?;
    }
    Ok(removed_mapped_row)
}

fn rebuild_job_matches_tx(connection: &Connection, job: &RebuildJob) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_index_rebuild_jobs
         WHERE agent_id = ?1 AND generation = ?2 AND phase = ?3)",
        params![job.agent_id, job.generation, job.phase],
        |row| row.get::<_, bool>(0),
    )?)
}

fn agent_document_keys_page(
    connection: &Connection,
    table: &str,
    agent_id: &str,
    cursor: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let (sql, lower_bound) = agent_document_keys_page_query(table, agent_id, cursor);
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params![
            lower_bound,
            format!("{agent_id};"),
            limit.min(MEMORY_INDEX_MAINTENANCE_BATCH)
        ],
        |row| row.get::<_, String>(0),
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn agent_document_keys_page_query(table: &str, agent_id: &str, cursor: &str) -> (String, String) {
    let prefix = format!("{agent_id}:");
    let (comparison, lower_bound) = if cursor < prefix.as_str() {
        (">=", prefix)
    } else {
        (">", cursor.to_owned())
    };
    // One effective lower bound is essential: SQLite may otherwise seek to
    // the prefix and filter all rows preceding the stricter cursor.
    (
        format!(
            "SELECT document_key FROM {table}
             WHERE document_key {comparison} ?1 AND document_key < ?2
             ORDER BY document_key LIMIT ?3"
        ),
        lower_bound,
    )
}

fn document_key(document: &MemoryDocument) -> String {
    document_key_for(&document.agent_id, &document.source_ref)
}

pub fn document_key_for(agent_id: &str, source_ref: &str) -> String {
    format!("{agent_id}:{source_ref}")
}

fn rebuild_intent_source_ref(agent_id: &str) -> String {
    format!("{MEMORY_INDEX_REBUILD_SOURCE_REF_PREFIX}:{agent_id}")
}

pub fn all_backfill_source_kinds() -> &'static [&'static str] {
    &[
        "agent_memory_markdown",
        "workspace_profile",
        "message",
        "brief",
        "context_episode",
        "work_item",
        "task",
        "tool_command_receipt",
    ]
}

fn source_id_from_ref(source_ref: &str) -> &str {
    source_ref
        .split_once(':')
        .map(|(_, source_id)| source_id)
        .unwrap_or(source_ref)
}

fn parse_db_timestamp(column: usize, value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
        })
}

pub fn projection_hash(document: &MemoryDocument) -> Result<String> {
    let metadata_json = serde_json::to_string(&document.metadata)?;
    let mut hasher = Sha256::new();
    hasher.update(MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(
        MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION
            .to_string()
            .as_bytes(),
    );
    hasher.update(b"\0");
    hasher.update(document.title.as_bytes());
    hasher.update(b"\0");
    hasher.update(document.body.as_bytes());
    hasher.update(b"\0");
    hasher.update(document.sanitized_excerpt.as_bytes());
    hasher.update(b"\0");
    hasher.update(metadata_json.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn excerpt(text: &str) -> String {
    text.split_whitespace()
        .take(48)
        .collect::<Vec<_>>()
        .join(" ")
}

fn indexed_text(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len() * 2);
    expanded.push_str(text);
    expanded.push('\n');
    expanded.push_str(&mixed_cjk_bigrams(text));
    expanded
}

fn search_query(query: &str) -> String {
    let expanded = indexed_text(query);
    expanded
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(escape_fts_term)
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn escape_fts_term(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

fn mixed_cjk_bigrams(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut grams = Vec::new();
    for window in chars.windows(2) {
        if window.iter().all(|ch| is_cjk(*ch)) {
            grams.push(window.iter().collect::<String>());
        }
    }
    grams.join(" ")
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
    )
}

pub fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    let mut chars = text.chars();
    let truncated = chars.clone().nth(max_chars).is_some();
    let content = chars.by_ref().take(max_chars).collect::<String>();
    (content, truncated)
}

/// A connection to the rebuildable SQLite memory projection.
pub struct IndexStore {
    connection: Connection,
    access: MemoryIndexAccess,
    write_coordinator: Arc<MemoryIndexWriteCoordinator>,
    writer_metrics: WriterMetrics,
}

impl IndexStore {
    pub fn open_in(shared_indexes_dir: &Path, writer_metrics: WriterMetrics) -> Result<Self> {
        holon_domain::private_fs::create_private_dir_all(shared_indexes_dir)
            .with_context(|| format!("failed to create {}", shared_indexes_dir.display()))?;
        let index_path = shared_indexes_dir.join(INDEX_FILENAME);
        let write_coordinator = memory_index_write_coordinator(&index_path)?;
        let deadline = Instant::now() + MEMORY_INDEX_TRANSACTION_DEADLINE;
        let _turn = write_coordinator.wait_turn_until_observed(
            MemoryIndexWriteClass::Foreground,
            "memory_index.open_writer",
            deadline,
            writer_metrics,
        )?;
        let result = (|| {
            let connection = Connection::open(&index_path)?;
            if let Err(error) = holon_domain::private_fs::restrict_sqlite_database(&index_path) {
                tracing::warn!(
                    index = %index_path.display(),
                    %error,
                    "failed to restrict memory index permissions"
                );
            }
            let index = Self {
                connection,
                access: MemoryIndexAccess::Writer,
                write_coordinator: Arc::clone(&write_coordinator),
                writer_metrics,
            };
            index.connection.busy_timeout(MEMORY_INDEX_BUSY_TIMEOUT)?;
            index.run_retryable_write_until(
                "memory_index.open_writer.configure",
                deadline,
                || {
                    index.connection.execute_batch(
                        r#"
                        PRAGMA journal_mode = WAL;
                        PRAGMA synchronous = NORMAL;
                        PRAGMA wal_autocheckpoint = 10000;
                        PRAGMA mmap_size = 268435456;
                        "#,
                    )?;
                    Ok(())
                },
            )?;
            index.ensure_schema_until(deadline)?;
            Ok(index)
        })();
        log_memory_index_write_error(&write_coordinator, "memory_index.open_writer", &result);
        result
    }

    pub fn open_reader_in(
        shared_indexes_dir: &Path,
        writer_metrics: WriterMetrics,
    ) -> Result<Self> {
        let index_path = shared_indexes_dir.join(INDEX_FILENAME);
        if index_path.exists() {
            if let Ok(reader) = Self::open_existing_reader(&index_path, writer_metrics) {
                if reader.schema_is_ready().unwrap_or(false) {
                    return Ok(reader);
                }
            }
        }
        drop(Self::open_in(shared_indexes_dir, writer_metrics)?);
        let reader = Self::open_existing_reader(&index_path, writer_metrics)?;
        anyhow::ensure!(
            reader.schema_is_ready()?,
            "memory index schema is incomplete after writer initialization"
        );
        Ok(reader)
    }

    fn open_existing_reader(index_path: &Path, writer_metrics: WriterMetrics) -> Result<Self> {
        let write_coordinator = memory_index_write_coordinator(&index_path)?;
        let connection =
            Connection::open_with_flags(&index_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if let Err(error) = holon_domain::private_fs::restrict_sqlite_database(&index_path) {
            tracing::warn!(
                index = %index_path.display(),
                %error,
                "failed to restrict memory index permissions"
            );
        }
        connection.execute_batch(
            r#"
            PRAGMA query_only = ON;
            PRAGMA busy_timeout = 5000;
            PRAGMA mmap_size = 268435456;
            "#,
        )?;
        Ok(Self {
            connection,
            access: MemoryIndexAccess::Reader,
            write_coordinator,
            writer_metrics,
        })
    }

    fn schema_is_ready(&self) -> Result<bool> {
        Ok(self.table_exists("memory_documents")?
            && self.table_exists("memory_documents_fts")?
            && self.table_exists("memory_documents_fts_rows")?
            && self.table_exists("memory_index_pending_sources")?
            && self.table_has_column("memory_index_pending_sources", "revision")?
            && self.table_exists("memory_index_source_state")?
            && self.table_has_column("memory_index_source_state", "projection_version")?
            && self.table_exists("memory_index_meta")?
            && self.table_exists("memory_index_checkpoints")?
            && self.table_exists("memory_index_cursors")?
            && self.table_exists("memory_index_rebuild_jobs")?
            && self.table_exists("memory_index_rebuild_seen")?)
    }

    fn write_turn_until(
        &self,
        write_class: MemoryIndexWriteClass,
        operation: &'static str,
        deadline: Instant,
    ) -> Result<MemoryIndexWriteTurn> {
        anyhow::ensure!(
            self.access == MemoryIndexAccess::Writer,
            "cannot write through read-only memory index connection"
        );
        self.write_coordinator.wait_turn_until_observed(
            write_class,
            operation,
            deadline,
            self.writer_metrics,
        )
    }

    pub fn finish_write<T>(&self, operation: &'static str, result: Result<T>) -> Result<T> {
        log_memory_index_write_error(&self.write_coordinator, operation, &result);
        result
    }

    fn run_write_transaction<T>(
        &self,
        operation: &'static str,
        f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.run_write_transaction_with_deadline(
            MemoryIndexWriteClass::Foreground,
            operation,
            MEMORY_INDEX_TRANSACTION_DEADLINE,
            f,
        )
    }

    fn run_maintenance_write_transaction<T>(
        &self,
        operation: &'static str,
        f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.run_write_transaction_with_deadline(
            MemoryIndexWriteClass::Maintenance,
            operation,
            MEMORY_INDEX_TRANSACTION_DEADLINE,
            f,
        )
    }

    fn run_write_transaction_with_deadline<T>(
        &self,
        write_class: MemoryIndexWriteClass,
        operation: &'static str,
        deadline: Duration,
        f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + deadline;
        let _turn = self.write_turn_until(write_class, operation, deadline)?;
        self.run_immediate_transaction_until(operation, deadline, f)
    }

    pub fn delete_agent_projection(&self, agent_id: &str) -> Result<()> {
        let mut cursor = String::new();
        while let Some(next) = self.delete_agent_documents_slice(agent_id, &cursor)? {
            cursor = next;
        }

        // Sweep residual/duplicate FTS rows even if their document or rowid
        // mapping was already removed by a previous, interrupted cleanup.
        let mut rowid = 0;
        while let Some(next) = self.delete_agent_fts_repair_slice(agent_id, rowid)? {
            rowid = next;
        }

        for table in [
            "memory_documents",
            "memory_index_source_state",
            "memory_index_pending_sources",
        ] {
            let mut cursor = String::new();
            loop {
                let next = self.run_maintenance_write_transaction(
                    "memory_index.delete_agent",
                    |transaction| {
                        let keys = agent_document_keys_page(
                            transaction,
                            table,
                            agent_id,
                            &cursor,
                            MEMORY_INDEX_MAINTENANCE_BATCH,
                        )?;
                        for key in &keys {
                            transaction.execute(
                                &format!(
                                    "DELETE FROM {table} WHERE document_key = ?1 AND agent_id = ?2"
                                ),
                                params![key, agent_id],
                            )?;
                        }
                        Ok(keys.last().cloned())
                    },
                )?;
                match next {
                    Some(next) => cursor = next,
                    None => break,
                }
            }
        }
        // The prefix is unambiguous: agent ids cannot contain ':'.
        loop {
            let removed = self.run_maintenance_write_transaction(
                "memory_index.delete_agent",
                |transaction| {
                    let keys = agent_document_keys_page(
                        transaction,
                        "memory_documents_fts_rows",
                        agent_id,
                        "",
                        MEMORY_INDEX_MAINTENANCE_BATCH,
                    )?;
                    for key in &keys {
                        transaction.execute(
                            "DELETE FROM memory_documents_fts_rows WHERE document_key = ?1",
                            [key],
                        )?;
                    }
                    Ok(keys.len())
                },
            )?;
            if removed == 0 {
                break;
            }
        }
        loop {
            let removed = self.run_maintenance_write_transaction(
                "memory_index.delete_agent",
                |transaction| {
                    Ok(transaction.execute(
                        "DELETE FROM memory_index_rebuild_seen WHERE rowid IN (
                             SELECT rowid FROM memory_index_rebuild_seen
                             WHERE agent_id = ?1 LIMIT ?2
                         )",
                        params![agent_id, MEMORY_INDEX_MAINTENANCE_BATCH],
                    )?)
                },
            )?;
            if removed == 0 {
                break;
            }
        }
        // These metadata tables are small, but page candidates by rowid so
        // cleanup never depends on an unindexed agent_id scan either.
        for table in [
            "memory_index_checkpoints",
            "memory_index_cursors",
            "memory_index_meta",
            "memory_index_rebuild_jobs",
        ] {
            let mut cursor = 0_i64;
            loop {
                let next = self.run_maintenance_write_transaction(
                    "memory_index.delete_agent",
                    |transaction| {
                        let mut statement = transaction.prepare(&format!(
                            "SELECT rowid, agent_id FROM {table}
                             WHERE rowid > ?1 ORDER BY rowid LIMIT ?2"
                        ))?;
                        let rows = statement
                            .query_map(params![cursor, MEMORY_INDEX_MAINTENANCE_BATCH], |row| {
                                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        for (rowid, owner) in &rows {
                            if owner == agent_id {
                                transaction.execute(
                                    &format!("DELETE FROM {table} WHERE rowid = ?1"),
                                    [rowid],
                                )?;
                            }
                        }
                        Ok(rows.last().map(|(rowid, _)| *rowid))
                    },
                )?;
                match next {
                    Some(next) => cursor = next,
                    None => break,
                }
            }
        }
        Ok(())
    }

    fn delete_agent_documents_slice(&self, agent_id: &str, cursor: &str) -> Result<Option<String>> {
        self.run_maintenance_write_transaction("memory_index.delete_agent", |transaction| {
            let started = Instant::now();
            let keys = agent_document_keys_page(
                transaction,
                "memory_documents",
                agent_id,
                cursor,
                MEMORY_INDEX_MAINTENANCE_BATCH,
            )?;
            let mut next = None;
            for key in keys {
                if delete_mapped_fts_document_tx(transaction, &key)? {
                    delete_document_records_tx(transaction, &key)?;
                }
                // Missing or incorrect mappings retain their ownership records
                // for a single bounded-rowid repair pass, not one scan per key.
                next = Some(key);
                if started.elapsed() >= MEMORY_INDEX_MAINTENANCE_SLICE_TIME {
                    break;
                }
            }
            Ok(next)
        })
    }

    fn delete_agent_fts_repair_slice(&self, agent_id: &str, cursor: i64) -> Result<Option<i64>> {
        self.run_maintenance_write_transaction("memory_index.delete_agent", |transaction| {
            let started = Instant::now();
            let mut statement = transaction.prepare(
                "SELECT rowid, document_key FROM memory_documents_fts
                 WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![cursor, MEMORY_INDEX_MAINTENANCE_BATCH], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            let mut next = None;
            for (rowid, key) in rows {
                // Agent ids cannot contain ':', so this does not match a
                // similarly named peer or depend on still-present owner rows.
                if key.starts_with(&format!("{agent_id}:")) {
                    transaction
                        .execute("DELETE FROM memory_documents_fts WHERE rowid = ?1", [rowid])?;
                    transaction.execute(
                        "DELETE FROM memory_documents_fts_rows WHERE document_key = ?1",
                        [&key],
                    )?;
                }
                next = Some(rowid);
                if started.elapsed() >= MEMORY_INDEX_MAINTENANCE_SLICE_TIME {
                    break;
                }
            }
            Ok(next)
        })
    }

    fn run_immediate_transaction_until<T>(
        &self,
        operation: &'static str,
        deadline: Instant,
        mut f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.run_retryable_write_until(operation, deadline, || {
            let transaction =
                Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            let value = match f(&transaction) {
                Ok(value) => value,
                Err(error) => {
                    return Err(rollback_failed_transaction(transaction, operation, error));
                }
            };
            match transaction.execute_batch("COMMIT") {
                Ok(()) => Ok(value),
                Err(error) => Err(rollback_failed_transaction(
                    transaction,
                    operation,
                    error.into(),
                )),
            }
        })
    }

    fn run_retryable_write_until<T>(
        &self,
        operation: &'static str,
        deadline: Instant,
        mut f: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        let started_at = Instant::now();
        let mut retry_count = 0_u32;
        let mut retry_delay = MEMORY_INDEX_TRANSACTION_RETRY_INITIAL_DELAY;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            anyhow::ensure!(
                !remaining.is_zero(),
                "memory index write {operation} exhausted retry deadline before attempt"
            );
            self.connection
                .busy_timeout(MEMORY_INDEX_BUSY_TIMEOUT.min(remaining))?;
            let attempt = f();
            match attempt {
                Ok(value) => {
                    if retry_count > 0 {
                        tracing::debug!(
                            db_role = "index",
                            db_path_hash = %self.write_coordinator.db_path_hash(),
                            operation,
                            retry_count,
                            elapsed_ms = elapsed_millis(started_at),
                            "memory index write succeeded after retry"
                        );
                    }
                    return Ok(value);
                }
                Err(error) if is_retryable_memory_index_error(&error) => {
                    retry_count = retry_count.saturating_add(1);
                    let elapsed = started_at.elapsed();
                    if Instant::now() >= deadline {
                        tracing::warn!(
                            db_role = "index",
                            db_path_hash = %self.write_coordinator.db_path_hash(),
                            operation,
                            retry_count,
                            elapsed_ms = elapsed_millis(started_at),
                            error = %error,
                            "memory index write retry deadline exhausted"
                        );
                        return Err(error.context(format!(
                            "memory index write {operation} exhausted retry deadline after {} ms and {retry_count} retries",
                            elapsed.as_millis()
                        )));
                    }
                    tracing::trace!(
                        db_role = "index",
                        db_path_hash = %self.write_coordinator.db_path_hash(),
                        operation,
                        retry_count,
                        elapsed_ms = elapsed_millis(started_at),
                        retry_delay_ms = retry_delay.as_millis(),
                        error = %error,
                        "memory index write retrying"
                    );
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    thread::sleep(retry_delay.min(remaining));
                    retry_delay = retry_delay
                        .saturating_mul(2)
                        .min(MEMORY_INDEX_TRANSACTION_RETRY_MAX_DELAY);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Enqueues one pending upsert on an already-open connection.

    pub fn enqueue_upsert(
        &mut self,
        agent_id: &str,
        source_kind: &str,
        source_id: &str,
        source_ref: &str,
    ) -> Result<()> {
        let result = self.enqueue_source(
            agent_id,
            source_kind,
            source_id,
            source_ref,
            "upsert",
            None,
            "source_write",
        );
        self.finish_write("memory_index.enqueue_upsert", result)
    }

    fn ensure_schema_until(&self, deadline: Instant) -> Result<()> {
        if self.table_exists("memory_documents")?
            && (self.table_has_column("memory_documents", "original_body")?
                || !self.table_has_column("memory_documents", "document_key")?)
        {
            self.run_retryable_write_until(
                "memory_index.ensure_schema.drop_legacy",
                deadline,
                || {
                    self.connection.execute_batch(
                        r#"
                        DROP TABLE IF EXISTS memory_documents_fts_rows;
                        DROP TABLE IF EXISTS memory_documents_fts;
                        DROP TABLE IF EXISTS memory_documents;
                        "#,
                    )?;
                    Ok(())
                },
            )?;
        }
        self.run_retryable_write_until("memory_index.ensure_schema.create", deadline, || {
            self.connection.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS memory_documents (
                document_key TEXT PRIMARY KEY,
                source_ref TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                scope_kind TEXT NOT NULL,
                workspace_id TEXT,
                agent_id TEXT NOT NULL,
                source_path TEXT,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                sanitized_excerpt TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memory_documents_fts
            USING fts5(document_key UNINDEXED, title, body, sanitized_excerpt, tokenize='unicode61');
            CREATE TABLE IF NOT EXISTS memory_index_pending_sources (
                document_key TEXT PRIMARY KEY,
                source_ref TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_id TEXT NOT NULL,
                operation TEXT NOT NULL,
                source_updated_at TEXT,
                reason TEXT,
                enqueued_at TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                revision INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS memory_index_source_state (
                document_key TEXT PRIMARY KEY,
                source_ref TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_id TEXT NOT NULL,
                source_updated_at TEXT,
                content_hash TEXT,
                indexed_at TEXT NOT NULL,
                index_schema_version INTEGER NOT NULL,
                projection_version INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS memory_index_meta (
                agent_id TEXT PRIMARY KEY,
                schema_version INTEGER NOT NULL,
                projection_version INTEGER NOT NULL,
                rebuilt_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memory_index_checkpoints (
                agent_id TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                cursor TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (agent_id, source_kind)
            );
            CREATE TABLE IF NOT EXISTS memory_index_cursors (
                runtime_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                cursor_kind TEXT NOT NULL,
                last_change_seq INTEGER NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (runtime_id, agent_id, cursor_kind)
            );
            CREATE TABLE IF NOT EXISTS memory_index_rebuild_jobs (
                agent_id TEXT PRIMARY KEY,
                generation TEXT NOT NULL,
                phase TEXT NOT NULL,
                source_kind_index INTEGER NOT NULL,
                source_cursor TEXT NOT NULL,
                source_offset INTEGER NOT NULL,
                runtime_high_watermark INTEGER NOT NULL,
                documents_processed INTEGER NOT NULL DEFAULT 0,
                started_at TEXT NOT NULL,
                last_progress_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memory_index_rebuild_seen (
                agent_id TEXT NOT NULL,
                generation TEXT NOT NULL,
                document_key TEXT NOT NULL,
                PRIMARY KEY (agent_id, generation, document_key)
            );
                CREATE INDEX IF NOT EXISTS idx_memory_documents_agent
                    ON memory_documents(agent_id);
                CREATE INDEX IF NOT EXISTS idx_memory_index_source_state_agent
                    ON memory_index_source_state(agent_id);
                CREATE INDEX IF NOT EXISTS idx_memory_index_rebuild_seen_agent_generation
                    ON memory_index_rebuild_seen(agent_id, generation);
                "#,
            )?;
            Ok(())
        })?;
        if !self.table_exists("memory_documents_fts_rows")? {
            self.run_immediate_transaction_until(
                "memory_index.ensure_schema.create_fts_rows",
                deadline,
                |transaction| {
                    transaction.execute_batch(
                        r#"
                        CREATE TABLE memory_documents_fts_rows (
                            document_key TEXT PRIMARY KEY,
                            fts_rowid INTEGER NOT NULL UNIQUE
                        );
                        INSERT INTO memory_documents_fts_rows (document_key, fts_rowid)
                            SELECT document_key, rowid FROM memory_documents_fts;
                        "#,
                    )?;
                    Ok(())
                },
            )?;
        }
        if !self.table_has_column("memory_index_source_state", "projection_version")? {
            self.run_retryable_write_until(
                "memory_index.ensure_schema.add_projection_version",
                deadline,
                || {
                    self.connection.execute_batch(
                        "ALTER TABLE memory_index_source_state
                         ADD COLUMN projection_version INTEGER NOT NULL DEFAULT 0;",
                    )?;
                    Ok(())
                },
            )?;
        }
        if !self.table_has_column("memory_index_pending_sources", "revision")? {
            self.run_retryable_write_until(
                "memory_index.ensure_schema.add_pending_revision",
                deadline,
                || {
                    self.connection.execute_batch(
                        "ALTER TABLE memory_index_pending_sources
                         ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;",
                    )?;
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    fn table_exists(&self, table: &str) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
                [table],
                |_| Ok(true),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(Into::into)
    }

    fn table_has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn prune_rebuild_slice(&mut self, job: &RebuildJob, slice_limit: usize) -> Result<usize> {
        self.run_maintenance_write_transaction("memory_index.rebuild_prune", |transaction| {
                if !rebuild_job_matches_tx(transaction, job)? {
                    return Ok(0);
                }
                let started = Instant::now();
                let keys = agent_document_keys_page(
                    transaction, "memory_documents", &job.agent_id, &job.source_cursor, slice_limit,
                )?;
                if keys.is_empty() {
                    transaction.execute(
                        "UPDATE memory_index_rebuild_jobs
                         SET phase = ?1, source_cursor = '', last_progress_at = ?2
                         WHERE agent_id = ?3 AND generation = ?4",
                        params![
                            MEMORY_INDEX_REBUILD_PHASE_FINALIZE,
                            Utc::now().to_rfc3339(),
                            job.agent_id,
                            job.generation,
                        ],
                    )?;
                    return Ok(0);
                }
                let mut examined = 0;
                let mut cursor = job.source_cursor.clone();
                for document_key in &keys {
                    let seen = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM memory_index_rebuild_seen
                         WHERE agent_id = ?1 AND generation = ?2 AND document_key = ?3)",
                        params![job.agent_id, job.generation, document_key],
                        |row| row.get::<_, bool>(0),
                    )?;
                    if !seen {
                        // Bulk prune must not use the ordinary UNINDEXED-key
                        // repair fallback while holding a maintenance turn.
                        if !delete_mapped_fts_document_tx(transaction, document_key)?
                            && job.phase != MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIRED
                        {
                            transaction.execute(
                                "UPDATE memory_index_rebuild_jobs
                                 SET phase = ?1, source_cursor = '', source_offset = 0, last_progress_at = ?2
                                 WHERE agent_id = ?3 AND generation = ?4",
                                params![MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIR,
                                        Utc::now().to_rfc3339(), job.agent_id, job.generation],
                            )?;
                            return Ok(examined + 1);
                        }
                        delete_document_records_tx(transaction, document_key)?;
                    }
                    cursor = document_key.clone();
                    examined += 1;
                    if started.elapsed() >= MEMORY_INDEX_MAINTENANCE_SLICE_TIME {
                        break;
                    }
                }
                transaction.execute(
                    "UPDATE memory_index_rebuild_jobs
                     SET source_cursor = ?1, last_progress_at = ?2
                     WHERE agent_id = ?3 AND generation = ?4",
                    params![cursor, Utc::now().to_rfc3339(), job.agent_id, job.generation],
                )?;
                Ok(examined)
            })
    }

    pub fn prune_rebuild_fts_repair_slice(&self, job: &RebuildJob, limit: usize) -> Result<usize> {
        self.run_maintenance_write_transaction("memory_index.rebuild_prune", |transaction| {
            if !rebuild_job_matches_tx(transaction, job)? {
                return Ok(0);
            }
            let started = Instant::now();
            let mut statement = transaction.prepare(
                "SELECT rowid, document_key FROM memory_documents_fts
                 WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            )?;
            let rows = statement
                .query_map(
                    params![
                        i64::try_from(job.source_offset).unwrap_or(i64::MAX),
                        limit.min(MEMORY_INDEX_MAINTENANCE_BATCH)
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            let mut cursor = job.source_offset;
            let mut examined = 0;
            for (rowid, key) in &rows {
                let stale = transaction.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM memory_documents d
                         WHERE d.document_key = ?1 AND d.agent_id = ?2
                           AND NOT EXISTS(SELECT 1 FROM memory_index_rebuild_seen s
                               WHERE s.agent_id = ?2 AND s.generation = ?3 AND s.document_key = ?1)
                     )",
                    params![key, job.agent_id, job.generation],
                    |row| row.get::<_, bool>(0),
                )?;
                if stale {
                    transaction
                        .execute("DELETE FROM memory_documents_fts WHERE rowid = ?1", [rowid])?;
                    transaction.execute(
                        "DELETE FROM memory_documents_fts_rows WHERE document_key = ?1",
                        [key],
                    )?;
                }
                cursor =
                    usize::try_from(*rowid).context("FTS rowid exceeds rebuild repair cursor")?;
                examined += 1;
                if started.elapsed() >= MEMORY_INDEX_MAINTENANCE_SLICE_TIME {
                    break;
                }
            }
            transaction.execute(
                "UPDATE memory_index_rebuild_jobs
                 SET phase = ?1, source_offset = ?2, source_cursor = '', last_progress_at = ?3
                 WHERE agent_id = ?4 AND generation = ?5",
                params![
                    if rows.is_empty() {
                        MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIRED
                    } else {
                        &job.phase
                    },
                    i64::try_from(cursor).unwrap_or(i64::MAX),
                    Utc::now().to_rfc3339(),
                    job.agent_id,
                    job.generation,
                ],
            )?;
            Ok(examined)
        })
    }

    pub fn cleanup_rebuild_pending_slice(&self, job: &RebuildJob, limit: usize) -> Result<usize> {
        self.run_maintenance_write_transaction("memory_index.rebuild_finalize", |transaction| {
            if !rebuild_job_matches_tx(transaction, job)? {
                return Ok(0);
            }
            let keys = agent_document_keys_page(
                transaction,
                "memory_index_pending_sources",
                &job.agent_id,
                &job.source_cursor,
                limit,
            )?;
            for key in &keys {
                transaction.execute(
                    "DELETE FROM memory_index_pending_sources
                     WHERE document_key = ?1 AND agent_id = ?2 AND enqueued_at <= ?3",
                    params![key, job.agent_id, job.started_at.to_rfc3339()],
                )?;
            }
            transaction.execute(
                "UPDATE memory_index_rebuild_jobs
                 SET phase = ?1, source_cursor = ?2, last_progress_at = ?3
                 WHERE agent_id = ?4 AND generation = ?5",
                params![
                    if keys.is_empty() {
                        MEMORY_INDEX_REBUILD_PHASE_CLEANUP_SEEN
                    } else {
                        &job.phase
                    },
                    keys.last().map(String::as_str).unwrap_or(""),
                    Utc::now().to_rfc3339(),
                    job.agent_id,
                    job.generation,
                ],
            )?;
            Ok(keys.len())
        })
    }

    pub fn cleanup_rebuild_seen_slice(&self, job: &RebuildJob, limit: usize) -> Result<usize> {
        self.run_maintenance_write_transaction("memory_index.rebuild_finalize", |transaction| {
            if !rebuild_job_matches_tx(transaction, job)? {
                return Ok(0);
            }
            let removed = transaction.execute(
                "DELETE FROM memory_index_rebuild_seen WHERE rowid IN (
                     SELECT rowid FROM memory_index_rebuild_seen
                     WHERE agent_id = ?1 AND generation = ?2 LIMIT ?3
                 )",
                params![
                    job.agent_id,
                    job.generation,
                    limit.min(MEMORY_INDEX_MAINTENANCE_BATCH)
                ],
            )?;
            if removed == 0 {
                transaction.execute(
                    "UPDATE memory_index_rebuild_jobs SET phase = ?1, last_progress_at = ?2
                     WHERE agent_id = ?3 AND generation = ?4",
                    params![
                        MEMORY_INDEX_REBUILD_PHASE_FINALIZE_COMMIT,
                        Utc::now().to_rfc3339(),
                        job.agent_id,
                        job.generation
                    ],
                )?;
            }
            Ok(removed)
        })
    }

    pub fn update_rebuild_phase(&self, job: &RebuildJob, phase: &str) -> Result<()> {
        self.run_maintenance_write_transaction(
            "memory_index.update_rebuild_phase",
            |transaction| {
                transaction.execute(
                    "UPDATE memory_index_rebuild_jobs
                 SET phase = ?1, source_cursor = '', last_progress_at = ?2
                 WHERE agent_id = ?3 AND generation = ?4",
                    params![phase, Utc::now().to_rfc3339(), job.agent_id, job.generation],
                )?;
                Ok(())
            },
        )?;
        tracing::info!(
            agent_id = %job.agent_id,
            generation = %job.generation,
            from_phase = %job.phase,
            to_phase = phase,
            "advanced memory index rebuild phase"
        );
        Ok(())
    }

    pub fn rebuild_job(&self, agent_id: &str) -> Result<Option<RebuildJob>> {
        self.connection
            .query_row(
                "SELECT agent_id, generation, phase, source_kind_index, source_cursor,
                        source_offset, runtime_high_watermark, documents_processed,
                        started_at, last_progress_at
                 FROM memory_index_rebuild_jobs
                 WHERE agent_id = ?1",
                [agent_id],
                |row| {
                    let started_at: String = row.get(8)?;
                    let last_progress_at: String = row.get(9)?;
                    Ok(RebuildJob {
                        agent_id: row.get(0)?,
                        generation: row.get(1)?,
                        phase: row.get(2)?,
                        source_kind_index: row.get::<_, i64>(3)?.max(0) as usize,
                        source_cursor: row.get(4)?,
                        source_offset: row.get::<_, i64>(5)?.max(0) as usize,
                        runtime_high_watermark: row.get(6)?,
                        documents_processed: row.get::<_, i64>(7)?.max(0) as u64,
                        started_at: parse_db_timestamp(8, started_at)?,
                        last_progress_at: parse_db_timestamp(9, last_progress_at)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_document(&self, document: &MemoryDocument) -> Result<()> {
        self.run_write_transaction("memory_index.upsert_document", |transaction| {
            upsert_document_tx(transaction, document)?;
            upsert_source_state_tx(
                transaction,
                document,
                source_id_from_ref(&document.source_ref),
            )
        })
    }

    pub fn delete_document(&self, agent_id: &str, source_ref: &str) -> Result<()> {
        let document_key = document_key_for(agent_id, source_ref);
        self.run_write_transaction("memory_index.delete_document", |transaction| {
            delete_document_tx(transaction, &document_key)
        })
    }

    pub fn enqueue_source(
        &self,
        agent_id: &str,
        source_kind: &str,
        source_id: &str,
        source_ref: &str,
        operation: &str,
        source_updated_at: Option<DateTime<Utc>>,
        reason: &str,
    ) -> Result<()> {
        let document_key = document_key_for(agent_id, source_ref);
        self.run_write_transaction("memory_index.enqueue_source", |transaction| {
            transaction.execute(
                r#"
                INSERT INTO memory_index_pending_sources (
                    document_key, source_ref, agent_id, source_kind, source_id, operation,
                    source_updated_at, reason, enqueued_at, attempts, last_error
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, NULL)
                ON CONFLICT(document_key) DO UPDATE SET
                    source_ref=excluded.source_ref,
                    agent_id=excluded.agent_id,
                    source_kind=excluded.source_kind,
                    source_id=excluded.source_id,
                    operation=excluded.operation,
                    source_updated_at=excluded.source_updated_at,
                    reason=excluded.reason,
                    enqueued_at=excluded.enqueued_at,
                    revision=memory_index_pending_sources.revision + 1
                "#,
                params![
                    document_key,
                    source_ref,
                    agent_id,
                    source_kind,
                    source_id,
                    operation,
                    source_updated_at.map(|dt| dt.to_rfc3339()),
                    reason,
                    Utc::now().to_rfc3339(),
                ],
            )?;
            Ok(())
        })
    }

    pub fn enqueue_rebuild_intent(
        &self,
        agent_id: &str,
        active_workspace_id: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let result = (|| {
            if reason.starts_with("self_heal") && self.rebuild_job(agent_id)?.is_some() {
                return Ok(());
            }
            let source_ref = rebuild_intent_source_ref(agent_id);
            self.enqueue_source(
                agent_id,
                MEMORY_INDEX_REBUILD_SOURCE_KIND,
                active_workspace_id.unwrap_or(MEMORY_INDEX_REBUILD_SOURCE_ID),
                &source_ref,
                "rebuild",
                None,
                reason,
            )
        })();
        self.finish_write("memory_index.enqueue_rebuild_intent", result)
    }

    pub fn consume_pending_source_snapshot(
        &self,
        pending_source: &PendingSourceSnapshot,
        prepared: &Result<PreparedPendingSource>,
    ) -> Result<()> {
        self.run_write_transaction("memory_index.consume_pending_source", |transaction| {
            let source = &pending_source.source;
            let current_revision = transaction
                .query_row(
                    "SELECT revision FROM memory_index_pending_sources
                     WHERE document_key = ?1",
                    [&source.document_key],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if current_revision != Some(pending_source.revision) {
                return Ok(());
            }

            let result = prepared
                .as_ref()
                .map_err(|error| anyhow::anyhow!(error.to_string()))
                .and_then(|prepared| {
                    apply_prepared_pending_source_tx(transaction, source, prepared)
                });
            match result {
                Ok(()) => {
                    transaction.execute(
                        "DELETE FROM memory_index_pending_sources
                         WHERE document_key = ?1 AND revision = ?2",
                        params![source.document_key, pending_source.revision],
                    )?;
                }
                Err(error) if is_retryable_memory_index_error(&error) => return Err(error),
                Err(error) => {
                    transaction.execute(
                        "UPDATE memory_index_pending_sources
                         SET attempts = attempts + 1, last_error = ?3
                         WHERE document_key = ?1 AND revision = ?2",
                        params![
                            source.document_key,
                            pending_source.revision,
                            error.to_string()
                        ],
                    )?;
                }
            }
            Ok(())
        })
    }

    pub fn cursor(&self, runtime_id: &str, agent_id: &str, cursor_kind: &str) -> Result<i64> {
        self.connection
            .query_row(
                "SELECT last_change_seq FROM memory_index_cursors
                 WHERE runtime_id = ?1 AND agent_id = ?2 AND cursor_kind = ?3",
                params![runtime_id, agent_id, cursor_kind],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.unwrap_or(0))
            .map_err(Into::into)
    }

    pub fn cursor_updated_at(
        &self,
        runtime_id: &str,
        agent_id: &str,
        cursor_kind: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        let updated_at: Option<String> = self
            .connection
            .query_row(
                "SELECT updated_at FROM memory_index_cursors
                 WHERE runtime_id = ?1 AND agent_id = ?2 AND cursor_kind = ?3",
                params![runtime_id, agent_id, cursor_kind],
                |row| row.get(0),
            )
            .optional()?;
        updated_at
            .map(|value| DateTime::parse_from_rfc3339(&value).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .map_err(Into::into)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_sources_for_agent(&self, agent_id: &str) -> Result<Vec<PendingSource>> {
        self.pending_sources_for_agent_with_limit(agent_id, usize::MAX)
            .map(|sources| sources.into_iter().map(|source| source.source).collect())
    }

    pub fn pending_sources_for_agent_with_limit(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<PendingSourceSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT document_key, source_ref, agent_id, source_kind, source_id, operation, revision
             FROM memory_index_pending_sources
             WHERE agent_id = ?1
             ORDER BY enqueued_at ASC, document_key ASC
             LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(params![agent_id, limit], |row| {
            Ok(PendingSourceSnapshot {
                source: PendingSource {
                    document_key: row.get(0)?,
                    source_ref: row.get(1)?,
                    source_kind: row.get(3)?,
                    source_id: row.get(4)?,
                    operation: row.get(5)?,
                },
                revision: row.get(6)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn rebuild_intents_for_agent(&self, agent_id: &str) -> Result<Vec<PendingSource>> {
        let mut statement = self.connection.prepare(
            "SELECT document_key, source_ref, agent_id, source_kind, source_id, operation
             FROM memory_index_pending_sources
             WHERE agent_id = ?1 AND source_kind = ?2
             ORDER BY enqueued_at ASC, document_key ASC",
        )?;
        let rows =
            statement.query_map(params![agent_id, MEMORY_INDEX_REBUILD_SOURCE_KIND], |row| {
                Ok(PendingSource {
                    document_key: row.get(0)?,
                    source_ref: row.get(1)?,
                    source_kind: row.get(3)?,
                    source_id: row.get(4)?,
                    operation: row.get(5)?,
                })
            })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn has_pending_sources_for_agent(&self, agent_id: &str) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT 1 FROM memory_index_pending_sources
                 WHERE agent_id = ?1
                 LIMIT 1",
                [agent_id],
                |_| Ok(true),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(Into::into)
    }

    pub fn stale_source_states_for_agent_with_limit(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<PendingSource>> {
        let mut statement = self.connection.prepare(
            "SELECT document_key, source_ref, source_kind, source_id
             FROM memory_index_source_state
             WHERE agent_id = ?1
               AND (index_schema_version != ?2 OR projection_version != ?3)
             ORDER BY indexed_at ASC, document_key ASC
             LIMIT ?4",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(
            params![
                agent_id,
                MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION,
                MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION,
                limit
            ],
            |row| {
                Ok(PendingSource {
                    document_key: row.get(0)?,
                    source_ref: row.get(1)?,
                    source_kind: row.get(2)?,
                    source_id: row.get(3)?,
                    operation: "upsert".to_string(),
                })
            },
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn has_stale_source_states_for_agent(&self, agent_id: &str) -> Result<bool> {
        has_stale_source_states_for_agent_tx(&self.connection, agent_id)
    }

    pub fn has_backfill_checkpoints_for_agent(&self, agent_id: &str) -> Result<bool> {
        for source_kind in all_backfill_source_kinds() {
            let exists = self
                .connection
                .query_row(
                    "SELECT 1 FROM memory_index_checkpoints
                     WHERE agent_id = ?1 AND source_kind = ?2 AND cursor = ?3
                     LIMIT 1",
                    params![agent_id, source_kind, MEMORY_INDEX_BACKFILL_CURSOR],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !exists {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn agent_ids_with_pending_sources(&self) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT agent_id
             FROM memory_index_pending_sources
             ORDER BY agent_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn agent_ids_with_cursors(&self) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT agent_id
             FROM memory_index_cursors
             ORDER BY agent_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn has_current_source_state_for_agent(&self, agent_id: &str) -> Result<bool> {
        let stale: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM memory_index_source_state
             WHERE agent_id = ?1 AND (index_schema_version != ?2 OR projection_version != ?3)",
            params![
                agent_id,
                MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION,
                MEMORY_INDEX_DOCUMENT_PROJECTION_VERSION
            ],
            |row| row.get(0),
        )?;
        if stale > 0 {
            return Ok(false);
        }
        let states: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM memory_index_source_state WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?;
        Ok(states > 0)
    }

    pub fn document_hash(&self, agent_id: &str, source_ref: &str) -> Result<Option<String>> {
        let document_key = document_key_for(agent_id, source_ref);
        self.connection
            .query_row(
                "SELECT content_hash FROM memory_documents WHERE document_key = ?1",
                [document_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        agent_ids: &[String],
        source_kinds: &[String],
        active_workspace_id: Option<&str>,
        include_all_workspaces: bool,
    ) -> Result<Vec<MemorySearchResult>> {
        let query = search_query(query);
        let limit = limit.clamp(1, SEARCH_LIMIT_MAX);
        let agent_filter = if agent_ids.is_empty() {
            "0".to_string()
        } else {
            std::iter::repeat_n("?", agent_ids.len())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let source_kind_filter = if source_kinds.is_empty() {
            None
        } else {
            Some(
                std::iter::repeat_n("?", source_kinds.len())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        };
        let source_kind_clause = source_kind_filter
            .as_ref()
            // Bare placeholders continue after MATCH ?1 and the agent filter,
            // matching the order used to build sql_params below.
            .map(|filter| format!("AND d.source_kind IN ({filter})"))
            .unwrap_or_default();
        let workspace_filter = if include_all_workspaces {
            None
        } else {
            active_workspace_id.map(ToString::to_string)
        };
        let include_all_workspaces = include_all_workspaces as i64;
        let sql = format!(
            r#"
            WITH candidates AS (
                SELECT d.document_key, d.updated_at,
                       bm25(memory_documents_fts) AS score
                FROM memory_documents_fts
                JOIN memory_documents d ON d.document_key = memory_documents_fts.document_key
                WHERE memory_documents_fts MATCH ?1
                  AND d.agent_id IN ({agent_filter})
                  {source_kind_clause}
                  AND (? OR d.scope_kind = 'agent' OR (? IS NOT NULL AND d.workspace_id = ?))
                ORDER BY score ASC, d.updated_at DESC
                LIMIT ?
            )
            SELECT d.source_ref, d.source_kind, d.scope_kind, d.workspace_id, d.agent_id,
                   d.source_path, d.title, d.sanitized_excerpt, d.metadata_json,
                   candidates.updated_at, candidates.score
            FROM candidates
            JOIN memory_documents d ON d.document_key = candidates.document_key
            ORDER BY candidates.score ASC, candidates.updated_at DESC
            "#,
        );
        let workspace_value = workspace_filter
            .as_ref()
            .map(|value| SqlValue::Text(value.clone()))
            .unwrap_or(SqlValue::Null);
        let mut sql_params = Vec::with_capacity(agent_ids.len() + source_kinds.len() + 5);
        sql_params.push(SqlValue::Text(query));
        sql_params.extend(agent_ids.iter().cloned().map(SqlValue::Text));
        sql_params.extend(source_kinds.iter().cloned().map(SqlValue::Text));
        sql_params.push(SqlValue::Integer(include_all_workspaces));
        sql_params.push(workspace_value.clone());
        sql_params.push(workspace_value);
        sql_params.push(SqlValue::Integer(limit as i64));
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(sql_params), |row| {
            let metadata_json: String = row.get(8)?;
            let updated_at: String = row.get(9)?;
            Ok(MemorySearchResult {
                kind: row.get(1)?,
                source_ref: row.get(0)?,
                scope_kind: row.get(2)?,
                workspace_id: row.get(3)?,
                agent_id: row.get(4)?,
                source_path: row.get(5)?,
                title: row.get(6)?,
                snippet: row.get(7)?,
                score: row.get(10)?,
                updated_at: DateTime::parse_from_rfc3339(&updated_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                metadata: serde_json::from_str(&metadata_json).unwrap_or(Value::Null),
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    pub fn get(
        &self,
        source_ref: &str,
        max_chars: usize,
        agent_id: &str,
        _active_workspace_id: Option<&str>,
    ) -> Result<Option<MemoryGetResult>> {
        self.connection
            .query_row(
                r#"
                SELECT source_ref, source_kind, scope_kind, workspace_id, agent_id, source_path,
                       title, body, metadata_json, updated_at
                FROM memory_documents
                WHERE source_ref = ?1 AND agent_id = ?2
                "#,
                params![source_ref, agent_id],
                |row| {
                    let content: String = row.get(7)?;
                    let metadata_json: String = row.get(8)?;
                    let updated_at: String = row.get(9)?;
                    let (content, truncated) = truncate_chars(&content, max_chars);
                    Ok(MemoryGetResult {
                        source_ref: row.get(0)?,
                        kind: row.get(1)?,
                        scope_kind: row.get(2)?,
                        workspace_id: row.get(3)?,
                        agent_id: row.get(4)?,
                        source_path: row.get(5)?,
                        title: row.get(6)?,
                        content,
                        truncated,
                        updated_at: DateTime::parse_from_rfc3339(&updated_at)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now()),
                        metadata: serde_json::from_str(&metadata_json).unwrap_or(Value::Null),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn start_rebuild(
        &self,
        agent_id: &str,
        generation: &str,
        runtime_high_watermark: i64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.run_maintenance_write_transaction("memory_index.start_rebuild", |transaction| {
            transaction.execute(
                "INSERT INTO memory_index_rebuild_jobs (
                    agent_id, generation, phase, source_kind_index, source_cursor, source_offset,
                    runtime_high_watermark, documents_processed, started_at, last_progress_at
                 ) VALUES (?1, ?2, ?3, 0, '', 0, ?4, 0, ?5, ?5)
                 ON CONFLICT(agent_id) DO NOTHING",
                params![
                    agent_id,
                    generation,
                    MEMORY_INDEX_REBUILD_PHASE_SCAN,
                    runtime_high_watermark,
                    now.to_rfc3339(),
                ],
            )?;
            Ok(())
        })
    }

    pub fn advance_rebuild_source_kind(
        &self,
        job: &RebuildJob,
        next_index: usize,
        phase: &str,
    ) -> Result<()> {
        self.run_maintenance_write_transaction(
            "memory_index.advance_rebuild_cursor",
            |transaction| {
                transaction.execute(
                    "UPDATE memory_index_rebuild_jobs
                     SET phase = ?1, source_kind_index = ?2, source_cursor = '',
                         source_offset = 0, last_progress_at = ?3
                     WHERE agent_id = ?4 AND generation = ?5",
                    params![
                        phase,
                        i64::try_from(next_index).unwrap_or(i64::MAX),
                        Utc::now().to_rfc3339(),
                        job.agent_id,
                        job.generation,
                    ],
                )?;
                Ok(())
            },
        )
    }

    pub fn apply_rebuild_scan(
        &self,
        job: &RebuildJob,
        documents: &[MemoryDocument],
        next_cursor: &str,
        next_offset: usize,
    ) -> Result<()> {
        let now = Utc::now();
        self.run_maintenance_write_transaction("memory_index.rebuild_scan", |transaction| {
            for document in documents {
                upsert_document_if_needed_tx(transaction, document)?;
                upsert_source_state_tx(
                    transaction,
                    document,
                    source_id_from_ref(&document.source_ref),
                )?;
                transaction.execute(
                    "INSERT OR IGNORE INTO memory_index_rebuild_seen (
                        agent_id, generation, document_key
                     ) VALUES (?1, ?2, ?3)",
                    params![job.agent_id, job.generation, document_key(document)],
                )?;
            }
            transaction.execute(
                "UPDATE memory_index_rebuild_jobs
                 SET source_cursor = ?1, source_offset = ?2,
                     documents_processed = documents_processed + ?3,
                     last_progress_at = ?4
                 WHERE agent_id = ?5 AND generation = ?6",
                params![
                    next_cursor,
                    i64::try_from(next_offset).unwrap_or(i64::MAX),
                    i64::try_from(documents.len()).unwrap_or(i64::MAX),
                    now.to_rfc3339(),
                    job.agent_id,
                    job.generation,
                ],
            )?;
            Ok(())
        })
    }

    pub fn finalize_rebuild(
        &self,
        job: &RebuildJob,
        runtime_id: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.run_maintenance_write_transaction("memory_index.rebuild_finalize", |transaction| {
            anyhow::ensure!(rebuild_job_matches_tx(transaction, job)?, "memory index rebuild job changed before finalize");
            for source_kind in all_backfill_source_kinds() {
                transaction.execute(
                    "INSERT INTO memory_index_checkpoints (agent_id, source_kind, cursor, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(agent_id, source_kind) DO UPDATE SET
                        cursor=excluded.cursor,
                        updated_at=excluded.updated_at",
                    params![
                        job.agent_id,
                        source_kind,
                        MEMORY_INDEX_BACKFILL_CURSOR,
                        now.to_rfc3339(),
                    ],
                )?;
            }
            upsert_cursor_tx(
                transaction,
                runtime_id,
                &job.agent_id,
                MEMORY_INDEX_OUTBOX_CURSOR,
                job.runtime_high_watermark,
            )?;
            upsert_index_meta_tx(transaction, &job.agent_id, Some(now))?;
            transaction.execute(
                "DELETE FROM memory_index_rebuild_jobs
                 WHERE agent_id = ?1 AND generation = ?2",
                params![job.agent_id, job.generation],
            )?;
            Ok(())
        })
    }

    pub fn apply_stale_source(
        &self,
        agent_id: &str,
        source: &PendingSource,
        prepared: &PreparedPendingSource,
    ) -> Result<()> {
        self.run_write_transaction("memory_index.consume_stale_source", |transaction| {
            if let Err(error) = apply_prepared_pending_source_tx(transaction, source, prepared) {
                if is_retryable_memory_index_error(&error) {
                    return Err(error);
                }
                tracing::warn!(
                    agent_id,
                    source_kind = %source.source_kind,
                    source_ref = %source.source_ref,
                    error = %error,
                    "skipping failed stale memory index source refresh"
                );
            }
            Ok(())
        })
    }

    pub fn refresh_stale_meta(&self, agent_id: &str) -> Result<()> {
        if !self.has_stale_source_states_for_agent(agent_id)? {
            self.run_write_transaction("memory_index.refresh_stale_meta", |transaction| {
                if !has_stale_source_states_for_agent_tx(transaction, agent_id)? {
                    upsert_index_meta_tx(transaction, agent_id, None)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Commit the prepared projection and its applied sequence together.
    pub fn apply_outbox_change(
        &self,
        runtime_id: &str,
        agent_id: &str,
        source: &PendingSource,
        prepared: &PreparedPendingSource,
        change_seq: i64,
    ) -> Result<()> {
        self.run_write_transaction("memory_index.consume_runtime_outbox", |transaction| {
            apply_prepared_pending_source_tx(transaction, source, prepared)?;
            upsert_cursor_tx(
                transaction,
                runtime_id,
                agent_id,
                MEMORY_INDEX_OUTBOX_CURSOR,
                change_seq,
            )
        })
    }

    #[cfg(feature = "test-support")]
    pub fn test_connection(&self) -> &Connection {
        &self.connection
    }
}

#[cfg(test)]
mod maintenance_tests;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
