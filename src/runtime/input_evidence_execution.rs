//! Completeness metadata for the existing input/evidence domain. These indexes
//! never grant record bytes or replace execution protocol reducer authority.
use super::{
    CapturePhase, CapturedInputEvidence, InputEvidenceFamily as Family, InputEvidenceKey as Key,
    InputEvidenceReadError, PreparedInputEvidence,
};
use crate::domain::execution_protocol::{
    ExecutionAttempt, ExecutionAttemptState, ExecutionProtocolState,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceAttemptObservation {
    pub attempt_id: String,
    /// Stable allocation sequence, retained after the canonical row is pruned.
    pub allocation_ordinal: u64,
    /// Canonical legacy ordering is the original admitted_at string, then ID.
    pub admitted_at: String,
    pub state: ExecutionAttemptState,
    /// Payload hint only. Actual Turn ownership still requires qualified proof.
    pub turn_id_hint: Option<String>,
    pub terminal_outcome_id: Option<String>,
    pub pruned: bool,
}
impl SourceAttemptObservation {
    pub(crate) fn from_attempt(attempt: &ExecutionAttempt, allocation_ordinal: u64) -> Self {
        Self {
            attempt_id: attempt.attempt_id.clone(),
            allocation_ordinal,
            admitted_at: attempt.admitted_at.clone(),
            state: attempt.state,
            turn_id_hint: attempt.turn_id.clone(),
            terminal_outcome_id: attempt.terminal_outcome_id.clone(),
            pruned: false,
        }
    }
    pub(crate) fn matches(&self, attempt: &ExecutionAttempt) -> bool {
        !self.pruned
            && self.attempt_id == attempt.attempt_id
            && self.admitted_at == attempt.admitted_at
            && self.state == attempt.state
            && self.turn_id_hint == attempt.turn_id
            && self.terminal_outcome_id == attempt.terminal_outcome_id
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecutionSourceCoverage {
    pub source_message_id: String,
    /// 0 is a fixed seed; current writer revisions start at 1.
    pub revision: u64,
    pub allocation_highwater: u64,
    /// Complete admitted IDs for this source, including small prune tombstones.
    /// Presence here is metadata qualification, never body qualification.
    pub attempts: BTreeMap<String, SourceAttemptObservation>,
}
impl ExecutionSourceCoverage {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.source_message_id.is_empty(),
            "execution source ID is empty"
        );
        let mut ordinals = BTreeSet::new();
        for (id, record) in &self.attempts {
            ensure!(
                id == &record.attempt_id && !id.is_empty(),
                "execution source index key mismatch"
            );
            ensure!(
                record.allocation_ordinal > 0
                    && record.allocation_ordinal <= self.allocation_highwater,
                "execution source ordinal exceeds highwater"
            );
            ensure!(
                ordinals.insert(record.allocation_ordinal),
                "duplicate execution source ordinal"
            );
        }
        Ok(())
    }
    pub(crate) fn metadata_bytes(&self) -> Result<usize> {
        Ok(encoded_size(self)?.saturating_add((1 + self.attempts.len()).saturating_mul(128)))
    }
    pub(crate) fn next_attempt_id(&self) -> Result<String> {
        let ordinal = self
            .allocation_highwater
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("execution source attempt ordinal exhausted"))?;
        let base = format!("activation:message:{}", self.source_message_id);
        Ok(if ordinal == 1 {
            base
        } else {
            format!("{base}:attempt:{ordinal}")
        })
    }
    pub(crate) fn latest(&self) -> Option<&SourceAttemptObservation> {
        self.attempts
            .values()
            .filter(|attempt| !attempt.pruned)
            .max_by(|a, b| {
                a.admitted_at
                    .cmp(&b.admitted_at)
                    .then_with(|| a.attempt_id.cmp(&b.attempt_id))
            })
    }
}
/// Focused same-scope observation. Pruning an old registered row cannot turn
/// it into permission to register a replacement with the same Work ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionWorkPresence {
    Present,
    Absent,
    Pruned,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecutionWorkObservation {
    pub revision: u64,
    pub presence: ExecutionWorkPresence,
}
pub(crate) enum ObservedWorkExecution<'a> {
    Present(&'a crate::domain::execution_protocol::WorkItemExecutionRecord),
    Absent,
    Pruned,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct ExecutionInputCoverage {
    /// Structural same-readview partition observation; it grants no old rows.
    pub initialized: bool,
    pub open_attempt_ids: BTreeSet<String>,
    pub nonterminal_work_item_ids: BTreeSet<String>,
    pub sources: BTreeMap<String, ExecutionSourceCoverage>,
    pub work_items: BTreeMap<String, ExecutionWorkObservation>,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct ExecutionInputDelta {
    pub initialized: Option<bool>,
    /// Exact replacements for touched sources only, with monotonic revision.
    pub sources: BTreeMap<String, ExecutionSourceCoverage>,
    pub open_attempts: BTreeMap<String, bool>,
    pub nonterminal_work_items: BTreeMap<String, bool>,
    pub work_items: BTreeMap<String, ExecutionWorkObservation>,
}
impl ExecutionInputDelta {
    pub(crate) fn merge(&mut self, next: Self) -> Result<()> {
        let mut coverage = ExecutionInputCoverage {
            initialized: self.initialized.unwrap_or(false),
            sources: self.sources.clone(),
            work_items: self.work_items.clone(),
            ..Default::default()
        };
        coverage.apply(&ExecutionInputDelta {
            initialized: next.initialized,
            sources: next.sources,
            work_items: next.work_items,
            ..Default::default()
        })?;
        self.initialized = (self.initialized.is_some() || next.initialized.is_some())
            .then_some(coverage.initialized);
        self.sources = coverage.sources;
        self.work_items = coverage.work_items;
        self.open_attempts.extend(next.open_attempts);
        self.nonterminal_work_items
            .extend(next.nonterminal_work_items);
        Ok(())
    }
}
impl ExecutionInputCoverage {
    pub(crate) fn apply(&mut self, delta: &ExecutionInputDelta) -> Result<()> {
        if let Some(initialized) = delta.initialized {
            ensure!(
                initialized || !self.initialized,
                "execution partition initialization regressed"
            );
            self.initialized |= initialized;
        }
        for (id, next) in &delta.sources {
            ensure!(
                id == &next.source_message_id,
                "execution source replacement key mismatch"
            );
            next.validate()?;
            if let Some(old) = self.sources.get(id) {
                ensure!(
                    next.revision >= old.revision,
                    "stale execution source coverage"
                );
                ensure!(
                    next.allocation_highwater >= old.allocation_highwater,
                    "execution source highwater regressed"
                );
                if next.revision == old.revision {
                    ensure!(next == old, "same execution source revision differs");
                }
                for (attempt_id, previous) in &old.attempts {
                    let retained = next.attempts.get(attempt_id).ok_or_else(|| {
                        anyhow::anyhow!("execution source lost an ordinal tombstone")
                    })?;
                    ensure!(
                        retained.allocation_ordinal == previous.allocation_ordinal,
                        "execution source ordinal changed"
                    );
                    ensure!(
                        !previous.pruned || retained.pruned,
                        "pruned attempt ID was resurrected"
                    );
                }
            }
            self.sources.insert(id.clone(), next.clone());
        }
        for (id, next) in &delta.work_items {
            ensure!(!id.is_empty(), "execution Work observation ID is empty");
            if let Some(old) = self.work_items.get(id) {
                ensure!(
                    next.revision >= old.revision,
                    "stale execution Work observation"
                );
                if next.revision == old.revision {
                    ensure!(next == old, "same Work observation revision differs");
                }
                ensure!(
                    old.presence != ExecutionWorkPresence::Pruned
                        || next.presence == ExecutionWorkPresence::Pruned,
                    "pruned execution Work ID was resurrected"
                );
                ensure!(
                    old.presence != ExecutionWorkPresence::Present
                        || next.presence != ExecutionWorkPresence::Absent,
                    "registered execution Work became never registered"
                );
            }
            self.work_items.insert(id.clone(), next.clone());
        }
        for (id, present) in &delta.open_attempts {
            if *present {
                self.open_attempt_ids.insert(id.clone());
            } else {
                self.open_attempt_ids.remove(id);
            }
        }
        for (id, present) in &delta.nonterminal_work_items {
            if *present {
                self.nonterminal_work_item_ids.insert(id.clone());
            } else {
                self.nonterminal_work_item_ids.remove(id);
            }
        }
        ensure!(
            self.open_attempt_ids.len() <= 1,
            "multiple qualified open execution attempts"
        );
        Ok(())
    }
    /// Selected requests copy only source metadata they can actually use. The
    /// finite full scheduler capture keeps its complete source indexes.
    pub(crate) fn for_sources(
        &self,
        sources: &BTreeSet<String>,
        work_items: &BTreeSet<String>,
    ) -> Self {
        self.for_sources_observed(sources, work_items, &mut CapturePhase::unobserved())
            .expect("unobserved execution selection cannot fail")
    }
    pub(super) fn for_sources_observed(
        &self,
        sources: &BTreeSet<String>,
        work_items: &BTreeSet<String>,
        phase: &mut CapturePhase<'_>,
    ) -> Result<Self> {
        phase.json(
            &(&self.open_attempt_ids, &self.nonterminal_work_item_ids),
            1,
        )?;
        for (id, value) in &self.work_items {
            if work_items.contains(id) {
                phase.json(&(id, value), 1)?;
            }
        }
        for (id, record) in &self.sources {
            if sources.contains(id) {
                phase.json(&(id, record), 1)?;
            }
        }
        Ok(Self {
            initialized: self.initialized,
            open_attempt_ids: self.open_attempt_ids.clone(),
            nonterminal_work_item_ids: self.nonterminal_work_item_ids.clone(),
            work_items: self
                .work_items
                .iter()
                .filter(|(id, _)| work_items.contains(*id))
                .map(|(id, value)| (id.clone(), value.clone()))
                .collect(),
            sources: self
                .sources
                .iter()
                .filter(|(id, _)| sources.contains(*id))
                .map(|(id, record)| (id.clone(), record.clone()))
                .collect(),
        })
    }
    pub(crate) fn metadata_count(&self) -> usize {
        self.open_attempt_ids
            .len()
            .saturating_add(self.nonterminal_work_item_ids.len())
            .saturating_add(self.sources.len())
            .saturating_add(self.work_items.len())
            .saturating_add(
                self.sources
                    .values()
                    .map(|x| x.attempts.len())
                    .sum::<usize>(),
            )
    }
    /// Exact encoded metadata bytes, plus conservative collection-node overhead;
    /// no payload bodies are encoded and no encoded buffer is retained.
    pub(crate) fn metadata_bytes(&self) -> Result<usize> {
        Ok(encoded_size(self)?.saturating_add(self.metadata_count().saturating_mul(128)))
    }
    pub(crate) fn required_body_keys(&self, roots: &BTreeSet<Key>) -> BTreeSet<Key> {
        self.required_body_keys_observed(roots, &mut CapturePhase::unobserved())
            .expect("unobserved execution roots cannot fail")
    }
    pub(super) fn required_body_keys_observed(
        &self,
        roots: &BTreeSet<Key>,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<Key>> {
        phase.charge(
            u64::try_from(roots.len())?
                .checked_mul(256)
                .ok_or_else(|| anyhow::anyhow!("execution Turn selector overflow"))?,
            roots.len(),
        )?;
        let mut result = BTreeSet::new();
        let turns = roots
            .iter()
            .filter(|key| key.family == Family::Turn)
            .map(|key| key.id.as_str())
            .collect::<BTreeSet<_>>();
        for (source, coverage) in &self.sources {
            if roots
                .iter()
                .any(|key| key.family == Family::Message && key.id == *source)
            {
                if let Some(attempt) = coverage.latest() {
                    phase.key(Family::Attempt, &attempt.attempt_id, 1)?;
                    result.insert(Key::new(Family::Attempt, &attempt.attempt_id));
                }
            }
            for attempt in coverage.attempts.values().filter(|attempt| {
                !attempt.pruned
                    && attempt
                        .turn_id_hint
                        .as_deref()
                        .is_some_and(|turn| turns.contains(turn))
                    && matches!(
                        attempt.state,
                        ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
                    )
                    && attempt.terminal_outcome_id.is_some()
            }) {
                phase.key(Family::Attempt, &attempt.attempt_id, 1)?;
                result.insert(Key::new(Family::Attempt, &attempt.attempt_id));
            }
        }
        Ok(result)
    }
    /// Explicit planner preparation chooses all interrupted candidates for the
    /// named sources. This finite index never grants their historical bodies.
    pub(crate) fn required_interrupted_body_keys(
        &self,
        sources: &BTreeSet<String>,
    ) -> Result<BTreeSet<Key>> {
        self.required_interrupted_body_keys_observed(sources, &mut CapturePhase::unobserved())
    }
    pub(super) fn required_interrupted_body_keys_observed(
        &self,
        sources: &BTreeSet<String>,
        phase: &mut CapturePhase<'_>,
    ) -> Result<BTreeSet<Key>> {
        let mut keys = BTreeSet::new();
        for source in sources {
            phase.key(Family::Message, source, 2)?;
            for attempt in self.source(source)?.attempts.values().filter(|attempt| {
                !attempt.pruned && attempt.state == ExecutionAttemptState::Interrupted
            }) {
                phase.key(Family::Attempt, &attempt.attempt_id, 1)?;
            }
            keys.extend(
                self.source(source)?
                    .attempts
                    .values()
                    .filter(|attempt| {
                        !attempt.pruned && attempt.state == ExecutionAttemptState::Interrupted
                    })
                    .map(|attempt| Key::new(Family::Attempt, &attempt.attempt_id)),
            );
        }
        Ok(keys)
    }
    pub(crate) fn source(&self, source: &str) -> Result<&ExecutionSourceCoverage> {
        self.sources.get(source).ok_or_else(|| {
            InputEvidenceReadError::NeedsPreparation(Key::new(Family::Message, source)).into()
        })
    }
}
fn encoded_size(value: &impl Serialize) -> Result<usize> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("execution metadata size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}
impl CapturedInputEvidence {
    pub(crate) fn execution_work_item(&self, id: &str) -> Result<ObservedWorkExecution<'_>> {
        let key = Key::new(Family::WorkExecution, id);
        if let Some(evidence) = self.records.iter().find(|x| x.locator.key == key) {
            ensure!(
                self.execution
                    .work_items
                    .get(id)
                    .is_none_or(|x| x.presence == ExecutionWorkPresence::Present),
                "execution Work negative observation has a qualified body"
            );
            return match &evidence.record {
                PreparedInputEvidence::WorkExecution {
                    work_item_id,
                    record,
                } if work_item_id == id => Ok(ObservedWorkExecution::Present(record)),
                _ => anyhow::bail!("execution Work key differs from qualified body"),
            };
        }
        match self
            .execution
            .work_items
            .get(id)
            .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(key.clone()))?
            .presence
        {
            ExecutionWorkPresence::Absent => Ok(ObservedWorkExecution::Absent),
            ExecutionWorkPresence::Pruned => Ok(ObservedWorkExecution::Pruned),
            ExecutionWorkPresence::Present => {
                Err(InputEvidenceReadError::NeedsPreparation(key).into())
            }
        }
    }
    pub(crate) fn next_execution_attempt_id(&self, source: &str) -> Result<String> {
        self.execution.source(source)?.next_attempt_id()
    }
    /// Complete source-index candidates with qualified bodies. The Attempt Turn
    /// field remains a payload hint; callers must validate actual Message
    /// assignment, Turn trigger/owner/terminal and original lineage separately.
    pub(crate) fn execution_interrupted_attempts_for_source(
        &self,
        source: &str,
    ) -> Result<Vec<&ExecutionAttempt>> {
        self.execution
            .source(source)?
            .attempts
            .values()
            .filter(|attempt| {
                !attempt.pruned && attempt.state == ExecutionAttemptState::Interrupted
            })
            .map(|observed| self.observed_attempt(source, observed))
            .collect()
    }
    pub(crate) fn execution_predecessor_candidates(
        &self,
        source: &str,
        turn: &str,
    ) -> Result<Vec<&ExecutionAttempt>> {
        let mut result = Vec::new();
        for observed in self
            .execution
            .source(source)?
            .attempts
            .values()
            .filter(|x| {
                !x.pruned
                    && x.turn_id_hint.as_deref() == Some(turn)
                    && matches!(
                        x.state,
                        ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
                    )
                    && x.terminal_outcome_id.is_some()
            })
        {
            let attempt = self.observed_attempt(source, observed)?;
            let evidence = self
                .lookup(&Key::new(Family::Attempt, &attempt.attempt_id))?
                .ok_or_else(|| {
                    InputEvidenceReadError::NeedsPreparation(Key::new(
                        Family::Attempt,
                        &attempt.attempt_id,
                    ))
                })?;
            if !evidence
                .attempt_turn_keys
                .as_ref()
                .is_some_and(|keys| keys.contains(&Key::new(Family::Turn, turn)))
            {
                return Err(
                    InputEvidenceReadError::NeedsPreparation(Key::new(Family::Turn, turn)).into(),
                );
            }
            result.push(attempt);
        }
        Ok(result)
    }
    fn observed_attempt<'a>(
        &'a self,
        source: &str,
        observed: &SourceAttemptObservation,
    ) -> Result<&'a ExecutionAttempt> {
        let key = Key::new(Family::Attempt, &observed.attempt_id);
        match self.lookup(&key)? {
            Some(evidence) => match &evidence.record {
                PreparedInputEvidence::Attempt(attempt)
                    if attempt.source_message_id.as_deref() == Some(source)
                        && observed.matches(attempt) =>
                {
                    Ok(attempt)
                }
                _ => anyhow::bail!("execution source metadata differs from qualified allocation"),
            },
            None => Err(InputEvidenceReadError::NeedsPreparation(key).into()),
        }
    }
    /// Reducers may use this only after full live-frontier coverage is checked.
    /// The existing protocol_state() remains the selected-record projection used
    /// by context, and must not prove planner absence/latest/count.
    pub(crate) fn execution_state_if_initialized(&self) -> Result<Option<ExecutionProtocolState>> {
        self.execution_state_if_initialized_observed(&mut None)
    }
    pub(in crate::runtime) fn execution_state_if_initialized_observed(
        &self,
        observer: &mut Option<&mut dyn FnMut(u64, usize) -> Result<()>>,
    ) -> Result<Option<ExecutionProtocolState>> {
        use crate::work_item_scheduling::observe_scheduling_clone as observe;
        if !self.execution.initialized {
            ensure!(
                !self.records.iter().any(|x| matches!(
                    x.record,
                    PreparedInputEvidence::Attempt(_)
                        | PreparedInputEvidence::Outcome(_)
                        | PreparedInputEvidence::WorkExecution { .. }
                )),
                "uninitialized execution partition contains qualified records"
            );
            return Ok(None);
        }
        for id in &self.execution.open_attempt_ids {
            observe(id, observer)?;
            let key = Key::new(Family::Attempt, id);
            ensure!(
                matches!(self.lookup(&key)?.map(|x| &x.record),Some(PreparedInputEvidence::Attempt(x)) if x.state==ExecutionAttemptState::Open),
                "open execution frontier lacks its exact qualified allocation"
            );
        }
        for id in &self.execution.nonterminal_work_item_ids {
            observe(id, observer)?;
            let key = Key::new(Family::WorkExecution, id);
            ensure!(
                matches!(self.lookup(&key)?.map(|x| &x.record),Some(PreparedInputEvidence::WorkExecution{record,..}) if !matches!(record.state,crate::domain::execution_protocol::WorkItemExecutionState::Terminal{..})),
                "nonterminal execution Work frontier lacks its exact qualified allocation"
            );
        }
        let state = self.protocol_state_observed(observer)?;
        for record in state
            .attempts
            .values()
            .filter(|x| x.state == ExecutionAttemptState::Open)
        {
            observe(&record.attempt_id, observer)?;
        }
        for (id, _) in state.work_items.iter().filter(|(_, x)| {
            !matches!(
                x.state,
                crate::domain::execution_protocol::WorkItemExecutionState::Terminal { .. }
            )
        }) {
            observe(id, observer)?;
        }
        let actual_open = state
            .attempts
            .values()
            .filter(|x| x.state == ExecutionAttemptState::Open)
            .map(|x| x.attempt_id.clone())
            .collect::<BTreeSet<_>>();
        let actual_live = state
            .work_items
            .iter()
            .filter(|(_, x)| {
                !matches!(
                    x.state,
                    crate::domain::execution_protocol::WorkItemExecutionState::Terminal { .. }
                )
            })
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        ensure!(
            actual_open == self.execution.open_attempt_ids
                && actual_live == self.execution.nonterminal_work_item_ids,
            "selected execution projection disagrees with its complete live frontier"
        );
        Ok(Some(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> ExecutionSourceCoverage {
        ExecutionSourceCoverage {
            source_message_id: "input".into(),
            revision: 1,
            allocation_highwater: 4,
            attempts: BTreeMap::from([(
                "activation:message:input:attempt:4".into(),
                SourceAttemptObservation {
                    attempt_id: "activation:message:input:attempt:4".into(),
                    allocation_ordinal: 4,
                    admitted_at: "now".into(),
                    state: ExecutionAttemptState::Settled,
                    turn_id_hint: Some("turn".into()),
                    terminal_outcome_id: Some("outcome".into()),
                    pruned: true,
                },
            )]),
        }
    }
    #[test]
    fn prune_tombstone_never_reuses_attempt_allocation_id() {
        let value = source();
        assert!(value.latest().is_none());
        assert_eq!(
            value.next_attempt_id().unwrap(),
            "activation:message:input:attempt:5"
        );
        assert_eq!(
            ExecutionSourceCoverage {
                source_message_id: "new".into(),
                revision: 1,
                allocation_highwater: 0,
                attempts: BTreeMap::new()
            }
            .next_attempt_id()
            .unwrap(),
            "activation:message:new"
        );
    }
    #[test]
    fn missing_source_is_not_an_empty_or_first_attempt_proof() {
        let coverage = ExecutionInputCoverage::default();
        assert!(coverage
            .source("input")
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
    }
    #[test]
    fn stale_source_or_discarded_tombstone_is_rejected() {
        let original = source();
        let mut coverage = ExecutionInputCoverage {
            sources: BTreeMap::from([("input".into(), original.clone())]),
            ..Default::default()
        };
        let mut stale = original.clone();
        stale.revision = 0;
        assert!(coverage
            .apply(&ExecutionInputDelta {
                sources: BTreeMap::from([("input".into(), stale)]),
                ..Default::default()
            })
            .is_err());
        let mut lost = original;
        lost.revision = 2;
        lost.attempts.clear();
        assert!(coverage
            .apply(&ExecutionInputDelta {
                sources: BTreeMap::from([("input".into(), lost)]),
                ..Default::default()
            })
            .is_err());
    }
}
