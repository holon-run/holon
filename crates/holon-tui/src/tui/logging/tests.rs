use super::*;
use crate::operator_event::{OperatorEventCategory, OperatorEventPresentation, OperatorVisibility};
use crate::presentation::Outcome;
use crate::tui::projection::{ProjectionEventLane, ProjectionEventRecord};
use serde_json::json;

#[test]
fn new_uses_log_root_not_agent_root() {
    let tempdir = tempfile::tempdir().unwrap();
    let log_root = tempdir.path().join("logs");

    let writer = TuiLogWriter::new(&log_root).unwrap();

    assert_eq!(writer.root, log_root.join("tui"));
    assert!(writer.root.exists());
    assert!(
        !tempdir
            .path()
            .join("agents")
            .join("logs")
            .join("tui")
            .exists(),
        "TUI diagnostics must not create a pseudo agent under agents/logs"
    );
}

#[test]
fn presentation_log_is_disabled_by_default() {
    let writer = TuiLogWriter::new_temp().unwrap();
    let event = ProjectionEventRecord {
        id: "evt_1".into(),
        event_seq: 7,
        ts: Utc::now(),
        kind: "brief_created".into(),
        lane: ProjectionEventLane::Timeline,
        summary: "completed work".into(),
        presentation: OperatorEventPresentation {
            visibility: OperatorVisibility::TurnResult,
            category: OperatorEventCategory::Brief,
            title: "Holon".into(),
            body: Some("completed work".into()),
            summary: "completed work".into(),
            source_event_kind: "brief_created".into(),
        },
        payload: json!({ "id": "brief_1", "text": "completed work" }),
    };
    let item = TimedItem::with_key(
        PresentationItem::AssistantResult {
            brief_id: Some("brief_1".into()),
            body: "completed work".into(),
            outcome: Outcome::Success,
        },
        event.ts,
        "test-item",
    );

    writer
        .write_event(&event)
        .and_then(|_| writer.write_presentation_items(std::slice::from_ref(&event), &[item]))
        .unwrap();

    assert!(
        !writer.root.join("presentation.jsonl").exists(),
        "presentation logging is debug instrumentation and must default off"
    );
}

#[test]
fn presentation_log_records_display_decisions_without_raw_conversation_log_when_enabled() {
    let writer = TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();
    let event = ProjectionEventRecord {
        id: "evt_1".into(),
        event_seq: 7,
        ts: Utc::now(),
        kind: "brief_created".into(),
        lane: ProjectionEventLane::Timeline,
        summary: "completed work".into(),
        presentation: OperatorEventPresentation {
            visibility: OperatorVisibility::TurnResult,
            category: OperatorEventCategory::Brief,
            title: "Holon".into(),
            body: Some("completed work".into()),
            summary: "completed work".into(),
            source_event_kind: "brief_created".into(),
        },
        payload: json!({ "id": "brief_1", "text": "completed work" }),
    };
    let item = TimedItem::with_key(
        PresentationItem::AssistantResult {
            brief_id: Some("brief_1".into()),
            body: "completed work".into(),
            outcome: Outcome::Success,
        },
        event.ts,
        "test-item",
    );

    writer
        .write_event(&event)
        .and_then(|_| writer.write_presentation_items(std::slice::from_ref(&event), &[item]))
        .unwrap();

    let presentation_path = writer.root.join("presentation.jsonl");
    let line = fs::read_to_string(&presentation_path).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["item_kind"], "assistant_result");
    assert_eq!(record["min_display_level"], 3);
    assert_eq!(record["reducer_event_ids"], json!(["evt_1"]));
    assert_eq!(record["displays"][0]["display_level"], 3);
    assert_eq!(record["displays"][0]["decision"], "shown");
    assert_eq!(record["displays"][0]["cells"][0]["speaker"], "Holon");
    assert_eq!(
        record["displays"][0]["cells"][0]["body_preview"],
        "✓ completed work"
    );
    assert!(
        !writer.root.join("conversation.jsonl").exists(),
        "TUI should not duplicate raw conversation events"
    );
}

#[test]
fn presentation_log_marks_lower_display_levels_hidden() {
    let writer = TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();
    let event = ProjectionEventRecord {
        id: "evt_debug".into(),
        event_seq: 9,
        ts: Utc::now(),
        kind: "provider_round_completed".into(),
        lane: ProjectionEventLane::Debug,
        summary: "provider completed".into(),
        presentation: OperatorEventPresentation {
            visibility: OperatorVisibility::Trace,
            category: OperatorEventCategory::Trace,
            title: "Provider".into(),
            body: None,
            summary: "provider completed".into(),
            source_event_kind: "provider_round_completed".into(),
        },
        payload: json!({ "model": "test-model" }),
    };
    let item = TimedItem::with_key(
        PresentationItem::GenericEvent {
            kind: "provider_round_completed".into(),
            summary: "provider completed".into(),
        },
        event.ts,
        "test-item",
    );

    writer
        .write_presentation_items(std::slice::from_ref(&event), &[item])
        .unwrap();

    let line = fs::read_to_string(writer.root.join("presentation.jsonl")).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["min_display_level"], 5);
    assert_eq!(record["displays"][0]["decision"], "hidden");
    assert_eq!(record["displays"][1]["decision"], "hidden");
    assert_eq!(record["displays"][2]["decision"], "shown");
}

