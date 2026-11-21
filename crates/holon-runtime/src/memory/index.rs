use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    agent_template::{agent_memory_operator_path, agent_memory_self_path},
    memory::refs::{RuntimeRef, ToolExecutionRefSelector, ToolOutputSelector},
    object_resolver::RuntimeObjectResolver,
    runtime_db::{EvidenceKind, RuntimeDb, RuntimeIndexOperation, RuntimeIndexOutboxRow},
    storage::AppStorage,
    tool::helpers::command_preview,
    types::{
        BriefKind, BriefRecord, ContextEpisodeRecord, MessageEnvelope, TaskRecord,
        ToolExecutionRecord, WorkItemRecord, WorkspaceEntry,
    },
};

use holon_memory_index::projection::{
    self, agent_memory_markdown_document, command_execution_record_document_count,
    command_execution_record_documents_slice, command_output_document, command_receipt_document,
    episode_document, generic_tool_execution_output_document_from_record, memory_get_result,
    message_document, message_document_body_with_limit, task_document, turn_document,
    work_item_document, workspace_profile_document,
};
use holon_memory_index::{
    all_backfill_source_kinds, document_key_for, projection_hash, IndexStore, MemoryDocument,
    PendingSource, PreparedPendingSource, RebuildJob, WriterMetrics, INDEX_FILENAME,
    MEMORY_INDEX_OUTBOX_CURSOR, MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING,
    MEMORY_INDEX_REBUILD_PHASE_CLEANUP_SEEN, MEMORY_INDEX_REBUILD_PHASE_FINALIZE,
    MEMORY_INDEX_REBUILD_PHASE_FINALIZE_COMMIT, MEMORY_INDEX_REBUILD_PHASE_PRUNE,
    MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIR, MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIRED,
    MEMORY_INDEX_REBUILD_PHASE_SCAN, MEMORY_INDEX_REBUILD_SOURCE_KIND,
};
pub use holon_memory_index::{MemoryGetResult, MemorySearchResult};

const LEGACY_INDEX_FILENAME: &str = "memory.sqlite3";

const DIRTY_FILENAME: &str = "memory.dirty";

const MEMORY_INDEX_OUTBOX_CONSUME_LIMIT: usize = 500;

const GET_CHARS_DEFAULT: usize = 12_000;

const GET_CHARS_MAX: usize = 50_000;

const MEMORY_INDEX_REBUILD_DEFAULT_SLICE: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchIndexStatus {
    pub freshness: String,
    /// Highest applied contiguous runtime outbox `change_seq`.
    pub cursor: i64,
    /// Monotonic produced watermark: highest `change_seq` ever appended for
    /// the agent. Unlike the drained-outbox maximum, it never returns to 0.
    pub high_watermark: i64,
    /// Sequence distance `produced - applied`. Cross-agent global
    /// autoincrement makes this an upper-bound hint only; `pending_count` is
    /// the exact backlog metric.
    pub lag: i64,
    /// Exact pending outbox row count for this agent.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub pending_count: i64,
    /// Age of the oldest pending outbox row, the real propagation delay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_pending_age_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub indexing_needed: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub results_may_be_incomplete: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub consumption_was_limited: bool,
    /// Failures hit by this index handle's current consume pass. Transient
    /// by design; not a durable health history.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_error_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_at: Option<DateTime<Utc>>,
    /// Machine-readable reasons the index is not currently fresh:
    /// `index_missing`, `dirty_marker`, `backfill_incomplete`, `outbox_lag`,
    /// `pending_sources`, `stale_projection`, `consume_error`. Empty when
    /// fresh. The background daemon self-heals every reason; they are
    /// diagnostic, not a manual-work order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_documents_processed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_last_progress_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchQueryResult {
    pub results: Vec<MemorySearchResult>,
    /// Aggregate status across the agents actually queried. For a
    /// default-agent-only search this equals that agent's status exactly.
    pub index_status: MemorySearchIndexStatus,
    /// Per-agent status covering exactly the queried agents. Present only
    /// when the query spanned more than one agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_status_by_agent: Option<std::collections::BTreeMap<String, MemorySearchIndexStatus>>,
}

pub fn memory_index_path(storage: &AppStorage) -> PathBuf {
    storage.shared_indexes_dir().join(INDEX_FILENAME)
}

pub(crate) fn delete_agent_memory_index_projection(
    shared_indexes_dir: &Path,
    agent_id: &str,
) -> Result<()> {
    let index_path = shared_indexes_dir.join(INDEX_FILENAME);
    if !index_path.exists() {
        return Ok(());
    }
    let index = MemoryIndex::open_shared(shared_indexes_dir)?;
    index.store.delete_agent_projection(agent_id)
}

fn legacy_memory_index_path(storage: &AppStorage) -> PathBuf {
    storage.shared_indexes_dir().join(LEGACY_INDEX_FILENAME)
}

pub fn rebuild_memory_index(storage: &AppStorage, active_workspace_id: Option<&str>) -> Result<()> {
    let mut index = MemoryIndex::open(storage)?;
    index.store.enqueue_rebuild_intent(
        &storage_agent_id(storage),
        active_workspace_id,
        "explicit_rebuild",
    )?;
    loop {
        let result = index.advance_rebuild(storage, MEMORY_INDEX_REBUILD_DEFAULT_SLICE);
        if !index
            .store
            .finish_write("memory_index.advance_rebuild", result)?
        {
            break;
        }
    }
    Ok(())
}

pub fn request_memory_index_rebuild(
    storage: &AppStorage,
    active_workspace_id: Option<&str>,
    reason: &str,
) -> Result<()> {
    let index = MemoryIndex::open(storage)?;
    index
        .store
        .enqueue_rebuild_intent(&storage_agent_id(storage), active_workspace_id, reason)
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
    let result = repair_known_markdown_sources(storage, &index);
    index
        .store
        .finish_write("memory_index.repair_changed_paths", result)
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
    source_kinds: &[String],
    _agent_storages: &[AppStorage],
) -> Result<MemorySearchQueryResult> {
    let agent_id = storage_agent_id(storage);
    let agent_filter = normalize_memory_search_agent_filter(&agent_id, agent_ids);
    log_legacy_index_deprecation(storage);
    let index = MemoryIndex::open_reader(storage)?;
    let results = index.store.search(
        query,
        limit,
        &agent_filter,
        source_kinds,
        active_workspace_id,
        include_all_workspaces,
    )?;
    let mut index_status_by_agent = std::collections::BTreeMap::new();
    for filter_agent_id in &agent_filter {
        let status = index.index_status(storage, filter_agent_id)?;
        index_status_by_agent.insert(filter_agent_id.clone(), status);
    }
    let index_status = aggregate_index_status(index_status_by_agent.values());
    // An empty agent_ids filter means default-agent-only; one status is the
    // whole story, so per-agent detail would be pure duplication.
    let index_status_by_agent = if index_status_by_agent.len() > 1 {
        Some(index_status_by_agent)
    } else {
        None
    };
    Ok(MemorySearchQueryResult {
        results,
        index_status,
        index_status_by_agent,
    })
}

/// Merge per-agent statuses into one aggregate without pretending multiple

/// agents' cursors are a single agent's: freshness is the worst, reasons are

/// the union, pending counts sum, and sequence fields report cross-agent

/// maxima with the details available per agent.

fn aggregate_index_status<'a>(
    statuses: impl Iterator<Item = &'a MemorySearchIndexStatus>,
) -> MemorySearchIndexStatus {
    let mut aggregate = MemorySearchIndexStatus {
        freshness: "fresh".into(),
        cursor: 0,
        high_watermark: 0,
        lag: 0,
        pending_count: 0,
        oldest_pending_age_ms: None,
        indexing_needed: false,
        results_may_be_incomplete: false,
        consumption_was_limited: false,
        skipped_error_count: 0,
        last_indexed_at: None,
        stale_reasons: Vec::new(),
        rebuild_phase: None,
        rebuild_documents_processed: None,
        rebuild_started_at: None,
        rebuild_last_progress_at: None,
    };
    let mut freshness_rank = 0; // fresh < stale < missing
    let mut reasons_seen = std::collections::BTreeSet::new();
    for status in statuses {
        let rank = match status.freshness.as_str() {
            "missing" => 2,
            "stale" => 1,
            _ => 0,
        };
        if rank > freshness_rank {
            freshness_rank = rank;
            aggregate.freshness = status.freshness.clone();
        }
        aggregate.cursor = aggregate.cursor.max(status.cursor);
        aggregate.high_watermark = aggregate.high_watermark.max(status.high_watermark);
        aggregate.lag = aggregate.lag.max(status.lag);
        aggregate.pending_count += status.pending_count;
        aggregate.oldest_pending_age_ms = aggregate
            .oldest_pending_age_ms
            .max(status.oldest_pending_age_ms);
        aggregate.consumption_was_limited |= status.consumption_was_limited;
        aggregate.skipped_error_count += status.skipped_error_count;
        aggregate.last_indexed_at = match (aggregate.last_indexed_at, status.last_indexed_at) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        if aggregate.rebuild_phase.is_none() {
            aggregate.rebuild_phase = status.rebuild_phase.clone();
            aggregate.rebuild_documents_processed = status.rebuild_documents_processed;
            aggregate.rebuild_started_at = status.rebuild_started_at;
            aggregate.rebuild_last_progress_at = status.rebuild_last_progress_at;
        }
        reasons_seen.extend(status.stale_reasons.iter().cloned());
    }
    aggregate.indexing_needed = aggregate.freshness != "fresh";
    aggregate.results_may_be_incomplete = aggregate.indexing_needed;
    aggregate.stale_reasons = reasons_seen.into_iter().collect();
    aggregate
}

pub fn refresh_memory_index_bounded(
    storage: &AppStorage,
    active_workspace_id: Option<&str>,
    batch_limit: usize,
) -> Result<MemorySearchIndexStatus> {
    log_legacy_index_deprecation(storage);
    let mut index = MemoryIndex::open(storage)?;
    let agent_id = storage_agent_id(storage);
    let handled_rebuild = index.store.rebuild_job(&agent_id)?.is_some()
        || !index.store.rebuild_intents_for_agent(&agent_id)?.is_empty();
    let result = index.advance_rebuild(storage, batch_limit.max(1));
    index
        .store
        .finish_write("memory_index.advance_rebuild", result)?;
    if !handled_rebuild {
        refresh_memory_index_for_storage(&mut index, storage, active_workspace_id, batch_limit)?;
    }
    index.index_status(storage, &agent_id)
}

pub fn memory_index_agent_ids_with_pending(storage: &AppStorage) -> Result<Vec<String>> {
    let index = MemoryIndex::open_reader(storage)?;
    index.store.agent_ids_with_pending_sources()
}

/// Agents whose only outstanding index work is self-heal shaped: a dirty

/// marker or missing full-backfill checkpoints, with no pending rows. The

/// daemon uses this to include them in the background work set; without it

/// they are invisible to every existing enumeration and stay stale forever.

///

/// Candidates are agents already known to the index plus `extra_agent_ids`

/// (for example the host's registered agents). A dirty marker for an agent

/// outside every candidate set cannot be mapped back safely (marker keys are

/// lossy) and is left to explicit rebuild.

pub fn memory_index_agent_ids_needing_backfill(
    storage: &AppStorage,
    extra_agent_ids: &[String],
) -> Result<Vec<String>> {
    let index = MemoryIndex::open_reader(storage)?;
    let mut candidates = std::collections::BTreeSet::new();
    candidates.extend(extra_agent_ids.iter().cloned());
    candidates.extend(index.store.agent_ids_with_cursors()?);
    candidates.extend(index.store.agent_ids_with_pending_sources()?);
    let shared_indexes_dir = storage.shared_indexes_dir();
    let mut needing = Vec::new();
    for agent_id in candidates {
        if memory_index_is_dirty_for(&shared_indexes_dir, &agent_id)
            || !index.store.has_backfill_checkpoints_for_agent(&agent_id)?
        {
            needing.push(agent_id);
        }
    }
    Ok(needing)
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
    get_memory_with_limit(
        storage,
        source_ref,
        max_chars
            .unwrap_or(GET_CHARS_DEFAULT)
            .clamp(1, GET_CHARS_MAX),
        active_workspace_id,
        MemoryReadScope::SelfAgent,
    )
}

/// Point-lookup reading scope for runtime-owned memory sources.
///
/// `SelfAgent` keeps today's semantics: only sources owned by the current
/// agent resolve. `AnyAgentMessage` additionally resolves `message:` refs
/// owned by any agent as cross-agent evidence — the response names the source
/// owner and carries an explicit evidence marker, and the source message's
/// authority never transfers to the reading agent. Every other ref kind
/// stays self-agent-only in both scopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryReadScope {
    SelfAgent,
    AnyAgentMessage,
}

