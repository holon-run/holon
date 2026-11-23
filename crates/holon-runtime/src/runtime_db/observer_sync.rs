use anyhow::Result;
use holon_storage::observer_sync::{read_foundations, ObserverSync};
use rusqlite::{Connection, TransactionBehavior};
use std::sync::atomic::Ordering;

pub(crate) use holon_storage::observer_sync::{parse_work_item_plan_status, parse_work_item_state};
pub use holon_storage::observer_sync::{
    AgentEventRecoveryWindow, AgentProjectionSnapshotRow, AgentProjectionSnapshotRows,
    AgentProjectionWorkItemRow, AgentRosterLatestBriefRow, AgentRosterRow, AgentRosterSnapshotRows,
    ObserverSyncFoundationVerification, PUBLIC_SCOPE_ENTITLEMENT, PUBLIC_SCOPE_PRINCIPAL,
};

#[cfg(all(test, holon_lib_test_shard = "b"))]
pub(super) use holon_storage::observer_sync::{
    AGENT_IDENTITY_RESERVED, AGENT_LATEST_BRIEF_SQL, AGENT_ROSTER_LATEST_BRIEFS_SQL,
    EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
};

#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_domain::AuditEvent;
#[cfg(test)]
use holon_domain::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
    AgentVisibility,
};
#[cfg(test)]
use holon_storage::observer_sync::test_support::ObserverSyncTestSupport;
#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_storage::observer_sync::test_support::{
    current_event_projection_effect_verification, event_projection_effect_proof,
    persist_verification, reusable_event_projection_effect_verification,
};
#[cfg(all(test, holon_lib_test_shard = "b"))]
use holon_storage::observer_sync::{
    VerificationSuccessDetail, BRIEF_ATOMIC_LINKAGE_VERIFIED, CONVERSATION_READ_VERIFIED,
    EVENT_PROJECTION_EFFECT_COMPLETE, PROJECTION_SNAPSHOT_VERIFIED, ROSTER_SNAPSHOT_VERIFIED,
    RUNTIME_IDENTITY_STABLE,
};
#[cfg(test)]
const SYNC_TEST: ObserverSyncTestSupport<'_> = ObserverSyncTestSupport(&OBSERVER_SYNC);

#[cfg(all(test, holon_lib_test_shard = "b"))]
fn count_rows(connection: &Connection, sql: &str) -> Result<u64> {
    Ok(connection.query_row(sql, [], |row| row.get(0))?)
}

const OBSERVER_SYNC: ObserverSync = ObserverSync {
    material: super::AGENT_REGISTRY_MATERIAL,
    active_material: crate::runtime::agent_observation::active_observation_material,
    check_active_budget: crate::diagnostics::resource_io::check_active_budget,
    write_probe_identity: super::evidence::upsert_agent_identity_tx,
    diagnostics: &super::storage_diagnostics::DIAGNOSTICS,
};

pub(crate) fn verify_observer_sync_foundations(connection: &mut Connection) -> Result<()> {
    OBSERVER_SYNC.verify_observer_sync_foundations(connection)
}

pub(crate) fn record_appended_event_projection_effect(
    connection: &Connection,
    event: &holon_domain::AuditEvent,
) -> Result<()> {
    OBSERVER_SYNC.record_appended_event_projection_effect(connection, event)
}

pub(crate) fn agent_event_recovery_window_on(
    connection: &Connection,
    agent_id: Option<&str>,
) -> Result<AgentEventRecoveryWindow> {
    OBSERVER_SYNC.agent_event_recovery_window_on(connection, agent_id)
}

impl crate::runtime_db::RuntimeDb {
    /// Read-side self-heal throttle: while the persisted S1 verdict is
    /// failing, the verification is re-run at most once per window.
    const OBSERVER_SYNC_SELF_HEAL_MIN_INTERVAL_MS: u64 = 60_000;

