//! Original TaskInput enqueue only needs the existing Queue terminal and creation-time scalars.
use super::{
    input_evidence_snapshot::{read_material::InputReadMaterial, InputEvidenceLoadRequest},
    repositories, RuntimeDb,
};
use crate::{
    runtime::{input_evidence_budget::InputEvidenceBudgetLedger, OriginalTaskInputChildMaterial},
    types::QueueEntryStatus,
};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, TransactionBehavior};

impl RuntimeDb {
    /// This scratch and decoded Queue die before the original owner await. The
    /// still-live Host request loan is the only prior entering the next phase.
    pub(crate) fn original_task_input_queue_observation(
        &self,
        message_id: &str,
        material: &OriginalTaskInputChildMaterial,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(bool, Option<chrono::DateTime<chrono::Utc>>)> {
        let scope = material.child_scope();
        material.validate_binding(scope, epoch, ledger)?;
        let limits = ledger.limits();
        let request = InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("TaskInput Queue limit overflow")?,
            )?
            .min(64 * 1024 * 1024),
            record_limit: usize::try_from(limits.record_limit)?.min(65536),
        };
        let mut observed =
            InputReadMaterial::new(scope, &request, ledger, material.material_estimate()?)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let size: Option<u64> = tx
            .query_row(
                "SELECT length(CAST(payload_json AS BLOB)) FROM queue_entries WHERE message_id=?1",
                [message_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(size) = size else {
            return Ok((false, None));
        };
        // Admit raw JSON parser scratch before json_tree, then all value and
        // object-key nodes before the original String/typed Queue decoder.
        observed.observe_projected(
            size.checked_add(512)
                .and_then(|n| n.checked_mul(8))
                .context("TaskInput Queue raw bytes overflow")?,
            8,
        )?;
        let native:usize=tx.query_row(
            "SELECT CASE WHEN json_valid(payload_json) THEN (SELECT COUNT(*)+COALESCE(SUM(CASE WHEN typeof(key)='text' THEN 1 ELSE 0 END),0) FROM json_tree(payload_json)) ELSE 0 END FROM queue_entries WHERE message_id=?1",
            [message_id],|row|row.get(0),
        )?;
        let native = native
            .checked_add(16)
            .and_then(|n| n.checked_mul(4))
            .context("TaskInput Queue native count overflow")?;
        observed.observe_projected(
            u64::try_from(native)?
                .checked_mul(256)
                .context("TaskInput Queue native bytes overflow")?,
            native,
        )?;
        let payload: String = tx.query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id=?1",
            [message_id],
            |row| row.get(0),
        )?;
        let queue = repositories::decode_queue_entry_payload(&payload)?;
        let terminal = matches!(
            queue.status,
            QueueEntryStatus::Dequeued
                | QueueEntryStatus::Interjected
                | QueueEntryStatus::Processed
                | QueueEntryStatus::Aborted
                | QueueEntryStatus::Dropped
                | QueueEntryStatus::Quarantined
        );
        let created_at = queue.created_at;
        drop(queue);
        drop(payload);
        drop(tx);
        drop(connection);
        drop(observed);
        Ok((terminal, Some(created_at)))
    }
}
