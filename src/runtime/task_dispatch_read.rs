//! Borrowed Task/Result facts for the original scheduler decision.
use super::*;
use crate::runtime::resident_task::{
    TaskAvailability, TaskResultDependencyRelation, TaskResultMetadata,
};
use crate::runtime::resident_work::{CapturedWork, WorkCaptureRequest};

pub(in crate::runtime) fn scheduler_task_request(
    scope: &super::super::resident_input_evidence::InputEvidenceScope,
    work: &WorkCaptureRequest,
    coverage: Option<&super::super::resident_task::TaskCoverage>,
    ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
) -> Result<(TaskCaptureRequest, Arc<BudgetViewGuard>)> {
    // These are the original finite command's Task/Wait addresses, not a new
    // historical role. Reserve the independent selector before copying IDs.
    let count = work.required_task_ids.len();
    let bytes = serialized_bytes(&work.required_task_ids)?
        .checked_add(
            u64::try_from(count)?
                .checked_mul(128)
                .context("dispatch Task selector nodes overflow")?,
        )
        .and_then(|n| n.checked_add(256))
        .and_then(|n| n.checked_mul(8))
        .context("dispatch Task selector bytes overflow")?;
    anyhow::ensure!(
        count.checked_mul(8).is_some_and(|n| n <= record_limit)
            && bytes <= u64::try_from(byte_limit)?,
        TaskReadError::ResourceExhausted("dispatch Task selectors")
    );
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?;
    // At most one borrowed entry per requested Task. The selector guard is
    // already live, and the complete Result inventory is traversed only once.
    let invalid: BTreeSet<&str> = coverage
        .into_iter()
        .flat_map(|coverage| {
            coverage
                .result_metadata
                .values()
                .filter(move |row| {
                    work.required_task_ids.contains(&row.task_id)
                        && work.required_trigger_message_ids.contains(&row.message_id)
                        && coverage.unsettled_result_ids.contains(&row.result_identity)
                        && coverage
                            .result_dependencies
                            .get(&row.result_identity)
                            .is_some_and(|observed| {
                                matches!(
                                    &observed.relation,
                                    TaskResultDependencyRelation::Invalid { .. }
                                )
                            })
                })
                .map(|row| row.task_id.as_str())
        })
        .collect();
    Ok((
        TaskCaptureRequest {
            include_active: true,
            include_unsettled: true,
            terminal_limit: 0,
            required_keys: work
                .required_task_ids
                .iter()
                .filter(|id| !invalid.contains(id.as_str()))
                .map(|id| TaskKey::new(TaskFamily::Task, id))
                .collect(),
        },
        guard,
    ))
}

pub(in crate::runtime) fn dispatch_task_record<'a>(
    tasks: &'a CapturedTasks,
    id: &str,
) -> Result<Option<&'a TaskRecord>> {
    let key = TaskKey::new(TaskFamily::Task, id);
    anyhow::ensure!(
        tasks.coverage.observations.get(&key) != Some(&TaskAvailability::Pruned),
        TaskReadError::UnavailableQuery(key)
    );
    task_record(tasks, id)
}

pub(in crate::runtime) fn dispatch_task_rejoin(
    tasks: &CapturedTasks,
    id: &str,
) -> Result<Option<crate::domain::execution_protocol::RejoinFence>> {
    dispatch_task_record(tasks, id)?
        .map(super::super::tasks::task_rejoin_fence)
        .transpose()
}

fn result_record<'a>(
    tasks: &'a CapturedTasks,
    metadata: &TaskResultMetadata,
) -> Result<&'a TaskResultSettlementRecord> {
    let key = TaskKey::new(TaskFamily::ResultSettlement, &metadata.result_identity);
    let row = tasks
        .lookup(&key)?
        .ok_or(TaskReadError::NeedsPreparation(key))?;
    row.validate(&tasks.scope)?;
    let CanonicalTaskFact::ResultSettlement(record) = &row.fact else {
        anyhow::bail!("dispatch Result family differs")
    };
    anyhow::ensure!(
        metadata.matches_record(record),
        "dispatch Result metadata differs from body"
    );
    Ok(record)
}

