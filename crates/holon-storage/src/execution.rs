//! Execution protocol row persistence and durable authority fences.

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use holon_domain::execution_protocol::{
    self, ExecutionOutcomeRecord, ExecutionProtocolState, WorkItemExecutionState,
};
use holon_domain::{AgentIdentityRecord, AgentRegistryStatus, AgentStatus};
use rusqlite::{params, OptionalExtension, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionAuthorityFences {
    pub agent_control_revision: u64,
    pub host_registry_revision: u64,
}

pub fn authority_fences_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<ExecutionAuthorityFences> {
    let (status, control_revision) = tx
        .query_row(
            "SELECT status, control_revision
             FROM agent_states
             WHERE agent_id = ?1",
            [agent_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("execution admission requires durable agent control state"))?;
    let status: AgentStatus =
        serde_json::from_str(&format!("\"{status}\"")).context("decoding agent control status")?;
    if status == AgentStatus::Stopped {
        bail!("execution admission rejected because agent control state is stopped");
    }
    let agent_control_revision =
        u64::try_from(control_revision).context("agent control revision is negative")?;
    if agent_control_revision == 0 {
        bail!("agent control revision must be nonzero");
    }

    let identity_payload = tx
        .query_row(
            "SELECT payload_json
             FROM agent_identities
             WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("execution admission requires durable host identity"))?;
    let identity: AgentIdentityRecord =
        serde_json::from_str(&identity_payload).context("decoding host identity")?;
    if identity.status != AgentRegistryStatus::Active {
        bail!("execution admission rejected because host identity is not active");
    }
    let host_registry_revision = identity
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("host identity revision overflow"))?;

    Ok(ExecutionAuthorityFences {
        agent_control_revision,
        host_registry_revision,
    })
}

pub enum ExecutionRowKind {
    WorkItem,
    Attempt,
    Outcome,
}

pub fn persist_state_rows_tx(
    tx: &Transaction<'_>,
    state: &ExecutionProtocolState,
    previous: Option<&ExecutionProtocolState>,
    includes: &dyn Fn(ExecutionRowKind, &str) -> bool,
) -> Result<()> {
    execution_protocol::assert_invariants(state)
        .map_err(|error| anyhow!("invalid execution protocol state: {error}"))?;
    tx.execute(
        "INSERT INTO execution_protocol_partitions (agent_id, updated_at)
         VALUES (?1, ?2)
         ON CONFLICT(agent_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![state.agent_id, Utc::now().to_rfc3339()],
    )?;
    for (work_item_id, work_record) in &state.work_items {
        if !includes(ExecutionRowKind::WorkItem, work_item_id)
            || previous.is_some_and(|prior| prior.work_items.get(work_item_id) == Some(work_record))
        {
            continue;
        }
        tx.execute(
            "INSERT INTO execution_protocol_work_items (
               agent_id, work_item_id, source_revision, generation,
               lifecycle_state, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(agent_id, work_item_id) DO UPDATE SET
               source_revision = excluded.source_revision,
               generation = excluded.generation,
               lifecycle_state = excluded.lifecycle_state,
               payload_json = excluded.payload_json",
            params![
                state.agent_id,
                work_item_id,
                to_i64(work_record.source_revision)?,
                to_i64(work_record.generation())?,
                work_item_state_token(&work_record.state),
                serde_json::to_string(work_record)?,
            ],
        )?;
    }
    for attempt in state.attempts.values() {
        if !includes(ExecutionRowKind::Attempt, &attempt.attempt_id)
            || previous
                .is_some_and(|prior| prior.attempts.get(&attempt.attempt_id) == Some(attempt))
        {
            continue;
        }
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state,
               source_identity_json, source_generation, recovery_of_attempt_id,
               terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, attempt_id) DO UPDATE SET
               lifecycle_state = excluded.lifecycle_state,
               terminal_outcome_id = excluded.terminal_outcome_id,
               payload_json = excluded.payload_json",
            params![
                state.agent_id,
                attempt.attempt_id,
                enum_token(&attempt.state)?,
                serde_json::to_string(&attempt.source.identity)?,
                to_i64(attempt.source.generation)?,
                attempt.recovery_of_attempt_id,
                attempt.terminal_outcome_id,
                serde_json::to_string(attempt)?,
            ],
        )?;
    }
    for outcome in state.outcomes.values() {
        if !includes(ExecutionRowKind::Outcome, &outcome.outcome_id)
            || previous
                .is_some_and(|prior| prior.outcomes.get(&outcome.outcome_id) == Some(outcome))
        {
            continue;
        }
        let payload = serde_json::to_string(outcome)?;
        let existing = tx
            .query_row(
                "SELECT payload_json
                 FROM execution_protocol_outcomes
                 WHERE agent_id = ?1 AND outcome_id = ?2",
                params![state.agent_id, outcome.outcome_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if !stored_outcome_matches(&existing, outcome)? {
                bail!(
                    "execution outcome identity conflict for {}",
                    outcome.outcome_id
                );
            }
            continue;
        }
        tx.execute(
            "INSERT INTO execution_protocol_outcomes (
               agent_id, outcome_id, attempt_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                state.agent_id,
                outcome.outcome_id,
                outcome.attempt_id,
                payload,
            ],
        )?;
    }
    Ok(())
}

