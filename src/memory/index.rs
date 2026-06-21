use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    agent_template::{agent_memory_operator_path, agent_memory_self_path},
    memory::refs::{RuntimeRef, ToolExecutionRefSelector, ToolOutputSelector},
    object_resolver::RuntimeObjectResolver,
    runtime_db::{EvidenceKind, RuntimeDb, RuntimeIndexOperation, RuntimeIndexOutboxRow},
    storage::AppStorage,
    tool::helpers::{
        command_digest, command_output_source_ref, command_preview, command_receipt_source_ref,
    },
    types::{
        BriefKind, BriefRecord, CommandTaskStatusSnapshot, ContextEpisodeRecord, MessageBody,
        MessageEnvelope, TaskRecord, TaskStatus, ToolExecutionRecord, TurnRecord, WorkItemRecord,
        WorkspaceEntry,
    },
};

const INDEX_FILENAME: &str = "memory.v2.sqlite3";
const LEGACY_INDEX_FILENAME: &str = "memory.sqlite3";
const DIRTY_FILENAME: &str = "memory.dirty";
const SEARCH_LIMIT_MAX: usize = 50;
const MEMORY_INDEX_OUTBOX_CONSUME_LIMIT: usize = 500;
const GET_CHARS_DEFAULT: usize = 12_000;
const GET_CHARS_MAX: usize = 50_000;
const MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION: i64 = 2;
const MEMORY_INDEX_BACKFILL_CURSOR: &str = "full";
const MEMORY_INDEX_OUTBOX_CURSOR: &str = "runtime_index_outbox";
const MEMORY_INDEX_SEARCH_TEXT_MAX_CHARS: usize = 12_000;

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
pub struct MemorySearchIndexStatus {
    pub freshness: String,
    pub cursor: i64,
    pub high_watermark: i64,
    pub lag: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub consumption_was_limited: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_error_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchQueryResult {
    pub results: Vec<MemorySearchResult>,
    pub index_status: MemorySearchIndexStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
struct MemoryDocument {
    source_ref: String,
    source_kind: String,
    scope_kind: String,
    workspace_id: Option<String>,
    agent_id: String,
    source_path: Option<PathBuf>,
    title: String,
    body: String,
    sanitized_excerpt: String,
    metadata: Value,
    updated_at: DateTime<Utc>,
}

pub fn memory_index_path(storage: &AppStorage) -> PathBuf {
    storage.shared_indexes_dir().join(INDEX_FILENAME)
}

fn legacy_memory_index_path(storage: &AppStorage) -> PathBuf {
    storage.shared_indexes_dir().join(LEGACY_INDEX_FILENAME)
}

pub fn rebuild_memory_index(storage: &AppStorage, active_workspace_id: Option<&str>) -> Result<()> {
    let mut index = MemoryIndex::open(storage)?;
    index.rebuild(storage, active_workspace_id)
}

pub(crate) fn enqueue_memory_index_upsert(
    storage: &AppStorage,
    source_kind: &str,
    source_id: &str,
    source_ref: &str,
) -> Result<()> {
    let index = MemoryIndex::open(storage)?;
    index.enqueue_source(
        &storage_agent_id(storage),
        source_kind,
        source_id,
        source_ref,
        "upsert",
        None,
        "source_write",
    )
}

pub fn repair_memory_index_for_paths(storage: &AppStorage, changed_paths: &[String]) -> Result<()> {
    let known = known_memory_markdown_sources(storage);
    if !changed_paths.iter().any(|path| {
        let path = Path::new(path);
        known.iter().any(|known| {
            path == known.path || known.path.ends_with(path) || path.ends_with(&known.path)
        })
    }) {
        return Ok(());
    }
    let index = MemoryIndex::open(storage)?;
    repair_known_markdown_sources(storage, &index)
}

pub fn search_memory(
    storage: &AppStorage,
    query: &str,
    limit: usize,
    active_workspace_id: Option<&str>,
    include_all_workspaces: bool,
) -> Result<Vec<MemorySearchResult>> {
    Ok(search_memory_query(
        storage,
        query,
        limit,
        active_workspace_id,
        include_all_workspaces,
    )?
    .results)
}

pub fn search_memory_query(
    storage: &AppStorage,
    query: &str,
    limit: usize,
    active_workspace_id: Option<&str>,
    include_all_workspaces: bool,
) -> Result<MemorySearchQueryResult> {
    search_memory_query_for_agents(
        storage,
        query,
        limit,
        active_workspace_id,
        include_all_workspaces,
        &[],
    )
}

pub fn search_memory_query_for_agents(
    storage: &AppStorage,
    query: &str,
    limit: usize,
    active_workspace_id: Option<&str>,
    include_all_workspaces: bool,
    agent_ids: &[String],
) -> Result<MemorySearchQueryResult> {
    search_memory_query_for_agent_storages(
        storage,
        query,
        limit,
        active_workspace_id,
        include_all_workspaces,
        agent_ids,
        &[],
    )
}

pub fn search_memory_query_for_agent_storages(
    storage: &AppStorage,
    query: &str,
    limit: usize,
    active_workspace_id: Option<&str>,
    include_all_workspaces: bool,
    agent_ids: &[String],
    agent_storages: &[AppStorage],
) -> Result<MemorySearchQueryResult> {
    let agent_id = storage_agent_id(storage);
    let agent_filter = normalize_memory_search_agent_filter(&agent_id, agent_ids);
    let index = ensure_memory_indexes_current(storage, active_workspace_id, agent_storages)?;
    let results = index.search(
        query,
        limit,
        &agent_filter,
        active_workspace_id,
        include_all_workspaces,
    )?;
    let index_status = index.index_status(storage, &agent_id)?;
    Ok(MemorySearchQueryResult {
        results,
        index_status,
    })
}

fn normalize_memory_search_agent_filter(
    default_agent_id: &str,
    agent_ids: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut filter = Vec::new();
    for agent_id in agent_ids {
        let agent_id = agent_id.trim();
        if !agent_id.is_empty() && seen.insert(agent_id.to_string()) {
            filter.push(agent_id.to_string());
        }
    }
    if filter.is_empty() {
        filter.push(default_agent_id.to_string());
    }
    filter
}

pub fn get_memory(
    storage: &AppStorage,
    source_ref: &str,
    max_chars: Option<usize>,
    active_workspace_id: Option<&str>,
) -> Result<Option<MemoryGetResult>> {
    let agent_id = storage_agent_id(storage);
    if let Ok(runtime_ref) = RuntimeRef::parse(source_ref) {
        let Some(document) = document_for_runtime_ref(storage, &runtime_ref)? else {
            return Ok(None);
        };
        if document.agent_id != agent_id {
            return Ok(None);
        }
        return Ok(Some(memory_get_result(document, max_chars)));
    }

    let index = ensure_memory_index_current(storage, active_workspace_id)?;
    index.get(source_ref, max_chars, &agent_id, active_workspace_id)
}

fn memory_get_result(document: MemoryDocument, max_chars: Option<usize>) -> MemoryGetResult {
    let max_chars = max_chars
        .unwrap_or(GET_CHARS_DEFAULT)
        .clamp(1, GET_CHARS_MAX);
    let (content, truncated) = truncate_chars(&document.body, max_chars);
    MemoryGetResult {
        kind: document.source_kind,
        source_ref: document.source_ref,
        scope_kind: document.scope_kind,
        workspace_id: document.workspace_id,
        agent_id: document.agent_id,
        source_path: document.source_path.map(|path| path.display().to_string()),
        title: document.title,
        content,
        truncated,
        updated_at: document.updated_at,
        metadata: document.metadata,
    }
}

fn ensure_memory_index_current(
    storage: &AppStorage,
    active_workspace_id: Option<&str>,
) -> Result<MemoryIndex> {
    ensure_memory_indexes_current(storage, active_workspace_id, &[])
}

fn ensure_memory_indexes_current(
    storage: &AppStorage,
    active_workspace_id: Option<&str>,
    agent_storages: &[AppStorage],
) -> Result<MemoryIndex> {
    log_legacy_index_deprecation(storage);
    let mut index = MemoryIndex::open(storage)?;
    let mut refreshed_agent_ids = BTreeSet::new();
    refresh_memory_index_for_storage(&mut index, storage, active_workspace_id)?;
    refreshed_agent_ids.insert(storage_agent_id(storage));
    for agent_storage in agent_storages {
        if refreshed_agent_ids.insert(storage_agent_id(agent_storage)) {
            refresh_memory_index_for_storage(&mut index, agent_storage, active_workspace_id)?;
        }
    }
    Ok(index)
}

fn refresh_memory_index_for_storage(
    index: &mut MemoryIndex,
    storage: &AppStorage,
    active_workspace_id: Option<&str>,
) -> Result<()> {
    index.consume_runtime_outbox(storage, MEMORY_INDEX_OUTBOX_CONSUME_LIMIT)?;
    index.consume_pending_sources(storage, MEMORY_INDEX_OUTBOX_CONSUME_LIMIT)?;
    repair_known_markdown_sources(storage, index)?;
    if memory_index_is_dirty(storage) {
        let agent_id = storage_agent_id(storage);
        tracing::debug!(
            agent_id,
            active_workspace_id,
            "memory index remains dirty; search returns bounded stale v2 projection"
        );
    }
    Ok(())
}

fn log_legacy_index_deprecation(storage: &AppStorage) {
    let v2_path = memory_index_path(storage);
    let legacy_path = legacy_memory_index_path(storage);
    if !v2_path.exists() && legacy_path.exists() {
        tracing::warn!(
            legacy_index_path = %legacy_path.display(),
            v2_index_path = %v2_path.display(),
            "legacy memory index v1 is ignored by memory index v2; run explicit rebuild/backfill if historical search results are needed"
        );
    }
}

fn repair_known_markdown_sources(storage: &AppStorage, index: &MemoryIndex) -> Result<()> {
    let agent_id = storage_agent_id(storage);
    for source in known_memory_markdown_sources(storage) {
        if source.path.exists() {
            let Some(document) =
                agent_memory_document(storage, source.name, source.title, &source.path)?
            else {
                continue;
            };
            if index.document_hash(&agent_id, &document.source_ref)?
                != Some(content_hash(&document.body))
            {
                index.upsert_document(&document)?;
            }
        } else {
            index.delete_document(&agent_id, source.source_ref)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct KnownMemoryMarkdownSource {
    source_ref: &'static str,
    name: &'static str,
    title: &'static str,
    path: PathBuf,
}

fn known_memory_markdown_sources(storage: &AppStorage) -> Vec<KnownMemoryMarkdownSource> {
    vec![
        KnownMemoryMarkdownSource {
            source_ref: "agent_memory:self",
            name: "self",
            title: "Agent self memory",
            path: agent_memory_self_path(storage.data_dir()),
        },
        KnownMemoryMarkdownSource {
            source_ref: "agent_memory:operator",
            name: "operator",
            title: "Operator collaboration memory",
            path: agent_memory_operator_path(storage.data_dir()),
        },
    ]
}

fn memory_index_is_dirty(storage: &AppStorage) -> bool {
    storage
        .shared_indexes_dir()
        .join(dirty_filename_for_agent(&storage_agent_id(storage)))
        .exists()
}

fn clear_memory_index_dirty(storage: &AppStorage) -> Result<()> {
    let path = storage
        .shared_indexes_dir()
        .join(dirty_filename_for_agent(&storage_agent_id(storage)));
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn dirty_filename_for_agent(agent_id: &str) -> String {
    let agent_key: String = agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    DIRTY_FILENAME.replace(".dirty", &format!(".{agent_key}.dirty"))
}

struct MemoryIndex {
    connection: Connection,
    last_outbox_consume_reached_limit: bool,
    last_outbox_error_count: usize,
}

impl MemoryIndex {
    fn open(storage: &AppStorage) -> Result<Self> {
        fs::create_dir_all(storage.shared_indexes_dir()).with_context(|| {
            format!(
                "failed to create {}",
                storage.shared_indexes_dir().display()
            )
        })?;
        let connection = Connection::open(memory_index_path(storage))?;
        let index = Self {
            connection,
            last_outbox_consume_reached_limit: false,
            last_outbox_error_count: 0,
        };
        index.connection.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;
            "#,
        )?;
        index.ensure_schema()?;
        Ok(index)
    }

    fn ensure_schema(&self) -> Result<()> {
        if self.table_exists("memory_documents")?
            && (self.table_has_column("memory_documents", "original_body")?
                || !self.table_has_column("memory_documents", "document_key")?)
        {
            self.connection.execute_batch(
                r#"
                DROP TABLE IF EXISTS memory_documents_fts;
                DROP TABLE IF EXISTS memory_documents;
                "#,
            )?;
        }
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
                last_error TEXT
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
                index_schema_version INTEGER NOT NULL
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
            "#,
        )?;
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

    fn rebuild(&mut self, storage: &AppStorage, active_workspace_id: Option<&str>) -> Result<()> {
        let agent_id = storage_agent_id(storage);
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM memory_documents_fts
             WHERE document_key IN (
                SELECT document_key FROM memory_documents WHERE agent_id = ?1
             )",
            [&agent_id],
        )?;
        transaction.execute(
            "DELETE FROM memory_documents WHERE agent_id = ?1",
            [&agent_id],
        )?;
        transaction.execute(
            "DELETE FROM memory_index_source_state WHERE agent_id = ?1",
            [&agent_id],
        )?;
        for document in collect_documents(storage, active_workspace_id)? {
            upsert_document_tx(&transaction, &document)?;
            upsert_source_state_tx(
                &transaction,
                &document,
                source_id_from_ref(&document.source_ref),
            )?;
        }
        transaction.execute(
            "DELETE FROM memory_index_pending_sources WHERE agent_id = ?1",
            [&agent_id],
        )?;
        for source_kind in all_backfill_source_kinds() {
            transaction.execute(
                "INSERT INTO memory_index_checkpoints (agent_id, source_kind, cursor, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(agent_id, source_kind) DO UPDATE SET
                    cursor=excluded.cursor,
                    updated_at=excluded.updated_at",
                params![
                    agent_id,
                    source_kind,
                    MEMORY_INDEX_BACKFILL_CURSOR,
                    Utc::now().to_rfc3339(),
                ],
            )?;
        }
        transaction.commit()?;
        clear_memory_index_dirty(storage)?;
        Ok(())
    }

    fn upsert_document(&self, document: &MemoryDocument) -> Result<()> {
        upsert_document_tx(&self.connection, document)?;
        upsert_source_state_tx(
            &self.connection,
            document,
            source_id_from_ref(&document.source_ref),
        )
    }

    fn delete_document(&self, agent_id: &str, source_ref: &str) -> Result<()> {
        let document_key = document_key_for(agent_id, source_ref);
        self.connection.execute(
            "DELETE FROM memory_documents_fts WHERE document_key = ?1",
            [document_key.as_str()],
        )?;
        self.connection.execute(
            "DELETE FROM memory_documents WHERE document_key = ?1",
            [document_key.as_str()],
        )?;
        self.connection.execute(
            "DELETE FROM memory_index_source_state WHERE document_key = ?1",
            [document_key.as_str()],
        )?;
        Ok(())
    }

    fn enqueue_source(
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
        self.connection.execute(
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
                enqueued_at=excluded.enqueued_at
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
    }

    fn consume_pending_sources(&mut self, storage: &AppStorage, limit: usize) -> Result<()> {
        let agent_id = storage_agent_id(storage);
        let pending = self.pending_sources_for_agent_with_limit(&agent_id, limit)?;
        for source in pending {
            let transaction = self.connection.transaction()?;
            let result = apply_pending_source_tx(&transaction, storage, &source);
            match result {
                Ok(()) => {
                    transaction.execute(
                        "DELETE FROM memory_index_pending_sources WHERE document_key = ?1",
                        [&source.document_key],
                    )?;
                    transaction.commit()?;
                }
                Err(error) => {
                    transaction.execute(
                        "UPDATE memory_index_pending_sources
                         SET attempts = attempts + 1, last_error = ?2
                         WHERE document_key = ?1",
                        params![source.document_key, error.to_string()],
                    )?;
                    transaction.commit()?;
                }
            }
        }
        Ok(())
    }

    fn consume_runtime_outbox(&mut self, storage: &AppStorage, limit: usize) -> Result<()> {
        self.last_outbox_consume_reached_limit = false;
        self.last_outbox_error_count = 0;
        let Some(runtime_db) = storage.runtime_db()? else {
            return Ok(());
        };
        let agent_id = storage_agent_id(storage);
        let runtime_id = runtime_index_runtime_id(&runtime_db);
        let cursor = self.cursor(&runtime_id, &agent_id, MEMORY_INDEX_OUTBOX_CURSOR)?;
        let rows = runtime_db
            .runtime_index_outbox()
            .read_after(&agent_id, cursor, limit)?;
        let row_count = rows.len();
        let mut last_change_seq = cursor;
        for row in rows {
            last_change_seq = last_change_seq.max(row.change_seq);
            let source = pending_source_from_outbox_row(&row);
            let transaction = self.connection.transaction()?;
            if let Err(error) = apply_pending_source_tx(&transaction, storage, &source) {
                self.last_outbox_error_count += 1;
                tracing::warn!(
                    agent_id = %row.agent_id,
                    change_seq = row.change_seq,
                    source_kind = %row.source_kind,
                    source_ref = %row.source_ref,
                    error = %error,
                    "skipping failed memory index outbox row"
                );
            }
            upsert_cursor_tx(
                &transaction,
                &runtime_id,
                &agent_id,
                MEMORY_INDEX_OUTBOX_CURSOR,
                last_change_seq,
            )?;
            transaction.commit()?;
        }
        self.last_outbox_consume_reached_limit = limit > 0 && row_count >= limit;
        Ok(())
    }

    fn cursor(&self, runtime_id: &str, agent_id: &str, cursor_kind: &str) -> Result<i64> {
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

    fn cursor_updated_at(
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

    fn index_status(
        &self,
        storage: &AppStorage,
        agent_id: &str,
    ) -> Result<MemorySearchIndexStatus> {
        let Some(runtime_db) = storage.runtime_db()? else {
            return Ok(MemorySearchIndexStatus {
                freshness: "fresh".into(),
                cursor: 0,
                high_watermark: 0,
                lag: 0,
                consumption_was_limited: false,
                skipped_error_count: self.last_outbox_error_count,
                last_indexed_at: None,
            });
        };
        let runtime_id = runtime_index_runtime_id(&runtime_db);
        let cursor = self.cursor(&runtime_id, agent_id, MEMORY_INDEX_OUTBOX_CURSOR)?;
        let high_watermark = runtime_db
            .runtime_index_outbox()
            .high_watermark_for_agent(agent_id)?;
        let lag = high_watermark.saturating_sub(cursor);
        let freshness = if !memory_index_path(storage).exists() {
            "missing"
        } else if memory_index_is_dirty(storage) || lag > 0 {
            "stale"
        } else {
            "fresh"
        };
        Ok(MemorySearchIndexStatus {
            freshness: freshness.into(),
            cursor,
            high_watermark,
            lag,
            consumption_was_limited: self.last_outbox_consume_reached_limit && lag > 0,
            skipped_error_count: self.last_outbox_error_count,
            last_indexed_at: self.cursor_updated_at(
                &runtime_id,
                agent_id,
                MEMORY_INDEX_OUTBOX_CURSOR,
            )?,
        })
    }

    #[cfg(test)]
    fn pending_sources_for_agent(&self, agent_id: &str) -> Result<Vec<PendingSource>> {
        self.pending_sources_for_agent_with_limit(agent_id, usize::MAX)
    }

    fn pending_sources_for_agent_with_limit(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<PendingSource>> {
        let mut statement = self.connection.prepare(
            "SELECT document_key, source_ref, agent_id, source_kind, source_id, operation
             FROM memory_index_pending_sources
             WHERE agent_id = ?1
             ORDER BY enqueued_at ASC, document_key ASC
             LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(params![agent_id, limit], |row| {
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

    #[cfg(test)]
    fn has_backfill_checkpoints_for_agent(&self, agent_id: &str) -> Result<bool> {
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

    #[cfg(test)]
    fn has_current_source_state_for_agent(&self, agent_id: &str) -> Result<bool> {
        let stale: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM memory_index_source_state
             WHERE agent_id = ?1 AND index_schema_version != ?2",
            params![agent_id, MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION],
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

    fn document_hash(&self, agent_id: &str, source_ref: &str) -> Result<Option<String>> {
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

    fn search(
        &self,
        query: &str,
        limit: usize,
        agent_ids: &[String],
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
        let workspace_filter = if include_all_workspaces {
            None
        } else {
            active_workspace_id.map(ToString::to_string)
        };
        let include_all_workspaces = include_all_workspaces as i64;
        let sql = format!(
            r#"
            SELECT d.source_ref, d.source_kind, d.scope_kind, d.workspace_id, d.agent_id,
                   d.source_path, d.title, d.sanitized_excerpt, d.metadata_json,
                   d.updated_at, bm25(memory_documents_fts) AS score
            FROM memory_documents_fts
            JOIN memory_documents d ON d.document_key = memory_documents_fts.document_key
            WHERE memory_documents_fts MATCH ?1
              AND d.agent_id IN ({agent_filter})
              AND (? OR d.scope_kind = 'agent' OR (? IS NOT NULL AND d.workspace_id = ?))
            ORDER BY score ASC, d.updated_at DESC
            LIMIT ?
            "#,
        );
        let workspace_value = workspace_filter
            .as_ref()
            .map(|value| SqlValue::Text(value.clone()))
            .unwrap_or(SqlValue::Null);
        let mut sql_params = Vec::with_capacity(agent_ids.len() + 5);
        sql_params.push(SqlValue::Text(query));
        sql_params.extend(agent_ids.iter().cloned().map(SqlValue::Text));
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

    fn get(
        &self,
        source_ref: &str,
        max_chars: Option<usize>,
        agent_id: &str,
        _active_workspace_id: Option<&str>,
    ) -> Result<Option<MemoryGetResult>> {
        let max_chars = max_chars
            .unwrap_or(GET_CHARS_DEFAULT)
            .clamp(1, GET_CHARS_MAX);
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
}

#[derive(Debug, Clone)]
struct PendingSource {
    document_key: String,
    source_ref: String,
    source_kind: String,
    source_id: String,
    operation: String,
}

fn pending_source_from_outbox_row(row: &RuntimeIndexOutboxRow) -> PendingSource {
    PendingSource {
        document_key: document_key_for(&row.agent_id, &row.source_ref),
        source_ref: row.source_ref.clone(),
        source_kind: row.source_kind.clone(),
        source_id: row.source_id.clone(),
        operation: row.operation.as_str().to_string(),
    }
}

fn apply_pending_source_tx(
    connection: &Connection,
    storage: &AppStorage,
    source: &PendingSource,
) -> Result<()> {
    if source.operation == RuntimeIndexOperation::Delete.as_str() {
        return delete_document_tx(connection, &source.document_key);
    }
    match document_for_pending_source(storage, source)? {
        Some(document) => {
            let hash = content_hash(&document.body);
            let existing_hash = connection
                .query_row(
                    "SELECT content_hash FROM memory_index_source_state WHERE document_key = ?1",
                    [&source.document_key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if existing_hash.as_deref() != Some(hash.as_str()) {
                upsert_document_tx(connection, &document)?;
            }
            upsert_source_state_tx(connection, &document, &source.source_id)
        }
        None => delete_document_tx(connection, &source.document_key),
    }
}

fn runtime_index_runtime_id(runtime_db: &RuntimeDb) -> String {
    runtime_db.path().display().to_string()
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
            last_change_seq=excluded.last_change_seq,
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
    let hash = content_hash(&document.body);
    let search_text = indexed_text(&bounded_search_text(&document.body));
    let document_key = document_key(document);
    let source_path = document
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());
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
        "DELETE FROM memory_documents_fts WHERE document_key = ?1",
        [document_key.as_str()],
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
    Ok(())
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
            content_hash, indexed_at, index_schema_version
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(document_key) DO UPDATE SET
            source_ref=excluded.source_ref,
            agent_id=excluded.agent_id,
            source_kind=excluded.source_kind,
            source_id=excluded.source_id,
            source_updated_at=excluded.source_updated_at,
            content_hash=excluded.content_hash,
            indexed_at=excluded.indexed_at,
            index_schema_version=excluded.index_schema_version
        "#,
        params![
            document_key(document),
            document.source_ref,
            document.agent_id,
            document.source_kind,
            source_id,
            document.updated_at.to_rfc3339(),
            content_hash(&document.body),
            Utc::now().to_rfc3339(),
            MEMORY_INDEX_DOCUMENT_SCHEMA_VERSION,
        ],
    )?;
    Ok(())
}

fn delete_document_tx(connection: &Connection, document_key: &str) -> Result<()> {
    connection.execute(
        "DELETE FROM memory_documents_fts WHERE document_key = ?1",
        [document_key],
    )?;
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

fn document_key(document: &MemoryDocument) -> String {
    document_key_for(&document.agent_id, &document.source_ref)
}

fn document_key_for(agent_id: &str, source_ref: &str) -> String {
    format!("{agent_id}:{source_ref}")
}

fn all_backfill_source_kinds() -> &'static [&'static str] {
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

fn collect_documents(
    storage: &AppStorage,
    _active_workspace_id: Option<&str>,
) -> Result<Vec<MemoryDocument>> {
    let runtime_db = storage.runtime_db()?;
    let mut documents = Vec::new();
    documents.extend(agent_memory_documents(storage)?);
    documents.extend(workspace_profile_documents(storage)?);
    documents.extend(message_documents(storage)?);
    documents.extend(brief_documents(storage, runtime_db.as_ref())?);
    documents.extend(context_episode_documents(storage)?);
    documents.extend(work_item_documents(storage, runtime_db.as_ref())?);
    documents.extend(task_documents(storage, runtime_db.as_ref())?);
    documents.extend(command_execution_documents(storage, runtime_db.as_ref())?);
    Ok(documents)
}

fn document_for_pending_source(
    storage: &AppStorage,
    source: &PendingSource,
) -> Result<Option<MemoryDocument>> {
    match source.source_kind.as_str() {
        "agent_memory_markdown" => known_memory_markdown_sources(storage)
            .into_iter()
            .find(|known| known.source_ref == source.source_ref)
            .map(|known| agent_memory_document(storage, known.name, known.title, &known.path))
            .transpose()
            .map(|value| value.flatten()),
        "workspace_profile" => workspace_profile_document_by_id(storage, &source.source_id),
        "message" => message_document_by_id(storage, &source.source_id),
        "brief" => brief_document_by_id(storage, &source.source_id),
        "context_episode" => context_episode_document_by_id(storage, &source.source_id),
        "work_item" => work_item_document_by_id(storage, &source.source_id),
        "task" => task_document_by_id(storage, &source.source_id),
        "tool_command_receipt" | "tool_command_output" | "tool_command_output_preview" => {
            command_tool_execution_document_by_ref(storage, &source.source_ref)
        }
        "tool_execution_output" | "tool_execution_output_preview" => {
            generic_tool_execution_output_document_by_ref(storage, &source.source_ref)
        }
        _ => Ok(None),
    }
}

fn document_for_runtime_ref(
    storage: &AppStorage,
    runtime_ref: &RuntimeRef,
) -> Result<Option<MemoryDocument>> {
    match runtime_ref {
        RuntimeRef::AgentMemory { name } => known_memory_markdown_sources(storage)
            .into_iter()
            .find(|known| known.name == name)
            .map(|known| agent_memory_document(storage, known.name, known.title, &known.path))
            .transpose()
            .map(|value| value.flatten()),
        RuntimeRef::WorkspaceProfile { workspace_id } => {
            workspace_profile_document_by_id(storage, workspace_id)
        }
        RuntimeRef::Brief { id } => brief_document_by_id(storage, id),
        RuntimeRef::Message { id } => message_document_by_id(storage, id),
        RuntimeRef::Turn { id } => turn_document_by_id(storage, id),
        RuntimeRef::Episode { id } => context_episode_document_by_id(storage, id),
        RuntimeRef::WorkItem { id } => work_item_document_by_id(storage, id),
        RuntimeRef::Task { id } => task_document_by_id(storage, id),
        RuntimeRef::ToolExecution {
            id,
            batch_item_index,
            selector,
        } => {
            let document =
                command_tool_execution_document(storage, id, *batch_item_index, *selector)?;
            if document.is_some() {
                Ok(document)
            } else {
                generic_tool_execution_output_document(storage, id, *batch_item_index, *selector)
            }
        }
    }
}

fn agent_memory_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    let mut documents = Vec::new();
    for source in known_memory_markdown_sources(storage) {
        if let Some(document) =
            agent_memory_document(storage, source.name, source.title, &source.path)?
        {
            documents.push(document);
        }
    }
    Ok(documents)
}

fn agent_memory_document(
    storage: &AppStorage,
    name: &str,
    title: &str,
    path: &Path,
) -> Result<Option<MemoryDocument>> {
    if !path.exists() {
        return Ok(None);
    }
    let body =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(MemoryDocument {
        source_ref: format!("agent_memory:{name}"),
        source_kind: "agent_memory_markdown".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: storage_agent_id(storage),
        source_path: Some(path.to_path_buf()),
        title: title.into(),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "memory_name": name,
            "governance_surface": "curated_durable_memory",
            "provenance_class": "agent_home_memory_markdown",
            "trust_class": "agent_curated",
        }),
        updated_at: file_updated_at(path),
    }))
}

fn workspace_profile_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    let mut latest = BTreeMap::<String, WorkspaceEntry>::new();
    for entry in storage.read_recent_workspace_entries(usize::MAX)? {
        latest.insert(entry.workspace_id.clone(), entry);
    }
    Ok(latest
        .into_values()
        .map(|entry| {
            let title = entry
                .repo_name
                .clone()
                .unwrap_or_else(|| format!("Workspace {}", entry.workspace_id));
            let body = format!(
                "{}\nworkspace_id: {}\nworkspace_anchor: {}",
                title,
                entry.workspace_id,
                entry.workspace_anchor.display()
            );
            MemoryDocument {
                source_ref: format!("workspace_profile:{}", entry.workspace_id),
                source_kind: "workspace_profile".into(),
                scope_kind: "workspace".into(),
                workspace_id: Some(entry.workspace_id.clone()),
                agent_id: storage_agent_id(storage),
                source_path: Some(entry.workspace_anchor.clone()),
                title,
                sanitized_excerpt: excerpt(&body),
                body,
                metadata: json!({
                    "workspace_anchor": entry.workspace_anchor,
                    "governance_surface": "workspace_profile_projection",
                    "provenance_class": "workspace_registry",
                    "trust_class": "runtime_projection",
                }),
                updated_at: entry.updated_at,
            }
        })
        .collect())
}

fn workspace_profile_document_by_id(
    storage: &AppStorage,
    workspace_id: &str,
) -> Result<Option<MemoryDocument>> {
    Ok(workspace_profile_documents(storage)?
        .into_iter()
        .find(|document| document.workspace_id.as_deref() == Some(workspace_id)))
}

fn brief_documents(
    storage: &AppStorage,
    runtime_db: Option<&RuntimeDb>,
) -> Result<Vec<MemoryDocument>> {
    let briefs = if let Some(runtime_db) = runtime_db {
        runtime_db
            .evidence()
            .recent_payloads(EvidenceKind::Brief, &storage_agent_id(storage), usize::MAX)?
            .into_iter()
            .map(|row| serde_json::from_str::<BriefRecord>(&row.payload_json).map_err(Into::into))
            .collect::<Result<Vec<_>>>()?
    } else {
        storage.read_recent_briefs(usize::MAX)?
    };
    Ok(briefs
        .into_iter()
        .filter(semantic_brief_is_retrievable)
        .map(|brief| brief_document(storage, brief))
        .collect())
}

fn semantic_brief_is_retrievable(brief: &BriefRecord) -> bool {
    brief.kind != BriefKind::Ack && !brief.text.trim().is_empty()
}

fn brief_document(storage: &AppStorage, brief: BriefRecord) -> MemoryDocument {
    let body = RuntimeObjectResolver::new(storage)
        .resolve_brief_content(&brief)
        .unwrap_or_else(|_| brief.text.clone());
    MemoryDocument {
        source_ref: format!("brief:{}", brief.id),
        source_kind: "brief".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(brief.workspace_id),
        agent_id: brief.agent_id,
        source_path: None,
        title: format!("Brief {:?}", brief.kind),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "work_item_id": brief.work_item_id,
            "related_message_id": brief.related_message_id,
            "related_task_id": brief.related_task_id,
            "agent_home": storage.data_dir(),
            "governance_surface": "runtime_evidence",
            "provenance_class": "brief_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: brief.created_at,
    }
}

fn brief_document_by_id(storage: &AppStorage, brief_id: &str) -> Result<Option<MemoryDocument>> {
    let runtime_db = storage.runtime_db()?;
    let brief = if let Some(runtime_db) = runtime_db.as_ref() {
        runtime_db
            .evidence()
            .payload_by_id(EvidenceKind::Brief, &storage_agent_id(storage), brief_id)?
            .map(|row| serde_json::from_str::<BriefRecord>(&row.payload_json))
            .transpose()?
    } else {
        storage
            .read_recent_briefs(usize::MAX)?
            .into_iter()
            .find(|brief| brief.id == brief_id)
    };
    Ok(brief
        .filter(semantic_brief_is_retrievable)
        .map(|brief| brief_document(storage, brief)))
}

fn message_document_by_id(
    storage: &AppStorage,
    message_id: &str,
) -> Result<Option<MemoryDocument>> {
    Ok(storage
        .read_message_by_id(message_id)?
        .map(message_document))
}

fn message_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    Ok(storage
        .read_all_messages()?
        .into_iter()
        .map(message_document)
        .collect())
}

fn message_document(message: MessageEnvelope) -> MemoryDocument {
    let body = message_document_body(&message);
    let excerpt_body = message_body_text_for_memory(&message.body);
    let title = format!("Message {}", message.id);
    MemoryDocument {
        source_ref: format!("message:{}", message.id),
        source_kind: "message".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: message.agent_id.clone(),
        source_path: None,
        title,
        sanitized_excerpt: excerpt(&excerpt_body),
        body,
        metadata: json!({
            "message_id": message.id,
            "turn_id": message.turn_id,
            "message_seq": message.message_seq,
            "work_item_id": message.work_item_id,
            "task_id": message.task_id,
            "governance_surface": "runtime_evidence",
            "provenance_class": "message_envelope",
            "trust_class": "runtime_evidence",
        }),
        updated_at: message.created_at,
    }
}

fn message_document_body(message: &MessageEnvelope) -> String {
    let mut lines = vec![
        format!("message_ref: message:{}", message.id),
        format!("message_id: {}", message.id),
    ];
    if let Some(turn_id) = message.turn_id.as_deref() {
        lines.push(format!("turn_ref: turn:{turn_id}"));
    }
    if let Some(message_seq) = message.message_seq {
        lines.push(format!("message_seq: {message_seq}"));
    }
    lines.push(format!("kind: {:?}", message.kind));
    lines.push(format!("origin: {:?}", message.origin));
    lines.push(format!("authority_class: {:?}", message.authority_class));
    lines.push(format!("priority: {:?}", message.priority));
    if let Some(trigger_kind) = message.trigger_kind {
        lines.push(format!("trigger_kind: {:?}", trigger_kind));
    }
    if let Some(work_item_id) = message.work_item_id.as_deref() {
        lines.push(format!("work_item_ref: work_item:{work_item_id}"));
    }
    if let Some(task_id) = message.task_id.as_deref() {
        lines.push(format!("task_ref: task:{task_id}"));
    }
    if let Some(delivery_surface) = message.delivery_surface {
        lines.push(format!("delivery_surface: {:?}", delivery_surface));
    }
    if let Some(admission_context) = message.admission_context {
        lines.push(format!("admission_context: {:?}", admission_context));
    }
    if !message.source_refs.is_empty() {
        lines.push(format!(
            "source_refs: {}",
            message
                .source_refs
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let body = message_body_text_for_memory(&message.body);
    lines.push("body:".to_string());
    lines.push(truncate_multiline(&body, 8_000));
    lines.join("\n")
}

fn message_body_text_for_memory(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

fn truncate_multiline(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n[truncated]")
    } else {
        truncated
    }
}

fn context_episode_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    Ok(storage
        .read_recent_context_episodes(usize::MAX)?
        .into_iter()
        .map(episode_document)
        .collect())
}

fn episode_document(episode: ContextEpisodeRecord) -> MemoryDocument {
    let body = episode_document_body(&episode);
    let title = episode
        .work_summary
        .clone()
        .or_else(|| episode.objective.clone())
        .unwrap_or_else(|| format!("Episode {}", episode.id));
    MemoryDocument {
        source_ref: format!("episode:{}", episode.id),
        source_kind: "context_episode".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(episode.workspace_id),
        agent_id: episode.agent_id,
        source_path: None,
        title,
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "episode_id": episode.id,
            "current_work_item_id": episode.current_work_item_id,
            "boundary_reason": episode.boundary_reason,
            "working_set_files": episode.working_set_files,
            "source_refs": episode.source_refs,
            "governance_surface": "runtime_evidence",
            "provenance_class": "context_episode_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: episode.finalized_at,
    }
}

fn context_episode_document_by_id(
    storage: &AppStorage,
    episode_id: &str,
) -> Result<Option<MemoryDocument>> {
    Ok(storage
        .read_recent_context_episodes(usize::MAX)?
        .into_iter()
        .find(|episode| episode.id == episode_id)
        .map(episode_document))
}

fn episode_document_body(episode: &ContextEpisodeRecord) -> String {
    let mut lines = vec![
        format!("episode_ref: episode:{}", episode.id),
        format!(
            "turns: {}-{}",
            episode.start_turn_index, episode.end_turn_index
        ),
        format!("boundary: {:?}", episode.boundary_reason),
    ];
    if let Some(work_item_id) = episode.current_work_item_id.as_deref() {
        lines.push(format!("work_item_ref: work_item:{work_item_id}"));
    }
    if let Some(objective) = episode.objective.as_deref() {
        lines.push(format!(
            "objective_preview: {}",
            truncate_inline(objective, 180)
        ));
    }
    if let Some(work_summary) = episode.work_summary.as_deref() {
        lines.push(format!(
            "work_summary_preview: {}",
            truncate_inline(work_summary, 180)
        ));
    }
    if !episode.source_refs.is_empty() {
        lines.push(format!(
            "retrieval_refs: {}",
            episode
                .source_refs
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.source_turn_ids.is_empty() {
        lines.push(format!(
            "provenance_turn_ids: {}",
            episode
                .source_turn_ids
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.working_set_files.is_empty() {
        lines.push(format!(
            "files: {}",
            episode
                .working_set_files
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.carry_forward.is_empty() {
        lines.push(format!(
            "followups: {}",
            episode
                .carry_forward
                .iter()
                .take(8)
                .map(|item| truncate_inline(item, 160))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.waiting_on.is_empty() {
        lines.push(format!(
            "waiting_on: {}",
            episode
                .waiting_on
                .iter()
                .take(8)
                .map(|item| truncate_inline(item, 160))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.join("\n")
}

fn turn_document_by_id(storage: &AppStorage, turn_id: &str) -> Result<Option<MemoryDocument>> {
    Ok(storage
        .read_recent_turns(usize::MAX)?
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)
        .map(turn_document))
}

fn turn_document(turn: TurnRecord) -> MemoryDocument {
    let body = turn_document_body(&turn);
    MemoryDocument {
        source_ref: format!("turn:{}", turn.turn_id),
        source_kind: "turn".into(),
        scope_kind: "workspace".into(),
        workspace_id: None,
        agent_id: turn.agent_id,
        source_path: None,
        title: format!("Turn {}", turn.turn_index),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "turn_id": turn.turn_id,
            "turn_index": turn.turn_index,
            "current_work_item_id": turn.current_work_item_id,
            "governance_surface": "runtime_evidence",
            "provenance_class": "turn_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: turn.created_at,
    }
}

fn turn_document_body(turn: &TurnRecord) -> String {
    let mut lines = vec![
        format!("turn_ref: turn:{}", turn.turn_id),
        format!("turn_index: {}", turn.turn_index),
    ];
    if let Some(trigger) = turn.trigger.as_ref() {
        lines.push(format!("trigger_kind: {:?}", trigger.kind));
        if let Some(message_id) = trigger.message_id.as_deref() {
            lines.push(format!("trigger_message_ref: message:{message_id}"));
        }
    }
    if let Some(work_item_id) = turn.current_work_item_id.as_deref() {
        lines.push(format!("current_work_item_ref: work_item:{work_item_id}"));
    }
    if !turn.input_message_ids.is_empty() {
        lines.push(format!(
            "input_message_refs: {}",
            turn.input_message_ids
                .iter()
                .take(12)
                .map(|id| format!("message:{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !turn.tool_execution_ids.is_empty() {
        append_id_list(&mut lines, "tool_execution_ids", &turn.tool_execution_ids);
        lines.push(format!(
            "tool_execution_refs_when_available: {}",
            turn.tool_execution_ids
                .iter()
                .take(12)
                .flat_map(|id| {
                    [
                        format!("tool_execution:{id}:cmd"),
                        format!("tool_execution:{id}:output"),
                    ]
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !turn.produced_brief_ids.is_empty() {
        lines.push(format!(
            "brief_refs_when_semantic: {}",
            turn.produced_brief_ids
                .iter()
                .take(12)
                .map(|id| format!("brief:{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !turn.completed_work_item_ids.is_empty() {
        lines.push(format!(
            "completed_work_item_refs: {}",
            turn.completed_work_item_ids
                .iter()
                .take(12)
                .map(|id| format!("work_item:{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    append_id_list(
        &mut lines,
        "delivery_summary_ids",
        &turn.delivery_summary_ids,
    );
    append_id_list(
        &mut lines,
        "waiting_condition_ids",
        &turn.waiting_condition_ids,
    );
    if let Some(terminal) = turn.terminal.as_ref() {
        lines.push(format!("terminal_kind: {:?}", terminal.kind));
        if let Some(reason) = terminal.reason.as_deref() {
            lines.push(format!("terminal_reason: {}", truncate_inline(reason, 180)));
        }
    }
    lines.join("\n")
}

fn append_id_list(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!(
        "{label}: {}",
        values
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    ));
}

fn truncate_inline(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn work_item_documents(
    storage: &AppStorage,
    runtime_db: Option<&RuntimeDb>,
) -> Result<Vec<MemoryDocument>> {
    let latest = if let Some(runtime_db) = runtime_db {
        runtime_db
            .work_items()
            .latest_for_agent(&storage_agent_id(storage), usize::MAX)?
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect()
    } else {
        let mut latest = BTreeMap::<String, WorkItemRecord>::new();
        for item in storage.read_recent_work_items(usize::MAX)? {
            latest.insert(item.id.clone(), item);
        }
        latest
    };
    Ok(latest.into_values().map(work_item_document).collect())
}

fn work_item_document(item: WorkItemRecord) -> MemoryDocument {
    let body = work_item_document_body(&item);
    MemoryDocument {
        source_ref: format!("work_item:{}", item.id),
        source_kind: "work_item".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(item.workspace_id),
        agent_id: item.agent_id,
        source_path: None,
        title: item.objective.clone(),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "work_item_id": item.id,
            "state": item.state,
            "blocked_by": item.blocked_by,
            "governance_surface": "runtime_evidence",
            "provenance_class": "work_item_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: item.updated_at,
    }
}

fn work_item_document_by_id(
    storage: &AppStorage,
    work_item_id: &str,
) -> Result<Option<MemoryDocument>> {
    let runtime_db = storage.runtime_db()?;
    let item = if let Some(runtime_db) = runtime_db.as_ref() {
        runtime_db.work_items().latest(work_item_id)?
    } else {
        storage
            .read_recent_work_items(usize::MAX)?
            .into_iter()
            .rev()
            .find(|item| item.id == work_item_id)
    };
    Ok(item.map(work_item_document))
}

fn work_item_document_body(item: &WorkItemRecord) -> String {
    let mut lines = vec![
        format!("Objective: {}", item.objective),
        format!(
            "Plan status: {}",
            work_item_plan_status_label(item.plan_status)
        ),
    ];
    if let Some(plan_artifact) = item.plan_artifact.as_ref() {
        if !plan_artifact.preview.trim().is_empty() {
            lines.push("Plan preview:".into());
            lines.push(plan_artifact.preview.clone());
        }
    }
    if let Some(result_brief_id) = item
        .result_brief_id
        .as_ref()
        .filter(|result_brief_id| !result_brief_id.trim().is_empty())
    {
        lines.push(format!("Result ref: brief:{result_brief_id}"));
    }
    if let Some(result_summary) = item
        .result_summary
        .as_ref()
        .filter(|result_summary| !result_summary.trim().is_empty())
    {
        lines.push("Legacy result summary:".into());
        lines.push(result_summary.clone());
    }
    if !item.todo_list.is_empty() {
        lines.push("Todo list:".into());
        for todo in &item.todo_list {
            lines.push(format!(
                "- [{}] {}",
                todo_item_state_label(todo.state),
                todo.text
            ));
        }
    }
    lines.join("\n")
}

fn task_documents(
    storage: &AppStorage,
    runtime_db: Option<&RuntimeDb>,
) -> Result<Vec<MemoryDocument>> {
    let tasks = if let Some(runtime_db) = runtime_db {
        runtime_db
            .tasks()
            .latest_for_agent(&storage_agent_id(storage), usize::MAX)?
    } else {
        storage.latest_task_records_from_recent(usize::MAX)?
    };
    Ok(tasks.into_iter().map(task_document).collect())
}

fn task_document(task: TaskRecord) -> MemoryDocument {
    let agent_id = task.agent_id.clone();
    let task_kind = task.kind.as_str();
    let command = CommandTaskStatusSnapshot::identity_from_task_record(&task);
    let command = command.as_ref();
    let command_text = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("cmd"))
        .and_then(Value::as_str)
        .or_else(|| command.and_then(|entry| entry.cmd.as_deref()));
    let child_agent_id = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("child_agent_id"))
        .and_then(Value::as_str);
    let exit_status = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("exit_status"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let mut body = task_document_body(
        &task,
        task_kind,
        command,
        command_text,
        child_agent_id,
        exit_status,
    );
    if let Some(cmd) = command_text {
        body.push('\n');
        if cmd.contains('\n') {
            body.push_str("cmd:\n");
        } else {
            body.push_str("cmd: ");
        }
        body.push_str(cmd);
    }
    MemoryDocument {
        source_ref: format!("task:{}", task.id),
        source_kind: "task".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: agent_id.clone(),
        source_path: None,
        title: task
            .summary
            .clone()
            .unwrap_or_else(|| format!("Task {}", task.id)),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "task_id": task.id,
            "kind": task_kind,
            "status": task_status_label(&task.status),
            "summary": task.summary,
            "work_item_id": task.work_item_id,
            "cmd_digest": task_command_digest(command, command_text),
            "cmd_preview": command
                .and_then(|entry| entry.cmd.as_deref())
                .or(command_text)
                .map(command_preview),
            "exit_status": exit_status,
            "agent_id": agent_id,
            "created_at": task.created_at.to_rfc3339(),
            "governance_surface": "runtime_evidence",
            "provenance_class": "task_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: task.updated_at,
    }
}

fn task_document_by_id(storage: &AppStorage, task_id: &str) -> Result<Option<MemoryDocument>> {
    let runtime_db = storage.runtime_db()?;
    let task = if let Some(runtime_db) = runtime_db.as_ref() {
        runtime_db.tasks().latest(task_id)?
    } else {
        storage.latest_task_record(task_id)?
    };
    Ok(task.map(task_document))
}

fn task_document_body(
    task: &TaskRecord,
    task_kind: &str,
    command: Option<&CommandTaskStatusSnapshot>,
    command_text: Option<&str>,
    child_agent_id: Option<&str>,
    exit_status: Option<i32>,
) -> String {
    let mut lines = vec![
        format!("task_id: {}", task.id),
        format!("kind: {task_kind}"),
        format!("status: {}", task_status_label(&task.status)),
        format!("summary: {}", task.summary.clone().unwrap_or_default()),
        format!("created_at: {}", task.created_at.to_rfc3339()),
        format!("updated_at: {}", task.updated_at.to_rfc3339()),
    ];
    if let Some(work_item_id) = task.work_item_id.as_deref() {
        lines.push(format!("work_item_id: {work_item_id}"));
    }
    if let Some(cmd_digest) = task_command_digest(command, command_text) {
        lines.push(format!("cmd_digest: {cmd_digest}"));
    }
    if let Some(cmd_preview) = command
        .and_then(|entry| entry.cmd.as_deref())
        .or(command_text)
        .map(command_preview)
    {
        lines.push(format!("cmd_preview: {cmd_preview}"));
    }
    if let Some(exit_status) = exit_status {
        lines.push(format!("exit_status: {exit_status}"));
    }
    if let Some(child_agent_id) = child_agent_id {
        lines.push(format!("child_agent_id: {child_agent_id}"));
    }
    lines.join("\n")
}

fn task_command_digest(
    command: Option<&CommandTaskStatusSnapshot>,
    command_text: Option<&str>,
) -> Option<String> {
    command
        .and_then(|entry| {
            entry
                .cmd_digest
                .clone()
                .or_else(|| entry.cmd.as_ref().map(|cmd| command_digest(cmd)))
        })
        .or_else(|| command_text.map(command_digest))
}

fn task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

fn command_execution_documents(
    storage: &AppStorage,
    runtime_db: Option<&RuntimeDb>,
) -> Result<Vec<MemoryDocument>> {
    let mut documents = Vec::new();
    let records = if let Some(runtime_db) = runtime_db {
        runtime_db
            .evidence()
            .recent_payloads(
                EvidenceKind::ToolExecution,
                &storage_agent_id(storage),
                usize::MAX,
            )?
            .into_iter()
            .map(|row| {
                serde_json::from_str::<ToolExecutionRecord>(&row.payload_json).map_err(Into::into)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        storage.read_recent_tool_executions(usize::MAX)?
    };
    for record in records {
        match record.tool_name.as_str() {
            "ExecCommand" => {
                if let Some(cmd) = record.input.get("cmd").and_then(Value::as_str) {
                    documents.push(command_receipt_document(&record, None, None, cmd));
                }
            }
            "ExecCommandBatch" => {
                if let Some(items) = record.input.get("items").and_then(Value::as_array) {
                    for (offset, item) in items.iter().enumerate() {
                        if let Some(cmd) = item.get("cmd").and_then(Value::as_str) {
                            let index = offset + 1;
                            documents.push(command_receipt_document(
                                &record,
                                Some(index),
                                Some(item),
                                cmd,
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(documents)
}

fn command_tool_execution_document_by_ref(
    storage: &AppStorage,
    source_ref: &str,
) -> Result<Option<MemoryDocument>> {
    let runtime_ref = RuntimeRef::parse(source_ref).ok();
    if let Some(RuntimeRef::ToolExecution {
        id,
        batch_item_index,
        selector,
    }) = runtime_ref
    {
        return command_tool_execution_document(storage, &id, batch_item_index, selector);
    };
    Ok(None)
}

fn generic_tool_execution_output_document_by_ref(
    storage: &AppStorage,
    source_ref: &str,
) -> Result<Option<MemoryDocument>> {
    let runtime_ref = RuntimeRef::parse(source_ref).ok();
    if let Some(RuntimeRef::ToolExecution {
        id,
        batch_item_index,
        selector,
    }) = runtime_ref
    {
        return generic_tool_execution_output_document(storage, &id, batch_item_index, selector);
    };
    Ok(None)
}

fn tool_execution_record(
    storage: &AppStorage,
    tool_execution_id: &str,
) -> Result<Option<ToolExecutionRecord>> {
    let runtime_db = storage.runtime_db()?;
    if let Some(runtime_db) = runtime_db.as_ref() {
        Ok(runtime_db
            .evidence()
            .payload_by_id(
                EvidenceKind::ToolExecution,
                &storage_agent_id(storage),
                tool_execution_id,
            )?
            .map(|row| serde_json::from_str::<ToolExecutionRecord>(&row.payload_json))
            .transpose()?)
    } else {
        Ok(storage
            .read_recent_tool_executions(usize::MAX)?
            .into_iter()
            .find(|record| record.id == tool_execution_id))
    }
}

fn command_tool_execution_document(
    storage: &AppStorage,
    tool_execution_id: &str,
    batch_item_index: Option<usize>,
    selector: ToolExecutionRefSelector,
) -> Result<Option<MemoryDocument>> {
    let record = tool_execution_record(storage, tool_execution_id)?;
    let Some(record) = record else {
        return Ok(None);
    };
    match (record.tool_name.as_str(), batch_item_index, selector) {
        ("ExecCommand", None, ToolExecutionRefSelector::Cmd) => Ok(record
            .input
            .get("cmd")
            .and_then(Value::as_str)
            .map(|cmd| command_receipt_document(&record, None, None, cmd))),
        ("ExecCommandBatch", Some(index), ToolExecutionRefSelector::Cmd) => Ok(record
            .input
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| items.get(index - 1))
            .and_then(|item| {
                item.get("cmd")
                    .and_then(Value::as_str)
                    .map(|cmd| command_receipt_document(&record, Some(index), Some(item), cmd))
            })),
        ("ExecCommand", None, ToolExecutionRefSelector::Output(stream)) => Ok(
            command_output_document(&record, None, stream.as_ref_selector()),
        ),
        ("ExecCommandBatch", Some(index), ToolExecutionRefSelector::Output(stream)) => Ok(
            command_output_document(&record, Some(index), stream.as_ref_selector()),
        ),
        _ => Ok(None),
    }
}

fn generic_tool_execution_output_document(
    storage: &AppStorage,
    tool_execution_id: &str,
    batch_item_index: Option<usize>,
    selector: ToolExecutionRefSelector,
) -> Result<Option<MemoryDocument>> {
    if batch_item_index.is_some()
        || selector != ToolExecutionRefSelector::Output(ToolOutputSelector::Output)
    {
        return Ok(None);
    }
    let Some(record) = tool_execution_record(storage, tool_execution_id)? else {
        return Ok(None);
    };
    if matches!(
        record.tool_name.as_str(),
        "ExecCommand" | "ExecCommandBatch"
    ) {
        return Ok(None);
    }
    Ok(Some(generic_tool_execution_output_document_from_record(
        &record,
    )))
}

fn generic_tool_execution_output_document_from_record(
    record: &ToolExecutionRecord,
) -> MemoryDocument {
    let source_ref = command_output_source_ref(&record.id, None, "output");
    let output = json!({
        "source_type": "tool_execution_output",
        "tool_execution_id": record.id,
        "tool_name": record.tool_name,
        "selector": "output",
        "status": record.status,
        "summary": record.summary,
        "output": record.output,
        "message": "Output evidence recovered from the tool execution record."
    });
    let body = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
    MemoryDocument {
        source_ref,
        source_kind: "tool_execution_output".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: record.agent_id.clone(),
        source_path: None,
        title: format!("{} tool output", record.tool_name),
        sanitized_excerpt: format!(
            "tool_execution_id={} tool_name={} selector=output summary={}",
            record.id,
            record.tool_name,
            truncate_chars(&record.summary, 240).0
        ),
        body,
        metadata: json!({
            "tool_execution_id": record.id,
            "tool_name": record.tool_name,
            "turn_index": record.turn_index,
            "work_item_id": record.work_item_id,
            "selector": "output",
            "governance_surface": "runtime_evidence",
            "provenance_class": "tool_execution_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: record.completed_at.unwrap_or(record.created_at),
    }
}

fn command_output_document(
    record: &ToolExecutionRecord,
    batch_item_index: Option<usize>,
    stream: &'static str,
) -> Option<MemoryDocument> {
    let output = command_output_envelope(record, batch_item_index, stream)?;
    let source_ref = command_output_source_ref(&record.id, batch_item_index, stream);
    let title = match batch_item_index {
        Some(index) => format!("{} {stream} output item {}", record.tool_name, index),
        None => format!("{} {stream} output", record.tool_name),
    };
    let body = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
    let preview = output
        .get("preview")
        .and_then(Value::as_str)
        .or_else(|| output.get("content").and_then(Value::as_str))
        .unwrap_or("");
    Some(MemoryDocument {
        source_ref,
        source_kind: "tool_command_output".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: record.agent_id.clone(),
        source_path: None,
        title,
        sanitized_excerpt: format!(
            "tool_execution_id={} tool_name={} selector={} batch_item_index={:?} preview={}",
            record.id,
            record.tool_name,
            stream,
            batch_item_index,
            truncate_chars(preview, 240).0
        ),
        body,
        metadata: json!({
            "tool_execution_id": record.id,
            "tool_name": record.tool_name,
            "turn_index": record.turn_index,
            "work_item_id": record.work_item_id,
            "batch_item_index": batch_item_index,
            "selector": stream,
            "governance_surface": "runtime_evidence",
            "provenance_class": "tool_execution_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: record.completed_at.unwrap_or(record.created_at),
    })
}

fn command_output_envelope(
    record: &ToolExecutionRecord,
    batch_item_index: Option<usize>,
    stream: &'static str,
) -> Option<Value> {
    let output = match (record.tool_name.as_str(), batch_item_index) {
        ("ExecCommand", None) => record
            .output
            .get("result")
            .or_else(|| {
                record
                    .output
                    .get("envelope")
                    .and_then(|value| value.get("result"))
            })
            .unwrap_or(&record.output),
        ("ExecCommandBatch", Some(index)) => record
            .output
            .get("result")
            .or_else(|| {
                record
                    .output
                    .get("envelope")
                    .and_then(|value| value.get("result"))
            })
            .unwrap_or(&record.output)
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| items.get(index - 1))
            .map(|item| item.get("result").unwrap_or(item))?,
        _ => return None,
    };
    let content = match stream {
        "stdout" => output.get("stdout_preview").and_then(Value::as_str),
        "stderr" => output.get("stderr_preview").and_then(Value::as_str),
        "output" => output
            .get("stdout_preview")
            .and_then(Value::as_str)
            .or_else(|| output.get("stderr_preview").and_then(Value::as_str))
            .or_else(|| output.get("initial_output_preview").and_then(Value::as_str)),
        _ => None,
    };
    let artifact_key = match stream {
        "stdout" => Some("stdout_artifact"),
        "stderr" => Some("stderr_artifact"),
        _ => None,
    };
    let artifact = artifact_key
        .and_then(|key| output.get(key).and_then(Value::as_u64))
        .and_then(|index| {
            output
                .get("artifacts")
                .and_then(Value::as_array)
                .and_then(|artifacts| artifacts.get(index as usize))
        })
        .cloned();
    let available = content.is_some() || artifact.is_some();
    Some(json!({
        "source_type": "tool_command_output",
        "tool_execution_id": record.id,
        "tool_name": record.tool_name,
        "batch_item_index": batch_item_index,
        "selector": stream,
        "status": record.status,
        "disposition": output.get("disposition").and_then(Value::as_str).unwrap_or("unknown"),
        "content": content,
        "preview": content.unwrap_or(""),
        "output_available": available,
        "availability": if available { "available" } else { "output_unavailable" },
        "truncated": output.get("truncated").and_then(Value::as_bool).unwrap_or(false)
            || output.get("initial_output_truncated").and_then(Value::as_bool).unwrap_or(false),
        "artifact": artifact,
        "message": if available {
            "Output evidence recovered from the tool execution record. If artifact is present, use that pointer for full bytes rather than expecting MemoryGet to stream them."
        } else {
            "No persisted output evidence is available for this tool execution stream; older records may contain command input only."
        },
    }))
}

fn command_receipt_document(
    record: &ToolExecutionRecord,
    batch_item_index: Option<usize>,
    batch_item_input: Option<&Value>,
    cmd: &str,
) -> MemoryDocument {
    let cmd_digest = command_digest(cmd);
    let source_ref = command_receipt_source_ref(&record.id, batch_item_index);
    let title = match batch_item_index {
        Some(index) => format!("{} command receipt item {}", record.tool_name, index),
        None => format!("{} command receipt", record.tool_name),
    };
    let preview = command_preview(cmd);
    let input_json = batch_item_input.unwrap_or(&record.input).to_owned();
    let input_json =
        serde_json::to_string_pretty(&input_json).unwrap_or_else(|_| input_json.to_string());
    let batch_item_input = batch_item_input
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()));
    let body = format!(
        "tool_execution_id: {}\ntool_name: {}\ncmd_digest: {}\ninput_json:\n{}\n{}cmd:\n{}",
        record.id,
        record.tool_name,
        cmd_digest,
        input_json,
        batch_item_input
            .as_deref()
            .map(|value| format!("batch_item_input_json:\n{value}\n"))
            .unwrap_or_default(),
        cmd
    );
    MemoryDocument {
        source_ref,
        source_kind: "tool_command_receipt".into(),
        scope_kind: "agent".into(),
        workspace_id: None,
        agent_id: record.agent_id.clone(),
        source_path: None,
        title,
        sanitized_excerpt: format!(
            "tool_execution_id={} tool_name={} cmd_digest={} cmd_preview={}",
            record.id, record.tool_name, cmd_digest, preview
        ),
        body,
        metadata: json!({
            "tool_execution_id": record.id,
            "tool_name": record.tool_name,
            "turn_index": record.turn_index,
            "work_item_id": record.work_item_id,
            "batch_item_index": batch_item_index,
            "cmd_digest": cmd_digest,
            "cmd_preview": preview,
            "governance_surface": "runtime_evidence",
            "provenance_class": "tool_execution_record",
            "trust_class": "runtime_evidence",
        }),
        updated_at: record.completed_at.unwrap_or(record.created_at),
    }
}

fn work_item_plan_status_label(status: crate::types::WorkItemPlanStatus) -> &'static str {
    match status {
        crate::types::WorkItemPlanStatus::Draft => "draft",
        crate::types::WorkItemPlanStatus::Ready => "ready",
        crate::types::WorkItemPlanStatus::NeedsInput => "needs_input",
    }
}

fn todo_item_state_label(state: crate::types::TodoItemState) -> &'static str {
    match state {
        crate::types::TodoItemState::Pending => "pending",
        crate::types::TodoItemState::InProgress => "in_progress",
        crate::types::TodoItemState::Completed => "completed",
    }
}

fn storage_agent_id(storage: &AppStorage) -> String {
    storage
        .current_agent_id()
        .ok()
        .flatten()
        .unwrap_or_else(|| "global".into())
}

fn file_updated_at(path: &Path) -> DateTime<Utc> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now())
}

fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn excerpt(text: &str) -> String {
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

fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    let mut chars = text.chars();
    let truncated = chars.clone().nth(max_chars).is_some();
    let content = chars.by_ref().take(max_chars).collect::<String>();
    (content, truncated)
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::{
        agent_template::ensure_agent_home_layout,
        runtime_db::RuntimeIndexChange,
        types::{
            AgentState, BriefContentSource, BriefKind, ContextEpisodeRecord, EpisodeBoundaryReason,
            TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState, TranscriptEntry,
            TranscriptEntryKind, TurnRecord, WorkItemPlanStatus, WorkItemState,
        },
    };
    use serde_json::json;

    fn brief_with_workspace(
        agent_id: &str,
        kind: BriefKind,
        text: &str,
        workspace_id: &str,
    ) -> BriefRecord {
        let mut brief = BriefRecord::new(agent_id, kind, text, None, None);
        brief.workspace_id = workspace_id.to_string();
        brief
    }

    fn work_item_with_workspace(
        agent_id: &str,
        objective: &str,
        status: WorkItemState,
        workspace_id: &str,
    ) -> WorkItemRecord {
        let mut work_item = WorkItemRecord::new(agent_id, objective, status);
        work_item.workspace_id = workspace_id.to_string();
        work_item
    }

    fn task_record(
        id: &str,
        agent_id: &str,
        kind: TaskKind,
        status: TaskStatus,
        summary: &str,
        work_item_id: Option<String>,
        detail: Option<Value>,
        created_offset_seconds: i64,
        updated_offset_seconds: i64,
    ) -> TaskRecord {
        let now = Utc::now();
        TaskRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            kind,
            status,
            created_at: now + chrono::Duration::seconds(created_offset_seconds),
            updated_at: now + chrono::Duration::seconds(updated_offset_seconds),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary.into()),
            detail,
            recovery: None,
        }
    }

    #[test]
    fn memory_search_indexes_agent_memory_and_repairs_external_edits() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        fs::write(
            agent_memory_self_path(dir.path()),
            "The agent prefers precise release checklists.",
        )
        .unwrap();

        rebuild_memory_index(&storage, None).unwrap();
        let results = search_memory(&storage, "release", 10, None, false).unwrap();
        assert_eq!(results[0].kind, "agent_memory_markdown");
        assert_eq!(
            results[0].metadata["governance_surface"].as_str(),
            Some("curated_durable_memory")
        );
        assert_eq!(
            results[0].metadata["trust_class"].as_str(),
            Some("agent_curated")
        );

        fs::write(
            agent_memory_self_path(dir.path()),
            "The agent now remembers 混合 搜索 diagnostics.",
        )
        .unwrap();
        let results = search_memory(&storage, "混合搜索", 10, None, false).unwrap();
        assert!(results.iter().any(|result| {
            result.source_ref == "agent_memory:self" && result.snippet.contains("混合")
        }));
    }

    #[test]
    fn shared_memory_index_is_scoped_by_agent_for_search_and_get() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");
        let alpha_home = agents_dir.join("alpha");
        let beta_home = agents_dir.join("beta");
        let alpha = AppStorage::new(&alpha_home).unwrap();
        let beta = AppStorage::new(&beta_home).unwrap();
        alpha.write_agent(&AgentState::new("alpha")).unwrap();
        beta.write_agent(&AgentState::new("beta")).unwrap();
        ensure_agent_home_layout(&alpha_home).unwrap();
        ensure_agent_home_layout(&beta_home).unwrap();
        fs::write(
            agent_memory_self_path(&alpha_home),
            "alpha private recall shared-index-sentinel",
        )
        .unwrap();
        fs::write(
            agent_memory_self_path(&beta_home),
            "beta private recall shared-index-sentinel",
        )
        .unwrap();

        assert_eq!(memory_index_path(&alpha), memory_index_path(&beta));

        rebuild_memory_index(&alpha, None).unwrap();
        rebuild_memory_index(&beta, None).unwrap();

        let alpha_results = search_memory(&alpha, "shared-index-sentinel", 10, None, true).unwrap();
        assert!(alpha_results
            .iter()
            .any(|result| result.source_ref == "agent_memory:self"
                && result.agent_id == "alpha"
                && result.snippet.contains("alpha private recall")));
        assert!(alpha_results
            .iter()
            .all(|result| result.agent_id == "alpha"));

        let beta_results = search_memory(&beta, "shared-index-sentinel", 10, None, true).unwrap();
        assert!(beta_results
            .iter()
            .any(|result| result.source_ref == "agent_memory:self"
                && result.agent_id == "beta"
                && result.snippet.contains("beta private recall")));
        assert!(beta_results.iter().all(|result| result.agent_id == "beta"));

        let alpha_memory = get_memory(&alpha, "agent_memory:self", None, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            alpha_memory.content,
            "alpha private recall shared-index-sentinel"
        );
        let beta_memory = get_memory(&beta, "agent_memory:self", None, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            beta_memory.content,
            "beta private recall shared-index-sentinel"
        );
    }

    #[test]
    fn shared_memory_index_dirty_and_readiness_are_scoped_by_agent() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");
        let alpha_home = agents_dir.join("alpha");
        let beta_home = agents_dir.join("beta");
        let alpha = AppStorage::new(&alpha_home).unwrap();
        let beta = AppStorage::new(&beta_home).unwrap();
        alpha.write_agent(&AgentState::new("alpha")).unwrap();
        beta.write_agent(&AgentState::new("beta")).unwrap();
        ensure_agent_home_layout(&alpha_home).unwrap();
        ensure_agent_home_layout(&beta_home).unwrap();
        fs::write(agent_memory_self_path(&alpha_home), "alpha initial slice").unwrap();
        fs::write(agent_memory_self_path(&beta_home), "beta initial slice").unwrap();

        rebuild_memory_index(&alpha, None).unwrap();
        assert!(search_memory(&beta, "initial", 10, None, true)
            .unwrap()
            .iter()
            .any(|result| result.agent_id == "beta"
                && result.source_ref == "agent_memory:self"
                && result.snippet.contains("beta initial slice")));

        beta.append_brief(&brief_with_workspace(
            "beta",
            BriefKind::Result,
            "beta fresh dirty marker remains scoped",
            "ws-beta",
        ))
        .unwrap();
        fs::write(
            agent_memory_self_path(&alpha_home),
            "alpha rebuild should not clear beta dirty marker",
        )
        .unwrap();
        alpha.mark_memory_index_dirty().unwrap();

        assert!(search_memory(&alpha, "rebuild", 10, None, true)
            .unwrap()
            .iter()
            .any(|result| result.agent_id == "alpha"
                && result.source_ref == "agent_memory:self"
                && result.snippet.contains("alpha rebuild")));
        assert!(search_memory(&beta, "fresh", 10, Some("ws-beta"), true)
            .unwrap()
            .iter()
            .any(|result| result.agent_id == "beta"
                && result.kind == "brief"
                && result.snippet.contains("fresh dirty marker")));
    }

    #[test]
    fn memory_get_returns_exact_markdown_and_runtime_source_content() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        let markdown = "The agent now remembers 混合 搜索 diagnostics.";
        fs::write(agent_memory_self_path(dir.path()), markdown).unwrap();
        let brief = brief_with_workspace(
            "default",
            BriefKind::Result,
            "runtime exact evidence body",
            "ws-holon",
        );
        let brief_ref = format!("brief:{}", brief.id);
        storage.append_brief(&brief).unwrap();
        let other_brief = brief_with_workspace(
            "default",
            BriefKind::Result,
            "other workspace exact evidence",
            "ws-other",
        );
        let other_brief_ref = format!("brief:{}", other_brief.id);
        storage.append_brief(&other_brief).unwrap();

        rebuild_memory_index(&storage, Some("ws-holon")).unwrap();

        let memory = get_memory(&storage, "agent_memory:self", None, Some("ws-holon"))
            .unwrap()
            .unwrap();
        assert_eq!(memory.content, markdown);
        assert!(!memory.content.contains("混合 搜索 diagnostics\n混合"));
        assert!(!memory.truncated);

        let memory = get_memory(&storage, &brief_ref, Some(12), Some("ws-holon"))
            .unwrap()
            .unwrap();
        assert_eq!(memory.content, "runtime exac");
        assert!(memory.truncated);

        let memory = get_memory(&storage, &brief_ref, None, Some("ws-holon"))
            .unwrap()
            .unwrap();
        assert_eq!(memory.content, "runtime exact evidence body");
        assert!(!memory.truncated);

        // source_ref is globally unique; MemoryGet resolves it regardless of
        // active workspace (fixes #1454).
        let other = get_memory(&storage, &other_brief_ref, None, Some("ws-holon"))
            .unwrap()
            .unwrap();
        assert_eq!(other.content, "other workspace exact evidence");
        assert!(get_memory(&storage, &brief_ref, None, None)
            .unwrap()
            .is_some());
    }

    #[test]
    fn memory_get_resolves_known_runtime_refs_without_search_index_hit() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        rebuild_memory_index(&storage, None).unwrap();

        let mut brief = brief_with_workspace(
            "default",
            BriefKind::Result,
            "direct result evidence after index rebuild",
            "ws-holon",
        );
        brief.work_item_id = Some("work-direct-1663".into());
        let brief_ref = format!("brief:{}", brief.id);
        storage.append_brief(&brief).unwrap();

        let mut message = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "direct message source evidence".into(),
            },
        );
        message.id = "msg-direct-1663".into();
        message.turn_id = Some("turn-direct-1663".into());
        message.message_seq = Some(7);
        storage.append_message(&message).unwrap();

        storage
            .append_task(&task_record(
                "task-direct-1663",
                "default",
                TaskKind::CommandTask,
                TaskStatus::Completed,
                "direct task source evidence",
                Some("work-direct-1663".into()),
                Some(json!({"cmd": "echo direct-task-1663"})),
                0,
                0,
            ))
            .unwrap();

        let mut work_item = work_item_with_workspace(
            "default",
            "direct work item ref",
            WorkItemState::Open,
            "ws-holon",
        );
        work_item.id = "work-direct-1663".into();
        work_item.result_brief_id = Some(brief.id.clone());
        work_item.todo_list = vec![TodoItem {
            text: "prove direct work item get".into(),
            state: TodoItemState::InProgress,
        }];
        storage.append_work_item(&work_item).unwrap();

        storage
            .append_context_episode(&ContextEpisodeRecord {
                id: "episode-direct-1663".into(),
                agent_id: "default".into(),
                workspace_id: "ws-holon".into(),
                created_at: Utc::now(),
                finalized_at: Utc::now(),
                start_turn_index: 1,
                end_turn_index: 2,
                start_message_count: 1,
                end_message_count: 2,
                boundary_reason: EpisodeBoundaryReason::HardTurnCap,
                current_work_item_id: Some("work-direct-1663".into()),
                objective: Some("runtime refs".into()),
                work_summary: Some("direct episode source".into()),
                scope_hints: vec![],
                source_turn_ids: vec!["turn-direct-1663".into()],
                source_refs: vec!["turn:turn-direct-1663".into()],
                generated_by: None,
                working_set_files: vec![],
                decisions: vec![],
                carry_forward: vec![],
                waiting_on: vec![],
            })
            .unwrap();

        let mut turn = TurnRecord::new("default", "turn-direct-1663", 7);
        turn.current_work_item_id = Some("work-direct-1663".into());
        turn.input_message_ids = vec![message.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&message));
        turn.tool_execution_ids = vec!["tool-direct-1663".into()];
        turn.produced_brief_ids = vec![brief.id.clone()];
        storage.append_turn(&turn).unwrap();

        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-direct-1663".into(),
                agent_id: "default".into(),
                work_item_id: Some("work-direct-1663".into()),
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommand".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({"cmd": "printf direct-tool-1663"}),
                output: json!({
                    "stdout_preview": "direct-tool-1663",
                    "stderr_preview": "",
                    "truncated": false
                }),
                summary: "command exited with status 0".into(),
                invocation_surface: None,
            })
            .unwrap();

        assert_eq!(
            get_memory(&storage, &brief_ref, None, None)
                .unwrap()
                .unwrap()
                .content,
            "direct result evidence after index rebuild"
        );
        assert!(get_memory(&storage, "task:task-direct-1663", None, None)
            .unwrap()
            .unwrap()
            .content
            .contains("direct task source evidence"));
        let message_memory = get_memory(&storage, "message:msg-direct-1663", None, None)
            .unwrap()
            .expect("message memory source should be gettable");
        assert_eq!(message_memory.kind, "message");
        assert!(message_memory
            .content
            .contains("message_ref: message:msg-direct-1663"));
        assert!(message_memory
            .content
            .contains("turn_ref: turn:turn-direct-1663"));
        assert!(message_memory
            .content
            .contains("direct message source evidence"));
        assert!(
            get_memory(&storage, "work_item:work-direct-1663", None, None)
                .unwrap()
                .unwrap()
                .content
                .contains("prove direct work item get")
        );
        assert!(
            get_memory(&storage, "work_item:work-direct-1663", None, None)
                .unwrap()
                .unwrap()
                .content
                .contains(&format!("Result ref: {brief_ref}"))
        );
        assert!(
            get_memory(&storage, "episode:episode-direct-1663", None, None)
                .unwrap()
                .unwrap()
                .content
                .contains("retrieval_refs: turn:turn-direct-1663")
        );
        let turn_memory = get_memory(&storage, "turn:turn-direct-1663", None, None)
            .unwrap()
            .expect("turn memory source should be gettable");
        assert_eq!(turn_memory.kind, "turn");
        assert!(turn_memory
            .content
            .contains("turn_ref: turn:turn-direct-1663"));
        assert!(turn_memory
            .content
            .contains("current_work_item_ref: work_item:work-direct-1663"));
        assert!(turn_memory
            .content
            .contains("trigger_message_ref: message:msg-direct-1663"));
        assert!(turn_memory
            .content
            .contains("input_message_refs: message:msg-direct-1663"));
        assert!(turn_memory
            .content
            .contains("tool_execution:tool-direct-1663:output"));
        assert!(turn_memory.content.contains(&format!("brief:{}", brief.id)));
        assert!(
            get_memory(&storage, "tool_execution:tool-direct-1663:cmd", None, None)
                .unwrap()
                .unwrap()
                .content
                .contains("direct-tool-1663")
        );
    }

    #[test]
    fn memory_get_message_refs_are_scoped_to_current_agent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();

        let mut other_agent_message = MessageEnvelope::new(
            "other-agent",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator { actor_id: None },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "other agent secret message".into(),
            },
        );
        other_agent_message.id = "msg-other-agent-1685".into();
        storage.append_message(&other_agent_message).unwrap();

        assert!(
            get_memory(&storage, "message:msg-other-agent-1685", None, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn ack_briefs_are_not_semantic_memory_get_sources() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let ack = brief_with_workspace(
            "default",
            BriefKind::Ack,
            "Queued work: direct ack should stay lifecycle evidence",
            "ws-holon",
        );
        let ack_ref = format!("brief:{}", ack.id);
        storage.append_brief(&ack).unwrap();

        assert!(get_memory(&storage, &ack_ref, None, Some("ws-holon"))
            .unwrap()
            .is_none());
        assert!(
            !search_memory(&storage, "direct ack", 10, Some("ws-holon"), false)
                .unwrap()
                .iter()
                .any(|result| result.source_ref == ack_ref)
        );
    }

    #[test]
    fn memory_get_returns_db_backed_runtime_evidence_content() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();
        let body = format!(
            "db backed exact evidence {}\n{}",
            "sentinel_1623",
            "x".repeat(4096)
        );
        let brief = brief_with_workspace("default", BriefKind::Result, &body, "ws-holon");
        let brief_ref = format!("brief:{}", brief.id);

        storage.append_brief(&brief).unwrap();
        assert!(!storage.ledger_dir().join("briefs.jsonl").exists());

        let memory = get_memory(&storage, &brief_ref, None, Some("ws-holon"))
            .unwrap()
            .expect("DB-backed runtime evidence should be directly retrievable");
        assert_eq!(memory.content, body);
        assert!(!memory.truncated);
    }

    #[test]
    fn memory_search_consumes_runtime_outbox_without_full_rebuild() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();
        let brief = brief_with_workspace(
            "default",
            BriefKind::Result,
            "outbox discovery sentinel 1848",
            "ws-holon",
        );
        let brief_ref = format!("brief:{}", brief.id);

        storage.append_brief(&brief).unwrap();
        assert_eq!(
            runtime_db
                .runtime_index_outbox()
                .read_after("default", 0, 10)
                .unwrap()
                .len(),
            1
        );

        let response =
            search_memory_query(&storage, "outbox discovery", 10, Some("ws-holon"), false).unwrap();
        assert!(response
            .results
            .iter()
            .any(|result| result.source_ref == brief_ref));
        assert_eq!(response.index_status.freshness, "fresh");
        assert_eq!(response.index_status.lag, 0);
        assert!(!response.index_status.consumption_was_limited);
        assert_eq!(response.index_status.skipped_error_count, 0);
    }

    #[test]
    fn memory_search_status_reports_limited_runtime_outbox_consumption() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db.clone())
            .unwrap();
        let consume_limit = 2;
        let changes = (0..=consume_limit)
            .map(|index| RuntimeIndexChange {
                agent_id: "default".into(),
                source_kind: "brief".into(),
                source_id: format!("missing-{index}"),
                source_ref: format!("brief:missing-{index}"),
                operation: RuntimeIndexOperation::Upsert,
                source_updated_at: Some(Utc::now()),
                reason: "test_backlog".into(),
            })
            .collect::<Vec<_>>();
        runtime_db
            .runtime_index_outbox()
            .append_changes(&changes)
            .unwrap();

        let mut index = MemoryIndex::open(&storage).unwrap();
        index
            .consume_runtime_outbox(&storage, consume_limit)
            .unwrap();
        let status = index.index_status(&storage, "default").unwrap();

        assert_eq!(status.cursor, consume_limit as i64);
        assert_eq!(status.high_watermark, (consume_limit + 1) as i64);
        assert_eq!(status.lag, 1);
        assert!(status.consumption_was_limited);
        assert_eq!(status.skipped_error_count, 0);
    }

    #[test]
    fn memory_get_hydrates_transcript_backed_brief_content() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();

        let entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::AssistantRound,
            Some(1),
            None,
            json!({
                "blocks": [
                    {"type": "text", "Text": {"text": "full transcript"}},
                    {"type": "text", "text": "brief body sentinel_4839"}
                ]
            }),
        );
        storage.append_transcript_entry(&entry).unwrap();

        let mut brief = brief_with_workspace("default", BriefKind::Result, "preview", "ws-holon");
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: entry.id.clone(),
            relation: crate::types::BriefContentSourceRelation::DerivedFrom,
        };
        let brief_ref = format!("brief:{}", brief.id);
        storage.append_brief(&brief).unwrap();

        let memory = get_memory(&storage, &brief_ref, None, Some("ws-holon"))
            .unwrap()
            .expect("transcript-backed brief should be retrievable");
        assert_eq!(memory.content, "full transcript brief body sentinel_4839");
    }

    #[test]
    fn deleting_known_memory_markdown_removes_index_row() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        let path = agent_memory_self_path(dir.path());
        fs::write(&path, "deletable precise memory").unwrap();
        rebuild_memory_index(&storage, None).unwrap();
        assert!(search_memory(&storage, "deletable", 10, None, false)
            .unwrap()
            .iter()
            .any(|result| result.source_ref == "agent_memory:self"));

        fs::remove_file(&path).unwrap();
        repair_memory_index_for_paths(&storage, &[path.display().to_string()]).unwrap();

        assert!(!search_memory(&storage, "deletable", 10, None, false)
            .unwrap()
            .iter()
            .any(|result| result.source_ref == "agent_memory:self"));
        assert!(get_memory(&storage, "agent_memory:self", None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn memory_search_indexes_workspace_profile_briefs_episodes_and_work_items() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        storage
            .append_workspace_entry(&WorkspaceEntry::new(
                "ws-holon",
                PathBuf::from("/repo/holon"),
                Some("holon runtime".into()),
            ))
            .unwrap();
        storage
            .append_workspace_entry(&WorkspaceEntry::new(
                "ws-other",
                PathBuf::from("/repo/other"),
                Some("other runtime".into()),
            ))
            .unwrap();
        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "cache diagnostics completed for holon",
                "ws-holon",
            ))
            .unwrap();
        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "other workspace cache diagnostics",
                "ws-other",
            ))
            .unwrap();
        let mut message = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "searchable operator message sentinel1879".into(),
            },
        );
        message.id = "msg-memory-search".into();
        storage.append_message(&message).unwrap();
        let mut work_item = WorkItemRecord::new(
            "default",
            "MemorySearch index implementation",
            WorkItemState::Completed,
        );
        work_item.workspace_id = "ws-holon".into();
        work_item.plan_status = WorkItemPlanStatus::Ready;
        work_item.plan_artifact = Some(crate::types::WorkItemPlanArtifact {
            owner_agent_id: "default".into(),
            workspace_id: crate::types::agent_home_workspace_id("default"),
            workspace_alias: Some(crate::types::AGENT_HOME_WORKSPACE_ID.into()),
            relative_path: PathBuf::from("work-items/work-memory/plan.md"),
            path: dir.path().join("work-items/work-memory/plan.md"),
            hash: "sha256:test".into(),
            bytes: 54,
            updated_at: Utc::now(),
            preview: "Persist checksum-oriented task understanding in recall.".into(),
            preview_complete: true,
        });
        work_item.todo_list = vec![
            TodoItem {
                text: "Index durable objective plan text".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "Verify checklist retrieval marker".into(),
                state: TodoItemState::InProgress,
            },
        ];
        storage.append_work_item(&work_item).unwrap();
        storage
            .append_context_episode(&ContextEpisodeRecord {
                id: "episode-1".into(),
                agent_id: "default".into(),
                workspace_id: "ws-holon".into(),
                created_at: Utc::now(),
                finalized_at: Utc::now(),
                start_turn_index: 1,
                end_turn_index: 2,
                start_message_count: 1,
                end_message_count: 3,
                boundary_reason: EpisodeBoundaryReason::HardTurnCap,
                current_work_item_id: Some(work_item.id.clone()),
                objective: Some("memory search".into()),
                work_summary: Some("index worker".into()),
                scope_hints: vec![],
                source_turn_ids: vec!["turn-memory-index".into()],
                source_refs: vec!["turn:turn-memory-index".into()],
                generated_by: None,
                working_set_files: vec!["src/memory/index.rs".into()],
                decisions: vec!["Use SQLite FTS5".into()],
                carry_forward: vec![],
                waiting_on: vec![],
            })
            .unwrap();

        rebuild_memory_index(&storage, Some("ws-holon")).unwrap();
        let results = search_memory(&storage, "holon", 10, Some("ws-holon"), false).unwrap();
        assert!(results
            .iter()
            .any(|result| result.kind == "workspace_profile"));
        let brief_result = results
            .iter()
            .find(|result| result.kind == "brief")
            .expect("brief memory result");
        assert_eq!(
            brief_result.metadata["governance_surface"].as_str(),
            Some("runtime_evidence")
        );
        assert_eq!(
            brief_result.metadata["provenance_class"].as_str(),
            Some("brief_record")
        );
        let results =
            search_memory(&storage, "turn-memory-index", 10, Some("ws-holon"), false).unwrap();
        assert!(results
            .iter()
            .any(|result| result.kind == "context_episode"));
        let results = search_memory(&storage, "SQLite", 10, Some("ws-holon"), false).unwrap();
        assert!(!results
            .iter()
            .any(|result| result.kind == "context_episode"));
        let results = search_memory(&storage, "MemorySearch", 10, Some("ws-holon"), false).unwrap();
        assert!(results.iter().any(|result| result.kind == "work_item"));
        let results = search_memory(&storage, "sentinel1879", 10, Some("ws-holon"), false).unwrap();
        let message_result = results
            .iter()
            .find(|result| result.source_ref == "message:msg-memory-search")
            .expect("message memory result");
        assert_eq!(
            message_result.snippet,
            "searchable operator message sentinel1879"
        );
        assert!(!message_result.snippet.contains("message_ref:"));
        let results = search_memory(&storage, "checksum", 10, Some("ws-holon"), false).unwrap();
        assert!(results.iter().any(|result| result.kind == "work_item"));
        let results = search_memory(&storage, "checklist", 10, Some("ws-holon"), false).unwrap();
        assert!(results.iter().any(|result| result.kind == "work_item"));
        let work_item_doc = get_memory(
            &storage,
            &format!("work_item:{}", work_item.id),
            None,
            Some("ws-holon"),
        )
        .unwrap()
        .expect("work item memory document");
        assert!(work_item_doc.content.contains("Plan status: ready"));
        assert!(work_item_doc
            .content
            .contains("Verify checklist retrieval marker"));
        assert!(results.iter().all(|result| result.scope_kind == "agent"
            || result.workspace_id.as_deref() == Some("ws-holon")));
        let other_results = search_memory(&storage, "other", 10, Some("ws-holon"), false).unwrap();
        assert!(other_results.iter().all(|result| {
            result.scope_kind == "agent" || result.workspace_id.as_deref() == Some("ws-holon")
        }));
        let unscoped_results = search_memory(&storage, "MemorySearch", 10, None, false).unwrap();
        assert!(unscoped_results
            .iter()
            .all(|result| result.scope_kind == "agent"));

        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "fresh dirty marker recall",
                "ws-holon",
            ))
            .unwrap();
        let results = search_memory(&storage, "fresh", 10, Some("ws-holon"), false).unwrap();
        assert!(results
            .iter()
            .any(|result| result.kind == "brief" && result.snippet.contains("fresh")));
    }

    #[test]
    fn missing_index_does_not_rebuild_all_existing_memory_sources_during_search() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        storage
            .append_workspace_entry(&WorkspaceEntry::new(
                "ws-existing",
                PathBuf::from("/repo/existing"),
                Some("existing workspace".into()),
            ))
            .unwrap();
        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "existing ledger brief memory",
                "ws-existing",
            ))
            .unwrap();
        let work_item = work_item_with_workspace(
            "default",
            "existing work item",
            WorkItemState::Completed,
            "ws-existing",
        );
        storage.append_work_item(&work_item).unwrap();
        storage
            .append_context_episode(&ContextEpisodeRecord {
                id: "episode-existing".into(),
                agent_id: "default".into(),
                workspace_id: "ws-existing".into(),
                created_at: Utc::now(),
                finalized_at: Utc::now(),
                start_turn_index: 1,
                end_turn_index: 2,
                start_message_count: 1,
                end_message_count: 2,
                boundary_reason: EpisodeBoundaryReason::HardTurnCap,
                current_work_item_id: Some(work_item.id),
                objective: Some("existing episode".into()),
                work_summary: Some("existing episode summary".into()),
                scope_hints: vec![],
                source_turn_ids: vec![],
                source_refs: vec![],
                generated_by: None,
                working_set_files: vec![],
                decisions: vec![],
                carry_forward: vec![],
                waiting_on: vec![],
            })
            .unwrap();

        let _ = fs::remove_file(memory_index_path(&storage));
        let _ = fs::remove_file(
            storage
                .shared_indexes_dir()
                .join(dirty_filename_for_agent("default")),
        );

        let results = search_memory(&storage, "existing", 10, Some("ws-existing"), false).unwrap();
        assert!(
            results.is_empty(),
            "MemorySearch must not synchronously rebuild all runtime sources"
        );
    }

    #[test]
    fn controlled_changed_paths_repair_known_memory_markdown_only() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        fs::write(
            agent_memory_operator_path(dir.path()),
            "operator prefers short status",
        )
        .unwrap();
        rebuild_memory_index(&storage, None).unwrap();

        fs::write(
            agent_memory_operator_path(dir.path()),
            "operator prefers direct Chinese status",
        )
        .unwrap();
        repair_memory_index_for_paths(
            &storage,
            &[agent_memory_operator_path(dir.path()).display().to_string()],
        )
        .unwrap();
        let results = search_memory(&storage, "Chinese", 10, None, false).unwrap();
        assert!(results
            .iter()
            .any(|result| result.source_ref == "agent_memory:operator"));
    }

    #[test]
    fn ordinary_workspace_markdown_is_not_indexed_as_memory() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let readme_marker = "governance_sentinel_924_workspace_markdown";
        fs::write(
            workspace.join("README.md"),
            format!("ordinary workspace markdown {readme_marker} should not become Holon memory"),
        )
        .unwrap();
        storage
            .append_workspace_entry(&WorkspaceEntry::new(
                "ws-markdown",
                workspace,
                Some("markdown workspace".into()),
            ))
            .unwrap();

        rebuild_memory_index(&storage, Some("ws-markdown")).unwrap();
        let results =
            search_memory(&storage, readme_marker, 10, Some("ws-markdown"), false).unwrap();

        assert!(
            results.is_empty(),
            "workspace README marker must not be searchable as Holon memory: {results:?}"
        );
    }

    #[test]
    fn memory_search_indexes_task_records_with_summary_and_work_item_lookup() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();

        storage
            .append_task(&task_record(
                "task-lookup-1246",
                "default",
                TaskKind::CommandTask,
                TaskStatus::Completed,
                "lookup task command summary",
                Some("wi-lookup-1246".into()),
                Some(json!({"cmd": "echo task lookup evidence"})),
                0,
                0,
            ))
            .unwrap();
        storage
            .append_task(&task_record(
                "task-work-item-1246",
                "default",
                TaskKind::ChildAgentTask,
                TaskStatus::Completed,
                "work item specific task summary",
                None,
                None,
                1,
                1,
            ))
            .unwrap();

        rebuild_memory_index(&storage, None).unwrap();

        let task_id_results = search_memory(&storage, "task-lookup-1246", 10, None, false).unwrap();
        assert!(task_id_results
            .iter()
            .any(|result| result.source_ref == "task:task-lookup-1246"));

        let summary_results =
            search_memory(&storage, "lookup task command summary", 10, None, false).unwrap();
        assert!(summary_results
            .iter()
            .any(|result| result.source_ref == "task:task-lookup-1246"));
        let work_item_lookup_results =
            search_memory(&storage, "work item specific task summary", 10, None, false).unwrap();
        assert!(work_item_lookup_results
            .iter()
            .any(|result| result.source_ref == "task:task-work-item-1246"));
        let work_item_results = search_memory(&storage, "wi-lookup-1246", 10, None, false).unwrap();
        assert!(work_item_results
            .iter()
            .any(|result| result.metadata["work_item_id"].as_str() == Some("wi-lookup-1246")));
        assert!(work_item_results
            .iter()
            .any(|result| result.source_ref == "task:task-lookup-1246"));
    }

    #[test]
    fn memory_search_indexes_task_records_by_command_fragment_and_digest() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();

        let command = "rg -n \"memory\" src/memory/index.rs && echo task_digest_1246";
        let digest = command_digest(command);

        storage
            .append_task(&task_record(
                "task-command-1246",
                "default",
                TaskKind::CommandTask,
                TaskStatus::Completed,
                "command task command digest check",
                None,
                Some(json!({"cmd": command})),
                0,
                0,
            ))
            .unwrap();

        rebuild_memory_index(&storage, None).unwrap();

        let fragment_results =
            search_memory(&storage, "task_digest_1246", 10, None, false).unwrap();
        assert!(fragment_results
            .iter()
            .any(|result| result.source_ref == "task:task-command-1246"));
        let digest_results = search_memory(&storage, &digest, 10, None, false).unwrap();
        assert!(digest_results
            .iter()
            .any(|result| result.source_ref == "task:task-command-1246"));
        assert!(digest_results
            .iter()
            .any(|result| result.metadata["cmd_digest"].as_str() == Some(digest.as_str())));
        assert!(digest_results.iter().all(|result| result.kind == "task"));
    }

    #[test]
    fn memory_search_task_index_uses_latest_snapshot_only() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();

        storage
            .append_task(&task_record(
                "task-repeat-1246",
                "default",
                TaskKind::CommandTask,
                TaskStatus::Running,
                "repeat task running",
                None,
                Some(json!({"cmd": "echo task repeat"})),
                0,
                0,
            ))
            .unwrap();
        storage
            .append_task(&task_record(
                "task-repeat-1246",
                "default",
                TaskKind::CommandTask,
                TaskStatus::Completed,
                "repeat task completed",
                None,
                Some(json!({"cmd": "echo task repeat"})),
                1,
                1,
            ))
            .unwrap();

        rebuild_memory_index(&storage, None).unwrap();
        let results = search_memory(&storage, "repeat task", 10, None, false).unwrap();
        let task_results: Vec<_> = results
            .iter()
            .filter(|result| result.source_ref == "task:task-repeat-1246")
            .collect();
        assert_eq!(task_results.len(), 1);
        assert_eq!(
            task_results[0].metadata["status"].as_str(),
            Some("completed")
        );
        assert_eq!(
            task_results[0].metadata["summary"].as_str(),
            Some("repeat task completed")
        );
    }

    #[test]
    fn memory_search_task_index_full_backfill_indexes_older_task_history() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();

        storage
            .append_task(&task_record(
                "task-outside-bound-1270",
                "default",
                TaskKind::CommandTask,
                TaskStatus::Completed,
                "outside bounded history sentinel 1270",
                None,
                Some(json!({"cmd": "echo outside_bound_1270"})),
                0,
                0,
            ))
            .unwrap();
        for index in 0..8 {
            storage
                .append_task(&task_record(
                    &format!("task-recent-bound-{index}"),
                    "default",
                    TaskKind::CommandTask,
                    TaskStatus::Completed,
                    "recent bounded history task",
                    None,
                    Some(json!({"cmd": format!("echo recent_bound_{index}")})),
                    index as i64 + 1,
                    index as i64 + 1,
                ))
                .unwrap();
        }

        rebuild_memory_index(&storage, None).unwrap();

        let old_results = search_memory(&storage, "outside_bound_1270", 10, None, false).unwrap();
        assert!(old_results
            .iter()
            .any(|result| result.source_ref == "task:task-outside-bound-1270"));
        let recent_results = search_memory(&storage, "recent_bound_7", 10, None, false).unwrap();
        assert!(recent_results
            .iter()
            .any(|result| result.source_ref == "task:task-recent-bound-7"));
    }

    #[test]
    fn pending_source_queue_incrementally_upserts_new_brief_after_backfill() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();

        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "initial incremental queue seed",
                "ws-holon",
            ))
            .unwrap();
        rebuild_memory_index(&storage, Some("ws-holon")).unwrap();

        let fresh = brief_with_workspace(
            "default",
            BriefKind::Result,
            "fresh pending source queue sentinel",
            "ws-holon",
        );
        let fresh_ref = format!("brief:{}", fresh.id);
        storage.append_brief(&fresh).unwrap();

        let index = MemoryIndex::open(&storage).unwrap();
        assert!(index
            .pending_sources_for_agent("default")
            .unwrap()
            .iter()
            .any(|source| source.source_ref == fresh_ref));

        let results = search_memory(
            &storage,
            "pending source queue",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        assert!(results.iter().any(|result| result.source_ref == fresh_ref));
        assert!(MemoryIndex::open(&storage)
            .unwrap()
            .pending_sources_for_agent("default")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pending_delete_removes_stale_document_and_source_state() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        fs::write(
            agent_memory_self_path(dir.path()),
            "temporary delete stale index sentinel",
        )
        .unwrap();
        rebuild_memory_index(&storage, None).unwrap();
        assert!(get_memory(&storage, "agent_memory:self", None, None)
            .unwrap()
            .is_some());
        fs::remove_file(agent_memory_self_path(dir.path())).unwrap();

        let index = MemoryIndex::open(&storage).unwrap();
        index
            .enqueue_source(
                "default",
                "agent_memory_markdown",
                "self",
                "agent_memory:self",
                "delete",
                None,
                "test_delete",
            )
            .unwrap();

        let _ = search_memory(&storage, "temporary delete stale", 10, None, false).unwrap();
        assert!(get_memory(&storage, "agent_memory:self", None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn missing_checkpoint_does_not_force_full_backfill_recovery() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "checkpoint recovery sentinel",
                "ws-holon",
            ))
            .unwrap();
        rebuild_memory_index(&storage, Some("ws-holon")).unwrap();

        let index = MemoryIndex::open(&storage).unwrap();
        index
            .connection
            .execute(
                "DELETE FROM memory_documents WHERE agent_id = ?1",
                ["default"],
            )
            .unwrap();
        index
            .connection
            .execute(
                "DELETE FROM memory_index_checkpoints WHERE agent_id = ?1",
                ["default"],
            )
            .unwrap();
        drop(index);

        let results =
            search_memory(&storage, "checkpoint recovery", 10, Some("ws-holon"), false).unwrap();
        assert!(
            results.is_empty(),
            "MemorySearch must not use missing checkpoints to trigger full rebuild"
        );
        assert!(!MemoryIndex::open(&storage)
            .unwrap()
            .has_backfill_checkpoints_for_agent("default")
            .unwrap());
    }

    #[test]
    fn stale_source_state_schema_does_not_force_search_rebuild() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "schema version rebuild sentinel",
                "ws-holon",
            ))
            .unwrap();
        rebuild_memory_index(&storage, Some("ws-holon")).unwrap();

        let index = MemoryIndex::open(&storage).unwrap();
        index
            .connection
            .execute(
                "UPDATE memory_index_source_state SET index_schema_version = 0 WHERE agent_id = ?1",
                ["default"],
            )
            .unwrap();
        drop(index);

        let results = search_memory(
            &storage,
            "schema version rebuild",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        assert!(results.iter().any(|result| result.kind == "brief"));
        assert!(!MemoryIndex::open(&storage)
            .unwrap()
            .has_current_source_state_for_agent("default")
            .unwrap());
    }

    #[test]
    fn extra_checkpoint_rows_do_not_hide_missing_required_backfill_kind() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "checkpoint required kind sentinel",
                "ws-holon",
            ))
            .unwrap();
        rebuild_memory_index(&storage, Some("ws-holon")).unwrap();

        let index = MemoryIndex::open(&storage).unwrap();
        index
            .connection
            .execute(
                "DELETE FROM memory_index_checkpoints
                 WHERE agent_id = ?1 AND source_kind = ?2",
                params!["default", all_backfill_source_kinds()[0]],
            )
            .unwrap();
        index
            .connection
            .execute(
                "INSERT INTO memory_index_checkpoints (agent_id, source_kind, cursor, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    "default",
                    "future_unknown_kind",
                    MEMORY_INDEX_BACKFILL_CURSOR,
                    Utc::now().to_rfc3339(),
                ],
            )
            .unwrap();

        assert!(!index.has_backfill_checkpoints_for_agent("default").unwrap());
    }

    #[test]
    fn memory_get_returns_latest_task_record_for_task_source_ref() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();

        storage
            .append_task(&task_record(
                "task-get-1246",
                "default",
                TaskKind::CommandTask,
                TaskStatus::Completed,
                "get compact task receipt",
                Some("wi-get-1246".into()),
                Some(json!({"cmd": "echo task_get"})),
                0,
                0,
            ))
            .unwrap();

        rebuild_memory_index(&storage, None).unwrap();
        let results = search_memory(&storage, "task-get-1246", 10, None, false).unwrap();
        let source_ref = results
            .iter()
            .find(|result| result.source_ref == "task:task-get-1246")
            .expect("task source should be searchable")
            .source_ref
            .clone();
        let memory = get_memory(&storage, &source_ref, None, None)
            .unwrap()
            .expect("task memory source should be gettable");
        assert_eq!(memory.kind, "task");
        assert!(memory.content.contains("task_id: task-get-1246"));
        assert!(memory.content.contains("status: completed"));
        assert!(memory.content.contains("get compact task receipt"));
        assert!(memory.content.contains("work_item_id: wi-get-1246"));
        assert!(memory.content.contains("cmd: echo task_get"));
        assert!(!memory.truncated);
    }

    #[test]
    fn command_receipts_preserve_long_exec_command_input_for_memory_get() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let command = "python - <<'PY'\nprint('receipt_start')\nprint('sentinel_middle_line_1246')\nprint('receipt_end')\nPY";
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-exec-1246".into(),
                agent_id: "default".into(),
                work_item_id: Some("work-1246".into()),
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommand".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({"cmd": command}),
                output: json!({"exit_code": 0}),
                summary: "command exited with status 0".into(),
                invocation_surface: None,
            })
            .unwrap();

        let results = search_memory(
            &storage,
            "sentinel_middle_line_1246",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        let result = results
            .iter()
            .find(|result| result.kind == "tool_command_receipt")
            .expect("command receipt should be indexed");
        assert_eq!(result.source_ref, "tool_execution:tool-exec-1246:cmd");
        assert_eq!(
            result.metadata["tool_execution_id"].as_str(),
            Some("tool-exec-1246")
        );
        assert_eq!(
            result.metadata["cmd_preview"].as_str(),
            Some("[omitted: command contains heredoc or inline script]")
        );

        let memory = get_memory(&storage, &result.source_ref, None, Some("ws-holon"))
            .unwrap()
            .expect("command receipt should be retrievable");
        assert!(memory.content.contains(command));
        assert!(memory.content.contains("sentinel_middle_line_1246"));
        assert!(!memory.truncated);
    }

    #[test]
    fn command_receipts_preserve_exec_command_batch_item_inputs() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let first_command = "rg -n \"MemorySearch\" src/memory/index.rs";
        let second_command = "node - <<'NODE'\nconsole.log('batch_receipt_middle_1246')\nNODE";
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-batch-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommandBatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 20,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({
                    "items": [
                        {"cmd": first_command},
                        {"cmd": second_command}
                    ]
                }),
                output: json!({"completed_count": 2}),
                summary: "ExecCommandBatch completed 2/2 items".into(),
                invocation_surface: None,
            })
            .unwrap();

        let results = search_memory(
            &storage,
            "batch_receipt_middle_1246",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        let result = results
            .iter()
            .find(|result| result.source_ref == "tool_execution:tool-batch-1246:batch_item:2:cmd")
            .expect("second batch item receipt should be indexed");
        assert_eq!(result.metadata["batch_item_index"].as_u64(), Some(2));
        assert_eq!(
            result.metadata["cmd_preview"].as_str(),
            Some("[omitted: command contains heredoc or inline script]")
        );
        let memory = get_memory(&storage, &result.source_ref, None, Some("ws-holon"))
            .unwrap()
            .expect("batch command receipt should be retrievable");
        assert!(memory.content.contains(second_command));
        assert!(!memory.content.contains(first_command));
    }

    #[test]
    fn pending_exec_command_batch_item_upsert_resolves_tool_execution_record() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        storage
            .append_brief(&brief_with_workspace(
                "default",
                BriefKind::Result,
                "seed before pending batch command receipt",
                "ws-holon",
            ))
            .unwrap();
        rebuild_memory_index(&storage, Some("ws-holon")).unwrap();

        let first_command = "echo first_pending_batch_1246";
        let second_command = "python - <<'PY'\nprint('pending_batch_receipt_middle_1246')\nPY";
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-pending-batch-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommandBatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 20,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({
                    "items": [
                        {"cmd": first_command},
                        {"cmd": second_command}
                    ]
                }),
                output: json!({"completed_count": 2}),
                summary: "ExecCommandBatch completed 2/2 items".into(),
                invocation_surface: None,
            })
            .unwrap();

        let source_ref = "tool_execution:tool-pending-batch-1246:batch_item:2:cmd";
        assert!(MemoryIndex::open(&storage)
            .unwrap()
            .pending_sources_for_agent("default")
            .unwrap()
            .iter()
            .any(|source| source.source_ref == source_ref));

        let results = search_memory(
            &storage,
            "pending_batch_receipt_middle_1246",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        let result = results
            .iter()
            .find(|result| result.source_ref == source_ref)
            .expect("pending batch item receipt should be incrementally indexed");
        assert_eq!(result.metadata["batch_item_index"].as_u64(), Some(2));
        let memory = get_memory(&storage, source_ref, None, Some("ws-holon"))
            .unwrap()
            .expect("pending batch command receipt should be retrievable");
        assert!(memory.content.contains(second_command));
        assert!(!memory.content.contains(first_command));
        assert!(MemoryIndex::open(&storage)
            .unwrap()
            .pending_sources_for_agent("default")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn command_output_resolves_exec_command_batch_envelope_items() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-batch-output-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommandBatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 20,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({
                    "items": [
                        {"cmd": "echo first"},
                        {"cmd": "echo second"}
                    ]
                }),
                output: json!({
                    "envelope": {
                        "result": {
                            "items": [
                                {"result": {"stdout_preview": "first_batch_output_1246\n", "stderr_preview": "", "truncated": false, "artifacts": []}},
                                {"result": {"stdout_preview": "second_batch_output_1246\n", "stderr_preview": "", "truncated": false, "artifacts": []}}
                            ]
                        }
                    }
                }),
                summary: "ExecCommandBatch completed 2/2 items".into(),
                invocation_surface: None,
            })
            .unwrap();

        let memory = get_memory(
            &storage,
            "tool_execution:tool-batch-output-1246:batch_item:2:stdout",
            None,
            Some("ws-holon"),
        )
        .unwrap()
        .expect("batch command output should be retrievable");

        assert!(memory.content.contains("second_batch_output_1246"));
        assert!(!memory.content.contains("first_batch_output_1246"));
    }

