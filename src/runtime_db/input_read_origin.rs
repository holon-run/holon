//! Additional original origin rows, after the selected overlay's raw/native gate.
use super::*;
impl InputReadMaterial<'_> {
    /// Call only on the actual legacy branch. The selected/source actor already
    /// admitted that exact overlay's Strings and complete maintenance proof.
    pub(in crate::runtime_db::input_evidence_snapshot) fn origin_context_tx(
        &mut self,
        tx: &Transaction<'_>,
        seed: &str,
        key: &Key,
    ) -> Result<()> {
        let (maintenance,recovery):(bool,bool)=tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3 AND origin_kind='maintenance_created'),EXISTS(SELECT 1 FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3 AND origin_kind='recovery_created')",
            params![seed,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Input original origin context");
        if maintenance {
            let (bytes,rows):(usize,usize)=tx.query_row(
                "SELECT COALESCE(SUM(length(CAST(agent_id AS BLOB))+length(CAST(boundary_id AS BLOB))+length(CAST(source_kind AS BLOB))),0),COUNT(*) FROM episode_continuity_seeds WHERE seed_id=?1",
                [seed],|r|Ok((r.get(0)?,r.get(1)?)),
            )?;
            self.raw_cells(bytes, rows.checked_mul(4).context(exhausted())?)?;
            if key.family == Family::Outcome {
                // Original fallback validation decodes this exact selected
                // Outcome before the main selected-body decoder runs.
                self.record_body_tx(tx, key)?;
            }
        } else if recovery && key.family == Family::Outcome {
            let (bytes,rows):(usize,usize)=tx.query_row(
                "SELECT COALESCE(SUM(length(CAST(c.payload_hash AS BLOB))),0),COUNT(*) FROM execution_protocol_command_results c JOIN episode_continuity_seeds s ON s.agent_id=c.agent_id JOIN input_evidence_seed_recovery_current r ON r.seed_id=s.seed_id AND r.command_kind=c.command_kind AND r.command_identity=c.command_identity WHERE r.seed_id=?1 AND r.family=?2 AND r.record_id=?3 AND r.origin_kind='recovery_created'",
                params![seed,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?)),
            )?;
            self.raw_cells(bytes, rows.checked_mul(4).context(exhausted())?)?;
        }
        Ok(())
    }
}
