//! Durable revision and linkage metadata for the conversation read model.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{
    params, params_from_iter, Connection, OptionalExtension, Transaction, TransactionBehavior,
};
use serde_json::Value;

use crate::domain::conversation::{
    map_result, presentation_class, ActivityItem, ActivityKey, Attention, ConversationActivity,
    ConversationActivityPage, ConversationChange, ConversationShadowDiagnostics,
    ConversationShadowMetadata, ConversationShadowMismatch, ConversationShadowMismatchKind,
    ConversationSummaryPage, ConversationTurnSummary, CursorBinding, CursorCodec,
    CursorDecodeError, DetailCoverage, DetailCoverageReason, DetailCursor, ExecutionState,
    HistoryCursor, PendingInput, PendingInputState, PresentationClass, StreamCursor,
    TerminalOutcome, TurnInputSummary, TurnKey, CONVERSATION_QUERY_VERSION,
    CONVERSATION_SCHEMA_VERSION,
};
use crate::runtime_db::types::ConversationRepository;
use crate::types::{TurnRecord, TurnTerminalKind};

const MAX_CHANGE_ID_JSON_DEPTH: usize = 32;

pub(crate) const SOURCE_OPERATOR: &str = "operator";
pub(crate) const SOURCE_ASSISTANT: &str = "assistant";
pub(crate) const SOURCE_TOOL: &str = "tool";
pub(crate) const SOURCE_WAIT: &str = "wait";
pub(crate) const SOURCE_ERROR: &str = "error";

pub const MAX_HISTORY_PAGE_LIMIT: usize = 100;
pub const MAX_ACTIVITIES_PAGE_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSnapshot<T> {
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    pub snapshot_cursor: String,
    pub next_before_cursor: Option<String>,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationChangeBatch {
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub from_seq: u64,
    pub through_seq: u64,
    pub oldest_retained_seq: u64,
    pub checkpoint: String,
    pub changes: Vec<ConversationChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationResetReason {
    RetentionExpired,
    CursorAhead,
    ReplayLimitExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConversationReadError {
    #[error("{resource} limit must be between {minimum} and {maximum}, got {actual}")]
    InvalidLimit {
        resource: &'static str,
        minimum: usize,
        maximum: usize,
        actual: usize,
    },
    #[error("{resource} exceeds the bounded count limit {limit}")]
    CountLimitExceeded {
        resource: &'static str,
        limit: usize,
    },
    #[error("conversation cursor ordering is outside its fixed membership boundary")]
    CursorOutsideCoverage,
    #[error("conversation cursor rejected: {0}")]
    Cursor(#[from] CursorDecodeError),
    #[error("conversation stream reset required: {reason:?}")]
    ResetRequired {
        reason: ConversationResetReason,
        requested_seq: u64,
        oldest_retained_seq: u64,
        event_head_seq: u64,
    },
}

pub(crate) const CONVERSATION_HISTORY_FIRST_SQL: &str = "
SELECT turns.payload_json,
       COALESCE(revisions.summary_revision, 1),
       COALESCE(revisions.detail_revision, 1),
       COALESCE(revisions.result_settled, 0)
FROM turn_records AS turns
LEFT JOIN conversation_turn_revisions AS revisions
  ON revisions.agent_id = turns.agent_id
 AND revisions.turn_id = turns.turn_id
WHERE turns.agent_id = ?1
  AND (
    turns.turn_index < ?2
    OR (turns.turn_index = ?2 AND turns.turn_id <= ?3)
  )
ORDER BY turns.turn_index DESC, turns.turn_id DESC
LIMIT ?4";

pub(crate) const CONVERSATION_HISTORY_BEFORE_SQL: &str = "
SELECT turns.payload_json,
       COALESCE(revisions.summary_revision, 1),
       COALESCE(revisions.detail_revision, 1),
       COALESCE(revisions.result_settled, 0)
FROM turn_records AS turns
LEFT JOIN conversation_turn_revisions AS revisions
  ON revisions.agent_id = turns.agent_id
 AND revisions.turn_id = turns.turn_id
WHERE turns.agent_id = ?1
  AND (
    turns.turn_index < ?2
    OR (turns.turn_index = ?2 AND turns.turn_id <= ?3)
  )
  AND (
    turns.turn_index < ?4
    OR (turns.turn_index = ?4 AND turns.turn_id < ?5)
  )
ORDER BY turns.turn_index DESC, turns.turn_id DESC
LIMIT ?6";

pub(crate) const CONVERSATION_ACTIVITY_FIRST_SQL: &str = "
SELECT sources.source_kind,
       sources.source_id,
       sources.activity_seq,
       sources.revision,
       CASE sources.source_kind
         WHEN 'operator' THEN messages.preview
         WHEN 'assistant' THEN transcript.preview
         WHEN 'tool' THEN tools.preview
         WHEN 'wait' THEN waits.waiting_for
         WHEN 'error' THEN transcript.preview
         ELSE NULL
       END
FROM conversation_source_revisions AS sources
LEFT JOIN messages
  ON sources.source_kind = 'operator'
 AND messages.evidence_id = sources.source_id
LEFT JOIN transcript_entries AS transcript
  ON sources.source_kind IN ('assistant', 'error')
 AND transcript.evidence_id = sources.source_id
LEFT JOIN tool_executions AS tools
  ON sources.source_kind = 'tool'
 AND tools.evidence_id = sources.source_id
LEFT JOIN wait_conditions AS waits
  ON sources.source_kind = 'wait'
 AND waits.wait_condition_id = sources.source_id
WHERE sources.agent_id = ?1
  AND sources.turn_id = ?2
  AND sources.activity_seq <= ?3
ORDER BY sources.activity_seq DESC, (sources.source_kind || ':' || sources.source_id) DESC
LIMIT ?4";

pub(crate) const CONVERSATION_ACTIVITY_BEFORE_SQL: &str = "
SELECT sources.source_kind,
       sources.source_id,
       sources.activity_seq,
       sources.revision,
       CASE sources.source_kind
         WHEN 'operator' THEN messages.preview
         WHEN 'assistant' THEN transcript.preview
         WHEN 'tool' THEN tools.preview
         WHEN 'wait' THEN waits.waiting_for
         WHEN 'error' THEN transcript.preview
         ELSE NULL
       END
FROM conversation_source_revisions AS sources
LEFT JOIN messages
  ON sources.source_kind = 'operator'
 AND messages.evidence_id = sources.source_id
LEFT JOIN transcript_entries AS transcript
  ON sources.source_kind IN ('assistant', 'error')
 AND transcript.evidence_id = sources.source_id
LEFT JOIN tool_executions AS tools
  ON sources.source_kind = 'tool'
 AND tools.evidence_id = sources.source_id
LEFT JOIN wait_conditions AS waits
  ON sources.source_kind = 'wait'
 AND waits.wait_condition_id = sources.source_id
WHERE sources.agent_id = ?1
  AND sources.turn_id = ?2
  AND sources.activity_seq <= ?3
  AND (
    sources.activity_seq < ?4
    OR (
      sources.activity_seq = ?4
      AND (sources.source_kind || ':' || sources.source_id) < ?5
    )
  )
ORDER BY sources.activity_seq DESC, (sources.source_kind || ':' || sources.source_id) DESC
LIMIT ?6";

const MAX_ACTIVE_TURNS: usize = 32;
const MAX_PENDING_INPUTS: usize = 100;
const MAX_BRIEFS_PER_TURN: usize = 64;
const MAX_INPUTS_PER_TURN: usize = 8;
const MAX_CONVERSATION_SHADOW_MISMATCH_SAMPLES: usize = 32;
pub(crate) const MAX_CONVERSATION_CHANGE_EVENTS: usize = 256;
pub(crate) const MAX_CONVERSATION_CHANGE_ACTIVITIES: usize = 64;
pub(crate) const MAX_CONVERSATION_SHADOW_TURNS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationTurnRevision {
    pub summary: u64,
    pub detail: u64,
    pub result_settled: bool,
}

pub(crate) fn ensure_turn_revision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    updated_at: DateTime<Utc>,
) -> Result<ConversationTurnRevision> {
    if !table_exists_tx(tx, "conversation_turn_revisions")? {
        return Ok(unavailable_turn_revision(false));
    }
    if let Some(revision) = turn_revision_tx(tx, agent_id, turn_id)? {
        return Ok(revision);
    }
    tx.execute(
        "INSERT INTO conversation_turn_revisions (
           agent_id, turn_id, summary_revision, detail_revision, result_settled, updated_at
         ) VALUES (?1, ?2, 1, 1, 0, ?3)",
        params![agent_id, turn_id, timestamp(updated_at)],
    )?;
    Ok(ConversationTurnRevision {
        summary: 1,
        detail: 1,
        result_settled: false,
    })
}

pub(crate) fn bump_turn_revision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    summary_changed: bool,
    detail_changed: bool,
    updated_at: DateTime<Utc>,
) -> Result<ConversationTurnRevision> {
    if !table_exists_tx(tx, "conversation_turn_revisions")? {
        return Ok(unavailable_turn_revision(false));
    }
    let Some(existing) = turn_revision_tx(tx, agent_id, turn_id)? else {
        return ensure_turn_revision_tx(tx, agent_id, turn_id, updated_at);
    };
    if !summary_changed && !detail_changed {
        return Ok(existing);
    }
    tx.execute(
        "UPDATE conversation_turn_revisions
         SET summary_revision = summary_revision + ?3,
             detail_revision = detail_revision + ?4,
             updated_at = ?5
         WHERE agent_id = ?1 AND turn_id = ?2",
        params![
            agent_id,
            turn_id,
            i64::from(summary_changed),
            i64::from(detail_changed),
            timestamp(updated_at),
        ],
    )?;
    turn_revision_tx(tx, agent_id, turn_id)?
        .ok_or_else(|| anyhow::anyhow!("conversation turn revision disappeared after update"))
}

pub(crate) fn settle_turn_result_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    updated_at: DateTime<Utc>,
) -> Result<ConversationTurnRevision> {
    if !table_exists_tx(tx, "conversation_turn_revisions")? {
        return Ok(unavailable_turn_revision(true));
    }
    let existing = ensure_turn_revision_tx(tx, agent_id, turn_id, updated_at)?;
    if existing.result_settled {
        return Ok(existing);
    }
    tx.execute(
        "UPDATE conversation_turn_revisions
         SET result_settled = 1,
             summary_revision = summary_revision + 1,
             updated_at = ?3
         WHERE agent_id = ?1 AND turn_id = ?2",
        params![agent_id, turn_id, timestamp(updated_at)],
    )?;
    turn_revision_tx(tx, agent_id, turn_id)?
        .ok_or_else(|| anyhow::anyhow!("conversation turn revision disappeared after settlement"))
}

