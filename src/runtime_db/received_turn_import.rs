//! Material for the original received-Vec Turn import, never a SQL source grant.
use super::*;
use crate::types::{TurnRecord, WaitConditionRecord};

pub(in crate::runtime_db) struct ReceivedTurnMaterial<'a> {
    scratch: &'a InputEvidenceBudgetLedger,
    source: Option<RowMaterial<'a>>,
    derived: RowMaterial<'a>,
}
impl<'a> ReceivedTurnMaterial<'a> {
    pub(in crate::runtime_db) fn new(
        scratch: &'a InputEvidenceBudgetLedger,
        messages: &Vec<serde_json::Value>,
        tools: &Vec<ToolExecutionRecord>,
        briefs: &Vec<BriefRecord>,
        deliveries: &Vec<DeliverySummaryRecord>,
        waits: &Vec<WaitConditionRecord>,
    ) -> Result<Self> {
        // All five incoming allocations coexist at the first normalizer. The
        // same eight-copy window covers its temporary normalized message/key.
        let (mut bytes, mut records) =
            received_material(&(messages, tools, briefs, deliveries, waits))?;
        for extra in [
            received_vec_slack(messages)?,
            received_vec_slack(tools)?,
            received_vec_slack(briefs)?,
            received_vec_slack(deliveries)?,
            received_vec_slack(waits)?,
        ] {
            bytes = bytes
                .checked_add(extra.0)
                .context("received Turn source Vec bytes overflow")?;
            records = received_joint_records(records, extra.1)?;
        }
        let mut source = RowMaterial::new(scratch);
        source.charge(bytes, records)?;
        let mut derived = RowMaterial::new(scratch);
        received_joint_records(source.records, derived.records + 16)?;
        derived.charge(4096, 16)?;
        Ok(Self {
            scratch,
            source: Some(source),
            derived,
        })
    }

    pub(in crate::runtime_db) fn observe_derived(
        &mut self,
        bytes: u64,
        nodes: usize,
    ) -> Result<()> {
        let prior = self.source.as_ref().map_or(0, |source| source.records);
        let next = received_joint_records(self.derived.records, nodes)?;
        received_joint_records(prior, next)?;
        self.derived.charge(bytes, nodes)
    }

    /// The algorithm consumed all five Vecs; only its derived map/Vec survives.
    /// Their guards cover the move, Vec slots, merged clone and write buffers.
    pub(in crate::runtime_db) fn source_consumed(&mut self) {
        drop(self.source.take());
    }

