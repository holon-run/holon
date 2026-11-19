use super::super::{
    app::TuiApp,
    chat::{collect_chat_items, ConversationCell},
    runtime::TuiRuntimeMessage,
};
use super::*;
use serde_json::json;

fn turn(
    id: &str,
    index: u64,
    revision: u64,
    execution: Value,
    briefs: Vec<&str>,
) -> ConversationTurnSummary {
    serde_json::from_value(json!({
            "turn_id": id, "key": {"turn_index": index, "turn_id": id}, "revision": revision,
            "presentation_class": "operator", "inputs": [{"message_id": format!("input-{id}"), "preview": "operator prompt"}],
            "execution": execution, "started_at": "2026-01-01T00:00:00Z", "completed_at": null,
            "duration_ms": null, "result": {"kind": if briefs.is_empty() { "pending" } else { "available" }},
            "settled": false, "attention": null, "detail_coverage": {"kind": "complete"}, "brief_ids": briefs
        })).unwrap()
}

fn page(
    turns: Vec<ConversationTurnSummary>,
    active_turns: Vec<ConversationTurnSummary>,
    before: Option<&str>,
) -> SummaryPage {
    serde_json::from_value(json!({
            "schema_version": 2, "query_version": 2, "runtime_id": "runtime", "event_log_epoch": "epoch",
            "visibility_scope_id": "public", "snapshot_through_seq": 10, "snapshot_cursor": "snapshot-10",
            "turns": turns, "active_turns": active_turns, "pending_inputs": [],
            "next_before_cursor": before, "has_more": before.is_some()
        })).unwrap()
}

fn model() -> ConversationModel {
    ConversationModel::snapshot(
        "default".into(),
        page(
            vec![],
            vec![turn("active", 2, 1, json!({"kind": "active"}), vec![])],
            None,
        ),
    )
    .unwrap()
}

fn begin(from: u64, through: u64) -> Value {
    json!({"type": "batch_begin", "batch_id": "batch", "schema_version": 2, "query_version": 2,
            "runtime_id": "runtime", "event_log_epoch": "epoch", "visibility_scope_id": "public", "from_seq": from, "through_seq": through})
}

fn checkpoint(through: u64) -> Value {
    json!({"type": "checkpoint", "batch_id": "batch", "event_log_epoch": "epoch", "visibility_scope_id": "public", "through_seq": through, "checkpoint": format!("checkpoint-{through}")})
}

fn activity() -> ConversationActivity {
    serde_json::from_value(json!({"kind": "tool", "id": "tool-1", "key": {"event_seq": 11, "activity_id": "tool-1"}, "revision": 1, "summary": "tool detail"})).unwrap()
}

fn app(model: ConversationModel) -> TuiApp {
    let client = LocalClient::new(super::super::tests::test_config()).unwrap();
    let mut app = TuiApp::new(
        client,
        super::super::logging::TuiLogWriter::new_temp().unwrap(),
    );
    app.agents = vec![super::super::tests::sample_agent_summary("default")];
    app.conversation = Some(model);
    app
}

fn bodies(app: &TuiApp) -> Vec<String> {
    collect_chat_items(app)
            .iter()
            .filter(|cell| !matches!(cell, ConversationCell::Canonical { key, .. } if key.ends_with("/header")))
            .map(|cell| cell.sort_body().to_owned())
            .collect()
}

fn readable_brief(id: &str, text: &str) -> BriefRecord {
    serde_json::from_value(json!({
        "id": id, "agent_id": "default", "kind": "result",
        "created_at": "2026-01-01T00:00:01Z", "text": text,
        "attachments": null, "related_message_id": null, "related_task_id": null
    }))
    .unwrap()
}

fn screen(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn consecutive_frames_stop_working_keep_details_then_fold_with_readable_brief() {
    for width in [80, 84, 120] {
        let mut model = model();
        model.activities.insert("active".into(), vec![activity()]);
        let mut app = app(model);
        app.composer = super::super::composer::ComposerState::from("未发送 draft");
        let cursor = app.composer.cursor();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 32)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut app))
            .unwrap();
        let running = screen(&terminal);
        assert_eq!(running.matches("Working").count(), 1);
        assert!(running.contains("tool detail"));

        let model = app.conversation.as_mut().unwrap();
        model.upsert_turn(turn(
            "active",
            2,
            2,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec![],
        ));
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut app))
            .unwrap();
        let pending = screen(&terminal);
        assert!(!pending.contains("Working"));
        assert!(pending.contains("tool detail"));
        assert!(pending.contains("Awaiting canonical result"));

        let model = app.conversation.as_mut().unwrap();
        model.upsert_turn(turn(
            "active",
            2,
            3,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec!["brief"],
        ));
        model
            .brief_errors
            .insert("brief".into(), "slow or failed".into());
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut app))
            .unwrap();
        let loading = screen(&terminal);
        assert!(loading.contains("tool detail"));
        assert!(loading.contains("retrying"));
        assert!(!loading.contains("Working"));

        app.conversation.as_mut().unwrap().briefs.insert(
            "brief".into(),
            readable_brief("brief", "最终 canonical result"),
        );
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut app))
            .unwrap();
        let finished = screen(&terminal);
        assert!(finished.contains("canonical result"), "{finished}");
        assert!(finished.contains("collapsed"));
        assert!(!finished.contains("tool detail"));
        assert!(!finished.contains("Working"));
        assert_eq!(app.composer.as_str(), "未发送 draft");
        assert_eq!(app.composer.cursor(), cursor);
        assert!(app
            .conversation
            .as_ref()
            .unwrap()
            .activities
            .contains_key("active"));

        app.conversation
            .as_mut()
            .unwrap()
            .set_details("active".into(), true);
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut app))
            .unwrap();
        let reopened = screen(&terminal);
        assert!(reopened.contains("tool detail"));
        assert!(reopened.contains("canonical result"));
    }
}

