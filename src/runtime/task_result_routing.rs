//! Named acquisition for the already accepted TaskResult reducer route.
use super::*;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
};
use crate::runtime::resident_task::{
    CanonicalTaskFact, CapturedTasks, TaskCaptureRequest, TaskFamily, TaskKey, TaskReadError,
    TaskResultDependencyRelation,
};
use crate::runtime::resident_work::{
    CanonicalWorkRecord, CapturedWork, WorkAvailability, WorkCaptureRequest, WorkFamily, WorkKey,
    WorkReadError,
};
use crate::runtime_db::{
    task_snapshot::{TaskDomainLoadRequest, TaskResultParentRead},
    TaskResultSettlementDisposition, TaskResultSettlementRecord,
};
use crate::types::{
    AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageKind, MessageOrigin,
};
use std::collections::BTreeSet;

/// Only this module mints the parsed-source branch. The Task's two constructor
/// clocks are local facts; every source-derived field must equal the real parser.
pub(crate) struct AcceptedTaskResultSource<'a> {
    message: &'a MessageEnvelope,
    candidate: &'a TaskRecord,
    replay: Option<&'a scheduler_executor::TaskResultReplayProjection>,
    _material: Arc<BudgetViewGuard>,
}
impl<'a> AcceptedTaskResultSource<'a> {
    #[cfg(test)]
    pub(crate) fn prepare_read_fixture(
        scope: &resident_input_evidence::InputEvidenceScope,
        message: &'a MessageEnvelope,
        candidate: &'a TaskRecord,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<(Self, (usize, usize))> {
        parsed_source_with_ledger(
            ledger,
            scope,
            message,
            candidate,
            None,
            record_limit,
            byte_limit,
        )
    }

    pub(crate) fn validate_candidate(
        &self,
        message: &MessageEnvelope,
        candidate: &TaskRecord,
    ) -> Result<()> {
        anyhow::ensure!(
            self.message == message && self.candidate == candidate,
            "TaskResult parsed source changed"
        );
        Ok(())
    }

