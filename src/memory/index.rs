use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    agent_template::{agent_memory_operator_path, agent_memory_self_path},
    storage::AppStorage,
    types::{BriefRecord, ContextEpisodeRecord, WorkItemRecord, WorkspaceEntry},
};

const INDEX_FILENAME: &str = "memory.sqlite3";
const DIRTY_FILENAME: &str = "memory.dirty";
const SEARCH_LIMIT_MAX: usize = 50;
const GET_CHARS_DEFAULT: usize = 12_000;
const GET_CHARS_MAX: usize = 50_000;
const REBUILD_BRIEF_LIMIT: usize = 500;
const REBUILD_EPISODE_LIMIT: usize = 500;
const REBUILD_WORK_ITEM_LIMIT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    storage.indexes_dir().join(INDEX_FILENAME)
}

pub fn rebuild_memory_index(storage: &AppStorage, active_workspace_id: Option<&str>) -> Result<()> {
    let mut index = MemoryIndex::open(storage)?;
    index.rebuild(storage, active_workspace_id)
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
    let index = ensure_memory_index_current(storage, active_workspace_id)?;
    index.search(query, limit, active_workspace_id, include_all_workspaces)
}

pub fn get_memory(
    storage: &AppStorage,
    source_ref: &str,
    max_chars: Option<usize>,
    active_workspace_id: Option<&str>,
) -> Result<Option<MemoryGetResult>> {
    let index = ensure_memory_index_current(storage, active_workspace_id)?;
    index.get(source_ref, max_chars, active_workspace_id)
}

fn ensure_memory_index_current(
    storage: &AppStorage,
    active_workspace_id: Option<&str>,
) -> Result<MemoryIndex> {
    let index_file_exists = memory_index_path(storage).exists();
    let mut index = MemoryIndex::open(storage)?;
    if !index_file_exists || memory_index_is_dirty(storage) || !index.has_any_documents()? {
        index.rebuild(storage, active_workspace_id)?;
    } else {
        repair_known_markdown_sources(storage, &index)?;
    }
    Ok(index)
}

