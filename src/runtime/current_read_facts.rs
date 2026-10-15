//! Finite Queue and audit facts from the same owner publication as State.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
};
use crate::types::{AgentIdentityRecord, AgentState, AuditEvent, QueueEntryRecord};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[path = "current_event_reads.rs"]
mod event_reads;
pub(crate) use event_reads::{CapturedCurrentEvents, CurrentEventQueryCoverage};

pub(crate) const CURRENT_EVENT_WINDOW: usize = 512;
pub(crate) const SCHEDULER_EVENT_BODY_WINDOW: usize = 64;
pub(crate) const SUMMARY_EVENT_COUNT_WINDOW: usize = 100;
pub(crate) const CURRENT_QUEUE_HISTORY: usize = 128;
pub(crate) const CURRENT_READ_MAX_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const CURRENT_READ_MAX_RECORDS: usize = 65_536;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CurrentReadFamily {
    Queue,
    Event,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CurrentReadKey {
    pub family: CurrentReadFamily,
    pub id: String,
}
impl CurrentReadKey {
    pub(crate) fn queue(id: impl Into<String>) -> Self {
        Self {
            family: CurrentReadFamily::Queue,
            id: id.into(),
        }
    }
    pub(crate) fn event(id: impl Into<String>) -> Self {
        Self {
            family: CurrentReadFamily::Event,
            id: id.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", content = "record", rename_all = "snake_case")]
pub(crate) enum CurrentReadFact {
    Queue(QueueEntryRecord),
    Event(AuditEvent),
}
impl CurrentReadFact {
    fn key_parts(&self) -> (CurrentReadFamily, &str) {
        match self {
            Self::Queue(q) => (CurrentReadFamily::Queue, &q.message_id),
            Self::Event(e) => (CurrentReadFamily::Event, &e.id),
        }
    }
    pub(crate) fn key(&self) -> CurrentReadKey {
        match self {
            Self::Queue(r) => CurrentReadKey::queue(&r.message_id),
            Self::Event(r) => CurrentReadKey::event(&r.id),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentReadLocator {
    pub scope: InputEvidenceScope,
    pub key: CurrentReadKey,
    pub revision: u64,
    pub content_digest: String,
    pub projection_digest: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QualifiedCurrentReadFact {
    pub locator: CurrentReadLocator,
    pub fact: CurrentReadFact,
    pub resident_bytes: usize,
    pub native_records: usize,
}
impl QualifiedCurrentReadFact {
    pub(crate) fn validate(&self, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.locator.scope == *scope
                && (self.locator.key.family, self.locator.key.id.as_str()) == self.fact.key_parts()
                && self.locator.revision > 0,
            "CurrentRead locator differs"
        );
        if let CurrentReadFact::Queue(q) = &self.fact {
            ensure!(
                q.agent_id == scope.agent_id,
                "CurrentRead Queue owner differs"
            );
        }
        ensure!(
            [
                &self.locator.content_digest,
                &self.locator.projection_digest
            ]
            .into_iter()
            .all(|s| s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
            "CurrentRead digest invalid"
        );
        ensure!(
            self.resident_bytes >= usize::try_from(serialized_bytes(&self.fact)?)?
                && self.native_records > 0,
            "CurrentRead material incomplete"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CurrentReadAvailability {
    Present,
    Absent,
    Pruned,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentReadObservation {
    pub key: CurrentReadKey,
    pub availability: CurrentReadAvailability,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentReadCoverage {
    pub revision: u64,
    pub state_digest: String,
    pub identity_digest: String,
    pub queue_complete: bool,
    pub queue_counts: BTreeMap<String, u64>,
    pub pending_queue: BTreeSet<CurrentReadKey>,
    pub terminal_queue: Vec<CurrentReadKey>,
    pub terminal_queue_limit: usize,
    /// Newest first, matching original event_seq/normalized created_at ordering.
    pub events: Vec<CurrentReadKey>,
    pub events_exhaustive: bool,
    /// Body materialization is an explicit consumer role, independent of headers.
    pub event_body_limit: usize,
    pub event_header_limit: usize,
    pub observations: Vec<CurrentReadObservation>,
    #[serde(default)]
    pub event_query: CurrentEventQueryCoverage,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentReadRequest {
    pub include_pending_queue: bool,
    pub terminal_queue_limit: usize,
    pub event_limit: usize,
    /// Summary needs a bounded count, not these Event Value bodies.
    pub event_count_limit: usize,
    pub required_queue_ids: BTreeSet<String>,
    #[serde(default)]
    pub event_birth_after: Option<u64>,
    pub byte_limit: usize,
    pub record_limit: usize,
}
impl CurrentReadRequest {
    pub(crate) fn current(byte_limit: usize, record_limit: usize) -> Self {
        Self {
            include_pending_queue: true,
            terminal_queue_limit: 0,
            event_limit: SCHEDULER_EVENT_BODY_WINDOW,
            event_count_limit: SUMMARY_EVENT_COUNT_WINDOW,
            required_queue_ids: BTreeSet::new(),
            event_birth_after: None,
            byte_limit,
            record_limit,
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.event_limit <= CURRENT_EVENT_WINDOW
                && self.event_count_limit <= CURRENT_EVENT_WINDOW
                && self.terminal_queue_limit <= CURRENT_QUEUE_HISTORY,
            CurrentReadError::WindowExpansion
        );
        ensure!(
            self.byte_limit > 0 && self.record_limit > 0,
            CurrentReadError::ResourceExhausted
        );
        Ok(())
    }
    pub(crate) fn check_material(&self, bytes: usize, records: usize) -> Result<()> {
        #[cfg(test)]
        if bytes > self.byte_limit.min(CURRENT_READ_MAX_BYTES)
            || records > self.record_limit.min(CURRENT_READ_MAX_RECORDS) {
            eprintln!("CurrentRead actual phase rejected: bytes={bytes} nodes={records} byte_limit={} node_limit={}",
                self.byte_limit.min(CURRENT_READ_MAX_BYTES), self.record_limit.min(CURRENT_READ_MAX_RECORDS));
        }
        ensure!(
            bytes <= self.byte_limit.min(CURRENT_READ_MAX_BYTES)
                && records <= self.record_limit.min(CURRENT_READ_MAX_RECORDS),
            CurrentReadError::ResourceExhausted
        );
        Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum CurrentReadError {
    #[error("CurrentRead needs explicit preparation")]
    NeedsPreparation,
    #[error("CurrentRead exact record pruned: {0:?}")]
    Pruned(CurrentReadKey),
    #[error("CurrentRead requested window needs explicit preparation")]
    WindowExpansion,
    #[error("CurrentRead scope retired or lacks State")]
    RetiredScope,
    #[error("CurrentRead canonical body/projection differs")]
    CanonicalChanged,
    #[error("CurrentRead unqualified canonical namespace: {0:?}")]
    UnknownNamespace(CurrentReadFamily),
    #[error("CurrentRead revision changed")]
    RevisionMismatch,
    #[error("CurrentRead original canonical table unavailable")]
    HistoricalUnavailable,
    #[error("CurrentRead ResourceExhausted")]
    ResourceExhausted,
}
#[derive(Debug)]
pub(crate) struct ResidentCurrentReadFacts {
    scope: InputEvidenceScope,
    coverage: CurrentReadCoverage,
    records: BTreeMap<CurrentReadKey, Arc<QualifiedCurrentReadFact>>,
    ledger: InputEvidenceBudgetLedger,
    _views: Vec<Arc<BudgetViewGuard>>,
}
#[derive(Debug)]
pub(crate) struct CapturedCurrentReadFacts {
    pub scope: InputEvidenceScope,
    pub revision: u64,
    pub state_digest: String,
    pub queue_counts: BTreeMap<String, u64>,
    pub records: Vec<Arc<QualifiedCurrentReadFact>>,
    /// Original EventLog order: oldest to newest within the selected head.
    pub events: Vec<Arc<QualifiedCurrentReadFact>>,
    pub required_queue: BTreeMap<String, CurrentReadAvailability>,
    pub recent_event_count: usize,
    material: (usize, usize),
    loaned: (usize, usize),
    _views: Vec<Arc<BudgetViewGuard>>,
}
impl CapturedCurrentReadFacts {
    pub(crate) fn has_queued(&self) -> bool {
        self.queue_counts.get("queued").copied().unwrap_or(0) > 0
    }
    pub(crate) fn has_interrupted(&self) -> bool {
        self.queue_counts.get("interrupted").copied().unwrap_or(0) > 0
    }
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok((
            u64::try_from(
                self.material
                    .0
                    .checked_add(self.loaned.0)
                    .context("CurrentRead standalone bytes overflow")?,
            )?,
            self.material
                .1
                .checked_add(self.loaned.1)
                .context("CurrentRead standalone nodes overflow")?,
        ))
    }
    pub(in crate::runtime) fn additional_material_estimate(&self) -> Result<(u64, usize)> {
        Ok((u64::try_from(self.material.0)?, self.material.1))
    }
}
impl ResidentCurrentReadFacts {
    pub(crate) fn bootstrap(
        scope: InputEvidenceScope,
        identity: &AgentIdentityRecord,
        coverage: CurrentReadCoverage,
        records: Vec<Arc<QualifiedCurrentReadFact>>,
        ledger: InputEvidenceBudgetLedger,
        request: &CurrentReadRequest,
    ) -> Result<Self> {
        request.validate()?;
        ensure!(
            identity.agent_id == scope.agent_id
                && identity.incarnation == scope.identity_incarnation
                && scope.identity_incarnation > 0
                && coverage.revision > 0
                && coverage.queue_complete,
            CurrentReadError::NeedsPreparation
        );
        let size = metadata_material(&scope, &coverage, records.len())?;
        let bytes = records.iter().try_fold(size.0, |n, r| {
            n.checked_add(r.resident_bytes)
                .context("CurrentRead resident bytes overflow")
        })?;
        let count = records.iter().try_fold(size.1, |n, r| {
            n.checked_add(r.native_records)
                .context("CurrentRead resident count overflow")
        })?;
        request.check_material(bytes, count)?;
        let _metadata = ledger.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(size.0)?,
            },
        )?;
        let _candidate = ledger.register_current_read_view(
            &scope,
            InputMemoryCategory::Candidate,
            records.iter(),
        )?;
        let mut map = BTreeMap::new();
        for record in records {
            record.validate(&scope)?;
            ensure!(
                record.locator.revision <= coverage.revision
                    && map.insert(record.locator.key.clone(), record).is_none(),
                "CurrentRead duplicate/revision"
            );
        }
        ensure!(
            coverage.observations.iter().all(|o| {
                let needs_body = o.key.family == CurrentReadFamily::Queue
                    || (o.key.family == CurrentReadFamily::Event
                        && coverage.event_query.born_event_seqs.contains_key(&o.key.id))
                    || coverage
                        .events
                        .iter()
                        .take(coverage.event_body_limit)
                        .any(|k| k == &o.key);
                map.contains_key(&o.key)
                    == (needs_body && o.availability == CurrentReadAvailability::Present)
            }),
            "CurrentRead observation/body differs"
        );
        ensure!(
            map.keys().all(|k| coverage
                .observations
                .iter()
                .any(|o| &o.key == k && o.availability == CurrentReadAvailability::Present)),
            "CurrentRead body not observed"
        );
        coverage.event_query.validate()?;
        for id in coverage.event_query.born_event_seqs.keys() {
            ensure!(
                coverage
                    .observations
                    .iter()
                    .any(|o| o.key.family == CurrentReadFamily::Event
                        && &o.key.id == id
                        && o.availability != CurrentReadAvailability::Absent),
                CurrentReadError::NeedsPreparation
            );
        }
        for (id, seq) in &coverage.event_query.born_event_seqs {
            let observed = coverage
                .observations
                .iter()
                .find(|o| o.key.family == CurrentReadFamily::Event && &o.key.id == id)
                .ok_or(CurrentReadError::NeedsPreparation)?;
            if observed.availability == CurrentReadAvailability::Present {
                let record = map
                    .get(&observed.key)
                    .ok_or(CurrentReadError::NeedsPreparation)?;
                ensure!(
                    matches!(&record.fact,CurrentReadFact::Event(event) if event.event_seq==*seq),
                    CurrentReadError::CanonicalChanged
                );
            }
        }
        for key in coverage
            .pending_queue
            .iter()
            .chain(&coverage.terminal_queue)
            .chain(coverage.events.iter().take(coverage.event_body_limit))
        {
            ensure!(map.contains_key(key), CurrentReadError::NeedsPreparation);
        }
        let views = vec![
            ledger.register_current_read_view(
                &scope,
                InputMemoryCategory::Current,
                map.values(),
            )?,
            ledger.register_owned_bytes(
                &scope,
                InputMemoryCategory::Current,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: u64::try_from(size.0)?,
                },
            )?,
        ];
        ledger.check_history_budget()?;
        Ok(Self {
            scope,
            coverage,
            records: map,
            ledger,
            _views: views,
        })
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(crate) fn coverage(&self) -> &CurrentReadCoverage {
        &self.coverage
    }
    pub(crate) fn validate_state(&self, state: &AgentState) -> Result<()> {
        ensure!(
            state.id == self.scope.agent_id
                && current_read_digest(state)? == self.coverage.state_digest,
            CurrentReadError::CanonicalChanged
        );
        Ok(())
    }
    pub(crate) fn validate_identity(&self, identity: &AgentIdentityRecord) -> Result<()> {
        ensure!(
            identity.agent_id == self.scope.agent_id
                && identity.incarnation == self.scope.identity_incarnation
                && current_read_digest(identity)? == self.coverage.identity_digest,
            CurrentReadError::CanonicalChanged
        );
        Ok(())
    }
    pub(in crate::runtime) fn validate_capture_material_binding(
        &self,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.uses_ledger(ledger) && !self._views.is_empty(),
            "scheduler domain material ledger differs"
        );
        for guard in &self._views {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        Ok(())
    }
    pub(crate) fn uses_ledger(&self, ledger: &InputEvidenceBudgetLedger) -> bool {
        self.ledger.same_ledger(ledger)
    }
    pub(crate) fn material_estimate(&self) -> Result<(usize, usize)> {
        let mut size = metadata_material(&self.scope, &self.coverage, self.records.len())?;
        for record in self.records.values() {
            size.0 = size
                .0
                .checked_add(record.resident_bytes)
                .context("CurrentRead shared bytes overflow")?;
            size.1 = size
                .1
                .checked_add(record.native_records)
                .context("CurrentRead shared records overflow")?;
        }
        Ok(size)
    }
    pub(crate) fn metadata_estimate(&self) -> Result<(usize, usize)> {
        metadata_material(&self.scope, &self.coverage, self.records.len())
    }
    /// Pure reconstruction shares every qualified body Arc. SQL authorization
    /// remains in the original same-TX proof and its actual committed facts.
    pub(crate) fn rebind_state_digest(
        &self,
        before_digest: &str,
        after: &AgentState,
        identity: &AgentIdentityRecord,
        revision: u64,
        request: &CurrentReadRequest,
    ) -> Result<Self> {
        ensure!(
            before_digest == self.coverage.state_digest,
            CurrentReadError::CanonicalChanged
        );
        ensure!(
            after.id == self.scope.agent_id
                && current_read_digest(identity)? == self.coverage.identity_digest,
            CurrentReadError::CanonicalChanged
        );
        ensure!(
            revision == self.coverage.revision
                || self.coverage.revision.checked_add(1) == Some(revision),
            CurrentReadError::RevisionMismatch
        );
        // A State-only ACK can narrow the live Event role using the original
        // complete source, but cannot recover older bodies from absent coverage.
        if let Some(after) = request.event_birth_after {
            self.coverage.event_query.require_after(after)?;
        }
        let size = self.metadata_estimate()?;
        let total = self.material_estimate()?;
        request.check_material(
            total
                .0
                .checked_add(size.0)
                .context("CurrentRead rebind bytes overflow")?,
            total
                .1
                .checked_add(size.1)
                .context("CurrentRead rebind count overflow")?,
        )?;
        let _candidate = self.ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(size.0)?,
            },
        )?;
        let mut coverage = self.coverage.clone();
        coverage.revision = revision;
        coverage.state_digest = current_read_digest(after)?;
        coverage.event_body_limit = request.event_limit;
        coverage.event_header_limit = request.event_count_limit.max(request.event_limit);
        if coverage.events.len() > coverage.event_header_limit {
            coverage.events_exhaustive = false;
        }
        coverage.events.truncate(coverage.event_header_limit);
        coverage.event_query.birth_after = request.event_birth_after;
        coverage
            .event_query
            .born_event_seqs
            .retain(|_, seq| request.event_birth_after.is_some_and(|after| *seq > after));
        let ordinary_events = &coverage.events;
        let born = &coverage.event_query.born_event_seqs;
        coverage.observations.retain(|observation| {
            observation.key.family == CurrentReadFamily::Queue
                || ordinary_events.contains(&observation.key)
                || born.contains_key(&observation.key.id)
        });
        // Header-only ordinary Events keep their count without retaining a body
        // that was solely owned by the now released birth-query role.
        let records = self
            .records
            .iter()
            .filter(|(key, _)| {
                key.family == CurrentReadFamily::Queue
                    || coverage
                        .events
                        .iter()
                        .take(coverage.event_body_limit)
                        .any(|id| id == *key)
                    || coverage.event_query.born_event_seqs.contains_key(&key.id)
            })
            .map(|(_, record)| record.clone())
            .collect();
        Self::bootstrap(
            self.scope.clone(),
            identity,
            coverage,
            records,
            self.ledger.clone(),
            request,
        )
    }
    pub(crate) fn capture(&self, request: &CurrentReadRequest) -> Result<CapturedCurrentReadFacts> {
        self.capture_with_material(request, (0, 0), None)
    }
    /// The prior is still held by the caller; only identical Event Arcs may be
    /// shared. Queue bodies, selectors and output slots always remain new cost.
    pub(in crate::runtime) fn capture_with_material(
        &self,
        request: &CurrentReadRequest,
        prior: (u64, usize),
        loan: Option<&CapturedCurrentEvents>,
    ) -> Result<CapturedCurrentReadFacts> {
        request.validate()?;
        if let Some(loan) = loan {
            loan.validate_scheduler_body_loan(self, &self.ledger)?;
            let actual = loan.material_estimate()?;
            ensure!(
                prior.0 >= actual.0 && prior.1 >= actual.1,
                "CurrentRead prior omits original Event loan"
            );
        }
        let configured = self.ledger.limits();
        let configured_bytes = usize::try_from(
            configured
                .history_per_agent_bytes
                .checked_add(configured.pending_per_agent_bytes)
                .context("CurrentRead actual limits overflow")?,
        )?;
        let check = |bytes: usize, count: usize| -> Result<()> {
            let bytes = usize::try_from(prior.0)?
                .checked_add(bytes)
                .context(CurrentReadError::ResourceExhausted)?;
            let count = prior
                .1
                .checked_add(count)
                .context(CurrentReadError::ResourceExhausted)?;
            request.check_material(bytes, count)?;
            ensure!(
                bytes <= configured_bytes && count <= usize::try_from(configured.record_limit)?,
                CurrentReadError::ResourceExhausted
            );
            Ok(())
        };
        ensure!(
            self.coverage.queue_complete,
            CurrentReadError::NeedsPreparation
        );
        // Borrowed selector sizing precedes every set/String/Arc clone.
        let mut bytes = usize::try_from(serialized_bytes(&(
            request,
            &self.scope,
            &self.coverage.queue_counts,
        ))?)?
        .checked_add(2048)
        .context("CurrentRead capture metadata overflow")?;
        let mut count = request
            .required_queue_ids
            .len()
            .checked_add(self.coverage.queue_counts.len())
            .and_then(|n| n.checked_add(32))
            .context("CurrentRead selector count overflow")?;
        let mut keys: Vec<&CurrentReadKey> = Vec::new();
        let index_size = self
            .coverage
            .pending_queue
            .len()
            .checked_add(request.terminal_queue_limit)
            .and_then(|n| n.checked_add(request.event_limit))
            .and_then(|n| n.checked_add(request.required_queue_ids.len()))
            .context("CurrentRead selector index overflow")?;
        bytes = bytes
            .checked_add(
                index_size
                    .checked_mul(64)
                    .context("CurrentRead index bytes overflow")?,
            )
            .context("CurrentRead capture bytes overflow")?;
        count = count
            .checked_add(index_size)
            .context("CurrentRead capture count overflow")?;
        check(bytes, count)?;
        let metadata = self.ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(bytes)?,
            },
        )?;
        if request.include_pending_queue {
            keys.extend(&self.coverage.pending_queue);
        }
        ensure!(
            request.terminal_queue_limit <= self.coverage.terminal_queue_limit,
            CurrentReadError::WindowExpansion
        );
        keys.extend(
            self.coverage
                .terminal_queue
                .iter()
                .take(request.terminal_queue_limit),
        );
        ensure!(
            request.event_limit <= self.coverage.event_body_limit
                && (request.event_count_limit <= self.coverage.events.len()
                    || self.coverage.events_exhaustive),
            CurrentReadError::WindowExpansion
        );
        keys.extend(self.coverage.events.iter().take(request.event_limit));
        // Exact by-ID observations never turn missing resident bodies into SQL.
        for id in &request.required_queue_ids {
            let observed = self
                .coverage
                .observations
                .iter()
                .find(|o| o.key.family == CurrentReadFamily::Queue && &o.key.id == id)
                .ok_or(CurrentReadError::NeedsPreparation)?;
            match observed.availability {
                CurrentReadAvailability::Pruned => {
                    return Err(CurrentReadError::Pruned(observed.key.clone()).into())
                }
                CurrentReadAvailability::Present => keys.push(&observed.key),
                CurrentReadAvailability::Absent => {}
            }
        }
        keys.sort();
        keys.dedup();
        let mut loaned = (0usize, 0usize);
        for key in &keys {
            let record = self
                .records
                .get(*key)
                .ok_or(CurrentReadError::NeedsPreparation)?;
            if loan.is_some_and(|loan| {
                loan.events
                    .iter()
                    .any(|original| Arc::ptr_eq(original, record))
            }) {
                loaned.0 = loaned
                    .0
                    .checked_add(record.resident_bytes)
                    .context("CurrentRead Event loan bytes overflow")?;
                loaned.1 = loaned
                    .1
                    .checked_add(record.native_records)
                    .context("CurrentRead Event loan nodes overflow")?;
                continue;
            }
            bytes = bytes
                .checked_add(record.resident_bytes)
                .context("CurrentRead capture bytes overflow")?;
            count = count
                .checked_add(record.native_records)
                .context("CurrentRead capture count overflow")?;
        }
        check(bytes, count)?;
        let pins = self.ledger.register_current_read_view(
            &self.scope,
            InputMemoryCategory::Pins,
            keys.iter().map(|key| &self.records[*key]),
        )?;
        let records = keys
            .into_iter()
            .map(|key| self.records[key].clone())
            .collect::<Vec<_>>();
        let events = self
            .coverage
            .events
            .iter()
            .take(request.event_limit)
            .rev()
            .map(|key| self.records[key].clone())
            .collect();
        let required_queue = request
            .required_queue_ids
            .iter()
            .map(|id| {
                let value = self
                    .coverage
                    .observations
                    .iter()
                    .find(|o| o.key.family == CurrentReadFamily::Queue && &o.key.id == id)
                    .unwrap()
                    .availability;
                (id.clone(), value)
            })
            .collect();
        Ok(CapturedCurrentReadFacts {
            scope: self.scope.clone(),
            revision: self.coverage.revision,
            state_digest: self.coverage.state_digest.clone(),
            queue_counts: self.coverage.queue_counts.clone(),
            records,
            events,
            required_queue,
            recent_event_count: self.coverage.events.len().min(request.event_count_limit),
            material: (bytes, count),
            loaned,
            _views: vec![metadata, pins],
        })
    }
}
fn metadata_material(
    scope: &InputEvidenceScope,
    coverage: &CurrentReadCoverage,
    records: usize,
) -> Result<(usize, usize)> {
    let count = coverage
        .observations
        .len()
        .checked_add(coverage.pending_queue.len())
        .and_then(|n| n.checked_add(coverage.terminal_queue.len()))
        .and_then(|n| n.checked_add(coverage.events.len()))
        .and_then(|n| n.checked_add(coverage.event_query.born_event_seqs.len().checked_mul(3)?))
        .and_then(|n| n.checked_add(records))
        .and_then(|n| n.checked_add(32))
        .context("CurrentRead metadata count overflow")?;
    let bytes = usize::try_from(serialized_bytes(&(scope, coverage))?)?
        .checked_add(
            count
                .checked_mul(128)
                .context("CurrentRead metadata nodes overflow")?,
        )
        .context("CurrentRead metadata bytes overflow")?;
    Ok((bytes, count))
}

/// Count borrowed typed material without creating a JSON body or Value tree.
pub(crate) fn borrowed_current_read_material(
    value: &impl serde::Serialize,
) -> Result<(usize, usize)> {
    #[derive(Default)]
    struct Counter {
        bytes: usize,
        nodes: usize,
        string: bool,
        escape: bool,
        primitive: bool,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("CurrentRead byte count overflow"))?;
            for byte in bytes {
                if self.string {
                    if self.escape {
                        self.escape = false;
                    } else if *byte == b'\\' {
                        self.escape = true;
                    } else if *byte == b'"' {
                        self.string = false;
                    }
                    continue;
                }
                let start = match *byte {
                    b'"' => {
                        self.string = true;
                        self.primitive = false;
                        true
                    }
                    b'{' | b'[' => {
                        self.primitive = false;
                        true
                    }
                    b'}' | b']' | b',' | b':' => {
                        self.primitive = false;
                        false
                    }
                    b' ' | b'\n' | b'\r' | b'\t' => false,
                    _ if !self.primitive => {
                        self.primitive = true;
                        true
                    }
                    _ => false,
                };
                if start {
                    self.nodes = self
                        .nodes
                        .checked_add(1)
                        .ok_or_else(|| std::io::Error::other("CurrentRead node count overflow"))?;
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter::default();
    serde_json::to_writer(&mut count, value)?;
    Ok((count.bytes, count.nodes))
}

fn current_read_digest(value: &impl serde::Serialize) -> Result<String> {
    use sha2::{Digest, Sha256};
    struct Writer(Sha256);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(Sha256::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(format!("{:x}", writer.0.finalize()))
}
