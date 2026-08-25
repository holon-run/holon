use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{
    AgentStateChangedEvent, BriefCreatedAuditEvent, MessageLifecycleAuditEvent,
    SchedulerDiagnosticAuditEvent, TaskLifecycleAuditEvent, WorkItemLifecycleAuditEvent,
};

/// Envelope contract version emitted while `events.projection-effect.v1` is
/// advertised. Version 3 adds the additive `projection_effect` field.
pub const RUNTIME_EVENT_CONTRACT_VERSION: u32 = 3;
pub const LEGACY_RUNTIME_EVENT_CONTRACT_VERSION: u32 = 1;
pub const LEGACY_PAYLOAD_SCHEMA: &str = "holon.runtime_event.legacy";

/// Additive `StreamEventEnvelope` classification published by event pages
/// and SSE once `events.projection-effect.v1` is enabled. The runtime event
/// registry is the source of truth; legacy or otherwise unclassified events
/// default conservatively to `DisplayInvalidation`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionEffect {
    /// Does not affect the Web projection; never blocks readiness.
    None,
    /// Invalidates display state derived from the referenced record; blocks
    /// projection readiness until resolved.
    DisplayInvalidation,
}

/// Sound classification of one persisted runtime event envelope.
///
/// Exact typed events use the registry-declared effect. Recognizable legacy
/// envelopes are safe but intentionally conservative. Unsupported typed
/// shapes prevent the durable projection-effect capability from being
/// advertised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionEffectClassification {
    Exact(ProjectionEffect),
    ConservativeLegacy(ProjectionEffect),
    Unsupported(UnsupportedProjectionEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedProjectionEvent {
    UnknownTypedKind,
    PayloadSchemaMismatch,
    FuturePayloadSchemaVersion,
    InvalidMetadata,
}

/// Every `RuntimeEventKind` variant, in declaration order. The registry
/// inventory tests and the durable capability verification use this list to
/// prove each served event family is classified exactly once.
pub const ALL_RUNTIME_EVENT_KINDS: &[RuntimeEventKind] = &[
    RuntimeEventKind::MessageEnqueued,
    RuntimeEventKind::MessageProcessingStarted,
    RuntimeEventKind::BriefCreated,
    RuntimeEventKind::TaskCreated,
    RuntimeEventKind::TaskStatusUpdated,
    RuntimeEventKind::TaskResultReceived,
    RuntimeEventKind::WorkItemWritten,
    RuntimeEventKind::AgentStateChanged,
    RuntimeEventKind::SchedulerDiagnostic,
];

pub fn legacy_contract_version() -> u32 {
    LEGACY_RUNTIME_EVENT_CONTRACT_VERSION
}

pub fn legacy_payload_schema() -> String {
    LEGACY_PAYLOAD_SCHEMA.to_string()
}

pub fn legacy_payload_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventDisplayFamily {
    Message,
    Brief,
    Task,
    WorkItem,
    AgentState,
    Scheduler,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventKind {
    MessageEnqueued,
    MessageProcessingStarted,
    BriefCreated,
    TaskCreated,
    TaskStatusUpdated,
    TaskResultReceived,
    WorkItemWritten,
    AgentStateChanged,
    SchedulerDiagnostic,
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeEventDescriptor {
    pub kind: RuntimeEventKind,
    pub wire_name: &'static str,
    pub payload_schema: &'static str,
    pub payload_schema_version: u32,
    pub display_family: RuntimeEventDisplayFamily,
    pub projection_effect: ProjectionEffect,
    #[schemars(skip)]
    pub fixture_json: &'static str,
}

pub trait RuntimeEventPayload: Serialize {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32 = 1;
}

impl RuntimeEventPayload for MessageLifecycleAuditEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.message_lifecycle";
}

impl RuntimeEventPayload for BriefCreatedAuditEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.brief_created";
}

impl RuntimeEventPayload for TaskLifecycleAuditEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.task_lifecycle";
}

impl RuntimeEventPayload for WorkItemLifecycleAuditEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.work_item_lifecycle";
}

impl RuntimeEventPayload for AgentStateChangedEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.agent_state_changed";
}

