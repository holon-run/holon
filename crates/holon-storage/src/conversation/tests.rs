use super::*;

#[test]
fn change_id_collection_ignores_values_beyond_the_depth_budget() {
    let mut nested = serde_json::json!({
        "turn_id": "turn-too-deep",
        "message_id": "message-too-deep",
    });
    for _ in 0..=MAX_CHANGE_ID_JSON_DEPTH {
        nested = serde_json::json!({ "nested": nested });
    }
    let payload = serde_json::json!({
        "turn_id": "turn-visible",
        "message_id": "message-visible",
        "nested": nested,
    });
    let mut turn_ids = std::collections::BTreeSet::new();
    let mut message_ids = std::collections::BTreeSet::new();

    collect_change_ids(&payload, &mut turn_ids, &mut message_ids, 0);

    assert_eq!(turn_ids.into_iter().collect::<Vec<_>>(), ["turn-visible"]);
    assert_eq!(
        message_ids.into_iter().collect::<Vec<_>>(),
        ["message-visible"]
    );
}

#[test]
fn transcript_activity_source_classifies_every_entry_kind_explicitly() {
    // Parameterized on purpose: a new `TranscriptEntryKind` variant has to state
    // whether it backs an activity source instead of being silently ignored, and
    // the four known detail kinds must not be reported as unknown by coverage.
    let cases = [
        (TranscriptEntryKind::IncomingMessage, None),
        (TranscriptEntryKind::AssistantRound, Some(SOURCE_ASSISTANT)),
        (TranscriptEntryKind::ToolResults, None),
        (TranscriptEntryKind::RuntimeFailure, Some(SOURCE_ERROR)),
        (TranscriptEntryKind::ContinuationPrompt, None),
        (TranscriptEntryKind::SubagentPrompt, None),
        (
            TranscriptEntryKind::SubagentAssistantRound,
            Some(SOURCE_ASSISTANT),
        ),
    ];
    for (kind, expected) in cases {
        assert_eq!(transcript_activity_source(&kind), expected, "kind {kind:?}");
        // Coverage decodes stored kinds through this same contract, so every
        // known kind must round-trip instead of looking like an unknown value.
        let stored = serde_json::to_value(&kind).expect("kind serializes");
        let decoded: TranscriptEntryKind = serde_json::from_value(stored).expect("kind decodes");
        assert_eq!(decoded, kind);
    }
    assert!(
        serde_json::from_value::<TranscriptEntryKind>(serde_json::json!("future_kind")).is_err(),
        "unregistered kinds must stay outside the closed vocabulary"
    );
}
