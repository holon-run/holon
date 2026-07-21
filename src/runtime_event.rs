use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{
    AgentStateChangedEvent, BriefCreatedAuditEvent, MessageLifecycleAuditEvent,
    SchedulerDiagnosticAuditEvent, TaskLifecycleAuditEvent, WorkItemLifecycleAuditEvent,
};

pub const RUNTIME_EVENT_CONTRACT_VERSION: u32 = 2;
pub const LEGACY_RUNTIME_EVENT_CONTRACT_VERSION: u32 = 1;
pub const LEGACY_PAYLOAD_SCHEMA: &str = "holon.runtime_event.legacy";

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
        payload_schema: MessageLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: MessageLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Message,
        fixture_json: r#"{"message_id":"msg_fixture","agent_id":"default","kind":"operator_prompt","origin":{"kind":"system","subsystem":"fixture"},"authority_class":"runtime_instruction","priority":"normal","source_refs":{}}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::MessageProcessingStarted,
        wire_name: "message_processing_started",
        payload_schema: MessageLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: MessageLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Message,
        fixture_json: r#"{"message_id":"msg_fixture","agent_id":"default","kind":"operator_prompt","origin":{"kind":"system","subsystem":"fixture"},"authority_class":"runtime_instruction","priority":"normal","source_refs":{}}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::BriefCreated,
        wire_name: "brief_created",
        payload_schema: BriefCreatedAuditEvent::SCHEMA_ID,
        payload_schema_version: BriefCreatedAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Brief,
        fixture_json: r#"{"brief_id":"brief_fixture","agent_id":"default","workspace_id":"agent_home","kind":"result","created_at":"2026-01-01T00:00:00Z","content_source":{"kind":"inline"},"content_char_count":0}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::TaskCreated,
        wire_name: "task_created",
        payload_schema: TaskLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: TaskLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Task,
        fixture_json: r#"{"task_id":"task_fixture","agent_id":"default","kind":"command_task","status":"queued","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::TaskStatusUpdated,
        wire_name: "task_status_updated",
        payload_schema: TaskLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: TaskLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Task,
        fixture_json: r#"{"task_id":"task_fixture","agent_id":"default","kind":"command_task","status":"running","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::TaskResultReceived,
        wire_name: "task_result_received",
        payload_schema: TaskLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: TaskLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Task,
        fixture_json: r#"{"task_id":"task_fixture","agent_id":"default","kind":"command_task","status":"completed","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::WorkItemWritten,
        wire_name: "work_item_written",
        payload_schema: WorkItemLifecycleAuditEvent::SCHEMA_ID,
        payload_schema_version: WorkItemLifecycleAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::WorkItem,
        fixture_json: r#"{"agent_id":"default","work_item_id":"work_fixture","workspace_id":"agent_home","revision":1,"action":"created","state":"open","plan_status":"draft","readiness":"runnable","updated_at":"2026-01-01T00:00:00Z","objective_preview":"fixture","objective_len":7}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::AgentStateChanged,
        wire_name: "agent_state_changed",
        payload_schema: AgentStateChangedEvent::SCHEMA_ID,
        payload_schema_version: AgentStateChangedEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::AgentState,
        fixture_json: r#"{"agent_id":"default","status":"awake_idle","pending":0,"turn_index":0,"attached_workspace_ids":[],"worktree_active":false}"#,
    },
    RuntimeEventDescriptor {
        kind: RuntimeEventKind::SchedulerDiagnostic,
        wire_name: "scheduler_diagnostic",
        payload_schema: SchedulerDiagnosticAuditEvent::SCHEMA_ID,
        payload_schema_version: SchedulerDiagnosticAuditEvent::SCHEMA_VERSION,
        display_family: RuntimeEventDisplayFamily::Scheduler,
        fixture_json: r#"{"agent_id":"default","decision":"StartModelTurn","reason":"message_admitted","boundary":"run_loop","scenario_class":"message_admission","shadow_matched":true,"divergence_code":null,"work_item_id":null,"message_id":"msg_fixture","task_id":null,"evidence":["queue_len=1"]}"#,
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
}