/// Read the same authorized source without an inline display limit;
/// `SelfAgent` keeps today's self-owned semantics while `AnyAgentMessage`
/// only widens `message:` refs and marks foreign hits as cross-agent
/// evidence.
pub(crate) fn get_memory_snapshot_scoped(
    storage: &AppStorage,
    source_ref: &str,
    active_workspace_id: Option<&str>,
    scope: MemoryReadScope,
) -> Result<Option<MemoryGetResult>> {
    let Some(mut memory) =
        get_memory_with_limit(storage, source_ref, usize::MAX, active_workspace_id, scope)?
    else {
        return Ok(None);
    };
    // Authorization above must succeed before reading any underlying source.
    if let Ok(RuntimeRef::Message { id }) = RuntimeRef::parse(source_ref) {
        let message = match scope {
            MemoryReadScope::SelfAgent => storage.read_message_by_id(&id)?,
            MemoryReadScope::AnyAgentMessage => storage.read_message_by_id_any_agent(&id)?,
        };
        if let Some(message) = message {
            if scope == MemoryReadScope::SelfAgent && message.agent_id != storage_agent_id(storage)
            {
                return Ok(None);
            }
            memory.content = message_document_body_with_limit(&message, usize::MAX);
        }
    }
    if memory.kind == "tool_command_output" {
        let output: Value = serde_json::from_str(&memory.content)?;
        let selector = output["selector"].as_str().unwrap_or("");
        let recovered = if matches!(selector, "stdout" | "stderr") {
            if let Some(path) = output["artifact"]["path"].as_str() {
                read_snapshot_stream_artifact(storage, path)
            } else if output["truncated"] == false && output["disposition"] == "completed" {
                Ok(output["content"].as_str().unwrap_or("").to_string())
            } else {
                Err(anyhow::anyhow!("full stream artifact is unavailable"))
            }
        } else {
            Err(anyhow::anyhow!(
                "aggregate command preview is not a complete stream; use stdout/stderr refs"
            ))
        };
        match recovered {
            Ok(content) => memory.content = content,
            Err(error) => {
                memory.truncated = true;
                memory.metadata["source_incomplete_reason"] = Value::String(error.to_string());
            }
        }
    }
    Ok(Some(memory))
}

fn read_snapshot_stream_artifact(storage: &AppStorage, path: &str) -> Result<String> {
    let root = storage.data_dir().join("tool-artifacts").canonicalize()?;
    let path = Path::new(path).canonicalize()?;
    // Recorded paths are evidence, not authority to read arbitrary files.
    anyhow::ensure!(
        path.starts_with(&root) && path.is_file(),
        "stream artifact is outside the current agent artifact directory"
    );
    fs::read_to_string(path).context("full stream artifact is unavailable or not UTF-8")
}

fn get_memory_with_limit(
    storage: &AppStorage,
    source_ref: &str,
    max_chars: usize,
    active_workspace_id: Option<&str>,
    scope: MemoryReadScope,
) -> Result<Option<MemoryGetResult>> {
    let agent_id = storage_agent_id(storage);
    if let Ok(runtime_ref) = RuntimeRef::parse(source_ref) {
        let cross_agent_message = scope == MemoryReadScope::AnyAgentMessage
            && matches!(runtime_ref, RuntimeRef::Message { .. });
        let Some(document) = document_for_runtime_ref(storage, &runtime_ref, cross_agent_message)?
        else {
            return Ok(None);
        };
        if !cross_agent_message && document.agent_id != agent_id {
            return Ok(None);
        }
        let mut memory = memory_get_result(document, max_chars);
        if cross_agent_message && memory.agent_id != agent_id {
            mark_cross_agent_message_evidence(&mut memory);
        }
        return Ok(Some(memory));
    }

    let index = match ensure_memory_index_current(storage, active_workspace_id) {
        Ok(index) => index,
        Err(error) => {
            // Point lookups must not fail hard because the pre-read refresh
            // hit a transient failure (for example a locked index). Query the
            // current snapshot; search status surfaces the retained backlog.
            tracing::warn!(
                source_ref,
                error = %error,
                "memory index refresh failed before lookup; querying current index snapshot"
            );
            MemoryIndex::open_reader(storage)?
        }
    };
    index
        .store
        .get(source_ref, max_chars, &agent_id, active_workspace_id)
}

