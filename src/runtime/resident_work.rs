//! Qualified Work/wait/continuation records, separate from scheduling authority.
//! Loading/qualification belongs to same-transaction storage adapters.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use super::resident_input_evidence::InputEvidenceScope;
#[cfg(test)]
use super::resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey};
use crate::types::{
    AgentState, ExternalTriggerRecord, WaitConditionRecord, WaitConditionStatus,
    WorkItemContinuationFrame, WorkItemContinuationState, WorkItemRecord, WorkItemState,
};

pub(crate) const WORK_NONOPEN_WINDOW: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkFamily {
    Work,
    Wait,
    Frame,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct WorkKey {
    pub family: WorkFamily,
    pub id: String,
}
impl WorkKey {
    pub(crate) fn new(family: WorkFamily, id: impl Into<String>) -> Self {
        Self {
            family,
            id: id.into(),
        }
    }
}

// BTreeMap's borrowed ordering must exactly match WorkKey's derived ordering.
trait WorkKeyLookup {
    fn parts(&self) -> (WorkFamily, &str);
}
impl WorkKeyLookup for WorkKey {
    fn parts(&self) -> (WorkFamily, &str) {
        (self.family, &self.id)
    }
}
impl WorkKeyLookup for (WorkFamily, &str) {
    fn parts(&self) -> (WorkFamily, &str) {
        *self
    }
}
impl PartialEq for dyn WorkKeyLookup + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.parts() == other.parts()
    }
}
impl Eq for dyn WorkKeyLookup + '_ {}
impl PartialOrd for dyn WorkKeyLookup + '_ {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for dyn WorkKeyLookup + '_ {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.parts().cmp(&other.parts())
    }
}
impl<'a> std::borrow::Borrow<dyn WorkKeyLookup + 'a> for WorkKey {
    fn borrow(&self) -> &(dyn WorkKeyLookup + 'a) {
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "family", content = "record", rename_all = "snake_case")]
pub(crate) enum CanonicalWorkRecord {
    Work(WorkItemRecord),
    Wait(WaitConditionRecord),
    Frame(WorkItemContinuationFrame),
}
impl CanonicalWorkRecord {
    pub(crate) fn key(&self) -> WorkKey {
        match self {
            Self::Work(x) => WorkKey::new(WorkFamily::Work, &x.id),
            Self::Wait(x) => WorkKey::new(WorkFamily::Wait, &x.id),
            Self::Frame(x) => WorkKey::new(WorkFamily::Frame, &x.id),
        }
    }
    pub(crate) fn agent_id(&self) -> &str {
        match self {
            Self::Work(x) => &x.agent_id,
            Self::Wait(x) => &x.agent_id,
            Self::Frame(x) => &x.agent_id,
        }
    }
    /// Domain owning edges only. D3 evidence dependencies are prepared separately.
    pub(crate) fn references(&self) -> BTreeSet<WorkKey> {
        let mut keys = BTreeSet::new();
        match self {
            Self::Work(x) => {
                if let Some(id) = &x.blocked_by_wait_id {
                    keys.insert(WorkKey::new(WorkFamily::Wait, id));
                }
            }
            Self::Wait(x) => {
                if let Some(id) = &x.work_item_id {
                    keys.insert(WorkKey::new(WorkFamily::Work, id));
                }
            }
            Self::Frame(x) => {
                keys.insert(WorkKey::new(WorkFamily::Work, &x.suspended_work_item_id));
                keys.insert(WorkKey::new(WorkFamily::Work, &x.active_work_item_id));
            }
        }
        keys
    }
}