fn repair_known_markdown_sources(storage: &AppStorage, index: &MemoryIndex) -> Result<()> {
    for source in known_memory_markdown_sources(storage) {
        if source.path.exists() {
            let Some(document) =
                agent_memory_document(storage, source.name, source.title, &source.path)?
            else {
                continue;
            };
            if index.document_hash(&document.source_ref)? != Some(content_hash(&document.body)) {
                index.upsert_document(&document)?;
            }
        } else {
            index.delete_document(source.source_ref)?;
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
    storage.indexes_dir().join(DIRTY_FILENAME).exists()
}

fn clear_memory_index_dirty(storage: &AppStorage) -> Result<()> {
    let path = storage.indexes_dir().join(DIRTY_FILENAME);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

struct MemoryIndex {
    connection: Connection,
}

impl MemoryIndex {
    fn open(storage: &AppStorage) -> Result<Self> {
        fs::create_dir_all(storage.indexes_dir())
            .with_context(|| format!("failed to create {}", storage.indexes_dir().display()))?;
        let connection = Connection::open(memory_index_path(storage))?;
        let index = Self { connection };
        index.ensure_schema()?;
        Ok(index)
    }

    fn ensure_schema(&self) -> Result<()> {
        if self.table_exists("memory_documents")?
            && !self.table_has_column("memory_documents", "original_body")?
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
                source_ref TEXT PRIMARY KEY,
                source_kind TEXT NOT NULL,
                scope_kind TEXT NOT NULL,
                workspace_id TEXT,
                agent_id TEXT NOT NULL,
                source_path TEXT,
                title TEXT NOT NULL,
                original_body TEXT NOT NULL,
                body TEXT NOT NULL,
                sanitized_excerpt TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memory_documents_fts
            USING fts5(source_ref UNINDEXED, title, body, sanitized_excerpt, tokenize='unicode61');
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
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM memory_documents", [])?;
        transaction.execute("DELETE FROM memory_documents_fts", [])?;
        for document in collect_documents(storage, active_workspace_id)? {
            upsert_document_tx(&transaction, &document)?;
        }
        transaction.commit()?;
        clear_memory_index_dirty(storage)?;
        Ok(())
    }

    fn upsert_document(&self, document: &MemoryDocument) -> Result<()> {
        upsert_document_tx(&self.connection, document)
    }

    fn delete_document(&self, source_ref: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM memory_documents_fts WHERE source_ref = ?1",
            [source_ref],
        )?;
        self.connection.execute(
            "DELETE FROM memory_documents WHERE source_ref = ?1",
            [source_ref],
        )?;
        Ok(())
    }

    fn has_any_documents(&self) -> Result<bool> {
        let count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM memory_documents", [], |row| {
                    row.get(0)
                })?;
        Ok(count > 0)
    }

    fn document_hash(&self, source_ref: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT content_hash FROM memory_documents WHERE source_ref = ?1",
                [source_ref],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn search(
        &self,
        query: &str,
        limit: usize,
        active_workspace_id: Option<&str>,
        include_all_workspaces: bool,
    ) -> Result<Vec<MemorySearchResult>> {
        let query = search_query(query);
        let limit = limit.clamp(1, SEARCH_LIMIT_MAX);
        let workspace_filter = if include_all_workspaces {
            None
        } else {
            active_workspace_id.map(ToString::to_string)
        };
        let include_all_workspaces = include_all_workspaces as i64;
        let mut statement = self.connection.prepare(
            r#"
            SELECT d.source_ref, d.source_kind, d.scope_kind, d.workspace_id, d.agent_id,
                   d.source_path, d.title, d.sanitized_excerpt, d.metadata_json,
                   d.updated_at, bm25(memory_documents_fts) AS score
            FROM memory_documents_fts
            JOIN memory_documents d ON d.source_ref = memory_documents_fts.source_ref
            WHERE memory_documents_fts MATCH ?1
              AND (?3 OR d.scope_kind = 'agent' OR (?2 IS NOT NULL AND d.workspace_id = ?2))
            ORDER BY score ASC, d.updated_at DESC
            LIMIT ?4
            "#,
        )?;
        let rows = statement.query_map(
            params![
                query,
                workspace_filter,
                include_all_workspaces,
                limit as i64
            ],
            |row| {
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
            },
        )?;
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
        active_workspace_id: Option<&str>,
    ) -> Result<Option<MemoryGetResult>> {
        let max_chars = max_chars
            .unwrap_or(GET_CHARS_DEFAULT)
            .clamp(1, GET_CHARS_MAX);
        let workspace_filter = active_workspace_id.map(ToString::to_string);
        self.connection
            .query_row(
                r#"
                SELECT source_ref, source_kind, scope_kind, workspace_id, agent_id, source_path,
                       title, original_body, metadata_json, updated_at
                FROM memory_documents
                WHERE source_ref = ?1
                  AND (scope_kind = 'agent' OR (?2 IS NOT NULL AND workspace_id = ?2))
                "#,
                params![source_ref, workspace_filter],
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

fn upsert_document_tx(connection: &Connection, document: &MemoryDocument) -> Result<()> {
    let metadata_json = serde_json::to_string(&document.metadata)?;
    let hash = content_hash(&document.body);
    let source_path = document
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());
    connection.execute(
        r#"
        INSERT INTO memory_documents (
            source_ref, source_kind, scope_kind, workspace_id, agent_id, source_path,
            title, original_body, body, sanitized_excerpt, metadata_json, content_hash, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(source_ref) DO UPDATE SET
            source_kind=excluded.source_kind,
            scope_kind=excluded.scope_kind,
            workspace_id=excluded.workspace_id,
            agent_id=excluded.agent_id,
            source_path=excluded.source_path,
            title=excluded.title,
            original_body=excluded.original_body,
            body=excluded.body,
            sanitized_excerpt=excluded.sanitized_excerpt,
            metadata_json=excluded.metadata_json,
            content_hash=excluded.content_hash,
            updated_at=excluded.updated_at
        "#,
        params![
            document.source_ref,
            document.source_kind,
            document.scope_kind,
            document.workspace_id,
            document.agent_id,
            source_path,
            document.title,
            document.body,
            indexed_text(&document.body),
            document.sanitized_excerpt,
            metadata_json,
            hash,
            document.updated_at.to_rfc3339(),
        ],
    )?;
    connection.execute(
        "DELETE FROM memory_documents_fts WHERE source_ref = ?1",
        [document.source_ref.as_str()],
    )?;
    connection.execute(
        "INSERT INTO memory_documents_fts(source_ref, title, body, sanitized_excerpt) VALUES (?1, ?2, ?3, ?4)",
        params![
            document.source_ref,
            indexed_text(&document.title),
            indexed_text(&document.body),
            indexed_text(&document.sanitized_excerpt)
        ],
    )?;
    Ok(())
}

fn collect_documents(
    storage: &AppStorage,
    _active_workspace_id: Option<&str>,
) -> Result<Vec<MemoryDocument>> {
    let mut documents = Vec::new();
    documents.extend(agent_memory_documents(storage)?);
    documents.extend(workspace_profile_documents(storage)?);
    documents.extend(brief_documents(storage)?);
    documents.extend(context_episode_documents(storage)?);
    documents.extend(work_item_documents(storage)?);
    Ok(documents)
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
        metadata: json!({ "memory_name": name }),
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
                metadata: json!({ "workspace_anchor": entry.workspace_anchor }),
                updated_at: entry.updated_at,
            }
        })
        .collect())
}

