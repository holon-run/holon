use anyhow::Result;
use holon_domain::AgentIdentityRecord;
use rusqlite::{Connection, Transaction};

use crate::agent_registry::AgentRegistryMaterial;

pub(crate) fn migrated_connection() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    crate::migrations::migrate(&mut connection, |_, _| Ok(()))?;
    Ok(connection)
}

pub(crate) const MATERIAL: AgentRegistryMaterial = AgentRegistryMaterial {
    admit_json_payload: |_| Ok(()),
    charge_owned_metadata: |_, _| Ok(()),
    admit_serialized: |_| Ok(()),
};

pub(crate) fn write_identity(tx: &Transaction<'_>, identity: &AgentIdentityRecord) -> Result<()> {
    MATERIAL
        .upsert_agent_identity_tx(tx, identity, |name| Ok(holon_domain::agent_name_key(name)))?;
    Ok(())
}

pub(crate) fn seed_runtime_metadata(connection: &Connection) -> Result<()> {
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
    Ok(())
}
