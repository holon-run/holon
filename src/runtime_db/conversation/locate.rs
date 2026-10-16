//! Exact canonical source ownership and bounded bidirectional history windows.
use super::*;
use crate::domain::conversation::HistoryAfterCursor;
use crate::memory::refs::RuntimeRef;
use schemars::JsonSchema;
use serde::Serialize;

pub(super) const HISTORY_AFTER_SQL: &str =
    "SELECT turns.payload_json, COALESCE(revisions.summary_revision, 1), COALESCE(revisions.detail_revision, 1), COALESCE(revisions.result_settled, 0)
     FROM turn_records AS turns LEFT JOIN conversation_turn_revisions AS revisions ON revisions.agent_id = turns.agent_id AND revisions.turn_id = turns.turn_id
     WHERE turns.agent_id = ?1 AND (turns.turn_index, turns.turn_id) > (?2, ?3) AND (turns.turn_index, turns.turn_id) <= (?4, ?5)
     ORDER BY turns.turn_index ASC, turns.turn_id ASC LIMIT ?6";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationLocateTarget {
    InputMessage {
        message_id: String,
        turn_id: Option<String>,
        activity_id: Option<String>,
    },
    Brief {
        brief_id: String,
        turn_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ConversationLocateResult {
    pub identity: ConversationIdentity,
    pub status: &'static str,
    pub source_ref: String,
    pub target: Option<ConversationLocateTarget>,
    pub reason: Option<&'static str>,
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub window: Option<ConversationSnapshot<ConversationSummaryPage>>,
}

impl ConversationRepository<'_> {
    pub fn summary_window_snapshot(
        &self,
        agent_id: &str,
        limit: usize,
        before: Option<&str>,
        after: Option<&str>,
        principal: &str,
        entitlement: &str,
    ) -> Result<Option<ConversationSnapshot<ConversationSummaryPage>>> {
        validate_limit("conversation history", limit, MAX_HISTORY_PAGE_LIMIT)?;
        if before.is_some() && after.is_some() {
            return Err(ConversationReadError::InvalidRequest(
                "before and after are mutually exclusive".into(),
            )
            .into());
        }
        let mut connection = self.db.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let Some(context) = self.snapshot_context(&tx, agent_id, principal, entitlement)? else {
            return Ok(None);
        };
        let binding = context.binding(agent_id);
        let codec = CursorCodec::new(context.cursor_signing_key.as_bytes());
        let before = before
            .map(|value| codec.decode::<HistoryCursor>(value, &binding))
            .transpose()
            .map_err(ConversationReadError::from)?;
        let after = after
            .map(|value| codec.decode::<HistoryAfterCursor>(value, &binding))
            .transpose()
            .map_err(ConversationReadError::from)?;
        let page = if let Some(cursor) = after {
            if cursor.after > cursor.membership_upper_bound {
                return Err(ConversationReadError::CursorOutsideCoverage.into());
            }
            let turns = forward_rows(
                &tx,
                agent_id,
                limit,
                &cursor.after,
                &cursor.membership_upper_bound,
            )?;
            ConversationSummaryPage {
                turns,
                active_turns: active_turn_rows(&tx, agent_id)?,
                pending_inputs: pending_input_rows(&tx, agent_id)?,
                membership_upper_bound: Some(cursor.membership_upper_bound),
                next_before: None,
                has_more: false,
            }
        } else {
            summary_page_in(
                &tx,
                agent_id,
                limit,
                before.as_ref().map(|c| &c.before),
                before.as_ref().map(|c| &c.membership_upper_bound),
            )?
        };
        let snapshot = window_snapshot(&tx, agent_id, context, page)?;
        tx.commit()?;
        Ok(Some(snapshot))
    }

    pub fn locate_source(
        &self,
        agent_id: &str,
        source_ref: &str,
        limit: usize,
        principal: &str,
        entitlement: &str,
    ) -> Result<Option<ConversationLocateResult>> {
        validate_limit("conversation history", limit, MAX_HISTORY_PAGE_LIMIT)?;
        let source = RuntimeRef::parse(source_ref).map_err(|error| {
            ConversationReadError::InvalidRequest(error.validation_error().into())
        })?;
        let mut connection = self.db.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let Some(context) = self.snapshot_context(&tx, agent_id, principal, entitlement)? else {
            return Ok(None);
        };
        let mut result = ConversationLocateResult {
            identity: context.identity.clone(),
            status: "source_only",
            source_ref: source.source_ref(),
            target: None,
            reason: Some("non_conversation_source"),
            runtime_id: context.runtime_id.clone(),
            event_log_epoch: context.event_log_epoch.clone(),
            visibility_scope_id: context.visibility_scope_id.clone(),
            window: None,
        };
        let (id, brief) = match source {
            RuntimeRef::Message { id } => (id, false),
            RuntimeRef::Brief { id } => (id, true),
            RuntimeRef::Turn { .. } | RuntimeRef::ToolExecution { .. } => {
                result.reason = Some("unsupported_conversation_source");
                tx.commit()?;
                return Ok(Some(result));
            }
            _ => {
                tx.commit()?;
                return Ok(Some(result));
            }
        };
        // Identity lookup is agent-scoped; never infer turn ownership from event or message sequences.
        let table = if brief { "briefs" } else { "messages" };
        let source_turn: Option<Option<String>> = tx
            .query_row(
                &format!("SELECT turn_id FROM {table} WHERE agent_id = ?1 AND evidence_id = ?2"),
                params![agent_id, id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(source_turn) = source_turn else {
            return Err(ConversationReadError::SourceNotFound.into());
        };
        let owner = if brief {
            source_turn
        } else {
            assigned_turn_id_tx(&tx, agent_id, &id)?
        };
        result.target = Some(if brief {
            ConversationLocateTarget::Brief {
                brief_id: id.clone(),
                turn_id: owner.clone(),
            }
        } else {
            ConversationLocateTarget::InputMessage {
                message_id: id.clone(),
                turn_id: owner.clone(),
                activity_id: None,
            }
        });
        if !brief && owner.is_none() {
            let pending: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id = ?1 AND message_id = ?2 AND status IN ('queued', 'dequeued'))",
                params![agent_id, id], |row| row.get(0),
            )?;
            if pending {
                result.status = "pending";
                result.reason = None;
                let page = summary_page_in_with_pending_target(
                    &tx,
                    agent_id,
                    limit,
                    None,
                    None,
                    Some(&id),
                )?;
                result.window = Some(window_snapshot(&tx, agent_id, context, page)?);
                tx.commit()?;
                return Ok(Some(result));
            }
        }
        let Some(owner) = owner else {
            result.reason = Some("unattributed_source");
            tx.commit()?;
            return Ok(Some(result));
        };
        let Some((target_turn, _)) =
            turn_summary_by_id_with_input(&tx, agent_id, &owner, (!brief).then_some(id.as_str()))?
        else {
            result.reason = Some("owner_turn_unavailable");
            tx.commit()?;
            return Ok(Some(result));
        };
        if let Some(ConversationLocateTarget::InputMessage {
            activity_id: target_activity,
            ..
        }) = result.target.as_mut()
        {
            *target_activity = tx.query_row(
                "SELECT source_id FROM conversation_source_revisions WHERE agent_id = ?1 AND turn_id = ?2 AND source_kind = 'operator' AND source_id = ?3",
                params![agent_id, owner, id], |row| row.get::<_, String>(0),
            ).optional()?.map(|id| activity_id(SOURCE_OPERATOR, &id));
        }
        let upper = latest_turn_key(&tx, agent_id)?.expect("target turn exists");
        let older_limit = (limit - 1) / 2;
        let mut turns = if older_limit > 0 {
            let mut rows =
                history_page_rows(&tx, agent_id, older_limit, Some(&target_turn.key), &upper)?;
            rows.truncate(older_limit);
            rows.reverse();
            rows
        } else {
            Vec::new()
        };
        let newer_limit = limit - 1 - turns.len();
        let newer = forward_rows(&tx, agent_id, newer_limit, &target_turn.key, &upper)?;
        turns.push(target_turn);
        turns.extend(newer);
        let page = ConversationSummaryPage {
            turns,
            active_turns: active_turn_rows(&tx, agent_id)?,
            pending_inputs: pending_input_rows(&tx, agent_id)?,
            membership_upper_bound: Some(upper),
            next_before: None,
            has_more: false,
        };
        result.status = "located";
        result.reason = None;
        result.window = Some(window_snapshot(&tx, agent_id, context, page)?);
        tx.commit()?;
        Ok(Some(result))
    }
}

