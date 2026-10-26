//! Observer-sync HTTP contract surface and roster snapshot handler.
//!
//! Implements the contract surface of
//! `docs/rfcs/observer-sync-agent-summary-and-read-markers.md` (S0):
//! snapshot DTOs, projection-effect classification, the rich cursor error
//! shape, and the capability evaluator, plus the S4 roster snapshot
//! handler and the S5 per-Agent projection snapshot handler. A capability
//! is advertised only after its durable verification
//! succeeds, so route registration alone never serves a snapshot contract.

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use super::serialize_json;
use crate::diagnostics;
use crate::types::{AgentListEntry, WorkItemPlanStatus, WorkItemState};
// The projection-effect classification lives with the runtime event registry
// (its source of truth); re-exported here to keep the S0 contract surface.
use super::{
    authentication_error_response, authorize_remote_access, error_response, http_error,
    projection_gate_error_response, traced_json_bytes, traced_json_bytes_with_stale_marker,
    AppState, AxumResponse, HttpErrorEnvelope, IntoResponse, ProjectionFailure,
    ProjectionGateError, ProjectionKey,
};
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use crate::runtime_event::ProjectionEffect;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};

#[cfg(test)]
#[path = "observer_sync/projection_decode_tests.rs"]
mod projection_decode_tests;
#[cfg(test)]
#[path = "observer_sync/roster_material_tests.rs"]
mod roster_material_tests;

pub(crate) const ROSTER_SNAPSHOT_CAPABILITY: &str = "agents.roster-snapshot.v1";
pub(crate) const PROJECTION_SNAPSHOT_CAPABILITY: &str = "agents.projection-snapshot.v1";
pub(crate) const CONVERSATION_READ_CAPABILITY: &str = "agents.conversation-read.v1";
pub(crate) const CONVERSATION_INCARNATION_CAPABILITY: &str = "agents.conversation-incarnation.v1";
pub(crate) const CONVERSATION_LOCATE_CAPABILITY: &str = "agents.conversation-locate.v1";
pub(crate) const CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY: &str =
    "agents.conversation-history-bidirectional.v1";
pub(crate) const PROJECTION_EFFECT_CAPABILITY: &str = "events.projection-effect.v1";
pub(crate) const ATOMIC_BRIEF_CREATED_EVENT_CAPABILITY: &str = "briefs.atomic-created-event.v1";

pub(crate) const AGENT_ROSTER_SNAPSHOT_CONTRACT_VERSION: u32 = 1;
pub(crate) const AGENT_PROJECTION_SNAPSHOT_CONTRACT_VERSION: u32 = 2;

/// Upper bound for `AgentLatestBrief::preview`, in UTF-8 bytes.
pub(crate) const LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES: usize = 512;

/// Durable verification results required before any observer-sync capability
/// may be advertised. Each field is the outcome of a stored verification
/// against the current runtime database, not a route or schema existence
/// check. S1 introduces the verification source; until then every capability
/// stays disabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ObserverSyncCapabilityVerification {
    /// Stable `runtime_id` and current `event_log_epoch` are persisted and
    /// survived the latest reopen check.
    pub(crate) runtime_identity_stable: bool,
    /// Agent identity reservation/tombstones are durable; a retired id is
    /// never implicitly reused and can only return through the audited
    /// reincarnation path after a fully completed deletion.
    pub(crate) agent_identity_reserved: bool,
    /// Roster snapshot consistent read view, authorization, all-or-nothing
    /// failure, and limit tests passed for this database.
    pub(crate) roster_snapshot_verified: bool,
    /// Per-Agent projection snapshot consistency boundary and fault tests
    /// passed for this database.
    pub(crate) projection_snapshot_verified: bool,
    /// Every served event family is classified in the event registry
    /// inventory (page, SSE, and history).
    pub(crate) event_projection_effect_complete: bool,
    /// Every Brief publication path commits record plus unique
    /// `brief_created` event atomically (or via durable outbox) and the
    /// historical backfill is unambiguous.
    pub(crate) brief_atomic_linkage_verified: bool,
    /// Every conversation source has canonical ownership, monotonic revision,
    /// and atomic source-event coverage.
    pub(crate) conversation_read_verified: bool,
}

/// Evaluates the five observer-sync capabilities independently. Snapshot
/// capabilities additionally require stable runtime and Agent identity
/// verification because their responses anchor browser cache partitions to
/// `(runtime_id, visibility_scope_id, event_log_epoch)`.
pub(crate) fn advertised_observer_sync_capabilities(
    verification: &ObserverSyncCapabilityVerification,
) -> Vec<&'static str> {
    let mut capabilities = Vec::new();
    if verification.runtime_identity_stable
        && verification.agent_identity_reserved
        && verification.roster_snapshot_verified
    {
        capabilities.push(ROSTER_SNAPSHOT_CAPABILITY);
    }
    if verification.runtime_identity_stable
        && verification.agent_identity_reserved
        && verification.projection_snapshot_verified
    {
        capabilities.push(PROJECTION_SNAPSHOT_CAPABILITY);
    }
    if verification.event_projection_effect_complete {
        capabilities.push(PROJECTION_EFFECT_CAPABILITY);
    }
    if verification.brief_atomic_linkage_verified {
        capabilities.push(ATOMIC_BRIEF_CREATED_EVENT_CAPABILITY);
    }
    capabilities.push(CONVERSATION_READ_CAPABILITY);
    capabilities.push(CONVERSATION_INCARNATION_CAPABILITY);
    capabilities.push(CONVERSATION_LOCATE_CAPABILITY);
    capabilities.push(CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY);
    capabilities
}

/// Authoritative roster snapshot for membership, authorization visibility,
/// and per-Agent event-window anchors. All-or-nothing: a failure to assemble
/// one Agent fails the whole response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentRosterSnapshot {
    pub(crate) contract_version: u32,
    /// Stable public identity of the runtime installation. Distinguishes a
    /// replaced server from an ordinary restart. Not a secret.
    pub(crate) runtime_id: String,
    pub(crate) event_log_epoch: String,
    pub(crate) visibility_scope_id: String,
    /// Active public Agents visible to the caller. Private child Agents are
    /// never included.
    pub(crate) agents: Vec<AgentRosterEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentRosterEntry {
    /// Same entry shape as `GET /api/agents/list`.
    pub(crate) agent: AgentListEntry,
    pub(crate) event_window: AgentEventWindow,
    /// Latest canonical Brief; `null` when the Agent has no Brief yet.
    pub(crate) latest_brief: Option<AgentLatestBrief>,
}

/// Committed event window for one Agent inside the snapshot read view. Both
/// values must come from the same committed database view, never from an
/// in-memory watcher or a sequence allocator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentEventWindow {
    /// Greatest committed `event_seq` visible in the response read view.
    pub(crate) event_head_seq: u64,
    /// Retention floor; `0` means no retained prefix has ever been deleted.
    pub(crate) oldest_retained_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentLatestBrief {
    pub(crate) brief_id: String,
    /// `brief_created` event sequence linkage. `null` when the Brief cannot
    /// be linked to a unique retained event; such Briefs do not participate
    /// in exact unread calculation.
    pub(crate) created_event_seq: Option<u64>,
    pub(crate) created_at: DateTime<Utc>,
    /// Bounded preview, at most `LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES`
    /// UTF-8 bytes. Full text remains available from the Brief APIs.
    pub(crate) preview: String,
}

/// Per-Agent canonical projection snapshot at one consistency boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentProjectionSnapshot {
    pub(crate) contract_version: u32,
    pub(crate) runtime_id: String,
    pub(crate) event_log_epoch: String,
    pub(crate) visibility_scope_id: String,
    pub(crate) agent_id: String,
    /// Every event with `event_seq <= snapshot_through_seq` that affects the
    /// current canonical state is already reflected in `projection`.
    pub(crate) snapshot_through_seq: u64,
    /// May be greater than `snapshot_through_seq`; names a committed event
    /// available through the event page.
    pub(crate) event_head_seq: u64,
    pub(crate) oldest_retained_seq: u64,
    pub(crate) projection: AgentCanonicalProjection,
}

/// First concrete `AgentCanonicalProjection`: compact current state plus
/// stable revision anchors only. It deliberately excludes verbose timelines
/// and full transcript/message history (see
/// `docs/implementation-decisions/104-agent-canonical-projection-v1-boundary.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentCanonicalProjection {
    /// Lifecycle, posture, and compact card facts.
    pub(crate) agent: AgentListEntry,
    /// Independent canonical relation and policy axes. Missing normalized
    /// records fall back through one explicit legacy compatibility mapper.
    pub(crate) relations: crate::types::AgentCanonicalRelationsProjection,
    /// Current focused/open WorkItem anchor; `null` when none exists.
    pub(crate) current_work_item: Option<AgentWorkItemAnchor>,
    pub(crate) conversation: ConversationRevisionAnchors,
    pub(crate) latest_brief: Option<AgentLatestBrief>,
    /// Records deleted at or before the snapshot boundary. They terminate
    /// pending hydration without fetching a record that no longer exists.
    #[serde(default)]
    pub(crate) hydration_tombstones: Vec<AgentHydrationKey>,
    /// Records referenced by the projection but still resolvable through the
    /// batch record APIs.
    #[serde(default)]
    pub(crate) hydration_references: Vec<AgentHydrationKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentWorkItemAnchor {
    pub(crate) work_item_id: String,
    pub(crate) state: WorkItemState,
    pub(crate) plan_status: WorkItemPlanStatus,
    /// Canonical WorkItem revision at the snapshot boundary.
    pub(crate) revision: u64,
    pub(crate) updated_at: DateTime<Utc>,
}

