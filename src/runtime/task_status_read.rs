//! Status projection retains only the selected original facts and accounting.
use super::*;
use crate::types::TaskStatusSnapshot;

pub(crate) struct TaskStatusRead {
    pub snapshot: TaskStatusSnapshot,
    _selected: CurrentTaskRecordRead,
    _query: Option<super::super::task_queries::TaskQueryLease>,
    _owned: Arc<BudgetViewGuard>,
    _external: Option<Arc<BudgetViewGuard>>,
    material: StatusMaterialAllowance,
}

struct StatusMaterialAllowance {
    bytes: u64,
    records: u64,
    byte_limit: u64,
    record_limit: u64,
}
impl StatusMaterialAllowance {
    fn additional(&self, bytes: u64, records: u64) -> Result<Self> {
        let next = Self {
            bytes: self
                .bytes
                .checked_add(bytes)
                .context("TaskStatus combined bytes overflow")?,
            records: self
                .records
                .checked_add(records)
                .context("TaskStatus combined records overflow")?,
            byte_limit: self.byte_limit,
            record_limit: self.record_limit,
        };
        anyhow::ensure!(
            next.bytes <= next.byte_limit && next.records <= next.record_limit,
            TaskReadError::ResourceExhausted("TaskStatus combined material")
        );
        Ok(next)
    }
}

impl TaskStatusRead {
    fn transfer_child_observation(
        &mut self,
        ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
        observation: crate::types::ChildAgentObservabilitySnapshot,
    ) -> Result<()> {
        let bytes = serialized_bytes(&observation)?
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(512))
            .context("TaskStatus child material overflow")?;
        let next = self.material.additional(bytes, 32)?;
        let guard = ledger.register_owned_bytes(
            &self._selected.tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: bytes - 512,
                metadata_estimate_bytes: 512,
            },
        )?;
        self.snapshot.child_observability = Some(observation);
        self._external = Some(guard);
        self.material = next;
        Ok(())
    }
}

impl RuntimeHandle {
    fn capture_task_status_material(&self, id: &str) -> Result<TaskStatusRead> {
        self.capture_task_status_material_with_query(id, None)
    }

    fn capture_task_status_material_with_query(
        &self,
        id: &str,
        query: Option<super::super::task_queries::TaskQueryLease>,
    ) -> Result<TaskStatusRead> {
        let selected = self.capture_current_task_record(id)?;
        let record = selected
            .tasks
            .records
            .iter()
            .find(|record| {
                record.locator.key.family == TaskFamily::Task && record.locator.key.id == id
            })
            .ok_or_else(|| {
                crate::runtime_error::RuntimeError::not_found(
                    "task_not_found",
                    format!("task {id} not found"),
                )
                .with_safe_context("task_id", id)
            })?;
        let head = selected
            .tasks
            .coverage
            .status
            .heads
            .get(id)
            .ok_or(TaskReadError::IncompleteStatusProjection)?;
        let nodes = head.projection_material_nodes(record)?;
        let payload = serialized_bytes(&(&record.fact, head))?
            .checked_mul(8)
            .context("TaskStatus projection bytes overflow")?;
        let metadata = u64::try_from(nodes)?
            .checked_mul(std::mem::size_of::<serde_json::Value>() as u64)
            .and_then(|bytes| bytes.checked_mul(8))
            .context("TaskStatus projection nodes overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let (selected_bytes, selected_count) = selected.tasks.selected_material_estimate()?;
        let selector_bytes = serialized_bytes(&id)?
            .checked_add(128)
            .and_then(|bytes| bytes.checked_mul(4))
            .context("TaskStatus selector bytes overflow")?;
        let mut material = StatusMaterialAllowance {
            bytes: 0,
            records: 0,
            byte_limit: limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("TaskStatus projection limit overflow")?,
            record_limit: limits.record_limit,
        }
        .additional(
            selected_bytes
                .checked_add(selector_bytes)
                .context("TaskStatus selected total overflow")?,
            u64::try_from(selected_count)?
                .checked_add(4)
                .context("TaskStatus selected count overflow")?,
        )?;
        if let Some(query) = &query {
            let bytes = serialized_bytes(&(&selected.tasks.scope, query.selection()))?
                .checked_add(512)
                .and_then(|bytes| bytes.checked_mul(8))
                .context("TaskStatus query material overflow")?;
            let records = u64::try_from(query.selection().selector_record_count()?)?
                .checked_add(4)
                .and_then(|count| count.checked_mul(8))
                .context("TaskStatus query material count overflow")?;
            material = material.additional(bytes, records)?;
        }
        material = material.additional(
            payload
                .checked_add(metadata)
                .context("TaskStatus projection total overflow")?,
            u64::try_from(nodes)?
                .checked_mul(8)
                .context("TaskStatus projection count overflow")?,
        )?;
        let owned = self.inner.input_evidence_budget.register_owned_bytes(
            &selected.tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: payload,
                metadata_estimate_bytes: metadata,
            },
        )?;
        // Exact-version validation and every DTO clone follow the reservation.
        let observed = head.overlay(record)?;
        let mut snapshot = TaskStatusSnapshot::from_task_record(&observed);
        snapshot.child_supervision = head.child_supervision(record)?;
        Ok(TaskStatusRead {
            snapshot,
            _selected: selected,
            _query: query,
            _owned: owned,
            _external: None,
            material,
        })
    }