    pub(crate) fn validate_canonical_source(
        &self,
        source: &resident_input_evidence::QualifiedInputEvidence,
    ) -> Result<()> {
        if let Some(replay) = self.replay {
            return replay.validate(source, self.message);
        }
        anyhow::ensure!(
            matches!(&source.record,
                resident_input_evidence::PreparedInputEvidence::Message(actual)
                    if actual == self.message),
            "TaskResult source differs from accepted canonical Message"
        );
        Ok(())
    }
}
fn parsed_source<'a>(
    runtime: &RuntimeHandle,
    scope: &resident_input_evidence::InputEvidenceScope,
    message: &'a MessageEnvelope,
    candidate: &'a TaskRecord,
    replay: Option<&'a scheduler_executor::TaskResultReplayProjection>,
    record_limit: usize,
    byte_limit: usize,
) -> Result<(AcceptedTaskResultSource<'a>, (usize, usize))> {
    parsed_source_with_ledger(
        &runtime.inner.input_evidence_budget,
        scope,
        message,
        candidate,
        replay,
        record_limit,
        byte_limit,
    )
}
fn parsed_source_with_ledger<'a>(
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    scope: &resident_input_evidence::InputEvidenceScope,
    message: &'a MessageEnvelope,
    candidate: &'a TaskRecord,
    replay: Option<&'a scheduler_executor::TaskResultReplayProjection>,
    record_limit: usize,
    byte_limit: usize,
) -> Result<(AcceptedTaskResultSource<'a>, (usize, usize))> {
    anyhow::ensure!(
        message.agent_id == scope.agent_id
            && candidate.agent_id == scope.agent_id
            && message.kind == MessageKind::TaskResult
            && message.authority_class == AuthorityClass::RuntimeInstruction
            && message.admission_context == Some(AdmissionContext::RuntimeOwned)
            && message.delivery_surface == Some(MessageDeliverySurface::TaskRejoin)
            && message.task_id.as_deref() == Some(candidate.id.as_str())
            && matches!(&message.origin,MessageOrigin::Task{task_id} if task_id==&candidate.id),
        "TaskResult parent read requires its original runtime-owned source"
    );
    let (bytes, records) = super::task_reads::dispatch_plan_material(message, None)?;
    let bytes = usize::try_from(bytes)?
        .checked_add(usize::try_from(serialized_bytes(candidate)?)?)
        .and_then(|n| n.checked_mul(4))
        .context("TaskResult parser material overflow")?;
    let records = usize::try_from(records)?
        .checked_mul(4)
        .and_then(|n| n.checked_add(16))
        .context("TaskResult parser nodes overflow")?;
    anyhow::ensure!(
        bytes <= byte_limit && records <= record_limit,
        TaskReadError::ResourceExhausted("TaskResult parsed source")
    );
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let mut actual = super::tasks::task_from_message(message, &scope.agent_id)?;
    actual.created_at = candidate.created_at;
    actual.updated_at = candidate.updated_at;
    anyhow::ensure!(
        actual == *candidate,
        "TaskResult candidate differs from its original Message parser"
    );
    Ok((
        AcceptedTaskResultSource {
            message,
            candidate,
            replay,
            _material: guard,
        },
        (bytes, records),
    ))
}
fn work_request(id: Option<&str>) -> WorkCaptureRequest {
    WorkCaptureRequest {
        include_live: false,
        nonopen_limit: 0,
        mandatory_roots: id
            .into_iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id))
            .collect(),
        latest_result_work_ids: BTreeSet::new(),
        required_turn_ids: BTreeSet::new(),
        required_task_ids: BTreeSet::new(),
        required_trigger_message_ids: BTreeSet::new(),
    }
}
fn exact_task_request(keys: BTreeSet<TaskKey>) -> TaskCaptureRequest {
    TaskCaptureRequest {
        include_active: false,
        include_unsettled: false,
        terminal_limit: 0,
        recovery_source_ids: Default::default(),
        required_keys: keys,
    }
}
fn required_domains(mutation: &state_mutation::RuntimeMutation) -> Result<()> {
    let root = mutation.capture_published_root()?;
    root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
    root.work.as_ref().ok_or(WorkReadError::IncompleteLive)?;
    root.input_evidence.as_ref().ok_or_else(|| {
        resident_input_evidence::InputEvidenceReadError::NeedsPreparation(
            resident_input_evidence::InputEvidenceKey::new(
                resident_input_evidence::InputEvidenceFamily::Message,
                "TaskResult route",
            ),
        )
    })?;
    Ok(())
}
fn capture_domains(
    mutation: &state_mutation::RuntimeMutation,
    work_request: &WorkCaptureRequest,
    task_request: &TaskCaptureRequest,
) -> Result<(Arc<AgentState>, CapturedWork, CapturedTasks)> {
    let root = mutation.capture_published_root()?;
    let scope = mutation.input_evidence_scope();
    let work = root.work.as_ref().ok_or(WorkReadError::IncompleteLive)?;
    let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("TaskResult Input domain is not prepared")?;
    anyhow::ensure!(
        work.scope() == &scope && tasks.scope() == &scope && inputs.scope() == &scope,
        "TaskResult publication scopes differ"
    );
    Ok((
        root.state.clone(),
        work.capture(work_request)?,
        tasks.capture(task_request)?,
    ))
}
fn selected_capture_material(tasks: &CapturedTasks, work: &CapturedWork) -> Result<(usize, usize)> {
    let (bytes, records) = tasks.selected_material_estimate()?;
    let work_bytes = work.records.iter().try_fold(
        usize::try_from(work.selected_metadata_estimate_bytes()?)?,
        |sum, row| {
            sum.checked_add(row.resident_bytes)
                .context("TaskResult Work material overflow")
        },
    )?;
    Ok((
        usize::try_from(bytes)?
            .checked_add(work_bytes)
            .context("TaskResult selected bytes overflow")?,
        records
            .checked_add(work.records.len())
            .and_then(|n| n.checked_add(work.observations.len()))
            .and_then(|n| n.checked_add(16))
            .context("TaskResult selected records overflow")?,
    ))
}
fn restricted_route_limits(
    actual: (usize, usize),
    restriction: Option<(usize, usize)>,
) -> Result<(usize, usize)> {
    let actual = TaskDomainLoadRequest::bounded_material_limits(actual.0, actual.1);
    let limits = restriction.map_or(actual, |(records, bytes)| {
        (records.min(actual.0), bytes.min(actual.1))
    });
    anyhow::ensure!(
        limits.0 > 0 && limits.1 > 0,
        TaskReadError::ResourceExhausted("TaskResult empty read allowance")
    );
    Ok(limits)
}
fn combined_route_material(
    selected: (usize, usize),
    selector_bytes: usize,
    other: (usize, usize),
    limits: (usize, usize),
) -> Result<(usize, usize)> {
    let combined = (
        selected
            .0
            .checked_add(selector_bytes)
            .and_then(|n| n.checked_add(other.0))
            .context("TaskResult combined bytes overflow")?,
        selected
            .1
            .checked_add(16)
            .and_then(|n| n.checked_add(other.1))
            .context("TaskResult combined records overflow")?,
    );
    anyhow::ensure!(
        combined.0 <= limits.1 && combined.1 <= limits.0,
        TaskReadError::ResourceExhausted("TaskResult combined selected/selector/outcome material")
    );
    Ok(combined)
}
fn owner_disposition(
    work: &CapturedWork,
    id: &str,
) -> Result<Option<TaskResultSettlementDisposition>> {
    let key = WorkKey::new(WorkFamily::Work, id);
    if work.observations.get(&key) == Some(&WorkAvailability::Pruned) {
        return Err(WorkReadError::NeedsPreparation(key).into());
    }
    match work.lookup(&key)? {
        None => Ok(Some(TaskResultSettlementDisposition::OwnerMissing)),
        Some(record) => {
            record.validate(&work.scope)?;
            let CanonicalWorkRecord::Work(owner) = &record.record else {
                anyhow::bail!("TaskResult owner family differs")
            };
            Ok((owner.state != WorkItemState::Open)
                .then_some(TaskResultSettlementDisposition::OwnerClosed))
        }
    }
}

