//! Actual selected SQL IDs and borrowed owning-reference containers.
use super::*;

impl InputReadMaterial<'_> {
    /// Preserve the original qualification, DISTINCT/UNION, order and limit.
    /// Wrapping the original ID query avoids deduplicating equal-length IDs.
    pub(in crate::runtime_db::input_evidence_snapshot) fn selected_id_pool_tx(
        &mut self,
        tx: &Transaction<'_>,
        sql: &str,
        column: &str,
        params: impl rusqlite::Params,
    ) -> Result<()> {
        let (bytes, rows): (usize, usize) = tx.query_row(
            &format!(
                "SELECT COALESCE(SUM(length(CAST({column} AS BLOB))),0),COUNT(*) FROM ({sql})"
            ),
            params,
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // Window/owning keys coexist in coverage, selected, pending paths and
        // their indexes. Body guards are independent of these copied IDs.
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input selected ID containers",
                ))?,
        )
    }

    pub(in crate::runtime_db::input_evidence_snapshot) fn message_assignment_id_tx(
        &mut self,
        tx: &Transaction<'_>,
        message: &str,
    ) -> Result<()> {
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(turn_id AS BLOB))),0),COUNT(*) FROM conversation_input_assignments WHERE agent_id=?1 AND message_id=?2",
            params![self.scope.agent_id,message],
            |r| Ok((r.get(0)?,r.get(1)?)),
        )?;
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input assigned Turn containers",
                ))?,
        )
    }

    pub(in crate::runtime_db::input_evidence_snapshot) fn reference_selectors(
        &mut self,
        record: &QualifiedInputEvidence,
    ) -> Result<()> {
        ensure!(
            record.locator.scope == *self.scope,
            "Input reference scope differs"
        );
        let mut bytes = Some(0usize);
        let mut count = Some(0usize);
        record.visit_references(|family, id| {
            bytes = bytes
                .and_then(|n| n.checked_add(id.len()))
                .and_then(|n| n.checked_add(family.as_str().len()));
            count = count.and_then(|n| n.checked_add(1));
        });
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Input reference selectors");
        self.raw_cells(
            bytes.context(exhausted())?,
            count
                .context(exhausted())?
                .checked_mul(4)
                .context(exhausted())?,
        )
    }
}
