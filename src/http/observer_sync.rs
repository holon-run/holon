//! Observer-sync HTTP contract surface and roster snapshot handler.
//!
//! Implements the contract surface of
//! `docs/rfcs/observer-sync-agent-summary-and-read-markers.md` (S0):
//! snapshot DTOs, projection-effect classification, the rich cursor error
//! shape, and the capability evaluator, plus the S4 roster snapshot
//! handler and the S5 per-Agent projection snapshot handler. A capability
//! is advertised only after its durable verification
//! succeeds, so route registration alone never serves a snapshot contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::diagnostics;
use crate::types::{AgentListEntry, WorkItemPlanStatus, WorkItemState};
// The projection-effect classification lives with the runtime event registry
// (its source of truth); re-exported here to keep the S0 contract surface.
use super::{
    agents::load_observer_sync_verification, auth_required, authorize_remote_access,
    error_response, http_error, projection_gate_error_response, serialize_json, traced_json_bytes,
    AppState, AxumResponse, HttpErrorEnvelope, IntoResponse, ProjectionFailure, ProjectionKey,
};
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use crate::runtime_event::ProjectionEffect;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};

pub(crate) const ROSTER_SNAPSHOT_CAPABILITY: &str = "agents.roster-snapshot.v1";
pub(crate) const PROJECTION_SNAPSHOT_CAPABILITY: &str = "agents.projection-snapshot.v1";
pub(crate) const PROJECTION_EFFECT_CAPABILITY: &str = "events.projection-effect.v1";
pub(crate) const ATOMIC_BRIEF_CREATED_EVENT_CAPABILITY: &str = "briefs.atomic-created-event.v1";

pub(crate) const AGENT_ROSTER_SNAPSHOT_CONTRACT_VERSION: u32 = 1;
pub(crate) const AGENT_PROJECTION_SNAPSHOT_CONTRACT_VERSION: u32 = 1;

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
    /// Agent identity reservation/tombstones are durable; same-epoch
    /// `agent_id` reuse is impossible.
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
}

/// Evaluates the four observer-sync capabilities independently. Snapshot
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
    /// First raw event still replayable in that view; `null` when the Agent
    /// has no events (`event_head_seq = 0`).
    pub(crate) oldest_retained_seq: Option<u64>,
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
    pub(crate) oldest_retained_seq: Option<u64>,
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
    pub(crate) oldest_retained_seq: Option<u64>,
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
        return auth_required(error.to_string()).into_response();
    }
    let verification = load_observer_sync_verification(&state);
    if !advertised_observer_sync_capabilities(&verification).contains(&ROSTER_SNAPSHOT_CAPABILITY) {
        diagnostics::record_roster_snapshot_failure();
        return http_error(
            StatusCode::SERVICE_UNAVAILABLE,
            HttpErrorEnvelope::new(
                "the agents.roster-snapshot.v1 capability is not verified for this database",
            )
            .code("capability_unavailable")
            .hint("see the handshake capabilities; route registration alone never serves this contract"),
        )
        .into_response();
    }
    let gate_state = Arc::clone(&state);
    let result = state
        .projection_gate
        .run(ProjectionKey::AgentsRosterSnapshot, || async {
            let limits = gate_state.roster_snapshot_limits.clone();
            let host = gate_state.host.clone();
            let snapshot = match tokio::time::timeout(
                limits.timeout,
                tokio::task::spawn_blocking(move || host.agent_roster_snapshot()),
            )
            .await
            {
                Ok(joined) => joined
                    .map_err(|error| ProjectionFailure::from(error_response(error.into())))?
                    .map_err(|error| ProjectionFailure::from(error_response(error)))?,
                Err(_) => {
                    return Err(ProjectionFailure::from(http_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        HttpErrorEnvelope::new(format!(
                            "roster snapshot assembly exceeded the {} second budget",
                            limits.timeout.as_secs(),
                        ))
                        .code("roster_snapshot_timeout")
                        .retryable(true),
                    )));
                }
            };
            if snapshot.agents.len() > limits.max_agents {
                return Err(ProjectionFailure::from(http_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    HttpErrorEnvelope::new(
                        "roster snapshot exceeds the maximum Agent count for one response",
                    )
                    .code("roster_snapshot_too_large")
                    .extension("agent_count", snapshot.agents.len())
                    .extension("max_agents", limits.max_agents),
                )));
            }
            let visibility_scope_id = observer_visibility_scope(
                &gate_state,
                &snapshot.runtime_id,
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
                        latest_brief: entry.latest_brief.map(|brief| AgentLatestBrief {
                            brief_id: brief.brief_id,
                            created_event_seq: brief.created_event_seq,
                            created_at: brief.created_at,
                            preview: brief.preview,
                        }),
                    })
                    .collect(),
            };
            let agent_count = snapshot.agents.len();
            let bytes =
                serialize_json("/agents/snapshot", &snapshot).map_err(ProjectionFailure::from)?;
            if bytes.len() > limits.max_serialized_bytes {
                return Err(ProjectionFailure::from(http_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    HttpErrorEnvelope::new(
                        "roster snapshot exceeds the maximum serialized response size",
                    )
                    .code("roster_snapshot_too_large")
                    .extension("serialized_bytes", bytes.len())
                    .extension("max_serialized_bytes", limits.max_serialized_bytes),
                )));
            }
            diagnostics::record_roster_snapshot(started_at.elapsed(), agent_count, bytes.len());
            Ok(bytes)
        })
        .await;
    match result {
        Ok(bytes) => traced_json_bytes("/agents/snapshot", started_at, bytes),
        Err(error) => {
            diagnostics::record_roster_snapshot_failure();
            projection_gate_error_response(error)
        }
    }
}

