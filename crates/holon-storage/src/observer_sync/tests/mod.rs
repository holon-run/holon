use super::*;
use crate::diagnostics::NO_DIAGNOSTICS;

mod brief_linkage;
mod event_epoch;
mod foundation_errors;
mod inventory;
mod projection_members;
mod state_identity;

use crate::tests::{seed_runtime_metadata, write_identity, MATERIAL};

const SYNC: ObserverSync = ObserverSync {
    material: MATERIAL,
    active_material: || Ok(None),
    check_active_budget: || Ok(()),
    write_probe_identity: write_identity,
    diagnostics: &NO_DIAGNOSTICS,
};

fn migrated_connection() -> Result<Connection> {
    let connection = crate::tests::migrated_connection()?;
    seed_runtime_metadata(&connection)?;
    Ok(connection)
}
