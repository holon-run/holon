use super::*;
use serde_json::json;

#[test]
fn message_header_projects_envelope_creation_time() {
    let mut message = MessageEnvelope::new(
        "target",
        holon_domain::MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        holon_domain::Priority::Normal,
        MessageBody::Text {
            text: "request".into(),
        },
    );
    message.created_at = "2024-01-02T03:04:05.123Z".parse().unwrap();
    message.metadata = Some(json!({"occurred_at": "2020-01-01T00:00:00Z"}));
    let header = message_header(&message);
    assert!(header.contains("[message_created_at=2024-01-02T03:04:05.123Z UTC]"));
    assert!(!header.contains("2020-01-01"));
    for budget in [1, 256] {
        let input = super::render_current_input_section(&message, budget, None);
        assert!(input.content.contains(&header));
    }
}

#[test]
fn reply_expectation_context_requires_runtime_owned_consistent_metadata() {
    let mut message = MessageEnvelope::new(
        "target",
        holon_domain::MessageKind::InternalFollowup,
        MessageOrigin::Task {
            task_id: "task-1".into(),
        },
        AuthorityClass::RuntimeInstruction,
        holon_domain::Priority::Normal,
        MessageBody::Text {
            text: "request".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
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

    let rendered = message_reply_expectation_context(&message).unwrap();
    assert!(rendered.contains("sender"));
    assert!(rendered.contains("sender is waiting for a reply"));
    let current_input = super::render_current_input_section(&message, 256, None);
    assert!(current_input.content.contains(&rendered));
    assert!(current_input.content.contains("\n  request"));

    message.correlation_id = Some("other-task".into());
    assert!(message_reply_expectation_context(&message).is_none());
    assert!(!super::render_current_input_section(&message, 256, None)
        .content
        .contains("sender is waiting for a reply"));
}

#[test]
fn ordinary_messages_do_not_render_reply_expectation_context() {
    let message = MessageEnvelope::new(
        "target",
        holon_domain::MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "agent_message".into(),
        },
        AuthorityClass::RuntimeInstruction,
        holon_domain::Priority::Normal,
        MessageBody::Text {
            text: "notification".into(),
        },
    );

    assert!(message_reply_expectation_context(&message).is_none());
}

#[test]
fn compact_routing_context_shows_sender_recipient_and_reply_target() {
    let mut message = MessageEnvelope::new(
        "target",
        holon_domain::MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "agent_message".into(),
        },
        AuthorityClass::RuntimeInstruction,
        holon_domain::Priority::Normal,
        MessageBody::Text {
            text: "forwarded message".into(),
        },
    );
    message.routing_context = Some(AgentMessageRoutingContext {
        message_id: message.id.clone(),
        sender_agent_id: Some("relay-agent".into()),
        recipient_agent_id: "target".into(),
        reply_to_agent_id: Some("origin-agent".into()),
        correlation_id: None,
        in_reply_to_message_id: Some("incoming-message".into()),
        original_sender_agent_id: Some("origin-agent".into()),
        original_reply_to_agent_id: Some("origin-agent".into()),
    });

    let compact = message_routing_context_compact(&message, 48).unwrap();
    assert!(compact.contains("sender=relay-agent"));
    assert!(compact.contains("recipient=target"));
    assert!(compact.contains("reply=origin-agent"));
}
