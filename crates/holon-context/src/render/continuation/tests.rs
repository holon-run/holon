use super::*;
use holon_domain::{AuthorityClass, Priority};

#[test]
fn resolve_latest_trusted_operator_input_prefers_current_operator_input() {
    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "new request".into(),
        },
    );
    let mut historical = current_message.clone();
    historical.id = "msg-historical".into();
    historical.message_seq = Some(7);

    let resolved = resolve_latest_trusted_operator_input(Some(&historical), &current_message);
    assert_eq!(
        resolved.map(|message| message.id.clone()),
        Some(current_message.id)
    );
}

#[test]
fn resolve_latest_trusted_operator_input_falls_back_to_historical_anchor() {
    let mut historical = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "older request".into(),
        },
    );
    historical.id = "msg-historical".into();
    historical.message_seq = Some(7);
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

    assert!(resolve_latest_trusted_operator_input(None, &current_message).is_none());
    let resolved = resolve_latest_trusted_operator_input(Some(&historical), &current_message)
        .expect("historical anchor expected");
    assert_eq!(resolved.id, "msg-historical");
}
