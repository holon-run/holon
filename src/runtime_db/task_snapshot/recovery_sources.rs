//! Recovery observations are selected by the original finite scheduler request.
use super::*;
use crate::{
    runtime::task_reads::TaskRecoverySourceObservation,
    runtime_db::task_result_settlement::{observe_recovery_source_tx, TaskResultSettlementRecord},
    types::MessageEnvelope,
};

// The original predicates follow these exact source/Result/attempt edges. This
// integer-only gate precedes their body reads; it neither adopts nor publishes
// any of the addressed canonical records. Their existing wait scans remain
// charged, but only selected claims and reply receipts extend the body frontier.
// Exact references also charge stale foreign bodies before canonical rejection.
const MATERIAL: &str = "
WITH RECURSIVE
selected AS (
 SELECT * FROM task_result_settlements
 WHERE message_id=?3 OR result_identity=?4
),
selected_tasks AS (
 SELECT * FROM tasks WHERE task_id IN (SELECT task_id FROM selected)
),
claiming_waits AS (
 SELECT * FROM wait_conditions waits WHERE agent_id=?1 AND (
   (json_extract(payload_json,'$.kind')='task'
     AND status IN ('active','triggered','resolved') AND EXISTS(
     SELECT 1 FROM json_each(waits.payload_json,'$.wake_sources') wake
     WHERE json_extract(wake.value,'$.kind')='task_result'
       AND json_extract(wake.value,'$.task_id') IN (SELECT task_id FROM selected)))
   OR wait_condition_id IN (
     SELECT json_extract(payload_json,'$.source_refs.wait_id') FROM messages
     WHERE agent_id=?1 AND message_id=?2))
),
deliveries AS (
 SELECT * FROM agent_message_deliveries WHERE delivery_id IN (
   SELECT json_extract(payload_json,'$.recovery.delivery_id') FROM selected_tasks
   UNION SELECT json_extract(payload_json,'$.detail.response_delivery_id') FROM selected_tasks)
),
base_message_ids(id) AS (
 SELECT ?2
 UNION SELECT message_id FROM selected
 UNION SELECT json_extract(payload_json,'$.detail.message_id') FROM selected_tasks
 UNION SELECT json_extract(payload_json,'$.trigger_message_id') FROM claiming_waits
 UNION SELECT json_extract(m.payload_json,'$.metadata.task_detail.message_id')
   FROM messages m WHERE m.message_id IN (SELECT message_id FROM selected)
),
related_turns AS (
 SELECT * FROM turn_records turns WHERE agent_id=?1 AND (
   turn_id IN (SELECT parent_turn_id FROM selected)
   OR turn_id IN (SELECT json_extract(payload_json,'$.turn_id') FROM deliveries)
   OR trigger_message_id IN (SELECT id FROM base_message_ids)
   OR EXISTS(SELECT 1 FROM json_each(turns.payload_json,'$.input_message_ids') input
     WHERE input.value IN (SELECT id FROM base_message_ids)))
),
attempts AS (
 SELECT * FROM execution_protocol_attempts
 WHERE agent_id=?1 AND (lifecycle_state='open' OR attempt_id IN
   (SELECT activation_id FROM selected)
   OR attempt_id IN (SELECT json_extract(payload_json,'$.activation_id') FROM deliveries)
   OR attempt_id IN (
     SELECT json_extract(payload_json,'$.idle_input_batch.activation_id') FROM related_turns
     UNION SELECT json_extract(batch.value,'$.activation_id')
       FROM related_turns turns,json_each(turns.payload_json,'$.live_input_batches') batch)
   OR (lifecycle_state='settled' AND (
     json_extract(payload_json,'$.source_message_id') IN (SELECT message_id FROM selected)
     OR attempt_id IN (
       SELECT admission.root_attempt_id FROM task_result_recovery_admissions admission
       JOIN selected ON selected.result_identity=admission.result_identity
       WHERE admission.recovery_epoch<=selected.recovery_epoch))))
 UNION
 SELECT prior.* FROM execution_protocol_attempts prior JOIN attempts current
 ON prior.attempt_id=json_extract(current.payload_json,'$.recovery_of_attempt_id')
 WHERE prior.agent_id=?1
),
message_ids(id) AS (
 SELECT id FROM base_message_ids
 UNION SELECT json_extract(payload_json,'$.source_message_id') FROM attempts
 UNION SELECT json_extract(payload_json,'$.trigger_message_id')
   FROM wait_conditions WHERE agent_id=?1
   AND wait_condition_id IN (
     SELECT json_extract(payload_json,'$.source.identity.wait_id') FROM attempts)
 UNION SELECT json_extract(m.payload_json,'$.metadata.task_detail.message_id')
   FROM messages m JOIN message_ids ids ON m.message_id=ids.id WHERE m.agent_id=?1
),
bodies(payload_json) AS (
 SELECT payload_json FROM selected
 UNION ALL SELECT payload_json FROM selected_tasks
 UNION ALL SELECT payload_json FROM attempts
 UNION ALL SELECT payload_json FROM messages
   WHERE message_id IN (SELECT id FROM message_ids)
 UNION ALL SELECT payload_json FROM queue_entries
   WHERE message_id IN (SELECT id FROM message_ids)
 UNION ALL SELECT payload_json FROM wait_conditions WHERE agent_id=?1
 UNION ALL SELECT payload_json FROM work_items WHERE agent_id=?1
   AND work_item_id IN (SELECT json_extract(payload_json,'$.work_item_id') FROM claiming_waits)
 UNION ALL SELECT payload_json FROM deliveries
 UNION ALL SELECT payload_json FROM turn_records turns WHERE agent_id=?1 AND (
   turn_id IN (SELECT turn_id FROM related_turns)
   OR turn_id IN (SELECT json_extract(payload_json,'$.turn_id') FROM attempts)
   OR trigger_message_id IN (SELECT id FROM message_ids)
   OR EXISTS(SELECT 1 FROM json_each(turns.payload_json,'$.input_message_ids') input
     WHERE input.value IN (SELECT id FROM message_ids))
   OR json_extract(turns.payload_json,'$.idle_input_batch.activation_id')
     IN (SELECT attempt_id FROM attempts)
   OR EXISTS(SELECT 1 FROM json_each(turns.payload_json,'$.live_input_batches') batch
     WHERE json_extract(batch.value,'$.activation_id') IN (SELECT attempt_id FROM attempts))
 )
)";

