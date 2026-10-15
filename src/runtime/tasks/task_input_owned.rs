//! An admitted input request retains delivery, its Audit and its result.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate, OwnedCompletionBudget,
    },
    resident_input_evidence::InputEvidenceScope,
    resident_task::{
        CanonicalTaskFact, CapturedTasks, TaskCaptureMaterial, TaskCaptureRequest, TaskFamily,
        TaskKey, TaskReadError,
    },
    task_queries::{TaskQueryLease, TaskQuerySpec},
};
use std::{collections::BTreeSet, ops::Deref, sync::Arc};

pub struct OwnedTaskInputResult {
    result: TaskInputResult,
    material: Arc<BudgetViewGuard>,
    usage: (u64, usize),
    ledger: InputEvidenceBudgetLedger,
}
impl Deref for OwnedTaskInputResult {
    type Target = TaskInputResult;
    fn deref(&self) -> &Self::Target {
        &self.result
    }
}
impl std::fmt::Debug for OwnedTaskInputResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.result.fmt(f)
    }
}
impl serde::Serialize for OwnedTaskInputResult {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.result.serialize(s)
    }
}
#[path = "task_input_wire.rs"]
mod wire;

struct OriginalTaskInputEventPlan {
    task_id: String,
    task_kind: TaskKind,
    target: String,
    child: Option<String>,
}
pub(in crate::runtime) struct OriginalTaskInputMaterial {
    scope: InputEvidenceScope,
    epoch: u64,
    ledger: InputEvidenceBudgetLedger,
    budget: OwnedCompletionBudget,
    usage: (u64, usize),
    selected: (u64, usize),
    limits: (u64, usize),
    event_plan: Option<OriginalTaskInputEventPlan>,
}
fn copy_cost(value: &impl serde::Serialize, copies: usize) -> Result<(u64, usize)> {
    let (encoded, nodes) = borrowed_json_material(value)?;
    let nodes = nodes
        .checked_add(32)
        .and_then(|n| n.checked_mul(copies))
        .context("TaskInput copy nodes overflow")?;
    let bytes = encoded
        .checked_add(1024)
        .and_then(|n| n.checked_mul(u64::try_from(copies).ok()?))
        .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
        .context("TaskInput copy bytes overflow")?;
    Ok((bytes, nodes))
}
impl OriginalTaskInputMaterial {
    pub(in crate::runtime) fn validate_owner(&self, runtime: &RuntimeHandle) -> Result<()> {
        anyhow::ensure!(
            self.scope.agent_id == runtime.instance_agent_id()
                && self.scope.identity_incarnation == runtime.instance_incarnation()
                && self.epoch == runtime.instance_owner_epoch()
                && self
                    .ledger
                    .same_ledger(&runtime.inner.input_evidence_budget)
                && self.budget.matches_ledger(&self.ledger),
            "original TaskInput material owner changed"
        );
        Ok(())
    }
    pub(in crate::runtime) fn source_parts(
        &self,
    ) -> (
        InputEvidenceScope,
        u64,
        InputEvidenceBudgetLedger,
        Arc<BudgetViewGuard>,
        usize,
    ) {
        (
            self.scope.clone(),
            self.epoch,
            self.ledger.clone(),
            self.budget.guard(),
            self.usage.1,
        )
    }
    fn check(&self, owned: (u64, usize), selected: (u64, usize)) -> Result<()> {
        anyhow::ensure!(
            owned
                .0
                .checked_add(selected.0)
                .is_some_and(|n| n <= self.limits.0)
                && owned
                    .1
                    .checked_add(selected.1)
                    .is_some_and(|n| n <= self.limits.1),
            TaskReadError::ResourceExhausted("TaskInput owned request, selected and DTO material")
        );
        Ok(())
    }
    fn remaining(&self) -> Result<(usize, usize)> {
        self.check(self.usage, self.selected)?;
        Ok((
            usize::try_from(self.limits.0 - self.usage.0 - self.selected.0)?,
            self.limits.1 - self.usage.1 - self.selected.1,
        ))
    }
    fn charge(&mut self, value: &impl serde::Serialize, copies: usize) -> Result<()> {
        anyhow::ensure!(
            self.event_plan.is_none(),
            "TaskInput request material is already sealed"
        );
        let cost = copy_cost(value, copies)?;
        let next = (
            self.usage
                .0
                .checked_add(cost.0)
                .context("TaskInput phase bytes overflow")?,
            self.usage
                .1
                .checked_add(cost.1)
                .context("TaskInput phase nodes overflow")?,
        );
        self.check(next, self.selected)?;
        self.budget.observe_material(cost.0, 0);
        self.usage = next;
        Ok(())
    }
    // Admission is before physical delivery. ID/time are still frozen only
    // after its original successful reply, as in the existing actor.
    pub(in crate::runtime) fn reserve_original_event(
        &mut self,
        task: &TaskRecord,
        target: &str,
        child: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(
            task.agent_id == self.scope.agent_id
                && match child {
                    None => task.kind == TaskKind::CommandTask,
                    Some(_) => task.is_child_agent_task(),
                },
            "TaskInput delivery has another original Task family"
        );
        self.charge(&(&task.id, &task.kind, u64::MAX, target, child), 4)?;
        self.event_plan = Some(OriginalTaskInputEventPlan {
            task_id: task.id.clone(),
            task_kind: task.kind.clone(),
            target: target.into(),
            child: child.map(str::to_owned),
        });
        Ok(())
    }
    pub(in crate::runtime) fn original_event(
        &self,
        task: &TaskRecord,
        bytes: u64,
        target: &str,
        child: Option<&str>,
    ) -> Result<AuditEvent> {
        let plan = self
            .event_plan
            .as_ref()
            .context("TaskInput event material is not sealed")?;
        anyhow::ensure!(
            plan.task_id == task.id
                && plan.task_kind == task.kind
                && plan.target == target
                && plan.child.as_deref() == child,
            "TaskInput event differs from its original admitted delivery"
        );
        let data = match child {
            None => {
                serde_json::json!({"task_id":task.id,"task_kind":task.kind,"bytes_written":bytes,"input_target":target})
            }
            Some(child) => {
                serde_json::json!({"task_id":task.id,"task_kind":task.kind,"child_agent_id":child,"bytes_written":bytes,"input_target":target})
            }
        };
        Ok(AuditEvent::legacy("task_input_delivered", data))
    }
    fn result(self, result: TaskInputResult) -> OwnedTaskInputResult {
        OwnedTaskInputResult {
            result,
            material: self.budget.guard(),
            usage: self.usage,
            ledger: self.ledger,
        }
    }
}
struct TaskInputSelected {
    tasks: CapturedTasks,
    _material: TaskCaptureMaterial,
    _query: Option<TaskQueryLease>,
}
impl TaskInputSelected {
    fn material_estimate(&self) -> Result<(u64, usize)> {
        let owned = self._material.material_estimate()?;
        let query = self
            ._query
            .as_ref()
            .map_or((0, 0), TaskQueryLease::material_estimate);
        Ok((
            owned
                .0
                .checked_add(query.0)
                .context("TaskInput query bytes overflow")?,
            owned
                .1
                .checked_add(query.1)
                .context("TaskInput query nodes overflow")?,
        ))
    }
    fn task(&self, id: &str) -> Result<&TaskRecord> {
        self.tasks
            .records
            .iter()
            .find_map(|row| match &row.fact {
                CanonicalTaskFact::Task(task) if task.id == id => Some(task),
                _ => None,
            })
            .ok_or_else(|| task_not_found_error(id).into())
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn start_original_task_input_owned(
        &self,
        id: &str,
        input: &str,
        authority: Option<AuthorityClass>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<OwnedTaskInputResult>>> {
        let admission = self.execution_admission_lease()?;
        let material = self.prepare_original_task_input_material(id, input, authority)?;
        let id = id.to_string();
        let input = input.to_string();
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = runtime
                .deliver_original_task_input_owned(id, input, authority, material)
                .await;
            if let Err(error) = &result {
                tracing::warn!(error=%error,"original TaskInput continuation failed");
            }
            let _ = send.send(result);
        })?;
        Ok(receive)
    }
    fn prepare_original_task_input_material(
        &self,
        id: &str,
        input: &str,
        authority: Option<AuthorityClass>,
    ) -> Result<OriginalTaskInputMaterial> {
        let ledger = self.inner.input_evidence_budget.clone();
        let configured = ledger.limits();
        let limits = (
            configured
                .history_per_agent_bytes
                .checked_add(configured.pending_per_agent_bytes)
                .context("TaskInput cap overflow")?
                .min(64 * 1024 * 1024),
            usize::try_from(configured.record_limit)?.min(65536),
        );
        let usage = copy_cost(
            &(
                id,
                input,
                authority,
                self.instance_agent_id(),
                self.instance_incarnation(),
                self.instance_owner_epoch(),
            ),
            8,
        )?;
        anyhow::ensure!(
            usage.0 <= limits.0 && usage.1 <= limits.1,
            TaskReadError::ResourceExhausted("TaskInput first request copy")
        );
        let budget = ledger.reserve_host_owned_completion(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: usage.0,
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok(OriginalTaskInputMaterial {
            scope: InputEvidenceScope {
                agent_id: self.instance_agent_id().into(),
                identity_incarnation: self.instance_incarnation(),
            },
            epoch: self.instance_owner_epoch(),
            ledger,
            budget,
            usage,
            selected: (0, 0),
            limits,
            event_plan: None,
        })
    }
    #[cfg(test)]
    pub(crate) async fn original_task_input_child_binding_for_test(
        &self,
        id: &str,
        input: &str,
        authority: AuthorityClass,
    ) -> Result<Arc<OriginalTaskInputChildBinding>> {
        let _admission = self.execution_admission_lease()?;
        let mut material = self.prepare_original_task_input_material(id, input, Some(authority))?;
        let selected = self
            .with_task_publication(|| {
                self.capture_original_task_input_selected(id, &material, None)
            })
            .await?;
        material.selected = selected.material_estimate()?;
        let task = selected.task(id)?;
        material.charge(task, 4)?;
        let task = task.clone();
        drop(selected);
        material.selected = (0, 0);
        let child = task
            .detail
            .as_ref()
            .and_then(|d| d.get("child_agent_id"))
            .and_then(serde_json::Value::as_str)
            .context("actual child Task has no child address")?;
        self.capture_original_task_input_child_binding(&task, &mut material, child)
            .await?
            .context("actual child producer has no original target binding")
    }
    #[cfg(test)]
    pub(crate) async fn quiesce_original_task_input_resources_for_test(&self) -> Result<()> {
        self.quiesce_context_resource_observer_task_for_test().await
    }
    #[cfg(test)]
    pub(crate) async fn reserve_original_task_input_owner_for_test(
        &self,
    ) -> Result<crate::runtime::mutation_owner::MutationReservation> {
        Ok(self.inner.mutation_owner.reserve().await?)
    }
    #[cfg(test)]
    pub(crate) fn close_original_task_input_admission_for_test(&self) {
        self.close_mutation_admission();
    }
    fn capture_original_task_input_selected(
        &self,
        id: &str,
        m: &OriginalTaskInputMaterial,
        query: Option<TaskQueryLease>,
    ) -> Result<TaskInputSelected> {
        m.validate_owner(self)?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("TaskInput Task Root unpublished")?;
        let domain = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            domain.scope() == &m.scope,
            "TaskInput selection scope differs"
        );
        let query_prior = query
            .as_ref()
            .map_or((0, 0), TaskQueryLease::material_estimate);
        m.check(m.usage, query_prior)?;
        let prior = (
            m.usage
                .0
                .checked_add(query_prior.0)
                .context("TaskInput selected prior overflow")?,
            m.usage
                .1
                .checked_add(query_prior.1)
                .context("TaskInput selected prior nodes overflow")?,
        );
        let request = TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            required_keys: BTreeSet::from([TaskKey::new(TaskFamily::Task, id)]),
        };
        let (tasks, material) = domain.capture_with_material(
            &request,
            &m.ledger,
            (usize::try_from(m.limits.0)?, m.limits.1),
            prior,
            None,
        )?;
        let selected = TaskInputSelected {
            tasks,
            _material: material,
            _query: query,
        };
        m.check(m.usage, selected.material_estimate()?)?;
        Ok(selected)
    }
    pub(in crate::runtime) async fn capture_original_task_input_child_binding(
        &self,
        task: &TaskRecord,
        material: &mut OriginalTaskInputMaterial,
        child_id: &str,
    ) -> Result<Option<Arc<OriginalTaskInputChildBinding>>> {
        let lease = {
            let handles = self.inner.task_handles.lock().await;
            let Some(handle) = handles.get(&task.id) else {
                return Ok(None);
            };
            let Some(lease) = handle.control_lease() else {
                return Ok(None);
            };
            if lease.is_command_producer() || !lease.matches_task_generation(task) {
                return Ok(None);
            }
            lease
        };
        let Some(source) = lease.original_child()? else {
            return Ok(None);
        };
        if source.is_durable_finished() {
            return Ok(None);
        }
        source.validate_material_ledger(&self.inner.input_evidence_budget)?;
        let view = match source.source_view() {
            Ok(view) => view,
            Err(_) if source.is_durable_finished() => return Ok(None),
            Err(error) => return Err(error),
        };
        view.validate_generation(task)?;
        let Some(target) = view
            .target()
            .filter(|target| target.has_created_child_duty())
        else {
            return Ok(None);
        };
        let Some(scope) = target.target_scope() else {
            return Ok(None);
        };
        if target.target_id() != child_id || scope.agent_id != child_id {
            return Ok(None);
        }
        // This is the first clone of the finite target address, not of its body.
        material.charge(&(&view.address().scope, &task.id, scope), 8)?;
        material.reserve_original_event(task, "child_followup", Some(child_id))?;
        OriginalTaskInputChildBinding::from_original_target(
            self,
            task,
            &view,
            material.budget.guard(),
            material.usage,
        )
    }
    async fn deliver_original_task_input_owned(
        &self,
        id: String,
        input: String,
        authority: Option<AuthorityClass>,
        mut material: OriginalTaskInputMaterial,
    ) -> Result<OwnedTaskInputResult> {
        let selected = match self
            .with_task_publication(|| {
                self.capture_original_task_input_selected(&id, &material, None)
            })
            .await
        {
            Ok(selected) => selected,
            Err(error) if matches!(error.downcast_ref::<TaskReadError>(),Some(TaskReadError::NeedsPreparation(key)) if key.family==TaskFamily::Task&&key.id==id) =>
            {
                material.charge(&(TaskFamily::Task, &id), 4)?;
                let keys = [TaskKey::new(TaskFamily::Task, &id)];
                let remaining = material.remaining()?;
                let query = self.inner.task_queries.register(
                    &material.scope,
                    material.epoch,
                    TaskQuerySpec {
                        required_keys: &keys,
                        ..Default::default()
                    },
                    &material.ledger,
                    remaining.1,
                    remaining.0,
                )?;
                material.selected = query.material_estimate();
                material.charge(query.selection(), 4)?;
                let remaining = material.remaining()?;
                let load = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                    query: query.selection().clone(),
                    capture: TaskCaptureRequest {
                        required_keys: query.selection().required_keys.clone(),
                        ..TaskCaptureRequest::current()
                    },
                    byte_limit: remaining.0,
                    record_limit: remaining.1,
                };
                let mut owner = self.begin_mutation().await?;
                self.inner
                    .task_queries
                    .validate_lease(&query, &material.scope, material.epoch)?;
                owner.prepare_owner_task_domain_request(&load).await?;
                drop((owner, load, keys));
                material.selected = (0, 0);
                self.with_task_publication(|| {
                    self.capture_original_task_input_selected(&id, &material, Some(query))
                })
                .await?
            }
            Err(error) => return Err(error),
        };
        material.selected = selected.material_estimate()?;
        let task = selected.task(&id)?;
        // Bound original DTO/command copies and serde's child Value temporary.
        material.charge(task, 4)?;
        let snapshot = TaskStatusSnapshot::from_task_record(task);
        let command = snapshot.command.clone();
        let task = task.clone();
        drop(selected);
        material.selected = (0, 0);
        let (result, event) = if matches!(
            task.status,
            TaskStatus::Cancelling
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            (
                TaskInputResult {
                    task: snapshot,
                    accepted_input: false,
                    input_target: command.and_then(|value| value.input_target),
                    bytes_written: None,
                    summary_text: Some("task is not currently accepting input".into()),
                },
                None,
            )
        } else if task.kind == TaskKind::CommandTask {
            self.deliver_command_task_input(&task, snapshot, command, &input, &mut material)
                .await?
        } else if let Some(authority) = authority.filter(|_| task.is_child_agent_task()) {
            self.deliver_child_task_input(&task, snapshot, &input, &authority, &mut material)
                .await?
        } else {
            (
                TaskInputResult {
                    task: snapshot,
                    accepted_input: false,
                    input_target: None,
                    bytes_written: None,
                    summary_text: Some("task does not support input delivery".into()),
                },
                None,
            )
        };
        if let Some(event) = event {
            self.finish_owned_original_task_input_audit(event, &material)
                .await?;
        }
        Ok(material.result(result))
    }
}

#[cfg(test)]
#[path = "task_input_owned_tests.rs"]
mod tests;
