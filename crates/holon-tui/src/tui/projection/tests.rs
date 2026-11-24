use super::{
    summarize_event, OperatorDisplayMode, OperatorVisibility, ProjectionEventLane, ProjectionSlice,
    TuiProjection, EVENT_LOG_LIMIT, TASK_TAIL_LIMIT,
};
use crate::{
    client::{
        AgentStateSnapshot, AgentStreamEvent, StateSessionSnapshot, StateWorkspaceSnapshot,
        StreamEventEnvelope,
    },
    operator_event::{present_operator_event, OperatorPresentationContext},
    system::{ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
        AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState,
        AgentStateChangedEvent, AgentSummary, AgentTokenUsageSummary, AgentVisibility,
        BriefCreatedAuditEvent, BriefKind, BriefRecord, CallbackDeliveryMode, ChildAgentSummary,
        ClosureDecision, ClosureOutcome, ExternalTriggerScope, ExternalTriggerStateSnapshot,
        ExternalTriggerStatus, LoadedAgentsMdView, MessageBody, MessageDeliverySurface,
        MessageEnvelope, MessageKind, MessageOrigin, Priority, RuntimePosture, SkillsRuntimeView,
        TaskLifecycleAuditEvent, TaskRecord, TaskStatus, TimerRecord, TimerStatus, TodoItem,
        TodoItemState, TokenUsage, TurnTerminalKind, TurnTerminalRecord, WaitingReason,
        WorkItemRecord, WorkItemState,
    },
};
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_SAMPLE_EVENT_SEQ: AtomicU64 = AtomicU64::new(1_000_000);

#[test]
fn raw_diagnostics_never_overwrite_authoritative_state() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let pending = projection.agent.agent.pending;
    let mut old_state = projection.agent.agent.clone();
    old_state.pending = pending + 10;
    projection.append_diagnostic_event(
        sample_event(
            "agent_state_changed",
            serde_json::to_value(old_state).unwrap(),
        ),
        &test_log_writer(),
    );
    assert_eq!(projection.agent.agent.pending, pending);
    assert!(projection.stale_slices.is_empty());
    assert_eq!(projection.event_log().len(), 1);
}

#[test]
fn diagnostic_tail_is_bounded_and_survives_state_replacement_only() {
    let writer = test_log_writer();
    let mut previous = TuiProjection::from_snapshot(sample_snapshot());
    for index in 0..=EVENT_LOG_LIMIT {
        previous.append_diagnostic_event(
            sample_event_with_id(&format!("diagnostic-{index}"), "runtime_error", json!({})),
            &writer,
        );
    }
    let mut refreshed = TuiProjection::from_snapshot(sample_snapshot());
    refreshed.take_diagnostics_from(&mut previous);
    assert_eq!(refreshed.event_log().len(), EVENT_LOG_LIMIT);
    assert!(previous.event_log().is_empty());
    assert_eq!(refreshed.event_log()[0].id, "diagnostic-1");
    assert!(refreshed.durable_conversation_log.is_empty());
    assert!(refreshed.stale_slices.is_empty());
}

fn test_log_writer() -> crate::tui::logging::TuiLogWriter {
    crate::tui::logging::TuiLogWriter::new_temp().unwrap()
}

fn route_ref(value: &str) -> crate::config::ModelRouteRef {
    crate::config::ModelRouteRef::parse_compatible(value).unwrap()
}

#[test]
fn projection_bootstraps_from_snapshot() {
    let snapshot = sample_snapshot();
    let projection = TuiProjection::from_snapshot(snapshot.clone());

    assert_eq!(projection.agent.identity.agent_id, "default");
    assert_eq!(
        projection.external_triggers.len(),
        snapshot.external_triggers.len()
    );
    assert!(projection.stale_slices.is_empty());
    assert!(projection.cursor.is_none());
}

#[test]
fn projection_bootstrap_uses_external_triggers_without_legacy_fallback() {
    let mut snapshot = sample_snapshot();
    snapshot.external_triggers.clear();
    let projection = TuiProjection::from_snapshot(snapshot.clone());

    assert_eq!(
        projection.external_triggers.len(),
        snapshot.external_triggers.len()
    );
}

#[test]
fn projection_bootstrap_seeds_raw_event_log_from_snapshot_tail() {
    let snapshot = sample_snapshot();
    let events_tail = vec![StreamEventEnvelope {
        projection_effect: None,
        event_log_epoch: Some("epoch-test".into()),
        payload_schema: None,
        payload_schema_version: None,
        id: "evt-tail-1".into(),
        event_seq: 0,
        ts: Utc::now(),
        agent_id: "default".into(),
        event_type: "assistant_round_recorded".into(),
        payload: json!({
            "stop_reason": "tool_use",
            "tool_names": ["ExecCommand"],
            "tool_call_count": 1,
            "has_tool_calls": true,
            "raw_text": "debug assistant body"
        }),
    }];
    let mut projection = TuiProjection::from_snapshot(snapshot);
    projection.replace_event_window(events_tail, Some(0));

    assert_eq!(projection.event_log().len(), 1);
    assert_eq!(
        projection
            .event_log()
            .last()
            .map(|event| event.summary.as_str()),
        Some("Assistant requested tools: ExecCommand")
    );
    assert_eq!(projection.cursor, Some(0));
    assert!(projection.durable_conversation_events().next().is_none());
}

#[test]
fn stream_event_writes_enabled_presentation_debug_log_incrementally() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();

    projection.apply_event(
        sample_event(
            "assistant_round_recorded",
            json!({ "round": 1, "text_preview": "streamed assistant progress" }),
        ),
        &writer,
    );

    let line = std::fs::read_to_string(writer.root().join("presentation.jsonl")).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["item_kind"], "assistant_progress");
    assert_eq!(
        record["reducer_event_ids"],
        json!(["evt-assistant_round_recorded"])
    );
}

#[test]
fn presentation_debug_log_records_command_result_without_start_window() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();

    projection.apply_event(
        sample_event(
            "process_execution_requested",
            json!({ "exec_command_cmd": "cargo test" }),
        ),
        &writer,
    );
    projection.apply_event(
        sample_event(
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo test",
                "duration_ms": 12,
                "exit_status": 0,
                "stdout_preview": "ok"
            }),
        ),
        &writer,
    );

    let log = std::fs::read_to_string(writer.root().join("presentation.jsonl")).unwrap();
    let record: Value = log
        .lines()
        .last()
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap();
    assert_eq!(record["item_kind"], "command_executed");
    assert_eq!(record["reducer_event_ids"], json!(["evt-tool_executed"]));
}

