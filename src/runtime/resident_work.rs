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

#[path = "resident_work/material.rs"]
mod material;
pub(crate) use material::{OriginalWorkHostAddress, OriginalWorkHostAddressLoan};
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

    /// Conservative native footprint of this immutable typed allocation.
    /// It is deterministic across decode/clone and does not allocate a JSON DTO.
    pub(crate) fn resident_material_estimate(&self) -> Result<(usize, usize)> {
        fn value_nodes(value: &serde_json::Value, depth: usize) -> Result<usize> {
            ensure!(
                depth < 128,
                "Work native JSON depth exceeds its decoder bound"
            );
            let children = match value {
                serde_json::Value::Array(values) => {
                    values.iter().try_fold(0usize, |n, value| {
                        n.checked_add(value_nodes(value, depth + 1)?)
                            .ok_or_else(|| anyhow::anyhow!("Work native nodes overflow"))
                    })?
                }
                serde_json::Value::Object(values) => {
                    values.values().try_fold(values.len(), |n, value| {
                        n.checked_add(value_nodes(value, depth + 1)?)
                            .ok_or_else(|| anyhow::anyhow!("Work native nodes overflow"))
                    })?
                }
                _ => 0,
            };
            children
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Work native nodes overflow"))
        }
        let overflow = || anyhow::anyhow!("Work resident material overflow");
        let (slots, nodes, collection_entries) = match self {
            Self::Work(work) => {
                let slots = std::mem::size_of_val(work.todo_list.as_slice())
                    .checked_add(std::mem::size_of_val(work.work_refs.as_slice()))
                    .ok_or_else(overflow)?;
                let nodes = work.work_refs.iter().try_fold(0usize, |n, reference| {
                    reference.metadata.values().try_fold(
                        n.checked_add(reference.metadata.len())
                            .ok_or_else(overflow)?,
                        |n, value| n.checked_add(value_nodes(value, 0)?).ok_or_else(overflow),
                    )
                })?;
                let entries = work
                    .todo_list
                    .len()
                    .checked_add(work.work_refs.len())
                    .ok_or_else(overflow)?;
                (slots, nodes, entries)
            }
            Self::Wait(wait) => (
                std::mem::size_of_val(wait.wake_sources.as_slice()),
                wait.continuation
                    .as_ref()
                    .map(|value| value_nodes(value, 0))
                    .transpose()?
                    .unwrap_or_default(),
                wait.wake_sources.len(),
            ),
            Self::Frame(_) => (0, 0, 0),
        };
        // Serde Vec decode/clone capacity is bounded by two live element slots;
        // native JSON object/array nodes get their own container allowance.
        let address_bytes = self
            .agent_id()
            .len()
            .checked_add(self.record_id().len())
            .and_then(|n| n.checked_add(64))
            .ok_or_else(overflow)?;
        let native = slots
            .checked_mul(2)
            .and_then(|n| {
                nodes
                    .checked_mul(256)
                    .and_then(|nodes| n.checked_add(nodes))
            })
            .and_then(|n| n.checked_add(std::mem::size_of::<QualifiedWorkRecord>()))
            .and_then(|n| n.checked_add(address_bytes))
            .ok_or_else(overflow)?;
        let bytes = usize::try_from(serialized_bytes(self)?)?
            .checked_add(native)
            .ok_or_else(overflow)?;
        let records = nodes
            .checked_add(collection_entries)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(overflow)?;
        Ok((bytes, records))
    }
    pub(crate) fn borrowed_references(&self) -> [Option<(WorkFamily, &str)>; 2] {
        match self {
            Self::Work(work) => [
                work.blocked_by_wait_id
                    .as_deref()
                    .map(|id| (WorkFamily::Wait, id)),
                None,
            ],
            Self::Wait(wait) => [
                wait.work_item_id
                    .as_deref()
                    .map(|id| (WorkFamily::Work, id)),
                None,
            ],
            Self::Frame(frame) => [
                Some((WorkFamily::Work, &frame.suspended_work_item_id)),
                Some((WorkFamily::Work, &frame.active_work_item_id)),
            ],
        }
    }
    fn family(&self) -> WorkFamily {
        match self {
            Self::Work(_) => WorkFamily::Work,
            Self::Wait(_) => WorkFamily::Wait,
            Self::Frame(_) => WorkFamily::Frame,
        }
    }
    fn record_id(&self) -> &str {
        match self {
            Self::Work(x) => &x.id,
            Self::Wait(x) => &x.id,
            Self::Frame(x) => &x.id,
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
                && self.record.family() == self.key.family
                && self.record.record_id() == self.key.id,
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
            self.resident_bytes >= usize::try_from(serialized_bytes(&self.record)?)?,
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

/// A selected Work inherits the capture's exact scope; these metadata refs
/// carry no body authority and preserve two independent legacy selectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "presence", rename_all = "snake_case")]
pub(crate) enum WorkDeliveryHead {
    Present {
        record_id: String,
        content_digest: String,
        source_revision: u64,
        availability: super::resident_input_evidence::LegacyDeliverySourceAvailability,
    },
    Absent,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkDeliveryHeads {
    pub latest_any: WorkDeliveryHead,
    pub latest_nonempty: WorkDeliveryHead,
}
impl WorkDeliveryHead {
    pub(crate) fn body_id(&self) -> Option<&str> {
        match self {
            Self::Present {
                record_id,
                availability:
                    super::resident_input_evidence::LegacyDeliverySourceAvailability::Present,
                ..
            } => Some(record_id),
            _ => None,
        }
    }
    fn validate(&self) -> Result<()> {
        if let Self::Present { content_digest, .. } = self {
            ensure!(
                content_digest.len() == 64
                    && content_digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "Work Delivery head address/digest is invalid"
            );
        }
        Ok(())
    }
    pub(crate) fn metadata_records(&self) -> usize {
        if matches!(self, Self::Present { .. }) {
            16
        } else {
            1
        }
    }
    pub(crate) fn from_observation(
        scope: &InputEvidenceScope,
        observation: super::resident_input_evidence::LegacyDeliveryHeadObservation,
    ) -> Result<Self> {
        use super::resident_input_evidence::{InputEvidenceFamily, LegacyDeliveryHeadObservation};
        Ok(match observation {
            LegacyDeliveryHeadObservation::Present { source } => {
                ensure!(
                    &source.scope == scope
                        && source.key.family == InputEvidenceFamily::LegacyDelivery,
                    "Work Delivery head Source crossed its actual scope/family"
                );
                Self::Present {
                    record_id: source.key.id,
                    content_digest: source.content_digest,
                    source_revision: source.source_revision,
                    availability: source.availability,
                }
            }
            LegacyDeliveryHeadObservation::Absent => Self::Absent,
            LegacyDeliveryHeadObservation::Unavailable => Self::Unavailable,
        })
    }
}
impl WorkDeliveryHeads {
    pub(crate) fn metadata_records(&self) -> Result<usize> {
        self.latest_any
            .metadata_records()
            .checked_add(self.latest_nonempty.metadata_records())
            .and_then(|n| n.checked_add(1))
            .context("Work Delivery head metadata overflow")
    }
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
    /// Missing entry is unprepared; Absent is a positive original Source fact.
    #[serde(default)]
    pub legacy_delivery_heads: BTreeMap<String, WorkDeliveryHeads>,
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
    /// Temporary actual readback facts survive original Known/Unknown clones.
    pub(crate) _material_views: Option<Arc<Vec<Arc<BudgetViewGuard>>>>,
}

#[derive(Debug, Clone)]
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
    pub legacy_delivery_heads: BTreeMap<String, WorkDeliveryHeads>,
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
        self.refresh_budget_views(None)?;
        if let Some(budget) = &self.budget {
            budget.check_history_budget()?;
        }
        Ok(self)
    }
    fn refresh_budget_views(&mut self, prepaid: Option<Arc<BudgetViewGuard>>) -> Result<()> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        let metadata = match prepaid {
            Some(guard) => Some(guard),
            None => material::coverage_preflight(
                self,
                &self.coverage,
                self.records.len(),
                InputMemoryCategory::Current,
            )?,
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
            metadata.expect("budgeted Work metadata has its original owned guard"),
        ];
        self.budget_views = views;
        Ok(())
    }
    pub(crate) fn candidate_budget_views(&self) -> Result<Vec<Arc<BudgetViewGuard>>> {
        let Some(budget) = self.budget.as_ref() else {
            return Ok(Vec::new());
        };
        let metadata = material::candidate_preflight(self)?
            .expect("budgeted Work candidate has its original owned guard");
        let body = budget.register_work_view(
            &self.scope,
            InputMemoryCategory::Candidate,
            self.records.values(),
        )?;
        Ok(vec![body, metadata])
    }
    /// Borrow only an already-qualified current observation; no selectors or SQL.
    pub(crate) fn qualified_context_plan_source(
        &self,
        id: &str,
    ) -> Option<&Arc<QualifiedWorkRecord>> {
        self.records.iter().find_map(|(key, qualified)| {
            (key.family == WorkFamily::Work
                && key.id == id
                && self.coverage.observations.get(key) == Some(&WorkAvailability::Present)
                && matches!(&qualified.record, CanonicalWorkRecord::Work(_)))
            .then_some(qualified)
        })
    }
    /// Borrow only original qualified Present members; no history acquisition.
    pub(crate) fn context_plan_sources(&self) -> impl Iterator<Item = &WorkItemRecord> {
        self.records.iter().filter_map(move |(key, qualified)| {
            if self.coverage.observations.get(key) != Some(&WorkAvailability::Present) {
                return None;
            }
            match &qualified.record {
                CanonicalWorkRecord::Work(item) => Some(item),
                _ => None,
            }
        })
    }
    pub(crate) fn borrowed_records(&self) -> impl Iterator<Item = &Arc<QualifiedWorkRecord>> {
        self.records.values()
    }
    pub(in crate::runtime) fn validate_capture_material_binding(
        &self,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.budget
                .as_ref()
                .is_some_and(|actual| actual.same_ledger(ledger))
                && !self.budget_views.is_empty(),
            "scheduler Work material ledger differs"
        );
        for guard in &self.budget_views {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        Ok(())
    }
    pub(in crate::runtime) fn acquisition_registration_count(&self) -> Result<usize> {
        Ok(self.records.len())
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
        let count = self
            .records
            .len()
            .checked_add(delta.records.len())
            .ok_or_else(|| anyhow::anyhow!("Work candidate metadata overflow"))?;
        let metadata = material::coverage_preflight(
            self,
            &delta.coverage,
            count,
            InputMemoryCategory::Current,
        )?;
        let _candidate_body_view = self
            .budget
            .as_ref()
            .map(|budget| {
                budget.register_work_view(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    self.records.values().chain(delta.records.iter()),
                )
            })
            .transpose()?;
        let mut next = Self {
            scope: self.scope.clone(),
            coverage: delta.coverage.clone(),
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
        next.trim_and_validate()?;
        next.refresh_budget_views(metadata)?;
        if let Some(budget) = &next.budget {
            budget.check_history_budget()?;
        }
        Ok(next)
    }
    pub(crate) fn capture(&self, request: &WorkCaptureRequest) -> Result<CapturedWork> {
        self.capture_accounted(request, self.budget.as_ref(), None)
    }
    // All allocations below have already been admitted by selected-only
    // borrowed preflight; this keeps the original business projection intact.
    fn capture_prepaid(&self, request: &WorkCaptureRequest) -> Result<CapturedWork> {
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
        let legacy_delivery_heads = work_ids
            .iter()
            .filter_map(|id| {
                self.coverage
                    .legacy_delivery_heads
                    .get(id)
                    .map(|heads| (id.clone(), heads.clone()))
            })
            .collect();
        let captured = CapturedWork {
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
            legacy_delivery_heads,
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
                    for (family, id) in record.record.borrowed_references().into_iter().flatten() {
                        pending.insert(WorkKey::new(family, id));
                    }
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
                    for (family, id) in record.record.borrowed_references().into_iter().flatten() {
                        let dependency = WorkKey::new(family, id);
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
        for (id, heads) in &self.coverage.legacy_delivery_heads {
            ensure!(
                self.coverage
                    .observations
                    .get(&WorkKey::new(WorkFamily::Work, id))
                    == Some(&WorkAvailability::Present),
                "Work Delivery head has no selected Present Work"
            );
            heads.latest_any.validate()?;
            heads.latest_nonempty.validate()?;
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
        self.coverage
            .legacy_delivery_heads
            .retain(|id, _| selected.contains(&WorkKey::new(WorkFamily::Work, id)));
        let bytes = self.records.values().try_fold(
            usize::try_from(serialized_bytes(&self.coverage)?)?,
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
    pub(crate) fn delivery_heads(&self, id: &str) -> Result<&WorkDeliveryHeads> {
        self.legacy_delivery_heads.get(id).ok_or_else(|| {
            WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Work, id)).into()
        })
    }
    /// Exact encoded metadata footprint through borrowed observations.
    pub(crate) fn encoded_selected_metadata_bytes(&self) -> Result<u64> {
        Ok(serialized_bytes(&(
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
            &self.legacy_delivery_heads,
            work_observations::Entries(&self.observations),
        ))?)
    }
    /// Native metadata entries copied by this selected carrier, without bodies.
    pub(crate) fn selected_metadata_record_count(&self) -> Result<usize> {
        let mut count = 1usize;
        for size in [
            self.records.len(),
            self.live_work_ids.len(),
            self.unresolved_wait_ids.len(),
            self.active_frame_ids.len(),
            self.nonopen.keys.len(),
            self.observations.len(),
            self.latest_results.len(),
            self.legacy_delivery_heads.len(),
            self.turn_wait_keys.len(),
            self.task_wait_keys.len(),
            self.trigger_wait_keys.len(),
        ] {
            count = count
                .checked_add(size)
                .ok_or_else(|| anyhow::anyhow!("selected Work metadata count overflow"))?;
        }
        for ids in self
            .turn_wait_keys
            .values()
            .chain(self.task_wait_keys.values())
            .chain(self.trigger_wait_keys.values())
        {
            count = count
                .checked_add(ids.len())
                .ok_or_else(|| anyhow::anyhow!("selected Work relation count overflow"))?;
        }
        for heads in self.legacy_delivery_heads.values() {
            count = count
                .checked_add(heads.metadata_records()?)
                .context("selected Work Delivery metadata overflow")?;
        }
        count
            .checked_add(
                self.latest_results
                    .values()
                    .filter(|value| value.is_some())
                    .count(),
            )
            .ok_or_else(|| anyhow::anyhow!("selected Work result metadata count overflow"))
    }
    /// No lookup, copying, ledger registration or new membership authority.
    pub(crate) fn selected_material_estimate(&self) -> Result<(u64, usize)> {
        let metadata = self.selected_metadata_record_count()?;
        let mut bytes = self
            .encoded_selected_metadata_bytes()?
            .checked_add(
                u64::try_from(metadata)?
                    .checked_mul(128)
                    .ok_or_else(|| anyhow::anyhow!("selected Work metadata overflow"))?,
            )
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>() as u64))
            .ok_or_else(|| anyhow::anyhow!("selected Work material overflow"))?;
        let mut records = metadata;
        for record in &self.records {
            let (body, count) = record.record.resident_material_estimate()?;
            bytes = bytes
                .checked_add(u64::try_from(body)?)
                .ok_or_else(|| anyhow::anyhow!("selected Work bodies overflow"))?;
            records = records
                .checked_add(count)
                .ok_or_else(|| anyhow::anyhow!("selected Work records overflow"))?;
        }
        Ok((bytes, records))
    }
    pub(crate) fn selected_metadata_estimate_bytes(&self) -> Result<u64> {
        let metadata = self.encoded_selected_metadata_bytes()?;
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
        match self.observations.get(key) {
            Some(WorkAvailability::Present) => self
                .records
                .iter()
                .find(|x| x.key == *key)
                .map(Some)
                .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()).into()),
            Some(WorkAvailability::Absent | WorkAvailability::Pruned) => Ok(None),
            None => Err(WorkReadError::NeedsPreparation(key.clone()).into()),
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
        self.latest_results
            .get(work_item_id)
            .map(Option::as_ref)
            .ok_or_else(|| {
                WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Work, work_item_id)).into()
            })
    }
    /// The trigger delivery observation remains an explicit D6 adapter; no SQL
    /// or Task/Timer/Trigger authority is hidden inside this pure projection.
    pub(crate) fn work_queue(
        &self,
        state: &AgentState,
        triggers: Vec<ExternalTriggerRecord>,
    ) -> Result<crate::work_item_scheduling::WorkQueueReadModel> {
        self.work_queue_observed(state, triggers, &mut None)
    }
    pub(in crate::runtime) fn work_queue_observed(
        &self,
        state: &AgentState,
        triggers: Vec<ExternalTriggerRecord>,
        observer: &mut Option<&mut dyn FnMut(u64, usize) -> Result<()>>,
    ) -> Result<crate::work_item_scheduling::WorkQueueReadModel> {
        use crate::work_item_scheduling::{
            observe_scheduling_clone as observe, observe_scheduling_slots as slots,
        };
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
        for evidence in &self.records {
            if let CanonicalWorkRecord::Work(work) = &evidence.record {
                if work.state == WorkItemState::Open {
                    observe(&work.id, observer)?;
                }
            }
        }
        for key in &self.nonopen.keys {
            observe(&key.id, observer)?;
        }
        let displayed_work_ids = self
            .records
            .iter()
            .filter_map(|evidence| match &evidence.record {
                CanonicalWorkRecord::Work(work) if work.state == WorkItemState::Open => {
                    Some(work.id.clone())
                }
                _ => None,
            })
            .chain(self.nonopen.keys.iter().map(|key| key.id.clone()))
            .collect::<BTreeSet<_>>();
        let mut works = std::collections::HashMap::new();
        let mut waits = Vec::new();
        let mut frames = Vec::new();
        for evidence in &self.records {
            match &evidence.record {
                CanonicalWorkRecord::Work(x) if displayed_work_ids.contains(&x.id) => {
                    observe(&x.id, observer)?;
                    observe(x, observer)?;
                    slots(2, observer)?;
                    works.insert(x.id.clone(), x.clone());
                }
                CanonicalWorkRecord::Wait(x)
                    if self.unresolved_wait_ids.contains(&x.id)
                        && x.status == WaitConditionStatus::Active =>
                {
                    observe(x, observer)?;
                    waits.push(x.clone())
                }
                CanonicalWorkRecord::Frame(x) if self.active_frame_ids.contains(&x.id) => {
                    observe(x, observer)?;
                    frames.push(x.clone())
                }
                _ => {}
            }
        }
        // Triggered waits remain resident obligations, but the existing queue
        // reader projects only Active waits owned by an Open WorkItem.
        let waits = waits
            .into_iter()
            .filter(|record| {
                record.work_item_id.as_ref().is_none_or(|id| {
                    works
                        .get(id)
                        .is_some_and(|work| work.state == WorkItemState::Open)
                })
            })
            .collect();
        observe(&state.current_work_item_id, observer)?;
        crate::storage::derive_context_work_queue_observed(
            state.current_work_item_id.clone(),
            works,
            triggers,
            waits,
            frames,
            observer,
        )
    }
}

