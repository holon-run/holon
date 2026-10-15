//! Material of the original finite Attempt -> actual Turn/claim observation.
use super::*;
impl InputReadMaterial<'_> {
    /// All table/filter/column fragments below are private static call sites.
    /// Match the original row predicate; this gate grants no body membership.
    pub(in crate::runtime_db::input_evidence_snapshot) fn attempt_relation_json_tx(
        &mut self,
        tx: &Transaction<'_>,
        table: &'static str,
        filter: &'static str,
        normalized: &'static str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<()> {
        let (raw, rows): (usize, usize) = tx.query_row(
            &format!("SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+{normalized}),0),COUNT(*) FROM {table} WHERE {filter}"),
            params, |r| Ok((r.get(0)?,r.get(1)?)),
        )?;
        let exhausted = || InputEvidenceSnapshotError::ResourceExhausted("Input Attempt relation");
        self.raw_cells(raw, rows.checked_mul(4).context(exhausted())?)?;
        let nodes: usize = tx.query_row(
            &format!("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM {table} r,json_tree(r.payload_json) j WHERE {filter}"),
            params, |r|r.get(0),
        )?;
        let nodes = nodes.checked_mul(2).context(exhausted())?;
        self.charge(0, nodes.checked_mul(256).context(exhausted())?, nodes)
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn attempt_assignment_tx(
        &mut self,
        tx: &Transaction<'_>,
        message: &str,
    ) -> Result<()> {
        let (bytes,rows):(usize,usize)=tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(agent_id AS BLOB))+length(CAST(turn_id AS BLOB))),0),COUNT(*) FROM conversation_input_assignments WHERE message_id=?1",
            [message], |r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input Attempt assignment",
                ))?,
        )
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn attempt_fixed_proofs_tx(
        &mut self,
        tx: &Transaction<'_>,
        seed: &str,
        attempt: &str,
    ) -> Result<()> {
        let (bytes,rows):(usize,usize)=tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(proof_json AS BLOB))),0),COUNT(*) FROM input_evidence_seed_attempt_turns WHERE seed_id=?1 AND attempt_id=?2",
            params![seed,attempt], |r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Input fixed Attempt Turn proof");
        self.raw_cells(bytes, rows.checked_mul(4).context(exhausted())?)?;
        let nodes:usize=tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_seed_attempt_turns r,json_tree(r.proof_json) j WHERE r.seed_id=?1 AND r.attempt_id=?2",
            params![seed,attempt], |r|r.get(0),
        )?;
        // Original Vec<String>, decoded proof and union/copy containers coexist.
        let nodes = nodes.checked_mul(8).context(exhausted())?;
        self.charge(0, nodes.checked_mul(256).context(exhausted())?, nodes)
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn attempt_turn_alias_tx(
        &mut self,
        tx: &Transaction<'_>,
        key: &Key,
    ) -> Result<()> {
        let (bytes,rows):(usize,usize)=tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(revision AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+length(CAST(fingerprint_json AS BLOB))+length(CAST(availability AS BLOB))+COALESCE(length(CAST(attempt_turn_proof_json AS BLOB)),0)),0),COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
            params![self.scope.agent_id,self.scope.identity_incarnation,key.family.as_str(),key.id], |r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Input Attempt Turn tombstone");
        self.raw_cells(bytes, rows.checked_mul(4).context(exhausted())?)?;
        let nodes:usize=tx.query_row(
            "SELECT (SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_current r,json_tree(r.fingerprint_json) j WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family=?3 AND r.record_id=?4)+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_current r,json_tree(r.attempt_turn_proof_json) j WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family=?3 AND r.record_id=?4)",
            params![self.scope.agent_id,self.scope.identity_incarnation,key.family.as_str(),key.id], |r|r.get(0),
        )?;
        let nodes = nodes.checked_mul(2).context(exhausted())?;
        self.charge(0, nodes.checked_mul(256).context(exhausted())?, nodes)
    }
}
