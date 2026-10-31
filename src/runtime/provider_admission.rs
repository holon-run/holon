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
        ProviderAdmissionLease, ProviderAdmissionScope, ProviderCooldownCause,
        ProviderCooldownPolicy, ProviderDispatchPause, ProviderQueueParking,
        ProviderRecoveryAdmissionReservation, ProviderRecoveryBinding, ProviderRecoveryBudgetKind,
        ProviderRecoveryBudgetState, ProviderRecoveryDispatch, ProviderRecoveryDispatchDenial,
        ProviderRecoveryDispatchResult, ProviderRecoveryEpisode, ProviderRecoveryRequestOutcome,
    },
    runtime_db::RuntimeDb,
    types::{
        AdmissionContext, AgentState, AgentStatus, AuditEvent, AuthorityClass, BriefRecord,
        MessageEnvelope, MessageKind, MessageOrigin,
    },
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
    payment: ProviderAdmissionScope,
    contract: ProviderAdmissionScope,
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

enum ProviderCandidateSelection {
    Granted(Arc<ProviderTurnAdmission>),
    Cooling {
        not_before: DateTime<Utc>,
        untried: bool,
    },
}

pub(super) enum CoolingContinuation {
    Reselect(ModelRouteRef),
    RetrySame,
    Interject,
}

enum CoolingReplanOutcome {
    Reselect(ModelRouteRef),
    Wait,
}

#[derive(Debug, thiserror::Error)]
#[error("provider admission deferred before dispatch: {0}")]
struct DispatchDeferred(LocalDispatchDenial, Option<Box<ProviderDispatchPause>>);

fn dispatch_denial(error: &anyhow::Error) -> Option<&DispatchDeferred> {
    // Anyhow typed context carries the latest denial but is not a source node.
    error.downcast_ref::<DispatchDeferred>().or_else(|| {
        error
            .chain()
            .find_map(|source| source.downcast_ref::<DispatchDeferred>())
    })
}

pub(super) fn dispatch_pause(error: &anyhow::Error) -> Option<&ProviderDispatchPause> {
    if !crate::provider::is_dispatch_guard_rejected(error) {
        return None;
    }
    dispatch_denial(error)?.1.as_deref()
}

/// User-facing summary for a confirmed local dispatch admission rejection.
///
/// Returns `None` unless the error already carries the
/// `provider_dispatch_rejected` descriptor, so provider IO failures keep their
/// real failure text. The summary avoids internal message-kind and transport
/// jargon; the descriptor, audit events, and source chain stay untouched.
pub(super) fn local_dispatch_rejection_user_summary(error: &anyhow::Error) -> Option<String> {
    let descriptor = crate::runtime_error::describe_runtime_error(error);
    if descriptor.code != "provider_dispatch_rejected" {
        return None;
    }
    let denial = dispatch_denial(error).map(|deferred| deferred.0);
    if matches!(
        denial,
        Some(LocalDispatchDenial::Dispatch(
            ProviderRecoveryDispatchDenial::StaleExecution(
                crate::runtime_db::provider_admission::ProviderDispatchRejection::WorkItemRevisionMismatch
                | crate::runtime_db::provider_admission::ProviderDispatchRejection::WorkItemRevisionDrift
            )
        ))
    ) {
        return Some(
            "The input was received, but this model send was blocked by a local source revision fault. \
             Repair is required; inspect scheduler-repair diagnostics. Retained inputs are not automatically replayed."
                .into(),
        );
    }
    let base = match denial {
        Some(LocalDispatchDenial::ScopeCooling { .. }) => {
            "The model channel is cooling down. This send was blocked."
        }
        Some(_) => "A local safety check blocked this model send.",
        // Legacy diagnostics cannot prove a reason or whether this send occurred.
        None => "The model request could not continue.",
    };
    let needs_repair = descriptor
        .safe_context
        .get("recovery_disposition")
        .is_some_and(|disposition| disposition == "needs_repair");
    Some(if needs_repair {
        format!("{base} Recovery needs attention; check the diagnostics before retrying.")
    } else {
        format!("{base} Check the diagnostics for recovery status.")
    })
}

#[derive(Clone, Copy)]
enum ProviderPauseDisposition {
    Waiting,
    SuspendedByControl,
}

impl ProviderPauseDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::SuspendedByControl => "suspended_by_control",
        }
    }
}