fn brief_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    Ok(storage
        .read_recent_briefs(REBUILD_BRIEF_LIMIT)?
        .into_iter()
        .filter(|brief| !brief.text.trim().is_empty())
        .map(|brief| brief_document(storage, brief))
        .collect())
}

fn brief_document(storage: &AppStorage, brief: BriefRecord) -> MemoryDocument {
    MemoryDocument {
        source_ref: format!("brief:{}", brief.id),
        source_kind: "brief".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(brief.workspace_id),
        agent_id: brief.agent_id,
        source_path: None,
        title: format!("Brief {:?}", brief.kind),
        sanitized_excerpt: excerpt(&brief.text),
        body: brief.text,
        metadata: json!({
            "work_item_id": brief.work_item_id,
            "related_message_id": brief.related_message_id,
            "related_task_id": brief.related_task_id,
            "agent_home": storage.data_dir(),
        }),
        updated_at: brief.created_at,
    }
}

fn context_episode_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    Ok(storage
        .read_recent_context_episodes(REBUILD_EPISODE_LIMIT)?
        .into_iter()
        .filter(|episode| !episode.summary.trim().is_empty())
        .map(episode_document)
        .collect())
}

fn episode_document(episode: ContextEpisodeRecord) -> MemoryDocument {
    let body = [
        episode.summary.clone(),
        episode.work_summary.clone().unwrap_or_default(),
        episode.commands.join("\n"),
        episode.verification.join("\n"),
        episode.decisions.join("\n"),
        episode.carry_forward.join("\n"),
    ]
    .into_iter()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n");
    MemoryDocument {
        source_ref: format!("episode:{}", episode.id),
        source_kind: "context_episode".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(episode.workspace_id),
        agent_id: episode.agent_id,
        source_path: None,
        title: episode
            .work_summary
            .clone()
            .unwrap_or_else(|| "Context episode".into()),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "episode_id": episode.id,
            "current_work_item_id": episode.current_work_item_id,
            "boundary_reason": episode.boundary_reason,
            "working_set_files": episode.working_set_files,
        }),
        updated_at: episode.finalized_at,
    }
}

fn work_item_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    let mut latest = BTreeMap::<String, WorkItemRecord>::new();
    for item in storage.read_recent_work_items(REBUILD_WORK_ITEM_LIMIT)? {
        latest.insert(item.id.clone(), item);
    }
    Ok(latest.into_values().map(work_item_document).collect())
}

