//! Provider availability never owns the input, task result, or business wait.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

use crate::{
    config::{ModelRouteCapability, ModelRouteRef},
    domain::execution_protocol::ExecutionAttempt,
    provider::{
        build_candidate, classify_provider_error, provider_transport_diagnostics,
        ProviderDispatchGuard, ProviderFailureKind, ProviderThrottleKind, ProviderTurnResponse,
    },
    runtime_db::provider_admission::{
        ProviderAdmissionLease, ProviderAdmissionScope, ProviderCooldownPolicy,
        ProviderQueueParking, ProviderRecoveryAdmissionReservation, ProviderRecoveryBinding,
        ProviderRecoveryBudgetKind, ProviderRecoveryBudgetState, ProviderRecoveryDispatch,
        ProviderRecoveryDispatchDenial, ProviderRecoveryDispatchResult, ProviderRecoveryEpisode,
        ProviderRecoveryRequestOutcome,
    },
    runtime_db::RuntimeDb,
    types::{AdmissionContext, AuditEvent, AuthorityClass, MessageEnvelope, MessageOrigin},
};

use super::{RuntimeHandle, RuntimeInner};

// A lease permits controlled takeover after a crashed owner, not exactly-once
// external IO and not a new wall-clock timeout for a streaming model request.
const PROBE_LEASE: Duration = Duration::minutes(10);

mod local_failure;
pub(super) use local_failure::local_dispatch_failure_settlement;

#[derive(Debug, Clone)]
struct Candidate {
    model: ModelRouteRef,
    scopes: Vec<ProviderAdmissionScope>,
    quota: Option<ProviderAdmissionScope>,
    rate_limit: ProviderAdmissionScope,
    connection: ProviderAdmissionScope,
    request: ProviderAdmissionScope,
}

struct Chain {
    fingerprint: String,
    candidates: Vec<Candidate>,
}

pub(super) enum ProviderTurnGate {
    Bypass,
    Granted(Arc<ProviderTurnAdmission>),
    Parked,
}

#[derive(Debug, thiserror::Error)]
#[error("provider admission deferred before dispatch: {0}")]
struct DispatchDeferred(LocalDispatchDenial);

pub(super) fn is_parked_dispatch_rejection(error: &anyhow::Error) -> bool {
    crate::provider::is_dispatch_guard_rejected(error)
        && error.chain().any(|source| {
            source
                .downcast_ref::<DispatchDeferred>()
                .is_some_and(|denial| denial.0.is_availability_pause())
        })
}

#[derive(Debug, Clone, Copy)]
enum LocalDispatchDenial {
    ScopeCooling { not_before: DateTime<Utc> },
    ChainCooling { not_before: DateTime<Utc> },
    EpisodeChanged,
    Reservation(ProviderRecoveryDispatchDenial),
    Dispatch(ProviderRecoveryDispatchDenial),
}

impl std::fmt::Display for LocalDispatchDenial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl LocalDispatchDenial {
    fn is_availability_pause(self) -> bool {
        matches!(
            self,
            Self::ScopeCooling { .. }
                | Self::ChainCooling { .. }
                | Self::Reservation(
                    ProviderRecoveryDispatchDenial::EpisodePaused
                        | ProviderRecoveryDispatchDenial::BudgetExhausted
                )
                | Self::Dispatch(
                    ProviderRecoveryDispatchDenial::EpisodePaused
                        | ProviderRecoveryDispatchDenial::BudgetExhausted
                )
        )
    }
}

#[derive(Debug)]
struct RequestState {
    lease: Option<ProviderAdmissionLease>,
    dispatch: Option<ProviderRecoveryDispatch>,
    unknown_dispatch: bool,
    side_effect_boundary: bool,
}

pub(super) struct ProviderTurnAdmission {
    db: RuntimeDb,
    clock: Arc<dyn super::clock::Clock>,
    attempt: ExecutionAttempt,
    episode_key: String,
    chain_fingerprint: String,
    candidate: Candidate,
    next_cursor: u32,
    state: Mutex<RequestState>,
}

impl std::fmt::Debug for ProviderTurnAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderTurnAdmission")
            .field("attempt_id", &self.attempt.attempt_id)
            .field("episode_key", &self.episode_key)
            .field("model", &self.candidate.model)
            .finish_non_exhaustive()
    }
}

