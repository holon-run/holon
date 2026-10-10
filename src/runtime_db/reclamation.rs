//! Admission checks shared by parent cleanup and idle runtime retirement.
//!
//! These queries run in the deletion admission transaction. Producers check
//! the durable fence in the same transaction as their new work/reference.

use anyhow::{bail, Result};
use rusqlite::{OptionalExtension, Transaction};

/// Existing operator/one-shot deletion retains its cancellation semantics.
/// Parent cleanup closes new admission without cancelling accepted work.
pub(crate) fn ensure_work_admission_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
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
pub(crate) fn ensure_root_admission_tx(tx: &Transaction<'_>, root_id: &str) -> Result<()> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM execution_root_entries WHERE execution_root_id = ?1",
            [root_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(payload) = payload {
        let root: crate::types::ExecutionRootEntry = serde_json::from_str(&payload)?;
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

/// Durable state that proves live execution or scheduling. These block both
/// deletion admission and reminder delivery because the agent is not idle.
const EXECUTION_BLOCKER_CHECKS: &[(&str, &str)] = &[
    ("active_task", "SELECT EXISTS(SELECT 1 FROM tasks WHERE owner_agent_id = ?1 AND status IN ('queued','running','cancelling'))"),
    ("inbound_invocation", "SELECT EXISTS(SELECT 1 FROM tasks WHERE child_agent_id = ?1 AND status IN ('queued','running','cancelling'))"),
    ("queued_or_claimed_input", "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id = ?1 AND status IN ('queued','dequeued','interrupted'))"),
    ("accepted_delivery", "SELECT EXISTS(SELECT 1 FROM agent_message_deliveries WHERE target_agent_id = ?1 AND state IN ('queued','dispatched'))"),
    ("wait", "SELECT EXISTS(SELECT 1 FROM wait_conditions WHERE agent_id = ?1 AND status IN ('active','triggered'))"),
    ("pending_result_settlement", "SELECT EXISTS(SELECT 1 FROM task_result_settlements WHERE agent_id = ?1 AND state != 'settled')"),
    ("open_execution", "SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts WHERE agent_id = ?1 AND lifecycle_state = 'open')"),
    ("workspace_occupancy", "SELECT EXISTS(SELECT 1 FROM workspace_occupancies WHERE holder_agent_id = ?1 AND released_at IS NULL)"),
];

/// Durable retention state that outlives the current turn. The parent owns the
/// decision to keep or release it, so it is a diagnostic hint rather than a
/// blocker for deletion admission or reminder delivery.
const RETENTION_BLOCKER_CHECKS: &[(&str, &str)] = &[
    ("open_work_item", "SELECT EXISTS(SELECT 1 FROM work_items WHERE agent_id = ?1 AND state != 'completed')"),
    ("timer", "SELECT EXISTS(SELECT 1 FROM timers WHERE agent_id = ?1 AND status = 'active')"),
    ("timer_wake", "SELECT EXISTS(SELECT 1 FROM timers t JOIN timer_wakes w USING(timer_id) WHERE t.agent_id = ?1 AND w.status = 'pending')"),
];

/// Fail closed on unknown state. Historical rows alone are not live work.
fn runtime_state_blocker_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<String>> {
    let state: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(state) = state else {
        return Ok(Some("unknown_runtime_state".into()));
    };
    let state: crate::types::AgentState = serde_json::from_str(&state)?;
    if !matches!(
        state.status,
        crate::types::AgentStatus::AwakeIdle
            | crate::types::AgentStatus::Asleep
            | crate::types::AgentStatus::Stopped
    ) || state.current_run_id.is_some()
        || state.pending > 0
        || state.sleeping_until.is_some()
        || state.pending_wake_hint.is_some()
        || state.last_runtime_failure.is_some()
    {
        return Ok(Some("runtime_execution_or_wake".into()));
    }
    Ok(None)
}

// Each query uses an owner/target index. Keep these predicates aligned with
// producer fencing; a caller must never make work idle by cancelling it.
fn first_blocker_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    checks: &[(&str, &str)],
) -> Result<Option<String>> {
    for (reason, sql) in checks {
        if tx.query_row(sql, [agent_id], |row| row.get::<_, bool>(0))? {
            return Ok(Some((*reason).into()));
        }
    }
    Ok(None)
}

/// Full idle safety facts. Used by idle runtime retirement, which must keep
/// considering retention state before exiting a runtime.
pub(crate) fn idle_blocker_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<String>> {
    if let Some(blocker) = runtime_state_blocker_tx(tx, agent_id)? {
        return Ok(Some(blocker));
    }
    if let Some(blocker) = first_blocker_tx(tx, agent_id, EXECUTION_BLOCKER_CHECKS)? {
        return Ok(Some(blocker));
    }
    first_blocker_tx(tx, agent_id, RETENTION_BLOCKER_CHECKS)
}

/// Execution-only blockers: live runtime state plus active, queued, or waiting
/// work. Retention state (open work items, timers, timer wakes) is excluded so
/// callers can treat it as a hint instead of a hard stop.
pub(crate) fn execution_blocker_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<String>> {
    if let Some(blocker) = runtime_state_blocker_tx(tx, agent_id)? {
        return Ok(Some(blocker));
    }
    first_blocker_tx(tx, agent_id, EXECUTION_BLOCKER_CHECKS)
}

/// Deletion admission for a supervised child. Execution blockers, descendant
/// responsibility, and protected artifacts block cleanup. Retention state
/// (open work items, timers) and retained command-task output do not: the parent
/// decides whether they are still needed, and deletion later terminalizes or
/// cancels them through the normal phases.
pub(crate) fn parent_cleanup_blocker_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<Option<String>> {
    if let Some(blocker) = execution_blocker_tx(tx, agent_id)? {
        return Ok(Some(blocker));
    }
    for (reason, sql) in [
        ("descendant_responsibility", "SELECT EXISTS(SELECT 1 FROM agent_supervisions WHERE supervisor_agent_id = ?1 AND state != 'closed')"),
        ("protected_artifact", "SELECT EXISTS(SELECT 1 FROM artifact_metadata WHERE agent_id = ?1)"),
    ] {
        if tx.query_row(sql, [agent_id], |row| row.get::<_, bool>(0))? { return Ok(Some(reason.into())); }
    }
    Ok(None)
}

pub(crate) fn retirable_child_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<bool> {
    use crate::types::*;
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1 AND status = 'active'",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
    if identity.kind != AgentKind::Child
        || identity.visibility != AgentVisibility::Private
        || identity.ownership() != AgentOwnership::ParentSupervised
    {
        return Ok(false);
    }
    let relations = super::agent_relations::canonical_relations_from_connection(tx, agent_id)?;
    Ok(relations.is_some_and(|r| {
        r.resolution == AgentCanonicalResolution::Resolved
            && r.durability
                .is_some_and(|d| d.durability == AgentCanonicalDurability::Ephemeral)
            && r.lifecycle_attachment
                .is_some_and(|a| a.attachment == AgentLifecycleAttachment::SupervisionAttached)
            && r.supervision.is_some_and(|s| {
                matches!(
                    s.state,
                    AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
                )
            })
    }))
}

pub(crate) fn idle_evidence_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<String> {
    let (incarnation, state): (u64, String) = tx.query_row("SELECT COALESCE(json_extract(i.payload_json, '$.incarnation'), 1), COALESCE(s.payload_json, 'null') FROM agent_identities i LEFT JOIN agent_states s USING(agent_id) WHERE i.agent_id = ?1", [agent_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let state: Option<crate::types::AgentState> = serde_json::from_str(&state)?;
    let Some(state) = state else {
        return Ok(serde_json::to_string(&(
            incarnation,
            "unknown_runtime_state",
        ))?);
    };
    let latest_task: Option<String> = tx.query_row(
        "SELECT MAX(updated_at) FROM tasks WHERE owner_agent_id = ?1 OR child_agent_id = ?1",
        [agent_id],
        |row| row.get(0),
    )?;
    Ok(serde_json::to_string(&(
        incarnation,
        state.turn_index,
        state.total_message_count,
        state.last_turn_terminal,
        latest_task,
    ))?)
}