#[test]
fn one_working_for_a_hundred_activities_and_manual_expansion_survives_result() {
    let mut model = model();
    for index in 0..100 {
        let mut item = activity_item(&activity()).clone();
        item.id = format!("activity-{index}");
        item.key.activity_id = item.id.clone();
        model.change(ConversationChange::ActivityUpsert {
            turn_id: "active".into(),
            activity: ConversationActivity::Tool(item),
        });
    }
    model
        .briefs
        .insert("brief".into(), readable_brief("brief", "early result"));
    model.upsert_turn(turn(
        "active",
        2,
        2,
        json!({"kind": "active"}),
        vec!["brief"],
    ));
    assert!(model.details_expanded(&model.turns["active"]));
    let view = app(model.clone());
    let text = super::super::chat::chat_text_for_width(&view, 100).to_string();
    assert_eq!(text.matches("Working").count(), 1);
    assert_eq!(text.matches("tool detail").count(), 100);
    model.set_details("active".into(), true);
    model.upsert_turn(turn(
        "active",
        2,
        3,
        json!({"kind": "terminal", "outcome": "completed"}),
        vec!["brief"],
    ));
    assert!(model.details_expanded(&model.turns["active"]));
    assert!(!super::super::chat::chat_text_for_width(&app(model), 100)
        .to_string()
        .contains("Working"));
}

#[test]
fn terminal_detail_cache_is_bounded_and_old_turns_can_be_reopened() {
    let mut model = model();
    for index in 0..40 {
        let id = format!("done-{index}");
        model.upsert_turn(turn(
            &id,
            index + 3,
            1,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec![],
        ));
        model.set_details(id.clone(), true);
        model.activities.insert(id, vec![activity()]);
    }
    assert_eq!(model.activities.len(), RETAINED_DETAIL_TURNS);
    assert!(!model.activities.contains_key("done-0"));
    model.set_details("done-0".into(), true);
    assert!(model.dirty_details.contains("done-0"));
    assert!(model.details_expanded(&model.turns["done-0"]));
    assert_eq!(model.activities.len(), RETAINED_DETAIL_TURNS - 1);
}

#[test]
fn typed_task_result_cards_separate_queue_state_and_keep_original_reply_identity() {
    let result: holon_domain::conversation::TaskResultPresentation =
        serde_json::from_value(json!({
            "task_id": "child-task", "status": "completed", "summary": "Review finished",
            "preview": "command task completed: output_path: private-envelope",
            "response_message_id": "reply-123", "runtime_only": false
        }))
        .unwrap();
    let text = super::super::chat::task_result_text(&result, Some("Queued for consumption"));
    assert!(text.contains("Completed"));
    assert!(text.contains("Queued for consumption"));
    assert!(text.contains("reply-123"));
    assert!(!text.contains("private-envelope"));
    assert!(!text.contains("Working"));
}

#[test]
fn summary_pages_merge_by_revision_without_replacing_live_membership_or_cursor() {
    let mut model = model();
    model.change(ConversationChange::OperatorRemove {
        message_id: "removed".into(),
        revision: 2,
    });
    let mut older = page(
        vec![turn(
            "old",
            1,
            1,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec![],
        )],
        vec![turn("active", 2, 0, json!({"kind": "active"}), vec![])],
        Some("older"),
    );
    older.pending_inputs = vec![serde_json::from_value(json!({"message_id": "removed", "revision": 1, "state": "queued", "preview": "stale", "presentation_class": "operator", "created_at": "2026-01-01T00:00:00Z"})).unwrap()];
    model.merge_page(older).unwrap();
    assert_eq!(model.turns.len(), 2);
    assert_eq!(model.turns["active"].revision, 1);
    assert_eq!(model.cursor, "snapshot-10");
    assert!(model.has_older());
    assert!(model.inputs.is_empty());
}

#[test]
fn deltas_are_atomic_and_gap_reset_and_mismatched_checkpoint_are_rejected() {
    let mut model = model();
    assert!(model.delta(begin(9, 11)).is_err());
    assert!(!model.delta(begin(10, 11)).unwrap());
    model
        .delta(json!({"type": "activity_upsert", "turn_id": "active", "activity": activity()}))
        .unwrap();
    assert!(model.activities.is_empty());
    assert_eq!(model.cursor, "snapshot-10");
    assert!(model.delta(checkpoint(12)).is_err());
    assert!(model.activities.is_empty());
    assert!(model
        .delta(json!({"type": "reset_required", "reason": "event_log_epoch_mismatch"}))
        .is_err());
}

