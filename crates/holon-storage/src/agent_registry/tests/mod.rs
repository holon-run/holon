use anyhow::Result;
use rusqlite::Connection;

use super::relations::AgentRelations;
use super::AgentRegistryMaterial;
use crate::migrations::{
    apply_migration, backfill_wait_condition_payload_columns, backfill_work_item_recheck_columns,
    ensure_migration_table, MIGRATIONS,
};

mod list;
mod relations;

const MATERIAL: AgentRegistryMaterial = AgentRegistryMaterial {
    admit_json_payload: |_| Ok(()),
    charge_owned_metadata: |_, _| Ok(()),
    admit_serialized: |_| Ok(()),
};

const RELATIONS: AgentRelations = AgentRelations { material: MATERIAL };

fn registry_connection() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS {
        // These fresh fixtures have no historical runtime data to qualify.
        apply_migration(&mut connection, migration, |_, _| Ok(()))?;
    }
    backfill_wait_condition_payload_columns(&connection)?;
    backfill_work_item_recheck_columns(&connection)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(connection)
}

fn name_key(name: &str) -> Result<String> {
    Ok(holon_domain::agent_name_key(name))
}
