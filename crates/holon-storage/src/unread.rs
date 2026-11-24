//! Per-agent Brief read cursors and exact unread counts.

use std::borrow::Cow;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::sequences::audit_event_sequence_scope;

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

#[derive(Clone, Copy)]
pub enum BriefReadScope<'a> {
    Explicit(&'a str),
    Observer(&'a str),
}

impl<'a> BriefReadScope<'a> {
    pub fn resolve(&self, connection: &Connection, principal_id: &str) -> Result<Cow<'a, str>> {
        let entitlement = match self {
            Self::Explicit(scope) => return Ok(Cow::Borrowed(scope)),
            Self::Observer(entitlement) => entitlement,
        };
        // Bind scope to the cursor operation's committed view, without loading
        // roster, agent state or Brief JSON.
        let (runtime_id, generation): (String, String) = connection.query_row(
            "SELECT
                (SELECT value FROM runtime_metadata WHERE key = 'runtime_id'),
                (SELECT value FROM runtime_metadata WHERE key = 'visibility_policy_generation')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        anyhow::ensure!(!runtime_id.is_empty(), "runtime installation id is missing");
        let generation = generation
            .parse()
            .with_context(|| format!("invalid visibility policy generation {generation}"))?;
        Ok(Cow::Owned(holon_domain::ids::visibility_scope_id(
            &runtime_id,
            principal_id,
            entitlement,
            generation,
        )))
    }
}

fn event_head(connection: &Connection, agent_id: &str) -> Result<u64> {
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

fn oldest_retained(connection: &Connection, agent_id: &str) -> Result<u64> {
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
    connection: &Connection,
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

fn agent_is_public(connection: &Connection, agent_id: &str) -> Result<bool> {
    let public: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM agent_identities
             WHERE agent_id = ?1 AND status = 'active' AND visibility = 'public'",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(public.is_some())
}

fn cursor_row_exists(
    connection: &Connection,
    principal_id: &str,
    visibility_scope_id: &str,
    agent_id: &str,
    epoch: &str,
) -> Result<bool> {
    let pinned: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM agent_brief_read_cursors
             WHERE principal_id = ?1 AND visibility_scope_id = ?2
               AND agent_id = ?3 AND event_log_epoch = ?4",
            params![principal_id, visibility_scope_id, agent_id, epoch],
            |row| row.get(0),
        )
        .optional()?;
    Ok(pinned.is_some())
}

pub enum BriefReadStateFastPath {
    NotPublic,
    Ready(BriefReadState),
    NeedsPin,
}

pub enum MarkBriefReadFastPath {
    NotPublic,
    Noop(MarkBriefReadResult),
    NeedsWriter,
}

pub fn brief_read_state_fast_path(
    transaction: &Transaction<'_>,
    principal_id: &str,
    scope: BriefReadScope<'_>,
    agent_id: &str,
) -> Result<BriefReadStateFastPath> {
    let visibility_scope_id = scope.resolve(transaction, principal_id)?;
    if !agent_is_public(transaction, agent_id)? {
        return Ok(BriefReadStateFastPath::NotPublic);
    }
    let state = state_for_connection(
        transaction,
        principal_id,
        &visibility_scope_id,
        agent_id,
        false,
    )?;
    if !cursor_row_exists(
        transaction,
        principal_id,
        visibility_scope_id.as_ref(),
        agent_id,
        &state.event_log_epoch,
    )? {
        return Ok(BriefReadStateFastPath::NeedsPin);
    }
    Ok(BriefReadStateFastPath::Ready(state))
}

pub fn brief_read_state_pin_cursor(
    transaction: &Transaction<'_>,
    principal_id: &str,
    scope: BriefReadScope<'_>,
    agent_id: &str,
) -> Result<Option<BriefReadState>> {
    let visibility_scope_id = scope.resolve(transaction, principal_id)?;
    if !agent_is_public(transaction, agent_id)? {
        return Ok(None);
    }
    Ok(Some(state_for_connection(
        transaction,
        principal_id,
        &visibility_scope_id,
        agent_id,
        true,
    )?))
}

pub fn mark_brief_read_fast_path(
    transaction: &Transaction<'_>,
    principal_id: &str,
    scope: BriefReadScope<'_>,
    agent_id: &str,
    requested_cursor: u64,
) -> Result<MarkBriefReadFastPath> {
    let visibility_scope_id = scope.resolve(transaction, principal_id)?;
    if !agent_is_public(transaction, agent_id)? {
        return Ok(MarkBriefReadFastPath::NotPublic);
    }
    let state = state_for_connection(
        transaction,
        principal_id,
        &visibility_scope_id,
        agent_id,
        false,
    )?;
    // The guarded UPDATE below only changes a row while the persisted
    // cursor is below the clamped request; otherwise it would open a
    // write transaction just to change nothing. Answer that case here.
    let applied = requested_cursor.min(state.event_head_seq);
    if state.read_through_event_seq < applied {
        return Ok(MarkBriefReadFastPath::NeedsWriter);
    }
    // A derived-but-never-persisted cursor must still go through the
    // writer once so first-touch initialization keeps its exact
    // previous behavior.
    if !cursor_row_exists(
        transaction,
        principal_id,
        visibility_scope_id.as_ref(),
        agent_id,
        &state.event_log_epoch,
    )? {
        return Ok(MarkBriefReadFastPath::NeedsWriter);
    }
    Ok(MarkBriefReadFastPath::Noop(MarkBriefReadResult {
        applied_read_through_event_seq: state.read_through_event_seq,
        state,
    }))
}

pub fn mark_brief_read_via_writer(
    transaction: &Transaction<'_>,
    principal_id: &str,
    scope: BriefReadScope<'_>,
    agent_id: &str,
    requested_cursor: u64,
) -> Result<Option<MarkBriefReadResult>> {
    let visibility_scope_id = scope.resolve(transaction, principal_id)?;
    if !agent_is_public(transaction, agent_id)? {
        return Ok(None);
    }
    let state = state_for_connection(
        transaction,
        principal_id,
        &visibility_scope_id,
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
            i64::try_from(applied).context("applied cursor exceeds SQLite integer range")?,
            timestamp(),
            principal_id,
            visibility_scope_id.as_ref(),
            agent_id,
            state.event_log_epoch,
        ],
    )?;
    let state = state_for_connection(
        transaction,
        principal_id,
        &visibility_scope_id,
        agent_id,
        true,
    )?;
    Ok(Some(MarkBriefReadResult {
        applied_read_through_event_seq: state.read_through_event_seq,
        state,
    }))
}

pub fn brief_read_states(
    connection: &Transaction<'_>,
    principal_id: &str,
    visibility_scope_id: &str,
) -> Result<Vec<BriefReadState>> {
    let mut statement = connection.prepare(
        "SELECT agent_id FROM agent_identities
         WHERE status = 'active' AND visibility = 'public'
         ORDER BY agent_id",
    )?;
    let agent_ids: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    agent_ids
        .iter()
        .map(|agent_id| {
            state_for_connection(
                connection,
                principal_id,
                visibility_scope_id,
                agent_id,
                false,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
