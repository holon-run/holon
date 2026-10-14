//! Selected Task facts from one owner publication; acquisition is explicit.

use super::state_mutation::RuntimeMutation;
use super::*;
use crate::types::TaskListEntry;
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
        },
        resident_task::{
            CanonicalTaskFact, CapturedTasks, TaskCaptureRequest, TaskFamily, TaskKey,
            TaskReadError,
        },
    },
    runtime_db::task_result_settlement::{TaskResultSettlementRecord, TaskResultSettlementState},
};
use std::collections::BTreeSet;

#[path = "task_dispatch_read.rs"]
mod dispatch;
pub(in crate::runtime) use dispatch::{
    dispatch_plan_material, dispatch_result_is_invalid, dispatch_task_record, dispatch_task_rejoin,
    scheduler_task_request, task_result_recovery_deliverable,
};

#[path = "task_status_read.rs"]
mod status;
pub(crate) use status::TaskStatusRead;

#[path = "task_active_records.rs"]
mod active_records;

#[path = "task_message_wait_read.rs"]
mod message_wait;

pub(in crate::runtime) struct TaskReadSnapshot {
    pub token: mutation_owner::PublicationToken,
    pub state: Arc<AgentState>,
    pub tasks: CapturedTasks,
}

pub(crate) fn active_task_request() -> TaskCaptureRequest {
    TaskCaptureRequest {
        include_active: true,
        include_unsettled: false,
        terminal_limit: 0,
        required_keys: BTreeSet::new(),
    }
}

pub(crate) fn active_task_count(tasks: &CapturedTasks) -> Result<usize> {
    if !tasks.coverage.active_complete {
        return Err(TaskReadError::IncompleteActive.into());
    }
    Ok(tasks.coverage.active_task_ids.len())
}

/// Complete active metadata is an observation, not a bounded historical list.
pub(crate) fn active_task_updated_after(
    tasks: &CapturedTasks,
    anchor: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    active_task_count(tasks)?;
    for id in &tasks.coverage.active_task_ids {
        let meta =
            tasks.coverage.task_metadata.get(id).ok_or_else(|| {
                TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::Task, id))
            })?;
        anyhow::ensure!(
            meta.task_id == *id
                && matches!(
                    meta.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                ),
            "Task active metadata differs from its namespace"
        );
        // The old signal predicate compared payload DateTime, not SQL ordering.
        if meta.updated_at > anchor {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn task_record<'a>(
    tasks: &'a CapturedTasks,
    id: &str,
) -> Result<Option<&'a TaskRecord>> {
    tasks
        .lookup(&TaskKey::new(TaskFamily::Task, id))?
        .map(|row| {
            row.validate(&tasks.scope)?;
            match &row.fact {
                CanonicalTaskFact::Task(task) => Ok(task),
                _ => anyhow::bail!("selected Task fact family mismatch"),
            }
        })
        .transpose()
}

