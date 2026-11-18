//! Conversation source parsing and scoped read entry points.

use super::*;
use crate::domain::conversation::{HistoryAfterCursor, HistoryCursor};
use crate::memory::refs::RuntimeRef;
use holon_storage::conversation::{
    active_turn_rows, forward_rows, locate_source_in, pending_input_rows, window_snapshot,
    ConversationSource,
};
pub use holon_storage::conversation::{ConversationLocateResult, ConversationLocateTarget};

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
        self.db.with_read(|connection| {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let Some(context) = self.snapshot_context(&tx, agent_id, principal, entitlement)?
            else {
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
        })
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
        self.db.with_read(|connection| {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let Some(context) = self.snapshot_context(&tx, agent_id, principal, entitlement)?
            else {
                return Ok(None);
            };
            let source_ref = source.source_ref();
            let source = match source {
                RuntimeRef::Message { id } => ConversationSource::Message(id),
                RuntimeRef::Brief { id } => ConversationSource::Brief(id),
                RuntimeRef::Turn { .. } | RuntimeRef::ToolExecution { .. } => {
                    ConversationSource::Unsupported
                }
                _ => ConversationSource::Other,
            };
            let result = locate_source_in(&tx, agent_id, source_ref, source, limit, context)?;
            tx.commit()?;
            Ok(Some(result))
        })
    }
}