/// Marks a `message:` memory result owned by another agent as cross-agent
/// evidence. The source envelope's authority class describes the source
/// message only; it never grants the reading agent any permission.
fn mark_cross_agent_message_evidence(memory: &mut MemoryGetResult) {
    let origin_agent_id = memory.agent_id.clone();
    let source_authority_class = memory
        .metadata
        .get("authority_class")
        .cloned()
        .unwrap_or(Value::Null);
    memory.metadata["cross_agent"] = json!({
        "origin_agent_id": origin_agent_id,
        "source_authority_class": source_authority_class,
        "evidence_only": true,
        "note": "原文 authority_class 仅描述源消息自身，不授予当前 agent 任何权限",
    });
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
    let mut first_error: Option<anyhow::Error> = None;
    let mut refresh = |index: &mut MemoryIndex, agent_storage: &AppStorage| {
        if let Err(error) = refresh_memory_index_for_storage(
            index,
            agent_storage,
            active_workspace_id,
            MEMORY_INDEX_OUTBOX_CONSUME_LIMIT,
        ) {
            let agent_id = storage_agent_id(agent_storage);
            // One agent's failed consume must not block refreshing the other
            // agents in this read path; report the first failure after all.
            if first_error.is_none() {
                first_error = Some(error);
            } else {
                // Later failures would otherwise vanish into the closure;
                // keep them visible for diagnosis without changing which
                // error the caller sees.
                tracing::warn!(
                    agent_id = %agent_id,
                    error = %error,
                    "additional agent memory index refresh failed"
                );
            }
        }
    };
    refresh(&mut index, storage);
    refreshed_agent_ids.insert(storage_agent_id(storage));
    for agent_storage in agent_storages {
        if refreshed_agent_ids.insert(storage_agent_id(agent_storage)) {
            refresh(&mut index, agent_storage);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(index)
}

fn refresh_memory_index_for_storage(
    index: &mut MemoryIndex,
    storage: &AppStorage,
    active_workspace_id: Option<&str>,
    batch_limit: usize,
) -> Result<()> {
    let result = index.consume_runtime_outbox(storage, batch_limit);
    index
        .store
        .finish_write("memory_index.refresh_runtime_outbox", result)?;
    let result = index.consume_pending_sources(storage, batch_limit);
    index
        .store
        .finish_write("memory_index.refresh_pending_sources", result)?;
    let result = index.consume_stale_source_states(storage, batch_limit);
    index
        .store
        .finish_write("memory_index.refresh_stale_sources", result)?;
    let result = repair_known_markdown_sources(storage, index);
    index
        .store
        .finish_write("memory_index.repair_markdown_sources", result)?;
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
            if index.store.document_hash(&agent_id, &document.source_ref)?
                != Some(projection_hash(&document)?)
            {
                index.store.upsert_document(&document)?;
            }
        } else {
            index.store.delete_document(&agent_id, source.source_ref)?;
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

fn memory_index_is_dirty_for(shared_indexes_dir: &Path, agent_id: &str) -> bool {
    shared_indexes_dir
        .join(dirty_filename_for_agent(agent_id))
        .exists()
}

fn memory_index_is_dirty(storage: &AppStorage) -> bool {
    memory_index_is_dirty_for(&storage.shared_indexes_dir(), &storage_agent_id(storage))
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

#[derive(Debug)]
struct RebuildSourceRecord {
    cursor: String,
    documents: Vec<MemoryDocument>,
    document_offset: usize,
    total_documents: usize,
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

fn prepare_pending_source(
    storage: &AppStorage,
    source: &PendingSource,
) -> Result<PreparedPendingSource> {
    if source.operation == RuntimeIndexOperation::Delete.as_str() {
        return Ok(PreparedPendingSource::Delete);
    }
    Ok(match document_for_pending_source(storage, source)? {
        Some(document) => PreparedPendingSource::Upsert(document),
        None => PreparedPendingSource::Delete,
    })
}

fn runtime_index_runtime_id(runtime_db: &RuntimeDb) -> String {
    runtime_db.path().display().to_string()
}

fn rebuild_source_records(
    storage: &AppStorage,
    source_kind: &str,
    cursor: &str,
    inclusive: bool,
    source_offset: usize,
    limit: usize,
) -> Result<Vec<RebuildSourceRecord>> {
    let runtime_db = storage
        .runtime_db()?
        .context("runtime database is required for memory index rebuild")?;
    let agent_id = storage_agent_id(storage);
    match source_kind {
        "agent_memory_markdown" => {
            let mut documents = agent_memory_documents(storage)?;
            documents.sort_by(|left, right| left.source_ref.cmp(&right.source_ref));
            Ok(documents
                .into_iter()
                .filter(|document| {
                    if inclusive {
                        document.source_ref.as_str() >= cursor
                    } else {
                        document.source_ref.as_str() > cursor
                    }
                })
                .take(limit)
                .map(|document| RebuildSourceRecord {
                    cursor: document.source_ref.clone(),
                    documents: vec![document],
                    document_offset: 0,
                    total_documents: 1,
                })
                .collect())
        }
        "workspace_profile" => Ok(runtime_payload_page::<WorkspaceEntry>(
            &runtime_db,
            "workspace_entries",
            "workspace_id",
            None,
            cursor,
            inclusive,
            limit,
        )?
        .into_iter()
        .map(|(cursor, entry)| RebuildSourceRecord {
            cursor,
            documents: vec![workspace_profile_document(entry, &agent_id)],
            document_offset: 0,
            total_documents: 1,
        })
        .collect()),
        "message" => Ok(runtime_payload_page::<MessageEnvelope>(
            &runtime_db,
            "messages",
            "evidence_id",
            Some(("agent_id", &agent_id)),
            cursor,
            inclusive,
            limit,
        )?
        .into_iter()
        .map(|(cursor, message)| RebuildSourceRecord {
            cursor,
            documents: vec![message_document(message)],
            document_offset: 0,
            total_documents: 1,
        })
        .collect()),
        "brief" => Ok(runtime_payload_page::<BriefRecord>(
            &runtime_db,
            "briefs",
            "evidence_id",
            Some(("agent_id", &agent_id)),
            cursor,
            inclusive,
            limit,
        )?
        .into_iter()
        .map(|(cursor, brief)| {
            let documents: Vec<_> = semantic_brief_is_retrievable(&brief)
                .then(|| brief_document(storage, brief))
                .into_iter()
                .collect();
            RebuildSourceRecord {
                cursor,
                total_documents: documents.len(),
                documents,
                document_offset: 0,
            }
        })
        .collect()),
        "context_episode" => Ok(runtime_payload_page::<ContextEpisodeRecord>(
            &runtime_db,
            "context_episode_anchors",
            "episode_id",
            Some(("agent_id", &agent_id)),
            cursor,
            inclusive,
            limit,
        )?
        .into_iter()
        .map(|(cursor, episode)| RebuildSourceRecord {
            cursor,
            documents: vec![episode_document(episode)],
            document_offset: 0,
            total_documents: 1,
        })
        .collect()),
        "work_item" => Ok(runtime_payload_page::<WorkItemRecord>(
            &runtime_db,
            "work_items",
            "work_item_id",
            Some(("agent_id", &agent_id)),
            cursor,
            inclusive,
            limit,
        )?
        .into_iter()
        .map(|(cursor, item)| RebuildSourceRecord {
            cursor,
            documents: vec![work_item_document(item)],
            document_offset: 0,
            total_documents: 1,
        })
        .collect()),
        "task" => Ok(runtime_payload_page::<TaskRecord>(
            &runtime_db,
            "tasks",
            "task_id",
            Some(("owner_agent_id", &agent_id)),
            cursor,
            inclusive,
            limit,
        )?
        .into_iter()
        .map(|(cursor, task)| RebuildSourceRecord {
            cursor,
            documents: vec![task_document(task, command_preview)],
            document_offset: 0,
            total_documents: 1,
        })
        .collect()),
        "tool_command_receipt" => Ok(runtime_payload_page::<ToolExecutionRecord>(
            &runtime_db,
            "tool_executions",
            "evidence_id",
            Some(("agent_id", &agent_id)),
            cursor,
            inclusive,
            1,
        )?
        .into_iter()
        .map(|(record_cursor, record)| {
            let document_offset = if inclusive && record_cursor == cursor {
                source_offset
            } else {
                0
            };
            let total_documents = command_execution_record_document_count(&record);
            RebuildSourceRecord {
                cursor: record_cursor,
                documents: command_execution_record_documents_slice(
                    &record,
                    document_offset,
                    limit,
                    command_preview,
                ),
                document_offset,
                total_documents,
            }
        })
        .collect()),
        other => anyhow::bail!("unsupported memory index rebuild source kind {other:?}"),
    }
}

fn runtime_payload_page<T: DeserializeOwned>(
    runtime_db: &RuntimeDb,
    table: &str,
    key_column: &str,
    agent_filter: Option<(&str, &str)>,
    cursor: &str,
    inclusive: bool,
    limit: usize,
) -> Result<Vec<(String, T)>> {
    let comparison = if inclusive { ">=" } else { ">" };
    let sql = if let Some((agent_column, _)) = agent_filter {
        format!(
            "SELECT {key_column}, payload_json
             FROM {table}
             WHERE {agent_column} = ?1 AND {key_column} {comparison} ?2
             ORDER BY {key_column}
             LIMIT ?3"
        )
    } else {
        format!(
            "SELECT {key_column}, payload_json
             FROM {table}
             WHERE {key_column} {comparison} ?1
             ORDER BY {key_column}
             LIMIT ?2"
        )
    };
    let rows = runtime_db.with_read(|connection| {
        let mut statement = connection.prepare(&sql)?;
        let decode = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String)> {
            Ok((row.get(0)?, row.get(1)?))
        };
        let rows = if let Some((_, agent_id)) = agent_filter {
            statement
                .query_map(
                    params![agent_id, cursor, i64::try_from(limit).unwrap_or(i64::MAX)],
                    decode,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            statement
                .query_map(
                    params![cursor, i64::try_from(limit).unwrap_or(i64::MAX)],
                    decode,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    })?;
    rows.into_iter()
        .map(|(key, payload)| {
            serde_json::from_str(&payload)
                .with_context(|| format!("failed to decode {table} payload for {key}"))
                .map(|record| (key, record))
        })
        .collect()
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
        "message" => message_document_by_id(storage, &source.source_id, false),
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
    any_agent_message: bool,
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
        RuntimeRef::Message { id } => message_document_by_id(storage, id, any_agent_message),
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
    Ok(Some(agent_memory_markdown_document(
        storage_agent_id(storage),
        name,
        title,
        path,
        body,
        file_updated_at(path),
    )))
}

fn workspace_profile_documents(storage: &AppStorage) -> Result<Vec<MemoryDocument>> {
    let mut latest = BTreeMap::<String, WorkspaceEntry>::new();
    for entry in storage.read_recent_workspace_entries(usize::MAX)? {
        latest.insert(entry.workspace_id.clone(), entry);
    }
    Ok(latest
        .into_values()
        .map(|entry| workspace_profile_document(entry, &storage_agent_id(storage)))
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

fn semantic_brief_is_retrievable(brief: &BriefRecord) -> bool {
    brief.kind != BriefKind::Ack && !brief.text.trim().is_empty()
}

fn brief_document(storage: &AppStorage, brief: BriefRecord) -> MemoryDocument {
    let body = RuntimeObjectResolver::new(storage)
        .resolve_brief_content(&brief)
        .unwrap_or_else(|_| brief.text.clone());
    projection::brief_document(brief, body, storage.data_dir())
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
    any_agent: bool,
) -> Result<Option<MemoryDocument>> {
    let message = if any_agent {
        storage.read_message_by_id_any_agent(message_id)?
    } else {
        storage.read_message_by_id(message_id)?
    };
    Ok(message.map(message_document))
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

fn turn_document_by_id(storage: &AppStorage, turn_id: &str) -> Result<Option<MemoryDocument>> {
    let Some(turn) = storage
        .read_recent_turns(usize::MAX)?
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)
    else {
        return Ok(None);
    };
    let tool_execution_ids = turn
        .tool_execution_ids
        .iter()
        .take(12)
        .cloned()
        .collect::<Vec<_>>();
    let tool_executions = storage.read_tool_executions_by_ids(&tool_execution_ids)?;
    Ok(Some(turn_document(turn, &tool_executions)))
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

fn task_document_by_id(storage: &AppStorage, task_id: &str) -> Result<Option<MemoryDocument>> {
    let runtime_db = storage.runtime_db()?;
    let task = if let Some(runtime_db) = runtime_db.as_ref() {
        runtime_db.tasks().latest(task_id)?
    } else {
        storage.latest_task_record(task_id)?
    };
    Ok(task.map(|task| task_document(task, command_preview)))
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
            .map(|cmd| command_receipt_document(&record, None, None, cmd, command_preview))),
        ("ExecCommandBatch", Some(index), ToolExecutionRefSelector::Cmd) => Ok(record
            .input
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| items.get(index - 1))
            .and_then(|item| {
                item.get("cmd").and_then(Value::as_str).map(|cmd| {
                    command_receipt_document(&record, Some(index), Some(item), cmd, command_preview)
                })
            })),
        ("ExecCommand", None, ToolExecutionRefSelector::Output(stream)) => Ok(
            command_output_document(&record, None, stream.as_ref_selector()),
        ),
        (
            "ExecCommandBatch",
            None,
            ToolExecutionRefSelector::Output(ToolOutputSelector::Output),
            // Batch-level refs expose only the aggregate output; stream refs remain per-item.
        ) => Ok(Some(generic_tool_execution_output_document_from_record(
            &record,
        ))),
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

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

const WRITER_METRICS: WriterMetrics = WriterMetrics {
    enqueued: crate::diagnostics::record_memory_index_writer_enqueued,
    dequeued: crate::diagnostics::record_memory_index_writer_dequeued,
    timed_out: crate::diagnostics::record_memory_index_writer_timeout,
    queue_wait: crate::diagnostics::record_memory_index_writer_queue_wait,
};

pub(crate) struct MemoryIndex {
    store: IndexStore,
    last_outbox_consume_reached_limit: bool,
    last_outbox_error_count: usize,
}

impl MemoryIndex {
    fn open(storage: &AppStorage) -> Result<Self> {
        Self::open_shared(&storage.shared_indexes_dir())
    }

    fn open_reader(storage: &AppStorage) -> Result<Self> {
        let store = IndexStore::open_reader_in(&storage.shared_indexes_dir(), WRITER_METRICS)?;
        Ok(Self::from_store(store))
    }

    /// Opens the shared memory index for a long-lived cached connection.
    pub(crate) fn open_shared(shared_indexes_dir: &Path) -> Result<Self> {
        Ok(Self::from_store(IndexStore::open_in(
            shared_indexes_dir,
            WRITER_METRICS,
        )?))
    }

    fn advance_rebuild(&mut self, storage: &AppStorage, slice_limit: usize) -> Result<bool> {
        let agent_id = storage_agent_id(storage);
        let mut remaining = slice_limit.max(1);
        let started_job = if self.store.rebuild_job(&agent_id)?.is_none() {
            self.start_rebuild_job(storage)?
        } else {
            false
        };
        let Some(initial_job) = self.store.rebuild_job(&agent_id)? else {
            return Ok(false);
        };
        if started_job {
            self.consume_initial_dirty_marker(storage, &initial_job)?;
        }
        loop {
            let Some(job) = self.store.rebuild_job(&agent_id)? else {
                return Ok(false);
            };
            let used = match job.phase.as_str() {
                MEMORY_INDEX_REBUILD_PHASE_SCAN => {
                    self.scan_rebuild_slice(storage, &job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_PRUNE | MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIRED => {
                    self.store.prune_rebuild_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_PRUNE_REPAIR => {
                    self.store.prune_rebuild_fts_repair_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_FINALIZE => {
                    // Also upgrades jobs persisted in the old unsliced final
                    // phase. Do not publish completion before cleanup resumes.
                    self.store
                        .update_rebuild_phase(&job, MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING)?;
                    0
                }
                MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING => {
                    self.store.cleanup_rebuild_pending_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_CLEANUP_SEEN => {
                    self.store.cleanup_rebuild_seen_slice(&job, remaining)?
                }
                MEMORY_INDEX_REBUILD_PHASE_FINALIZE_COMMIT => {
                    self.finalize_rebuild(storage, &job)?;
                    return Ok(false);
                }
                phase => anyhow::bail!(
                    "unsupported memory index rebuild phase {phase:?} for agent {}",
                    job.agent_id
                ),
            };
            remaining = remaining.saturating_sub(used);
            if used > 0 && remaining == 0 {
                return Ok(true);
            }
        }
    }

    fn start_rebuild_job(&mut self, storage: &AppStorage) -> Result<bool> {
        let agent_id = storage_agent_id(storage);
        let Some(_intent) = self
            .store
            .rebuild_intents_for_agent(&agent_id)?
            .into_iter()
            .next()
        else {
            return Ok(false);
        };
        let runtime_db = storage
            .runtime_db()?
            .context("runtime database is required for memory index rebuild")?;
        let runtime_high_watermark = runtime_db
            .runtime_index_outbox()
            .produced_watermark_for_agent(&agent_id)?;
        let generation = Uuid::new_v4().simple().to_string();
        let now = Utc::now();
        self.store
            .start_rebuild(&agent_id, &generation, runtime_high_watermark, now)?;
        tracing::info!(
            agent_id = %agent_id,
            generation = %generation,
            runtime_high_watermark,
            "started bounded memory index rebuild"
        );
        Ok(true)
    }

    fn consume_initial_dirty_marker(&self, storage: &AppStorage, job: &RebuildJob) -> Result<()> {
        let dirty_path = storage
            .shared_indexes_dir()
            .join(dirty_filename_for_agent(&job.agent_id));
        if !dirty_path.exists() {
            return Ok(());
        }
        let consumed_path = dirty_path.with_extension(format!("rebuild-{}.dirty", job.generation));
        match fs::rename(&dirty_path, &consumed_path) {
            Ok(()) => {
                let _ = fs::remove_file(consumed_path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn scan_rebuild_slice(
        &mut self,
        storage: &AppStorage,
        job: &RebuildJob,
        slice_limit: usize,
    ) -> Result<usize> {
        let source_kinds = all_backfill_source_kinds();
        if job.source_kind_index >= source_kinds.len() {
            self.store
                .update_rebuild_phase(job, MEMORY_INDEX_REBUILD_PHASE_PRUNE)?;
            return Ok(0);
        }
        let source_kind = source_kinds[job.source_kind_index];
        let records = rebuild_source_records(
            storage,
            source_kind,
            &job.source_cursor,
            job.source_offset > 0,
            job.source_offset,
            slice_limit,
        )?;
        if records.is_empty() {
            let next_index = job.source_kind_index + 1;
            let phase = if next_index >= source_kinds.len() {
                MEMORY_INDEX_REBUILD_PHASE_PRUNE
            } else {
                MEMORY_INDEX_REBUILD_PHASE_SCAN
            };
            self.store
                .advance_rebuild_source_kind(job, next_index, phase)?;
            return Ok(0);
        }

        let mut remaining = slice_limit;
        let mut next_cursor = job.source_cursor.clone();
        let mut next_offset = job.source_offset;
        let mut documents = Vec::new();
        for record in records {
            let offset = record.document_offset.min(record.total_documents);
            let available = record.total_documents.saturating_sub(offset);
            let take = record.documents.len().min(remaining);
            documents.extend(record.documents.into_iter().take(take));
            remaining = remaining.saturating_sub(take.max(1));
            if take < available {
                next_cursor = record.cursor;
                next_offset = offset + take;
                break;
            }
            next_cursor = record.cursor;
            next_offset = 0;
            if remaining == 0 {
                break;
            }
        }

        self.store
            .apply_rebuild_scan(job, &documents, &next_cursor, next_offset)?;
        tracing::debug!(
            agent_id = %job.agent_id,
            generation = %job.generation,
            source_kind,
            documents = documents.len(),
            "advanced bounded memory index rebuild scan"
        );
        Ok(slice_limit.saturating_sub(remaining))
    }

    fn finalize_rebuild(&mut self, storage: &AppStorage, job: &RebuildJob) -> Result<()> {
        let runtime_db = storage
            .runtime_db()?
            .context("runtime database is required for memory index rebuild")?;
        let runtime_id = runtime_index_runtime_id(&runtime_db);
        let now = Utc::now();
        self.store.finalize_rebuild(job, &runtime_id, now)?;
        if job.runtime_high_watermark > 0 {
            if let Err(error) = runtime_db
                .runtime_index_outbox()
                .delete_acknowledged_through(&job.agent_id, job.runtime_high_watermark)
            {
                tracing::warn!(
                    agent_id = %job.agent_id,
                    through_change_seq = job.runtime_high_watermark,
                    error = %error,
                    "failed to delete memory index outbox rows acknowledged by rebuild"
                );
            }
        }
        tracing::info!(
            agent_id = %job.agent_id,
            generation = %job.generation,
            documents_processed = job.documents_processed,
            elapsed_ms = (now - job.started_at).num_milliseconds().max(0),
            "completed bounded memory index rebuild"
        );
        Ok(())
    }

    fn consume_pending_sources(&mut self, storage: &AppStorage, limit: usize) -> Result<()> {
        let agent_id = storage_agent_id(storage);
        let pending = self
            .store
            .pending_sources_for_agent_with_limit(&agent_id, limit)?;
        for pending_source in pending {
            let source = &pending_source.source;
            if source.source_kind == MEMORY_INDEX_REBUILD_SOURCE_KIND {
                continue;
            }
            let prepared = prepare_pending_source(storage, source);
            self.store
                .consume_pending_source_snapshot(&pending_source, &prepared)?;
        }
        Ok(())
    }

    fn consume_stale_source_states(&mut self, storage: &AppStorage, limit: usize) -> Result<()> {
        let agent_id = storage_agent_id(storage);
        let stale_sources = self
            .store
            .stale_source_states_for_agent_with_limit(&agent_id, limit)?;
        for source in stale_sources {
            let prepared = match prepare_pending_source(storage, &source) {
                Ok(prepared) => prepared,
                Err(error) => {
                    tracing::warn!(
                        agent_id,
                        source_kind = %source.source_kind,
                        source_ref = %source.source_ref,
                        error = %error,
                        "skipping failed stale memory index source refresh"
                    );
                    continue;
                }
            };
            self.store
                .apply_stale_source(&agent_id, &source, &prepared)?;
        }
        self.store.refresh_stale_meta(&agent_id)
    }

    fn consume_runtime_outbox(&mut self, storage: &AppStorage, limit: usize) -> Result<()> {
        self.last_outbox_consume_reached_limit = false;
        self.last_outbox_error_count = 0;
        let Some(runtime_db) = storage.runtime_db()? else {
            return Ok(());
        };
        let agent_id = storage_agent_id(storage);
        let runtime_id = runtime_index_runtime_id(&runtime_db);
        let cursor = self
            .store
            .cursor(&runtime_id, &agent_id, MEMORY_INDEX_OUTBOX_CURSOR)?;
        let rows = runtime_db
            .runtime_index_outbox()
            .read_after(&agent_id, cursor, limit)?;
        let row_count = rows.len();
        // The cursor only advances over the contiguous acknowledged prefix:
        // each row must apply successfully and advance the cursor inside one
        // index transaction before it counts as consumed.
        let mut last_successful_seq = cursor;
        let mut consume_error: Option<anyhow::Error> = None;
        for row in rows {
            let source = pending_source_from_outbox_row(&row);
            let prepared = match prepare_pending_source(storage, &source) {
                Ok(prepared) => prepared,
                Err(error) => {
                    consume_error = Some(error.context(format!(
                        "agent_id={} change_seq={} source_kind={} source_ref={}",
                        row.agent_id, row.change_seq, row.source_kind, row.source_ref
                    )));
                    break;
                }
            };
            let applied = self.store.apply_outbox_change(
                &runtime_id,
                &agent_id,
                &source,
                &prepared,
                row.change_seq,
            );
            match applied {
                Ok(()) => {
                    last_successful_seq = row.change_seq;
                }
                Err(error) => {
                    consume_error = Some(error.context(format!(
                        "agent_id={} change_seq={} source_kind={} source_ref={}",
                        row.agent_id, row.change_seq, row.source_kind, row.source_ref
                    )));
                    break;
                }
            }
        }

        // Acknowledged rows are those at or below the committed cursor: rows
        // applied this round, plus any rows a crash between apply and delete
        // or a full rebuild cursor jump left behind. A failed row and
        // everything after it stay in the runtime outbox as a durable retry
        // queue; only rows at or below `last_successful_seq` are ever
        // deleted.
        if last_successful_seq > 0 {
            if let Err(error) = runtime_db
                .runtime_index_outbox()
                .delete_acknowledged_through(&agent_id, last_successful_seq)
            {
                tracing::warn!(
                    agent_id = %agent_id,
                    through_change_seq = last_successful_seq,
                    error = %error,
                    "failed to delete consumed memory index outbox rows"
                );
            }
        }
        if let Some(error) = &consume_error {
            self.last_outbox_error_count += 1;
            tracing::warn!(
                agent_id = %agent_id,
                through_change_seq = last_successful_seq,
                error = ?error,
                "memory index outbox apply failed; retaining rows for ordered retry"
            );
            // Head-of-line blocking is intentional: the consumer stops at the
            // first failure so the applied cursor never crosses an unapplied
            // row and no row is dropped before it is proven applied.
            return Err(consume_error.unwrap());
        }
        self.last_outbox_consume_reached_limit = limit > 0 && row_count >= limit;
        Ok(())
    }

    fn index_status(
        &self,
        storage: &AppStorage,
        agent_id: &str,
    ) -> Result<MemorySearchIndexStatus> {
        let shared_indexes_dir = storage.shared_indexes_dir();
        let has_stale_projection = self.store.has_stale_source_states_for_agent(agent_id)?;
        let has_pending_sources = self.store.has_pending_sources_for_agent(agent_id)?;
        let lacks_full_backfill = !self.store.has_backfill_checkpoints_for_agent(agent_id)?;
        let is_dirty = memory_index_is_dirty_for(&shared_indexes_dir, agent_id);
        let rebuild_job = self.store.rebuild_job(agent_id)?;
        // Order matters for readability, not semantics: every reason is
        // independently healed by the background daemon.
        let mut stale_reasons = Vec::new();
        if !memory_index_path(storage).exists() {
            stale_reasons.push("index_missing".to_string());
        }
        if is_dirty {
            stale_reasons.push("dirty_marker".to_string());
        }
        if lacks_full_backfill {
            stale_reasons.push("backfill_incomplete".to_string());
        }
        if has_stale_projection {
            stale_reasons.push("stale_projection".to_string());
        }
        if has_pending_sources {
            stale_reasons.push("pending_sources".to_string());
        }
        if rebuild_job.is_some() {
            stale_reasons.push("rebuild_in_progress".to_string());
        }
        if self.last_outbox_error_count > 0 {
            stale_reasons.push("consume_error".to_string());
        }
        let Some(runtime_db) = storage.runtime_db()? else {
            let indexing_needed = is_dirty
                || has_stale_projection
                || has_pending_sources
                || lacks_full_backfill
                || rebuild_job.is_some();
            return Ok(MemorySearchIndexStatus {
                freshness: if indexing_needed { "stale" } else { "fresh" }.into(),
                cursor: 0,
                high_watermark: 0,
                lag: 0,
                pending_count: 0,
                oldest_pending_age_ms: None,
                indexing_needed,
                results_may_be_incomplete: indexing_needed,
                consumption_was_limited: false,
                skipped_error_count: self.last_outbox_error_count,
                last_indexed_at: None,
                stale_reasons,
                rebuild_phase: rebuild_job.as_ref().map(|job| job.phase.clone()),
                rebuild_documents_processed: rebuild_job
                    .as_ref()
                    .map(|job| job.documents_processed),
                rebuild_started_at: rebuild_job.as_ref().map(|job| job.started_at),
                rebuild_last_progress_at: rebuild_job.as_ref().map(|job| job.last_progress_at),
            });
        };
        let runtime_id = runtime_index_runtime_id(&runtime_db);
        let cursor = self
            .store
            .cursor(&runtime_id, agent_id, MEMORY_INDEX_OUTBOX_CURSOR)?;
        let high_watermark = runtime_db
            .runtime_index_outbox()
            .produced_watermark_for_agent(agent_id)?;
        let pending_count = runtime_db
            .runtime_index_outbox()
            .pending_count_for_agent(agent_id, cursor)?;
        let oldest_pending_age_ms = match runtime_db
            .runtime_index_outbox()
            .oldest_pending_created_at_for_agent(agent_id, cursor)?
        {
            Some(oldest_pending_at) if pending_count > 0 => {
                Some((Utc::now() - oldest_pending_at).num_milliseconds().max(0))
            }
            _ => None,
        };
        let lag = (high_watermark - cursor).max(0);
        if lag > 0 || pending_count > 0 {
            stale_reasons.push("outbox_lag".to_string());
        }
        let freshness = if !memory_index_path(storage).exists() {
            "missing"
        } else if is_dirty
            || lag > 0
            || pending_count > 0
            || has_stale_projection
            || has_pending_sources
            || lacks_full_backfill
            || rebuild_job.is_some()
        {
            "stale"
        } else {
            "fresh"
        };
        let indexing_needed = freshness != "fresh";
        Ok(MemorySearchIndexStatus {
            freshness: freshness.into(),
            cursor,
            high_watermark,
            lag,
            pending_count,
            oldest_pending_age_ms,
            indexing_needed,
            results_may_be_incomplete: indexing_needed,
            consumption_was_limited: self.last_outbox_consume_reached_limit && lag > 0,
            skipped_error_count: self.last_outbox_error_count,
            last_indexed_at: self.store.cursor_updated_at(
                &runtime_id,
                agent_id,
                MEMORY_INDEX_OUTBOX_CURSOR,
            )?,
            stale_reasons,
            rebuild_phase: rebuild_job.as_ref().map(|job| job.phase.clone()),
            rebuild_documents_processed: rebuild_job.as_ref().map(|job| job.documents_processed),
            rebuild_started_at: rebuild_job.as_ref().map(|job| job.started_at),
            rebuild_last_progress_at: rebuild_job.as_ref().map(|job| job.last_progress_at),
        })
    }

    fn from_store(store: IndexStore) -> Self {
        Self {
            store,
            last_outbox_consume_reached_limit: false,
            last_outbox_error_count: 0,
        }
    }

    pub(crate) fn enqueue_upsert(
        &mut self,
        agent_id: &str,
        source_kind: &str,
        source_id: &str,
        source_ref: &str,
    ) -> Result<()> {
        self.store
            .enqueue_upsert(agent_id, source_kind, source_id, source_ref)
    }
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
#[path = "index_workspace_tests.rs"]
mod tests_workspace;

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {

    use holon_memory_index::MEMORY_INDEX_BACKFILL_CURSOR;

    use holon_memory_index::test_support::{
        maintenance_test_documents, maintenance_test_job, seed_outbox_cursor,
    };
    use tempfile::tempdir;

    use super::*;
    use crate::{
        agent_template::ensure_agent_home_layout,
        runtime_db::RuntimeIndexChange,
        tool::helpers::command_digest,
        types::{
            AgentState, BriefContentSource, BriefKind, ContextEpisodeRecord, EpisodeBoundaryReason,
            MessageBody, TaskKind, TaskRecord, TaskStatus, TodoItem, TodoItemState,
            TranscriptEntry, TranscriptEntryKind, TurnRecord, WorkItemPlanStatus, WorkItemState,
        },
    };
    use serde_json::json;

    #[test]
    fn turn_document_only_advertises_resolvable_tool_refs() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        let turn_id = "turn-ref-contract";
        let command_id = "tool-command-ref-contract";
        let patch_id = "tool-patch-ref-contract";
        let batch_id = "tool-batch-ref-contract";

        let mut turn = TurnRecord::new("default", turn_id, 1);
        turn.tool_execution_ids = vec![
            command_id.into(),
            patch_id.into(),
            batch_id.into(),
            "tool-missing-ref-contract".into(),
        ];
        storage.append_turn(&turn).unwrap();

        let record = |id: &str, tool_name: &str, input: Value, output: Value| ToolExecutionRecord {
            id: id.into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: Some(turn_id.into()),
            tool_name: tool_name.into(),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: 1,
            authority_class: crate::types::AuthorityClass::OperatorInstruction,
            status: crate::types::ToolExecutionStatus::Success,
            input,
            output,
            summary: format!("{tool_name} test record"),
            call_binding: None,
            invocation_surface: None,
        };

        storage
            .append_tool_execution(&record(
                command_id,
                "ExecCommand",
                json!({"cmd": "printf command"}),
                json!({"stdout_preview": "command"}),
            ))
            .unwrap();
        storage
            .append_tool_execution(&record(
                patch_id,
                "ApplyPatch",
                json!({"patch": "*** Begin Patch"}),
                json!({"applied": true}),
            ))
            .unwrap();
        storage
            .append_tool_execution(&record(
                batch_id,
                "ExecCommandBatch",
                json!({
                    "items": [
                        {"cmd": "printf batch-one"},
                        {"path": "README.md"},
                        {"cmd": "printf batch-three"}
                    ]
                }),
                json!({"items": []}),
            ))
            .unwrap();

        let turn_memory = get_memory(&storage, &format!("turn:{turn_id}"), None, None)
            .unwrap()
            .unwrap();
        assert!(turn_memory
            .content
            .contains(&format!("tool_execution:{command_id}:cmd")));
        assert!(turn_memory
            .content
            .contains(&format!("tool_execution:{command_id}:output")));
        assert!(turn_memory
            .content
            .contains(&format!("tool_execution:{patch_id}:output")));
        assert!(turn_memory
            .content
            .contains(&format!("tool_execution:{batch_id}:batch_item:1:cmd")));
        assert!(turn_memory
            .content
            .contains(&format!("tool_execution:{batch_id}:batch_item:3:cmd")));
        assert!(turn_memory
            .content
            .contains(&format!("tool_execution:{batch_id}:output")));
        assert!(!turn_memory
            .content
            .contains(&format!("tool_execution:{patch_id}:cmd")));
        assert!(!turn_memory
            .content
            .contains(&format!("tool_execution:{batch_id}:cmd")));
        assert!(!turn_memory
            .content
            .contains("tool_execution:tool-missing-ref-contract:"));

        for source_ref in [
            format!("tool_execution:{command_id}:cmd"),
            format!("tool_execution:{command_id}:output"),
            format!("tool_execution:{patch_id}:output"),
            format!("tool_execution:{batch_id}:batch_item:1:cmd"),
            format!("tool_execution:{batch_id}:batch_item:3:cmd"),
            format!("tool_execution:{batch_id}:output"),
        ] {
            assert!(
                get_memory(&storage, &source_ref, None, None)
                    .unwrap()
                    .is_some(),
                "expected MemoryGet to resolve {source_ref}"
            );
        }
        assert!(get_memory(
            &storage,
            &format!("tool_execution:{patch_id}:cmd"),
            None,
            None
        )
        .unwrap()
        .is_none());
        assert!(get_memory(
            &storage,
            &format!("tool_execution:{batch_id}:cmd"),
            None,
            None
        )
        .unwrap()
        .is_none());
        assert!(get_memory(
            &storage,
            "tool_execution:tool-missing-ref-contract:output",
            None,
            None
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn maintenance_legacy_finalize_cleans_in_slices_before_completion() -> Result<()> {
        let directory = tempdir()?;
        let storage = AppStorage::new_for_agent_for_test(directory.path(), "default")?;
        let mut index = MemoryIndex::open(&storage)?;
        let keys = maintenance_test_documents(&index.store, "default", 140)?;
        for number in 0..140 {
            index.enqueue_upsert(
                "default",
                "message",
                &number.to_string(),
                &format!("message:pending-{number:04}"),
            )?;
        }
        let job =
            maintenance_test_job(&index.store, "default", MEMORY_INDEX_REBUILD_PHASE_FINALIZE)?;
        for key in keys {
            index.store.test_connection().execute(
                "INSERT INTO memory_index_rebuild_seen VALUES (?1, ?2, ?3)",
                params![job.agent_id, job.generation, key],
            )?;
        }
        assert!(index.advance_rebuild(&storage, 7)?);
        let progress = index.store.rebuild_job("default")?.unwrap();
        assert_eq!(progress.phase, MEMORY_INDEX_REBUILD_PHASE_CLEANUP_PENDING);
        assert!(!progress.source_cursor.is_empty());
        assert_eq!(index.store.pending_sources_for_agent("default")?.len(), 133);
        index.enqueue_upsert("default", "message", "new", "message:aaa-new")?;
        index.store.test_connection().execute(
            "UPDATE memory_index_pending_sources SET enqueued_at = ?1
             WHERE source_ref = 'message:aaa-new'",
            [(job.started_at + chrono::Duration::seconds(1)).to_rfc3339()],
        )?;
        drop(index);
        let mut index = MemoryIndex::open(&storage)?;
        assert_eq!(
            index.store.rebuild_job("default")?.unwrap().source_cursor,
            progress.source_cursor
        );
        for _ in 0..128 {
            if !index.advance_rebuild(&storage, 7)? {
                break;
            }
        }
        assert!(index.store.rebuild_job("default")?.is_none());
        let pending = index.store.pending_sources_for_agent("default")?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].source_ref, "message:aaa-new");
        let seen: usize = index.store.test_connection().query_row(
            "SELECT COUNT(*) FROM memory_index_rebuild_seen",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(seen, 0);
        Ok(())
    }

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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        refresh_memory_index_bounded(&storage, None, 10).unwrap();
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
        let alpha = AppStorage::new_for_agent_for_test(&alpha_home, "alpha").unwrap();
        let beta = AppStorage::new_for_agent_for_test(&beta_home, "beta").unwrap();
        alpha
            .write_agent_for_fixture(&AgentState::new("alpha"))
            .unwrap();
        beta.write_agent_for_fixture(&AgentState::new("beta"))
            .unwrap();
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
        let alpha = AppStorage::new_for_agent_for_test(&alpha_home, "alpha").unwrap();
        let beta = AppStorage::new_for_agent_for_test(&beta_home, "beta").unwrap();
        alpha
            .write_agent_for_fixture(&AgentState::new("alpha"))
            .unwrap();
        beta.write_agent_for_fixture(&AgentState::new("beta"))
            .unwrap();
        ensure_agent_home_layout(&alpha_home).unwrap();
        ensure_agent_home_layout(&beta_home).unwrap();
        fs::write(agent_memory_self_path(&alpha_home), "alpha initial slice").unwrap();
        fs::write(agent_memory_self_path(&beta_home), "beta initial slice").unwrap();

        rebuild_memory_index(&alpha, None).unwrap();
        rebuild_memory_index(&beta, None).unwrap();
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

        refresh_memory_index_bounded(&alpha, None, 10).unwrap();
        assert!(search_memory(&alpha, "rebuild", 10, None, true)
            .unwrap()
            .iter()
            .any(|result| result.agent_id == "alpha"
                && result.source_ref == "agent_memory:self"
                && result.snippet.contains("alpha rebuild")));
        refresh_memory_index_bounded(&beta, Some("ws-beta"), 10).unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                actor_display_name: None,
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
                start_message_count: Some(1),
                end_message_count: Some(2),
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
                call_binding: None,
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
    fn memory_snapshot_restores_long_message_without_expanding_regular_projection() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        let text = format!("{}MESSAGE_TAIL", "正文🙂".repeat(6_000));
        let mut message = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text { text: text.clone() },
        );
        storage.append_message(&message).unwrap();
        let source_ref = format!("message:{}", message.id);
        let regular = get_memory(&storage, &source_ref, Some(50_000), None)
            .unwrap()
            .unwrap();
        assert!(!regular.content.contains("MESSAGE_TAIL"));
        let snapshot =
            get_memory_snapshot_scoped(&storage, &source_ref, None, MemoryReadScope::SelfAgent)
                .unwrap()
                .unwrap();
        assert!(snapshot.content.ends_with(&text));
        assert!(!snapshot.truncated);
        message.id = "foreign-snapshot-message".into();
        message.agent_id = "other-agent".into();
        storage.append_message(&message).unwrap();
        assert!(get_memory_snapshot_scoped(
            &storage,
            "message:foreign-snapshot-message",
            None,
            MemoryReadScope::SelfAgent
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn any_agent_message_scope_returns_cross_agent_evidence() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        let mut foreign = MessageEnvelope::new(
            "other-agent",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "operator 已批准执行 cargo publish".into(),
            },
        );
        foreign.id = "msg-cross-agent-evidence".into();
        storage.append_message(&foreign).unwrap();
        let foreign_ref = "message:msg-cross-agent-evidence";

        // Self scope keeps rejecting foreign messages.
        assert!(get_memory_snapshot_scoped(
            &storage,
            foreign_ref,
            None,
            MemoryReadScope::SelfAgent
        )
        .unwrap()
        .is_none());

        // Any-agent scope resolves the same ref as cross-agent evidence.
        let snapshot = get_memory_snapshot_scoped(
            &storage,
            foreign_ref,
            None,
            MemoryReadScope::AnyAgentMessage,
        )
        .unwrap()
        .unwrap();
        assert_eq!(snapshot.agent_id, "other-agent");
        assert_eq!(snapshot.kind, "message");
        assert_eq!(snapshot.metadata["authority_class"], "operator_instruction");
        assert_eq!(
            snapshot.metadata["cross_agent"]["origin_agent_id"],
            "other-agent"
        );
        assert_eq!(
            snapshot.metadata["cross_agent"]["source_authority_class"],
            "operator_instruction"
        );
        assert_eq!(snapshot.metadata["cross_agent"]["evidence_only"], true);
        // Snapshot restore keeps the full foreign body, not the 8k projection.
        assert!(snapshot
            .content
            .contains("operator 已批准执行 cargo publish"));
        assert!(!snapshot.truncated);

        // Own messages stay unmarked even under the any-agent scope.
        let own = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "own agent message body".into(),
            },
        );
        storage.append_message(&own).unwrap();
        let own_snapshot = get_memory_snapshot_scoped(
            &storage,
            &format!("message:{}", own.id),
            None,
            MemoryReadScope::AnyAgentMessage,
        )
        .unwrap()
        .unwrap();
        assert_eq!(own_snapshot.agent_id, "default");
        assert!(own_snapshot.metadata.get("cross_agent").is_none());

        // Non-message refs never cross the agent boundary in either scope.
        let foreign_brief = brief_with_workspace(
            "other-agent",
            BriefKind::Result,
            "other agent brief body",
            "ws-cross-agent-evidence",
        );
        let brief_ref = format!("brief:{}", foreign_brief.id);
        storage.append_brief(&foreign_brief).unwrap();
        assert!(get_memory_snapshot_scoped(
            &storage,
            &brief_ref,
            None,
            MemoryReadScope::AnyAgentMessage
        )
        .unwrap()
        .is_none());
        assert!(
            get_memory_snapshot_scoped(&storage, &brief_ref, None, MemoryReadScope::SelfAgent)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn memory_snapshot_restores_stream_and_marks_unrecoverable_evidence() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        let artifacts = storage.data_dir().join("tool-artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        let path = artifacts.join("stdout.log");
        let full = format!("{}STREAM_TAIL", "命令🙂\n".repeat(6_000));
        fs::write(&path, &full).unwrap();
        let mut record = ToolExecutionRecord {
            id: "snapshot-stream".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".into(),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: 1,
            authority_class: crate::types::AuthorityClass::OperatorInstruction,
            status: crate::types::ToolExecutionStatus::Success,
            input: json!({"cmd": "test"}),
            output: json!({"result": {
                "disposition": "completed", "stdout_preview": "命令🙂",
                "truncated": true, "stdout_artifact": 0,
                "artifacts": [{"path": path}]
            }}),
            summary: "completed".into(),
            call_binding: None,
            invocation_surface: None,
        };
        storage.append_tool_execution(&record).unwrap();
        let source_ref = "tool_execution:snapshot-stream:stdout";
        assert!(!get_memory(&storage, source_ref, None, None)
            .unwrap()
            .unwrap()
            .content
            .contains("STREAM_TAIL"));
        let snapshot =
            get_memory_snapshot_scoped(&storage, source_ref, None, MemoryReadScope::SelfAgent)
                .unwrap()
                .unwrap();
        assert_eq!(snapshot.content, full);
        assert!(!snapshot.truncated);
        fs::remove_file(&path).unwrap();
        let missing =
            get_memory_snapshot_scoped(&storage, source_ref, None, MemoryReadScope::SelfAgent)
                .unwrap()
                .unwrap();
        assert!(missing.truncated);
        assert!(missing.metadata["source_incomplete_reason"].is_string());
        let outside = dir.path().join("secret.txt");
        fs::write(&outside, "MUST_NOT_READ").unwrap();
        record.id = "snapshot-outside".into();
        record.output["result"]["artifacts"][0]["path"] = json!(outside);
        storage.append_tool_execution(&record).unwrap();
        let denied = get_memory_snapshot_scoped(
            &storage,
            "tool_execution:snapshot-outside:stdout",
            None,
            MemoryReadScope::SelfAgent,
        )
        .unwrap()
        .unwrap();
        assert!(denied.truncated);
        assert!(!denied.content.contains("MUST_NOT_READ"));
        assert!(denied.metadata["source_incomplete_reason"]
            .as_str()
            .unwrap()
            .contains("outside"));
        record.id = "snapshot-legacy".into();
        record.output["result"]
            .as_object_mut()
            .unwrap()
            .remove("artifacts");
        storage.append_tool_execution(&record).unwrap();
        let legacy = get_memory_snapshot_scoped(
            &storage,
            "tool_execution:snapshot-legacy:stdout",
            None,
            MemoryReadScope::SelfAgent,
        )
        .unwrap()
        .unwrap();
        assert!(legacy.truncated);
        assert!(legacy.metadata["source_incomplete_reason"].is_string());
    }

    #[test]
    fn memory_get_message_refs_are_scoped_to_current_agent() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();

        let mut other_agent_message = MessageEnvelope::new(
            "other-agent",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let _runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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

        let stale_response =
            search_memory_query(&storage, "outbox discovery", 10, Some("ws-holon"), false).unwrap();
        assert!(!stale_response
            .results
            .iter()
            .any(|result| result.source_ref == brief_ref));
        assert_eq!(stale_response.index_status.freshness, "stale");
        assert_eq!(stale_response.index_status.lag, 1);
        assert!(stale_response.index_status.indexing_needed);
        assert!(stale_response.index_status.results_may_be_incomplete);

        let status = refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
        assert_eq!(status.freshness, "stale");
        assert_eq!(status.lag, 0);
        assert!(status.indexing_needed);
        assert!(status.results_may_be_incomplete);
        assert!(!status.consumption_was_limited);
        assert_eq!(status.skipped_error_count, 0);

        let response =
            search_memory_query(&storage, "outbox discovery", 10, Some("ws-holon"), false).unwrap();
        assert!(response
            .results
            .iter()
            .any(|result| result.source_ref == brief_ref));
        assert_eq!(response.index_status.freshness, "stale");
        assert_eq!(response.index_status.lag, 0);
        assert!(response.index_status.results_may_be_incomplete);
        assert!(!response.index_status.consumption_was_limited);
        assert_eq!(response.index_status.skipped_error_count, 0);
    }

    #[test]
    fn memory_search_status_reports_limited_runtime_outbox_consumption() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
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
    fn consume_runtime_outbox_deletes_consumed_rows() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();

        let changes: Vec<_> = (0..5)
            .map(|i| RuntimeIndexChange {
                agent_id: "default".into(),
                source_kind: "brief".into(),
                source_id: format!("test-{i}"),
                source_ref: format!("brief:test-{i}"),
                operation: RuntimeIndexOperation::Upsert,
                source_updated_at: Some(Utc::now()),
                reason: "test_consume_delete".into(),
            })
            .collect();
        runtime_db
            .runtime_index_outbox()
            .append_changes(&changes)
            .unwrap();

        // Outbox has rows before consumption.
        assert_eq!(
            runtime_db
                .runtime_index_outbox()
                .high_watermark_for_agent("default")
                .unwrap(),
            5
        );

        // Consume all rows via the full refresh path.
        let status = refresh_memory_index_bounded(&storage, None, 100).unwrap();
        assert_eq!(status.lag, 0);
        assert!(!status.consumption_was_limited);

        // All consumed rows should have been deleted.
        assert_eq!(
            runtime_db
                .runtime_index_outbox()
                .high_watermark_for_agent("default")
                .unwrap(),
            0
        );
    }

    #[test]
    fn consume_runtime_outbox_stops_at_failed_row_and_retains_backlog() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();

        // The middle row must fail projection: `agent_memory:self` points at
        // a directory, so reading the source document returns a real error.
        let self_memory_dir = agent_memory_self_path(storage.data_dir());
        std::fs::create_dir_all(&self_memory_dir).unwrap();

        let noop = |id: String| RuntimeIndexChange {
            agent_id: "default".into(),
            source_kind: "brief".into(),
            source_id: id.clone(),
            source_ref: format!("brief:{id}"),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(Utc::now()),
            reason: "test_outbox_retry".into(),
        };
        let failing = RuntimeIndexChange {
            agent_id: "default".into(),
            source_kind: "agent_memory_markdown".into(),
            source_id: "self".into(),
            source_ref: "agent_memory:self".into(),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(Utc::now()),
            reason: "test_outbox_retry".into(),
        };
        runtime_db
            .runtime_index_outbox()
            .append_changes(&[noop("noop-1".into()), failing, noop("noop-2".into())])
            .unwrap();

        let mut index = MemoryIndex::open(&storage).unwrap();
        let error = index
            .consume_runtime_outbox(&storage, 10)
            .expect_err("middle row apply must fail");
        assert!(error.to_string().contains("agent_memory:self"));

        // The cursor stops at the acknowledged contiguous prefix.
        let runtime_id = runtime_index_runtime_id(&runtime_db);
        let cursor = index
            .store
            .cursor(&runtime_id, "default", MEMORY_INDEX_OUTBOX_CURSOR)
            .unwrap();
        assert_eq!(cursor, 1);

        // The failed row and its successor stay pending for ordered retry.
        let outbox = runtime_db.runtime_index_outbox();
        let remaining: Vec<i64> = outbox
            .read_after("default", 0, 10)
            .unwrap()
            .into_iter()
            .map(|row| row.change_seq)
            .collect();
        assert_eq!(remaining, vec![2, 3]);
        assert_eq!(
            outbox.pending_count_for_agent("default", cursor).unwrap(),
            2
        );
        assert_eq!(outbox.produced_watermark_for_agent("default").unwrap(), 3);
        assert!(outbox
            .oldest_pending_created_at_for_agent("default", cursor)
            .unwrap()
            .is_some());

        // Clear the failure and retry: replay resumes at the failed row, the
        // applied prefix is not re-processed, and the backlog fully drains.
        std::fs::remove_dir(&self_memory_dir).unwrap();
        index.consume_runtime_outbox(&storage, 10).unwrap();
        let status = index.index_status(&storage, "default").unwrap();
        assert_eq!(status.cursor, 3);
        assert_eq!(status.high_watermark, 3);
        assert_eq!(status.lag, 0);
        assert_eq!(status.pending_count, 0);
        assert_eq!(
            outbox
                .pending_count_for_agent("default", status.cursor)
                .unwrap(),
            0
        );
        assert_eq!(
            outbox
                .oldest_pending_created_at_for_agent("default", status.cursor)
                .unwrap(),
            None
        );
    }

    #[test]
    fn memory_search_status_keeps_produced_watermark_after_drain() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        let changes: Vec<_> = (0..2)
            .map(|i| RuntimeIndexChange {
                agent_id: "default".into(),
                source_kind: "brief".into(),
                source_id: format!("wm-{i}"),
                source_ref: format!("brief:wm-{i}"),
                operation: RuntimeIndexOperation::Upsert,
                source_updated_at: Some(Utc::now()),
                reason: "test_watermark".into(),
            })
            .collect();
        runtime_db
            .runtime_index_outbox()
            .append_changes(&changes)
            .unwrap();

        let status = refresh_memory_index_bounded(&storage, None, 100).unwrap();
        // Draining the outbox must not reset the watermark to 0: `lag` and
        // freshness stay meaningful after row GC.
        assert_eq!(status.cursor, 2);
        assert_eq!(status.high_watermark, 2);
        assert_eq!(status.lag, 0);
        assert_eq!(status.pending_count, 0);
        assert!(status.oldest_pending_age_ms.is_none());
    }

    #[test]
    fn memory_search_status_reports_exact_pending_backlog() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        let consume_limit = 2;
        let changes = (0..=consume_limit)
            .map(|index| RuntimeIndexChange {
                agent_id: "default".into(),
                source_kind: "brief".into(),
                source_id: format!("pending-{index}"),
                source_ref: format!("brief:pending-{index}"),
                operation: RuntimeIndexOperation::Upsert,
                source_updated_at: Some(Utc::now()),
                reason: "test_pending_backlog".into(),
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
        assert_eq!(status.pending_count, 1);
        let oldest_pending_age_ms = status.oldest_pending_age_ms.expect("one pending row");
        assert!(oldest_pending_age_ms >= 0);
        assert!(status.consumption_was_limited);
    }

    #[test]
    fn memory_get_hydrates_transcript_backed_brief_content() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();

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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                actor_display_name: None,
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
                start_message_count: Some(1),
                end_message_count: Some(3),
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
        refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
        let results = search_memory(&storage, "fresh", 10, Some("ws-holon"), false).unwrap();
        assert!(results
            .iter()
            .any(|result| result.kind == "brief" && result.snippet.contains("fresh")));
    }

    #[test]
    fn missing_index_does_not_rebuild_all_existing_memory_sources_during_search() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                start_message_count: Some(1),
                end_message_count: Some(2),
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
            .store
            .pending_sources_for_agent("default")
            .unwrap()
            .iter()
            .any(|source| source.source_ref == fresh_ref));

        refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
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
            .store
            .pending_sources_for_agent("default")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pending_source_snapshot_does_not_apply_after_same_key_reenqueue() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        fs::write(
            agent_memory_self_path(dir.path()),
            "pending source revision sentinel",
        )
        .unwrap();
        rebuild_memory_index(&storage, None).unwrap();

        let mut index = MemoryIndex::open(&storage).unwrap();
        index
            .store
            .enqueue_source(
                "default",
                "agent_memory_markdown",
                "self",
                "agent_memory:self",
                "delete",
                None,
                "test_stale_delete",
            )
            .unwrap();
        let document_key = document_key_for("default", "agent_memory:self");
        let stale = index
            .store
            .pending_sources_for_agent_with_limit("default", usize::MAX)
            .unwrap()
            .into_iter()
            .find(|source| source.source.document_key == document_key)
            .unwrap();

        index
            .store
            .enqueue_source(
                "default",
                "agent_memory_markdown",
                "self",
                "agent_memory:self",
                "upsert",
                None,
                "test_new_upsert",
            )
            .unwrap();
        index
            .store
            .consume_pending_source_snapshot(&stale, &Ok(PreparedPendingSource::Delete))
            .unwrap();

        let (operation, revision): (String, i64) = index
            .store
            .test_connection()
            .query_row(
                "SELECT operation, revision FROM memory_index_pending_sources
                 WHERE document_key = ?1",
                [&document_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(operation, "upsert");
        assert!(revision > stale.revision);
        let document_exists: bool = index
            .store
            .test_connection()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM memory_documents WHERE document_key = ?1
                 )",
                [&document_key],
                |row| row.get(0),
            )
            .unwrap();
        assert!(document_exists);

        index.consume_pending_sources(&storage, 10).unwrap();
        assert!(index
            .store
            .pending_sources_for_agent("default")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pending_delete_removes_stale_document_and_source_state() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
            .store
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
            .store
            .test_connection()
            .execute(
                "DELETE FROM memory_documents WHERE agent_id = ?1",
                ["default"],
            )
            .unwrap();
        index
            .store
            .test_connection()
            .execute(
                "DELETE FROM memory_index_checkpoints WHERE agent_id = ?1",
                ["default"],
            )
            .unwrap();
        drop(index);

        let response =
            search_memory_query(&storage, "checkpoint recovery", 10, Some("ws-holon"), false)
                .unwrap();
        assert!(
            response.results.is_empty(),
            "MemorySearch must not use missing checkpoints to trigger full rebuild"
        );
        assert_eq!(response.index_status.freshness, "stale");
        assert!(response.index_status.indexing_needed);
        assert!(response.index_status.results_may_be_incomplete);
        assert!(!MemoryIndex::open(&storage)
            .unwrap()
            .store
            .has_backfill_checkpoints_for_agent("default")
            .unwrap());
    }

    #[test]
    fn stale_source_state_schema_is_refreshed_by_bounded_backfill() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
            .store
            .test_connection()
            .execute(
                "UPDATE memory_documents
                 SET sanitized_excerpt = 'stale schema version projection'
                 WHERE agent_id = ?1",
                ["default"],
            )
            .unwrap();
        index
            .store
            .test_connection()
            .execute(
                "UPDATE memory_index_source_state
                 SET index_schema_version = 0, projection_version = 0
                 WHERE agent_id = ?1",
                ["default"],
            )
            .unwrap();
        drop(index);

        let stale_response = search_memory_query(
            &storage,
            "schema version rebuild",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        let stale_result = stale_response
            .results
            .iter()
            .find(|result| result.kind == "brief")
            .expect("stale projection remains searchable before bounded refresh");
        assert_eq!(stale_response.index_status.freshness, "stale");
        assert_eq!(stale_result.snippet, "stale schema version projection");
        assert!(!MemoryIndex::open(&storage)
            .unwrap()
            .store
            .has_current_source_state_for_agent("default")
            .unwrap());

        let status = refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
        assert_eq!(status.freshness, "fresh");
        let refreshed = search_memory_query(
            &storage,
            "schema version rebuild",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        let refreshed_result = refreshed
            .results
            .iter()
            .find(|result| result.kind == "brief")
            .expect("bounded refresh should keep the document searchable");
        assert_ne!(refreshed_result.snippet, "stale schema version projection");
        assert!(MemoryIndex::open(&storage)
            .unwrap()
            .store
            .has_current_source_state_for_agent("default")
            .unwrap());
    }

    #[test]
    fn rebuild_intent_marks_index_stale_and_is_consumed_by_bounded_refresh() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        fs::write(
            agent_memory_self_path(dir.path()),
            "obsolete-only-token rebuild intent sentinel",
        )
        .unwrap();
        rebuild_memory_index(&storage, None).unwrap();

        fs::write(
            agent_memory_self_path(dir.path()),
            "refreshed-only-token rebuild intent sentinel",
        )
        .unwrap();
        request_memory_index_rebuild(&storage, None, "test_rebuild").unwrap();
        assert_eq!(
            memory_index_agent_ids_with_pending(&storage).unwrap(),
            vec!["default"]
        );

        let stale = search_memory_query(&storage, "obsolete-only-token", 10, None, false).unwrap();
        assert_eq!(stale.index_status.freshness, "stale");
        assert!(stale
            .results
            .iter()
            .any(|result| result.source_ref == "agent_memory:self"));

        let status = refresh_memory_index_bounded(&storage, None, 10).unwrap();
        assert_eq!(status.freshness, "fresh");
        assert!(
            search_memory(&storage, "obsolete-only-token", 10, None, false)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_memory(&storage, "refreshed-only-token", 10, None, false)
                .unwrap()
                .iter()
                .any(|result| result.source_ref == "agent_memory:self")
        );
        assert!(!MemoryIndex::open(&storage)
            .unwrap()
            .store
            .has_pending_sources_for_agent("default")
            .unwrap());
        assert!(memory_index_agent_ids_with_pending(&storage)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn failed_rebuild_preserves_intent_for_retry() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        rebuild_memory_index(&storage, None).unwrap();

        let self_path = agent_memory_self_path(dir.path());
        fs::remove_file(&self_path).unwrap();
        fs::create_dir(&self_path).unwrap();
        request_memory_index_rebuild(&storage, None, "test_rebuild_failure").unwrap();

        refresh_memory_index_bounded(&storage, None, 10).unwrap_err();
        assert_eq!(
            memory_index_agent_ids_with_pending(&storage).unwrap(),
            vec!["default"]
        );
        assert_eq!(
            MemoryIndex::open(&storage)
                .unwrap()
                .store
                .rebuild_intents_for_agent("default")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn sliced_rebuild_persists_progress_and_prunes_old_documents() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        fs::write(
            agent_memory_self_path(dir.path()),
            "obsolete sliced rebuild sentinel",
        )
        .unwrap();
        rebuild_memory_index(&storage, None).unwrap();
        fs::remove_file(agent_memory_self_path(dir.path())).unwrap();

        for index in 0..4 {
            let mut message = MessageEnvelope::new(
                "default",
                crate::types::MessageKind::OperatorPrompt,
                crate::types::MessageOrigin::Operator {
                    actor_id: Some("operator:test".into()),
                    actor_display_name: None,
                },
                crate::types::AuthorityClass::OperatorInstruction,
                crate::types::Priority::Normal,
                MessageBody::Text {
                    text: format!("sliced rebuild message {index}"),
                },
            );
            message.id = format!("msg-sliced-rebuild-{index}");
            storage.append_message(&message).unwrap();
        }
        request_memory_index_rebuild(&storage, None, "test_sliced_resume").unwrap();

        let first = refresh_memory_index_bounded(&storage, None, 1).unwrap();
        assert_eq!(first.rebuild_phase.as_deref(), Some("scan"));
        assert_eq!(first.rebuild_documents_processed, Some(1));
        let first_job = MemoryIndex::open(&storage)
            .unwrap()
            .store
            .rebuild_job("default")
            .unwrap()
            .unwrap();

        let mut previous_documents = first_job.documents_processed;
        let mut final_status = first;
        for _ in 0..64 {
            final_status = refresh_memory_index_bounded(&storage, None, 1).unwrap();
            let job = MemoryIndex::open(&storage)
                .unwrap()
                .store
                .rebuild_job("default")
                .unwrap();
            if let Some(job) = job {
                assert!(job.documents_processed >= previous_documents);
                previous_documents = job.documents_processed;
            } else {
                break;
            }
        }

        assert_eq!(
            final_status.freshness, "fresh",
            "unexpected final status: {final_status:?}"
        );
        assert!(final_status.rebuild_phase.is_none());
        assert!(MemoryIndex::open(&storage)
            .unwrap()
            .store
            .rebuild_job("default")
            .unwrap()
            .is_none());
        let obsolete_results = search_memory(
            &storage,
            "obsolete sliced rebuild sentinel",
            10,
            None,
            false,
        )
        .unwrap();
        assert!(
            obsolete_results
                .iter()
                .all(|result| result.source_ref != "agent_memory:self"),
            "unexpected obsolete self-memory result: {obsolete_results:?}"
        );
        assert!(
            search_memory(&storage, "sliced rebuild message 3", 10, None, false)
                .unwrap()
                .iter()
                .any(|result| result.source_ref == "message:msg-sliced-rebuild-3")
        );
    }

    #[test]
    fn rebuild_finalize_preserves_new_dirty_pending_and_monotonic_cursor() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        rebuild_memory_index(&storage, None).unwrap();

        let first_brief = brief_with_workspace(
            "default",
            BriefKind::Result,
            "rebuild snapshot watermark",
            "ws-holon",
        );
        storage.append_brief(&first_brief).unwrap();
        request_memory_index_rebuild(&storage, None, "test_dirty_revision").unwrap();
        refresh_memory_index_bounded(&storage, None, 1).unwrap();

        let job = MemoryIndex::open(&storage)
            .unwrap()
            .store
            .rebuild_job("default")
            .unwrap()
            .unwrap();
        let runtime_db = storage.runtime_db().unwrap().unwrap();
        let runtime_id = runtime_index_runtime_id(&runtime_db);
        let advanced_cursor = job.runtime_high_watermark + 100;
        let index = MemoryIndex::open(&storage).unwrap();
        seed_outbox_cursor(&index.store, &runtime_id, "default", advanced_cursor).unwrap();

        let second_brief = brief_with_workspace(
            "default",
            BriefKind::Result,
            "post snapshot dirty and pending sentinel",
            "ws-holon",
        );
        storage.append_brief(&second_brief).unwrap();
        storage.mark_memory_index_dirty().unwrap();
        let pending_ref = format!("brief:{}", second_brief.id);
        MemoryIndex::open(&storage)
            .unwrap()
            .enqueue_upsert("default", "brief", &second_brief.id, &pending_ref)
            .unwrap();
        request_memory_index_rebuild(&storage, None, "self_heal").unwrap();

        let mut status = refresh_memory_index_bounded(&storage, None, 10).unwrap();
        for _ in 0..32 {
            if status.rebuild_phase.is_none() {
                break;
            }
            status = refresh_memory_index_bounded(&storage, None, 10).unwrap();
        }

        let index = MemoryIndex::open(&storage).unwrap();
        assert_eq!(
            index
                .store
                .cursor(&runtime_id, "default", MEMORY_INDEX_OUTBOX_CURSOR)
                .unwrap(),
            advanced_cursor
        );
        assert!(memory_index_is_dirty(&storage));
        assert!(index
            .store
            .pending_sources_for_agent("default")
            .unwrap()
            .iter()
            .any(|source| source.source_ref == pending_ref));
        assert!(index
            .store
            .rebuild_intents_for_agent("default")
            .unwrap()
            .is_empty());
        assert!(status.stale_reasons.contains(&"dirty_marker".to_string()));
        assert!(status
            .stale_reasons
            .contains(&"pending_sources".to_string()));
    }

    #[test]
    fn one_agent_sliced_rebuild_does_not_block_another_agent_refresh() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");
        let alpha_home = agents_dir.join("alpha");
        let beta_home = agents_dir.join("beta");
        let alpha = AppStorage::new_for_agent_for_test(&alpha_home, "alpha").unwrap();
        let beta = AppStorage::new_for_agent_for_test(&beta_home, "beta").unwrap();
        alpha
            .write_agent_for_fixture(&AgentState::new("alpha"))
            .unwrap();
        beta.write_agent_for_fixture(&AgentState::new("beta"))
            .unwrap();
        ensure_agent_home_layout(&alpha_home).unwrap();
        ensure_agent_home_layout(&beta_home).unwrap();
        rebuild_memory_index(&alpha, None).unwrap();
        rebuild_memory_index(&beta, None).unwrap();

        for index in 0..8 {
            let mut message = MessageEnvelope::new(
                "alpha",
                crate::types::MessageKind::OperatorPrompt,
                crate::types::MessageOrigin::Operator {
                    actor_id: Some("operator:test".into()),
                    actor_display_name: None,
                },
                crate::types::AuthorityClass::OperatorInstruction,
                crate::types::Priority::Normal,
                MessageBody::Text {
                    text: format!("large alpha rebuild message {index}"),
                },
            );
            message.id = format!("msg-alpha-rebuild-{index}");
            alpha.append_message(&message).unwrap();
        }
        request_memory_index_rebuild(&alpha, None, "test_fairness").unwrap();
        let alpha_status = refresh_memory_index_bounded(&alpha, None, 1).unwrap();
        assert!(alpha_status.rebuild_phase.is_some());

        let beta_brief = brief_with_workspace(
            "beta",
            BriefKind::Result,
            "beta advances before alpha rebuild completes",
            "ws-holon",
        );
        beta.append_brief(&beta_brief).unwrap();
        let beta_status = refresh_memory_index_bounded(&beta, None, 10).unwrap();
        assert_eq!(beta_status.pending_count, 0);
        assert_eq!(beta_status.cursor, beta_status.high_watermark);
        let beta_results = search_memory(
            &beta,
            "beta advances before alpha rebuild completes",
            10,
            Some("ws-holon"),
            false,
        )
        .unwrap();
        let indexed_beta_documents = {
            let index = MemoryIndex::open(&beta).unwrap();
            let mut statement = index
                .store
                .test_connection()
                .prepare(
                    "SELECT agent_id, source_ref, title
                     FROM memory_documents
                     WHERE agent_id = 'beta'
                     ORDER BY source_ref",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert!(
            beta_results
                .iter()
                .any(|result| result.source_ref == format!("brief:{}", beta_brief.id)),
            "unexpected beta results: {beta_results:?}; indexed={indexed_beta_documents:?}"
        );
        assert!(MemoryIndex::open(&alpha)
            .unwrap()
            .store
            .rebuild_job("alpha")
            .unwrap()
            .is_some());
    }

    #[test]
    fn rebuild_command_record_paging_bounds_document_construction() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-rebuild-page".into(),
                agent_id: "default".into(),
                work_item_id: None,
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommandBatch".into(),
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 10,
                authority_class: crate::types::AuthorityClass::OperatorInstruction,
                status: crate::types::ToolExecutionStatus::Success,
                input: json!({
                    "items": [
                        {"cmd": "echo rebuild-page-1"},
                        {"cmd": "echo rebuild-page-2"},
                        {"cmd": "echo rebuild-page-3"}
                    ]
                }),
                output: json!({"completed_count": 3}),
                summary: "ExecCommandBatch completed 3/3 items".into(),
                call_binding: None,
                invocation_surface: None,
            })
            .unwrap();

        let first =
            rebuild_source_records(&storage, "tool_command_receipt", "", false, 0, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].documents.len(), 1);
        assert_eq!(first[0].total_documents, 3);
        assert_eq!(
            first[0].documents[0].source_ref,
            "tool_execution:tool-rebuild-page:batch_item:1:cmd"
        );

        let second = rebuild_source_records(
            &storage,
            "tool_command_receipt",
            &first[0].cursor,
            true,
            1,
            1,
        )
        .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].document_offset, 1);
        assert_eq!(second[0].documents.len(), 1);
        assert_eq!(
            second[0].documents[0].source_ref,
            "tool_execution:tool-rebuild-page:batch_item:2:cmd"
        );
    }

    #[test]
    fn self_heal_discovery_finds_dirty_and_backfill_missing_agents() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        let candidates = vec!["default".to_string()];

        // Never backfilled: the agent is invisible to pending-row
        // enumerations but must be discovered for self-heal.
        assert_eq!(
            memory_index_agent_ids_needing_backfill(&storage, &candidates).unwrap(),
            vec!["default".to_string()]
        );

        rebuild_memory_index(&storage, None).unwrap();
        assert!(
            memory_index_agent_ids_needing_backfill(&storage, &candidates)
                .unwrap()
                .is_empty()
        );

        // A dirty marker alone (no pending rows anywhere) is rediscovered.
        storage.mark_memory_index_dirty().unwrap();
        assert_eq!(
            memory_index_agent_ids_needing_backfill(&storage, &candidates).unwrap(),
            vec!["default".to_string()]
        );
    }

    #[test]
    fn daemon_self_heal_flow_backfills_and_clears_dirty_marker() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        fs::write(
            agent_memory_self_path(dir.path()),
            "self-heal backfill sentinel",
        )
        .unwrap();
        storage.mark_memory_index_dirty().unwrap();

        // Daemon behavior for a discovered self-heal agent: enqueue a durable
        // intent (idempotently), then run the bounded refresh.
        request_memory_index_rebuild(&storage, None, "self_heal").unwrap();
        request_memory_index_rebuild(&storage, None, "self_heal_retry").unwrap();
        let status = refresh_memory_index_bounded(&storage, None, 10).unwrap();

        assert_eq!(status.freshness, "fresh");
        assert!(status.stale_reasons.is_empty());
        assert!(!memory_index_is_dirty(&storage));
        assert!(MemoryIndex::open(&storage)
            .unwrap()
            .store
            .has_backfill_checkpoints_for_agent("default")
            .unwrap());
        assert!(memory_index_agent_ids_with_pending(&storage)
            .unwrap()
            .is_empty());
        assert!(
            search_memory(&storage, "self-heal backfill sentinel", 10, None, false)
                .unwrap()
                .iter()
                .any(|result| result.source_ref == "agent_memory:self")
        );
    }

    #[test]
    fn rebuild_after_outbox_drain_keeps_cursor_at_produced_watermark() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        let brief = brief_with_workspace(
            "default",
            BriefKind::Result,
            "drained watermark rebuild sentinel",
            "ws-holon",
        );
        storage.append_brief(&brief).unwrap();

        // Incremental consume drains the outbox; produced watermark stays 1
        // while the remaining-row maximum collapses to 0.
        let status = refresh_memory_index_bounded(&storage, None, 10).unwrap();
        assert_eq!(status.lag, 0);
        assert_eq!(status.cursor, 1);
        assert_eq!(status.high_watermark, 1);
        assert_eq!(status.freshness, "stale");
        assert!(status
            .stale_reasons
            .contains(&"backfill_incomplete".to_string()));

        // A rebuild triggered after the drain must acknowledge the produced
        // watermark, not the drained maximum, or the cursor regresses to 0
        // and the agent reports outbox lag forever.
        request_memory_index_rebuild(&storage, None, "self_heal").unwrap();
        let rebuilt = refresh_memory_index_bounded(&storage, None, 10).unwrap();
        assert_eq!(rebuilt.freshness, "fresh");
        assert_eq!(rebuilt.cursor, 1);
        assert_eq!(rebuilt.high_watermark, 1);
        assert_eq!(rebuilt.lag, 0);
        assert!(rebuilt.stale_reasons.is_empty());
    }

    #[test]
    fn multi_agent_search_reports_per_agent_status_and_aggregate() {
        let dir = tempdir().unwrap();
        let default_storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        default_storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        ensure_agent_home_layout(dir.path()).unwrap();
        rebuild_memory_index(&default_storage, None).unwrap();

        let other_storage = AppStorage::new_for_agent_for_test(dir.path(), "other").unwrap();
        other_storage
            .write_agent_for_fixture(&AgentState::new("other"))
            .unwrap();

        // Single-agent query keeps the compact shape: no per-agent map, and
        // the aggregate is exactly that agent's status.
        let single = search_memory_query_for_agent_storages(
            &default_storage,
            "sentinel",
            10,
            None,
            false,
            &[],
            &[],
            &[],
        )
        .unwrap();
        assert!(single.index_status_by_agent.is_none());
        assert_eq!(single.index_status.freshness, "fresh");

        // Two-agent query: the un-backfilled agent dominates the aggregate
        // and both statuses are reported per agent.
        let multi = search_memory_query_for_agent_storages(
            &default_storage,
            "sentinel",
            10,
            None,
            false,
            &["default".to_string(), "other".to_string()],
            &[],
            &[],
        )
        .unwrap();
        let by_agent = multi.index_status_by_agent.expect("per-agent status map");
        assert_eq!(by_agent.len(), 2);
        assert_eq!(by_agent["default"].freshness, "fresh");
        assert_eq!(by_agent["other"].freshness, "stale");
        assert!(by_agent["other"]
            .stale_reasons
            .contains(&"backfill_incomplete".to_string()));
        assert_eq!(multi.index_status.freshness, "stale");
        assert!(multi
            .index_status
            .stale_reasons
            .contains(&"backfill_incomplete".to_string()));
    }

    #[test]
    fn extra_checkpoint_rows_do_not_hide_missing_required_backfill_kind() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
            .store
            .test_connection()
            .execute(
                "DELETE FROM memory_index_checkpoints
                 WHERE agent_id = ?1 AND source_kind = ?2",
                params!["default", all_backfill_source_kinds()[0]],
            )
            .unwrap();
        index
            .store
            .test_connection()
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

        assert!(!index
            .store
            .has_backfill_checkpoints_for_agent("default")
            .unwrap());
    }

    #[test]
    fn memory_get_returns_latest_task_record_for_task_source_ref() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                call_binding: None,
                invocation_surface: None,
            })
            .unwrap();

        refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                call_binding: None,
                invocation_surface: None,
            })
            .unwrap();

        refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                call_binding: None,
                invocation_surface: None,
            })
            .unwrap();

        let source_ref = "tool_execution:tool-pending-batch-1246:batch_item:2:cmd";
        assert!(MemoryIndex::open(&storage)
            .unwrap()
            .store
            .pending_sources_for_agent("default")
            .unwrap()
            .iter()
            .any(|source| source.source_ref == source_ref));

        refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
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
            .store
            .pending_sources_for_agent("default")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn command_output_resolves_exec_command_batch_envelope_items() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                call_binding: None,
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
    fn command_output_resolves_exec_command_batch_top_level_output() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-batch-top-output-1246".into(),
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
                            "completed_count": 2,
                            "item_count": 2,
                            "items": [
                                {"index": 1, "result": {"stdout_preview": "first_batch_top_output_1246\n"}},
                                {"index": 2, "result": {"stdout_preview": "second_batch_top_output_1246\n"}}
                            ]
                        }
                    },
                    "is_error": false
                }),
                summary: "ExecCommandBatch completed 2/2 items".into(),
                call_binding: None,
                invocation_surface: None,
            })
            .unwrap();

        let memory = get_memory(
            &storage,
            "tool_execution:tool-batch-top-output-1246:output",
            None,
            Some("ws-holon"),
        )
        .unwrap()
        .expect("top-level batch output should be retrievable");

        assert!(memory.content.contains("first_batch_top_output_1246"));
        assert!(memory.content.contains("second_batch_top_output_1246"));
        assert!(memory
            .content
            .contains("\"source_type\": \"tool_execution_output\""));
        assert!(get_memory(
            &storage,
            "tool_execution:tool-batch-top-output-1246:stdout",
            None,
            Some("ws-holon"),
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn command_output_is_retrievable_but_not_search_indexed() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                call_binding: None,
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

        // Consume pending sources before searching. The background memory indexer
        // normally handles this, but tests run without a runtime loop.
        ensure_memory_indexes_current(&storage, Some("ws-holon"), &[]).unwrap();

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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                call_binding: None,
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
        let storage = AppStorage::new_for_agent_for_test(dir.path(), "default").unwrap();
        storage
            .write_agent_for_fixture(&AgentState::new("default"))
            .unwrap();
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
                call_binding: None,
                invocation_surface: None,
            })
            .unwrap();

        let source_ref = "tool_execution:tool-generic-output-1246:output";
        refresh_memory_index_bounded(&storage, Some("ws-holon"), 10).unwrap();
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

    #[test]
    fn maintenance_prune_bounds_seen_candidates_and_resumes_repair() -> Result<()> {
        let directory = tempdir()?;
        let storage = AppStorage::new_for_agent_for_test(directory.path(), "default")?;
        let mut index = MemoryIndex::open(&storage)?;
        let keys = maintenance_test_documents(&index.store, "default", 140)?;
        maintenance_test_documents(&index.store, "default-extra", 3)?;
        let job = maintenance_test_job(&index.store, "default", MEMORY_INDEX_REBUILD_PHASE_PRUNE)?;
        for key in &keys[..139] {
            index.store.test_connection().execute(
                "INSERT INTO memory_index_rebuild_seen VALUES (?1, ?2, ?3)",
                params![job.agent_id, job.generation, key],
            )?;
        }
        index.store.test_connection().execute(
            "DELETE FROM memory_documents_fts_rows WHERE document_key = ?1",
            [&keys[139]],
        )?;
        index.store.test_connection().execute(
            "INSERT INTO memory_documents_fts (document_key, title, body, sanitized_excerpt)
             VALUES (?1, 'duplicate', 'duplicate', 'duplicate')",
            [&keys[139]],
        )?;
        assert_eq!(index.store.prune_rebuild_slice(&job, 3)?, 3);
        assert_eq!(
            index.store.rebuild_job("default")?.unwrap().source_cursor,
            keys[2]
        );
        drop(index);
        let mut index = MemoryIndex::open(&storage)?;
        let resumed = index.store.rebuild_job("default")?.unwrap();
        assert_eq!(index.store.prune_rebuild_slice(&resumed, 3)?, 3);
        assert_eq!(
            index.store.rebuild_job("default")?.unwrap().source_cursor,
            keys[5]
        );
        for _ in 0..256 {
            if !index.advance_rebuild(&storage, 3)? {
                break;
            }
        }
        assert!(index.store.rebuild_job("default")?.is_none());
        for table in [
            "memory_documents",
            "memory_documents_fts",
            "memory_documents_fts_rows",
        ] {
            let count: usize = index.store.test_connection().query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE document_key = ?1"),
                [&keys[139]],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0, "{table} retains stale broken-map document");
        }
        let remaining: usize = index.store.test_connection().query_row(
            "SELECT COUNT(*) FROM memory_documents",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(remaining, 142);
        Ok(())
    }

    #[test]
    fn rebuild_repairs_incomplete_projection_with_matching_source_state() -> Result<()> {
        for missing_part in ["document", "row_map", "fts_row"] {
            let directory = tempdir()?;
            let storage = AppStorage::new_for_agent_for_test(directory.path(), "default")?;
            storage.write_agent_for_fixture(&AgentState::new("default"))?;
            let brief = brief_with_workspace(
                "default",
                BriefKind::Result,
                &format!("missing {missing_part} rebuild sentinel"),
                "ws-holon",
            );
            let source_ref = format!("brief:{}", brief.id);
            storage.append_brief(&brief)?;
            rebuild_memory_index(&storage, Some("ws-holon"))?;

            let index = MemoryIndex::open(&storage)?;
            let document_key = document_key_for("default", &source_ref);
            match missing_part {
                "document" => index.store.test_connection().execute(
                    "DELETE FROM memory_documents WHERE document_key = ?1",
                    [&document_key],
                )?,
                "row_map" => index.store.test_connection().execute(
                    "DELETE FROM memory_documents_fts_rows WHERE document_key = ?1",
                    [&document_key],
                )?,
                "fts_row" => index.store.test_connection().execute(
                    "DELETE FROM memory_documents_fts
                     WHERE rowid = (
                         SELECT fts_rowid FROM memory_documents_fts_rows
                         WHERE document_key = ?1
                     )",
                    [&document_key],
                )?,
                _ => unreachable!(),
            };
            drop(index);

            rebuild_memory_index(&storage, Some("ws-holon"))?;

            let index = MemoryIndex::open(&storage)?;
            for table in [
                "memory_documents",
                "memory_documents_fts",
                "memory_documents_fts_rows",
            ] {
                let count: i64 = index.store.test_connection().query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE document_key = ?1"),
                    [&document_key],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 1, "{table} must recover a missing {missing_part}");
            }
        }
        Ok(())
    }
}
