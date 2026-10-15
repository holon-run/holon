//! Original born members stay selected through the whole RunOnce projection.
use super::{remaining_material, CapturedRunBirth};
use crate::{
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate},
        input_queries::InputQueryError,
        resident_input_evidence::{PreparedInputEvidence, QualifiedInputEvidence},
        resident_task::{CanonicalTaskFact, TaskReadError},
    },
    types::{AuditEvent, TaskRecord},
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

#[path = "run_birth_poll.rs"]
mod poll;
pub(crate) use poll::{RunPollActivity, RunPollFacts};

pub(crate) struct RunBirthReportProjection {
    pub(crate) source: CapturedRunBirth,
    input_indices: Vec<usize>,
    task_indices: Vec<usize>,
    material: (u64, usize),
    _indices: Arc<BudgetViewGuard>,
}
impl CapturedRunBirth {
    pub(crate) fn project_report(self, limits: (usize, usize)) -> Result<RunBirthReportProjection> {
        let ledger = self.inputs.actual_material_ledger(&self.token)?;
        ensure!(
            self.tasks.scope == self.inputs.input.scope
                && self.events.selected.scope == self.inputs.input.scope,
            "original report domains crossed scope"
        );
        let baseline = self.baseline;
        let query = &self.inputs.query;
        let cursor = query.birth_cursor.ok_or(InputQueryError::Incomplete)?;
        ensure!(
            baseline.input_cursor <= cursor
                && (baseline.input_cursor == cursor
                    || query
                        .birth_after
                        .is_some_and(|after| after <= baseline.input_cursor)),
            InputQueryError::FrontierNotPrepared
        );
        // This original Task capture validates each mandatory born body. It
        // also contains older closure dependencies, which are not run output.
        let task_count = self
            .tasks
            .born_task_ids_after(baseline.task_cursor)?
            .count();
        let born_input = |evidence: &QualifiedInputEvidence| {
            query
                .born_key_seqs
                .get(&evidence.locator.key)
                .is_some_and(|seq| *seq > baseline.input_cursor)
        };
        let mut input_count = 0usize;
        for evidence in &self.inputs.input.records {
            if born_input(evidence) {
                if matches!(&evidence.record, PreparedInputEvidence::ToolExecution(_)) {
                    evidence.tool_run_report()?;
                }
                input_count = input_count
                    .checked_add(1)
                    .context(InputQueryError::ResourceExhausted("Run input indices"))?;
            }
        }
        for (key, seq) in &query.born_key_seqs {
            if *seq > baseline.input_cursor {
                ensure!(!self.inputs.input.unavailable.contains(key),
                    crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(key.clone()));
            }
        }
        ensure!(
            self.events.selected.birth_cursor >= baseline.event_cursor,
            crate::runtime::current_read_facts::CurrentReadError::NeedsPreparation
        );
        let prior = self.material_estimate()?;
        let slots = input_count
            .checked_add(task_count)
            .context(InputQueryError::ResourceExhausted("Run report index count"))?;
        let own = (
            u64::try_from(slots)?
                .checked_mul(128)
                .and_then(|n| n.checked_add(4096))
                .context(InputQueryError::ResourceExhausted("Run report index bytes"))?,
            slots
                .checked_mul(4)
                .and_then(|n| n.checked_add(32))
                .context(InputQueryError::ResourceExhausted("Run report index nodes"))?,
        );
        let remaining = remaining_material(ledger, limits, prior)?;
        ensure!(
            own.0 <= u64::try_from(remaining.0)? && own.1 <= remaining.1,
            InputQueryError::ResourceExhausted("Run report source and indices")
        );
        let guard = ledger.register_owned_bytes(
            &self.inputs.input.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: own.0,
            },
        )?;
        let mut input_indices = Vec::with_capacity(input_count);
        for (index, evidence) in self.inputs.input.records.iter().enumerate() {
            if born_input(evidence) {
                input_indices.push(index);
            }
        }
        input_indices.sort_unstable_by(|left, right| {
            let a = &self.inputs.input.records[*left];
            let b = &self.inputs.input.records[*right];
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.locator.key.id.cmp(&b.locator.key.id))
        });
        let mut task_indices = Vec::with_capacity(task_count);
        for (index, fact) in self.tasks.records.iter().enumerate() {
            if let CanonicalTaskFact::Task(task) = &fact.fact {
                if self
                    .tasks
                    .coverage
                    .query
                    .born_task_seqs
                    .get(&task.id)
                    .is_some_and(|seq| *seq > baseline.task_cursor)
                {
                    task_indices.push(index);
                }
            }
        }
        ensure!(
            task_indices.len() == task_count,
            TaskReadError::IncompleteQuery
        );
        task_indices.sort_unstable_by(|left, right| {
            match (
                &self.tasks.records[*left].fact,
                &self.tasks.records[*right].fact,
            ) {
                (CanonicalTaskFact::Task(a), CanonicalTaskFact::Task(b)) => a
                    .created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id)),
                _ => unreachable!("original born Task indices"),
            }
        });
        Ok(RunBirthReportProjection {
            source: self,
            input_indices,
            task_indices,
            material: own,
            _indices: guard,
        })
    }
}
impl RunBirthReportProjection {
    pub(crate) fn born_events(&self) -> impl DoubleEndedIterator<Item = &AuditEvent> {
        self.source
            .events
            .selected
            .events
            .iter()
            .filter_map(|record| match &record.fact {
                crate::runtime::current_read_facts::CurrentReadFact::Event(event) => Some(event),
                _ => None,
            })
    }
    pub(crate) fn born_inputs(&self) -> impl DoubleEndedIterator<Item = &QualifiedInputEvidence> {
        self.input_indices
            .iter()
            .map(|index| self.source.inputs.input.records[*index].as_ref())
    }
    pub(crate) fn born_tasks(&self) -> impl DoubleEndedIterator<Item = &TaskRecord> {
        self.task_indices
            .iter()
            .filter_map(|index| match &self.source.tasks.records[*index].fact {
                CanonicalTaskFact::Task(task) => Some(task),
                _ => None,
            })
    }
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        super::add_material(self.source.material_estimate()?, self.material)
    }
}

#[cfg(test)]
#[path = "run_birth_report_projection_tests.rs"]
mod tests;
