//! Startup can finish memory only for the exact, durably cancelled producer.
use super::*;
use crate::runtime_db::current_run_abort::{settle_attempt, CurrentRunAbortIntent};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct AbortedAddress {
    intent_digest: String,
    terminal_digest: String,
    queue_digest: String,
    attempt_digest: Option<String>,
    outcome_digest: Option<String>,
}

pub(super) fn proof_material_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
) -> Result<(usize, usize)> {
    let key = binding.key();
    let invocation = binding.invocation();
    let intent = "SELECT * FROM current_run_abort_intents WHERE agent_id=?1 AND identity_incarnation=?4 AND run_id=(SELECT run_id FROM turn_records WHERE turn_id=?3)";
    let attempt = "SELECT a.* FROM execution_protocol_attempts a JOIN intent i ON a.agent_id=i.agent_id AND a.attempt_id=json_extract(i.payload_json,'$.activation_id')";
    let mut material = (0, 0);
    for selection in [
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(evidence_id AS BLOB)) size FROM messages WHERE evidence_id=?2",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(message_id AS BLOB)) size FROM queue_entries WHERE message_id=?2",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(turn_id AS BLOB)) size FROM turn_records WHERE turn_id=?3",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) size FROM agent_identities WHERE agent_id=?1",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(run_id AS BLOB))+length(CAST(source_message_id AS BLOB))+16 size FROM intent",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(attempt_id AS BLOB))+length(CAST(source_identity_json AS BLOB))+COALESCE(length(CAST(terminal_outcome_id AS BLOB)),0)+length(CAST(lifecycle_state AS BLOB))+16 size FROM attempt",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(outcome_id AS BLOB))+length(CAST(attempt_id AS BLOB)) size FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id IN (SELECT terminal_outcome_id FROM attempt)",
    ] {
        let (count, bytes): (usize, usize) = tx.query_row(
            &format!("WITH intent AS ({intent}), attempt AS ({attempt}) SELECT COUNT(*),COALESCE(SUM(size),0) FROM ({selection})"),
            params![key.agent_id, invocation.message_id, key.actual_dispatch_turn_id, key.identity_incarnation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        add_proof_material(&mut material, bytes, count)?;
    }
    let (bytes, records) = lineage::source_assignment_material_tx(tx, binding)?;
    add_proof_material(&mut material, bytes, records)?;
    Ok(material)
}

pub(super) fn read_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
    original: &OriginalAddress,
) -> Result<AbortedAddress> {
    let key = binding.key();
    let invocation = binding.invocation();
    let incarnation: u64 = tx.query_row(
        "SELECT json_extract(payload_json,'$.incarnation') FROM agent_identities WHERE agent_id=?1",
        [&key.agent_id],
        |row| row.get(0),
    )?;
    ensure!(
        incarnation == key.identity_incarnation,
        "operator abort belongs to another incarnation"
    );
    let (payload, kind, completed_at): (String, String, String) = tx.query_row(
        "SELECT json_extract(payload_json,'$.terminal'),terminal_kind,completed_at FROM turn_records WHERE agent_id=?1 AND turn_id=?2",
        params![key.agent_id, key.actual_dispatch_turn_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let terminal: serde_json::Value = serde_json::from_str(&payload)?;
    let at: DateTime<Utc> = terminal["completed_at"]
        .as_str()
        .context("original abort has no terminal time")?
        .parse()?;
    ensure!(
        kind == "aborted"
            && terminal["kind"].as_str() == Some("aborted")
            && terminal["reason"].as_str() == Some("operator_aborted")
            && completed_at == crate::runtime_db::repositories::timestamp(at),
        "original dispatch has no canonical operator abort"
    );
    let (source, payload): (String, String) = tx.query_row(
        "SELECT source_message_id,payload_json FROM current_run_abort_intents WHERE agent_id=?1 AND identity_incarnation=?2 AND run_id=?3",
        params![key.agent_id, key.identity_incarnation, original.run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let intent: CurrentRunAbortIntent = serde_json::from_str(&payload)?;
    ensure!(
        intent.agent_id == key.agent_id
            && intent.identity_incarnation == key.identity_incarnation
            && intent.run_id == original.run_id
            && intent.turn_id.as_deref() == Some(key.actual_dispatch_turn_id.as_str())
            && intent.source_message_id == invocation.message_id
            && source == invocation.message_id
            && !intent.runtime_lease_id.is_empty(),
        "operator abort intent differs from original memory dispatch"
    );
    let (agent, status, updated_at, payload): (String, String, String, String) = tx.query_row(
        "SELECT agent_id,status,updated_at,payload_json FROM queue_entries WHERE message_id=?1",
        [&invocation.message_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let queue: crate::types::QueueEntryRecord = serde_json::from_str(&payload)?;
    ensure!(
        agent == key.agent_id
            && queue.agent_id == agent
            && queue.message_id == invocation.message_id
            && serde_json::to_value(&queue.status)?.as_str() == Some(status.as_str())
            && matches!(status.as_str(), "aborted" | "processed")
            && updated_at == crate::runtime_db::repositories::timestamp(queue.updated_at),
        "operator-aborted source has an unfinished/new queue claim"
    );
    let open: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts WHERE agent_id=?1 AND lifecycle_state='open' AND json_extract(payload_json,'$.source_message_id')=?2)",
        params![key.agent_id, invocation.message_id],
        |row| row.get(0),
    )?;
    ensure!(!open, "operator-aborted source still has an open attempt");
    let (attempt_digest, outcome_digest) = if let Some(id) = &intent.activation_id {
        let (state, identity, generation, outcome_id, payload): (String, String, u64, String, String) = tx.query_row(
            "SELECT lifecycle_state,source_identity_json,source_generation,terminal_outcome_id,payload_json FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",
            params![key.agent_id, id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
        let attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
        ensure!(
            state == "settled"
                && attempt.state == ExecutionAttemptState::Settled
                && attempt.agent_id == key.agent_id
                && attempt.attempt_id == *id
                && attempt.source_message_id.as_deref() == Some(invocation.message_id.as_str())
                && serde_json::from_str::<
                    crate::domain::execution_protocol::ExecutionSourceIdentity,
                >(&identity)?
                    == attempt.source.identity
                && generation == attempt.source.generation
                && attempt
                    .run_id
                    .as_deref()
                    .is_none_or(|id| id == original.run_id)
                && attempt.turn_id.as_deref().is_none_or(|id| {
                    id == key.actual_dispatch_turn_id
                        || original.source_ingress_turn_id.as_deref() == Some(id)
                })
                && attempt.terminal_outcome_id.as_deref() == Some(outcome_id.as_str()),
            "operator abort attempt differs from original dispatch"
        );
        let (outcome_attempt, payload): (String, String) = tx.query_row(
            "SELECT attempt_id,payload_json FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id=?2",
            params![key.agent_id, outcome_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let outcome: ExecutionOutcomeRecord = serde_json::from_str(&payload)?;
        let at = attempt
            .terminal_at
            .as_deref()
            .context("operator abort attempt has no terminal time")?
            .parse()?;
        let crate::domain::execution_protocol::ExecutionProtocolCommand::Settle(expected) =
            settle_attempt(&attempt, at)
        else {
            unreachable!("operator abort always settles the attempt")
        };
        ensure!(
            outcome_attempt == *id && outcome == expected.outcome,
            "original attempt has no canonical operator abort outcome"
        );
        (
            Some(canonical_command_digest(&attempt)?),
            Some(canonical_command_digest(&outcome)?),
        )
    } else {
        (None, None)
    };
    Ok(AbortedAddress {
        intent_digest: canonical_command_digest(&intent)?,
        terminal_digest: canonical_command_digest(&terminal)?,
        queue_digest: canonical_command_digest(&queue)?,
        attempt_digest,
        outcome_digest,
    })
}
