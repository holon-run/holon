//! Current AgentMessageWait decisions never acquire a durable Task body.
use super::*;

impl RuntimeHandle {
    pub(in crate::runtime) fn agent_message_wait_reference_budget(
        &self,
        tasks: &CapturedTasks,
    ) -> Result<Arc<BudgetViewGuard>> {
        let count = active_task_count(tasks)?;
        let (selected_bytes, selected_records) = tasks.selected_material_estimate()?;
        let metadata = count
            .checked_mul(std::mem::size_of::<&TaskRecord>())
            .and_then(|n| n.checked_add(std::mem::size_of::<Vec<&TaskRecord>>() + 512))
            .and_then(|n| n.checked_mul(4))
            .context("message Wait reference slots overflow")?;
        let records = count
            .checked_mul(4)
            .context("message Wait reference count overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let (max_records, max_bytes) =
            crate::runtime_db::task_snapshot::TaskDomainLoadRequest::bounded_material_limits(
                usize::try_from(limits.record_limit)?,
                usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("message Wait reference limits overflow")?,
                )?,
            );
        anyhow::ensure!(
            selected_bytes
                .checked_add(u64::try_from(metadata)?)
                .is_some_and(|n| n <= max_bytes as u64)
                && selected_records
                    .checked_add(records)
                    .is_some_and(|n| n <= max_records),
            TaskReadError::ResourceExhausted("message Wait active references")
        );
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            &tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(metadata)?,
            },
        )?)
    }

    pub(in crate::runtime) fn agent_message_wait_reply_detail_budget(
        &self,
        tasks: &CapturedTasks,
        task: &TaskRecord,
        target: &str,
        recipient: &str,
        delivery: &str,
        message: Option<&str>,
        boundary: i64,
    ) -> Result<(Arc<BudgetViewGuard>, (u64, usize))> {
        let count = active_task_count(tasks)?;
        let (selected_bytes, selected_records) = tasks.selected_material_estimate()?;
        let reference_bytes = count
            .checked_mul(std::mem::size_of::<&TaskRecord>())
            .and_then(|n| n.checked_add(std::mem::size_of::<Vec<&TaskRecord>>() + 512))
            .and_then(|n| n.checked_mul(4))
            .context("message Wait retained references overflow")?;
        let native = task
            .detail
            .as_ref()
            .map(message_wait_value_nodes)
            .transpose()?
            .unwrap_or(0)
            .checked_add(32)
            .context("message Wait reply projection nodes overflow")?;
        let payload =
            serialized_bytes(&(&task.detail, target, recipient, delivery, message, boundary))?
                .checked_mul(8)
                .context("message Wait reply projection bytes overflow")?;
        let metadata = native
            .checked_mul(std::mem::size_of::<serde_json::Value>() + 128)
            .and_then(|n| n.checked_mul(8))
            .context("message Wait reply projection metadata overflow")?;
        let bytes = selected_bytes
            .checked_add(u64::try_from(reference_bytes)?)
            .and_then(|n| n.checked_add(payload))
            .and_then(|n| n.checked_add(u64::try_from(metadata).ok()?))
            .context("message Wait retained reply bytes overflow")?;
        let records = selected_records
            .checked_add(
                count
                    .checked_mul(4)
                    .context("message Wait retained reference count overflow")?,
            )
            .and_then(|n| n.checked_add(native.checked_mul(8)?))
            .context("message Wait retained reply records overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let (max_records, max_bytes) =
            crate::runtime_db::task_snapshot::TaskDomainLoadRequest::bounded_material_limits(
                usize::try_from(limits.record_limit)?,
                usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("message Wait reply limits overflow")?,
                )?,
            );
        anyhow::ensure!(
            bytes <= u64::try_from(max_bytes)? && records <= max_records,
            TaskReadError::ResourceExhausted("message Wait retained reply detail")
        );
        let guard = self.inner.input_evidence_budget.register_owned_bytes(
            &tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: payload,
                metadata_estimate_bytes: u64::try_from(metadata)?,
            },
        )?;
        Ok((guard, (bytes, records)))
    }

    pub(in crate::runtime) fn validate_agent_message_wait_task(
        &self,
        original: &TaskRecord,
        current: &TaskRecord,
    ) -> Result<()> {
        anyhow::ensure!(
            original.agent_id == self.inner.mutation_owner.agent_id()
                && current.agent_id == original.agent_id,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ForeignRecord(TaskKey::new(
                TaskFamily::Task,
                &original.id
            )),
        );
        anyhow::ensure!(
            current.id == original.id
                && current.kind == crate::types::TaskKind::AgentMessageWait
                && original.kind == current.kind,
            crate::runtime_db::task_snapshot::TaskSnapshotError::CanonicalChanged(TaskKey::new(
                TaskFamily::Task,
                &original.id
            )),
        );
        Ok(())
    }

    pub(in crate::runtime) fn validate_agent_message_wait_generation(
        &self,
        original: &TaskRecord,
        current: &TaskRecord,
    ) -> Result<()> {
        fn field<'a>(task: &'a TaskRecord, name: &str) -> Option<&'a serde_json::Value> {
            task.detail.as_ref().and_then(|value| value.get(name))
        }
        let has_rejoin_contract = ["rejoin_obligation_id", "rejoin_generation"]
            .into_iter()
            .any(|name| field(original, name).is_some() || field(current, name).is_some());
        // A parser may add parent_turn_id as provenance without a rejoin contract.
        // Markers must describe the exact creator shape or a complete valid fence.
        let same_rejoin = !has_rejoin_contract
            || ([
                "rejoin_obligation_id",
                "rejoin_generation",
                "parent_turn_id",
            ]
            .into_iter()
            .all(|name| field(current, name) == field(original, name))
                && (super::super::tasks::unbound_task_rejoin_generation(original).is_some_and(
                    |generation| {
                        super::super::tasks::unbound_task_rejoin_generation(current)
                            == Some(generation)
                    },
                ) || original.rejoin_fence().ok().is_some_and(|original_fence| {
                    current.rejoin_fence().ok().as_ref() == Some(&original_fence)
                })));
        anyhow::ensure!(
            current.work_item_id == original.work_item_id
                && current.recovery == original.recovery
                && same_rejoin,
            crate::runtime_db::task_snapshot::TaskSnapshotError::CanonicalChanged(TaskKey::new(
                TaskFamily::Task,
                &original.id
            )),
        );
        Ok(())
    }

    pub(in crate::runtime) fn agent_message_wait_projection_budget(
        &self,
        selected: &CurrentTaskRecordRead,
        detail: &serde_json::Value,
        text: &str,
        retained_material: (u64, usize),
    ) -> Result<Arc<BudgetViewGuard>> {
        let task = selected.record().context("message Wait Task is absent")?;
        let count = message_wait_value_nodes(detail)?
            .checked_add(
                task.detail
                    .as_ref()
                    .map(message_wait_value_nodes)
                    .transpose()?
                    .unwrap_or(0),
            )
            .and_then(|n| n.checked_add(32))
            .context("message Wait material nodes overflow")?;
        let payload = serialized_bytes(&(task, detail, text))?
            .checked_mul(8)
            .context("message Wait projection bytes overflow")?;
        let metadata = count
            .checked_mul(std::mem::size_of::<serde_json::Value>() + 128)
            .and_then(|n| n.checked_add(std::mem::size_of::<TaskRecord>() + 512))
            .and_then(|n| n.checked_mul(8))
            .context("message Wait projection metadata overflow")?;
        let (captured_bytes, captured_records) = selected.tasks.selected_material_estimate()?;
        let selector = serialized_bytes(&task.id)?
            .checked_add(128)
            .and_then(|n| n.checked_mul(4))
            .context("message Wait selector overlap overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let (max_records, max_bytes) =
            crate::runtime_db::task_snapshot::TaskDomainLoadRequest::bounded_material_limits(
                usize::try_from(limits.record_limit)?,
                usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("message Wait byte limit overflow")?,
                )?,
            );
        let bytes = captured_bytes
            .checked_add(retained_material.0)
            .and_then(|n| n.checked_add(selector))
            .and_then(|n| n.checked_add(payload))
            .and_then(|n| n.checked_add(u64::try_from(metadata).ok()?))
            .context("message Wait combined bytes overflow")?;
        let records = count
            .checked_mul(8)
            .and_then(|n| n.checked_add(4))
            .and_then(|n| n.checked_add(captured_records))
            .and_then(|n| n.checked_add(retained_material.1))
            .context("message Wait combined records overflow")?;
        anyhow::ensure!(
            bytes <= u64::try_from(max_bytes)? && records <= max_records,
            TaskReadError::ResourceExhausted("message Wait terminal projection")
        );
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            &selected.tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: payload,
                metadata_estimate_bytes: u64::try_from(metadata)?,
            },
        )?)
    }
}

#[cfg(test)]
#[path = "task_message_wait_read_tests.rs"]
mod tests;

fn message_wait_value_nodes(value: &serde_json::Value) -> Result<usize> {
    let children = match value {
        serde_json::Value::Array(values) => values.iter().try_fold(0usize, |sum, value| {
            sum.checked_add(message_wait_value_nodes(value)?)
                .context("message Wait array nodes overflow")
        })?,
        serde_json::Value::Object(values) => values.values().try_fold(0usize, |sum, value| {
            sum.checked_add(message_wait_value_nodes(value)?)
                .and_then(|n| n.checked_add(1))
                .context("message Wait object nodes overflow")
        })?,
        _ => 0,
    };
    children
        .checked_add(1)
        .context("message Wait nodes overflow")
}
