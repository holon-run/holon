//! Conversation repository facade and durable revision writes.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::domain::conversation::{
    ActivityKey, ConversationActivityPage, ConversationShadowDiagnostics, ConversationSummaryPage,
    CursorCodec, CursorDecodeError, DetailCursor, HistoryCursor, StreamCursor, TurnKey,
};
use crate::runtime_db::types::ConversationRepository;

use holon_storage::conversation::{
    activities_in, canonical_shadow_metadata, change_batch_in, compare_shadow_metadata,
    conversation_snapshot_context, summary_page_in, validate_limit, ConversationSnapshotContext,
};
pub(crate) use holon_storage::conversation::{assigned_turn_id_tx, SOURCE_OPERATOR, SOURCE_WAIT};
#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_storage::conversation::{turn_revision_tx, MAX_BRIEFS_PER_TURN};
pub use holon_storage::conversation::{
    ConversationChangeBatch, ConversationIdentity, ConversationReadError, ConversationResetReason,
    ConversationSnapshot, ConversationTurnRevision, MAX_ACTIVITIES_PAGE_LIMIT,
    MAX_CONVERSATION_CHANGE_ACTIVITIES, MAX_CONVERSATION_CHANGE_EVENTS,
    MAX_CONVERSATION_SHADOW_TURNS, MAX_HISTORY_PAGE_LIMIT,
};

pub(super) const REVISION_WRITES: holon_storage::conversation::RevisionWrites =
    holon_storage::conversation::RevisionWrites::new(
        crate::diagnostics::resource_io::admit_serialized_value,
    );

mod locate;
pub use locate::{ConversationLocateResult, ConversationLocateTarget};

#[cfg(all(test, holon_lib_test_shard = "b"))]
fn ensure_turn_revision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    updated_at: DateTime<Utc>,
) -> Result<ConversationTurnRevision> {
    REVISION_WRITES.ensure_turn_revision_tx(tx, agent_id, turn_id, updated_at)
}

pub(crate) fn bump_turn_revision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    summary_changed: bool,
    detail_changed: bool,
    updated_at: DateTime<Utc>,
) -> Result<ConversationTurnRevision> {
    REVISION_WRITES.bump_turn_revision_tx(
        tx,
        agent_id,
        turn_id,
        summary_changed,
        detail_changed,
        updated_at,
    )
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
fn settle_turn_result_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    updated_at: DateTime<Utc>,
) -> Result<ConversationTurnRevision> {
    REVISION_WRITES.settle_turn_result_tx(tx, agent_id, turn_id, updated_at)
}

pub(crate) fn bump_source_revision_tx(
    tx: &Transaction<'_>,
    source_kind: &str,
    source_id: &str,
    agent_id: &str,
    turn_id: Option<&str>,
    updated_at: DateTime<Utc>,
) -> Result<u64> {
    REVISION_WRITES.bump_source_revision_tx(
        tx,
        source_kind,
        source_id,
        agent_id,
        turn_id,
        updated_at,
    )
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
fn assign_input_to_turn_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
    turn_id: &str,
    assigned_at: DateTime<Utc>,
) -> Result<bool> {
    REVISION_WRITES.assign_input_to_turn_tx(tx, agent_id, message_id, turn_id, assigned_at)
}

impl ConversationRepository<'_> {
    /// Fence protocol snapshots in their read transaction, before cursor decoding.
    pub fn with_expected_incarnation(mut self, expected_incarnation: Option<u64>) -> Self {
        self.expected_incarnation = expected_incarnation;
        self
    }

    fn snapshot_context(
        &self,
        connection: &Connection,
        agent_id: &str,
        principal: &str,
        entitlement: &str,
    ) -> Result<Option<ConversationSnapshotContext>> {
        let context = conversation_snapshot_context(connection, agent_id, principal, entitlement)?;
        if context.is_none() && self.expected_incarnation.is_some() {
            return Err(ConversationReadError::IdentityChanged.into());
        }
        if let Some(context) = &context {
            if self
                .expected_incarnation
                .is_some_and(|expected| expected != context.identity.agent_incarnation)
            {
                return Err(ConversationReadError::IdentityChanged.into());
            }
        }
        Ok(context)
    }

    pub fn brief_snapshot(
        &self,
        agent_id: &str,
        brief_id: &str,
    ) -> Result<Option<(ConversationIdentity, crate::types::BriefRecord)>> {
        self.db.with_read(|connection| {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let Some(context) = self.snapshot_context(&tx, agent_id, "", "")? else {
                return Ok(None);
            };
            let brief = holon_storage::conversation::brief_in(&tx, agent_id, brief_id)?;
            tx.commit()?;
            Ok(brief.map(|brief| (context.identity, brief)))
        })
    }

    pub fn summary_page(
        &self,
        agent_id: &str,
        limit: usize,
        before: Option<&TurnKey>,
        membership_upper_bound: Option<&TurnKey>,
    ) -> Result<ConversationSummaryPage> {
        validate_limit("conversation history", limit, MAX_HISTORY_PAGE_LIMIT)?;
        self.db.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let page = summary_page_in(
                &transaction,
                agent_id,
                limit,
                before,
                membership_upper_bound,
            )?;
            transaction.commit()?;
            Ok(page)
        })
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
        self.db.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let Some(context) =
                self.snapshot_context(&transaction, agent_id, scope_principal, scope_entitlement)?
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
        })
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
        self.db.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let Some(context) =
                self.snapshot_context(&transaction, agent_id, scope_principal, scope_entitlement)?
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
                identity: context.identity,
                runtime_id: context.runtime_id,
                event_log_epoch: context.event_log_epoch,
                visibility_scope_id: context.visibility_scope_id,
                event_head_seq: context.event_head_seq,
                oldest_retained_seq: context.oldest_retained_seq,
                snapshot_cursor,
                next_before_cursor,
                next_after_cursor: None,
                value: page,
            }))
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
        validate_limit("conversation activity", limit, MAX_ACTIVITIES_PAGE_LIMIT)?;
        self.db.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
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
        })
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
        self.db.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let Some(context) =
                self.snapshot_context(&transaction, agent_id, scope_principal, scope_entitlement)?
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
                return Err(
                    ConversationReadError::Cursor(CursorDecodeError::BindingMismatch).into(),
                );
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
                identity: context.identity,
                runtime_id: context.runtime_id,
                event_log_epoch: context.event_log_epoch,
                visibility_scope_id: context.visibility_scope_id,
                event_head_seq: context.event_head_seq,
                oldest_retained_seq: context.oldest_retained_seq,
                snapshot_cursor,
                next_before_cursor,
                next_after_cursor: None,
                value: page,
            }))
        })
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
        self.db.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let Some(context) =
                self.snapshot_context(&transaction, agent_id, scope_principal, scope_entitlement)?
            else {
                transaction.commit()?;
                return Ok(None);
            };
            let batch = change_batch_in(
                &transaction,
                agent_id,
                after_cursor,
                event_limit,
                activity_limit,
                context,
            )?;
            transaction.commit()?;
            Ok(Some(batch))
        })
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod revision_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod assignment_budget_tests;