// JSON object keys cannot represent the typed family/id pair. An ordered pair
// list preserves its exact shape and rejects duplicate keys on durable decode.
mod work_observations {
    use super::{BTreeMap, WorkAvailability, WorkKey};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) struct Entries<'a>(pub &'a BTreeMap<WorkKey, WorkAvailability>);
    impl Serialize for Entries<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serialize(self.0, serializer)
        }
    }

    pub(super) fn serialize<S: Serializer>(
        values: &BTreeMap<WorkKey, WorkAvailability>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(values.iter())
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

    #[test]
    fn borrowed_work_observations_preserve_the_original_durable_pair_shape() -> Result<()> {
        let observations = BTreeMap::from([
            (
                WorkKey::new(WorkFamily::Work, "work-first"),
                WorkAvailability::Present,
            ),
            (
                WorkKey::new(WorkFamily::Wait, "wait-second"),
                WorkAvailability::Pruned,
            ),
        ]);
        let original = serde_json::to_vec(&observations.iter().collect::<Vec<_>>())?;
        let borrowed = work_observations::Entries(&observations);
        assert_eq!(serde_json::to_vec(&borrowed)?, original);
        assert_eq!(serialized_bytes(&borrowed)?, u64::try_from(original.len())?);
        Ok(())
    }

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
            legacy_delivery_heads: BTreeMap::new(),
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
        ResidentWorkDomain::bootstrap(scope(), coverage, records, 65_536, 4 * 1024 * 1024).unwrap()
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
            _material_views: None,
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
                        _material_views: None,
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
                state.current_work_item_id.clone(),
                [(work.id.clone(), work.clone())].into_iter().collect(),
                vec![],
                legacy_waits,
                vec![],
            );
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

    #[test]
    fn selected_material_rejects_before_record_arc_clone_and_restores_ledger() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let record = qualified("bounded", WorkItemState::Open, 1);
        let mut resident = domain(vec![record.clone()], coverage(&[record.clone()], 1))
            .with_budget(ledger.clone())?;
        let baseline = ledger.report()?.observed_bytes;
        let strong = Arc::strong_count(&record);
        resident.byte_limit = 4096;
        let error = resident
            .capture(&WorkCaptureRequest::current())
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::ResourceExhausted(_))
        ));
        assert_eq!(Arc::strong_count(&record), strong);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop(resident);
        drop(record);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn narrow_capture_does_not_charge_unselected_large_completion_metadata() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let a = qualified("a", WorkItemState::Completed, 1);
        let b = qualified("b", WorkItemState::Completed, 1);
        let mut prepared = coverage(&[a.clone(), b.clone()], 1);
        prepared.latest_results.insert(
            "b".into(),
            Some(WorkCompletionRef {
                brief_id: "unselected".repeat(8192),
                transcript_entry_id: None,
                source_message_id: None,
            }),
        );
        let mut resident = domain(vec![a, b], prepared).with_budget(ledger.clone())?;
        resident.byte_limit = 32 * 1024;
        let request = WorkCaptureRequest {
            include_live: false,
            nonopen_limit: 1,
            mandatory_roots: Default::default(),
            latest_result_work_ids: Default::default(),
            required_turn_ids: Default::default(),
            required_task_ids: Default::default(),
            required_trigger_message_ids: Default::default(),
        };
        let selected = resident.capture(&request)?;
        assert_eq!(selected.records.len(), 1);
        assert_eq!(selected.records[0].key.id, "a");
        assert!(!selected.latest_results.contains_key("b"));
        assert!(resident.capture(&WorkCaptureRequest::current()).is_err());
        drop(resident);
        drop(selected);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn actual_observer_runs_before_any_selected_body_arc_is_cloned() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let record = qualified("actual-observer", WorkItemState::Open, 1);
        let resident = domain(vec![record.clone()], coverage(&[record.clone()], 1));
        let strong = Arc::strong_count(&record);
        let mut calls = 0;
        let mut observer =
            |owned: &Arc<BudgetViewGuard>,
             bytes: usize,
             _: usize,
             origin: Option<&OriginalWorkHostAddress<'_>>| {
                assert!(owned.matches_resource_ledger(&ledger));
                assert!(owned.owned_material_bytes()? >= u64::try_from(bytes)?);
                assert_eq!(Arc::strong_count(&record), strong);
                if calls == 0 {
                    assert!(
                        ledger.report()?.host_owned.metadata_estimate_bytes
                            >= u64::try_from(bytes)?
                    );
                }
                if calls == 0 {
                    origin
                        .context("first callback lost actual Host origin")?
                        .validate_binding(resident.scope(), &ledger, owned)?;
                } else {
                    assert!(origin.is_none());
                    owned.validate_owned_material_binding(resident.scope(), &ledger, 0)?;
                }
                calls += 1;
                Ok(())
            };
        let selected = resident.capture_with_material(
            &WorkCaptureRequest::current(),
            &ledger,
            &mut observer,
        )?;
        assert!(calls > 1);
        assert_eq!(Arc::strong_count(&record), strong + 1);
        let body = Arc::downgrade(&record);
        let guard = Arc::downgrade(&selected.budget_views[1]);
        drop(resident);
        drop(record);
        assert!(body.upgrade().is_some() && guard.upgrade().is_some());
        drop(selected);
        assert!(body.upgrade().is_none() && guard.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn candidate_view_accounts_its_owned_membership_without_copying_work_bodies() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let record = qualified("candidate-membership", WorkItemState::Open, 1);
        let resident = domain(vec![record.clone()], coverage(&[record.clone()], 1))
            .with_budget(ledger.clone())?;
        let baseline = ledger.report()?.observed_bytes;
        let strong = Arc::strong_count(&record);
        let views = resident.candidate_budget_views()?;
        assert_eq!(views.len(), 2);
        assert_eq!(Arc::strong_count(&record), strong);
        let report = ledger.report()?;
        assert_eq!(report.physical_records.allocations, 1);
        assert!(report.observed_bytes > baseline);
        let owned = Arc::downgrade(&views[1]);
        let body = Arc::downgrade(&record);
        drop(resident);
        drop(record);
        assert!(body.upgrade().is_none(), "candidate membership is weak");
        assert!(owned.upgrade().is_some());
        drop(views);
        assert!(owned.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn candidate_preflights_old_long_map_keys_after_focused_role_is_released() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let a = qualified("a", WorkItemState::Completed, 1);
        let b = qualified(&"long".repeat(8192), WorkItemState::Completed, 1);
        let mut initial = coverage(&[a.clone(), b.clone()], 1);
        initial.nonopen.keys.truncate(1);
        initial.nonopen.limit = 1;
        initial.nonopen.is_exhaustive = false;
        initial.mandatory_roots.insert(b.key.clone());
        let mut resident =
            domain(vec![a.clone(), b.clone()], initial).with_budget(ledger.clone())?;
        let baseline = ledger.report()?.observed_bytes;
        resident.byte_limit = 16 * 1024;
        let mut final_coverage = coverage(&[a.clone()], 2);
        final_coverage.nonopen.limit = 1;
        final_coverage.nonopen.is_exhaustive = false;
        let delta = CommittedWorkDelta {
            scope: scope(),
            base_revision: 1,
            revision: 2,
            records: vec![],
            removals: Default::default(),
            coverage: final_coverage,
            _material_views: None,
        };
        // B is still qualified durable history; only its focused role is gone.
        // Final coverage has no B string, but cloning the old map would copy it.
        assert!(!delta.coverage.observations.contains_key(&b.key));
        let strong = Arc::strong_count(&b);
        let error = resident.with_committed(&delta).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::ResourceExhausted(_))
        ));
        assert_eq!(Arc::strong_count(&b), strong);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop(delta);
        drop(resident);
        drop(a);
        drop(b);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
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