    /// Ordinary prepared reads are pure. Only an explicitly named missing
    /// historical Task enters the original owner preparation protocol.
    pub(crate) async fn task_status_read(&self, id: &str) -> Result<TaskStatusRead> {
        let mut read = match self
            .with_task_publication(|| self.capture_task_status_material(id))
            .await
        {
            Ok(read) => read,
            Err(error) if matches!(error.downcast_ref::<TaskReadError>(),Some(TaskReadError::NeedsPreparation(key)) if key.family == TaskFamily::Task && key.id == id) =>
            {
                let limits = self.inner.input_evidence_budget.limits();
                let bytes = serialized_bytes(&id)?
                    .checked_add(512)
                    .and_then(|bytes| bytes.checked_mul(8))
                    .context("named TaskStatus selector overflow")?;
                anyhow::ensure!(
                    limits.record_limit >= 8
                        && bytes
                            <= limits
                                .history_per_agent_bytes
                                .checked_add(limits.pending_per_agent_bytes)
                                .context("named TaskStatus limit overflow")?,
                    TaskReadError::ResourceExhausted("named TaskStatus selector")
                );
                let _selector = self.inner.input_evidence_budget.register_host_owned_bytes(
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: bytes,
                    },
                )?;
                let mut owner = self.begin_mutation().await?;
                let required = [TaskKey::new(TaskFamily::Task, id)];
                let query =
                    self.register_task_query(super::super::task_queries::TaskQuerySpec {
                        required_keys: &required,
                        ..Default::default()
                    })?;
                owner.prepare_owner_task_query(&query).await?;
                drop(owner);
                self.with_task_publication(|| {
                    self.capture_task_status_material_with_query(id, Some(query))
                })
                .await?
            }
            Err(error) => return Err(error),
        };
        if read
            ._selected
            .record()
            .is_some_and(TaskRecord::is_child_agent_task)
            && matches!(
                read.snapshot.status,
                TaskStatus::Queued
                    | TaskStatus::Running
                    | TaskStatus::Cancelling
                    | TaskStatus::Interrupted
            )
        {
            if let (Some(bridge), Some(child)) = (
                self.inner.host_bridge.as_ref(),
                read.snapshot.child_agent_id.as_deref(),
            ) {
                if let Some(observation) = bridge.child_observability(child).await? {
                    // The bridge owns the external observation; this carrier
                    // accounts its transfer into the final DTO without a clone.
                    read.transfer_child_observation(
                        &self.inner.input_evidence_budget,
                        observation,
                    )?;
                }
            }
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;

    #[tokio::test]
    async fn acknowledged_status_reads_keep_selected_material_across_await_without_storage(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let at = chrono::Utc::now();
        let task = TaskRecord {
            id: "owned-status".into(),
            agent_id: "default".into(),
            kind: crate::types::TaskKind::CommandTask,
            status: TaskStatus::Completed,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("actual original status".into()),
            detail: Some(serde_json::json!({"output_summary":"kept"})),
            recovery: None,
        };
        runtime
            .persist_task_transition(&task, "task_completed")
            .await?;
        let _resource_refresh = runtime.hold_resource_refresh_for_test().await?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch("ALTER TABLE tasks RENAME TO status_owned_tasks_forbidden;ALTER TABLE agent_states RENAME TO status_owned_state_forbidden;ALTER TABLE agent_identities RENAME TO status_owned_identity_forbidden;ALTER TABLE control_cancellation_obligations RENAME TO status_owned_control_forbidden;ALTER TABLE work_item_delegations RENAME TO status_owned_delegation_forbidden;")?;
            Ok(())
        })?;
        let read = runtime.task_status_read(&task.id).await?;
        assert_eq!(read.snapshot, TaskStatusSnapshot::from_task_record(&task));
        tokio::task::yield_now().await;
        let repeat = runtime.task_status_read(&task.id).await?;
        assert_eq!(read.snapshot, repeat.snapshot);
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
        drop((read, repeat));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        Ok(())
    }

    #[tokio::test]
    async fn actual_status_carrier_uses_remaining_allowance_for_child_transfer() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let at = chrono::Utc::now();
        let task = TaskRecord {
            id: "combined-status".into(),
            agent_id: "default".into(),
            kind: crate::types::TaskKind::ChildAgentTask,
            status: TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("actual selected child".repeat(100)),
            detail: Some(serde_json::json!({"child_agent_id":"bounded-child"})),
            recovery: None,
        };
        runtime
            .persist_task_transition(&task, "task_queued")
            .await?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let mut read = runtime.task_status_read(&task.id).await?;
        let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let observation = crate::types::ChildAgentObservabilitySnapshot {
            _read_material: None,
            _source_material: None,
            phase: crate::types::ChildAgentPhase::Running,
            blocked_reason: None,
            waiting_reason: None,
            current_work_item_id: None,
            work_summary: Some("x".repeat(usize::try_from(read.material.byte_limit / 4 - 512)?)),
            last_progress_brief: None,
            last_result_brief: None,
        };
        let standalone = serialized_bytes(&observation)?
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(512))
            .unwrap();
        assert!(standalone < read.material.byte_limit);
        assert!(read.material.bytes + standalone > read.material.byte_limit);
        let error = read
            .transfer_child_observation(&runtime.inner.input_evidence_budget, observation)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ));
        assert!(read.snapshot.child_observability.is_none() && read._external.is_none());
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            held
        );
        read.transfer_child_observation(
            &runtime.inner.input_evidence_budget,
            crate::types::ChildAgentObservabilitySnapshot {
                _read_material: None,
                _source_material: None,
                phase: crate::types::ChildAgentPhase::Running,
                blocked_reason: None,
                waiting_reason: None,
                current_work_item_id: None,
                work_summary: Some("bounded original child".into()),
                last_progress_brief: None,
                last_result_brief: None,
            },
        )?;
        assert_eq!(
            read.snapshot
                .child_observability
                .as_ref()
                .unwrap()
                .work_summary
                .as_deref(),
            Some("bounded original child")
        );
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > held);
        drop(read);
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        Ok(())
    }
}