/// Original active_for_agent order, before any display limit.
pub(crate) fn active_task_records(tasks: &CapturedTasks) -> Result<Vec<&TaskRecord>> {
    active_task_count(tasks)?;
    let mut rows = tasks
        .coverage
        .active_task_ids
        .iter()
        .map(|id| {
            let key = TaskKey::new(TaskFamily::Task, id);
            let task = task_record(tasks, id)?.ok_or(TaskReadError::NeedsPreparation(key))?;
            anyhow::ensure!(
                matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                ),
                "Task active metadata differs from its body"
            );
            Ok(task)
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|a, b| {
        b.updated_at
            .timestamp_millis()
            .cmp(&a.updated_at.timestamp_millis())
            .then_with(|| {
                b.created_at
                    .timestamp_millis()
                    .cmp(&a.created_at.timestamp_millis())
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

/// Open activations remain present. Retry filtering is a separate operation.
pub(crate) fn admitted_result_ids<'a>(
    tasks: &'a CapturedTasks,
    activation: &str,
) -> Result<Vec<&'a str>> {
    if !tasks.coverage.unsettled_complete {
        return Err(TaskReadError::IncompleteResults.into());
    }
    let mut rows = tasks
        .coverage
        .activation_result_ids
        .get(activation)
        .into_iter()
        .flatten()
        .map(|id| {
            let metadata = tasks.coverage.result_metadata.get(id).ok_or_else(|| {
                TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::ResultSettlement, id))
            })?;
            anyhow::ensure!(
                metadata.activation_id.as_deref() == Some(activation)
                    && metadata.state == TaskResultSettlementState::CallerAdmitted
                    && tasks.coverage.unsettled_result_ids.contains(id),
                "Task activation index differs from admitted metadata"
            );
            Ok(metadata)
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|a, b| {
        a.created_at
            .timestamp_millis()
            .cmp(&b.created_at.timestamp_millis())
            .then_with(|| a.result_identity.cmp(&b.result_identity))
    });
    Ok(rows
        .into_iter()
        .map(|row| row.result_identity.as_str())
        .collect())
}

pub(crate) fn admitted_results<'a>(
    tasks: &'a CapturedTasks,
    activation: &str,
) -> Result<Vec<&'a TaskResultSettlementRecord>> {
    admitted_result_ids(tasks, activation)?
        .into_iter()
        .map(|id| {
            let key = TaskKey::new(TaskFamily::ResultSettlement, id);
            let row = tasks
                .lookup(&key)?
                .ok_or_else(|| TaskReadError::NeedsPreparation(key))?;
            row.validate(&tasks.scope)?;
            let CanonicalTaskFact::ResultSettlement(record) = &row.fact else {
                anyhow::bail!("Task admitted fact family mismatch")
            };
            let metadata = &tasks.coverage.result_metadata[id];
            anyhow::ensure!(
                metadata.matches_record(record),
                "Task admitted metadata differs from body"
            );
            Ok(record)
        })
        .collect()
}

pub(crate) fn context_task_request(
    tasks: &CapturedTasks,
    state: &AgentState,
    ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
) -> Result<(TaskCaptureRequest, Arc<BudgetViewGuard>)> {
    anyhow::ensure!(
        state.id == tasks.scope.agent_id,
        "context Task selector State scope differs"
    );
    let activation = state
        .current_execution_binding
        .as_ref()
        .and_then(|binding| binding.activation_id.as_deref());
    let ids = if let Some(activation) = activation {
        if !tasks.coverage.unsettled_complete {
            return Err(TaskReadError::IncompleteResults.into());
        }
        let ids = tasks.coverage.activation_result_ids.get(activation);
        for id in ids.into_iter().flatten() {
            let metadata = tasks.coverage.result_metadata.get(id).ok_or_else(|| {
                TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::ResultSettlement, id))
            })?;
            anyhow::ensure!(
                metadata.activation_id.as_deref() == Some(activation)
                    && metadata.state == TaskResultSettlementState::CallerAdmitted
                    && tasks.coverage.unsettled_result_ids.contains(id),
                "Task activation index differs from admitted metadata"
            );
        }
        ids
    } else {
        None
    };
    // Borrow metadata until its independent request allocation has a guard.
    let count = ids.map_or(0, BTreeSet::len);
    let bytes = serialized_bytes(&ids)?
        .checked_add(
            u64::try_from(count)?
                .checked_mul(128)
                .context("context Task selector nodes overflow")?,
        )
        .and_then(|n| n.checked_add(128))
        .and_then(|n| n.checked_mul(4))
        .context("context Task selector bytes overflow")?;
    let records = count
        .checked_mul(4)
        .context("context Task selector count overflow")?;
    anyhow::ensure!(
        records <= record_limit && bytes <= u64::try_from(byte_limit)?,
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "context Task selectors"
        )
    );
    let guard = ledger.register_owned_bytes(
        &tasks.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?;
    let mut request = active_task_request();
    request.required_keys.extend(
        ids.into_iter()
            .flatten()
            .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
    );
    Ok((request, guard))
}

impl RuntimeMutation {
    pub(in crate::runtime) fn capture_task_read(
        &self,
        request: &TaskCaptureRequest,
    ) -> Result<TaskReadSnapshot> {
        let (token, state, tasks) = self.capture_tasks_with_state(request)?;
        Ok(TaskReadSnapshot {
            token,
            state,
            tasks,
        })
    }