#[test]
fn presentation_debug_log_does_not_pair_non_exec_tool_with_command_window() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let writer =
        crate::tui::logging::TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();

    projection.apply_event(
        sample_event(
            "process_execution_requested",
            json!({ "exec_command_cmd": "agentinbox inbox ack --agent-id holon-pm" }),
        ),
        &writer,
    );
    projection.apply_event(
        sample_event(
            "tool_executed",
            json!({
                "tool_name": "TaskStatus",
                "summary": "inspected task status"
            }),
        ),
        &writer,
    );

    let log = std::fs::read_to_string(writer.root().join("presentation.jsonl")).unwrap();
    let records = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        records.last().unwrap()["reducer_event_ids"],
        json!(["evt-tool_executed"])
    );
    assert_ne!(records.last().unwrap()["item_kind"], "command_executed");
}

#[test]
fn stream_event_does_not_write_presentation_debug_log_by_default() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let writer = crate::tui::logging::TuiLogWriter::new_temp().unwrap();

    projection.apply_event(
        sample_event(
            "assistant_round_recorded",
            json!({ "round": 1, "text_preview": "streamed assistant progress" }),
        ),
        &writer,
    );

    assert!(!writer.root().join("presentation.jsonl").exists());
}

#[test]
fn projection_resets_event_history_when_epoch_changes() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let mut old_event = sample_event_envelope("evt-old-epoch", 7);
    old_event.event_log_epoch = Some("epoch-old".into());
    projection.replace_event_window(vec![old_event], Some(7));

    let mut new_event = sample_event_envelope("evt-new-epoch", 1);
    new_event.event_log_epoch = Some("epoch-new".into());
    projection.merge_event_tail(vec![new_event], Some(1));

    assert_eq!(projection.event_log_epoch.as_deref(), Some("epoch-new"));
    assert_eq!(projection.cursor, Some(1));
    assert_eq!(
        projection
            .event_log()
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        vec!["evt-new-epoch"]
    );
}

#[test]
fn projection_rejects_conflicting_content_for_same_event_identity() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let old_event = sample_event_envelope_with_payload(
        "evt-old",
        7,
        "tool_executed",
        json!({ "tool_name": "ExecCommand", "summary": "old" }),
    );
    projection.replace_event_window(vec![old_event], Some(7));

    let conflicting = sample_event_envelope_with_payload(
        "evt-new",
        7,
        "tool_executed",
        json!({ "tool_name": "ExecCommand", "summary": "new" }),
    );
    projection.merge_event_tail(vec![conflicting], Some(7));

    assert_eq!(projection.cursor, None);
    assert!(projection.event_log().is_empty());
    assert!(projection.stale_slices.contains(&ProjectionSlice::Agent));
    assert!(projection.stale_slices.contains(&ProjectionSlice::Tasks));
    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::Workspace));
}

#[test]
fn projection_preserves_unknown_schema_without_applying_domain_transition() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let original_pending = projection.session.pending_count;
    let mut event = sample_event(
        "agent_state_changed",
        json!({
            "agent_id": "default",
            "status": "awake_running",
            "pending": original_pending + 10,
            "turn_index": 99,
            "attached_workspace_ids": [],
            "worktree_active": false
        }),
    );
    event.data.payload_schema = Some("holon.runtime_event.future_agent_state".into());

    projection.apply_event(event, &test_log_writer());

    assert_eq!(projection.session.pending_count, original_pending);
    assert_eq!(projection.event_log().len(), 1);
}

#[test]
fn projection_merges_tail_refresh_truncates_after_chronological_sort() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let events_tail = (0..EVENT_LOG_LIMIT)
        .map(|index| sample_event_envelope(&format!("evt-existing-{index}"), (index + 100) as u64))
        .collect::<Vec<_>>();
    projection.replace_event_window(events_tail, Some((EVENT_LOG_LIMIT + 99) as u64));

    projection.merge_event_tail(vec![sample_event_envelope("evt-old-refresh", 1)], None);

    let ids = projection
        .event_log()
        .iter()
        .map(|event| event.id.as_str())
        .collect::<Vec<_>>();
    let newest_id = format!("evt-existing-{}", EVENT_LOG_LIMIT - 1);
    assert_eq!(projection.event_log().len(), EVENT_LOG_LIMIT);
    assert!(!ids.contains(&"evt-old-refresh"));
    assert!(ids.contains(&"evt-existing-0"));
    assert!(ids.iter().any(|id| *id == newest_id.as_str()));
}

#[test]
fn projection_refresh_keeps_latest_live_event_at_log_cap() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let events_tail = (1..=EVENT_LOG_LIMIT as u64)
        .map(|seq| sample_event_envelope(&format!("evt-existing-{seq}"), seq))
        .collect::<Vec<_>>();
    projection.replace_event_window(events_tail, Some(EVENT_LOG_LIMIT as u64));

    projection.apply_event(
        AgentStreamEvent {
            contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
            id: "evt-live".into(),
            event: "assistant_round_recorded".into(),
            data: sample_event_envelope("evt-live", EVENT_LOG_LIMIT as u64 + 1),
        },
        &test_log_writer(),
    );
    projection.merge_event_tail(vec![sample_event_envelope("evt-existing-1", 1)], None);

    let ids = projection
        .event_log()
        .iter()
        .map(|event| event.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(projection.event_log().len(), EVENT_LOG_LIMIT);
    assert!(ids.contains(&"evt-live"));
    assert!(!ids.contains(&"evt-old-refresh"));
    assert_eq!(projection.cursor, Some(EVENT_LOG_LIMIT as u64 + 1));
}

#[test]
fn projection_bootstrap_preserves_agent_identity() {
    let projection = TuiProjection::from_snapshot(sample_snapshot());

    assert_eq!(projection.agent.identity.agent_id, "default");
    assert!(projection.agent.identity.is_default_agent);
}