impl ProviderTurnAdmission {
    fn deny(&self, state: &mut RequestState, reason: LocalDispatchDenial) -> Result<()> {
        // A rejected send is not IO. Never release an earlier unknown request.
        if state.dispatch.is_none() && !state.unknown_dispatch {
            if let Some(lease) = state.lease.as_ref() {
                self.db.release_provider_admission(lease)?;
                state.lease = None;
            }
        }
        if reason.is_availability_pause() {
            let episode = self.episode()?;
            let not_before = match reason {
                LocalDispatchDenial::ScopeCooling { not_before }
                | LocalDispatchDenial::ChainCooling { not_before } => not_before,
                _ => episode
                    .not_before
                    .context("provider pause deadline missing")?,
            };
            // Persist the original deadline before releasing execution ownership.
            // Interrupted replay retains this attempt's canonical recovery root.
            self.db.park_provider_queue(&ProviderQueueParking {
                agent_id: self.attempt.agent_id.clone(),
                message_id: self
                    .attempt
                    .source_message_id
                    .clone()
                    .context("provider source message missing")?,
                chain_fingerprint: self.chain_fingerprint.clone(),
                episode_key: episode.key().to_owned(),
                not_before,
                reason: if episode.state == ProviderRecoveryBudgetState::BudgetExhausted {
                    "recovery_budget_exhausted"
                } else {
                    "chain_cooling"
                }
                .into(),
            })?;
        }
        Err(DispatchDeferred(reason).into())
    }

    pub(super) fn model(&self) -> &ModelRouteRef {
        &self.candidate.model
    }

    fn episode(&self) -> Result<ProviderRecoveryEpisode> {
        self.db
            .provider_recovery_episode(&self.episode_key)?
            .context("provider recovery episode disappeared")
    }

    pub(super) fn mark_side_effect_boundary(&self, crossed: bool) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| anyhow!("provider admission poisoned"))?
            .side_effect_boundary |= crossed;
        Ok(())
    }
}

impl Drop for ProviderTurnAdmission {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            // Once dispatched, an unknown result retains its charge and lease.
            if state.dispatch.is_none() && !state.unknown_dispatch {
                if let Some(lease) = state.lease.take() {
                    let _ = self.db.release_provider_admission(&lease);
                }
            }
        }
    }
}