#[test]
fn state_refresh_hints_follow_committed_heads_not_hydration_or_uncommitted_changes() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut current = model();
    let mut app = app(current.clone());
    app.state_refresh.started(now - Duration::from_secs(10));
    app.state_refresh
        .settled(now - Duration::from_secs(10), true);
    let publish = |app: &mut TuiApp, model: ConversationModel| {
        app.runtime_tx
            .send(TuiRuntimeMessage::ConversationLoaded {
                generation: app.conversation_generation,
                agent_id: "default".into(),
                model,
            })
            .unwrap();
        app.process_runtime_messages();
    };
    publish(&mut app, current.clone());
    assert!(!app
        .state_refresh
        .is_due(Instant::now() + Duration::from_secs(1), false));
    current.delta(begin(10, 11)).unwrap();
    publish(&mut app, current.clone());
    assert!(!app
        .state_refresh
        .is_due(Instant::now() + Duration::from_secs(1), false));
    // No visible changes: the checkpoint alone advances source coverage.
    current.delta(checkpoint(11)).unwrap();
    publish(&mut app, current);
    assert!(app
        .state_refresh
        .is_due(Instant::now() + Duration::from_secs(1), false));
}

#[test]
fn checkpoint_during_state_read_remains_dirty_after_the_read_settles() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut current = model();
    let mut app = app(current.clone());
    app.state_refresh.started(now);
    current.delta(begin(10, 11)).unwrap();
    current.delta(checkpoint(11)).unwrap();
    app.runtime_tx
        .send(TuiRuntimeMessage::ConversationLoaded {
            generation: app.conversation_generation,
            agent_id: "default".into(),
            model: current,
        })
        .unwrap();
    app.process_runtime_messages();
    assert!(!app
        .state_refresh
        .is_due(now + Duration::from_secs(6), false));
    app.state_refresh
        .settled(now + Duration::from_millis(1), true);
    assert!(app
        .state_refresh
        .is_due(now + Duration::from_secs(6), false));
}

#[test]
fn conversation_scope_reset_fences_only_its_own_pending_state_read() {
    use std::time::Instant;
    for (pending, cancelled) in [("default", true), ("other", false)] {
        let mut app = app(model());
        app.snapshot_refresh_request_id = 5;
        app.snapshot_refresh_in_flight = true;
        app.snapshot_refresh_agent_id = Some(pending.into());
        app.state_refresh.started(Instant::now());
        app.runtime_tx
            .send(TuiRuntimeMessage::ConversationStatus {
                generation: app.conversation_generation,
                error: "epoch replaced".into(),
                reset: true,
            })
            .unwrap();
        app.process_runtime_messages();
        assert_eq!(
            app.snapshot_refresh_request_id,
            if cancelled { 6 } else { 5 }
        );
        assert_eq!(app.snapshot_refresh_in_flight, !cancelled);
        assert!(app.conversation.is_none());
    }
}

#[test]
fn schema_versions_and_detail_revisions_are_checked_without_reviving_terminal_details() {
    let mut unsupported = page(vec![], vec![], None);
    unsupported.query_version = 1;
    assert!(ConversationModel::snapshot("default".into(), unsupported).is_err());
    let mut model = model();
    model.dirty_details.clear();
    model.detail_revisions.insert("active".into(), 2);
    model.change(ConversationChange::DetailInvalidated {
        turn_id: "active".into(),
        detail_revision: 2,
    });
    assert!(model.dirty_details.is_empty());
    model.change(ConversationChange::DetailInvalidated {
        turn_id: "active".into(),
        detail_revision: 3,
    });
    assert!(model.dirty_details.contains("active"));
    model.upsert_turn(turn(
        "active",
        2,
        2,
        json!({"kind": "terminal", "outcome": "completed"}),
        vec![],
    ));
    model.change(ConversationChange::DetailInvalidated {
        turn_id: "active".into(),
        detail_revision: 4,
    });
    assert!(model.dirty_details.is_empty());
}

#[test]
fn interim_brief_does_not_terminate_execution_or_hide_details() {
    let mut model = model();
    model.activities.insert("active".into(), vec![activity()]);
    model.delta(begin(10, 12)).unwrap();
    model.delta(json!({"type": "turn_summary_upsert", "turn": turn("active", 2, 2, json!({"kind": "active"}), vec!["brief"])})).unwrap();
    model.delta(checkpoint(12)).unwrap();
    assert!(active(&model.turns["active"]));
    assert_eq!(model.activities["active"].len(), 1);
    let bodies = bodies(&app(model));
    assert!(bodies.iter().any(|body| body == "tool detail"));
    assert!(bodies
        .iter()
        .any(|body| body == "Loading canonical result…"));
}

#[test]
fn terminal_retains_details_without_fabricating_a_brief_for_failed_or_cancelled_turns() {
    for outcome in [
        "completed",
        "aborted",
        "interrupted",
        "provider_failed_needs_recovery",
    ] {
        let mut model = model();
        model.activities.insert("active".into(), vec![activity()]);
        model.delta(begin(10, 12)).unwrap();
        model.delta(json!({"type": "turn_summary_upsert", "turn": turn("active", 2, 2, json!({"kind": "terminal", "outcome": outcome}), vec![])})).unwrap();
        model.delta(checkpoint(12)).unwrap();
        assert_eq!(model.activities["active"].len(), 1);
        assert!(model.briefs.is_empty());
        model.change(ConversationChange::ActivityUpsert {
            turn_id: "active".into(),
            activity: activity(),
        });
        assert_eq!(model.activities["active"].len(), 1);
        model.upsert_turn(turn("active", 2, 3, json!({"kind": "active"}), vec![]));
        assert!(!active(&model.turns["active"]));
        let bodies = bodies(&app(model));
        assert!(!bodies.iter().any(|body| body == "Working…"));
        assert!(bodies.iter().any(|body| body == "tool detail"));
        assert_eq!(
            bodies
                .iter()
                .filter(|body| body.starts_with("Turn ended:"))
                .count(),
            usize::from(outcome != "completed")
        );
    }
}