#[test]
fn presentation_log_rotates_when_enabled_debug_log_reaches_limit() {
    let writer = TuiLogWriter::new_temp_with_presentation_logging(1800).unwrap();
    let event = ProjectionEventRecord {
        id: "evt_debug".into(),
        event_seq: 9,
        ts: Utc::now(),
        kind: "provider_round_completed".into(),
        lane: ProjectionEventLane::Debug,
        summary: "provider completed".into(),
        presentation: OperatorEventPresentation {
            visibility: OperatorVisibility::Trace,
            category: OperatorEventCategory::Trace,
            title: "Provider".into(),
            body: None,
            summary: "provider completed".into(),
            source_event_kind: "provider_round_completed".into(),
        },
        payload: json!({ "model": "test-model" }),
    };
    let item = TimedItem::with_key(
        PresentationItem::GenericEvent {
            kind: "provider_round_completed".into(),
            summary: "provider completed".into(),
        },
        event.ts,
        "test-item",
    );

    for _ in 0..8 {
        writer
            .write_presentation_items(std::slice::from_ref(&event), std::slice::from_ref(&item))
            .unwrap();
    }

    let path = writer.root.join("presentation.jsonl");
    let rotated = writer.root.join("presentation.jsonl.1");
    assert!(path.exists());
    assert!(rotated.exists());
    assert!(path.metadata().unwrap().len() <= writer.presentation_log_max_bytes);
    assert!(rotated.metadata().unwrap().len() <= writer.presentation_log_max_bytes);
}

#[test]
fn presentation_log_drops_single_records_larger_than_limit() {
    let writer = TuiLogWriter::new_temp_with_presentation_logging(128).unwrap();
    let event = ProjectionEventRecord {
        id: "evt_large".into(),
        event_seq: 9,
        ts: Utc::now(),
        kind: "provider_round_completed".into(),
        lane: ProjectionEventLane::Debug,
        summary: "provider completed".repeat(100),
        presentation: OperatorEventPresentation {
            visibility: OperatorVisibility::Trace,
            category: OperatorEventCategory::Trace,
            title: "Provider".into(),
            body: None,
            summary: "provider completed".repeat(100),
            source_event_kind: "provider_round_completed".into(),
        },
        payload: json!({ "model": "test-model" }),
    };
    let item = TimedItem::with_key(
        PresentationItem::GenericEvent {
            kind: "provider_round_completed".into(),
            summary: "provider completed".repeat(100),
        },
        event.ts,
        "test-item",
    );

    writer
        .write_presentation_items(std::slice::from_ref(&event), std::slice::from_ref(&item))
        .unwrap();

    assert!(!writer.root.join("presentation.jsonl").exists());
}

#[test]
fn turn_terminal_records_are_routed_to_turns_not_errors() {
    let writer = TuiLogWriter::new_temp().unwrap();
    let event = ProjectionEventRecord {
        id: "evt_terminal".into(),
        event_seq: 10,
        ts: Utc::now(),
        kind: "turn_terminal".into(),
        lane: ProjectionEventLane::Debug,
        summary: "turn completed".into(),
        presentation: OperatorEventPresentation {
            visibility: OperatorVisibility::Trace,
            category: OperatorEventCategory::Trace,
            title: "Turn".into(),
            body: None,
            summary: "turn completed".into(),
            source_event_kind: "turn_terminal".into(),
        },
        payload: json!({ "kind": "completed" }),
    };

    writer.write_event(&event).unwrap();

    assert!(!writer.root.join("errors.jsonl").exists());
    let line = fs::read_to_string(writer.root.join("turns.jsonl")).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["kind"], "turn_terminal");
}

#[test]
fn runtime_errors_are_routed_to_errors_not_turns() {
    let writer = TuiLogWriter::new_temp().unwrap();
    let event = ProjectionEventRecord {
        id: "evt_error".into(),
        event_seq: 11,
        ts: Utc::now(),
        kind: "runtime_error".into(),
        lane: ProjectionEventLane::Debug,
        summary: "runtime failed".into(),
        presentation: OperatorEventPresentation {
            visibility: OperatorVisibility::Trace,
            category: OperatorEventCategory::Trace,
            title: "Runtime".into(),
            body: None,
            summary: "runtime failed".into(),
            source_event_kind: "runtime_error".into(),
        },
        payload: json!({ "error": "failed" }),
    };

    writer.write_event(&event).unwrap();

    assert!(!writer.root.join("turns.jsonl").exists());
    let line = fs::read_to_string(writer.root.join("errors.jsonl")).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["kind"], "runtime_error");
}
