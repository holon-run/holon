//! Timer, pending wake and ingress facts selected from one owner publication.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
};
use crate::types::{ExternalTriggerRecord, TimerRecord};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[path = "scheduling_capture_material.rs"]
mod capture_material;
pub(crate) use capture_material::SchedulingCaptureMaterial;
use capture_material::SchedulingCapturePhase;

pub(crate) const SCHEDULING_HISTORY_WINDOW: usize = 128;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SchedulingFamily {
    Timer,
    TimerWake,
    ExternalTrigger,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct SchedulingKey {
    pub family: SchedulingFamily,
    pub id: String,
}
impl SchedulingKey {
    pub(crate) fn new(family: SchedulingFamily, id: impl Into<String>) -> Self {
        Self {
            family,
            id: id.into(),
        }
    }
    pub(crate) fn wake(timer_id: &str, message_id: &str) -> Self {
        Self::new(
            SchedulingFamily::TimerWake,
            serde_json::to_string(&(timer_id, message_id)).expect("string tuple serializes"),
        )
    }
    pub(crate) fn wake_pair(&self) -> Result<(String, String)> {
        ensure!(self.family == SchedulingFamily::TimerWake, "not a wake key");
        let pair: (String, String) = serde_json::from_str(&self.id)?;
        ensure!(
            !pair.0.is_empty() && !pair.1.is_empty() && Self::wake(&pair.0, &pair.1) == *self,
            "noncanonical wake key"
        );
        Ok(pair)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SchedulingWakeStatus {
    Pending,
    Incorporated,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulingWakeFact {
    pub agent_id: String,
    pub timer_id: String,
    pub message_id: String,
    pub fire_count: u64,
    pub status: SchedulingWakeStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub incorporated_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", content = "record", rename_all = "snake_case")]
pub(crate) enum CanonicalSchedulingFact {
    Timer(TimerRecord),
    TimerWake(SchedulingWakeFact),
    ExternalTrigger(ExternalTriggerRecord),
}
impl CanonicalSchedulingFact {
    pub(crate) fn key(&self) -> SchedulingKey {
        match self {
            Self::Timer(x) => SchedulingKey::new(SchedulingFamily::Timer, &x.id),
            Self::TimerWake(x) => SchedulingKey::wake(&x.timer_id, &x.message_id),
            Self::ExternalTrigger(x) => {
                SchedulingKey::new(SchedulingFamily::ExternalTrigger, &x.external_trigger_id)
            }
        }
    }
    pub(crate) fn agent_id(&self) -> &str {
        match self {
            Self::Timer(x) => &x.agent_id,
            Self::TimerWake(x) => &x.agent_id,
            Self::ExternalTrigger(x) => &x.target_agent_id,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulingLocator {
    pub scope: InputEvidenceScope,
    pub key: SchedulingKey,
    pub revision: u64,
    pub content_digest: String,
    pub projection_digest: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QualifiedSchedulingFact {
    pub locator: SchedulingLocator,
    pub fact: CanonicalSchedulingFact,
    pub resident_bytes: usize,
}
impl QualifiedSchedulingFact {
    pub(crate) fn validate(&self, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.locator.scope == *scope
                && self.fact.agent_id() == scope.agent_id
                && self.locator.key == self.fact.key()
                && self.locator.revision > 0,
            "Scheduling scope/key/revision differs"
        );
        ensure!(
            [
                &self.locator.content_digest,
                &self.locator.projection_digest
            ]
            .into_iter()
            .all(|s| s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
            "invalid Scheduling digest"
        );
        ensure!(
            self.resident_bytes >= usize::try_from(serialized_bytes(&self.fact)?)?,
            "Scheduling body estimate incomplete"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SchedulingAvailability {
    Present,
    Absent,
    Pruned,
    HistoricalUnavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulingObservation {
    pub key: SchedulingKey,
    pub availability: SchedulingAvailability,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulingHistory {
    pub keys: Vec<SchedulingKey>,
    pub loaded_keys: Vec<SchedulingKey>,
    pub is_exhaustive: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulingCoverage {
    pub revision: u64,
    pub active_timers_complete: bool,
    pub pending_wakes_complete: bool,
    pub active_triggers_complete: bool,
    pub active_timers: BTreeSet<SchedulingKey>,
    pub pending_wakes: BTreeSet<SchedulingKey>,
    pub active_triggers: BTreeSet<SchedulingKey>,
    pub history: BTreeMap<SchedulingFamily, SchedulingHistory>,
    pub required_keys: BTreeSet<SchedulingKey>,
    pub observations: Vec<SchedulingObservation>,
}
impl SchedulingCoverage {
    pub(crate) fn empty(revision: u64) -> Self {
        Self {
            revision,
            active_timers_complete: true,
            pending_wakes_complete: true,
            active_triggers_complete: true,
            active_timers: BTreeSet::new(),
            pending_wakes: BTreeSet::new(),
            active_triggers: BTreeSet::new(),
            history: BTreeMap::new(),
            required_keys: BTreeSet::new(),
            observations: Vec::new(),
        }
    }
    fn availability(&self, key: &SchedulingKey) -> Option<SchedulingAvailability> {
        self.observations
            .iter()
            .find(|o| &o.key == key)
            .map(|o| o.availability)
    }
    fn roots(&self, request: &SchedulingCaptureRequest) -> Result<BTreeSet<SchedulingKey>> {
        self.roots_observed(request, &mut SchedulingCapturePhase::unobserved())
    }
    fn roots_observed(
        &self,
        request: &SchedulingCaptureRequest,
        phase: &mut SchedulingCapturePhase<'_>,
    ) -> Result<BTreeSet<SchedulingKey>> {
        ensure!(
            request.history_limit <= SCHEDULING_HISTORY_WINDOW,
            "Scheduling history request exceeds policy"
        );
        phase.json(&request.required_keys, 1)?;
        let mut keys = request.required_keys.clone();
        if request.include_live {
            ensure!(
                self.active_timers_complete
                    && self.pending_wakes_complete
                    && self.active_triggers_complete,
                SchedulingReadError::IncompleteLive
            );
            for key in self
                .active_timers
                .iter()
                .chain(&self.pending_wakes)
                .chain(&self.active_triggers)
            {
                phase.key(key, 1)?;
                keys.insert(key.clone());
            }
        }
        for family in [
            SchedulingFamily::Timer,
            SchedulingFamily::TimerWake,
            SchedulingFamily::ExternalTrigger,
        ] {
            let history = self
                .history
                .get(&family)
                .context(SchedulingReadError::NeedsHistory)?;
            ensure!(
                (history.keys.len() >= request.history_limit
                    && history.loaded_keys.len() >= request.history_limit)
                    || history.is_exhaustive,
                SchedulingReadError::NeedsHistory
            );
            ensure!(
                history.loaded_keys.len() >= history.keys.len().min(request.history_limit),
                SchedulingReadError::NeedsHistory
            );
            for key in history.keys.iter().take(request.history_limit) {
                phase.key(key, 1)?;
                keys.insert(key.clone());
            }
            for key in history.loaded_keys.iter().take(request.history_limit) {
                phase.key(key, 1)?;
                keys.insert(key.clone());
            }
        }
        Ok(keys)
    }
    fn live_keys(&self) -> BTreeSet<SchedulingKey> {
        self.active_timers
            .iter()
            .chain(&self.pending_wakes)
            .chain(&self.active_triggers)
            .cloned()
            .collect()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulingCaptureRequest {
    pub include_live: bool,
    pub history_limit: usize,
    pub required_keys: BTreeSet<SchedulingKey>,
}
impl SchedulingCaptureRequest {
    pub(crate) fn current() -> Self {
        Self {
            include_live: true,
            history_limit: SCHEDULING_HISTORY_WINDOW,
            required_keys: BTreeSet::new(),
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum SchedulingReadError {
    #[error("Scheduling live coverage requires preparation")]
    IncompleteLive,
    #[error("Scheduling history window requires preparation")]
    NeedsHistory,
    #[error("Scheduling key requires preparation: {0:?}")]
    NeedsPreparation(SchedulingKey),
    #[error("Scheduling key was pruned: {0:?}")]
    Pruned(SchedulingKey),
    #[error("Scheduling historical table was unavailable: {0:?}")]
    HistoricalUnavailable(SchedulingKey),
    #[error("Scheduling revision changed")]
    RevisionMismatch,
    #[error("Scheduling ResourceExhausted: {0}")]
    ResourceExhausted(&'static str),
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommittedSchedulingDelta {
    pub scope: InputEvidenceScope,
    pub base_revision: u64,
    pub revision: u64,
    pub coverage: SchedulingCoverage,
    pub records: Vec<Arc<QualifiedSchedulingFact>>,
}
#[derive(Debug)]
pub(crate) struct ResidentSchedulingDomain {
    scope: InputEvidenceScope,
    coverage: SchedulingCoverage,
    records: BTreeMap<SchedulingKey, Arc<QualifiedSchedulingFact>>,
    record_limit: usize,
    byte_limit: usize,
    budget: Option<InputEvidenceBudgetLedger>,
    views: Vec<Arc<BudgetViewGuard>>,
}
#[derive(Debug)]
pub(crate) struct CapturedScheduling {
    pub scope: InputEvidenceScope,
    pub revision: u64,
    pub coverage: SchedulingCoverage,
    pub records: Vec<Arc<QualifiedSchedulingFact>>,
    _views: Vec<Arc<BudgetViewGuard>>,
}
impl CapturedScheduling {
    pub(crate) fn validate_material_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(self.scope == *scope, "selected Scheduling scope differs");
        for guard in &self._views {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok(())
    }
}

impl ResidentSchedulingDomain {
    pub(crate) fn bootstrap(
        scope: InputEvidenceScope,
        coverage: SchedulingCoverage,
        records: Vec<Arc<QualifiedSchedulingFact>>,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Self> {
        ensure!(
            !scope.agent_id.is_empty()
                && scope.identity_incarnation > 0
                && coverage.revision > 0
                && record_limit > 0
                && byte_limit > 0,
            "invalid Scheduling bootstrap"
        );
        let mut map = BTreeMap::new();
        for record in records {
            record.validate(&scope)?;
            ensure!(
                record.locator.revision <= coverage.revision,
                "Scheduling fact revision exceeds its domain"
            );
            ensure!(
                map.insert(record.locator.key.clone(), record).is_none(),
                "duplicate Scheduling body"
            );
        }
        let domain = Self {
            scope,
            coverage,
            records: map,
            record_limit,
            byte_limit,
            budget: None,
            views: Vec::new(),
        };
        domain.validate()?;
        Ok(domain)
    }
    pub(in crate::runtime) fn validate_capture_material_binding(
        &self,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.budget
                .as_ref()
                .is_some_and(|actual| actual.same_ledger(ledger))
                && !self.views.is_empty(),
            "scheduler Scheduling material ledger differs"
        );
        for guard in &self.views {
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
    pub(crate) fn coverage(&self) -> &SchedulingCoverage {
        &self.coverage
    }
    pub(crate) fn selected_install_records(&self) -> Vec<Arc<QualifiedSchedulingFact>> {
        self.records.values().cloned().collect()
    }
    pub(crate) fn with_budget(mut self, budget: InputEvidenceBudgetLedger) -> Result<Self> {
        self.budget = Some(budget);
        self.refresh_views()?;
        self.budget.as_ref().unwrap().check_history_budget()?;
        Ok(self)
    }
    pub(crate) fn candidate_budget_view(&self) -> Result<Option<Arc<BudgetViewGuard>>> {
        self.budget
            .as_ref()
            .map(|b| {
                b.register_scheduling_view(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    self.records.values(),
                )
                .map_err(anyhow::Error::from)
            })
            .transpose()
    }
    fn required_closure(&self, keys: BTreeSet<SchedulingKey>) -> Result<BTreeSet<SchedulingKey>> {
        self.required_closure_observed(keys, &mut SchedulingCapturePhase::unobserved())
    }
    fn required_closure_observed(
        &self,
        mut keys: BTreeSet<SchedulingKey>,
        phase: &mut SchedulingCapturePhase<'_>,
    ) -> Result<BTreeSet<SchedulingKey>> {
        let optional_pruned = |key: &SchedulingKey| {
            self.coverage.availability(key) == Some(SchedulingAvailability::Pruned)
                && !self.coverage.required_keys.contains(key)
                && !self.coverage.active_timers.contains(key)
                && !self.coverage.pending_wakes.contains(key)
                && !self.coverage.active_triggers.contains(key)
        };
        phase.json(&keys, 1)?;
        let initial: Vec<_> = keys.iter().cloned().collect();
        for key in initial {
            if optional_pruned(&key) {
                continue;
            }
            if let Some(record) = self.lookup_observed(&key, phase)? {
                if let CanonicalSchedulingFact::TimerWake(w) = &record.fact {
                    phase.key_parts(SchedulingFamily::Timer, &w.timer_id, 1)?;
                    keys.insert(SchedulingKey::new(SchedulingFamily::Timer, &w.timer_id));
                }
            }
        }
        for key in &keys {
            if optional_pruned(key) {
                continue;
            }
            self.lookup_observed(key, phase)?;
        }
        for record in self
            .records
            .values()
            .filter(|r| keys.contains(&r.locator.key))
        {
            if let CanonicalSchedulingFact::TimerWake(w) = &record.fact {
                phase.key_parts(SchedulingFamily::Timer, &w.timer_id, 1)?;
                let parent = self
                    .lookup_observed(
                        &SchedulingKey::new(SchedulingFamily::Timer, &w.timer_id),
                        phase,
                    )?
                    .context("Scheduling Wake has no qualified Timer body")?;
                ensure!(
                    matches!(&parent.fact,CanonicalSchedulingFact::Timer(t) if t.id==w.timer_id),
                    "Scheduling Wake parent family differs"
                );
            }
        }
        Ok(keys)
    }
    fn lookup(&self, key: &SchedulingKey) -> Result<Option<&Arc<QualifiedSchedulingFact>>> {
        self.lookup_observed(key, &mut SchedulingCapturePhase::unobserved())
    }
    fn lookup_observed(
        &self,
        key: &SchedulingKey,
        phase: &mut SchedulingCapturePhase<'_>,
    ) -> Result<Option<&Arc<QualifiedSchedulingFact>>> {
        match self.coverage.availability(key) {
            Some(SchedulingAvailability::Absent) => Ok(None),
            Some(SchedulingAvailability::Pruned) => {
                phase.key(key, 1)?;
                Err(SchedulingReadError::Pruned(key.clone()).into())
            }
            Some(SchedulingAvailability::HistoricalUnavailable) => {
                phase.key(key, 1)?;
                Err(SchedulingReadError::HistoricalUnavailable(key.clone()).into())
            }
            Some(SchedulingAvailability::Present) => match self.records.get(key) {
                Some(record) => {
                    phase.body(record)?;
                    Ok(Some(record))
                }
                None => {
                    phase.key(key, 1)?;
                    Err(SchedulingReadError::NeedsPreparation(key.clone()).into())
                }
            },
            None => {
                phase.key(key, 1)?;
                Err(SchedulingReadError::NeedsPreparation(key.clone()).into())
            }
        }
    }
    fn validate(&self) -> Result<()> {
        let mut unique = BTreeSet::new();
        for o in &self.coverage.observations {
            ensure!(unique.insert(&o.key), "duplicate Scheduling observation");
            ensure!(
                self.records.contains_key(&o.key)
                    == (o.availability == SchedulingAvailability::Present),
                "Scheduling availability/body mismatch"
            );
        }
        ensure!(
            self.records
                .keys()
                .all(|k| self.coverage.availability(k) == Some(SchedulingAvailability::Present)),
            "Scheduling body lacks selected observation"
        );
        let mut wake_timers = BTreeSet::new();
        for key in self.coverage.live_keys() {
            let record = self
                .lookup(&key)?
                .context("complete live Scheduling body missing")?;
            let valid = match &record.fact {
                CanonicalSchedulingFact::Timer(x) => {
                    self.coverage.active_timers.contains(&key)
                        && x.status == crate::types::TimerStatus::Active
                }
                CanonicalSchedulingFact::TimerWake(x) => {
                    self.coverage.pending_wakes.contains(&key)
                        && x.status == SchedulingWakeStatus::Pending
                        && wake_timers.insert(&x.timer_id)
                }
                CanonicalSchedulingFact::ExternalTrigger(x) => {
                    self.coverage.active_triggers.contains(&key)
                        && x.status == crate::types::ExternalTriggerStatus::Active
                }
            };
            ensure!(valid, "complete live Scheduling index/body mismatch");
        }
        ensure!(
            self.coverage.active_triggers.len() <= 1,
            "duplicate active Trigger default"
        );
        let roots = self.coverage.roots(&SchedulingCaptureRequest {
            include_live: true,
            history_limit: 0,
            required_keys: self.coverage.required_keys.clone(),
        })?;
        self.required_closure(roots)?;
        for (family, history) in &self.coverage.history {
            for key in history.keys.iter().chain(&history.loaded_keys) {
                ensure!(*family == key.family, "Scheduling history family differs");
                if self.coverage.availability(key) != Some(SchedulingAvailability::Pruned) {
                    self.lookup(key)?;
                }
            }
        }
        let bytes = self.records.values().try_fold(
            metadata_bytes(&self.scope, &self.coverage, self.records.len())?,
            |n, r| {
                n.checked_add(u64::try_from(r.resident_bytes)?)
                    .context("Scheduling byte overflow")
            },
        )?;
        ensure!(
            self.records
                .len()
                .checked_add(self.coverage.observations.len())
                .is_some_and(|n| n <= self.record_limit)
                && bytes <= u64::try_from(self.byte_limit)?,
            "Scheduling ResourceExhausted"
        );
        Ok(())
    }
    fn refresh_views(&mut self) -> Result<()> {
        let Some(b) = &self.budget else { return Ok(()) };
        let live = self.required_closure(self.coverage.live_keys())?;
        self.views = vec![
            b.register_scheduling_view(
                &self.scope,
                InputMemoryCategory::Current,
                self.records
                    .iter()
                    .filter(|(k, _)| live.contains(*k))
                    .map(|(_, v)| v),
            )?,
            b.register_scheduling_view(
                &self.scope,
                InputMemoryCategory::History,
                self.records
                    .iter()
                    .filter(|(k, _)| !live.contains(*k))
                    .map(|(_, v)| v),
            )?,
            b.register_owned_bytes(
                &self.scope,
                InputMemoryCategory::Current,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: metadata_bytes(
                        &self.scope,
                        &self.coverage,
                        self.records.len(),
                    )?,
                },
            )?,
        ];
        Ok(())
    }
    pub(crate) fn with_committed(&self, delta: &CommittedSchedulingDelta) -> Result<Self> {
        ensure!(
            delta.scope == self.scope
                && delta.revision == delta.coverage.revision
                && delta.revision >= delta.base_revision,
            "Scheduling delta namespace/revision differs"
        );
        ensure!(
            delta.base_revision == self.coverage.revision,
            SchedulingReadError::RevisionMismatch
        );
        let _candidate = self
            .budget
            .as_ref()
            .map(|b| {
                b.register_scheduling_view(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    delta.records.iter(),
                )
            })
            .transpose()?;
        let delta_metadata = metadata_bytes(&self.scope, &delta.coverage, delta.records.len())?;
        let _metadata = self
            .budget
            .as_ref()
            .map(|b| {
                b.register_owned_bytes(
                    &self.scope,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: delta_metadata,
                    },
                )
            })
            .transpose()?;
        let records = delta
            .records
            .iter()
            .map(|r| {
                self.records
                    .get(&r.locator.key)
                    .filter(|old| old.locator == r.locator)
                    .cloned()
                    .unwrap_or_else(|| r.clone())
            })
            .collect();
        let mut next = Self::bootstrap(
            self.scope.clone(),
            delta.coverage.clone(),
            records,
            self.record_limit,
            self.byte_limit,
        )?;
        next.budget = self.budget.clone();
        next.refresh_views()?;
        Ok(next)
    }
    /// Complete Active Trigger input for Work queue routing. Other Scheduling
    /// families remain explicitly unavailable in this selected observation.
    pub(in crate::runtime) fn capture_work_query_triggers_with_prior(
        &self,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        limits: (usize, usize),
    ) -> Result<(CapturedScheduling, (usize, usize))> {
        ensure!(
            self.coverage.active_triggers_complete,
            SchedulingReadError::IncompleteLive
        );
        if let Some(original) = &self.budget {
            ensure!(
                original.same_ledger(ledger),
                "Scheduling query ledger differs"
            );
        }
        let byte_limit = self.byte_limit.min(limits.0).min(64 * 1024 * 1024);
        let record_limit = self.record_limit.min(limits.1).min(65_536);
        let source = (
            &self.scope,
            self.coverage.revision,
            &self.coverage.active_triggers,
        );
        let nodes = crate::runtime::resource_refresh::borrowed_resource_record_count(&source)?
            .checked_add(
                self.coverage
                    .active_triggers
                    .len()
                    .checked_mul(4)
                    .context("Trigger query slots overflow")?,
            )
            .and_then(|n| n.checked_add(8))
            .context("Trigger query metadata count overflow")?;
        let metadata = serialized_bytes(&source)?
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Trigger query native bytes overflow")?,
            )
            .and_then(|n| n.checked_mul(8))
            .context("Trigger query metadata bytes overflow")?;
        let count = nodes
            .checked_mul(8)
            .context("Trigger query metadata count overflow")?;
        let mut used = (
            prior
                .0
                .checked_add(usize::try_from(metadata)?)
                .context("Trigger query combined bytes overflow")?,
            prior
                .1
                .checked_add(count)
                .context("Trigger query combined count overflow")?,
        );
        ensure!(
            used.0 <= byte_limit && used.1 <= record_limit,
            SchedulingReadError::ResourceExhausted("Work query Trigger metadata")
        );
        // Admit every String/BTree/Vec/Arc slot before lookup errors or selection.
        let guard = ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: metadata,
            },
        )?;
        for key in &self.coverage.active_triggers {
            let record = self
                .lookup(key)?
                .context("complete Active Trigger body missing")?;
            ensure!(
                key.family == SchedulingFamily::ExternalTrigger
                    && matches!(&record.fact,CanonicalSchedulingFact::ExternalTrigger(trigger)
                    if trigger.status==crate::types::ExternalTriggerStatus::Active),
                "Work query Trigger root differs"
            );
            let count =
                crate::runtime::resource_refresh::borrowed_resource_record_count(&record.fact)?;
            let bytes = record
                .resident_bytes
                .checked_add(
                    count
                        .checked_mul(256)
                        .context("Trigger query body native bytes overflow")?,
                )
                .context("Trigger query body bytes overflow")?;
            used.0 = used
                .0
                .checked_add(bytes)
                .context("Trigger query body total overflow")?;
            used.1 = used
                .1
                .checked_add(count)
                .context("Trigger query body count overflow")?;
            ensure!(
                used.0 <= byte_limit && used.1 <= record_limit,
                SchedulingReadError::ResourceExhausted("Work query Trigger selected bodies")
            );
        }
        let mut coverage = SchedulingCoverage::empty(self.coverage.revision);
        coverage.active_timers_complete = false;
        coverage.pending_wakes_complete = false;
        coverage.active_triggers = self.coverage.active_triggers.clone();
        coverage.required_keys = self.coverage.active_triggers.clone();
        let mut records = Vec::new();
        for key in &self.coverage.active_triggers {
            coverage.observations.push(SchedulingObservation {
                key: key.clone(),
                availability: SchedulingAvailability::Present,
            });
            records.push(
                self.lookup(key)?
                    .context("validated Active Trigger missing")?
                    .clone(),
            );
        }
        let body = ledger.register_scheduling_view(
            &self.scope,
            InputMemoryCategory::Pins,
            records.iter(),
        )?;
        let captured = CapturedScheduling {
            scope: self.scope.clone(),
            revision: self.coverage.revision,
            coverage,
            records,
            _views: vec![guard, body],
        };
        Ok((captured, used))
    }
    pub(crate) fn capture_with_material(
        &self,
        request: &SchedulingCaptureRequest,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<(CapturedScheduling, SchedulingCaptureMaterial)> {
        let mut phase = SchedulingCapturePhase::new(self, ledger, limits, prior)?;
        let captured = self.capture_observed(request, &mut phase)?;
        phase.finish(captured)
    }
    pub(crate) fn capture(&self, request: &SchedulingCaptureRequest) -> Result<CapturedScheduling> {
        self.capture_observed(request, &mut SchedulingCapturePhase::unobserved())
    }
    fn capture_observed(
        &self,
        request: &SchedulingCaptureRequest,
        phase: &mut SchedulingCapturePhase<'_>,
    ) -> Result<CapturedScheduling> {
        let request_bytes = serialized_bytes(&request.required_keys)?
            .checked_mul(4)
            .context("Scheduling capture selector overflow")?;
        ensure!(
            request.required_keys.len() <= self.record_limit
                && request_bytes <= u64::try_from(self.byte_limit)?,
            "Scheduling capture ResourceExhausted"
        );
        let capture_metadata = metadata_bytes(&self.scope, &self.coverage, self.records.len())?
            .checked_add(request_bytes)
            .context("Scheduling capture metadata overflow")?;
        ensure!(
            capture_metadata <= u64::try_from(self.byte_limit)?,
            "Scheduling capture ResourceExhausted"
        );
        let metadata = self
            .budget
            .as_ref()
            .filter(|_| !phase.is_observed())
            .map(|b| {
                b.register_owned_bytes(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: capture_metadata,
                    },
                )
            })
            .transpose()?;
        for key in &request.required_keys {
            self.lookup_observed(key, phase)?;
        }
        let roots = self.coverage.roots_observed(request, phase)?;
        let selected = self.required_closure_observed(roots, phase)?;
        phase.slots(selected.len(), 1)?;
        for key in &selected {
            if let Some(record) = self.records.get(key) {
                phase.body(record)?;
            }
        }
        let records: Vec<_> = selected
            .iter()
            .filter_map(|k| self.records.get(k).cloned())
            .collect();
        phase.json(&self.coverage, 1)?;
        let mut coverage = self.coverage.clone();
        phase.json(&request.required_keys, 1)?;
        coverage.required_keys = request.required_keys.clone();
        coverage.observations.retain(|o| selected.contains(&o.key));
        if !request.include_live {
            coverage.active_timers_complete = false;
            coverage.pending_wakes_complete = false;
            coverage.active_triggers_complete = false;
            coverage.active_timers.clear();
            coverage.pending_wakes.clear();
            coverage.active_triggers.clear();
        }
        for history in coverage.history.values_mut() {
            history.is_exhaustive &= history.keys.len() <= request.history_limit;
            history.keys.truncate(request.history_limit);
            history.loaded_keys.truncate(request.history_limit);
        }
        phase.json(&self.scope, 3)?;
        phase.slots(records.len(), 3)?;
        let mut views = Vec::new();
        if let Some(b) = &self.budget {
            views.extend(metadata);
            views.push(b.register_scheduling_view(
                &self.scope,
                InputMemoryCategory::Pins,
                records.iter(),
            )?);
        }
        Ok(CapturedScheduling {
            scope: self.scope.clone(),
            revision: coverage.revision,
            coverage,
            records,
            _views: views,
        })
    }
}
impl CapturedScheduling {
    /// Pure retained selection estimate. This grants no lookup/body authority.
    pub(crate) fn selected_material_estimate(&self) -> Result<(u64, usize)> {
        let mut count = self
            .records
            .len()
            .checked_add(self.coverage.active_timers.len())
            .and_then(|n| n.checked_add(self.coverage.pending_wakes.len()))
            .and_then(|n| n.checked_add(self.coverage.active_triggers.len()))
            .and_then(|n| n.checked_add(self.coverage.required_keys.len()))
            .and_then(|n| n.checked_add(self.coverage.observations.len()))
            .and_then(|n| n.checked_add(self.coverage.history.len()))
            .context("Scheduling selected metadata count overflow")?;
        for history in self.coverage.history.values() {
            count = count
                .checked_add(history.keys.len())
                .and_then(|n| n.checked_add(history.loaded_keys.len()))
                .context("Scheduling history count overflow")?;
        }
        let mut bytes = serialized_bytes(&(&self.scope, &self.coverage))?
            .checked_add(
                u64::try_from(count)?
                    .checked_mul(128)
                    .context("Scheduling selected slots overflow")?,
            )
            .context("Scheduling selected metadata bytes overflow")?;
        for record in &self.records {
            bytes = bytes
                .checked_add(u64::try_from(record.resident_bytes)?)
                .context("Scheduling selected body bytes overflow")?;
        }
        Ok((bytes, count))
    }
    pub(crate) fn lookup(
        &self,
        key: &SchedulingKey,
    ) -> Result<Option<&Arc<QualifiedSchedulingFact>>> {
        match self.coverage.availability(key) {
            Some(SchedulingAvailability::Absent) => Ok(None),
            Some(SchedulingAvailability::Pruned) => {
                Err(SchedulingReadError::Pruned(key.clone()).into())
            }
            Some(SchedulingAvailability::HistoricalUnavailable) => {
                Err(SchedulingReadError::HistoricalUnavailable(key.clone()).into())
            }
            Some(SchedulingAvailability::Present) => self
                .records
                .iter()
                .find(|r| &r.locator.key == key)
                .map(Some)
                .ok_or_else(|| SchedulingReadError::NeedsPreparation(key.clone()).into()),
            None => Err(SchedulingReadError::NeedsPreparation(key.clone()).into()),
        }
    }
    pub(crate) fn active_timer_count(&self) -> Result<usize> {
        ensure!(
            self.coverage.active_timers_complete,
            SchedulingReadError::IncompleteLive
        );
        Ok(self.coverage.active_timers.len())
    }
    pub(crate) fn pending_wake(&self, timer_id: &str) -> Result<Option<&SchedulingWakeFact>> {
        ensure!(
            self.coverage.pending_wakes_complete,
            SchedulingReadError::IncompleteLive
        );
        let mut found = None;
        for key in &self.coverage.pending_wakes {
            let record = self.lookup(key)?.context("pending Wake body missing")?;
            let CanonicalSchedulingFact::TimerWake(w) = &record.fact else {
                anyhow::bail!("pending Wake family differs")
            };
            if w.timer_id == timer_id {
                ensure!(found.is_none(), "multiple pending Wakes for Timer");
                found = Some(w);
            }
        }
        Ok(found)
    }
    pub(crate) fn active_default_trigger(&self) -> Result<Option<&ExternalTriggerRecord>> {
        ensure!(
            self.coverage.active_triggers_complete,
            SchedulingReadError::IncompleteLive
        );
        ensure!(
            self.coverage.active_triggers.len() <= 1,
            "multiple active default Triggers"
        );
        self.coverage
            .active_triggers
            .iter()
            .next()
            .map(|key| {
                let record = self.lookup(key)?.context("active Trigger body missing")?;
                let CanonicalSchedulingFact::ExternalTrigger(x) = &record.fact else {
                    anyhow::bail!("active Trigger family differs")
                };
                Ok(x)
            })
            .transpose()
    }
}
fn metadata_bytes(
    scope: &InputEvidenceScope,
    coverage: &SchedulingCoverage,
    records: usize,
) -> Result<u64> {
    serialized_bytes(coverage)?
        .checked_add(
            u64::try_from(scope.agent_id.len())?
                .checked_mul(
                    u64::try_from(records)?
                        .checked_add(8)
                        .context("Scheduling scope map overflow")?,
                )
                .context("Scheduling scope bytes overflow")?,
        )
        .context("Scheduling scope estimate overflow")?
        .checked_add(
            u64::try_from(records)?
                .checked_mul(128)
                .context("Scheduling map estimate overflow")?,
        )
        .context("Scheduling metadata estimate overflow")
}