/// Latest-record anchors for conversation families whose canonical records
/// carry no per-record revision counter. Combined with replayed events they
/// let a client decide whether a local record is current at the boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ConversationRevisionAnchors {
    pub(crate) latest_message_id: Option<String>,
    pub(crate) latest_transcript_entry_id: Option<String>,
}

/// Identifies one canonical record for hydration termination or resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentHydrationKey {
    pub(crate) record_kind: ObserverSyncRecordKind,
    pub(crate) record_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObserverSyncRecordKind {
    Message,
    Brief,
    TranscriptEntry,
}

/// Rich `cursor_not_found` body for event pages and SSE. `event_log_epoch`,
/// `oldest_retained_seq`, and `event_head_seq` must come from one committed
/// read view so clients can distinguish a retained-prefix gap from an epoch
/// change and select the correct reset path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct RichCursorNotFoundError {
    pub(crate) ok: bool,
    pub(crate) error: String,
    pub(crate) code: String,
    pub(crate) after_seq: u64,
    pub(crate) event_log_epoch: String,
    pub(crate) oldest_retained_seq: u64,
    pub(crate) event_head_seq: u64,
}

/// First-version hard limits for the roster snapshot response. When the
/// deployment outgrows them, pagination bound to a server-pinned snapshot
/// token replaces them; independently read pages cannot form a roster.
pub(crate) const ROSTER_SNAPSHOT_MAX_AGENTS: usize = 512;
pub(crate) const ROSTER_SNAPSHOT_MAX_SERIALIZED_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const ROSTER_SNAPSHOT_ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Principal and entitlement used to derive the observer scope while the
/// control token is required (and therefore presented); local
/// unauthenticated mode keeps the runtime-local public scope.
pub(crate) const CONTROL_SCOPE_PRINCIPAL: &str = "control-token";
pub(crate) const CONTROL_SCOPE_ENTITLEMENT: &str = "control";

#[derive(Debug, Clone)]
pub(crate) struct RosterSnapshotLimits {
    pub max_agents: usize,
    pub max_serialized_bytes: usize,
    pub timeout: Duration,
}

impl Default for RosterSnapshotLimits {
    fn default() -> Self {
        Self {
            max_agents: ROSTER_SNAPSHOT_MAX_AGENTS,
            max_serialized_bytes: ROSTER_SNAPSHOT_MAX_SERIALIZED_BYTES,
            timeout: ROSTER_SNAPSHOT_ASSEMBLY_TIMEOUT,
        }
    }
}

/// First-version hard limits for one per-Agent projection snapshot
/// response. A snapshot is one Agent's compact projection, so the byte
/// budget is well below the roster's; the timeout bounds the committed
/// read view plus assembly.
pub(crate) const PROJECTION_SNAPSHOT_MAX_SERIALIZED_BYTES: usize = 1024 * 1024;
pub(crate) const PROJECTION_SNAPSHOT_ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct ProjectionSnapshotLimits {
    pub max_serialized_bytes: usize,
    pub timeout: Duration,
}

impl Default for ProjectionSnapshotLimits {
    fn default() -> Self {
        Self {
            max_serialized_bytes: PROJECTION_SNAPSHOT_MAX_SERIALIZED_BYTES,
            timeout: PROJECTION_SNAPSHOT_ASSEMBLY_TIMEOUT,
        }
    }
}

/// `GET /api/agents/snapshot`: the authoritative roster snapshot. The
/// handler does authorization, the capability gate, hard limits, and
/// serialization only; membership and anchors come from one committed
/// database read view assembled by the host. The response is
/// all-or-nothing: one unassemblable Agent fails the whole request.
pub async fn agent_roster_snapshot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let (principal, entitlement) = match observer_principal_and_entitlement(&headers, &state) {
        Ok(authority) => authority,
        Err(error) => return authentication_error_response(error).into_response(),
    };
    let budget = state.roster_snapshot_limits.timeout;
    let deadline = tokio::time::Instant::now() + budget;
    let verification =
        match super::agents::load_observer_sync_verification_scoped(&state, deadline).await {
            Ok(verification) => verification,
            Err(error) => {
                diagnostics::record_roster_snapshot_failure();
                return projection_gate_error_response("/agents/snapshot", error.into());
            }
        };
    if !advertised_observer_sync_capabilities(&verification).contains(&ROSTER_SNAPSHOT_CAPABILITY) {
        tracing::warn!(
            capability = "agents.roster-snapshot.v1",
            runtime_identity_stable = verification.runtime_identity_stable,
            agent_identity_reserved = verification.agent_identity_reserved,
            roster_snapshot_verified = verification.roster_snapshot_verified,
            "observer-sync capability gate rejected the request; the durable \
             verification for this database is failing (see \
             observer_sync_capability_verifications rows)"
        );
        diagnostics::record_roster_snapshot_failure();
        return http_error(
            StatusCode::SERVICE_UNAVAILABLE,
            HttpErrorEnvelope::new(
                "capability_unavailable",
                "the agents.roster-snapshot.v1 capability is not verified for this database",
            )
            .hint("see the handshake capabilities; route registration alone never serves this contract"),
        )
        .into_response();
    }
    let gate_state = Arc::clone(&state);
    let key = ProjectionKey::AgentsRosterSnapshot(principal.clone());
    let result = state
        .projection_gate
        .run_owned(key, deadline, budget, move |context| async move {
            context
                .blocking(move |scope| {
                    let limits = gate_state.roster_snapshot_limits.clone();
                    let host = gate_state.host.clone();
                    let db = host.runtime_db().with_read_scope(scope);
                    let snapshot = host
                        .agent_roster_snapshot_scoped(&db)
                        .map_err(error_response)
                        .map_err(ProjectionFailure::from)?;
                    if snapshot.agents.len() > limits.max_agents {
                        return Err(ProjectionFailure::from(http_error(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            HttpErrorEnvelope::new(
                                "roster_snapshot_too_large",
                                "roster snapshot exceeds the maximum Agent count for one response",
                            )
                            .extension("agent_count", snapshot.agents.len())
                            .extension("max_agents", limits.max_agents),
                        )));
                    }
                    // Keep the original synchronous read phase through wire assembly
                    // and serialization. Before this move, rejection drops the holder
                    // last as part of the original snapshot.
                    let _material = snapshot._material;
                    let acquisition = snapshot.acquisition;
                    let snapshot = acquisition
                        .read(|| {
                            crate::diagnostics::resource_io::charge_owned_metadata(
                                u64::try_from(
                                    snapshot
                                        .agents
                                        .len()
                                        .checked_mul(std::mem::size_of::<AgentRosterEntry>())
                                        .context("observer roster slots overflow")?
                                        .checked_add(256)
                                        .context("observer roster framing overflow")?,
                                )?,
                                snapshot.agents.len() + 1,
                            )?;
                            let visibility_scope_id = observer_visibility_scope_for(
                                &snapshot.runtime_id,
                                &principal,
                                entitlement,
                                snapshot.visibility_policy_generation,
                            );
                            let snapshot = AgentRosterSnapshot {
                                contract_version: AGENT_ROSTER_SNAPSHOT_CONTRACT_VERSION,
                                runtime_id: snapshot.runtime_id,
                                event_log_epoch: snapshot.event_log_epoch,
                                visibility_scope_id,
                                agents: snapshot
                                    .agents
                                    .into_iter()
                                    .map(|entry| AgentRosterEntry {
                                        agent: entry.agent,
                                        event_window: AgentEventWindow {
                                            event_head_seq: entry.event_head_seq,
                                            oldest_retained_seq: entry.oldest_retained_seq,
                                        },
                                        latest_brief: entry.latest_brief.map(|brief| {
                                            AgentLatestBrief {
                                                brief_id: brief.brief_id,
                                                created_event_seq: brief.created_event_seq,
                                                created_at: brief.created_at,
                                                preview: brief.preview,
                                            }
                                        }),
                                    })
                                    .collect(),
                            };
                            Ok(snapshot)
                        })
                        .map_err(error_response)
                        .map_err(ProjectionFailure::from)?;
                    let agent_count = snapshot.agents.len();
                    let bytes = crate::host::observer_json_bytes(&snapshot, acquisition)
                        .map_err(error_response)
                        .map_err(ProjectionFailure::from)?;
                    if bytes.len() > limits.max_serialized_bytes {
                        return Err(ProjectionFailure::from(http_error(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            HttpErrorEnvelope::new(
                                "roster_snapshot_too_large",
                                "roster snapshot exceeds the maximum serialized response size",
                            )
                            .extension("serialized_bytes", bytes.len())
                            .extension("max_serialized_bytes", limits.max_serialized_bytes),
                        )));
                    }
                    diagnostics::record_roster_snapshot(
                        started_at.elapsed(),
                        agent_count,
                        bytes.len(),
                    );
                    db.check_read_scope()
                        .map_err(error_response)
                        .map_err(ProjectionFailure::from)?;
                    Ok(bytes)
                })
                .await?
        })
        .await;
    match result {
        Ok(bytes) => traced_json_bytes("/agents/snapshot", started_at, bytes),
        Err(ProjectionGateError::StaleServed(bytes)) => {
            traced_json_bytes_with_stale_marker("/agents/snapshot", started_at, bytes)
        }
        Err(error) => {
            diagnostics::record_roster_snapshot_failure();
            projection_gate_error_response("/agents/snapshot", error)
        }
    }
}

/// Resolves one authority for snapshots and read cursors. OIDC credentials
/// identify the user, but the scope/cache key never contains the credential,
/// so rotation within the same principal keeps the scope stable.
pub(crate) fn observer_principal_and_entitlement(
    headers: &HeaderMap,
    state: &AppState,
) -> super::AuthenticationResult<(String, &'static str)> {
    if state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc {
        Ok((
            super::control_actor(headers, state)?.principal_id(),
            CONTROL_SCOPE_ENTITLEMENT,
        ))
    } else {
        let (principal, entitlement) = observer_scope_authority(state);
        Ok((principal.to_string(), entitlement))
    }
}