impl RuntimeEventPayload for SchedulerDiagnosticAuditEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.scheduler_diagnostic";
}

const REGISTRY: &[RuntimeEventDescriptor] = &[
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::MessageEnqueued,
        wire_name: "message_enqueued",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: MessageLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: MessageLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Message,
        fixture_json: r#"{"message_id":"msg_fixture","agent_id":"default","kind":"operator_prompt","origin":{"kind":"system","subsystem":"fixture"},"authority_class":"runtime_instruction","priority":"normal","source_refs":{}}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::MessageProcessingStarted,
        wire_name: "message_processing_started",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: MessageLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: MessageLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Message,
        fixture_json: r#"{"message_id":"msg_fixture","agent_id":"default","kind":"operator_prompt","origin":{"kind":"system","subsystem":"fixture"},"authority_class":"runtime_instruction","priority":"normal","source_refs":{}}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::BriefCreated,
        wire_name: "brief_created",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: BriefCreatedAuditEvent::SCHEMA_ID,
        payload_schema_version: BriefCreatedAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Brief,
        fixture_json: r#"{"brief_id":"brief_fixture","agent_id":"default","workspace_id":"agent_home","kind":"result","created_at":"2026-01-01T00:00:00Z","content_source":{"kind":"inline"},"content_char_count":0}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::TaskCreated,
        wire_name: "task_created",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: TaskLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: TaskLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Task,
        fixture_json: r#"{"task_id":"task_fixture","agent_id":"default","kind":"command_task","status":"queued","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::TaskStatusUpdated,
        wire_name: "task_status_updated",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: TaskLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: TaskLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Task,
        fixture_json: r#"{"task_id":"task_fixture","agent_id":"default","kind":"command_task","status":"running","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::TaskResultReceived,
        wire_name: "task_result_received",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: TaskLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: TaskLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Task,
        fixture_json: r#"{"task_id":"task_fixture","agent_id":"default","kind":"command_task","status":"completed","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::WorkItemWritten,
        wire_name: "work_item_written",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: WorkItemLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: WorkItemLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::WorkItem,
        fixture_json: r#"{"agent_id":"default","work_item_id":"work_fixture","workspace_id":"agent_home","revision":1,"action":"created","state":"open","plan_status":"draft","readiness":"runnable","updated_at":"2026-01-01T00:00:00Z","objective_preview":"fixture","objective_len":7}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::AgentStateChanged,
        wire_name: "agent_state_changed",
        projection_effect: ProjectionEffect::DisplayInvalidation,
        payload_schema: AgentStateChangedEvent::SCHEMA_ID,
        payload_schema_version: AgentStateChangedEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::AgentState,
        fixture_json: r#"{"agent_id":"default","status":"awake_idle","pending":0,"turn_index":0,"attached_workspace_ids":[],"worktree_active":false}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::SchedulerDiagnostic,
        wire_name: "scheduler_diagnostic",
        projection_effect: ProjectionEffect::None,
        payload_schema: SchedulerDiagnosticAuditEvent::SCHEMA_ID,
        payload_schema_version: SchedulerDiagnosticAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Scheduler,
        fixture_json: r#"{"agent_id":"default","decision":"StartModelTurn","reason":"message_admitted","boundary":"run_loop","scenario_class":"message_admission","work_item_id":null,"message_id":"msg_fixture","task_id":null,"evidence":["queue_len=1"]}"#,
    },
];

impl RuntimeEventKind {
    pub fn descriptor(self) -> &'static RuntimeEventDescriptor {
        REGISTRY
            .iter()
            .find(|entry| entry.kind == self)
            .expect("every RuntimeEventKind must have a registry descriptor")
    }

    pub fn from_wire_name(wire_name: &str) -> Option<Self> {
        REGISTRY
            .iter()
            .find(|entry| entry.wire_name == wire_name)
            .map(|entry| entry.kind)
    }
}

pub fn runtime_event_registry() -> &'static [RuntimeEventDescriptor] {
    REGISTRY
}