pub(in crate::runtime) fn dispatch_result_is_invalid(
    tasks: &CapturedTasks,
    message: &MessageEnvelope,
) -> Result<bool> {
    if message.kind != MessageKind::TaskResult {
        return Ok(false);
    }
    for row in tasks.coverage.result_metadata.values() {
        if row.message_id == message.id
            && tasks
                .coverage
                .unsettled_result_ids
                .contains(&row.result_identity)
        {
            result_record(tasks, row)?;
            return Ok(matches!(
                &tasks.result_dependency(&row.result_identity)?.relation,
                TaskResultDependencyRelation::Invalid { .. }
            ));
        }
    }
    Ok(false)
}

fn result_deliverable(
    tasks: &CapturedTasks,
    work: &CapturedWork,
    metadata: &TaskResultMetadata,
) -> Result<bool> {
    let record = result_record(tasks, metadata)?;
    let relation = tasks.result_dependency(&metadata.result_identity)?;
    let expected = match &relation.relation {
        TaskResultDependencyRelation::QualifiedCurrent { task } => task,
        TaskResultDependencyRelation::Invalid { .. } => return Ok(false),
    };
    // This header was observed from the actual Result source by the same-view
    // Task loader. It does not grant Message body or mutation authority.
    if metadata.reply_message_id.is_some() {
        return Ok(false);
    }
    let key = TaskKey::new(TaskFamily::Task, &record.task_id);
    let row = tasks
        .lookup(&key)?
        .ok_or(TaskReadError::NeedsPreparation(key))?;
    anyhow::ensure!(
        &row.locator == expected,
        "dispatch Result Task relation differs"
    );
    let task =
        dispatch_task_record(tasks, &record.task_id)?.context("dispatch related Task is absent")?;
    if !(task.agent_id == record.agent_id
        && task.effective_work_item_id() == record.work_item_id.as_deref()
        && super::super::task_state_reducer::is_terminal_task_status(&task.status)
        && task.rejoin_fence().is_ok_and(|fence| {
            fence.generation == record.rejoin_generation
                && fence.parent_turn_id == record.parent_turn_id
        }))
    {
        return Ok(false);
    }
    // Preserve the original Active-only/Open-owner filter without cloning Wait
    // or Work bodies into another unobserved owned collection.
    anyhow::ensure!(
        work.live_complete,
        super::super::resident_work::WorkReadError::IncompleteLive
    );
    for id in &work.unresolved_wait_ids {
        let wait = work
            .wait_condition(id)?
            .context("dispatch unresolved Wait is absent")?;
        anyhow::ensure!(
            matches!(
                wait.status,
                WaitConditionStatus::Active | WaitConditionStatus::Triggered
            ),
            "dispatch unresolved Wait has terminal status"
        );
        if wait.status == WaitConditionStatus::Active && wait.work_item_id == record.work_item_id {
            let open = match wait.work_item_id.as_deref() {
                None => true,
                Some(id) => work
                    .work_item(id)?
                    .is_some_and(|item| item.state == WorkItemState::Open),
            };
            if open {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub(in crate::runtime) fn task_result_recovery_deliverable(
    message: &MessageEnvelope,
    tasks: &CapturedTasks,
    work: &CapturedWork,
) -> Result<bool> {
    anyhow::ensure!(
        tasks.scope == work.scope && tasks.scope.agent_id == message.agent_id,
        "dispatch Task/Work/Message scopes differ"
    );
    anyhow::ensure!(
        tasks.coverage.unsettled_complete,
        TaskReadError::IncompleteResults
    );
    if !super::super::scheduler::runtime_owned_internal_followup(message)
        || message.authority_class != AuthorityClass::RuntimeInstruction
    {
        return Ok(false);
    }
    if let Some(owner) = message.source_refs.get("task_result_owner_key") {
        // Original SQL order and LIMIT 8. Full unsettled coverage still contains
        // Open CallerAdmitted rows; only this retry selection filters them.
        const LIMIT: usize =
            crate::runtime_db::task_result_settlement::TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT;
        let mut selected: [Option<&TaskResultMetadata>; LIMIT] = [None; LIMIT];
        for id in &tasks.coverage.unsettled_result_ids {
            let row = tasks.coverage.result_metadata.get(id).ok_or_else(|| {
                TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::ResultSettlement, id))
            })?;
            anyhow::ensure!(row.result_identity == *id, "dispatch Result index differs");
            if row.work_item_id != message.work_item_id || row.activation_open {
                continue;
            }
            let before = |old: &TaskResultMetadata| {
                row.created_at
                    .timestamp_millis()
                    .cmp(&old.created_at.timestamp_millis())
                    .then_with(|| row.result_identity.cmp(&old.result_identity))
                    .is_lt()
            };
            if let Some(index) = selected.iter().position(|old| old.is_none_or(before)) {
                selected[index..].rotate_right(1);
                selected[index] = Some(row);
            }
        }
        for metadata in selected.into_iter().flatten() {
            let row = result_record(tasks, metadata)?;
            if super::super::task_result_recovery::recovery_owner_key(row) == *owner
                && result_deliverable(tasks, work, metadata)?
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let Some(source) = message.source_refs.get("task_result_message_id") else {
        return Ok(false);
    };
    for id in &tasks.coverage.unsettled_result_ids {
        let row = tasks.coverage.result_metadata.get(id).ok_or_else(|| {
            TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::ResultSettlement, id))
        })?;
        if row.message_id == *source {
            if row.work_item_id != message.work_item_id
                || message.source_refs.get("task_result_identity") != Some(&row.result_identity)
            {
                return Ok(false);
            }
            return result_deliverable(tasks, work, row);
        }
    }
    Ok(false)
}

// Only borrowed traversal; no encoded body or temporary node vector exists
// before the plan's independent owned allocation has a guard.
pub(in crate::runtime) fn dispatch_plan_material(
    message: &MessageEnvelope,
    task: Option<&TaskRecord>,
) -> Result<(u64, u64)> {
    fn nodes(value: &serde_json::Value) -> Result<u64> {
        let children = match value {
            serde_json::Value::Array(values) => values.iter().try_fold(0u64, |n, value| {
                n.checked_add(nodes(value)?)
                    .context("dispatch Task array nodes overflow")
            })?,
            serde_json::Value::Object(values) => values.values().try_fold(0u64, |n, value| {
                n.checked_add(nodes(value)?)
                    .and_then(|n| n.checked_add(1))
                    .context("dispatch Task object nodes overflow")
            })?,
            _ => 0,
        };
        children
            .checked_add(1)
            .context("dispatch Task nodes overflow")
    }
    let count = message
        .metadata
        .as_ref()
        .map(nodes)
        .transpose()?
        .unwrap_or(0)
        .checked_add(
            task.and_then(|task| task.detail.as_ref())
                .map(nodes)
                .transpose()?
                .unwrap_or(0),
        )
        .and_then(|n| n.checked_add(8))
        .context("dispatch Task material nodes overflow")?;
    let records = count
        .checked_mul(8)
        .context("dispatch Task material records overflow")?;
    let bytes = serialized_bytes(&(message, task))?
        .checked_add(
            count
                .checked_mul(std::mem::size_of::<serde_json::Value>() as u64 + 128)
                .context("dispatch Task material slots overflow")?,
        )
        .and_then(|n| n.checked_add(std::mem::size_of::<TaskRecord>() as u64 + 512))
        .and_then(|n| n.checked_mul(8))
        .context("dispatch Task material bytes overflow")?;
    Ok((bytes, records))
}
