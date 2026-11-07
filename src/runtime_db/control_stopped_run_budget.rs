//! SQL-only acquisition sizes for the finite original Stop proof.
use super::*;
use crate::runtime::resident_input_evidence::InputEvidenceScope;

const MAX_PROOF_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROOF_RECORDS: usize = 65_536;

/// Count every candidate that the original-run locator may decode, including
/// all obligations of each command. This is not canonical or physical proof.
/// The caller observes the returned material in its shared combined budget
/// before constructing any owned receipt/source/Turn proof.
pub(crate) fn stopped_run_proof_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    run_id: &str,
    activation_id: Option<&str>,
) -> Result<(usize, usize)> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0 && !run_id.is_empty(),
        "original Stop material address is incomplete"
    );
    // Do not filter on the receipt's action: the canonical locator decodes a
    // matching run target before rejecting a corrupted/non-Stop receipt.
    let commands = "SELECT DISTINCT command_id FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND target_kind='run' AND target_identity=?3";
    let (receipts, bytes): (usize,usize) = tx.query_row(&format!(
        "WITH commands AS ({commands}) SELECT COUNT(*),COALESCE(SUM(length(CAST(receipt_json AS BLOB))+length(CAST(outcome_json AS BLOB))+length(CAST(command_id AS BLOB))+length(CAST(invocation_digest AS BLOB))+length(CAST(command_digest AS BLOB))+length(CAST(committed_at AS BLOB))+length(CAST(action AS BLOB))),0) FROM control_command_receipts WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id IN commands"),
        params![scope.agent_id,scope.identity_incarnation,run_id], |r|Ok((r.get(0)?,r.get(1)?)))?;
    let mut material = (bytes, receipts);
    bound(material)?;
    let (count, bytes): (usize,usize) = tx.query_row(&format!(
        "WITH commands AS ({commands}) SELECT COUNT(*),COALESCE(SUM(length(CAST(command_id AS BLOB))+length(CAST(obligation_id AS BLOB))+length(CAST(target_kind AS BLOB))+length(CAST(target_identity AS BLOB))+length(CAST(target_generation_json AS BLOB))+length(CAST(target_json AS BLOB))+COALESCE(length(CAST(completion_evidence_json AS BLOB)),0)+length(CAST(state AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))),0) FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id IN commands"),
        params![scope.agent_id,scope.identity_incarnation,run_id], |r|Ok((r.get(0)?,r.get(1)?)))?;
    add(&mut material, bytes, count)?;
    if count == 0 {
        return Ok(material);
    }
    // SQL JSON extraction produces only address/length scalars. Receipts and
    // obligation payloads have already passed their aggregate hard ceiling.
    let prefix = format!(
        "WITH commands AS ({commands}), receipts AS (SELECT outcome_json,receipt_json FROM control_command_receipts WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id IN commands), sources AS (SELECT DISTINCT json_extract(target_generation_json,'$.source_message_id') id FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id IN commands AND target_kind='run' AND target_identity=?3), turns AS (SELECT value id FROM receipts,json_each(outcome_json,'$.interrupted_turn_ids') UNION SELECT value FROM receipts,json_each(receipt_json,'$.outcome.interrupted_turn_ids') UNION SELECT turn_id FROM turn_records WHERE agent_id=?1 AND run_id=?3), attempts AS (SELECT attempt_id,terminal_outcome_id FROM execution_protocol_attempts WHERE agent_id=?1 AND (attempt_id=?4 OR json_extract(payload_json,'$.run_id')=?3 OR json_extract(payload_json,'$.source_message_id') IN (SELECT id FROM sources))) "
    );
    for selection in [
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(evidence_id AS BLOB)) size FROM messages WHERE agent_id=?1 AND message_id IN (SELECT id FROM sources)",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(message_id AS BLOB)) size FROM queue_entries WHERE agent_id=?1 AND message_id IN (SELECT id FROM sources)",
        // records_by_ids reads declared Turn IDs globally before validating
        // their original owner, so count foreign/corrupt rows as well.
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(turn_id AS BLOB)) size FROM turn_records WHERE turn_id IN (SELECT id FROM turns)",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(attempt_id AS BLOB))+length(CAST(source_identity_json AS BLOB))+COALESCE(length(CAST(recovery_of_attempt_id AS BLOB)),0)+COALESCE(length(CAST(terminal_outcome_id AS BLOB)),0)+length(CAST(lifecycle_state AS BLOB))+16 size FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id IN (SELECT attempt_id FROM attempts)",
        // The validator loads the original computed control outcome even if a
        // corrupt row's normalized attempt_id/terminal_outcome_id disagrees.
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(outcome_id AS BLOB))+length(CAST(attempt_id AS BLOB)) size FROM execution_protocol_outcomes WHERE agent_id=?1 AND (attempt_id IN (SELECT attempt_id FROM attempts) OR outcome_id IN (SELECT terminal_outcome_id FROM attempts) OR (?4 IS NOT NULL AND substr(outcome_id,1,8)='control:' AND substr(outcome_id,-length(':interrupt:'||?4))=':interrupt:'||?4))",
    ] {
        let sql = format!("{prefix} SELECT COUNT(*),COALESCE(SUM(size),0) FROM ({selection})");
        let (count, bytes): (usize,usize) = tx.query_row(&sql,
            params![scope.agent_id,scope.identity_incarnation,run_id,activation_id],
            |r|Ok((r.get(0)?,r.get(1)?)))?;
        add(&mut material, bytes, count)?;
    }
    Ok(material)
}

fn add(material: &mut (usize, usize), bytes: usize, records: usize) -> Result<()> {
    material.0 = material
        .0
        .checked_add(bytes)
        .context("original Stop proof byte overflow")?;
    material.1 = material
        .1
        .checked_add(records)
        .context("original Stop proof count overflow")?;
    bound(*material)
}
fn bound((bytes, records): (usize, usize)) -> Result<()> {
    ensure!(
        bytes <= MAX_PROOF_BYTES && records <= MAX_PROOF_RECORDS,
        "original Stop proof exceeds preparation limit"
    );
    Ok(())
}