#[cfg(test)]
#[path = "recovery_sources_material_tests.rs"]
mod material_tests;

fn preflight_predicate_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    source: &MessageEnvelope,
    identity: Option<&str>,
    request: &TaskDomainLoadRequest,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<()> {
    let parameters = params![
        scope.agent_id,
        source.id,
        source.source_refs.get("task_result_message_id"),
        identity
    ];
    let (count, bytes): (usize, usize) = tx.query_row(
        &format!("{MATERIAL} SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM bodies"),
        parameters,
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    bound(count, bytes, Some(request))?;
    material.charge_ids(
        bytes
            .checked_mul(2)
            .context("recovery predicate bytes overflow")?,
        count
            .checked_mul(2)
            .context("recovery predicate records overflow")?,
    )?;
    let nodes: usize = tx.query_row(
        &format!("{MATERIAL} SELECT COALESCE(SUM((SELECT COUNT(*) FROM json_tree(bodies.payload_json))),0) FROM bodies"),
        parameters,
        |row| row.get(0),
    )?;
    material.charge_ids(
        nodes
            .checked_mul(64)
            .context("recovery predicate nodes overflow")?,
        nodes,
    )
}

/// Charge the original reducer's SQL recheck without replacing its source capture.
pub(in crate::runtime_db) fn prepare_settled_recovery_source_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    source: &MessageEnvelope,
    request: &TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>> {
    ensure!(
        source.agent_id == scope.agent_id,
        "settled recovery source crosses original Task scope"
    );
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(ledger))
        .with_memory_budget(memory_budget);
    preflight_predicate_tx(tx, scope, source, None, request, &mut material)?;
    Ok(material.into_guards())
}