fn work_item_document(item: WorkItemRecord) -> MemoryDocument {
    let body = item.delivery_target.clone();
    MemoryDocument {
        source_ref: format!("work_item:{}", item.id),
        source_kind: "work_item".into(),
        scope_kind: "workspace".into(),
        workspace_id: Some(item.workspace_id),
        agent_id: item.agent_id,
        source_path: None,
        title: item.delivery_target.clone(),
        sanitized_excerpt: excerpt(&body),
        body,
        metadata: json!({
            "work_item_id": item.id,
            "state": item.state,
            "blocked_by": item.blocked_by,
        }),
        updated_at: item.updated_at,
    }
}

fn storage_agent_id(storage: &AppStorage) -> String {
    storage
        .read_agent()
        .ok()
        .flatten()
        .map(|agent| agent.id)
        .unwrap_or_else(|| "unknown".into())
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::{
        agent_template::ensure_agent_home_layout,
        types::{
            AgentState, BriefKind, ContextEpisodeRecord, EpisodeBoundaryReason, WorkItemState,
        },
    };

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
        delivery_target: &str,
        status: WorkItemState,
        workspace_id: &str,
    ) -> WorkItemRecord {
        let mut work_item = WorkItemRecord::new(agent_id, delivery_target, status);
        work_item.workspace_id = workspace_id.to_string();
        work_item
    }

    #[test]
    fn memory_search_indexes_agent_memory_and_repairs_external_edits() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
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
    fn memory_get_returns_exact_markdown_and_runtime_source_content() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
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

        assert!(
            get_memory(&storage, &other_brief_ref, None, Some("ws-holon"))
                .unwrap()
                .is_none()
        );
        assert!(get_memory(&storage, &brief_ref, None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn deleting_known_memory_markdown_removes_index_row() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
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
        let storage = AppStorage::new(dir.path()).unwrap();
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
        let mut work_item = WorkItemRecord::new(
            "default",
            "MemorySearch index implementation",
            WorkItemState::Done,
        );
        work_item.workspace_id = "ws-holon".into();
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
                delivery_target: Some("memory search".into()),
                work_summary: Some("index worker".into()),
                scope_hints: vec![],
                summary: "Implemented workspace-aware recall over runtime evidence".into(),
                working_set_files: vec!["src/memory/index.rs".into()],
                commands: vec![],
                verification: vec!["cargo test memory".into()],
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
        assert!(results.iter().any(|result| result.kind == "brief"));
        let results = search_memory(&storage, "SQLite", 10, Some("ws-holon"), false).unwrap();
        assert!(results
            .iter()
            .any(|result| result.kind == "context_episode"));
        let results = search_memory(&storage, "MemorySearch", 10, Some("ws-holon"), false).unwrap();
        assert!(results.iter().any(|result| result.kind == "work_item"));
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
    fn missing_index_rebuilds_all_existing_memory_sources_before_repair() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
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
            WorkItemState::Done,
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
                delivery_target: Some("existing episode".into()),
                work_summary: Some("existing episode summary".into()),
                scope_hints: vec![],
                summary: "existing context episode memory".into(),
                working_set_files: vec![],
                commands: vec![],
                verification: vec![],
                decisions: vec![],
                carry_forward: vec![],
                waiting_on: vec![],
            })
            .unwrap();

        let _ = fs::remove_file(memory_index_path(&storage));
        let _ = fs::remove_file(storage.indexes_dir().join(DIRTY_FILENAME));

        let results = search_memory(&storage, "existing", 10, Some("ws-existing"), false).unwrap();
        assert!(results.iter().any(|result| result.kind == "brief"));
        assert!(results.iter().any(|result| result.kind == "work_item"));
        assert!(results
            .iter()
            .any(|result| result.kind == "context_episode"));
    }

    #[test]
    fn controlled_changed_paths_repair_known_memory_markdown_only() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
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
}