#[test]
fn projection_applies_brief_and_message_events() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let brief = BriefRecord::new("default", BriefKind::Result, "streamed brief", None, None);
    let message = sample_message();

    projection.apply_event(
        sample_event("brief_created", serde_json::to_value(&brief).unwrap()),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event("message_enqueued", serde_json::to_value(&message).unwrap()),
        &test_log_writer(),
    );

    assert!(projection
        .event_log()
        .iter()
        .any(|event| event.kind == "brief_created"));
    assert!(projection
        .durable_operator_message_ids()
        .contains(&message.id));
    let lanes = projection
        .timeline_events()
        .map(|event| event.lane)
        .collect::<Vec<_>>();
    assert_eq!(
        lanes,
        vec![ProjectionEventLane::Timeline, ProjectionEventLane::Timeline]
    );
}

#[test]
fn projection_hydrates_inline_audit_brief_text_by_brief_id() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "resolved inline projection body",
        None,
        None,
    );
    let event_payload = serde_json::to_value(BriefCreatedAuditEvent::from_brief(&brief)).unwrap();
    let event = sample_event("brief_created", event_payload);
    projection.apply_event(event, &test_log_writer());

    assert_eq!(
        projection.missing_inline_brief_ids_for_hydration(),
        vec![brief.id.clone()]
    );
    assert!(projection
        .brief_text_for_event(projection.event_log().last().unwrap())
        .is_none());

    assert!(projection.hydrate_brief_records(vec![brief.clone()]));
    assert_eq!(
        projection.missing_inline_brief_ids_for_hydration(),
        Vec::<String>::new()
    );
    assert_eq!(
        projection.brief_text_for_event(projection.event_log().last().unwrap()),
        Some("resolved inline projection body")
    );
}

#[test]
fn projection_logs_provider_rounds_as_progress_activity() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event(
            "provider_round_completed",
            json!({ "round": 1, "stop_reason": "end_turn" }),
        ),
        &test_log_writer(),
    );

    let activity = projection.recent_activity_events();
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].kind, "provider_round_completed");
    assert_eq!(
        projection.event_log().last().map(|event| event.lane),
        Some(ProjectionEventLane::Debug)
    );
    assert_eq!(
        projection
            .event_log()
            .last()
            .map(|event| event.summary.as_str()),
        Some("Provider round 1: model=model; stop=end_turn; tokens unavailable; tools=0")
    );
}

#[test]
fn hidden_current_turn_events_stop_at_current_run_boundary() {
    let mut snapshot = sample_snapshot();
    snapshot.agent.agent.status = crate::types::AgentStatus::AwakeRunning;
    snapshot.agent.agent.turn_index = 2;
    snapshot.agent.agent.current_run_id = Some("run-2".into());
    let mut projection = TuiProjection::from_snapshot(snapshot);

    projection.apply_event(
        sample_event_with_id(
            "evt-prev-assistant-round",
            "assistant_round_recorded",
            json!({
                "run_id": "run-1",
                "turn_index": 1,
                "round": 1,
                "text_preview": "previous turn"
            }),
        ),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event(
            "turn_started",
            json!({
                "run_id": "run-2",
                "turn_index": 2,
                "message_id": "msg-2"
            }),
        ),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event_with_id(
            "evt-current-assistant-round",
            "assistant_round_recorded",
            json!({
                "run_id": "run-2",
                "turn_index": 2,
                "round": 1,
                "text_preview": "current turn"
            }),
        ),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event(
            "tool_executed",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo test"
            }),
        ),
        &test_log_writer(),
    );

    let events = projection.hidden_current_turn_events(OperatorDisplayMode::Info);
    assert_eq!(
        events
            .iter()
            .map(|event| event.summary.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Assistant round: current turn",
            "Command finished: cargo test"
        ]
    );
}

#[test]
fn brief_visibility_marks_operator_wait_and_completed_work() {
    let mut snapshot = sample_snapshot();
    snapshot.agent.closure.waiting_reason = Some(WaitingReason::AwaitingOperatorInput);
    let mut projection = TuiProjection::from_snapshot(snapshot);
    let wait_brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "Need operator input",
        None,
        None,
    );
    assert_eq!(
        {
            projection.apply_event(
                sample_event("brief_created", serde_json::to_value(&wait_brief).unwrap()),
                &test_log_writer(),
            );
            projection
                .event_log()
                .last()
                .map(|event| projection.operator_visibility(event))
        },
        Some(OperatorVisibility::ActionRequired)
    );

    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let mut completed = WorkItemRecord::new("default", "finish docs", WorkItemState::Completed);
    completed.id = "work-done".into();
    projection.work_items = vec![super::super::tests::sample_work_item_projection(
        completed, false,
    )];
    let mut work_brief = BriefRecord::new("default", BriefKind::Result, "Work done", None, None);
    work_brief.work_item_id = Some("work-done".into());
    projection.apply_event(
        sample_event("brief_created", serde_json::to_value(&work_brief).unwrap()),
        &test_log_writer(),
    );
    assert_eq!(
        projection
            .event_log()
            .last()
            .map(|event| projection.operator_visibility(event)),
        Some(OperatorVisibility::WorkDone)
    );
}

#[test]
fn projection_event_record_helper_uses_default_context() {
    let wait_brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "Need operator input",
        None,
        None,
    );
    let wait_event =
        projection_event_record("brief_created", serde_json::to_value(&wait_brief).unwrap());
    assert_eq!(
        wait_event.presentation.visibility,
        OperatorVisibility::TurnResult
    );
}

#[test]
fn assistant_round_operator_projection_renders_progress_fields() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event_with_id(
            "evt-assistant-tools",
            "assistant_round_recorded",
            json!({
                "redacted": true,
                "stop_reason": "tool_use",
                "text_preview": null,
                "tool_names": ["ExecCommand", "ReadFile"],
                "tool_call_count": 2,
                "has_tool_calls": true
            }),
        ),
        &test_log_writer(),
    );
    assert_eq!(
        projection
            .event_log()
            .last()
            .map(|event| event.summary.as_str()),
        Some("Assistant requested tools: ExecCommand, ReadFile")
    );

    projection.apply_event(
        sample_event_with_id(
            "evt-assistant-empty",
            "assistant_round_recorded",
            json!({
                "redacted": true,
                "stop_reason": "end_turn",
                "text_preview": null,
                "tool_names": [],
                "tool_call_count": 0,
                "has_text": false,
                "has_tool_calls": false
            }),
        ),
        &test_log_writer(),
    );
    assert_eq!(
        projection
            .event_log()
            .last()
            .map(|event| event.summary.as_str()),
        Some("Assistant round completed without text (stop=end_turn)")
    );
}

