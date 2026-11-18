//! Execution protocol row persistence and durable authority fences.

use anyhow::{anyhow, bail, ensure, Context, Result};
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct V113WorkItemRecord {
    source_revision: u64,
    state: V113WorkItemState,
}

#[derive(serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum V113WorkItemState {
    NeedsRepair { generation: u64, repair_id: String },
}

impl V113WorkItemRecord {
    fn into_paused(self, marker: &str) -> Result<execution_protocol::WorkItemExecutionRecord> {
        let V113WorkItemState::NeedsRepair {
            generation,
            repair_id,
        } = self.state;
        let source_revision = self.source_revision;
        ensure!(
            marker == format!("{source_revision}:{generation}:paused"),
            "legacy WorkExecution body does not match its v113 projection"
        );
        Ok(execution_protocol::WorkItemExecutionRecord {
            source_revision,
            state: WorkItemExecutionState::Paused {
                generation,
                reason: repair_id,
            },
        })
    }
}

/// Check the preserved pre-v113 body against the migrated storage columns.
/// The caller must separately verify the scoped qualification and raw digest.
pub fn is_v113_work_item_payload(payload: &str, marker: &str) -> bool {
    serde_json::from_str::<V113WorkItemRecord>(payload)
        .ok()
        .is_some_and(|record| record.into_paused(marker).is_ok())
}

/// v113 changed the lifecycle column but left the old body in place. Reading
/// that body preserves the pause; revision alignment is not execution authority.
pub fn decode_work_item_record_with_migration(
    payload: &str,
    marker: &str,
) -> Result<execution_protocol::WorkItemExecutionRecord> {
    match serde_json::from_str(payload) {
        Ok(record) => Ok(record),
        Err(current_error) => match serde_json::from_str::<V113WorkItemRecord>(payload) {
            Ok(legacy) => legacy.into_paused(marker),
            Err(_) => Err(current_error.into()),
        },
    }
}

/// Rewrite a stored legacy WorkItem `needs_repair` outcome payload with the
/// same mapping the load path applies. Returns `Ok(None)` for every other
/// payload. A structurally damaged legacy row fails closed instead of being
/// silently legalized.
pub fn rewrite_outcome_needs_repair(payload: &str) -> Result<Option<String>> {
    let mut value: serde_json::Value = serde_json::from_str(payload)?;
    if !migrate_outcome_needs_repair_in_place(&mut value)? {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&value)?))
}