pub(super) struct TaskResultOwnerRead {
    tasks: Option<CapturedTasks>,
    work: Option<CapturedWork>,
    owner_id: Option<String>,
    _selector: Option<Arc<BudgetViewGuard>>,
}
impl TaskResultOwnerRead {
    pub(super) fn disposition(&self) -> Result<Option<TaskResultSettlementDisposition>> {
        let (Some(tasks), Some(work), Some(id)) = (&self.tasks, &self.work, &self.owner_id) else {
            return Ok(None);
        };
        anyhow::ensure!(tasks.scope == work.scope, "TaskResult owner domains differ");
        owner_disposition(work, id)
    }
}

/// Kept as selected captures only, so the ordinary decision can be exercised
/// with every durable Task/Work/Turn/Brief reader disabled.
pub(super) struct TaskResultRoutingRead {
    tasks: CapturedTasks,
    _work: CapturedWork,
    ignore: bool,
    parent: Option<TaskResultParentRead>,
    _selector: Arc<BudgetViewGuard>,
}
impl TaskResultRoutingRead {
    pub(super) fn should_ignore(&self) -> bool {
        self.ignore
    }
    pub(super) fn parent_delivered(&self) -> Result<bool> {
        // Keep the exact selected Task allocation with this pure decision.
        anyhow::ensure!(
            self.tasks.scope == self._work.scope,
            "TaskResult selected domains differ"
        );
        self.parent
            .as_ref()
            .map_or(Ok(false), TaskResultParentRead::parent_delivered)
    }
}
impl RuntimeHandle {
    pub(super) async fn acquire_task_result_owner(
        &self,
        settlement: &TaskResultSettlementRecord,
    ) -> Result<TaskResultOwnerRead> {
        self.acquire_task_result_owner_inner(settlement, None).await
    }
    async fn acquire_task_result_owner_inner(
        &self,
        settlement: &TaskResultSettlementRecord,
        restriction: Option<(usize, usize)>,
    ) -> Result<TaskResultOwnerRead> {
        let Some(id) = settlement.work_item_id.as_deref() else {
            return Ok(TaskResultOwnerRead {
                tasks: None,
                work: None,
                owner_id: None,
                _selector: None,
            });
        };
        let mut mutation = self.begin_mutation().await?;
        required_domains(&mutation)?;
        let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        let (record_limit, byte_limit) =
            restricted_route_limits((record_limit, byte_limit), restriction)?;
        let selector_bytes = serialized_bytes(&(settlement.result_identity.as_str(), id))?
            .checked_add(512)
            .context("TaskResult owner selector overflow")?;
        anyhow::ensure!(
            selector_bytes <= u64::try_from(byte_limit)? && record_limit >= 16,
            TaskReadError::ResourceExhausted("TaskResult owner selectors")
        );
        let selector = self.inner.input_evidence_budget.register_owned_bytes(
            &mutation.input_evidence_scope(),
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: selector_bytes,
            },
        )?;
        let task_request = exact_task_request(BTreeSet::from([TaskKey::new(
            TaskFamily::ResultSettlement,
            &settlement.result_identity,
        )]));
        mutation
            .prepare_owner_task_domain_request(&TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: task_request.clone(),
                record_limit,
                byte_limit,
            })
            .await?;
        let captured = mutation.capture_task_read(&task_request)?;
        // Invalid dependencies carry no Work ownership grant. Preserve their
        // original settlement actor's outcome without following the raw hint.
        let relation = captured
            .tasks
            .result_dependency(&settlement.result_identity)?;
        if matches!(
            &relation.relation,
            TaskResultDependencyRelation::Invalid { .. }
        ) {
            let (bytes, records) = captured.tasks.selected_material_estimate()?;
            combined_route_material(
                (usize::try_from(bytes)?, records),
                usize::try_from(selector_bytes)?,
                (usize::try_from(serialized_bytes(settlement)?)?, 1),
                (record_limit, byte_limit),
            )?;
            return Ok(TaskResultOwnerRead {
                tasks: Some(captured.tasks),
                work: None,
                owner_id: None,
                _selector: Some(selector),
            });
        }
        let result = captured
            .tasks
            .lookup(&TaskKey::new(
                TaskFamily::ResultSettlement,
                &settlement.result_identity,
            ))?
            .context("TaskResult original settlement is absent")?;
        let CanonicalTaskFact::ResultSettlement(actual) = &result.fact else {
            anyhow::bail!("TaskResult settlement family differs")
        };
        anyhow::ensure!(
            actual.agent_id == settlement.agent_id
                && actual.task_id == settlement.task_id
                && actual.message_id == settlement.message_id
                && actual.work_item_id == settlement.work_item_id,
            "TaskResult original settlement address changed"
        );
        drop(captured);
        let request = work_request(Some(id));
        mutation.prepare_owner_work_request(&request).await?;
        let (_, work, tasks) = capture_domains(&mutation, &request, &task_request)?;
        tasks.result_dependency(&settlement.result_identity)?;
        combined_route_material(
            selected_capture_material(&tasks, &work)?,
            usize::try_from(selector_bytes)?,
            (usize::try_from(serialized_bytes(settlement)?)?, 1),
            (record_limit, byte_limit),
        )?;
        Ok(TaskResultOwnerRead {
            tasks: Some(tasks),
            work: Some(work),
            owner_id: Some(id.to_owned()),
            _selector: Some(selector),
        })
    }

    #[cfg(test)]
    pub(super) async fn acquire_task_result_routing(
        &self,
        message: &MessageEnvelope,
        candidate: &TaskRecord,
    ) -> Result<TaskResultRoutingRead> {
        self.acquire_task_result_routing_inner(message, candidate, None, None)
            .await
    }
    pub(super) async fn acquire_task_result_routing_for_dispatch(
        &self,
        message: &MessageEnvelope,
        candidate: &TaskRecord,
        replay: Option<&scheduler_executor::TaskResultReplayProjection>,
    ) -> Result<TaskResultRoutingRead> {
        self.acquire_task_result_routing_inner(message, candidate, None, replay)
            .await
    }
    async fn acquire_task_result_routing_inner(
        &self,
        message: &MessageEnvelope,
        candidate: &TaskRecord,
        restriction: Option<(usize, usize)>,
        replay: Option<&scheduler_executor::TaskResultReplayProjection>,
    ) -> Result<TaskResultRoutingRead> {
        let mut mutation = self.begin_mutation().await?;
        required_domains(&mutation)?;
        let scope = mutation.input_evidence_scope();
        let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        let (record_limit, byte_limit) =
            restricted_route_limits((record_limit, byte_limit), restriction)?;
        let bytes = serialized_bytes(&(candidate.id.as_str(), message.id.as_str()))?
            .checked_add(1024)
            .context("TaskResult route selector overflow")?;
        anyhow::ensure!(
            bytes <= u64::try_from(byte_limit)? && record_limit >= 16,
            TaskReadError::ResourceExhausted("TaskResult route selectors")
        );
        let selector = self.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let task_request = exact_task_request(BTreeSet::from([TaskKey::new(
            TaskFamily::Task,
            &candidate.id,
        )]));
        mutation
            .prepare_owner_task_domain_request(&TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: task_request.clone(),
                record_limit,
                byte_limit,
            })
            .await?;
        let work_request = work_request(None);
        // Select all three domains from one final publication. The empty Work
        // selector is explicit; it grants no additional historical Work body.
        let (state, work, tasks) = capture_domains(&mutation, &work_request, &task_request)?;
        let selected = combined_route_material(
            selected_capture_material(&tasks, &work)?,
            usize::try_from(bytes)?,
            replay.map_or((0, 0), |replay| replay.material()),
            (record_limit, byte_limit),
        )?;
        let current = super::task_reads::dispatch_task_record(&tasks, &candidate.id)?;
        let ignore = super::task_state_reducer::should_ignore_task_update_ref(current, candidate);
        let parent = if ignore {
            None
        } else {
            // The incoming parent is a source relationship even when a
            // canonical Task already exists with older/missing detail.
            let parsed = Some(parsed_source(
                self,
                &scope,
                message,
                candidate,
                replay,
                record_limit
                    .checked_sub(selected.1)
                    .filter(|n| *n > 0)
                    .ok_or(TaskReadError::ResourceExhausted(
                        "TaskResult parser record overlap",
                    ))?,
                byte_limit
                    .checked_sub(selected.0)
                    .filter(|n| *n > 0)
                    .ok_or(TaskReadError::ResourceExhausted(
                        "TaskResult parser byte overlap",
                    ))?,
            )?);
            let task_fact = tasks.lookup(&TaskKey::new(TaskFamily::Task, &candidate.id))?;
            let extra = parsed.as_ref().map_or((0, 0), |(_, material)| *material);
            // Selector is already included once. Add only the simultaneously
            // held parser proof before the DB phase subtracts this total.
            let selected = (
                selected
                    .0
                    .checked_add(extra.0)
                    .context("TaskResult parsed overlap bytes overflow")?,
                selected
                    .1
                    .checked_add(extra.1)
                    .context("TaskResult parsed overlap records overflow")?,
            );
            let request = TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: task_request.clone(),
                record_limit,
                byte_limit,
            };
            Some(self.inner.runtime_db.transaction(|tx| {
                crate::runtime_db::task_snapshot::prepare_task_result_parent_read_tx(
                    tx,
                    &scope,
                    &state,
                    tasks.coverage.revision,
                    work.revision,
                    task_fact.map(Arc::as_ref),
                    message,
                    candidate,
                    parsed.as_ref().map(|(proof, _)| proof),
                    selected,
                    &request,
                    &self.inner.input_evidence_budget,
                )
            })?)
        };
        Ok(TaskResultRoutingRead {
            tasks,
            _work: work,
            ignore,
            parent,
            _selector: selector,
        })
    }
}

#[cfg(test)]
#[path = "task_result_routing_tests.rs"]
mod tests;
