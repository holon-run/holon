//! Borrowed Task/Result facts for the original scheduler decision.
use super::*;
use crate::runtime::resident_task::{
    TaskAvailability, TaskResultDependencyRelation, TaskResultMetadata,
};
use crate::runtime::resident_work::{CapturedWork, WorkCaptureRequest};
use crate::runtime_db::task_result_settlement::TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT;

/// Same-view predicates for one explicitly selected recovery Message. A missing
/// entry is unprepared; a present negative observation is not a grant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TaskRecoverySourceObservation {
    pub exact_source_digest: Option<String>,
    pub durable_source_digest: Option<String>,
    pub candidate: Option<TaskRecoveryCandidate>,
    pub exact_wait_id: Option<String>,
    pub wait_has_successor: bool,
    /// Whether this source's latest queue entry is Interrupted in the same view.
    pub queue_interrupted: bool,
    pub agent_not_stopped: bool,
    pub settled: bool,
    /// Current-epoch observations for paused candidates, using this source's
    /// canonical root. Missing entries must not become negative observations.
    pub paused_replays: BTreeMap<String, TaskRecoveryReplayObservation>,
}

impl TaskRecoverySourceObservation {
    fn ensure_prepared(&self) -> Result<()> {
        anyhow::ensure!(
            self.exact_source_digest.is_some() && self.durable_source_digest.is_some(),
            "Task recovery source digests were not prepared"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TaskRecoveryReplayObservation {
    pub recovery_epoch: u64,
    pub root_is_charged: bool,
    pub interrupted_anchor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TaskRecoveryCandidate {
    pub result_identity: String,
    pub task_id: String,
}

/// Hash exactly the fields compared by exact_waiter_recovery_source_tx, not the
/// Message body or unrelated refs which the original predicate never compared.
pub(crate) fn exact_recovery_source_digest(source: &MessageEnvelope) -> Result<String> {
    recovery_digest(&(
        &source.id,
        &source.agent_id,
        &source.work_item_id,
        &source.kind,
        &source.origin,
        &source.authority_class,
        &source.delivery_surface,
        &source.admission_context,
        [
            "wait_id",
            "task_result_message_id",
            "task_result_identity",
            "task_result_parent_turn_id",
            "task_result_rejoin_generation",
            "task_result_owner_key",
        ]
        .map(|key| source.source_refs.get(key)),
    ))
}

pub(crate) fn durable_recovery_source_digest(source: &MessageEnvelope) -> Result<String> {
    recovery_digest(source)
}

fn recovery_digest(value: &impl serde::Serialize) -> Result<String> {
    use sha2::{Digest, Sha256};
    struct DigestWriter(Sha256);
    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

fn recovery_source_observation<'a>(
    source: &MessageEnvelope,
    tasks: &'a CapturedTasks,
) -> Result<&'a TaskRecoverySourceObservation> {
    anyhow::ensure!(
        tasks.scope.agent_id == source.agent_id,
        "recovery Task/Message scopes differ"
    );
    let observed = tasks
        .coverage
        .recovery_sources
        .get(&source.id)
        .context("Task recovery source was not prepared")?;
    observed.ensure_prepared()?;
    Ok(observed)
}

fn recovery_source_shape(source: &MessageEnvelope) -> bool {
    source.kind == MessageKind::InternalFollowup
        && matches!(&source.origin, MessageOrigin::System { subsystem }
            if subsystem == "task_result_recovery")
        && source.authority_class == AuthorityClass::RuntimeInstruction
        && source.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
        && source.admission_context == Some(AdmissionContext::RuntimeOwned)
        && source.source_refs.contains_key("task_result_message_id")
}

pub(in crate::runtime) fn exact_waiter_recovery_wait<'a>(
    source: &MessageEnvelope,
    tasks: &CapturedTasks,
    work: &'a CapturedWork,
) -> Result<Option<&'a WaitConditionRecord>> {
    anyhow::ensure!(
        tasks.scope == work.scope,
        "recovery Task/Work scopes differ"
    );
    if !recovery_source_shape(source) {
        return Ok(None);
    }
    let observed = recovery_source_observation(source, tasks)?;
    let Some(candidate) = &observed.candidate else {
        return Ok(None);
    };
    if observed.exact_source_digest.as_deref()
        != Some(exact_recovery_source_digest(source)?.as_str())
    {
        return Ok(None);
    }
    let wait_id = observed
        .exact_wait_id
        .as_deref()
        .context("recovery candidate has no exact Wait observation")?;
    let wait = work
        .wait_condition(wait_id)?
        .context("recovery exact Wait is absent")?;
    anyhow::ensure!(
        wait.agent_id == source.agent_id
            && wait.work_item_id == source.work_item_id
            && wait.trigger_message_id() == Some(source.id.as_str())
            && wait.source.as_deref() == Some("WaitFor")
            && wait.kind == WaitConditionKind::Task
            && matches!(
                wait.status,
                WaitConditionStatus::Triggered | WaitConditionStatus::Resolved
            )
            && wait.subject_ref.as_deref() == Some(candidate.task_id.as_str())
            && matches!(
                wait.wake_sources.as_slice(),
                [WakeSource::TaskResult { task_id }] if task_id == &candidate.task_id
            )
            && source.source_refs.get("task_result_identity") == Some(&candidate.result_identity)
            && source
                .source_refs
                .get("wait_id")
                .is_none_or(|id| id == &wait.id),
        "recovery source candidate differs from captured exact Wait"
    );
    Ok(Some(wait))
}

pub(in crate::runtime) fn exact_waiter_recovery_replay_wait<'a>(
    source: &MessageEnvelope,
    tasks: &CapturedTasks,
    work: &'a CapturedWork,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<&'a WaitConditionRecord>> {
    let Some(wait) = exact_waiter_recovery_wait(source, tasks, work)? else {
        return Ok(None);
    };
    let observed = recovery_source_observation(source, tasks)?;
    Ok((observed.queue_interrupted
        && wait.status == WaitConditionStatus::Resolved
        && wait.cancelled_at.is_none()
        && wait.expires_at.is_none_or(|expires| expires > now)
        && !observed.wait_has_successor
        && observed.agent_not_stopped)
        .then_some(wait))
}

pub(in crate::runtime) fn settled_recovery_source(
    source: &MessageEnvelope,
    tasks: &CapturedTasks,
) -> Result<bool> {
    if !recovery_source_shape(source)
        || source.source_refs.contains_key("wait_id")
        || !source.source_refs.contains_key("task_result_owner_key")
    {
        return Ok(false);
    }
    let observed = recovery_source_observation(source, tasks)?;
    Ok(observed.settled
        && observed.durable_source_digest.as_deref()
            == Some(durable_recovery_source_digest(source)?.as_str()))
}

pub(crate) fn scheduler_task_request(
    scope: &super::super::resident_input_evidence::InputEvidenceScope,
    work: &WorkCaptureRequest,
    coverage: Option<&super::super::resident_task::TaskCoverage>,
    ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
) -> Result<(TaskCaptureRequest, Arc<BudgetViewGuard>)> {
    // These are the original finite command's Task/Wait addresses, not a new
    // historical role. Reserve the independent selector before copying IDs.
    let count = work
        .required_task_ids
        .len()
        .checked_add(work.required_trigger_message_ids.len())
        .context("dispatch Task selector count overflow")?;
    let bytes = serialized_bytes(&work.required_task_ids)?
        .checked_add(serialized_bytes(&work.required_trigger_message_ids)?)
        .context("dispatch recovery selector bytes overflow")?
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
            recovery_source_ids: work.required_trigger_message_ids.clone(),
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

pub(in crate::runtime) fn result_record<'a>(
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
    recovered_replies: &BTreeSet<String>,
    source: &MessageEnvelope,
) -> Result<bool> {
    let record = result_record(tasks, metadata)?;
    if record.recovery_paused_at.is_some() {
        let observed = recovery_source_observation(source, tasks)?
            .paused_replays
            .get(&record.result_identity)
            .context("paused Result recovery replay was not prepared")?;
        anyhow::ensure!(
            observed.recovery_epoch == record.recovery_epoch,
            "paused Result recovery observation has a different epoch"
        );
        if !observed.root_is_charged
            || observed.interrupted_anchor.as_deref() != Some(source.id.as_str())
        {
            return Ok(false);
        }
    }
    let relation = tasks.result_dependency(&metadata.result_identity)?;
    let expected = match &relation.relation {
        TaskResultDependencyRelation::QualifiedCurrent { task } => task,
        TaskResultDependencyRelation::Invalid { .. } => return Ok(false),
    };
    // This header was observed from the actual Result source by the same-view
    // Task loader. It does not grant Message body or mutation authority.
    if metadata.reply_message_id.is_some() && !recovered_replies.contains(&metadata.result_identity)
    {
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
        if wait.status == WaitConditionStatus::Active
            && wait.work_item_id == record.work_item_id
            && !(wait.kind == WaitConditionKind::Task
                && wait.trigger_message_id() == Some(source.id.as_str()))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(in crate::runtime) fn recovery_result_candidates<'a>(
    tasks: &'a CapturedTasks,
    owner: Option<&str>,
) -> Result<[Option<&'a TaskResultMetadata>; TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT]> {
    anyhow::ensure!(
        tasks.coverage.unsettled_complete,
        TaskReadError::IncompleteResults
    );
    let mut selected: [Option<&TaskResultMetadata>; TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT] =
        [None; TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT];
    for id in &tasks.coverage.unsettled_result_ids {
        let row = tasks.coverage.result_metadata.get(id).ok_or_else(|| {
            TaskReadError::NeedsPreparation(TaskKey::new(TaskFamily::ResultSettlement, id))
        })?;
        anyhow::ensure!(row.result_identity == *id, "dispatch Result index differs");
        if row.work_item_id.as_deref() != owner || row.activation_open {
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
    Ok(selected)
}

pub(in crate::runtime) fn task_result_recovery_deliverable(
    message: &MessageEnvelope,
    tasks: &CapturedTasks,
    work: &CapturedWork,
    recovered_replies: &BTreeSet<String>,
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
        let selected = recovery_result_candidates(tasks, message.work_item_id.as_deref())?;
        for metadata in selected.into_iter().flatten() {
            let row = result_record(tasks, metadata)?;
            if super::super::task_result_recovery::recovery_owner_key(row) == *owner
                && result_deliverable(tasks, work, metadata, recovered_replies, message)?
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
            return result_deliverable(tasks, work, row, recovered_replies, message);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_observation_requires_both_digests_even_for_negative_predicates() {
        let mut observed = TaskRecoverySourceObservation {
            exact_source_digest: Some("exact".into()),
            durable_source_digest: Some("durable".into()),
            candidate: None,
            exact_wait_id: None,
            wait_has_successor: false,
            queue_interrupted: false,
            agent_not_stopped: false,
            settled: false,
            paused_replays: BTreeMap::new(),
        };
        assert!(observed.ensure_prepared().is_ok());
        let exact = observed.exact_source_digest.take();
        assert!(observed.ensure_prepared().is_err());
        observed.exact_source_digest = exact;
        observed.durable_source_digest = None;
        assert!(observed.ensure_prepared().is_err());
    }
}