#[test]
fn terminal_keeps_only_canonical_brief_and_inputs() {
    let mut model = model();
    model.activities.insert("active".into(), vec![activity()]);
    let brief: BriefRecord = serde_json::from_value(json!({"id": "brief", "agent_id": "default", "kind": "result", "created_at": "2026-01-01T00:00:01Z", "text": "canonical result", "attachments": null, "related_message_id": null, "related_task_id": null})).unwrap();
    model.briefs.insert("brief".into(), brief);
    model.upsert_turn(turn(
        "active",
        2,
        2,
        json!({"kind": "terminal", "outcome": "completed"}),
        vec!["brief"],
    ));
    let app = app(model);
    assert_eq!(bodies(&app), ["operator prompt", "canonical result"]);
    assert!(collect_chat_items(&app)
        .iter()
        .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
    let rendered = super::super::chat::chat_text_for_width(&app, 100)
        .lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("canonical result"));
    assert!(rendered.contains("Holon"));
    assert!(!rendered.contains("Working"));
}

#[test]
fn canonical_inputs_decode_message_bodies_and_bounded_text_previews() {
    for (preview, expected) in [
        (
            r#"{"type":"text","text":"readable\nprompt"}"#,
            "readable\nprompt",
        ),
        (r#"{"type":"text","text":"truncated\u4"#, "truncated…"),
        (
            r#"{"type":"text","text":"中文\"quoted\""#,
            "中文\"quoted\"…",
        ),
        (
            r#"{"type":"json","value":{"key":"value"}}"#,
            r#"{"key":"value"}"#,
        ),
        ("plain prompt", "plain prompt"),
        (r#"{"unknown":"shape"#, r#"{"unknown":"shape"#),
    ] {
        let mut model = model();
        model.turns.get_mut("active").unwrap().inputs[0].preview = preview.into();
        model.inputs.insert(
            "pending".into(),
            serde_json::from_value(json!({
                "message_id": "pending", "revision": 1, "state": "queued",
                "preview": preview, "presentation_class": "operator",
                "created_at": "2026-01-01T00:00:01Z"
            }))
            .unwrap(),
        );
        let cells = collect_chat_items(&app(model));
        let input_bodies: Vec<_> = cells
            .iter()
            .filter_map(|cell| {
                if let ConversationCell::UserMessage { body, .. } = cell.unkeyed() {
                    Some(body.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(input_bodies, [expected, expected]);
    }
}

#[test]
fn terminal_notices_and_nonoperator_inputs_are_static() {
    let mut model = model();
    let mut terminal = turn(
        "active",
        2,
        2,
        json!({"kind": "terminal", "outcome": "aborted"}),
        vec![],
    );
    terminal.presentation_class = holon_domain::conversation::PresentationClass::Internal;
    model.upsert_turn(terminal);
    let app = app(model);
    assert!(collect_chat_items(&app)
        .iter()
        .all(|cell| matches!(cell.unkeyed(), ConversationCell::SystemNotice { .. })));
    let rendered = super::super::chat::chat_text_for_width(&app, 100)
        .lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("internal"));
    assert!(rendered.contains("Turn ended: Aborted"));
    assert!(!rendered.contains("Working"));
}

#[test]
fn completed_turn_with_pending_delivery_keeps_static_result_status() {
    let mut model = model();
    model.upsert_turn(turn(
        "active",
        2,
        2,
        json!({"kind": "terminal", "outcome": "completed"}),
        vec![],
    ));
    let app = app(model);
    assert_eq!(
        bodies(&app),
        ["operator prompt", "Awaiting canonical result"]
    );
    assert!(collect_chat_items(&app)
        .iter()
        .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
}

#[test]
fn terminal_turn_without_result_preserves_typed_reason_as_static_notice() {
    for (reason, notice) in [
        (
            json!({"kind": "reducer_only", "reason": "handled by reducer"}),
            "No canonical result: reducer-only (handled by reducer)",
        ),
        (
            json!({"kind": "aborted"}),
            "No canonical result: turn aborted",
        ),
        (
            json!({"kind": "interrupted"}),
            "No canonical result: turn interrupted",
        ),
        (
            json!({"kind": "tool_only_wait"}),
            "No canonical result: tool-only wait",
        ),
    ] {
        let mut model = model();
        model.activities.insert("active".into(), vec![activity()]);
        let mut terminal = turn(
            "active",
            2,
            2,
            json!({"kind": "terminal", "outcome": "completed"}),
            vec![],
        );
        terminal.result =
            serde_json::from_value(json!({"kind": "none", "reason": reason})).unwrap();
        model.upsert_turn(terminal);
        assert_eq!(model.activities["active"].len(), 1);
        assert!(model.briefs.is_empty());
        let app = app(model);
        assert_eq!(bodies(&app), ["operator prompt", notice]);
        assert!(collect_chat_items(&app)
            .iter()
            .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
    }
}

#[test]
fn completed_available_result_only_shows_brief_fetch_status_until_hydrated() {
    let mut model = model();
    model.upsert_turn(turn(
        "active",
        2,
        2,
        json!({"kind": "terminal", "outcome": "completed"}),
        vec!["brief"],
    ));
    assert_eq!(
        bodies(&app(model.clone())),
        ["operator prompt", "Loading canonical result…"]
    );
    model
        .brief_errors
        .insert("brief".into(), "temporary failure".into());
    assert_eq!(
        bodies(&app(model.clone())),
        ["operator prompt", "Canonical result unavailable; retrying"]
    );
    model.briefs.insert(
        "brief".into(),
        serde_json::from_value(json!({
            "id": "brief", "agent_id": "default", "kind": "result",
            "created_at": "2026-01-01T00:00:01Z", "text": "canonical result",
            "attachments": null, "related_message_id": null, "related_task_id": null
        }))
        .unwrap(),
    );
    model.brief_errors.remove("brief");
    let app = app(model);
    assert_eq!(bodies(&app), ["operator prompt", "canonical result"]);
    assert!(collect_chat_items(&app)
        .iter()
        .all(|cell| !matches!(cell, ConversationCell::ActiveActivity { .. })));
}

#[test]
fn pending_inputs_follow_queue_time_and_preserve_assignment_state() {
    let mut model = model();
    for (id, timestamp, state) in [
        ("z-first", "2026-01-01T00:00:01Z", "queued"),
        ("a-later", "2026-01-01T00:00:02Z", "assigning"),
    ] {
        model.inputs.insert(
            id.into(),
            serde_json::from_value(json!({
                "message_id": id, "revision": 1, "state": state, "preview": id,
                "presentation_class": "operator", "created_at": timestamp,
            }))
            .unwrap(),
        );
    }
    let cells = collect_chat_items(&app(model));
    let pending: Vec<_> = cells
        .iter()
        .filter_map(|cell| {
            if let ConversationCell::UserMessage {
                body,
                status: Some(status),
                ..
            } = cell.unkeyed()
            {
                Some((body.as_str(), status))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        pending,
        [
            ("z-first", &crate::types::OperatorMessageStatus::Queued),
            ("a-later", &crate::types::OperatorMessageStatus::Processing),
        ]
    );
}

#[test]
fn assistant_activity_only_exposes_visible_text_from_legacy_envelopes() {
    for (summary, expected) in [
        ("plain assistant text", Some("plain assistant text")),
        (
            r#"{"blocks":[{"type":"text","text":"visible"},{"type":"thinking","text":"PRIVATE"},{"type":"text","text":"second"}],"signature":"PRIVATE"}"#,
            Some("visible\n\nsecond"),
        ),
        (r#"{"blocks":[{"type":"thinking","text":"PRIVATE"}]}"#, None),
        (r#"{"blocks":[{"type":"text","text":"cut"#, None),
        (r#"{"thinking":"PRIVATE","signature":"PRIVATE"}"#, None),
        ("", None),
    ] {
        let mut model = model();
        let item = serde_json::from_value(json!({
            "id": "assistant", "key": {"event_seq": 11, "activity_id": "assistant"},
            "revision": 1, "summary": summary
        }))
        .unwrap();
        model
            .activities
            .insert("active".into(), vec![ConversationActivity::Assistant(item)]);
        let app = app(model);
        let bodies = bodies(&app);
        let mut expected_bodies = vec!["operator prompt"];
        expected_bodies.extend(expected);
        assert_eq!(bodies, expected_bodies);
        assert!(bodies.iter().all(|body| !body.contains("PRIVATE")));
    }
}

#[tokio::test]
async fn brief_hydration_retries_without_new_stream_events() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let request_calls = calls.clone();
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let calls = request_calls.clone();
            handlers.spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let read = socket.read(&mut buffer).await.unwrap();
                        assert!(read > 0);
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let request = String::from_utf8(request).unwrap();
                    if request.starts_with("GET /api/agents/default/conversation/stream?") {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n: heartbeat\n\n").await.unwrap();
                        while socket.read(&mut buffer).await.unwrap_or(0) > 0 {}
                        return;
                    }
                    let (status, body) = if request.starts_with("GET /api/agents/default/briefs/brief ") {
                        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            ("500 Internal Server Error", json!({"error": "transient"}))
                        } else {
                            ("200 OK", json!({
                                "id": "brief", "agent_id": "default", "kind": "result",
                                "created_at": "2026-01-01T00:00:01Z", "text": "recovered canonical result",
                                "attachments": null, "related_message_id": null, "related_task_id": null
                            }))
                        }
                    } else {
                        assert!(request.starts_with("GET /api/agents/default/conversation?"));
                        ("200 OK", json!({
                            "schema_version": 2, "query_version": 2, "runtime_id": "runtime",
                            "event_log_epoch": "epoch", "visibility_scope_id": "public",
                            "snapshot_through_seq": 10, "snapshot_cursor": "snapshot-10",
                            "turns": [turn("done", 1, 1, json!({"kind": "terminal", "outcome": "completed"}), vec!["brief"])],
                            "active_turns": [], "pending_inputs": [], "next_before_cursor": null, "has_more": false
                        }))
                    };
                    let body = body.to_string();
                    socket.write_all(format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ).as_bytes()).await.unwrap();
                });
        }
    });
    let client = LocalClient::remote(
        super::super::tests::test_config(),
        format!("http://{addr}"),
        "test-token",
    )
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_history, history_rx) = tokio::sync::mpsc::channel(1);
    let observer = tokio::spawn(observe(client, "default".into(), 1, tx, history_rx));
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(message) = rx.recv().await {
            if let TuiRuntimeMessage::ConversationLoaded { model, .. } = message {
                if model.briefs.contains_key("brief") {
                    return true;
                }
            }
        }
        false
    })
    .await;
    observer.abort();
    server.abort();
    assert!(recovered.unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn activity_replay_deduplicates_ids_but_keeps_distinct_same_body_items() {
    let mut model = model();
    for _ in 0..2 {
        model.change(ConversationChange::ActivityUpsert {
            turn_id: "active".into(),
            activity: activity(),
        });
    }
    assert_eq!(model.activities["active"].len(), 1);
    let distinct = serde_json::from_value(json!({
        "kind": "tool", "id": "tool-2", "key": {"event_seq": 12, "activity_id": "tool-2"},
        "revision": 1, "summary": "tool detail"
    }))
    .unwrap();
    model.change(ConversationChange::ActivityUpsert {
        turn_id: "active".into(),
        activity: distinct,
    });
    assert_eq!(
        bodies(&app(model))
            .iter()
            .filter(|body| *body == "tool detail")
            .count(),
        2
    );
}

#[test]
fn turn_inputs_keep_turn_order_independent_of_snapshot_page_order() {
    let mut model = model();
    let mut older = turn(
        "old",
        1,
        1,
        json!({"kind": "terminal", "outcome": "completed"}),
        vec![],
    );
    older.inputs[0].preview = "older input".into();
    model.upsert_turn(older);
    let bodies = bodies(&app(model));
    assert_eq!(bodies[0], "older input");
    assert_eq!(bodies[1], "Awaiting canonical result");
    assert_eq!(bodies[2], "operator prompt");
    assert_eq!(bodies.len(), 3);
}

#[cfg(unix)]
#[tokio::test]
async fn projected_unix_sse_does_not_require_runtime_event_contract_header() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let directory = tempfile::tempdir().unwrap();
    let socket_path = directory.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 1024];
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                break;
            }
        }
        assert!(String::from_utf8(request)
            .unwrap()
            .starts_with("GET /api/agents/default/conversation/stream?"));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n: heartbeat\n\ndata: {\"type\":\"reset_required\",\"reason\":\"retention_expired\"}\n\n").await.unwrap();
    });
    let mut config = super::super::tests::test_config();
    config.socket_path = socket_path;
    let client = LocalClient::new(config).unwrap();
    let mut stream = client
        .stream_read_path("/agents/default/conversation/stream?after=checkpoint")
        .await
        .unwrap();
    assert_eq!(stream.next_json().await.unwrap()["type"], "reset_required");
    server.await.unwrap();
}

