//! An explicit full-output read keeps one bounded selected historical closure.
use super::*;
use crate::runtime_db::{
    input_evidence_snapshot::InputEvidenceLoadRequest,
    input_evidence_snapshot::TaskOutputInputRecall,
};
use anyhow::Context;

impl ResidentInputEvidence {
    pub(crate) fn with_task_output_recall(
        original: Option<&Self>,
        recall: TaskOutputInputRecall,
        budget: InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let (mut selected, task, mut material) = recall.into_parts()?;
        let limits = budget.limits();
        ensure!(
            original.is_none_or(|original| original.scope == selected.scope),
            "Output recall crossed Input namespace"
        );
        let (metadata, count) = original
            .map(|domain| domain.task_output_recall_copy_material(selected.records.len()))
            .transpose()?
            .unwrap_or_default();
        // SQL/predecode guards are still alive. Admit this overlapping copy
        // against that exact original request before cloning the old domain.
        material.admit_copy(&budget, metadata, count)?;
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .ok_or_else(|| anyhow::anyhow!("Output recall byte limit overflow"))?;
        ensure!(
            u64::try_from(count)? <= limits.record_limit && metadata <= byte_limit,
            "Output recall metadata exceeds the actual owner request"
        );
        let _original_copy = budget.register_owned_bytes(
            &selected.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: metadata,
            },
        )?;
        let roots = std::mem::take(&mut selected.coverage.mandatory_roots);
        let mut next = if let Some(original) = original {
            let mut next = original.clone();
            next.output_recall_roots.clear();
            next.output_recall_task = None;
            next.trim_and_validate()?;
            next.merge_output_recall_records(selected.records)?;
            for (id, assigned) in selected.coverage.message_turn_keys {
                if let Some(existing) = next.coverage.message_turn_keys.get(&id) {
                    ensure!(
                        *existing == assigned,
                        "Output assignment differs from the published Input relation"
                    );
                }
                next.coverage.message_turn_keys.insert(id, assigned);
            }
            for (id, transcripts) in selected.coverage.turn_transcript_keys {
                if let Some(existing) = next.coverage.turn_transcript_keys.get(&id) {
                    ensure!(
                        *existing == transcripts,
                        "Output Transcript frontier differs from the published Input relation"
                    );
                }
                next.coverage.turn_transcript_keys.insert(id, transcripts);
            }
            next
        } else {
            selected
        };
        next.output_recall_roots = roots;
        next.output_recall_task = Some(task);
        next.coverage.record_limit = usize::try_from(limits.record_limit)?;
        next.coverage.byte_limit = usize::try_from(byte_limit)?;
        next.trim_and_validate()?;
        next.with_budget(budget)
    }

    pub(super) fn merge_output_recall_records(
        &mut self,
        records: BTreeMap<InputEvidenceKey, Arc<QualifiedInputEvidence>>,
    ) -> Result<()> {
        for (key, record) in records {
            if let Some(existing) = self.records.get(&key) {
                ensure!(
                    existing.as_ref() == record.as_ref(),
                    "Output recall cannot replace another published Input version"
                );
            } else {
                self.window_projection = None;
                self.records.insert(key, record);
            }
        }
        Ok(())
    }

    pub(crate) fn task_output_recall_copy_material(
        &self,
        selected_records: usize,
    ) -> Result<(u64, usize)> {
        Ok((
            self.metadata_estimate_bytes()?
                .checked_mul(4)
                .context("Output original metadata copies overflow")?,
            self.records
                .len()
                .checked_add(selected_records)
                .and_then(|count| count.checked_mul(4))
                .context("Output merged metadata count overflow")?,
        ))
    }

    /// Task-only changes must revalidate the exact Task facet. An older recall
    /// may retain its bytes, but cannot impersonate the new Task's output view.
    pub(crate) fn capture_task_output_for_request(
        &self,
        task: &crate::runtime::resident_task::TaskFactLocator,
        request: &InputEvidenceLoadRequest,
    ) -> Result<CapturedInputEvidence> {
        let captured = self.capture_for_request(request)?;
        if captured
            .records
            .iter()
            .any(|record| self.output_recall_roots.contains(&record.locator.key))
        {
            ensure!(
                self.output_recall_task.as_ref() == Some(task),
                InputEvidenceReadError::NeedsPreparation(
                    request
                        .mandatory_roots
                        .first()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Output recall has no selected source"))?
                )
            );
        }
        Ok(captured)
    }
}
