pub use holon_domain::runtime_event::*;

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use crate::decision_telemetry::{DecisionAdvisoryCompletedEvent, DecisionOutcomeRecordedEvent};
    use crate::types::{
        AgentStateChangedEvent, BriefCreatedAuditEvent, MessageLifecycleAuditEvent,
        SchedulerDiagnosticAuditEvent, TaskLifecycleAuditEvent, WorkItemLifecycleAuditEvent,
    };
    use serde::Serialize;
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
                RuntimeEventKind::DecisionAdvisoryCompleted => {
                    serde_json::from_str::<DecisionAdvisoryCompletedEvent>(entry.fixture_json)
                        .expect("decision advisory fixture must match its payload type");
                }
                RuntimeEventKind::DecisionOutcomeRecorded => {
                    serde_json::from_str::<DecisionOutcomeRecordedEvent>(entry.fixture_json)
                        .expect("decision outcome fixture must match its payload type");
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
    fn registry_matches_the_public_typed_event_matrix() {
        let registry_names: HashSet<_> = runtime_event_registry()
            .iter()
            .map(|entry| entry.wire_name)
            .collect();
        let matrix_names: HashSet<_> = PUBLIC_TYPED_RUNTIME_EVENT_WIRE_NAMES
            .iter()
            .copied()
            .collect();

        assert_eq!(
            registry_names, matrix_names,
            "the server registry and public typed-event matrix must stay in sync"
        );
        assert_eq!(
            PUBLIC_TYPED_RUNTIME_EVENT_WIRE_NAMES.len(),
            registry_names.len(),
            "the public typed-event matrix must not contain duplicate wire names"
        );
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
        // Self-contained diagnostics and decision telemetry are outside
        // AgentCanonicalProjection v1; every other family references
        // canonical display state.
        let neutral_kinds = [
            RuntimeEventKind::SchedulerDiagnostic,
            RuntimeEventKind::DecisionAdvisoryCompleted,
            RuntimeEventKind::DecisionOutcomeRecorded,
        ];
        for entry in runtime_event_registry() {
            if neutral_kinds.contains(&entry.kind) {
                assert_eq!(entry.projection_effect, ProjectionEffect::None);
            } else {
                assert_eq!(
                    entry.projection_effect,
                    ProjectionEffect::DisplayInvalidation
                );
            }
        }
    }
}