pub(super) fn observe_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    records: &[Arc<QualifiedTaskFact>],
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<BTreeMap<String, TaskRecoverySourceObservation>> {
    let mut observations = BTreeMap::new();
    if request.capture.recovery_source_ids.is_empty() {
        return Ok(observations);
    }
    let now = request
        .recovery_at
        .context("Task recovery selectors require a frozen recovery_at")?;
    material.charge_ids(
        records
            .len()
            .checked_mul(16)
            .context("recovery selector overflow")?,
        records.len(),
    )?;
    let paused: Vec<&TaskResultSettlementRecord> = records
        .iter()
        .filter_map(|fact| match &fact.fact {
            CanonicalTaskFact::ResultSettlement(record) if record.recovery_paused_at.is_some() => {
                Some(record)
            }
            _ => None,
        })
        .collect();
    for source_id in &request.capture.recovery_source_ids {
        // Only a fixed/original ACK header can name a source in this incarnation.
        const SOURCE: &str = "FROM messages m JOIN task_message_order_headers h
          ON h.agent_id=m.agent_id AND h.evidence_id=m.evidence_id AND h.message_id=m.message_id
          WHERE m.agent_id=?1 AND h.identity_incarnation=?2 AND m.message_id=?3
          AND NOT EXISTS(SELECT 1 FROM input_evidence_current a
            WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='message'
              AND a.record_id=m.evidence_id
              AND (a.availability!='present' OR a.content_digest IS NOT h.content_digest))";
        let bytes: Option<usize> = tx.query_row(
            &format!("SELECT length(CAST(m.payload_json AS BLOB))+length(CAST(h.content_digest AS BLOB)) {SOURCE}"),
            params![scope.agent_id,scope.identity_incarnation,source_id],
            |row| row.get(0),
        ).optional()?;
        // An unavailable selector is not a negative predicate observation.
        // Reject preparation instead of returning incomplete recovery coverage.
        let bytes = bytes
            .ok_or_else(|| TaskReadError::IncompleteRecoverySource(source_id.clone()))
            .with_context(|| {
                format!(
                    "Task recovery source {source_id} is missing or unavailable from its original Message ACK/header"
                )
            })?;
        bound(1, bytes, Some(request))?;
        material.charge_message(source_id, bytes)?;
        let nodes: usize = tx.query_row(
            &format!("SELECT (SELECT COUNT(*) FROM json_tree(m.payload_json)) {SOURCE}"),
            params![scope.agent_id, scope.identity_incarnation, source_id],
            |row| row.get(0),
        )?;
        material.charge_ids(
            nodes
                .checked_mul(64)
                .context("recovery source nodes overflow")?,
            nodes,
        )?;
        let (raw, digest): (String, String) = tx.query_row(
            &format!("SELECT m.payload_json,h.content_digest {SOURCE}"),
            params![scope.agent_id, scope.identity_incarnation, source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            format!("{:x}", Sha256::digest(raw.as_bytes())) == digest,
            "Task recovery source changed from its original Message ACK"
        );
        let source: MessageEnvelope = serde_json::from_str(&raw)?;
        ensure!(
            source.id == *source_id && source.agent_id == scope.agent_id,
            "Task recovery source projection differs"
        );
        if source.source_refs.contains_key("task_result_message_id") || !paused.is_empty() {
            preflight_predicate_tx(tx, scope, &source, None, request, material)?;
            for record in &paused {
                preflight_predicate_tx(
                    tx,
                    scope,
                    &source,
                    Some(&record.result_identity),
                    request,
                    material,
                )?;
            }
        }
        let observation = observe_recovery_source_tx(tx, &source, &paused, now)?;
        material.charge_ids(
            read_preparation::encoded_metadata_bytes(&(&source.id, &observation))?,
            paused
                .len()
                .checked_add(1)
                .context("recovery observation count overflow")?,
        )?;
        observations.insert(source.id, observation);
    }
    Ok(observations)
}