    #[test]
    fn command_output_is_retrievable_but_not_search_indexed() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-output-search-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommand".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({"cmd": "printf searchable_command_input_1246"}),
                output: json!({
                    "result": {
                        "stdout_preview": "command_output_only_sentinel_1246\n",
                        "stderr_preview": "",
                        "truncated": false,
                        "artifacts": []
                    }
                }),
                summary: "command exited with status 0".into(),
                invocation_surface: None,
            })
            .unwrap();

        let memory = get_memory(
            &storage,
            "tool_execution:tool-output-search-1246:stdout",
            None,
            Some("ws-holon"),
        )
        .unwrap()
        .expect("command output should remain retrievable by explicit source ref");
        assert!(memory.content.contains("command_output_only_sentinel_1246"));

        let output_results = search_memory(
            &storage,
            "command_output_only_sentinel_1246",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        assert!(output_results.is_empty());

        let receipt_results = search_memory(
            &storage,
            "searchable_command_input_1246",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        assert!(receipt_results
            .iter()
            .any(|result| result.kind == "tool_command_receipt"));
    }

    #[test]
    fn command_output_indexes_unavailable_batch_item_without_result() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-batch-unavailable-1246".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommandBatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 20,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({"items": [{"cmd": "echo skipped"}]}),
                output: json!({
                    "result": {
                        "items": [
                            {"disposition": "skipped"}
                        ]
                    }
                }),
                summary: "ExecCommandBatch completed 0/1 items".into(),
                invocation_surface: None,
            })
            .unwrap();

        let memory = get_memory(
            &storage,
            "tool_execution:tool-batch-unavailable-1246:batch_item:1:stdout",
            None,
            Some("ws-holon"),
        )
        .unwrap()
        .expect("unavailable batch command output should still be retrievable");

        assert!(memory
            .content
            .contains("\"availability\": \"output_unavailable\""));
    }

    #[test]
    fn generic_tool_output_source_ref_is_indexed_and_retrievable() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent(dir.path(), "default").unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-generic-output-1246".into(),
                agent_id: "default".into(),
                work_item_id: Some("work-generic-output-1246".into()),
                turn_index: 7,
                turn_id: Some("turn-generic-output-1246".into()),
                tool_name: "ViewImage".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 42,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({
                    "path": "fixtures/pixel.png",
                    "prompt": "inspect generic_tool_output_1246"
                }),
                output: json!({
                    "envelope": {
                        "result": {
                            "visual_observation": "generic_tool_output_1246 visual observation",
                            "metadata": {
                                "width": 1,
                                "height": 1
                            }
                        }
                    },
                    "is_error": false
                }),
                summary: "validated image metadata".into(),
                invocation_surface: None,
            })
            .unwrap();

        let source_ref = "tool_execution:tool-generic-output-1246:output";
        let results = search_memory(
            &storage,
            "generic_tool_output_1246",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        let result = results
            .iter()
            .find(|result| result.source_ref == source_ref)
            .expect("generic tool output should be indexed");
        assert_eq!(result.kind, "tool_execution_output");
        assert_eq!(result.metadata["tool_name"].as_str(), Some("ViewImage"));

        let memory = get_memory(&storage, source_ref, None, Some("ws-holon"))
            .unwrap()
            .expect("generic tool output should be retrievable");
        assert!(memory.content.contains("generic_tool_output_1246"));
        assert!(memory.content.contains("\"tool_name\": \"ViewImage\""));
        assert!(memory
            .content
            .contains("\"source_type\": \"tool_execution_output\""));
    }
}
