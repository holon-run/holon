//! Qualified Task/result facts. Completeness is explicit; capture owns selected Arcs.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
};
use crate::{
    domain::execution_protocol::RejoinFence,
    runtime_db::task_result_settlement::{TaskResultSettlementRecord, TaskResultSettlementState},
    types::{TaskRecord, TaskStatus},
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[path = "resident_task_stop.rs"]
mod stop;
use crate::runtime_db::task_stop::TaskStopRecord;
pub(crate) use stop::{
    task_stop_generation_digest, TaskStopCoverage, TaskStopHeadObservation, TaskStopLocator,
    TaskStopMetadata,
};

#[path = "resident_task_query.rs"]
mod query;
pub(crate) use query::{
    task_output_status_from_message, TaskOutputCoverage, TaskOutputHeadObservation,
    TaskOutputSourceAvailability, TaskOutputSourceHead, TaskOutputSourcePlace,
    TaskOutputWindowObservation,
};

#[path = "resident_task_status.rs"]
mod status;
pub(crate) use status::{
    TaskDelegationMetadata, TaskDelegationObservation, TaskStatusProjectionCoverage,
    TaskStatusProjectionObservation, TaskWorktreeObservation, TASK_WORKTREE_CLEANUP_FIELDS,
};

pub(crate) use super::task_queries::TaskQuerySelection;

#[path = "resident_task_relations.rs"]
mod relations;
pub(crate) use relations::{TaskParentTurnHeader, TaskQueryCoverage};

#[path = "resident_task_running.rs"]
mod running;
pub(crate) use running::{
    fixed_running_matches_task, task_running_generation_digest, RunningSourceGeneration,
    TaskRunningSourceCoverage,
};

pub(crate) const TASK_TERMINAL_WINDOW: usize = 128;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskFamily {
    Task,
    ResultSettlement,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct TaskKey {
    pub family: TaskFamily,
    pub id: String,
}
impl TaskKey {
    pub(crate) fn new(family: TaskFamily, id: impl Into<String>) -> Self {
        Self {
            family,
            id: id.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", content = "record", rename_all = "snake_case")]
pub(crate) enum CanonicalTaskFact {
    Task(TaskRecord),
    ResultSettlement(TaskResultSettlementRecord),
}
impl CanonicalTaskFact {
    pub(crate) fn key(&self) -> TaskKey {
        match self {
            Self::Task(x) => TaskKey::new(TaskFamily::Task, &x.id),
            Self::ResultSettlement(x) => {
                TaskKey::new(TaskFamily::ResultSettlement, &x.result_identity)
            }
        }
    }
    pub(crate) fn agent_id(&self) -> &str {
        match self {
            Self::Task(x) => &x.agent_id,
            Self::ResultSettlement(x) => &x.agent_id,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskFactLocator {
    pub scope: InputEvidenceScope,
    pub key: TaskKey,
    pub revision: u64,
    pub content_digest: String,
    pub projection_digest: String,
}
/// Same-view relation, not permission to read or mutate an unqualified Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskResultDependencyObservation {
    pub result: TaskFactLocator,
    pub relation: TaskResultDependencyRelation,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "relation", rename_all = "snake_case")]
pub(crate) enum TaskResultDependencyRelation {
    QualifiedCurrent {
        task: TaskFactLocator,
    },
    Invalid {
        target: TaskDependencyTarget,
        reason: TaskDependencyInvalidReason,
        predicate_digest: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub(crate) enum TaskDependencyTarget {
    GlobalAbsent {
        task_id: String,
    },
    Pruned {
        task_id: String,
    },
    FixedUnavailable {
        task_id: String,
    },
    CanonicalObserved {
        task_id: String,
        owner: String,
        normalized_revision: i64,
    },
}
impl TaskDependencyTarget {
    fn task_id(&self) -> &str {
        match self {
            Self::GlobalAbsent { task_id }
            | Self::Pruned { task_id }
            | Self::FixedUnavailable { task_id }
            | Self::CanonicalObserved { task_id, .. } => task_id,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskDependencyInvalidReason {
    Missing,
    Pruned,
    FixedUnavailable,
    ForeignOwner,
    PredicateMismatch,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QualifiedTaskFact {
    pub locator: TaskFactLocator,
    pub fact: CanonicalTaskFact,
    pub resident_bytes: usize,
}
impl QualifiedTaskFact {
    pub(crate) fn validate(&self, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.locator.scope == *scope
                && self.fact.agent_id() == scope.agent_id
                && self.locator.key == self.fact.key(),
            "Task fact scope/key mismatch"
        );
        ensure!(
            self.locator.revision > 0
                && valid_digest(&self.locator.content_digest)
                && valid_digest(&self.locator.projection_digest),
            "Task fact locator invalid"
        );
        ensure!(
            self.resident_bytes >= serde_json::to_vec(&self.fact)?.len(),
            "Task fact body estimate incomplete"
        );
        Ok(())
    }
}
fn valid_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskAvailability {
    Present,
    Absent,
    Pruned,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskMetadata {
    pub task_id: String,
    pub kind: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub work_item_id: Option<String>,
    pub parent_message_id: Option<String>,
    pub rejoin: Option<RejoinFence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskResultMetadata {
    pub result_identity: String,
    pub task_id: String,
    pub message_id: String,
    pub work_item_id: Option<String>,
    pub rejoin_generation: u64,
    pub parent_turn_id: String,
    pub state: TaskResultSettlementState,
    pub activation_id: Option<String>,
    pub activation_open: bool,
    pub reply_message_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub next_recheck_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskHistoryCoverage {
    pub limit: usize,
    /// Canonical updated_at DESC, created_at DESC, task_id ASC.
    pub keys: Vec<TaskKey>,
    pub is_exhaustive: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskCoverage {
    pub revision: u64,
    #[serde(default)]
    pub stop: TaskStopCoverage,
    #[serde(default)]
    pub output: TaskOutputCoverage,
    #[serde(default)]
    pub status: TaskStatusProjectionCoverage,
    #[serde(default)]
    pub query: TaskQueryCoverage,
    #[serde(default)]
    pub running: TaskRunningSourceCoverage,
    pub active_complete: bool,
    pub unsettled_complete: bool,
    pub active_task_ids: BTreeSet<String>,
    pub unsettled_result_ids: BTreeSet<String>,
    pub task_metadata: BTreeMap<String, TaskMetadata>,
    pub result_metadata: BTreeMap<String, TaskResultMetadata>,
    pub result_dependencies: BTreeMap<String, TaskResultDependencyObservation>,
    /// Complete result-ID index for each observed Task, including an explicit empty set.
    pub task_result_ids: BTreeMap<String, BTreeSet<String>>,
    /// All qualified CallerAdmitted rows, including currently Open activations.
    pub activation_result_ids: BTreeMap<String, BTreeSet<String>>,
    pub reply_result_ids: BTreeMap<String, BTreeSet<String>>,
    pub required_keys: BTreeSet<TaskKey>,
    #[serde(with = "observations")]
    pub observations: BTreeMap<TaskKey, TaskAvailability>,
    pub terminal: TaskHistoryCoverage,
}
impl TaskCoverage {
    pub(crate) fn owned_metadata_record_count(&self) -> Result<usize> {
        coverage_record_count(self)
    }
    fn clone_without_stops(&self) -> Self {
        Self {
            revision: self.revision,
            stop: TaskStopCoverage::default(),
            output: self.output.clone(),
            status: self.status.clone(),
            query: self.query.clone(),
            running: self.running.clone(),
            active_complete: self.active_complete,
            unsettled_complete: self.unsettled_complete,
            active_task_ids: self.active_task_ids.clone(),
            unsettled_result_ids: self.unsettled_result_ids.clone(),
            task_metadata: self.task_metadata.clone(),
            result_metadata: self.result_metadata.clone(),
            result_dependencies: self.result_dependencies.clone(),
            task_result_ids: self.task_result_ids.clone(),
            activation_result_ids: self.activation_result_ids.clone(),
            reply_result_ids: self.reply_result_ids.clone(),
            required_keys: self.required_keys.clone(),
            observations: self.observations.clone(),
            terminal: self.terminal.clone(),
        }
    }
    fn references(&self, record: &QualifiedTaskFact) -> Result<BTreeSet<TaskKey>> {
        let result = match &record.fact {
            CanonicalTaskFact::Task(task) => {
                let ids = self
                    .task_result_ids
                    .get(&task.id)
                    .ok_or_else(|| TaskReadError::NeedsPreparation(record.locator.key.clone()))?;
                return Ok(ids
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
                    .collect());
            }
            CanonicalTaskFact::ResultSettlement(result) => result,
        };
        let observed = self
            .result_dependencies
            .get(&result.result_identity)
            .ok_or_else(|| TaskReadError::NeedsPreparation(record.locator.key.clone()))?;
        ensure!(
            observed.result == record.locator,
            "Result relation locator mismatch"
        );
        match &observed.relation {
            TaskResultDependencyRelation::QualifiedCurrent { task } => {
                ensure!(
                    task.scope == record.locator.scope
                        && task.key == TaskKey::new(TaskFamily::Task, &result.task_id),
                    "Result relation Task scope/key mismatch"
                );
                Ok(BTreeSet::from([task.key.clone()]))
            }
            TaskResultDependencyRelation::Invalid {
                target,
                predicate_digest,
                ..
            } => {
                ensure!(
                    target.task_id() == result.task_id && valid_digest(predicate_digest),
                    "Invalid Result relation target/digest mismatch"
                );
                Ok(BTreeSet::new())
            }
        }
    }
    fn roots(&self, request: &TaskCaptureRequest) -> Result<BTreeSet<TaskKey>> {
        ensure!(
            request.terminal_limit <= TASK_TERMINAL_WINDOW,
            "Task terminal window exceeds fixed maximum"
        );
        if request.include_active && !self.active_complete {
            return Err(TaskReadError::IncompleteActive.into());
        }
        if request.include_unsettled && !self.unsettled_complete {
            return Err(TaskReadError::IncompleteResults.into());
        }
        if request.terminal_limit > self.terminal.limit && !self.terminal.is_exhaustive {
            return Err(TaskReadError::WindowExpansion.into());
        }
        let mut keys = request.required_keys.clone();
        if request.include_active {
            keys.extend(
                self.active_task_ids
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::Task, id)),
            );
        }
        if request.include_unsettled {
            keys.extend(
                self.unsettled_result_ids
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
            );
        }
        keys.extend(
            self.terminal
                .keys
                .iter()
                .take(request.terminal_limit)
                .cloned(),
        );
        Ok(keys)
    }
    fn resident_roots(&self) -> BTreeSet<TaskKey> {
        self.required_keys
            .iter()
            .cloned()
            .chain(
                self.active_task_ids
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::Task, id)),
            )
            .chain(
                self.unsettled_result_ids
                    .iter()
                    .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
            )
            .chain(self.terminal.keys.iter().cloned())
            .chain(
                self.query
                    .all_selected_task_ids()
                    .map(|id| TaskKey::new(TaskFamily::Task, id)),
            )
            .collect()
    }
}
#[derive(Debug, Clone)]
pub(crate) struct TaskCaptureRequest {
    pub include_active: bool,
    pub include_unsettled: bool,
    pub terminal_limit: usize,
    pub required_keys: BTreeSet<TaskKey>,
}
impl TaskCaptureRequest {
    pub(crate) fn current() -> Self {
        Self {
            include_active: true,
            include_unsettled: true,
            terminal_limit: TASK_TERMINAL_WINDOW,
            required_keys: BTreeSet::new(),
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) struct CommittedTaskDelta {
    pub scope: InputEvidenceScope,
    pub base_revision: u64,
    pub revision: u64,
    pub records: Vec<Arc<QualifiedTaskFact>>,
    pub stop_records: Vec<Arc<TaskStopRecord>>,
    pub removals: BTreeMap<TaskKey, TaskAvailability>,
    pub coverage: TaskCoverage,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskReadError {
    #[error("Task domain needs preparation: {0:?}")]
    NeedsPreparation(TaskKey),
    #[error("Task active metadata is not complete")]
    IncompleteActive,
    #[error("Task unsettled result metadata is not complete")]
    IncompleteResults,
    #[error("Task Stop responsibility/head metadata is not complete")]
    IncompleteStops,
    #[error("Task query relation metadata is not prepared")]
    IncompleteQuery,
    #[error("Task query member is unavailable: {0:?}")]
    UnavailableQuery(TaskKey),
    #[error("Task output head/window metadata is not prepared")]
    IncompleteOutput,
    #[error("Task status projection metadata is not prepared")]
    IncompleteStatusProjection,
    #[error("Task output source is unavailable: {0:?}")]
    UnavailableOutputSource(TaskOutputSourceAvailability),
    #[error("Task terminal history window requires preparation")]
    WindowExpansion,
    #[error("Task domain revision mismatch")]
    RevisionMismatch,
    #[error("Task domain ResourceExhausted: {0}")]
    ResourceExhausted(&'static str),
}
#[derive(Debug)]
pub(crate) struct ResidentTaskDomain {
    scope: InputEvidenceScope,
    coverage: TaskCoverage,
    records: BTreeMap<TaskKey, Arc<QualifiedTaskFact>>,
    stop_records: BTreeMap<u64, Arc<TaskStopRecord>>,
    record_limit: usize,
    byte_limit: usize,
    budget: Option<InputEvidenceBudgetLedger>,
    budget_views: Vec<Arc<BudgetViewGuard>>,
}
/// No root/domain/map reference. Metadata is an independently observed allocation.
#[derive(Debug)]
pub(crate) struct CapturedTasks {
    pub scope: InputEvidenceScope,
    pub revision: u64,
    pub coverage: TaskCoverage,
    pub records: Vec<Arc<QualifiedTaskFact>>,
    pub stop_records: Vec<Arc<TaskStopRecord>>,
    _budget_views: Vec<Arc<BudgetViewGuard>>,
}
impl ResidentTaskDomain {
    #[cfg(test)]
    pub(crate) fn bootstrap(
        scope: InputEvidenceScope,
        coverage: TaskCoverage,
        records: Vec<Arc<QualifiedTaskFact>>,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Self> {
        Self::bootstrap_with_stops(
            scope,
            coverage,
            records,
            Vec::new(),
            record_limit,
            byte_limit,
        )
    }
    pub(crate) fn bootstrap_with_stops(
        scope: InputEvidenceScope,
        coverage: TaskCoverage,
        records: Vec<Arc<QualifiedTaskFact>>,
        stop_records: Vec<Arc<TaskStopRecord>>,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Self> {
        ensure!(
            !scope.agent_id.is_empty()
                && scope.identity_incarnation > 0
                && coverage.revision > 0
                && record_limit > 0
                && byte_limit > 0,
            "invalid Task domain bootstrap"
        );
        let mut domain = Self {
            scope,
            coverage,
            records: BTreeMap::new(),
            stop_records: BTreeMap::new(),
            record_limit,
            byte_limit,
            budget: None,
            budget_views: Vec::new(),
        };
        for record in records {
            record.validate(&domain.scope)?;
            ensure!(
                domain
                    .records
                    .insert(record.locator.key.clone(), record)
                    .is_none(),
                "duplicate Task body"
            );
        }
        for record in stop_records {
            ensure!(
                domain
                    .stop_records
                    .insert(record.effect_seq, record)
                    .is_none(),
                "duplicate Stop body"
            );
        }
        domain.trim_and_validate()?;
        Ok(domain)
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(crate) fn coverage(&self) -> &TaskCoverage {
        &self.coverage
    }
    /// Final publication needs every prepared body, independent of prompt selection.
    pub(crate) fn selected_install_records(&self) -> Vec<Arc<QualifiedTaskFact>> {
        self.records.values().cloned().collect()
    }
    pub(crate) fn selected_install_stop_records(&self) -> Vec<Arc<TaskStopRecord>> {
        self.stop_records.values().cloned().collect()
    }
    pub(crate) fn candidate_stop_budget_view(&self) -> Result<Option<Arc<BudgetViewGuard>>> {
        self.budget
            .as_ref()
            .map(|budget| {
                budget.register_task_stop_view(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    self.stop_records.values(),
                )
            })
            .transpose()
            .map_err(Into::into)
    }
    pub(crate) fn candidate_budget_view(&self) -> Result<Option<Arc<BudgetViewGuard>>> {
        self.budget
            .as_ref()
            .map(|budget| {
                budget.register_task_view(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    self.records.values(),
                )
            })
            .transpose()
            .map_err(Into::into)
    }
    pub(crate) fn with_budget(mut self, budget: InputEvidenceBudgetLedger) -> Result<Self> {
        self.budget = Some(budget);
        self.refresh_views()?;
        self.budget.as_ref().unwrap().check_history_budget()?;
        Ok(self)
    }
    pub(crate) fn with_committed(&self, delta: &CommittedTaskDelta) -> Result<Self> {
        ensure!(
            delta.scope == self.scope
                && delta.coverage.revision == delta.revision
                && delta.revision >= delta.base_revision,
            "Task delta scope/revision mismatch"
        );
        if delta.base_revision != self.coverage.revision {
            return Err(TaskReadError::RevisionMismatch.into());
        }
        if delta.revision == delta.base_revision {
            ensure!(
                delta.records.is_empty()
                    && delta.stop_records.is_empty()
                    && delta.removals.is_empty()
                    && delta.coverage == self.coverage,
                "Task no-op delta changed facts"
            );
        }
        let _candidate = self.observe_candidate(delta)?;
        let mut next = Self {
            scope: self.scope.clone(),
            coverage: delta.coverage.clone(),
            records: self.records.clone(),
            stop_records: self.stop_records.clone(),
            record_limit: self.record_limit,
            byte_limit: self.byte_limit,
            budget: self.budget.clone(),
            budget_views: Vec::new(),
        };
        for (key, availability) in &delta.removals {
            ensure!(
                *availability != TaskAvailability::Present
                    && delta.coverage.observations.get(key) == Some(availability),
                "Task removal lacks exact observation"
            );
            next.records.remove(key);
        }
        for record in &delta.records {
            record.validate(&next.scope)?;
            ensure!(
                delta.coverage.observations.get(&record.locator.key)
                    == Some(&TaskAvailability::Present),
                "Task addition lacks Present proof"
            );
            next.records
                .insert(record.locator.key.clone(), record.clone());
        }
        for record in &delta.stop_records {
            let locator = TaskStopLocator::from_record(record)?;
            ensure!(
                delta
                    .coverage
                    .stop
                    .records
                    .get(&record.effect_seq)
                    .is_some_and(|m| m.locator == locator),
                "Stop addition lacks exact metadata"
            );
            next.stop_records.insert(record.effect_seq, record.clone());
        }
        next.trim_and_validate()?;
        next.refresh_views()?;
        if let Some(b) = &next.budget {
            b.check_history_budget()?;
        }
        Ok(next)
    }
    fn observe_candidate(&self, delta: &CommittedTaskDelta) -> Result<Vec<Arc<BudgetViewGuard>>> {
        let Some(b) = &self.budget else {
            return Ok(Vec::new());
        };
        Ok(vec![
            b.register_task_stop_view(
                &self.scope,
                InputMemoryCategory::Candidate,
                self.stop_records.values().chain(delta.stop_records.iter()),
            )?,
            b.register_task_view(
                &self.scope,
                InputMemoryCategory::Candidate,
                self.records.values().chain(delta.records.iter()),
            )?,
            b.register_owned_bytes(
                &self.scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: metadata_bytes(
                        &delta.coverage,
                        self.records.len() + delta.records.len(),
                    )?,
                },
            )?,
        ])
    }
    fn closure(&self, mut keys: BTreeSet<TaskKey>) -> Result<BTreeSet<TaskKey>> {
        let mut pending: Vec<_> = keys.iter().cloned().collect();
        while let Some(key) = pending.pop() {
            match self.coverage.observations.get(&key) {
                Some(TaskAvailability::Absent | TaskAvailability::Pruned) => continue,
                Some(TaskAvailability::Present) => {}
                None => return Err(TaskReadError::NeedsPreparation(key).into()),
            }
            let fact = self
                .records
                .get(&key)
                .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()))?;
            for related in self.coverage.references(fact)? {
                if keys.insert(related.clone()) {
                    pending.push(related);
                }
            }
            ensure!(
                keys.len() <= self.record_limit,
                TaskReadError::ResourceExhausted("closure records")
            );
        }
        Ok(keys)
    }
    fn trim_and_validate(&mut self) -> Result<()> {
        ensure!(
            self.coverage.terminal.limit <= TASK_TERMINAL_WINDOW
                && self.coverage.terminal.keys.len() <= self.coverage.terminal.limit,
            "Task terminal coverage invalid"
        );
        ensure!(
            coverage_record_count(&self.coverage)? <= self.record_limit,
            TaskReadError::ResourceExhausted("metadata records")
        );
        let roots = self.closure(self.coverage.resident_roots())?;
        self.records.retain(|key, _| roots.contains(key));
        self.stop_records
            .retain(|seq, _| self.coverage.stop.records.contains_key(seq));
        self.coverage
            .stop
            .validate(&self.scope, &self.records, &self.stop_records)?;
        self.coverage.output.validate(&self.scope, &self.records)?;
        self.coverage.status.validate(&self.scope, &self.records)?;
        validate_coverage(&self.coverage, &self.records)?;
        let stop_bytes = self.stop_records.values().try_fold(0usize, |n, r| {
            n.checked_add(usize::try_from(r.resident_bytes()?)?)
                .context("Stop body bytes overflow")
        })?;
        let body = self.records.values().try_fold(0usize, |n, r| {
            n.checked_add(r.resident_bytes)
                .context("Task body bytes overflow")
        })?;
        let metadata = usize::try_from(metadata_bytes(&self.coverage, self.records.len())?)?;
        ensure!(
            roots.len() + self.stop_records.len() <= self.record_limit
                && body
                    .checked_add(stop_bytes)
                    .context("Task Stop bytes overflow")?
                    .checked_add(metadata)
                    .context("Task bytes overflow")?
                    <= self.byte_limit,
            TaskReadError::ResourceExhausted("resident materials")
        );
        Ok(())
    }
    fn refresh_views(&mut self) -> Result<()> {
        let Some(b) = &self.budget else { return Ok(()) };
        let current = self.closure(
            self.coverage
                .required_keys
                .iter()
                .cloned()
                .chain(
                    self.coverage
                        .active_task_ids
                        .iter()
                        .map(|id| TaskKey::new(TaskFamily::Task, id)),
                )
                .chain(
                    self.coverage
                        .unsettled_result_ids
                        .iter()
                        .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
                )
                .chain(
                    self.coverage
                        .query
                        .all_selected_task_ids()
                        .map(|id| TaskKey::new(TaskFamily::Task, id)),
                )
                .collect(),
        )?;
        let current_stops = self
            .coverage
            .stop
            .selected_sequences(&current, self.coverage.stop.pending_complete)?;
        self.budget_views = vec![
            b.register_task_stop_view(
                &self.scope,
                InputMemoryCategory::Current,
                self.stop_records
                    .iter()
                    .filter(|(seq, _)| current_stops.contains(seq))
                    .map(|(_, r)| r),
            )?,
            b.register_task_stop_view(
                &self.scope,
                InputMemoryCategory::History,
                self.stop_records
                    .iter()
                    .filter(|(seq, _)| !current_stops.contains(seq))
                    .map(|(_, r)| r),
            )?,
            b.register_task_view(
                &self.scope,
                InputMemoryCategory::Current,
                self.records
                    .iter()
                    .filter(|(k, _)| current.contains(*k))
                    .map(|(_, r)| r),
            )?,
            b.register_task_view(
                &self.scope,
                InputMemoryCategory::History,
                self.records
                    .iter()
                    .filter(|(k, _)| !current.contains(*k))
                    .map(|(_, r)| r),
            )?,
            b.register_owned_bytes(
                &self.scope,
                InputMemoryCategory::Current,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: metadata_bytes(&self.coverage, self.records.len())?,
                },
            )?,
        ];
        Ok(())
    }
    /// An original source actor only borrows already selected Status facts.
    /// This creates no capture metadata, lookup fallback or body grant.
    pub(crate) fn original_status_fact(
        &self,
        locator: &TaskFactLocator,
    ) -> Result<Option<&Arc<QualifiedTaskFact>>> {
        anyhow::ensure!(
            locator.scope == self.scope && locator.key.family == TaskFamily::Task,
            "original Status observation scope differs"
        );
        if !self.coverage.status.heads.contains_key(&locator.key.id) {
            return Ok(None);
        }
        anyhow::ensure!(
            self.coverage.observations.get(&locator.key) == Some(&TaskAvailability::Present),
            "original Status head lacks its present Task observation"
        );
        let record = self
            .records
            .get(&locator.key)
            .context("original Status selected Task body missing")?;
        anyhow::ensure!(
            record.locator == *locator,
            "original Status selected Task locator differs"
        );
        Ok(Some(record))
    }

    pub(crate) fn capture(&self, request: &TaskCaptureRequest) -> Result<CapturedTasks> {
        self.capture_with_stops(request, false)
    }
    pub(crate) fn capture_pending_stops(&self) -> Result<CapturedTasks> {
        self.capture_with_stops(
            &TaskCaptureRequest {
                include_active: false,
                include_unsettled: false,
                terminal_limit: 0,
                required_keys: BTreeSet::new(),
            },
            true,
        )
    }
    fn capture_with_stops(
        &self,
        request: &TaskCaptureRequest,
        pending: bool,
    ) -> Result<CapturedTasks> {
        // Observe the independently owned coverage before allocating its maps.
        let metadata_guard = self
            .budget
            .as_ref()
            .map(|b| {
                b.register_owned_bytes(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: metadata_bytes(
                            &self.coverage,
                            self.records.len(),
                        )?,
                    },
                )
                .map_err(anyhow::Error::from)
            })
            .transpose()?;
        let selected = self.closure(self.coverage.roots(request)?)?;
        let records: Vec<_> = selected
            .iter()
            .filter_map(|key| self.records.get(key).cloned())
            .collect();
        let selected_stops = self.coverage.stop.selected_sequences(&selected, pending)?;
        let stop_records: Vec<_> = selected_stops
            .iter()
            .map(|seq| {
                self.stop_records
                    .get(seq)
                    .cloned()
                    .ok_or_else(|| anyhow::Error::from(TaskReadError::IncompleteStops))
            })
            .collect::<Result<_>>()?;
        let mut coverage = self.coverage.clone_without_stops();
        coverage.stop = TaskStopCoverage {
            pending_complete: pending && self.coverage.stop.pending_complete,
            pending_effect_seqs: self
                .coverage
                .stop
                .pending_effect_seqs
                .iter()
                .filter(|seq| selected_stops.contains(seq))
                .copied()
                .collect(),
            heads: self
                .coverage
                .stop
                .heads
                .iter()
                .filter(|(id, _)| selected.contains(&TaskKey::new(TaskFamily::Task, *id)))
                .map(|(id, head)| (id.clone(), head.clone()))
                .collect(),
            records: self
                .coverage
                .stop
                .records
                .iter()
                .filter(|(seq, _)| selected_stops.contains(seq))
                .map(|(seq, meta)| (*seq, meta.clone()))
                .collect(),
        };
        coverage
            .output
            .heads
            .retain(|id, _| selected.contains(&TaskKey::new(TaskFamily::Task, id)));
        coverage
            .status
            .heads
            .retain(|id, _| selected.contains(&TaskKey::new(TaskFamily::Task, id)));
        coverage.required_keys = request.required_keys.clone();
        coverage
            .observations
            .retain(|key, _| selected.contains(key));
        coverage
            .result_dependencies
            .retain(|id, _| selected.contains(&TaskKey::new(TaskFamily::ResultSettlement, id)));
        coverage.terminal.keys.truncate(request.terminal_limit);
        // A narrower capture never turns a truncated namespace into exhaustive.
        coverage.terminal.is_exhaustive &=
            self.coverage.terminal.keys.len() <= request.terminal_limit;
        coverage.terminal.limit = request.terminal_limit;
        let budget_views = match &self.budget {
            None => Vec::new(),
            Some(b) => vec![
                b.register_task_view(&self.scope, InputMemoryCategory::Pins, records.iter())?,
                b.register_task_stop_view(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    stop_records.iter(),
                )?,
                metadata_guard.expect("budget metadata guard observed"),
            ],
        };
        Ok(CapturedTasks {
            scope: self.scope.clone(),
            revision: self.coverage.revision,
            coverage,
            records,
            stop_records,
            _budget_views: budget_views,
        })
    }
}
impl CapturedTasks {
    /// The selected physical bodies and independently copied coverage stay
    /// alive together with any caller-owned projection.
    pub(crate) fn selected_material_estimate(&self) -> Result<(u64, usize)> {
        let mut bytes = metadata_bytes(&self.coverage, self.records.len())?;
        for record in &self.records {
            bytes = bytes
                .checked_add(u64::try_from(record.resident_bytes)?)
                .context("selected Task body material overflow")?;
        }
        for record in &self.stop_records {
            bytes = bytes
                .checked_add(record.resident_bytes()?)
                .context("selected TaskStop material overflow")?;
        }
        let count = self
            .coverage
            .owned_metadata_record_count()?
            .checked_add(self.records.len())
            .and_then(|count| count.checked_add(self.stop_records.len()))
            .context("selected Task material count overflow")?;
        Ok((bytes, count))
    }

    pub(crate) fn output_source(&self, task_id: &str) -> Result<Option<&TaskOutputSourceHead>> {
        let key = TaskKey::new(TaskFamily::Task, task_id);
        let record = self
            .lookup(&key)?
            .ok_or_else(|| TaskReadError::NeedsPreparation(key))?;
        ensure!(
            matches!(record.fact, CanonicalTaskFact::Task(_)),
            "Task output has another family"
        );
        self.coverage.output.head(&record.locator)
    }
    pub(crate) fn result_dependency(
        &self,
        result_id: &str,
    ) -> Result<&TaskResultDependencyObservation> {
        let key = TaskKey::new(TaskFamily::ResultSettlement, result_id);
        let record = self
            .lookup(&key)?
            .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()))?;
        self.coverage.references(record)?;
        self.coverage
            .result_dependencies
            .get(result_id)
            .ok_or_else(|| TaskReadError::NeedsPreparation(key).into())
    }
    pub(crate) fn lookup(&self, key: &TaskKey) -> Result<Option<&Arc<QualifiedTaskFact>>> {
        match self.coverage.observations.get(key) {
            Some(TaskAvailability::Absent | TaskAvailability::Pruned) => Ok(None),
            Some(TaskAvailability::Present) => self
                .records
                .iter()
                .find(|r| r.locator.key == *key)
                .map(Some)
                .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()).into()),
            None => Err(TaskReadError::NeedsPreparation(key.clone()).into()),
        }
    }
    #[cfg(test)]
    pub(crate) fn unsettled_for_owner(
        &self,
        work_id: Option<&str>,
        include_open_admitted: bool,
    ) -> Result<Vec<&TaskResultMetadata>> {
        if !self.coverage.unsettled_complete {
            return Err(TaskReadError::IncompleteResults.into());
        }
        let mut rows: Vec<_> = self
            .coverage
            .result_metadata
            .values()
            .filter(|r| {
                self.coverage
                    .unsettled_result_ids
                    .contains(&r.result_identity)
                    && r.work_item_id.as_deref() == work_id
                    && (include_open_admitted || !r.activation_open)
            })
            .collect();
        rows.sort_by(|a, b| {
            a.created_at
                .timestamp_millis()
                .cmp(&b.created_at.timestamp_millis())
                .then_with(|| a.result_identity.cmp(&b.result_identity))
        });
        Ok(rows)
    }
    pub(crate) fn next_recheck_at(&self) -> Result<Option<DateTime<Utc>>> {
        if !self.coverage.unsettled_complete {
            return Err(TaskReadError::IncompleteResults.into());
        }
        Ok(self
            .coverage
            .result_metadata
            .values()
            .filter(|r| {
                self.coverage
                    .unsettled_result_ids
                    .contains(&r.result_identity)
                    && !r.activation_open
            })
            .filter_map(|r| r.next_recheck_at)
            .min_by_key(|at| at.timestamp_millis())
            .map(|at| {
                DateTime::from_timestamp_millis(at.timestamp_millis())
                    .context("Task recheck SQL timestamp is out of range")
            })
            .transpose()?)
    }
}
impl TaskMetadata {
    pub(crate) fn from_record(task: &TaskRecord) -> Self {
        Self {
            task_id: task.id.clone(),
            kind: task.kind.as_str().into(),
            status: task.status.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
            work_item_id: task.work_item_id.clone(),
            parent_message_id: task.parent_message_id.clone(),
            rejoin: task.rejoin_fence().ok(),
        }
    }
}
impl TaskResultMetadata {
    pub(crate) fn matches_record(&self, x: &TaskResultSettlementRecord) -> bool {
        self.result_identity == x.result_identity
            && self.task_id == x.task_id
            && self.message_id == x.message_id
            && self.work_item_id == x.work_item_id
            && self.rejoin_generation == x.rejoin_generation
            && self.parent_turn_id == x.parent_turn_id
            && self.state == x.state
            && self.activation_id == x.activation_id
            && self.created_at == x.created_at
            && self.next_recheck_at == x.next_recheck_at
    }
}
fn coverage_record_count(c: &TaskCoverage) -> Result<usize> {
    [
        c.running
            .sources
            .len()
            .checked_mul(8)
            .context("Running association node count overflow")?,
        c.status.owned_metadata_record_count()?,
        c.query.owned_metadata_record_count()?,
        c.output.heads.len(),
        c.output
            .window
            .as_ref()
            .map_or(0, |window| window.message_ids.len()),
        c.task_metadata.len(),
        c.result_metadata.len(),
        c.result_dependencies.len(),
        c.observations.len(),
        c.task_result_ids.values().map(BTreeSet::len).sum(),
        c.activation_result_ids.values().map(BTreeSet::len).sum(),
        c.reply_result_ids.values().map(BTreeSet::len).sum(),
        c.stop.records.len(),
        c.stop.heads.len(),
        c.stop.pending_effect_seqs.len(),
    ]
    .into_iter()
    .try_fold(0usize, |n, x| {
        n.checked_add(x).context("Task metadata count overflow")
    })
}
fn metadata_bytes(coverage: &TaskCoverage, count: usize) -> Result<u64> {
    serialized_bytes(coverage)?
        .checked_add(u64::try_from(
            count
                .checked_add(coverage.stop.records.len())
                .context("Stop map count overflow")?
                .checked_mul(std::mem::size_of::<(TaskKey, Arc<QualifiedTaskFact>)>())
                .context("Task map estimate overflow")?,
        )?)
        .context("Task metadata estimate overflow")
}
fn validate_coverage(
    coverage: &TaskCoverage,
    records: &BTreeMap<TaskKey, Arc<QualifiedTaskFact>>,
) -> Result<()> {
    ensure!(coverage.revision > 0, "zero Task coverage revision");
    coverage.query.validate(coverage)?;
    ensure!(
        coverage.terminal.keys.iter().collect::<BTreeSet<_>>().len()
            == coverage.terminal.keys.len(),
        "duplicate terminal Task key"
    );
    for (key, record) in records {
        ensure!(
            record.locator.revision <= coverage.revision
                && coverage.observations.get(key) == Some(&TaskAvailability::Present),
            "Task body lacks current Present observation"
        );
        match &record.fact {
            CanonicalTaskFact::Task(task) => ensure!(
                coverage.task_metadata.get(&task.id) == Some(&TaskMetadata::from_record(task)),
                "Task metadata/body mismatch"
            ),
            CanonicalTaskFact::ResultSettlement(result) => {
                let meta = coverage
                    .result_metadata
                    .get(&result.result_identity)
                    .context("result metadata missing")?;
                ensure!(meta.matches_record(result), "result metadata/body mismatch");
                for key in coverage.references(record)? {
                    let dependency = records
                        .get(&key)
                        .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()))?;
                    let TaskResultDependencyRelation::QualifiedCurrent { task } =
                        &coverage.result_dependencies[&result.result_identity].relation
                    else {
                        unreachable!()
                    };
                    ensure!(
                        dependency.locator == *task,
                        "Result relation Task locator stale"
                    );
                }
            }
        }
    }
    for (id, meta) in &coverage.task_metadata {
        ensure!(
            id == &meta.task_id && coverage.task_result_ids.contains_key(id),
            "Task metadata/index key mismatch"
        );
        let active = matches!(
            meta.status,
            TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
        );
        ensure!(
            !coverage.active_complete || coverage.active_task_ids.contains(id) == active,
            "Task active metadata index mismatch"
        );
    }
    for (id, meta) in &coverage.result_metadata {
        ensure!(
            id == &meta.result_identity
                && coverage
                    .task_result_ids
                    .get(&meta.task_id)
                    .is_some_and(|ids| ids.contains(id)),
            "Task result reverse index mismatch"
        );
        ensure!(
            !coverage.unsettled_complete
                || coverage.unsettled_result_ids.contains(id)
                    == (meta.state != TaskResultSettlementState::Settled),
            "Task result completeness mismatch"
        );
        ensure!(
            !meta.activation_open
                || (meta.state == TaskResultSettlementState::CallerAdmitted
                    && meta.activation_id.is_some()),
            "Task result Open activation mismatch"
        );
        if let Some(activation) = &meta.activation_id {
            ensure!(
                meta.state != TaskResultSettlementState::CallerAdmitted
                    || coverage
                        .activation_result_ids
                        .get(activation)
                        .is_some_and(|ids| ids.contains(id)),
                "Task result activation index missing"
            );
        }
        if let Some(reply) = &meta.reply_message_id {
            ensure!(
                coverage
                    .reply_result_ids
                    .get(reply)
                    .is_some_and(|ids| ids.contains(id)),
                "Task result reply index missing"
            );
        }
    }
    for (task, ids) in &coverage.task_result_ids {
        for id in ids {
            ensure!(
                coverage
                    .result_metadata
                    .get(id)
                    .is_some_and(|r| &r.task_id == task),
                "Task result index has foreign member"
            );
        }
    }
    for (activation, ids) in &coverage.activation_result_ids {
        for id in ids {
            ensure!(
                coverage
                    .result_metadata
                    .get(id)
                    .is_some_and(|r| r.state == TaskResultSettlementState::CallerAdmitted
                        && r.activation_id.as_ref() == Some(activation)),
                "activation index has foreign result"
            );
        }
    }
    for (reply, ids) in &coverage.reply_result_ids {
        for id in ids {
            ensure!(
                coverage
                    .result_metadata
                    .get(id)
                    .is_some_and(|r| r.reply_message_id.as_ref() == Some(reply)),
                "reply index has foreign result"
            );
        }
    }
    for id in &coverage.active_task_ids {
        let key = TaskKey::new(TaskFamily::Task, id);
        let fact = records
            .get(&key)
            .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()))?;
        let CanonicalTaskFact::Task(task) = &fact.fact else {
            anyhow::bail!("active Task has another family")
        };
        ensure!(
            matches!(
                task.status,
                TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
            ),
            "active Task index points to terminal body"
        );
    }
    for id in &coverage.unsettled_result_ids {
        let key = TaskKey::new(TaskFamily::ResultSettlement, id);
        let fact = records
            .get(&key)
            .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()))?;
        let CanonicalTaskFact::ResultSettlement(result) = &fact.fact else {
            anyhow::bail!("result index has another family")
        };
        ensure!(
            result.state != TaskResultSettlementState::Settled,
            "unsettled index points to Settled"
        );
    }
    Ok(())
}
mod observations {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        map: &BTreeMap<TaskKey, TaskAvailability>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<BTreeMap<TaskKey, TaskAvailability>, D::Error> {
        let pairs = Vec::<(TaskKey, TaskAvailability)>::deserialize(deserializer)?;
        let mut map = BTreeMap::new();
        for (key, value) in pairs {
            if map.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("duplicate Task observation"));
            }
        }
        Ok(map)
    }
}