/// The single retired `needs_repair` mapping shared by the decoder and the
/// data migration: an interrupted attempt whose reason preserves the original
/// repair identifier. Only the outcome body changes; the rest of the envelope
/// is preserved verbatim.
fn migrate_outcome_needs_repair_in_place(value: &mut serde_json::Value) -> Result<bool> {
    let Some(envelope) = value.get("outcome") else {
        return Ok(false);
    };
    let Some(inner) = envelope.get("outcome") else {
        return Ok(false);
    };
    if inner.get("kind").and_then(serde_json::Value::as_str) != Some("needs_repair") {
        return Ok(false);
    }
    let owner = envelope
        .get("owner")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if owner != "work_item" {
        bail!("legacy needs_repair outcome carries unexpected owner {owner:?}");
    }
    let repair_id = inner
        .get("repair_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("legacy needs_repair outcome lacks repair_id"))?
        .to_owned();
    value["outcome"] = serde_json::json!({
        "owner": "work_item",
        "outcome": { "kind": "interrupted", "reason": repair_id },
    });
    Ok(true)
}

/// Decode a stored execution outcome, translating the removed WorkItem
/// `needs_repair` outcome into the closest surviving record: an interrupted
/// attempt whose reason preserves the original repair identifier.
pub fn decode_outcome_record_with_migration(payload: &str) -> Result<ExecutionOutcomeRecord> {
    let mut value: serde_json::Value = serde_json::from_str(payload)?;
    migrate_outcome_needs_repair_in_place(&mut value)?;
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
    use holon_domain::execution_protocol::{
        ConversationOutcome, ExecutionOutcome, WaitReference, WorkItemOutcome,
    };
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
    fn legacy_needs_repair_record_requires_the_v113_projection_and_stays_paused() {
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
            decode_work_item_record_with_migration(&legacy, "4:3:paused").unwrap(),
            WorkItemExecutionRecord {
                source_revision: 4,
                state: WorkItemExecutionState::Paused {
                    generation: 3,
                    reason: "work_item_waits_ambiguous:work-a".into(),
                }
            }
        );
        for wrong_marker in [
            "5:3:paused",
            "4:4:paused",
            "4:3:runnable",
            "4:3:needs_repair",
        ] {
            assert!(decode_work_item_record_with_migration(&legacy, wrong_marker).is_err());
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
            decode_work_item_record_with_migration(&payload, "2:1:runnable").unwrap(),
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

    fn legacy_needs_repair_payload() -> serde_json::Value {
        serde_json::json!({
            "outcome_id": "outcome:message:msg-1",
            "attempt_id": "attempt-1",
            "outcome": {
                "owner": "work_item",
                "outcome": {
                    "kind": "needs_repair",
                    "repair_id": "provider_dispatch_rejected:turn-1"
                }
            },
            "created_at": "2026-08-05T00:00:00Z"
        })
    }

    #[test]
    fn retired_needs_repair_rewrite_is_field_equivalent_to_the_decoder() {
        let legacy = legacy_needs_repair_payload().to_string();
        let decoded = decode_outcome_record_with_migration(&legacy).unwrap();
        let rewritten = rewrite_outcome_needs_repair(&legacy)
            .unwrap()
            .expect("legacy needs_repair payload is rewritten");
        assert_eq!(
            serde_json::from_str::<ExecutionOutcomeRecord>(&rewritten).unwrap(),
            decoded,
            "migration rewrite and load-time decode must agree field by field"
        );
        let value: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(value["outcome"]["owner"], "work_item");
        assert_eq!(value["outcome"]["outcome"]["kind"], "interrupted");
        assert_eq!(
            value["outcome"]["outcome"]["reason"],
            "provider_dispatch_rejected:turn-1"
        );
        assert_eq!(value["outcome_id"], "outcome:message:msg-1");
        assert_eq!(value["attempt_id"], "attempt-1");
        assert_eq!(value["created_at"], "2026-08-05T00:00:00Z");
        // The rewrite is idempotent.
        assert_eq!(rewrite_outcome_needs_repair(&rewritten).unwrap(), None);
    }

    #[test]
    fn retired_needs_repair_rewrite_preserves_other_outcomes() {
        let wait = wait_outcome("wait-9");
        assert_eq!(
            rewrite_outcome_needs_repair(&serde_json::to_string(&wait).unwrap()).unwrap(),
            None
        );
        let conversation = ExecutionOutcomeRecord {
            outcome_id: "outcome-2".into(),
            attempt_id: "attempt-2".into(),
            outcome: ExecutionOutcome::Conversation(ConversationOutcome::Replied),
            created_at: "2026-08-05T00:00:00Z".into(),
        };
        assert_eq!(
            rewrite_outcome_needs_repair(&serde_json::to_string(&conversation).unwrap()).unwrap(),
            None
        );
    }

    #[test]
    fn damaged_retired_needs_repair_fails_closed() {
        let mut missing = legacy_needs_repair_payload();
        missing["outcome"]["outcome"]
            .as_object_mut()
            .unwrap()
            .remove("repair_id");
        let error = rewrite_outcome_needs_repair(&missing.to_string()).unwrap_err();
        assert!(error.to_string().contains("lacks repair_id"));

        let mut foreign_owner = legacy_needs_repair_payload();
        foreign_owner["outcome"]["owner"] = serde_json::json!("conversation");
        let error = rewrite_outcome_needs_repair(&foreign_owner.to_string()).unwrap_err();
        assert!(error.to_string().contains("unexpected owner"));
    }
}
