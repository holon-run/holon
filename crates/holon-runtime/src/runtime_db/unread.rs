//! Per-agent Brief read cursors and exact unread counts.

use anyhow::Result;
use holon_storage::unread::{
    self as storage, BriefReadScope, BriefReadStateFastPath, MarkBriefReadFastPath,
};
use rusqlite::TransactionBehavior;

pub use holon_storage::unread::{BriefReadState, MarkBriefReadResult};
use std::time::Instant;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
#[path = "unread_scope_tests.rs"]
mod scope_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
#[path = "unread_fast_path_tests.rs"]
mod fast_path_tests;

impl crate::runtime_db::RuntimeDb {
    pub fn brief_read_state(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        self.brief_read_state_with_scope(
            principal_id,
            BriefReadScope::Explicit(visibility_scope_id),
            agent_id,
        )
    }

    pub fn observer_brief_read_state(
        &self,
        principal_id: &str,
        entitlement: &str,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        self.brief_read_state_with_scope(
            principal_id,
            BriefReadScope::Observer(entitlement),
            agent_id,
        )
    }

    fn brief_read_state_with_scope(
        &self,
        principal_id: &str,
        scope: BriefReadScope<'_>,
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
        scope: BriefReadScope<'_>,
        agent_id: &str,
    ) -> Result<BriefReadStateFastPath> {
        self.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            storage::brief_read_state_fast_path(&transaction, principal_id, scope, agent_id)
        })
    }

    fn brief_read_state_pin_cursor(
        &self,
        principal_id: &str,
        scope: BriefReadScope<'_>,
        agent_id: &str,
    ) -> Result<Option<BriefReadState>> {
        self.transaction(|transaction| {
            storage::brief_read_state_pin_cursor(transaction, principal_id, scope, agent_id)
        })
    }

    pub fn brief_read_states(
        &self,
        principal_id: &str,
        visibility_scope_id: &str,
    ) -> Result<Vec<BriefReadState>> {
        self.brief_read_states_with_scope(
            principal_id,
            BriefReadScope::Explicit(visibility_scope_id),
        )
    }

    pub fn observer_brief_read_states(
        &self,
        principal_id: &str,
        entitlement: &str,
    ) -> Result<Vec<BriefReadState>> {
        self.brief_read_states_with_scope(principal_id, BriefReadScope::Observer(entitlement))
    }

    fn brief_read_states_with_scope(
        &self,
        principal_id: &str,
        scope: BriefReadScope<'_>,
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
            let states =
                storage::brief_read_states(&transaction, principal_id, &visibility_scope_id)?;
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
            BriefReadScope::Explicit(visibility_scope_id),
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
            BriefReadScope::Observer(entitlement),
            agent_id,
            requested_cursor,
        )
    }

    fn mark_brief_read_with_scope(
        &self,
        principal_id: &str,
        scope: BriefReadScope<'_>,
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
        scope: BriefReadScope<'_>,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<MarkBriefReadFastPath> {
        self.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let result = storage::mark_brief_read_fast_path(
                &transaction,
                principal_id,
                scope,
                agent_id,
                requested_cursor,
            )?;
            if let MarkBriefReadFastPath::Noop(result) = &result {
                tracing::debug!(
                    target: "holon::http::unread",
                    requested_cursor,
                    read_through_event_seq = result.state.read_through_event_seq,
                    "mark brief read answered from reader pool"
                );
            }
            Ok(result)
        })
    }

    fn mark_brief_read_via_writer(
        &self,
        principal_id: &str,
        scope: BriefReadScope<'_>,
        agent_id: &str,
        requested_cursor: u64,
    ) -> Result<Option<MarkBriefReadResult>> {
        self.transaction(|transaction| {
            storage::mark_brief_read_via_writer(
                transaction,
                principal_id,
                scope,
                agent_id,
                requested_cursor,
            )
        })
    }
}
