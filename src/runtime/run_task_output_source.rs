//! Task output sources borrowed from the original held RunOnce publication.
use super::*;
use crate::runtime::{
    mutation_owner::PostureObservation, run_birth_sessions::RunBirthReportProjection,
};

/// No root/owner/database survives selection. The original report supplies the
/// Task and output-head guards; exact old Inputs retain their own selected pins.
pub(in crate::runtime) struct RunTaskOutputSelection {
    read: TaskOutputRead,
}
impl RunTaskOutputSelection {
    pub(in crate::runtime) fn material_estimate(&self) -> (u64, usize) {
        (self.read.material.bytes, self.read.material.records)
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn capture_run_task_output_from_snapshot(
        &self,
        root: &Arc<PostureObservation>,
        report: Arc<RunBirthReportProjection>,
        id: &str,
        limits: (usize, usize),
        caller_prior: (u64, usize),
    ) -> Result<RunTaskOutputSelection> {
        report.source.validate_material_binding(self, root)?;
        let domain = root
            .input_evidence
            .as_ref()
            .context("Run output Input unpublished")?;
        let ledger = &self.inner.input_evidence_budget;
        domain.validate_scheduler_body_loan(&report.source.inputs.input, ledger)?;
        let held = report.material_estimate()?;
        anyhow::ensure!(
            caller_prior.0 >= held.0 && caller_prior.1 >= held.1,
            "Run output prior omits its held report"
        );
        anyhow::ensure!(
            report.born_tasks().any(|task| task.id == id),
            "Run output target is not an original born Task"
        );
        let tasks = &report.source.tasks;
        let fact_index = tasks
            .records
            .iter()
            .position(|row| row.locator.key.family == TaskFamily::Task && row.locator.key.id == id)
            .context("Run output original Task missing")?;
        let CanonicalTaskFact::Task(task) = &tasks.records[fact_index].fact else {
            anyhow::bail!("Run output family differs")
        };
        let source = tasks.output_source(id)?;
        let reply =
            if task.kind == TaskKind::AgentMessageWait && task.status == TaskStatus::Completed {
                task.detail
                    .as_ref()
                    .and_then(|value| value.get("message_id"))
                    .and_then(Value::as_str)
            } else {
                None
            };
        // This exact selector guard precedes every key/String/set/Arc copy.
        let (encoded, native) =
            borrowed_json_material(&(id, &tasks.scope, source.map(|head| &head.message), reply))?;
        let nodes = native
            .checked_add(24)
            .and_then(|n| n.checked_mul(8))
            .context("Run output selector native overflow")?;
        let bytes = encoded
            .checked_add(512)
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
            .context("Run output selector bytes overflow")?;
        let mut material = OutputMaterialAllowance::new(ledger.limits())?;
        material.byte_limit = material
            .byte_limit
            .min(u64::try_from(limits.0.min(64 * 1024 * 1024))?);
        material.record_limit = material.record_limit.min(limits.1.min(65536));
        material = material
            .additional(caller_prior.0, caller_prior.1)?
            .additional(bytes, nodes)?;
        let selectors = self.task_output_owned_budget(&tasks.scope, bytes, nodes)?;
        let mut roots = BTreeSet::new();
        roots.extend(source.map(|head| head.message.key.clone()));
        roots.extend(reply.map(|id| InputEvidenceKey::new(InputEvidenceFamily::Message, id)));
        let inputs = if roots.is_empty() {
            None
        } else {
            let request = InputEvidenceLoadRequest {
                windows: vec![],
                mandatory_roots: roots,
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                record_limit: material.record_limit,
                byte_limit: usize::try_from(material.byte_limit)?,
            };
            let (inputs, captured_material) = domain
                .capture_task_output_for_request_with_body_loan(
                    &tasks.records[fact_index].locator,
                    &request,
                    limits,
                    (material.bytes, material.records),
                    ledger,
                    &report.source.inputs.input,
                )?;
            let actual = captured_material.additional_material_estimate()?;
            material = material.additional(actual.0, actual.1)?;
            Some(inputs)
        };
        Ok(RunTaskOutputSelection {
            read: TaskOutputRead {
                tasks: TaskOutputTasks::Run(report),
                fact_index,
                inputs,
                _selectors: selectors,
                material,
            },
        })
    }
    pub(in crate::runtime) async fn render_run_task_output(
        &self,
        mut selection: RunTaskOutputSelection,
        caller_prior: (u64, usize),
    ) -> Result<TaskOutputResultRead> {
        let TaskOutputTasks::Run(report) = &selection.read.tasks else {
            anyhow::bail!("Run rendering requires its original selected report")
        };
        report.source.validate_render_accounting(self)?;
        let old = selection.material_estimate();
        anyhow::ensure!(
            caller_prior.0 >= old.0 && caller_prior.1 >= old.1,
            "Run output rendering omitted selected sources"
        );
        // Other selected Tasks and earlier rendered results remain live in the
        // outer report. Use their cumulative allowance before the next clone.
        selection.read.material.bytes = caller_prior.0;
        selection.read.material.records = caller_prior.1;
        selection.read.material = selection.read.material.additional(0, 0)?;
        let ready = task_output_ready(selection.read.task(), &selection.read.status()?);
        let rendered = self.task_output_snapshot_from_read(selection.read).await?;
        Ok(TaskOutputResultRead {
            result: TaskOutputResult {
                retrieval_status: if ready {
                    TaskOutputRetrievalStatus::Success
                } else {
                    TaskOutputRetrievalStatus::NotReady
                },
                task: rendered.snapshot,
            },
            _role: None,
            _selected: rendered.selected,
            _render_budget: rendered.render_budget,
            _file_budget: rendered.file_budget,
            material: rendered.material,
        })
    }
}

#[cfg(test)]
#[path = "run_task_output_source_tests.rs"]
mod tests;