#[test]
fn transient_activity_events_do_not_force_state_refresh() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    for (kind, payload) in [
        (
            "provider_round_completed",
            json!({ "round": 1, "text_preview": "working" }),
        ),
        (
            "text_only_round_observed",
            json!({ "round": 2, "text_preview": "still working" }),
        ),
        (
            "max_output_tokens_recovery",
            json!({ "agent_id": "default", "attempt": 1 }),
        ),
        (
            "runtime_error",
            json!({
                "message_id": "message-1",
                "message_kind": "operator_prompt",
                "error": "visible error",
                "token_usage": null
            }),
        ),
        (
            "deferred_to_fallback",
            json!({
                "operator_message": "OpenAI Codex authentication failed. Queued fallback turn on anthropic/claude-sonnet-4-6.",
                "fallback_model_ref": "anthropic/claude-sonnet-4-6"
            }),
        ),
        (
            "provider_failed_needs_recovery",
            json!({
                "operator_message": "Provider failed after output. Queued recovery turn on anthropic/claude-sonnet-4-6.",
                "fallback_model_ref": "anthropic/claude-sonnet-4-6"
            }),
        ),
        (
            "message_admitted",
            json!({
                "message_id": "message-1",
                "agent_id": "default",
                "kind": "operator_prompt",
                "origin": { "kind": "operator", "actor_id": null },
                "authority_class": "operator_instruction",
                "delivery_surface": "http_control_prompt",
                "admission_context": "local_process",
                "correlation_id": null,
                "causation_id": null
            }),
        ),
        ("control_applied", json!({ "action": "pause" })),
    ] {
        projection.apply_event(sample_event(kind, payload), &test_log_writer());
    }

    assert!(projection.stale_slices.is_empty());
}

#[test]
fn provider_lineage_failure_events_are_visible_in_info_conversation() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
            sample_event(
                "deferred_to_fallback",
                json!({
                    "operator_message": "OpenAI Codex authentication failed. Queued fallback turn on anthropic/claude-sonnet-4-6.",
                    "fallback_model_ref": "anthropic/claude-sonnet-4-6"
                }),
            ),
            &test_log_writer(),
        );

    let events = projection.presentation_events(OperatorDisplayMode::Info);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "deferred_to_fallback");
    assert_eq!(events[0].lane, ProjectionEventLane::Timeline);
    assert!(events[0]
        .summary
        .contains("OpenAI Codex authentication failed"));
}

#[test]
fn projection_recent_activity_clears_after_brief_or_terminal_event() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event(
            "provider_round_completed",
            json!({ "round": 1, "text_preview": "partial" }),
        ),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event("tool_executed", json!({ "tool_name": "ExecCommand" })),
        &test_log_writer(),
    );

    let activity = projection.recent_activity_events();
    assert_eq!(activity.len(), 2);
    assert_eq!(activity[0].kind, "provider_round_completed");
    assert_eq!(activity[1].kind, "tool_executed");

    projection.apply_event(
        sample_event(
            "brief_created",
            serde_json::to_value(BriefRecord::new(
                "default",
                BriefKind::Result,
                "done",
                None,
                None,
            ))
            .unwrap(),
        ),
        &test_log_writer(),
    );
    assert!(projection.recent_activity_events().is_empty());
}

#[test]
fn projection_recent_log_events_keep_high_signal_and_drop_noise() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event("agent_state_changed", json!({ "status": "Idle" })),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event(
            "provider_round_completed",
            json!({ "round": 1, "text_preview": "partial" }),
        ),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event(
            "tool_executed",
            json!({ "tool_name": "ExecCommand", "exec_command_cmd": "git status" }),
        ),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event(
            "operator_notification_requested",
            json!({
                "id": "notification-1",
                "summary": "needs review",
                "message": "needs review",
                "created_at": Utc::now()
            }),
        ),
        &test_log_writer(),
    );

    let events = projection.recent_log_events(10);
    let kinds = events
        .iter()
        .map(|event| event.kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "provider_round_completed",
            "tool_executed",
            "operator_notification_requested"
        ]
    );
}

#[test]
fn info_presentation_events_survive_callback_delivered_churn() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    // Seed a brief and an operator message early in the event log.
    let brief = BriefRecord::new("default", BriefKind::Result, "work completed", None, None);
    projection.apply_event(
        sample_event("brief_created", serde_json::to_value(&brief).unwrap()),
        &test_log_writer(),
    );
    let message = sample_message();
    projection.apply_event(
        sample_event("message_enqueued", serde_json::to_value(&message).unwrap()),
        &test_log_writer(),
    );

    // Fill with enough callback_delivered events to exceed the
    // durable conversation log limit (256) so the early brief/message
    // are pushed out of the durable conversation log, but stay within
    // the event_log limit (1024) so they remain in the event_log.
    let callback_count = 266;
    for index in 0..callback_count {
        projection.apply_event(
            AgentStreamEvent {
                contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
                id: format!("evt-callback-{index}"),
                event: "callback_delivered".into(),
                data: StreamEventEnvelope {
                    projection_effect: None,
                    event_log_epoch: Some("epoch-test".into()),
                    payload_schema: None,
                    payload_schema_version: None,
                    id: format!("evt-callback-{index}"),
                    event_seq: (index + 3) as u64,
                    ts: Utc::now(),
                    agent_id: "default".into(),
                    event_type: "callback_delivered".into(),
                    payload: json!({
                        "waiting_intent_id": format!("wait-{index}"),
                        "source": "github"
                    }),
                },
            },
            &test_log_writer(),
        );
    }

    // The durable conversation log should be full of callback_delivered,
    // pushing the early brief/message out.
    assert!(
        projection
            .durable_conversation_events()
            .all(|e| e.kind == "callback_delivered"),
        "durable log should be all callback_delivered"
    );

    // But presentation_events(Info) must still surface the brief and message
    // because it also scans the full event_log for durable events.
    let info_events = projection.presentation_events(OperatorDisplayMode::Info);
    let brief_count = info_events
        .iter()
        .filter(|e| e.kind == "brief_created")
        .count();
    let message_count = info_events
        .iter()
        .filter(|e| e.kind == "message_enqueued")
        .count();
    assert!(
        brief_count >= 1,
        "Info presentation should include brief_created even after callback_delivered churn"
    );
    assert!(
        message_count >= 1,
        "Info presentation should include message_enqueued even after callback_delivered churn"
    );
}

