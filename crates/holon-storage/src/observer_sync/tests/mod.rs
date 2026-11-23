use super::*;
use crate::diagnostics::NO_DIAGNOSTICS;

mod brief_linkage;
mod event_epoch;
mod foundation_errors;
mod inventory;
mod projection_members;
mod state_identity;

const MATERIAL: AgentRegistryMaterial = AgentRegistryMaterial {
    admit_json_payload: |_| Ok(()),
    charge_owned_metadata: |_, _| Ok(()),
    admit_serialized: |_| Ok(()),
};
const SYNC: ObserverSync = ObserverSync {
    material: MATERIAL,
    active_material: || Ok(None),
    check_active_budget: || Ok(()),
    write_probe_identity: write_identity,
    diagnostics: &NO_DIAGNOSTICS,
};

fn write_identity(tx: &Transaction<'_>, identity: &AgentIdentityRecord) -> Result<()> {
    MATERIAL
        .upsert_agent_identity_tx(tx, identity, |name| Ok(holon_domain::agent_name_key(name)))?;
    Ok(())
}

fn migrated_connection() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    crate::migrations::migrate(&mut connection, |_, _| Ok(()))?;
    connection.execute(
        "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
         VALUES
           ('runtime_id', 'runtime-test', ?1, ?1),
           ('event_log_epoch', 'epoch-test', ?1, ?1),
           ('visibility_policy_generation', '0', ?1, ?1),
           ('conversation_cursor_signing_key', 'cursor-test', ?1, ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        ["2026-01-01T00:00:00.000Z"],
    )?;
    Ok(connection)
}