impl ProviderDispatchGuard for ProviderTurnAdmission {
    fn before_dispatch(&self) -> Result<()> {
        let now = self.clock.now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("provider admission poisoned"))?;
        if state.dispatch.take().is_some() {
            state.unknown_dispatch = true;
        }
        let mut episode = self.episode()?;
        if episode.state == ProviderRecoveryBudgetState::Resolved {
            let next = self.db.begin_provider_recovery_probe_epoch(&episode, now)?;
            let Some(next) = next else {
                return self.deny(&mut state, LocalDispatchDenial::EpisodeChanged);
            };
            episode = next;
        }
        if state.lease.is_none() {
            state.lease = match self.db.reserve_provider_recovery_admission(
                &episode,
                &self.candidate.scopes,
                now,
                PROBE_LEASE,
            )? {
                ProviderRecoveryAdmissionReservation::Granted(lease) => Some(lease),
                ProviderRecoveryAdmissionReservation::Blocked(blocked) => {
                    return self.deny(
                        &mut state,
                        LocalDispatchDenial::ScopeCooling {
                            not_before: blocked.not_before,
                        },
                    );
                }
                ProviderRecoveryAdmissionReservation::Denied(reason) => {
                    return self.deny(&mut state, LocalDispatchDenial::Reservation(reason));
                }
            };
        }
        let lease = state.lease.as_ref().expect("reservation was checked");
        match self.db.charge_provider_recovery_dispatch_for_attempt(
            &episode,
            lease,
            &uuid::Uuid::new_v4().to_string(),
            self.next_cursor,
            &self.attempt,
            now,
        )? {
            ProviderRecoveryDispatchResult::Charged(dispatch) => {
                // A previous HTTP result not reported by the adapter is unknown:
                // it remains charged, never inferred successful or refunded.
                state.dispatch = Some(dispatch);
                Ok(())
            }
            ProviderRecoveryDispatchResult::Denied(reason) => {
                self.deny(&mut state, LocalDispatchDenial::Dispatch(reason))
            }
        }
    }

    fn allows_auxiliary_dispatch(&self) -> Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("provider admission poisoned"))?;
        // A scarce recovery probe must request an actual model response.
        Ok(!state.lease.as_ref().is_some_and(|lease| lease.is_probe()))
    }

    fn after_request(
        &self,
        result: std::result::Result<Option<&ProviderTurnResponse>, &anyhow::Error>,
    ) -> Result<()> {
        let now = self.clock.now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("provider admission poisoned"))?;
        let Some(dispatch) = state.dispatch.take() else {
            return Ok(()); // Local construction or the final guard rejected IO.
        };
        let lease = state
            .lease
            .take()
            .context("dispatched provider lease missing")?;
        let outcome = match result {
            Ok(Some(response))
                if response.blocks.iter().any(|block| match block {
                    crate::provider::ModelBlock::Text { text } => !text.trim().is_empty(),
                    crate::provider::ModelBlock::ToolUse { id, name, .. } => {
                        !id.is_empty() && !name.is_empty()
                    }
                    _ => false,
                }) =>
            {
                self.db.record_provider_admission_success(&lease, now)?;
                ProviderRecoveryRequestOutcome::AvailableResponse
            }
            Ok(None) => {
                self.db.release_provider_admission(&lease)?;
                ProviderRecoveryRequestOutcome::NonRecoveryResponse
            }
            Err(error) => {
                let kind = classify_provider_error(error).kind;
                let failure = match kind {
                    ProviderFailureKind::Timeout
                        if provider_transport_diagnostics(error).is_some_and(|diagnostics| {
                            diagnostics.stage == "request_send"
                                && diagnostics
                                    .reqwest
                                    .as_ref()
                                    .is_some_and(|error| error.is_connect)
                        }) =>
                    {
                        Some((
                            &self.candidate.connection,
                            ProviderCooldownPolicy::Transport,
                        ))
                    }
                    ProviderFailureKind::Connection => Some((
                        &self.candidate.connection,
                        ProviderCooldownPolicy::Transport,
                    )),
                    ProviderFailureKind::Timeout | ProviderFailureKind::ServerError => {
                        Some((&self.candidate.request, ProviderCooldownPolicy::Transport))
                    }
                    ProviderFailureKind::RateLimited => {
                        let hard = provider_transport_diagnostics(error)
                            .and_then(|diagnostics| diagnostics.throttle.as_ref())
                            .is_some_and(|throttle| {
                                throttle.kind == ProviderThrottleKind::HardQuota
                            });
                        Some((
                            &self.candidate.rate_limit,
                            if hard {
                                ProviderCooldownPolicy::HardQuota
                            } else {
                                ProviderCooldownPolicy::TransientQuota
                            },
                        ))
                    }
                    _ => None,
                };
                if let Some((scope, policy)) = failure {
                    let server_not_before =
                        provider_transport_diagnostics(error).and_then(|diagnostics| {
                            diagnostics.retry_after_not_before.max(
                                diagnostics
                                    .throttle
                                    .as_ref()
                                    .and_then(|throttle| throttle.not_before),
                            )
                        });
                    self.db.record_provider_admission_failure(
                        &lease,
                        scope,
                        policy,
                        now,
                        server_not_before,
                        &self.candidate.model.as_string(),
                    )?;
                    if policy == ProviderCooldownPolicy::Transport {
                        ProviderRecoveryRequestOutcome::NetworkFailure(scope)
                    } else {
                        ProviderRecoveryRequestOutcome::RateLimited(scope)
                    }
                } else {
                    self.db.release_provider_admission(&lease)?;
                    if lease.is_probe() {
                        ProviderRecoveryRequestOutcome::OtherFailure
                    } else {
                        ProviderRecoveryRequestOutcome::NonRecoveryResponse
                    }
                }
            }
            _ => {
                self.db.release_provider_admission(&lease)?;
                ProviderRecoveryRequestOutcome::OtherFailure
            }
        };
        self.db.record_provider_recovery_request_outcome(
            &dispatch,
            outcome,
            state.side_effect_boundary,
            now,
        )?;
        Ok(())
    }
}