    pub(in crate::runtime) fn capture_scheduler_task_domains(
        &self,
        request: &super::resident_work::WorkCaptureRequest,
        task_request: &TaskCaptureRequest,
    ) -> Result<(
        Arc<AgentState>,
        super::resident_work::CapturedWork,
        super::resident_input_evidence::CapturedInputEvidence,
        CapturedTasks,
    )> {
        let root = self.capture_published_root()?;
        let work = root
            .work
            .as_ref()
            .ok_or(super::resident_work::WorkReadError::IncompleteLive)?;
        let inputs = root.input_evidence.as_ref().ok_or_else(||
            crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(
                "resident input domain has not been prepared".into()))?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            work.scope() == &self.input_evidence_scope()
                && work.scope() == inputs.scope()
                && inputs.scope() == tasks.scope(),
            "scheduler domain scope differs"
        );
        Ok((
            root.state.clone(),
            work.capture(request)?,
            inputs.capture()?,
            tasks.capture(task_request)?,
        ))
    }

    /// One final token for every domain used by loaded context acquisition.
    pub(in crate::runtime) fn capture_context_with_inputs_and_work_and_tasks(
        &self,
        episode_limit: usize,
        input_request: &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
        work_request: &super::resident_work::WorkCaptureRequest,
        task_request: &TaskCaptureRequest,
    ) -> Result<super::resident_state::CapturedAgentContext> {
        let root = self.capture_published_root()?;
        let inputs=root.input_evidence.as_ref().ok_or_else(||
            crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(
                "resident input domain has not been prepared".into()))?;
        let work = root
            .work
            .as_ref()
            .ok_or(super::resident_work::WorkReadError::IncompleteLive)?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            inputs.scope() == work.scope()
                && work.scope() == tasks.scope()
                && tasks.scope() == &self.input_evidence_scope(),
            "context domain scope differs"
        );
        let mut captured = root.capture_context(episode_limit)?;
        captured.input_evidence = Some(inputs.capture_for_request(input_request)?);
        captured.work = Some(work.capture(work_request)?);
        captured.tasks = Some(tasks.capture(task_request)?);
        Ok(captured)
    }
}

/// Ordinary current-task selection retains only selected Arcs and accounting.
/// No owner, published root, domain maps or durable reader survives capture.
pub(in crate::runtime) struct CurrentTaskRecordRead {
    tasks: CapturedTasks,
    index: Option<usize>,
    _selector: Arc<BudgetViewGuard>,
}
impl CurrentTaskRecordRead {
    pub(in crate::runtime) fn record(&self) -> Option<&TaskRecord> {
        self.index
            .map(|index| match &self.tasks.records[index].fact {
                CanonicalTaskFact::Task(task) => task,
                _ => unreachable!("validated Task capture family"),
            })
    }
}