fn forward_rows(
    connection: &Connection,
    agent_id: &str,
    limit: usize,
    after: &TurnKey,
    upper: &TurnKey,
) -> Result<Vec<ConversationTurnSummary>> {
    let mut statement = connection.prepare(HISTORY_AFTER_SQL)?;
    let mut rows = statement
        .query_map(
            params![
                agent_id,
                i64::try_from(after.turn_index)?,
                after.turn_id,
                i64::try_from(upper.turn_index)?,
                upper.turn_id,
                i64::try_from(limit)?
            ],
            decode_turn_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.iter_mut()
        .try_for_each(|row| hydrate_turn_summary(connection, row))?;
    Ok(rows.into_iter().map(|row| row.summary).collect())
}

fn window_snapshot(
    connection: &Connection,
    agent_id: &str,
    context: ConversationSnapshotContext,
    mut page: ConversationSummaryPage,
) -> Result<ConversationSnapshot<ConversationSummaryPage>> {
    let binding = context.binding(agent_id);
    let codec = CursorCodec::new(context.cursor_signing_key.as_bytes());
    let mut before = None;
    let mut after = None;
    if let (Some(first), Some(last), Some(upper)) = (
        page.turns.first(),
        page.turns.last(),
        page.membership_upper_bound.as_ref(),
    ) {
        let older: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM turn_records WHERE agent_id = ?1 AND (turn_index, turn_id) < (?2, ?3))",
            params![agent_id, i64::try_from(first.key.turn_index)?, first.turn_id], |row| row.get(0))?;
        let newer: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM turn_records WHERE agent_id = ?1 AND (turn_index, turn_id) > (?2, ?3) AND (turn_index, turn_id) <= (?4, ?5))",
            params![agent_id, i64::try_from(last.key.turn_index)?, last.turn_id, i64::try_from(upper.turn_index)?, upper.turn_id], |row| row.get(0))?;
        if older {
            before = Some(codec.encode(&HistoryCursor {
                binding: binding.clone(),
                before: first.key.clone(),
                membership_upper_bound: upper.clone(),
            }));
        }
        if newer {
            after = Some(codec.encode(&HistoryAfterCursor {
                binding: binding.clone(),
                after: last.key.clone(),
                membership_upper_bound: upper.clone(),
            }));
        }
        page.has_more = older;
        page.next_before = older.then(|| first.key.clone());
    }
    Ok(ConversationSnapshot {
        identity: context.identity,
        runtime_id: context.runtime_id,
        event_log_epoch: context.event_log_epoch,
        visibility_scope_id: context.visibility_scope_id,
        event_head_seq: context.event_head_seq,
        oldest_retained_seq: context.oldest_retained_seq,
        snapshot_cursor: codec.encode(&StreamCursor {
            binding,
            event_seq: context.event_head_seq,
        }),
        next_before_cursor: before,
        next_after_cursor: after,
        value: page,
    })
}