#[test]
fn durable_conversation_events_survive_debug_event_churn() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event(
            "work_item_written",
            json!({
                "action": "created",
                "record": {
                    "id": "work-1",
                    "agent_id": "default",
                    "workspace_id": "agent_home",
                    "objective": "operator",
                    "state": "open",
                    "plan_status": "draft",
                    "todo_list": [],
                    "created_at": Utc::now(),
                    "updated_at": Utc::now()
                }
            }),
        ),
        &test_log_writer(),
    );

    for index in 0..=EVENT_LOG_LIMIT {
        projection.apply_event(
            AgentStreamEvent {
                contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
                id: format!("evt-debug-{index}"),
                event: "provider_round_completed".into(),
                data: StreamEventEnvelope {
                    projection_effect: None,
                    event_log_epoch: Some("epoch-test".into()),
                    payload_schema: None,
                    payload_schema_version: None,
                    id: format!("evt-debug-{index}"),
                    event_seq: (index + 2) as u64,
                    ts: Utc::now(),
                    agent_id: "default".into(),
                    event_type: "provider_round_completed".into(),
                    payload: json!({ "text_preview": format!("partial-{index}") }),
                },
            },
            &test_log_writer(),
        );
    }

    assert_eq!(projection.event_log().len(), EVENT_LOG_LIMIT);
    assert_eq!(
        projection
            .durable_conversation_events()
            .last()
            .map(|event| event.summary.as_str()),
        Some("Work item Open: operator")
    );
    assert!(projection
        .event_log()
        .iter()
        .any(|event| event.id == format!("evt-debug-{EVENT_LOG_LIMIT}")));
}

#[test]
fn projection_updates_workspace_from_workspace_events() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event(
            "workspace_entered",
            json!({
                "workspace_id": "ws-main",
                "workspace_anchor": "/tmp/ws-main",
                "execution_root_id": "root-1",
                "execution_root": "/tmp/ws-main/worktree",
                "projection_kind": "git_worktree_root",
                "access_mode": "exclusive_write",
                "cwd": "/tmp/ws-main/worktree"
            }),
        ),
        &test_log_writer(),
    );

    assert_eq!(
        projection
            .workspace
            .active_workspace()
            .map(|ws| ws.workspace_id.as_str()),
        Some("ws-main")
    );
    assert_eq!(
        projection
            .workspace
            .active_workspace()
            .and_then(|ws| ws.projection_kind),
        Some(WorkspaceProjectionKind::GitWorktreeRoot)
    );
    assert_eq!(
        projection
            .agent
            .agent
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.cwd.as_path()),
        Some(PathBuf::from("/tmp/ws-main/worktree").as_path())
    );
    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::Workspace));
}

#[test]
fn projection_updates_workspace_from_workspace_used_event() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    // worktree_session is now part of the unified workspaces array

    projection.apply_event(
        sample_event(
            "workspace_used",
            json!({
                "workspace_id": crate::types::AGENT_HOME_WORKSPACE_ID,
                "workspace_anchor": "/tmp/agent-home",
                "execution_root_id": "canonical_root:agent_home",
                "execution_root": "/tmp/agent-home",
                "projection_kind": "canonical_root",
                "access_mode": "exclusive_write",
                "cwd": "/tmp/agent-home"
            }),
        ),
        &test_log_writer(),
    );

    let ws = projection
        .workspace
        .active_workspace()
        .expect("workspace_used should update active workspace");
    assert_eq!(ws.workspace_id, crate::types::AGENT_HOME_WORKSPACE_ID);
    assert_eq!(ws.execution_root.as_deref(), Some("/tmp/agent-home"));
    assert_eq!(
        projection
            .agent
            .agent
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.workspace_id.as_str()),
        Some(crate::types::AGENT_HOME_WORKSPACE_ID)
    );
    assert!(!projection.stale_slices.contains(&ProjectionSlice::Agent));
    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::Workspace));
    assert_eq!(
        projection
            .workspace
            .active_workspace()
            .and_then(|ws| ws.cwd.as_deref()),
        Some("/tmp/agent-home")
    );
    assert!(projection
        .workspace
        .active_workspace()
        .and_then(|w| w.worktree.as_ref())
        .is_none());
    assert!(projection.agent.agent.worktree_session.is_none());
}

#[test]
fn projection_updates_model_from_override_lifecycle_events() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let mut model = projection.agent.model.clone();
    let override_model = route_ref("openai/gpt-5.4");
    model.source = AgentModelSource::AgentOverride;
    model.effective_model = override_model.clone();
    model.requested_model = Some(override_model.clone());
    model.active_model = Some(override_model.clone());
    model.override_model = Some(override_model);
    model.override_reasoning_effort = Some("high".into());

    projection.apply_event(
        sample_event(
            "agent_model_override_set",
            json!({
                "agent_id": "default",
                "model": model,
                "pending_next_turn": false
            }),
        ),
        &test_log_writer(),
    );

    assert_eq!(
        projection
            .agent
            .model
            .override_model
            .as_ref()
            .map(|model| model.as_string()),
        Some("openai@default/gpt-5.4".into())
    );
    assert_eq!(
        projection.agent.model.override_reasoning_effort.as_deref(),
        Some("high")
    );
    assert!(!projection.stale_slices.contains(&ProjectionSlice::Agent));
}

#[test]
fn projection_updates_model_from_provider_round_events() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event(
            "provider_round_completed",
            json!({
                "requested_model": "openai/gpt-5.4",
                "active_model": "anthropic/claude-sonnet-4-6"
            }),
        ),
        &test_log_writer(),
    );

    assert_eq!(
        projection
            .agent
            .model
            .requested_model
            .as_ref()
            .map(|model| model.as_string()),
        Some("openai@default/gpt-5.4".into())
    );
    assert_eq!(
        projection
            .agent
            .model
            .active_model
            .as_ref()
            .map(|model| model.as_string()),
        Some("anthropic@default/claude-sonnet-4-6".into())
    );
    assert!(projection.agent.model.fallback_active);
    assert!(!projection.stale_slices.contains(&ProjectionSlice::Agent));

    projection.apply_event(
        sample_event_with_id(
            "evt-provider-round-completed-clear",
            "provider_round_completed",
            json!({
                "requested_model": null,
                "active_model": null,
                "fallback_active": false
            }),
        ),
        &test_log_writer(),
    );

    assert!(projection.agent.model.requested_model.is_none());
    assert!(projection.agent.model.active_model.is_none());
    assert!(!projection.agent.model.fallback_active);
}

