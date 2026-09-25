//! Per-agent Brief read cursors and exact unread counts.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::runtime_db::evidence::audit_event_sequence_scope;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BriefReadState {
    pub agent_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    pub read_through_event_seq: u64,
    pub unread_count: u64,
    pub revision: u64,
    pub reset_required: bool,
    pub retention_gap: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarkBriefReadResult {
    pub state: BriefReadState,
    pub applied_read_through_event_seq: u64,
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn as_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("{field} must not be negative"))
}

fn event_head(connection: &rusqlite::Connection, agent_id: &str) -> Result<u64> {
    let scope = audit_event_sequence_scope(Some(agent_id));
    let value: i64 = connection
        .query_row(
            "SELECT COALESCE(last_value, 0)
             FROM runtime_sequences
             WHERE domain = 'audit_event' AND scope_key = ?1",
            [scope],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    as_u64(value, "event head")
}

fn oldest_retained(connection: &rusqlite::Connection, agent_id: &str) -> Result<u64> {
    let scope = audit_event_sequence_scope(Some(agent_id));
    let value: i64 = connection
        .query_row(
            "SELECT COALESCE(oldest_retained_seq, 0)
             FROM audit_event_retention_watermarks
             WHERE scope_key = ?1",
            [scope],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    as_u64(value, "oldest retained sequence")
}

fn state_for_connection(
    connection: &rusqlite::Connection,
    principal_id: &str,
    visibility_scope_id: &str,
    agent_id: &str,
    initialize_cursor: bool,
) -> Result<BriefReadState> {
    let epoch: String = connection.query_row(
        "SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'",
        [],
        |row| row.get(0),
    )?;
    let head = event_head(connection, agent_id)?;
    let oldest = oldest_retained(connection, agent_id)?;
    if initialize_cursor {
        connection.execute(
            "INSERT OR IGNORE INTO agent_brief_read_cursors (
                principal_id, visibility_scope_id, agent_id, event_log_epoch,
                read_through_event_seq, revision, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            params![
                principal_id,
                visibility_scope_id,
                agent_id,
                epoch,
                i64::try_from(head).context("event head exceeds SQLite integer range")?,
                timestamp(),
            ],
        )?;
    }
    let cursor_row: Option<(i64, i64)> = connection
        .query_row(
            "SELECT read_through_event_seq, revision
         FROM agent_brief_read_cursors
         WHERE principal_id = ?1 AND visibility_scope_id = ?2
           AND agent_id = ?3 AND event_log_epoch = ?4",
            params![principal_id, visibility_scope_id, agent_id, epoch],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (cursor, revision) = cursor_row.unwrap_or((
        i64::try_from(head).context("event head exceeds SQLite integer range")?,
        0,
    ));
    let cursor = as_u64(cursor, "read cursor")?;
    let revision = as_u64(revision, "cursor revision")?;
    let retention_gap = oldest > 0 && cursor.saturating_add(1) < oldest;
    let unread_count: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM briefs
         WHERE agent_id = ?1
           AND created_event_seq IS NOT NULL
           AND created_event_seq > ?2
           AND created_event_seq <= ?3",
        params![
            agent_id,
            i64::try_from(cursor).context("read cursor exceeds SQLite integer range")?,
            i64::try_from(head).context("event head exceeds SQLite integer range")?,
        ],
        |row| row.get(0),
    )?;
    Ok(BriefReadState {
        agent_id: agent_id.to_string(),
        event_log_epoch: epoch,
        visibility_scope_id: visibility_scope_id.to_string(),
        event_head_seq: head,
        oldest_retained_seq: oldest,
        read_through_event_seq: cursor,
        unread_count: as_u64(unread_count, "unread count")?,
        revision,
        reset_required: retention_gap,
        retention_gap,
    })
}

impl crate::runtime_db::RuntimeDb {
    pub fn brief_read_state(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        self.transaction(|transaction| {
            let public: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM agent_identities
                     WHERE agent_id = ?1 AND status = 'active' AND visibility = 'public'",
                    [agent_id],
                    |row| row.get(0),
                )
                .optional()?;
            if public.is_none() {
                return Ok(None);
            }
            Ok(Some(state_for_connection(
                transaction,
                principal_id,
                visibility_scope_id,
                agent_id,
                true,
            )?))
        })
    }

    pub fn brief_read_states(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
    ) -> Result<Vec<BriefReadState>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let mut statement = transaction.prepare(
            "SELECT agent_id FROM agent_identities
             WHERE status = 'active' AND visibility = 'public'
             ORDER BY agent_id",
        )?;
        let agent_ids: Vec<String> = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let states = agent_ids
            .iter()
            .map(|agent_id| {
                state_for_connection(
                    &transaction,
                    principal_id,
                    visibility_scope_id,
                    agent_id,
                    false,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        transaction.commit()?;
        Ok(states)
    }

    pub fn mark_brief_read(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<Option<MarkBriefReadResult>> {
        self.transaction(|transaction| {
            let public: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM agent_identities
                     WHERE agent_id = ?1 AND status = 'active' AND visibility = 'public'",
                    [agent_id],
                    |row| row.get(0),
                )
                .optional()?;
            if public.is_none() {
                return Ok(None);
            }
            let state = state_for_connection(
                transaction,
                principal_id,
                visibility_scope_id,
                agent_id,
                true,
            )?;
            let applied = requested_cursor.min(state.event_head_seq);
            transaction.execute(
                "UPDATE agent_brief_read_cursors
                 SET read_through_event_seq = MAX(read_through_event_seq, ?1),
                     revision = revision + 1,
                     updated_at = ?2
                 WHERE principal_id = ?3 AND visibility_scope_id = ?4
                   AND agent_id = ?5 AND event_log_epoch = ?6
                   AND read_through_event_seq < ?1",
                params![
                    i64::try_from(applied)
                        .context("applied cursor exceeds SQLite integer range")?,
                    timestamp(),
                    principal_id,
                    visibility_scope_id,
                    agent_id,
                    state.event_log_epoch,
                ],
            )?;
            let state = state_for_connection(
                transaction,
                principal_id,
                visibility_scope_id,
                agent_id,
                true,
            )?;
            Ok(Some(MarkBriefReadResult {
                applied_read_through_event_seq: state.read_through_event_seq,
                state,
            }))
        })
    }
}
