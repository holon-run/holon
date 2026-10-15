//! Only scalar/native material for original receipt decoding; no authority.
use super::*;
impl InputReadMaterial<'_> {
    pub(in crate::runtime_db) fn original_receipt_material_tx(
        &mut self, tx:&Transaction<'_>, prepared:&crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
    ) -> Result<()> {
        let (bytes,rows):(usize,usize)=tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(family AS BLOB))+length(CAST(phase_version AS BLOB))+length(CAST(command_digest AS BLOB))+length(CAST(scope_digest AS BLOB))+length(CAST(scopes_json AS BLOB))+length(CAST(prepared_at AS BLOB))+length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB))+length(CAST(committed_at AS BLOB))+length(CAST(result_digest AS BLOB))),0),COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [prepared.command_nonce()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let exhausted=||InputEvidenceSnapshotError::ResourceExhausted("original writer receipt material");
        self.observe_projected(u64::try_from(bytes.checked_mul(8)
            .and_then(|n|rows.checked_mul(8*256).and_then(|m|n.checked_add(m))).context(exhausted())?)?,
            rows.checked_mul(8).context(exhausted())?)?;
        let nodes:usize=tx.query_row(
            "SELECT (SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_mutation_receipts r,json_tree(r.scopes_json) j WHERE r.command_nonce=?1)+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_mutation_receipts r,json_tree(r.prepared_json) j WHERE r.command_nonce=?1)+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_mutation_receipts r,json_tree(r.outcome_json) j WHERE r.command_nonce=?1)",
            [prepared.command_nonce()],|r|r.get(0))?;
        let nodes=nodes.checked_mul(8).context(exhausted())?;
        self.observe_projected(u64::try_from(nodes.checked_mul(256).context(exhausted())?)?,nodes)
    }
}
