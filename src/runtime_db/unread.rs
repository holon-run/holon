//! Per-agent Brief read cursors and exact unread counts.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::time::Instant;

#[cfg(test)]
#[path = "unread_scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "unread_fast_path_tests.rs"]
mod fast_path_tests;

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

#[derive(Clone, Copy)]
enum ReadScope<'a> {
    Explicit(&'a str),
    Observer(&'a str),
}

impl<'a> ReadScope<'a> {
    fn resolve(
        &self,
        connection: &rusqlite::Connection,
        principal_id: &str,
    ) -> Result<Cow<'a, str>> {
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
        Ok(Cow::Owned(crate::ids::visibility_scope_id(
            &runtime_id,
            principal_id,
            entitlement,
            generation,
        )))
    }
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

fn agent_is_public(connection: &rusqlite::Connection, agent_id: &str) -> Result<bool> {
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
    connection: &rusqlite::Connection,
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

enum BriefReadStateFastPath {
    NotPublic,
    Ready(BriefReadState),
    NeedsPin,
}

enum MarkBriefReadFastPath {
    NotPublic,
    Noop(MarkBriefReadResult),
    NeedsWriter,
}

impl crate::runtime_db::RuntimeDb {
    pub fn brief_read_state(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        self.brief_read_state_with_scope(
            principal_id,
            ReadScope::Explicit(visibility_scope_id),
            agent_id,
        )
    }

    pub fn observer_brief_read_state(
        &self,
        principal_id: &str,
        entitlement: &str,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        self.brief_read_state_with_scope(principal_id, ReadScope::Observer(entitlement), agent_id)
    }

    fn brief_read_state_with_scope(
        &self,
        principal_id: &str,
        scope: ReadScope<'_>,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        match self.brief_read_state_fast_path(principal_id, scope, agent_id)? {
            BriefReadStateFastPath::Ready(state) => Ok(Some(state)),
            BriefReadStateFastPath::NotPublic => Ok(None),
            BriefReadStateFastPath::NeedsPin => {
                self.brief_read_state_pin_cursor(principal_id, scope, agent_id)
            }
        }
    }

    /// Polled repeat reads never need the writer channel: once a cursor row
    /// exists, the state is pure derivation over committed rows. Only the
    /// first-ever read for a principal/scope/agent still pins the baseline
    /// cursor, preserving first-view unread semantics.
    fn brief_read_state_fast_path(
        &self,
        principal_id: &str,
        scope: ReadScope<'_>,
        agent_id: &str,
    ) -> Result<BriefReadStateFastPath> {
        self.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let visibility_scope_id = scope.resolve(&transaction, principal_id)?;
            if !agent_is_public(&transaction, agent_id)? {
                return Ok(BriefReadStateFastPath::NotPublic);
            }
            let state = state_for_connection(
                &transaction,
                principal_id,
                &visibility_scope_id,
                agent_id,
                false,
            )?;
            if !cursor_row_exists(
                &transaction,
                principal_id,
                visibility_scope_id.as_ref(),
                agent_id,
                &state.event_log_epoch,
            )? {
                return Ok(BriefReadStateFastPath::NeedsPin);
            }
            Ok(BriefReadStateFastPath::Ready(state))
        })
    }

    fn brief_read_state_pin_cursor(
        &self,
        principal_id: &str,
        scope: ReadScope<'_>,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        self.transaction(|transaction| {
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
        })
    }

    pub fn brief_read_states(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
    ) -> Result<Vec<BriefReadState>> {
        self.brief_read_states_with_scope(principal_id, ReadScope::Explicit(visibility_scope_id))
    }

    pub fn observer_brief_read_states(
        &self,
        principal_id: &str,
        entitlement: &str,
    ) -> Result<Vec<BriefReadState>> {
        self.brief_read_states_with_scope(principal_id, ReadScope::Observer(entitlement))
    }

    fn brief_read_states_with_scope(
        &self,
        principal_id: &str,
        scope: ReadScope<'_>,
    ) -> Result<Vec<BriefReadState>> {
        let started = Instant::now();
        self.with_read(|connection| {
            let connected = started.elapsed();
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let scope_started = Instant::now();
            let visibility_scope_id = scope.resolve(&transaction, principal_id)?;
            let scope_elapsed = scope_started.elapsed();
            let query_started = Instant::now();
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
                        &visibility_scope_id,
                        agent_id,
                        false,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let query_elapsed = query_started.elapsed();
            transaction.commit()?;
            tracing::debug!(
                target: "holon::http::unread",
                connection_us = connected.as_micros() as u64,
                scope_metadata_us = scope_elapsed.as_micros() as u64,
                unread_query_us = query_elapsed.as_micros() as u64,
                db_total_us = started.elapsed().as_micros() as u64,
                agents = states.len(),
                "unread batch database phases"
            );
            Ok(states)
        })
    }

    pub fn mark_brief_read(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<Option<MarkBriefReadResult>> {
        self.mark_brief_read_with_scope(
            principal_id,
            ReadScope::Explicit(visibility_scope_id),
            agent_id,
            requested_cursor,
        )
    }

    pub fn observer_mark_brief_read(
        &self,
        principal_id: &str,
        entitlement: &str,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<Option<MarkBriefReadResult>> {
        self.mark_brief_read_with_scope(
            principal_id,
            ReadScope::Observer(entitlement),
            agent_id,
            requested_cursor,
        )
    }

    fn mark_brief_read_with_scope(
        &self,
        principal_id: &str,
        scope: ReadScope<'_>,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<Option<MarkBriefReadResult>> {
        match self.mark_brief_read_fast_path(principal_id, scope, agent_id, requested_cursor)? {
            MarkBriefReadFastPath::Noop(result) => Ok(Some(result)),
            MarkBriefReadFastPath::NotPublic => Ok(None),
            MarkBriefReadFastPath::NeedsWriter => {
                self.mark_brief_read_via_writer(principal_id, scope, agent_id, requested_cursor)
            }
        }
    }

    /// Polled clients re-send cursors the persisted row already covers. Those
    /// requests (and non-public agent lookups) are answered entirely from the
    /// reader pool, so the single writer channel only sees cursors that
    /// actually advance or pin a baseline.
    fn mark_brief_read_fast_path(
        &self,
        principal_id: &str,
        scope: ReadScope<'_>,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<MarkBriefReadFastPath> {
        self.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let visibility_scope_id = scope.resolve(&transaction, principal_id)?;
            if !agent_is_public(&transaction, agent_id)? {
                return Ok(MarkBriefReadFastPath::NotPublic);
            }
            let state = state_for_connection(
                &transaction,
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
                &transaction,
                principal_id,
                visibility_scope_id.as_ref(),
                agent_id,
                &state.event_log_epoch,
            )? {
                return Ok(MarkBriefReadFastPath::NeedsWriter);
            }
            tracing::debug!(
                target: "holon::http::unread",
                requested_cursor,
                read_through_event_seq = state.read_through_event_seq,
                "mark brief read answered from reader pool"
            );
            Ok(MarkBriefReadFastPath::Noop(MarkBriefReadResult {
                applied_read_through_event_seq: state.read_through_event_seq,
                state,
            }))
        })
    }

    fn mark_brief_read_via_writer(
        &self,
        principal_id: &str,
        scope: ReadScope<'_>,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<Option<MarkBriefReadResult>> {
        self.transaction(|transaction| {
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
                    i64::try_from(applied)
                        .context("applied cursor exceeds SQLite integer range")?,
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
        })
    }
}
