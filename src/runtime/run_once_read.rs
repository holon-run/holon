//! All RunOnce polling and final output sources share one owner publication.
use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{
            borrowed_json_material, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
        },
        resident_input_evidence::PreparedInputEvidence,
        resident_task::CanonicalTaskFact,
        resident_work::WorkCaptureRequest,
        state_mutation::{PreparedSchedulerRead, SchedulerReadBodyLoans},
        tasks::{RunTaskOutputLifetime, RunTaskOutputSelection, TaskOutputResultRead},
    },
    types::{AuditEvent, ClosureDecision, TaskStatus, ToolExecutionStatus},
};
use std::collections::BTreeSet;

pub(crate) struct RunPollRead {
    pub(crate) report: Arc<RunBirthReportProjection>,
    pub(crate) poll: RunPollFacts,
    pub(crate) closure: Option<ClosureDecision>,
    pub(crate) active_task_ids: Vec<String>,
    pub(crate) terminal_text: Option<Arc<RetainedRunTerminalText>>,
    scheduler: PreparedSchedulerRead,
    extra_tasks: Option<(
        crate::runtime::resident_task::CapturedTasks,
        crate::runtime::resident_task::TaskCaptureMaterial,
    )>,
    material: (u64, usize),
    _selectors: Arc<BudgetViewGuard>,
    _closure: Arc<BudgetViewGuard>,
}
pub(crate) struct RunFinalRead {
    pub(crate) poll: RunPollRead,
    outputs: Vec<RunTaskOutputSelection>,
    material: (u64, usize),
    _containers: Arc<BudgetViewGuard>,
}
impl RunPollRead {
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.material
    }
    pub(crate) fn task_status(&self, id: &str) -> Result<TaskStatus> {
        for tasks in [
            Some(&self.report.source.tasks),
            Some(&self.scheduler.tasks),
            self.extra_tasks.as_ref().map(|(tasks, _)| tasks),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(task) = selected_task(tasks, id) {
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
                return Ok(tasks.output_source(id)?.map_or_else(
                    || task.status.clone(),
                    |source| source.message_status.clone(),
                ));
            }
        }
        Err(crate::runtime_error::RuntimeError::not_found(
            "task_not_found",
            format!("task {id} not found"),
        )
        .with_safe_context("task_id", id)
        .into())
    }
}
fn selected_task<'a>(
    tasks: &'a crate::runtime::resident_task::CapturedTasks,
    id: &str,
) -> Option<&'a crate::types::TaskRecord> {
    tasks.records.iter().find_map(|row| match &row.fact {
        CanonicalTaskFact::Task(task) if task.id == id => Some(task),
        _ => None,
    })
}