pub(crate) fn observer_visibility_scope_for(
    runtime_id: &str,
    principal: &str,
    entitlement: &str,
    visibility_policy_generation: u64,
) -> String {
    crate::ids::visibility_scope_id(
        runtime_id,
        principal,
        entitlement,
        visibility_policy_generation,
    )
}

pub(crate) fn observer_scope_authority(state: &AppState) -> (&'static str, &'static str) {
    if state.require_control_token {
        (CONTROL_SCOPE_PRINCIPAL, CONTROL_SCOPE_ENTITLEMENT)
    } else {
        (
            crate::runtime_db::observer_sync::PUBLIC_SCOPE_PRINCIPAL,
            crate::runtime_db::observer_sync::PUBLIC_SCOPE_ENTITLEMENT,
        )
    }
}

/// `GET /api/agents/{agent_id}/projection-snapshot`: the per-Agent
/// canonical projection snapshot at one committed consistency boundary.
/// The handler does authorization, the capability gate, hard limits, and
/// serialization only; every anchor comes from one committed database read
/// view assembled by the host, and the boundary equals that view's
/// per-Agent event head. Non-members answer with the same not-found shape
/// whether unknown, private, or deleted, so no membership metadata leaks.
pub async fn agent_projection_snapshot(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let (principal, entitlement) = match observer_principal_and_entitlement(&headers, &state) {
        Ok(authority) => authority,
        Err(error) => return authentication_error_response(error).into_response(),
    };
    let budget = state.projection_snapshot_limits.timeout;
    let deadline = tokio::time::Instant::now() + budget;
    let verification =
        match super::agents::load_observer_sync_verification_scoped(&state, deadline).await {
            Ok(verification) => verification,
            Err(error) => {
                diagnostics::record_projection_snapshot_failure();
                return projection_gate_error_response(
                    "/agents/{agent_id}/projection-snapshot",
                    error.into(),
                );
            }
        };
    if !advertised_observer_sync_capabilities(&verification)
        .contains(&PROJECTION_SNAPSHOT_CAPABILITY)
    {
        tracing::warn!(
            capability = "agents.projection-snapshot.v1",
            runtime_identity_stable = verification.runtime_identity_stable,
            agent_identity_reserved = verification.agent_identity_reserved,
            projection_snapshot_verified = verification.projection_snapshot_verified,
            "observer-sync capability gate rejected the request; the durable \
             verification for this database is failing (see \
             observer_sync_capability_verifications rows)"
        );
        diagnostics::record_projection_snapshot_failure();
        return http_error(
            StatusCode::SERVICE_UNAVAILABLE,
            HttpErrorEnvelope::new(
                "capability_unavailable",
                "the agents.projection-snapshot.v1 capability is not verified for this database",
            )
            .hint("see the handshake capabilities; route registration alone never serves this contract"),
        )
        .into_response();
    }
    let gate_state = Arc::clone(&state);
    let boundary_agent_id = agent_id.clone();
    let result = state
        .projection_gate
        .run_owned(
            ProjectionKey::AgentProjectionSnapshot(agent_id.clone(), principal.clone()),
            deadline,
            budget,
            move |context| async move {
                context
                    .blocking(move |scope| {
                        let limits = gate_state.projection_snapshot_limits.clone();
                        let host = gate_state.host.clone();
                        let db = host.runtime_db().with_read_scope(scope);
                        let snapshot = host
                            .agent_projection_snapshot_scoped(&boundary_agent_id, &db)
                            .map_err(error_response)
                            .map_err(ProjectionFailure::from)?;
                        let Some(snapshot) = snapshot else {
                            // Unknown, private, and deleted identities share one
                            // not-found shape: no runtime, epoch, or scope facts.
                            return Err(ProjectionFailure::from(http_error(
                                StatusCode::NOT_FOUND,
                                HttpErrorEnvelope::new(
                                    "agent_not_found",
                                    "no accessible Agent for this request",
                                ),
                            )));
                        };
                        // Keep the original synchronous read phase through wire assembly
                        // and serialization, including either failure path.
                        let _material = snapshot._material;
                        let acquisition = snapshot.acquisition;
                        let snapshot = acquisition
                            .read(|| {
                                crate::diagnostics::resource_io::charge_owned_metadata(
                                    u64::try_from(
                                        snapshot
                                            .hydration_references
                                            .len()
                                            .checked_mul(std::mem::size_of::<AgentHydrationKey>())
                                            .and_then(|size| size.checked_add(256))
                                            .context("observer projection framing overflow")?,
                                    )?,
                                    snapshot.hydration_references.len() + 1,
                                )?;
                                let visibility_scope_id = observer_visibility_scope_for(
                                    &snapshot.runtime_id,
                                    &principal,
                                    entitlement,
                                    snapshot.visibility_policy_generation,
                                );
                                let snapshot = AgentProjectionSnapshot {
                                    contract_version: AGENT_PROJECTION_SNAPSHOT_CONTRACT_VERSION,
                                    runtime_id: snapshot.runtime_id,
                                    event_log_epoch: snapshot.event_log_epoch,
                                    visibility_scope_id,
                                    agent_id: snapshot.agent_id,
                                    snapshot_through_seq: snapshot.snapshot_through_seq,
                                    event_head_seq: snapshot.event_head_seq,
                                    oldest_retained_seq: snapshot.oldest_retained_seq,
                                    projection: AgentCanonicalProjection {
                                        agent: snapshot.agent,
                                        relations: snapshot.canonical_relations,
                                        current_work_item: snapshot.current_work_item.map(
                                            |work_item| AgentWorkItemAnchor {
                                                work_item_id: work_item.work_item_id,
                                                state: work_item.state,
                                                plan_status: work_item.plan_status,
                                                revision: work_item.revision,
                                                updated_at: work_item.updated_at,
                                            },
                                        ),
                                        conversation: ConversationRevisionAnchors {
                                            latest_message_id: snapshot
                                                .conversation
                                                .latest_message_id,
                                            latest_transcript_entry_id: snapshot
                                                .conversation
                                                .latest_transcript_entry_id,
                                        },
                                        latest_brief: snapshot.latest_brief.map(|brief| {
                                            AgentLatestBrief {
                                                brief_id: brief.brief_id,
                                                created_event_seq: brief.created_event_seq,
                                                created_at: brief.created_at,
                                                preview: brief.preview,
                                            }
                                        }),
                                        hydration_tombstones: Vec::new(),
                                        hydration_references: snapshot
                                            .hydration_references
                                            .into_iter()
                                            .map(|reference| {
                                                AgentHydrationKey {
                                record_kind: match reference.record_kind {
                                    crate::host::ObserverSyncRecordKindData::Message => {
                                        ObserverSyncRecordKind::Message
                                    }
                                    crate::host::ObserverSyncRecordKindData::Brief => {
                                        ObserverSyncRecordKind::Brief
                                    }
                                    crate::host::ObserverSyncRecordKindData::TranscriptEntry => {
                                        ObserverSyncRecordKind::TranscriptEntry
                                    }
                                },
                                record_id: reference.record_id,
                            }
                                            })
                                            .collect(),
                                    },
                                };
                                Ok(snapshot)
                            })
                            .map_err(error_response)
                            .map_err(ProjectionFailure::from)?;
                        let bytes = crate::host::observer_json_bytes(&snapshot, acquisition)
                            .map_err(error_response)
                            .map_err(ProjectionFailure::from)?;
                        if bytes.len() > limits.max_serialized_bytes {
                            return Err(ProjectionFailure::from(http_error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        HttpErrorEnvelope::new(
                            "projection_snapshot_too_large",
                            "projection snapshot exceeds the maximum serialized response size",
                        )
                        .extension("serialized_bytes", bytes.len())
                        .extension("max_serialized_bytes", limits.max_serialized_bytes),
                    )));
                        }
                        diagnostics::record_projection_snapshot(started_at.elapsed(), bytes.len());
                        db.check_read_scope()
                            .map_err(error_response)
                            .map_err(ProjectionFailure::from)?;
                        Ok(bytes)
                    })
                    .await?
            },
        )
        .await;
    match result {
        Ok(bytes) => traced_json_bytes("/agents/{agent_id}/projection-snapshot", started_at, bytes),
        Err(ProjectionGateError::StaleServed(bytes)) => traced_json_bytes_with_stale_marker(
            "/agents/{agent_id}/projection-snapshot",
            started_at,
            bytes,
        ),
        Err(error) => {
            diagnostics::record_projection_snapshot_failure();
            projection_gate_error_response("/agents/{agent_id}/projection-snapshot", error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FIXTURE_DIR: &str = "tests/fixtures/observer_sync";

    fn load_fixture(name: &str) -> Value {
        let path = format!("{FIXTURE_DIR}/{name}");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {path}: {err}"));
        serde_json::from_str(&raw).unwrap_or_else(|err| panic!("invalid fixture {path}: {err}"))
    }

    #[test]
    fn visibility_scope_for_isolated_principals() {
        let user_a = observer_visibility_scope_for("runtime_fixture", "user-a", "control", 0);
        let user_b = observer_visibility_scope_for("runtime_fixture", "user-b", "control", 0);

        assert_ne!(user_a, user_b);
        assert_eq!(
            user_a,
            observer_visibility_scope_for("runtime_fixture", "user-a", "control", 0)
        );
    }

    fn assert_round_trip<T: serde::de::DeserializeOwned + Serialize>(name: &str) -> T {
        let fixture = load_fixture(name);
        let parsed: T = serde_json::from_value(fixture.clone())
            .unwrap_or_else(|err| panic!("fixture {name} failed to deserialize: {err}"));
        let serialized = serde_json::to_value(&parsed)
            .unwrap_or_else(|err| panic!("fixture {name} failed to serialize: {err}"));
        assert_eq!(serialized, fixture, "fixture {name} does not round-trip");
        parsed
    }

    #[test]
    fn evaluator_always_advertises_supported_conversation_protocol() {
        let advertised = advertised_observer_sync_capabilities(&Default::default());
        assert_eq!(
            advertised,
            vec![
                CONVERSATION_READ_CAPABILITY,
                CONVERSATION_INCARNATION_CAPABILITY,
                CONVERSATION_LOCATE_CAPABILITY,
                CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY,
            ]
        );
    }

    #[test]
    fn evaluator_requires_identity_prerequisites_for_snapshot_capabilities() {
        let verification = ObserverSyncCapabilityVerification {
            roster_snapshot_verified: true,
            projection_snapshot_verified: true,
            brief_atomic_linkage_verified: true,
            event_projection_effect_complete: true,
            conversation_read_verified: true,
            ..Default::default()
        };
        let advertised = advertised_observer_sync_capabilities(&verification);
        assert!(!advertised.contains(&ROSTER_SNAPSHOT_CAPABILITY));
        assert!(!advertised.contains(&PROJECTION_SNAPSHOT_CAPABILITY));
        assert!(advertised.contains(&PROJECTION_EFFECT_CAPABILITY));
        assert!(advertised.contains(&ATOMIC_BRIEF_CREATED_EVENT_CAPABILITY));
        assert!(advertised.contains(&CONVERSATION_READ_CAPABILITY));
    }

    #[test]
    fn evaluator_advertises_capabilities_independently() {
        let verification = ObserverSyncCapabilityVerification {
            runtime_identity_stable: true,
            agent_identity_reserved: true,
            roster_snapshot_verified: true,
            ..Default::default()
        };
        assert_eq!(
            advertised_observer_sync_capabilities(&verification),
            vec![
                ROSTER_SNAPSHOT_CAPABILITY,
                CONVERSATION_READ_CAPABILITY,
                CONVERSATION_INCARNATION_CAPABILITY,
                CONVERSATION_LOCATE_CAPABILITY,
                CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY,
            ]
        );

        let verification = ObserverSyncCapabilityVerification {
            runtime_identity_stable: true,
            agent_identity_reserved: true,
            projection_snapshot_verified: true,
            ..Default::default()
        };
        assert_eq!(
            advertised_observer_sync_capabilities(&verification),
            vec![
                PROJECTION_SNAPSHOT_CAPABILITY,
                CONVERSATION_READ_CAPABILITY,
                CONVERSATION_INCARNATION_CAPABILITY,
                CONVERSATION_LOCATE_CAPABILITY,
                CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY,
            ]
        );
    }

    #[test]
    fn evaluator_keeps_conversation_diagnostic_out_of_capability_gate() {
        let mut verification = ObserverSyncCapabilityVerification {
            runtime_identity_stable: true,
            agent_identity_reserved: true,
            roster_snapshot_verified: true,
            projection_snapshot_verified: true,
            event_projection_effect_complete: true,
            brief_atomic_linkage_verified: true,
            ..Default::default()
        };
        assert!(advertised_observer_sync_capabilities(&verification)
            .contains(&CONVERSATION_READ_CAPABILITY));

        verification.conversation_read_verified = true;
        assert!(advertised_observer_sync_capabilities(&verification)
            .contains(&CONVERSATION_READ_CAPABILITY));
    }

    #[test]
    fn roster_snapshot_fixture_round_trips() {
        let snapshot: AgentRosterSnapshot = assert_round_trip("agent_roster_snapshot.json");
        assert_eq!(
            snapshot.contract_version,
            AGENT_ROSTER_SNAPSHOT_CONTRACT_VERSION
        );
        for entry in &snapshot.agents {
            assert_brief_preview_bounded(entry.latest_brief.as_ref());
        }
    }

    #[test]
    fn projection_snapshot_fixture_round_trips() {
        let snapshot: AgentProjectionSnapshot = assert_round_trip("agent_projection_snapshot.json");
        assert_eq!(
            snapshot.contract_version,
            AGENT_PROJECTION_SNAPSHOT_CONTRACT_VERSION
        );
        assert_brief_preview_bounded(snapshot.projection.latest_brief.as_ref());
    }

    #[test]
    fn cursor_not_found_fixture_round_trips() {
        let error: RichCursorNotFoundError = assert_round_trip("cursor_not_found_error.json");
        assert!(!error.ok);
        assert_eq!(error.code, "cursor_not_found");
    }

    #[test]
    fn projection_effect_serializes_rfc_values() {
        assert_eq!(
            serde_json::to_value(ProjectionEffect::None).unwrap(),
            Value::String("none".into())
        );
        assert_eq!(
            serde_json::to_value(ProjectionEffect::DisplayInvalidation).unwrap(),
            Value::String("display_invalidation".into())
        );
        assert_eq!(
            serde_json::from_value::<ProjectionEffect>(Value::String("none".into())).unwrap(),
            ProjectionEffect::None
        );
        assert_eq!(
            serde_json::from_value::<ProjectionEffect>(Value::String(
                "display_invalidation".into()
            ))
            .unwrap(),
            ProjectionEffect::DisplayInvalidation
        );
    }

    #[test]
    fn openapi_registers_observer_sync_schemas_without_routes() {
        let api = crate::openapi::generate_openapi_json();
        let schemas = api["components"]["schemas"].as_object().unwrap();
        for name in [
            "AgentRosterSnapshot",
            "AgentRosterEntry",
            "AgentEventWindow",
            "AgentLatestBrief",
            "AgentProjectionSnapshot",
            "AgentCanonicalProjection",
            "AgentWorkItemAnchor",
            "ConversationRevisionAnchors",
            "AgentHydrationKey",
            "ProjectionEffect",
            "CursorNotFoundError",
        ] {
            assert!(schemas.contains_key(name), "missing schema {name}");
        }
        let paths = api["paths"].as_object().unwrap();
        // S4 registers the roster snapshot route and S5 the per-Agent
        // projection snapshot route. Route existence still must never be
        // mistaken for capability support: the handlers gate on durable
        // verification.
        assert!(paths.contains_key("/api/agents/snapshot"));
        assert_eq!(
            paths["/api/agents/snapshot"]["get"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/AgentRosterSnapshot"
        );
        assert!(paths.contains_key("/api/agents/{agent_id}/projection-snapshot"));
        assert_eq!(
            paths["/api/agents/{agent_id}/projection-snapshot"]["get"]["responses"]["200"]
                ["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/AgentProjectionSnapshot"
        );
    }

    fn assert_brief_preview_bounded(latest_brief: Option<&AgentLatestBrief>) {
        let Some(brief) = latest_brief else {
            return;
        };
        assert!(
            brief.preview.len() <= LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES,
            "preview exceeds {} UTF-8 bytes",
            LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES
        );
    }

    mod roster_http_tests {
        use super::super::*;
        use crate::{
            config::AppConfig,
            host::RuntimeHost,
            provider::StubProvider,
            types::{
                AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
            },
        };
        use axum::{
            body::{to_bytes, Body},
            http::{Request, StatusCode},
        };
        use std::sync::Arc;
        use tower::ServiceExt;

        async fn roster_test_host() -> (tempfile::TempDir, RuntimeHost) {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(
                home.path().join("config.json"),
                r#"{"model":{"default":"openai/gpt-5.4"}}"#,
            )
            .unwrap();
            let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
            let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))
                .unwrap();
            host.ensure_default_agent_identity().unwrap();
            host.create_named_agent("web", None).await.unwrap();
            (home, host)
        }

        async fn authenticated_json(
            app: axum::Router,
            credential: &str,
            path: &str,
        ) -> serde_json::Value {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("authorization", format!("Bearer {credential}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(status, StatusCode::OK, "{body}");
            body
        }

        #[tokio::test]
        async fn oidc_login_scope_matches_read_states_and_isolates_snapshot_cache() {
            let (home, _) = roster_test_host().await;
            let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
            config.auth.mode = crate::authentication::AuthenticationMode::Oidc;
            let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))
                .unwrap();
            let now = chrono::Utc::now();
            for (user_id, credential) in [
                ("user-a", "session-a"),
                ("user-b", "session-b"),
                ("user-a", "rotated-session-a"),
            ] {
                let authentication = host.runtime_db().authentication();
                authentication
                    .upsert_user(&crate::authentication::AuthUserRecord {
                        user_id: user_id.to_string(),
                        issuer: "https://issuer.example".to_string(),
                        subject: user_id.to_string(),
                        display_name: None,
                        email: None,
                        created_at: now,
                        updated_at: now,
                        disabled_at: None,
                    })
                    .unwrap();
                authentication
                    .create_session(&crate::authentication::AuthSessionRecord {
                        session_digest: crate::authentication::digest_secret(credential),
                        user_id: user_id.to_string(),
                        auth_method: "oidc".to_string(),
                        created_at: now,
                        expires_at: None,
                        last_seen_at: now,
                        revoked_at: None,
                    })
                    .unwrap();
            }
            let mut state = AppState::for_tcp(host);
            // Force cache hits throughout the test, including between users.
            state.projection_gate = Arc::new(crate::http::projection_gate::ProjectionGate::new(
                4,
                Duration::from_secs(60),
            ));
            let app = crate::http::router(state);
            let mut scopes = Vec::new();
            for credential in ["session-a", "session-b", "rotated-session-a"] {
                let roster =
                    authenticated_json(app.clone(), credential, "/api/agents/snapshot").await;
                let projection = authenticated_json(
                    app.clone(),
                    credential,
                    "/api/agents/web/projection-snapshot",
                )
                .await;
                let reads =
                    authenticated_json(app.clone(), credential, "/api/agents/brief-read-states")
                        .await;
                let read = reads
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|read| read["agent_id"] == "web")
                    .unwrap();
                let scope = roster["visibility_scope_id"].as_str().unwrap().to_string();
                assert_eq!(scope, projection["visibility_scope_id"]);
                assert_eq!(scope, read["visibility_scope_id"]);
                assert_eq!(roster["runtime_id"], projection["runtime_id"]);
                scopes.push(scope);
            }
            assert_ne!(scopes[0], scopes[1], "users must have isolated scopes");
            assert_eq!(scopes[0], scopes[2], "credential rotation preserves scope");
        }

        async fn get_snapshot(state: AppState) -> (StatusCode, serde_json::Value) {
            let app = crate::http::router(state);
            let response = app
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/api/agents/snapshot")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            (status, value)
        }

        #[tokio::test]
        async fn roster_snapshot_serves_membership_anchors_and_bounded_preview() {
            let (_home, host) = roster_test_host().await;
            host.runtime_db()
                .audit_events()
                .append(
                    Some("web"),
                    &crate::types::AuditEvent::legacy(
                        "roster_http_event",
                        serde_json::json!({ "index": 1 }),
                    ),
                )
                .unwrap();
            let long_preview = "预".repeat(400);
            host.runtime_db()
                .connection()
                .unwrap()
                .execute(
                    "INSERT INTO briefs (
                        evidence_id, agent_id, created_at, kind, preview, payload_json
                     ) VALUES ('brief-http', 'web', '2026-02-01T00:00:00.000Z', 'result', ?1, ?2)",
                    rusqlite::params![
                        long_preview,
                        serde_json::json!({
                            "id": "brief-http",
                            "agent_id": "web",
                            "kind": "result",
                            "created_at": "2026-02-01T00:00:00.000Z",
                            "text": "brief text",
                        })
                        .to_string()
                    ],
                )
                .unwrap();

            let (status, body) = get_snapshot(AppState::for_tcp(host)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["contract_version"], 1);
            assert!(!body["runtime_id"].as_str().unwrap().is_empty());
            assert!(!body["event_log_epoch"].as_str().unwrap().is_empty());
            assert!(body["visibility_scope_id"]
                .as_str()
                .is_some_and(|scope| scope.starts_with("vscope1_")));
            let agents = body["agents"].as_array().unwrap();
            let entry = agents
                .iter()
                .find(|entry| entry["agent"]["identity"]["agent_id"] == "web")
                .expect("web membership");
            assert!(entry["event_window"]["event_head_seq"].as_u64().unwrap() >= 1);
            assert!(entry["event_window"]["oldest_retained_seq"].is_u64());
            let brief = entry["latest_brief"].as_object().expect("latest brief");
            assert_eq!(brief["brief_id"], "brief-http");
            assert!(brief["created_event_seq"].is_null());
            let preview = brief["preview"].as_str().unwrap();
            assert!(
                preview.len() <= LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES,
                "preview was not bounded to {} UTF-8 bytes",
                LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES
            );
        }

        #[tokio::test]
        async fn roster_snapshot_authorizes_before_serving() {
            let (_home, host) = roster_test_host().await;
            let mut state = AppState::for_tcp(host);
            state.require_control_token = true;
            let (status, body) = get_snapshot(state).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["code"], "auth_required");
            assert!(body.get("agents").is_none());
        }

        #[tokio::test]
        async fn roster_snapshot_gates_on_durable_capability_verification() {
            let (_home, host) = roster_test_host().await;
            let mut state = AppState::for_tcp(host.clone());
            state.projection_gate =
                Arc::new(crate::http::ProjectionGate::new(1, Duration::from_secs(60)));
            let (status, _body) = get_snapshot(state.clone()).await;
            assert_eq!(status, StatusCode::OK);
            host.runtime_db()
                .connection()
                .unwrap()
                .execute(
                    "UPDATE observer_sync_capability_verifications
                     SET verified = 0 WHERE capability = 'roster_snapshot_verified'",
                    [],
                )
                .unwrap();
            // The same gate still has warm bytes; capability revocation must win.
            let (status, body) = get_snapshot(state).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["code"], "capability_unavailable");
            assert!(body.get("agents").is_none());
        }

        #[tokio::test]
        async fn roster_snapshot_agent_count_limit_is_all_or_nothing() {
            let (_home, host) = roster_test_host().await;
            let mut state = AppState::for_tcp(host);
            state.roster_snapshot_limits.max_agents = 0;
            let (status, body) = get_snapshot(state).await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(body["code"], "roster_snapshot_too_large");
            assert_eq!(body["max_agents"], 0);
            assert!(body.get("agents").is_none());
        }

        #[tokio::test]
        async fn roster_snapshot_serialized_size_limit_rejects_response() {
            let (_home, host) = roster_test_host().await;
            let mut state = AppState::for_tcp(host);
            state.roster_snapshot_limits.max_serialized_bytes = 16;
            let (status, body) = get_snapshot(state).await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(body["code"], "roster_snapshot_too_large");
            assert!(body.get("agents").is_none());
        }

        #[tokio::test]
        async fn roster_snapshot_assembly_failure_fails_whole_response() {
            let (_home, host) = roster_test_host().await;
            host.runtime_db()
                .connection()
                .unwrap()
                .execute(
                    "UPDATE agent_states SET payload_json = '{not json' WHERE agent_id = 'web'",
                    [],
                )
                .unwrap();
            let (status, body) = get_snapshot(AppState::for_tcp(host)).await;
            assert!(status.is_server_error(), "unexpected status {status}");
            assert_eq!(body["ok"], false);
            // All-or-nothing: no partial membership is ever serialized.
            assert!(body.get("agents").is_none());
        }

        #[tokio::test]
        async fn roster_snapshot_excludes_private_children_without_leaking_them() {
            let (_home, host) = roster_test_host().await;
            let mut private = AgentIdentityRecord::new(
                "child-secret",
                AgentKind::Child,
                AgentVisibility::Private,
                AgentOwnership::ParentSupervised,
                AgentProfilePreset::PrivateChild,
                Some("web".into()),
                None,
            );
            private.status = crate::types::AgentRegistryStatus::Active;
            host.runtime_db()
                .agent_identities()
                .upsert(&private)
                .unwrap();
            host.runtime_db()
                .audit_events()
                .append(
                    Some("child-secret"),
                    &crate::types::AuditEvent::legacy(
                        "private_child_event",
                        serde_json::json!({ "secret": true }),
                    ),
                )
                .unwrap();

            let (status, body) = get_snapshot(AppState::for_tcp(host)).await;
            assert_eq!(status, StatusCode::OK);
            let serialized = body.to_string();
            assert!(
                !serialized.contains("child-secret"),
                "private child leaked into the roster: {serialized}"
            );
            let agents = body["agents"].as_array().unwrap();
            // The runtime's default agent plus "web" are public members.
            assert_eq!(agents.len(), 2);
            let ids: Vec<&str> = agents
                .iter()
                .filter_map(|entry| entry["agent"]["identity"]["agent_id"].as_str())
                .collect();
            assert!(ids.contains(&"web"));
            assert!(!ids.contains(&"child-secret"));
        }
    }

    mod projection_http_tests {
        use super::super::*;
        use crate::{
            config::AppConfig,
            host::RuntimeHost,
            provider::StubProvider,
            types::{
                AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
            },
        };
        use axum::{
            body::{to_bytes, Body},
            http::{Request, StatusCode},
        };
        use std::sync::Arc;
        use tower::ServiceExt;

        async fn projection_test_host() -> (tempfile::TempDir, RuntimeHost) {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(
                home.path().join("config.json"),
                r#"{"model":{"default":"openai/gpt-5.4"}}"#,
            )
            .unwrap();
            let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
            let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))
                .unwrap();
            host.create_named_agent("web", None).await.unwrap();
            (home, host)
        }

        #[tokio::test]
        async fn lineage_children_snapshot_survives_host_clone_and_detail_http(
        ) -> anyhow::Result<()> {
            use crate::{
                config::StateMemoryLimits,
                diagnostics::resource_io::ResourceReadMaterial,
                runtime::{
                    input_evidence_budget::InputEvidenceBudgetLedger,
                    resident_input_evidence::InputEvidenceScope,
                },
                types::AgentDetail,
            };
            let (_home, host) = projection_test_host().await;
            let empty = host.operator_agent_detail("web").unwrap();
            let empty_wire = serde_json::to_value(&empty)?;
            assert!(empty_wire.get("lineage_children").is_none());
            assert_eq!(serde_json::from_value::<AgentDetail>(empty_wire)?, empty);
            let child = AgentIdentityRecord::new(
                "lineage-detail-child",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                Some("web".into()),
                None,
            );
            host.runtime_db().agent_identities().upsert(&child)?;
            let baseline = host.operator_agent_detail("web").unwrap();
            assert_eq!(baseline.lineage_children.len(), 1);
            assert!(baseline.lineage_children._material.is_none());
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let scope = InputEvidenceScope {
                agent_id: "lineage-detail-host".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let holder = phase.guard()?;
            let detail = {
                let _entry = phase.enter();
                host.operator_agent_detail("web").unwrap()
            };
            assert_eq!(detail, baseline);
            assert!(Arc::ptr_eq(
                detail.lineage_children._material.as_ref().unwrap(),
                &holder
            ));
            let used = phase.finish()?.0;
            let cloned = detail.clone();
            assert_eq!(cloned, detail);
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            drop((phase, holder, detail));
            assert_eq!(ledger.report()?.observed_bytes, used);
            // Axum Json borrows the typed snapshot while serializing, then drops it.
            let response = axum::response::IntoResponse::into_response(axum::Json(cloned));
            assert_eq!(ledger.report()?.observed_bytes, 0);
            let body = to_bytes(response.into_body(), usize::MAX).await?;
            let decoded: AgentDetail = serde_json::from_slice(&body)?;
            assert_eq!(decoded, baseline);
            assert!(decoded.lineage_children._material.is_none());
            let value: serde_json::Value = serde_json::from_slice(&body)?;
            assert!(value["lineage_children"].is_array());
            assert!(!String::from_utf8(body.to_vec())?.contains("_material"));

            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let app = crate::http::router(AppState::for_tcp(host));
            let response = {
                let _entry = phase.enter();
                app.oneshot(
                    Request::builder()
                        .uri("/api/control/agents/web/detail")
                        .body(Body::empty())?,
                )
                .await?
            };
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await?;
            assert_eq!(serde_json::from_slice::<AgentDetail>(&body)?, baseline);
            assert!(phase.finish()?.0 > (scope.agent_id.len() as u64 + 256) * 8);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            Ok(())
        }

        async fn get_projection_snapshot(
            state: AppState,
            agent_id: &str,
        ) -> (StatusCode, serde_json::Value) {
            let app = crate::http::router(state);
            let response = app
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(format!("/api/agents/{agent_id}/projection-snapshot"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            (status, value)
        }

        #[tokio::test]
        async fn projection_snapshot_host_retains_original_phase_and_releases_failed_assembly() {
            use crate::{
                config::StateMemoryLimits,
                diagnostics::resource_io::ResourceReadMaterial,
                runtime::{
                    input_evidence_budget::InputEvidenceBudgetLedger,
                    resident_input_evidence::InputEvidenceScope,
                },
            };

            let (_home, host) = projection_test_host().await;
            let baseline = host.agent_projection_snapshot("web").unwrap().unwrap();
            assert!(baseline._material.is_some());
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default()).unwrap();
            let scope = InputEvidenceScope {
                agent_id: "projection-host".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
            let holder = phase.guard().unwrap();
            let snapshot = {
                let _entry = phase.enter();
                host.agent_projection_snapshot("web").unwrap().unwrap()
            };
            assert_eq!(snapshot, baseline);
            assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
            assert!(ResourceReadMaterial::active_guard().unwrap().is_none());
            let used = phase.finish().unwrap().0;
            let cloned = snapshot.clone();
            assert_eq!(cloned, snapshot);
            assert!(!Arc::ptr_eq(cloned._material.as_ref().unwrap(), &holder));
            assert_eq!(ledger.report().unwrap().observed_bytes, used * 2);

            host.runtime_db()
                .connection()
                .unwrap()
                .execute(
                    "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
                 VALUES ('web', 'idle', '2026-01-01T00:00:00Z', 'null')
                 ON CONFLICT(agent_id) DO UPDATE SET payload_json = 'null'",
                    [],
                )
                .unwrap();
            {
                let _entry = phase.enter();
                let error = host.agent_projection_snapshot("web").unwrap_err();
                assert!(
                    format!("{error:#}").contains("unreadable projection agent state payload"),
                    "{error:#}",
                );
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard().unwrap().unwrap(),
                    &holder,
                ));
            }
            assert!(phase.finish().unwrap().0 > used);
            // Host failure releases its temporary raw holder, not successful
            // prior ownership or the original phase's cumulative charges.
            drop((phase, holder, snapshot));
            assert_eq!(ledger.report().unwrap().observed_bytes, used);
            assert_eq!(cloned.runtime_id, baseline.runtime_id);
            drop(cloned);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }

        #[tokio::test]
        async fn projection_snapshot_typed_relation_material_survives_host_and_serialization(
        ) -> anyhow::Result<()> {
            use crate::{
                config::StateMemoryLimits,
                diagnostics::resource_io::ResourceReadMaterial,
                runtime::{
                    input_evidence_budget::InputEvidenceBudgetLedger,
                    resident_input_evidence::InputEvidenceScope,
                },
                types::{
                    AgentMessagePolicyRecord, AgentMessagePolicyRule, AgentMessagePrincipalKind,
                    AgentPolicyEffect,
                },
            };
            let (_home, host) = projection_test_host().await;
            let policy = AgentMessagePolicyRecord {
                agent_id: "web".into(),
                revision: 901,
                default_effect: AgentPolicyEffect::Deny,
                rules: vec![
                    AgentMessagePolicyRule {
                        principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                        principal_id: Some("parent-汉字\0\"\\\n".into()),
                        route: Some("route-汉字\0\"\\\n".into()),
                        effect: AgentPolicyEffect::Allow,
                    },
                    AgentMessagePolicyRule {
                        principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                        principal_id: None,
                        route: None,
                        effect: AgentPolicyEffect::Deny,
                    },
                ],
                // Typed SQL relation timestamps have millisecond precision.
                created_at: chrono::DateTime::from_timestamp_millis(1_760_000_000_123).unwrap(),
            };
            host.runtime_db()
                .agent_canonical_relations()
                .upsert_message_policy(&policy)?;
            let baseline = host.agent_projection_snapshot("web")?.unwrap();
            assert_eq!(
                baseline.canonical_relations.message_policy.as_ref(),
                Some(&policy)
            );
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let scope = InputEvidenceScope {
                agent_id: "relation-host".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let holder = phase.guard()?;
            let snapshot = {
                let _entry = phase.enter();
                host.agent_projection_snapshot("web")?.unwrap()
            };
            assert_eq!(snapshot, baseline);
            assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let used = phase.finish()?.0;
            let cloned = snapshot.clone();
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            // This is the relation value moved into AgentCanonicalProjection.
            let wire = serde_json::to_vec(&snapshot.canonical_relations)?;
            let decoded: crate::types::AgentCanonicalRelationsProjection =
                serde_json::from_slice(&wire)?;
            assert_eq!(decoded.message_policy.as_ref(), Some(&policy));
            host.runtime_db().connection()?.execute(
                "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
                 VALUES ('web', 'idle', '2026-01-01T00:00:00Z', 'null')
                 ON CONFLICT(agent_id) DO UPDATE SET payload_json = 'null'",
                [],
            )?;
            {
                let _entry = phase.enter();
                let error = host.agent_projection_snapshot("web").unwrap_err();
                assert!(
                    format!("{error:#}").contains("unreadable projection agent state payload"),
                    "{error:#}"
                );
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
            }
            assert!(phase.finish()?.0 > used);
            drop((phase, holder, snapshot));
            assert_eq!(ledger.report()?.observed_bytes, used);
            assert_eq!(cloned.canonical_relations, decoded);
            drop(cloned);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            Ok(())
        }

        #[tokio::test]
        async fn projection_snapshot_identity_task_material_retains_original_holder(
        ) -> anyhow::Result<()> {
            use crate::{
                config::StateMemoryLimits,
                diagnostics::resource_io::ResourceReadMaterial,
                runtime::{
                    input_evidence_budget::InputEvidenceBudgetLedger,
                    resident_input_evidence::InputEvidenceScope,
                },
                types::{TaskKind, TaskRecord, TaskStatus},
            };
            let (_home, host) = projection_test_host().await;
            let db = host.runtime_db();
            let now = chrono::DateTime::from_timestamp_millis(1_760_000_000_123).unwrap();
            let task = TaskRecord {
                id: "projection-task-汉字".into(),
                agent_id: "web".into(),
                kind: TaskKind::ActorInvocation,
                status: TaskStatus::Running,
                created_at: now,
                updated_at: now,
                parent_message_id: None,
                work_item_id: None,
                summary: Some("escaped-汉字\0\"\\\n{}[]:,".into()),
                detail: Some(serde_json::json!({"work_item_id": "work-fallback-汉字"})),
                recovery: None,
            };
            db.tasks().upsert(&task)?;
            let mut identity = db.agent_identities().latest("web")?.unwrap();
            identity.delegated_from_task_id = Some(task.id.clone());
            db.agent_identities().upsert(&identity)?;
            let baseline = host.agent_projection_snapshot("web")?.unwrap();
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let scope = InputEvidenceScope {
                agent_id: "identity-task-host".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let holder = phase.guard()?;
            let snapshot = {
                let _entry = phase.enter();
                host.agent_projection_snapshot("web")?.unwrap()
            };
            assert_eq!(snapshot, baseline);
            assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let used = phase.finish()?.0;
            let cloned = snapshot.clone();
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            let wire = serde_json::to_vec(&snapshot.canonical_relations)?;
            let decoded: crate::types::AgentCanonicalRelationsProjection =
                serde_json::from_slice(&wire)?;
            assert_eq!(decoded, baseline.canonical_relations);
            let payload = serde_json::to_string(&task)?;
            db.connection()?.execute(
                "UPDATE tasks SET payload_json = 'null' WHERE task_id = ?1",
                [&task.id],
            )?;
            {
                let _entry = phase.enter();
                let error = host.agent_projection_snapshot("web").unwrap_err();
                assert!(
                    format!("{error:#}").contains("decoding legacy supervision task"),
                    "{error:#}"
                );
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
                db.connection()?.execute(
                    "UPDATE tasks SET payload_json = ?1 WHERE task_id = ?2",
                    rusqlite::params![payload, task.id],
                )?;
                assert_eq!(host.agent_projection_snapshot("web")?, Some(baseline));
            }
            assert!(phase.finish()?.0 > used);
            drop((phase, holder, snapshot));
            assert_eq!(ledger.report()?.observed_bytes, used);
            assert_eq!(cloned.canonical_relations, decoded);
            drop(cloned);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            Ok(())
        }

        #[tokio::test]
        async fn projection_snapshot_legacy_relation_ids_keep_original_holder() -> anyhow::Result<()>
        {
            use crate::{
                config::StateMemoryLimits,
                diagnostics::resource_io::ResourceReadMaterial,
                runtime::{
                    input_evidence_budget::InputEvidenceBudgetLedger,
                    resident_input_evidence::InputEvidenceScope,
                },
                types::{
                    AgentCanonicalRelationsProjection, AgentCanonicalValueSource, AgentOwnership,
                    AgentSupervisionState, TaskKind, TaskRecord, TaskStatus,
                },
            };
            let (_home, host) = projection_test_host().await;
            host.create_named_agent("parent", None).await?;
            let db = host.runtime_db();
            let mut identity = db.agent_identities().latest("web")?.unwrap();
            identity.parent_agent_id = Some("parent".into());
            identity.lineage_parent_agent_id = Some("parent".into());
            identity.ownership = Some(AgentOwnership::ParentSupervised);
            identity.delegated_from_task_id = Some("legacy-task-汉字".into());
            db.agent_identities().upsert(&identity)?;
            db.tasks().upsert(&TaskRecord {
                id: "legacy-task-汉字".into(),
                agent_id: "parent".into(),
                kind: TaskKind::ChildAgentTask,
                status: TaskStatus::Running,
                created_at: identity.created_at,
                updated_at: identity.updated_at,
                parent_message_id: None,
                work_item_id: Some("legacy-work-汉字\0\"\\\n".into()),
                summary: None,
                detail: Some(serde_json::json!({"child_agent_id": "web"})),
                recovery: None,
            })?;
            db.connection()?.execute_batch(
                "DELETE FROM agent_lineages WHERE child_agent_id = 'web';
                 DELETE FROM agent_supervisions WHERE child_agent_id = 'web';",
            )?;
            let baseline = host.agent_projection_snapshot("web")?.unwrap();
            assert!(baseline._material.is_some());
            let expected = &baseline.canonical_relations;
            assert_eq!(
                expected.sources.lineage,
                Some(AgentCanonicalValueSource::Legacy)
            );
            assert_eq!(
                expected.sources.supervision,
                Some(AgentCanonicalValueSource::Legacy)
            );
            assert_eq!(expected.lineage.as_ref().unwrap().parent_agent_id, "parent");
            let supervision = expected.supervision.as_ref().unwrap();
            assert_eq!(supervision.supervision_id, "legacy:legacy-task-汉字");
            assert_eq!(supervision.supervisor_agent_id, "parent");
            assert_eq!(supervision.child_agent_id, "web");
            assert_eq!(
                supervision.delegated_from_task_id.as_deref(),
                Some("legacy-task-汉字")
            );
            assert_eq!(
                supervision.delegated_from_work_item_id.as_deref(),
                Some("legacy-work-汉字\0\"\\\n")
            );
            assert_eq!(supervision.state, AgentSupervisionState::Active);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let scope = InputEvidenceScope {
                agent_id: "legacy-relation-host".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let holder = phase.guard()?;
            let snapshot = {
                let _entry = phase.enter();
                host.agent_projection_snapshot("web")?.unwrap()
            };
            assert_eq!(snapshot, baseline);
            assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let used = phase.finish()?.0;
            let cloned = snapshot.clone();
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            drop((phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            let wire = serialize_json(
                "/agents/{agent_id}/projection-snapshot",
                &snapshot.canonical_relations,
            )
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            let decoded: AgentCanonicalRelationsProjection = serde_json::from_slice(&wire)?;
            assert_eq!(decoded, *expected);
            drop(snapshot);
            assert_eq!(ledger.report()?.observed_bytes, used);
            assert_eq!(cloned.canonical_relations, decoded);
            drop(cloned);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            Ok(())
        }

        #[tokio::test]
        async fn legacy_policy_material_moves_through_host_and_http_with_original_holder(
        ) -> anyhow::Result<()> {
            use crate::{
                config::StateMemoryLimits,
                diagnostics::resource_io::ResourceReadMaterial,
                runtime::{
                    input_evidence_budget::InputEvidenceBudgetLedger,
                    resident_input_evidence::InputEvidenceScope,
                },
                types::{
                    AgentCanonicalDurability, AgentCanonicalRelationsProjection,
                    AgentCanonicalValueSource, AgentLifecycleAttachment,
                },
            };
            let (_home, host) = projection_test_host().await;
            host.runtime_db().connection()?.execute_batch(
                "DELETE FROM agent_durability_records WHERE agent_id = 'web';
                 DELETE FROM agent_lifecycle_attachment_records WHERE agent_id = 'web';
                 DELETE FROM agent_capability_policy_rules WHERE agent_id = 'web';
                 DELETE FROM agent_capability_policy_records WHERE agent_id = 'web';
                 DELETE FROM agent_message_policy_rules WHERE agent_id = 'web';
                 DELETE FROM agent_message_policy_records WHERE agent_id = 'web';",
            )?;
            let baseline = host.agent_projection_snapshot("web")?.unwrap();
            assert!(baseline._material.is_some());
            let expected = &baseline.canonical_relations;
            assert_eq!(expected.agent_id, "web");
            for source in [
                expected.sources.durability,
                expected.sources.lifecycle_attachment,
                expected.sources.capability_policy,
                expected.sources.message_policy,
            ] {
                assert_eq!(source, Some(AgentCanonicalValueSource::Legacy));
            }
            assert_eq!(
                expected.durability.as_ref().unwrap().durability,
                AgentCanonicalDurability::Persistent
            );
            assert_eq!(
                expected.lifecycle_attachment.as_ref().unwrap().attachment,
                AgentLifecycleAttachment::Independent
            );
            assert_eq!(expected.capability_policy.as_ref().unwrap().rules.len(), 6);
            assert_eq!(expected.message_policy.as_ref().unwrap().rules.len(), 2);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let scope = InputEvidenceScope {
                agent_id: "legacy-policy-host".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let holder = phase.guard()?;
            let snapshot = {
                let _entry = phase.enter();
                host.agent_projection_snapshot("web")?.unwrap()
            };
            assert_eq!(snapshot, baseline);
            assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let used = phase.finish()?.0;
            let cloned = snapshot.clone();
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            drop((phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            let wire = serialize_json(
                "/agents/{agent_id}/projection-snapshot",
                &snapshot.canonical_relations,
            )
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            let decoded: AgentCanonicalRelationsProjection = serde_json::from_slice(&wire)?;
            assert_eq!(decoded, *expected);
            drop(snapshot);
            assert_eq!(ledger.report()?.observed_bytes, used);
            assert_eq!(cloned.canonical_relations, decoded);
            drop(cloned);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            let (status, body) = get_projection_snapshot(AppState::for_tcp(host), "web").await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let served: AgentCanonicalRelationsProjection =
                serde_json::from_value(body["projection"]["relations"].clone())?;
            assert_eq!(served, decoded);
            Ok(())
        }

        #[tokio::test]
        async fn relation_diagnostics_keep_original_holder_through_host_http_and_failure(
        ) -> anyhow::Result<()> {
            use crate::{
                config::StateMemoryLimits,
                diagnostics::resource_io::ResourceReadMaterial,
                runtime::{
                    input_evidence_budget::InputEvidenceBudgetLedger,
                    resident_input_evidence::InputEvidenceScope,
                },
                types::{AgentCanonicalRelationsProjection, AgentCanonicalResolution},
            };
            let (_home, host) = projection_test_host().await;
            host.runtime_db().connection()?.execute(
                "UPDATE agent_identities SET payload_json = json_set(
                   payload_json, '$.profile_preset', 'private_child',
                   '$.durability', 'ephemeral') WHERE agent_id = 'web'",
                [],
            )?;
            let baseline = host.agent_projection_snapshot("web")?.unwrap();
            assert!(baseline._material.is_some());
            assert_eq!(
                baseline.canonical_relations.resolution,
                AgentCanonicalResolution::Contradictory
            );
            let details: Vec<_> = baseline
                .canonical_relations
                .issues
                .iter()
                .map(|issue| issue.detail.as_str())
                .collect();
            assert_eq!(
                details,
                [
                    "private_child legacy preset has no lifecycle ownership evidence",
                    "canonical durability differs from explicit legacy durability",
                ]
            );
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let scope = InputEvidenceScope {
                agent_id: "diagnostic-host".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope)?;
            let holder = phase.guard()?;
            let snapshot = {
                let _entry = phase.enter();
                host.agent_projection_snapshot("web")?.unwrap()
            };
            assert_eq!(snapshot, baseline);
            assert!(Arc::ptr_eq(snapshot._material.as_ref().unwrap(), &holder));
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let used = phase.finish()?.0;
            let cloned = snapshot.clone();
            assert_eq!(ledger.report()?.observed_bytes, used * 2);
            let wire = serialize_json(
                "/agents/{agent_id}/projection-snapshot",
                &snapshot.canonical_relations,
            )
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            let decoded: AgentCanonicalRelationsProjection = serde_json::from_slice(&wire)?;
            assert_eq!(decoded, baseline.canonical_relations);
            // A failure after diagnostic assembly must not leak the raw holder.
            host.runtime_db().connection()?.execute(
                "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
                 VALUES ('web', 'idle', '2026-01-01T00:00:00Z', 'null')
                 ON CONFLICT(agent_id) DO UPDATE SET payload_json = 'null'",
                [],
            )?;
            {
                let _entry = phase.enter();
                let error = host.agent_projection_snapshot("web").unwrap_err();
                assert!(format!("{error:#}").contains("unreadable projection agent state payload"));
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder,
                ));
            }
            assert!(phase.finish()?.0 > used);
            drop((phase, holder, snapshot));
            assert_eq!(ledger.report()?.observed_bytes, used);
            assert_eq!(cloned.canonical_relations, decoded);
            drop(cloned);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            host.runtime_db()
                .connection()?
                .execute("DELETE FROM agent_states WHERE agent_id = 'web'", [])?;
            let (status, body) = get_projection_snapshot(AppState::for_tcp(host), "web").await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let served: AgentCanonicalRelationsProjection =
                serde_json::from_value(body["projection"]["relations"].clone())?;
            assert_eq!(served, decoded);
            Ok(())
        }

        #[tokio::test]
        async fn projection_snapshot_serves_anchors_at_one_boundary() {
            let (_home, host) = projection_test_host().await;
            let mut work_item_record = crate::types::WorkItemRecord::new(
                "web",
                "objective",
                crate::types::WorkItemState::Open,
            );
            work_item_record.id = "work-http".to_string();
            work_item_record.revision = 7;
            work_item_record.plan_status = crate::types::WorkItemPlanStatus::Ready;
            let work_item_payload = serde_json::to_string(&work_item_record).unwrap();
            let connection = host.runtime_db().connection().unwrap();
            connection
                .execute(
                    "INSERT INTO work_items (
                        work_item_id, agent_id, state, objective, plan_status, revision,
                        current_focus, created_at, updated_at, payload_json
                     ) VALUES (
                        'work-http', 'web', 'open', 'objective', 'ready', 7, 1,
                        '2026-02-01T00:00:00.000Z', '2026-02-02T00:00:00.000Z', ?1)",
                    [&work_item_payload],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO messages (
                        evidence_id, agent_id, created_at, kind, payload_json
                     ) VALUES ('msg-http', 'web', '2026-02-01T00:00:00.000Z', 'operator_prompt', '{}')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO transcript_entries (
                        evidence_id, agent_id, created_at, kind, payload_json
                     ) VALUES ('te-http', 'web', '2026-02-01T00:00:00.000Z', 'assistant', '{}')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO briefs (
                        evidence_id, agent_id, created_event_seq, created_at, kind, preview, payload_json
                     ) VALUES ('brief-http', 'web', 1, '2026-02-01T00:00:00.000Z', 'result', 'preview', '{}')",
                    [],
                )
                .unwrap();
            drop(connection);
            host.runtime_db()
                .audit_events()
                .append(
                    Some("web"),
                    &crate::types::AuditEvent::legacy(
                        "projection_http_event",
                        serde_json::json!({ "index": 1 }),
                    ),
                )
                .unwrap();
            host.runtime_db()
                .audit_events()
                .append(
                    Some("web"),
                    &crate::types::AuditEvent::legacy(
                        "projection_http_event",
                        serde_json::json!({ "index": 2 }),
                    ),
                )
                .unwrap();

            let (status, body) = get_projection_snapshot(AppState::for_tcp(host), "web").await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                body["contract_version"],
                AGENT_PROJECTION_SNAPSHOT_CONTRACT_VERSION
            );
            assert_eq!(body["agent_id"], "web");
            let head = body["event_head_seq"].as_u64().unwrap();
            assert!(head >= 2);
            // One committed view: the boundary equals the head it read.
            assert_eq!(body["snapshot_through_seq"].as_u64().unwrap(), head);
            assert_eq!(body["oldest_retained_seq"].as_u64(), Some(0));
            assert!(body["visibility_scope_id"]
                .as_str()
                .is_some_and(|scope| scope.starts_with("vscope1_")));
            let projection = &body["projection"];
            assert_eq!(projection["agent"]["identity"]["agent_id"], "web");
            let work_item = &projection["current_work_item"];
            assert_eq!(work_item["work_item_id"], "work-http");
            assert_eq!(work_item["state"], "open");
            assert_eq!(work_item["plan_status"], "ready");
            assert_eq!(work_item["revision"], 7);
            assert_eq!(projection["conversation"]["latest_message_id"], "msg-http");
            assert_eq!(
                projection["conversation"]["latest_transcript_entry_id"],
                "te-http"
            );
            assert_eq!(projection["latest_brief"]["brief_id"], "brief-http");
            assert_eq!(
                projection["latest_brief"]["created_event_seq"],
                serde_json::json!(1)
            );
            let references = projection["hydration_references"]
                .as_array()
                .expect("hydration references");
            let keyed: Vec<(String, String)> = references
                .iter()
                .map(|reference| {
                    (
                        reference["record_kind"].as_str().unwrap().to_string(),
                        reference["record_id"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            assert!(keyed.contains(&("message".into(), "msg-http".into())));
            assert!(keyed.contains(&("transcript_entry".into(), "te-http".into())));
            assert!(keyed.contains(&("brief".into(), "brief-http".into())));
            assert_eq!(
                projection["hydration_tombstones"].as_array().map(Vec::len),
                Some(0)
            );
        }

        #[tokio::test]
        async fn projection_snapshot_replays_only_events_after_boundary() {
            let (_home, host) = projection_test_host().await;
            // Identity-only membership: no runtime activation, so the only
            // events for this Agent are the ones appended below.
            host.runtime_db()
                .agent_identities()
                .upsert(&crate::types::AgentIdentityRecord::new(
                    "quiet",
                    crate::types::AgentKind::Named,
                    crate::types::AgentVisibility::Public,
                    crate::types::AgentOwnership::SelfOwned,
                    crate::types::AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ))
                .unwrap();
            host.runtime_db()
                .audit_events()
                .append(
                    Some("quiet"),
                    &crate::types::AuditEvent::legacy("before_boundary", serde_json::json!({})),
                )
                .unwrap();
            let (status, body) =
                get_projection_snapshot(AppState::for_tcp(host.clone()), "quiet").await;
            assert_eq!(status, StatusCode::OK);
            let boundary = body["snapshot_through_seq"].as_u64().unwrap();
            let replayable: i64 = host
                .runtime_db()
                .connection()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM audit_events WHERE agent_id = 'quiet' AND event_seq <= ?1",
                    [boundary as i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(replayable, boundary as i64);
            // A later event stays readable through the raw page cursor.
            host.runtime_db()
                .audit_events()
                .append(
                    Some("quiet"),
                    &crate::types::AuditEvent::legacy("after_boundary", serde_json::json!({})),
                )
                .unwrap();
            let after: i64 = host
                .runtime_db()
                .connection()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM audit_events WHERE agent_id = 'quiet' AND event_seq > ?1",
                    [boundary as i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(after, 1);
        }

        #[tokio::test]
        async fn projection_snapshot_authorizes_before_serving() {
            let (_home, host) = projection_test_host().await;
            let mut state = AppState::for_tcp(host);
            state.require_control_token = true;
            let (status, body) = get_projection_snapshot(state, "web").await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["code"], "auth_required");
            assert!(body.get("projection").is_none());
            assert!(body.get("runtime_id").is_none());
        }

        #[tokio::test]
        async fn projection_snapshot_gates_on_durable_capability_verification() {
            let (_home, host) = projection_test_host().await;
            let mut state = AppState::for_tcp(host.clone());
            state.projection_gate =
                Arc::new(crate::http::ProjectionGate::new(1, Duration::from_secs(60)));
            let (status, _body) = get_projection_snapshot(state.clone(), "web").await;
            assert_eq!(status, StatusCode::OK);
            host.runtime_db()
                .connection()
                .unwrap()
                .execute(
                    "UPDATE observer_sync_capability_verifications
                     SET verified = 0 WHERE capability = 'projection_snapshot_verified'",
                    [],
                )
                .unwrap();
            let (status, body) = get_projection_snapshot(state, "web").await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["code"], "capability_unavailable");
            assert!(body.get("projection").is_none());
        }

        #[tokio::test]
        async fn projection_snapshot_not_found_shape_hides_membership() {
            let (_home, host) = projection_test_host().await;
            let mut private = AgentIdentityRecord::new(
                "child-secret",
                AgentKind::Child,
                AgentVisibility::Private,
                AgentOwnership::ParentSupervised,
                AgentProfilePreset::PrivateChild,
                Some("web".into()),
                None,
            );
            private.status = crate::types::AgentRegistryStatus::Active;
            host.runtime_db()
                .agent_identities()
                .upsert(&private)
                .unwrap();
            host.runtime_db()
                .audit_events()
                .append(
                    Some("child-secret"),
                    &crate::types::AuditEvent::legacy(
                        "private_child_event",
                        serde_json::json!({ "secret": true }),
                    ),
                )
                .unwrap();

            let (unknown_status, unknown_body) =
                get_projection_snapshot(AppState::for_tcp(host.clone()), "never-existed").await;
            assert_eq!(unknown_status, StatusCode::NOT_FOUND);
            assert_eq!(unknown_body["code"], "agent_not_found");
            // The not-found shape carries no runtime, epoch, or scope facts.
            assert!(unknown_body.get("runtime_id").is_none());
            assert!(unknown_body.get("event_log_epoch").is_none());
            assert!(unknown_body.get("visibility_scope_id").is_none());

            let (private_status, private_body) =
                get_projection_snapshot(AppState::for_tcp(host), "child-secret").await;
            assert_eq!(private_status, unknown_status);
            assert_eq!(private_body["code"], unknown_body["code"]);
            assert_eq!(private_body["error"], unknown_body["error"]);
        }

        #[tokio::test]
        async fn projection_snapshot_serialized_size_limit_rejects_response() {
            let (_home, host) = projection_test_host().await;
            let mut state = AppState::for_tcp(host);
            state.projection_snapshot_limits.max_serialized_bytes = 16;
            let (status, body) = get_projection_snapshot(state, "web").await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(body["code"], "projection_snapshot_too_large");
            assert!(body.get("projection").is_none());
        }

        #[tokio::test]
        async fn projection_snapshot_assembly_failure_fails_whole_response() {
            let (_home, host) = projection_test_host().await;
            host.runtime_db()
                .connection()
                .unwrap()
                .execute(
                    "UPDATE agent_states SET payload_json = '{not json' WHERE agent_id = 'web'",
                    [],
                )
                .unwrap();
            let (status, body) = get_projection_snapshot(AppState::for_tcp(host), "web").await;
            assert!(status.is_server_error(), "unexpected status {status}");
            assert_eq!(body["ok"], false);
            // All-or-nothing: no partial projection is ever serialized.
            assert!(body.get("projection").is_none());
            assert!(body.get("runtime_id").is_none());
        }
    }
}
