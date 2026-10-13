//! Host admission charges and observed input/evidence allocations.
//! Pending tickets retain SQL responsibility; memory guards only observe views.
use super::resident_input_evidence::{InputEvidenceScope, QualifiedInputEvidence};
use super::resident_work::QualifiedWorkRecord;
use crate::config::StateMemoryLimits;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopeKey(String, u64);
impl From<&InputEvidenceScope> for ScopeKey {
    fn from(s: &InputEvidenceScope) -> Self {
        Self(s.agent_id.clone(), s.identity_incarnation)
    }
}
impl ScopeKey {
    fn scope(&self) -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: self.0.clone(),
            identity_incarnation: self.1,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BudgetResource {
    PendingAgentCount,
    PendingAgentBytes,
    PendingHostBytes,
    HistoryAgentBytes,
    HistoryHostBytes,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum InputEvidenceBudgetError {
    #[error("ResourceExhausted: {resource:?}, used={used}, requested={requested}, limit={limit}")]
    ResourceExhausted {
        resource: BudgetResource,
        scope: Option<InputEvidenceScope>,
        used: u64,
        requested: u64,
        limit: u64,
    },
    #[error("pending input needs original-attempt reconciliation: {message_id}")]
    NeedsReconcile {
        scope: InputEvidenceScope,
        message_id: String,
    },
    #[error("same scoped message has a different body charge: {message_id}")]
    ConflictingMessage { message_id: String },
    #[error("pending ticket belongs to another ledger, generation or attempt")]
    TicketMismatch,
    #[error("pending ticket phase does not permit this resolution")]
    InvalidPendingPhase,
    #[error("cold pending bootstrap cannot replace an unresolved attempt")]
    ScopeHasUnresolvedAttempt,
    #[error("invalid memory budget input: {0}")]
    InvalidInput(&'static str),
    #[error("memory budget counter overflow")]
    CounterOverflow,
    #[error("memory budget ledger lock was poisoned")]
    Poisoned,
}
type Result<T> = std::result::Result<T, InputEvidenceBudgetError>;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
/// Logical accepted/pre-reserved body responsibility. This does not measure
/// currently resident allocator bytes; record/owned observations are separate.
pub(crate) struct PendingUsage {
    pub count: u64,
    pub body_bytes: u64,
    pub unresolved_count: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPhase {
    Reserved,
    AttemptStarted,
    Unknown,
    Accepted,
}
#[derive(Debug)]
struct PendingEntry {
    bytes: u64,
    nonce: u64,
    phase: PendingPhase,
}
/// No Drop release: caller cancellation cannot prove SQL did not commit.
#[derive(Debug, Clone)]
pub(crate) struct PendingAdmissionTicket {
    ledger: Weak<LedgerInner>,
    scope: ScopeKey,
    message_id: String,
    nonce: u64,
}
#[derive(Debug)]
pub(crate) enum PendingAdmission {
    Reserved(PendingAdmissionTicket),
    /// A budget observation, never a receipt or permission to repeat SQL.
    ExistingAccepted,
}
#[derive(Debug, Clone)]
pub(crate) struct PendingBodyCharge {
    pub message_id: String,
    /// Canonical MessageBody encoded bytes; use the same policy before SQL and
    /// at bootstrap. Envelope/container/index estimates belong to other accounts.
    pub body_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputMemoryCategory {
    Current,
    History,
    Candidate,
    Pins,
    PreparedPrompt,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AllocationUsage {
    pub allocations: u64,
    /// Prepared-record serialized bytes or explicitly estimated owned-data bytes.
    /// `report.owned` separates the latter; neither measures allocator/RSS bytes.
    pub bytes: u64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct OwnedBytesEstimate {
    pub payload_bytes: u64,
    pub metadata_estimate_bytes: u64,
}
impl OwnedBytesEstimate {
    fn total(self) -> Result<u64> {
        add(self.payload_bytes, self.metadata_estimate_bytes)
    }
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScopeMemoryReport {
    pub scope: InputEvidenceScope,
    pub pending: PendingUsage,
    /// Categories overlap; adding them would count shared allocations twice.
    pub categories: BTreeMap<InputMemoryCategory, AllocationUsage>,
    pub physical_records: AllocationUsage,
    pub retained_unclassified: AllocationUsage,
    pub reclaimable_history: AllocationUsage,
    pub owned: OwnedBytesEstimate,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct InputEvidenceBudgetReport {
    pub limits: StateMemoryLimits,
    pub scopes: Vec<ScopeMemoryReport>,
    pub pending: PendingUsage,
    pub categories: BTreeMap<InputMemoryCategory, AllocationUsage>,
    pub physical_records: AllocationUsage,
    pub retained_unclassified: AllocationUsage,
    pub reclaimable_history: AllocationUsage,
    pub owned: OwnedBytesEstimate,
    /// Physical prepared records plus owned payload/metadata estimate. Excludes
    /// SQL/provider/tool temporaries, allocator overhead, other domains and RSS.
    pub observed_bytes: u64,
}
#[derive(Debug)]
enum AllocationWeak {
    Evidence(Weak<QualifiedInputEvidence>),
    Queue(Weak<crate::types::MessageEnvelope>),
    Work(Weak<QualifiedWorkRecord>),
}
impl AllocationWeak {
    fn strong_count(&self) -> usize {
        match self {
            Self::Evidence(record) => record.strong_count(),
            Self::Queue(message) => message.strong_count(),
            Self::Work(record) => record.strong_count(),
        }
    }
}
#[derive(Debug)]
struct AllocationEntry {
    weak: AllocationWeak,
    scope: ScopeKey,
    bytes: u64,
    memberships: BTreeMap<u64, InputMemoryCategory>,
}
#[derive(Debug)]
struct ViewEntry {
    scope: ScopeKey,
    category: InputMemoryCategory,
    allocations: BTreeSet<usize>,
    owned: OwnedBytesEstimate,
}
#[derive(Debug, Default)]
struct LedgerState {
    next_nonce: u64,
    pending: BTreeMap<(ScopeKey, String), PendingEntry>,
    pending_usage: BTreeMap<ScopeKey, PendingUsage>,
    pending_host_bytes: u64,
    allocations: BTreeMap<usize, AllocationEntry>,
    views: BTreeMap<u64, ViewEntry>,
}
impl LedgerState {
    fn nonce(&mut self) -> Result<u64> {
        self.next_nonce = add(self.next_nonce, 1)?;
        Ok(self.next_nonce)
    }
    fn remove_pending(&mut self, key: &(ScopeKey, String)) {
        if let Some(entry) = self.pending.remove(key) {
            let usage = self.pending_usage.get_mut(&key.0).expect("charged scope");
            usage.count -= 1;
            usage.body_bytes -= entry.bytes;
            usage.unresolved_count -= u64::from(entry.phase != PendingPhase::Accepted);
            self.pending_host_bytes -= entry.bytes;
            if usage.count == 0 {
                self.pending_usage.remove(&key.0);
            }
        }
    }
    fn prune_dead(&mut self) {
        self.allocations
            .retain(|_, entry| entry.weak.strong_count() != 0);
    }
}
#[derive(Debug)]
struct LedgerInner {
    limits: StateMemoryLimits,
    state: Mutex<LedgerState>,
}
#[derive(Debug, Clone)]
pub(crate) struct InputEvidenceBudgetLedger {
    inner: Arc<LedgerInner>,
}
/// Held by the actual root/candidate/captured data. Arc clones share a view;
/// final guard Drop releases only observation membership, never SQL charges.
#[derive(Debug)]
pub(crate) struct BudgetViewGuard {
    ledger: Arc<LedgerInner>,
    nonce: u64,
}
impl Drop for BudgetViewGuard {
    fn drop(&mut self) {
        let mut state = self
            .ledger
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(view) = state.views.remove(&self.nonce) {
            for id in view.allocations {
                if let Some(entry) = state.allocations.get_mut(&id) {
                    entry.memberships.remove(&self.nonce);
                }
            }
        }
        state.prune_dead();
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum PendingTicketResolution {
    Started,
    Unknown,
    Accepted,
    DefinitelyNotCommitted,
    FreshNotCommitted,
}
impl InputEvidenceBudgetLedger {
    pub(crate) fn new(limits: StateMemoryLimits) -> Result<Self> {
        limits
            .validate()
            .map_err(|_| InputEvidenceBudgetError::InvalidInput("limits must be positive"))?;
        Ok(Self {
            inner: Arc::new(LedgerInner {
                limits,
                state: Mutex::new(LedgerState::default()),
            }),
        })
    }
    pub(crate) fn limits(&self) -> StateMemoryLimits {
        self.inner.limits
    }
    pub(crate) fn same_ledger(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    /// Validate every original ticket before changing any logical responsibility.
    pub(crate) fn resolve_original_tickets(
        &self,
        tickets: &[&PendingAdmissionTicket],
        resolution: PendingTicketResolution,
    ) -> Result<()> {
        let mut keys = BTreeSet::new();
        for ticket in tickets {
            self.validate_ticket(ticket)?;
            if !keys.insert((ticket.scope.clone(), ticket.message_id.clone())) {
                return Err(InputEvidenceBudgetError::TicketMismatch);
            }
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        let mut changes = Vec::with_capacity(tickets.len());
        for ticket in tickets {
            let old = matching(&state, ticket)?.phase;
            use PendingPhase as Phase;
            use PendingTicketResolution as Resolution;
            let next = match (resolution, old) {
                (Resolution::Started, Phase::Reserved) => Some(Phase::AttemptStarted),
                (Resolution::Unknown, Phase::AttemptStarted | Phase::Unknown) => {
                    Some(Phase::Unknown)
                }
                (
                    Resolution::Accepted,
                    Phase::AttemptStarted | Phase::Unknown | Phase::Accepted,
                ) => Some(Phase::Accepted),
                (Resolution::DefinitelyNotCommitted, Phase::Reserved | Phase::AttemptStarted) => {
                    None
                }
                (
                    Resolution::FreshNotCommitted,
                    Phase::Reserved | Phase::AttemptStarted | Phase::Unknown,
                ) => None,
                _ => return Err(InputEvidenceBudgetError::InvalidPendingPhase),
            };
            changes.push(((ticket.scope.clone(), ticket.message_id.clone()), old, next));
        }
        for (key, old, next) in changes {
            if let Some(next) = next {
                state
                    .pending
                    .get_mut(&key)
                    .expect("validated original ticket")
                    .phase = next;
                if old != PendingPhase::Accepted && next == PendingPhase::Accepted {
                    state
                        .pending_usage
                        .get_mut(&key.0)
                        .expect("ticket scope usage")
                        .unresolved_count -= 1;
                }
            } else {
                state.remove_pending(&key);
            }
        }
        Ok(())
    }
    /// Caller already owns exact scope reservation and command/receipt authority.
    pub(crate) fn reserve_pending(
        &self,
        scope: &InputEvidenceScope,
        message_id: &str,
        body_bytes: u64,
    ) -> Result<PendingAdmission> {
        valid_scope(scope)?;
        if message_id.is_empty() {
            return Err(InputEvidenceBudgetError::InvalidInput("empty message ID"));
        }
        let sk = ScopeKey::from(scope);
        let key = (sk.clone(), message_id.to_owned());
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        if let Some(entry) = state.pending.get(&key) {
            if entry.bytes != body_bytes {
                return Err(InputEvidenceBudgetError::ConflictingMessage {
                    message_id: message_id.to_owned(),
                });
            }
            return if entry.phase == PendingPhase::Accepted {
                Ok(PendingAdmission::ExistingAccepted)
            } else {
                Err(InputEvidenceBudgetError::NeedsReconcile {
                    scope: scope.clone(),
                    message_id: message_id.to_owned(),
                })
            };
        }
        let used = state.pending_usage.get(&sk).copied().unwrap_or_default();
        check(
            BudgetResource::PendingAgentCount,
            Some(scope),
            used.count,
            1,
            self.inner.limits.pending_per_agent_count,
        )?;
        check(
            BudgetResource::PendingAgentBytes,
            Some(scope),
            used.body_bytes,
            body_bytes,
            self.inner.limits.pending_per_agent_bytes,
        )?;
        check(
            BudgetResource::PendingHostBytes,
            None,
            state.pending_host_bytes,
            body_bytes,
            self.inner.limits.pending_host_bytes,
        )?;
        let nonce = state.nonce()?;
        let usage = state.pending_usage.entry(sk.clone()).or_default();
        usage.count += 1;
        usage.body_bytes += body_bytes;
        usage.unresolved_count += 1;
        state.pending_host_bytes += body_bytes;
        state.pending.insert(
            key,
            PendingEntry {
                bytes: body_bytes,
                nonce,
                phase: PendingPhase::Reserved,
            },
        );
        Ok(PendingAdmission::Reserved(PendingAdmissionTicket {
            ledger: Arc::downgrade(&self.inner),
            scope: sk,
            message_id: message_id.to_owned(),
            nonce,
        }))
    }
    /// Synchronous boundary immediately before the original SQL attempt.
    #[cfg(test)]
    pub(crate) fn mark_attempt_started(&self, ticket: &PendingAdmissionTicket) -> Result<()> {
        self.update_phase(ticket, |p| match p {
            PendingPhase::Reserved => Ok(PendingPhase::AttemptStarted),
            _ => Err(InputEvidenceBudgetError::InvalidPendingPhase),
        })
    }
    #[cfg(test)]
    pub(crate) fn mark_unknown(&self, ticket: &PendingAdmissionTicket) -> Result<()> {
        self.update_phase(ticket, |p| match p {
            PendingPhase::AttemptStarted | PendingPhase::Unknown => Ok(PendingPhase::Unknown),
            _ => Err(InputEvidenceBudgetError::InvalidPendingPhase),
        })
    }
    /// Original ACK or the original family's positive witness; idempotent.
    #[cfg(test)]
    pub(crate) fn mark_accepted(&self, ticket: &PendingAdmissionTicket) -> Result<()> {
        self.update_phase(ticket, |p| match p {
            PendingPhase::AttemptStarted | PendingPhase::Unknown | PendingPhase::Accepted => {
                Ok(PendingPhase::Accepted)
            }
            _ => Err(InputEvidenceBudgetError::InvalidPendingPhase),
        })
    }
    pub(crate) fn definitely_not_committed(&self, ticket: &PendingAdmissionTicket) -> Result<()> {
        self.release_ticket(ticket, false)
    }
    /// Caller family verifies finished attempt, fresh absence, witness coverage,
    /// retention and matching operation. The ledger cannot prove these facts.
    #[cfg(test)]
    pub(crate) fn resolve_not_committed(&self, ticket: &PendingAdmissionTicket) -> Result<()> {
        self.release_ticket(ticket, true)
    }
    fn validate_ticket(&self, ticket: &PendingAdmissionTicket) -> Result<()> {
        if !Weak::ptr_eq(&ticket.ledger, &Arc::downgrade(&self.inner)) {
            return Err(InputEvidenceBudgetError::TicketMismatch);
        }
        Ok(())
    }
    fn release_ticket(&self, ticket: &PendingAdmissionTicket, resolved: bool) -> Result<()> {
        self.validate_ticket(ticket)?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        let p = matching(&state, ticket)?.phase;
        if p == PendingPhase::Accepted || (!resolved && p == PendingPhase::Unknown) {
            return Err(InputEvidenceBudgetError::InvalidPendingPhase);
        }
        state.remove_pending(&(ticket.scope.clone(), ticket.message_id.clone()));
        Ok(())
    }
    #[cfg(test)]
    fn update_phase(
        &self,
        ticket: &PendingAdmissionTicket,
        change: impl FnOnce(PendingPhase) -> Result<PendingPhase>,
    ) -> Result<()> {
        self.validate_ticket(ticket)?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        let old = matching(&state, ticket)?.phase;
        let next = change(old)?;
        state
            .pending
            .get_mut(&(ticket.scope.clone(), ticket.message_id.clone()))
            .unwrap()
            .phase = next;
        if old != PendingPhase::Accepted && next == PendingPhase::Accepted {
            state
                .pending_usage
                .get_mut(&ticket.scope)
                .unwrap()
                .unresolved_count -= 1;
        }
        Ok(())
    }
    /// Exact committed queue removals. Validate all members before any release;
    /// Unknown entries require their original resolver, not a current queue read.
    pub(crate) fn release_accepted(
        &self,
        scope: &InputEvidenceScope,
        message_ids: &[String],
    ) -> Result<()> {
        valid_scope(scope)?;
        let sk = ScopeKey::from(scope);
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        let keys = message_ids
            .iter()
            .map(|id| (sk.clone(), id.clone()))
            .collect::<BTreeSet<_>>();
        for key in &keys {
            if state
                .pending
                .get(key)
                .is_some_and(|e| e.phase != PendingPhase::Accepted)
            {
                return Err(InputEvidenceBudgetError::NeedsReconcile {
                    scope: scope.clone(),
                    message_id: key.1.clone(),
                });
            }
        }
        for key in keys {
            state.remove_pending(&key);
        }
        Ok(())
    }
    /// Metadata-only cold check before decoding accepted Message payloads.
    /// This is conservative allocation gating, not an Accepted body charge.
    pub(crate) fn preflight_actual_pending(
        &self,
        scope: &InputEvidenceScope,
        count: u64,
        conservative_payload_bytes: u64,
    ) -> Result<()> {
        valid_scope(scope)?;
        let sk = ScopeKey::from(scope);
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        if state
            .pending
            .iter()
            .any(|((scope, _), entry)| scope == &sk && entry.phase != PendingPhase::Accepted)
        {
            return Err(InputEvidenceBudgetError::ScopeHasUnresolvedAttempt);
        }
        let old = state.pending_usage.get(&sk).copied().unwrap_or_default();
        let other_host_bytes = state
            .pending_host_bytes
            .checked_sub(old.body_bytes)
            .ok_or(InputEvidenceBudgetError::CounterOverflow)?;
        check(
            BudgetResource::PendingAgentCount,
            Some(scope),
            0,
            count,
            self.inner.limits.pending_per_agent_count,
        )?;
        check(
            BudgetResource::PendingAgentBytes,
            Some(scope),
            0,
            conservative_payload_bytes,
            self.inner.limits.pending_per_agent_bytes,
        )?;
        check(
            BudgetResource::PendingHostBytes,
            None,
            other_host_bytes,
            conservative_payload_bytes,
            self.inner.limits.pending_host_bytes,
        )?;
        Ok(())
    }
    /// Cold activation, under scope reservation and one fresh durable snapshot.
    /// Over-limit still installs actual Accepted charges, preserving durable work;
    /// ResourceExhausted rejects activation until explicitly adjusted limits.
    pub(crate) fn install_actual_pending(
        &self,
        scope: &InputEvidenceScope,
        bodies: &[PendingBodyCharge],
    ) -> Result<PendingUsage> {
        valid_scope(scope)?;
        let sk = ScopeKey::from(scope);
        let mut actual = BTreeMap::new();
        let mut usage = PendingUsage::default();
        for body in bodies {
            if body.message_id.is_empty() {
                return Err(InputEvidenceBudgetError::InvalidInput("empty message ID"));
            }
            if let Some(old) = actual.insert(body.message_id.clone(), body.body_bytes) {
                if old != body.body_bytes {
                    return Err(InputEvidenceBudgetError::ConflictingMessage {
                        message_id: body.message_id.clone(),
                    });
                }
                continue;
            }
            usage.count = add(usage.count, 1)?;
            usage.body_bytes = add(usage.body_bytes, body.body_bytes)?;
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        if state
            .pending
            .iter()
            .any(|((scope, _), entry)| scope == &sk && entry.phase != PendingPhase::Accepted)
        {
            return Err(InputEvidenceBudgetError::ScopeHasUnresolvedAttempt);
        }
        let old = state.pending_usage.get(&sk).copied().unwrap_or_default();
        let next_host = add(state.pending_host_bytes - old.body_bytes, usage.body_bytes)?;
        for (id, bytes) in &actual {
            if state
                .pending
                .get(&(sk.clone(), id.clone()))
                .is_some_and(|entry| entry.bytes != *bytes)
            {
                return Err(InputEvidenceBudgetError::ConflictingMessage {
                    message_id: id.clone(),
                });
            }
        }
        let retained_nonces = state
            .pending
            .iter()
            .filter(|((scope, _), _)| scope == &sk)
            .map(|((_, id), entry)| (id.clone(), entry.nonce))
            .collect::<BTreeMap<_, _>>();
        let new_count = actual
            .keys()
            .filter(|id| !retained_nonces.contains_key(*id))
            .count() as u64;
        let end_nonce = add(state.next_nonce, new_count)?;
        let old_keys = state
            .pending
            .keys()
            .filter(|(scope, _)| scope == &sk)
            .cloned()
            .collect::<Vec<_>>();
        for key in old_keys {
            state.remove_pending(&key);
        }
        for (id, bytes) in actual {
            let nonce = match retained_nonces.get(&id) {
                Some(nonce) => *nonce,
                None => state.nonce()?,
            };
            state.pending.insert(
                (sk.clone(), id),
                PendingEntry {
                    bytes,
                    nonce,
                    phase: PendingPhase::Accepted,
                },
            );
        }
        debug_assert_eq!(state.next_nonce, end_nonce);
        if usage.count > 0 {
            state.pending_usage.insert(sk, usage);
        }
        state.pending_host_bytes = next_host;
        check(
            BudgetResource::PendingAgentCount,
            Some(scope),
            usage.count,
            0,
            self.inner.limits.pending_per_agent_count,
        )?;
        check(
            BudgetResource::PendingAgentBytes,
            Some(scope),
            usage.body_bytes,
            0,
            self.inner.limits.pending_per_agent_bytes,
        )?;
        check(
            BudgetResource::PendingHostBytes,
            None,
            next_host,
            0,
            self.inner.limits.pending_host_bytes,
        )?;
        Ok(usage)
    }
    /// Each live root/candidate/capture gets a unique view. Old roots remain
    /// classified until their actual guard drops. Ledger stores record Weak only.
    pub(crate) fn register_view(
        &self,
        scope: &InputEvidenceScope,
        category: InputMemoryCategory,
        records: &[Arc<QualifiedInputEvidence>],
    ) -> Result<Arc<BudgetViewGuard>> {
        valid_scope(scope)?;
        let sk = ScopeKey::from(scope);
        let mut unique = BTreeMap::new();
        for record in records {
            if &record.locator.scope != scope {
                return Err(InputEvidenceBudgetError::InvalidInput(
                    "record scope differs from view",
                ));
            }
            let bytes = u64::try_from(record.resident_bytes)
                .map_err(|_| InputEvidenceBudgetError::CounterOverflow)?;
            unique.insert(Arc::as_ptr(record) as usize, (record, bytes));
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        state.prune_dead();
        let nonce = state.nonce()?;
        for (id, (record, bytes)) in &unique {
            let entry = state
                .allocations
                .entry(*id)
                .or_insert_with(|| AllocationEntry {
                    weak: AllocationWeak::Evidence(Arc::downgrade(record)),
                    scope: sk.clone(),
                    bytes: *bytes,
                    memberships: BTreeMap::new(),
                });
            entry.memberships.insert(nonce, category);
        }
        state.views.insert(
            nonce,
            ViewEntry {
                scope: sk,
                category,
                allocations: unique.into_keys().collect(),
                owned: OwnedBytesEstimate::default(),
            },
        );
        Ok(Arc::new(BudgetViewGuard {
            ledger: self.inner.clone(),
            nonce,
        }))
    }
    /// Observe the actual queue allocations. Candidate clones share these bodies;
    /// the ledger retains Weak only and serializes a new allocation once.
    pub(crate) fn register_queue_view<'a>(
        &self,
        scope: &InputEvidenceScope,
        category: InputMemoryCategory,
        records: impl IntoIterator<Item = &'a Arc<crate::types::MessageEnvelope>>,
    ) -> Result<Arc<BudgetViewGuard>> {
        valid_scope(scope)?;
        let sk = ScopeKey::from(scope);
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        state.prune_dead();
        let mut unique = BTreeMap::new();
        for message in records {
            if message.agent_id != scope.agent_id {
                return Err(InputEvidenceBudgetError::InvalidInput(
                    "queue scope differs from view",
                ));
            }
            let id = Arc::as_ptr(message) as usize;
            if let Some(entry) = state.allocations.get(&id) {
                if entry.scope != sk || !matches!(&entry.weak, AllocationWeak::Queue(_)) {
                    return Err(InputEvidenceBudgetError::InvalidInput(
                        "queue allocation scope changed",
                    ));
                }
                unique.insert(id, (message, entry.bytes));
            } else {
                // Counting writer avoids allocating a second encoded body. This
                // measures serialized envelope bytes, not allocator capacity.
                #[derive(Default)]
                struct EncodedBytes(u64);
                impl std::io::Write for EncodedBytes {
                    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                        self.0 = self
                            .0
                            .checked_add(bytes.len() as u64)
                            .ok_or_else(|| std::io::Error::other("encoded size overflow"))?;
                        Ok(bytes.len())
                    }
                    fn flush(&mut self) -> std::io::Result<()> {
                        Ok(())
                    }
                }
                let mut count = EncodedBytes::default();
                serde_json::to_writer(&mut count, message.as_ref()).map_err(|_| {
                    InputEvidenceBudgetError::InvalidInput("queue serialization failed")
                })?;
                unique.insert(id, (message, count.0));
            }
        }
        let nonce = state.nonce()?;
        for (id, (message, bytes)) in &unique {
            let entry = state
                .allocations
                .entry(*id)
                .or_insert_with(|| AllocationEntry {
                    weak: AllocationWeak::Queue(Arc::downgrade(message)),
                    scope: sk.clone(),
                    bytes: *bytes,
                    memberships: BTreeMap::new(),
                });
            entry.memberships.insert(nonce, category);
        }
        state.views.insert(
            nonce,
            ViewEntry {
                scope: sk,
                category,
                allocations: unique.into_keys().collect(),
                owned: OwnedBytesEstimate::default(),
            },
        );
        Ok(Arc::new(BudgetViewGuard {
            ledger: self.inner.clone(),
            nonce,
        }))
    }
    /// Work views observe the same immutable record allocations as publication
    /// and capture. The ledger holds no strong domain/body reference.
    pub(crate) fn register_work_view<'a>(
        &self,
        scope: &InputEvidenceScope,
        category: InputMemoryCategory,
        records: impl IntoIterator<Item = &'a Arc<QualifiedWorkRecord>>,
    ) -> Result<Arc<BudgetViewGuard>> {
        valid_scope(scope)?;
        let sk = ScopeKey::from(scope);
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        state.prune_dead();
        let mut unique = BTreeMap::new();
        for record in records {
            if &record.scope != scope || record.record.agent_id() != scope.agent_id {
                return Err(InputEvidenceBudgetError::InvalidInput(
                    "Work scope differs from view",
                ));
            }
            let id = Arc::as_ptr(record) as usize;
            let bytes = u64::try_from(record.resident_bytes)
                .map_err(|_| InputEvidenceBudgetError::CounterOverflow)?;
            if let Some(entry) = state.allocations.get(&id) {
                if entry.scope != sk
                    || !matches!(&entry.weak, AllocationWeak::Work(_))
                    || entry.bytes != bytes
                {
                    return Err(InputEvidenceBudgetError::InvalidInput(
                        "Work allocation scope/charge changed",
                    ));
                }
            }
            unique.insert(id, (record, bytes));
        }
        let nonce = state.nonce()?;
        for (id, (record, bytes)) in &unique {
            let entry = state
                .allocations
                .entry(*id)
                .or_insert_with(|| AllocationEntry {
                    weak: AllocationWeak::Work(Arc::downgrade(record)),
                    scope: sk.clone(),
                    bytes: *bytes,
                    memberships: BTreeMap::new(),
                });
            entry.memberships.insert(nonce, category);
        }
        state.views.insert(
            nonce,
            ViewEntry {
                scope: sk,
                category,
                allocations: unique.into_keys().collect(),
                owned: OwnedBytesEstimate::default(),
            },
        );
        Ok(Arc::new(BudgetViewGuard {
            ledger: self.inner.clone(),
            nonce,
        }))
    }
    /// One guard per owned data allocation; enclosing Arc clones share the guard.
    pub(crate) fn register_owned_bytes(
        &self,
        scope: &InputEvidenceScope,
        category: InputMemoryCategory,
        bytes: OwnedBytesEstimate,
    ) -> Result<Arc<BudgetViewGuard>> {
        valid_scope(scope)?;
        bytes.total()?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        let nonce = state.nonce()?;
        state.views.insert(
            nonce,
            ViewEntry {
                scope: ScopeKey::from(scope),
                category,
                allocations: BTreeSet::new(),
                owned: bytes,
            },
        );
        Ok(Arc::new(BudgetViewGuard {
            ledger: self.inner.clone(),
            nonce,
        }))
    }
    /// No record/root/domain Arc escapes in a report. Weak bookkeeping can
    /// observe allocations that outlive every registered view as unclassified.
    pub(crate) fn report(&self) -> Result<InputEvidenceBudgetReport> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| InputEvidenceBudgetError::Poisoned)?;
        state.prune_dead();
        let mut scopes = BTreeMap::<ScopeKey, ScopeMemoryReport>::new();
        let mut report = InputEvidenceBudgetReport {
            limits: self.inner.limits,
            scopes: Vec::new(),
            pending: PendingUsage::default(),
            categories: BTreeMap::new(),
            physical_records: AllocationUsage::default(),
            retained_unclassified: AllocationUsage::default(),
            reclaimable_history: AllocationUsage::default(),
            owned: OwnedBytesEstimate::default(),
            observed_bytes: 0,
        };
        for (sk, usage) in &state.pending_usage {
            scopes
                .entry(sk.clone())
                .or_insert_with(|| empty_scope(sk))
                .pending = *usage;
            report.pending.count = add(report.pending.count, usage.count)?;
            report.pending.body_bytes = add(report.pending.body_bytes, usage.body_bytes)?;
            report.pending.unresolved_count =
                add(report.pending.unresolved_count, usage.unresolved_count)?;
        }
        for entry in state.allocations.values() {
            let scope = scopes
                .entry(entry.scope.clone())
                .or_insert_with(|| empty_scope(&entry.scope));
            add_usage(&mut scope.physical_records, entry.bytes)?;
            add_usage(&mut report.physical_records, entry.bytes)?;
            let categories = entry.memberships.values().copied().collect::<BTreeSet<_>>();
            if categories.is_empty() {
                add_usage(&mut scope.retained_unclassified, entry.bytes)?;
                add_usage(&mut report.retained_unclassified, entry.bytes)?;
            }
            for category in &categories {
                add_usage(scope.categories.entry(*category).or_default(), entry.bytes)?;
                add_usage(report.categories.entry(*category).or_default(), entry.bytes)?;
            }
            if categories.contains(&InputMemoryCategory::History)
                && !categories.iter().any(|c| {
                    matches!(
                        c,
                        InputMemoryCategory::Current
                            | InputMemoryCategory::Candidate
                            | InputMemoryCategory::Pins
                            | InputMemoryCategory::PreparedPrompt
                    )
                })
            {
                add_usage(&mut scope.reclaimable_history, entry.bytes)?;
                add_usage(&mut report.reclaimable_history, entry.bytes)?;
            }
        }
        for view in state.views.values() {
            let bytes = view.owned.total()?;
            if bytes == 0 {
                continue;
            }
            let scope = scopes
                .entry(view.scope.clone())
                .or_insert_with(|| empty_scope(&view.scope));
            add_owned(&mut scope.owned, view.owned)?;
            add_owned(&mut report.owned, view.owned)?;
            add_usage(scope.categories.entry(view.category).or_default(), bytes)?;
            add_usage(report.categories.entry(view.category).or_default(), bytes)?;
        }
        report.observed_bytes = add(report.physical_records.bytes, report.owned.total()?)?;
        report.scopes = scopes.into_values().collect();
        Ok(report)
    }
    /// Explicit pressure only, never hidden eviction. History remains charged
    /// while also pinned; reclaimable history is reported separately.
    pub(crate) fn check_history_budget(&self) -> Result<()> {
        let report = self.report()?;
        for scope in &report.scopes {
            let bytes = scope
                .categories
                .get(&InputMemoryCategory::History)
                .map_or(0, |x| x.bytes);
            check(
                BudgetResource::HistoryAgentBytes,
                Some(&scope.scope),
                bytes,
                0,
                self.inner.limits.history_per_agent_bytes,
            )?;
        }
        let bytes = report
            .categories
            .get(&InputMemoryCategory::History)
            .map_or(0, |x| x.bytes);
        check(
            BudgetResource::HistoryHostBytes,
            None,
            bytes,
            0,
            self.inner.limits.history_host_bytes,
        )
    }
}
/// Encoded footprint estimate without a temporary encoded body allocation.
pub(crate) fn serialized_bytes(value: &impl Serialize) -> Result<u64> {
    #[derive(Default)]
    struct Count(u64);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| std::io::Error::other("encoded size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count::default();
    serde_json::to_writer(&mut count, value)
        .map_err(|_| InputEvidenceBudgetError::InvalidInput("footprint serialization failed"))?;
    Ok(count.0)
}

fn empty_scope(sk: &ScopeKey) -> ScopeMemoryReport {
    ScopeMemoryReport {
        scope: sk.scope(),
        pending: PendingUsage::default(),
        categories: BTreeMap::new(),
        physical_records: AllocationUsage::default(),
        retained_unclassified: AllocationUsage::default(),
        reclaimable_history: AllocationUsage::default(),
        owned: OwnedBytesEstimate::default(),
    }
}
fn matching<'a>(
    state: &'a LedgerState,
    ticket: &PendingAdmissionTicket,
) -> Result<&'a PendingEntry> {
    state
        .pending
        .get(&(ticket.scope.clone(), ticket.message_id.clone()))
        .filter(|e| e.nonce == ticket.nonce)
        .ok_or(InputEvidenceBudgetError::TicketMismatch)
}
fn valid_scope(s: &InputEvidenceScope) -> Result<()> {
    if s.agent_id.is_empty() || s.identity_incarnation == 0 {
        return Err(InputEvidenceBudgetError::InvalidInput(
            "scope needs agent ID and nonzero incarnation",
        ));
    }
    Ok(())
}
fn check(
    resource: BudgetResource,
    scope: Option<&InputEvidenceScope>,
    used: u64,
    requested: u64,
    limit: u64,
) -> Result<()> {
    if add(used, requested)? > limit {
        return Err(InputEvidenceBudgetError::ResourceExhausted {
            resource,
            scope: scope.cloned(),
            used,
            requested,
            limit,
        });
    }
    Ok(())
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or(InputEvidenceBudgetError::CounterOverflow)
}
fn add_usage(usage: &mut AllocationUsage, bytes: u64) -> Result<()> {
    usage.allocations = add(usage.allocations, 1)?;
    usage.bytes = add(usage.bytes, bytes)?;
    Ok(())
}
fn add_owned(total: &mut OwnedBytesEstimate, bytes: OwnedBytesEstimate) -> Result<()> {
    total.payload_bytes = add(total.payload_bytes, bytes.payload_bytes)?;
    total.metadata_estimate_bytes =
        add(total.metadata_estimate_bytes, bytes.metadata_estimate_bytes)?;
    Ok(())
}
#[cfg(test)]
#[path = "input_evidence_budget_tests.rs"]
mod tests;
