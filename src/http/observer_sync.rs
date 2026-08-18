//! Observer-sync HTTP contract skeleton.
//!
//! Implements the contract surface of
//! `docs/rfcs/observer-sync-agent-summary-and-read-markers.md` (S0):
//! snapshot DTOs, projection-effect classification, the rich cursor error
//! shape, and the capability evaluator. The snapshot endpoints are
//! intentionally not registered yet: a capability is advertised only after
//! its durable verification succeeds, and until a verification source exists
//! (S1+) the evaluator keeps all four capabilities disabled.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{AgentListEntry, WorkItemPlanStatus, WorkItemState};

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

/// Additive `StreamEventEnvelope` classification published by event pages
/// and SSE once `events.projection-effect.v1` is enabled. The runtime event
/// registry is the source of truth; legacy or otherwise unclassified events
/// default conservatively to `DisplayInvalidation`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectionEffect {
    /// Does not affect the Web projection; never blocks readiness.
    None,
    /// Invalidates display state derived from the referenced record; blocks
    /// projection readiness until resolved.
    DisplayInvalidation,
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
        // S0 registers no snapshot routes: route existence must never be
        // mistaken for capability support.
        assert!(!paths.contains_key("/api/agents/snapshot"));
        assert!(!paths.contains_key("/api/agents/{agent_id}/projection-snapshot"));
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
}