impl RuntimeHandle {
    /// Pure ordinary acquisition. A missing prepared key is typed debt, not SQL.
    #[inline(never)]
    pub(in crate::runtime) fn capture_current_task_record(
        &self,
        id: &str,
    ) -> Result<CurrentTaskRecordRead> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Task root is unpublished")?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && tasks.scope().agent_id == self.inner.mutation_owner.agent_id()
                && tasks.scope().identity_incarnation == self.inner.identity_incarnation,
            "current Task capture scope differs"
        );
        let limits = self.inner.input_evidence_budget.limits();
        let bytes = serialized_bytes(&id)?
            .checked_add(128)
            .and_then(|bytes| bytes.checked_mul(4))
            .context("Task selector bytes overflow")?;
        anyhow::ensure!(
            limits.record_limit >= 4
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("Task selector limit overflow")?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "current Task selector"
            )
        );
        let guard = self.inner.input_evidence_budget.register_owned_bytes(
            tasks.scope(),
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let key = TaskKey::new(TaskFamily::Task, id);
        let capture = tasks.capture(&TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: BTreeSet::from([key.clone()]),
        })?;
        let present = task_record(&capture, id)?.is_some();
        let index = if present {
            Some(
                capture
                    .records
                    .iter()
                    .position(|record| record.locator.key == key)
                    .context("selected current Task body is missing")?,
            )
        } else {
            None
        };
        Ok(CurrentTaskRecordRead {
            tasks: capture,
            index,
            _selector: guard,
        })
    }

    /// Original output readiness status uses received Source metadata only.
    pub(crate) fn current_task_output_status(&self, id: &str) -> Result<TaskStatus> {
        let read = self.capture_current_task_record(id)?;
        let task = read.record().ok_or_else(|| {
            crate::runtime_error::RuntimeError::not_found(
                "task_not_found",
                format!("task {id} not found"),
            )
            .with_safe_context("task_id", id)
        })?;
        if matches!(
            task.status,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
                | TaskStatus::Cancelling
        ) {
            return Ok(task.status.clone());
        }
        Ok(read.tasks.output_source(id)?.map_or_else(
            || task.status.clone(),
            |source| source.message_status.clone(),
        ))
    }

    /// Wait for an active writer, then perform only the caller's synchronous
    /// resident read. An abandoned recovery gate remains an error.
    pub(in crate::runtime) async fn with_task_publication<T: Send>(
        &self,
        read: impl FnOnce() -> Result<T> + Send,
    ) -> Result<T> {
        let reservation = self.inner.mutation_owner.reserve().await?;
        let result = read();
        drop(reservation);
        result
    }

    pub(in crate::runtime) async fn capture_task_read(
        &self,
        request: &TaskCaptureRequest,
    ) -> Result<TaskReadSnapshot> {
        let _reservation = self.inner.mutation_owner.reserve().await?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Task root is unpublished")?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && tasks.scope().agent_id == root.state.id
                && tasks.scope().identity_incarnation == root.token.identity_incarnation
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "ordinary Task capture scope differs"
        );
        Ok(TaskReadSnapshot {
            token: root.token,
            state: root.state.clone(),
            tasks: tasks.capture(request)?,
        })
    }

    pub(in crate::runtime) fn task_projection_budget(
        &self,
        tasks: &CapturedTasks,
        records: &[&TaskRecord],
        copies: u64,
    ) -> Result<Arc<BudgetViewGuard>> {
        let bytes = serialized_bytes(&records)?
            .checked_mul(copies)
            .context("Task projection budget overflow")?;
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            &tasks.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: u64::try_from(records.len())?
                    .checked_mul(std::mem::size_of::<TaskRecord>() as u64)
                    .and_then(|size| size.checked_mul(copies))
                    .context("Task projection metadata overflow")?,
            },
        )?)
    }
}

/// Internal projection guard survives until its final synchronous wire return.
pub(crate) struct TaskListRead {
    pub total_active: usize,
    pub entries: Vec<TaskListEntry>,
    pub(crate) _owned_budget: Arc<BudgetViewGuard>,
}

#[cfg(test)]
#[path = "task_reads_tests.rs"]
pub(crate) mod tests;

/// Original completed-wait order is normalized created_at, then Task ID.
/// Only the original raw Work binding participates in this selector.
pub(in crate::runtime) fn reply_work_owner<'a>(
    tasks: &'a CapturedTasks,
    work: &super::resident_work::CapturedWork,
    message_id: &str,
) -> Result<Option<&'a str>> {
    anyhow::ensure!(tasks.scope == work.scope, "reply capture scope differs");
    anyhow::ensure!(
        work.live_complete,
        super::resident_work::WorkReadError::IncompleteLive
    );
    let mut selected: Option<&TaskRecord> = None;
    for id in tasks.reply_task_ids(message_id)? {
        let task = task_record(tasks, id)?.context("accepted reply Task body is missing")?;
        let Some(owner) = task.work_item_id.as_deref() else {
            continue;
        };
        if !work.live_work_ids.contains(owner) {
            continue;
        }
        let owner = work
            .work_item(owner)?
            .context("complete live Work body is missing")?;
        if owner.state != WorkItemState::Open {
            continue;
        }
        if selected.is_none_or(|old| {
            (task.created_at.timestamp_millis(), task.id.as_str())
                < (old.created_at.timestamp_millis(), old.id.as_str())
        }) {
            selected = Some(task);
        }
    }
    Ok(selected.and_then(|task| task.work_item_id.as_deref()))
}

#[cfg(test)]
#[path = "task_publication_read_tests.rs"]
mod publication_read_tests;