#[test]
fn projection_tracks_task_and_work_item_updates() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let mut task = sample_task();
    task.id = "task-stream".into();
    task.status = TaskStatus::Running;
    task.updated_at = Utc::now();

    let mut work_item = WorkItemRecord::new("default", "queued delivery", WorkItemState::Open);
    work_item.id = "work-stream".into();

    projection.apply_event(
        sample_event("task_status_updated", serde_json::to_value(&task).unwrap()),
        &test_log_writer(),
    );
    projection.apply_event(
        sample_event(
            "work_item_written",
            json!({
                "action": "created",
                "record": work_item
            }),
        ),
        &test_log_writer(),
    );

    assert_eq!(
        projection.tasks.last().map(|record| record.id.as_str()),
        Some("task-stream")
    );
    assert!(!projection
        .work_items
        .iter()
        .any(|record| record.id == "work-stream"));
    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::WorkItems));
}

#[test]
fn summarize_event_uses_lightweight_task_audit_payload() {
    let mut task = sample_task();
    task.id = "task-stream".into();
    task.status = TaskStatus::Running;
    task.summary = Some("running stream task".into());

    let event = sample_event(
        "task_status_updated",
        serde_json::to_value(TaskLifecycleAuditEvent::from_task(&task)).unwrap(),
    );

    assert_eq!(
        summarize_event(&event),
        "running stream task [Running]".to_string()
    );
}

#[test]
fn projection_upserts_active_tasks_and_removes_terminal_updates() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    projection.tasks.clear();
    let mut task = sample_task();
    task.id = "task-active".into();
    task.status = TaskStatus::Queued;

    projection.apply_event(
        sample_event("task_created", serde_json::to_value(&task).unwrap()),
        &test_log_writer(),
    );
    assert_eq!(projection.tasks.len(), 1);
    assert_eq!(projection.tasks[0].status, TaskStatus::Queued);

    task.status = TaskStatus::Running;
    task.summary = Some("running latest".into());
    task.updated_at = task.updated_at + chrono::Duration::seconds(1);
    projection.apply_event(
        sample_event("task_status_updated", serde_json::to_value(&task).unwrap()),
        &test_log_writer(),
    );
    assert_eq!(projection.tasks.len(), 1);
    assert_eq!(
        projection.tasks[0].summary.as_deref(),
        Some("running latest")
    );
    assert_eq!(projection.tasks[0].status, TaskStatus::Running);

    task.status = TaskStatus::Completed;
    task.updated_at = task.updated_at + chrono::Duration::seconds(1);
    projection.apply_event(
        sample_event("task_result_received", serde_json::to_value(&task).unwrap()),
        &test_log_writer(),
    );
    assert!(projection.tasks.is_empty());
}

#[test]
fn projection_bootstrap_keeps_only_active_tasks_in_chronological_order() {
    let mut old_running = sample_task();
    old_running.id = "old-running".into();
    old_running.status = TaskStatus::Running;
    old_running.updated_at = Utc::now() - chrono::Duration::seconds(10);

    let mut completed = sample_task();
    completed.id = "completed".into();
    completed.status = TaskStatus::Completed;
    completed.updated_at = Utc::now() - chrono::Duration::seconds(5);

    let mut new_running = sample_task();
    new_running.id = "new-running".into();
    new_running.status = TaskStatus::Running;
    new_running.updated_at = Utc::now();

    let mut snapshot = sample_snapshot();
    snapshot.tasks = vec![new_running, completed, old_running];

    let projection = TuiProjection::from_snapshot(snapshot);
    let task_ids = projection
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(task_ids, vec!["old-running", "new-running"]);
}

#[test]
fn projection_bootstrap_bounds_active_tasks() {
    let now = Utc::now();
    let mut snapshot = sample_snapshot();
    snapshot.tasks = (0..(TASK_TAIL_LIMIT + 5))
        .map(|index| {
            let mut task = sample_task();
            task.id = format!("task-{index}");
            task.status = TaskStatus::Running;
            task.updated_at = now + chrono::Duration::seconds(index as i64);
            task
        })
        .collect();

    let projection = TuiProjection::from_snapshot(snapshot);
    let task_ids = projection
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(projection.tasks.len(), TASK_TAIL_LIMIT);
    assert_eq!(task_ids.first().copied(), Some("task-5"));
    assert_eq!(task_ids.last().copied(), Some("task-54"));
}

#[test]
fn projection_marks_waiting_and_external_triggers_stale_when_event_payload_is_partial() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event(
            "callback_delivered",
            json!({
                "external_trigger_id": "cb-2",
                "agent_id": "default",
                "scope": "agent",
                "delivery_mode": "wake_hint"
            }),
        ),
        &test_log_writer(),
    );

    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::ExternalTriggers));
}

#[test]
fn projection_applies_lightweight_agent_state_changed_and_marks_full_slices_stale() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let mut state = projection.agent.agent.clone();
    state.status = crate::types::AgentStatus::AwakeRunning;
    state.current_run_id = Some("run-updated".into());
    state.pending = 7;
    state.turn_index = 9;
    state.current_turn_id = Some("turn-updated".into());
    state.current_turn_work_item_id = Some("turn-work-updated".into());
    state.current_work_item_id = Some("work-updated".into());
    state.attached_workspaces = vec!["ws-updated".into()];
    if let Some(entry) = &mut state.active_workspace_entry {
        entry.workspace_id = "ws-updated".into();
    }
    state.last_turn_terminal = Some(TurnTerminalRecord {
        turn_index: 8,
        turn_id: "turn-previous".into(),
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some("full terminal text should not be in the event".into()),
        no_brief_reason: None,
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 10,
    });

    projection.apply_event(
        sample_event(
            "agent_state_changed",
            serde_json::to_value(AgentStateChangedEvent::from_state(&state)).unwrap(),
        ),
        &test_log_writer(),
    );

    assert_eq!(
        projection.agent.agent.status,
        crate::types::AgentStatus::AwakeRunning
    );
    assert_eq!(
        projection.session.current_run_id.as_deref(),
        Some("run-updated")
    );
    assert_eq!(projection.session.pending_count, 7);
    assert_eq!(projection.agent.agent.turn_index, 9);
    assert_eq!(
        projection.agent.agent.current_work_item_id.as_deref(),
        Some("work-updated")
    );
    assert_eq!(
        projection.agent.agent.attached_workspaces,
        vec!["ws-updated".to_string()]
    );
    assert!(projection.agent.agent.active_workspace_entry.is_none());
    assert!(projection.stale_slices.contains(&ProjectionSlice::Agent));
    assert!(projection.stale_slices.contains(&ProjectionSlice::Session));
    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::Workspace));
}

