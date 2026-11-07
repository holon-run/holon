//! The HTTP active list returns canonical records, separate from display summaries.
use super::*;

pub(crate) struct ActiveTaskRecordsRead {
    pub records: Vec<TaskRecord>,
    _selected: CapturedTasks,
    _references: Arc<BudgetViewGuard>,
    _owned: Arc<BudgetViewGuard>,
}

impl RuntimeHandle {
    pub(crate) async fn active_task_records_read(
        &self,
        limit: usize,
    ) -> Result<ActiveTaskRecordsRead> {
        let tasks = self.capture_task_read(&active_task_request()).await?.tasks;
        let count = active_task_count(&tasks)?;
        let limits = self.inner.input_evidence_budget.limits();
        let max_bytes = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("active Task record request byte limit overflow")?;
        let (captured_bytes, captured_records) = tasks.selected_material_estimate()?;
        let refs_bytes = u64::try_from(count)?
            .checked_mul(std::mem::size_of::<&TaskRecord>() as u64)
            .and_then(|n| n.checked_add(512))
            .and_then(|n| n.checked_mul(4))
            .context("active Task reference bytes overflow")?;
        let refs_records = count
            .checked_mul(4)
            .context("active Task reference count overflow")?;
        anyhow::ensure!(
            captured_bytes
                .checked_add(refs_bytes)
                .is_some_and(|n| n <= max_bytes)
                && captured_records
                    .checked_add(refs_records)
                    .is_some_and(|n| u64::try_from(n).is_ok_and(|n| n <= limits.record_limit)),
            TaskReadError::ResourceExhausted("active Task record references")
        );
        let references = self.inner.input_evidence_budget.register_owned_bytes(
            &tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: refs_bytes,
            },
        )?;
        // Original SQL active predicate and millis/created/ID ordering, before limit.
        let mut selected = active_task_records(&tasks)?;
        selected.truncate(limit);
        let payload = serialized_bytes(&selected)?
            .checked_mul(8)
            .context("active Task record clone bytes overflow")?;
        let slots = u64::try_from(selected.len())?
            .checked_mul(std::mem::size_of::<TaskRecord>() as u64)
            .and_then(|n| n.checked_mul(8))
            .context("active Task record slots overflow")?;
        let bytes = captured_bytes
            .checked_add(refs_bytes)
            .and_then(|n| n.checked_add(payload))
            .and_then(|n| n.checked_add(slots))
            .context("active Task record combined bytes overflow")?;
        let records = selected
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(refs_records))
            .and_then(|n| n.checked_add(captured_records))
            .context("active Task record combined count overflow")?;
        anyhow::ensure!(
            bytes <= max_bytes && u64::try_from(records)? <= limits.record_limit,
            TaskReadError::ResourceExhausted("active Task record DTO material")
        );
        let owned = self.inner.input_evidence_budget.register_owned_bytes(
            &tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: payload,
                metadata_estimate_bytes: slots,
            },
        )?;
        let records = selected.into_iter().cloned().collect();
        Ok(ActiveTaskRecordsRead {
            records,
            _selected: tasks,
            _references: references,
            _owned: owned,
        })
    }
}