fn stored_outcome_matches(existing: &str, outcome: &ExecutionOutcomeRecord) -> Result<bool> {
    let existing: ExecutionOutcomeRecord = decode_outcome_record_with_migration(existing)
        .with_context(|| format!("decoding stored execution outcome {}", outcome.outcome_id))?;
    Ok(existing == *outcome)
}

/// Decode a stored WorkItemExecutionRecord, translating the removed
/// `needs_repair` state at load time: when the execution source revision has
/// caught up with the durable WorkItem revision the fence is gone and the
/// WorkItem is Runnable again; otherwise it pauses with the original repair
/// reason. `durable_revision` is the live `work_items.revision`, if any.
pub fn decode_work_item_record_with_migration(
    payload: &str,
    durable_revision: Option<u64>,
) -> Result<holon_domain::execution_protocol::WorkItemExecutionRecord> {
    let value: serde_json::Value = serde_json::from_str(payload)?;
    let legacy_state = value.get("state").filter(|state| {
        state.get("state").and_then(serde_json::Value::as_str) == Some("needs_repair")
    });
    let Some(legacy_state) = legacy_state else {
        return Ok(serde_json::from_value(value)?);
    };
    let source_revision = value
        .get("source_revision")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("legacy needs_repair record lacks source_revision"))?;
    let generation = legacy_state
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("legacy needs_repair record lacks generation"))?;
    let repair_id = legacy_state
        .get("repair_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("legacy needs_repair record lacks repair_id"))?
        .to_owned();
    let state = if durable_revision == Some(source_revision) {
        WorkItemExecutionState::Runnable {
            generation,
            recovery_ref: None,
        }
    } else {
        WorkItemExecutionState::Paused {
            generation,
            reason: repair_id,
        }
    };
    Ok(holon_domain::execution_protocol::WorkItemExecutionRecord {
        source_revision,
        state,
    })
}

/// Decode a stored execution outcome, translating the removed WorkItem
/// `needs_repair` outcome into the closest surviving record: an interrupted
/// attempt whose reason preserves the original repair identifier.
pub fn decode_outcome_record_with_migration(payload: &str) -> Result<ExecutionOutcomeRecord> {
    let mut value: serde_json::Value = serde_json::from_str(payload)?;
    let legacy = value
        .get("outcome")
        .filter(|outcome| {
            outcome.get("owner").and_then(serde_json::Value::as_str) == Some("work_item")
        })
        .and_then(|outcome| outcome.get("outcome"))
        .filter(|inner| {
            inner.get("kind").and_then(serde_json::Value::as_str) == Some("needs_repair")
        });
    if let Some(legacy) = legacy {
        let repair_id = legacy
            .get("repair_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("legacy needs_repair outcome lacks repair_id"))?;
        value["outcome"] = serde_json::json!({
            "owner": "work_item",
            "outcome": { "kind": "interrupted", "reason": repair_id },
        });
    }
    Ok(serde_json::from_value(value)?)
}

fn work_item_state_token(state: &WorkItemExecutionState) -> &'static str {
    match state {
        WorkItemExecutionState::Runnable { .. } => "runnable",
        WorkItemExecutionState::InFlight { .. } => "in_flight",
        WorkItemExecutionState::Waiting { .. } => "waiting",
        WorkItemExecutionState::Paused { .. } => "paused",
        WorkItemExecutionState::Terminal { .. } => "terminal",
    }
}

