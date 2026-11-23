//! Access for runtime tests exercising the storage queries with a real budget.

use super::*;

pub fn persist_verification(
    connection: &Connection,
    capability: &str,
    verified: bool,
    verified_at: &str,
    detail: &str,
) -> Result<()> {
    super::persist_verification(connection, capability, verified, verified_at, detail)
}

pub fn current_event_projection_effect_verification(
    connection: &Connection,
) -> Result<Option<bool>> {
    super::current_event_projection_effect_verification(connection)
}

pub fn reusable_event_projection_effect_verification(
    connection: &Connection,
    event_log_epoch: &str,
) -> Result<Option<bool>> {
    super::reusable_event_projection_effect_verification(connection, event_log_epoch)
}

pub fn event_projection_effect_proof(
    connection: &Connection,
    event_log_epoch: &str,
) -> Result<Option<EventProjectionEffectProof>> {
    super::event_projection_effect_proof(connection, event_log_epoch)
}

pub struct ObserverSyncTestSupport<'a>(pub &'a ObserverSync);

impl ObserverSyncTestSupport<'_> {
    pub fn verification_success_detail(&self, detail: &impl serde::Serialize) -> Result<String> {
        self.0.verification_success_detail(detail)
    }

    pub fn verification_error_detail(
        &self,
        error: &anyhow::Error,
        source: Option<&str>,
    ) -> Result<String> {
        self.0.verification_error_detail(error, source)
    }

    pub fn verify_projection_snapshot_view(&self, connection: &Connection) -> Result<bool> {
        self.0.verify_projection_snapshot_view(connection)
    }

    pub fn verify_event_projection_effect_inventory(
        &self,
        connection: &Connection,
    ) -> Result<bool> {
        self.0.verify_event_projection_effect_inventory(connection)
    }

    pub fn inventory_diagnostic(&self, arguments: std::fmt::Arguments<'_>) -> Result<String> {
        self.0.inventory_diagnostic(arguments)
    }

    pub fn observer_verification_timestamp(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String> {
        self.0.observer_verification_timestamp(now)
    }

    pub fn probe_reservation_enforcement(&self, connection: &mut Connection) -> Result<bool> {
        self.0.probe_reservation_enforcement(connection)
    }

    pub fn count_agent_state_identity_mismatches(&self, connection: &Connection) -> Result<u64> {
        self.0.count_agent_state_identity_mismatches(connection)
    }

    pub fn read_metadata(&self, connection: &Connection, key: &str) -> Result<String> {
        self.0.read_metadata(connection, key)
    }
}
