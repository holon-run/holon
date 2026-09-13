//! Durable revision and linkage metadata for the conversation read model.

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::domain::conversation::{
    map_result, presentation_class, ActivityItem, ActivityKey, Attention, ConversationActivity,
    ConversationActivityPage, ConversationSummaryPage, ConversationTurnSummary, DetailCoverage,
    DetailCoverageReason, ExecutionState, PendingInput, PendingInputState, PresentationClass,
    TerminalOutcome, TurnKey,
};
use crate::runtime_db::types::ConversationRepository;
use crate::types::{TurnRecord, TurnTerminalKind};

pub(crate) const SOURCE_OPERATOR: &str = "operator";
pub(crate) const SOURCE_ASSISTANT: &str = "assistant";
pub(crate) const SOURCE_TOOL: &str = "tool";
pub(crate) const SOURCE_WAIT: &str = "wait";
pub(crate) const SOURCE_ERROR: &str = "error";

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

const MAX_HISTORY_PAGE_LIMIT: usize = 100;
const MAX_ACTIVE_TURNS: usize = 32;
const MAX_PENDING_INPUTS: usize = 100;
const MAX_ACTIVITIES_PAGE_LIMIT: usize = 200;
const MAX_BRIEFS_PER_TURN: usize = 64;

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
         WHERE agent_id = ?1 AND message_id = ?2",
        params![agent_id, message_id],
        |row| row.get(0),
    )
    .optional()
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
        anyhow::ensure!(
            (1..=MAX_HISTORY_PAGE_LIMIT).contains(&limit),
            "conversation history limit must be between 1 and {MAX_HISTORY_PAGE_LIMIT}"
        );
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction()?;
        let membership_upper_bound = membership_upper_bound
            .cloned()
            .or(latest_turn_key(&transaction, agent_id)?);
        if let (Some(before), Some(upper_bound)) = (before, membership_upper_bound.as_ref()) {
            anyhow::ensure!(
                before <= upper_bound,
                "conversation history cursor is newer than its membership upper bound"
            );
        }
        let mut turns = match membership_upper_bound.as_ref() {
            Some(upper_bound) => {
                history_page_rows(&transaction, agent_id, limit, before, upper_bound)?
            }
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
        let active_turns = active_turn_rows(&transaction, agent_id)?;
        let pending_inputs = pending_input_rows(&transaction, agent_id)?;
        transaction.commit()?;
        Ok(ConversationSummaryPage {
            turns,
            active_turns,
            pending_inputs,
            membership_upper_bound,
            next_before,
            has_more,
        })
    }

    pub fn activities(
        &self,
        agent_id: &str,
        turn_id: &str,
        limit: usize,
        before: Option<&ActivityKey>,
        membership_upper_bound: Option<&ActivityKey>,
    ) -> Result<Option<ConversationActivityPage>> {
        anyhow::ensure!(
            (1..=MAX_ACTIVITIES_PAGE_LIMIT).contains(&limit),
            "conversation activity limit must be between 1 and {MAX_ACTIVITIES_PAGE_LIMIT}"
        );
        let mut connection = self.db.connection()?;
        let transaction = connection.transaction()?;
        let Some((turn, detail_revision)) = turn_summary_by_id(&transaction, agent_id, turn_id)?
        else {
            return Ok(None);
        };
        let membership_upper_bound = membership_upper_bound.cloned().or(latest_activity_key(
            &transaction,
            agent_id,
            turn_id,
        )?);
        if let (Some(before), Some(upper_bound)) = (before, membership_upper_bound.as_ref()) {
            anyhow::ensure!(
                before <= upper_bound,
                "conversation detail cursor is newer than its membership upper bound"
            );
        }
        let mut raw_rows = match membership_upper_bound.as_ref() {
            Some(upper_bound) => {
                activity_page_rows(&transaction, agent_id, turn_id, limit, before, upper_bound)?
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
        transaction.commit()?;
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
    anyhow::ensure!(
        rows.len() <= MAX_ACTIVE_TURNS,
        "agent {agent_id} exceeds the bounded active-turn recovery limit"
    );
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
    anyhow::ensure!(
        rows.len() <= MAX_PENDING_INPUTS,
        "agent {agent_id} exceeds the bounded pending-input recovery limit"
    );
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
    anyhow::ensure!(
        rows.len() <= MAX_BRIEFS_PER_TURN,
        "turn {turn_id} exceeds the bounded Brief membership limit"
    );
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