    /// Loads the persisted S1 foundation verification, re-running the
    /// verification at most once per throttle window while the persisted
    /// verdict is failing. This is the daemon's only recovery path from a
    /// verification that was persisted against a since-converged database:
    /// without it a stale failing row keeps the observer-sync capabilities
    /// disabled until the next daemon restart. A passing verdict is never
    /// re-checked here, so the heal can never downgrade it.
    pub fn observer_sync_foundations_with_self_heal(
        &self,
    ) -> Result<ObserverSyncFoundationVerification> {
        let foundations = self.observer_sync_foundations()?;
        if foundations.runtime_identity_stable && foundations.agent_identity_reserved {
            return Ok(foundations);
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let last_attempt_ms = self
            .observer_sync_self_heal_last_attempt_ms
            .load(Ordering::Relaxed);
        if last_attempt_ms != 0
            && now_ms.saturating_sub(last_attempt_ms)
                < Self::OBSERVER_SYNC_SELF_HEAL_MIN_INTERVAL_MS
        {
            return Ok(foundations);
        }
        if self
            .observer_sync_self_heal_last_attempt_ms
            .compare_exchange(
                last_attempt_ms,
                now_ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            // Another caller won the heal slot for this window.
            return Ok(foundations);
        }
        let mut connection = self.connection()?;
        match verify_observer_sync_foundations(&mut connection) {
            Ok(()) => {
                let healed = self.observer_sync_foundations()?;
                if healed.agent_identity_reserved && !foundations.agent_identity_reserved {
                    tracing::info!(
                        "observer-sync identity-reservation verification self-healed \
                         after the database converged"
                    );
                }
                if healed.runtime_identity_stable && !foundations.runtime_identity_stable {
                    tracing::info!(
                        "observer-sync runtime-identity-stable verification self-healed \
                         after the database converged"
                    );
                }
                Ok(healed)
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "observer-sync self-heal re-verification failed; keeping the persisted verdict"
                );
                Ok(foundations)
            }
        }
    }

    /// Loads the durable observer-sync verification results. Missing rows
    /// read as false; load errors should degrade, not fail, the caller.
    pub fn observer_sync_foundations(&self) -> Result<ObserverSyncFoundationVerification> {
        self.with_read(|connection| read_foundations(connection))
    }

    /// Reads every roster anchor family — identity registry, active public
    /// membership, per-Agent committed event windows, latest canonical
    /// Briefs, and the runtime identity metadata — inside one deferred read
    /// transaction, so all values share one committed database view.
    pub fn agent_roster_snapshot_rows(&self) -> Result<AgentRosterSnapshotRows> {
        let _read = crate::diagnostics::attribution::ROSTER_READ.start();
        self.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let rows = OBSERVER_SYNC.collect_agent_roster_rows(&transaction)?;
            transaction.commit()?;
            Ok(rows)
        })
    }

    /// Reads one Agent's canonical projection anchors — membership,
    /// committed state, event window, current WorkItem, conversation
    /// anchors, and latest Brief — plus the runtime identity metadata
    /// inside one deferred read transaction, so all values share one
    /// committed database view and the boundary equals that view's
    /// per-Agent event head.
    pub fn agent_projection_snapshot_rows(
        &self,
        agent_id: &str,
    ) -> Result<AgentProjectionSnapshotRows> {
        let _read = crate::diagnostics::attribution::AGENT_PROJECTION_READ.start();
        self.with_read(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let rows = OBSERVER_SYNC.collect_agent_projection_anchors(&transaction, agent_id)?;
            transaction.commit()?;
            Ok(rows)
        })
    }

    /// Reads one Agent's committed event recovery window — epoch, oldest
    /// retained `event_seq`, and head `event_seq` — from a single SQL
    /// statement so all three values share one committed read view. Rich
    /// `cursor_not_found` errors and snapshot anchors must use this view,
    /// never a watcher or the sequence allocator's next value.
    pub fn agent_event_recovery_window(
        &self,
        agent_id: Option<&str>,
    ) -> Result<AgentEventRecoveryWindow> {
        self.with_read(|connection| agent_event_recovery_window_on(connection, agent_id))
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod verification_success_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod verification_error_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod projection_members_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod projection_scalar_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod roster_material_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod inventory_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod foundation_epoch_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod append_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod foundation_timestamp_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod foundation_probe_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod state_identity_budget_tests;

#[cfg(test)]
mod registry_verdict_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod foundation_metadata_budget_tests;

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod foundation_scope_tests;
