//! Schema migrations and version tracking.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::types::{WaitConditionRecord, WorkItemRecord};

pub struct Migration {
    pub version: i64,
    pub(crate) name: &'static str,
    pub(crate) sql: &'static str,
}

pub(crate) const MIGRATIONS: &[Migration] = &[
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
  message_seq INTEGER,
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
  transcript_seq INTEGER,
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
CREATE INDEX IF NOT EXISTS idx_messages_seq
  ON messages(message_seq);
CREATE INDEX IF NOT EXISTS idx_messages_task
  ON messages(task_id);
CREATE INDEX IF NOT EXISTS idx_messages_work_item
  ON messages(work_item_id);

CREATE INDEX IF NOT EXISTS idx_transcript_entries_agent_turn
  ON transcript_entries(agent_id, turn_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_message
  ON transcript_entries(message_id);
CREATE INDEX IF NOT EXISTS idx_transcript_entries_seq
  ON transcript_entries(transcript_seq);
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
    Migration {
        version: 6,
        name: "scheduler_control_plane_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS wait_conditions (
  wait_condition_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  work_item_id TEXT,
  status TEXT NOT NULL,
  kind TEXT NOT NULL,
  source TEXT,
  subject_ref TEXT,
  waiting_for TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  expires_at TEXT,
  resolved_at TEXT,
  cancelled_at TEXT,
  last_turn_id TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS queue_entries (
  message_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS timers (
  timer_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  status TEXT NOT NULL,
  summary TEXT,
  created_at TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  interval_ms INTEGER,
  repeat INTEGER NOT NULL DEFAULT 0,
  next_fire_at TEXT,
  last_fired_at TEXT,
  fire_count INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_wait_conditions_agent_status
  ON wait_conditions(agent_id, status);

CREATE INDEX IF NOT EXISTS idx_wait_conditions_work_item_status
  ON wait_conditions(work_item_id, status);

CREATE INDEX IF NOT EXISTS idx_wait_conditions_subject
  ON wait_conditions(kind, subject_ref);

CREATE INDEX IF NOT EXISTS idx_queue_entries_agent_status
  ON queue_entries(agent_id, status, updated_at);

CREATE INDEX IF NOT EXISTS idx_timers_agent_status
  ON timers(agent_id, status, next_fire_at);
"#,
    },
    Migration {
        version: 7,
        name: "queue_entries_preserve_lifecycle_history",
        sql: r#"
CREATE TABLE IF NOT EXISTS queue_entries_v2 (
  message_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (message_id, status)
);

INSERT OR REPLACE INTO queue_entries_v2 (
  message_id, agent_id, priority, status, created_at, updated_at, payload_json
)
SELECT message_id, agent_id, priority, status, created_at, updated_at, payload_json
FROM queue_entries;

DROP TABLE queue_entries;

ALTER TABLE queue_entries_v2 RENAME TO queue_entries;

CREATE INDEX IF NOT EXISTS idx_queue_entries_agent_status
  ON queue_entries(agent_id, status, updated_at);
"#,
    },
    Migration {
        version: 8,
        name: "turn_records_spine",
        sql: r#"
CREATE TABLE IF NOT EXISTS turn_records (
  turn_id TEXT PRIMARY KEY,
  turn_index INTEGER NOT NULL,
  agent_id TEXT NOT NULL,
  run_id TEXT,
  current_work_item_id TEXT,
  trigger_message_id TEXT,
  terminal_kind TEXT,
  created_at TEXT NOT NULL,
  completed_at TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turn_records_agent_recent
  ON turn_records(agent_id, turn_index, created_at);

CREATE INDEX IF NOT EXISTS idx_turn_records_work_item
  ON turn_records(current_work_item_id);
"#,
    },
    Migration {
        version: 9,
        name: "agent_workspace_registry_current_state",
        sql: r#"
CREATE TABLE IF NOT EXISTS agent_states (
  agent_id TEXT PRIMARY KEY,
  status TEXT NOT NULL,
  turn_index INTEGER NOT NULL DEFAULT 0,
  current_run_id TEXT,
  current_work_item_id TEXT,
  active_workspace_id TEXT,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_entries (
  workspace_id TEXT PRIMARY KEY,
  workspace_alias TEXT,
  workspace_kind TEXT,
  owner_agent_id TEXT,
  workspace_anchor TEXT NOT NULL,
  repo_name TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_occupancies (
  occupancy_id TEXT PRIMARY KEY,
  execution_root_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  holder_agent_id TEXT NOT NULL,
  access_mode TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  released_at TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_identities (
  agent_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  visibility TEXT NOT NULL,
  ownership TEXT,
  profile_preset TEXT,
  status TEXT NOT NULL,
  parent_agent_id TEXT,
  lineage_parent_agent_id TEXT,
  delegated_from_task_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_states_status
  ON agent_states(status);

CREATE INDEX IF NOT EXISTS idx_workspace_entries_anchor
  ON workspace_entries(workspace_anchor);

CREATE INDEX IF NOT EXISTS idx_workspace_occupancies_root_active
  ON workspace_occupancies(execution_root_id, released_at);

CREATE INDEX IF NOT EXISTS idx_workspace_occupancies_holder
  ON workspace_occupancies(holder_agent_id);

CREATE INDEX IF NOT EXISTS idx_agent_identities_status
  ON agent_identities(status);
"#,
    },
    Migration {
        version: 10,
        name: "audit_events_agent_seq_index",
        sql: r#"
CREATE INDEX IF NOT EXISTS idx_audit_events_agent_seq_created
  ON audit_events(agent_id, event_seq, created_at);
"#,
    },
    Migration {
        version: 11,
        name: "memory_episode_delegation_domains",
        sql: r#"
CREATE TABLE IF NOT EXISTS work_item_delegations (
  delegation_id TEXT PRIMARY KEY,
  parent_agent_id TEXT NOT NULL,
  parent_work_item_id TEXT NOT NULL,
  child_agent_id TEXT NOT NULL,
  child_work_item_id TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS context_episode_anchors (
  episode_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  work_item_id TEXT,
  boundary_reason TEXT NOT NULL,
  start_turn_index INTEGER NOT NULL,
  end_turn_index INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_work_item_delegations_parent
  ON work_item_delegations(parent_agent_id, parent_work_item_id);
CREATE INDEX IF NOT EXISTS idx_work_item_delegations_child
  ON work_item_delegations(child_agent_id, child_work_item_id);
CREATE INDEX IF NOT EXISTS idx_work_item_delegations_state
  ON work_item_delegations(state);
CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_agent_turn
  ON context_episode_anchors(agent_id, end_turn_index);
CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_work_item
  ON context_episode_anchors(work_item_id);
"#,
    },
    Migration {
        version: 12,
        name: "work_item_continuation_stack",
        sql: r#"
CREATE TABLE IF NOT EXISTS work_item_continuations (
  continuation_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  suspended_work_item_id TEXT NOT NULL,
  active_work_item_id TEXT NOT NULL,
  return_policy TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  resolved_at TEXT,
  cancelled_at TEXT,
  resolution_reason TEXT,
  last_turn_id TEXT,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_work_item_continuations_agent_state
  ON work_item_continuations(agent_id, state);
CREATE INDEX IF NOT EXISTS idx_work_item_continuations_suspended
  ON work_item_continuations(agent_id, suspended_work_item_id, state);
CREATE INDEX IF NOT EXISTS idx_work_item_continuations_active
  ON work_item_continuations(agent_id, active_work_item_id, state);
"#,
    },
    Migration {
        version: 13,
        name: "context_episode_anchors_table",
        sql: r#"
DROP INDEX IF EXISTS idx_context_episodes_agent_turn;
DROP INDEX IF EXISTS idx_context_episodes_work_item;
DROP TABLE IF EXISTS context_episodes;
DELETE FROM storage_domains WHERE domain = 'context_episodes';

CREATE TABLE IF NOT EXISTS context_episode_anchors (
  episode_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  work_item_id TEXT,
  boundary_reason TEXT NOT NULL,
  start_turn_index INTEGER NOT NULL,
  end_turn_index INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_agent_turn
  ON context_episode_anchors(agent_id, end_turn_index);
CREATE INDEX IF NOT EXISTS idx_context_episode_anchors_work_item
  ON context_episode_anchors(work_item_id);
"#,
    },
    Migration {
        version: 14,
        name: "drop_working_memory_deltas",
        sql: r#"
DROP INDEX IF EXISTS idx_working_memory_deltas_revision;
DROP TABLE IF EXISTS working_memory_deltas;
DELETE FROM storage_domains WHERE domain = 'working_memory_deltas';
"#,
    },
    Migration {
        version: 15,
        name: "message_search_index",
        sql: r#"
-- Deprecated before release. HTTP and tool search now use memory v2.
SELECT 1;
"#,
    },
    Migration {
        version: 16,
        name: "wait_conditions_payload_columns",
        sql: r#"
-- Columns are added conditionally in backfill_wait_condition_payload_columns
-- to support test databases that may not have wait_conditions table
SELECT 1;
"#,
    },
    Migration {
        version: 17,
        name: "work_items_recheck_columns",
        sql: r#"
-- Columns are added conditionally in backfill_work_item_recheck_columns
-- to support databases that may already have them
SELECT 1;
"#,
    },
    Migration {
        version: 18,
        name: "queue_entries_current_view",
        sql: r#"
CREATE TABLE IF NOT EXISTS queue_entries (
  message_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (message_id, status)
);

CREATE TABLE IF NOT EXISTS queue_entries_current (
  message_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  priority TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

INSERT OR REPLACE INTO queue_entries_current (
  message_id, agent_id, priority, status, created_at, updated_at, payload_json
)
SELECT q.message_id, q.agent_id, q.priority, q.status, q.created_at, q.updated_at, q.payload_json
FROM queue_entries AS q
WHERE NOT EXISTS (
  SELECT 1
  FROM queue_entries AS newer
  WHERE newer.message_id = q.message_id
    AND (
      newer.updated_at > q.updated_at
      OR (
        newer.updated_at = q.updated_at
        AND newer.rowid > q.rowid
      )
    )
);

DROP TABLE queue_entries;

ALTER TABLE queue_entries_current RENAME TO queue_entries;

CREATE INDEX IF NOT EXISTS idx_queue_entries_agent_status
  ON queue_entries(agent_id, status, updated_at);
"#,
    },
    Migration {
        version: 19,
        name: "runtime_index_outbox",
        sql: r#"
CREATE TABLE IF NOT EXISTS runtime_index_outbox (
  change_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  agent_id TEXT NOT NULL,
  source_kind TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_ref TEXT NOT NULL,
  operation TEXT NOT NULL,
  source_updated_at TEXT,
  reason TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_runtime_index_outbox_agent_seq
  ON runtime_index_outbox(agent_id, change_seq);

CREATE INDEX IF NOT EXISTS idx_runtime_index_outbox_source
  ON runtime_index_outbox(source_kind, source_id);
"#,
    },
    Migration {
        version: 20,
        name: "drop_message_search_index",
        sql: r#"
DROP TABLE IF EXISTS message_search_index;
"#,
    },
    Migration {
        version: 21,
        name: "workspace_id_aliases",
        sql: r#"
CREATE TABLE IF NOT EXISTS workspace_id_aliases (
  old_workspace_id TEXT PRIMARY KEY,
  new_workspace_id TEXT NOT NULL,
  created_at TEXT NOT NULL
);
"#,
    },
];

pub(crate) fn ensure_migration_table(connection: &Connection) -> Result<()> {
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

pub(crate) fn apply_migration(connection: &mut Connection, migration: &Migration) -> Result<()> {
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

/// Backfill `wake_sources_json` and `continuation_json` from `payload_json`
/// for existing rows that still have the default values.
pub(crate) fn backfill_wait_condition_payload_columns(connection: &Connection) -> Result<()> {
    // Check if wait_conditions table exists (may not exist in test databases)
    let table_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'wait_conditions')",
        [],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(());
    }

    // Check if columns exist, add them if they don't
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(wait_conditions)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if !columns.iter().any(|c| c == "wake_sources_json") {
        connection.execute_batch(
            "ALTER TABLE wait_conditions ADD COLUMN wake_sources_json TEXT NOT NULL DEFAULT '[]'",
        )?;
    }
    if !columns.iter().any(|c| c == "continuation_json") {
        connection
            .execute_batch("ALTER TABLE wait_conditions ADD COLUMN continuation_json TEXT")?;
    }

    let needs_backfill: i64 = connection.query_row(
        "SELECT COUNT(*) FROM wait_conditions WHERE wake_sources_json = '[]' AND continuation_json IS NULL",
        [],
        |row| row.get(0),
    )?;
    if needs_backfill == 0 {
        return Ok(());
    }
    let mut stmt = connection.prepare(
        "SELECT wait_condition_id, payload_json FROM wait_conditions WHERE wake_sources_json = '[]' AND continuation_json IS NULL",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut updates = 0usize;
    for (id, payload) in rows {
        let record: WaitConditionRecord = serde_json::from_str(&payload)
            .context("decoding wait condition payload for backfill")?;
        let wake_sources_json = serde_json::to_string(&record.wake_sources)?;
        let continuation_json = record
            .continuation
            .as_ref()
            .map(|v| serde_json::to_string(v))
            .transpose()?;
        connection.execute(
            "UPDATE wait_conditions SET wake_sources_json = ?1, continuation_json = ?2 WHERE wait_condition_id = ?3",
            params![wake_sources_json, continuation_json, id],
        )?;
        updates += 1;
    }
    tracing::info!(
        updates,
        "backfilled wait_conditions wake_sources_json/continuation_json"
    );
    Ok(())
}

pub(crate) fn backfill_work_item_recheck_columns(connection: &Connection) -> Result<()> {
    // Check if work_items table exists (may not exist in test databases)
    let table_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'work_items')",
        [],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(());
    }

    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(work_items)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut added = false;
    if !columns.iter().any(|c| c == "blocked_by") {
        connection.execute_batch("ALTER TABLE work_items ADD COLUMN blocked_by TEXT")?;
        added = true;
    }
    if !columns.iter().any(|c| c == "recheck_at") {
        connection.execute_batch("ALTER TABLE work_items ADD COLUMN recheck_at TEXT")?;
        added = true;
    }
    if !columns.iter().any(|c| c == "recheck_consumed_at") {
        connection.execute_batch("ALTER TABLE work_items ADD COLUMN recheck_consumed_at TEXT")?;
        added = true;
    }
    if added {
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_work_items_recheck \
             ON work_items(agent_id, state, blocked_by, recheck_at)",
        )?;
    }

    let needs_backfill: i64 = connection.query_row(
        "SELECT COUNT(*) FROM work_items WHERE blocked_by IS NULL AND recheck_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    if needs_backfill == 0 {
        return Ok(());
    }
    let mut stmt = connection.prepare(
        "SELECT work_item_id, payload_json FROM work_items WHERE blocked_by IS NULL AND recheck_at IS NULL",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut updates = 0usize;
    for (id, payload) in rows {
        let record: WorkItemRecord = serde_json::from_str(&payload)
            .context("decoding work item payload for recheck column backfill")?;
        let blocked_by = record.blocked_by.clone();
        let recheck_at = record.recheck_at.map(|t| timestamp(t));
        let recheck_consumed_at = record.recheck_consumed_at.map(|t| timestamp(t));
        // Only update if there is actually a value to set
        if blocked_by.is_some() || recheck_at.is_some() || recheck_consumed_at.is_some() {
            connection.execute(
                "UPDATE work_items SET blocked_by = ?1, recheck_at = ?2, recheck_consumed_at = ?3 \
                 WHERE work_item_id = ?4",
                params![blocked_by, recheck_at, recheck_consumed_at, id],
            )?;
            updates += 1;
        }
    }
    if updates > 0 {
        tracing::info!(
            updates,
            "backfilled work_items blocked_by/recheck_at/recheck_consumed_at"
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn table_exists(connection: &Connection, table_name: &str) -> Result<bool> {
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table_name],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(exists)
}

pub(crate) fn current_schema_version(connection: &Connection) -> Result<i64> {
    ensure_migration_table(connection)?;
    let version = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}

pub(crate) fn max_known_migration_version() -> i64 {
    MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

pub(crate) fn timestamp(value: chrono::DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
