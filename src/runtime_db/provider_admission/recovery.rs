//! Durable dispatch accounting, not a retry scheduler or execution authority.

use std::collections::BTreeSet;

use crate::domain::execution_protocol::{
    AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionSource,
    WorkItemExecutionRecord, WorkItemExecutionState,
};
use crate::runtime_db::transitions::execution_protocol_repository::authority_fences_tx;
use crate::types::{
    AgentIdentityRecord, AgentRegistryStatus, AgentStatus, WorkItemRecord, WorkItemState,
};

use super::*;

const MAX_REQUESTS: u32 = 3;

/// Construct from the validated root attempt, never current focus or a recovery
/// message's newly allocated attempt. The runtime must still validate lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderRecoveryBinding {
    agent_id: String,
    root_attempt_id: String,
    root_message_id: String,
    source: ExecutionSource,
    owner: ExecutionBinding,
    fences: AdmittedFences,
    chain_fingerprint: String,
}

impl ProviderRecoveryBinding {
    pub fn from_root_attempt(attempt: &ExecutionAttempt, chain_fingerprint: &str) -> Result<Self> {
        let root_message_id = attempt
            .source_message_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .context("provider recovery requires a durable root message")?;
        ensure!(
            !attempt.agent_id.is_empty()
                && !attempt.attempt_id.is_empty()
                && attempt.source.generation > 0
                && attempt.recovery_of_attempt_id.is_none()
                && !chain_fingerprint.is_empty(),
            "provider recovery requires canonical source and chain identity"
        );
        Ok(Self {
            agent_id: attempt.agent_id.clone(),
            root_attempt_id: attempt.attempt_id.clone(),
            root_message_id: root_message_id.clone(),
            source: attempt.source.clone(),
            owner: attempt.binding.clone(),
            fences: attempt.admitted_fences.clone(),
            chain_fingerprint: chain_fingerprint.to_owned(),
        })
    }