/// One Arc allocation owns a complete typed record. The digest is the exact
/// canonical bytes' digest established by the original storage transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QualifiedWorkRecord {
    pub scope: InputEvidenceScope,
    pub key: WorkKey,
    pub revision: u64,
    pub content_digest: String,
    pub record: CanonicalWorkRecord,
    pub resident_bytes: usize,
}
impl QualifiedWorkRecord {
    pub(crate) fn validate(&self, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.scope == *scope
                && self.record.agent_id() == scope.agent_id
                && self.record.key() == self.key,
            "qualified Work record scope/key mismatch"
        );
        ensure!(
            self.revision > 0
                && self.content_digest.len() == 64
                && self
                    .content_digest
                    .bytes()
                    .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x)),
            "qualified Work record revision/digest invalid"
        );
        ensure!(
            self.resident_bytes
                >= usize::try_from(super::input_evidence_budget::serialized_bytes(
                    &self.record
                )?)?,
            "qualified Work body estimate is incomplete"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkAvailability {
    Present,
    Absent,
    Pruned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkCompletionRef {
    pub brief_id: String,
    pub transcript_entry_id: Option<String>,
    pub source_message_id: Option<String>,
}
impl WorkCompletionRef {
    #[cfg(test)]
    pub(crate) fn input_roots(&self) -> BTreeSet<InputEvidenceKey> {
        let mut keys = BTreeSet::from([InputEvidenceKey::new(
            InputEvidenceFamily::Brief,
            &self.brief_id,
        )]);
        if let Some(id) = &self.transcript_entry_id {
            keys.insert(InputEvidenceKey::new(InputEvidenceFamily::Transcript, id));
        }
        if let Some(id) = &self.source_message_id {
            keys.insert(InputEvidenceKey::new(InputEvidenceFamily::Message, id));
        }
        keys
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkHistoryCoverage {
    pub limit: usize,
    /// Canonical updated_at DESC, created_at DESC, work_item_id ASC.
    pub keys: Vec<WorkKey>,
    pub is_exhaustive: bool,
}

/// Completeness is an explicit same-read-view observation, never inferred from
/// whichever bodies happen to be resident. Every focused absence is named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkCoverage {
    pub revision: u64,
    pub live_complete: bool,
    pub live_work_ids: BTreeSet<String>,
    pub unresolved_wait_ids: BTreeSet<String>,
    pub active_frame_ids: BTreeSet<String>,
    /// Complete same-view Wait IDs for each explicitly observed Turn; empty is proven.
    #[serde(default)]
    pub turn_wait_keys: BTreeMap<String, BTreeSet<String>>,
    /// Complete all-status Wait IDs with an exact TaskResult wake source.
    #[serde(default)]
    pub task_wait_keys: BTreeMap<String, BTreeSet<String>>,
    /// Complete all-status Wait IDs with this normalized trigger Message ID.
    #[serde(default)]
    pub trigger_wait_keys: BTreeMap<String, BTreeSet<String>>,
    pub mandatory_roots: BTreeSet<WorkKey>,
    #[serde(with = "work_observations")]
    pub observations: BTreeMap<WorkKey, WorkAvailability>,
    pub nonopen: WorkHistoryCoverage,
    /// Missing map entry is unprepared. Some(None) is observed no result.
    pub latest_results: BTreeMap<String, Option<WorkCompletionRef>>,
}
impl WorkCoverage {
    fn roots(&self) -> BTreeSet<WorkKey> {
        let mut keys = self.mandatory_roots.clone();
        keys.extend(
            self.live_work_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Work, id)),
        );
        keys.extend(
            self.unresolved_wait_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        keys.extend(
            self.active_frame_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Frame, id)),
        );
        keys.extend(
            self.turn_wait_keys
                .values()
                .chain(self.task_wait_keys.values())
                .chain(self.trigger_wait_keys.values())
                .flatten()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        keys.extend(self.nonopen.keys.iter().cloned());
        keys
    }
}

/// A whole original transaction's exact facts. Coverage is final, complete and
/// finite; only touched/newly required bodies need be returned as additions.
#[derive(Debug, Clone)]
pub(crate) struct CommittedWorkDelta {
    pub scope: InputEvidenceScope,
    pub base_revision: u64,
    pub revision: u64,
    pub records: Vec<Arc<QualifiedWorkRecord>>,
    pub removals: BTreeMap<WorkKey, WorkAvailability>,
    pub coverage: WorkCoverage,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkCaptureRequest {
    pub include_live: bool,
    pub nonopen_limit: usize,
    pub mandatory_roots: BTreeSet<WorkKey>,
    pub latest_result_work_ids: BTreeSet<String>,
    /// Exact by-Turn relations, including Resolved/Cancelled Waits.
    pub required_turn_ids: BTreeSet<String>,
    /// Named by accepted/current TaskResult inputs, never arbitrary history recall.
    pub required_task_ids: BTreeSet<String>,
    pub required_trigger_message_ids: BTreeSet<String>,
}
impl WorkCaptureRequest {
    pub(crate) fn current() -> Self {
        Self {
            include_live: true,
            nonopen_limit: WORK_NONOPEN_WINDOW,
            mandatory_roots: BTreeSet::new(),
            latest_result_work_ids: BTreeSet::new(),
            required_turn_ids: BTreeSet::new(),
            required_task_ids: BTreeSet::new(),
            required_trigger_message_ids: BTreeSet::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkReadError {
    #[error("Work domain requires preparation: {0:?}")]
    NeedsPreparation(WorkKey),
    #[error("Work Wait relation requires preparation for Turn {0}")]
    TurnWaitsNotPrepared(String),
    #[error("Work Wait relation requires preparation for Task {0}")]
    TaskWaitsNotPrepared(String),
    #[error("Work Wait relation requires preparation for trigger Message {0}")]
    TriggerWaitsNotPrepared(String),
    #[error("Work live coverage requires preparation")]
    IncompleteLive,
    #[error(
        "Work history window requires preparation: requested={requested}, prepared={prepared}"
    )]
    WindowExpansion { requested: usize, prepared: usize },
    #[error("Work domain revision mismatch: expected={expected}, actual={actual}")]
    RevisionMismatch { expected: u64, actual: u64 },
    #[error("Work domain resource exhausted: {0}")]
    ResourceExhausted(&'static str),
}

#[derive(Debug)]
pub(crate) struct ResidentWorkDomain {
    scope: InputEvidenceScope,
    coverage: WorkCoverage,
    records: BTreeMap<WorkKey, Arc<QualifiedWorkRecord>>,
    record_limit: usize,
    byte_limit: usize,
    budget: Option<InputEvidenceBudgetLedger>,
    budget_views: Vec<Arc<BudgetViewGuard>>,
}

/// Contains only selected record Arcs and copied selected coverage. No resident
/// map, root, runtime or storage handle is retained. Deliberately not Clone.
#[derive(Debug)]
pub(crate) struct CapturedWork {
    pub scope: InputEvidenceScope,
    pub revision: u64,
    pub live_complete: bool,
    pub live_work_ids: BTreeSet<String>,
    pub unresolved_wait_ids: BTreeSet<String>,
    pub active_frame_ids: BTreeSet<String>,
    pub turn_wait_keys: BTreeMap<String, BTreeSet<String>>,
    pub task_wait_keys: BTreeMap<String, BTreeSet<String>>,
    pub trigger_wait_keys: BTreeMap<String, BTreeSet<String>>,
    pub nonopen: WorkHistoryCoverage,
    pub latest_results: BTreeMap<String, Option<WorkCompletionRef>>,
    pub observations: BTreeMap<WorkKey, WorkAvailability>,
    pub records: Vec<Arc<QualifiedWorkRecord>>,
    pub budget: Option<InputEvidenceBudgetLedger>,
    pub budget_views: Vec<Arc<BudgetViewGuard>>,
}

impl ResidentWorkDomain {
    pub(crate) fn bootstrap(
        scope: InputEvidenceScope,
        coverage: WorkCoverage,
        records: Vec<Arc<QualifiedWorkRecord>>,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Self> {
        ensure!(
            scope.identity_incarnation > 0 && !scope.agent_id.is_empty(),
            "Work scope is invalid"
        );
        let mut domain = Self {
            scope,
            coverage,
            records: BTreeMap::new(),
            record_limit,
            byte_limit,
            budget: None,
            budget_views: Vec::new(),
        };
        for record in records {
            domain.insert(record)?;
        }
        domain.trim_and_validate()?;
        Ok(domain)
    }
    pub(crate) fn with_budget(mut self, budget: InputEvidenceBudgetLedger) -> Result<Self> {
        self.budget = Some(budget);
        self.refresh_budget_views()?;
        if let Some(budget) = &self.budget {
            budget.check_history_budget()?;
        }
        Ok(self)
    }
    fn metadata_bytes(
        scope: &InputEvidenceScope,
        coverage: &WorkCoverage,
        record_count: usize,
    ) -> Result<u64> {
        let nodes = record_count
            .checked_mul(std::mem::size_of::<(WorkKey, Arc<QualifiedWorkRecord>)>())
            .ok_or_else(|| anyhow::anyhow!("Work metadata estimate overflow"))?;
        serialized_bytes(&(scope, coverage))?
            .checked_add(nodes as u64)
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>() as u64))
            .ok_or_else(|| anyhow::anyhow!("Work metadata estimate overflow"))
    }
    fn refresh_budget_views(&mut self) -> Result<()> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        let mut roots = BTreeSet::new();
        roots.extend(self.coverage.mandatory_roots.iter().cloned());
        roots.extend(
            self.coverage
                .live_work_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Work, id)),
        );
        roots.extend(
            self.coverage
                .unresolved_wait_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        roots.extend(
            self.coverage
                .active_frame_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Frame, id)),
        );
        roots.extend(
            self.coverage
                .turn_wait_keys
                .values()
                .chain(self.coverage.task_wait_keys.values())
                .chain(self.coverage.trigger_wait_keys.values())
                .flatten()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        let current = self.closure(roots)?;
        let history = self.closure(self.coverage.nonopen.keys.iter().cloned().collect())?;
        let views = vec![
            budget.register_work_view(
                &self.scope,
                InputMemoryCategory::Current,
                self.records
                    .iter()
                    .filter(|(key, _)| current.contains(*key))
                    .map(|(_, record)| record),
            )?,
            budget.register_work_view(
                &self.scope,
                InputMemoryCategory::History,
                self.records
                    .iter()
                    .filter(|(key, _)| history.contains(*key))
                    .map(|(_, record)| record),
            )?,
            budget.register_owned_bytes(
                &self.scope,
                InputMemoryCategory::Current,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: Self::metadata_bytes(
                        &self.scope,
                        &self.coverage,
                        self.records.len(),
                    )?,
                },
            )?,
        ];
        self.budget_views = views;
        Ok(())
    }
    pub(crate) fn candidate_budget_view(&self) -> Result<Option<Arc<BudgetViewGuard>>> {
        self.budget
            .as_ref()
            .map(|budget| {
                budget.register_work_view(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    self.records.values(),
                )
            })
            .transpose()
            .map_err(Into::into)
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(crate) fn coverage(&self) -> &WorkCoverage {
        &self.coverage
    }
    pub(crate) fn with_committed(&self, delta: &CommittedWorkDelta) -> Result<Self> {
        ensure!(
            delta.scope == self.scope && delta.revision == delta.coverage.revision,
            "Work delta scope/revision mismatch"
        );
        if delta.base_revision != self.coverage.revision {
            return Err(WorkReadError::RevisionMismatch {
                expected: self.coverage.revision,
                actual: delta.base_revision,
            }
            .into());
        }
        ensure!(
            delta.revision >= delta.base_revision,
            "Work delta revision regressed"
        );
        if delta.revision == delta.base_revision {
            ensure!(
                delta.records.is_empty()
                    && delta.removals.is_empty()
                    && delta.coverage == self.coverage,
                "Work no-op delta changed facts"
            );
        }
        // Observe the new map/coverage allocation before cloning it. These
        // temporary guards never own the bodies and transfer to final views.
        let _candidate_views = self
            .budget
            .as_ref()
            .map(|budget| -> Result<_> {
                let count = self
                    .records
                    .len()
                    .checked_add(delta.records.len())
                    .ok_or_else(|| anyhow::anyhow!("Work candidate metadata overflow"))?;
                Ok(vec![
                    budget.register_work_view(
                        &self.scope,
                        InputMemoryCategory::Candidate,
                        self.records.values().chain(delta.records.iter()),
                    )?,
                    budget.register_owned_bytes(
                        &self.scope,
                        InputMemoryCategory::Candidate,
                        OwnedBytesEstimate {
                            payload_bytes: 0,
                            metadata_estimate_bytes: Self::metadata_bytes(
                                &self.scope,
                                &delta.coverage,
                                count,
                            )?,
                        },
                    )?,
                ])
            })
            .transpose()?;
        let mut next = Self {
            scope: self.scope.clone(),
            coverage: self.coverage.clone(),
            records: self.records.clone(),
            record_limit: self.record_limit,
            byte_limit: self.byte_limit,
            budget: self.budget.clone(),
            budget_views: Vec::new(),
        };
        for (key, availability) in &delta.removals {
            ensure!(
                *availability != WorkAvailability::Present
                    && delta.coverage.observations.get(key) == Some(availability),
                "Work removal has no exact availability"
            );
            next.records.remove(key);
        }
        for record in &delta.records {
            ensure!(
                delta.coverage.observations.get(&record.key) == Some(&WorkAvailability::Present),
                "Work addition lacks an exact present observation"
            );
            next.insert(record.clone())?;
        }
        next.coverage = delta.coverage.clone();
        next.trim_and_validate()?;
        next.refresh_budget_views()?;
        if let Some(budget) = &next.budget {
            budget.check_history_budget()?;
        }
        Ok(next)
    }
    pub(crate) fn capture(&self, request: &WorkCaptureRequest) -> Result<CapturedWork> {
        if request.include_live && !self.coverage.live_complete {
            return Err(WorkReadError::IncompleteLive.into());
        }
        if request.nonopen_limit > self.coverage.nonopen.limit {
            return Err(WorkReadError::WindowExpansion {
                requested: request.nonopen_limit,
                prepared: self.coverage.nonopen.limit,
            }
            .into());
        }
        let mut roots = self.coverage.mandatory_roots.clone();
        roots.extend(request.mandatory_roots.iter().cloned());
        if request.include_live {
            roots.extend(
                self.coverage
                    .live_work_ids
                    .iter()
                    .map(|id| WorkKey::new(WorkFamily::Work, id)),
            );
            roots.extend(
                self.coverage
                    .unresolved_wait_ids
                    .iter()
                    .map(|id| WorkKey::new(WorkFamily::Wait, id)),
            );
            roots.extend(
                self.coverage
                    .active_frame_ids
                    .iter()
                    .map(|id| WorkKey::new(WorkFamily::Frame, id)),
            );
        }
        roots.extend(
            self.coverage
                .nonopen
                .keys
                .iter()
                .take(request.nonopen_limit)
                .cloned(),
        );
        roots.extend(
            request
                .latest_result_work_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Work, id)),
        );
        let turn_wait_keys = request
            .required_turn_ids
            .iter()
            .map(|turn_id| {
                self.coverage
                    .turn_wait_keys
                    .get(turn_id)
                    .cloned()
                    .map(|keys| (turn_id.clone(), keys))
                    .ok_or_else(|| WorkReadError::TurnWaitsNotPrepared(turn_id.clone()).into())
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        roots.extend(
            turn_wait_keys
                .values()
                .flatten()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        let task_wait_keys = request
            .required_task_ids
            .iter()
            .map(|id| {
                self.coverage
                    .task_wait_keys
                    .get(id)
                    .cloned()
                    .map(|keys| (id.clone(), keys))
                    .ok_or_else(|| WorkReadError::TaskWaitsNotPrepared(id.clone()).into())
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let trigger_wait_keys = request
            .required_trigger_message_ids
            .iter()
            .map(|id| {
                self.coverage
                    .trigger_wait_keys
                    .get(id)
                    .cloned()
                    .map(|keys| (id.clone(), keys))
                    .ok_or_else(|| WorkReadError::TriggerWaitsNotPrepared(id.clone()).into())
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        roots.extend(
            task_wait_keys
                .values()
                .chain(trigger_wait_keys.values())
                .flatten()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        let selected = self.closure(roots)?;
        let work_ids = selected
            .iter()
            .filter(|key| key.family == WorkFamily::Work)
            .map(|key| key.id.clone())
            .collect::<BTreeSet<_>>();
        let latest_results = work_ids
            .iter()
            .map(|id| {
                self.coverage
                    .latest_results
                    .get(id)
                    .cloned()
                    .map(|value| (id.clone(), value))
                    .ok_or_else(|| {
                        WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Work, id)).into()
                    })
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut captured = CapturedWork {
            scope: self.scope.clone(),
            revision: self.coverage.revision,
            live_complete: request.include_live && self.coverage.live_complete,
            live_work_ids: if request.include_live {
                self.coverage.live_work_ids.clone()
            } else {
                BTreeSet::new()
            },
            unresolved_wait_ids: if request.include_live {
                self.coverage.unresolved_wait_ids.clone()
            } else {
                BTreeSet::new()
            },
            active_frame_ids: if request.include_live {
                self.coverage.active_frame_ids.clone()
            } else {
                BTreeSet::new()
            },
            nonopen: WorkHistoryCoverage {
                limit: request.nonopen_limit,
                keys: self
                    .coverage
                    .nonopen
                    .keys
                    .iter()
                    .take(request.nonopen_limit)
                    .cloned()
                    .collect(),
                is_exhaustive: self.coverage.nonopen.is_exhaustive
                    && self.coverage.nonopen.keys.len() <= request.nonopen_limit,
            },
            latest_results,
            turn_wait_keys,
            task_wait_keys,
            trigger_wait_keys,
            observations: selected
                .iter()
                .map(|key| (key.clone(), self.coverage.observations[key]))
                .collect(),
            records: selected
                .iter()
                .filter_map(|key| self.records.get(key).cloned())
                .collect(),
            budget: self.budget.clone(),
            budget_views: Vec::new(),
        };
        if let Some(budget) = &captured.budget {
            captured.budget_views = vec![
                budget.register_work_view(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    &captured.records,
                )?,
                budget.register_owned_bytes(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: captured.selected_metadata_estimate_bytes()?,
                    },
                )?,
            ];
        }
        Ok(captured)
    }
    fn insert(&mut self, record: Arc<QualifiedWorkRecord>) -> Result<()> {
        record.validate(&self.scope)?;
        if let Some(old) = self.records.get(&record.key) {
            ensure!(
                record.revision >= old.revision
                    && (record.revision != old.revision || record.as_ref() == old.as_ref()),
                "Work record revision/body conflict"
            );
        }
        self.records.insert(record.key.clone(), record);
        Ok(())
    }
    fn closure(&self, roots: BTreeSet<WorkKey>) -> Result<BTreeSet<WorkKey>> {
        let mut selected = BTreeSet::new();
        let mut pending = roots;
        while let Some(key) = pending.pop_first() {
            if !selected.insert(key.clone()) {
                continue;
            }
            if selected.len() > self.record_limit {
                return Err(WorkReadError::ResourceExhausted("required record count").into());
            }
            match self.coverage.observations.get(&key) {
                Some(WorkAvailability::Present) => {
                    let record = self
                        .records
                        .get(&key)
                        .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?;
                    pending.extend(record.record.references());
                }
                Some(WorkAvailability::Absent | WorkAvailability::Pruned) => {}
                None => return Err(WorkReadError::NeedsPreparation(key).into()),
            }
        }
        Ok(selected)
    }
    fn trim_and_validate(&mut self) -> Result<()> {
        ensure!(
            self.coverage.live_complete && self.coverage.revision > 0,
            "Work domain needs complete live coverage"
        );
        let history = &self.coverage.nonopen;
        ensure!(
            history.limit <= WORK_NONOPEN_WINDOW
                && history.keys.len() <= history.limit
                && (history.is_exhaustive || history.keys.len() == history.limit),
            "Work history coverage incomplete"
        );
        ensure!(
            history.keys.iter().collect::<BTreeSet<_>>().len() == history.keys.len(),
            "Work history has duplicate keys"
        );
        let selected = self.closure(self.coverage.roots())?;
        for key in &selected {
            if let Some(record) = self.records.get(key) {
                record.validate(&self.scope)?;
                ensure!(
                    record.revision <= self.coverage.revision,
                    "Work body is newer than its coverage"
                );
                if self.coverage.observations.get(key) == Some(&WorkAvailability::Present) {
                    let agrees = match &record.record {
                        CanonicalWorkRecord::Work(x) => {
                            self.coverage.live_work_ids.contains(&x.id)
                                == matches!(
                                    x.state,
                                    WorkItemState::Open | WorkItemState::Completing
                                )
                        }
                        CanonicalWorkRecord::Wait(x) => {
                            self.coverage.unresolved_wait_ids.contains(&x.id)
                                == matches!(
                                    x.status,
                                    WaitConditionStatus::Active | WaitConditionStatus::Triggered
                                )
                        }
                        CanonicalWorkRecord::Frame(x) => {
                            self.coverage.active_frame_ids.contains(&x.id)
                                == (x.state == WorkItemContinuationState::Active)
                        }
                    };
                    ensure!(
                        agrees,
                        "Work complete membership omits an actual live record"
                    );
                    for dependency in record.record.references() {
                        ensure!(
                            self.coverage.observations.get(&dependency)
                                == Some(&WorkAvailability::Present),
                            "Work owning dependency lacks a present qualified body"
                        );
                    }
                }
            }
        }
        for id in &self.coverage.live_work_ids {
            let key = WorkKey::new(WorkFamily::Work, id);
            ensure!(
                self.coverage.observations.get(&key) == Some(&WorkAvailability::Present),
                "Work live membership has no present observation"
            );
            ensure!(
                matches!(&self.records.get(&key).ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?.record,
                CanonicalWorkRecord::Work(x) if matches!(x.state,WorkItemState::Open|WorkItemState::Completing)),
                "Work live membership differs from actual record"
            );
        }
        for id in &self.coverage.unresolved_wait_ids {
            let key = WorkKey::new(WorkFamily::Wait, id);
            ensure!(
                self.coverage.observations.get(&key) == Some(&WorkAvailability::Present),
                "Work live membership has no present observation"
            );
            ensure!(
                matches!(&self.records.get(&key).ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?.record,
                CanonicalWorkRecord::Wait(x) if matches!(x.status,WaitConditionStatus::Active|WaitConditionStatus::Triggered)),
                "Work wait membership differs from actual record"
            );
        }
        for id in &self.coverage.active_frame_ids {
            let key = WorkKey::new(WorkFamily::Frame, id);
            ensure!(
                self.coverage.observations.get(&key) == Some(&WorkAvailability::Present),
                "Work live membership has no present observation"
            );
            ensure!(
                matches!(&self.records.get(&key).ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?.record,
                CanonicalWorkRecord::Frame(x) if x.state==WorkItemContinuationState::Active),
                "Work frame membership differs from actual record"
            );
        }
        for (turn_id, wait_ids) in &self.coverage.turn_wait_keys {
            ensure!(
                !turn_id.is_empty(),
                "Work Turn Wait relation has an empty Turn ID"
            );
            for wait_id in wait_ids {
                let key = WorkKey::new(WorkFamily::Wait, wait_id);
                ensure!(
                    self.coverage.observations.get(&key) == Some(&WorkAvailability::Present),
                    "Work Turn Wait relation lacks a present qualified Wait"
                );
                ensure!(
                    matches!(&self.records.get(&key).ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?.record,
                    CanonicalWorkRecord::Wait(wait) if wait.turn_id.as_deref() == Some(turn_id.as_str())),
                    "Work Turn Wait relation disagrees with its Wait"
                );
            }
        }
        for record in self.records.values() {
            if let CanonicalWorkRecord::Wait(wait) = &record.record {
                if let Some(observed) = wait
                    .turn_id
                    .as_ref()
                    .and_then(|id| self.coverage.turn_wait_keys.get(id))
                {
                    ensure!(
                        observed.contains(&wait.id),
                        "Work Turn Wait relation omits an observed Wait"
                    );
                }
            }
        }
        for (task_id, ids) in &self.coverage.task_wait_keys {
            ensure!(
                !task_id.is_empty(),
                "Work Task Wait relation has an empty Task ID"
            );
            for id in ids {
                let key = WorkKey::new(WorkFamily::Wait, id);
                ensure!(
                    self.coverage.observations.get(&key) == Some(&WorkAvailability::Present),
                    "Work Task Wait relation lacks a present qualified Wait"
                );
                ensure!(
                    matches!(&self.records.get(&key).ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?.record,
                    CanonicalWorkRecord::Wait(wait) if task_wait_matches(wait, task_id)),
                    "Work Task Wait relation disagrees with its Wait"
                );
            }
        }
        for (message_id, ids) in &self.coverage.trigger_wait_keys {
            ensure!(
                !message_id.is_empty(),
                "Work trigger Wait relation has an empty Message ID"
            );
            for id in ids {
                let key = WorkKey::new(WorkFamily::Wait, id);
                ensure!(
                    self.coverage.observations.get(&key) == Some(&WorkAvailability::Present),
                    "Work trigger Wait relation lacks a present qualified Wait"
                );
                ensure!(
                    matches!(&self.records.get(&key).ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?.record,
                    CanonicalWorkRecord::Wait(wait) if wait.trigger_message_id() == Some(message_id.as_str())),
                    "Work trigger Wait relation disagrees with its Wait"
                );
            }
        }
        for record in self.records.values() {
            if let CanonicalWorkRecord::Wait(wait) = &record.record {
                for task_id in wait.wake_sources.iter().filter_map(|source| match source {
                    crate::types::WakeSource::TaskResult { task_id } => Some(task_id),
                    _ => None,
                }) {
                    if let Some(ids) = self.coverage.task_wait_keys.get(task_id) {
                        ensure!(
                            ids.contains(&wait.id),
                            "Work Task Wait relation omits an observed Wait"
                        );
                    }
                }
                if let Some(ids) = wait
                    .trigger_message_id()
                    .and_then(|id| self.coverage.trigger_wait_keys.get(id))
                {
                    ensure!(
                        ids.contains(&wait.id),
                        "Work trigger Wait relation omits an observed Wait"
                    );
                }
            }
        }
        for key in &history.keys {
            ensure!(
                self.coverage.observations.get(key) == Some(&WorkAvailability::Present),
                "Work history membership has no present observation"
            );
            ensure!(
                key.family == WorkFamily::Work
                    && matches!(&self.records.get(key).ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?.record,
                CanonicalWorkRecord::Work(x) if x.state != WorkItemState::Open),
                "Work history membership differs from actual record"
            );
        }
        for key in &selected {
            if key.family == WorkFamily::Work && !self.coverage.latest_results.contains_key(&key.id)
            {
                return Err(WorkReadError::NeedsPreparation(key.clone()).into());
            }
        }
        for (id, result) in &self.coverage.latest_results {
            if let Some(result) = result {
                ensure!(
                    !result.brief_id.is_empty()
                        && result
                            .transcript_entry_id
                            .as_ref()
                            .is_none_or(|id| !id.is_empty())
                        && result
                            .source_message_id
                            .as_ref()
                            .is_none_or(|id| !id.is_empty()),
                    "Work completion reference has an empty identifier"
                );
                ensure!(
                    self.coverage
                        .observations
                        .get(&WorkKey::new(WorkFamily::Work, id))
                        == Some(&WorkAvailability::Present),
                    "absent/pruned Work has a completion reference"
                );
            }
        }
        for pair in history.keys.windows(2) {
            let work = |key: &WorkKey| match &self.records[key].record {
                CanonicalWorkRecord::Work(record) => record,
                _ => unreachable!("validated history family"),
            };
            let first = work(&pair[0]);
            let second = work(&pair[1]);
            ensure!(
                (
                    std::cmp::Reverse(first.updated_at),
                    std::cmp::Reverse(first.created_at),
                    &first.id
                ) <= (
                    std::cmp::Reverse(second.updated_at),
                    std::cmp::Reverse(second.created_at),
                    &second.id
                ),
                "Work history order differs from the canonical frontier"
            );
        }
        self.records.retain(|key, _| {
            selected.contains(key)
                && self.coverage.observations.get(key) == Some(&WorkAvailability::Present)
        });
        self.coverage
            .observations
            .retain(|key, _| selected.contains(key));
        self.coverage
            .latest_results
            .retain(|id, _| selected.contains(&WorkKey::new(WorkFamily::Work, id)));
        let bytes = self.records.values().try_fold(
            serde_json::to_vec(&self.coverage)?.len(),
            |total, x| {
                total
                    .checked_add(x.resident_bytes)
                    .ok_or_else(|| anyhow::anyhow!("Work byte estimate overflow"))
            },
        )?;
        if bytes > self.byte_limit {
            return Err(WorkReadError::ResourceExhausted("required body/metadata bytes").into());
        }
        Ok(())
    }
}

impl CapturedWork {
    pub(crate) fn selected_metadata_estimate_bytes(&self) -> Result<u64> {
        let metadata = serialized_bytes(&(
            &self.scope,
            self.revision,
            self.live_complete,
            &self.live_work_ids,
            &self.unresolved_wait_ids,
            &self.active_frame_ids,
            &self.turn_wait_keys,
            &self.task_wait_keys,
            &self.trigger_wait_keys,
            &self.nonopen,
            &self.latest_results,
            self.observations.iter().collect::<Vec<_>>(),
        ))?;
        let nodes = self
            .records
            .len()
            .checked_mul(std::mem::size_of::<Arc<QualifiedWorkRecord>>())
            .ok_or_else(|| anyhow::anyhow!("captured Work metadata estimate overflow"))?;
        metadata
            .checked_add(nodes as u64)
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>() as u64))
            .ok_or_else(|| anyhow::anyhow!("captured Work metadata estimate overflow"))
    }
    pub(crate) fn lookup(&self, key: &WorkKey) -> Result<Option<&Arc<QualifiedWorkRecord>>> {
        self.lookup_parts(key.family, &key.id)
    }
    pub(crate) fn lookup_parts(
        &self,
        family: WorkFamily,
        id: &str,
    ) -> Result<Option<&Arc<QualifiedWorkRecord>>> {
        match self
            .observations
            .get::<dyn WorkKeyLookup + '_>(&(family, id))
        {
            Some(WorkAvailability::Present) => self
                .records
                .iter()
                .find(|x| x.key.family == family && x.key.id == id)
                .map(Some)
                .ok_or_else(|| WorkReadError::NeedsPreparation(WorkKey::new(family, id)).into()),
            Some(WorkAvailability::Absent | WorkAvailability::Pruned) => Ok(None),
            None => Err(WorkReadError::NeedsPreparation(WorkKey::new(family, id)).into()),
        }
    }
    /// This relation does not qualify or load the Turn body. It only preserves
    /// the original WaitRepository::for_turn membership and canonical order.
    pub(crate) fn waits_for_turn(&self, turn_id: &str) -> Result<Vec<&WaitConditionRecord>> {
        let keys = self
            .turn_wait_keys
            .get(turn_id)
            .ok_or_else(|| WorkReadError::TurnWaitsNotPrepared(turn_id.to_owned()))?;
        let mut waits = keys
            .iter()
            .map(|id| {
                let key = WorkKey::new(WorkFamily::Wait, id);
                let record = self
                    .lookup(&key)?
                    .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?;
                match &record.record {
                    CanonicalWorkRecord::Wait(wait) if wait.turn_id.as_deref() == Some(turn_id) => {
                        Ok(wait)
                    }
                    _ => anyhow::bail!("captured Work Turn Wait relation differs from its Wait"),
                }
            })
            .collect::<Result<Vec<_>>>()?;
        waits.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(waits)
    }
    pub(crate) fn waits_for_task(&self, task_id: &str) -> Result<Vec<&WaitConditionRecord>> {
        let ids = self
            .task_wait_keys
            .get(task_id)
            .ok_or_else(|| WorkReadError::TaskWaitsNotPrepared(task_id.to_owned()))?;
        self.waits_in_relation(ids, |wait| task_wait_matches(wait, task_id))
    }
    pub(crate) fn waits_triggered_by(&self, message_id: &str) -> Result<Vec<&WaitConditionRecord>> {
        let ids = self
            .trigger_wait_keys
            .get(message_id)
            .ok_or_else(|| WorkReadError::TriggerWaitsNotPrepared(message_id.to_owned()))?;
        self.waits_in_relation(ids, |wait| wait.trigger_message_id() == Some(message_id))
    }
    fn waits_in_relation(
        &self,
        ids: &BTreeSet<String>,
        predicate: impl Fn(&WaitConditionRecord) -> bool,
    ) -> Result<Vec<&WaitConditionRecord>> {
        ids.iter()
            .map(|id| {
                let key = WorkKey::new(WorkFamily::Wait, id);
                let record = self
                    .lookup(&key)?
                    .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?;
                match &record.record {
                    CanonicalWorkRecord::Wait(wait) if predicate(wait) => Ok(wait),
                    _ => anyhow::bail!("captured Work Wait relation differs from its Wait"),
                }
            })
            .collect()
    }
    pub(crate) fn latest_result(&self, work_item_id: &str) -> Result<Option<&WorkCompletionRef>> {
        if let Some(head) = self.latest_results.get(work_item_id) {
            return Ok(head.as_ref());
        }
        crate::diagnostics::resource_io::admit_cloned_value(&work_item_id)?;
        Err(WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Work, work_item_id)).into())
    }
    /// The trigger delivery observation remains an explicit D6 adapter; no SQL
    /// or Task/Timer/Trigger authority is hidden inside this pure projection.
    pub(crate) fn work_queue_with_material(
        &self,
        state: &AgentState,
        triggers: &[ExternalTriggerRecord],
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<crate::work_item_scheduling::WorkQueueReadModel> {
        let _entry = material.enter();
        let triggers = clone_context_queue_triggers(triggers)?;
        self.work_queue(state, triggers)
    }

    pub(crate) fn work_queue(
        &self,
        state: &AgentState,
        triggers: Vec<ExternalTriggerRecord>,
    ) -> Result<crate::work_item_scheduling::WorkQueueReadModel> {
        ensure!(
            state.id == self.scope.agent_id,
            "Work projection State scope mismatch"
        );
        if !self.live_complete {
            return Err(WorkReadError::IncompleteLive.into());
        }
        if self.nonopen.limit < WORK_NONOPEN_WINDOW && !self.nonopen.is_exhaustive {
            return Err(WorkReadError::WindowExpansion {
                requested: WORK_NONOPEN_WINDOW,
                prepared: self.nonopen.limit,
            }
            .into());
        }
        if let Some(id) = &state.current_work_item_id {
            self.lookup(&WorkKey::new(WorkFamily::Work, id))?;
        }
        ensure!(
            triggers
                .iter()
                .all(|record| record.target_agent_id == self.scope.agent_id),
            "Work trigger adapter scope mismatch"
        );
        let displayed_work_ids = queue_displayed_work_ids(self)?;
        let mut works = crate::storage::QueueWorkBuilder::default();
        let mut waits = Vec::new();
        let mut frames = Vec::new();
        for evidence in &self.records {
            match &evidence.record {
                CanonicalWorkRecord::Work(x)
                    if displayed_work_ids.binary_search(&x.id.as_str()).is_ok() =>
                {
                    let key = crate::storage::clone_queue_work_item_key(&x.id)?;
                    works.push(key, clone_queue_work_record(x)?)?;
                }
                CanonicalWorkRecord::Wait(x)
                    if self.unresolved_wait_ids.contains(&x.id)
                        && x.status == WaitConditionStatus::Active =>
                {
                    push_queue_record_clone(&mut waits, x)?;
                }
                CanonicalWorkRecord::Frame(x) if self.active_frame_ids.contains(&x.id) => {
                    push_queue_record_clone(&mut frames, x)?;
                }
                _ => {}
            }
        }
        // Triggered waits remain resident obligations, but the existing queue
        // reader projects only Active waits owned by an Open WorkItem.
        let works = works.finish();
        retain_queue_active_waits(&mut waits, &works);
        crate::storage::derive_context_work_queue(
            state.current_work_item_id.as_deref(),
            works,
            triggers,
            waits,
            frames,
        )
    }
}

fn clone_context_queue_triggers(
    triggers: &[ExternalTriggerRecord],
) -> Result<Vec<ExternalTriggerRecord>> {
    let mut records = Vec::new();
    if !triggers.is_empty() {
        crate::diagnostics::resource_io::admit_cloned_value(&triggers)
            .context("context queue trigger clone material")?;
        records
            .try_reserve_exact(triggers.len())
            .context("reserve context queue trigger records")?;
        records.extend_from_slice(triggers);
    }
    Ok(records)
}

fn queue_displayed_work_ids(captured: &CapturedWork) -> Result<Vec<&str>> {
    let candidates = captured
        .records
        .iter()
        .filter_map(|evidence| match &evidence.record {
            CanonicalWorkRecord::Work(work) if work.state == WorkItemState::Open => {
                Some(work.id.as_str())
            }
            _ => None,
        })
        .chain(captured.nonopen.keys.iter().map(|key| key.id.as_str()));
    let count = candidates.clone().count();
    let mut ids = Vec::new();
    if count > 0 {
        let bytes = count
            .checked_mul(std::mem::size_of::<&str>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| {
                std::io::Error::other("ResourceExhausted: resident displayed-ID slots")
            })?;
        crate::diagnostics::resource_io::charge_owned_metadata(bytes, count)
            .context("resident displayed-ID slots admission")?;
        ids.try_reserve_exact(count).map_err(|_| {
            std::io::Error::other("ResourceExhausted: resident displayed-ID allocation")
        })?;
    }
    ids.extend(candidates);
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn clone_queue_work_record(work: &WorkItemRecord) -> Result<WorkItemRecord> {
    crate::diagnostics::resource_io::admit_cloned_value(work)
        .context("resident queue WorkItem clone admission")?;
    Ok(work.clone())
}

fn push_queue_record_clone<T: Serialize + Clone>(records: &mut Vec<T>, record: &T) -> Result<()> {
    if records.len() == records.capacity() {
        let capacity = records.capacity();
        let additional = capacity.max(4);
        let bytes = capacity
            .checked_add(additional)
            .and_then(|_| additional.checked_mul(std::mem::size_of::<T>()))
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| {
                std::io::Error::other("ResourceExhausted: resident queue record slots")
            })?;
        crate::diagnostics::resource_io::charge_owned_metadata(bytes, additional)
            .context("resident queue record slots admission")?;
        records.try_reserve_exact(additional).map_err(|_| {
            std::io::Error::other("ResourceExhausted: resident queue record allocation")
        })?;
    }
    crate::diagnostics::resource_io::admit_cloned_value(record)
        .context("resident queue record clone admission")?;
    records.push(record.clone());
    Ok(())
}

fn retain_queue_active_waits(
    waits: &mut Vec<WaitConditionRecord>,
    works: &crate::storage::QueueLatestWork,
) {
    waits.retain(|wait| {
        wait.work_item_id.as_ref().is_none_or(|id| {
            works
                .get(id)
                .is_some_and(|work| work.state == WorkItemState::Open)
        })
    });
}

// JSON object keys cannot represent the typed family/id pair. An ordered pair
// list preserves its exact shape and rejects duplicate keys on durable decode.
mod work_observations {
    use super::{BTreeMap, WorkAvailability, WorkKey};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        values: &BTreeMap<WorkKey, WorkAvailability>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        values.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<WorkKey, WorkAvailability>, D::Error> {
        let entries = Vec::<(WorkKey, WorkAvailability)>::deserialize(deserializer)?;
        let mut values = BTreeMap::new();
        for (key, value) in entries {
            if values.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("duplicate Work availability key"));
            }
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod queue_displayed_id_material_tests;
    mod queue_record_material_tests;
    mod queue_work_material_tests;

    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "resident-work-agent".into(),
            identity_incarnation: 3,
        }
    }
    fn qualified(id: &str, state: WorkItemState, revision: u64) -> Arc<QualifiedWorkRecord> {
        let mut work = WorkItemRecord::new(&scope().agent_id, id, state);
        work.id = id.into();
        work.created_at = chrono::DateTime::from_timestamp_millis(1_000).unwrap();
        work.updated_at = work.created_at;
        let record = CanonicalWorkRecord::Work(work);
        Arc::new(QualifiedWorkRecord {
            scope: scope(),
            key: record.key(),
            revision,
            content_digest: "a".repeat(64),
            resident_bytes: serde_json::to_vec(&record).unwrap().len(),
            record,
        })
    }
    #[test]
    fn encoded_body_floor_preserves_escaped_exact_and_one_byte_short_boundaries() -> Result<()> {
        let mut record = Arc::try_unwrap(qualified("escaped-floor", WorkItemState::Open, 1))
            .expect("test fact has one holder");
        let CanonicalWorkRecord::Work(work) = &mut record.record else {
            unreachable!()
        };
        work.objective = "line\n\t\"\u{0000}中😀\\".repeat(32);
        let original_encoded = serde_json::to_vec(&record.record)?.len();
        record.resident_bytes = original_encoded;
        record.validate(&scope())?;
        record.resident_bytes = original_encoded - 1;
        assert!(record
            .validate(&scope())
            .unwrap_err()
            .to_string()
            .contains("body estimate is incomplete"));
        record.resident_bytes = original_encoded + 1;
        record.validate(&scope())?;
        let mut foreign = scope();
        foreign.identity_incarnation += 1;
        assert!(record
            .validate(&foreign)
            .unwrap_err()
            .to_string()
            .contains("scope/key mismatch"));
        Ok(())
    }

    fn coverage(records: &[Arc<QualifiedWorkRecord>], revision: u64) -> WorkCoverage {
        let mut value = WorkCoverage {
            revision,
            live_complete: true,
            live_work_ids: BTreeSet::new(),
            unresolved_wait_ids: BTreeSet::new(),
            active_frame_ids: BTreeSet::new(),
            turn_wait_keys: BTreeMap::new(),
            task_wait_keys: BTreeMap::new(),
            trigger_wait_keys: BTreeMap::new(),
            mandatory_roots: BTreeSet::new(),
            observations: BTreeMap::new(),
            nonopen: WorkHistoryCoverage {
                limit: WORK_NONOPEN_WINDOW,
                keys: Vec::new(),
                is_exhaustive: true,
            },
            latest_results: BTreeMap::new(),
        };
        for evidence in records {
            value
                .observations
                .insert(evidence.key.clone(), WorkAvailability::Present);
            match &evidence.record {
                CanonicalWorkRecord::Work(work) => {
                    if matches!(work.state, WorkItemState::Open | WorkItemState::Completing) {
                        value.live_work_ids.insert(work.id.clone());
                    }
                    if work.state != WorkItemState::Open {
                        value.nonopen.keys.push(evidence.key.clone());
                    }
                    value.latest_results.insert(work.id.clone(), None);
                }
                CanonicalWorkRecord::Wait(wait) => {
                    if matches!(
                        wait.status,
                        WaitConditionStatus::Active | WaitConditionStatus::Triggered
                    ) {
                        value.unresolved_wait_ids.insert(wait.id.clone());
                    }
                }
                CanonicalWorkRecord::Frame(frame) => {
                    if frame.state == WorkItemContinuationState::Active {
                        value.active_frame_ids.insert(frame.id.clone());
                    }
                }
            }
        }
        value.nonopen.keys.sort();
        value
    }
    fn domain(
        records: Vec<Arc<QualifiedWorkRecord>>,
        coverage: WorkCoverage,
    ) -> ResidentWorkDomain {
        ResidentWorkDomain::bootstrap(scope(), coverage, records, 1_000, 4 * 1024 * 1024).unwrap()
    }

    #[test]
    fn queue_latest_key_admission_preserves_resident_scope_result_and_phase() -> Result<()> {
        use crate::{config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial};
        let id = "resident-key-中🦀".repeat(31);
        let records = vec![qualified(&id, WorkItemState::Open, 1)];
        let captured = domain(records.clone(), coverage(&records, 1))
            .capture(&WorkCaptureRequest::current())?;
        let state = AgentState::new(&scope().agent_id);
        let expected = captured.work_queue(&state, vec![])?;
        let header = ((scope().agent_id.len() as u64 + 256) * 8, 64);
        let displayed = (std::mem::size_of::<&str>() as u64 * 8, 8);
        let prefix = (header.0 + displayed.0, header.1 + displayed.1);
        let key = ((id.len() + std::mem::size_of::<String>()) as u64 * 8, 8);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                prefix.0 + key.0 - u64::from(bytes),
                prefix.1 + key.1 - usize::from(!bytes),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &holder,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                for attempt in 0..2 {
                    let error = captured.work_queue(&state, vec![]).unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        if attempt == 0 {
                            "queue latest key admission"
                        } else {
                            "resident displayed-ID slots admission"
                        }
                    );
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        let queue = {
            let _entry = phase.enter();
            captured.work_queue(&state, vec![])?
        };
        assert_eq!(queue.items, expected.items);
        let total = phase.finish()?;
        drop((queue, phase));
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn live_work_is_complete_beyond_the_nonopen_history_window() {
        let mut records = (0..129)
            .map(|index| qualified(&format!("open-{index:03}"), WorkItemState::Open, 1))
            .collect::<Vec<_>>();
        records.push(qualified("completing", WorkItemState::Completing, 1));
        records.push(qualified("completed", WorkItemState::Completed, 1));
        let captured = domain(records.clone(), coverage(&records, 1))
            .capture(&WorkCaptureRequest::current())
            .unwrap();
        assert_eq!(captured.live_work_ids.len(), 130);
        assert!(captured.live_work_ids.contains("completing"));
        assert_eq!(captured.nonopen.keys.len(), 2);
        assert_eq!(captured.records.len(), 131);
    }

    #[test]
    fn selected_capture_releases_unselected_allocations_with_the_domain() {
        let first = qualified("first", WorkItemState::Open, 1);
        let second = qualified("second", WorkItemState::Open, 1);
        let first_weak = Arc::downgrade(&first);
        let second_weak = Arc::downgrade(&second);
        let records = vec![first, second];
        let mut prepared = coverage(&records, 1);
        prepared
            .mandatory_roots
            .insert(WorkKey::new(WorkFamily::Work, "first"));
        let resident = domain(records, prepared);
        let captured = resident
            .capture(&WorkCaptureRequest {
                include_live: false,
                nonopen_limit: 0,
                mandatory_roots: BTreeSet::new(),
                latest_result_work_ids: BTreeSet::new(),
                required_turn_ids: BTreeSet::new(),
                required_task_ids: BTreeSet::new(),
                required_trigger_message_ids: BTreeSet::new(),
            })
            .unwrap();
        drop(resident);
        assert!(first_weak.upgrade().is_some());
        assert!(second_weak.upgrade().is_none());
        assert_eq!(captured.records.len(), 1);
        drop(captured);
        assert!(first_weak.upgrade().is_none());
    }

    #[test]
    fn exact_absence_survives_selection_and_is_not_uncaptured_absence() {
        let missing = WorkKey::new(WorkFamily::Work, "observed-missing");
        let mut prepared = coverage(&[], 1);
        prepared.mandatory_roots.insert(missing.clone());
        prepared
            .observations
            .insert(missing.clone(), WorkAvailability::Absent);
        prepared.latest_results.insert(missing.id.clone(), None);
        let captured = domain(vec![], prepared)
            .capture(&WorkCaptureRequest::current())
            .unwrap();
        assert!(captured.lookup(&missing).unwrap().is_none());
        assert!(captured.latest_result(&missing.id).unwrap().is_none());
        let error = captured
            .lookup(&WorkKey::new(WorkFamily::Work, "not-prepared"))
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::NeedsPreparation(_))
        ));
    }

    #[test]
    fn owning_dependency_cannot_be_replaced_with_an_absent_observation() {
        let original = qualified("blocked", WorkItemState::Open, 1);
        let mut record = original.as_ref().clone();
        let CanonicalWorkRecord::Work(work) = &mut record.record else {
            unreachable!()
        };
        work.blocked_by_wait_id = Some("missing-wait".into());
        record.resident_bytes = serde_json::to_vec(&record.record).unwrap().len();
        let records = vec![Arc::new(record)];
        let mut prepared = coverage(&records, 1);
        prepared.observations.insert(
            WorkKey::new(WorkFamily::Wait, "missing-wait"),
            WorkAvailability::Absent,
        );
        let error =
            ResidentWorkDomain::bootstrap(scope(), prepared, records, 1_000, 4 * 1024 * 1024)
                .unwrap_err();
        assert!(error.to_string().contains("owning dependency"));
    }

    #[test]
    fn committed_delta_keeps_old_capture_and_requires_exact_base_revision() {
        let old = qualified("work", WorkItemState::Open, 1);
        let resident = domain(vec![old.clone()], coverage(&[old], 1));
        let captured = resident.capture(&WorkCaptureRequest::current()).unwrap();
        let new = qualified("work", WorkItemState::Completed, 2);
        let delta = CommittedWorkDelta {
            scope: scope(),
            base_revision: 1,
            revision: 2,
            records: vec![new.clone()],
            removals: BTreeMap::new(),
            coverage: coverage(&[new], 2),
        };
        let next = resident.with_committed(&delta).unwrap();
        assert!(next.coverage().live_work_ids.is_empty());
        assert!(
            matches!(&captured.records[0].record, CanonicalWorkRecord::Work(work) if work.state == WorkItemState::Open)
        );
        assert!(matches!(
            next.with_committed(&delta)
                .unwrap_err()
                .downcast_ref::<WorkReadError>(),
            Some(WorkReadError::RevisionMismatch { .. })
        ));
    }

    #[test]
    fn wait_lifecycle_projection_preserves_active_open_owner_reader_semantics() {
        let open = qualified("work", WorkItemState::Open, 1);
        let at = chrono::DateTime::from_timestamp_millis(1_000).unwrap();
        let mut wait = WaitConditionRecord {
            id: "wait".into(),
            agent_id: scope().agent_id,
            work_item_id: Some("work".into()),
            status: WaitConditionStatus::Active,
            kind: crate::types::WaitConditionKind::Task,
            source: None,
            subject_ref: Some("task".into()),
            waiting_for: "task result".into(),
            wake_sources: vec![crate::types::WakeSource::TaskResult {
                task_id: "task".into(),
            }],
            continuation: None,
            created_at: at,
            updated_at: at,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        let wait_key = WorkKey::new(WorkFamily::Wait, &wait.id);
        let mut resident: Option<ResidentWorkDomain> = None;
        let mut state = AgentState::new(scope().agent_id);
        state.current_work_item_id = Some("work".into());
        for (index, status) in [
            WaitConditionStatus::Active,
            WaitConditionStatus::Triggered,
            WaitConditionStatus::Resolved,
        ]
        .into_iter()
        .enumerate()
        {
            let revision = index as u64 + 1;
            wait.status = status;
            wait.updated_at = at + chrono::Duration::milliseconds(index as i64);
            if index > 0 {
                wait.trigger_message_id = Some("wake".into());
                wait.triggered_at = Some(at + chrono::Duration::milliseconds(1));
            }
            if index == 2 {
                wait.resolved_at = Some(wait.updated_at);
            }
            let record = CanonicalWorkRecord::Wait(wait.clone());
            let qualified_wait = Arc::new(QualifiedWorkRecord {
                scope: scope(),
                key: wait_key.clone(),
                revision,
                content_digest: "a".repeat(64),
                resident_bytes: serde_json::to_vec(&record).unwrap().len(),
                record,
            });
            let records = vec![open.clone(), qualified_wait.clone()];
            let mut final_coverage = coverage(&records, revision);
            final_coverage.mandatory_roots.insert(wait_key.clone());
            resident = Some(match resident {
                None => domain(records, final_coverage),
                Some(previous) => previous
                    .with_committed(&CommittedWorkDelta {
                        scope: scope(),
                        base_revision: revision - 1,
                        revision,
                        records: vec![qualified_wait],
                        removals: BTreeMap::new(),
                        coverage: final_coverage,
                    })
                    .unwrap(),
            });
            let captured = resident
                .as_ref()
                .unwrap()
                .capture(&WorkCaptureRequest::current())
                .unwrap();
            assert!(captured.lookup(&wait_key).unwrap().is_some());
            assert_eq!(captured.unresolved_wait_ids.contains("wait"), index < 2);
            let projected = captured.work_queue(&state, vec![]).unwrap();
            assert_eq!(projected.items[0].has_active_waits, index == 0);
            let CanonicalWorkRecord::Work(work) = &open.record else {
                unreachable!()
            };
            let legacy_waits = crate::storage::filter_context_active_waits(
                (index == 0).then(|| wait.clone()).into_iter().collect(),
                vec![work.clone()],
            );
            let expected = crate::storage::derive_context_work_queue(
                state.current_work_item_id.as_deref(),
                [(work.id.clone(), work.clone())].into_iter().collect(),
                vec![],
                legacy_waits,
                vec![],
            )
            .unwrap();
            assert_eq!(projected.items, expected.items);
        }

        // A still-Active wait on a no-longer-Open owner is resident but does
        // not reappear in the queue projection's active wait list.
        let completing = qualified("work", WorkItemState::Completing, 4);
        wait.status = WaitConditionStatus::Active;
        wait.resolved_at = None;
        wait.trigger_message_id = None;
        wait.triggered_at = None;
        let record = CanonicalWorkRecord::Wait(wait);
        let qualified_wait = Arc::new(QualifiedWorkRecord {
            scope: scope(),
            key: wait_key,
            revision: 4,
            content_digest: "a".repeat(64),
            resident_bytes: serde_json::to_vec(&record).unwrap().len(),
            record,
        });
        let records = vec![completing, qualified_wait];
        let captured = domain(records.clone(), coverage(&records, 4))
            .capture(&WorkCaptureRequest::current())
            .unwrap();
        assert!(captured.unresolved_wait_ids.contains("wait"));
        assert!(!captured.work_queue(&state, vec![]).unwrap().items[0].has_active_waits);
    }

    #[test]
    fn availability_metadata_roundtrips_and_rejects_duplicate_typed_keys() {
        let record = qualified("work", WorkItemState::Open, 1);
        let prepared = coverage(&[record], 1);
        let encoded = serde_json::to_value(&prepared).unwrap();
        assert_eq!(
            serde_json::from_value::<WorkCoverage>(encoded.clone()).unwrap(),
            prepared
        );
        let mut duplicate = encoded;
        let observations = duplicate["observations"].as_array_mut().unwrap();
        observations.push(observations[0].clone());
        assert!(serde_json::from_value::<WorkCoverage>(duplicate).is_err());
    }

    #[test]
    fn insufficient_current_budget_rejects_domain_instead_of_evicting_live_work() {
        let record = qualified("work", WorkItemState::Open, 1);
        let error = ResidentWorkDomain::bootstrap(
            scope(),
            coverage(&[record.clone()], 1),
            vec![record],
            1_000,
            1,
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::ResourceExhausted(_))
        ));
    }

    #[test]
    fn shared_current_candidate_and_selected_pin_charge_one_weak_allocation() {
        let budget =
            InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
        let record = qualified("work", WorkItemState::Open, 1);
        let weak = Arc::downgrade(&record);
        let resident = domain(vec![record.clone()], coverage(&[record], 1))
            .with_budget(budget.clone())
            .unwrap();
        let captured = resident.capture(&WorkCaptureRequest::current()).unwrap();
        let candidate = budget
            .register_work_view(&scope(), InputMemoryCategory::Candidate, &captured.records)
            .unwrap();
        let report = budget.report().unwrap();
        assert_eq!(report.physical_records.allocations, 1);
        assert_eq!(
            report.categories[&InputMemoryCategory::Candidate].allocations,
            1
        );
        assert_eq!(report.categories[&InputMemoryCategory::Pins].allocations, 2);
        assert!(report.owned.metadata_estimate_bytes > 0);
        drop(resident);
        assert!(weak.upgrade().is_some());
        assert_eq!(budget.report().unwrap().physical_records.allocations, 1);
        drop(captured);
        assert!(
            weak.upgrade().is_none(),
            "an observation guard must not retain Work body"
        );
        assert_eq!(budget.report().unwrap().observed_bytes, 0);
        drop(candidate);
        assert_eq!(budget.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn bounded_history_is_separate_from_current_and_selected_metadata() {
        let budget =
            InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
        let records = (0..129)
            .map(|index| qualified(&format!("closed-{index:03}"), WorkItemState::Completed, 1))
            .collect::<Vec<_>>();
        let outside = Arc::downgrade(&records[128]);
        let mut prepared = coverage(&records, 1);
        prepared.nonopen.keys.truncate(WORK_NONOPEN_WINDOW);
        prepared.nonopen.is_exhaustive = false;
        let resident = domain(records, prepared)
            .with_budget(budget.clone())
            .unwrap();
        assert!(outside.upgrade().is_none());
        let captured = resident
            .capture(&WorkCaptureRequest {
                include_live: false,
                nonopen_limit: 1,
                mandatory_roots: BTreeSet::new(),
                latest_result_work_ids: BTreeSet::new(),
                required_turn_ids: BTreeSet::new(),
                required_task_ids: BTreeSet::new(),
                required_trigger_message_ids: BTreeSet::new(),
            })
            .unwrap();
        assert_eq!(captured.records.len(), 1);
        assert_eq!(captured.observations.len(), 1);
        assert_eq!(captured.latest_results.len(), 1);
        assert!(!captured.nonopen.is_exhaustive);
        let report = budget.report().unwrap();
        assert_eq!(report.physical_records.allocations, 128);
        assert_eq!(
            report.categories[&InputMemoryCategory::History].allocations,
            128
        );
        assert_eq!(report.reclaimable_history.allocations, 127);
        drop(resident);
        let report = budget.report().unwrap();
        assert_eq!(report.physical_records.allocations, 1);
        assert!(!report
            .categories
            .contains_key(&InputMemoryCategory::History));
        assert_eq!(report.categories[&InputMemoryCategory::Pins].allocations, 2);
        drop(captured);
        assert_eq!(budget.report().unwrap().observed_bytes, 0);
    }
}

#[cfg(test)]
#[path = "resident_work_turn_wait_tests.rs"]
mod turn_wait_tests;

fn task_wait_matches(wait: &WaitConditionRecord, task_id: &str) -> bool {
    wait.wake_sources.iter().any(|source| {
        matches!(source,
        crate::types::WakeSource::TaskResult { task_id: id } if id == task_id)
    })
}

#[cfg(test)]
#[path = "resident_work_task_wait_tests.rs"]
mod task_wait_tests;