#[test]
fn projection_replays_legacy_session_state_changed_full_payload() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let mut state = projection.agent.agent.clone();
    state.pending = 5;
    state.current_run_id = Some("legacy-run".into());
    if let Some(entry) = &mut state.active_workspace_entry {
        entry.workspace_id = "legacy-ws".into();
    }

    projection.apply_event(
        sample_event(
            "session_state_changed",
            serde_json::to_value(&state).unwrap(),
        ),
        &test_log_writer(),
    );

    assert_eq!(projection.session.pending_count, 5);
    assert_eq!(
        projection.session.current_run_id.as_deref(),
        Some("legacy-run")
    );
    assert_eq!(
        projection
            .workspace
            .active_workspace()
            .map(|ws| ws.workspace_id.as_str()),
        Some("legacy-ws")
    );
    assert!(!projection.stale_slices.contains(&ProjectionSlice::Session));
}

#[test]
fn projection_marks_workspace_stale_for_agent_state_events_without_occupancy() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());
    let mut state = projection.agent.agent.clone();
    if let Some(entry) = &mut state.active_workspace_entry {
        entry.workspace_id = "ws-updated".into();
    }

    projection.apply_event(
        sample_event("agent_state_changed", serde_json::to_value(&state).unwrap()),
        &test_log_writer(),
    );

    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::Workspace));
    assert!(projection.agent.active_workspace_occupancy.is_none());
    assert_eq!(
        projection
            .workspace
            .active_workspace()
            .map(|ws| ws.workspace_id.as_str()),
        Some("ws-updated")
    );
}

#[test]
fn projection_keeps_workspace_stale_during_worktree_transitions() {
    let mut projection = TuiProjection::from_snapshot(sample_snapshot());

    projection.apply_event(
        sample_event(
            "worktree_entered",
            json!({
                "worktree": {
                    "original_cwd": "/tmp/ws-boot",
                    "original_branch": "main",
                    "worktree_path": "/tmp/ws-boot/feature",
                    "worktree_branch": "feature/demo"
                }
            }),
        ),
        &test_log_writer(),
    );

    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::Workspace));
    assert!(projection.stale_slices.contains(&ProjectionSlice::Agent));
    assert_eq!(
        projection
            .workspace
            .active_workspace()
            .and_then(|ws| ws.worktree.as_ref())
            .and_then(|wt| wt.branch.as_deref()),
        Some("feature/demo")
    );
    assert_eq!(
        projection
            .workspace
            .active_workspace()
            .and_then(|ws| ws.cwd.as_deref()),
        Some("/tmp/ws-boot/feature")
    );

    projection.apply_event(
        sample_event(
            "worktree_exited",
            json!({
                "worktree_path": "/tmp/ws-boot/feature",
                "worktree_branch": "feature/demo",
                "removed": false
            }),
        ),
        &test_log_writer(),
    );

    assert!(projection
        .stale_slices
        .contains(&ProjectionSlice::Workspace));
    assert!(projection
        .workspace
        .active_workspace()
        .and_then(|w| w.worktree.as_ref())
        .is_none());
}

fn sample_event(kind: &str, payload: Value) -> AgentStreamEvent {
    sample_event_with_id(&format!("evt-{kind}"), kind, payload)
}

fn sample_event_with_id(id: &str, kind: &str, payload: Value) -> AgentStreamEvent {
    let event_seq = NEXT_SAMPLE_EVENT_SEQ.fetch_add(1, Ordering::Relaxed);
    AgentStreamEvent {
        contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
        id: id.to_string(),
        event: kind.to_string(),
        data: sample_event_envelope_with_payload(id, event_seq, kind, payload),
    }
}

fn sample_event_envelope(id: &str, event_seq: u64) -> StreamEventEnvelope {
    sample_event_envelope_with_payload(
        id,
        event_seq,
        "tool_executed",
        json!({ "tool_name": "ExecCommand" }),
    )
}

fn sample_event_envelope_with_payload(
    id: &str,
    event_seq: u64,
    kind: &str,
    payload: Value,
) -> StreamEventEnvelope {
    StreamEventEnvelope {
        projection_effect: None,
        event_log_epoch: Some("epoch-test".into()),
        payload_schema: None,
        payload_schema_version: None,
        id: id.into(),
        event_seq,
        ts: chrono::DateTime::<Utc>::from_timestamp(event_seq as i64, 0)
            .expect("sample event sequence must produce a valid timestamp"),
        agent_id: "default".into(),
        event_type: kind.into(),
        payload,
    }
}

fn projection_event_record(kind: &str, payload: Value) -> super::ProjectionEventRecord {
    let presentation = present_operator_event(
        kind,
        &payload,
        kind,
        &OperatorPresentationContext::default(),
    );
    super::ProjectionEventRecord {
        id: format!("evt-{kind}"),
        event_seq: 1,
        ts: Utc::now(),
        kind: kind.to_string(),
        lane: super::classify_event_lane(&presentation),
        summary: presentation.summary.clone(),
        presentation,
        payload,
    }
}