#[test]
fn stale_agent_switch_and_removed_agent_responses_do_not_restore_old_cache() {
    let mut app = app(model());
    app.conversation_generation = 2;
    app.runtime_tx
        .send(TuiRuntimeMessage::ConversationLoaded {
            generation: 1,
            agent_id: "default".into(),
            model: model(),
        })
        .unwrap();
    app.process_runtime_messages();
    assert_eq!(app.conversation_generation, 2);
    app.clear_projection_view();
    app.runtime_tx
        .send(TuiRuntimeMessage::ConversationLoaded {
            generation: 2,
            agent_id: "default".into(),
            model: model(),
        })
        .unwrap();
    app.process_runtime_messages();
    assert!(app.conversation.is_none());
    app.agents.clear();
    let generation = app.conversation_generation;
    app.runtime_tx
        .send(TuiRuntimeMessage::ConversationLoaded {
            generation,
            agent_id: "default".into(),
            model: model(),
        })
        .unwrap();
    app.process_runtime_messages();
    assert!(app.conversation.is_none());
}

#[tokio::test]
async fn http_summary_pagination_and_active_activity_hydration_use_v2_contract() {
    use axum::{routing::get, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let requests = Arc::new(AtomicUsize::new(0));
    let request_count = requests.clone();
    let router = Router::new()
            .route("/api/agents/default/conversation", get(move |axum::extract::Query(params): axum::extract::Query<BTreeMap<String, String>>| {
                let count = request_count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let page = if params.contains_key("before") {
                        page(vec![turn("old", 1, 1, json!({"kind": "terminal", "outcome": "completed"}), vec![])], vec![], None)
                    } else { page(vec![], vec![turn("active", 2, 1, json!({"kind": "active"}), vec![])], Some("older&cursor")) };
                    Json(json!({"schema_version": page.schema_version, "query_version": page.query_version, "runtime_id": page.runtime_id, "event_log_epoch": page.event_log_epoch, "visibility_scope_id": page.visibility_scope_id, "snapshot_through_seq": page.snapshot_through_seq, "snapshot_cursor": page.snapshot_cursor, "turns": page.turns, "active_turns": page.active_turns, "pending_inputs": page.pending_inputs, "next_before_cursor": page.next_before_cursor, "has_more": page.has_more}))
                }
            }))
            .route("/api/agents/default/turns/active/activities", get(|| async { Json(json!({
                "schema_version": 2, "query_version": 2, "runtime_id": "runtime", "event_log_epoch": "epoch", "visibility_scope_id": "public",
                "turn": turn("active", 2, 1, json!({"kind": "active"}), vec![]), "detail_revision": 1, "activities": [activity()], "has_more": false, "next_before_cursor": null
            })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = LocalClient::remote(
        super::super::tests::test_config(),
        format!("http://{address}"),
        "test-token",
    )
    .unwrap();
    let mut model = ConversationModel::load(&client, "default").await.unwrap();
    model.hydrate(&client).await.unwrap();
    assert_eq!(model.activities["active"], vec![activity()]);
    model.older(&client).await.unwrap();
    assert_eq!(model.turns.len(), 2);
    assert!(!model.has_older());
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    server.abort();
}

#[test]
fn live_tool_candidates_cannot_be_starved_by_open_or_folded_history() {
    let mut model = model();
    let tool = |id: &str, seq: u64| {
        ConversationActivity::Tool(
            serde_json::from_value(json!({
                "id": format!("tool:{id}"), "key": {"event_seq": seq, "activity_id": id},
                "revision": 1, "summary": "tool"
            }))
            .unwrap(),
        )
    };
    model
        .activities
        .insert("active".into(), vec![tool("live", 1)]);
    model.upsert_turn(turn(
        "old",
        1,
        1,
        json!({"kind": "terminal", "outcome": "completed"}),
        vec![],
    ));
    model.set_details("old".into(), true);
    model.activities.insert(
        "old".into(),
        (0..150)
            .map(|seq| tool(&format!("old-{seq}"), seq))
            .collect(),
    );
    assert_eq!(model.tool_candidates()[0].1, "live");
    model.set_details("old".into(), false);
    assert!(!model.wants_details("old"));
    assert_eq!(model.tool_candidates()[0].1, "live");
    assert_eq!(model.tool_candidates().len(), TOOL_LIMIT);
}

#[tokio::test]
async fn history_details_and_tool_records_are_lazy_revision_keyed_and_bound_to_turn() {
    use axum::{routing::get, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let detail_count = detail_calls.clone();
    let tool_count = tool_calls.clone();
    let router = Router::new()
            .route("/api/agents/default/turns/done/activities", get(move || {
                detail_count.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({
                    "schema_version": 2, "query_version": 2, "runtime_id": "runtime",
                    "event_log_epoch": "epoch", "visibility_scope_id": "public",
                    "turn": turn("done", 1, 1, json!({"kind": "terminal", "outcome": "completed"}), vec![]),
                    "detail_revision": 1, "activities": [{
                        "kind": "tool", "id": "tool:record", "key": {"event_seq": 11, "activity_id": "tool:record"},
                        "revision": 1, "summary": "ReadFile · success"
                    }], "has_more": false, "next_before_cursor": null
                })) }
            }))
            .route("/api/agents/default/tool-executions/record", get(move || {
                let count = tool_count.fetch_add(1, Ordering::SeqCst);
                async move { Json(json!({
                    "id": "record", "agent_id": if count < 2 { "default" } else { "foreign" },
                    "turn_id": "done", "tool_name": "ReadFile", "created_at": "2026-01-01T00:00:00Z",
                    "authority_class": "runtime_instruction", "status": "success",
                    "input": {"path": "README.md"}, "output": {"result": {"text": "file contents"}},
                    "summary": "not parsed"
                })) }
            }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = LocalClient::remote(
        super::super::tests::test_config(),
        format!("http://{address}"),
        "test-token",
    )
    .unwrap();
    let mut model = ConversationModel::snapshot(
        "default".into(),
        page(
            vec![turn(
                "done",
                1,
                1,
                json!({"kind": "terminal", "outcome": "completed"}),
                vec![],
            )],
            vec![],
            None,
        ),
    )
    .unwrap();
    model.hydrate(&client).await.unwrap();
    assert_eq!(detail_calls.load(Ordering::SeqCst), 0);
    model.set_details("done".into(), true);
    model.hydrate(&client).await.unwrap();
    assert_eq!(detail_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.tools["record"].input["path"], "README.md");
    model.hydrate(&client).await.unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    for revision in [2, 3] {
        let mut item = activity_item(&model.activities["done"][0]).clone();
        item.revision = revision;
        model.change(ConversationChange::ActivityUpsert {
            turn_id: "done".into(),
            activity: ConversationActivity::Tool(item),
        });
        model.hydrate(&client).await.unwrap();
    }
    assert_eq!(tool_calls.load(Ordering::SeqCst), 3);
    assert_eq!(model.tools["record"].agent_id, "default");
    assert_eq!(model.tool_revisions["record"], 2);
    assert_eq!(model.tool_errors["record"], "Tool record binding mismatch");
    for _ in 0..5 {
        model.hydrate(&client).await.unwrap();
    }
    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        3,
        "same revision respects failure backoff"
    );
    model.tool_retries.get_mut("record").unwrap().retry_at = tokio::time::Instant::now();
    model.hydrate(&client).await.unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 4);
    assert_eq!(model.tool_retries["record"].attempt, 1);
    let mut item = activity_item(&model.activities["done"][0]).clone();
    item.revision = 4;
    model.change(ConversationChange::ActivityUpsert {
        turn_id: "done".into(),
        activity: ConversationActivity::Tool(item),
    });
    model.hydrate(&client).await.unwrap();
    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        5,
        "new revision may retry immediately"
    );
    assert_eq!(model.tool_retries["record"].attempt, 0);
    model.set_details("done".into(), true);
    assert!(
        !model.tool_retries.contains_key("record"),
        "manual retry clears only its turn's backoff"
    );
    server.abort();
}

#[tokio::test]
async fn requested_task_result_survives_full_error_cache_and_unicode_is_bounded() {
    use axum::{routing::get, Json, Router};
    let router = Router::new().route("/api/agents/default/tasks/{task_id}/output", get(
            |axum::extract::Path(id): axum::extract::Path<String>| async move {
                Json(json!({"retrieval_status": "success", "task": {
                    "task_id": if id == "mismatch" { "foreign" } else { &id },
                    "kind": "command_task", "status": "completed", "summary": "Original task result",
                    "output_preview": if id == "long" { "中文结果".repeat(10_000) } else { "complete task delivery".into() },
                    "output_truncated": false, "result_summary": null, "exit_status": 0
                }}))
            }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = LocalClient::remote(
        super::super::tests::test_config(),
        format!("http://{address}"),
        "test-token",
    )
    .unwrap();
    let mut model = model();
    for index in 0..RETAINED_DETAIL_TURNS {
        model
            .task_result_errors
            .insert(format!("error-{index}"), "unavailable".into());
    }
    model
        .load_task_result(&client, "a-selected".into(), None)
        .await;
    assert!(model.task_results["a-selected"].contains("complete task delivery"));
    assert_eq!(
        model.task_results.len() + model.task_result_errors.len(),
        RETAINED_DETAIL_TURNS
    );
    for index in 0..20 {
        let id = format!("z-{index}");
        model.load_task_result(&client, id.clone(), None).await;
        assert!(model.task_results.contains_key(&id));
    }
    model
        .load_task_result(&client, "a-selected".into(), None)
        .await;
    assert!(model.task_results.contains_key("a-selected"));
    model.load_task_result(&client, "long".into(), None).await;
    assert!(model.task_results["long"].contains("Display limit"));
    assert!(model.task_results["long"].len() < 66_000);
    model
        .load_task_result(&client, "mismatch".into(), None)
        .await;
    assert!(!model.task_results.contains_key("mismatch"));
    assert!(model.task_result_errors["mismatch"].contains("binding mismatch"));
    server.abort();
}

#[tokio::test]
async fn http_reset_required_discards_cursor_and_loads_a_fresh_snapshot() {
    use axum::{routing::get, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let router = Router::new()
            .route("/api/agents/default/conversation", get(move || {
                let count = count.clone();
                async move {
                    let read = count.fetch_add(1, Ordering::SeqCst);
                    let turns = if read == 0 { vec![] } else { vec![turn("fresh", 1, 1, json!({"kind":"terminal","outcome":"completed"}), vec![])] };
                    Json(json!({"schema_version":2,"query_version":2,"runtime_id":"runtime","event_log_epoch":"epoch","visibility_scope_id":"public","snapshot_through_seq":10,"snapshot_cursor":"snapshot-10","turns":turns,"active_turns":[],"pending_inputs":[],"next_before_cursor":null,"has_more":false}))
                }
            }))
            .route("/api/agents/default/conversation/stream", get(|| async {
                (axum::http::StatusCode::CONFLICT, Json(json!({"error":"expired","code":"conversation_reset_required"})))
            }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = LocalClient::remote(
        super::super::tests::test_config(),
        format!("http://{address}"),
        "test-token",
    )
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_history_tx, history_rx) = tokio::sync::mpsc::channel(1);
    let worker = tokio::spawn(observe(client, "default".into(), 1, tx, history_rx));
    let mut saw_reset = false;
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        loop {
            match rx.recv().await.unwrap() {
                TuiRuntimeMessage::ConversationStatus { reset, .. } => {
                    saw_reset |= reset;
                }
                TuiRuntimeMessage::ConversationLoaded { model, .. }
                    if model.turns.contains_key("fresh") =>
                {
                    break
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_reset);
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    worker.abort();
    server.abort();
}

#[tokio::test]
async fn sse_disconnect_replays_from_last_committed_checkpoint_not_partial_batch() {
    use axum::{
        response::sse::{Event, Sse},
        routing::get,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};
    let cursors = Arc::new(Mutex::new(Vec::new()));
    let recorded = cursors.clone();
    let router = Router::new()
            .route("/api/agents/default/conversation", get(|| async { Json(json!({
                "schema_version": 2, "query_version": 2, "runtime_id": "runtime", "event_log_epoch": "epoch", "visibility_scope_id": "public", "snapshot_through_seq": 10, "snapshot_cursor": "snapshot-10", "turns": [], "active_turns": [], "pending_inputs": [], "next_before_cursor": null, "has_more": false
            })) }))
            .route("/api/agents/default/conversation/stream", get(move |axum::extract::Query(params): axum::extract::Query<BTreeMap<String, String>>| {
                let recorded = recorded.clone();
                async move {
                    let count = { let mut cursors = recorded.lock().unwrap(); cursors.push(params["after"].clone()); cursors.len() };
                    let events = if count == 1 {
                        vec![begin(10, 11), json!({"type": "turn_summary_upsert", "turn": turn("partial", 1, 1, json!({"kind": "active"}), vec![])})]
                    } else {
                        vec![begin(10, 12), json!({"type": "turn_summary_upsert", "turn": turn("done", 2, 1, json!({"kind": "terminal", "outcome": "completed"}), vec![])}), checkpoint(12)]
                    };
                    Sse::new(tokio_stream::iter(events.into_iter().map(|value| Ok::<_, std::convert::Infallible>(Event::default().data(value.to_string())))))
                }
            }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = LocalClient::remote(
        super::super::tests::test_config(),
        format!("http://{address}"),
        "test-token",
    )
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (_history_tx, history_rx) = tokio::sync::mpsc::channel(1);
    let worker = tokio::spawn(observe(client, "default".into(), 4, tx, history_rx));
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            if let Some(TuiRuntimeMessage::ConversationLoaded {
                generation, model, ..
            }) = rx.recv().await
            {
                assert_eq!(generation, 4);
                assert!(!model.turns.contains_key("partial"));
                if model.turns.contains_key("done") {
                    assert_eq!(model.cursor, "checkpoint-12");
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        &cursors.lock().unwrap()[..2],
        ["snapshot-10", "snapshot-10"]
    );
    worker.abort();
    server.abort();
}
