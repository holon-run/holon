//! A retained queue recovery ACK can end its original dispatch before startup.
use super::*;
use crate::runtime_db::input_mutation_receipts::{
    lookup_input_mutation_receipt_tx, InputMutationFamily, PreparedInputMutation,
};
use crate::runtime_db::task_result_settlement::canonical_live_turn_id_tx;
use crate::types::{QueueEntryRecord, QueueEntryStatus};

// Preselect the expected Turn's batch addresses without decoding its body.
// The shared resolver below still verifies unique attachment, source, run and owner.
const AUDITS: &str = "SELECT a.audit_event_id FROM audit_events a
    WHERE a.agent_id=?1 AND a.kind='scheduler_bootstrap_claim_recovered'
      AND json_extract(a.data_json,'$.data.message_id')=?2
      AND json_extract(a.data_json,'$.data.recovery_outcome')='task_result_attempt_requeued_for_reentry'
      AND EXISTS (
        SELECT 1 FROM execution_protocol_attempts e
        WHERE e.agent_id=a.agent_id AND e.attempt_id=json_extract(a.data_json,'$.data.activation_id')
          AND json_extract(e.payload_json,'$.source_message_id')=?2
      )
      AND EXISTS (
        SELECT 1 FROM turn_records t WHERE t.agent_id=a.agent_id AND t.turn_id=?3 AND (
          json_extract(t.payload_json,'$.idle_input_batch.activation_id')=json_extract(a.data_json,'$.data.activation_id')
          OR EXISTS (
            SELECT 1 FROM json_each(t.payload_json,'$.live_input_batches') batch
            WHERE json_extract(batch.value,'$.activation_id')=json_extract(a.data_json,'$.data.activation_id')
          )
        )
      )";
const RECEIPTS: &str = "SELECT r.command_nonce FROM input_mutation_receipts r,json_each(r.outcome_json,'$.scopes') s,json_each(s.value,'$.audit_events') a WHERE r.family='queue' AND json_extract(s.value,'$.scope.agent_id')=?1 AND json_extract(a.value,'$.id') IN (SELECT audit_event_id FROM audits)";

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct QueueReentryAddress {
    audit_digest: String,
    receipt_digest: String,
    queue_digest: String,
    attempt_digest: String,
    outcome_digest: String,
    interrupted_at: DateTime<Utc>,
}

pub(super) fn is_queue_reentry_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT json_extract(payload_json,'$.terminal.reason')='interrupted_queue_claim_reentry' FROM turn_records WHERE turn_id=?1",
        [&binding.key().actual_dispatch_turn_id], |r| r.get::<_, Option<bool>>(0),
    )?.unwrap_or(false))
}

pub(super) fn proof_material_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
) -> Result<(usize, usize)> {
    // Select only scalar sizes before decoding the audit, original receipt or
    // execution proof. The enclosing startup observation retains this material.
    let attempts = "SELECT json_extract(a.data_json,'$.data.activation_id') id FROM audit_events a WHERE a.audit_event_id IN (SELECT audit_event_id FROM audits)";
    let mut material = (0, 0);
    for selection in [
        "SELECT length(CAST(data_json AS BLOB))+length(audit_event_id)+128 size FROM audit_events WHERE audit_event_id IN (SELECT audit_event_id FROM audits)",
        "SELECT length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB))+1024 size FROM input_mutation_receipts WHERE command_nonce IN (SELECT command_nonce FROM receipts)",
        "SELECT length(CAST(payload_json AS BLOB))+128 size FROM queue_entries WHERE message_id=?2",
        "SELECT length(CAST(payload_json AS BLOB))+128 size FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id IN (SELECT id FROM attempts)",
        "SELECT length(CAST(payload_json AS BLOB))+128 size FROM execution_protocol_outcomes WHERE agent_id=?1 AND attempt_id IN (SELECT id FROM attempts)",
        // A unique attachment can only decode the preselected Turn. Charge
        // this additional body read before resolving the canonical attachment.
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(turn_id AS BLOB))+128 size FROM turn_records WHERE agent_id=?1 AND turn_id=?3 AND EXISTS (SELECT 1 FROM audits)",
    ] {
        let (count, bytes) = tx.query_row(
            &format!("WITH audits AS ({AUDITS}),receipts AS ({RECEIPTS}),attempts AS ({attempts}) SELECT COUNT(*),COALESCE(SUM(size),0) FROM ({selection})"),
            params![binding.key().agent_id,binding.invocation().message_id,binding.key().actual_dispatch_turn_id],
            |r| Ok((r.get::<_,usize>(0)?,r.get::<_,usize>(1)?)),
        )?;
        add_proof_material(&mut material, bytes, count)?;
    }
    Ok(material)
}