fn visit_task_ids(
    report: &RunBirthReportProjection,
    mut visit: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    for task in report.born_tasks() {
        visit(&task.id)?;
    }
    for row in report.born_inputs() {
        if let PreparedInputEvidence::ToolExecution(tool) = &row.record {
            if tool.tool_name == "ExecCommand" && tool.status == ToolExecutionStatus::Success {
                if let Some(id) = tool.command.task_id.as_deref() {
                    visit(id)?;
                }
            }
        }
    }
    for event in report.born_events() {
        if let Some(id) = event_task_id(event) {
            visit(id)?;
        }
    }
    Ok(())
}
fn event_task_id(event: &AuditEvent) -> Option<&str> {
    if event.kind == "tool_executed"
        && event
            .data
            .get("tool_name")
            .and_then(serde_json::Value::as_str)
            == Some("ExecCommand")
        && event
            .data
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|status| status == "success")
    {
        return event
            .data
            .get("task_handle")
            .and_then(|value| value.get("task_id"))
            .and_then(serde_json::Value::as_str);
    }
    if event.kind == "process_execution_requested"
        && event
            .data
            .get("surface")
            .and_then(serde_json::Value::as_str)
            == Some("command_task")
        && event
            .data
            .get("promoted_from_exec_command")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        return event
            .data
            .get("task_id")
            .and_then(serde_json::Value::as_str);
    }
    None
}
fn reserve(
    runtime: &RuntimeHandle,
    scope: &super::super::resident_input_evidence::InputEvidenceScope,
    limits: (usize, usize),
    prior: (u64, usize),
    own: (u64, usize),
) -> Result<Arc<BudgetViewGuard>> {
    let remaining = remaining_material(&runtime.inner.input_evidence_budget, limits, prior)?;
    ensure!(
        own.0 <= u64::try_from(remaining.0)? && own.1 <= remaining.1,
        InputQueryError::ResourceExhausted("Run derived material")
    );
    Ok(runtime.inner.input_evidence_budget.register_owned_bytes(
        scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: own.0,
        },
    )?)
}
impl RuntimeHandle {
    pub(crate) async fn capture_run_poll_read(
        &self,
        sessions: &RunBirthSessions,
        retained: Option<Arc<RetainedRunTerminalText>>,
    ) -> Result<RunPollRead> {
        let held = self.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("Run poll Root unpublished")?;
        self.capture_run_poll_read_from_snapshot(
            sessions,
            &root,
            retained,
            (usize::MAX, usize::MAX),
        )
    }
    fn capture_run_poll_read_from_snapshot(
        &self,
        sessions: &RunBirthSessions,
        root: &Arc<PostureObservation>,
        retained: Option<Arc<RetainedRunTerminalText>>,
        limits: (usize, usize),
    ) -> Result<RunPollRead> {
        let mut source = self.capture_run_birth_sessions_from_snapshot_with_terminal_text(
            sessions,
            root,
            limits,
            retained.clone(),
        )?;
        let terminal_text = source.retain_terminal_text().or(retained);
        let report = Arc::new(source.project_report(limits)?);
        let scope = &report.source.inputs.input.scope;
        let mut own = (8192u64, 64usize);
        visit_task_ids(&report, |id| {
            let (bytes, nodes) = borrowed_json_material(&id)?;
            own = add_material(
                own,
                (
                    bytes
                        .checked_add(256)
                        .and_then(|n| n.checked_mul(8))
                        .context(InputQueryError::ResourceExhausted("Run Task selectors"))?,
                    nodes
                        .checked_add(2)
                        .and_then(|n| n.checked_mul(8))
                        .context(InputQueryError::ResourceExhausted("Run Task selectors"))?,
                ),
            )?;
            Ok(())
        })?;
        let mut material = add_material(report.material_estimate()?, own)?;
        let selectors = reserve(self, scope, limits, report.material_estimate()?, own)?;
        let mut ids = BTreeSet::new();
        visit_task_ids(&report, |id| {
            ids.insert(id.to_owned());
            Ok(())
        })?;
        // Candidate status needs Task facts, not additional Work/Wait predicates.
        // Preserve the original closure request; select missing Tasks separately.
        let request = WorkCaptureRequest::current();
        let loans = SchedulerReadBodyLoans::from_run_birth(self, root, &report.source)?;
        let scheduler = self.capture_scheduler_read_from_snapshot_with_material(
            root,
            &request,
            self.now(),
            limits,
            material,
            Some(&loans),
        )?;
        material = add_material(material, scheduler.additional_material_estimate()?)?;
        let missing = ids
            .iter()
            .filter(|id| {
                selected_task(&report.source.tasks, id).is_none()
                    && selected_task(&scheduler.tasks, id).is_none()
            })
            .map(|id| {
                crate::runtime::resident_task::TaskKey::new(
                    crate::runtime::resident_task::TaskFamily::Task,
                    id,
                )
            })
            .collect::<BTreeSet<_>>();
        // The original selector guard admits these finite keys before cloning.
        // Any absent historical body remains an explicit preparation error.
        let extra_tasks = if missing.is_empty() {
            None
        } else {
            let request = crate::runtime::resident_task::TaskCaptureRequest {
                include_active: false,
                include_unsettled: false,
                terminal_limit: 0,
                required_keys: missing,
            };
            let domain = root
                .tasks
                .as_ref()
                .ok_or(crate::runtime::resident_task::TaskReadError::IncompleteActive)?;
            let (tasks, guards) = domain.capture_with_material(
                &request,
                &self.inner.input_evidence_budget,
                limits,
                material,
                Some(&report.source.tasks),
            )?;
            material = add_material(material, guards.additional_material_estimate()?)?;
            Some((tasks, guards))
        };
        // The original closure derives at most finite evidence strings plus its
        // selected Work ID. Admit every possible selected ID before policy clones.
        let mut closure_own = (8192u64, 128usize);
        for work in scheduler
            .projection
            .current_work_item
            .iter()
            .chain(&scheduler.projection.queued_runnable_work_items)
            .chain(&scheduler.projection.waiting_work_item)
        {
            let (bytes, nodes) = borrowed_json_material(&work.id)?;
            closure_own = add_material(
                closure_own,
                (
                    bytes
                        .checked_add(256)
                        .and_then(|n| n.checked_mul(16))
                        .context("Run closure ID bytes overflow")?,
                    nodes
                        .checked_add(2)
                        .and_then(|n| n.checked_mul(16))
                        .context("Run closure ID nodes overflow")?,
                ),
            )?;
        }
        let closure_guard = reserve(self, scope, limits, material, closure_own)?;
        material = add_material(material, closure_own)?;
        let closure = if scheduler.state.current_run_id.is_none()
            && scheduler.state.pending == 0
            && scheduler.state.pending_wake_hint.is_none()
        {
            Some(Self::closure_decision_from_projection(
                &scheduler.state,
                &scheduler.projection,
                None,
            ))
        } else {
            None
        };
        let poll = report.poll_facts()?;
        let mut read = RunPollRead {
            report,
            poll,
            closure,
            active_task_ids: Vec::new(),
            terminal_text,
            scheduler,
            extra_tasks,
            material,
            _selectors: selectors,
            _closure: closure_guard,
        };
        for id in ids {
            if crate::storage::is_active_task_status(&read.task_status(&id)?) {
                read.active_task_ids.push(id);
            }
        }
        Ok(read)
    }
    pub(crate) async fn capture_run_final_read(
        &self,
        sessions: &RunBirthSessions,
        retained: Option<Arc<RetainedRunTerminalText>>,
    ) -> Result<RunFinalRead> {
        let held = self.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("Run final Root unpublished")?;
        self.capture_run_final_read_from_snapshot(
            sessions,
            &root,
            retained,
            (usize::MAX, usize::MAX),
        )
    }
    fn capture_run_final_read_from_snapshot(
        &self,
        sessions: &RunBirthSessions,
        root: &Arc<PostureObservation>,
        retained: Option<Arc<RetainedRunTerminalText>>,
        limits: (usize, usize),
    ) -> Result<RunFinalRead> {
        let read = self.capture_run_poll_read_from_snapshot(sessions, root, retained, limits)?;
        let count = read.report.born_tasks().count();
        let own = (
            u64::try_from(
                count
                    .checked_mul(
                        std::mem::size_of::<RunTaskOutputSelection>()
                            .checked_add(std::mem::size_of::<TaskOutputResultRead>())
                            .context("Run output slot overflow")?,
                    )
                    .and_then(|n| n.checked_add(4096))
                    .context("Run output Vec overflow")?,
            )?,
            count
                .checked_mul(8)
                .and_then(|n| n.checked_add(32))
                .context("Run output Vec nodes overflow")?,
        );
        let guard = reserve(
            self,
            &read.report.source.inputs.input.scope,
            limits,
            read.material,
            own,
        )?;
        let mut material = add_material(read.material, own)?;
        let mut outputs = Vec::with_capacity(count);
        for task in read.report.born_tasks() {
            let selected = self.capture_run_task_output_from_snapshot(
                root,
                read.report.clone(),
                &task.id,
                limits,
                material,
            )?;
            material = selected.material_estimate();
            outputs.push(selected);
        }
        Ok(RunFinalRead {
            poll: read,
            outputs,
            material,
            _containers: guard,
        })
    }
}