pub(super) struct ProviderPauseSettlement<'a> {
    pub pause: &'a ProviderDispatchPause,
    pub source: &'a MessageEnvelope,
    brief: BriefRecord,
    rejection: crate::runtime_error::RuntimeErrorDescriptor,
}

impl ProviderPauseSettlement<'_> {
    fn brief(&self, disposition: ProviderPauseDisposition) -> BriefRecord {
        let mut brief = self.brief.clone();
        let deadline = self.pause.parking.not_before.to_rfc3339();
        brief.text = match disposition {
            ProviderPauseDisposition::Waiting => format!(
                "Waiting for provider cooldown until {deadline}; input retained. Automatic retry is eligible only while the agent is running and all recovery fences hold."
            ),
            ProviderPauseDisposition::SuspendedByControl => format!(
                "Provider dispatch paused until {deadline}; input retained and the agent remains stopped. An explicit Start is required before recovery."
            ),
        };
        brief
    }

    pub(super) fn evidence(&self, state: &AgentState) -> Result<(BriefRecord, Vec<AuditEvent>)> {
        let disposition = if state.status == AgentStatus::Stopped {
            ProviderPauseDisposition::SuspendedByControl
        } else {
            ProviderPauseDisposition::Waiting
        };
        let brief = self.brief(disposition);
        let events = vec![
            AuditEvent::legacy(
                "provider_dispatch_paused",
                serde_json::json!({
                    "message_id": self.source.id,
                    "message_kind": self.source.kind,
                    "attempt_id": self.pause.attempt_id,
                    "turn_id": brief.turn_id,
                    "episode_key": self.pause.parking.episode_key,
                    "chain_fingerprint": self.pause.parking.chain_fingerprint,
                    "not_before": self.pause.parking.not_before.to_rfc3339(),
                    "denial": self.pause.denial,
                    "dispatch_phase": "before_send",
                    "recovery_disposition": disposition.as_str(),
                    "request_retryable": self.rejection.retryable,
                    "rejection": self.rejection,
                }),
            ),
            AuditEvent::legacy(
                "queue_entry_settled",
                serde_json::json!({
                    "message_id": self.source.id,
                    "message_kind": self.source.kind,
                    "status": crate::types::QueueEntryStatus::Interrupted,
                    "reason": "provider_dispatch_parked",
                    "recovery_disposition": disposition.as_str(),
                }),
            ),
            crate::types::brief_created_event_for(&brief)?,
        ];
        Ok((brief, events))
    }

    fn matches_brief(&self, stored: &BriefRecord) -> bool {
        [
            ProviderPauseDisposition::Waiting,
            ProviderPauseDisposition::SuspendedByControl,
        ]
        .into_iter()
        .any(|disposition| {
            let expected = self.brief(disposition);
            stored.id == expected.id
                && stored.text == expected.text
                && stored.turn_id == expected.turn_id
                && stored.turn_index == expected.turn_index
                && stored.kind == expected.kind
        })
    }
}

pub(super) fn is_parked_dispatch_rejection(error: &anyhow::Error) -> bool {
    dispatch_pause(error).is_some()
}

/// Inspect the denial only; callers must separately require a dispatch guard rejection.
pub(super) fn is_continuation_input_changed(error: &anyhow::Error) -> bool {
    matches!(
        dispatch_denial(error),
        Some(DispatchDeferred(LocalDispatchDenial::InputChanged, _))
    )
}

#[derive(Debug, Clone, Copy)]
enum LocalDispatchDenial {
    ScopeCooling { not_before: DateTime<Utc> },
    InputChanged,
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
                | Self::Reservation(ProviderRecoveryDispatchDenial::EpisodePaused)
                | Self::Dispatch(ProviderRecoveryDispatchDenial::EpisodePaused)
        )
    }
}