pub(crate) fn bump_source_revision_tx(
    tx: &Transaction<'_>,
    source_kind: &str,
    source_id: &str,
    agent_id: &str,
    turn_id: Option<&str>,
    updated_at: DateTime<Utc>,
) -> Result<u64> {
    if !table_exists_tx(tx, "conversation_source_revisions")? {
        return Ok(0);
    }
    let existing = tx
        .query_row(
            "SELECT agent_id, turn_id, revision
             FROM conversation_source_revisions
             WHERE source_kind = ?1 AND source_id = ?2",
            params![source_kind, source_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((existing_agent_id, existing_turn_id, revision)) = existing else {
        tx.execute(
            "INSERT INTO conversation_source_revisions (
               source_kind, source_id, agent_id, turn_id, activity_seq, revision, updated_at
             ) VALUES (
               ?1, ?2, ?3, ?4,
               (SELECT COALESCE(MAX(activity_seq), 0) + 1
                FROM conversation_source_revisions),
               1, ?5
             )",
            params![
                source_kind,
                source_id,
                agent_id,
                turn_id,
                timestamp(updated_at),
            ],
        )?;
        return Ok(1);
    };
    if existing_agent_id != agent_id {
        bail!("conversation source identity changed agent: kind={source_kind} id={source_id}");
    }
    if existing_turn_id.is_some() && turn_id.is_some() && existing_turn_id.as_deref() != turn_id {
        bail!("conversation source identity changed turn: kind={source_kind} id={source_id}");
    }
    let next = u64::try_from(revision)?.saturating_add(1);
    tx.execute(
        "UPDATE conversation_source_revisions
         SET turn_id = COALESCE(turn_id, ?3),
             activity_seq = COALESCE(
               activity_seq,
               (SELECT COALESCE(MAX(activity_seq), 0) + 1
                FROM conversation_source_revisions)
             ),
             revision = ?4,
             updated_at = ?5
         WHERE source_kind = ?1 AND source_id = ?2",
        params![
            source_kind,
            source_id,
            turn_id,
            i64::try_from(next)?,
            timestamp(updated_at),
        ],
    )?;
    Ok(next)
}

pub(crate) fn assign_input_to_turn_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
    turn_id: &str,
    assigned_at: DateTime<Utc>,
) -> Result<bool> {
    if !table_exists_tx(tx, "conversation_input_assignments")? {
        return Ok(false);
    }
    let existing = tx
        .query_row(
            "SELECT agent_id, turn_id
             FROM conversation_input_assignments
             WHERE message_id = ?1",
            [message_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((existing_agent_id, existing_turn_id)) = existing {
        if existing_agent_id != agent_id || existing_turn_id != turn_id {
            bail!("conversation input assignment identity conflict for {message_id}");
        }
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO conversation_input_assignments (
           message_id, agent_id, turn_id, revision, assigned_at
         ) VALUES (?1, ?2, ?3, 1, ?4)",
        params![message_id, agent_id, turn_id, timestamp(assigned_at)],
    )?;
    let _ = bump_source_revision_tx(
        tx,
        SOURCE_OPERATOR,
        message_id,
        agent_id,
        Some(turn_id),
        assigned_at,
    )?;
    let _ = bump_turn_revision_tx(tx, agent_id, turn_id, true, true, assigned_at)?;
    Ok(true)
}

pub(crate) fn assigned_turn_id_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
) -> Result<Option<String>> {
    if !table_exists_tx(tx, "conversation_input_assignments")? {
        return Ok(None);
    }
    tx.query_row(
        "SELECT turn_id
         FROM conversation_input_assignments
         WHERE agent_id = ?1
           AND message_id = ?2",
        params![agent_id, message_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn assigned_turn_ids_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if message_ids.is_empty() || !table_exists_tx(tx, "conversation_input_assignments")? {
        return Ok(BTreeSet::new());
    }

    let placeholders = std::iter::repeat_n("?", message_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = tx.prepare(&format!(
        "SELECT DISTINCT turn_id
         FROM conversation_input_assignments
         WHERE agent_id = ?
           AND message_id IN ({placeholders})"
    ))?;
    let rows = statement.query_map(
        params_from_iter(std::iter::once(agent_id).chain(message_ids.iter().map(String::as_str))),
        |row| row.get::<_, String>(0),
    )?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(Into::into)
}

impl ConversationRepository<'_> {
    pub fn summary_page(
        &self,
        agent_id: &str,
        limit: usize,
        before: Option<&TurnKey>,
        membership_upper_bound: Option<&TurnKey>,
    ) -> Result<ConversationSummaryPage> {
        validate_limit("conversation history", limit, MAX_HISTORY_PAGE_LIMIT)?;
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let page = summary_page_in(
            &transaction,
            agent_id,
            limit,
            before,
            membership_upper_bound,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    pub fn summary_snapshot(
        &self,
        agent_id: &str,
        limit: usize,
        before_cursor: Option<&str>,
        scope_principal: &str,
        scope_entitlement: &str,
    ) -> Result<Option<ConversationSnapshot<ConversationSummaryPage>>> {
        self.summary_snapshot_after_context(
            agent_id,
            limit,
            before_cursor,
            scope_principal,
            scope_entitlement,
            || {},
        )
    }

    pub fn shadow_diagnostics(
        &self,
        agent_id: &str,
        turn_limit: usize,
        scope_principal: &str,
        scope_entitlement: &str,
    ) -> Result<Option<ConversationShadowDiagnostics>> {
        validate_limit(
            "conversation shadow turns",
            turn_limit,
            MAX_CONVERSATION_SHADOW_TURNS,
        )?;
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let Some(context) = conversation_snapshot_context(
            &transaction,
            agent_id,
            scope_principal,
            scope_entitlement,
        )?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let projection = summary_page_in(&transaction, agent_id, turn_limit, None, None)?;
        let canonical = canonical_shadow_metadata(
            &transaction,
            agent_id,
            turn_limit,
            projection.membership_upper_bound.as_ref(),
        )?;
        let diagnostics = compare_shadow_metadata(context, turn_limit, canonical, projection)?;
        transaction.commit()?;
        Ok(Some(diagnostics))
    }

    fn summary_snapshot_after_context(
        &self,
        agent_id: &str,
        limit: usize,
        before_cursor: Option<&str>,
        scope_principal: &str,
        scope_entitlement: &str,
        after_context: impl FnOnce(),
    ) -> Result<Option<ConversationSnapshot<ConversationSummaryPage>>> {
        validate_limit("conversation history", limit, MAX_HISTORY_PAGE_LIMIT)?;
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let Some(context) = conversation_snapshot_context(
            &transaction,
            agent_id,
            scope_principal,
            scope_entitlement,
        )?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        after_context();
        let binding = context.binding(agent_id);
        let codec = CursorCodec::new(context.cursor_signing_key.as_bytes());
        let cursor = before_cursor
            .map(|encoded| codec.decode::<HistoryCursor>(encoded, &binding))
            .transpose()
            .map_err(ConversationReadError::from)?;
        let page = summary_page_in(
            &transaction,
            agent_id,
            limit,
            cursor.as_ref().map(|cursor| &cursor.before),
            cursor.as_ref().map(|cursor| &cursor.membership_upper_bound),
        )?;
        let next_before_cursor = page.next_before.as_ref().map(|before| {
            codec.encode(&HistoryCursor {
                binding: binding.clone(),
                before: before.clone(),
                membership_upper_bound: page
                    .membership_upper_bound
                    .clone()
                    .expect("a next history cursor requires a membership upper bound"),
            })
        });
        let snapshot_cursor = codec.encode(&StreamCursor {
            binding,
            event_seq: context.event_head_seq,
        });
        transaction.commit()?;
        Ok(Some(ConversationSnapshot {
            runtime_id: context.runtime_id,
            event_log_epoch: context.event_log_epoch,
            visibility_scope_id: context.visibility_scope_id,
            event_head_seq: context.event_head_seq,
            oldest_retained_seq: context.oldest_retained_seq,
            snapshot_cursor,
            next_before_cursor,
            value: page,
        }))
    }

    pub fn activities(
        &self,
        agent_id: &str,
        turn_id: &str,
        limit: usize,
        before: Option<&ActivityKey>,
        membership_upper_bound: Option<&ActivityKey>,
    ) -> Result<Option<ConversationActivityPage>> {
        validate_limit("conversation activity", limit, MAX_ACTIVITIES_PAGE_LIMIT)?;
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let page = activities_in(
            &transaction,
            agent_id,
            turn_id,
            limit,
            before,
            membership_upper_bound,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    pub fn activity_snapshot(
        &self,
        agent_id: &str,
        turn_id: &str,
        limit: usize,
        before_cursor: Option<&str>,
        scope_principal: &str,
        scope_entitlement: &str,
    ) -> Result<Option<ConversationSnapshot<Option<ConversationActivityPage>>>> {
        validate_limit("conversation activity", limit, MAX_ACTIVITIES_PAGE_LIMIT)?;
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let Some(context) = conversation_snapshot_context(
            &transaction,
            agent_id,
            scope_principal,
            scope_entitlement,
        )?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let binding = context.binding(agent_id);
        let codec = CursorCodec::new(context.cursor_signing_key.as_bytes());
        let cursor = before_cursor
            .map(|encoded| codec.decode::<DetailCursor>(encoded, &binding))
            .transpose()
            .map_err(ConversationReadError::from)?;
        if cursor
            .as_ref()
            .is_some_and(|cursor| cursor.turn_id != turn_id)
        {
            return Err(ConversationReadError::Cursor(CursorDecodeError::BindingMismatch).into());
        }
        let page = activities_in(
            &transaction,
            agent_id,
            turn_id,
            limit,
            cursor.as_ref().map(|cursor| &cursor.before),
            cursor.as_ref().map(|cursor| &cursor.membership_upper_bound),
        )?;
        let next_before_cursor = page.as_ref().and_then(|page| {
            page.next_before.as_ref().map(|before| {
                codec.encode(&DetailCursor {
                    binding: binding.clone(),
                    turn_id: turn_id.to_string(),
                    before: before.clone(),
                    membership_upper_bound: page
                        .membership_upper_bound
                        .clone()
                        .expect("a next detail cursor requires a membership upper bound"),
                })
            })
        });
        let snapshot_cursor = codec.encode(&StreamCursor {
            binding,
            event_seq: context.event_head_seq,
        });
        transaction.commit()?;
        Ok(Some(ConversationSnapshot {
            runtime_id: context.runtime_id,
            event_log_epoch: context.event_log_epoch,
            visibility_scope_id: context.visibility_scope_id,
            event_head_seq: context.event_head_seq,
            oldest_retained_seq: context.oldest_retained_seq,
            snapshot_cursor,
            next_before_cursor,
            value: page,
        }))
    }

    pub fn change_batch(
        &self,
        agent_id: &str,
        after_cursor: Option<&str>,
        event_limit: usize,
        activity_limit: usize,
        scope_principal: &str,
        scope_entitlement: &str,
    ) -> Result<Option<ConversationChangeBatch>> {
        validate_limit(
            "conversation change events",
            event_limit,
            MAX_CONVERSATION_CHANGE_EVENTS,
        )?;
        validate_limit(
            "conversation change activities",
            activity_limit,
            MAX_CONVERSATION_CHANGE_ACTIVITIES,
        )?;
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let Some(context) = conversation_snapshot_context(
            &transaction,
            agent_id,
            scope_principal,
            scope_entitlement,
        )?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let binding = context.binding(agent_id);
        let codec = CursorCodec::new(context.cursor_signing_key.as_bytes());
        let from_seq = after_cursor
            .map(|encoded| codec.decode::<StreamCursor>(encoded, &binding))
            .transpose()
            .map_err(ConversationReadError::from)?
            .map_or(context.event_head_seq, |cursor| cursor.event_seq);
        if from_seq > context.event_head_seq {
            return Err(ConversationReadError::ResetRequired {
                reason: ConversationResetReason::CursorAhead,
                requested_seq: from_seq,
                oldest_retained_seq: context.oldest_retained_seq,
                event_head_seq: context.event_head_seq,
            }
            .into());
        }
        if from_seq < context.event_head_seq
            && from_seq.saturating_add(1) < context.oldest_retained_seq
        {
            return Err(ConversationReadError::ResetRequired {
                reason: ConversationResetReason::RetentionExpired,
                requested_seq: from_seq,
                oldest_retained_seq: context.oldest_retained_seq,
                event_head_seq: context.event_head_seq,
            }
            .into());
        }

        let events = audit_events_between(
            &transaction,
            agent_id,
            from_seq,
            context.event_head_seq,
            event_limit + 1,
        )?;
        if events.len() > event_limit {
            return Err(ConversationReadError::ResetRequired {
                reason: ConversationResetReason::ReplayLimitExceeded,
                requested_seq: from_seq,
                oldest_retained_seq: context.oldest_retained_seq,
                event_head_seq: context.event_head_seq,
            }
            .into());
        }

        let mut changes = Vec::new();
        if !events.is_empty() {
            let mut turn_ids = BTreeSet::new();
            let mut message_ids = BTreeSet::new();
            for event in &events {
                collect_change_ids(&event.data, &mut turn_ids, &mut message_ids, 0);
            }
            turn_ids.extend(assigned_turn_ids_tx(&transaction, agent_id, &message_ids)?);

            let pending_inputs = pending_input_rows(&transaction, agent_id)?;
            let pending_ids = pending_inputs
                .iter()
                .map(|input| input.message_id.clone())
                .collect::<BTreeSet<_>>();
            changes.extend(
                pending_inputs
                    .into_iter()
                    .map(|input| ConversationChange::OperatorUpsert { input }),
            );
            for message_id in message_ids {
                if !pending_ids.contains(&message_id) {
                    changes.push(ConversationChange::OperatorRemove {
                        revision: source_revision(
                            &transaction,
                            SOURCE_OPERATOR,
                            &message_id,
                            agent_id,
                        )?,
                        message_id,
                    });
                }
            }

            for turn in active_turn_rows(&transaction, agent_id)? {
                turn_ids.insert(turn.turn_id);
            }
            let mut remaining_activity_limit = activity_limit;
            for turn_id in turn_ids {
                let Some((turn, detail_revision)) =
                    turn_summary_by_id(&transaction, agent_id, &turn_id)?
                else {
                    continue;
                };
                let active = matches!(turn.execution, ExecutionState::Active);
                changes.push(ConversationChange::TurnSummaryUpsert { turn });
                if active && remaining_activity_limit > 0 {
                    if let Some(page) = activities_in(
                        &transaction,
                        agent_id,
                        &turn_id,
                        remaining_activity_limit,
                        None,
                        None,
                    )? {
                        if !page.has_more {
                            remaining_activity_limit =
                                remaining_activity_limit.saturating_sub(page.activities.len());
                            changes.extend(page.activities.into_iter().map(|activity| {
                                ConversationChange::ActivityUpsert {
                                    turn_id: turn_id.clone(),
                                    activity,
                                }
                            }));
                        }
                    }
                }
                changes.push(ConversationChange::DetailInvalidated {
                    turn_id,
                    detail_revision,
                });
            }
        }

        let checkpoint = codec.encode(&StreamCursor {
            binding,
            event_seq: context.event_head_seq,
        });
        transaction.commit()?;
        Ok(Some(ConversationChangeBatch {
            runtime_id: context.runtime_id,
            event_log_epoch: context.event_log_epoch,
            visibility_scope_id: context.visibility_scope_id,
            from_seq,
            through_seq: context.event_head_seq,
            oldest_retained_seq: context.oldest_retained_seq,
            checkpoint,
            changes,
        }))
    }
}

fn audit_events_between(
    connection: &Connection,
    agent_id: &str,
    after_seq: u64,
    through_seq: u64,
    limit: usize,
) -> Result<Vec<crate::types::AuditEvent>> {
    let mut statement = connection.prepare(
        "SELECT data_json
         FROM audit_events
         WHERE agent_id = ?1 AND event_seq > ?2 AND event_seq <= ?3
         ORDER BY event_seq
         LIMIT ?4",
    )?;
    let events = statement
        .query_map(
            params![
                agent_id,
                i64::try_from(after_seq)?,
                i64::try_from(through_seq)?,
                i64::try_from(limit)?
            ],
            |row| row.get::<_, String>(0),
        )?
        .map(|row| {
            let json = row?;
            serde_json::from_str(&json).context("invalid canonical audit event payload")
        })
        .collect();
    events
}

fn collect_change_ids(
    value: &Value,
    turn_ids: &mut BTreeSet<String>,
    message_ids: &mut BTreeSet<String>,
    depth: usize,
) {
    if depth > MAX_CHANGE_ID_JSON_DEPTH {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if let Some(value) = value.as_str() {
                    if matches!(
                        key.as_str(),
                        "turn_id" | "current_turn_id" | "source_turn_id"
                    ) {
                        turn_ids.insert(value.to_string());
                    } else if matches!(
                        key.as_str(),
                        "message_id" | "related_message_id" | "source_message_id"
                    ) {
                        message_ids.insert(value.to_string());
                    }
                }
                collect_change_ids(value, turn_ids, message_ids, depth + 1);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_change_ids(value, turn_ids, message_ids, depth + 1);
            }
        }
        _ => {}
    }
}

fn source_revision(
    connection: &Connection,
    source_kind: &str,
    source_id: &str,
    agent_id: &str,
) -> Result<u64> {
    connection
        .query_row(
            "SELECT revision
             FROM conversation_source_revisions
             WHERE source_kind = ?1 AND source_id = ?2 AND agent_id = ?3",
            params![source_kind, source_id, agent_id],
            |row| {
                u64::try_from(row.get::<_, i64>(0)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })
            },
        )
        .optional()
        .map(|revision| revision.unwrap_or(1))
        .map_err(Into::into)
}

struct ConversationSnapshotContext {
    runtime_id: String,
    event_log_epoch: String,
    visibility_scope_id: String,
    event_head_seq: u64,
    oldest_retained_seq: u64,
    cursor_signing_key: String,
}

impl ConversationSnapshotContext {
    fn binding(&self, agent_id: &str) -> CursorBinding {
        CursorBinding {
            runtime_id: self.runtime_id.clone(),
            agent_id: agent_id.to_string(),
            event_log_epoch: self.event_log_epoch.clone(),
            visibility_scope_id: self.visibility_scope_id.clone(),
            schema_version: CONVERSATION_SCHEMA_VERSION,
            query_version: CONVERSATION_QUERY_VERSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShadowTurnMetadata {
    revision: Option<u64>,
    brief_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShadowPendingInputMetadata {
    revision: Option<u64>,
    state: PendingInputState,
}

struct CanonicalShadowMetadata {
    turns: BTreeMap<String, ShadowTurnMetadata>,
    active_turns: BTreeMap<String, ShadowTurnMetadata>,
    pending_inputs: BTreeMap<String, ShadowPendingInputMetadata>,
    legacy_unattributed_briefs: usize,
}

fn canonical_shadow_metadata(
    connection: &Connection,
    agent_id: &str,
    turn_limit: usize,
    membership_upper_bound: Option<&TurnKey>,
) -> Result<CanonicalShadowMetadata> {
    let mut turns = BTreeMap::new();
    if let Some(upper_bound) = membership_upper_bound {
        let mut statement = connection.prepare(
            "SELECT turns.turn_id, revisions.summary_revision
             FROM turn_records AS turns
             LEFT JOIN conversation_turn_revisions AS revisions
               ON revisions.agent_id = turns.agent_id
              AND revisions.turn_id = turns.turn_id
             WHERE turns.agent_id = ?1
               AND (
                 turns.turn_index < ?2
                 OR (turns.turn_index = ?2 AND turns.turn_id <= ?3)
               )
             ORDER BY turns.turn_index DESC, turns.turn_id DESC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                agent_id,
                i64::try_from(upper_bound.turn_index)?,
                upper_bound.turn_id,
                i64::try_from(turn_limit)?,
            ],
            |row| {
                let revision = row
                    .get::<_, Option<i64>>(1)?
                    .map(u64::try_from)
                    .transpose()
                    .map_err(sql_integer_error)?;
                Ok((row.get::<_, String>(0)?, revision))
            },
        )?;
        for row in rows {
            let (turn_id, revision) = row?;
            turns.insert(
                turn_id,
                ShadowTurnMetadata {
                    revision,
                    brief_ids: Vec::new(),
                },
            );
        }
    }

    let mut statement = connection.prepare(
        "SELECT turns.turn_id, revisions.summary_revision
         FROM turn_records AS turns
         LEFT JOIN conversation_turn_revisions AS revisions
           ON revisions.agent_id = turns.agent_id
          AND revisions.turn_id = turns.turn_id
         WHERE turns.agent_id = ?1
           AND turns.terminal_kind IS NULL
         ORDER BY turns.turn_index, turns.turn_id
         LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![agent_id, i64::try_from(MAX_ACTIVE_TURNS + 1)?],
        |row| {
            let revision = row
                .get::<_, Option<i64>>(1)?
                .map(u64::try_from)
                .transpose()
                .map_err(sql_integer_error)?;
            Ok((row.get::<_, String>(0)?, revision))
        },
    )?;
    let active_rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if active_rows.len() > MAX_ACTIVE_TURNS {
        return Err(ConversationReadError::CountLimitExceeded {
            resource: "active turns",
            limit: MAX_ACTIVE_TURNS,
        }
        .into());
    }
    let mut active_turns = active_rows
        .into_iter()
        .map(|(turn_id, revision)| {
            (
                turn_id,
                ShadowTurnMetadata {
                    revision,
                    brief_ids: Vec::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let turn_ids = turns
        .keys()
        .chain(active_turns.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let brief_ids = canonical_shadow_brief_ids(connection, agent_id, &turn_ids)?;
    for (turn_id, ids) in brief_ids {
        if let Some(turn) = turns.get_mut(&turn_id) {
            turn.brief_ids = ids.clone();
        }
        if let Some(turn) = active_turns.get_mut(&turn_id) {
            turn.brief_ids = ids;
        }
    }

    let mut statement = connection.prepare(
        "SELECT queue.message_id, revisions.revision, queue.status
         FROM queue_entries AS queue
         LEFT JOIN conversation_input_assignments AS assignments
           ON assignments.message_id = queue.message_id
          AND assignments.agent_id = queue.agent_id
         LEFT JOIN conversation_source_revisions AS revisions
           ON revisions.source_kind = 'operator'
          AND revisions.source_id = queue.message_id
          AND revisions.agent_id = queue.agent_id
         WHERE queue.agent_id = ?1
           AND queue.status IN ('queued', 'dequeued')
           AND assignments.message_id IS NULL
         ORDER BY queue.created_at, queue.message_id
         LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![agent_id, i64::try_from(MAX_PENDING_INPUTS + 1)?],
        |row| {
            let revision = row
                .get::<_, Option<i64>>(1)?
                .map(u64::try_from)
                .transpose()
                .map_err(sql_integer_error)?;
            let state = if row.get::<_, String>(2)? == "queued" {
                PendingInputState::Queued
            } else {
                PendingInputState::Assigning
            };
            Ok((
                row.get::<_, String>(0)?,
                ShadowPendingInputMetadata { revision, state },
            ))
        },
    )?;
    let pending_rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if pending_rows.len() > MAX_PENDING_INPUTS {
        return Err(ConversationReadError::CountLimitExceeded {
            resource: "pending inputs",
            limit: MAX_PENDING_INPUTS,
        }
        .into());
    }

    let legacy_unattributed_briefs = connection.query_row(
        "SELECT COUNT(*)
         FROM briefs
         WHERE agent_id = ?1 AND turn_id IS NULL",
        [agent_id],
        |row| row.get::<_, i64>(0),
    )?;

    Ok(CanonicalShadowMetadata {
        turns,
        active_turns,
        pending_inputs: pending_rows.into_iter().collect(),
        legacy_unattributed_briefs: usize::try_from(legacy_unattributed_briefs)
            .context("legacy unattributed Brief count is negative")?,
    })
}

fn canonical_shadow_brief_ids(
    connection: &Connection,
    agent_id: &str,
    turn_ids: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<String>>> {
    if turn_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = std::iter::repeat_n("?", turn_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection.prepare(&format!(
        "SELECT turn_id, evidence_id
         FROM briefs
         WHERE agent_id = ?
           AND turn_id IN ({placeholders})
         ORDER BY turn_id, COALESCE(created_event_seq, 9223372036854775807),
                  created_at, evidence_id"
    ))?;
    let rows = statement.query_map(
        params_from_iter(std::iter::once(agent_id).chain(turn_ids.iter().map(String::as_str))),
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut by_turn = BTreeMap::<String, Vec<String>>::new();
    for row in rows {
        let (turn_id, brief_id) = row?;
        let brief_ids = by_turn.entry(turn_id).or_default();
        if brief_ids.len() >= MAX_BRIEFS_PER_TURN {
            return Err(ConversationReadError::CountLimitExceeded {
                resource: "briefs per turn",
                limit: MAX_BRIEFS_PER_TURN,
            }
            .into());
        }
        brief_ids.push(brief_id);
    }
    Ok(by_turn)
}

fn compare_shadow_metadata(
    context: ConversationSnapshotContext,
    checked_turn_limit: usize,
    canonical: CanonicalShadowMetadata,
    projection: ConversationSummaryPage,
) -> Result<ConversationShadowDiagnostics> {
    let projection_turns = projection
        .turns
        .iter()
        .map(|turn| {
            (
                turn.turn_id.clone(),
                ShadowTurnMetadata {
                    revision: Some(turn.revision),
                    brief_ids: turn.brief_ids.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let projection_active_turns = projection
        .active_turns
        .iter()
        .map(|turn| {
            (
                turn.turn_id.clone(),
                ShadowTurnMetadata {
                    revision: Some(turn.revision),
                    brief_ids: turn.brief_ids.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let projection_pending_inputs = projection
        .pending_inputs
        .iter()
        .map(|input| {
            (
                input.message_id.clone(),
                ShadowPendingInputMetadata {
                    revision: Some(input.revision),
                    state: input.state,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let canonical_briefs = unique_shadow_brief_count(&canonical.turns, &canonical.active_turns);
    let projection_briefs = unique_shadow_brief_count(&projection_turns, &projection_active_turns);
    let canonical_metadata = ConversationShadowMetadata {
        turns: canonical.turns.len(),
        active_turns: canonical.active_turns.len(),
        pending_inputs: canonical.pending_inputs.len(),
        briefs: canonical_briefs,
    };
    let projection_metadata = ConversationShadowMetadata {
        turns: projection_turns.len(),
        active_turns: projection_active_turns.len(),
        pending_inputs: projection_pending_inputs.len(),
        briefs: projection_briefs,
    };

    let mut mismatch_count = 0;
    let mut mismatches = Vec::new();
    compare_shadow_turns(
        &canonical.turns,
        &projection_turns,
        ConversationShadowMismatchKind::MissingProjectionTurn,
        ConversationShadowMismatchKind::UnexpectedProjectionTurn,
        &mut mismatch_count,
        &mut mismatches,
    );
    compare_shadow_turns(
        &canonical.active_turns,
        &projection_active_turns,
        ConversationShadowMismatchKind::ActiveMembership,
        ConversationShadowMismatchKind::ActiveMembership,
        &mut mismatch_count,
        &mut mismatches,
    );
    compare_shadow_inputs(
        &canonical.pending_inputs,
        &projection_pending_inputs,
        &mut mismatch_count,
        &mut mismatches,
    );

    Ok(ConversationShadowDiagnostics {
        schema_version: CONVERSATION_SCHEMA_VERSION,
        query_version: CONVERSATION_QUERY_VERSION,
        runtime_id: context.runtime_id,
        event_log_epoch: context.event_log_epoch,
        visibility_scope_id: context.visibility_scope_id,
        event_head_seq: context.event_head_seq,
        oldest_retained_seq: context.oldest_retained_seq,
        checked_turn_limit,
        canonical: canonical_metadata,
        projection: projection_metadata,
        legacy_unattributed_briefs: canonical.legacy_unattributed_briefs,
        mismatch_count,
        mismatch_samples_truncated: mismatch_count > mismatches.len(),
        mismatches,
    })
}

fn unique_shadow_brief_count(
    turns: &BTreeMap<String, ShadowTurnMetadata>,
    active_turns: &BTreeMap<String, ShadowTurnMetadata>,
) -> usize {
    turns
        .iter()
        .chain(active_turns)
        .map(|(turn_id, turn)| (turn_id, turn.brief_ids.len()))
        .collect::<BTreeMap<_, _>>()
        .values()
        .sum()
}

fn compare_shadow_turns(
    canonical: &BTreeMap<String, ShadowTurnMetadata>,
    projection: &BTreeMap<String, ShadowTurnMetadata>,
    missing_kind: ConversationShadowMismatchKind,
    unexpected_kind: ConversationShadowMismatchKind,
    mismatch_count: &mut usize,
    mismatches: &mut Vec<ConversationShadowMismatch>,
) {
    for (turn_id, canonical_turn) in canonical {
        let Some(projection_turn) = projection.get(turn_id) else {
            push_shadow_mismatch(
                mismatch_count,
                mismatches,
                ConversationShadowMismatch {
                    kind: missing_kind,
                    entity_id: turn_id.clone(),
                    canonical_revision: canonical_turn.revision,
                    projection_revision: None,
                    canonical_count: Some(canonical_turn.brief_ids.len()),
                    projection_count: None,
                    canonical_state: None,
                    projection_state: None,
                },
            );
            continue;
        };
        if canonical_turn.revision != projection_turn.revision {
            push_shadow_mismatch(
                mismatch_count,
                mismatches,
                ConversationShadowMismatch {
                    kind: ConversationShadowMismatchKind::TurnRevision,
                    entity_id: turn_id.clone(),
                    canonical_revision: canonical_turn.revision,
                    projection_revision: projection_turn.revision,
                    canonical_count: None,
                    projection_count: None,
                    canonical_state: None,
                    projection_state: None,
                },
            );
        }
        if canonical_turn.brief_ids != projection_turn.brief_ids {
            push_shadow_mismatch(
                mismatch_count,
                mismatches,
                ConversationShadowMismatch {
                    kind: ConversationShadowMismatchKind::BriefMembership,
                    entity_id: turn_id.clone(),
                    canonical_revision: None,
                    projection_revision: None,
                    canonical_count: Some(canonical_turn.brief_ids.len()),
                    projection_count: Some(projection_turn.brief_ids.len()),
                    canonical_state: None,
                    projection_state: None,
                },
            );
        }
    }
    for (turn_id, projection_turn) in projection {
        if canonical.contains_key(turn_id) {
            continue;
        }
        push_shadow_mismatch(
            mismatch_count,
            mismatches,
            ConversationShadowMismatch {
                kind: unexpected_kind,
                entity_id: turn_id.clone(),
                canonical_revision: None,
                projection_revision: projection_turn.revision,
                canonical_count: None,
                projection_count: Some(projection_turn.brief_ids.len()),
                canonical_state: None,
                projection_state: None,
            },
        );
    }
}

fn compare_shadow_inputs(
    canonical: &BTreeMap<String, ShadowPendingInputMetadata>,
    projection: &BTreeMap<String, ShadowPendingInputMetadata>,
    mismatch_count: &mut usize,
    mismatches: &mut Vec<ConversationShadowMismatch>,
) {
    for (message_id, canonical_input) in canonical {
        let Some(projection_input) = projection.get(message_id) else {
            push_shadow_mismatch(
                mismatch_count,
                mismatches,
                ConversationShadowMismatch {
                    kind: ConversationShadowMismatchKind::MissingProjectionInput,
                    entity_id: message_id.clone(),
                    canonical_revision: canonical_input.revision,
                    projection_revision: None,
                    canonical_count: None,
                    projection_count: None,
                    canonical_state: Some(canonical_input.state),
                    projection_state: None,
                },
            );
            continue;
        };
        if canonical_input.revision != projection_input.revision {
            push_shadow_mismatch(
                mismatch_count,
                mismatches,
                ConversationShadowMismatch {
                    kind: ConversationShadowMismatchKind::InputRevision,
                    entity_id: message_id.clone(),
                    canonical_revision: canonical_input.revision,
                    projection_revision: projection_input.revision,
                    canonical_count: None,
                    projection_count: None,
                    canonical_state: None,
                    projection_state: None,
                },
            );
        }
        if canonical_input.state != projection_input.state {
            push_shadow_mismatch(
                mismatch_count,
                mismatches,
                ConversationShadowMismatch {
                    kind: ConversationShadowMismatchKind::InputState,
                    entity_id: message_id.clone(),
                    canonical_revision: None,
                    projection_revision: None,
                    canonical_count: None,
                    projection_count: None,
                    canonical_state: Some(canonical_input.state),
                    projection_state: Some(projection_input.state),
                },
            );
        }
    }
    for (message_id, projection_input) in projection {
        if canonical.contains_key(message_id) {
            continue;
        }
        push_shadow_mismatch(
            mismatch_count,
            mismatches,
            ConversationShadowMismatch {
                kind: ConversationShadowMismatchKind::UnexpectedProjectionInput,
                entity_id: message_id.clone(),
                canonical_revision: None,
                projection_revision: projection_input.revision,
                canonical_count: None,
                projection_count: None,
                canonical_state: None,
                projection_state: Some(projection_input.state),
            },
        );
    }
}

fn push_shadow_mismatch(
    mismatch_count: &mut usize,
    mismatches: &mut Vec<ConversationShadowMismatch>,
    mismatch: ConversationShadowMismatch,
) {
    *mismatch_count += 1;
    if mismatches.len() < MAX_CONVERSATION_SHADOW_MISMATCH_SAMPLES {
        mismatches.push(mismatch);
    }
}

fn validate_limit(resource: &'static str, actual: usize, maximum: usize) -> Result<()> {
    if (1..=maximum).contains(&actual) {
        return Ok(());
    }
    Err(ConversationReadError::InvalidLimit {
        resource,
        minimum: 1,
        maximum,
        actual,
    }
    .into())
}

fn conversation_snapshot_context(
    connection: &Connection,
    agent_id: &str,
    scope_principal: &str,
    scope_entitlement: &str,
) -> Result<Option<ConversationSnapshotContext>> {
    let visible = connection
        .query_row(
            "SELECT 1 FROM agent_identities
             WHERE agent_id = ?1 AND status = 'active' AND visibility = 'public'",
            [agent_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !visible {
        return Ok(None);
    }
    let metadata = |key: &str| -> Result<String> {
        connection
            .query_row(
                "SELECT value FROM runtime_metadata WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .with_context(|| format!("runtime metadata {key} is missing"))
    };
    let runtime_id = metadata("runtime_id")?;
    let event_log_epoch = metadata("event_log_epoch")?;
    let visibility_policy_generation: u64 = metadata("visibility_policy_generation")?
        .parse()
        .context("invalid visibility policy generation")?;
    let cursor_signing_key = metadata("conversation_cursor_signing_key")?;
    let scope_key = crate::runtime_db::evidence::audit_event_sequence_scope(Some(agent_id));
    let (oldest_retained_seq, event_head_seq): (i64, i64) = connection.query_row(
        "SELECT
           COALESCE((
             SELECT oldest_retained_seq FROM audit_event_retention_watermarks
             WHERE scope_key = ?1
           ), 0),
           COALESCE((
             SELECT last_value FROM runtime_sequences
             WHERE domain = 'audit_event' AND scope_key = ?1
           ), 0)",
        [scope_key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(Some(ConversationSnapshotContext {
        visibility_scope_id: crate::ids::visibility_scope_id(
            &runtime_id,
            scope_principal,
            scope_entitlement,
            visibility_policy_generation,
        ),
        runtime_id,
        event_log_epoch,
        event_head_seq: u64::try_from(event_head_seq)
            .context("stored conversation event head is negative")?,
        oldest_retained_seq: u64::try_from(oldest_retained_seq)
            .context("stored conversation retention watermark is negative")?,
        cursor_signing_key,
    }))
}

fn summary_page_in(
    connection: &Connection,
    agent_id: &str,
    limit: usize,
    before: Option<&TurnKey>,
    membership_upper_bound: Option<&TurnKey>,
) -> Result<ConversationSummaryPage> {
    let membership_upper_bound = membership_upper_bound
        .cloned()
        .or(latest_turn_key(connection, agent_id)?);
    if let (Some(before), Some(upper_bound)) = (before, membership_upper_bound.as_ref()) {
        if before > upper_bound {
            return Err(ConversationReadError::CursorOutsideCoverage.into());
        }
    }
    let mut turns = match membership_upper_bound.as_ref() {
        Some(upper_bound) => history_page_rows(connection, agent_id, limit, before, upper_bound)?,
        None => Vec::new(),
    };
    let has_more = turns.len() > limit;
    if has_more {
        turns.pop();
    }
    let next_before = has_more
        .then(|| turns.last().map(|turn| turn.key.clone()))
        .flatten();
    turns.reverse();
    Ok(ConversationSummaryPage {
        turns,
        active_turns: active_turn_rows(connection, agent_id)?,
        pending_inputs: pending_input_rows(connection, agent_id)?,
        membership_upper_bound,
        next_before,
        has_more,
    })
}

fn activities_in(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
    limit: usize,
    before: Option<&ActivityKey>,
    membership_upper_bound: Option<&ActivityKey>,
) -> Result<Option<ConversationActivityPage>> {
    let Some((turn, detail_revision)) = turn_summary_by_id(connection, agent_id, turn_id)? else {
        return Ok(None);
    };
    let membership_upper_bound = membership_upper_bound
        .cloned()
        .or(latest_activity_key(connection, agent_id, turn_id)?);
    if let (Some(before), Some(upper_bound)) = (before, membership_upper_bound.as_ref()) {
        if before > upper_bound {
            return Err(ConversationReadError::CursorOutsideCoverage.into());
        }
    }
    let mut raw_rows = match membership_upper_bound.as_ref() {
        Some(upper_bound) => {
            activity_page_rows(connection, agent_id, turn_id, limit, before, upper_bound)?
        }
        None => Vec::new(),
    };
    let has_more = raw_rows.len() > limit;
    if has_more {
        raw_rows.pop();
    }
    let next_before = has_more
        .then(|| raw_rows.last().map(ActivityRow::key))
        .flatten();
    let mut coverage = turn.detail_coverage.clone();
    let mut activities = Vec::with_capacity(raw_rows.len());
    for row in raw_rows.into_iter().rev() {
        if let Some(activity) = row.into_activity() {
            activities.push(activity);
        } else {
            coverage = DetailCoverage::Partial {
                reason: DetailCoverageReason::RetentionGap,
            };
        }
    }
    Ok(Some(ConversationActivityPage {
        turn,
        detail_revision,
        activities,
        coverage,
        membership_upper_bound,
        next_before,
        has_more,
    }))
}

#[derive(Debug)]
struct ActivityRow {
    source_kind: String,
    source_id: String,
    activity_seq: u64,
    revision: u64,
    summary: Option<String>,
}

impl ActivityRow {
    fn key(&self) -> ActivityKey {
        ActivityKey {
            event_seq: self.activity_seq,
            activity_id: activity_id(&self.source_kind, &self.source_id),
        }
    }

    fn into_activity(self) -> Option<ConversationActivity> {
        let item = ActivityItem {
            id: activity_id(&self.source_kind, &self.source_id),
            key: ActivityKey {
                event_seq: self.activity_seq,
                activity_id: activity_id(&self.source_kind, &self.source_id),
            },
            revision: self.revision,
            summary: self.summary?,
        };
        match self.source_kind.as_str() {
            SOURCE_OPERATOR => Some(ConversationActivity::Operator(item)),
            SOURCE_ASSISTANT => Some(ConversationActivity::Assistant(item)),
            SOURCE_TOOL => Some(ConversationActivity::Tool(item)),
            SOURCE_WAIT => Some(ConversationActivity::Wait(item)),
            SOURCE_ERROR => Some(ConversationActivity::Error(item)),
            _ => None,
        }
    }
}

fn latest_turn_key(connection: &Connection, agent_id: &str) -> Result<Option<TurnKey>> {
    connection
        .query_row(
            "SELECT turn_index, turn_id
             FROM turn_records
             WHERE agent_id = ?1
             ORDER BY turn_index DESC, turn_id DESC
             LIMIT 1",
            [agent_id],
            |row| {
                Ok(TurnKey {
                    turn_index: u64::try_from(row.get::<_, i64>(0)?).map_err(sql_integer_error)?,
                    turn_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn history_page_rows(
    connection: &Connection,
    agent_id: &str,
    limit: usize,
    before: Option<&TurnKey>,
    upper_bound: &TurnKey,
) -> Result<Vec<ConversationTurnSummary>> {
    let row_limit = i64::try_from(limit + 1)?;
    let mut rows = if let Some(before) = before {
        let mut statement = connection.prepare(CONVERSATION_HISTORY_BEFORE_SQL)?;
        let rows = statement
            .query_map(
                params![
                    agent_id,
                    i64::try_from(upper_bound.turn_index)?,
                    upper_bound.turn_id,
                    i64::try_from(before.turn_index)?,
                    before.turn_id,
                    row_limit,
                ],
                decode_turn_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    } else {
        let mut statement = connection.prepare(CONVERSATION_HISTORY_FIRST_SQL)?;
        let rows = statement
            .query_map(
                params![
                    agent_id,
                    i64::try_from(upper_bound.turn_index)?,
                    upper_bound.turn_id,
                    row_limit,
                ],
                decode_turn_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    rows.iter_mut()
        .try_for_each(|row| hydrate_turn_summary(connection, row))?;
    Ok(rows.into_iter().map(|row| row.summary).collect())
}

fn active_turn_rows(
    connection: &Connection,
    agent_id: &str,
) -> Result<Vec<ConversationTurnSummary>> {
    let mut statement = connection.prepare(
        "SELECT turns.payload_json,
                COALESCE(revisions.summary_revision, 1),
                COALESCE(revisions.detail_revision, 1),
                COALESCE(revisions.result_settled, 0)
         FROM turn_records AS turns
         LEFT JOIN conversation_turn_revisions AS revisions
           ON revisions.agent_id = turns.agent_id
          AND revisions.turn_id = turns.turn_id
         WHERE turns.agent_id = ?1
           AND turns.terminal_kind IS NULL
         ORDER BY turns.turn_index, turns.turn_id
         LIMIT ?2",
    )?;
    let mut rows = statement
        .query_map(
            params![agent_id, i64::try_from(MAX_ACTIVE_TURNS + 1)?],
            decode_turn_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > MAX_ACTIVE_TURNS {
        return Err(ConversationReadError::CountLimitExceeded {
            resource: "active turns",
            limit: MAX_ACTIVE_TURNS,
        }
        .into());
    }
    rows.iter_mut()
        .try_for_each(|row| hydrate_turn_summary(connection, row))?;
    Ok(rows.into_iter().map(|row| row.summary).collect())
}

fn pending_input_rows(connection: &Connection, agent_id: &str) -> Result<Vec<PendingInput>> {
    let mut statement = connection.prepare(
        "SELECT queue.message_id,
                COALESCE(revisions.revision, 1),
                queue.status
         FROM queue_entries AS queue
         LEFT JOIN conversation_input_assignments AS assignments
           ON assignments.message_id = queue.message_id
          AND assignments.agent_id = queue.agent_id
         LEFT JOIN conversation_source_revisions AS revisions
           ON revisions.source_kind = 'operator'
          AND revisions.source_id = queue.message_id
          AND revisions.agent_id = queue.agent_id
         WHERE queue.agent_id = ?1
           AND queue.status IN ('queued', 'dequeued')
           AND assignments.message_id IS NULL
         ORDER BY queue.created_at, queue.message_id
         LIMIT ?2",
    )?;
    let rows = statement
        .query_map(
            params![agent_id, i64::try_from(MAX_PENDING_INPUTS + 1)?],
            |row| {
                let status = row.get::<_, String>(2)?;
                let state = if status == "queued" {
                    PendingInputState::Queued
                } else {
                    PendingInputState::Assigning
                };
                Ok(PendingInput {
                    message_id: row.get(0)?,
                    revision: u64::try_from(row.get::<_, i64>(1)?).map_err(sql_integer_error)?,
                    state,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > MAX_PENDING_INPUTS {
        return Err(ConversationReadError::CountLimitExceeded {
            resource: "pending inputs",
            limit: MAX_PENDING_INPUTS,
        }
        .into());
    }
    Ok(rows)
}

fn turn_summary_by_id(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<(ConversationTurnSummary, u64)>> {
    let row = connection
        .query_row(
            "SELECT turns.payload_json,
                    COALESCE(revisions.summary_revision, 1),
                    COALESCE(revisions.detail_revision, 1),
                    COALESCE(revisions.result_settled, 0)
             FROM turn_records AS turns
             LEFT JOIN conversation_turn_revisions AS revisions
               ON revisions.agent_id = turns.agent_id
              AND revisions.turn_id = turns.turn_id
             WHERE turns.agent_id = ?1 AND turns.turn_id = ?2",
            params![agent_id, turn_id],
            decode_turn_row,
        )
        .optional()?;
    row.map(|mut row| {
        hydrate_turn_summary(connection, &mut row)?;
        Ok((row.summary, row.detail_revision))
    })
    .transpose()
}

struct TurnSummaryRow {
    record: TurnRecord,
    summary: ConversationTurnSummary,
    detail_revision: u64,
    result_settled: bool,
}

fn decode_turn_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnSummaryRow> {
    let payload = row.get::<_, String>(0)?;
    let record: TurnRecord = serde_json::from_str(&payload).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let summary_revision = u64::try_from(row.get::<_, i64>(1)?).map_err(sql_integer_error)?;
    let detail_revision = u64::try_from(row.get::<_, i64>(2)?).map_err(sql_integer_error)?;
    let result_settled = row.get(3)?;
    let presentation_class = record
        .trigger
        .as_ref()
        .map(presentation_class)
        .unwrap_or(PresentationClass::Operational);
    let execution = record
        .terminal
        .as_ref()
        .map_or(ExecutionState::Active, |terminal| {
            ExecutionState::Terminal {
                outcome: terminal.kind.into(),
            }
        });
    Ok(TurnSummaryRow {
        summary: ConversationTurnSummary {
            turn_id: record.turn_id.clone(),
            key: TurnKey {
                turn_index: record.turn_index,
                turn_id: record.turn_id.clone(),
            },
            revision: summary_revision,
            presentation_class,
            inputs: Vec::new(),
            execution,
            result: crate::domain::conversation::ResultState::Pending,
            settled: false,
            attention: None,
            detail_coverage: DetailCoverage::Unknown,
            brief_ids: Vec::new(),
        },
        record,
        detail_revision,
        result_settled,
    })
}

fn hydrate_turn_summary(connection: &Connection, row: &mut TurnSummaryRow) -> Result<()> {
    row.summary.brief_ids = brief_ids(connection, &row.record.agent_id, &row.record.turn_id)?;
    row.summary.inputs =
        turn_input_previews(connection, &row.record.agent_id, &row.record.turn_id)?;
    let (result, settled) = map_result(
        row.summary.brief_ids.len(),
        row.record
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.no_brief_reason.as_ref()),
        row.result_settled,
    );
    row.summary.result = result;
    row.summary.settled = settled;
    row.summary.attention = attention(connection, &row.record)?;
    row.summary.detail_coverage = detail_coverage(connection, &row.record.agent_id, &row.record)?;
    Ok(())
}

fn brief_ids(connection: &Connection, agent_id: &str, turn_id: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT evidence_id
         FROM briefs
         WHERE agent_id = ?1 AND turn_id = ?2
         ORDER BY COALESCE(created_event_seq, 9223372036854775807), created_at, evidence_id
         LIMIT ?3",
    )?;
    let rows = statement
        .query_map(
            params![agent_id, turn_id, i64::try_from(MAX_BRIEFS_PER_TURN + 1)?],
            |row| row.get(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > MAX_BRIEFS_PER_TURN {
        return Err(ConversationReadError::CountLimitExceeded {
            resource: "briefs per turn",
            limit: MAX_BRIEFS_PER_TURN,
        }
        .into());
    }
    Ok(rows)
}

fn turn_input_previews(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
) -> Result<Vec<TurnInputSummary>> {
    let mut statement = connection.prepare(
        "SELECT assignments.message_id, COALESCE(messages.preview, '')
         FROM conversation_input_assignments AS assignments
         LEFT JOIN messages
           ON messages.evidence_id = assignments.message_id
         WHERE assignments.agent_id = ?1 AND assignments.turn_id = ?2
         ORDER BY assignments.assigned_at, assignments.message_id
         LIMIT ?3",
    )?;
    let rows = statement
        .query_map(
            params![agent_id, turn_id, i64::try_from(MAX_INPUTS_PER_TURN)?],
            |row| {
                Ok(TurnInputSummary {
                    message_id: row.get(0)?,
                    preview: row.get(1)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn attention(connection: &Connection, record: &TurnRecord) -> Result<Option<Attention>> {
    if let Some(terminal) = record.terminal.as_ref() {
        return Ok(match terminal.kind {
            TurnTerminalKind::Completed => None,
            TurnTerminalKind::BaselineOverBudget => Some(Attention::Failed {
                outcome: TerminalOutcome::BaselineOverBudget,
            }),
            TurnTerminalKind::Aborted
            | TurnTerminalKind::DeferredToFallback
            | TurnTerminalKind::ProviderFailedNeedsRecovery => Some(Attention::Interrupted),
        });
    }
    let waiting: bool = connection.query_row(
        "SELECT EXISTS(
           SELECT 1
           FROM wait_conditions
           WHERE agent_id = ?1
             AND last_turn_id = ?2
             AND status = 'active'
         )",
        params![record.agent_id, record.turn_id],
        |row| row.get(0),
    )?;
    Ok(waiting.then_some(Attention::Waiting))
}

fn detail_coverage(
    connection: &Connection,
    agent_id: &str,
    record: &TurnRecord,
) -> Result<DetailCoverage> {
    let missing_source = connection.query_row(
        "SELECT EXISTS(
           SELECT 1
           FROM conversation_source_revisions AS sources
           LEFT JOIN messages
             ON sources.source_kind = 'operator'
            AND messages.evidence_id = sources.source_id
           LEFT JOIN transcript_entries AS transcript
             ON sources.source_kind IN ('assistant', 'error')
            AND transcript.evidence_id = sources.source_id
           LEFT JOIN tool_executions AS tools
             ON sources.source_kind = 'tool'
            AND tools.evidence_id = sources.source_id
           LEFT JOIN wait_conditions AS waits
             ON sources.source_kind = 'wait'
            AND waits.wait_condition_id = sources.source_id
           WHERE sources.agent_id = ?1
             AND sources.turn_id = ?2
             AND (
               sources.source_kind NOT IN ('operator', 'assistant', 'tool', 'wait', 'error')
               OR CASE sources.source_kind
                    WHEN 'operator' THEN messages.evidence_id
                    WHEN 'assistant' THEN transcript.evidence_id
                    WHEN 'tool' THEN tools.evidence_id
                    WHEN 'wait' THEN waits.wait_condition_id
                    WHEN 'error' THEN transcript.evidence_id
                    ELSE NULL
                  END IS NULL
             )
         )",
        params![agent_id, record.turn_id],
        |row| row.get::<_, bool>(0),
    )?;
    if missing_source {
        return Ok(DetailCoverage::Partial {
            reason: DetailCoverageReason::RetentionGap,
        });
    }
    let unknown_activity = connection.query_row(
        "SELECT EXISTS(
           SELECT 1
           FROM transcript_entries
           WHERE agent_id = ?1
             AND turn_id = ?2
             AND kind NOT IN ('assistant_round', 'subagent_assistant_round', 'runtime_failure')
         )",
        params![agent_id, record.turn_id],
        |row| row.get::<_, bool>(0),
    )?;
    if unknown_activity {
        return Ok(DetailCoverage::Partial {
            reason: DetailCoverageReason::UnknownActivityType,
        });
    }
    if record.trigger.is_none() {
        return Ok(DetailCoverage::Partial {
            reason: DetailCoverageReason::LegacyOwnership,
        });
    }
    Ok(DetailCoverage::Complete)
}

fn latest_activity_key(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<ActivityKey>> {
    connection
        .query_row(
            "SELECT activity_seq, source_kind, source_id
             FROM conversation_source_revisions
             WHERE agent_id = ?1 AND turn_id = ?2
             ORDER BY activity_seq DESC, (source_kind || ':' || source_id) DESC
             LIMIT 1",
            params![agent_id, turn_id],
            |row| {
                let source_kind = row.get::<_, String>(1)?;
                let source_id = row.get::<_, String>(2)?;
                Ok(ActivityKey {
                    event_seq: u64::try_from(row.get::<_, i64>(0)?).map_err(sql_integer_error)?,
                    activity_id: activity_id(&source_kind, &source_id),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn activity_page_rows(
    connection: &Connection,
    agent_id: &str,
    turn_id: &str,
    limit: usize,
    before: Option<&ActivityKey>,
    upper_bound: &ActivityKey,
) -> Result<Vec<ActivityRow>> {
    let row_limit = i64::try_from(limit + 1)?;
    let rows = if let Some(before) = before {
        let mut statement = connection.prepare(CONVERSATION_ACTIVITY_BEFORE_SQL)?;
        let rows = statement
            .query_map(
                params![
                    agent_id,
                    turn_id,
                    i64::try_from(upper_bound.event_seq)?,
                    i64::try_from(before.event_seq)?,
                    before.activity_id,
                    row_limit,
                ],
                decode_activity_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    } else {
        let mut statement = connection.prepare(CONVERSATION_ACTIVITY_FIRST_SQL)?;
        let rows = statement
            .query_map(
                params![
                    agent_id,
                    turn_id,
                    i64::try_from(upper_bound.event_seq)?,
                    row_limit,
                ],
                decode_activity_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    Ok(rows)
}

fn decode_activity_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityRow> {
    Ok(ActivityRow {
        source_kind: row.get(0)?,
        source_id: row.get(1)?,
        activity_seq: u64::try_from(row.get::<_, i64>(2)?).map_err(sql_integer_error)?,
        revision: u64::try_from(row.get::<_, i64>(3)?).map_err(sql_integer_error)?,
        summary: row.get(4)?,
    })
}

fn activity_id(source_kind: &str, source_id: &str) -> String {
    format!("{source_kind}:{source_id}")
}

fn sql_integer_error(error: std::num::TryFromIntError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(error))
}

fn turn_revision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<ConversationTurnRevision>> {
    tx.query_row(
        "SELECT summary_revision, detail_revision, result_settled
         FROM conversation_turn_revisions
         WHERE agent_id = ?1 AND turn_id = ?2",
        params![agent_id, turn_id],
        |row| {
            Ok(ConversationTurnRevision {
                summary: u64::try_from(row.get::<_, i64>(0)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
                detail: u64::try_from(row.get::<_, i64>(1)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
                result_settled: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn table_exists_tx(tx: &Transaction<'_>, table: &str) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        [table],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn unavailable_turn_revision(result_settled: bool) -> ConversationTurnRevision {
    ConversationTurnRevision {
        summary: 0,
        detail: 0,
        result_settled,
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests;
