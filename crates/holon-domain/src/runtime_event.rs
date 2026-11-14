use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::decision_telemetry::{DecisionAdvisoryCompletedEvent, DecisionOutcomeRecordedEvent};
use crate::{
    AgentStateChangedEvent, BriefCreatedAuditEvent, MessageLifecycleAuditEvent,
    SchedulerDiagnosticAuditEvent, TaskLifecycleAuditEvent, WorkItemLifecycleAuditEvent,
};

/// Envelope contract version emitted while `events.projection-effect.v1` is
/// advertised. Version 3 adds the additive `projection_effect` field.
pub const RUNTIME_EVENT_CONTRACT_VERSION: u32 = 3;
pub const LEGACY_RUNTIME_EVENT_CONTRACT_VERSION: u32 = 1;
pub const LEGACY_PAYLOAD_SCHEMA: &str = "holon.runtime_event.legacy";

/// Response header that declares the envelope contract version of an event
/// stream. The version is declared once per stream; individual envelopes no
/// longer repeat it.
pub const EVENT_CONTRACT_VERSION_HEADER: &str = "x-holon-event-contract-version";

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
    RuntimeEventKind::DecisionAdvisoryCompleted,
    RuntimeEventKind::DecisionOutcomeRecorded,
];

/// Wire names in the public typed-event matrix. Events outside this set stay
/// on the opaque legacy boundary until they have a stable payload contract.
pub const PUBLIC_TYPED_RUNTIME_EVENT_WIRE_NAMES: &[&str] = &[
    "message_enqueued",
    "message_processing_started",
    "brief_created",
    "task_created",
    "task_status_updated",
    "task_result_received",
    "work_item_written",
    "agent_state_changed",
    "scheduler_diagnostic",
    "decision_advisory_completed",
    "decision_outcome_recorded",
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
    DecisionAdvisoryCompleted,
    DecisionOutcomeRecorded,
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

impl RuntimeEventPayload for DecisionAdvisoryCompletedEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.decision_advisory_completed";
}

impl RuntimeEventPayload for DecisionOutcomeRecordedEvent {
    const SCHEMA_ID: &'static str = "holon.runtime_event.decision_outcome_recorded";
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
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::DecisionAdvisoryCompleted,
        wire_name: "decision_advisory_completed",
        projection_effect: ProjectionEffect::None,
        payload_schema: DecisionAdvisoryCompletedEvent::SCHEMA_ID,
        payload_schema_version: DecisionAdvisoryCompletedEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Scheduler,
        fixture_json: r#"{"decision_id":"decision_fixture","agent_id":"default","request_fingerprint":"fixture","provider":"fixture","model":"fixture","outcome":"select","choice":"yes","abstain":false,"fallback":false,"timeout":false,"evidence":[],"recorded_at":"2026-01-01T00:00:00Z"}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::DecisionOutcomeRecorded,
        wire_name: "decision_outcome_recorded",
        projection_effect: ProjectionEffect::None,
        payload_schema: DecisionOutcomeRecordedEvent::SCHEMA_ID,
        payload_schema_version: DecisionOutcomeRecordedEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Scheduler,
        fixture_json: r#"{"decision_id":"decision_fixture","agent_id":"default","task_id":"task_fixture","task_status":"completed","recorded_at":"2026-01-01T00:00:00Z"}"#,
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