fn sample_snapshot() -> AgentStateSnapshot {
    AgentStateSnapshot {
        agent: sample_agent_summary(),
        session: StateSessionSnapshot {
            current_run_id: Some("run-1".into()),
            pending_count: 1,
            last_turn: Some(TurnTerminalRecord {
                turn_index: 1,
                turn_id: "test".into(),
                kind: TurnTerminalKind::Completed,
                reason: None,
                last_assistant_message: Some("done".into()),
                no_brief_reason: None,
                checkpoint: None,
                completed_at: Utc::now(),
                duration_ms: 12,
            }),
        },
        tasks: vec![sample_task()],
        timers: vec![TimerRecord {
            id: "timer-1".into(),
            agent_id: "default".into(),
            created_at: Utc::now(),
            duration_ms: 1000,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            content: holon_domain::TimerContent::from_legacy_summary(Some("timer".into())),
            next_fire_at: Some(Utc::now()),
            last_fired_at: None,
            fire_count: 0,
            paused_remaining_ms: None,
            schedule_generation: 0,
            calendar: None,
        }],
        work_items: {
            let mut item = WorkItemRecord::new("default", "active delivery", WorkItemState::Open);
            item.todo_list = vec![TodoItem {
                text: "do it".into(),
                state: TodoItemState::InProgress,
            }];
            vec![super::super::tests::sample_work_item_projection(item, true)]
        },
        external_triggers: vec![ExternalTriggerStateSnapshot {
            external_trigger_id: "cb-1".into(),
            target_agent_id: "default".into(),
            scope: ExternalTriggerScope::Agent,
            delivery_mode: CallbackDeliveryMode::EnqueueMessage,
            status: ExternalTriggerStatus::Active,
            created_at: Utc::now(),
            revoked_at: None,
            last_delivered_at: None,
            delivery_count: 0,
        }],
        operator_notifications: Vec::new(),
        workspace: StateWorkspaceSnapshot {
            workspaces: vec![crate::types::AgentWorkspaceInfo {
                workspace_id: "ws-boot".into(),
                workspace_alias: None,
                workspace_anchor: Some("/tmp/ws-boot".into()),
                repo_name: None,
                is_active: true,
                execution_root_id: Some("root-boot".into()),
                execution_root: Some("/tmp/ws-boot".into()),
                cwd: Some("/tmp/ws-boot/wt".into()),
                projection_kind: Some(WorkspaceProjectionKind::CanonicalRoot),
                access_mode: Some(WorkspaceAccessMode::ExclusiveWrite),
                worktree: Some(crate::types::WorktreeInfo {
                    branch: Some("feature/demo".into()),
                    path: Some("/tmp/ws-boot/wt".into()),
                    original_branch: Some("main".into()),
                    original_cwd: Some("/tmp/ws-boot".into()),
                }),
            }],
        },
        execution: None,
    }
}

fn sample_task() -> TaskRecord {
    TaskRecord {
        id: "task-1".into(),
        agent_id: "default".into(),
        kind: crate::types::TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("demo task".into()),
        detail: Some(json!({ "wait_policy": "blocking" })),
        recovery: None,
    }
}

fn sample_message() -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        crate::types::AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "stream prompt".into(),
        },
    );
    message.delivery_surface = Some(MessageDeliverySurface::CliPrompt);
    message
}

fn sample_agent_summary() -> AgentSummary {
    let mut state = AgentState::new("default");
    state.status = crate::types::AgentStatus::AwakeIdle;
    state.pending = 1;
    state.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
        workspace_id: "ws-boot".into(),
        workspace_anchor: PathBuf::from("/tmp/ws-boot"),
        execution_root_id: "canonical_root:ws-boot".into(),
        execution_root: PathBuf::from("/tmp/ws-boot"),
        projection_kind: WorkspaceProjectionKind::CanonicalRoot,
        access_mode: WorkspaceAccessMode::ExclusiveWrite,
        cwd: PathBuf::from("/tmp/ws-boot"),
        occupancy_id: None,
        projection_metadata: None,
    });
    AgentSummary {
        identity: AgentIdentityView {
            agent_id: "default".into(),
            name: None,
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            can_rename: false,
            status: AgentRegistryStatus::Active,
            is_default_agent: true,
            incarnation: 1,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        },
        agent: state,
        active_task_count: 0,
        lifecycle: AgentLifecycleHint::default(),
        scheduling_posture: Default::default(),
        model: AgentModelState {
            effective_model: route_ref("anthropic/claude-sonnet-4-6"),
            requested_model: Some(route_ref("anthropic/claude-sonnet-4-6")),
            active_model: Some(route_ref("anthropic/claude-sonnet-4-6")),
            fallback_active: false,
            runtime_default_model: route_ref("anthropic/claude-sonnet-4-6"),
            override_model: None,
            override_reasoning_effort: None,
            override_service_tier: None,
            effective_service_tier: None,
            source: AgentModelSource::RuntimeDefault,
            effective_fallback_models: Vec::new(),
            override_unresolved: false,
            resolved_policy: holon_domain::model_values::ResolvedRuntimeModelPolicy {
                model_ref: crate::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
                display_name: "Claude Sonnet 4.6".into(),
                description: "Sample policy".into(),
                context_window_tokens: Some(200_000),
                effective_context_window_percent: 90,
                prompt_budget_estimated_tokens: 180_000,
                compaction_trigger_estimated_tokens: 180_000,
                compaction_keep_recent_estimated_tokens: 68_400,
                runtime_max_output_tokens: 32_000,
                verbosity: None,
                tool_output_truncation_estimated_tokens: 2_500,
                max_output_tokens_upper_limit: Some(128_000),
                capabilities: holon_domain::model_values::ModelCapabilityFlags {
                    image_input: true,
                    ..holon_domain::model_values::ModelCapabilityFlags::default()
                },
                agent_turn: true,
                decision_capable: false,
                decision_protocol: None,
                reasoning_effort_options: Vec::new(),
                source: holon_domain::model_values::ModelMetadataSource::BuiltInCatalog,
                evidence: Default::default(),
            },
        },
        token_usage: AgentTokenUsageSummary {
            total: TokenUsage::new(0, 0),
            total_model_rounds: 0,
            last_turn: None,
        },
        closure: ClosureDecision {
            outcome: ClosureOutcome::Completed,
            waiting_reason: None,
            work_signal: None,
            runtime_posture: RuntimePosture::Awake,
            evidence: Vec::new(),
        },
        execution: ExecutionSnapshot {
            profile: ExecutionProfile::default(),
            policy: ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: None,
            workspace_anchor: PathBuf::from("/tmp"),
            execution_root: PathBuf::from("/tmp"),
            cwd: PathBuf::from("/tmp"),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
            execution_roots: Vec::new(),
        },
        active_workspace_occupancy: None,
        loaded_agents_md: LoadedAgentsMdView::default(),
        skills: SkillsRuntimeView::default(),
        active_children: Vec::<ChildAgentSummary>::new(),
        active_wait_conditions: Vec::new(),
        active_external_triggers: Vec::new(),
        recent_operator_notifications: Vec::new(),
        recent_brief_count: 1,
        recent_event_count: 1,
        provider_cooldown_wait: None,
    }
}