pub(super) fn read_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
    original: &OriginalAddress,
) -> Result<QueueReentryAddress> {
    let key = binding.key();
    let invocation = binding.invocation();
    let (kind, reason, at, normalized_kind, normalized_at): (String,String,String,String,String) = tx.query_row(
        "SELECT json_extract(payload_json,'$.terminal.kind'),json_extract(payload_json,'$.terminal.reason'),json_extract(payload_json,'$.terminal.completed_at'),terminal_kind,completed_at FROM turn_records WHERE turn_id=?1",
        [&key.actual_dispatch_turn_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    let at: DateTime<Utc> = at.parse()?;
    ensure!(
        kind == "interrupted"
            && normalized_kind == kind
            && reason == "interrupted_queue_claim_reentry"
            && normalized_at == crate::runtime_db::repositories::timestamp(at),
        "queue reentry original Turn differs from its terminal columns"
    );
    let ids = tx
        .prepare(AUDITS)?
        .query_map(
            params![
                key.agent_id,
                invocation.message_id,
                key.actual_dispatch_turn_id
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        ids.len() == 1,
        "queue reentry requires one original recovery audit"
    );
    let (payload, normalized_seq, normalized_audit_at): (String, u64, String) = tx.query_row(
        "SELECT data_json,event_seq,created_at FROM audit_events WHERE audit_event_id=?1",
        [&ids[0]],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let audit: AuditEvent = serde_json::from_str(&payload)?;
    ensure!(
        audit.id == ids[0]
            && audit.kind == "scheduler_bootstrap_claim_recovered"
            && audit.event_seq == normalized_seq
            && crate::runtime_db::repositories::timestamp(audit.created_at) == normalized_audit_at
            && audit.created_at >= at
            && audit.data.get("agent_id").and_then(|v| v.as_str()) == Some(key.agent_id.as_str())
            && audit.data.get("message_id").and_then(|v| v.as_str())
                == Some(invocation.message_id.as_str())
            && audit.data.get("recovery_outcome").and_then(|v| v.as_str())
                == Some("task_result_attempt_requeued_for_reentry")
            && audit.data.get("provenance").and_then(|v| v.as_str())
                == Some("bootstrap_reconciliation")
            && audit.data.get("queue_status").and_then(|v| v.as_str()) == Some("queued"),
        "queue reentry recovery audit disagrees"
    );
    let prepared_payloads = tx.prepare(&format!("WITH audits AS ({AUDITS}),receipts AS ({RECEIPTS}) SELECT prepared_json FROM input_mutation_receipts WHERE command_nonce IN (SELECT command_nonce FROM receipts)"))?.query_map(params![key.agent_id,invocation.message_id,key.actual_dispatch_turn_id], |r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        prepared_payloads.len() == 1,
        "queue reentry requires one original whole Queue receipt"
    );
    let prepared: PreparedInputMutation = serde_json::from_str(&prepared_payloads[0])?;
    let scope = binding.scope();
    ensure!(
        prepared.family() == InputMutationFamily::Queue
            && prepared.scopes() == std::slice::from_ref(&scope),
        "queue reentry receipt has foreign scope or family"
    );
    let receipt = lookup_input_mutation_receipt_tx(tx, &prepared)?
        .context("queue reentry original receipt missing")?;
    ensure!(
        receipt.outcome.applied
            && receipt.outcome.scopes.len() == 1
            && receipt.outcome.scopes[0]
                .audit_events
                .iter()
                .any(|ack| audit == *ack),
        "queue reentry audit has no exact original writer ACK"
    );
    let (queue_payload, status, updated): (String, String, String) = tx.query_row(
        "SELECT payload_json,status,updated_at FROM queue_entries WHERE message_id=?1",
        [&invocation.message_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let queue: QueueEntryRecord = serde_json::from_str(&queue_payload)?;
    ensure!(
        queue.agent_id == key.agent_id
            && queue.message_id == invocation.message_id
            && queue.status == QueueEntryStatus::Queued
            && status == "queued"
            && updated == crate::runtime_db::repositories::timestamp(queue.updated_at)
            && queue.updated_at >= at
            && queue.updated_at <= audit.created_at,
        "queue reentry source changed since original recovery"
    );
    let attempt_id = audit
        .data
        .get("activation_id")
        .and_then(|v| v.as_str())
        .context("queue reentry original activation missing")?;
    let (attempt_payload,attempt_state,outcome_id): (String,String,String) = tx.query_row("SELECT payload_json,lifecycle_state,terminal_outcome_id FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2", params![key.agent_id,attempt_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let attempt: ExecutionAttempt = serde_json::from_str(&attempt_payload)?;
    ensure!(
        attempt.agent_id == key.agent_id
            && attempt.attempt_id == attempt_id
            && attempt.source_message_id.as_deref() == Some(invocation.message_id.as_str())
            && attempt.state == ExecutionAttemptState::Interrupted
            && attempt_state == "interrupted"
            && attempt.terminal_outcome_id.as_deref() == Some(outcome_id.as_str())
            && attempt.source.generation == original.source_sequence,
        "queue reentry Attempt differs from original dispatch"
    );
    ensure!(
        canonical_live_turn_id_tx(tx, &attempt)?.as_deref()
            == Some(key.actual_dispatch_turn_id.as_str()),
        "queue reentry activation is not uniquely attached to its original dispatch Turn"
    );
    let outcome_payload: String = tx.query_row("SELECT payload_json FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id=?2 AND attempt_id=?3", params![key.agent_id,outcome_id,attempt_id], |r|r.get(0))?;
    let outcome: ExecutionOutcomeRecord = serde_json::from_str(&outcome_payload)?;
    ensure!(
        outcome.outcome_id == outcome_id
            && outcome.attempt_id == attempt_id
            && interrupted_outcome_matches(&attempt, &outcome)
            && attempt
                .terminal_at
                .as_ref()
                .context("queue reentry Attempt has no end")?
                .parse::<DateTime<Utc>>()?
                == outcome.created_at.parse::<DateTime<Utc>>()?
            && outcome.created_at.parse::<DateTime<Utc>>()? <= at,
        "queue reentry original interrupted outcome disagrees"
    );
    Ok(QueueReentryAddress {
        audit_digest: canonical_command_digest(&audit)?,
        receipt_digest: canonical_command_digest(&receipt)?,
        queue_digest: canonical_command_digest(&queue)?,
        attempt_digest: canonical_command_digest(&attempt)?,
        outcome_digest: canonical_command_digest(&outcome)?,
        interrupted_at: at,
    })
}