#[derive(Debug)]
struct RequestState {
    lease: Option<ProviderAdmissionLease>,
    dispatch: Option<ProviderRecoveryDispatch>,
    unknown_dispatch: bool,
    dispatched: bool,
    /// The in-flight dispatch's contract rejection is adapter-correctable
    /// in-turn (a prepared lossless replay or an auxiliary probe the adapter
    /// can skip); it is not candidate fatal. Consumed by the next
    /// `after_request` settlement.
    adapter_correctable_dispatch: bool,
    /// The in-flight dispatch's auth rejection is adapter-correctable
    /// in-turn (a prepared credential refresh and resend); it is not
    /// candidate fatal. Consumed by the next `after_request` settlement.
    auth_refreshable_dispatch: bool,
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
    recovery_reselection_reason: Option<&'static str>,
    state: Mutex<RequestState>,
    input_fence: Mutex<Option<(Arc<std::sync::atomic::AtomicU64>, u64)>>,
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
    pub(super) fn fence_operator_interjections(
        &self,
        revision: Arc<std::sync::atomic::AtomicU64>,
        expected: u64,
    ) -> Result<()> {
        *self
            .input_fence
            .lock()
            .map_err(|_| anyhow!("provider input fence poisoned"))? = Some((revision, expected));
        Ok(())
    }

    fn deny(&self, state: &mut RequestState, reason: LocalDispatchDenial) -> Result<()> {
        // A rejected send is not IO. Never release an earlier unknown request.
        if state.dispatch.is_none() && !state.unknown_dispatch {
            if let Some(lease) = state.lease.as_ref() {
                self.db.release_provider_admission(lease)?;
                state.lease = None;
            }
        }
        let mut pause = None;
        if reason.is_availability_pause()
            && !state.dispatched
            && !state.unknown_dispatch
            && !state.side_effect_boundary
        {
            let episode = self.episode()?;
            let not_before = match reason {
                LocalDispatchDenial::ScopeCooling { not_before } => not_before,
                _ => episode
                    .not_before
                    .context("provider pause deadline missing")?,
            };
            // Persist the original deadline before releasing execution ownership.
            // Interrupted replay retains this attempt's canonical recovery root.
            let parking = ProviderQueueParking {
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
            };
            self.db.park_provider_queue(&parking)?;
            pause = Some(Box::new(ProviderDispatchPause {
                parking,
                attempt_id: self.attempt.attempt_id.clone(),
                denial: match reason {
                    LocalDispatchDenial::ScopeCooling { .. } => "scope_cooling",
                    _ => "episode_paused",
                },
            }));
        }
        Err(DispatchDeferred(reason, pause).into())
    }

    pub(super) fn model(&self) -> &ModelRouteRef {
        &self.candidate.model
    }