pub fn enum_token<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("execution protocol enum did not serialize to a string"))
}

pub fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("execution protocol generation exceeds SQLite INTEGER")
}

pub(crate) fn persist_migrated_state_tx(
    tx: &Transaction<'_>,
    state: &ExecutionProtocolState,
) -> Result<()> {
    crate::reclamation::ensure_work_admission_tx(tx, &state.agent_id)?;
    persist_state_rows_tx(tx, state, None, &|_, _| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use holon_domain::execution_protocol::{ExecutionOutcome, WaitReference, WorkItemOutcome};
    use holon_domain::execution_protocol::{WorkItemExecutionRecord, WorkItemExecutionState};

    fn wait_outcome(wait_id: &str) -> ExecutionOutcomeRecord {
        ExecutionOutcomeRecord {
            outcome_id: "outcome-1".into(),
            attempt_id: "attempt-1".into(),
            outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Wait {
                wait: WaitReference {
                    wait_id: wait_id.into(),
                },
            }),
            created_at: "2026-08-05T00:00:00Z".into(),
        }
    }

    #[test]
    fn stored_outcome_identity_ignores_removed_wait_generation_field() {
        let existing = serde_json::json!({
            "outcome_id": "outcome-1",
            "attempt_id": "attempt-1",
            "outcome": {
                "owner": "work_item",
                "outcome": {
                    "kind": "wait",
                    "wait": {
                        "wait_id": "wait-1",
                        "generation": 7
                    }
                }
            },
            "created_at": "2026-08-05T00:00:00Z"
        });
        assert!(stored_outcome_matches(&existing.to_string(), &wait_outcome("wait-1")).unwrap());
        assert!(!stored_outcome_matches(&existing.to_string(), &wait_outcome("wait-2")).unwrap());
    }

    #[test]
    fn legacy_needs_repair_record_migrates_by_revision_alignment() {
        let legacy = serde_json::json!({
            "source_revision": 4,
            "state": {
                "state": "needs_repair",
                "generation": 3,
                "repair_id": "work_item_waits_ambiguous:work-a"
            }
        })
        .to_string();
        assert_eq!(
            decode_work_item_record_with_migration(&legacy, Some(4)).unwrap(),
            WorkItemExecutionRecord {
                source_revision: 4,
                state: WorkItemExecutionState::Runnable {
                    generation: 3,
                    recovery_ref: None
                }
            },
            "an aligned revision means the fence is gone: Runnable again"
        );
        for unaligned in [Some(5u64), None] {
            assert_eq!(
                decode_work_item_record_with_migration(&legacy, unaligned).unwrap(),
                WorkItemExecutionRecord {
                    source_revision: 4,
                    state: WorkItemExecutionState::Paused {
                        generation: 3,
                        reason: "work_item_waits_ambiguous:work-a".into()
                    }
                },
                "unaligned or missing durable revision pauses with the original reason"
            );
        }
        // Current records pass through unchanged; the migration is idempotent.
        let current = WorkItemExecutionRecord {
            source_revision: 2,
            state: WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: Some("provider_dispatch_rejected:turn-1".into()),
            },
        };
        let payload = serde_json::to_string(&current).unwrap();
        assert_eq!(
            decode_work_item_record_with_migration(&payload, None).unwrap(),
            current
        );
    }

    #[test]
    fn legacy_needs_repair_outcome_migrates_to_interrupted() {
        let legacy = serde_json::json!({
            "outcome_id": "outcome-1",
            "attempt_id": "attempt-1",
            "outcome": {
                "owner": "work_item",
                "outcome": {
                    "kind": "needs_repair",
                    "repair_id": "provider_dispatch_rejected:turn-1"
                }
            },
            "created_at": "2026-08-05T00:00:00Z"
        });
        let migrated = decode_outcome_record_with_migration(&legacy.to_string()).unwrap();
        assert_eq!(
            migrated.outcome,
            ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted {
                reason: "provider_dispatch_rejected:turn-1".into()
            })
        );
        let wait = wait_outcome("wait-9");
        let payload = serde_json::to_string(&wait).unwrap();
        assert_eq!(
            decode_outcome_record_with_migration(&payload).unwrap(),
            wait
        );
    }
}