    pub(in crate::runtime_db) fn existing_turn_tx(
        &self,
        tx: &Transaction<'_>,
        turn_id: &str,
    ) -> Result<RowMaterial<'a>> {
        ensure!(self.source.is_none(), "received Turn source is still live");
        let mut row = RowMaterial::new(self.scratch);
        row.records = received_joint_records(row.records, self.derived.records)?;
        // Static SQL itself retains no Rust source body. This finite metadata
        // charge covers the row observer and its guard/statement parameters.
        row.charge(4096, 16)?;
        let raw: Option<u64> = tx
            .query_row(
                "SELECT length(CAST(payload_json AS BLOB))+length(CAST(turn_id AS BLOB))+length(CAST(agent_id AS BLOB)) FROM turn_records WHERE turn_id=?1",
                [turn_id], |r|r.get(0),
            ).optional()?;
        if let Some(raw) = raw {
            // merge and the unchanged upsert both decode this original row.
            // No String or JSON traversal precedes the scalar raw guard.
            row.charge(
                raw.checked_add(2048)
                    .and_then(|n| n.checked_mul(8))
                    .context("received existing Turn raw overflow")?,
                32,
            )?;
            let nodes: usize = tx.query_row(
                "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM turn_records t,json_tree(t.payload_json) j WHERE t.turn_id=?1",
                [turn_id], |r|r.get(0),
            )?;
            let nodes = nodes
                .checked_add(32)
                .and_then(|n| n.checked_mul(8))
                .context("received existing Turn native overflow")?;
            row.charge(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("received existing Turn native bytes overflow")?,
                nodes,
            )?;
        }
        Ok(row)
    }

    pub(in crate::runtime_db) fn assignment_material_tx(
        &self,
        tx: &Transaction<'_>,
        record: &TurnRecord,
        row: &mut RowMaterial<'a>,
    ) -> Result<()> {
        let assignments: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='conversation_input_assignments')",
            [], |r|r.get(0),
        )?;
        let sources: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='conversation_source_revisions')",
            [], |r|r.get(0),
        )?;
        // The unchanged assignment helper consumes one ID at a time. Borrow all
        // IDs but reserve only the largest real row window, not their sum.
        let mut max_bytes = 0u64;
        for id in &record.input_message_ids {
            let mut bytes = 2048u64;
            if assignments {
                let actual:u64=tx.query_row(
                    "SELECT COALESCE((SELECT length(CAST(message_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(turn_id AS BLOB)) FROM conversation_input_assignments WHERE message_id=?1),0)",
                    [id], |r|r.get(0),
                )?;
                bytes = bytes
                    .checked_add(actual)
                    .context("received Turn assignment bytes overflow")?;
            }
            let assigned: bool = assignments && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_input_assignments WHERE message_id=?1)",
                [id], |r|r.get(0),
            )?;
            if sources && !assigned {
                let actual:u64=tx.query_row(
                    "SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(turn_id AS BLOB)),0) FROM conversation_source_revisions WHERE source_kind='operator' AND source_id=?1),0)",
                    [id], |r|r.get(0),
                )?;
                bytes = bytes
                    .checked_add(actual)
                    .context("received Turn source revision bytes overflow")?;
            }
            max_bytes = max_bytes.max(bytes);
        }
        if max_bytes > 0 {
            row.charge(
                max_bytes
                    .checked_mul(8)
                    .context("received Turn assignment copies overflow")?,
                64,
            )?;
        }
        if let Some(replay) = record.replay.as_ref().filter(|replay| {
            replay.source_turn_id != record.turn_id
                && record.input_message_ids.contains(&replay.source_message_id)
        }) {
            let assigned: bool = assignments && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_input_assignments WHERE agent_id=?1 AND message_id=?2)",
                params![record.agent_id,replay.source_message_id], |r|r.get(0),
            )?;
            if assigned {
                return Ok(());
            }
            // The original helper may traverse this complete source payload to
            // check replay ownership. It does not decode it or acquire its body.
            let raw:u64=tx.query_row(
                "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM turn_records WHERE agent_id=?1 AND turn_id=?2),0)",
                params![record.agent_id,replay.source_turn_id], |r|r.get(0),
            )?;
            row.charge(
                raw.checked_mul(8)
                    .context("received Turn replay raw overflow")?,
                16,
            )?;
            let nodes:usize=tx.query_row(
                "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM turn_records t,json_tree(t.payload_json) j WHERE t.agent_id=?1 AND t.turn_id=?2",
                params![record.agent_id,replay.source_turn_id], |r|r.get(0),
            )?;
            let nodes = nodes
                .checked_mul(8)
                .context("received Turn replay nodes overflow")?;
            row.charge(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("received Turn replay native bytes overflow")?,
                nodes,
            )?;
        }
        Ok(())
    }
}

/// Only the fields the original algorithm is about to copy. In particular a
/// Tool's stdout/Brief text/Wait continuation is not copied into Turn metadata.
pub(in crate::runtime_db) fn derivation_material(
    value: &impl Serialize,
    new_turn: bool,
) -> Result<(u64, usize)> {
    let mut shape = BorrowedShape::default();
    if let Err(error) = serde_json::to_writer(&mut shape, value) {
        if shape.exhausted {
            return Err(InputEvidenceSnapshotError::ResourceExhausted(
                "received Turn derived metadata",
            )
            .into());
        }
        return Err(error.into());
    }
    // The fixed slack covers Turn/map/Vec headers and guard slots. Eight copies
    // include map-to-Vec move/reallocation and the one merged row/write window.
    let records = shape
        .nodes
        // A new Turn also allocates its fixed fields, map node and Vec slot.
        .checked_add(if new_turn { 64 } else { 8 })
        .and_then(|n| n.checked_mul(8))
        .context("received Turn derived nodes overflow")?;
    let bytes = shape
        .bytes
        .checked_add(512)
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(u64::try_from(records).ok()?.checked_mul(256)?))
        .context("received Turn derived bytes overflow")?;
    ensure!(
        records <= MAX_RECORDS && bytes <= MAX_METADATA_BYTES as u64,
        InputEvidenceSnapshotError::ResourceExhausted("received Turn derived metadata")
    );
    Ok((bytes, records))
}

#[cfg(test)]
#[path = "received_turn_import_tests.rs"]
mod tests;