    pub(super) fn recovery_reselection_reason(&self) -> Option<&'static str> {
        self.recovery_reselection_reason
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
    fn declare_adapter_correctable_dispatch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.adapter_correctable_dispatch = true;
        }
    }

    fn declare_auth_refreshable_dispatch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.auth_refreshable_dispatch = true;
        }
    }

    fn before_dispatch(&self) -> Result<()> {
        let now = self.clock.now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("provider admission poisoned"))?;
        if self
            .input_fence
            .lock()
            .map_err(|_| anyhow!("provider input fence poisoned"))?
            .as_ref()
            .is_some_and(|(revision, expected)| {
                revision.load(std::sync::atomic::Ordering::Acquire) != *expected
            })
        {
            return self.deny(&mut state, LocalDispatchDenial::InputChanged);
        }
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
                state.dispatched = true;
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
        let adapter_correctable_dispatch = state.adapter_correctable_dispatch;
        state.adapter_correctable_dispatch = false;
        let auth_refreshable_dispatch = state.auth_refreshable_dispatch;
        state.auth_refreshable_dispatch = false;
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
                    ProviderFailureKind::PaymentRequired => Some((
                        &self.candidate.payment,
                        ProviderCooldownPolicy::PaymentRequired,
                    )),
                    ProviderFailureKind::AuthError if auth_refreshable_dispatch => {
                        // The adapter refreshes the credential and resends
                        // this inference in-turn: consuming the candidate
                        // would block the corrected same-turn send.
                        None
                    }
                    ProviderFailureKind::AuthError => Some((
                        &self.candidate.rate_limit,
                        ProviderCooldownPolicy::Credential,
                    )),
                    ProviderFailureKind::ContractError if adapter_correctable_dispatch => {
                        // The adapter corrects this rejection in-turn: a
                        // lossless replay without the rejected continuation
                        // form, or skipping a rejected auxiliary probe. So
                        // consuming the candidate would block the corrected
                        // same-turn send.
                        None
                    }
                    ProviderFailureKind::ContractError => {
                        Some((&self.candidate.contract, ProviderCooldownPolicy::Contract))
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
                    let diagnostics = provider_transport_diagnostics(error);
                    let cause = ProviderCooldownCause {
                        failure_kind: match kind {
                            ProviderFailureKind::Connection => "connection",
                            ProviderFailureKind::Timeout => "timeout",
                            ProviderFailureKind::RateLimited => "rate_limited",
                            ProviderFailureKind::ServerError => "server_error",
                            ProviderFailureKind::PaymentRequired => "payment_required",
                            ProviderFailureKind::AuthError => "auth_error",
                            ProviderFailureKind::ContractError => "contract_error",
                            _ => "unknown",
                        }
                        .into(),
                        http_status: diagnostics.and_then(|diagnostics| diagnostics.status),
                        occurred_at: now,
                        server_not_before,
                        incident_id: None,
                        details_available: true,
                    };
                    self.db.record_provider_admission_failure_with_cause(
                        &lease,
                        scope,
                        policy,
                        now,
                        server_not_before,
                        &self.candidate.model.as_string(),
                        Some(&cause),
                    )?;
                    match policy {
                        ProviderCooldownPolicy::Transport => {
                            ProviderRecoveryRequestOutcome::NetworkFailure(scope)
                        }
                        ProviderCooldownPolicy::PaymentRequired => {
                            ProviderRecoveryRequestOutcome::PaymentRequired(scope)
                        }
                        ProviderCooldownPolicy::Credential => {
                            ProviderRecoveryRequestOutcome::AuthFailure(scope)
                        }
                        ProviderCooldownPolicy::Contract => {
                            ProviderRecoveryRequestOutcome::ContractFailure(scope)
                        }
                        _ => ProviderRecoveryRequestOutcome::RateLimited(scope),
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
    /// Only suppress selection. The source's canonical state and recovery
    /// authority remain untouched, including after the revision fault disappears.
    pub(super) fn suppress_repair_blocked_source(&self, message: &MessageEnvelope) -> Result<bool> {
        if matches!(
            message.kind,
            MessageKind::Control
                | MessageKind::TaskStatus
                | MessageKind::BriefAck
                | MessageKind::BriefResult
        ) {
            return Ok(false);
        }
        let blocked = self
            .inner
            .runtime_db
            .source_revision_repair_blocks_source(message, self.now())?;
        self.publish_pending_source_revision_repair_briefs(&message.agent_id)?;
        Ok(blocked)
    }

    pub(super) fn publish_pending_source_revision_repair_briefs(
        &self,
        agent_id: &str,
    ) -> Result<()> {
        let index = self.inner.storage.index_outbox();
        for brief in self
            .inner
            .runtime_db
            .pending_source_revision_repair_briefs(agent_id)?
        {
            let Some(commit) = self
                .inner
                .runtime_db
                .commit_source_revision_repair_brief(&brief, &index.changes_for_brief(&brief))?
            else {
                continue;
            };
            index.enqueue_brief_best_effort(&commit.brief)?;
            if commit.event_inserted {
                if let Err(error) = self
                    .inner
                    .storage
                    .event_log()
                    .publish_committed_event(Some(agent_id.into()), &commit.event)
                {
                    tracing::warn!(
                        error = %error,
                        event_id = %commit.event.id,
                        "failed to publish committed repair-block brief_created event"
                    );
                }
            }
            self.inner.notify.notify_one();
        }
        Ok(())
    }

    pub(super) async fn settle_provider_dispatch_pause(
        &self,
        message: &MessageEnvelope,
        error: &anyhow::Error,
        pause: &ProviderDispatchPause,
        duration_ms: u64,
    ) -> Result<()> {
        let source = self
            .inner
            .storage
            .read_message_by_id(&message.id)?
            .context("provider pause source disappeared")?;
        let mut terminal = self
            .build_turn_interrupted_record("provider_dispatch_parked", None, duration_ms)
            .await;
        let mut brief = crate::brief::make_ack(&message.agent_id, message);
        brief.turn_id = Some(terminal.turn_id.clone());
        brief.turn_index = Some(terminal.turn_index);
        terminal.no_brief_reason = None;
        let mut turn_record = self.build_turn_record(&terminal).await?;
        turn_record.produced_brief_ids.push(brief.id.clone());
        let transition = super::turn::TurnTerminalTransition {
            terminal,
            turn_record,
            prepared_work_item_completion: None,
            prepared_wait_for: None,
            terminal_tool_executions: Vec::new(),
        };
        let report = ProviderPauseSettlement {
            pause,
            source: &source,
            brief,
            rejection: crate::runtime_error::describe_runtime_error(error),
        };
        let record = crate::types::QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            priority: message.priority.clone(),
            status: crate::types::QueueEntryStatus::Interrupted,
            created_at: message.created_at,
            updated_at: self.now(),
        };
        let witness = super::terminal_execution_settlement_witness(
            &self.inner.storage,
            &self.inner.runtime_db,
            &record,
            &transition,
        )?;
        ensure!(
            witness
                .as_ref()
                .is_some_and(|witness| witness.attempt_id == pause.attempt_id),
            "provider pause execution identity changed before settlement"
        );
        match self
            .commit_queue_terminal_settlement_with_evidence(
                record.clone(),
                Vec::new(),
                true,
                Some(&transition),
                None,
                Vec::new(),
                Vec::new(),
                Some(&report),
            )
            .await
        {
            Ok(true) => Ok(()),
            outcome => {
                if self.terminal_settlement_readback(&record, &transition, witness.as_ref())?
                    == super::TerminalSettlementReadback::Committed
                    && self
                        .inner
                        .runtime_db
                        .provider_queue_parking(&source.agent_id, &source.id)?
                        .as_ref()
                        == Some(&pause.parking)
                    && self
                        .inner
                        .storage
                        .read_brief_by_id(&report.brief.id)?
                        .is_some_and(|stored| report.matches_brief(&stored))
                {
                    // Only reconcile memory after the entire pause transaction is durable.
                    self.restore_parked_provider_source(&source.id).await
                } else {
                    outcome.context("provider pause settlement did not commit")?;
                    anyhow::bail!("provider pause settlement was not applied")
                }
            }
        }
    }

    pub(super) fn local_provider_dispatch_error(&self, mut error: anyhow::Error) -> anyhow::Error {
        if !crate::provider::is_dispatch_guard_rejected(&error) {
            return error;
        }
        let Some(denial) = dispatch_denial(&error).map(|error| error.0) else {
            // The transport marker also covers storage/infrastructure failures.
            // Keep their domain, code and retryability rather than inventing a denial.
            return error;
        };
        let disposition = if dispatch_pause(&error).is_some() {
            "pause_requested"
        } else if matches!(denial, LocalDispatchDenial::ScopeCooling { .. })
            && self
                .active_provider_admission()
                .ok()
                .flatten()
                .is_some_and(|admission| {
                    admission
                        .state
                        .lock()
                        .is_ok_and(|state| state.dispatch.is_none() && !state.unknown_dispatch)
                })
        {
            // This is a local send result, not the final turn disposition.
            "continuation_pending"
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
            let payment = ProviderAdmissionScope::candidate(&format!(
                "{path}:{}:{:?}",
                route.route_ref.as_string(),
                quota.as_ref().map(ProviderAdmissionScope::key)
            ))?;
            let contract = ProviderAdmissionScope::candidate(&format!(
                "{path}:{}:{:?}:contract",
                route.route_ref.as_string(),
                quota.as_ref().map(ProviderAdmissionScope::key)
            ))?;
            let mut scopes: Vec<_> = quota.iter().cloned().collect();
            scopes.extend([
                connection.clone(),
                request.clone(),
                payment.clone(),
                contract.clone(),
            ]);
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
                payment,
                contract,
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
        match self.available_provider_turn_candidate(message, attempt, chain, episode, excluded)? {
            ProviderCandidateSelection::Granted(admission) => {
                self.inner
                    .runtime_db
                    .clear_provider_queue_parking(&message.agent_id, &message.id)?;
                Ok(ProviderTurnGate::Granted(admission))
            }
            ProviderCandidateSelection::Cooling {
                not_before,
                untried,
            } => {
                self.inner.runtime_db.park_provider_recovery_episode(
                    episode,
                    not_before,
                    untried,
                    self.now(),
                )?;
                let parked = self
                    .inner
                    .runtime_db
                    .provider_recovery_episode(episode.key())?
                    .context("parked provider episode missing")?;
                self.persist_provider_queue_parking(
                    message,
                    chain,
                    &parked,
                    parked
                        .not_before
                        .context("parked provider deadline missing")?,
                    parked.state == ProviderRecoveryBudgetState::BudgetExhausted,
                )?;
                Ok(ProviderTurnGate::Parked)
            }
        }
    }

    fn available_provider_turn_candidate(
        &self,
        message: &MessageEnvelope,
        attempt: &ExecutionAttempt,
        chain: &Chain,
        episode: &ProviderRecoveryEpisode,
        excluded: &std::collections::BTreeSet<String>,
    ) -> Result<ProviderCandidateSelection> {
        let now = self.now();
        let len = chain.candidates.len();
        let start = usize::try_from(episode.candidate_cursor)
            .unwrap_or(len)
            .min(len);
        let mut order: Vec<_> = (start..len).chain(0..start).collect();
        let selection = super::turn::TurnModelSelection::from_message(message)?;
        let requested = selection
            .recovery
            .as_ref()
            .map(|directive| &directive.fallback_model_ref);
        let mut reselection_reason = None;
        if let Some(requested) = requested {
            super::provider_recovery::resolve_for_context(&self.inner.storage, message)?
                .context("provider recovery source resolution missing at admission")?;
            if let Some(index) = chain
                .candidates
                .iter()
                .position(|candidate| &candidate.model == requested)
            {
                order.retain(|candidate| *candidate != index);
                order.insert(0, index);
            } else {
                reselection_reason = Some("requested_model_not_in_effective_chain");
            }
        }
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
                    if requested == Some(&chain.candidates[index].model) {
                        reselection_reason = Some("requested_candidate_cooling");
                    }
                }
            }
            order.sort_by_key(|index| cooling.contains(index));
        }
        let mut earliest: Option<DateTime<Utc>> = None;
        let mut untried = false;
        let mut pending_forward = false;
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
                if requested == Some(&candidate.model) {
                    reselection_reason = Some("requested_candidate_excluded");
                }
                continue;
            }
            if self
                .inner
                .runtime_db
                .provider_recovery_candidate_attempted(&episode, &candidate.scopes)?
            {
                if requested == Some(&candidate.model) {
                    reselection_reason = Some("requested_candidate_already_attempted");
                }
                continue;
            }
            untried = true;
            pending_forward |= index >= start;
            match self.inner.runtime_db.reserve_provider_recovery_admission(
                &episode,
                &candidate.scopes,
                now,
                PROBE_LEASE,
            )? {
                ProviderRecoveryAdmissionReservation::Granted(lease) => {
                    // Do not renew a failed prefix's floor while a later probe
                    // is pending. A prefix recovered by another root is usable.
                    if episode.kind == ProviderRecoveryBudgetKind::Network
                        && index < start
                        && pending_forward
                        && lease.is_probe()
                    {
                        self.inner.runtime_db.release_provider_admission(&lease)?;
                        continue;
                    }
                    return Ok(ProviderCandidateSelection::Granted(Arc::new(
                        ProviderTurnAdmission {
                            db: self.inner.runtime_db.clone(),
                            clock: self.inner.clock.clone(),
                            attempt: attempt.clone(),
                            episode_key: episode.key().to_owned(),
                            chain_fingerprint: chain.fingerprint.clone(),
                            candidate: candidate.clone(),
                            next_cursor: u32::try_from(index + 1)
                                .context("provider chain too long")?,
                            recovery_reselection_reason: requested
                                .filter(|requested| *requested != &candidate.model)
                                .map(|_| {
                                    reselection_reason.unwrap_or("requested_candidate_unavailable")
                                }),
                            state: Mutex::new(RequestState {
                                lease: Some(lease),
                                dispatch: None,
                                unknown_dispatch: false,
                                dispatched: false,
                                adapter_correctable_dispatch: false,
                                auth_refreshable_dispatch: false,
                                side_effect_boundary: episode.side_effect_boundary,
                            }),
                            input_fence: Mutex::new(None),
                        },
                    )));
                }
                ProviderRecoveryAdmissionReservation::Blocked(blocked) => {
                    if requested == Some(&candidate.model) {
                        reselection_reason = Some("requested_candidate_cooling");
                    }
                    earliest =
                        Some(earliest.map_or(blocked.not_before, |at| at.min(blocked.not_before)));
                }
                ProviderRecoveryAdmissionReservation::Denied(_) => {
                    if requested == Some(&candidate.model) {
                        reselection_reason = Some("requested_candidate_reservation_denied");
                    }
                    if blocked.is_none() {
                        earliest = Some(now);
                    }
                }
            }
        }
        Ok(ProviderCandidateSelection::Cooling {
            not_before: earliest.unwrap_or(now),
            untried,
        })
    }

    /// Replan an unsent candidate cooldown, never execution or budget fences.
    async fn reselect_cooling_provider(
        &self,
        current: &Arc<ProviderTurnAdmission>,
        cooling_until: DateTime<Utc>,
        excluded: &mut std::collections::BTreeSet<String>,
    ) -> Result<CoolingReplanOutcome> {
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
        let source_message_id = current
            .attempt
            .source_message_id
            .as_deref()
            .context("cooling provider source message missing")?;
        let message = self
            .inner
            .storage
            .read_message_by_id(source_message_id)?
            .context("cooling provider source message not persisted")?;
        match self.available_provider_turn_candidate(
            &message,
            &current.attempt,
            &chain,
            &episode,
            excluded,
        )? {
            ProviderCandidateSelection::Granted(next) => {
                {
                    // Candidate reselection stays in the same turn: it cannot
                    // erase evidence that replay would repeat a prior request.
                    let previous = current
                        .state
                        .lock()
                        .map_err(|_| anyhow!("provider admission poisoned"))?;
                    let mut replacement = next
                        .state
                        .lock()
                        .map_err(|_| anyhow!("provider admission poisoned"))?;
                    replacement.dispatched |= previous.dispatched;
                    replacement.unknown_dispatch |= previous.unknown_dispatch;
                    replacement.side_effect_boundary |= previous.side_effect_boundary;
                }
                let model = next.model().clone();
                let mut active = self
                    .inner
                    .turn_provider_admission
                    .lock()
                    .map_err(|_| anyhow!("turn provider admission poisoned"))?;
                ensure!(
                    active
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, current)),
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
                Ok(CoolingReplanOutcome::Reselect(model))
            }
            // A continuation has already crossed its source replay boundary.
            // Exhausted candidate selection must not mutate it into source parking.
            ProviderCandidateSelection::Cooling { .. } => Ok(CoolingReplanOutcome::Wait),
        }
    }

    /// A local denial may change candidates or wait, never replay the source.
    pub(super) async fn continue_cooling_provider(
        &self,
        error: anyhow::Error,
        excluded: &mut std::collections::BTreeSet<String>,
        may_reselect: bool,
        round: usize,
        observed_interjections: &mut std::collections::BTreeSet<String>,
    ) -> Result<CoolingContinuation> {
        if !crate::provider::is_dispatch_guard_rejected(&error) || dispatch_pause(&error).is_some()
        {
            return Err(error);
        }
        let Some(cooling_until) = scope_cooling_deadline(&error) else {
            return Err(error);
        };
        let current = self
            .active_provider_admission()?
            .context("cooling provider admission missing")?;
        {
            let state = current
                .state
                .lock()
                .map_err(|_| anyhow!("provider admission poisoned"))?;
            if state.dispatch.is_some() || state.unknown_dispatch {
                return Err(error);
            }
        }
        if may_reselect {
            match self
                .reselect_cooling_provider(&current, cooling_until, excluded)
                .await
                .context("scope cooldown candidate replan failed")?
            {
                CoolingReplanOutcome::Reselect(model) => {
                    return Ok(CoolingContinuation::Reselect(model));
                }
                CoolingReplanOutcome::Wait => {}
            }
        }
        self.wait_for_cooling_provider(&current, cooling_until, round, observed_interjections)
            .await
    }

    async fn wait_for_cooling_provider(
        &self,
        current: &Arc<ProviderTurnAdmission>,
        original_deadline: DateTime<Utc>,
        round: usize,
        observed_interjections: &mut std::collections::BTreeSet<String>,
    ) -> Result<CoolingContinuation> {
        let episode = current.episode()?;
        let abort = self.current_run_abort_token().await;
        // Expired/repeated denials still yield rather than spin at zero time.
        let mut deadline = original_deadline.max(self.now() + Duration::seconds(1));
        let mut visible = None;
        let result: Result<CoolingContinuation> = async {
            loop {
                let input_changed = self.inner.operator_interjection_notify.notified();
                tokio::pin!(input_changed);
                input_changed.as_mut().enable();
                if let Some(abort) = abort.as_ref().filter(|abort| abort.token.is_cancelled()) {
                    return Err(super::CurrentRunAborted {
                        run_id: abort.run_id.clone(),
                        reason: abort.reason(),
                    }
                    .into());
                }
                {
                    let state = current
                        .state
                        .lock()
                        .map_err(|_| anyhow!("provider admission poisoned"))?;
                    ensure!(
                        state.dispatch.is_none() && !state.unknown_dispatch,
                        "cannot wait with an in-flight or unknown provider request"
                    );
                }
                ensure!(
                    self.inner
                        .runtime_db
                        .provider_recovery_attempt_rejection(&episode, &current.attempt,)?
                        .is_none(),
                    "canonical provider execution changed during cooldown wait"
                );
                let agent_state = self.inner.agent.lock().await.state.clone();
                ensure!(
                    self.provider_admission_chain(&agent_state)?
                        .is_some_and(|chain| chain.fingerprint == current.chain_fingerprint),
                    "provider chain changed during cooldown wait"
                );
                let now = self.now();
                let blocked = self
                    .inner
                    .runtime_db
                    .provider_admission_blocked(&current.candidate.scopes, now)?;
                if let Some(blocked) = blocked.as_ref() {
                    deadline = deadline.max(blocked.not_before);
                }
                if self
                    .has_unseen_cooling_interjection(round, observed_interjections)
                    .await?
                {
                    return Ok(CoolingContinuation::Interject);
                }
                if now >= deadline {
                    return Ok(CoolingContinuation::RetrySame);
                }
                let cause = blocked
                    .as_ref()
                    .map(|blocked| {
                        self.inner
                            .runtime_db
                            .provider_cooldown_cause(&blocked.scope)
                    })
                    .transpose()?
                    .flatten()
                    .map(crate::types::ProviderCooldownCauseSummary::from)
                    .unwrap_or_else(|| crate::types::ProviderCooldownCauseSummary {
                        detail_unavailable_reason: Some("cause_record_unavailable".into()),
                        ..Default::default()
                    });
                let next_visible = (deadline, cause.clone());
                if visible.as_ref() != Some(&next_visible) {
                    if visible.is_none() {
                        self.begin_provider_cooldown_wait(crate::types::ProviderCooldownWait {
                            turn_id: agent_state.current_turn_id.clone(),
                            turn_index: agent_state.turn_index,
                            attempt_id: Some(current.attempt.attempt_id.clone()),
                            model: Some(current.model().clone()),
                            started_at: now,
                            not_before: deadline,
                            cause,
                        })
                        .await?;
                    } else {
                        self.extend_provider_cooldown_wait(deadline, Some(cause))
                            .await?;
                    }
                    visible = Some(next_visible);
                }
                let recheck = deadline.min(now + Duration::seconds(30));
                if let Some(abort) = abort.as_ref() {
                    tokio::select! {
                        _ = self.inner.clock.sleep_until(recheck) => {}
                        _ = &mut input_changed => {}
                        _ = abort.token.cancelled() => {
                            return Err(super::CurrentRunAborted {
                                run_id: abort.run_id.clone(),
                                reason: abort.reason(),
                            }.into());
                        }
                    }
                } else {
                    tokio::select! {
                        _ = self.inner.clock.sleep_until(recheck) => {}
                        _ = &mut input_changed => {}
                    }
                }
            }
        }
        .await;
        if visible.is_some() {
            use crate::types::ProviderCooldownWaitEndOutcome;
            let outcome = match &result {
                Ok(CoolingContinuation::Interject) => ProviderCooldownWaitEndOutcome::Interjected,
                Ok(_) => ProviderCooldownWaitEndOutcome::Resumed,
                Err(error) if error.downcast_ref::<super::CurrentRunAborted>().is_some() => {
                    ProviderCooldownWaitEndOutcome::Cancelled
                }
                Err(_) => ProviderCooldownWaitEndOutcome::ValidationFailed,
            };
            self.end_provider_cooldown_wait(outcome).await?;
        }
        result
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

fn scope_cooling_deadline(error: &anyhow::Error) -> Option<DateTime<Utc>> {
    dispatch_denial(error).and_then(|denial| match denial.0 {
        LocalDispatchDenial::ScopeCooling { not_before } => Some(not_before),
        _ => None,
    })
}

#[cfg(test)]
mod tests;
