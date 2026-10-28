//! Stop responsibility is independent of active Task and queue membership.
use super::*;
use crate::runtime_db::{
    input_mutation_receipts::canonical_command_digest, task_stop::TaskStopRecord,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskStopLocator {
    pub scope: InputEvidenceScope,
    pub effect_seq: u64,
    pub revision: u64,
    pub intent_digest: String,
    pub generation_digest: String,
    pub record_digest: String,
}
impl TaskStopLocator {
    pub(crate) fn from_record(record: &TaskStopRecord) -> Result<Self> {
        record.validate_resident()?;
        Ok(Self {
            scope: record.intent.task.scope.clone(),
            effect_seq: record.effect_seq,
            revision: record.revision,
            intent_digest: record.intent_digest.clone(),
            generation_digest: record.generation_digest.clone(),
            record_digest: canonical_command_digest(record)?,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskStopMetadata {
    pub locator: TaskStopLocator,
    pub pending: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskStopHeadObservation {
    pub task: TaskFactLocator,
    pub generation_digest: String,
    /// None is an observed empty exact-generation namespace, not a cache miss.
    pub head: Option<TaskStopLocator>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskStopCoverage {
    pub pending_complete: bool,
    pub pending_effect_seqs: BTreeSet<u64>,
    pub heads: BTreeMap<String, TaskStopHeadObservation>,
    pub records: BTreeMap<u64, TaskStopMetadata>,
}
impl TaskStopCoverage {
    pub(crate) fn empty_complete() -> Self {
        Self {
            pending_complete: true,
            ..Self::default()
        }
    }
    pub(super) fn selected_sequences(
        &self,
        selected: &BTreeSet<TaskKey>,
        pending: bool,
    ) -> Result<BTreeSet<u64>> {
        if pending && !self.pending_complete {
            return Err(TaskReadError::IncompleteStops.into());
        }
        let mut seqs = if pending {
            self.pending_effect_seqs.clone()
        } else {
            BTreeSet::new()
        };
        for key in selected.iter().filter(|key| key.family == TaskFamily::Task) {
            // Absent/Pruned Task roots have no generation and no head observation.
            if let Some(head) = self.heads.get(&key.id) {
                if let Some(locator) = &head.head {
                    seqs.insert(locator.effect_seq);
                }
            }
        }
        Ok(seqs)
    }
    pub(super) fn validate(
        &self,
        scope: &InputEvidenceScope,
        tasks: &BTreeMap<TaskKey, Arc<QualifiedTaskFact>>,
        records: &BTreeMap<u64, Arc<TaskStopRecord>>,
    ) -> Result<()> {
        ensure!(
            self.records.len() == records.len(),
            "Stop metadata/body selection differs"
        );
        for (seq, body) in records {
            let meta = self
                .records
                .get(seq)
                .context("Stop body has no observation")?;
            ensure!(
                body.intent.task.scope == *scope
                    && *seq == body.effect_seq
                    && meta.locator == TaskStopLocator::from_record(body)?
                    && meta.pending == body.progress.is_pending(),
                "Stop metadata/body differs"
            );
            ensure!(
                !self.pending_complete || self.pending_effect_seqs.contains(seq) == meta.pending,
                "Stop pending completeness differs from body"
            );
        }
        for seq in &self.pending_effect_seqs {
            ensure!(
                self.records.get(seq).is_some_and(|m| m.pending),
                "Stop pending body missing"
            );
        }
        for (id, head) in &self.heads {
            let key = TaskKey::new(TaskFamily::Task, id);
            let task = tasks
                .get(&key)
                .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()))?;
            let CanonicalTaskFact::Task(body) = &task.fact else {
                anyhow::bail!("Stop head has another Task family")
            };
            ensure!(
                head.task == task.locator
                    && head.generation_digest == task_stop_generation_digest(&task.locator, body)?,
                "Stop head Task generation differs"
            );
            if let Some(locator) = &head.head {
                ensure!(
                    locator.scope == *scope
                        && locator.generation_digest == head.generation_digest
                        && self
                            .records
                            .get(&locator.effect_seq)
                            .is_some_and(|m| &m.locator == locator),
                    "Stop head locator is unavailable"
                );
            }
        }
        if self.pending_complete {
            for key in tasks.keys().filter(|k| k.family == TaskFamily::Task) {
                ensure!(
                    self.heads.contains_key(&key.id),
                    "observed Task lacks exact Stop head"
                );
            }
        }
        Ok(())
    }
}
pub(crate) fn task_stop_generation_digest(
    locator: &TaskFactLocator,
    task: &TaskRecord,
) -> Result<String> {
    canonical_command_digest(&(
        &locator.scope,
        &locator.key,
        task.kind,
        task.created_at,
        task.rejoin_fence().ok(),
    ))
}
impl CapturedTasks {
    pub(crate) fn pending_stops(&self) -> Result<impl Iterator<Item = &Arc<TaskStopRecord>>> {
        if !self.coverage.stop.pending_complete {
            return Err(TaskReadError::IncompleteStops.into());
        }
        for seq in &self.coverage.stop.pending_effect_seqs {
            ensure!(
                self.stop_records.iter().any(|r| r.effect_seq == *seq),
                "captured pending Stop body missing"
            );
        }
        Ok(self.stop_records.iter().filter(|r| {
            self.coverage
                .stop
                .pending_effect_seqs
                .contains(&r.effect_seq)
        }))
    }
    pub(crate) fn stop_head(&self, task_id: &str) -> Result<Option<&Arc<TaskStopRecord>>> {
        let task = self
            .lookup(&TaskKey::new(TaskFamily::Task, task_id))?
            .context("Stop head Task absent")?;
        let head = self
            .coverage
            .stop
            .heads
            .get(task_id)
            .ok_or(TaskReadError::IncompleteStops)?;
        ensure!(head.task == task.locator, "captured Stop head Task changed");
        match &head.head {
            None => Ok(None),
            Some(locator) => self
                .stop_records
                .iter()
                .find(|r| r.effect_seq == locator.effect_seq)
                .map(Some)
                .ok_or_else(|| TaskReadError::IncompleteStops.into()),
        }
    }
}