pub(crate) struct RunRenderedFinal {
    pub(crate) poll: RunPollRead,
    pub(crate) outputs: Vec<TaskOutputResultRead>,
    material: (u64, usize),
    _containers: Arc<BudgetViewGuard>,
}
impl RunRenderedFinal {
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.material
    }
}
impl RunFinalRead {
    pub(crate) async fn render(self, runtime: &RuntimeHandle) -> Result<RunRenderedFinal> {
        self.poll
            .report
            .source
            .validate_render_accounting(runtime)?;
        let Self {
            poll,
            outputs,
            mut material,
            _containers,
        } = self;
        // Both Vec allocations were admitted before selecting any Task. The
        // original Source and every prior output stay held across the next await.
        let mut rendered = Vec::with_capacity(outputs.len());
        for selected in outputs {
            let output = runtime.render_run_task_output(selected, material).await?;
            material = output.material_estimate();
            rendered.push(output);
        }
        Ok(RunRenderedFinal {
            poll,
            outputs: rendered,
            material,
            _containers,
        })
    }
}

/// Every Task DTO is moved from its original render carrier. The response
/// owner keeps this selected source beside those DTOs through final encoding.
pub(crate) struct RunFinalResponseSource {
    pub(crate) poll: RunPollRead,
    material: (u64, usize),
    _outputs: Vec<RunTaskOutputLifetime>,
    _containers: Arc<BudgetViewGuard>,
    _handoff: Arc<BudgetViewGuard>,
}
pub(crate) struct RunResponseTasks {
    pub(crate) tasks: Vec<crate::types::TaskOutputSnapshot>,
    pub(crate) source: RunFinalResponseSource,
}
impl RunFinalResponseSource {
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.material
    }
}
impl RunRenderedFinal {
    pub(crate) fn into_response_tasks(self, runtime: &RuntimeHandle) -> Result<RunResponseTasks> {
        self.poll
            .report
            .source
            .validate_render_accounting(runtime)?;
        let count = self.outputs.len();
        // The original Vec remains live while both new Vecs are filled. Admit
        // all new headers first; the Task bodies themselves are moved, not copied.
        let slots = count
            .checked_mul(
                std::mem::size_of::<crate::types::TaskOutputSnapshot>()
                    .checked_add(std::mem::size_of::<RunTaskOutputLifetime>())
                    .context("Run response Task slots overflow")?,
            )
            .and_then(|n| n.checked_add(4096))
            .context("Run response Task containers overflow")?;
        let own = (
            u64::try_from(slots)?,
            count
                .checked_mul(8)
                .and_then(|n| n.checked_add(32))
                .context("Run response Task native overflow")?,
        );
        let handoff = reserve(
            runtime,
            &self.poll.report.source.inputs.input.scope,
            (usize::MAX, usize::MAX),
            self.material,
            own,
        )?;
        let material = add_material(self.material, own)?;
        let Self {
            poll,
            outputs,
            _containers,
            ..
        } = self;
        let mut tasks = Vec::with_capacity(count);
        let mut lifetimes = Vec::with_capacity(count);
        for output in outputs {
            let (task, lifetime) = output.into_run_response_parts()?;
            tasks.push(task);
            lifetimes.push(lifetime);
        }
        Ok(RunResponseTasks {
            tasks,
            source: RunFinalResponseSource {
                poll,
                material,
                _outputs: lifetimes,
                _containers,
                _handoff: handoff,
            },
        })
    }
}

#[cfg(test)]
#[path = "run_once_read_tests.rs"]
mod tests;