/// A turn-local handle with cancellation-safe release of undispatched claims.
pub(super) struct ActiveAdmission {
    inner: Arc<RuntimeInner>,
}

impl Drop for ActiveAdmission {
    fn drop(&mut self) {
        if let Ok(mut current) = self.inner.turn_provider_admission.lock() {
            current.take();
        }
    }
}

impl RuntimeHandle {
    pub(super) fn local_provider_dispatch_error(&self, mut error: anyhow::Error) -> anyhow::Error {
        if !crate::provider::is_dispatch_guard_rejected(&error) {
            return error;
        }
        let Some(denial) = error
            .chain()
            .find_map(|source| source.downcast_ref::<DispatchDeferred>())
            .map(|error| error.0)
        else {
            // The transport marker also covers storage/infrastructure failures.
            // Keep their domain, code and retryability rather than inventing a denial.
            return error;
        };
        let disposition = if denial.is_availability_pause() {
            "parked"
        } else {
            "needs_repair"
        };
        // The marker describes only this send. Earlier requests remain charged
        // and their diagnostics stay attached to the original error chain.
        let category = denial.to_string();
        if let Err(audit_error) = self.inner.storage.append_event(&AuditEvent::legacy(
            "provider_dispatch_rejected",
            serde_json::json!({
                "dispatch_occurred": false,
                "denial": category,
                "recovery_disposition": disposition,
            }),
        )) {
            error = error.context(audit_error);
        }
        error.context(
            crate::runtime_error::RuntimeError::new(
                crate::runtime_error::RuntimeErrorDomain::Conflict,
                "provider_dispatch_rejected",
                "Local provider dispatch admission rejected this send before IO.",
            )
            .with_safe_context("stage", "before_dispatch")
            .with_safe_context("predicate", category)
            .with_safe_context("recovery_disposition", disposition)
            .with_recovery_hint(
                "Recheck the canonical execution and admission state before resuming the retained source.",
            ),
        )
    }

