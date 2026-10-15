//! Same-phase scalar/native material before selected canonical Input decoding.
use super::*;
impl InputReadMaterial<'_> {
    pub(in crate::runtime_db::input_evidence_snapshot) fn binding_metadata_tx(
        &mut self,
        tx: &Transaction<'_>,
    ) -> Result<()> {
        let (bytes,rows):(usize,usize)=tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(agent_id AS BLOB))+length(CAST(identity_incarnation AS BLOB))+length(CAST(policy_version AS BLOB))+length(CAST(mode AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0)),0),COUNT(*) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",
            params![self.scope.agent_id,self.scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw_cells(bytes, rows)
    }
    pub(super) fn raw_cells(&mut self, bytes: usize, rows: usize) -> Result<()> {
        let exhausted = || InputEvidenceSnapshotError::ResourceExhausted("Input raw cells");
        self.charge(
            bytes.checked_mul(8).context(exhausted())?,
            rows.checked_mul(8 * 256).context(exhausted())?,
            rows.checked_mul(8).context(exhausted())?,
        )
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn record_metadata_tx(
        &mut self,
        tx: &Transaction<'_>,
        binding: &Binding,
        key: &Key,
    ) -> Result<()> {
        let (table, id) = table(key.family);
        // Every actual fingerprint String/int-affinity source participates. A
        // Tool's complete stdout is not an owned retained projection.
        let columns=match key.family {
            Family::Turn=>"COALESCE(length(CAST(r.turn_index AS BLOB)),0)+COALESCE(length(CAST(r.owner_kind AS BLOB)),0)+COALESCE(length(CAST(r.owner_id AS BLOB)),0)",
            Family::Message=>"COALESCE(length(CAST(r.content_hash AS BLOB)),0)+COALESCE(length(CAST(r.message_seq AS BLOB)),0)+COALESCE((SELECT length(CAST(a.turn_id AS BLOB)) FROM conversation_input_assignments a WHERE a.agent_id=r.agent_id AND a.message_id=r.evidence_id),0)",
            Family::Transcript=>"COALESCE(length(CAST(r.content_hash AS BLOB)),0)+COALESCE(length(CAST(r.transcript_seq AS BLOB)),0)+COALESCE(length(CAST(r.turn_id AS BLOB)),0)",
            Family::ToolExecution=>"COALESCE(length(CAST(r.content_hash AS BLOB)),0)+COALESCE(length(CAST(r.work_item_id AS BLOB)),0)+COALESCE(length(CAST(r.turn_id AS BLOB)),0)",
            Family::Brief=>"COALESCE(length(CAST(r.content_hash AS BLOB)),0)+COALESCE(length(CAST(r.created_event_seq AS BLOB)),0)",
            Family::LegacyDelivery=>"COALESCE(length(CAST(r.content_hash AS BLOB)),0)+COALESCE(length(CAST(r.kind AS BLOB)),0)+COALESCE(length(CAST(r.work_item_id AS BLOB)),0)+COALESCE(length(CAST(r.message_id AS BLOB)),0)+COALESCE(length(CAST(r.task_id AS BLOB)),0)+COALESCE(length(CAST(r.turn_id AS BLOB)),0)",
            Family::Attempt=>"COALESCE(length(CAST(r.lifecycle_state AS BLOB)),0)+COALESCE(length(CAST(r.source_generation AS BLOB)),0)+COALESCE(length(CAST(r.terminal_outcome_id AS BLOB)),0)+COALESCE(length(CAST(r.recovery_of_attempt_id AS BLOB)),0)",
            Family::Outcome=>"COALESCE(length(CAST(r.attempt_id AS BLOB)),0)",
            Family::WorkExecution=>"COALESCE(length(CAST(r.source_revision AS BLOB)),0)+COALESCE(length(CAST(r.generation AS BLOB)),0)+COALESCE(length(CAST(r.lifecycle_state AS BLOB)),0)",
        };
        let created = match key.family {
            Family::Attempt | Family::Outcome | Family::WorkExecution => "0",
            _ => "COALESCE(length(CAST(r.created_at AS BLOB)),0)",
        };
        let payload = if key.family == Family::ToolExecution {
            "0"
        } else {
            "length(CAST(r.payload_json AS BLOB))"
        };
        let (body,body_rows):(usize,usize)=tx.query_row(&format!("SELECT COALESCE(SUM({payload}+length(CAST(r.agent_id AS BLOB))+length(CAST(r.{id} AS BLOB))+{created}+{columns}),0),COUNT(*) FROM {table} r WHERE r.agent_id=?1 AND r.{id}=?2"),params![self.scope.agent_id,key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let (alias,alias_rows):(usize,usize)=tx.query_row("SELECT COALESCE(SUM(length(CAST(revision AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+length(CAST(fingerprint_json AS BLOB))+length(CAST(availability AS BLOB))+COALESCE(length(CAST(attempt_turn_proof_json AS BLOB)),0)),0),COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",params![self.scope.agent_id,self.scope.identity_incarnation,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let (fixed, fixed_rows): (usize, usize) = if let Some(seed) = binding.seed_id.as_deref() {
            tx.query_row("SELECT COALESCE(SUM(length(CAST(fingerprint_json AS BLOB))),0),COUNT(*) FROM input_evidence_seed_membership WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![seed,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?)))?
        } else {
            (0, 0)
        };
        let (overlay, overlay_rows): (usize, usize) = if let Some(seed) = binding.seed_id.as_deref()
        {
            tx.query_row("SELECT COALESCE(SUM(length(CAST(revision AS BLOB))+length(CAST(origin_kind AS BLOB))+COALESCE(length(CAST(origin_attempt_id AS BLOB)),0)+COALESCE(length(CAST(command_kind AS BLOB)),0)+COALESCE(length(CAST(command_identity AS BLOB)),0)+COALESCE(length(CAST(command_hash AS BLOB)),0)+COALESCE(length(CAST(maintenance_proof_json AS BLOB)),0)),0),COUNT(*) FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family=?2 AND record_id=?3",params![seed,key.family.as_str(),key.id],|r|Ok((r.get(0)?,r.get(1)?)))?
        } else {
            (0, 0)
        };
        let exhausted = || InputEvidenceSnapshotError::ResourceExhausted("Input selected metadata");
        let bytes = body
            .checked_add(alias)
            .and_then(|n| n.checked_add(fixed))
            .and_then(|n| n.checked_add(overlay))
            .and_then(|n| n.checked_add(key.id.len()))
            .context(exhausted())?;
        let rows = body_rows
            .checked_add(alias_rows)
            .and_then(|n| n.checked_add(fixed_rows))
            .and_then(|n| n.checked_add(overlay_rows))
            .and_then(|n| n.checked_add(4))
            .context(exhausted())?;
        self.raw_cells(bytes, rows)?;
        let aliases:usize=tx.query_row("SELECT (SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_current r,json_tree(r.fingerprint_json) j WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family=?3 AND r.record_id=?4)+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_current r,json_tree(r.attempt_turn_proof_json) j WHERE r.agent_id=?1 AND r.identity_incarnation=?2 AND r.family=?3 AND r.record_id=?4)",params![self.scope.agent_id,self.scope.identity_incarnation,key.family.as_str(),key.id],|r|r.get(0))?;
        let fixed_nodes: usize = if let Some(seed) = binding.seed_id.as_deref() {
            tx.query_row("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_seed_membership r,json_tree(r.fingerprint_json) j WHERE r.seed_id=?1 AND r.family=?2 AND r.record_id=?3",params![seed,key.family.as_str(),key.id],|r|r.get(0))?
        } else {
            0
        };
        let overlay_nodes: usize = if let Some(seed) = binding.seed_id.as_deref() {
            tx.query_row("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_seed_recovery_current r,json_tree(r.maintenance_proof_json) j WHERE r.seed_id=?1 AND r.family=?2 AND r.record_id=?3",params![seed,key.family.as_str(),key.id],|r|r.get(0))?
        } else {
            0
        };
        let nodes = aliases
            .checked_add(fixed_nodes)
            .and_then(|n| n.checked_add(overlay_nodes))
            .and_then(|n| n.checked_mul(8))
            .context(exhausted())?;
        self.charge(0, nodes.checked_mul(256).context(exhausted())?, nodes)
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn record_body_tx(
        &mut self,
        tx: &Transaction<'_>,
        key: &Key,
    ) -> Result<()> {
        if key.family == Family::ToolExecution {
            return Ok(());
        }
        let (table, id) = table(key.family);
        let nodes:usize=tx.query_row(&format!("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM {table} r,json_tree(r.payload_json) j WHERE r.agent_id=?1 AND r.{id}=?2"),params![self.scope.agent_id,key.id],|r|r.get(0))?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Input selected body nodes");
        let nodes = nodes.checked_mul(2).context(exhausted())?;
        self.charge(0, nodes.checked_mul(256).context(exhausted())?, nodes)
    }
}

#[cfg(test)]
#[path = "input_read_records_tests.rs"]
mod tests;
