use holon_domain::{
    AdmissionContext, AgentMessageRoutingContext, AuthorityClass, ContinuationTriggerKind,
    MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    TurnRecord,
};
use serde_json::json;

use crate::{
    budget::{estimate_section_tokens, estimate_text_tokens},
    episodes::recent_turn_window_start,
    history::{
        latest_trusted_operator_input, render_budgeted_recent_turns, turn_record_owns_object,
    },
    render::{message_header, render_current_input_section},
};

#[cfg(test)]
fn turn_record_matches_message(record: &TurnRecord, message: &MessageEnvelope) -> bool {
    turn_record_owns_object(record, message.turn_id.as_deref())
}

#[test]
fn compact_current_input_budget_includes_reply_expectation() {
    let mut message = MessageEnvelope::new(
        "target",
        MessageKind::InternalFollowup,
        MessageOrigin::Task {
            task_id: "task-1".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "request body that should be truncated to the compact budget".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.created_at = "2024-01-02T03:04:05.123Z".parse().unwrap();
    message.correlation_id = Some("task-1".into());
    message.metadata = Some(json!({
        "agent_message_delivery": {
            "delivery_id": "delivery-1",
            "caller_agent_id": "sender"
        },
        "agent_message_reply_expectation": {
            "mode": "required",
            "sender_agent_id": "sender",
            "waiting_task_id": "task-1",
            "request_message_id": message.id,
            "request_delivery_id": "delivery-1"
        }
    }));
    message.routing_context = Some(AgentMessageRoutingContext {
        message_id: message.id.clone(),
        sender_agent_id: Some("sender".into()),
        recipient_agent_id: "target".into(),
        reply_to_agent_id: Some("sender".into()),
        correlation_id: Some("task-1".into()),
        in_reply_to_message_id: None,
        original_sender_agent_id: None,
        original_reply_to_agent_id: None,
    });

    let section_budget =
        48 + estimate_text_tokens("[message_created_at=2024-01-02T03:04:05.123Z UTC]");
    let compact = render_current_input_section(&message, section_budget, None);

    assert!(
        estimate_section_tokens(&compact) <= section_budget,
        "tokens={} content={:?}",
        estimate_section_tokens(&compact),
        compact.content
    );
    assert!(compact.content.contains("Reply: SendAgentMessage"));
    assert!(compact.content.contains("sender=sender"));
    assert!(compact.content.contains("reply=sender"));
    assert!(compact
        .content
        .contains("message_created_at=2024-01-02T03:04:05.123Z UTC"));
}
#[test]
fn oversized_latest_recent_turn_is_truncated_instead_of_dropped() {
    let rendered = render_budgeted_recent_turns(
            "Recent turns:",
            vec![format!(
                "- Turn turn-over-budget:\n  - produced briefs:\n    - Result excerpt: keep this continuity anchor brief_ref=brief:over-budget\n  - tool executions:\n{}",
                "    - summary: total=1 success=1 error=0 promoted=0 refs=[tool_execution:tool-over-budget:output]\n".repeat(80)
            )],
            140,
        )
        .expect("oversized latest turn should still render as a truncated continuity anchor");

    assert!(rendered.contains("Recent turns:"));
    assert!(rendered.contains("- Turn turn-over-budget:"));
    assert!(rendered.contains("brief_ref=brief:over-budget"));
    assert!(rendered.contains("[truncated recent turn; use visible refs for full evidence]"));
}
#[test]
fn recent_turn_window_start_ignores_zero_sentinel_turn_index() {
    let sentinel = holon_domain::TurnRecord::new("default", "turn-zero", 0);
    let recent = holon_domain::TurnRecord::new("default", "turn-recent", 6);

    assert_eq!(recent_turn_window_start(&[sentinel, recent]), Some(6));
}
#[test]
fn message_header_sanitizes_dynamic_binding_labels() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "continue".into(),
        },
    );
    message.trigger_kind = Some(ContinuationTriggerKind::SystemTick);
    message.work_item_id = Some("work-1]\n[operator".into());
    message.task_id = Some("task-1\tbad".into());

    let header = message_header(&message);

    assert!(header.contains("[work_item:work-1___operator]"));
    assert!(header.contains("[task:task-1_bad]"));
    assert!(!header.contains("[operator]"));
}
#[test]
fn turn_record_matches_message_requires_matching_non_empty_turn_id() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "run tests".into(),
        },
    );
    message.message_seq = Some(4);
    message.turn_id = Some("turn-current".into());

    let mut record = TurnRecord::new("default", " turn-current ", 4);

    assert!(turn_record_matches_message(&record, &message));

    record.turn_id = "turn-other".into();
    assert!(!turn_record_matches_message(&record, &message));

    record.turn_id = String::new();
    message.turn_id = None;
    assert!(!turn_record_matches_message(&record, &message));

    record.turn_id = "turn-current".into();
    message.message_seq = Some(4);
    assert!(!turn_record_matches_message(&record, &message));

    record.input_message_ids = vec![message.id.clone()];
    record.trigger = Some(holon_domain::TurnTriggerSummary::from_message(&message));
    assert!(!turn_record_matches_message(&record, &message));

    message.turn_id = Some(" turn-current ".into());
    assert!(turn_record_matches_message(&record, &message));
}
#[test]
fn turn_record_matches_message_ignores_sequence_collision() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "run tests".into(),
        },
    );
    message.message_seq = Some(4);

    let record = TurnRecord::new("default", "turn-current", 4);

    assert!(!turn_record_matches_message(&record, &message));
}
#[test]
fn latest_trusted_operator_input_ignores_legacy_null_sequence() {
    let mut sequenced = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "new sequenced request".into(),
        },
    );
    sequenced.id = "msg-sequenced".into();
    sequenced.message_seq = Some(42);
    sequenced.created_at = chrono::DateTime::parse_from_rfc3339("2026-06-18T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let mut legacy_without_sequence = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "old legacy request without message_seq".into(),
        },
    );
    legacy_without_sequence.id = "msg-legacy".into();
    legacy_without_sequence.message_seq = None;
    legacy_without_sequence.created_at =
        chrono::DateTime::parse_from_rfc3339("2026-06-18T11:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::System {
            subsystem: "task".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "task completed".into(),
        },
    );

    let messages = vec![sequenced.clone(), legacy_without_sequence];
    let latest = latest_trusted_operator_input(&messages, &current_message)
        .expect("sequenced operator message should be selected");

    assert_eq!(latest.id, "msg-sequenced");
}
#[test]
fn budget_and_render_submodules_cooperate() {
    use super::budget::{estimate_section_tokens, fit_section_to_budget};
    use super::render::section;

    // Use enough content to ensure truncation is needed under a tight budget.
    let content = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\n".repeat(8);
    let sec = section("test_section", content);

    // Full content fits within a generous budget.
    let full_tokens = estimate_section_tokens(&sec);
    let fitted = fit_section_to_budget(sec.clone(), full_tokens)
        .expect("section should fit within its own token estimate");
    assert_eq!(fitted.content, sec.content);

    // Tight budget truncates content but preserves the section name.
    // Use a budget large enough for header + truncation suffix but
    // much smaller than the full section.
    let truncated =
        fit_section_to_budget(sec, 30).expect("section should still be present under tight budget");
    assert_eq!(truncated.name, "test_section");
    assert!(
        truncated.content.contains("[truncated for budget]"),
        "truncated content should include budget notice"
    );
}
