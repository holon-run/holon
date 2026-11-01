//! Prove replay ownership without moving the original input assignment.
use super::*;
use crate::types::{TurnReplayProvenance, TurnTerminalSummary};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

// UNION deduplicates identities, so even corrupt cyclic replay pointers terminate.
// Stop at the assigned Turn: its own predecessor is outside this proof.
const LINEAGE: &str = "
WITH RECURSIVE lineage(turn_id) AS (
    SELECT ?3
    UNION
    SELECT json_extract(t.payload_json,'$.replay.source_turn_id')
    FROM turn_records t JOIN lineage l ON t.turn_id=l.turn_id
    WHERE l.turn_id<>?4
      AND json_type(t.payload_json,'$.replay.source_turn_id')='text'
)
SELECT json_object(
    'turn_id',t.turn_id,'agent_id',t.agent_id,
    'payload_turn_id',json_extract(t.payload_json,'$.turn_id'),
    'payload_agent_id',json_extract(t.payload_json,'$.agent_id'),
    'inputs',json_extract(t.payload_json,'$.input_message_ids'),
    'replay',json_extract(t.payload_json,'$.replay'),
    'terminal',json_extract(t.payload_json,'$.terminal')
) AS proof
FROM turn_records t JOIN lineage l ON t.turn_id=l.turn_id
ORDER BY t.turn_id";

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct SourceAssignmentProof {
    assigned_turn_id: String,
    lineage_digest: String,
}

#[derive(Deserialize, Serialize)]
struct SourceTurn {
    turn_id: String,
    agent_id: String,
    payload_turn_id: String,
    payload_agent_id: String,
    inputs: Vec<String>,
    replay: Option<TurnReplayProvenance>,
    terminal: Option<TurnTerminalSummary>,
}

fn assignment_tx(tx: &Transaction<'_>, binding: &DispatchMemoryBinding) -> Result<String> {
    let assigned: Option<(String, String)> = tx
        .query_row(
            "SELECT agent_id,turn_id FROM conversation_input_assignments WHERE message_id=?1",
            [&binding.invocation().message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (agent, turn) = assigned.context("startup memory needs original source assignment")?;
    ensure!(
        agent == binding.key().agent_id && !turn.is_empty(),
        "startup original source assignment has a foreign agent or empty Turn"
    );
    Ok(turn)
}

pub(super) fn source_assignment_material_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
) -> Result<(usize, usize)> {
    // Keep this preflight SQL-only: even the assignment is selected as a scalar
    // subquery, before constructing an owned lineage or assignment String.
    let selection = LINEAGE.replace(
        "?4",
        "(SELECT turn_id FROM conversation_input_assignments WHERE message_id=?2)",
    );
    let (records, bytes): (usize, usize) = tx.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(proof AS BLOB))),0) FROM ({selection})"),
        params![
            binding.key().agent_id,
            binding.invocation().message_id,
            binding.key().actual_dispatch_turn_id,
        ],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let assignment_bytes: usize = tx.query_row(
        "SELECT COALESCE(SUM(length(CAST(agent_id AS BLOB))+length(CAST(turn_id AS BLOB))),0) FROM conversation_input_assignments WHERE message_id=?1",
        [&binding.invocation().message_id], |r| r.get(0),
    )?;
    let mut material = (0, 0);
    add_proof_material(&mut material, bytes, records)?;
    add_proof_material(&mut material, assignment_bytes, 1)?;
    Ok(material)
}

pub(super) fn read_source_assignment_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
) -> Result<SourceAssignmentProof> {
    let assigned = assignment_tx(tx, binding)?;
    let mut statement = tx.prepare(LINEAGE)?;
    let payloads = statement
        .query_map(
            params![
                binding.key().agent_id,
                binding.invocation().message_id,
                binding.key().actual_dispatch_turn_id,
                assigned,
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let turns: BTreeMap<String, SourceTurn> = payloads
        .iter()
        .map(|payload| {
            let turn: SourceTurn = serde_json::from_str(payload)?;
            Ok((turn.turn_id.clone(), turn))
        })
        .collect::<Result<_>>()?;
    let mut cursor = binding.key().actual_dispatch_turn_id.as_str();
    let mut visited = BTreeSet::new();
    loop {
        ensure!(
            visited.insert(cursor),
            "startup source replay lineage is cyclic"
        );
        let turn = turns
            .get(cursor)
            .context("startup source replay predecessor is missing")?;
        ensure!(
            turn.agent_id == binding.key().agent_id
                && turn.payload_agent_id == turn.agent_id
                && turn.payload_turn_id == turn.turn_id
                && turn.inputs.contains(&binding.invocation().message_id),
            "startup source replay Turn does not own the original input"
        );
        if cursor == assigned {
            break;
        }
        let replay = turn
            .replay
            .as_ref()
            .context("startup source assignment has no replay lineage")?;
        ensure!(
            replay.source_message_id == binding.invocation().message_id
                && replay.reason == "interrupted_queue_claim_reentry",
            "startup source replay provenance differs from original input"
        );
        let predecessor = turns
            .get(&replay.source_turn_id)
            .context("startup source replay predecessor is missing")?;
        ensure!(
            predecessor.terminal.is_some() && replay.prior_terminal == predecessor.terminal,
            "startup source replay prior terminal differs"
        );
        cursor = &replay.source_turn_id;
    }
    Ok(SourceAssignmentProof {
        assigned_turn_id: assigned,
        lineage_digest: canonical_command_digest(&payloads)?,
    })
}