    fn key(&self) -> Result<String> {
        Ok(format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(self)?)
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRecoveryBudgetKind {
    Network,
    /// The caller must authenticate the root operator ingress before granting
    /// this kind. This value is not itself proof of operator authority.
    Operator,
}

impl ProviderRecoveryBudgetKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Operator => "operator",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "network" => Ok(Self::Network),
            "operator" => Ok(Self::Operator),
            _ => anyhow::bail!("invalid provider recovery budget kind"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRecoveryBudgetState {
    Ready,
    Cooling,
    BudgetExhausted,
    Resolved,
}

impl ProviderRecoveryBudgetState {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "ready" => Ok(Self::Ready),
            "cooling" => Ok(Self::Cooling),
            "budget_exhausted" => Ok(Self::BudgetExhausted),
            "resolved" => Ok(Self::Resolved),
            _ => anyhow::bail!("invalid provider recovery budget state"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRecoveryEpisode {
    key: String,
    revision: i64,
    epoch: i64,
    pub kind: ProviderRecoveryBudgetKind,
    pub state: ProviderRecoveryBudgetState,
    pub request_count: u32,
    pub total_requests: i64,
    pub candidate_cursor: u32,
    pub not_before: Option<DateTime<Utc>>,
    pub side_effect_boundary: bool,
    failed_epochs: i64,
}

impl ProviderRecoveryEpisode {
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRecoveryDispatchDenial {
    DuplicateRequest,
    StaleExecution(ProviderDispatchRejection),
    StaleEpisode,
    StaleAdmission,
    EpisodePaused,
    BudgetExhausted,
    ScopeAlreadyAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDispatchRejection {
    AttemptMissing,
    AttemptNotOpen,
    AttemptTerminal,
    SnapshotSourceMismatch,
    SnapshotOwnerMismatch,
    SnapshotFencesMismatch,
    SnapshotProvenanceMismatch,
    SnapshotAncestryMismatch,
    SourceGenerationMismatch,
    InvalidAdmittedFences,
    AuthorityControlMissing,
    AuthorityStopped,
    AuthorityIdentityMissing,
    AuthorityIdentityInactive,
    AuthorityFenceMismatch,
    OwnerMissing,
    OwnerMismatch,
    OwnerNotOpen,
    WorkItemRevisionMismatch,
    LaneGenerationMismatch,
    EpisodeBindingMissing,
    EpisodeBindingMismatch,
    RootMismatch,
    RootBindingMismatch,
    AncestryCycle,
    AncestorMissing,
    AncestorNotClosed,
    AncestorOwnerMismatch,
}

#[derive(Debug)]
pub struct ProviderRecoveryDispatch {
    episode_key: String,
    request_id: String,
    epoch: i64,
    revision: i64,
}

#[derive(Debug)]
pub enum ProviderRecoveryDispatchResult {
    Charged(ProviderRecoveryDispatch),
    Denied(ProviderRecoveryDispatchDenial),
}

#[derive(Debug)]
pub enum ProviderRecoveryAdmissionReservation {
    Granted(ProviderAdmissionLease),
    Blocked(ProviderAdmissionBlocked),
    Denied(ProviderRecoveryDispatchDenial),
}

pub enum ProviderRecoveryRequestOutcome<'a> {
    AvailableResponse,
    NonRecoveryResponse,
    NetworkFailure(&'a ProviderAdmissionScope),
    RateLimited(&'a ProviderAdmissionScope),
    PaymentRequired(&'a ProviderAdmissionScope),
    OtherFailure,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RecordedScope {
    scope: ProviderAdmissionScope,
    was_probe: bool,
}

fn canonical_attempt(
    tx: &Transaction<'_>,
    agent_id: &str,
    attempt_id: &str,
) -> Result<Option<ExecutionAttempt>> {
    let raw = tx
        .query_row(
            "SELECT a.lifecycle_state, a.payload_json FROM execution_protocol_attempts a
             JOIN execution_protocol_partitions p ON p.agent_id = a.agent_id
             WHERE a.agent_id = ?1 AND a.attempt_id = ?2",
            params![agent_id, attempt_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    raw.map(|(state, payload)| {
        let attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
        ensure!(
            attempt.agent_id == agent_id
                && attempt.attempt_id == attempt_id
                && serde_json::to_value(attempt.state)? == serde_json::Value::String(state),
            "provider dispatch canonical attempt projection mismatch"
        );
        Ok(attempt)
    })
    .transpose()
}

fn dispatch_authority_rejection(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<Option<ProviderDispatchRejection>> {
    let control = tx
        .query_row(
            "SELECT status, control_revision FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((status, control_revision)) = control else {
        return Ok(Some(ProviderDispatchRejection::AuthorityControlMissing));
    };
    let status: AgentStatus =
        serde_json::from_str(&format!("\"{status}\"")).context("decoding agent control status")?;
    if status == AgentStatus::Stopped {
        return Ok(Some(ProviderDispatchRejection::AuthorityStopped));
    }
    let control_revision =
        u64::try_from(control_revision).context("agent control revision is negative")?;
    ensure!(
        control_revision > 0,
        "agent control revision must be nonzero"
    );
    let identity = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(identity) = identity else {
        return Ok(Some(ProviderDispatchRejection::AuthorityIdentityMissing));
    };
    let identity: AgentIdentityRecord =
        serde_json::from_str(&identity).context("decoding host identity")?;
    if identity.status != AgentRegistryStatus::Active {
        return Ok(Some(ProviderDispatchRejection::AuthorityIdentityInactive));
    }
    Ok(None)
}

fn dispatch_attempt_rejection(
    tx: &Transaction<'_>,
    episode: &ProviderRecoveryEpisode,
    expected: &ExecutionAttempt,
) -> Result<Option<ProviderDispatchRejection>> {
    let Some(attempt) = canonical_attempt(tx, &expected.agent_id, &expected.attempt_id)? else {
        return Ok(Some(ProviderDispatchRejection::AttemptMissing));
    };
    // Queue claim and turn startup fill run/turn IDs after the guard snapshot.
    // Only stable authorization fields are compared with that snapshot.
    if attempt.state != ExecutionAttemptState::Open || expected.state != ExecutionAttemptState::Open
    {
        return Ok(Some(ProviderDispatchRejection::AttemptNotOpen));
    }
    if attempt.source_message_id != expected.source_message_id || attempt.source != expected.source
    {
        return Ok(Some(ProviderDispatchRejection::SnapshotSourceMismatch));
    }
    if attempt.binding != expected.binding {
        return Ok(Some(ProviderDispatchRejection::SnapshotOwnerMismatch));
    }
    if attempt.admitted_fences != expected.admitted_fences {
        return Ok(Some(ProviderDispatchRejection::SnapshotFencesMismatch));
    }
    if attempt.recovery_of_attempt_id != expected.recovery_of_attempt_id {
        return Ok(Some(ProviderDispatchRejection::SnapshotAncestryMismatch));
    }
    if attempt.provenance != expected.provenance {
        return Ok(Some(ProviderDispatchRejection::SnapshotProvenanceMismatch));
    }
    if attempt.terminal_at.is_some() || attempt.terminal_outcome_id.is_some() {
        return Ok(Some(ProviderDispatchRejection::AttemptTerminal));
    }
    if attempt.source.generation == 0
        || attempt.source.generation != attempt.admitted_fences.source_revision
    {
        return Ok(Some(ProviderDispatchRejection::SourceGenerationMismatch));
    }
    if attempt.admitted_fences.agent_control_revision == 0
        || attempt.admitted_fences.host_registry_revision == 0
    {
        return Ok(Some(ProviderDispatchRejection::InvalidAdmittedFences));
    }
    if let Some(rejection) = dispatch_authority_rejection(tx, &attempt.agent_id)? {
        return Ok(Some(rejection));
    }
    // Expected lifecycle denials were classified above in this transaction.
    // Do not turn corrupt authority records or storage errors into a denial.
    let authority = authority_fences_tx(tx, &attempt.agent_id)?;
    // Control revisions also advance on benign state persistence. The canonical
    // authority helper checks stop/deletion; only host identity is a stable fence.
    if authority.host_registry_revision != attempt.admitted_fences.host_registry_revision {
        return Ok(Some(ProviderDispatchRejection::AuthorityFenceMismatch));
    }
    match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => {
            let owner = tx
                .query_row(
                    "SELECT w.payload_json, e.payload_json
                     FROM work_items w JOIN execution_protocol_work_items e
                       ON e.agent_id = w.agent_id AND e.work_item_id = w.work_item_id
                     WHERE w.agent_id = ?1 AND w.work_item_id = ?2",
                    params![attempt.agent_id, work_item_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let Some((work, execution)) = owner else {
                return Ok(Some(ProviderDispatchRejection::OwnerMissing));
            };
            let work: WorkItemRecord = serde_json::from_str(&work)?;
            let execution: WorkItemExecutionRecord = serde_json::from_str(&execution)?;
            if work.agent_id != attempt.agent_id || work.id != *work_item_id {
                return Ok(Some(ProviderDispatchRejection::OwnerMismatch));
            }
            if work.state != WorkItemState::Open {
                return Ok(Some(ProviderDispatchRejection::OwnerNotOpen));
            }
            if work.revision != execution.source_revision
                || !attempt
                    .admitted_fences
                    .work_item_source_revision
                    .is_some_and(|revision| revision > 0 && revision <= execution.source_revision)
            {
                return Ok(Some(ProviderDispatchRejection::WorkItemRevisionMismatch));
            }
            if !matches!(
                &execution.state,
                WorkItemExecutionState::InFlight { generation, attempt_id }
                    if attempt_id == &attempt.attempt_id
                        && Some(*generation) == attempt.admitted_fences.work_item_generation
            ) {
                return Ok(Some(ProviderDispatchRejection::LaneGenerationMismatch));
            }
        }
        ExecutionBinding::AgentLifecycle { agent_id } if agent_id != &attempt.agent_id => {
            return Ok(Some(ProviderDispatchRejection::OwnerMismatch));
        }
        _ => {}
    }

    let binding = tx
        .query_row(
            "SELECT binding_json FROM provider_recovery_episodes WHERE episode_key = ?1",
            [&episode.key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(binding) = binding else {
        return Ok(Some(ProviderDispatchRejection::EpisodeBindingMissing));
    };
    let binding: ProviderRecoveryBinding = serde_json::from_str(&binding)?;
    if binding.agent_id != attempt.agent_id
        || binding.owner != attempt.binding
        || binding.key()? != episode.key
    {
        return Ok(Some(ProviderDispatchRejection::EpisodeBindingMismatch));
    }
    let mut ancestor = attempt;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(ancestor.attempt_id.clone()) {
            return Ok(Some(ProviderDispatchRejection::AncestryCycle));
        }
        let Some(prior_id) = &ancestor.recovery_of_attempt_id else {
            if ancestor.attempt_id != binding.root_attempt_id {
                return Ok(Some(ProviderDispatchRejection::RootMismatch));
            }
            // Construction only checks in-memory root semantics; SQL/decoding
            // failures from canonical_attempt must still propagate.
            return Ok(
                match ProviderRecoveryBinding::from_root_attempt(
                    &ancestor,
                    &binding.chain_fingerprint,
                ) {
                    Ok(root) if root == binding => None,
                    _ => Some(ProviderDispatchRejection::RootBindingMismatch),
                },
            );
        };
        let Some(prior) = canonical_attempt(tx, &ancestor.agent_id, prior_id)? else {
            return Ok(Some(ProviderDispatchRejection::AncestorMissing));
        };
        if prior.state == ExecutionAttemptState::Open {
            return Ok(Some(ProviderDispatchRejection::AncestorNotClosed));
        }
        if prior.binding != binding.owner {
            return Ok(Some(ProviderDispatchRejection::AncestorOwnerMismatch));
        }
        ancestor = prior;
    }
}

fn read_episode(connection: &Connection, key: &str) -> Result<Option<ProviderRecoveryEpisode>> {
    let raw = connection
        .query_row(
            "SELECT revision, epoch, budget_kind, state, request_count, total_requests,
                    candidate_cursor, not_before, side_effect_boundary, failed_epochs
             FROM provider_recovery_episodes WHERE episode_key = ?1",
            [key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, u32>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, bool>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()?;
    raw.map(|raw| {
        let state = ProviderRecoveryBudgetState::parse(&raw.3)?;
        let not_before = raw
            .7
            .map(|time| time.parse::<DateTime<Utc>>())
            .transpose()?;
        ensure!(
            raw.0 > 0
                && raw.1 > 0
                && raw.4 <= MAX_REQUESTS
                && raw.5 >= i64::from(raw.4)
                && raw.9 >= 0,
            "invalid persisted provider recovery counters"
        );
        ensure!(
            !matches!(
                state,
                ProviderRecoveryBudgetState::Cooling | ProviderRecoveryBudgetState::BudgetExhausted
            ) || not_before.is_some(),
            "paused provider recovery requires a deadline"
        );
        Ok(ProviderRecoveryEpisode {
            key: key.to_owned(),
            revision: raw.0,
            epoch: raw.1,
            kind: ProviderRecoveryBudgetKind::parse(&raw.2)?,
            state,
            request_count: raw.4,
            total_requests: raw.5,
            candidate_cursor: raw.6,
            not_before,
            side_effect_boundary: raw.8,
            failed_epochs: raw.9,
        })
    })
    .transpose()
}

fn episode_floor(episode: &ProviderRecoveryEpisode, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    now.checked_add_signed(
        ProviderCooldownPolicy::Transport.delay(episode.failed_epochs.max(1), &episode.key),
    )
    .context("provider recovery floor overflow")
}

fn attempted_scopes(
    connection: &Connection,
    episode: &ProviderRecoveryEpisode,
) -> Result<BTreeSet<ProviderAdmissionScope>> {
    let mut attempted = BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT scopes_json, failed_scope_json, outcome FROM provider_recovery_requests
         WHERE episode_key = ?1 AND epoch = ?2",
    )?;
    let rows = statement.query_map(params![episode.key, episode.epoch], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (scopes, failed_scope, outcome) = row?;
        if episode.kind == ProviderRecoveryBudgetKind::Operator {
            let payment_failure = outcome.as_deref() == Some("payment_required");
            if payment_failure {
                if let Some(scope) = failed_scope {
                    attempted.insert(serde_json::from_str::<ProviderAdmissionScope>(&scope)?);
                }
            }
            for recorded in serde_json::from_str::<Vec<RecordedScope>>(&scopes)? {
                // A settled 402 consumes its candidate, not a healthy shared
                // quota. Any actual policy probe still consumes its one grant.
                if recorded.was_probe
                    || (!payment_failure
                        && matches!(
                            recorded.scope.kind,
                            ScopeKind::Quota | ScopeKind::RouteThrottle | ScopeKind::Candidate
                        ))
                {
                    attempted.insert(recorded.scope);
                }
            }
        } else if let Some(scope) = failed_scope {
            attempted.insert(serde_json::from_str::<ProviderAdmissionScope>(&scope)?);
        }
    }
    Ok(attempted)
}

impl RuntimeDb {
    /// Idempotent even after restart. Reads never renew an operator grant, reset
    /// counters, or begin another epoch.
    pub fn open_provider_recovery_episode(
        &self,
        binding: &ProviderRecoveryBinding,
        kind: ProviderRecoveryBudgetKind,
        now: DateTime<Utc>,
    ) -> Result<ProviderRecoveryEpisode> {
        let key = binding.key()?;
        let binding_json = serde_json::to_string(binding)?;
        self.transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO provider_recovery_episodes (
                    episode_key, binding_json, budget_kind, revision, epoch, state,
                    request_count, total_requests, candidate_cursor, failed_epochs,
                    side_effect_boundary, opened_at, updated_at
                 ) VALUES (?1, ?2, ?3, 1, 1, 'ready', 0, 0, 0, 0, 0, ?4, ?4)",
                params![key, binding_json, kind.as_str(), timestamp(now)],
            )?;
            let stored: String = tx.query_row(
                "SELECT binding_json FROM provider_recovery_episodes WHERE episode_key = ?1",
                [&key],
                |row| row.get(0),
            )?;
            ensure!(stored == binding_json, "provider recovery binding mismatch");
            read_episode(tx, &key)?.context("provider recovery episode missing")
        })
    }

    pub fn provider_recovery_episode(&self, key: &str) -> Result<Option<ProviderRecoveryEpisode>> {
        self.with_read(|connection| read_episode(connection, key))
    }

    /// Screen candidates without reserving admission or changing recovery budgets.
    pub fn provider_recovery_candidate_attempted(
        &self,
        expected: &ProviderRecoveryEpisode,
        scopes: &[ProviderAdmissionScope],
    ) -> Result<bool> {
        self.with_read(|connection| {
            let episode = read_episode(connection, &expected.key)?
                .context("provider recovery episode missing")?;
            ensure!(
                episode.epoch == expected.epoch && episode.revision == expected.revision,
                "stale provider recovery episode"
            );
            let attempted = attempted_scopes(connection, &episode)?;
            Ok(scopes.iter().any(|scope| attempted.contains(scope)))
        })
    }

    /// The pre-turn reservation and final dispatch use the same episode fence.
    /// Operator policy probes still obey server deadlines and active leases.
    pub fn reserve_provider_recovery_admission(
        &self,
        expected: &ProviderRecoveryEpisode,
        scopes: &[ProviderAdmissionScope],
        now: DateTime<Utc>,
        lease_duration: Duration,
    ) -> Result<ProviderRecoveryAdmissionReservation> {
        use ProviderRecoveryAdmissionReservation as Reservation;
        use ProviderRecoveryDispatchDenial as Denial;
        self.transaction(|tx| {
            let Some(episode) = read_episode(tx, &expected.key)? else {
                return Ok(Reservation::Denied(Denial::StaleEpisode));
            };
            if episode.epoch != expected.epoch || episode.revision != expected.revision {
                return Ok(Reservation::Denied(Denial::StaleEpisode));
            }
            if episode.request_count >= MAX_REQUESTS {
                return Ok(Reservation::Denied(Denial::BudgetExhausted));
            }
            if episode.state != ProviderRecoveryBudgetState::Ready {
                return Ok(Reservation::Denied(Denial::EpisodePaused));
            }
            let attempted = attempted_scopes(tx, &episode)?;
            if scopes.iter().any(|scope| attempted.contains(scope)) {
                return Ok(Reservation::Denied(Denial::ScopeAlreadyAttempted));
            }
            Ok(
                match reserve_admission(
                    tx,
                    scopes,
                    now,
                    lease_duration,
                    episode.kind == ProviderRecoveryBudgetKind::Operator,
                )? {
                    ProviderAdmissionReservation::Granted(lease) => Reservation::Granted(lease),
                    ProviderAdmissionReservation::Blocked(blocked) => Reservation::Blocked(blocked),
                },
            )
        })
    }

    /// Charge immediately before network dispatch, after local construction.
    /// A duplicate ID is a denial, not permission to repeat an unknown request.
    pub fn charge_provider_recovery_dispatch(
        &self,
        expected: &ProviderRecoveryEpisode,
        admission: &ProviderAdmissionLease,
        request_id: &str,
        next_candidate_cursor: u32,
        now: DateTime<Utc>,
    ) -> Result<ProviderRecoveryDispatchResult> {
        self.transaction(|tx| {
            Self::charge_provider_recovery_dispatch_tx(
                tx,
                expected,
                admission,
                request_id,
                next_candidate_cursor,
                now,
            )
        })
    }

    /// Lifecycle validation and charging share the writer transaction. The
    /// supplied attempt is a comparison fence, never execution authority.
    pub fn charge_provider_recovery_dispatch_for_attempt(
        &self,
        expected: &ProviderRecoveryEpisode,
        admission: &ProviderAdmissionLease,
        request_id: &str,
        next_candidate_cursor: u32,
        attempt: &ExecutionAttempt,
        now: DateTime<Utc>,
    ) -> Result<ProviderRecoveryDispatchResult> {
        self.transaction(|tx| {
            if let Some(rejection) = dispatch_attempt_rejection(tx, expected, attempt)? {
                return Ok(ProviderRecoveryDispatchResult::Denied(
                    ProviderRecoveryDispatchDenial::StaleExecution(rejection),
                ));
            }
            Self::charge_provider_recovery_dispatch_tx(
                tx,
                expected,
                admission,
                request_id,
                next_candidate_cursor,
                now,
            )
        })
    }

    fn charge_provider_recovery_dispatch_tx(
        tx: &Transaction<'_>,
        expected: &ProviderRecoveryEpisode,
        admission: &ProviderAdmissionLease,
        request_id: &str,
        next_candidate_cursor: u32,
        now: DateTime<Utc>,
    ) -> Result<ProviderRecoveryDispatchResult> {
        use ProviderRecoveryDispatchDenial as Denial;
        ensure!(
            !request_id.is_empty(),
            "provider dispatch requires a request ID"
        );
        let deny = |reason| Ok(ProviderRecoveryDispatchResult::Denied(reason));
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_recovery_requests WHERE request_id = ?1)",
            [request_id],
            |row| row.get::<_, bool>(0),
        )? {
            return deny(Denial::DuplicateRequest);
        }
        let Some(mut episode) = read_episode(tx, &expected.key)? else {
            return deny(Denial::StaleEpisode);
        };
        if episode.epoch != expected.epoch || episode.revision != expected.revision {
            return deny(Denial::StaleEpisode);
        }
        if episode.request_count >= MAX_REQUESTS {
            return deny(Denial::BudgetExhausted);
        }
        if episode.state != ProviderRecoveryBudgetState::Ready {
            return deny(Denial::EpisodePaused);
        }
        if !lease_is_valid(tx, admission, now)? {
            return deny(Denial::StaleAdmission);
        }
        let attempted = attempted_scopes(tx, &episode)?;
        if admission
            .scopes
            .iter()
            .any(|lease| attempted.contains(&lease.scope))
        {
            return deny(Denial::ScopeAlreadyAttempted);
        }
        let recorded: Vec<_> = admission
            .scopes
            .iter()
            .map(|lease| RecordedScope {
                scope: lease.scope.clone(),
                was_probe: lease.probe_token.is_some(),
            })
            .collect();
        episode.revision += 1;
        episode.request_count += 1;
        let exhausted = episode.request_count == MAX_REQUESTS;
        if exhausted {
            episode.failed_epochs = episode.failed_epochs.saturating_add(1);
        }
        let floor = exhausted
            .then(|| episode_floor(&episode, now))
            .transpose()?;
        tx.execute(
            "INSERT INTO provider_recovery_requests (
                    request_id, episode_key, epoch, revision, scopes_json, dispatched_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                request_id,
                episode.key,
                episode.epoch,
                episode.revision,
                serde_json::to_string(&recorded)?,
                timestamp(now)
            ],
        )?;
        tx.execute(
                "UPDATE provider_recovery_episodes SET revision = ?2, request_count = ?3,
                    total_requests = total_requests + 1, candidate_cursor = MAX(candidate_cursor, ?4),
                    state = ?5, not_before = ?6, failed_epochs = ?7, updated_at = ?8
                 WHERE episode_key = ?1",
                params![episode.key, episode.revision, episode.request_count,
                    next_candidate_cursor, if exhausted { "budget_exhausted" } else { "ready" },
                    floor.map(timestamp), episode.failed_epochs, timestamp(now)],
            )?;
        Ok(ProviderRecoveryDispatchResult::Charged(
            ProviderRecoveryDispatch {
                episode_key: episode.key,
                request_id: request_id.to_owned(),
                epoch: episode.epoch,
                revision: episode.revision,
            },
        ))
    }

    /// Only the current request can settle the budget. Availability incidents
    /// have their own matching admission fences and are not cleared here.
    pub fn record_provider_recovery_request_outcome(
        &self,
        dispatch: &ProviderRecoveryDispatch,
        outcome: ProviderRecoveryRequestOutcome<'_>,
        side_effect_boundary: bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.transaction(|tx| {
            let Some(episode) = read_episode(tx, &dispatch.episode_key)? else {
                return Ok(false);
            };
            if episode.epoch != dispatch.epoch || episode.revision != dispatch.revision {
                return Ok(false);
            }
            let Some(scopes) = tx
                .query_row(
                    "SELECT scopes_json FROM provider_recovery_requests
                 WHERE request_id = ?1 AND episode_key = ?2 AND epoch = ?3
                    AND revision = ?4 AND settled_at IS NULL",
                    params![
                        dispatch.request_id,
                        dispatch.episode_key,
                        dispatch.epoch,
                        dispatch.revision
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(false);
            };
            let (label, failed_scope, resolved) = match outcome {
                ProviderRecoveryRequestOutcome::AvailableResponse => {
                    ("available_response", None, true)
                }
                ProviderRecoveryRequestOutcome::NonRecoveryResponse => {
                    let has_failure = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM provider_recovery_requests
                         WHERE episode_key = ?1 AND epoch = ?2
                           AND (outcome IN ('network_failure', 'rate_limited', 'payment_required', 'other_failure')
                             OR (settled_at IS NULL AND request_id != ?3)))",
                        params![episode.key, episode.epoch, dispatch.request_id],
                        |row| row.get::<_, bool>(0),
                    )?;
                    ("non_recovery_response", None, !has_failure)
                }
                ProviderRecoveryRequestOutcome::OtherFailure => {
                    let has_recovery_failure = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM provider_recovery_requests
                         WHERE episode_key = ?1 AND epoch = ?2
                           AND (outcome IN ('network_failure', 'rate_limited', 'payment_required')
                             OR (settled_at IS NULL AND request_id != ?3)))",
                        params![episode.key, episode.epoch, dispatch.request_id],
                        |row| row.get::<_, bool>(0),
                    )?;
                    (
                        "other_failure",
                        None,
                        episode.kind == ProviderRecoveryBudgetKind::Network
                            && !has_recovery_failure,
                    )
                }
                ProviderRecoveryRequestOutcome::NetworkFailure(scope) => {
                    ensure!(
                        scope.kind == ScopeKind::Transport
                            && serde_json::from_str::<Vec<RecordedScope>>(&scopes)?
                                .iter()
                                .any(|recorded| recorded.scope == *scope),
                        "network failure scope was not admitted"
                    );
                    (
                        "network_failure",
                        Some(serde_json::to_string(scope)?),
                        false,
                    )
                }
                ProviderRecoveryRequestOutcome::RateLimited(scope) => {
                    ensure!(
                        matches!(scope.kind, ScopeKind::Quota | ScopeKind::RouteThrottle)
                            && serde_json::from_str::<Vec<RecordedScope>>(&scopes)?
                                .iter()
                                .any(|recorded| recorded.scope == *scope),
                        "rate limit scope was not admitted"
                    );
                    ("rate_limited", Some(serde_json::to_string(scope)?), false)
                }
                ProviderRecoveryRequestOutcome::PaymentRequired(scope) => {
                    ensure!(
                        scope.kind == ScopeKind::Candidate
                            && serde_json::from_str::<Vec<RecordedScope>>(&scopes)?
                                .iter()
                                .any(|recorded| recorded.scope == *scope),
                        "payment required scope was not admitted"
                    );
                    ("payment_required", Some(serde_json::to_string(scope)?), false)
                }
            };
            tx.execute(
                "UPDATE provider_recovery_requests SET outcome = ?2,
                    failed_scope_json = ?3, settled_at = ?4 WHERE request_id = ?1",
                params![dispatch.request_id, label, failed_scope, timestamp(now)],
            )?;
            let floor = if resolved {
                None
            } else if episode.request_count >= MAX_REQUESTS {
                Some(episode_floor(&episode, now)?.max(episode.not_before.unwrap_or(now)))
            } else {
                episode.not_before
            };
            tx.execute(
                "UPDATE provider_recovery_episodes SET revision = revision + 1,
                    state = CASE WHEN ?2 THEN 'resolved' ELSE state END,
                    not_before = ?3, side_effect_boundary = MAX(side_effect_boundary, ?4),
                    updated_at = ?5 WHERE episode_key = ?1",
                params![
                    episode.key,
                    resolved,
                    floor.map(timestamp),
                    side_effect_boundary,
                    timestamp(now)
                ],
            )?;
            Ok(true)
        })
    }

    /// Park once after candidate selection. This does not mutate queue, waits,
    /// task ownership, or execution protocol state.
    pub fn park_provider_recovery_episode(
        &self,
        expected: &ProviderRecoveryEpisode,
        earliest_candidate: DateTime<Utc>,
        untried_candidates: bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.transaction(|tx| {
            let Some(mut episode) = read_episode(tx, &expected.key)? else {
                return Ok(false);
            };
            if episode.epoch != expected.epoch
                || episode.revision != expected.revision
                || episode.state == ProviderRecoveryBudgetState::Resolved
            {
                return Ok(false);
            }
            let floor = if episode.state == ProviderRecoveryBudgetState::Ready
                && episode.request_count == 0
            {
                if earliest_candidate > now {
                    earliest_candidate
                } else {
                    // No availability deadline: pace rechecks without counting
                    // a failure or escalating an earlier epoch's failure floor.
                    now.checked_add_signed(ProviderCooldownPolicy::Transport.delay(1, &episode.key))
                        .context("provider recovery recheck overflow")?
                }
            } else if episode.state == ProviderRecoveryBudgetState::Ready {
                episode.failed_epochs = episode.failed_epochs.saturating_add(1);
                episode_floor(&episode, now)?
            } else {
                episode
                    .not_before
                    .context("paused provider recovery requires a deadline")?
            };
            let until = earliest_candidate.max(floor);
            let state = if untried_candidates && episode.request_count >= MAX_REQUESTS {
                "budget_exhausted"
            } else {
                "cooling"
            };
            if episode.not_before == Some(until)
                && ProviderRecoveryBudgetState::parse(state)? == episode.state
            {
                return Ok(false);
            }
            tx.execute(
                "UPDATE provider_recovery_episodes SET revision = revision + 1, state = ?2,
                    not_before = ?3, failed_epochs = ?4, updated_at = ?5 WHERE episode_key = ?1",
                params![
                    episode.key,
                    state,
                    timestamp(until),
                    episode.failed_epochs,
                    timestamp(now)
                ],
            )?;
            Ok(true)
        })
    }

    /// Explicit recheck only. A competing caller loses the revision CAS; reads,
    /// restarts, and unrelated wakes cannot replenish the budget.
    pub fn begin_provider_recovery_probe_epoch(
        &self,
        expected: &ProviderRecoveryEpisode,
        now: DateTime<Utc>,
    ) -> Result<Option<ProviderRecoveryEpisode>> {
        self.transaction(|tx| {
            let Some(episode) = read_episode(tx, &expected.key)? else { return Ok(None) };
            if episode.epoch != expected.epoch || episode.revision != expected.revision
                || episode.state == ProviderRecoveryBudgetState::Ready
                || (episode.state != ProviderRecoveryBudgetState::Resolved
                    && !episode.not_before.is_some_and(|until| until <= now))
            {
                return Ok(None);
            }
            tx.execute(
                "UPDATE provider_recovery_episodes SET revision = revision + 1, epoch = epoch + 1,
                    budget_kind = 'network', state = 'ready', request_count = 0, not_before = NULL,
                    candidate_cursor = CASE WHEN state = 'resolved' THEN 0 ELSE candidate_cursor END,
                    failed_epochs = CASE WHEN state = 'resolved' THEN 0 ELSE failed_epochs END,
                    updated_at = ?2 WHERE episode_key = ?1",
                params![episode.key, timestamp(now)],
            )?;
            read_episode(tx, &episode.key)
        })
    }
}

#[cfg(test)]
mod tests;
