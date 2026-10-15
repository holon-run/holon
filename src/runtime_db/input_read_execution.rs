//! Finite execution-source and derived-ID material of the original Input read.
use super::*;
impl InputReadMaterial<'_> {
    pub(in crate::runtime_db::input_evidence_snapshot) fn execution_selectors(
        &mut self,
        mandatory: &BTreeSet<Key>,
        sources: &BTreeSet<String>,
    ) -> Result<()> {
        let bytes = usize::try_from(serialized_bytes(&(mandatory, sources))?)?;
        let count = mandatory.len().checked_add(sources.len()).context(
            InputEvidenceSnapshotError::ResourceExhausted("Input execution selectors"),
        )?;
        self.raw_cells(
            bytes,
            count
                .checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input execution selectors",
                ))?,
        )
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn execution_live_keys_tx(
        &mut self,
        tx: &Transaction<'_>,
        sql: &str,
        id: &str,
        binding: &Binding,
        limit: usize,
    ) -> Result<()> {
        // Use exactly the original qualified predicate/order/limit. SQL only
        // returns count/length; it never copies or qualifies another key.
        let scalar = sql.replacen(
            &format!("SELECT r.{id} FROM"),
            &format!("SELECT length(CAST(r.{id} AS BLOB)) AS bytes FROM"),
            1,
        );
        ensure!(
            scalar != sql,
            "Input live selector changed its original projection"
        );
        let (bytes, rows): (usize, usize) = tx.query_row(
            &format!("SELECT COALESCE(SUM(bytes),0),COUNT(*) FROM ({scalar})"),
            params![
                self.scope.agent_id,
                self.scope.identity_incarnation,
                binding.seed_id,
                i64::try_from(limit.saturating_add(1))?
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input live protocol IDs",
                ))?,
        )
    }
    /// This body is not copied by the SQL scalar probe. Check the same-phase
    /// remaining raw bound before JSON extraction; the selected body actor
    /// registers its owned bytes once when that body is actually read.
    fn execution_payload_probe_tx(
        &self,
        tx: &Transaction<'_>,
        table: &str,
        id: &str,
        value: &str,
    ) -> Result<()> {
        let raw: usize = tx.query_row(
            &format!("SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM {table} WHERE agent_id=?1 AND {id}=?2"),
            params![self.scope.agent_id,value], |r|r.get(0),
        )?;
        ensure!(
            raw.checked_mul(8)
                .and_then(|n| self.bytes.checked_add(n))
                .is_some_and(|n| n <= self.byte_limit),
            InputEvidenceSnapshotError::ResourceExhausted("Input execution SQL body probe")
        );
        Ok(())
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn execution_source_id_tx(
        &mut self,
        tx: &Transaction<'_>,
        attempt: &str,
    ) -> Result<()> {
        self.execution_payload_probe_tx(tx, "execution_protocol_attempts", "attempt_id", attempt)?;
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(json_extract(payload_json,'$.source_message_id') AS BLOB))),0),COUNT(*) FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",
            params![self.scope.agent_id,attempt], |r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input Attempt source ID",
                ))?,
        )
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn execution_work_id_tx(
        &mut self,
        tx: &Transaction<'_>,
        message: &str,
    ) -> Result<()> {
        self.execution_payload_probe_tx(tx, "messages", "message_id", message)?;
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(json_extract(payload_json,'$.work_item_id') AS BLOB))),0),COUNT(*) FROM messages WHERE agent_id=?1 AND message_id=?2",
            params![self.scope.agent_id,message], |r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input Message Work ID",
                ))?,
        )
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn execution_work_observation_tx(
        &mut self,
        tx: &Transaction<'_>,
        binding: &Binding,
        work: &str,
    ) -> Result<()> {
        let (bytes, rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(revision AS BLOB))+length(CAST(presence AS BLOB))),0),COUNT(*) FROM input_evidence_execution_current_work WHERE agent_id=?1 AND identity_incarnation=?2 AND work_item_id=?3",
            params![self.scope.agent_id,self.scope.identity_incarnation,work], |r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input Work presence",
                ))?,
        )?;
        if rows == 0 {
            if let Some(seed) = binding.seed_id.as_deref() {
                let (bytes, rows): (usize, usize) = tx.query_row(
                    "SELECT COALESCE(SUM(length(CAST(presence AS BLOB))),0),COUNT(*) FROM input_evidence_execution_seed_work WHERE seed_id=?1 AND work_item_id=?2",
                    params![seed,work], |r|Ok((r.get(0)?,r.get(1)?)),
                )?;
                self.raw_cells(
                    bytes,
                    rows.checked_mul(4)
                        .context(InputEvidenceSnapshotError::ResourceExhausted(
                            "Input fixed Work presence",
                        ))?,
                )?;
            }
        }
        Ok(())
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn execution_source_tx(
        &mut self,
        tx: &Transaction<'_>,
        binding: &Binding,
        source: &str,
    ) -> Result<()> {
        let scope = self.scope;
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM input_evidence_execution_current_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND source_message_id=?3)",
            params![scope.agent_id,scope.identity_incarnation,source], |r|r.get(0),
        )?;
        let mut has_source = current;
        if current {
            self.execution_source_row_tx(
                tx,
                "input_evidence_execution_current_sources",
                "agent_id=?1 AND identity_incarnation=?2 AND source_message_id=?3",
                params![scope.agent_id, scope.identity_incarnation, source],
                true,
            )?;
        } else if let Some(seed) = binding.seed_id.as_deref() {
            let recovery: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_evidence_execution_seed_source_recovery WHERE seed_id=?1 AND source_message_id=?2)",
                params![seed,source], |r|r.get(0),
            )?;
            let fixed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_evidence_execution_seed_sources WHERE seed_id=?1 AND source_message_id=?2)",
                params![seed,source], |r|r.get(0),
            )?;
            has_source = recovery || fixed;
            if recovery {
                self.execution_source_row_tx(
                    tx,
                    "input_evidence_execution_seed_source_recovery",
                    "seed_id=?1 AND source_message_id=?2",
                    params![seed, source],
                    true,
                )?;
                // Recovery validates the original source/Attempt overlay. Its
                // borrowed SQL footprint is bounded before those helpers copy.
                let selected = "(r.family='message' AND r.record_id=?2) OR (r.family='attempt' AND r.record_id IN (SELECT a.key FROM input_evidence_execution_seed_source_recovery s,json_each(s.metadata_json,'$.attempts') a WHERE s.seed_id=?1 AND s.source_message_id=?2))";
                let (bytes,rows):(usize,usize)=tx.query_row(&format!("SELECT COALESCE(SUM(length(CAST(r.origin_kind AS BLOB))+COALESCE(length(CAST(r.origin_attempt_id AS BLOB)),0)+COALESCE(length(CAST(r.command_kind AS BLOB)),0)+COALESCE(length(CAST(r.command_identity AS BLOB)),0)+COALESCE(length(CAST(r.command_hash AS BLOB)),0)+COALESCE(length(CAST(r.maintenance_proof_json AS BLOB)),0)),0),COUNT(*) FROM input_evidence_seed_recovery_current r WHERE r.seed_id=?1 AND ({selected})"),params![seed,source],|r|Ok((r.get(0)?,r.get(1)?)))?;
                self.raw_cells(
                    bytes,
                    rows.checked_mul(8)
                        .context(InputEvidenceSnapshotError::ResourceExhausted(
                            "Input source recovery overlay",
                        ))?,
                )?;
                let nodes:usize=tx.query_row(&format!("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM input_evidence_seed_recovery_current r,json_tree(r.maintenance_proof_json) j WHERE r.seed_id=?1 AND ({selected})"),params![seed,source],|r|r.get(0))?;
                let nodes =
                    nodes
                        .checked_mul(2)
                        .context(InputEvidenceSnapshotError::ResourceExhausted(
                            "Input source recovery native",
                        ))?;
                self.charge(
                    0,
                    nodes.checked_mul(256).context(
                        InputEvidenceSnapshotError::ResourceExhausted(
                            "Input source recovery native",
                        ),
                    )?,
                    nodes,
                )?;
                if !fixed {
                    self.origin_context_tx(tx, seed, &Key::new(Family::Message, source))?;
                }
            }
            if fixed {
                self.execution_source_row_tx(
                    tx,
                    "input_evidence_execution_seed_sources",
                    "seed_id=?1 AND source_message_id=?2",
                    params![seed, source],
                    false,
                )?;
            }
        }
        if !has_source {
            // Optional history without original coverage is not adopted, nor
            // does it authorize a scan of that source's raw Attempt bodies.
            return Ok(());
        }
        let (bytes,rows):(usize,usize)=tx.query_row("SELECT COALESCE(SUM(length(CAST(attempt_id AS BLOB))+length(CAST(lifecycle_state AS BLOB))+COALESCE(length(CAST(json_extract(payload_json,'$.admitted_at') AS BLOB)),0)+COALESCE(length(CAST(json_extract(payload_json,'$.turn_id') AS BLOB)),0)+COALESCE(length(CAST(terminal_outcome_id AS BLOB)),0)),0),COUNT(*) FROM execution_protocol_attempts WHERE agent_id=?1 AND json_extract(payload_json,'$.source_message_id')=?2",params![scope.agent_id,source],|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw_cells(
            bytes,
            rows.checked_mul(8)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input actual execution scalars",
                ))?,
        )
    }
    fn execution_source_row_tx(
        &mut self,
        tx: &Transaction<'_>,
        table: &str,
        filter: &str,
        params: &[&dyn rusqlite::ToSql],
        revision: bool,
    ) -> Result<()> {
        let revision = if revision {
            "+length(CAST(revision AS BLOB))"
        } else {
            ""
        };
        let (bytes,rows):(usize,usize)=tx.query_row(&format!("SELECT COALESCE(SUM(length(CAST(metadata_json AS BLOB))+length(CAST(content_digest AS BLOB)){revision}),0),COUNT(*) FROM {table} WHERE {filter}"),params,|r|Ok((r.get(0)?,r.get(1)?)))?;
        self.raw_cells(
            bytes,
            rows.checked_mul(4)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input execution source raw",
                ))?,
        )?;
        let nodes:usize=tx.query_row(&format!("SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM {table} r,json_tree(r.metadata_json) j WHERE {filter}"),params,|r|r.get(0))?;
        let nodes = nodes
            .checked_mul(2)
            .context(InputEvidenceSnapshotError::ResourceExhausted(
                "Input execution source native",
            ))?;
        self.charge(
            0,
            nodes
                .checked_mul(256)
                .context(InputEvidenceSnapshotError::ResourceExhausted(
                    "Input execution source native",
                ))?,
            nodes,
        )
    }
}

#[cfg(test)]
#[path = "input_read_execution_tests.rs"]
mod tests;