/// Classifies one stored audit event against the registry and legacy
/// compatibility contract.
pub fn classify_projection_effect(
    kind: &str,
    contract_version: u32,
    payload_schema: &str,
    payload_schema_version: u32,
) -> ProjectionEffectClassification {
    if is_legacy_event_shape(payload_schema, contract_version) {
        return ProjectionEffectClassification::ConservativeLegacy(
            ProjectionEffect::DisplayInvalidation,
        );
    }
    match RuntimeEventKind::from_wire_name(kind) {
        Some(known) => {
            let descriptor = known.descriptor();
            if descriptor.payload_schema != payload_schema {
                ProjectionEffectClassification::Unsupported(
                    UnsupportedProjectionEvent::PayloadSchemaMismatch,
                )
            } else if payload_schema_version > descriptor.payload_schema_version {
                ProjectionEffectClassification::Unsupported(
                    UnsupportedProjectionEvent::FuturePayloadSchemaVersion,
                )
            } else {
                ProjectionEffectClassification::Exact(descriptor.projection_effect)
            }
        }
        None => ProjectionEffectClassification::Unsupported(
            UnsupportedProjectionEvent::UnknownTypedKind,
        ),
    }
}

/// Returns the conservative wire effect for a stored event.
///
/// Capability verification guarantees this is called only for exact or
/// recognizable legacy envelopes. The fallback remains defensive so an
/// unsupported event can never silently pass as projection-neutral.
pub fn projection_effect_of(
    kind: &str,
    contract_version: u32,
    payload_schema: &str,
    payload_schema_version: u32,
) -> ProjectionEffect {
    match classify_projection_effect(
        kind,
        contract_version,
        payload_schema,
        payload_schema_version,
    ) {
        ProjectionEffectClassification::Exact(effect)
        | ProjectionEffectClassification::ConservativeLegacy(effect) => effect,
        ProjectionEffectClassification::Unsupported(_) => ProjectionEffect::DisplayInvalidation,
    }
}