/// Derives the observer scope for the caller from facts inside the snapshot
/// read view plus the request's resolved authority mode. Credentials are
/// never an input, so token rotation with unchanged entitlement keeps the
/// scope stable.
fn observer_visibility_scope(
    state: &AppState,
    runtime_id: &str,
    visibility_policy_generation: u64,
) -> String {
    let (principal, entitlement) = if state.require_control_token {
        (CONTROL_SCOPE_PRINCIPAL, CONTROL_SCOPE_ENTITLEMENT)
    } else {
        (
            crate::runtime_db::observer_sync::PUBLIC_SCOPE_PRINCIPAL,
            crate::runtime_db::observer_sync::PUBLIC_SCOPE_ENTITLEMENT,
        )
    };
    crate::ids::visibility_scope_id(
        runtime_id,
        principal,
        entitlement,
        visibility_policy_generation,
    )
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
        return auth_required(error.to_string()).into_response();
    }
    let verification = load_observer_sync_verification(&state);
    if !advertised_observer_sync_capabilities(&verification)
        .contains(&PROJECTION_SNAPSHOT_CAPABILITY)
    {
        diagnostics::record_projection_snapshot_failure();
        return http_error(
            StatusCode::SERVICE_UNAVAILABLE,
            HttpErrorEnvelope::new(
                "the agents.projection-snapshot.v1 capability is not verified for this database",
            )
            .code("capability_unavailable")
            .hint("see the handshake capabilities; route registration alone never serves this contract"),
        )
        .into_response();
    }
    let gate_state = Arc::clone(&state);
    let boundary_agent_id = agent_id.clone();
    let result = state
        .projection_gate
        .run(
            ProjectionKey::AgentProjectionSnapshot(agent_id.clone()),
            || async {
                let limits = gate_state.projection_snapshot_limits.clone();
                let host = gate_state.host.clone();
                let snapshot = match tokio::time::timeout(
                    limits.timeout,
                    tokio::task::spawn_blocking(move || {
                        host.agent_projection_snapshot(&boundary_agent_id)
                    }),
                )
                .await
                {
                    Ok(joined) => joined
                        .map_err(|error| ProjectionFailure::from(error_response(error.into())))?
                        .map_err(|error| ProjectionFailure::from(error_response(error)))?,
                    Err(_) => {
                        return Err(ProjectionFailure::from(http_error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            HttpErrorEnvelope::new(format!(
                                "projection snapshot assembly exceeded the {} second budget",
                                limits.timeout.as_secs(),
                            ))
                            .code("projection_snapshot_timeout")
                            .retryable(true),
                        )));
                    }
                };
                let Some(snapshot) = snapshot else {
                    // Unknown, private, and deleted identities share one
                    // not-found shape: no runtime, epoch, or scope facts.
                    return Err(ProjectionFailure::from(http_error(
                        StatusCode::NOT_FOUND,
                        HttpErrorEnvelope::new("no accessible Agent for this request")
                            .code("agent_not_found"),
                    )));
                };
                let visibility_scope_id = observer_visibility_scope(
                    &gate_state,
                    &snapshot.runtime_id,
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
                        current_work_item: snapshot.current_work_item.map(|work_item| {
                            AgentWorkItemAnchor {
                                work_item_id: work_item.work_item_id,
                                state: work_item.state,
                                plan_status: work_item.plan_status,
                                revision: work_item.revision,
                                updated_at: work_item.updated_at,
                            }
                        }),
                        conversation: ConversationRevisionAnchors {
                            latest_message_id: snapshot.conversation.latest_message_id,
                            latest_transcript_entry_id: snapshot
                                .conversation
                                .latest_transcript_entry_id,
                        },
                        latest_brief: snapshot.latest_brief.map(|brief| AgentLatestBrief {
                            brief_id: brief.brief_id,
                            created_event_seq: brief.created_event_seq,
                            created_at: brief.created_at,
                            preview: brief.preview,
                        }),
                        hydration_tombstones: Vec::new(),
                        hydration_references: snapshot
                            .hydration_references
                            .into_iter()
                            .map(|reference| AgentHydrationKey {
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
                            })
                            .collect(),
                    },
                };
                let bytes = serialize_json("/agents/{agent_id}/projection-snapshot", &snapshot)
                    .map_err(ProjectionFailure::from)?;
                if bytes.len() > limits.max_serialized_bytes {
                    return Err(ProjectionFailure::from(http_error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        HttpErrorEnvelope::new(
                            "projection snapshot exceeds the maximum serialized response size",
                        )
                        .code("projection_snapshot_too_large")
                        .extension("serialized_bytes", bytes.len())
                        .extension("max_serialized_bytes", limits.max_serialized_bytes),
                    )));
                }
                diagnostics::record_projection_snapshot(started_at.elapsed(), bytes.len());
                Ok(bytes)
            },
        )
        .await;
    match result {
        Ok(bytes) => traced_json_bytes("/agents/{agent_id}/projection-snapshot", started_at, bytes),
        Err(error) => {
            diagnostics::record_projection_snapshot_failure();
            projection_gate_error_response(error)
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
    fn evaluator_disables_all_capabilities_without_verification() {
        let advertised = advertised_observer_sync_capabilities(&Default::default());
        assert!(advertised.is_empty());
    }

    #[test]
    fn evaluator_requires_identity_prerequisites_for_snapshot_capabilities() {
        let verification = ObserverSyncCapabilityVerification {
            roster_snapshot_verified: true,
            projection_snapshot_verified: true,
            brief_atomic_linkage_verified: true,
            event_projection_effect_complete: true,
            ..Default::default()
        };
        let advertised = advertised_observer_sync_capabilities(&verification);
        assert!(!advertised.contains(&ROSTER_SNAPSHOT_CAPABILITY));
        assert!(!advertised.contains(&PROJECTION_SNAPSHOT_CAPABILITY));
        assert!(advertised.contains(&PROJECTION_EFFECT_CAPABILITY));
        assert!(advertised.contains(&ATOMIC_BRIEF_CREATED_EVENT_CAPABILITY));
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
            vec![ROSTER_SNAPSHOT_CAPABILITY]
        );

        let verification = ObserverSyncCapabilityVerification {
            runtime_identity_stable: true,
            agent_identity_reserved: true,
            projection_snapshot_verified: true,
            ..Default::default()
        };
        assert_eq!(
            advertised_observer_sync_capabilities(&verification),
            vec![PROJECTION_SNAPSHOT_CAPABILITY]
        );
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
            host.create_named_agent("web", None).await.unwrap();
            (home, host)
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
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["code"], "auth_required");
            assert!(body.get("agents").is_none());
        }

        #[tokio::test]
        async fn roster_snapshot_gates_on_durable_capability_verification() {
            let (_home, host) = roster_test_host().await;
            let (status, _body) = get_snapshot(AppState::for_tcp(host.clone())).await;
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
            let (status, body) = get_snapshot(AppState::for_tcp(host)).await;
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
            assert_eq!(body["contract_version"], 1);
            assert_eq!(body["agent_id"], "web");
            let head = body["event_head_seq"].as_u64().unwrap();
            assert!(head >= 2);
            // One committed view: the boundary equals the head it read.
            assert_eq!(body["snapshot_through_seq"].as_u64().unwrap(), head);
            assert_eq!(body["oldest_retained_seq"].as_u64().unwrap_or(1), 1);
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
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["code"], "auth_required");
            assert!(body.get("projection").is_none());
            assert!(body.get("runtime_id").is_none());
        }

        #[tokio::test]
        async fn projection_snapshot_gates_on_durable_capability_verification() {
            let (_home, host) = projection_test_host().await;
            let (status, _body) =
                get_projection_snapshot(AppState::for_tcp(host.clone()), "web").await;
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
            let (status, body) = get_projection_snapshot(AppState::for_tcp(host), "web").await;
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
