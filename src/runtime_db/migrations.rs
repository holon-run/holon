//! Schema migrations and version tracking.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::runtime_db::evidence::content_hash;
use crate::types::{AgentState, WaitConditionRecord, WorkItemRecord};

pub struct Migration {
    pub version: i64,
    pub(crate) name: &'static str,
    pub(crate) sql: &'static str,
}

fn preflight_work_item_focus(connection: &Connection) -> Result<()> {
    let invalid_agent_focus = connection
        .query_row(
            "SELECT a.agent_id, a.current_work_item_id,
                    COALESCE(w.agent_id, '<missing>'), COALESCE(w.state, '<missing>')
             FROM agent_states a
             LEFT JOIN work_items w ON w.work_item_id = a.current_work_item_id
             WHERE a.current_work_item_id IS NOT NULL
               AND (
                 w.work_item_id IS NULL
                 OR w.agent_id != a.agent_id
                 OR w.state != 'open'
               )
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((agent_id, work_item_id, owner_agent_id, state)) = invalid_agent_focus {
        bail!(
            "work item focus migration found invalid canonical focus: agent_id={agent_id}, work_item_id={work_item_id}, owner_agent_id={owner_agent_id}, state={state}"
        );
    }

    let invalid_legacy_focus = connection
        .query_row(
            "SELECT work_item_id, agent_id, state
             FROM work_items
             WHERE current_focus != 0 AND state != 'open'
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((work_item_id, agent_id, state)) = invalid_legacy_focus {
        bail!(
            "work item focus migration found invalid legacy focus: agent_id={agent_id}, work_item_id={work_item_id}, state={state}"
        );
    }

    let duplicate_legacy_focus = connection
        .query_row(
            "SELECT agent_id, COUNT(*), GROUP_CONCAT(work_item_id, ',')
             FROM work_items
             WHERE current_focus != 0
             GROUP BY agent_id
             HAVING COUNT(*) > 1
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((agent_id, count, work_item_ids)) = duplicate_legacy_focus {
        bail!(
            "work item focus migration found multiple legacy focuses: agent_id={agent_id}, count={count}, work_item_ids={work_item_ids}"
        );
    }

    let conflicting_focus = connection
        .query_row(
            "SELECT a.agent_id, a.current_work_item_id, w.work_item_id
             FROM agent_states a
             JOIN work_items w
               ON w.agent_id = a.agent_id
              AND w.current_focus != 0
             WHERE a.current_work_item_id IS NOT NULL
               AND a.current_work_item_id != w.work_item_id
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((agent_id, canonical_id, legacy_id)) = conflicting_focus {
        bail!(
            "work item focus migration found conflicting focus facts: agent_id={agent_id}, canonical_work_item_id={canonical_id}, legacy_work_item_id={legacy_id}"
        );
    }

    let orphaned_legacy_focus = connection
        .query_row(
            "SELECT w.agent_id, w.work_item_id
             FROM work_items w
             LEFT JOIN agent_states a ON a.agent_id = w.agent_id
             WHERE w.current_focus != 0 AND a.agent_id IS NULL
             LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((agent_id, work_item_id)) = orphaned_legacy_focus {
        bail!(
            "work item focus migration cannot backfill legacy focus without agent state: agent_id={agent_id}, work_item_id={work_item_id}"
        );
    }
    Ok(())
}

fn migrate_work_item_focus(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT a.agent_id, a.current_work_item_id, a.payload_json,
                (
                  SELECT w.work_item_id
                  FROM work_items w
                  WHERE w.agent_id = a.agent_id AND w.current_focus != 0
                  LIMIT 1
                )
         FROM agent_states a",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);

    for (agent_id, canonical_id, payload_json, legacy_id) in rows {
        let selected_id = canonical_id.or(legacy_id);
        let mut state: AgentState = serde_json::from_str(&payload_json)
            .with_context(|| format!("decoding agent state {agent_id} for focus migration"))?;
        if state.id != agent_id {
            bail!(
                "work item focus migration found agent payload identity mismatch: row_agent_id={agent_id}, payload_agent_id={}",
                state.id
            );
        }
        if state.current_work_item_id != selected_id {
            state.current_work_item_id = selected_id.clone();
            connection.execute(
                "UPDATE agent_states
                 SET current_work_item_id = ?1, payload_json = ?2
                 WHERE agent_id = ?3",
                params![selected_id, serde_json::to_string(&state)?, agent_id],
            )?;
        }
    }
    connection.execute("UPDATE work_items SET current_focus = 0", [])?;
    Ok(())
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
    Migration {
        version: 22,
        name: "operator_domains",
        sql: r#"
CREATE TABLE IF NOT EXISTS operator_notifications (
  notification_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (notification_id, agent_id)
);

CREATE INDEX IF NOT EXISTS idx_operator_notifications_agent_created
  ON operator_notifications(agent_id, created_at DESC);

CREATE TABLE IF NOT EXISTS operator_transport_bindings (
  binding_id TEXT PRIMARY KEY,
  target_agent_id TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_operator_transport_bindings_agent_status
  ON operator_transport_bindings(target_agent_id, status);

CREATE TABLE IF NOT EXISTS operator_delivery_records (
  delivery_intent_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_operator_delivery_records_agent_created
  ON operator_delivery_records(agent_id, created_at DESC);
"#,
    },
    Migration {
        version: 23,
        name: "agent_template_remote_source_syncs",
        sql: r#"
CREATE TABLE IF NOT EXISTS agent_template_remote_source_syncs (
  source_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  url TEXT NOT NULL,
  requested_ref TEXT,
  enabled INTEGER NOT NULL,
  status TEXT NOT NULL,
  last_synced_at TEXT,
  resolved_ref TEXT,
  resolved_revision TEXT,
  catalog_json TEXT NOT NULL,
  diagnostics_json TEXT NOT NULL,
  error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_template_remote_source_syncs_status
  ON agent_template_remote_source_syncs(status);
"#,
    },
    Migration {
        version: 24,
        name: "execution_root_entries",
        sql: r#"
CREATE TABLE IF NOT EXISTS execution_root_entries (
  execution_root_id TEXT PRIMARY KEY,
  workspace_id       TEXT NOT NULL,
  filesystem_path    TEXT NOT NULL,
  root_kind          TEXT NOT NULL,
  created_at         TEXT NOT NULL,
  removed_at         TEXT,
  payload_json       TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_execution_root_entries_workspace
  ON execution_root_entries(workspace_id);
"#,
    },
    Migration {
        version: 25,
        name: "drop_workspace_id_aliases",
        sql: r#"
DROP TABLE IF EXISTS workspace_id_aliases;
"#,
    },
    Migration {
        version: 26,
        name: "strict_runtime_sequences",
        sql: r#"
CREATE TABLE IF NOT EXISTS runtime_sequences (
  domain TEXT NOT NULL,
  scope_key TEXT NOT NULL,
  last_value INTEGER NOT NULL,
  PRIMARY KEY (domain, scope_key)
);
"#,
    },
    Migration {
        version: 27,
        name: "canonical_work_item_focus",
        sql: r#"
CREATE INDEX IF NOT EXISTS idx_agent_states_current_work_item
  ON agent_states(current_work_item_id);

DROP INDEX IF EXISTS idx_work_items_current_focus;

CREATE TRIGGER IF NOT EXISTS trg_agent_states_focus_insert
BEFORE INSERT ON agent_states
WHEN NEW.current_work_item_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM work_items
    WHERE work_item_id = NEW.current_work_item_id
      AND agent_id = NEW.agent_id
      AND state = 'open'
  ) THEN RAISE(ABORT, 'current WorkItem focus must reference an owned open WorkItem') END;
END;

CREATE TRIGGER IF NOT EXISTS trg_agent_states_focus_update
BEFORE UPDATE OF current_work_item_id ON agent_states
WHEN NEW.current_work_item_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM work_items
    WHERE work_item_id = NEW.current_work_item_id
      AND agent_id = NEW.agent_id
      AND state = 'open'
  ) THEN RAISE(ABORT, 'current WorkItem focus must reference an owned open WorkItem') END;
END;

CREATE TRIGGER IF NOT EXISTS trg_work_items_preserve_focused_target
BEFORE UPDATE OF agent_id, state ON work_items
WHEN EXISTS (
  SELECT 1
  FROM agent_states
  WHERE current_work_item_id = OLD.work_item_id
    AND (agent_id != NEW.agent_id OR NEW.state != 'open')
)
BEGIN
  SELECT RAISE(ABORT, 'focused WorkItem must remain owned and open');
END;

CREATE TRIGGER IF NOT EXISTS trg_work_items_preserve_focused_delete
BEFORE DELETE ON work_items
WHEN EXISTS (
  SELECT 1
  FROM agent_states
  WHERE current_work_item_id = OLD.work_item_id
)
BEGIN
  SELECT RAISE(ABORT, 'focused WorkItem cannot be deleted');
END;
"#,
    },
    Migration {
        version: 28,
        name: "drop_work_item_readiness",
        sql: r#"
DROP INDEX IF EXISTS idx_work_items_readiness;
"#,
    },
    Migration {
        version: 29,
        name: "runtime_metadata",
        sql: r#"
CREATE TABLE IF NOT EXISTS runtime_metadata (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
"#,
    },
    Migration {
        version: 30,
        name: "runtime_retention_created_at_indexes",
        sql: "",
    },
    Migration {
        version: 31,
        name: "scheduler_protocol_canonical_facts",
        sql: r#"
CREATE TABLE IF NOT EXISTS scheduler_work_demands (
  agent_id TEXT NOT NULL,
  work_item_id TEXT NOT NULL,
  metadata_revision INTEGER NOT NULL CHECK (metadata_revision >= 0),
  scheduling_generation INTEGER NOT NULL CHECK (scheduling_generation >= 0),
  status TEXT NOT NULL CHECK (
    status IN ('runnable', 'waiting', 'needs_settlement', 'paused', 'terminal')
  ),
  status_reference_id TEXT,
  capabilities_json TEXT NOT NULL,
  locks_json TEXT NOT NULL,
  locality TEXT NOT NULL,
  cost_class TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, work_item_id),
  CHECK (
    (status IN ('runnable', 'terminal') AND status_reference_id IS NULL)
    OR (status IN ('waiting', 'needs_settlement', 'paused') AND status_reference_id IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS scheduler_activation_authorities (
  agent_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  activation_id TEXT NOT NULL,
  work_item_id TEXT NOT NULL,
  expected_scheduling_generation INTEGER NOT NULL CHECK (expected_scheduling_generation >= 0),
  expected_dispatch_revision INTEGER NOT NULL CHECK (expected_dispatch_revision >= 0),
  consumed_by_activation_id TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, authority_id),
  UNIQUE (agent_id, activation_id),
  UNIQUE (
    agent_id,
    authority_id,
    activation_id,
    work_item_id,
    expected_scheduling_generation
  ),
  FOREIGN KEY (agent_id, work_item_id)
    REFERENCES scheduler_work_demands(agent_id, work_item_id),
  FOREIGN KEY (agent_id, consumed_by_activation_id)
    REFERENCES scheduler_activations(agent_id, activation_id),
  CHECK (
    consumed_by_activation_id IS NULL
    OR consumed_by_activation_id = activation_id
  )
);

CREATE TABLE IF NOT EXISTS scheduler_activations (
  agent_id TEXT NOT NULL,
  activation_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  work_item_id TEXT NOT NULL,
  admitted_generation INTEGER NOT NULL CHECK (admitted_generation >= 0),
  admission_kind TEXT NOT NULL CHECK (
    admission_kind IN ('scheduling', 'wait_resume', 'settlement_recovery')
  ),
  recovery_for_activation_id TEXT,
  wait_id TEXT,
  wait_generation INTEGER CHECK (wait_generation IS NULL OR wait_generation >= 0),
  lifecycle_state TEXT NOT NULL CHECK (
    lifecycle_state IN (
      'admitted',
      'running',
      'settled',
      'interrupted',
      'cancelled',
      'settlement_missing'
    )
  ),
  idempotency_key TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, activation_id),
  UNIQUE (agent_id, authority_id),
  UNIQUE (agent_id, idempotency_key),
  UNIQUE (agent_id, activation_id, work_item_id, admitted_generation),
  FOREIGN KEY (
    agent_id,
    authority_id,
    activation_id,
    work_item_id,
    admitted_generation
  ) REFERENCES scheduler_activation_authorities(
    agent_id,
    authority_id,
    activation_id,
    work_item_id,
    expected_scheduling_generation
  ),
  FOREIGN KEY (agent_id, work_item_id)
    REFERENCES scheduler_work_demands(agent_id, work_item_id),
  FOREIGN KEY (agent_id, recovery_for_activation_id)
    REFERENCES scheduler_activations(agent_id, activation_id),
  FOREIGN KEY (agent_id, wait_id, wait_generation)
    REFERENCES scheduler_wait_generations(agent_id, wait_id, generation),
  CHECK (
    (admission_kind = 'scheduling'
      AND recovery_for_activation_id IS NULL
      AND wait_id IS NULL
      AND wait_generation IS NULL)
    OR (admission_kind = 'wait_resume'
      AND recovery_for_activation_id IS NULL
      AND wait_id IS NOT NULL
      AND wait_generation IS NOT NULL)
    OR (admission_kind = 'settlement_recovery'
      AND recovery_for_activation_id IS NOT NULL
      AND wait_id IS NULL
      AND wait_generation IS NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_scheduler_activations_ordinary_admission_fence
  ON scheduler_activations(agent_id, work_item_id, admitted_generation)
  WHERE admission_kind IN ('scheduling', 'wait_resume');

CREATE UNIQUE INDEX IF NOT EXISTS idx_scheduler_activations_recovery_admission_fence
  ON scheduler_activations(
    agent_id,
    work_item_id,
    admitted_generation,
    recovery_for_activation_id
  )
  WHERE admission_kind = 'settlement_recovery';

CREATE TABLE IF NOT EXISTS scheduler_waits (
  agent_id TEXT NOT NULL,
  wait_id TEXT NOT NULL,
  owner_work_item_id TEXT NOT NULL,
  current_generation INTEGER NOT NULL CHECK (current_generation >= 0),
  payload_json TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, wait_id),
  FOREIGN KEY (agent_id, owner_work_item_id)
    REFERENCES scheduler_work_demands(agent_id, work_item_id)
);

CREATE TABLE IF NOT EXISTS scheduler_wait_generations (
  agent_id TEXT NOT NULL,
  wait_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation >= 0),
  owner_work_item_id TEXT NOT NULL,
  lifecycle_state TEXT NOT NULL CHECK (
    lifecycle_state IN ('active', 'triggered', 'consumed', 'resolved')
  ),
  trigger_id TEXT,
  trigger_generation INTEGER CHECK (trigger_generation IS NULL OR trigger_generation >= 0),
  consuming_activation_id TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, wait_id, generation),
  FOREIGN KEY (agent_id, wait_id)
    REFERENCES scheduler_waits(agent_id, wait_id),
  FOREIGN KEY (agent_id, owner_work_item_id)
    REFERENCES scheduler_work_demands(agent_id, work_item_id),
  FOREIGN KEY (agent_id, consuming_activation_id)
    REFERENCES scheduler_activations(agent_id, activation_id),
  CHECK (
    (trigger_id IS NULL AND trigger_generation IS NULL)
    OR (trigger_id IS NOT NULL AND trigger_generation IS NOT NULL)
  ),
  CHECK (
    (lifecycle_state IN ('active', 'triggered', 'resolved')
      AND consuming_activation_id IS NULL)
    OR (lifecycle_state = 'consumed' AND consuming_activation_id IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS scheduler_agent_slots (
  agent_id TEXT PRIMARY KEY,
  slot_kind TEXT NOT NULL CHECK (slot_kind IN ('idle', 'running')),
  activation_id TEXT,
  work_item_id TEXT,
  admitted_generation INTEGER CHECK (admitted_generation IS NULL OR admitted_generation >= 0),
  recovery_for_activation_id TEXT,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (agent_id, activation_id, work_item_id, admitted_generation)
    REFERENCES scheduler_activations(
      agent_id,
      activation_id,
      work_item_id,
      admitted_generation
    ),
  FOREIGN KEY (agent_id, recovery_for_activation_id)
    REFERENCES scheduler_activations(agent_id, activation_id),
  CHECK (
    (slot_kind = 'idle'
      AND activation_id IS NULL
      AND work_item_id IS NULL
      AND admitted_generation IS NULL
      AND recovery_for_activation_id IS NULL)
    OR (slot_kind = 'running'
      AND activation_id IS NOT NULL
      AND work_item_id IS NOT NULL
      AND admitted_generation IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS scheduler_agent_dispatch (
  agent_id TEXT PRIMARY KEY,
  dispatch_kind TEXT NOT NULL CHECK (dispatch_kind IN ('open', 'awaiting')),
  wait_id TEXT,
  wait_generation INTEGER CHECK (wait_generation IS NULL OR wait_generation >= 0),
  dispatch_revision INTEGER NOT NULL CHECK (dispatch_revision >= 0),
  updated_at TEXT NOT NULL,
  FOREIGN KEY (agent_id, wait_id, wait_generation)
    REFERENCES scheduler_wait_generations(agent_id, wait_id, generation),
  CHECK (
    (dispatch_kind = 'open' AND wait_id IS NULL AND wait_generation IS NULL)
    OR (dispatch_kind = 'awaiting' AND wait_id IS NOT NULL AND wait_generation IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS scheduler_agent_focus (
  agent_id TEXT PRIMARY KEY,
  focused_work_item_id TEXT,
  focus_revision INTEGER NOT NULL CHECK (focus_revision >= 0),
  updated_at TEXT NOT NULL,
  FOREIGN KEY (agent_id, focused_work_item_id)
    REFERENCES scheduler_work_demands(agent_id, work_item_id)
);

CREATE TRIGGER IF NOT EXISTS trg_scheduler_agent_focus_insert
BEFORE INSERT ON scheduler_agent_focus
WHEN NEW.focused_work_item_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM scheduler_work_demands
    WHERE agent_id = NEW.agent_id
      AND work_item_id = NEW.focused_work_item_id
      AND status != 'terminal'
  ) THEN RAISE(ABORT, 'scheduler focus must reference an owned open WorkItem demand') END;
END;

CREATE TRIGGER IF NOT EXISTS trg_scheduler_agent_focus_update
BEFORE UPDATE OF focused_work_item_id ON scheduler_agent_focus
WHEN NEW.focused_work_item_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM scheduler_work_demands
    WHERE agent_id = NEW.agent_id
      AND work_item_id = NEW.focused_work_item_id
      AND status != 'terminal'
  ) THEN RAISE(ABORT, 'scheduler focus must reference an owned open WorkItem demand') END;
END;

CREATE TRIGGER IF NOT EXISTS trg_scheduler_work_demands_preserve_focus
BEFORE UPDATE OF status ON scheduler_work_demands
WHEN NEW.status = 'terminal' AND EXISTS (
  SELECT 1
  FROM scheduler_agent_focus
  WHERE agent_id = OLD.agent_id
    AND focused_work_item_id = OLD.work_item_id
)
BEGIN
  SELECT RAISE(ABORT, 'focused scheduler WorkItem demand must remain open');
END;

CREATE TRIGGER IF NOT EXISTS trg_scheduler_work_demands_preserve_focus_delete
BEFORE DELETE ON scheduler_work_demands
WHEN EXISTS (
  SELECT 1
  FROM scheduler_agent_focus
  WHERE agent_id = OLD.agent_id
    AND focused_work_item_id = OLD.work_item_id
)
BEGIN
  SELECT RAISE(ABORT, 'focused scheduler WorkItem demand cannot be deleted');
END;

CREATE TABLE IF NOT EXISTS scheduler_activation_settlements (
  agent_id TEXT NOT NULL,
  settlement_id TEXT NOT NULL,
  activation_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, settlement_id),
  UNIQUE (agent_id, activation_id),
  FOREIGN KEY (agent_id, activation_id)
    REFERENCES scheduler_activations(agent_id, activation_id)
);

CREATE TABLE IF NOT EXISTS scheduler_missing_settlements (
  agent_id TEXT NOT NULL,
  missing_settlement_id TEXT NOT NULL,
  activation_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, missing_settlement_id),
  UNIQUE (agent_id, activation_id),
  FOREIGN KEY (agent_id, activation_id)
    REFERENCES scheduler_activations(agent_id, activation_id)
);

CREATE TABLE IF NOT EXISTS scheduler_continuation_admissions (
  agent_id TEXT NOT NULL,
  admission_id TEXT NOT NULL,
  settlement_id TEXT NOT NULL,
  completed_work_item_id TEXT NOT NULL,
  caller_work_item_id TEXT NOT NULL,
  expected_caller_generation INTEGER NOT NULL CHECK (expected_caller_generation >= 0),
  admitted_caller_generation INTEGER NOT NULL CHECK (admitted_caller_generation >= 0),
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, admission_id),
  UNIQUE (agent_id, settlement_id),
  FOREIGN KEY (agent_id, settlement_id)
    REFERENCES scheduler_activation_settlements(agent_id, settlement_id),
  FOREIGN KEY (agent_id, completed_work_item_id)
    REFERENCES scheduler_work_demands(agent_id, work_item_id),
  FOREIGN KEY (agent_id, caller_work_item_id)
    REFERENCES scheduler_work_demands(agent_id, work_item_id)
);

CREATE TABLE IF NOT EXISTS scheduler_protocol_command_results (
  agent_id TEXT NOT NULL,
  command_kind TEXT NOT NULL,
  command_identity TEXT NOT NULL,
  canonical_schema_version INTEGER NOT NULL CHECK (canonical_schema_version > 0),
  payload_hash TEXT NOT NULL,
  decision TEXT NOT NULL,
  conflict_kind TEXT,
  conflict_code TEXT,
  result_references_json TEXT NOT NULL,
  pre_state_fence_json TEXT NOT NULL,
  post_state_fence_json TEXT NOT NULL,
  outcome_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, command_kind, command_identity),
  CHECK (
    (conflict_kind IS NULL AND conflict_code IS NULL)
    OR (conflict_kind IS NOT NULL AND conflict_code IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS scheduler_protocol_command_conflict_attempts (
  conflict_attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
  partition_kind TEXT NOT NULL CHECK (
    partition_kind IN ('agent', 'global_rollout')
  ),
  partition_key TEXT NOT NULL,
  command_kind TEXT NOT NULL,
  command_identity TEXT NOT NULL,
  canonical_schema_version INTEGER NOT NULL CHECK (canonical_schema_version > 0),
  existing_payload_hash TEXT NOT NULL,
  incoming_payload_hash TEXT NOT NULL,
  conflict_kind TEXT NOT NULL,
  conflict_code TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scheduler_protocol_migrations (
  agent_id TEXT NOT NULL,
  migration_identity TEXT NOT NULL,
  migration_version INTEGER NOT NULL CHECK (migration_version > 0),
  source_kind TEXT NOT NULL,
  source_id TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  decision TEXT NOT NULL,
  rejection_kind TEXT,
  rejection_code TEXT,
  imported_fact_references_json TEXT NOT NULL,
  outcome_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, migration_identity),
  UNIQUE (agent_id, source_kind, source_id),
  CHECK (
    (rejection_kind IS NULL AND rejection_code IS NULL)
    OR (rejection_kind IS NOT NULL AND rejection_code IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS scheduler_protocol_config (
  config_id INTEGER PRIMARY KEY CHECK (config_id = 1),
  protocol_mode TEXT NOT NULL CHECK (
    protocol_mode IN ('legacy', 'shadow', 'authoritative')
  ),
  config_revision INTEGER NOT NULL CHECK (config_revision >= 0),
  latest_preflight_revision INTEGER NOT NULL CHECK (latest_preflight_revision >= 0),
  updated_at TEXT NOT NULL
);

INSERT OR IGNORE INTO scheduler_protocol_config (
  config_id, protocol_mode, config_revision, latest_preflight_revision, updated_at
) VALUES (1, 'legacy', 0, 0, CURRENT_TIMESTAMP);

CREATE TABLE IF NOT EXISTS scheduler_rollout_command_results (
  command_kind TEXT NOT NULL,
  command_identity TEXT NOT NULL,
  canonical_schema_version INTEGER NOT NULL CHECK (canonical_schema_version > 0),
  payload_hash TEXT NOT NULL,
  decision TEXT NOT NULL,
  conflict_kind TEXT,
  conflict_code TEXT,
  result_references_json TEXT NOT NULL,
  pre_state_fence_json TEXT NOT NULL,
  post_state_fence_json TEXT NOT NULL,
  outcome_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (command_kind, command_identity),
  CHECK (
    (conflict_kind IS NULL AND conflict_code IS NULL)
    OR (conflict_kind IS NOT NULL AND conflict_code IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS scheduler_rollout_preflights (
  preflight_revision INTEGER PRIMARY KEY CHECK (preflight_revision >= 0),
  manifest_revision INTEGER NOT NULL CHECK (manifest_revision >= 0),
  state TEXT NOT NULL CHECK (state IN ('open', 'completed', 'consumed')),
  manifest_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scheduler_rollout_manifests (
  manifest_revision INTEGER PRIMARY KEY CHECK (manifest_revision >= 0),
  preflight_revision INTEGER NOT NULL,
  payload_json TEXT NOT NULL,
  installed_at TEXT NOT NULL,
  FOREIGN KEY (preflight_revision)
    REFERENCES scheduler_rollout_preflights(preflight_revision)
);

CREATE TABLE IF NOT EXISTS scheduler_scenario_authorities (
  scenario_class TEXT PRIMARY KEY,
  mode TEXT NOT NULL CHECK (mode IN ('off', 'shadow', 'authoritative')),
  rollback_target TEXT NOT NULL CHECK (rollback_target IN ('off', 'shadow')),
  manifest_revision INTEGER,
  preflight_revision INTEGER,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (manifest_revision)
    REFERENCES scheduler_rollout_manifests(manifest_revision),
  FOREIGN KEY (preflight_revision)
    REFERENCES scheduler_rollout_preflights(preflight_revision)
);

CREATE TABLE IF NOT EXISTS scheduler_scenario_hard_blockers (
  scenario_class TEXT NOT NULL,
  blocker_code TEXT NOT NULL,
  config_revision INTEGER NOT NULL CHECK (config_revision >= 0),
  manifest_revision INTEGER NOT NULL,
  preflight_revision INTEGER NOT NULL,
  trigger_kind TEXT NOT NULL,
  action_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (
    scenario_class,
    blocker_code,
    config_revision,
    manifest_revision,
    preflight_revision
  ),
  FOREIGN KEY (manifest_revision)
    REFERENCES scheduler_rollout_manifests(manifest_revision),
  FOREIGN KEY (preflight_revision)
    REFERENCES scheduler_rollout_preflights(preflight_revision)
);

CREATE INDEX IF NOT EXISTS idx_scheduler_work_demands_status
  ON scheduler_work_demands(agent_id, status, scheduling_generation);

CREATE INDEX IF NOT EXISTS idx_scheduler_wait_generations_state
  ON scheduler_wait_generations(agent_id, lifecycle_state);

CREATE INDEX IF NOT EXISTS idx_scheduler_activations_state
  ON scheduler_activations(agent_id, lifecycle_state);

CREATE INDEX IF NOT EXISTS idx_scheduler_command_results_created
  ON scheduler_protocol_command_results(agent_id, created_at);

CREATE INDEX IF NOT EXISTS idx_scheduler_command_conflict_attempts_identity
  ON scheduler_protocol_command_conflict_attempts(
    partition_kind,
    partition_key,
    command_kind,
    command_identity,
    conflict_attempt_id
  );

CREATE INDEX IF NOT EXISTS idx_scheduler_rollout_command_results_created
  ON scheduler_rollout_command_results(created_at);

CREATE INDEX IF NOT EXISTS idx_scheduler_migrations_created
  ON scheduler_protocol_migrations(agent_id, created_at);
"#,
    },
    Migration {
        version: 32,
        name: "scheduler_shadow_comparisons",
        sql: r#"
CREATE TABLE IF NOT EXISTS scheduler_shadow_comparisons (
  agent_id TEXT NOT NULL,
  scenario_class TEXT NOT NULL,
  comparison_identity TEXT NOT NULL,
  canonical_schema_version INTEGER NOT NULL CHECK (canonical_schema_version > 0),
  payload_hash TEXT NOT NULL,
  boundary TEXT NOT NULL,
  input_identity TEXT NOT NULL,
  authority_mode TEXT NOT NULL CHECK (
    authority_mode IN ('shadow', 'authoritative')
  ),
  legacy_observation_json TEXT NOT NULL,
  shadow_candidate_json TEXT NOT NULL,
  comparison_outcome TEXT NOT NULL CHECK (
    comparison_outcome IN ('matched', 'diverged')
  ),
  divergence_code TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, scenario_class, comparison_identity),
  CHECK (
    (comparison_outcome = 'matched' AND divergence_code IS NULL)
    OR (comparison_outcome = 'diverged' AND divergence_code IS NOT NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_scheduler_shadow_comparisons_scenario
  ON scheduler_shadow_comparisons(
    scenario_class,
    authority_mode,
    created_at
  );
"#,
    },
    Migration {
        version: 33,
        name: "scheduler_semantic_shadow_decisions",
        sql: r#"
CREATE TABLE IF NOT EXISTS scheduler_semantic_shadow_decisions (
  agent_id TEXT NOT NULL,
  source_id TEXT NOT NULL,
  contract_revision INTEGER NOT NULL CHECK (contract_revision > 0),
  payload_hash TEXT NOT NULL,
  authority_mode TEXT NOT NULL CHECK (authority_mode = 'shadow'),
  input_json TEXT NOT NULL,
  provider_json TEXT NOT NULL,
  response_json TEXT NOT NULL,
  policy_json TEXT NOT NULL,
  resolution_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, source_id)
);

CREATE INDEX IF NOT EXISTS idx_scheduler_semantic_shadow_decisions_created
  ON scheduler_semantic_shadow_decisions(agent_id, created_at);
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
    if migration.name == "strict_runtime_sequences" {
        repair_runtime_sequence_duplicates(&transaction)?;
    }
    if migration.name == "canonical_work_item_focus" {
        preflight_work_item_focus(&transaction)?;
        migrate_work_item_focus(&transaction)?;
    }
    transaction.execute_batch(migration.sql)?;
    if migration.name == "drop_work_item_readiness" {
        drop_work_item_readiness(&transaction)?;
    }
    if migration.name == "strict_runtime_sequences" {
        migrate_runtime_sequences(&transaction)?;
    }
    if migration.name == "runtime_retention_created_at_indexes" {
        migrate_runtime_retention_created_at_indexes(&transaction)?;
    }
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

fn migrate_runtime_retention_created_at_indexes(connection: &Connection) -> Result<()> {
    if table_exists_internal(connection, "transcript_entries")? {
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_transcript_entries_created
               ON transcript_entries(created_at, evidence_id);",
        )?;
    }
    if table_exists_internal(connection, "tool_executions")? {
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tool_executions_created
               ON tool_executions(created_at, evidence_id);",
        )?;
    }
    Ok(())
}

fn drop_work_item_readiness(connection: &Connection) -> Result<()> {
    if !table_exists_internal(connection, "work_items")? {
        return Ok(());
    }
    let columns = connection
        .prepare("PRAGMA table_info(work_items)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if columns.iter().any(|column| column == "readiness") {
        connection.execute_batch("ALTER TABLE work_items DROP COLUMN readiness;")?;
    }
    Ok(())
}

fn repair_runtime_sequence_duplicates(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "DROP INDEX IF EXISTS idx_audit_events_agent_event_seq_unique;
         DROP INDEX IF EXISTS idx_audit_events_host_event_seq_unique;
         DROP INDEX IF EXISTS idx_messages_agent_message_seq_unique;
         DROP INDEX IF EXISTS idx_transcript_entries_agent_transcript_seq_unique;",
    )?;
    if table_exists_internal(connection, "audit_events")? {
        let audit_resequenced = resequence_duplicate_scopes(
            connection,
            "audit_events",
            "CASE WHEN agent_id IS NULL THEN 'host' ELSE 'agent:' || agent_id END",
            "event_seq",
            "audit_event_id",
            "data_json",
            false,
        )?;
        if audit_resequenced {
            rotate_audit_event_epoch(connection)?;
        }
    }
    if table_exists_internal(connection, "messages")? {
        resequence_duplicate_scopes(
            connection,
            "messages",
            "'agent:' || agent_id",
            "message_seq",
            "evidence_id",
            "payload_json",
            true,
        )?;
    }
    if table_exists_internal(connection, "transcript_entries")? {
        resequence_duplicate_scopes(
            connection,
            "transcript_entries",
            "'agent:' || agent_id",
            "transcript_seq",
            "evidence_id",
            "payload_json",
            true,
        )?;
    }
    Ok(())
}

fn migrate_runtime_sequences(connection: &Connection) -> Result<()> {
    if table_exists_internal(connection, "audit_events")? {
        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_audit_events_agent_event_seq_unique
               ON audit_events(agent_id, event_seq)
               WHERE agent_id IS NOT NULL AND event_seq IS NOT NULL;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_audit_events_host_event_seq_unique
               ON audit_events(event_seq)
               WHERE agent_id IS NULL AND event_seq IS NOT NULL;
             INSERT INTO runtime_sequences(domain, scope_key, last_value)
             SELECT
               'audit_event',
               CASE WHEN agent_id IS NULL THEN 'host' ELSE 'agent:' || agent_id END,
               MAX(event_seq)
             FROM audit_events
             WHERE event_seq IS NOT NULL
             GROUP BY agent_id
             ON CONFLICT(domain, scope_key) DO UPDATE SET
               last_value = MAX(runtime_sequences.last_value, excluded.last_value);",
        )?;
    }
    if table_exists_internal(connection, "messages")? {
        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_agent_message_seq_unique
               ON messages(agent_id, message_seq)
               WHERE message_seq IS NOT NULL;
             INSERT INTO runtime_sequences(domain, scope_key, last_value)
             SELECT 'message', 'agent:' || agent_id, MAX(message_seq)
             FROM messages
             WHERE message_seq IS NOT NULL
             GROUP BY agent_id
             ON CONFLICT(domain, scope_key) DO UPDATE SET
               last_value = MAX(runtime_sequences.last_value, excluded.last_value);",
        )?;
    }
    if table_exists_internal(connection, "transcript_entries")? {
        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_transcript_entries_agent_transcript_seq_unique
               ON transcript_entries(agent_id, transcript_seq)
               WHERE transcript_seq IS NOT NULL;
             INSERT INTO runtime_sequences(domain, scope_key, last_value)
             SELECT 'transcript', 'agent:' || agent_id, MAX(transcript_seq)
             FROM transcript_entries
             WHERE transcript_seq IS NOT NULL
             GROUP BY agent_id
             ON CONFLICT(domain, scope_key) DO UPDATE SET
               last_value = MAX(runtime_sequences.last_value, excluded.last_value);",
        )?;
    }
    Ok(())
}

fn table_exists_internal(connection: &Connection, table_name: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
             )",
            [table_name],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn resequence_duplicate_scopes(
    connection: &Connection,
    table: &str,
    scope_expression: &str,
    sequence_column: &str,
    id_column: &str,
    payload_column: &str,
    update_content_hash: bool,
) -> Result<bool> {
    let sql = format!(
        "WITH scoped AS (
           SELECT
             {id_column} AS record_id,
             {scope_expression} AS scope_key,
             {sequence_column} AS sequence,
             created_at,
             {payload_column} AS payload_json
           FROM {table}
           WHERE {sequence_column} IS NOT NULL
         ),
         duplicate_scopes AS (
           SELECT DISTINCT scope_key
           FROM scoped
           GROUP BY scope_key, sequence
           HAVING COUNT(*) > 1
         ),
         ranked AS (
           SELECT
             record_id,
             scope_key,
             sequence,
             payload_json,
             ROW_NUMBER() OVER (
               PARTITION BY scope_key
               ORDER BY sequence, created_at, record_id
             ) AS new_sequence
           FROM scoped
           WHERE scope_key IN (SELECT scope_key FROM duplicate_scopes)
         )
         SELECT record_id, scope_key, sequence, new_sequence, payload_json
         FROM ranked
         ORDER BY scope_key, new_sequence"
    );
    let resequenced = connection
        .prepare(&sql)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let changed = !resequenced.is_empty();
    for (record_id, scope_key, old_sequence, new_sequence, payload_json) in resequenced {
        let mut payload: serde_json::Value = serde_json::from_str(&payload_json).with_context(|| {
            format!(
                "decoding runtime sequence payload during migration: table={table}, record_id={record_id}"
            )
        })?;
        let object = payload.as_object_mut().with_context(|| {
            format!(
                "runtime sequence payload is not an object: table={table}, record_id={record_id}"
            )
        })?;
        object.insert(
            sequence_column.to_string(),
            serde_json::Value::from(new_sequence),
        );
        let payload_json = serde_json::to_string(&payload)?;
        let updated = if update_content_hash {
            let sql = format!(
                "UPDATE {table}
                 SET {sequence_column} = ?1, {payload_column} = ?2, content_hash = ?3
                 WHERE {id_column} = ?4 AND {sequence_column} = ?5"
            );
            connection.execute(
                &sql,
                params![
                    new_sequence,
                    payload_json,
                    content_hash(&payload_json),
                    record_id,
                    old_sequence
                ],
            )?
        } else {
            let sql = format!(
                "UPDATE {table}
                 SET {sequence_column} = ?1, {payload_column} = ?2
                 WHERE {id_column} = ?3 AND {sequence_column} = ?4"
            );
            connection.execute(
                &sql,
                params![new_sequence, payload_json, record_id, old_sequence],
            )?
        };
        anyhow::ensure!(
            updated == 1,
            "runtime sequence migration failed to repair duplicate: table={table}, scope={scope_key}, record_id={record_id}"
        );
    }
    Ok(changed)
}

fn rotate_audit_event_epoch(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS runtime_metadata (
           key TEXT PRIMARY KEY,
           value TEXT NOT NULL,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL
         );",
    )?;
    let epoch = crate::ids::event_log_epoch_id();
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    connection.execute(
        "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
         VALUES ('event_log_epoch', ?1, ?2, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![epoch, now],
    )?;

    let events = connection
        .prepare("SELECT audit_event_id, data_json FROM audit_events")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (record_id, payload_json) in events {
        let mut payload: serde_json::Value =
            serde_json::from_str(&payload_json).with_context(|| {
                format!(
                    "decoding audit event payload while rotating event-log epoch: record_id={record_id}"
                )
            })?;
        let object = payload.as_object_mut().with_context(|| {
            format!(
                "audit event payload is not an object while rotating event-log epoch: record_id={record_id}"
            )
        })?;
        object.insert(
            "event_log_epoch".to_string(),
            serde_json::Value::String(epoch.clone()),
        );
        connection.execute(
            "UPDATE audit_events SET data_json = ?1 WHERE audit_event_id = ?2",
            params![serde_json::to_string(&payload)?, record_id],
        )?;
    }
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