/// Whether a stored event carries the legacy classification markers used by
/// the durable `event_projection_effect_complete` verification: a legacy
/// payload schema or the pre-registry contract version.
pub fn is_legacy_event_shape(payload_schema: &str, contract_version: u32) -> bool {
    payload_schema == LEGACY_PAYLOAD_SCHEMA
        || contract_version == LEGACY_RUNTIME_EVENT_CONTRACT_VERSION
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn registry_entries_have_unique_names_and_valid_fixtures() {
        let mut wire_names = HashSet::new();
        for entry in runtime_event_registry() {
            assert!(wire_names.insert(entry.wire_name));
            assert!(!entry.payload_schema.is_empty());
            assert!(entry.payload_schema_version > 0);
            assert!(
                serde_json::from_str::<serde_json::Value>(entry.fixture_json)
                    .expect("registry fixture must be valid JSON")
                    .is_object()
            );
            assert_eq!(
                RuntimeEventKind::from_wire_name(entry.wire_name),
                Some(entry.kind)
            );
            match entry.kind {
                RuntimeEventKind::MessageEnqueued | RuntimeEventKind::MessageProcessingStarted => {
                    serde_json::from_str::<MessageLifecycleAuditEvent>(entry.fixture_json)
                        .expect("message fixture must match its payload type");
                }
                RuntimeEventKind::BriefCreated => {
                    serde_json::from_str::<BriefCreatedAuditEvent>(entry.fixture_json)
                        .expect("brief fixture must match its payload type");
                }
                RuntimeEventKind::TaskCreated
                | RuntimeEventKind::TaskStatusUpdated
                | RuntimeEventKind::TaskResultReceived => {
                    serde_json::from_str::<TaskLifecycleAuditEvent>(entry.fixture_json)
                        .expect("task fixture must match its payload type");
                }
                RuntimeEventKind::WorkItemWritten => {
                    serde_json::from_str::<WorkItemLifecycleAuditEvent>(entry.fixture_json)
                        .expect("work item fixture must match its payload type");
                }
                RuntimeEventKind::AgentStateChanged => {
                    serde_json::from_str::<AgentStateChangedEvent>(entry.fixture_json)
                        .expect("agent-state fixture must match its payload type");
                }
                RuntimeEventKind::SchedulerDiagnostic => {
                    serde_json::from_str::<SchedulerDiagnosticAuditEvent>(entry.fixture_json)
                        .expect("scheduler diagnostic fixture must match its payload type");
                }
            }
        }
    }

    #[test]
    fn web_gui_supports_current_runtime_event_contract() {
        let session_events = include_str!("../web-gui/app/src/runtime/session-events.ts");
        let expected =
            format!("const RUNTIME_EVENT_CONTRACT_VERSION = {RUNTIME_EVENT_CONTRACT_VERSION};");

        assert!(
            session_events.contains(&expected),
            "Web GUI runtime event contract must match backend version {RUNTIME_EVENT_CONTRACT_VERSION}"
        );
    }

    #[test]
    fn typed_constructor_rejects_mismatched_payload_schema() {
        #[derive(Serialize)]
        struct WrongPayload;

        impl RuntimeEventPayload for WrongPayload {
            const SCHEMA_ID: &'static str = "holon.runtime_event.wrong";
        }

        let error =
            crate::types::AuditEvent::typed(RuntimeEventKind::MessageEnqueued, &WrongPayload)
                .unwrap_err();
        assert!(error.to_string().contains("requires payload schema"));
    }

    #[test]
    fn legacy_audit_events_double_read_missing_contract_metadata() {
        let event: crate::types::AuditEvent = serde_json::from_value(serde_json::json!({
            "id": "event_fixture",
            "event_seq": 7,
            "created_at": "2026-01-01T00:00:00Z",
            "kind": "future_event",
            "data": { "opaque": true }
        }))
        .unwrap();
        assert_eq!(
            event.contract_version,
            LEGACY_RUNTIME_EVENT_CONTRACT_VERSION
        );
        assert_eq!(event.payload_schema, LEGACY_PAYLOAD_SCHEMA);
        assert_eq!(event.payload_schema_version, 1);
        assert!(event.event_log_epoch.is_empty());
        assert_eq!(RuntimeEventKind::from_wire_name(&event.kind), None);
        assert_eq!(event.data["opaque"], true);
    }

    #[test]
    fn registry_inventory_covers_every_event_family_exactly_once() {
        // The inventory test gates `events.projection-effect.v1`: every
        // served event family must be classified exactly once before the
        // capability may be advertised.
        assert_eq!(
            runtime_event_registry().len(),
            ALL_RUNTIME_EVENT_KINDS.len(),
            "registry and RuntimeEventKind variant counts must match"
        );
        let mut seen_kinds = Vec::new();
        for entry in runtime_event_registry() {
            assert!(
                !seen_kinds.contains(&entry.kind),
                "registry must not duplicate kind {:?}",
                entry.kind
            );
            seen_kinds.push(entry.kind);
        }
        for kind in ALL_RUNTIME_EVENT_KINDS {
            assert!(
                seen_kinds.contains(kind),
                "kind {kind:?} has no registry descriptor"
            );
            assert_eq!(
                RuntimeEventKind::from_wire_name(kind.descriptor().wire_name),
                Some(*kind)
            );
        }
    }

    #[test]
    fn registry_fixtures_classify_to_their_declared_projection_effect() {
        for entry in runtime_event_registry() {
            assert_eq!(
                projection_effect_of(
                    entry.wire_name,
                    RUNTIME_EVENT_CONTRACT_VERSION,
                    entry.payload_schema,
                    entry.payload_schema_version
                ),
                entry.projection_effect,
                "fixture classification must match the declared effect for {}",
                entry.wire_name
            );
            // Older payload schema versions of a known schema still classify.
            assert_eq!(
                projection_effect_of(
                    entry.wire_name,
                    RUNTIME_EVENT_CONTRACT_VERSION,
                    entry.payload_schema,
                    1
                ),
                entry.projection_effect,
                "older schema versions of a known schema must stay classified for {}",
                entry.wire_name
            );
        }
    }

    #[test]
    fn projection_effect_falls_back_conservatively() {
        // Unknown wire name with a non-legacy schema shape.
        assert_eq!(
            projection_effect_of(
                "future_kind",
                RUNTIME_EVENT_CONTRACT_VERSION,
                "holon.runtime_event.future",
                1
            ),
            ProjectionEffect::DisplayInvalidation
        );
        // Known wire name with a mismatched payload schema.
        assert_eq!(
            projection_effect_of(
                "brief_created",
                RUNTIME_EVENT_CONTRACT_VERSION,
                "holon.runtime_event.some_other_schema",
                1
            ),
            ProjectionEffect::DisplayInvalidation
        );
        // Known schema identity but a payload schema version newer than the
        // registry's cannot be classified by this binary.
        let brief = RuntimeEventKind::BriefCreated.descriptor();
        assert_eq!(
            projection_effect_of(
                brief.wire_name,
                RUNTIME_EVENT_CONTRACT_VERSION,
                brief.payload_schema,
                brief.payload_schema_version + 1
            ),
            ProjectionEffect::DisplayInvalidation
        );
        // Legacy markers classify conservatively even for known wire names.
        assert_eq!(
            projection_effect_of(
                "brief_created",
                LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                LEGACY_PAYLOAD_SCHEMA,
                1
            ),
            ProjectionEffect::DisplayInvalidation
        );
        assert!(is_legacy_event_shape(LEGACY_PAYLOAD_SCHEMA, 2));
        assert!(is_legacy_event_shape("holon.runtime_event.other", 1));
        assert!(!is_legacy_event_shape("holon.runtime_event.other", 2));
    }

    #[test]
    fn projection_classification_distinguishes_exact_legacy_and_unsupported() {
        let scheduler = RuntimeEventKind::SchedulerDiagnostic.descriptor();
        assert_eq!(
            classify_projection_effect(
                scheduler.wire_name,
                RUNTIME_EVENT_CONTRACT_VERSION,
                scheduler.payload_schema,
                scheduler.payload_schema_version,
            ),
            ProjectionEffectClassification::Exact(ProjectionEffect::None)
        );

        for kind in [
            "agent_state_changed",
            "brief_created",
            "scheduler_diagnostic",
            "future_legacy_kind",
        ] {
            assert_eq!(
                classify_projection_effect(
                    kind,
                    LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                    LEGACY_PAYLOAD_SCHEMA,
                    1,
                ),
                ProjectionEffectClassification::ConservativeLegacy(
                    ProjectionEffect::DisplayInvalidation
                ),
                "{kind} must remain soundly classifiable as legacy"
            );
        }

        assert_eq!(
            classify_projection_effect(
                "brief_created",
                RUNTIME_EVENT_CONTRACT_VERSION,
                "holon.runtime_event.wrong",
                1,
            ),
            ProjectionEffectClassification::Unsupported(
                UnsupportedProjectionEvent::PayloadSchemaMismatch
            )
        );
        assert_eq!(
            classify_projection_effect(
                scheduler.wire_name,
                RUNTIME_EVENT_CONTRACT_VERSION,
                scheduler.payload_schema,
                scheduler.payload_schema_version + 1,
            ),
            ProjectionEffectClassification::Unsupported(
                UnsupportedProjectionEvent::FuturePayloadSchemaVersion
            )
        );
        assert_eq!(
            classify_projection_effect(
                "future_typed_kind",
                RUNTIME_EVENT_CONTRACT_VERSION,
                "holon.runtime_event.future",
                1,
            ),
            ProjectionEffectClassification::Unsupported(
                UnsupportedProjectionEvent::UnknownTypedKind
            )
        );
    }

    #[test]
    fn scheduler_diagnostics_are_projection_neutral() {
        // Self-contained diagnostics are outside AgentCanonicalProjection v1;
        // every other family references canonical display state.
        assert_eq!(
            RuntimeEventKind::SchedulerDiagnostic
                .descriptor()
                .projection_effect,
            ProjectionEffect::None
        );
        for entry in runtime_event_registry() {
            if entry.kind != RuntimeEventKind::SchedulerDiagnostic {
                assert_eq!(
                    entry.projection_effect,
                    ProjectionEffect::DisplayInvalidation
                );
            }
        }
    }
}
