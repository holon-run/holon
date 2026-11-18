use super::*;
use crate::{AuthorityClass, MessageOrigin, Priority};

fn binding() -> CursorBinding {
    CursorBinding {
        runtime_id: "runtime".into(),
        agent_id: "agent".into(),
        agent_incarnation: 1,
        event_log_epoch: "epoch".into(),
        visibility_scope_id: "scope".into(),
        schema_version: CONVERSATION_SCHEMA_VERSION,
        query_version: CONVERSATION_QUERY_VERSION,
    }
}

fn trigger(kind: MessageKind, trigger_kind: Option<ContinuationTriggerKind>) -> TurnTriggerSummary {
    TurnTriggerSummary {
        message_id: Some("message".into()),
        kind,
        origin: MessageOrigin::System {
            subsystem: "test".into(),
        },
        authority_class: AuthorityClass::RuntimeInstruction,
        priority: Priority::Normal,
        trigger_kind,
        task_id: None,
    }
}

#[test]
fn task_result_presentation_is_bounded_and_optional() {
    let mut message = crate::MessageEnvelope::new(
        "agent",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "task".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        crate::MessageBody::Text {
            text: "界".repeat(1000),
        },
    );
    message.metadata = Some(
        serde_json::json!({"task_id":"task", "task_status":"failed", "task_summary":"摘要".repeat(200)}),
    );
    let result = TaskResultPresentation::from_message_payload(Some(
        serde_json::to_string(&message).unwrap(),
    ))
    .unwrap();
    assert_eq!(result.preview.chars().count(), 320);
    assert_eq!(result.summary.unwrap().chars().count(), 160);
    assert_eq!(result.status, crate::TaskStatus::Failed);
    assert!(TaskResultPresentation::from_message_payload(None).is_none());
    message.kind = MessageKind::InternalFollowup;
    assert!(TaskResultPresentation::from_message_payload(Some(
        serde_json::to_string(&message).unwrap()
    ))
    .is_none());
}

#[test]
fn presentation_is_derived_from_creation_trigger() {
    assert_eq!(
        presentation_class(&trigger(
            MessageKind::InternalFollowup,
            Some(ContinuationTriggerKind::TaskResult),
        )),
        PresentationClass::Task
    );
    assert_eq!(
        presentation_class(&trigger(MessageKind::OperatorPrompt, None)),
        PresentationClass::Operator
    );
}

#[test]
fn result_mapping_does_not_infer_settled() {
    assert_eq!(map_result(0, None, false), (ResultState::Pending, false));
    assert_eq!(
        map_result(0, None, true),
        (
            ResultState::Unavailable {
                reason: ResultUnavailableReason::MissingCanonicalLinkage,
                retryable: false,
            },
            true
        )
    );
    assert_eq!(map_result(2, None, false), (ResultState::Available, false));
    assert_eq!(map_result(2, None, true), (ResultState::Available, true));
    assert_eq!(
        map_result(0, Some(&TurnNoBriefReason::ToolOnlyWait), false),
        (
            ResultState::None {
                reason: NoBriefReason::ToolOnlyWait
            },
            true
        )
    );
}

#[test]
fn cursor_round_trips_and_is_url_safe() {
    let cursor = HistoryCursor {
        binding: binding(),
        before: TurnKey {
            turn_index: 10,
            turn_id: "turn-10".into(),
        },
        membership_upper_bound: TurnKey {
            turn_index: 20,
            turn_id: "turn-20".into(),
        },
    };
    let codec = CursorCodec::new(b"secret".to_vec());
    let encoded = codec.encode(&cursor);
    assert!(!encoded.contains(['+', '/', '=']));
    assert_eq!(codec.decode(&encoded, &binding()), Ok(cursor));
}

#[test]
fn cursor_rejects_kind_binding_version_and_tampering() {
    let cursor = StreamCursor {
        binding: binding(),
        event_seq: 42,
    };
    let codec = CursorCodec::new(b"secret".to_vec());
    let encoded = codec.encode(&cursor);

    assert_eq!(
        codec.decode::<HistoryCursor>(&encoded, &binding()),
        Err(CursorDecodeError::KindMismatch)
    );

    let mut other = binding();
    other.agent_id = "other".into();
    assert_eq!(
        codec.decode::<StreamCursor>(&encoded, &other),
        Err(CursorDecodeError::BindingMismatch)
    );

    for other in [
        CursorBinding {
            agent_incarnation: 2,
            ..binding()
        },
        CursorBinding {
            runtime_id: "other".into(),
            ..binding()
        },
        CursorBinding {
            visibility_scope_id: "other".into(),
            ..binding()
        },
    ] {
        assert_eq!(
            codec.decode::<StreamCursor>(&encoded, &other),
            Err(CursorDecodeError::BindingMismatch)
        );
    }

    let mut other = binding();
    other.event_log_epoch = "other".into();
    assert_eq!(
        codec.decode::<StreamCursor>(&encoded, &other),
        Err(CursorDecodeError::EventLogEpochMismatch)
    );

    let mut other = binding();
    other.schema_version += 1;
    assert_eq!(
        codec.decode::<StreamCursor>(&encoded, &other),
        Err(CursorDecodeError::SchemaVersionMismatch {
            expected: 3,
            actual: 2
        })
    );

    let mut other = binding();
    other.query_version += 1;
    assert_eq!(
        codec.decode::<StreamCursor>(&encoded, &other),
        Err(CursorDecodeError::QueryVersionMismatch {
            expected: 3,
            actual: 2
        })
    );

    let mut tampered = encoded.into_bytes();
    tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        codec.decode::<StreamCursor>(
            std::str::from_utf8(&tampered).expect("ASCII cursor"),
            &binding()
        ),
        Err(CursorDecodeError::Tampered)
    );
}