    fn provider_admission_chain(&self, state: &crate::types::AgentState) -> Result<Option<Chain>> {
        let snapshot = self.inner.config_snapshot.load();
        let Some(reconfig) = snapshot.provider_reconfig.as_ref() else {
            return Ok(None); // Static/local providers have no configured HTTP path.
        };
        let config = &reconfig.config;
        let mut candidates = Vec::new();
        for model in snapshot
            .model_catalog
            .provider_chain(state.model_override.as_ref())
            .into_iter()
            .take(if config.provider_fallback_disabled() {
                1
            } else {
                usize::MAX
            })
        {
            let Some(route) = snapshot.model_catalog.resolve_explicit_model_route(
                &snapshot.base_context_config,
                &model,
                ModelRouteCapability::Turn,
            ) else {
                continue;
            };
            let Ok(provider) = build_candidate(config, &model) else {
                continue;
            };
            let endpoint = &route.endpoint.runtime_config;
            // Include the actual configured proxy path, never infer a host-wide
            // outage from one route. Only hashes leave this local construction.
            let proxies: Vec<_> = [
                "HTTPS_PROXY",
                "https_proxy",
                "HTTP_PROXY",
                "http_proxy",
                "ALL_PROXY",
                "all_proxy",
                "NO_PROXY",
                "no_proxy",
            ]
            .map(|name| std::env::var(name).unwrap_or_default())
            .into();
            let path = format!("{:?}:{}:{proxies:?}", endpoint.transport, endpoint.base_url);
            let connection = ProviderAdmissionScope::transport("connection", &path)?;
            let request = ProviderAdmissionScope::transport(
                "request",
                &format!("{path}:{}", route.route_ref.as_string()),
            )?;
            let quota = provider
                .provider
                .quota_identity()
                .as_ref()
                .map(ProviderAdmissionScope::quota);
            let mut scopes: Vec<_> = quota.iter().cloned().collect();
            scopes.extend([connection.clone(), request.clone()]);
            let rate_limit = match quota.as_ref() {
                Some(scope) => scope.clone(),
                None => {
                    let scope = ProviderAdmissionScope::route_throttle(&format!(
                        "{path}:{}",
                        route.route_ref.as_string()
                    ))?;
                    scopes.push(scope.clone());
                    scope
                }
            };
            candidates.push(Candidate {
                model,
                scopes,
                quota,
                rate_limit,
                connection,
                request,
            });
        }
        ensure!(
            !candidates.is_empty(),
            "no configured model candidate is available for admission"
        );
        let identity: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                // Supplemental throttle admission must not rebind unfinished
                // pre-upgrade roots to a fresh recovery budget.
                (
                    candidate.model.as_string(),
                    candidate
                        .quota
                        .iter()
                        .chain([&candidate.connection, &candidate.request])
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        Ok(Some(Chain {
            fingerprint: format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&identity)?)
            ),
            candidates,
        }))
    }

    pub(super) fn prepare_provider_turn_admission(
        &self,
        state: &crate::types::AgentState,
        message: &MessageEnvelope,
        attempt: &ExecutionAttempt,
    ) -> Result<ProviderTurnGate> {
        let Some(chain) = self.provider_admission_chain(state)? else {
            return Ok(ProviderTurnGate::Bypass);
        };
        let now = self.now();
        if self
            .inner
            .runtime_db
            .provider_queue_parking(&message.agent_id, &message.id)?
            .is_some_and(|parking| {
                parking.chain_fingerprint == chain.fingerprint && parking.not_before > now
            })
        {
            return Ok(ProviderTurnGate::Parked);
        }
        // Canonical admission already resolved source/owner eligibility. Walk
        // that exact predecessor chain, never select a root by current focus.
        let protocol = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&message.agent_id)?;
        let mut root = attempt.clone();
        let mut visited = std::collections::BTreeSet::new();
        while let Some(prior) = root.recovery_of_attempt_id.as_ref() {
            ensure!(visited.insert(prior.clone()), "provider root cycle");
            root = protocol
                .as_ref()
                .and_then(|state| state.attempts.get(prior))
                .context("canonical provider predecessor missing")?
                .clone();
        }
        let binding = ProviderRecoveryBinding::from_root_attempt(&root, &chain.fingerprint)?;
        let root_message = self
            .inner
            .storage
            .read_message_by_id(
                root.source_message_id
                    .as_deref()
                    .context("provider root message missing")?,
            )?
            .context("persisted provider root input missing")?;
        let operator = root_message.authority_class == AuthorityClass::OperatorInstruction
            && matches!(root_message.origin, MessageOrigin::Operator { .. })
            && matches!(
                root_message.admission_context,
                Some(
                    AdmissionContext::ControlAuthenticated
                        | AdmissionContext::OperatorTransportAuthenticated
                        | AdmissionContext::LocalProcess
                )
            );
        let mut episode = self.inner.runtime_db.open_provider_recovery_episode(
            &binding,
            if operator {
                ProviderRecoveryBudgetKind::Operator
            } else {
                ProviderRecoveryBudgetKind::Network
            },
            now,
        )?;
        if episode.state != ProviderRecoveryBudgetState::Ready {
            if episode.state == ProviderRecoveryBudgetState::Resolved
                || episode.not_before.is_some_and(|until| until <= now)
            {
                let Some(next) = self
                    .inner
                    .runtime_db
                    .begin_provider_recovery_probe_epoch(&episode, now)?
                else {
                    return Ok(ProviderTurnGate::Parked);
                };
                episode = next;
            } else {
                self.persist_provider_queue_parking(
                    message,
                    &chain,
                    &episode,
                    episode
                        .not_before
                        .context("provider pause deadline missing")?,
                    episode.state == ProviderRecoveryBudgetState::BudgetExhausted,
                )?;
                return Ok(ProviderTurnGate::Parked);
            }
        }
        self.select_provider_turn_candidate(
            message,
            attempt,
            &chain,
            &episode,
            &std::collections::BTreeSet::new(),
        )
    }

    fn select_provider_turn_candidate(
        &self,
        message: &MessageEnvelope,
        attempt: &ExecutionAttempt,
        chain: &Chain,
        episode: &ProviderRecoveryEpisode,
        excluded: &std::collections::BTreeSet<String>,
    ) -> Result<ProviderTurnGate> {
        let now = self.now();
        let len = chain.candidates.len();
        let start = usize::try_from(episode.candidate_cursor)
            .unwrap_or(len)
            .min(len);
        let mut order: Vec<_> = (start..len).chain(0..start).collect();
        // A new operator input tries healthy candidates before exercising its
        // one-shot permission to probe a local-policy cooldown.
        if episode.kind == ProviderRecoveryBudgetKind::Operator {
            let mut cooling = Vec::new();
            for &index in &order {
                if self
                    .inner
                    .runtime_db
                    .provider_admission_blocked(&chain.candidates[index].scopes, now)?
                    .is_some()
                {
                    cooling.push(index);
                }
            }
            order.sort_by_key(|index| cooling.contains(index));
        }
        let mut earliest: Option<DateTime<Utc>> = None;
        let mut untried = false;
        for index in order {
            let candidate = &chain.candidates[index];
            let blocked = self
                .inner
                .runtime_db
                .provider_admission_blocked(&candidate.scopes, now)?;
            if let Some(blocked) = blocked.as_ref() {
                earliest =
                    Some(earliest.map_or(blocked.not_before, |at| at.min(blocked.not_before)));
            }
            if excluded.contains(&candidate.model.as_string()) {
                continue;
            }
            if self
                .inner
                .runtime_db
                .provider_recovery_candidate_attempted(&episode, &candidate.scopes)?
            {
                continue;
            }
            untried = true;
            match self.inner.runtime_db.reserve_provider_recovery_admission(
                &episode,
                &candidate.scopes,
                now,
                PROBE_LEASE,
            )? {
                ProviderRecoveryAdmissionReservation::Granted(lease) => {
                    self.inner
                        .runtime_db
                        .clear_provider_queue_parking(&message.agent_id, &message.id)?;
                    return Ok(ProviderTurnGate::Granted(Arc::new(ProviderTurnAdmission {
                        db: self.inner.runtime_db.clone(),
                        clock: self.inner.clock.clone(),
                        attempt: attempt.clone(),
                        episode_key: episode.key().to_owned(),
                        chain_fingerprint: chain.fingerprint.clone(),
                        candidate: candidate.clone(),
                        next_cursor: u32::try_from(index + 1).context("provider chain too long")?,
                        state: Mutex::new(RequestState {
                            lease: Some(lease),
                            dispatch: None,
                            unknown_dispatch: false,
                            side_effect_boundary: episode.side_effect_boundary,
                        }),
                    })));
                }
                ProviderRecoveryAdmissionReservation::Blocked(blocked) => {
                    earliest =
                        Some(earliest.map_or(blocked.not_before, |at| at.min(blocked.not_before)));
                }
                ProviderRecoveryAdmissionReservation::Denied(_) => {
                    if blocked.is_none() {
                        earliest = Some(now);
                    }
                }
            }
        }
        self.inner.runtime_db.park_provider_recovery_episode(
            &episode,
            earliest.unwrap_or(now),
            untried,
            now,
        )?;
        let parked = self
            .inner
            .runtime_db
            .provider_recovery_episode(episode.key())?
            .context("parked provider episode missing")?;
        self.persist_provider_queue_parking(
            message,
            &chain,
            &parked,
            parked
                .not_before
                .context("parked provider deadline missing")?,
            parked.state == ProviderRecoveryBudgetState::BudgetExhausted,
        )?;
        Ok(ProviderTurnGate::Parked)
    }

    /// Replan an unsent candidate cooldown, never execution or budget fences.
    pub(super) async fn reselect_cooling_provider(
        &self,
        error: anyhow::Error,
        excluded: &mut std::collections::BTreeSet<String>,
    ) -> Result<ModelRouteRef> {
        if !crate::provider::is_dispatch_guard_rejected(&error) {
            return Err(error);
        }
        let Some(cooling_until) =
            error.chain().find_map(
                |source| match source.downcast_ref::<DispatchDeferred>()?.0 {
                    LocalDispatchDenial::ScopeCooling { not_before } => Some(not_before),
                    _ => None,
                },
            )
        else {
            return Err(error);
        };
        let current = self
            .active_provider_admission()?
            .context("cooling provider admission missing")?;
        excluded.insert(current.model().as_string());
        let state = self.inner.agent.lock().await.state.clone();
        let chain = self
            .provider_admission_chain(&state)?
            .context("cooling provider chain disappeared")?;
        ensure!(
            chain.fingerprint == current.chain_fingerprint,
            "provider chain changed during candidate replan"
        );
        let episode = current.episode()?;
        // before_dispatch already opened the epoch; replan cannot renew it.
        ensure!(
            episode.state == ProviderRecoveryBudgetState::Ready,
            "provider episode changed during candidate replan"
        );
        let message = self
            .inner
            .storage
            .read_message_by_id(
                current
                    .attempt
                    .source_message_id
                    .as_deref()
                    .context("provider source message missing")?,
            )?
            .context("provider source message disappeared")?;
        match self.select_provider_turn_candidate(
            &message,
            &current.attempt,
            &chain,
            &episode,
            excluded,
        )? {
            ProviderTurnGate::Granted(next) => {
                let model = next.model().clone();
                let mut active = self
                    .inner
                    .turn_provider_admission
                    .lock()
                    .map_err(|_| anyhow!("turn provider admission poisoned"))?;
                ensure!(
                    active
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &current)),
                    "provider admission changed during candidate replan"
                );
                *active = Some(next);
                self.inner.storage.append_event(&AuditEvent::legacy(
                    "provider_candidate_reselected",
                    serde_json::json!({
                        "from_model_ref": current.model(),
                        "model_ref": model,
                        "episode_key": episode.key(),
                        "dispatch_occurred": false,
                        "predicate": "scope_cooling",
                        "cooling_until": cooling_until,
                    }),
                ))?;
                Ok(model)
            }
            ProviderTurnGate::Parked => {
                let parking = self
                    .inner
                    .runtime_db
                    .provider_queue_parking(&message.agent_id, &message.id)?
                    .context("exhausted provider chain was not parked")?;
                Err(
                    self.local_provider_dispatch_error(error.context(DispatchDeferred(
                        LocalDispatchDenial::ChainCooling {
                            not_before: parking.not_before,
                        },
                    ))),
                )
            }
            ProviderTurnGate::Bypass => {
                unreachable!("configured candidate selection cannot bypass")
            }
        }
    }

    fn persist_provider_queue_parking(
        &self,
        message: &MessageEnvelope,
        chain: &Chain,
        episode: &ProviderRecoveryEpisode,
        not_before: DateTime<Utc>,
        exhausted: bool,
    ) -> Result<()> {
        let parking = ProviderQueueParking {
            agent_id: message.agent_id.clone(),
            message_id: message.id.clone(),
            chain_fingerprint: chain.fingerprint.clone(),
            episode_key: episode.key().to_owned(),
            not_before,
            reason: if exhausted {
                "recovery_budget_exhausted"
            } else {
                "chain_cooling"
            }
            .into(),
        };
        if self.inner.runtime_db.park_provider_queue(&parking)? {
            self.inner.storage.append_event(&AuditEvent::legacy(
                "provider_queue_parked",
                serde_json::json!({
                    "agent_id": parking.agent_id,
                    "message_id": parking.message_id,
                    "reason": parking.reason,
                    "not_before": parking.not_before,
                    "episode_key": parking.episode_key,
                    "request_count": episode.request_count,
                    "candidate_cursor": episode.candidate_cursor,
                    "untried_candidates": exhausted,
                }),
            ))?;
        }
        Ok(())
    }

    pub(super) fn active_provider_admission(&self) -> Result<Option<Arc<ProviderTurnAdmission>>> {
        Ok(self
            .inner
            .turn_provider_admission
            .lock()
            .map_err(|_| anyhow!("turn provider admission poisoned"))?
            .clone())
    }

    pub(super) fn activate_provider_admission(
        &self,
        admission: Option<Arc<ProviderTurnAdmission>>,
    ) -> Result<ActiveAdmission> {
        let mut active = self
            .inner
            .turn_provider_admission
            .lock()
            .map_err(|_| anyhow!("turn provider admission poisoned"))?;
        ensure!(active.is_none(), "another provider turn is already active");
        *active = admission;
        Ok(ActiveAdmission {
            inner: self.inner.clone(),
        })
    }
}

#[cfg(test)]
mod tests;
