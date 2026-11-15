//! Admission checks shared by parent cleanup and idle runtime retirement.
//!
//! These queries run in the deletion admission transaction. Producers check
//! the durable fence in the same transaction as their new work/reference.

use anyhow::{bail, Result};
use rusqlite::{OptionalExtension, Transaction};

/// Existing operator/one-shot deletion retains its cancellation semantics.
/// Parent cleanup closes new admission without cancelling accepted work.
pub fn ensure_work_admission_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    let fenced: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_deletion_jobs j
         JOIN agent_identities i ON i.agent_id = j.agent_id
         WHERE j.agent_id = ?1 AND i.status != 'active'
           AND json_extract(j.payload_json, '$.mode') = 'parent_cleanup')",
        [agent_id],
        |row| row.get(0),
    )?;
    if fenced {
        bail!("agent_cleanup_fenced: agent {agent_id} is being deleted");
    }
    Ok(())
}

/// A root cannot gain new users or change ownership while its owner is fenced.
pub fn ensure_root_admission_tx(tx: &Transaction<'_>, root_id: &str) -> Result<()> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM execution_root_entries WHERE execution_root_id = ?1",
            [root_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(payload) = payload {
        let root: holon_domain::ExecutionRootEntry = serde_json::from_str(&payload)?;
        if let Some(worktree) = root.worktree {
            for agent_id in worktree
                .authorized_agent_ids
                .iter()
                .chain(worktree.registered_by_agent_id.iter())
            {
                ensure_work_admission_tx(tx, agent_id)?;
            }
        }
    }
    Ok(())
}
